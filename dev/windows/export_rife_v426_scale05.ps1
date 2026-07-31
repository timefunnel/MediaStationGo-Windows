param(
    [string]$Python = "python",
    [string]$VsRifeDir = (Join-Path $env:TEMP "mediastation-vs-rife-3488617283"),
    [string]$WeightPath = (Join-Path $env:TEMP "flownet_v4.26.pkl"),
    [string]$OutputPath = "",
    [switch]$SkipDownload
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
$Exporter = Join-Path $PSScriptRoot "rife_v426_scale05_export.py"
$ExpectedCommit = "3488617283db7c428a83ba4a19382285da698b6a"
$ExpectedWeightSha256 = "45c7f74156704769dc9f85cfcaf8552e1e926f9399dcfa3a553dee88fac6f53f"
$WeightUrl = "https://github.com/HolyWu/vs-rife/releases/download/model/flownet_v4.26.pkl"

if (-not (Test-Path -LiteralPath $Exporter -PathType Leaf)) {
    throw "Export wrapper is missing: $Exporter"
}
if (-not (Get-Command $Python -ErrorAction SilentlyContinue)) {
    throw "Python executable is unavailable: $Python"
}

if (-not (Test-Path -LiteralPath $VsRifeDir -PathType Container)) {
    & git clone --depth 1 https://github.com/HolyWu/vs-rife.git $VsRifeDir
    if ($LASTEXITCODE -ne 0) {
        throw "Could not clone the pinned vs-rife source"
    }
}
$ActualCommit = (& git -C $VsRifeDir rev-parse HEAD 2>$null).Trim()
if ($LASTEXITCODE -ne 0) {
    throw "vs-rife source directory is not a Git checkout: $VsRifeDir"
}
if ($ActualCommit -ne $ExpectedCommit) {
    & git -C $VsRifeDir fetch --depth 1 origin $ExpectedCommit
    if ($LASTEXITCODE -ne 0) {
        throw "Could not fetch pinned vs-rife commit: $ExpectedCommit"
    }
    & git -C $VsRifeDir checkout --detach $ExpectedCommit
    if ($LASTEXITCODE -ne 0) {
        throw "Could not check out pinned vs-rife commit: $ExpectedCommit"
    }
}
$ActualCommit = (& git -C $VsRifeDir rev-parse HEAD).Trim()
if ($ActualCommit -ne $ExpectedCommit) {
    throw "vs-rife commit mismatch: expected=$ExpectedCommit actual=$ActualCommit"
}

if (-not $SkipDownload -and -not (Test-Path -LiteralPath $WeightPath -PathType Leaf)) {
    & curl.exe -L --fail --retry 5 --retry-delay 3 --retry-all-errors --output $WeightPath $WeightUrl
    if ($LASTEXITCODE -ne 0) {
        throw "Could not download the official v4.26 weight"
    }
}
if (-not (Test-Path -LiteralPath $WeightPath -PathType Leaf)) {
    throw "Official v4.26 weight is missing: $WeightPath"
}
$WeightPath = (Resolve-Path -LiteralPath $WeightPath).Path
$ActualWeightSha256 = (Get-FileHash -LiteralPath $WeightPath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($ActualWeightSha256 -ne $ExpectedWeightSha256) {
    throw "Official v4.26 weight SHA-256 mismatch: expected=$ExpectedWeightSha256 actual=$ActualWeightSha256"
}

if (-not $OutputPath) {
    $OutputPath = Join-Path $RepoRoot "third_party\frame-interpolation-runtime\vapoursynth\plugins\models\rife\rife_v4.26_scale0.5.onnx"
}
$OutputPath = [System.IO.Path]::GetFullPath($OutputPath)
$OutputDir = Split-Path -Parent $OutputPath
New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
$TempOutput = "$OutputPath.exporting"
Remove-Item -LiteralPath $TempOutput -Force -ErrorAction SilentlyContinue

try {
    & $Python $Exporter `
        --vs-rife-dir $VsRifeDir `
        --weight $WeightPath `
        --output $TempOutput
    if ($LASTEXITCODE -ne 0) {
        throw "RIFE v4.26 scale=0.5 ONNX export or PyTorch validation failed"
    }
    Move-Item -LiteralPath $TempOutput -Destination $OutputPath -Force
} finally {
    Remove-Item -LiteralPath $TempOutput -Force -ErrorAction SilentlyContinue
}

$OutputSha256 = (Get-FileHash -LiteralPath $OutputPath -Algorithm SHA256).Hash.ToLowerInvariant()
Write-Host "RIFE_V426_SCALE05_EXPORT_OK output=$OutputPath sha256=$OutputSha256 upstream=$ExpectedCommit weight=$ActualWeightSha256" -ForegroundColor Green
