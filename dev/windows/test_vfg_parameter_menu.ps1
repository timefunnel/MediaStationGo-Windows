param(
    [Parameter(Mandatory)][string]$ResultDirectory,
    [ValidateRange(1024, 65534)][int]$Port = 9263
)
$ErrorActionPreference = 'Stop'
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
$BuildRoot = [IO.Path]::GetFullPath((Join-Path $RepoRoot 'build')) + '\'
$ResultDirectory = [IO.Path]::GetFullPath($ResultDirectory)
$TestRoot = Join-Path $BuildRoot ('vfg-menu-test-' + [Guid]::NewGuid().ToString('N'))
if (-not $ResultDirectory.StartsWith($BuildRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Reports must stay inside build/' }
if (Test-Path -LiteralPath $ResultDirectory) { throw 'Use a new report directory' }
. (Join-Path $PSScriptRoot 'env.ps1')
$App = Join-Path $BuildRoot 'jellium-desktop.exe'
$Bridge = Join-Path $BuildRoot 'video_enhancement.dll'
$Runtime = Join-Path $RepoRoot 'third_party\video-enhancement-deps\Veyra-2.0.6-NVIDIA\runtime\nvidia-vfg'
function FilterQuote([string]$Value) { '%' + [Text.Encoding]::UTF8.GetByteCount($Value) + '%' + $Value }
New-Item -ItemType Directory -Path $ResultDirectory, $TestRoot | Out-Null
try {
    $Clip = Join-Path $TestRoot '4k25.mp4'
    & ffmpeg.exe -hide_banner -loglevel error -f lavfi -i 'testsrc2=size=3840x2160:rate=25:duration=40' -f lavfi -i 'sine=frequency=440:sample_rate=48000:duration=40' -c:v libx264 -preset ultrafast -crf 21 -pix_fmt yuv420p -c:a aac -t 40 $Clip
    if ($LASTEXITCODE -ne 0) { throw 'Fixture generation failed' }
    [ordered]@{ appSha256=(Get-FileHash $App).Hash; mpvSha256=(Get-FileHash (Join-Path $BuildRoot 'libmpv-2.dll')).Hash; bridgeSha256=(Get-FileHash $Bridge).Hash; sourceSha256=(Get-FileHash $Clip).Hash; source='3840x2160 25 fps'; quality='Medium'; audio='muted WASAPI'; installedAppChanged=$false } | ConvertTo-Json | Set-Content (Join-Path $ResultDirectory 'manifest.json') -Encoding utf8
    foreach ($Multiplier in @(0, 2)) {
        $Report = Join-Path $ResultDirectory "multiplier-$Multiplier"
        $Config = Join-Path $TestRoot "config-$Multiplier"
        $Cache = Join-Path $TestRoot "cache-$Multiplier"
        $MpvConfig = Join-Path $Config 'mpv'
        New-Item -ItemType Directory -Path $Report, $MpvConfig, $Cache | Out-Null
        $Filter = '@nvconvert:d3d11vpp=format=bgra,@enhance_vfg:nvvideo=mode=2:multiplier=' + $Multiplier + ':quality=1:bridge=' + (FilterQuote $Bridge) + ':runtime=' + (FilterQuote $Runtime) + ':cache=' + (FilterQuote (Join-Path $TestRoot 'engines')) + ',@nvoutput:d3d11vpp=format=nv12'
        [IO.File]::WriteAllText((Join-Path $MpvConfig 'mpv.conf'), "vf=$Filter`nvideo-sync=display-vdrop`nvolume=0`nao=wasapi`n", [Text.UTF8Encoding]::new($false))
        $AppProcess = $null
        try {
            $Arguments = @('--config-dir', ('"' + $Config + '"'), '--cache-dir', ('"' + $Cache + '"'), '--remote-debug-port', $Port, '--log-file', ('"' + (Join-Path $Report 'app.log') + '"'), '--log-level', 'info,scroll_latency=trace')
            $AppProcess = Start-Process -FilePath $App -ArgumentList $Arguments -WorkingDirectory $BuildRoot -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $Report 'stderr.log') -RedirectStandardOutput (Join-Path $Report 'stdout.log')
            Start-Sleep -Seconds 3
            & node (Join-Path $PSScriptRoot 'test_vfg_parameter_menu.cjs') $Port $Clip $Report *> (Join-Path $Report 'probe.log')
            if ($LASTEXITCODE -ne 0) { throw "VFG menu probe failed: $Report\probe.log" }
            Get-Content (Join-Path $Report 'probe.log')
        } finally {
            if ($AppProcess -and -not $AppProcess.HasExited) { Stop-Process -Id $AppProcess.Id; $AppProcess.WaitForExit() }
        }
    }
} finally {
    $ResolvedTestRoot = [IO.Path]::GetFullPath($TestRoot)
    if (-not $ResolvedTestRoot.StartsWith($BuildRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Unexpected cleanup target' }
    if (Test-Path -LiteralPath $ResolvedTestRoot) { Remove-Item -LiteralPath $ResolvedTestRoot -Recurse }
}
