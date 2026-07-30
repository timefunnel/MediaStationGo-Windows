$ErrorActionPreference = "Stop"

$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
$ClangBin = "C:\msys64\clang64\bin"
$Compiler = Join-Path $ClangBin "clang++.exe"
$Source = Join-Path $PSScriptRoot "nvof_memc_probe.cpp"
$IncludeDir = Join-Path $RepoRoot "third_party\nvofapi\include"
$BuildDir = Join-Path $env:TEMP "mediastationgo-nvof-memc-probe"
$Executable = Join-Path $BuildDir "nvof_memc_probe.exe"

foreach ($Required in @(
    $Compiler,
    $Source,
    (Join-Path $IncludeDir "nvOpticalFlowCommon.h"),
    (Join-Path $IncludeDir "nvOpticalFlowD3D11.h")
)) {
    if (-not (Test-Path -LiteralPath $Required)) {
        throw "NVOF MEMC probe dependency is missing: $Required"
    }
}

New-Item -ItemType Directory -Force -Path $BuildDir | Out-Null

Write-Host "NVOF_MEMC_PROBE_BUILD compiler=$Compiler" -ForegroundColor Cyan
& $Compiler `
    -std=c++20 `
    -O2 `
    -Wall `
    -Wextra `
    -Werror `
    "-I$IncludeDir" `
    $Source `
    -ld3d11 `
    -ldxgi `
    -lole32 `
    -static `
    -o $Executable
if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $Executable)) {
    throw "NVOF MEMC probe compilation failed with exit code $LASTEXITCODE"
}

$PreviousPath = $env:PATH
$env:PATH = "$ClangBin;$env:PATH"
try {
    $Cases = @(
        @{ Profile = "official-sample"; Input = "gray8" },
        @{ Profile = "official-sample"; Input = "nv12" },
        @{ Profile = "strict-memc"; Input = "gray8" },
        @{ Profile = "strict-memc"; Input = "nv12" },
        @{ Profile = "forward-cost"; Input = "gray8" },
        @{ Profile = "both-no-cost"; Input = "gray8" }
    )
    foreach ($Case in $Cases) {
        $Profile = $Case.Profile
        $InputFormat = $Case.Input
        Write-Host "NVOF_MEMC_PROBE_RUN profile=$Profile input=$InputFormat" -ForegroundColor Cyan
        & $Executable "--input=$InputFormat" "--profile=$Profile"
        if ($LASTEXITCODE -ne 0) {
            throw "NVOF MEMC probe failed for profile=$Profile input=$InputFormat with exit code $LASTEXITCODE"
        }
    }
} finally {
    $env:PATH = $PreviousPath
}
