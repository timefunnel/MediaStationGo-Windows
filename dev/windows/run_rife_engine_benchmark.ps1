param(
    [string]$RuntimeDir,
    [ValidateRange(320, 7680)]
    [int]$Width = 3840,
    [ValidateRange(240, 4320)]
    [int]$Height = 2160,
    [ValidateSet(1, 2)]
    [int]$Implementation = 1,
    [ValidateSet("fp16", "fp32")]
    [string]$Precision = "fp16",
    [ValidateRange(1.0, 240.0)]
    [double]$RequiredPairFps = 24.0,
    [ValidateRange(1.0, 1000.0)]
    [double]$MaximumP95Milliseconds = 35.0,
    [ValidateRange(1, 60)]
    [int]$DurationSeconds = 5
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
$TensorRtBin = Join-Path $RuntimeDir ".tensorrt\TensorRT-RTX-1.4.0.76\bin"
$TensorRtExe = Join-Path $TensorRtBin "tensorrt_rtx.exe"
$EngineDir = Join-Path $RuntimeDir "engines\poc-rife-v4_25-lite-impl${Implementation}-${Width}x${Height}-scale1_0-${Precision}"
$Engines = @(Get-ChildItem -LiteralPath $EngineDir -Filter "*.engine" -File -ErrorAction SilentlyContinue)
if (-not (Test-Path -LiteralPath $TensorRtExe)) {
    throw "TensorRT-RTX benchmark executable is missing: $TensorRtExe"
}
if ($Engines.Count -ne 1) {
    throw "Expected exactly one cached TensorRT engine in $EngineDir, found $($Engines.Count)"
}

$PreviousPath = $env:PATH
$env:PATH = "$TensorRtBin;$(Join-Path $RuntimeDir 'bin');$PreviousPath"
try {
    $PreviousErrorActionPreference = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $Output = & $TensorRtExe `
        "--loadEngine=$($Engines[0].FullName)" `
        "--duration=$DurationSeconds" `
        --warmUp=1000 `
        --iterations=100 `
        --noDataTransfers `
        --useCudaGraph `
        --percentile=50,90,95,99 `
        --avgRuns=50 2>&1 | ForEach-Object { $_.ToString() }
    $ExitCode = $LASTEXITCODE
    $ErrorActionPreference = $PreviousErrorActionPreference
} finally {
    $env:PATH = $PreviousPath
}
$Output | ForEach-Object { Write-Host $_ }
if ($ExitCode -ne 0) {
    throw "TensorRT-RTX engine benchmark failed with exit code $ExitCode"
}

$Text = $Output -join "`n"
$ThroughputMatch = [regex]::Match(
    $Text,
    '(?im)^.*Throughput:\s+([0-9.]+)\s+qps\s*$')
$P95Match = [regex]::Match(
    $Text,
    'percentile\(95%\)\s*=\s*([0-9.]+)\s+ms')
$GpuMemoryMatch = [regex]::Match(
    $Text,
    'GPU \+([0-9]+), now: CPU [0-9]+, GPU ([0-9]+) \(MiB\)')
if (-not $ThroughputMatch.Success -or -not $P95Match.Success) {
    throw "TensorRT-RTX benchmark output did not contain throughput and p95 latency"
}
$Throughput = [double]::Parse(
    $ThroughputMatch.Groups[1].Value,
    [System.Globalization.CultureInfo]::InvariantCulture)
$P95Milliseconds = [double]::Parse(
    $P95Match.Groups[1].Value,
    [System.Globalization.CultureInfo]::InvariantCulture)
if ($Throughput -lt $RequiredPairFps) {
    throw ("RIFE engine throughput {0:N2} qps is below required {1:N2} pair fps" -f $Throughput, $RequiredPairFps)
}
if ($P95Milliseconds -gt $MaximumP95Milliseconds) {
    throw ("RIFE engine p95 latency {0:N2} ms exceeds budget {1:N2} ms" -f $P95Milliseconds, $MaximumP95Milliseconds)
}

$GpuMemory = if ($GpuMemoryMatch.Success) {
    $GpuMemoryMatch.Groups[2].Value
} else {
    "unknown"
}
Write-Host ("FRAME_INTERPOLATION_ENGINE_BENCHMARK_OK input={0}x{1} implementation={2} precision={3} model=RIFE-v4.25-lite backend=TensorRT-RTX cuda_graph=yes throughput={4:N2}qps p95={5:N2}ms required={6:N2}qps budget={7:N2}ms gpu_memory={8}MiB" -f $Width, $Height, $Implementation, $Precision, $Throughput, $P95Milliseconds, $RequiredPairFps, $MaximumP95Milliseconds, $GpuMemory) -ForegroundColor Green
