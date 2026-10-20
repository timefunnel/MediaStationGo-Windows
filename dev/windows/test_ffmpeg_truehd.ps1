param(
    [Parameter(Mandatory = $true)]
    [string]$FfmpegPath
)

$ErrorActionPreference = "Stop"
$FfmpegPath = (Resolve-Path -LiteralPath $FfmpegPath).Path
$TempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd('\') + '\'
$TestDir = Join-Path $TempRoot ("jellium-truehd-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $TestDir | Out-Null

function Invoke-TestFfmpeg {
    param([string[]]$Arguments)
    $Log = Join-Path $TestDir "ffmpeg.log"
    & $FfmpegPath -nostdin -hide_banner -loglevel error -xerror @Arguments 2>$Log
    if ($LASTEXITCODE -ne 0) {
        throw "TrueHD regression failed (exit=$LASTEXITCODE): $([System.IO.File]::ReadAllText($Log))"
    }
    if ((Get-Item -LiteralPath $Log).Length -ne 0) {
        throw "TrueHD regression emitted decoding errors: $([System.IO.File]::ReadAllText($Log))"
    }
}

function Assert-Pcm {
    param([string]$Path, [int]$Length, [string]$Sha256)
    if ((Get-Item -LiteralPath $Path).Length -ne $Length -or
        (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() -ne $Sha256) {
        throw "TrueHD decoded PCM differs from the expected samples: $Path"
    }
}

try {
    # Synthetic 40 ms / 48 kHz / 5.1 fixture; each channel is a distinct constant.
    # Generated with the pinned TrueHD encoder using one substream, all channels
    # in s[0], and thd_substream_info=0x04. Contains no media from a real film.
    $Fixture = Join-Path $PSScriptRoot "ffmpeg\single-substream-5point1.thd.hex"
    $Single = Join-Path $TestDir "single.thd"
    $Hex = ([System.IO.File]::ReadAllText($Fixture) -replace '\s', '')
    [System.IO.File]::WriteAllBytes($Single, [Convert]::FromHexString($Hex))
    $PcmHash = "11c7c33007917e8970416f3e42463ca7867e6ad70e01f4b3acbfafc46900804d"
    $Pcm = Join-Path $TestDir "single.pcm"
    Invoke-TestFfmpeg @("-i", $Single, "-c:a", "pcm_s24le", "-f", "s24le", $Pcm)
    Assert-Pcm $Pcm 34560 $PcmHash

    # The normal encoder includes a stereo downmix plus the 5.1 substream.
    $Multi = Join-Path $TestDir "multi.thd"
    Invoke-TestFfmpeg @("-f", "lavfi", "-i", "aevalsrc=0.125|0.25|-0.125|0.0625|-0.25|-0.0625:s=48000:c=5.1", "-t", "0.04", "-c:a", "truehd", "-strict", "experimental", $Multi)
    $MultiPcm = Join-Path $TestDir "multi.pcm"
    Invoke-TestFfmpeg @("-i", $Multi, "-c:a", "pcm_s24le", "-f", "s24le", $MultiPcm)
    Assert-Pcm $MultiPcm 34560 $PcmHash

    foreach ($Channels in @(1, 2)) {
        $Layout = if ($Channels -eq 1) { "mono" } else { "stereo" }
        $Encoded = Join-Path $TestDir "$Layout.thd"
        $Decoded = Join-Path $TestDir "$Layout.pcm"
        Invoke-TestFfmpeg @("-f", "lavfi", "-i", "anullsrc=r=48000:cl=$Layout", "-t", "0.04", "-c:a", "truehd", "-strict", "experimental", $Encoded)
        Invoke-TestFfmpeg @("-i", $Encoded, "-c:a", "pcm_s24le", "-f", "s24le", $Decoded)
        $Bytes = [System.IO.File]::ReadAllBytes($Decoded)
        if ($Bytes.Length -ne 1920 * $Channels * 3 -or ($Bytes | Where-Object { $_ -ne 0 }).Count -ne 0) {
            throw "TrueHD $Layout regression produced incorrect PCM samples"
        }
    }
    Write-Host "FFMPEG_TRUEHD_TEST_OK single_5point1=pass multi_5point1=pass mono=pass stereo=pass"
} finally {
    $ResolvedTestDir = [System.IO.Path]::GetFullPath($TestDir)
    if (-not $ResolvedTestDir.StartsWith($TempRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to clean a TrueHD test directory outside $TempRoot"
    }
    Remove-Item -LiteralPath $ResolvedTestDir -Recurse -Force
}
