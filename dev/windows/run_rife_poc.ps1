param(
    [string]$RuntimeDir,
    [string]$TargetFps = "48",
    [string]$SourceFps = "24",
    [switch]$StrictDouble,
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
    [ValidateSet(1, 2)]
    [int]$Implementation = 1,
    [ValidateSet("fp16", "fp32")]
    [string]$Precision = "fp16",
    [ValidateRange(0, 64)]
    [int]$Requests = 0,
    [ValidateSet("rgb", "yuv10")]
    [string]$Pipeline = "rgb",
    [switch]$WriteRawFrames,
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
if ($Implementation -eq 2 -and $Precision -ne "fp32") {
    throw "Implementation 2 requires Precision=fp32 because TensorRT-RTX rejects its converted FP16 graph"
}
function ConvertTo-FpsRational {
    param(
        [string]$Value,
        [string]$Name
    )

    $Match = [regex]::Match(
        $Value,
        '^(?<numerator>[1-9][0-9]{0,8})(?:/(?<denominator>[1-9][0-9]{0,8}))?$')
    if (-not $Match.Success) {
        throw "$Name must be a positive integer or rational such as 24000/1001"
    }
    $Numerator = [long]::Parse(
        $Match.Groups['numerator'].Value,
        [System.Globalization.CultureInfo]::InvariantCulture)
    $Denominator = if ($Match.Groups['denominator'].Success) {
        [long]::Parse(
            $Match.Groups['denominator'].Value,
            [System.Globalization.CultureInfo]::InvariantCulture)
    } else {
        1L
    }
    $Left = $Numerator
    $Right = $Denominator
    while ($Right -ne 0) {
        $Remainder = $Left % $Right
        $Left = $Right
        $Right = $Remainder
    }
    [pscustomobject]@{
        Numerator = [long]($Numerator / $Left)
        Denominator = [long]($Denominator / $Left)
        Value = [double]$Numerator / $Denominator
    }
}

$SourceRate = ConvertTo-FpsRational -Value $SourceFps -Name "SourceFps"
$TargetRate = ConvertTo-FpsRational -Value $TargetFps -Name "TargetFps"
if ($TargetRate.Value -gt 240.0) {
    throw "TargetFps must not exceed 240"
}
if ($TargetRate.Value -le $SourceRate.Value) {
    throw "TargetFps must be greater than SourceFps"
}
if ($StrictDouble -and
    $TargetRate.Numerator * $SourceRate.Denominator -ne
        2 * $SourceRate.Numerator * $TargetRate.Denominator) {
    throw "StrictDouble requires TargetFps to equal exactly twice SourceFps"
}
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
$EngineDir = Join-Path $EngineRoot "poc-rife-v4_26-impl${Implementation}-${InferenceWidth}x${InferenceHeight}-scale${ScaleKey}-${Precision}"
$Vspipe = Join-Path $BinDir "vspipe.exe"
$PocScript = Join-Path $PSScriptRoot "rife_poc.vpy"
$ModelPath = Join-Path $PluginDir "models\rife\rife_v4.26.onnx"
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
$env:MSGO_RIFE_SOURCE_FPS = $SourceFps
$env:MSGO_RIFE_SOURCE_FRAMES = [string][Math]::Max(
    48,
    [Math]::Ceiling($Frames * $SourceRate.Value / $TargetRate.Value) + 2)
$env:MSGO_RIFE_WIDTH = [string]$Width
$env:MSGO_RIFE_HEIGHT = [string]$Height
$env:MSGO_RIFE_INFERENCE_WIDTH = [string]$InferenceWidth
$env:MSGO_RIFE_INFERENCE_HEIGHT = [string]$InferenceHeight
$env:MSGO_RIFE_SCALE = $Scale.ToString([System.Globalization.CultureInfo]::InvariantCulture)
$env:MSGO_RIFE_STREAMS = [string]$Streams
$env:MSGO_RIFE_IMPLEMENTATION = [string]$Implementation
$env:MSGO_RIFE_PRECISION = $Precision
$env:MSGO_RIFE_PIPELINE = $Pipeline

$ExistingEngines = @(Get-ChildItem -LiteralPath $EngineDir -Filter "*.engine" -File -ErrorAction SilentlyContinue)
if ($ExistingEngines.Count -eq 0) {
    Write-Host "FRAME_INTERPOLATION_ENGINE_BUILDING input=${Width}x${Height} inference=${InferenceWidth}x${InferenceHeight} scale=$Scale streams=$Streams implementation=$Implementation precision=$Precision pipeline=$Pipeline source=${SourceFps}fps target=${TargetFps}fps backend=TRT_RTX model=RIFE-v4.26" -ForegroundColor Yellow
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
$TargetFpsInfoPattern = if ($TargetRate.Denominator -eq 1) {
    "$($TargetRate.Numerator)(?:/1)?"
} else {
    "$($TargetRate.Numerator)/$($TargetRate.Denominator)"
}
if ($InfoText -notmatch "(?im)^FPS:\s+$TargetFpsInfoPattern\s*\(") {
    throw "RIFE PoC reported an unexpected output FPS. Expected $TargetFps.`n$InfoText"
}

$EndFrame = $Frames - 1
$Started = Get-Date
$ErrorActionPreference = "Continue"
$VspipeArguments = @("--progress", "--filter-time", "--end", $EndFrame)
if ($Requests -gt 0) {
    $VspipeArguments += @("--requests", $Requests)
}
$VspipeArguments += $PocScript
$VspipeArguments += if ($WriteRawFrames) { "NUL" } else { "--" }
$ProcessingOutput = & $Vspipe @VspipeArguments 2>&1 | ForEach-Object { $_.ToString() }
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
$MinimumFilterFps = $TargetRate.Value * $MinimumRealtimeRatio
if ($FilterFps -lt $MinimumFilterFps) {
    throw ("RIFE PoC throughput {0:N2} FPS is below required {1:N2} FPS ({2:N2}x realtime)" -f $FilterFps, $MinimumFilterFps, $MinimumRealtimeRatio)
}
$Engines = @(Get-ChildItem -LiteralPath $EngineDir -Filter "*.engine" -File -ErrorAction SilentlyContinue)
if ($Engines.Count -eq 0) {
    throw "TensorRT-RTX did not produce an engine cache"
}

Write-Host ("FRAME_INTERPOLATION_POC_OK input={0}x{1} inference={2}x{3} scale={4:N1} streams={5} implementation={6} precision={7} requests={8} pipeline={9} raw_output={10} source={11}fps output={12}fps frames={13} wall={14:N2}s filter={15:N2}fps realtime={16:N2}x engines={17}" -f $Width, $Height, $InferenceWidth, $InferenceHeight, $Scale, $Streams, $Implementation, $Precision, $Requests, $Pipeline, $WriteRawFrames.IsPresent, $SourceFps, $TargetFps, $Frames, $Elapsed.TotalSeconds, $FilterFps, ($FilterFps / $TargetRate.Value), $Engines.Count) -ForegroundColor Green
