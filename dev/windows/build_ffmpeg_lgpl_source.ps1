param(
    [string]$MsysPath = "C:\msys64",
    [ValidateSet("x64", "arm64")]
    [string]$Arch = "x64",
    [string]$MsysRepoAlias = "",
    [switch]$Force
)

$ErrorActionPreference = "Stop"

$FfmpegVersion = "8.1.2"
$FfmpegTag = "n$FfmpegVersion"
$FfmpegCommit = "38b88335f99e76ed89ff3c93f877fdefce736c13"
$FfmpegRepository = "https://github.com/FFmpeg/FFmpeg.git"

$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
$ThirdPartyDir = Join-Path $RepoRoot "third_party"
$SourceDir = Join-Path $ThirdPartyDir "ffmpeg-$FfmpegCommit"
$BuildDir = Join-Path $ThirdPartyDir "ffmpeg-build-$FfmpegCommit"
$InstallDir = Join-Path $ThirdPartyDir "ffmpeg-install-$FfmpegCommit"
$BuildStamp = Join-Path $InstallDir "lib\mediastation-ffmpeg-source.sha256"

if ($Arch -eq "arm64") {
    $MsysEnv = "CLANGARM64"
    $PkgPrefix = "mingw-w64-clang-aarch64"
    $TargetArch = "aarch64"
} else {
    $MsysEnv = "CLANG64"
    $PkgPrefix = "mingw-w64-clang-x86_64"
    $TargetArch = "x86_64"
}
$LinkDir = Join-Path $MsysPath "mediastation\$MsysEnv\ffmpeg-$FfmpegCommit"
$MsysBash = Join-Path $MsysPath "usr\bin\bash.exe"

