param(
    [string]$RuntimeLibDir,
    [ValidateRange(1, 1000)][int]$Warmup = 30,
    [ValidateRange(1, 10000)][int]$Iterations = 100
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
if (-not $RuntimeLibDir) {
    $RuntimeLibDir = Join-Path $RepoRoot "third_party\mpv-install\lib"
}
$RuntimeLibDir = (Resolve-Path -LiteralPath $RuntimeLibDir).Path
$Builder = Join-Path $RuntimeLibDir "rife_engine_builder.exe"
$Runtime = Join-Path $RuntimeLibDir "rife_runtime.dll"
$Cuda = Join-Path $RuntimeLibDir "cudart64_12.dll"
$Probe = Join-Path $RepoRoot "build\rife-runtime\rife_runtime_probe.exe"
$Manifest = Get-Content -LiteralPath (Join-Path $RuntimeLibDir "frame-interpolation\runtime-manifest.json") `
    -Raw -Encoding utf8 | ConvertFrom-Json
if ($Manifest.engineBuilder -ne "rife_engine_builder.exe") {
    throw "Manifest still names the NVIDIA SDK command-line tool"
}
foreach ($Required in @($Builder, $Runtime, $Cuda, $Probe)) {
    if (-not (Test-Path -LiteralPath $Required -PathType Leaf)) {
        throw "RIFE builder test input is missing: $Required"
    }
}
if (Test-Path -LiteralPath (Join-Path $RuntimeLibDir "tensorrt_rtx.exe")) {
    throw "NVIDIA SDK developer tool is still present in the staged runtime"
}

$TestRoot = Join-Path $RepoRoot ("build\rife-builder-test-" + [guid]::NewGuid().ToString("N"))
$BuildPrefix = [System.IO.Path]::GetFullPath((Join-Path $RepoRoot "build")).TrimEnd('\') + '\'
if (-not [System.IO.Path]::GetFullPath($TestRoot).StartsWith(
        $BuildPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Builder test directory must remain below the repository build directory"
}
New-Item -ItemType Directory -Path $TestRoot | Out-Null
$PreviousAlignment = $env:MSGO_RIFE_ALIGNMENT
function Get-BuilderArguments {
    param($Model, [string]$Onnx, [string]$Engine)
    $Result = @("--onnx=$Onnx", "--saveEngine=$Engine")
    for ($Index = 0; $Index -lt $Model.profiles.Count; $Index++) {
        $Profile = $Model.profiles[$Index]
        $Result += @(
            "--profile=$Index",
            "--minShapes=input:1x11x$($Profile.minHeight)x$($Profile.minWidth)",
            "--optShapes=input:1x11x$($Profile.optHeight)x$($Profile.optWidth)",
            "--maxShapes=input:1x11x$($Profile.maxHeight)x$($Profile.maxWidth)"
        )
    }
    $Result
}
try {
    $Model = $Manifest.models[0]
    $Onnx = Join-Path $RuntimeLibDir "frame-interpolation\models\$($Model.onnxFile)"
    $RejectedOutput = Join-Path $TestRoot "rejected.engine"
    $CompleteArguments = @(Get-BuilderArguments $Model $Onnx $RejectedOutput)
    $BadRange = @($CompleteArguments)
    $BadRange[3] = '--minShapes=input:1x11x4096x4096'
    $MissingOnnx = Join-Path $TestRoot 'missing.onnx'
    $Cases = @(
        @{ Name = "missing-arguments"; Arguments = @() },
        @{ Name = "unsupported-option"; Arguments = $CompleteArguments + "--useGpu" },
        @{ Name = "duplicate-path"; Arguments = $CompleteArguments + "--onnx=$Onnx" },
        @{ Name = "incomplete-profile"; Arguments = @($CompleteArguments[0..4]) },
        @{ Name = "duplicate-shape"; Arguments = $CompleteArguments + "--minShapes=input:1x11x64x64" },
        @{ Name = "profile-order"; Arguments = @("--onnx=$Onnx", "--saveEngine=$RejectedOutput", "--profile=1") },
        @{ Name = "shape-overflow"; Arguments = @("--onnx=$Onnx", "--saveEngine=$RejectedOutput", "--profile=0", "--minShapes=input:1x11x999999999999x64") },
        @{ Name = "non-monotonic-shapes"; Arguments = $BadRange },
        @{ Name = "missing-onnx"; Arguments = @(Get-BuilderArguments $Model $MissingOnnx $RejectedOutput) },
        @{ Name = "corrupt-onnx"; Arguments = @(Get-BuilderArguments $Model $PSCommandPath $RejectedOutput) }
    )
    foreach ($Case in $Cases) {
        $Arguments = $Case.Arguments
        & $Builder @Arguments
        if ($LASTEXITCODE -eq 0 -or (Test-Path -LiteralPath $RejectedOutput)) {
            throw "Builder did not reject malformed input: $($Case.Name)"
        }
        Write-Host "RIFE_BUILDER_REJECTION_OK case=$($Case.Name)"
    }
    foreach ($Model in $Manifest.models) {
        $Onnx = Join-Path $RuntimeLibDir "frame-interpolation\models\$($Model.onnxFile)"
        $OnnxHash = (Get-FileHash -LiteralPath $Onnx -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($OnnxHash -ne $Model.onnxSha256) {
            throw "RIFE model hash mismatch: $($Model.id)"
        }
        $Engine = Join-Path $TestRoot "$($Model.id).engine"
        $Arguments = @(Get-BuilderArguments $Model $Onnx $Engine)
        & $Builder @Arguments
        if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $Engine -PathType Leaf)) {
            throw "Owned builder failed for model $($Model.id)"
        }
        $BeforeOverwrite = (Get-FileHash -LiteralPath $Engine -Algorithm SHA256).Hash
        & $Builder @Arguments
        if ($LASTEXITCODE -eq 0 -or (Get-FileHash -LiteralPath $Engine -Algorithm SHA256).Hash -ne $BeforeOverwrite) {
            throw "Builder overwrote an existing Engine for $($Model.id)"
        }
        $env:MSGO_RIFE_ALIGNMENT = [string]$Model.shapeAlignment
        Write-Host "RIFE_BUILDER_MODEL_CHECK model=$($Model.id) onnx=$OnnxHash profiles=$($Model.profiles.Count)"
        & $Probe $Runtime $Engine $Cuda 3840 2160 $Warmup $Iterations
        if ($LASTEXITCODE -ne 0) {
            throw "D3D11 P010 runtime rejected the new Engine for $($Model.id)"
        }
    }
    Write-Host "RIFE_ENGINE_BUILDER_TEST_OK models=$($Manifest.models.Count)" -ForegroundColor Green
} finally {
    $env:MSGO_RIFE_ALIGNMENT = $PreviousAlignment
    # TestRoot was resolved and bounded above; never touch user Engine caches.
    Remove-Item -LiteralPath $TestRoot -Recurse -Force
}
