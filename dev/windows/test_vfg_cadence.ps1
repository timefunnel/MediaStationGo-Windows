param(
    [ValidateSet('1920x1080', '3840x2160')][string]$SourceSize = '1920x1080',
    [ValidateRange(1, 600)][int]$Seconds = 60,
    [ValidateRange(0, 2)][int]$Quality = 0,
    [ValidateSet('audio', 'display-vdrop')][string]$VideoSync = 'audio',
    [string]$ResultDirectory = '',
    [string]$PresentMonPath = '',
    [switch]$ExerciseControls,
    [switch]$ElevatePresentationCapture,
    [ValidateSet('24000/1001', '25')][string]$SourceRate = '24000/1001'
)
$ErrorActionPreference = 'Stop'
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
. (Join-Path $PSScriptRoot 'env.ps1')
$BuildRoot = [IO.Path]::GetFullPath((Join-Path $RepoRoot 'build')) + '\'
$TestRoot = [IO.Path]::GetFullPath((Join-Path $BuildRoot ('vfg-cadence-test-' + [Guid]::NewGuid().ToString('N'))))
if (-not $ResultDirectory) {
    $ResultDirectory = Join-Path $BuildRoot ('vfg-cadence-' + $SourceSize + '-' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
}
$ResultDirectory = [IO.Path]::GetFullPath($ResultDirectory)
if (-not $TestRoot.StartsWith($BuildRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Unexpected test directory' }
if (Test-Path -LiteralPath $ResultDirectory) { throw "Result directory already exists: $ResultDirectory" }
New-Item -ItemType Directory -Path $ResultDirectory | Out-Null
New-Item -ItemType Directory -Path $TestRoot | Out-Null
$Mpv = Join-Path $RepoRoot 'third_party\mpv-install'
$Bridge = Join-Path $RepoRoot 'build\video-enhancement\video_enhancement.dll'
$Runtime = Join-Path $RepoRoot 'third_party\video-enhancement-deps\Veyra-2.0.6-NVIDIA\runtime\nvidia-vfg'
$Capture = $null
try {
    & cl.exe /nologo /MD /std:c++17 /EHsc /W4 /WX "/I$Mpv\include" (Join-Path $PSScriptRoot 'mpv\test_vfg_cadence.cpp') "/Fo$TestRoot\test_vfg_cadence.obj" "/Fe$TestRoot\vfgbench.exe" /link "$Mpv\lib\mpv.lib" user32.lib
    if ($LASTEXITCODE -ne 0) { throw 'VFG cadence benchmark compilation failed' }
    $Dimensions = $SourceSize.Split('x')
    $CanvasWidth = 2 * [int]$Dimensions[0]
    $ClipSeconds = $Seconds + 7
    $Subtitle = Join-Path $TestRoot 'cadence.srt'
    $SubtitleEnd = [TimeSpan]::FromSeconds($ClipSeconds).ToString('hh\:mm\:ss') + ',000'
    [IO.File]::WriteAllText($Subtitle, "1`n00:00:00,000 --> $SubtitleEnd`nVFG cadence comparison - original subtitle timeline`n", [Text.UTF8Encoding]::new($false))
    [IO.File]::WriteAllText((Join-Path $TestRoot 'cadence-timed.srt'), "1`n00:00:01,300 --> 00:00:02,100`nVFG timed subtitle A`n`n2`n00:00:02,300 --> 00:00:03,800`nVFG timed subtitle B`n", [Text.UTF8Encoding]::new($false))
    $Pan = "color=c=0x283040:size=${CanvasWidth}x$($Dimensions[1]):rate=${SourceRate}:duration=$ClipSeconds,drawgrid=w=128:h=128:t=3:c=white,drawbox=x=500:y=200:w=500:h=500:c=0xdc5030:t=fill,crop=w=$($Dimensions[0]):h=$($Dimensions[1]):x='(in_w-out_w)*(0.5-0.5*cos(2*PI*t/20))':y=0"
    $Source = Join-Path $TestRoot 'pan.mp4'
    & ffmpeg.exe -hide_banner -loglevel error -f lavfi -i $Pan -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=$ClipSeconds" -i $Subtitle -map 0:v -map 1:a -map 1:a -map 2:s -c:v libx264 -preset ultrafast -crf 18 -pix_fmt yuv420p -c:a aac -c:s mov_text -t $ClipSeconds $Source
    if ($LASTEXITCODE -ne 0) { throw 'Horizontal-pan fixture generation failed' }
    $Manifest = [ordered]@{
        sourceSize = $SourceSize; sourceRate = $SourceRate; seconds = $Seconds; warmupSeconds = 5
        quality = $Quality; videoSync = $VideoSync; divisors = @(3, 2); audio = 'wasapi-muted'
        sourceSha256 = (Get-FileHash -LiteralPath $Source -Algorithm SHA256).Hash
        mpvSha256 = (Get-FileHash -LiteralPath "$Mpv\lib\libmpv-2.dll" -Algorithm SHA256).Hash
        bridgeSha256 = (Get-FileHash -LiteralPath $Bridge -Algorithm SHA256).Hash
        presentationCapture = [bool]$PresentMonPath
    }
    if ($PresentMonPath) {
        $PresentMonPath = (Resolve-Path -LiteralPath $PresentMonPath).Path
        $Manifest.presentMonSha256 = (Get-FileHash -LiteralPath $PresentMonPath -Algorithm SHA256).Hash
        # Match both benchmark hard deadlines, including SDK/renderer startup.
        $CaptureArguments = @('--process_name', 'vfgbench.exe', '--output_file', (Join-Path $ResultDirectory 'presentation.csv'), '--timed', ((($Seconds + 50) * 2) + 5).ToString(), '--terminate_after_timed', '--no_console_stats', '--v2_metrics', '--qpc_time_ms', '--session_name', ('VFG-Cadence-' + [Guid]::NewGuid().ToString('N')))
        $CaptureConfig = Join-Path $ResultDirectory 'capture-config.json'
        [ordered]@{ executable=$PresentMonPath; arguments=$CaptureArguments; resultDirectory=$ResultDirectory } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $CaptureConfig -Encoding utf8
        # Elevate only the capture helper. Keep its effective arguments, console
        # output and exit status even when no presentation events reach the CSV.
        $CaptureHelper = Join-Path $TestRoot 'capture.ps1'
        @'
param([Parameter(Mandatory)][string]$Config)
$ErrorActionPreference = 'Stop'
$Settings = Get-Content -LiteralPath $Config -Raw | ConvertFrom-Json
$Status = [ordered]@{ elevated=[Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator); exitCode=$null; error=$null }
try {
    $CaptureArguments = [string[]]$Settings.arguments
    & $Settings.executable @CaptureArguments *> (Join-Path $Settings.resultDirectory 'presentmon-console.log')
    $Status.exitCode = $LASTEXITCODE
} catch {
    $Status.error = $_.ToString()
} finally {
    $Status | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $Settings.resultDirectory 'capture-status.json') -Encoding utf8
}
if ($Status.error) { exit 1 }
exit $Status.exitCode
'@ | Set-Content -LiteralPath $CaptureHelper -Encoding utf8
        $Launch = @{FilePath=(Get-Process -Id $PID).Path; ArgumentList=@('-NoProfile', '-NonInteractive', '-File', ('"' + $CaptureHelper + '"'), '-Config', ('"' + $CaptureConfig + '"')); WindowStyle='Hidden'; PassThru=$true}
        if ($ElevatePresentationCapture) { $Launch.Verb = 'RunAs' }
        $Capture = Start-Process @Launch
        Start-Sleep -Seconds 2
        if ($Capture.HasExited) { throw "PresentMon exited before measurement: $($Capture.ExitCode)" }
    } elseif ($ElevatePresentationCapture) {
        throw 'ElevatePresentationCapture requires an explicit PresentMonPath'
    }
    $Manifest | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $ResultDirectory 'manifest.json') -Encoding utf8
    foreach ($Divisor in @(3, 2)) {
        $Prefix = Join-Path $ResultDirectory "divisor-$Divisor"
        Write-Output "VFG_CADENCE_BEGIN size=$SourceSize divisor=$Divisor quality=$Quality seconds=$Seconds sync=$VideoSync"
        & (Join-Path $TestRoot 'vfgbench.exe') $Source $Bridge $Runtime $TestRoot $Divisor $Quality $Seconds $Prefix $VideoSync ([int]$ExerciseControls.IsPresent) *> "$Prefix.log"
        if ($LASTEXITCODE -ne 0) { throw "VFG cadence measurement failed: $Prefix.log" }
        Get-Content -LiteralPath "$Prefix-summary.json"
    }
    if ($Capture) {
        $Capture.WaitForExit()
        $CaptureStatus = Get-Content -LiteralPath (Join-Path $ResultDirectory 'capture-status.json') -Raw | ConvertFrom-Json
        if ($Capture.ExitCode -ne 0 -or $CaptureStatus.exitCode -ne 0 -or $CaptureStatus.error) { throw "PresentMon failed; inspect capture-status.json and presentmon-console.log in $ResultDirectory" }
        if (-not (Test-Path -LiteralPath (Join-Path $ResultDirectory 'presentation.csv'))) { throw "PresentMon did not produce presentation.csv; inspect presentmon-console.log in $ResultDirectory" }
    }
    Write-Output "VFG_CADENCE_COMPLETE results=$ResultDirectory"
} finally {
    if ($Capture -and -not $Capture.HasExited) { $Capture.WaitForExit() }
    $Absolute = [IO.Path]::GetFullPath($TestRoot)
    if (-not $Absolute.StartsWith($BuildRoot, [StringComparison]::OrdinalIgnoreCase)) { throw "Unexpected cleanup target: $Absolute" }
    if (Test-Path -LiteralPath $Absolute) { Remove-Item -LiteralPath $Absolute -Recurse -Force }
}
