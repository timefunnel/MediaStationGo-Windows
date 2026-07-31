param(
    [Parameter(Mandatory = $true)]
    [string]$OutputLibDir
)

$ErrorActionPreference = "Stop"

$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
$RuntimeDir = Join-Path $RepoRoot "third_party\frame-interpolation-runtime"
$TensorRtVersion = "1.4.0.76"
$RuntimeAbi = 4
$EngineSpecs = @(
    @{ Width = 1920; Height = 1080 },
    @{ Width = 2304; Height = 1296 },
    @{ Width = 2560; Height = 1440 },
    @{ Width = 3840; Height = 2160 }
)
$Models = @(
    @{
        Id = "rife-v4.26"
        Name = "RIFE v4.26"
        ModelPath = Join-Path $RuntimeDir "vapoursynth\plugins\models\rife\rife_v4.26.onnx"
        EnginePrefix = "poc-rife-v4_26-impl1"
    },
    @{
        Id = "rife-v4.25-lite"
        Name = "RIFE v4.25 Lite"
        ModelPath = Join-Path $RuntimeDir "vapoursynth\plugins\models\rife\rife_v4.25_lite.onnx"
        EnginePrefix = "poc-rife-v4_25-lite-impl1"
    }
)

function Get-LowerSha256 {
    param([Parameter(Mandatory = $true)][string]$Path)
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Get-TextSha256 {
    param([Parameter(Mandatory = $true)][string]$Value)
    $Hasher = [System.Security.Cryptography.SHA256]::Create()
    try {
        $Bytes = [System.Text.Encoding]::UTF8.GetBytes($Value)
        ([System.BitConverter]::ToString($Hasher.ComputeHash($Bytes)) -replace '-', '').ToLowerInvariant()
    } finally {
        $Hasher.Dispose()
    }
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
foreach ($Model in $Models) {
    if (-not (Test-Path -LiteralPath $Model.ModelPath -PathType Leaf)) {
        throw "Required RIFE model is missing: $($Model.ModelPath)"
    }
}

$GpuOutput = @(& nvidia-smi --query-gpu=name,uuid,driver_version --format=csv,noheader,nounits 2>&1)
$GpuExitCode = $LASTEXITCODE
$GpuLine = $GpuOutput | Select-Object -First 1
if ($GpuExitCode -ne 0 -or -not $GpuLine) {
    $GpuDiagnostic = ($GpuOutput | Out-String).Trim()
    throw "nvidia-smi could not provide the GPU identity for the RIFE engine cache (exit $GpuExitCode): $GpuDiagnostic"
}
$GpuFields = @($GpuLine.ToString().Split(',') | ForEach-Object { $_.Trim() })
if ($GpuFields.Count -ne 3 -or
    @($GpuFields | Where-Object { [string]::IsNullOrWhiteSpace($_) }).Count -gt 0) {
    throw "nvidia-smi returned an invalid GPU identity: $GpuLine"
}
$GpuName, $GpuUuid, $DriverVersion = $GpuFields

$EngineCacheDir = Join-Path $OutputLibDir "frame-interpolation\engine-cache"
New-Item -ItemType Directory -Path $EngineCacheDir -Force | Out-Null
$ModelEntries = @()
$ExpectedEngineFiles = [System.Collections.Generic.HashSet[string]]::new(
    [System.StringComparer]::OrdinalIgnoreCase)
foreach ($Model in $Models) {
    $ModelSha256 = Get-LowerSha256 $Model.ModelPath
    $EngineEntries = @()
    foreach ($Spec in $EngineSpecs) {
        $Width = $Spec.Width
        $Height = $Spec.Height
        $SourceDir = Join-Path $RuntimeDir (
            "engines\$($Model.EnginePrefix)-${Width}x${Height}-scale1_0-fp16")
        $Candidates = @(Get-ChildItem -LiteralPath $SourceDir -Filter "*.engine" -File -ErrorAction SilentlyContinue)
        if ($Candidates.Count -ne 1) {
            throw "Expected exactly one validated $($Model.Name) engine in $SourceDir, found $($Candidates.Count)"
        }
        $KeyMaterial = "gpu_uuid=$GpuUuid`ndriver=$DriverVersion`ntensorrt=$TensorRtVersion`nmodel_sha256=$ModelSha256`nwidth=$Width`nheight=$Height`nscale=1.0`nprecision=fp16"
        $EngineKey = Get-TextSha256 $KeyMaterial
        $EngineFile = "$EngineKey.engine"
        $Destination = Join-Path $EngineCacheDir $EngineFile
        Copy-Item -LiteralPath $Candidates[0].FullName -Destination $Destination -Force
        [void]$ExpectedEngineFiles.Add($EngineFile)
        $EngineEntries += [ordered]@{
            width = $Width
            height = $Height
            scale = "1.0"
            precision = "fp16"
            engineKey = $EngineKey
            file = $EngineFile
            sha256 = Get-LowerSha256 $Destination
        }
    }
    $ModelEntries += [ordered]@{
        id = $Model.Id
        name = $Model.Name
        modelSha256 = $ModelSha256
        engines = $EngineEntries
    }
}

Get-ChildItem -LiteralPath $EngineCacheDir -Filter "*.engine" -File | Where-Object {
    -not $ExpectedEngineFiles.Contains($_.Name)
} | Remove-Item -Force

$Manifest = [ordered]@{
    schema = 2
    gpuName = $GpuName
    gpuUuid = $GpuUuid
    driverVersion = $DriverVersion
    tensorRtVersion = $TensorRtVersion
    runtimeAbi = $RuntimeAbi
    runtimeDll = "rife_runtime.dll"
    cudaRuntimeDll = "cudart64_12.dll"
    tensorRtDll = "tensorrt_rtx_1_4.dll"
    models = $ModelEntries
}
$ManifestPath = Join-Path $OutputLibDir "frame-interpolation\runtime-manifest.json"
$Utf8NoBom = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllText(
    $ManifestPath,
    ($Manifest | ConvertTo-Json -Depth 8),
    $Utf8NoBom)

$EngineCount = ($ModelEntries | ForEach-Object { $_.engines.Count } | Measure-Object -Sum).Sum
Write-Host "Staged $EngineCount keyed RIFE engines across $($ModelEntries.Count) models" -ForegroundColor Green
