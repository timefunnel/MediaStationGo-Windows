param(
    [string]$BuildDir = "build\mediastation-public-release",
    [string]$InstallDir = "build\mediastation-public-install",
    [string]$DistDir = "dist\public",
    [string]$MsysPath = "C:\msys64",
    [ValidateSet("x64")]
    [string]$Arch = "x64",
    [string]$InnoCompiler = "",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"

$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
$BuildDir = [System.IO.Path]::GetFullPath((Join-Path $RepoRoot $BuildDir))
$InstallDir = [System.IO.Path]::GetFullPath((Join-Path $RepoRoot $InstallDir))
$DistDir = [System.IO.Path]::GetFullPath((Join-Path $RepoRoot $DistDir))
$MpvDir = Join-Path $RepoRoot "third_party\mpv-install"
$Utf8NoBom = [System.Text.UTF8Encoding]::new($false)

function Assert-ManagedPath {
    param([string]$Path, [string]$Root, [string]$Description)
    $ResolvedRoot = [System.IO.Path]::GetFullPath($Root).TrimEnd('\') + '\'
    $ResolvedPath = [System.IO.Path]::GetFullPath($Path)
    if (-not $ResolvedPath.StartsWith($ResolvedRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to modify $Description outside ${ResolvedRoot}: $ResolvedPath"
    }
}

Assert-ManagedPath -Path $BuildDir -Root (Join-Path $RepoRoot "build") -Description "release build"
Assert-ManagedPath -Path $InstallDir -Root (Join-Path $RepoRoot "build") -Description "release install"
Assert-ManagedPath -Path $DistDir -Root (Join-Path $RepoRoot "dist") -Description "release distribution"

if (-not (Test-Path -LiteralPath (Join-Path $MpvDir "lib\libmpv-2.dll") -PathType Leaf)) {
    throw "Pinned libmpv release runtime is missing: $MpvDir"
}

. (Join-Path $PSScriptRoot "env.ps1")

if (-not $SkipBuild) {
    & cargo xtask build --external-mpv $MpvDir --out $BuildDir
    if ($LASTEXITCODE -ne 0) {
        throw "Public release build failed"
    }
} elseif (-not (Test-Path -LiteralPath (Join-Path $BuildDir "jellium-desktop.exe") -PathType Leaf)) {
    throw "SkipBuild was requested but the release build is missing: $BuildDir"
}

if (Test-Path -LiteralPath $InstallDir) {
    Remove-Item -LiteralPath $InstallDir -Recurse -Force
}
if (Test-Path -LiteralPath $DistDir) {
    Remove-Item -LiteralPath $DistDir -Recurse -Force
}
New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
New-Item -ItemType Directory -Path $DistDir -Force | Out-Null

& cargo xtask install --skip-build --external-mpv $MpvDir --out $BuildDir --prefix $InstallDir
if ($LASTEXITCODE -ne 0) {
    throw "Public release staging failed"
}

$Version = (& cargo xtask version).Trim()
if ($LASTEXITCODE -ne 0 -or -not $Version) {
    throw "Failed to determine the release version"
}
$VersionSlug = $Version -replace '[^A-Za-z0-9._-]', '-'
$BaseName = "MediaStationGo-$VersionSlug-windows-$Arch"
$SourceArchiveName = "$BaseName-source.zip"

& (Join-Path $PSScriptRoot "stage_public_release_licenses.ps1") `
    -OutputDir $InstallDir `
    -MsysPath $MsysPath `
    -Arch $Arch `
    -SourceArchiveName $SourceArchiveName
if ($LASTEXITCODE -ne 0) {
    throw "Failed to stage public release licenses"
}

$ForbiddenRuntimeNames = @(
    'VSScript.dll', 'libvapoursynth.dll', 'libvapoursynth-script-0.dll',
    'libpython3.14.dll', 'libplacebo-360.dll', 'libx264-165.dll', 'libx265-216.dll'
)
$Forbidden = Get-ChildItem -LiteralPath $InstallDir -Recurse -File |
    Where-Object { $_.Name -in $ForbiddenRuntimeNames }
if ($Forbidden) {
    throw "Forbidden public runtime files were staged: $($Forbidden.Name -join ', ')"
}
foreach ($RequiredRuntime in @(
    'jellium-desktop.exe', 'mediastation-portable-updater.exe',
    'libmpv-2.dll', 'avcodec-62.dll', 'libplacebo-364.dll',
    'rife_runtime.dll', 'tensorrt_rtx_1_4.dll', 'cudart64_12.dll'
)) {
    if (-not (Test-Path -LiteralPath (Join-Path $InstallDir $RequiredRuntime) -PathType Leaf)) {
        throw "Required public runtime file is missing: $RequiredRuntime"
    }
}
foreach ($Model in @(
    'rife_v4.26_fp16_io.onnx', 'rife_v4.26_scale0.5.onnx',
    'rife_v4.25_lite_fp16_io.onnx'
)) {
    if (-not (Test-Path -LiteralPath (Join-Path $InstallDir "frame-interpolation\models\$Model") -PathType Leaf)) {
        throw "Required RIFE model is missing: $Model"
    }
}

$PortableArchive = Join-Path $DistDir "$BaseName-portable.zip"
$PortableMarkerName = '.mediastation-portable'
$PortableManifestName = '.mediastation-portable-files.txt'
$PortableMarker = Join-Path $InstallDir $PortableMarkerName
$PortableManifest = Join-Path $InstallDir $PortableManifestName
if (Test-Path -LiteralPath $PortableArchive) {
    Remove-Item -LiteralPath $PortableArchive -Force
}
[System.IO.File]::WriteAllText($PortableMarker, "$Version`n", $Utf8NoBom)
$PortableFiles = Get-ChildItem -LiteralPath $InstallDir -Recurse -File -Force |
    ForEach-Object {
        $_.FullName.Substring($InstallDir.Length + 1).Replace('\', '/')
    } |
    Where-Object { $_ -ne $PortableManifestName }
$PortableFiles = @($PortableFiles + $PortableManifestName | Sort-Object -Unique)
[System.IO.File]::WriteAllLines($PortableManifest, $PortableFiles, $Utf8NoBom)
try {
    Compress-Archive -Path (Join-Path $InstallDir '*') -DestinationPath $PortableArchive `
        -CompressionLevel Optimal
} finally {
    Remove-Item -LiteralPath $PortableMarker, $PortableManifest -Force -ErrorAction SilentlyContinue
}
Remove-Item -LiteralPath (Join-Path $InstallDir 'mediastation-portable-updater.exe') -Force

Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$SourceArchive = Join-Path $DistDir $SourceArchiveName
if (Test-Path -LiteralPath $SourceArchive) {
    Remove-Item -LiteralPath $SourceArchive -Force
}
$SourceStream = [System.IO.File]::Open($SourceArchive, [System.IO.FileMode]::CreateNew)
$SourceZip = [System.IO.Compression.ZipArchive]::new(
    $SourceStream,
    [System.IO.Compression.ZipArchiveMode]::Create
)

function Add-SourceFile {
    param([string]$FilePath, [string]$ArchivePath)
    if (-not (Test-Path -LiteralPath $FilePath -PathType Leaf)) {
        return
    }
    $Entry = $SourceZip.CreateEntry(
        $ArchivePath.Replace('\', '/'),
        [System.IO.Compression.CompressionLevel]::Optimal
    )
    $Input = [System.IO.File]::OpenRead($FilePath)
    $Output = $Entry.Open()
    try {
        $Input.CopyTo($Output)
    } finally {
        $Output.Dispose()
        $Input.Dispose()
    }
}

try {
    $ArchiveRoot = "$BaseName-source"
    $MainFiles = & git -C $RepoRoot ls-files
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to enumerate application source"
    }
    foreach ($RelativePath in $MainFiles) {
        Add-SourceFile (Join-Path $RepoRoot $RelativePath) "$ArchiveRoot/$RelativePath"
    }

    $MpvSource = Join-Path $RepoRoot "third_party\mpv"
    $MpvFiles = & git -C $MpvSource ls-files
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to enumerate mpv source"
    }
    foreach ($RelativePath in $MpvFiles) {
        Add-SourceFile (Join-Path $MpvSource $RelativePath) `
            "$ArchiveRoot/third_party/mpv/$RelativePath"
    }

    $FfmpegSource = Join-Path $RepoRoot "third_party\ffmpeg-38b88335f99e76ed89ff3c93f877fdefce736c13"
    $FfmpegFiles = & git -C $FfmpegSource ls-files
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to enumerate FFmpeg source"
    }
    foreach ($RelativePath in $FfmpegFiles) {
        Add-SourceFile (Join-Path $FfmpegSource $RelativePath) `
            "$ArchiveRoot/third_party/ffmpeg/$RelativePath"
    }

    $LibplaceboSource = Join-Path $RepoRoot "third_party\libplacebo-1733c8601edec161b714e4a799c72a9f5e5aa2f0"
    Get-ChildItem -LiteralPath $LibplaceboSource -Recurse -File |
        Where-Object { $_.FullName -notmatch '[\\/]\.git[\\/]' } |
        ForEach-Object {
            $RelativePath = $_.FullName.Substring($LibplaceboSource.Length + 1)
            Add-SourceFile $_.FullName "$ArchiveRoot/third_party/libplacebo/$RelativePath"
        }

    $Commit = (& git -C $RepoRoot rev-parse HEAD).Trim()
    $MpvCommit = (& git -C $MpvSource rev-parse HEAD).Trim()
    $FfmpegCommit = (& git -C $FfmpegSource rev-parse HEAD).Trim()
    & git -C $RepoRoot diff --quiet HEAD --
    if ($LASTEXITCODE -eq 0) {
        $Dirty = $false
    } elseif ($LASTEXITCODE -eq 1) {
        $Dirty = $true
    } else {
        throw "Failed to inspect tracked application changes"
    }
    $State = @"
MediaStationGo source archive state
applicationCommit=$Commit
applicationWorkingTreeDirty=$($Dirty.ToString().ToLowerInvariant())
mpvCommit=$MpvCommit
ffmpegCommit=$FfmpegCommit
libplaceboCommit=1733c8601edec161b714e4a799c72a9f5e5aa2f0
"@
    $StateEntry = $SourceZip.CreateEntry("$ArchiveRoot/SOURCE-STATE.txt")
    $StateWriter = [System.IO.StreamWriter]::new($StateEntry.Open(), $Utf8NoBom)
    try {
        $StateWriter.Write($State)
    } finally {
        $StateWriter.Dispose()
    }
} finally {
    $SourceZip.Dispose()
    $SourceStream.Dispose()
}

if (-not $InnoCompiler) {
    $Candidates = @(
        (Join-Path ${env:ProgramFiles(x86)} "Inno Setup 6\ISCC.exe"),
        (Join-Path $env:LOCALAPPDATA "Programs\Inno Setup 6\ISCC.exe")
    )
    $InnoCompiler = $Candidates | Where-Object {
        $_ -and (Test-Path -LiteralPath $_ -PathType Leaf)
    } | Select-Object -First 1
}
if (-not $InnoCompiler -or -not (Test-Path -LiteralPath $InnoCompiler -PathType Leaf)) {
    throw "Inno Setup 6 is required to build the installer"
}

$InstallerBaseName = "$BaseName-setup"
& $InnoCompiler `
    "/DPackageSource=$InstallDir" `
    "/DOutputDir=$DistDir" `
    "/DAppVersion=$Version" `
    "/DOutputBaseFilename=$InstallerBaseName" `
    (Join-Path $PSScriptRoot "mediastationgo.iss")
if ($LASTEXITCODE -ne 0) {
    throw "Inno Setup failed"
}

$Artifacts = @(
    (Join-Path $DistDir "$InstallerBaseName.exe"),
    $PortableArchive,
    $SourceArchive
)
foreach ($Artifact in $Artifacts) {
    if (-not (Test-Path -LiteralPath $Artifact -PathType Leaf)) {
        throw "Release artifact is missing: $Artifact"
    }
}
$HashLines = $Artifacts | ForEach-Object {
    $Hash = (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash.ToLowerInvariant()
    "$Hash  $([System.IO.Path]::GetFileName($_))"
}
[System.IO.File]::WriteAllLines(
    (Join-Path $DistDir "SHA256SUMS.txt"),
    $HashLines,
    $Utf8NoBom
)

Write-Host "PUBLIC_RELEASE_PACKAGE_OK version=$Version output=$DistDir" -ForegroundColor Green
