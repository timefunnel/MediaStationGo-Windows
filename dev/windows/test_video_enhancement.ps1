param([string]$SdkDirectory = '', [switch]$HdrOnly)
$ErrorActionPreference = 'Stop'
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
. (Join-Path $PSScriptRoot 'env.ps1')
if (-not $SdkDirectory) { $SdkDirectory = Join-Path $RepoRoot 'third_party\video-enhancement-deps\RTX-Video-SDK-1.1.0' }
$SdkDirectory = (Resolve-Path -LiteralPath $SdkDirectory).Path
$TestRoot = [IO.Path]::GetFullPath((Join-Path $RepoRoot ('build\video-enhancement-test-' + [Guid]::NewGuid().ToString('N'))))
$BuildRoot = [IO.Path]::GetFullPath((Join-Path $RepoRoot 'build')) + '\'
if (-not $TestRoot.StartsWith($BuildRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Test directory must be inside this workspace build directory' }
$Mpv = Join-Path $RepoRoot 'third_party\mpv-install'
try {
    & (Join-Path $PSScriptRoot 'build_video_enhancement.ps1') -SdkDirectory $SdkDirectory -OutputDir $TestRoot -TestGpu
    & cl.exe /nologo /MD /std:c++17 /EHsc /W4 /WX "/I$Mpv\include" (Join-Path $PSScriptRoot 'mpv\test_nvvideo_playback.cpp') "/Fo$TestRoot\test_nvvideo_playback.obj" "/Fe$TestRoot\test_nvvideo_playback.exe" /link "$Mpv\lib\mpv.lib" user32.lib
    if ($LASTEXITCODE -ne 0) { throw 'mpv playback test compilation failed' }
    $Ffmpeg = (Get-Command ffmpeg.exe -ErrorAction Stop).Source
    & $Ffmpeg -hide_banner -loglevel error -f lavfi -i 'testsrc2=size=640x360:rate=24:duration=4' -f lavfi -i 'sine=frequency=440:sample_rate=48000:duration=4' -c:v libx264 -pix_fmt yuv420p -c:a aac -shortest (Join-Path $TestRoot 'source.mp4')
    if ($LASTEXITCODE -ne 0) { throw 'Local synthetic video generation failed' }
    & $Ffmpeg -hide_banner -loglevel error -f lavfi -i 'testsrc2=size=1920x1080:rate=24:duration=4' -f lavfi -i 'sine=frequency=440:sample_rate=48000:duration=4' -c:v libx264 -pix_fmt yuv420p -c:a aac -shortest (Join-Path $TestRoot 'source-hd.mp4')
    if ($LASTEXITCODE -ne 0) { throw 'Local HD synthetic video generation failed' }
    foreach ($Size in @('640x360', '1920x1080')) {
        & $Ffmpeg -hide_banner -loglevel error -f lavfi -i "testsrc2=size=${Size}:rate=24:duration=4" -f lavfi -i 'sine=frequency=440:sample_rate=48000:duration=4' -c:v libx265 -preset ultrafast -pix_fmt yuv420p10le -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc -color_range tv -chroma_sample_location left -x265-params 'log-level=error:hdr10=1:colorprim=9:transfer=16:colormatrix=9:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(40000000,50):max-cll=4000,400' -c:a aac -shortest (Join-Path $TestRoot "hdr-$Size.mp4")
        if ($LASTEXITCODE -ne 0) { throw 'HDR10 synthetic video generation failed' }
        $ProbeJson = & ffprobe.exe -v error -select_streams v:0 -show_entries stream=pix_fmt,color_space,color_transfer,color_primaries,color_range,chroma_location -of json (Join-Path $TestRoot "hdr-$Size.mp4")
        if ($LASTEXITCODE -ne 0) { throw 'HDR10 source metadata probe failed' }
        $Stream = ($ProbeJson | ConvertFrom-Json).streams[0]
        if ($Stream.pix_fmt -ne 'yuv420p10le' -or $Stream.color_space -ne 'bt2020nc' -or $Stream.color_transfer -ne 'smpte2084' -or $Stream.color_primaries -ne 'bt2020' -or $Stream.color_range -ne 'tv' -or $Stream.chroma_location -ne 'left') { throw 'Generated source does not have the required HDR10 VUI' }
        Write-Output "HDR10_SOURCE_VUI_OK size=$Size format=p010 primaries=bt2020 transfer=pq matrix=bt2020nc range=limited chroma=left"
        & $Ffmpeg -hide_banner -loglevel error -f lavfi -i "testsrc2=size=${Size}:rate=24:duration=4" -f lavfi -i 'sine=frequency=440:sample_rate=48000:duration=4' -c:v libx265 -preset ultrafast -pix_fmt yuv420p10le -color_primaries bt2020 -color_trc arib-std-b67 -colorspace bt2020nc -color_range tv -chroma_sample_location left -x265-params 'log-level=error:colorprim=9:transfer=18:colormatrix=9' -c:a aac -shortest (Join-Path $TestRoot "hlg-$Size.mp4")
        if ($LASTEXITCODE -ne 0) { throw 'HLG synthetic video generation failed' }
        $ProbeJson = & ffprobe.exe -v error -select_streams v:0 -show_entries stream=pix_fmt,color_space,color_transfer,color_primaries,color_range,chroma_location -of json (Join-Path $TestRoot "hlg-$Size.mp4")
        if ($LASTEXITCODE -ne 0) { throw 'HLG source metadata probe failed' }
        $Stream = ($ProbeJson | ConvertFrom-Json).streams[0]
        if ($Stream.pix_fmt -ne 'yuv420p10le' -or $Stream.color_space -ne 'bt2020nc' -or $Stream.color_transfer -ne 'arib-std-b67' -or $Stream.color_primaries -ne 'bt2020' -or $Stream.color_range -ne 'tv' -or $Stream.chroma_location -ne 'left') { throw 'Generated source does not have the required HLG VUI' }
        Write-Output "HLG_SOURCE_VUI_OK size=$Size format=p010 primaries=bt2020 transfer=hlg matrix=bt2020nc range=limited chroma=left"
    }
    & (Join-Path $TestRoot 'test_nvvideo_playback.exe') (Join-Path $TestRoot 'source.mp4') (Join-Path $TestRoot 'video_enhancement.dll') "$SdkDirectory\bin\Windows\x64\rel" "$RepoRoot\third_party\video-enhancement-deps\Veyra-2.0.6-NVIDIA\runtime\nvidia-vfg" $TestRoot "$RepoRoot\third_party\video-enhancement-deps\Veyra-2.0.6-NVIDIA\runtime\experimental\nr-original" (Join-Path $TestRoot 'source-hd.mp4') (Join-Path $TestRoot 'hdr-640x360.mp4') (Join-Path $TestRoot 'hdr-1920x1080.mp4') ([int]$HdrOnly.IsPresent) (Join-Path $TestRoot 'hlg-640x360.mp4') (Join-Path $TestRoot 'hlg-1920x1080.mp4')
    if ($LASTEXITCODE -ne 0) { throw 'Native mpv playback/seek/EOF test failed' }
} finally {
    $Absolute = [IO.Path]::GetFullPath($TestRoot)
    if (-not $Absolute.StartsWith($BuildRoot, [StringComparison]::OrdinalIgnoreCase)) { throw "Unexpected test cleanup target: $Absolute" }
    if (Test-Path -LiteralPath $Absolute) { Remove-Item -LiteralPath $Absolute -Recurse -Force }
}
