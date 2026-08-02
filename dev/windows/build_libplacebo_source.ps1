param(
    [string]$MsysPath = "C:\msys64",
    [ValidateSet("x64", "arm64")]
    [string]$Arch = "x64",
    [switch]$Force
)

$ErrorActionPreference = "Stop"

$LibplaceboCommit = "1733c8601edec161b714e4a799c72a9f5e5aa2f0"
$LibplaceboVersion = "7.364.0"
$LibplaceboDllName = "libplacebo-364.dll"
$ArchiveSha256 = "eead38de93cee45fa83d82477e2b27dad5015b7ca7ac354c1cbd798064d8282a"
$ArchiveUrl = "https://code.videolan.org/videolan/libplacebo/-/archive/$LibplaceboCommit/libplacebo-$LibplaceboCommit.tar.gz"
$HdrPeakPatchCommit = "2d0979fb54e025e904c7372666fffbf5dae40f66"
$HdrPeakPatch = Join-Path $PSScriptRoot "libplacebo\2d0979f-hdr-peak-source-colorspace.patch"
$SourcePatches = @(
    @{ Id = $HdrPeakPatchCommit; Path = $HdrPeakPatch }
)

$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
$ThirdPartyDir = Join-Path $RepoRoot "third_party"
$ArchivePath = Join-Path $ThirdPartyDir "libplacebo-$LibplaceboCommit.tar.gz"
$SourceDir = Join-Path $ThirdPartyDir "libplacebo-$LibplaceboCommit"
$SourceRelativeDir = "third_party/libplacebo-$LibplaceboCommit"
$BuildDir = Join-Path $ThirdPartyDir "libplacebo-build-$LibplaceboCommit"
$InstallDir = Join-Path $ThirdPartyDir "libplacebo-install-$LibplaceboCommit"
$OutputDll = Join-Path $InstallDir "bin\$LibplaceboDllName"
$PkgConfigFile = Join-Path $InstallDir "lib\pkgconfig\libplacebo.pc"
$BuildStamp = Join-Path $InstallDir "lib\mediastation-libplacebo-source.sha256"
$SourceStamp = Join-Path $SourceDir ".mediastation-libplacebo-source.sha256"

if ($Arch -eq "arm64") {
    $MsysEnv = "CLANGARM64"
    $PkgPrefix = "mingw-w64-clang-aarch64"
} else {
    $MsysEnv = "CLANG64"
    $PkgPrefix = "mingw-w64-clang-x86_64"
}