if ($MsysRepoAlias) {
    $MsysRepoAlias = [System.IO.Path]::GetFullPath($MsysRepoAlias).TrimEnd('\')
    if ($MsysRepoAlias -match '[^\x00-\x7F]') {
        throw "The MSYS2 repository alias must be ASCII: $MsysRepoAlias"
    }
    if (-not (Test-Path -LiteralPath $MsysRepoAlias -PathType Container)) {
        throw "The MSYS2 repository alias does not exist: $MsysRepoAlias"
    }
    $AliasItem = Get-Item -LiteralPath $MsysRepoAlias
    if ($AliasItem.Target -and
        ([System.IO.Path]::GetFullPath($AliasItem.Target) -ne [System.IO.Path]::GetFullPath($RepoRoot))) {
        throw "The MSYS2 repository alias does not target this repository: $MsysRepoAlias"
    }
}

function Assert-ManagedPath {
    param([string]$Path, [string]$Root, [string]$Description)
    $ResolvedRoot = [System.IO.Path]::GetFullPath($Root).TrimEnd('\') + '\'
    $ResolvedPath = [System.IO.Path]::GetFullPath($Path)
    if (-not $ResolvedPath.StartsWith(
            $ResolvedRoot,
            [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to modify $Description outside ${ResolvedRoot}: $ResolvedPath"
    }
}

function ConvertTo-MsysPath {
    param([string]$Path)
    $FullPath = [System.IO.Path]::GetFullPath($Path)
    $RepoPrefix = [System.IO.Path]::GetFullPath($RepoRoot).TrimEnd('\') + '\'
    if ($MsysRepoAlias -and $FullPath.StartsWith($RepoPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        $RelativePath = $FullPath.Substring($RepoPrefix.Length)
        $FullPath = Join-Path $MsysRepoAlias $RelativePath
    }
    $FullPath = $FullPath -replace '\\', '/'
    if ($FullPath -match '^([A-Za-z]):(.*)') {
        return '/' + $matches[1].ToLowerInvariant() + $matches[2]
    }
    return $FullPath
}

function Invoke-Msys2 {
    param([string]$Command, [string]$Description)
    Write-Host "$Description..." -ForegroundColor Cyan
    $env:MSYSTEM = $MsysEnv
    $env:CHERE_INVOKING = "1"
    & $MsysBash -l -c $Command
    if ($LASTEXITCODE -ne 0) {
        throw "Failed: $Description"
    }
}

function Assert-FfmpegSource {
    if (-not (Test-Path -LiteralPath (Join-Path $SourceDir "configure") -PathType Leaf)) {
        throw "Pinned FFmpeg source is incomplete: $SourceDir"
    }
    $ActualCommit = (& git -C $SourceDir rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0 -or $ActualCommit -ne $FfmpegCommit) {
        throw "Pinned FFmpeg commit mismatch: expected=$FfmpegCommit actual=$ActualCommit"
    }
    $Dirty = (& git -C $SourceDir status --porcelain) -join "`n"
    if ($LASTEXITCODE -ne 0 -or $Dirty) {
        throw "Pinned FFmpeg source must remain unmodified: $SourceDir"
    }
}

function Assert-FfmpegInstall {
    foreach ($Path in @(
        (Join-Path $InstallDir "bin\avcodec-62.dll"),
        (Join-Path $InstallDir "bin\ffmpeg.exe"),
        (Join-Path $InstallDir "include\libavcodec\avcodec.h"),
        (Join-Path $InstallDir "lib\pkgconfig\libavcodec.pc")
    )) {
        if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
            throw "Pinned LGPL FFmpeg install is incomplete: $Path"
        }
    }

    $ConfigHeader = Join-Path $BuildDir "config.h"
    $Config = Get-Content -LiteralPath $ConfigHeader -Raw
    foreach ($DisabledFeature in @("CONFIG_GPL", "CONFIG_VERSION3", "CONFIG_NONFREE")) {
        if ($Config -notmatch "(?m)^#define $DisabledFeature 0$") {
            throw "FFmpeg license feature is not disabled: $DisabledFeature"
        }
    }
}

function Sync-FfmpegLinkPrefix {
    $ManagedRoot = Join-Path $MsysPath "mediastation\$MsysEnv"
    Assert-ManagedPath -Path $LinkDir -Root $ManagedRoot -Description "FFmpeg link prefix"
    if (Test-Path -LiteralPath $LinkDir) {
        Remove-Item -LiteralPath $LinkDir -Recurse -Force
    }
    New-Item -ItemType Directory -Path $LinkDir -Force | Out-Null
    Get-ChildItem -LiteralPath $InstallDir -Force | ForEach-Object {
        Copy-Item -LiteralPath $_.FullName -Destination $LinkDir -Recurse -Force
    }

    $AsciiPrefix = $LinkDir -replace '\\', '/'
    $Utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    Get-ChildItem -LiteralPath (Join-Path $LinkDir "lib\pkgconfig") -Filter "*.pc" -File |
        ForEach-Object {
            $Content = Get-Content -LiteralPath $_.FullName -Raw
            $Content = $Content -replace '(?m)^prefix=.*$', "prefix=$AsciiPrefix"
            [System.IO.File]::WriteAllText($_.FullName, $Content, $Utf8NoBom)
        }
}

if (-not (Test-Path -LiteralPath $MsysBash -PathType Leaf)) {
    throw "MSYS2 is required to build pinned FFmpeg: $MsysBash"
}

$ContractMaterial = @(
    $FfmpegVersion,
    $FfmpegCommit,
    (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash.ToLowerInvariant()
) -join "`n"
$Hasher = [System.Security.Cryptography.SHA256]::Create()
try {
    $ContractBytes = [System.Text.Encoding]::UTF8.GetBytes($ContractMaterial)
    $SourceContractHash = ([System.BitConverter]::ToString(
        $Hasher.ComputeHash($ContractBytes)
    ) -replace '-', '').ToLowerInvariant()
} finally {
    $Hasher.Dispose()
}

$StampMatches = (Test-Path -LiteralPath $BuildStamp -PathType Leaf) -and
    ((Get-Content -LiteralPath $BuildStamp -Raw).Trim() -eq $SourceContractHash)
if (-not $Force -and $StampMatches) {
    Assert-FfmpegSource
    Assert-FfmpegInstall
    Sync-FfmpegLinkPrefix
    Write-Host "Pinned LGPL FFmpeg already built at $InstallDir" -ForegroundColor Green
    exit 0
}

Invoke-Msys2 @"
pacman -S --needed --noconfirm \
    diffutils \
    make \
    $PkgPrefix-cc \
    $PkgPrefix-pkgconf \
    $PkgPrefix-nasm \
    $PkgPrefix-yasm \
    $PkgPrefix-bzip2 \
    $PkgPrefix-dav1d \
    $PkgPrefix-libiconv \
    $PkgPrefix-zlib
"@ -Description "Installing LGPL FFmpeg build dependencies"

Assert-ManagedPath -Path $SourceDir -Root $ThirdPartyDir -Description "FFmpeg source"
if ($Force -and (Test-Path -LiteralPath $SourceDir)) {
    Remove-Item -LiteralPath $SourceDir -Recurse -Force
}
if (-not (Test-Path -LiteralPath (Join-Path $SourceDir ".git") -PathType Container)) {
    & git clone --depth 1 --branch $FfmpegTag $FfmpegRepository $SourceDir
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to clone pinned FFmpeg $FfmpegTag"
    }
}
Assert-FfmpegSource

foreach ($Path in @($BuildDir, $InstallDir)) {
    Assert-ManagedPath -Path $Path -Root $ThirdPartyDir -Description "FFmpeg build output"
    if (Test-Path -LiteralPath $Path) {
        Remove-Item -LiteralPath $Path -Recurse -Force
    }
}
New-Item -ItemType Directory -Path $BuildDir -Force | Out-Null

$MsysSourceDir = ConvertTo-MsysPath $SourceDir
$MsysBuildDir = ConvertTo-MsysPath $BuildDir
$MsysInstallDir = ConvertTo-MsysPath $InstallDir

Invoke-Msys2 @"
cd '$MsysBuildDir' && \
'$MsysSourceDir/configure' \
    --prefix='$MsysInstallDir' \
    --target-os=mingw32 \
    --arch=$TargetArch \
    --cc=clang \
    --cxx=clang++ \
    --enable-shared \
    --disable-static \
    --disable-debug \
    --disable-doc \
    --disable-ffplay \
    --disable-autodetect \
    --disable-gpl \
    --disable-version3 \
    --disable-nonfree \
    --enable-bzlib \
    --enable-iconv \
    --enable-zlib \
    --enable-libdav1d \
    --enable-dxva2 \
    --enable-d3d11va \
    --enable-d3d12va \
    --enable-schannel \
    --enable-w32threads \
    --extra-libs=-liconv \
    --enable-runtime-cpudetect
"@ -Description "Configuring LGPL FFmpeg $FfmpegVersion"

Invoke-Msys2 "make -C '$MsysBuildDir' -j`$(nproc)" `
    -Description "Building LGPL FFmpeg $FfmpegVersion"
Invoke-Msys2 "make -C '$MsysBuildDir' install" `
    -Description "Installing LGPL FFmpeg $FfmpegVersion"

Assert-FfmpegInstall
Sync-FfmpegLinkPrefix

$MsysLinkDir = ConvertTo-MsysPath $LinkDir
Invoke-Msys2 @"
export PATH='$MsysLinkDir/bin':`$PATH
'$MsysLinkDir/bin/ffmpeg.exe' -hide_banner -L 2>&1 | grep -q 'GNU Lesser General Public'
if '$MsysLinkDir/bin/ffmpeg.exe' -hide_banner -L 2>&1 | grep -q '^GNU General Public'; then
    exit 41
fi
'$MsysLinkDir/bin/ffmpeg.exe' -hide_banner -protocols 2>&1 | grep -q 'https'
'$MsysLinkDir/bin/ffmpeg.exe' -hide_banner -hwaccels 2>&1 | grep -q 'd3d11va'
'$MsysLinkDir/bin/ffmpeg.exe' -hide_banner -decoders 2>&1 | grep -Eq '^[. ][A-Z.]{6} +h264 '
'$MsysLinkDir/bin/ffmpeg.exe' -hide_banner -decoders 2>&1 | grep -Eq '^[. ][A-Z.]{6} +hevc '
'$MsysLinkDir/bin/ffmpeg.exe' -hide_banner -decoders 2>&1 | grep -Eq '^[. ][A-Z.]{6} +av1 '
'$MsysLinkDir/bin/ffmpeg.exe' -hide_banner -decoders 2>&1 | grep -Eq '^[. ][A-Z.]{6} +vp9 '
'$MsysLinkDir/bin/ffmpeg.exe' -hide_banner -decoders 2>&1 | grep -Eq '^[. ][A-Z.]{6} +aac '
"@ -Description "Validating LGPL FFmpeg runtime capabilities"

$Utf8NoBom = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllText($BuildStamp, $SourceContractHash, $Utf8NoBom)
$OutputDll = Join-Path $InstallDir "bin\avcodec-62.dll"
$OutputHash = (Get-FileHash -LiteralPath $OutputDll -Algorithm SHA256).Hash.ToLowerInvariant()
Write-Host "FFMPEG_LGPL_BUILD_OK version=$FfmpegVersion commit=$FfmpegCommit dll=$OutputDll sha256=$OutputHash" -ForegroundColor Green
