param(
    [ValidateRange(1.0, 1000.0)]
    [double]$MinimumFps = 72.0,
    [ValidateRange(1, 10000)]
    [int]$Iterations = 240
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
$ClangBin = "C:\msys64\clang64\bin"
$Compiler = Join-Path $ClangBin "clang++.exe"
if (-not (Test-Path -LiteralPath $Compiler)) {
    throw "clang64 compiler is unavailable: $Compiler"
}

$Source = Join-Path $PSScriptRoot "nvofa_poc.cpp"
$IncludeDir = Join-Path $RepoRoot "third_party\nvofapi\include"
$BuildDir = Join-Path $env:TEMP "mediastationgo-nvofa-poc"
$Executable = Join-Path $BuildDir "nvofa_poc.exe"
New-Item -ItemType Directory -Force -Path $BuildDir | Out-Null

$RequiredHeaders = @(
    (Join-Path $IncludeDir "nvOpticalFlowCommon.h"),
    (Join-Path $IncludeDir "nvOpticalFlowD3D11.h")
)
foreach ($Required in @($Source) + $RequiredHeaders) {
    if (-not (Test-Path -LiteralPath $Required)) {
        throw "NVOFA PoC source dependency is missing: $Required"
    }
}

$Inputs = @(Get-Item -LiteralPath $Source) + @(Get-Item -LiteralPath $RequiredHeaders)
$NewestInput = ($Inputs | Measure-Object -Property LastWriteTimeUtc -Maximum).Maximum
$NeedsBuild = -not (Test-Path -LiteralPath $Executable)
if (-not $NeedsBuild) {
    $NeedsBuild = (Get-Item -LiteralPath $Executable).LastWriteTimeUtc -lt $NewestInput
}
if ($NeedsBuild) {
    Write-Host "NVOFA_POC_BUILD compiler=$Compiler" -ForegroundColor Cyan
    & $Compiler `
        -std=c++20 `
        -O2 `
        -Wall `
        -Wextra `
        -Werror `
        "-I$IncludeDir" `
        $Source `
        -ld3d11 `
        -ld3dcompiler `
        -ldxgi `
        -lole32 `
        -static `
        -o $Executable
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $Executable)) {
        throw "NVOFA PoC compilation failed with exit code $LASTEXITCODE"
    }
}

$PreviousPath = $env:PATH
$env:PATH = "$ClangBin;$env:PATH"
try {
    foreach ($Resolution in @(
        @{ Width = 1920; Height = 1080 },
        @{ Width = 2560; Height = 1440 },
        @{ Width = 3840; Height = 2160 }
    )) {
        Write-Host "NVOFA_POC_RUN resolution=$($Resolution.Width)x$($Resolution.Height) iterations=$Iterations minimumFps=$MinimumFps" -ForegroundColor Cyan
        & $Executable $Resolution.Width $Resolution.Height $Iterations $MinimumFps
        if ($LASTEXITCODE -ne 0) {
            throw "NVOFA PoC failed for $($Resolution.Width)x$($Resolution.Height) with exit code $LASTEXITCODE"
        }
    }
} finally {
    $env:PATH = $PreviousPath
}

Write-Host "NVOFA_POC_SUITE_OK minimumFps=$MinimumFps" -ForegroundColor Green
