param(
    [string]$RuntimeDir,
    [ValidateSet(60, 90, 120)]
    [int]$TargetFps = 60,
    [ValidateRange(320, 7680)]
    [int]$Width = 1920,
    [ValidateRange(240, 4320)]
    [int]$Height = 1080,
    [ValidateRange(0, 7680)]
    [int]$InferenceWidth = 0,
    [ValidateRange(0, 4320)]
    [int]$InferenceHeight = 0,
    [ValidateSet(0.5, 1.0)]
    [double]$Scale = 1.0,
    [ValidateSet(1, 2)]
    [int]$Streams = 1,
    [ValidateSet("rgb", "yuv10")]
    [string]$Pipeline = "rgb",
    [ValidateRange(1, 36000)]
    [int]$Frames = 60,
    [ValidateRange(0.1, 4.0)]
    [double]$MinimumRealtimeRatio = 1.0
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
if (-not $RuntimeDir) {
    $RuntimeDir = Join-Path $RepoRoot "third_party\frame-interpolation-runtime"
}
$RuntimeDir = (Resolve-Path -LiteralPath $RuntimeDir).Path
$InferenceWidth = if ($InferenceWidth -gt 0) { $InferenceWidth } else { $Width }
$InferenceHeight = if ($InferenceHeight -gt 0) { $InferenceHeight } else { $Height }
if ($InferenceWidth -gt $Width -or $InferenceHeight -gt $Height) {
    throw "Inference dimensions cannot exceed source dimensions"
}
$ManifestPath = Join-Path $RuntimeDir "runtime-manifest.json"
if (-not (Test-Path $ManifestPath)) {
    throw "Frame interpolation runtime is incomplete: $ManifestPath is missing"
}

$BinDir = Join-Path $RuntimeDir "bin"
$PythonLibDir = Join-Path $RuntimeDir "lib\python3.14"
$PluginDir = Join-Path $RuntimeDir "vapoursynth\plugins"
$ScriptDir = Join-Path $RuntimeDir "scripts"
$EngineRoot = Join-Path $RuntimeDir "engines"
$ScaleKey = $Scale.ToString("0.0", [System.Globalization.CultureInfo]::InvariantCulture).Replace(".", "_")
$EngineDir = Join-Path $EngineRoot "poc-rife-v4_25-lite-${InferenceWidth}x${InferenceHeight}-scale${ScaleKey}-fp16"
$Vspipe = Join-Path $BinDir "vspipe.exe"
$PocScript = Join-Path $PSScriptRoot "rife_poc.vpy"
$ModelPath = Join-Path $PluginDir "models\rife\rife_v4.25_lite.onnx"
foreach ($Required in @($Vspipe, (Join-Path $BinDir "VSScript.dll"), (Join-Path $PluginDir "vstrt_rtx.dll"), (Join-Path $ScriptDir "vsmlrt.py"), $ModelPath)) {
    if (-not (Test-Path $Required)) {
        throw "Frame interpolation dependency is missing: $Required"
    }
}
New-Item -ItemType Directory -Path $EngineDir -Force | Out-Null

$env:PATH = "$BinDir;$env:PATH"
$env:PYTHONHOME = $RuntimeDir
$env:PYTHONPATH = "$PythonLibDir;$($PythonLibDir)\site-packages;$ScriptDir"
$env:VSSCRIPT_PATH = Join-Path $BinDir "VSScript.dll"
$env:VAPOURSYNTH_PLUGIN_PATH = $PluginDir
$env:MSGO_VSMLRT_PLUGIN = Join-Path $PluginDir "vstrt_rtx.dll"
$env:MSGO_RIFE_ENGINE_DIR = $EngineDir
$env:MSGO_RIFE_TARGET_FPS = [string]$TargetFps
$env:MSGO_RIFE_SOURCE_FPS = "24"
$env:MSGO_RIFE_SOURCE_FRAMES = [string][Math]::Max(48, [Math]::Ceiling($Frames * 24 / $TargetFps) + 2)
$env:MSGO_RIFE_WIDTH = [string]$Width
$env:MSGO_RIFE_HEIGHT = [string]$Height
$env:MSGO_RIFE_INFERENCE_WIDTH = [string]$InferenceWidth
$env:MSGO_RIFE_INFERENCE_HEIGHT = [string]$InferenceHeight
$env:MSGO_RIFE_SCALE = $Scale.ToString([System.Globalization.CultureInfo]::InvariantCulture)
$env:MSGO_RIFE_STREAMS = [string]$Streams
$env:MSGO_RIFE_PIPELINE = $Pipeline

$ExistingEngines = @(Get-ChildItem -LiteralPath $EngineDir -Filter "*.engine" -File -ErrorAction SilentlyContinue)
if ($ExistingEngines.Count -eq 0) {
    Write-Host "FRAME_INTERPOLATION_ENGINE_BUILDING input=${Width}x${Height} inference=${InferenceWidth}x${InferenceHeight} scale=$Scale streams=$Streams pipeline=$Pipeline target=${TargetFps}fps backend=TRT_RTX model=RIFE-v4.25-lite" -ForegroundColor Yellow
} else {
    Write-Host "FRAME_INTERPOLATION_ENGINE_CACHE_PRESENT count=$($ExistingEngines.Count)" -ForegroundColor Cyan
}

$PreviousErrorActionPreference = $ErrorActionPreference
$ErrorActionPreference = "Continue"
$Info = & $Vspipe --info $PocScript - 2>&1 | ForEach-Object { $_.ToString() }
$InfoExitCode = $LASTEXITCODE
$ErrorActionPreference = $PreviousErrorActionPreference
if ($InfoExitCode -ne 0) {
    $Info | ForEach-Object { Write-Host $_ -ForegroundColor Red }
    throw "RIFE PoC graph initialization failed"
}
$InfoText = $Info -join "`n"
if ($InfoText -notmatch "(?im)^FPS:\s+$TargetFps(?:/1)?\s*\(") {
    throw "RIFE PoC reported an unexpected output FPS. Expected $TargetFps.`n$InfoText"
}

$EndFrame = $Frames - 1
$Started = Get-Date
$ErrorActionPreference = "Continue"
$ProcessingOutput = & $Vspipe --progress --filter-time --end $EndFrame $PocScript NUL 2>&1 | ForEach-Object { $_.ToString() }
$ProcessingExitCode = $LASTEXITCODE
$ErrorActionPreference = $PreviousErrorActionPreference
$ProcessingOutput | ForEach-Object { Write-Host $_ }
if ($ProcessingExitCode -ne 0) {
    throw "RIFE PoC frame processing failed"
}
$Elapsed = (Get-Date) - $Started
$ProcessingText = $ProcessingOutput -join "`n"
if ($ProcessingText -notmatch "Output\s+\d+\s+frames\s+in\s+[\d.]+\s+seconds\s+\(([\d.]+)\s+fps\)") {
    throw "vspipe did not report the actual filter throughput"
}
$FilterFps = [double]::Parse($matches[1], [System.Globalization.CultureInfo]::InvariantCulture)
$MinimumFilterFps = $TargetFps * $MinimumRealtimeRatio
if ($FilterFps -lt $MinimumFilterFps) {
    throw ("RIFE PoC throughput {0:N2} FPS is below required {1:N2} FPS ({2:N2}x realtime)" -f $FilterFps, $MinimumFilterFps, $MinimumRealtimeRatio)
}
$Engines = @(Get-ChildItem -LiteralPath $EngineDir -Filter "*.engine" -File -ErrorAction SilentlyContinue)
if ($Engines.Count -eq 0) {
    throw "TensorRT-RTX did not produce an engine cache"
}

Write-Host ("FRAME_INTERPOLATION_POC_OK input={0}x{1} inference={2}x{3} scale={4:N1} streams={5} pipeline={6} output={7}fps frames={8} wall={9:N2}s filter={10:N2}fps realtime={11:N2}x engines={12}" -f $Width, $Height, $InferenceWidth, $InferenceHeight, $Scale, $Streams, $Pipeline, $TargetFps, $Frames, $Elapsed.TotalSeconds, $FilterFps, ($FilterFps / $TargetFps), $Engines.Count) -ForegroundColor Green
