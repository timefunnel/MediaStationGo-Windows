param([string]$SdkDirectory = '')
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
    & (Join-Path $TestRoot 'test_nvvideo_playback.exe') (Join-Path $TestRoot 'source.mp4') (Join-Path $TestRoot 'video_enhancement.dll') "$SdkDirectory\bin\Windows\x64\rel" "$RepoRoot\third_party\video-enhancement-deps\Veyra-2.0.6-NVIDIA\runtime\nvidia-vfg" $TestRoot "$RepoRoot\third_party\video-enhancement-deps\Veyra-2.0.6-NVIDIA\runtime\experimental\nr-original" (Join-Path $TestRoot 'source-hd.mp4')
    if ($LASTEXITCODE -ne 0) { throw 'Native mpv playback/seek/EOF test failed' }
} finally {
    $Absolute = [IO.Path]::GetFullPath($TestRoot)
    if (-not $Absolute.StartsWith($BuildRoot, [StringComparison]::OrdinalIgnoreCase)) { throw "Unexpected test cleanup target: $Absolute" }
    if (Test-Path -LiteralPath $Absolute) { Remove-Item -LiteralPath $Absolute -Recurse -Force }
}
