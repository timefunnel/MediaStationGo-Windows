param([switch]$SkipBridgeBuild)
$ErrorActionPreference = 'Stop'
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
. (Join-Path $PSScriptRoot 'env.ps1')
$BuildRoot = [IO.Path]::GetFullPath((Join-Path $RepoRoot 'build')).TrimEnd('\') + '\'
$TestRoot = Join-Path $BuildRoot ('dynamic-hdr-test-' + [Guid]::NewGuid().ToString('N'))
$Native = Join-Path $BuildRoot 'video-enhancement'
$Mpv = Join-Path $RepoRoot 'third_party\mpv-install'
$Sdk = Join-Path $RepoRoot 'third_party\video-enhancement-deps\RTX-Video-SDK-1.1.0'
$VeyraRuntime = Join-Path $RepoRoot 'third_party\video-enhancement-deps\Veyra-2.0.6-NVIDIA\runtime'
function Fetch-Fixture([string]$Url, [string]$Name) {
    $Response = & curl.exe --fail --http1.1 --location --silent --show-error --retry 2 --max-time 30 $Url
    if ($LASTEXITCODE -ne 0) { throw "Metadata fixture download failed: $Name" }
    $Blob = $Response | ConvertFrom-Json
    if ($Blob.encoding -ne 'base64' -or -not $Blob.content) { throw "Invalid metadata fixture response: $Name" }
    [IO.File]::WriteAllBytes((Join-Path $TestRoot $Name), [Convert]::FromBase64String($Blob.content))
}
try {
    New-Item -ItemType Directory -Path $TestRoot | Out-Null
    # Extract metadata from pinned upstream test fixtures. Every pixel and audio
    # sample used for playback acceptance is generated locally.
    $DoviCommit = '41dae646193c12de152333ff5dbc33579cb9ef4a'
    $HdrCommit = '82cb01ba387184476dbf5fa3bddebf0a345693d7'
    foreach ($Name in @('profile5', 'profile8')) {
        Fetch-Fixture "https://api.github.com/repos/quietvoid/dovi_tool/contents/assets/tests/$Name.bin`?ref=$DoviCommit" "$Name.bin"
    }
    Fetch-Fixture "https://api.github.com/repos/quietvoid/dovi_tool/contents/assets/tests/profile5-02.bin?ref=$DoviCommit" 'profile5-02.bin'
    $SecondRpu = [IO.File]::ReadAllBytes((Join-Path $TestRoot 'profile5-02.bin'))
    Fetch-Fixture "https://api.github.com/repos/quietvoid/hdr10plus_tool/contents/assets/hevc_tests/single-frame.hevc?ref=$HdrCommit" 'hdr-template.hevc'
    $HdrBytes = [IO.File]::ReadAllBytes((Join-Path $TestRoot 'hdr-template.hevc'))
    $HdrText = [Text.Encoding]::Latin1.GetString($HdrBytes)
    $HdrNals = [regex]::Matches($HdrText, '\x00\x00\x00?\x01')
    $HdrSei = $null
    for ($Index = 0; $Index -lt $HdrNals.Count; $Index++) {
        $Begin = $HdrNals[$Index].Index + $HdrNals[$Index].Length
        $End = if ($Index + 1 -lt $HdrNals.Count) { $HdrNals[$Index + 1].Index } else { $HdrBytes.Length }
        $NalType = ($HdrBytes[$Begin] -shr 1) -band 63
        if ($NalType -in 39, 40 -and $HdrText.Substring($Begin, $End - $Begin).Contains([Text.Encoding]::Latin1.GetString([byte[]]@(181, 0, 60, 0, 1, 4)))) {
            $HdrSei = [byte[]](@(0, 0, 0, 1, 78, 1) + $HdrBytes[($Begin + 2)..($End - 1)])
            break
        }
    }
    if (-not $HdrSei) { throw 'Pinned fixture contains no HDR10+ ITU-T T.35 SEI' }
    Push-Location $TestRoot
    try {
        foreach ($Kind in @('profile5', 'profile8', 'hdr10plus')) {
            $Extra = 'aud=1'
            & ffmpeg.exe -hide_banner -loglevel error -f lavfi -i 'testsrc2=size=640x360:rate=24:duration=4' -c:v libx265 -preset ultrafast -pix_fmt yuv420p10le -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc -color_range tv -chroma_sample_location left -x265-params ("log-level=error:bframes=0:colorprim=9:transfer=16:colormatrix=9:$Extra") -f hevc "$Kind.hevc"
            if ($LASTEXITCODE -ne 0) { throw "Dynamic HDR generation failed: $Kind" }
            # FFmpeg overwrites x265's Dolby profile from its input side data.
            # Inject the pinned RPU as HEVC type-62 NALs into our own pixels.
            $VideoBytes = [IO.File]::ReadAllBytes((Join-Path $TestRoot "$Kind.hevc"))
            $RpuBytes = if ($Kind -eq 'hdr10plus') { $null } else { [IO.File]::ReadAllBytes((Join-Path $TestRoot "$Kind.bin")) }
            $Nals = [regex]::Matches([Text.Encoding]::Latin1.GetString($VideoBytes), '\x00\x00\x00?\x01')
            $Injected = [IO.MemoryStream]::new()
            $RpuFrames = 0
            try {
                for ($Index = 0; $Index -lt $Nals.Count; $Index++) {
                    $Begin = $Nals[$Index].Index
                    $End = if ($Index + 1 -lt $Nals.Count) { $Nals[$Index + 1].Index } else { $VideoBytes.Length }
                    $NalType = ($VideoBytes[$Begin + $Nals[$Index].Length] -shr 1) -band 63
                    if ($NalType -lt 32 -and $Kind -eq 'hdr10plus') { $Injected.Write($HdrSei, 0, $HdrSei.Length) }
                    $Injected.Write($VideoBytes, $Begin, $End - $Begin)
                    if ($NalType -lt 32) {
                        if ($Kind -ne 'hdr10plus') {
                            $FrameRpu = if ($Kind -eq 'profile5' -and $RpuFrames -ge 48) { $SecondRpu } else { $RpuBytes }
                            $Injected.Write([byte[]]@(0, 0, 0, 1, 124, 1), 0, 6)
                            $Injected.Write($FrameRpu, 4, $FrameRpu.Length - 4)
                        }
                        $RpuFrames++
                    }
                }
                if ($RpuFrames -ne 96) { throw "Expected 96 VCL frames, got $RpuFrames" }
                [IO.File]::WriteAllBytes((Join-Path $TestRoot "$Kind.hevc"), $Injected.ToArray())
            } finally { $Injected.Dispose() }
            & ffmpeg.exe -hide_banner -loglevel error -fflags +genpts -r 24 -i "$Kind.hevc" -f lavfi -i 'sine=frequency=440:sample_rate=48000:duration=4' -c:v copy -bsf:v 'setts=pts=N/(24*TB):dts=N/(24*TB)' -c:a aac -shortest "$Kind.mkv"
            if ($LASTEXITCODE -ne 0) { throw "Dynamic HDR mux failed: $Kind" }
            $ProbeJson = & ffprobe.exe -v error -select_streams v:0 -read_intervals '%+#1' -show_frames -of json "$Kind.mkv"
            if ($LASTEXITCODE -ne 0) { throw "Dynamic HDR probe failed: $Kind" }
            $Probe = $ProbeJson | ConvertFrom-Json
            $SideData = @($Probe.frames[0].side_data_list.side_data_type)
            $Expected = if ($Kind -eq 'hdr10plus') { 'HDR Dynamic Metadata SMPTE2094-40 (HDR10+)' } else { 'Dolby Vision Metadata' }
            if ($SideData -notcontains $Expected) { throw "Generated fixture is missing $Expected; found $($SideData -join ', ')" }
            Write-Output "DYNAMIC_HDR_SOURCE_OK kind=$Kind side-data=$($SideData -join ',')"
        }
    } finally { Pop-Location }
    if(-not $SkipBridgeBuild) { & (Join-Path $PSScriptRoot 'build_video_enhancement.ps1') }
    & cl.exe /nologo /MD /std:c++17 /EHsc /W4 /WX "/I$Mpv\include" (Join-Path $PSScriptRoot 'mpv\test_dynamic_hdr_playback.cpp') "/Fo$TestRoot\test_dynamic_hdr_playback.obj" "/Fe$TestRoot\test_dynamic_hdr_playback.exe" /link "$Mpv\lib\mpv.lib" user32.lib
    if ($LASTEXITCODE -ne 0) { throw 'Dynamic HDR playback probe compilation failed' }
    & (Join-Path $TestRoot 'test_dynamic_hdr_playback.exe') (Join-Path $TestRoot 'profile5.mkv') (Join-Path $TestRoot 'profile8.mkv') (Join-Path $TestRoot 'hdr10plus.mkv') "$Native\video_enhancement.dll" "$Sdk\bin\Windows\x64\rel" "$VeyraRuntime\nvidia-vfg" $TestRoot "$VeyraRuntime\experimental\nr-original"
    if ($LASTEXITCODE -ne 0) { throw 'Dynamic HDR native playback checks failed' }
} finally {
    $Absolute = [IO.Path]::GetFullPath($TestRoot)
    if (-not $Absolute.StartsWith($BuildRoot, [StringComparison]::OrdinalIgnoreCase)) { throw "Unexpected cleanup target: $Absolute" }
    if (Test-Path -LiteralPath $Absolute) { Remove-Item -LiteralPath $Absolute -Recurse -Force }
}
