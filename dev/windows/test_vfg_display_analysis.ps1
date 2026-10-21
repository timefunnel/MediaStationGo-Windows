$ErrorActionPreference = 'Stop'
$Repo = (Get-Item $PSScriptRoot).Parent.Parent.FullName
$Build = [IO.Path]::GetFullPath((Join-Path $Repo 'build')) + '\'
$Temporary = [IO.Path]::GetFullPath((Join-Path $Build ('vfg-analysis-test-' + [Guid]::NewGuid().ToString('N'))))
if (-not $Temporary.StartsWith($Build, [StringComparison]::OrdinalIgnoreCase)) { throw 'Invalid fixture path' }
New-Item -ItemType Directory -Path $Temporary | Out-Null
try {
    foreach ($Case in @('complete', 'missing-repeat', 'missing-boundary')) {
        $Directory = Join-Path $Temporary $Case
        New-Item -ItemType Directory -Path $Directory | Out-Null
        '{}' | Set-Content -LiteralPath (Join-Path $Directory 'manifest.json') -Encoding utf8
        foreach ($Divisor in @(3, 2)) {
            @{ startQpcMs=100; endQpcMs=190; seconds=.08; targetFps=152.0/$Divisor; divisor=$Divisor } |
                ConvertTo-Json | Set-Content -LiteralPath (Join-Path $Directory "divisor-$Divisor-summary.json") -Encoding utf8
            $Lines = foreach ($Id in 1..12) {
                $Pts = [Math]::Floor(($Id - 1) / $Divisor)
                "VFG_CONTENT_PRESENT hr=00000000 present=$Id pts=$Pts mix=1"
                if ($Case -eq 'missing-repeat' -and $Id -eq 2) { continue }
                if ($Case -eq 'missing-boundary' -and $Id -eq ($Divisor + 1)) { continue }
                $Refresh = 1000 + $Id; $Qpc = 1000000 + $Id * 65790
                "VFG_DXGI_STATS hr=00000000 present=$Id refresh=$Refresh sync=$Refresh qpc=$Qpc query=$Qpc freq=10000000"
            }
            $Lines | Set-Content -LiteralPath (Join-Path $Directory "divisor-$Divisor-mpv.log") -Encoding utf8
        }
        $Rejected = $false
        try { & (Join-Path $PSScriptRoot 'analyze_vfg_display.ps1') -ResultDirectory $Directory | Out-Null }
        catch {
            if ($_.ToString() -notlike '*99 percent coverage gate*') { throw }
            $Rejected = $true
        }
        $Result = Get-Content -LiteralPath (Join-Path $Directory 'display-content-comparison.json') -Raw | ConvertFrom-Json
        foreach ($Item in $Result.comparison) {
            if ($Item.physicalDroppedFrames -ne $null) { throw 'Unknown physical drops were treated as zero' }
            $Keys = @($Item.refreshCycleHistogram.PSObject.Properties.Name)
            if ($Keys.Count -ne 1 -or $Keys[0] -ne $Item.playback.divisor.ToString()) { throw 'Repeated presents were counted as new content' }
            if ($Case -eq 'missing-boundary') {
                if (-not $Rejected -or $Item.meets99PercentCoverageGate -or $Item.ambiguousPresentGaps -lt 1) { throw 'An unseen content boundary was silently filled' }
            } elseif ($Rejected -or $Item.knownContentCoveragePercent -ne 100) { throw 'A proven identical repeat made a content interval ambiguous' }
        }
    }
    Write-Output 'VFG_DISPLAY_ANALYSIS_OK repeats-grouped/proven-repeat-gap/unseen-boundary-rejected/unknown-drops-preserved'
} finally {
    if (-not [IO.Path]::GetFullPath($Temporary).StartsWith($Build, [StringComparison]::OrdinalIgnoreCase)) { throw 'Invalid cleanup path' }
    Remove-Item -LiteralPath $Temporary -Recurse -Force
}
