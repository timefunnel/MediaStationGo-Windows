param([Parameter(Mandatory)][string]$ResultDirectory, [switch]$ReportOnly)
$ErrorActionPreference = 'Stop'
function Quantile([double[]]$Values, [double]$Q) {
    $Sorted = @($Values | Sort-Object)
    return $Sorted[[int][Math]::Ceiling($Sorted.Count * $Q) - 1]
}
$Results = foreach ($Divisor in @(3, 2)) {
    $Summary = Get-Content -LiteralPath (Join-Path $ResultDirectory "divisor-$Divisor-summary.json") -Raw | ConvertFrom-Json
    $Content = @{}
    $Displayed = @{}
    # Use mpv's direct log file, independent of the bounded client-event queue.
    foreach ($Line in Get-Content -LiteralPath (Join-Path $ResultDirectory "divisor-$Divisor-mpv.log")) {
        if ($Line -match 'VFG_CONTENT_PRESENT hr=(\w+) present=(\d+) pts=([-\d.]+) mix=(\d+)') {
            if ($Matches[1] -ne '00000000') { throw 'Cannot identify submitted content' }
            $Content[[long]$Matches[2]] = @{ pts=$Matches[3]; mix=[int]$Matches[4] }
        } elseif ($Line -match 'VFG_DXGI_STATS hr=(\w+) present=(\d+) refresh=(\d+) sync=(\d+) qpc=(\d+) query=(\d+) freq=(\d+)') {
            $QueryMs = [double]$Matches[6] * 1000 / [double]$Matches[7]
            if ($QueryMs -lt $Summary.startQpcMs -or $QueryMs -gt $Summary.endQpcMs + 100) { continue }
            if ($Matches[1] -ne '00000000' -or [long]$Matches[2] -le 0) { throw 'Invalid display statistics' }
            if ($Matches[3] -ne $Matches[4]) { continue } # not a timestamp for this displayed frame
            $Ms = [double]$Matches[5] * 1000 / [double]$Matches[7]
            if ($Ms -ge $Summary.startQpcMs -and $Ms -le $Summary.endQpcMs) {
                $Displayed[[long]$Matches[2]] = @{ present=[long]$Matches[2]; refresh=[long]$Matches[3]; ms=$Ms }
            }
        }
    }
    $Events = @($Displayed.Values | Sort-Object present)
    if ($Events.Count -lt $Summary.seconds * $Summary.targetFps * .95) { throw 'Too few displayed samples' }
    if ($Events[0].ms - $Summary.startQpcMs -gt 100 -or $Summary.endQpcMs - $Events[-1].ms -gt 100) { throw 'Incomplete measurement boundaries' }
    $Durations = [Collections.Generic.List[double]]::new()
    $Histogram = @{}
    $Unknown = 0; $Transitions = 0; $Repeated = 0; $AmbiguousGaps = 0; $PossibleUnseenTransitions = 0
    $Start = $null; $Previous = $null
    foreach ($Event in $Events) {
        $Image = $Content[$Event.present]
        if (-not $Image -or $Image.mix -ne 1) { throw "Ambiguous rendered content at present $($Event.present)" }
        $Event.pts = $Image.pts
        if ($Previous) {
            if ($Event.present -le $Previous.present -or $Event.refresh -le $Previous.refresh) { throw 'Non-increasing display counters' }
            $Known = $true
            $CandidatePts = $Previous.pts; $PossibleTransitions = 0
            for ($Id = $Previous.present + 1; $Id -lt $Event.present; $Id++) {
                if (-not $Content.ContainsKey($Id) -or $Content[$Id].mix -ne 1) { throw 'Missing or ambiguous intervening submitted image' }
                if ($Content[$Id].pts -ne $CandidatePts) { $PossibleTransitions++ }
                $CandidatePts = $Content[$Id].pts
                if ($CandidatePts -ne $Previous.pts) { $Known = $false }
            }
            if ($Event.pts -ne $CandidatePts) { $PossibleTransitions++ }
            $ObservedTransitions = if ($Event.pts -ne $Previous.pts) { 1 } else { 0 }
            $PossibleUnseenTransitions += $PossibleTransitions - $ObservedTransitions
            if (-not $Known) { $Start = $null; $AmbiguousGaps++ }
            if ($Event.pts -eq $Previous.pts) { $Repeated++ }
            else {
                $Transitions++
                # Even a missed Present with identical content is safe only
                # when every intervening submission proves the old PTS. A
                # possible unseen content boundary stays explicitly unknown.
                if ($Start -and $Known) {
                    $Durations.Add($Event.ms - $Start.ms)
                    $Cycles = ($Event.refresh - $Start.refresh).ToString()
                    if (-not $Histogram.ContainsKey($Cycles)) { $Histogram[$Cycles] = 0 }
                    $Histogram[$Cycles]++
                } else { $Unknown++ }
                $Start = if ($Known) { $Event } else { $null }
            }
        }
        $Previous = $Event
    }
    # The first sampled content frame has no observed start and is excluded.
    $Coverage = 100.0 * $Durations.Count / [Math]::Max(1, $Transitions - 1 + $PossibleUnseenTransitions)
    if (-not $Durations.Count) { throw 'No fully observed content intervals' }
    $Span = $Events[-1].ms - $Events[0].ms
    $Stats = $Durations | Measure-Object -Average -Maximum
    [ordered]@{
        scope='DXGI displayed Present ID mapped to the actual single libplacebo source image PTS; repeats grouped; no optical measurement'
        playback=$Summary; analysisSeconds=$Span/1000; displayedPresentSamples=$Events.Count
        repeatedContentSamples=$Repeated; contentTransitions=$Transitions; knownContentIntervals=$Durations.Count
        unknownContentIntervals=$Unknown; ambiguousPresentGaps=$AmbiguousGaps; knownContentCoveragePercent=$Coverage
        possibleUnseenContentTransitions=$PossibleUnseenTransitions
        meets99PercentCoverageGate=($Coverage -ge 99)
        observedContentFps=1000*$Transitions/$Span
        measuredRefreshHz=1000*($Events[-1].refresh-$Events[0].refresh)/$Span
        contentMeanMs=$Stats.Average; contentP95Ms=(Quantile $Durations.ToArray() .95)
        contentP99Ms=(Quantile $Durations.ToArray() .99); contentMaxMs=$Stats.Maximum
        refreshCycleHistogram=$Histogram; physicalDroppedFrames=$null
    }
}
[ordered]@{manifest=(Get-Content -LiteralPath (Join-Path $ResultDirectory 'manifest.json') -Raw | ConvertFrom-Json); comparison=@($Results)} | ConvertTo-Json -Depth 9 | Set-Content -LiteralPath (Join-Path $ResultDirectory 'display-content-comparison.json') -Encoding utf8
$Results | ForEach-Object { [pscustomobject]@{ fps=$_.playback.targetFps; sync=$_.playback.videoSync; p99Ms=$_.contentP99Ms; maxMs=$_.contentMaxMs; coverage=$_.knownContentCoveragePercent; cycles=($_.refreshCycleHistogram | ConvertTo-Json -Compress) } } | Format-Table -AutoSize
if (@($Results | Where-Object { -not $_.meets99PercentCoverageGate }).Count) {
    if ($ReportOnly) { Write-Warning 'Partial display sampling: the 99 percent coverage gate FAILED. Percentiles describe fully observed intervals only; missing boundaries remain unknown.' }
    else { throw 'Display sampling did not meet the 99 percent coverage gate; inspect display-content-comparison.json' }
}