function Assert-ThirdPartyPath {
    param([string]$Path)
    $Root = [System.IO.Path]::GetFullPath($ThirdPartyDir).TrimEnd('\') + '\'
    $Target = [System.IO.Path]::GetFullPath($Path)
    if (-not $Target.StartsWith($Root, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to modify a path outside third_party: $Target"
    }
}

function Test-GitPatchApplies {
    param(
        [string]$RepositoryDirectory,
        [string]$TargetDirectory,
        [string]$PatchPath,
        [switch]$Reverse
    )
    $Arguments = @("-C", $RepositoryDirectory, "apply", "--directory=$TargetDirectory")
    if ($Reverse) {
        $Arguments += "--reverse"
    }
    $Arguments += @("--check", $PatchPath)
    $PreviousErrorActionPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = "SilentlyContinue"
        & git @Arguments 2>$null
        $Succeeded = $LASTEXITCODE -eq 0
    } finally {
        $ErrorActionPreference = $PreviousErrorActionPreference
    }
    return $Succeeded
}

function ConvertTo-MsysPath {
    param([string]$Path)
    $FullPath = [System.IO.Path]::GetFullPath($Path) -replace '\\', '/'
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

function Assert-LibplaceboInstall {
    if (-not (Test-Path -LiteralPath $OutputDll -PathType Leaf)) {
        throw "Pinned libplacebo DLL is missing: $OutputDll"
    }
    if (-not (Test-Path -LiteralPath $PkgConfigFile -PathType Leaf)) {
        throw "Pinned libplacebo pkg-config file is missing: $PkgConfigFile"
    }
    $VersionLine = Get-Content -LiteralPath $PkgConfigFile |
        Where-Object { $_ -match '^Version:\s*' } |
        Select-Object -First 1
    if (($VersionLine -replace '^Version:\s*', '').Trim() -ne $LibplaceboVersion) {
        throw "Pinned libplacebo version mismatch in ${PkgConfigFile}: $VersionLine"
    }
}

$ContractFiles = @($PSCommandPath) + @($SourcePatches | ForEach-Object { $_.Path })
foreach ($ContractFile in $ContractFiles) {
    if (-not (Test-Path -LiteralPath $ContractFile -PathType Leaf)) {
        throw "Pinned libplacebo source contract input is missing: $ContractFile"
    }
}
$ContractMaterial = $ContractFiles | ForEach-Object {
    (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash.ToLowerInvariant()
}
$Hasher = [System.Security.Cryptography.SHA256]::Create()
try {
    $ContractBytes = [System.Text.Encoding]::UTF8.GetBytes($ContractMaterial -join "`n")
    $SourceContractHash = ([System.BitConverter]::ToString(
        $Hasher.ComputeHash($ContractBytes)
    ) -replace '-', '').ToLowerInvariant()
} finally {
    $Hasher.Dispose()
}
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$StampMatches = (Test-Path -LiteralPath $BuildStamp -PathType Leaf) -and
    ((Get-Content -LiteralPath $BuildStamp -Raw).Trim() -eq $SourceContractHash)
if (-not $Force -and $StampMatches) {
    Assert-LibplaceboInstall
    Write-Host "Pinned libplacebo already built at $InstallDir" -ForegroundColor Green
    exit 0
}

$MsysBash = Join-Path $MsysPath "usr\bin\bash.exe"
if (-not (Test-Path -LiteralPath $MsysBash -PathType Leaf)) {
    throw "MSYS2 is required to build pinned libplacebo: $MsysBash"
}

Invoke-Msys2 @"
pacman -S --needed --noconfirm \
    $PkgPrefix-cc \
    $PkgPrefix-meson \
    $PkgPrefix-pkgconf \
    $PkgPrefix-python-jinja \
    $PkgPrefix-vulkan-headers \
    $PkgPrefix-vulkan-loader \
    $PkgPrefix-shaderc \
    $PkgPrefix-spirv-cross \
    $PkgPrefix-lcms2 \
    $PkgPrefix-libdovi \
    $PkgPrefix-llvm \
    $PkgPrefix-tools
"@ -Description "Installing pinned libplacebo build dependencies"

if (-not (Test-Path -LiteralPath $ArchivePath -PathType Leaf)) {
    $DownloadPath = "$ArchivePath.download"
    try {
        & curl.exe -L --fail --retry 5 --retry-delay 3 --retry-all-errors `
            --output $DownloadPath $ArchiveUrl
        if ($LASTEXITCODE -ne 0) {
            throw "Failed to download pinned libplacebo source archive"
        }
        $DownloadedHash = (Get-FileHash -LiteralPath $DownloadPath -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($DownloadedHash -ne $ArchiveSha256) {
            throw "Downloaded libplacebo archive hash mismatch: expected=$ArchiveSha256 actual=$DownloadedHash"
        }
        Move-Item -LiteralPath $DownloadPath -Destination $ArchivePath
    } finally {
        Remove-Item -LiteralPath $DownloadPath -Force -ErrorAction SilentlyContinue
    }
}

$ActualArchiveHash = (Get-FileHash -LiteralPath $ArchivePath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($ActualArchiveHash -ne $ArchiveSha256) {
    throw "Pinned libplacebo archive hash mismatch: expected=$ArchiveSha256 actual=$ActualArchiveHash path=$ArchivePath"
}

if (-not ((Test-Path -LiteralPath $SourceStamp -PathType Leaf) -and
        ((Get-Content -LiteralPath $SourceStamp -Raw).Trim() -eq $SourceContractHash))) {
    Assert-ThirdPartyPath $SourceDir
    if (Test-Path -LiteralPath $SourceDir) {
        Remove-Item -LiteralPath $SourceDir -Recurse -Force
    }
    & tar.exe -xf $ArchivePath -C $ThirdPartyDir
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to extract pinned libplacebo source archive"
    }
}
if (-not (Test-Path -LiteralPath (Join-Path $SourceDir "meson.build") -PathType Leaf)) {
    throw "Pinned libplacebo source archive did not produce the expected directory: $SourceDir"
}

foreach ($Patch in $SourcePatches) {
    if (Test-GitPatchApplies -RepositoryDirectory $RepoRoot -TargetDirectory $SourceRelativeDir `
            -PatchPath $Patch.Path) {
        & git -C $RepoRoot apply "--directory=$SourceRelativeDir" $Patch.Path
        if ($LASTEXITCODE -ne 0) {
            throw "Failed to apply libplacebo source patch $($Patch.Id)"
        }
        Write-Host "Applied libplacebo source patch $($Patch.Id)" -ForegroundColor Green
    } else {
        if (-not (Test-GitPatchApplies -RepositoryDirectory $RepoRoot `
                -TargetDirectory $SourceRelativeDir -PatchPath $Patch.Path -Reverse)) {
            throw "Pinned libplacebo source does not match source patch $($Patch.Id)"
        }
        Write-Host "Libplacebo source patch already applied: $($Patch.Id)" -ForegroundColor Green
    }
}
[System.IO.File]::WriteAllText($SourceStamp, $SourceContractHash, $Utf8NoBom)

foreach ($Path in @($BuildDir, $InstallDir)) {
    Assert-ThirdPartyPath $Path
    if (Test-Path -LiteralPath $Path) {
        Remove-Item -LiteralPath $Path -Recurse -Force
    }
}

$MsysSourceDir = ConvertTo-MsysPath $SourceDir
$MsysBuildDir = ConvertTo-MsysPath $BuildDir
$MsysInstallDir = ConvertTo-MsysPath $InstallDir
$MsysRegistry = "/$($MsysEnv.ToLowerInvariant())/share/vulkan/registry/vk.xml"

Invoke-Msys2 @"
meson setup '$MsysBuildDir' '$MsysSourceDir' \
    --prefix='$MsysInstallDir' \
    --buildtype=release \
    --default-library=shared \
    -Dbench=false \
    -Dtests=false \
    -Ddemos=false \
    -Dd3d11=enabled \
    -Dvulkan=enabled \
    -Dshaderc=enabled \
    -Dglslang=disabled \
    -Dopengl=disabled \
    -Dvulkan-registry='$MsysRegistry'
"@ -Description "Configuring pinned libplacebo $LibplaceboVersion"

Invoke-Msys2 "meson compile -C '$MsysBuildDir'" `
    -Description "Building pinned libplacebo $LibplaceboVersion"
Invoke-Msys2 "meson install -C '$MsysBuildDir'" `
    -Description "Installing pinned libplacebo $LibplaceboVersion"

Assert-LibplaceboInstall
[System.IO.File]::WriteAllText($BuildStamp, $SourceContractHash, $Utf8NoBom)
$OutputHash = (Get-FileHash -LiteralPath $OutputDll -Algorithm SHA256).Hash.ToLowerInvariant()
Write-Host "LIBPLACEBO_SOURCE_BUILD_OK version=$LibplaceboVersion base=$LibplaceboCommit patches=$HdrPeakPatchCommit dll=$OutputDll sha256=$OutputHash" -ForegroundColor Green
