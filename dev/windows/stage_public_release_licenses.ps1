param(
    [Parameter(Mandatory = $true)]
    [string]$OutputDir,
    [string]$MsysPath = "C:\msys64",
    [ValidateSet("x64")]
    [string]$Arch = "x64",
    [Parameter(Mandatory = $true)]
    [string]$SourceArchiveName
)

$ErrorActionPreference = "Stop"

$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
$OutputDir = [System.IO.Path]::GetFullPath($OutputDir)
$LicenseDir = Join-Path $OutputDir "licenses"
$FfmpegCommit = "38b88335f99e76ed89ff3c93f877fdefce736c13"
$LibplaceboCommit = "1733c8601edec161b714e4a799c72a9f5e5aa2f0"
$TensorRtRoot = Join-Path $RepoRoot "third_party\frame-interpolation-runtime\.tensorrt\TensorRT-RTX-1.4.0.76"
$Utf8NoBom = [System.Text.UTF8Encoding]::new($false)

function Copy-RequiredFile {
    param([string]$Source, [string]$Destination)
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
        throw "Required release notice is missing: $Source"
    }
    $Parent = Split-Path -Parent $Destination
    New-Item -ItemType Directory -Path $Parent -Force | Out-Null
    Copy-Item -LiteralPath $Source -Destination $Destination -Force
}

if (-not (Test-Path -LiteralPath $OutputDir -PathType Container)) {
    throw "Release output does not exist: $OutputDir"
}
if (Test-Path -LiteralPath $LicenseDir) {
    Remove-Item -LiteralPath $LicenseDir -Recurse -Force
}
New-Item -ItemType Directory -Path $LicenseDir -Force | Out-Null

Copy-RequiredFile (Join-Path $RepoRoot "LICENSE") `
    (Join-Path $LicenseDir "MediaStationGo-GPL-2.0.txt")
Copy-RequiredFile (Join-Path $RepoRoot "third_party\mpv\LICENSE.LGPL") `
    (Join-Path $LicenseDir "mpv-LGPL-2.1-or-later.txt")
Copy-RequiredFile (Join-Path $RepoRoot "third_party\ffmpeg-$FfmpegCommit\COPYING.LGPLv2.1") `
    (Join-Path $LicenseDir "FFmpeg-LGPL-2.1.txt")
Copy-RequiredFile (Join-Path $RepoRoot "third_party\libplacebo-$LibplaceboCommit\LICENSE") `
    (Join-Path $LicenseDir "libplacebo-LGPL-2.1-or-later.txt")
Copy-RequiredFile (Join-Path $RepoRoot "resources\win\licenses\CEF-LICENSE.txt") `
    (Join-Path $LicenseDir "CEF-LICENSE.txt")
Copy-RequiredFile (Join-Path $RepoRoot "resources\win\licenses\RIFE-LICENSE.txt") `
    (Join-Path $LicenseDir "RIFE-LICENSE.txt")
Copy-RequiredFile (Join-Path $RepoRoot "resources\win\licenses\NVIDIA-RUNTIME-NOTICE.txt") `
    (Join-Path $LicenseDir "NVIDIA-RUNTIME-NOTICE.txt")
Copy-RequiredFile (Join-Path $TensorRtRoot "doc\Acknowledgements.txt") `
    (Join-Path $LicenseDir "TensorRT-RTX-Acknowledgements.txt")
Copy-RequiredFile (Join-Path $TensorRtRoot "doc\README.txt") `
    (Join-Path $LicenseDir "TensorRT-RTX-README.txt")

$CefCredits = @(Get-ChildItem (Join-Path $RepoRoot ".cache\cef") -Recurse `
    -Filter "CREDITS.html" -File -ErrorAction SilentlyContinue)
if ($CefCredits.Count -ne 1) {
    throw "Expected exactly one pinned CEF CREDITS.html, found $($CefCredits.Count)"
}
Copy-RequiredFile $CefCredits[0].FullName (Join-Path $LicenseDir "CEF-CREDITS.html")

