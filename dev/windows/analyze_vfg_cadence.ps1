param([Parameter(Mandatory)][string]$ResultDirectory)
$ErrorActionPreference = 'Stop'
$ResultDirectory = (Resolve-Path -LiteralPath $ResultDirectory).Path
$Manifest = Get-Content -LiteralPath (Join-Path $ResultDirectory 'manifest.json') -Raw | ConvertFrom-Json
$Rows = @(Import-Csv -LiteralPath (Join-Path $ResultDirectory 'presentation.csv'))
if (-not $Rows.Count) { throw 'Empty presentation trace' }
foreach ($Column in @('ProcessID', 'CPUStartQPCTime', 'DisplayedTime', 'SwapChainAddress', 'PresentMode')) {
    if ($Column -notin $Rows[0].PSObject.Properties.Name) { throw "Missing PresentMon v2 column: $Column" }
}
function Read-Number([string]$Value) {
    $Number = 0.0
    if ([double]::TryParse($Value, [Globalization.NumberStyles]::Float, [Globalization.CultureInfo]::InvariantCulture, [ref]$Number) -and [double]::IsFinite($Number)) { return $Number }
    return $null
}
function Get-Percentile([double[]]$Values, [double]$Fraction) {
    if (-not $Values.Count) { throw 'Cannot summarize an empty measurement' }
    $Sorted = @($Values | Sort-Object)
    return $Sorted[[Math]::Max(0, [int][Math]::Ceiling($Fraction * $Sorted.Count) - 1)]
}
$Comparison = foreach ($Divisor in @(3, 2)) {
    $Playback = Get-Content -LiteralPath (Join-Path $ResultDirectory "divisor-$Divisor-summary.json") -Raw | ConvertFrom-Json
    if (-not $Playback.startQpcMs -or -not $Playback.endQpcMs) { throw 'Playback summary lacks the QPC measurement boundaries' }
    $ProcessRows = @($Rows | Where-Object {
        $Start = Read-Number $_.CPUStartQPCTime
        [int]$_.ProcessID -eq $Playback.processId -and $null -ne $Start -and $Start -ge $Playback.startQpcMs -and $Start -le $Playback.endQpcMs
    })
    if ($ProcessRows.Count -lt $Manifest.seconds * $Playback.targetFps * .8) { throw "Presentation trace is incomplete for divisor $Divisor" }
    $SwapChains = @($ProcessRows | Group-Object SwapChainAddress)
    if ($SwapChains.Count -ne 1) { throw "More than one swap chain during steady-state divisor $Divisor; inspect trace before comparing" }
    $Intervals = [Collections.Generic.List[double]]::new()
    $HoldCycles = @{}
    $Dropped = 0
    $RefreshMs = 1000.0 / $Playback.displayHz
    foreach ($Row in $ProcessRows) {
        $Interval = Read-Number $Row.DisplayedTime
        if ($null -eq $Interval) { $Dropped++; continue }
        if ($Interval -le 0) { throw "Invalid displayed duration: $Interval" }
        $Intervals.Add($Interval)
        $Cycles = [Math]::Round($Interval / $RefreshMs).ToString()
        if (-not $HoldCycles.ContainsKey($Cycles)) { $HoldCycles[$Cycles] = 0 }
        $HoldCycles[$Cycles]++
    }
    if ($Intervals.Count -lt $Manifest.seconds * $Playback.targetFps * .8) { throw 'Too few displayed frames for a timing comparison' }
    $Stats = $Intervals | Measure-Object -Average -Maximum
    [ordered]@{
        divisor = $Divisor; targetFps = $Playback.targetFps; idealIntervalMs = 1000.0 / $Playback.targetFps
        presentedFrames = $ProcessRows.Count; displayedFrames = $Intervals.Count; droppedPresents = $Dropped
        displayMeanMs = $Stats.Average; displayMedianMs = Get-Percentile $Intervals.ToArray() .5
        displayP95Ms = Get-Percentile $Intervals.ToArray() .95; displayP99Ms = Get-Percentile $Intervals.ToArray() .99
        displayMaxMs = $Stats.Maximum; measuredDisplayFps = 1000.0 / $Stats.Average
        refreshCycleHistogram = $HoldCycles
        presentModes = @($ProcessRows | Group-Object PresentMode | Select-Object Name,Count)
        playback = $Playback
    }
}
[ordered]@{ source = $Manifest; comparison = @($Comparison) } | ConvertTo-Json -Depth 9 | Set-Content -LiteralPath (Join-Path $ResultDirectory 'comparison.json') -Encoding utf8
$Comparison | ForEach-Object { [pscustomobject]@{
    TargetFps = '{0:F3}' -f $_.targetFps
    DisplayMeanMs = '{0:F3}' -f $_.displayMeanMs
    DisplayP99Ms = '{0:F3}' -f $_.displayP99Ms
    DisplayMaxMs = '{0:F3}' -f $_.displayMaxMs
    DroppedPresents = $_.droppedPresents
    PlaybackDrops = $_.playback.frameDrops
    AVMaxMs = '{0:F3}' -f $_.playback.avMaxMs
} } | Format-Table -AutoSize
