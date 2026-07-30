param(
    [string]$RuntimeDir,
    [string]$EnginePath,
    [string]$CudaRuntimeDll,
    [ValidateRange(320, 7680)]
    [int]$Width = 3840,
    [ValidateRange(240, 4320)]
    [int]$Height = 2160,
    [ValidateRange(1, 1000)]
    [int]$Warmup = 20,
    [ValidateRange(1, 10000)]
    [int]$Iterations = 100,
    [ValidateRange(1.0, 240.0)]
    [double]$RequiredPairFps = 24.0,
    [ValidateRange(1.0, 1000.0)]
    [double]$MaximumP95Milliseconds = 41.67
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
if (-not $RuntimeDir) {
    $RuntimeDir = Join-Path $RepoRoot "third_party\frame-interpolation-runtime"
}
$RuntimeDir = (Resolve-Path -LiteralPath $RuntimeDir).Path
if (-not $CudaRuntimeDll) {
    $CudaRuntimeCandidates = @(Get-ChildItem -LiteralPath (Join-Path $RuntimeDir "bin") -Filter "cudart64_*.dll" -File -ErrorAction SilentlyContinue)
    if ($CudaRuntimeCandidates.Count -ne 1) {
        throw "Expected exactly one CUDA Runtime DLL under $RuntimeDir\bin; pass -CudaRuntimeDll explicitly"
    }
    $CudaRuntimeDll = $CudaRuntimeCandidates[0].FullName
}
if (-not $EnginePath) {
    $EngineDir = Join-Path $RuntimeDir "engines\poc-rife-v4_26-impl1-${Width}x${Height}-scale1_0-fp16"
    $Engines = @(Get-ChildItem -LiteralPath $EngineDir -Filter "*.engine" -File)
    if ($Engines.Count -ne 1) {
        throw "Expected exactly one 4K implementation 1 engine in $EngineDir"
    }
    $EnginePath = $Engines[0].FullName
}
$EnginePath = (Resolve-Path -LiteralPath $EnginePath).Path
$CudaRuntimeDll = (Resolve-Path -LiteralPath $CudaRuntimeDll).Path

$TensorRtRoot = Join-Path $RuntimeDir ".tensorrt\TensorRT-RTX-1.4.0.76"
$TensorRtInclude = Join-Path $TensorRtRoot "include"
$TensorRtLib = Join-Path $TensorRtRoot "lib"
$TensorRtDll = Join-Path $TensorRtRoot "bin\tensorrt_rtx_1_4.dll"
$Source = Join-Path $PSScriptRoot "rife_d3d11_trt_probe.cpp"
$Shader = Join-Path $PSScriptRoot "rife_d3d11_trt_probe.hlsl"
foreach ($Required in @($Source, $Shader, (Join-Path $TensorRtInclude "NvInferRuntime.h"), (Join-Path $TensorRtLib "tensorrt_rtx_1_4.lib"), $TensorRtDll)) {
    if (-not (Test-Path -LiteralPath $Required)) {
        throw "RIFE D3D11 TensorRT probe dependency is missing: $Required"
    }
}

$VsWhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path -LiteralPath $VsWhere)) {
    throw "Visual Studio Build Tools locator is missing: $VsWhere"
}
$VsPath = & $VsWhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
$VcVars = Join-Path $VsPath "VC\Auxiliary\Build\vcvars64.bat"
if (-not (Test-Path -LiteralPath $VcVars)) {
    throw "Visual Studio x64 build environment is missing: $VcVars"
}

$OutputDir = Join-Path $RepoRoot "build\rife-d3d11-trt-probe"
New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
$Executable = Join-Path $OutputDir "rife_d3d11_trt_probe.exe"
$EnvironmentScript = Join-Path $env:TEMP "mediastation-rife-probe-vcvars.cmd"
try {
    [System.IO.File]::WriteAllText(
        $EnvironmentScript,
        "@call `"$VcVars`"`r`n@set`r`n",
        [System.Text.Encoding]::ASCII)
    & cmd.exe /d /c $EnvironmentScript | ForEach-Object {
        if ($_ -match '^([^=]+)=(.*)$') {
            [Environment]::SetEnvironmentVariable($matches[1], $matches[2], "Process")
        }
    }
} finally {
    Remove-Item -LiteralPath $EnvironmentScript -Force -ErrorAction SilentlyContinue
}
$Compiler = Get-Command cl.exe -ErrorAction SilentlyContinue
if (-not $Compiler) {
    throw "Visual Studio C++ compiler is unavailable after importing vcvars64.bat"
}
& $Compiler.Source `
    /nologo /std:c++20 /EHsc /O2 /W4 /WX /wd4100 `
    "/I$PSScriptRoot" "/I$TensorRtInclude" $Source "/Fo:$OutputDir\" "/Fe:$Executable" `
    /link "/LIBPATH:$TensorRtLib" tensorrt_rtx_1_4.lib d3d11.lib dxgi.lib d3dcompiler.lib
if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $Executable)) {
    throw "RIFE D3D11 TensorRT probe compilation failed"
}
Copy-Item -LiteralPath $TensorRtDll -Destination $OutputDir -Force

$PreviousErrorActionPreference = $ErrorActionPreference
$ErrorActionPreference = "Continue"
$Output = & $Executable $EnginePath $CudaRuntimeDll $Shader $Width $Height $Warmup $Iterations 2>&1 | ForEach-Object { $_.ToString() }
$ExitCode = $LASTEXITCODE
$ErrorActionPreference = $PreviousErrorActionPreference
$Output | ForEach-Object { Write-Host $_ }
if ($ExitCode -ne 0) {
    throw "RIFE D3D11 TensorRT probe failed with exit code $ExitCode"
}
$Text = $Output -join "`n"
$Result = [regex]::Match($Text, 'RIFE_D3D11_TRT_PROBE_OK .* throughput=([0-9.]+)qps mean=([0-9.]+)ms p95=([0-9.]+)ms')
if (-not $Result.Success) {
    throw "RIFE D3D11 TensorRT probe output is missing performance metrics"
}
$Throughput = [double]::Parse($Result.Groups[1].Value, [System.Globalization.CultureInfo]::InvariantCulture)
$P95 = [double]::Parse($Result.Groups[3].Value, [System.Globalization.CultureInfo]::InvariantCulture)
if ($Throughput -lt $RequiredPairFps) {
    throw ("D3D11/CUDA/TensorRT throughput {0:N2} qps is below required {1:N2} pair fps" -f $Throughput, $RequiredPairFps)
}
if ($P95 -gt $MaximumP95Milliseconds) {
    throw ("D3D11/CUDA/TensorRT p95 {0:N2} ms exceeds budget {1:N2} ms" -f $P95, $MaximumP95Milliseconds)
}
Write-Host ("RIFE_D3D11_TRT_BUDGET_OK throughput={0:N2}qps p95={1:N2}ms required={2:N2}qps budget={3:N2}ms" -f $Throughput, $P95, $RequiredPairFps, $MaximumP95Milliseconds) -ForegroundColor Green
