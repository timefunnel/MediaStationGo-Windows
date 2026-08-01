param(
    [Parameter(Mandatory = $true)]
    [string]$OutputLibDir
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
$RuntimeDir = Join-Path $RepoRoot "third_party\frame-interpolation-runtime"
$TensorRtVersion = "1.4.0.76"
$RuntimeAbi = 7
$RuntimeBin = Join-Path $RuntimeDir "bin"
$SourceModelDir = Join-Path $RuntimeDir "vapoursynth\plugins\models\rife"
$Models = @(
    @{
        Id = "rife-v4.26"
        Name = "RIFE v4.26"
        File = "rife_v4.26_fp16_io.onnx"
        Scale = "1.0"
        Alignment = 64
        Profiles = @(
            @{ Purpose = "universal"; MinWidth = 64; MinHeight = 64; OptWidth = 3840; OptHeight = 2176; MaxWidth = 16384; MaxHeight = 16384 },
            @{ Purpose = "mid-range"; MinWidth = 64; MinHeight = 64; OptWidth = 2560; OptHeight = 1472; MaxWidth = 2560; MaxHeight = 1472 },
            @{ Purpose = "4k-range"; MinWidth = 64; MinHeight = 64; OptWidth = 3840; OptHeight = 2176; MaxWidth = 4096; MaxHeight = 2176 },
            @{ Purpose = "uhd-fixed"; MinWidth = 3840; MinHeight = 2176; OptWidth = 3840; OptHeight = 2176; MaxWidth = 3840; MaxHeight = 2176 }
        )
    },
    @{
        Id = "rife-v4.26-scale0.5"
        Name = "RIFE v4.26 (scale=0.5)"
        File = "rife_v4.26_scale0.5.onnx"
        Scale = "0.5"
        Alignment = 128
        Profiles = @(
            @{ Purpose = "universal"; MinWidth = 128; MinHeight = 128; OptWidth = 3840; OptHeight = 2176; MaxWidth = 16384; MaxHeight = 16384 },
            @{ Purpose = "mid-range"; MinWidth = 128; MinHeight = 128; OptWidth = 2560; OptHeight = 1536; MaxWidth = 2560; MaxHeight = 1536 },
            @{ Purpose = "4k-range"; MinWidth = 128; MinHeight = 128; OptWidth = 3840; OptHeight = 2176; MaxWidth = 4096; MaxHeight = 2176 }
        )
    },
    @{
        Id = "rife-v4.25-lite"
        Name = "RIFE v4.25 Lite"
        File = "rife_v4.25_lite_fp16_io.onnx"
        Scale = "1.0"
        Alignment = 128
        Profiles = @(
            @{ Purpose = "universal"; MinWidth = 128; MinHeight = 128; OptWidth = 3840; OptHeight = 2176; MaxWidth = 16384; MaxHeight = 16384 },
            @{ Purpose = "mid-range"; MinWidth = 128; MinHeight = 128; OptWidth = 2560; OptHeight = 1536; MaxWidth = 2560; MaxHeight = 1536 },
            @{ Purpose = "4k-range"; MinWidth = 128; MinHeight = 128; OptWidth = 3840; OptHeight = 2176; MaxWidth = 4096; MaxHeight = 2176 },
            @{ Purpose = "uhd-fixed"; MinWidth = 3840; MinHeight = 2176; OptWidth = 3840; OptHeight = 2176; MaxWidth = 3840; MaxHeight = 2176 }
        )
    }
)

function Get-LowerSha256 {
    param([Parameter(Mandatory = $true)][string]$Path)
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

$OutputLibDir = [System.IO.Path]::GetFullPath($OutputLibDir)
if (-not (Test-Path -LiteralPath $OutputLibDir -PathType Container)) {
    throw "The mpv output library directory is missing: $OutputLibDir"
}
foreach ($RuntimeFile in @("rife_runtime.dll", "cudart64_12.dll", "tensorrt_rtx_1_4.dll")) {
    $RuntimePath = Join-Path $OutputLibDir $RuntimeFile
    if (-not (Test-Path -LiteralPath $RuntimePath -PathType Leaf)) {
        throw "Required frame interpolation runtime file is missing: $RuntimePath"
    }
}
$BuilderComponents = @(
    "tensorrt_rtx.exe",
    "tensorrt_onnxparser_rtx_1_4.dll"
)
foreach ($Component in $BuilderComponents) {
    $Source = Join-Path $RuntimeBin $Component
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
        throw "Required TensorRT-RTX builder component is missing: $Source"
    }
    Copy-Item -LiteralPath $Source -Destination (Join-Path $OutputLibDir $Component) -Force
}

$StageDir = Join-Path $OutputLibDir "frame-interpolation"
$ModelDir = Join-Path $StageDir "models"
New-Item -ItemType Directory -Path $ModelDir -Force | Out-Null
$LegacyEngineDir = Join-Path $StageDir "engine-cache"
if (Test-Path -LiteralPath $LegacyEngineDir) {
    $ResolvedStage = [System.IO.Path]::GetFullPath($StageDir).TrimEnd('\') + '\'
    $ResolvedLegacy = [System.IO.Path]::GetFullPath($LegacyEngineDir)
    if (-not $ResolvedLegacy.StartsWith(
        $ResolvedStage,
        [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to remove an Engine directory outside the staged runtime: $ResolvedLegacy"
    }
    Remove-Item -LiteralPath $ResolvedLegacy -Recurse -Force
}

$ExpectedModels = [System.Collections.Generic.HashSet[string]]::new(
    [System.StringComparer]::OrdinalIgnoreCase)
$ModelEntries = @()
foreach ($Model in $Models) {
    $Source = Join-Path $SourceModelDir $Model.File
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
        throw "Required dynamic RIFE ONNX is missing: $Source"
    }
    $Destination = Join-Path $ModelDir $Model.File
    Copy-Item -LiteralPath $Source -Destination $Destination -Force
    [void]$ExpectedModels.Add($Model.File)
    $ModelEntries += [ordered]@{
        id = $Model.Id
        name = $Model.Name
        onnxFile = $Model.File
        onnxSha256 = Get-LowerSha256 $Destination
        scale = $Model.Scale
        precision = "fp16"
        shapeAlignment = $Model.Alignment
        profiles = @($Model.Profiles | ForEach-Object {
            [ordered]@{
                purpose = $_.Purpose
                minWidth = $_.MinWidth
                minHeight = $_.MinHeight
                optWidth = $_.OptWidth
                optHeight = $_.OptHeight
                maxWidth = $_.MaxWidth
                maxHeight = $_.MaxHeight
            }
        })
    }
}
Get-ChildItem -LiteralPath $ModelDir -Filter "*.onnx" -File | Where-Object {
    -not $ExpectedModels.Contains($_.Name)
} | Remove-Item -Force

$Manifest = [ordered]@{
    schema = 4
    tensorRtVersion = $TensorRtVersion
    runtimeAbi = $RuntimeAbi
    runtimeDll = "rife_runtime.dll"
    cudaRuntimeDll = "cudart64_12.dll"
    tensorRtDll = "tensorrt_rtx_1_4.dll"
    onnxParserDll = "tensorrt_onnxparser_rtx_1_4.dll"
    engineBuilder = "tensorrt_rtx.exe"
    models = $ModelEntries
}
$ManifestPath = Join-Path $StageDir "runtime-manifest.json"
$Utf8NoBom = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllText(
    $ManifestPath,
    ($Manifest | ConvertTo-Json -Depth 8),
    $Utf8NoBom)

Write-Host "Staged $($ModelEntries.Count) dynamic RIFE ONNX models; device Engines build on demand" -ForegroundColor Green
