param([string]$SdkDirectory = '', [string]$OutputDir = 'build\video-enhancement', [switch]$TestGpu)
$ErrorActionPreference = 'Stop'
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
if (-not $SdkDirectory) { $SdkDirectory = Join-Path $RepoRoot 'third_party\video-enhancement-deps\RTX-Video-SDK-1.1.0' }
$SdkDirectory = (Resolve-Path -LiteralPath $SdkDirectory).Path
if (-not [IO.Path]::IsPathRooted($OutputDir)) { $OutputDir = Join-Path $RepoRoot $OutputDir }
$OutputDir = [IO.Path]::GetFullPath($OutputDir)
if (-not $OutputDir.StartsWith($RepoRoot + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Native bridge output must be inside this workspace' }
. (Join-Path $PSScriptRoot 'env.ps1')
New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
& cl.exe /nologo /LD /MD /std:c++17 /EHsc /W4 /WX /external:W0 "/external:I$SdkDirectory\include" (Join-Path $PSScriptRoot 'mpv\video_enhancement.cpp') "/Fo$OutputDir\video_enhancement.obj" "/Fe$OutputDir\video_enhancement.dll" /link "$SdkDirectory\lib\Windows\x64\nvsdk_ngx_d.lib" d3d11.lib dxgi.lib advapi32.lib user32.lib
if ($LASTEXITCODE -ne 0) { throw 'Native video bridge compilation failed' }
if ($TestGpu) {
    & cl.exe /nologo /MD /std:c++17 /EHsc /W4 /WX (Join-Path $PSScriptRoot 'mpv\test_video_enhancement.cpp') "/Fo$OutputDir\test_video_enhancement.obj" "/Fe$OutputDir\test_video_enhancement.exe" /link "$OutputDir\video_enhancement.lib" d3d11.lib dxgi.lib
    if ($LASTEXITCODE -ne 0) { throw 'Native GPU test compilation failed' }
    & "$OutputDir\test_video_enhancement.exe" "$SdkDirectory\bin\Windows\x64\rel" "$RepoRoot\third_party\video-enhancement-deps\Veyra-2.0.6-NVIDIA\runtime\nvidia-vfg" $OutputDir
    if ($LASTEXITCODE -ne 0) { throw 'Native GPU test failed' }
}