$CargoAbout = Get-Command cargo-about -ErrorAction SilentlyContinue
if (-not $CargoAbout) {
    throw "cargo-about is required: cargo install cargo-about --locked --features cli"
}
& $CargoAbout.Source generate `
    --manifest-path (Join-Path $RepoRoot "src\jfn_rust\Cargo.toml") `
    --config (Join-Path $RepoRoot "about.toml") `
    --target "x86_64-pc-windows-msvc" `
    --locked `
    --fail `
    --output-file (Join-Path $LicenseDir "Rust-third-party-licenses.html") `
    (Join-Path $RepoRoot "dev\windows\rust-third-party-licenses.hbs")
if ($LASTEXITCODE -ne 0) {
    throw "Failed to generate Rust third-party licenses"
}

$MsysEnv = "clang64"
$Pacman = Join-Path $MsysPath "usr\bin\pacman.exe"
$MsysBin = Join-Path $MsysPath "$MsysEnv\bin"
if (-not (Test-Path -LiteralPath $Pacman -PathType Leaf)) {
    throw "MSYS2 pacman is missing: $Pacman"
}

$PinnedRuntimeNames = @(
    'avcodec-62.dll', 'avdevice-62.dll', 'avfilter-11.dll', 'avformat-62.dll',
    'avutil-60.dll', 'swresample-6.dll', 'swscale-9.dll', 'libmpv-2.dll',
    'libplacebo-364.dll', 'rife_runtime.dll', 'cudart64_12.dll',
    'tensorrt_rtx_1_4.dll', 'tensorrt_onnxparser_rtx_1_4.dll'
)
$MsysDllPaths = Get-ChildItem -LiteralPath $OutputDir -Filter "*.dll" -File |
    Where-Object {
        $_.Name -notin $PinnedRuntimeNames -and
        (Test-Path -LiteralPath (Join-Path $MsysBin $_.Name) -PathType Leaf)
    } |
    ForEach-Object { "/$MsysEnv/bin/$($_.Name)" }

$PackageNames = @()
if ($MsysDllPaths) {
    $env:LANG = "C"
    $PackageNames = @(& $Pacman -Qoq @MsysDllPaths 2>$null | Sort-Object -Unique)
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to resolve MSYS2 runtime package ownership"
    }
}

$NativeInventory = [System.Text.StringBuilder]::new()
[void]$NativeInventory.AppendLine("MediaStationGo native runtime package inventory")
[void]$NativeInventory.AppendLine("Generated from the installed MSYS2 package database.")
[void]$NativeInventory.AppendLine("Complete license texts are in the MSYS2-license-texts directory.")
[void]$NativeInventory.AppendLine("")
foreach ($PackageName in $PackageNames) {
    $Info = (& $Pacman -Qi $PackageName 2>$null) -join "`n"
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to read MSYS2 metadata for $PackageName"
    }
    foreach ($Field in @("Name", "Version", "URL", "Licenses")) {
        $Match = [regex]::Match($Info, "(?m)^$Field\s*:\s*(.+)$")
        if (-not $Match.Success) {
            throw "MSYS2 package metadata is missing ${Field}: $PackageName"
        }
        [void]$NativeInventory.AppendLine("${Field}: $($Match.Groups[1].Value.Trim())")
    }
    [void]$NativeInventory.AppendLine("")
}
[System.IO.File]::WriteAllText(
    (Join-Path $LicenseDir "MSYS2-native-packages.txt"),
    $NativeInventory.ToString(),
    $Utf8NoBom
)

$NativeLicenseDir = Join-Path $LicenseDir "MSYS2-license-texts"
New-Item -ItemType Directory -Path $NativeLicenseDir -Force | Out-Null
$PackageLicenseCounts = @{}
foreach ($PackageName in $PackageNames) {
    $PackageLicenseCounts[$PackageName] = 0
}

if ($PackageNames) {
    $PackageFiles = @(& $Pacman -Ql @PackageNames 2>$null)
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to enumerate MSYS2 package files"
    }
    $LicensePrefix = "/$MsysEnv/share/licenses/"
    foreach ($PackageFile in $PackageFiles) {
        $Match = [regex]::Match($PackageFile, '^(\S+)\s+(/\S.*)$')
        if (-not $Match.Success) {
            throw "Unexpected pacman file-list output: $PackageFile"
        }
        $PackageName = $Match.Groups[1].Value
        $MsysPathName = $Match.Groups[2].Value
        if (-not $MsysPathName.StartsWith($LicensePrefix, [System.StringComparison]::Ordinal) -or
            $MsysPathName.EndsWith('/', [System.StringComparison]::Ordinal)) {
            continue
        }

        $RelativeLicensePath = $MsysPathName.Substring($LicensePrefix.Length)
        $WindowsSource = Join-Path $MsysPath ($MsysPathName.TrimStart('/').Replace('/', '\'))
        $WindowsDestination = Join-Path `
            (Join-Path $NativeLicenseDir $PackageName) `
            $RelativeLicensePath.Replace('/', '\')
        Copy-RequiredFile $WindowsSource $WindowsDestination
        $PackageLicenseCounts[$PackageName]++
    }
}

$SupplementalLicenseFiles = @{
    'mingw-w64-clang-x86_64-libass' = @(
        (Join-Path $RepoRoot 'resources\win\licenses\libass-ISC.txt'),
        'libass\COPYING'
    )
    'mingw-w64-clang-x86_64-zimg' = @(
        (Join-Path $MsysPath "$MsysEnv\share\doc\zimg\COPYING"),
        'zimg\COPYING'
    )
}
foreach ($PackageName in $PackageNames) {
    if ($PackageLicenseCounts[$PackageName] -gt 0) {
        continue
    }
    if (-not $SupplementalLicenseFiles.ContainsKey($PackageName)) {
        throw "No license text was found for MSYS2 runtime package: $PackageName"
    }
    $Supplement = $SupplementalLicenseFiles[$PackageName]
    Copy-RequiredFile $Supplement[0] `
        (Join-Path (Join-Path $NativeLicenseDir $PackageName) $Supplement[1])
    $PackageLicenseCounts[$PackageName]++
}

$Commit = (& git -C $RepoRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) {
    throw "Failed to resolve source commit"
}
$SourceOffer = @"
MediaStationGo source code

This binary distribution is built from commit:
$Commit

Complete corresponding source is distributed next to the installer as:
$SourceArchiveName

Public source repository:
https://github.com/timefunnel/MediaStationGo-Windows

The source archive contains the application source, the pinned mpv submodule,
the pinned FFmpeg 8.1.2 source, the patched libplacebo 7.364.0 source, and the
build scripts needed to reproduce the native libraries. Build-time and
proprietary SDK archives are not relicensed by this offer.
"@
[System.IO.File]::WriteAllText(
    (Join-Path $OutputDir "SOURCE-OFFER.txt"),
    $SourceOffer,
    $Utf8NoBom
)

Write-Host "PUBLIC_RELEASE_LICENSES_OK output=$LicenseDir packages=$($PackageNames.Count)" -ForegroundColor Green
