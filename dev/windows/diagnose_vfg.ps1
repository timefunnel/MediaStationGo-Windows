param(
    [ValidateSet('Prepare', 'Run', 'Arm')][string]$Action = 'Prepare',
    [Parameter(Mandatory)][string]$ResultDirectory,
    [string]$PresentationDll = ''
)
$ErrorActionPreference = 'Stop'
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
$BuildRoot = [IO.Path]::GetFullPath((Join-Path $RepoRoot 'build')) + '\'
$ResultDirectory = [IO.Path]::GetFullPath($ResultDirectory)
if (-not $ResultDirectory.StartsWith($BuildRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'VFG diagnostic reports must stay inside this workspace build directory' }
$App = Join-Path $RepoRoot 'build\jellium-desktop.exe'
$ActiveBridge = Join-Path $RepoRoot 'build\video_enhancement.dll'
$ActiveMpv = Join-Path $RepoRoot 'build\libmpv-2.dll'
$Proxy = Join-Path $ResultDirectory 'vfg_capture_bridge.dll'
$OriginalBridge = Join-Path $ResultDirectory 'video_enhancement.dll'
$OriginalMpv = Join-Path $ResultDirectory 'original-libmpv-2.dll'
$ManifestPath = Join-Path $ResultDirectory 'diagnostic-manifest.json'
$CaptureDirectory = Join-Path $ResultDirectory 'capture'

function Hash([string]$Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash }
function RunningDiagnosticApp {
    @(Get-Process jellium-desktop -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $App })
}
if ($Action -eq 'Arm') {
    if (-not (Test-Path -LiteralPath (Join-Path $ResultDirectory 'running.json'))) { throw 'The VFG diagnostic player has not started' }
    $Running = Get-Content -LiteralPath (Join-Path $ResultDirectory 'running.json') -Raw | ConvertFrom-Json
    $Player = Get-Process -Id $Running.pid -ErrorAction SilentlyContinue
    if (-not $Player -or $Player.Path -ne $App) { throw 'The VFG diagnostic player has exited' }
    [IO.File]::WriteAllText((Join-Path $CaptureDirectory 'capture.request'), (Get-Date).ToString('O'), [Text.UTF8Encoding]::new($false))
    [IO.File]::WriteAllText((Join-Path $CaptureDirectory 'render.request'), (Get-Date).ToString('O'), [Text.UTF8Encoding]::new($false))
    Write-Output 'Armed the next eight seconds of VFG input, generated, and rendered pixel capture.'
    exit 0
}
if ($Action -eq 'Prepare') {
    if ((RunningDiagnosticApp).Count) { throw 'Close the existing build player before preparing diagnostics' }
    if (-not $PresentationDll) { throw 'PresentationDll must identify a diagnostic build from test_vfg_display.ps1 -BuildOnly -CapturePixels' }
    $MeasurementMpv = [IO.Path]::GetFullPath($PresentationDll)
    if (-not $MeasurementMpv.StartsWith($BuildRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Presentation diagnostics must stay inside the workspace build directory' }
    $MeasurementManifest = Get-Content -LiteralPath (Join-Path (Split-Path -Parent $MeasurementMpv) 'measurement-manifest.json') -Raw | ConvertFrom-Json
    if (-not $MeasurementManifest.capturesRenderedPixels -or
        (Hash $MeasurementMpv) -ne $MeasurementManifest.measurementMpvSha256 -or
        (Hash (Join-Path $PSScriptRoot 'mpv\vf_nvvideo.c')) -ne $MeasurementManifest.filterSourceSha256) {
        throw 'Rebuild presentation diagnostics from the current VFG filter with -BuildOnly -CapturePixels'
    }
    if (Test-Path -LiteralPath $ManifestPath) { throw 'This diagnostic result directory has already been prepared' }
    New-Item -ItemType Directory -Path $ResultDirectory -Force | Out-Null
    New-Item -ItemType Directory -Path $CaptureDirectory -Force | Out-Null
    . (Join-Path $PSScriptRoot 'env.ps1')
    & cl.exe /nologo /LD /MD /std:c++17 /EHsc /W4 /WX (Join-Path $PSScriptRoot 'mpv\vfg_capture_bridge.cpp') "/Fo$ResultDirectory\vfg_capture_bridge.obj" "/Fe$Proxy" /link d3d11.lib d3dcompiler.lib dxgi.lib
    if ($LASTEXITCODE -ne 0) { throw 'VFG capture proxy compilation failed' }
    Copy-Item -LiteralPath $ActiveBridge -Destination $OriginalBridge
    Copy-Item -LiteralPath $ActiveMpv -Destination $OriginalMpv
    # The original NR bridge resolves this sibling adapter by its own DLL path.
    Copy-Item -LiteralPath (Join-Path $RepoRoot 'build\nvngx.dll') -Destination (Join-Path $ResultDirectory 'nvngx.dll')
    [ordered]@{
        preparedAt = (Get-Date).ToString('O'); commit = (git -C $RepoRoot rev-parse HEAD)
        appSha256 = Hash $App; bridgeSha256 = Hash $OriginalBridge; mpvSha256 = Hash $OriginalMpv
        proxySha256 = Hash $Proxy; measurementMpvSha256 = Hash $MeasurementMpv
        measurementMpvPath = $MeasurementMpv; capturesRenderedPixels = $true
        filterSourceSha256 = $MeasurementManifest.filterSourceSha256
        sourceSha256 = Hash (Join-Path $PSScriptRoot 'mpv\vfg_capture_bridge.cpp')
        captureSeconds = 8; captureWidth = 320; captureHeight = 180
        productionInstallationChanged = $false
    } | ConvertTo-Json | Set-Content -LiteralPath $ManifestPath -Encoding utf8
    Write-Output "Prepared VFG diagnostics: $ResultDirectory"
    exit 0
}

$Manifest = Get-Content -LiteralPath $ManifestPath -Raw | ConvertFrom-Json
$MeasurementMpv = $Manifest.measurementMpvPath
if (-not $MeasurementMpv) { throw 'Prepare a new diagnostic directory with the current rendered-pixel capture build' }
if ((RunningDiagnosticApp).Count) { throw 'Close the existing build player before starting VFG diagnostics' }
foreach ($Check in @(
    @($App, $Manifest.appSha256), @($ActiveBridge, $Manifest.bridgeSha256), @($ActiveMpv, $Manifest.mpvSha256),
    @($Proxy, $Manifest.proxySha256), @($OriginalBridge, $Manifest.bridgeSha256), @($OriginalMpv, $Manifest.mpvSha256), @($MeasurementMpv, $Manifest.measurementMpvSha256)
)) { if ((Hash $Check[0]) -ne $Check[1]) { throw "Diagnostic input changed after preparation: $($Check[0])" } }
$BridgeChanged = $false; $MpvChanged = $false; $Process = $null
$Run = [ordered]@{ startedAt = (Get-Date).ToString('O'); exitCode = $null; restored = $false; error = $null }
try {
    Copy-Item -LiteralPath $Proxy -Destination $ActiveBridge; $BridgeChanged = $true
    Copy-Item -LiteralPath $MeasurementMpv -Destination $ActiveMpv; $MpvChanged = $true
    $env:VFG_CAPTURE_BRIDGE = $OriginalBridge
    $env:VFG_CAPTURE_DIRECTORY = $CaptureDirectory
    $env:VFG_CAPTURE_PROXY = $ActiveBridge
    $AppLog = Join-Path $ResultDirectory 'app.log'
    $Arguments = @('--remote-debug-port', '9271', '--log-level', 'info,CEF=debug', '--log-file', ('"' + $AppLog + '"'))
    $Process = Start-Process -FilePath $App -WorkingDirectory (Join-Path $RepoRoot 'build') -ArgumentList $Arguments -RedirectStandardError (Join-Path $ResultDirectory 'native-stderr.log') -RedirectStandardOutput (Join-Path $ResultDirectory 'native-stdout.log') -PassThru
    [ordered]@{ pid = $Process.Id; startedAt = (Get-Date).ToString('O'); port = 9271 } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $ResultDirectory 'running.json') -Encoding utf8
    $Process.WaitForExit(); $Run.exitCode = $Process.ExitCode
} catch {
    $Run.error = $_.ToString(); throw
} finally {
    $Deadline = [DateTime]::UtcNow.AddSeconds(10)
    while ((RunningDiagnosticApp).Count -and [DateTime]::UtcNow -lt $Deadline) { Start-Sleep -Milliseconds 200 }
    if ((RunningDiagnosticApp).Count) { throw 'Diagnostic player still running; preserve its DLLs until it exits' }
    if ($BridgeChanged) {
        if ((Hash $ActiveBridge) -ne $Manifest.proxySha256) { throw 'Active bridge changed during diagnostics; refusing to overwrite it' }
        Copy-Item -LiteralPath $OriginalBridge -Destination $ActiveBridge
    }
    if ($MpvChanged) {
        if ((Hash $ActiveMpv) -ne $Manifest.measurementMpvSha256) { throw 'Active mpv changed during diagnostics; refusing to overwrite it' }
        Copy-Item -LiteralPath $OriginalMpv -Destination $ActiveMpv
    }
    $Run.restored = (Hash $ActiveBridge) -eq $Manifest.bridgeSha256 -and (Hash $ActiveMpv) -eq $Manifest.mpvSha256
    $Run.finishedAt = (Get-Date).ToString('O')
    $Run | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $ResultDirectory 'run-result.json') -Encoding utf8
    if (-not $Run.restored) { throw 'Diagnostic DLL restoration hash verification failed' }
}
