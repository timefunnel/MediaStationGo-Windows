param(
    [string]$RuntimeDir,
    [string]$OutputDir
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
if (-not $RuntimeDir) {
    $RuntimeDir = Join-Path $RepoRoot "third_party\frame-interpolation-runtime"
}
if (-not $OutputDir) {
    $OutputDir = Join-Path $RepoRoot "build\rife-runtime"
}
$RuntimeDir = (Resolve-Path -LiteralPath $RuntimeDir).Path
$Source = Join-Path $PSScriptRoot "mpv\rife_runtime.cpp"
$Header = Join-Path $PSScriptRoot "mpv\rife_runtime.h"
$ProbeSource = Join-Path $PSScriptRoot "rife_runtime_probe.cpp"
$CudaHeaderDir = $PSScriptRoot
$TensorRtRoot = Join-Path $RuntimeDir ".tensorrt\TensorRT-RTX-1.4.0.76"
$TensorRtInclude = Join-Path $TensorRtRoot "include"
$TensorRtLib = Join-Path $TensorRtRoot "lib"

foreach ($Required in @(
    $Source,
    $Header,
    $ProbeSource,
    (Join-Path $CudaHeaderDir "cuda_runtime_api.h"),
    (Join-Path $TensorRtInclude "NvInferRuntime.h"),
    (Join-Path $TensorRtLib "tensorrt_rtx_1_4.lib")
)) {
    if (-not (Test-Path -LiteralPath $Required)) {
        throw "RIFE runtime build input is missing: $Required"
    }
}

$VsWhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path -LiteralPath $VsWhere)) {
    throw "Visual Studio Build Tools locator is missing: $VsWhere"
}
$VsPath = & $VsWhere -latest -products * `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -property installationPath
$VcVars = Join-Path $VsPath "VC\Auxiliary\Build\vcvars64.bat"
if (-not (Test-Path -LiteralPath $VcVars)) {
    throw "Visual Studio x64 build environment is missing: $VcVars"
}
& cmd.exe /d /c "`"$VcVars`" && set" | ForEach-Object {
    if ($_ -match '^([^=]+)=(.*)$') {
        [Environment]::SetEnvironmentVariable($matches[1], $matches[2], "Process")
    }
}
if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
    throw "Visual Studio C++ compiler is unavailable after importing vcvars64.bat"
}

New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
$OutputDir = (Resolve-Path -LiteralPath $OutputDir).Path
$OutputDll = Join-Path $OutputDir "rife_runtime.dll"
$OutputProbe = Join-Path $OutputDir "rife_runtime_probe.exe"
& cl.exe /nologo /std:c++20 /EHsc /O2 /W4 /WX /wd4100 /LD `
    "/I$CudaHeaderDir" "/I$TensorRtInclude" $Source "/Fo:$OutputDir\" `
    "/Fe:$OutputDll" /link "/LIBPATH:$TensorRtLib" `
    tensorrt_rtx_1_4.lib d3d11.lib dxgi.lib d3dcompiler.lib
if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $OutputDll)) {
    throw "RIFE runtime DLL compilation failed"
}

& cl.exe /nologo /std:c++20 /EHsc /O2 /W4 /WX $ProbeSource `
    "/Fo:$OutputDir\rife_runtime_probe.obj" "/Fe:$OutputProbe" /link `
    d3d11.lib dxgi.lib
if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $OutputProbe)) {
    throw "RIFE runtime probe compilation failed"
}

Write-Host "RIFE_RUNTIME_BUILD_OK path=$OutputDll" -ForegroundColor Green
Write-Host "RIFE_RUNTIME_PROBE_BUILD_OK path=$OutputProbe" -ForegroundColor Green
