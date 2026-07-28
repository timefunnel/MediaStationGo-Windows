param(
    [string]$RuntimeDir,
    [ValidateSet(60, 90, 120)]
    [int]$TargetFps = 60,
    [int]$Frames = 60
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
if (-not $RuntimeDir) {
    $RuntimeDir = Join-Path $RepoRoot "third_party\frame-interpolation-runtime"
}
$RuntimeDir = (Resolve-Path -LiteralPath $RuntimeDir).Path
$ManifestPath = Join-Path $RuntimeDir "runtime-manifest.json"
if (-not (Test-Path $ManifestPath)) {
    throw "Frame interpolation runtime is incomplete: $ManifestPath is missing"
}

$BinDir = Join-Path $RuntimeDir "bin"
$PythonLibDir = Join-Path $RuntimeDir "lib\python3.14"
$PluginDir = Join-Path $RuntimeDir "vapoursynth\plugins"
$ScriptDir = Join-Path $RuntimeDir "scripts"
$EngineDir = Join-Path $RuntimeDir "engines"
$Vspipe = Join-Path $BinDir "vspipe.exe"
$PocScript = Join-Path $PSScriptRoot "rife_poc.vpy"
foreach ($Required in @($Vspipe, (Join-Path $BinDir "VSScript.dll"), (Join-Path $PluginDir "vstrt_rtx.dll"), (Join-Path $ScriptDir "vsmlrt.py"), (Join-Path $PluginDir "models\rife\rife_v4.25_lite.onnx"))) {
    if (-not (Test-Path $Required)) {
        throw "Frame interpolation dependency is missing: $Required"
    }
}

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

$ExistingEngines = @(Get-ChildItem -LiteralPath $EngineDir -Filter "*.engine" -File -ErrorAction SilentlyContinue)
if ($ExistingEngines.Count -eq 0) {
    Write-Host "FRAME_INTERPOLATION_ENGINE_BUILDING target=${TargetFps}fps backend=TRT_RTX model=RIFE-v4.25-lite" -ForegroundColor Yellow
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
$Engines = @(Get-ChildItem -LiteralPath $EngineDir -Filter "*.engine" -File -ErrorAction SilentlyContinue)
if ($Engines.Count -eq 0) {
    throw "TensorRT-RTX did not produce an engine cache"
}

Write-Host ("FRAME_INTERPOLATION_POC_OK output={0}fps frames={1} wall={2:N2}s filter={3:N2}fps engines={4}" -f $TargetFps, $Frames, $Elapsed.TotalSeconds, $FilterFps, $Engines.Count) -ForegroundColor Green
