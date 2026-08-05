param(
    [string]$AssetUrl = "https://github.com/timefunnel/MediaStationGo-Windows/releases/download/rife-runtime-x64-v1/MediaStationGo-frame-interpolation-runtime-x64-v1.zip",
    [string]$AssetSha256 = "c815c70350580761140f2c17ec950c5c2844d7e3e0c4484acecc1ecdb61e892f",
    [string]$CacheDir,
    [string]$RuntimeDir
)

$ErrorActionPreference = "Stop"

$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
$ManagedCacheRoot = Join-Path $RepoRoot ".cache"
$ManagedRuntimeRoot = Join-Path $RepoRoot "third_party"
if (-not $CacheDir) {
    $CacheDir = Join-Path $ManagedCacheRoot "frame-interpolation-runtime"
}
if (-not $RuntimeDir) {
    $RuntimeDir = Join-Path $ManagedRuntimeRoot "frame-interpolation-runtime"
}
$CacheDir = [System.IO.Path]::GetFullPath($CacheDir)
$RuntimeDir = [System.IO.Path]::GetFullPath($RuntimeDir)

function Assert-ManagedPath {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][string]$Description
    )

    $ResolvedRoot = [System.IO.Path]::GetFullPath($Root).TrimEnd('\') + '\'
    $ResolvedPath = [System.IO.Path]::GetFullPath($Path)
    if (-not $ResolvedPath.StartsWith(
            $ResolvedRoot,
            [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "$Description must stay under ${ResolvedRoot}: $ResolvedPath"
    }
}

function Get-LowerSha256 {
    param([Parameter(Mandatory = $true)][string]$Path)

    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Assert-FileHash {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Expected
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Required RIFE build input is missing: $Path"
    }
    $Actual = Get-LowerSha256 $Path
    if ($Actual -ne $Expected.ToLowerInvariant()) {
        throw "SHA-256 mismatch for ${Path}: expected=$Expected actual=$Actual"
    }
}

Assert-ManagedPath -Path $CacheDir -Root $ManagedCacheRoot -Description "RIFE asset cache"
Assert-ManagedPath -Path $RuntimeDir -Root $ManagedRuntimeRoot -Description "RIFE runtime"
New-Item -ItemType Directory -Path $CacheDir -Force | Out-Null

$AssetName = [System.IO.Path]::GetFileName(([uri]$AssetUrl).AbsolutePath)
if ($AssetName -ne "MediaStationGo-frame-interpolation-runtime-x64-v1.zip") {
    throw "Unexpected RIFE build asset name: $AssetName"
}
$ArchivePath = Join-Path $CacheDir $AssetName
if (Test-Path -LiteralPath $ArchivePath -PathType Leaf) {
    Assert-FileHash -Path $ArchivePath -Expected $AssetSha256
    Write-Host "Using verified cached RIFE build asset: $ArchivePath" -ForegroundColor Cyan
} else {
    $Curl = Get-Command curl.exe -ErrorAction SilentlyContinue
    if (-not $Curl) {
        throw "curl.exe is required to download the RIFE build asset"
    }
    $PartialPath = "$ArchivePath.part"
    if (Test-Path -LiteralPath $PartialPath) {
        [System.IO.File]::Delete($PartialPath)
    }
    & $Curl.Source --fail --location --retry 3 --retry-delay 3 --retry-all-errors `
        --output $PartialPath $AssetUrl
    if ($LASTEXITCODE -ne 0) {
        if (Test-Path -LiteralPath $PartialPath) {
            [System.IO.File]::Delete($PartialPath)
        }
        throw "Failed to download the pinned RIFE build asset: $AssetUrl"
    }
    Assert-FileHash -Path $PartialPath -Expected $AssetSha256
    Move-Item -LiteralPath $PartialPath -Destination $ArchivePath
}

$ExtractRoot = Join-Path $CacheDir ("extract-" + [guid]::NewGuid().ToString("N"))
Assert-ManagedPath -Path $ExtractRoot -Root $CacheDir -Description "RIFE extraction directory"
New-Item -ItemType Directory -Path $ExtractRoot | Out-Null
try {
    Expand-Archive -LiteralPath $ArchivePath -DestinationPath $ExtractRoot
    $StagedRuntime = Join-Path $ExtractRoot "frame-interpolation-runtime"
    if (-not (Test-Path -LiteralPath $StagedRuntime -PathType Container)) {
        throw "RIFE build asset is missing its runtime root"
    }

    $ManifestPath = Join-Path $StagedRuntime "ASSET-SHA256SUMS.txt"
    if (-not (Test-Path -LiteralPath $ManifestPath -PathType Leaf)) {
        throw "RIFE build asset is missing ASSET-SHA256SUMS.txt"
    }
    $StagedPrefix = [System.IO.Path]::GetFullPath($StagedRuntime).TrimEnd('\') + '\'
    $ManifestEntries = 0
    foreach ($Line in Get-Content -LiteralPath $ManifestPath) {
        if ($Line -notmatch '^([0-9a-f]{64})  (.+)$') {
            continue
        }
        $ExpectedHash = $matches[1]
        $RelativePath = $matches[2].Replace('/', '\')
        $Candidate = [System.IO.Path]::GetFullPath((Join-Path $StagedRuntime $RelativePath))
        if (-not $Candidate.StartsWith(
                $StagedPrefix,
                [System.StringComparison]::OrdinalIgnoreCase)) {
            throw "RIFE asset manifest path escapes its runtime root: $RelativePath"
        }
        Assert-FileHash -Path $Candidate -Expected $ExpectedHash
        $ManifestEntries++
    }
    if ($ManifestEntries -ne 26) {
        throw "Unexpected RIFE asset manifest entry count: $ManifestEntries"
    }

    $PinnedHashes = [ordered]@{
        "bin\cudart64_12.dll" = "c2c9a9c22a9bcba90e261825968836787b331038047a26770cffb7a583c28344"
        ".tensorrt\TensorRT-RTX-1.4.0.76\bin\tensorrt_rtx_1_4.dll" = "b085ead3d11f28c6bb204f3614ffe7ec540f3cea6350e03114ad3324040614d6"
        ".tensorrt\TensorRT-RTX-1.4.0.76\bin\tensorrt_onnxparser_rtx_1_4.dll" = "bfd3297d666ff55fac9df7af1dd183952f6c515417b50d56c468b76e62e74e20"
        ".tensorrt\TensorRT-RTX-1.4.0.76\bin\tensorrt_rtx.exe" = "c6c1b9781e887d4bc9d5833932d0041ada3681c5dbf1bcad88274077d33f4afa"
        "vapoursynth\plugins\models\rife\rife_v4.26_fp16_io.onnx" = "534aeae1a47bd7585defc6902fd6135b71545f85522626197eb109888ab7dfc2"
        "vapoursynth\plugins\models\rife\rife_v4.26_scale0.5.onnx" = "212696b5befa040ab1989003dcc89b90903fbbdce21f46e0883cd5ab1bf91ca4"
        "vapoursynth\plugins\models\rife\rife_v4.25_lite_fp16_io.onnx" = "9b9209ebce65b666c1f24ba9ee203bcacb1b3054b0bcc7617fbea7b6736a19b4"
    }
    foreach ($Entry in $PinnedHashes.GetEnumerator()) {
        Assert-FileHash -Path (Join-Path $StagedRuntime $Entry.Key) -Expected $Entry.Value
    }

    $TensorRtBin = Join-Path $StagedRuntime ".tensorrt\TensorRT-RTX-1.4.0.76\bin"
    $RuntimeBin = Join-Path $StagedRuntime "bin"
    foreach ($Name in @("tensorrt_rtx.exe", "tensorrt_onnxparser_rtx_1_4.dll")) {
        Copy-Item -LiteralPath (Join-Path $TensorRtBin $Name) `
            -Destination (Join-Path $RuntimeBin $Name) -Force
    }
    Assert-FileHash -Path (Join-Path $RuntimeBin "tensorrt_rtx.exe") `
        -Expected $PinnedHashes[".tensorrt\\TensorRT-RTX-1.4.0.76\\bin\\tensorrt_rtx.exe"]
    Assert-FileHash -Path (Join-Path $RuntimeBin "tensorrt_onnxparser_rtx_1_4.dll") `
        -Expected $PinnedHashes[".tensorrt\\TensorRT-RTX-1.4.0.76\\bin\\tensorrt_onnxparser_rtx_1_4.dll"]

    if (Test-Path -LiteralPath $RuntimeDir) {
        Remove-Item -LiteralPath $RuntimeDir -Recurse -Force
    }
    Move-Item -LiteralPath $StagedRuntime -Destination $RuntimeDir
} finally {
    if (Test-Path -LiteralPath $ExtractRoot) {
        Remove-Item -LiteralPath $ExtractRoot -Recurse -Force
    }
}

Write-Host "PUBLIC_RIFE_RUNTIME_READY version=x64-v1 runtime=$RuntimeDir assetSha256=$AssetSha256" `
    -ForegroundColor Green
