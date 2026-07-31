param(
    [string]$Python = "python",
    [string]$RuntimeDir = ""
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
if (-not $RuntimeDir) {
    $RuntimeDir = Join-Path $RepoRoot "third_party\frame-interpolation-runtime"
}
$RuntimeDir = [System.IO.Path]::GetFullPath($RuntimeDir)
$Wrapper = Join-Path $PSScriptRoot "rife_fp16_io_wrapper.py"
if (-not (Test-Path -LiteralPath $Wrapper -PathType Leaf)) {
    throw "RIFE FP16 IO wrapper is missing: $Wrapper"
}
if (-not (Get-Command $Python -ErrorAction SilentlyContinue)) {
    throw "Python executable is unavailable: $Python"
}

$ModelDir = Join-Path $RuntimeDir "vapoursynth\plugins\models\rife"
$Models = @(
    @{
        Id = "rife-v4.26"
        Scale = "1.0"
        Alignment = 64
        Source = Join-Path $ModelDir "rife_v4.26.onnx"
        Output = Join-Path $ModelDir "rife_v4.26_fp16_io.onnx"
    },
    @{
        Id = "rife-v4.25-lite"
        Scale = "1.0"
        Alignment = 128
        Source = Join-Path $ModelDir "rife_v4.25_lite.onnx"
        Output = Join-Path $ModelDir "rife_v4.25_lite_fp16_io.onnx"
    }
)

foreach ($Model in $Models) {
    if (-not (Test-Path -LiteralPath $Model.Source -PathType Leaf)) {
        throw "RIFE source ONNX is missing: $($Model.Source)"
    }
    & $Python $Wrapper `
        --input $Model.Source `
        --output $Model.Output `
        --model-id $Model.Id `
        --scale $Model.Scale `
        --alignment $Model.Alignment
    if ($LASTEXITCODE -ne 0) {
        throw "RIFE FP16 IO wrapping failed for $($Model.Id)"
    }
}

Write-Host "RIFE_FP16_IO_MODELS_OK models=$($Models.Count)" -ForegroundColor Green
