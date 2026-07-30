# Build mpv from submodule source using MSYS2 for dependencies
# Produces the custom mpv fork (with gpu-next/Vulkan support) as a Windows DLL
#
# Prerequisites: MSYS2 installed (https://www.msys2.org/)
# Dependencies are installed automatically via pacman.

param(
    [string]$MsysPath = "C:\msys64",
    [ValidateSet("x64", "arm64")]
    [string]$Arch = "x64",
    [switch]$Force
)

$ErrorActionPreference = "Stop"

$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
$OutputDir = Join-Path $RepoRoot "third_party\mpv-install"
$MpvSourceDir = Join-Path $RepoRoot "third_party\mpv"
$NvofMemcSource = Join-Path $PSScriptRoot "mpv\vf_nvofmemc.c"
$NvofMemcPatch = Join-Path $PSScriptRoot "mpv\mpv-nvofmemc.patch"
$D3d11DevicePatch = Join-Path $PSScriptRoot "mpv\mpv-d3d11-device.patch"
$NvofMemcDestination = Join-Path $MpvSourceDir "video\filter\vf_nvofmemc.c"
$RifeRuntimeHeader = Join-Path $PSScriptRoot "mpv\rife_runtime.h"
$RifeRuntimeSource = Join-Path $PSScriptRoot "mpv\rife_runtime.cpp"
$RifeRuntimeBuildScript = Join-Path $PSScriptRoot "build_rife_runtime.ps1"
$RifeRuntimeHeaderDestination = Join-Path $MpvSourceDir "video\filter\rife_runtime.h"
$FrameInterpolationRuntimeDir = Join-Path $RepoRoot "third_party\frame-interpolation-runtime"
$RifeRuntimeBuildDir = Join-Path $RepoRoot "build\rife-runtime"
$NvofApiIncludeDir = Join-Path $RepoRoot "third_party\nvofapi\include"
$RifeTensorRtVersion = "1.4.0.76"
$RifeModelPath = Join-Path $FrameInterpolationRuntimeDir "vapoursynth\plugins\models\rife\rife_v4.26.onnx"
$RifeEngineSpecs = @(
    @{ Width = 1920; Height = 1080 },
    @{ Width = 2304; Height = 1296 },
    @{ Width = 2560; Height = 1440 },
    @{ Width = 3840; Height = 2160 }
)
$NvofMemcPatchApplied = $false
$D3d11DevicePatchApplied = $false
$NvofMemcSourceCreated = $false
$RifeRuntimeHeaderCreated = $false

# MSYS2 environment based on target architecture
if ($Arch -eq "arm64") {
    $MsysEnv = "CLANGARM64"
    $PkgPrefix = "mingw-w64-clang-aarch64"
    $LibMachine = "ARM64"
} else {
    $MsysEnv = "CLANG64"
    $PkgPrefix = "mingw-w64-clang-x86_64"
    $LibMachine = "X64"
}

# Check if already built
$OutputLib = Join-Path $OutputDir "lib\mpv.lib"
if ((Test-Path $OutputLib) -and -not $Force) {
    Write-Host "mpv already built at $OutputDir" -ForegroundColor Green
    Write-Host "Use -Force to rebuild"
    exit 0
}

# Verify mpv submodule exists
if (-not (Test-Path (Join-Path $MpvSourceDir "meson.build"))) {
    Write-Host "mpv submodule not found. Run: git submodule update --init --recursive" -ForegroundColor Red
    exit 1
}

foreach ($RequiredNvofFile in @(
    $NvofMemcSource,
    $NvofMemcPatch,
    $D3d11DevicePatch,
    $RifeRuntimeHeader,
    $RifeRuntimeSource,
    $RifeRuntimeBuildScript,
    $RifeModelPath,
    (Join-Path $FrameInterpolationRuntimeDir "bin\cudart64_12.dll"),
    (Join-Path $FrameInterpolationRuntimeDir ".tensorrt\TensorRT-RTX-1.4.0.76\bin\tensorrt_rtx_1_4.dll"),
    (Join-Path $NvofApiIncludeDir "nvOpticalFlowCommon.h"),
    (Join-Path $NvofApiIncludeDir "nvOpticalFlowD3D11.h")
)) {
    if (-not (Test-Path -LiteralPath $RequiredNvofFile)) {
        throw "NVOF MEMC mpv build input is missing: $RequiredNvofFile"
    }
}

# Check for MSYS2, install if missing
$MsysBash = Join-Path $MsysPath "usr\bin\bash.exe"
if (-not (Test-Path $MsysBash)) {
    Write-Host "MSYS2 not found at $MsysPath, installing..." -ForegroundColor Yellow

    $MsysInstaller = Join-Path $env:TEMP "msys2-installer.exe"
    $MsysUrl = "https://github.com/msys2/msys2-installer/releases/download/nightly-x86_64/msys2-base-x86_64-latest.sfx.exe"
    Write-Host "Downloading MSYS2..."
    & curl.exe -L -o $MsysInstaller $MsysUrl
    if ($LASTEXITCODE -ne 0) { throw "Failed to download MSYS2" }

    Write-Host "Extracting MSYS2 to C:\..."
    & $MsysInstaller -y "-oC:\"
    if ($LASTEXITCODE -ne 0) { throw "Failed to extract MSYS2" }
    Remove-Item $MsysInstaller -ErrorAction SilentlyContinue

    # Initialize MSYS2 (first run triggers setup)
    Write-Host "Initializing MSYS2..."
    $env:MSYSTEM = "MSYS"
    $env:CHERE_INVOKING = "1"
    & $MsysBash -l -c "pacman-key --init && pacman -Syu --noconfirm"

    if (-not (Test-Path $MsysBash)) {
        Write-Host "MSYS2 installation failed" -ForegroundColor Red
        exit 1
    }
    Write-Host "MSYS2 installed" -ForegroundColor Green
}

try {
    & git -C $MpvSourceDir apply --check $D3d11DevicePatch 2>$null
    if ($LASTEXITCODE -eq 0) {
        & git -C $MpvSourceDir apply $D3d11DevicePatch
        if ($LASTEXITCODE -ne 0) {
            throw "Failed to apply the D3D11 device access mpv patch"
        }
        $D3d11DevicePatchApplied = $true
    } else {
        & git -C $MpvSourceDir apply --reverse --check $D3d11DevicePatch 2>$null
        if ($LASTEXITCODE -ne 0) {
            throw "The mpv source does not match the pinned D3D11 device access patch"
        }
    }

    & git -C $MpvSourceDir apply --check $NvofMemcPatch 2>$null
    if ($LASTEXITCODE -eq 0) {
        & git -C $MpvSourceDir apply $NvofMemcPatch
        if ($LASTEXITCODE -ne 0) {
            throw "Failed to apply the NVOF MEMC mpv patch"
        }
        $NvofMemcPatchApplied = $true
    } else {
        & git -C $MpvSourceDir apply --reverse --check $NvofMemcPatch 2>$null
        if ($LASTEXITCODE -ne 0) {
            throw "The mpv source does not match the pinned NVOF MEMC patch"
        }
    }

    if (Test-Path -LiteralPath $NvofMemcDestination) {
        $SourceHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $NvofMemcSource).Hash
        $DestinationHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $NvofMemcDestination).Hash
        if ($SourceHash -ne $DestinationHash) {
            throw "Existing mpv NVOF MEMC filter differs from the pinned source: $NvofMemcDestination"
        }
    } else {
        Copy-Item -LiteralPath $NvofMemcSource -Destination $NvofMemcDestination
        $NvofMemcSourceCreated = $true
    }

    if (Test-Path -LiteralPath $RifeRuntimeHeaderDestination) {
        $HeaderHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $RifeRuntimeHeader).Hash
        $DestinationHeaderHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $RifeRuntimeHeaderDestination).Hash
        if ($HeaderHash -ne $DestinationHeaderHash) {
            throw "Existing mpv RIFE runtime header differs from the pinned source: $RifeRuntimeHeaderDestination"
        }
    } else {
        Copy-Item -LiteralPath $RifeRuntimeHeader -Destination $RifeRuntimeHeaderDestination
        $RifeRuntimeHeaderCreated = $true
    }

& $RifeRuntimeBuildScript -RuntimeDir $FrameInterpolationRuntimeDir `
    -OutputDir $RifeRuntimeBuildDir
if ($LASTEXITCODE -ne 0) {
    throw "Failed to build the RIFE runtime bridge"
}

Write-Host "=== Building mpv from submodule ===" -ForegroundColor Cyan
Write-Host "MSYS2: $MsysPath ($MsysEnv)"
Write-Host "Source: $MpvSourceDir"
Write-Host ""

# Convert Windows path to MSYS2 path (C:\foo\bar -> /c/foo/bar)
function ConvertTo-MsysPath($WinPath) {
    $Resolved = (Resolve-Path $WinPath).Path -replace '\\', '/'
    if ($Resolved -match '^([A-Za-z]):(.*)') {
        '/' + $matches[1].ToLower() + $matches[2]
    } else {
        $Resolved
    }
}

$MsysMpvSource = ConvertTo-MsysPath $MpvSourceDir
$MsysNvofApiInclude = ConvertTo-MsysPath $NvofApiIncludeDir

# Run a command in MSYS2
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

# Install build dependencies
Invoke-Msys2 @"
pacman -S --needed --noconfirm \
    $PkgPrefix-cc \
    $PkgPrefix-meson \
    $PkgPrefix-pkgconf \
    $PkgPrefix-ffmpeg \
    $PkgPrefix-libplacebo \
    $PkgPrefix-libass \
    $PkgPrefix-vulkan-headers \
    $PkgPrefix-vulkan-loader \
    $PkgPrefix-shaderc \
    $PkgPrefix-spirv-cross \
    $PkgPrefix-vapoursynth \
    $PkgPrefix-llvm \
    $PkgPrefix-tools
"@ -Description "Installing MSYS2 dependencies"

# Clean previous build if forcing
$MesonBuildDir = Join-Path $MpvSourceDir "build"
if ($Force -and (Test-Path $MesonBuildDir)) {
    Write-Host "Cleaning previous build..." -ForegroundColor Yellow
    Remove-Item -Recurse -Force $MesonBuildDir
}

# Configure with meson
if (-not (Test-Path (Join-Path $MesonBuildDir "build.ninja"))) {
    Invoke-Msys2 @"
cd '$MsysMpvSource' && \
CFLAGS="-I$MsysNvofApiInclude" \
meson setup build --default-library=shared \
    -Dlibmpv=true \
    -Dcplayer=true \
    -Dlua=disabled \
    -Djavascript=disabled \
    -Dcdda=disabled \
    -Ddvdnav=disabled \
    -Dlibbluray=disabled \
    -Dlibarchive=disabled \
    -Drubberband=disabled \
    -Dvapoursynth=enabled
"@ -Description "Configuring mpv with meson"
} else {
    Write-Host "Meson already configured (use -Force to reconfigure)" -ForegroundColor Yellow
}

# Build
Invoke-Msys2 "cd '$MsysMpvSource' && meson compile -C build" -Description "Building mpv"

# Verify the DLL was produced
$BuiltDll = Join-Path $MesonBuildDir "libmpv-2.dll"
if (-not (Test-Path $BuiltDll)) {
    Write-Host "Build succeeded but libmpv-2.dll not found" -ForegroundColor Red
    Get-ChildItem $MesonBuildDir -Filter "*.dll" -Recurse | ForEach-Object {
        Write-Host "  Found: $($_.FullName)"
    }
    exit 1
}

Write-Host ""
Write-Host "=== Setting up output directory ===" -ForegroundColor Cyan

# Setup output directory (matches EXTERNAL_MPV_DIR layout)
if (Test-Path $OutputDir) {
    Remove-Item -Recurse -Force $OutputDir
}
New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
$LibDir = Join-Path $OutputDir "lib"
$IncludeDir = Join-Path $OutputDir "include"
New-Item -ItemType Directory -Path $LibDir -Force | Out-Null
New-Item -ItemType Directory -Path $IncludeDir -Force | Out-Null

# Copy headers from submodule fork (includes render_vk.h for gpu-next)
Write-Host "Copying headers..."
Copy-Item (Join-Path $MpvSourceDir "include\mpv") (Join-Path $IncludeDir "mpv") -Recurse

# Copy ffmpeg headers — jellium-desktop links libavcodec directly to enumerate
# decoders for the Jellyfin device profile. Mirrors the mpv layout: headers
# under include/, import lib under lib/ alongside mpv.lib.
Write-Host "Copying ffmpeg headers..."
$MsysIncludeDir = Join-Path $MsysPath "$MsysEnv\include"
foreach ($pkg in @("libavcodec", "libavutil")) {
    $src = Join-Path $MsysIncludeDir $pkg
    if (Test-Path $src) {
        Copy-Item $src (Join-Path $IncludeDir $pkg) -Recurse
    } else {
        Write-Host "Missing $src — ffmpeg headers not installed in MSYS2" -ForegroundColor Red
        exit 1
    }
}

# Copy DLL
Write-Host "Copying libmpv-2.dll..."
Copy-Item $BuiltDll $LibDir

Write-Host "Copying RIFE/TensorRT/CUDA runtime DLLs..."
Copy-Item (Join-Path $RifeRuntimeBuildDir "rife_runtime.dll") $LibDir
Copy-Item (Join-Path $FrameInterpolationRuntimeDir "bin\cudart64_12.dll") $LibDir
Copy-Item (Join-Path $FrameInterpolationRuntimeDir ".tensorrt\TensorRT-RTX-$RifeTensorRtVersion\bin\tensorrt_rtx_1_4.dll") $LibDir

function Get-LowerSha256 {
    param([Parameter(Mandatory = $true)][string]$Path)
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Get-TextSha256 {
    param([Parameter(Mandatory = $true)][string]$Value)
    $Hasher = [System.Security.Cryptography.SHA256]::Create()
    try {
        $Bytes = [System.Text.Encoding]::UTF8.GetBytes($Value)
        ([System.BitConverter]::ToString($Hasher.ComputeHash($Bytes)) -replace '-', '').ToLowerInvariant()
    } finally {
        $Hasher.Dispose()
    }
}

Write-Host "Staging keyed RIFE engine cache..."
$GpuOutput = @(& nvidia-smi --query-gpu=name,uuid,driver_version --format=csv,noheader,nounits 2>&1)
$GpuExitCode = $LASTEXITCODE
$GpuLine = $GpuOutput | Select-Object -First 1
if ($GpuExitCode -ne 0 -or -not $GpuLine) {
    $GpuDiagnostic = ($GpuOutput | Out-String).Trim()
    throw "nvidia-smi could not provide the GPU identity for the RIFE engine cache (exit $GpuExitCode): $GpuDiagnostic"
}
$GpuFields = @($GpuLine.ToString().Split(',') | ForEach-Object { $_.Trim() })
if ($GpuFields.Count -ne 3 -or
    @($GpuFields | Where-Object { [string]::IsNullOrWhiteSpace($_) }).Count -gt 0) {
    throw "nvidia-smi returned an invalid GPU identity: $GpuLine"
}
$GpuName, $GpuUuid, $DriverVersion = $GpuFields
$ModelSha256 = Get-LowerSha256 $RifeModelPath
$EngineCacheDir = Join-Path $LibDir "frame-interpolation\engine-cache"
New-Item -ItemType Directory -Path $EngineCacheDir -Force | Out-Null
$EngineEntries = @()
foreach ($Spec in $RifeEngineSpecs) {
    $Width = $Spec.Width
    $Height = $Spec.Height
    $SourceDir = Join-Path $FrameInterpolationRuntimeDir (
        "engines\poc-rife-v4_26-impl1-${Width}x${Height}-scale1_0-fp16")
    $Candidates = @(Get-ChildItem -LiteralPath $SourceDir -Filter "*.engine" -File -ErrorAction SilentlyContinue)
    if ($Candidates.Count -ne 1) {
        throw "Expected exactly one validated RIFE engine in $SourceDir, found $($Candidates.Count)"
    }
    $KeyMaterial = "gpu_uuid=$GpuUuid`ndriver=$DriverVersion`ntensorrt=$RifeTensorRtVersion`nmodel_sha256=$ModelSha256`nwidth=$Width`nheight=$Height`nscale=1.0`nprecision=fp16"
    $EngineKey = Get-TextSha256 $KeyMaterial
    $EngineFile = "$EngineKey.engine"
    $Destination = Join-Path $EngineCacheDir $EngineFile
    Copy-Item -LiteralPath $Candidates[0].FullName -Destination $Destination
    $EngineEntries += [ordered]@{
        width = $Width
        height = $Height
        scale = "1.0"
        precision = "fp16"
        engineKey = $EngineKey
        file = $EngineFile
        sha256 = Get-LowerSha256 $Destination
    }
}
$EngineManifest = [ordered]@{
    schema = 1
    gpuName = $GpuName
    gpuUuid = $GpuUuid
    driverVersion = $DriverVersion
    tensorRtVersion = $RifeTensorRtVersion
    model = "RIFE v4.26"
    modelSha256 = $ModelSha256
    runtimeAbi = 3
    runtimeDll = "rife_runtime.dll"
    cudaRuntimeDll = "cudart64_12.dll"
    tensorRtDll = "tensorrt_rtx_1_4.dll"
    engines = $EngineEntries
}
$ManifestPath = Join-Path $LibDir "frame-interpolation\runtime-manifest.json"
$Utf8NoBom = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllText(
    $ManifestPath,
    ($EngineManifest | ConvertTo-Json -Depth 6),
    $Utf8NoBom)
Write-Host "Staged $($EngineEntries.Count) keyed RIFE engines" -ForegroundColor Green

# Generate MSVC import library
Write-Host "Generating MSVC import library..."

$HasMsvc = $false
if ($env:VSINSTALLDIR -and (Get-Command lib.exe -ErrorAction SilentlyContinue)) {
    $HasMsvc = $true
} else {
    $VsWhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path $VsWhere) {
        $VsPath = & $VsWhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        $VcVars = Join-Path $VsPath "VC\Auxiliary\Build\vcvars64.bat"
        if (Test-Path $VcVars) {
            $TempBat = Join-Path $env:TEMP "jfn_vcvars_mpv.bat"
            Set-Content $TempBat -Value ('@call "' + $VcVars + '"') -Encoding ASCII
            Add-Content $TempBat -Value '@set' -Encoding ASCII
            cmd /c $TempBat | ForEach-Object {
                if ($_ -match "^([^=]+)=(.*)$") {
                    [Environment]::SetEnvironmentVariable($matches[1], $matches[2], "Process")
                }
            }
            Remove-Item $TempBat -ErrorAction SilentlyContinue
            if (Get-Command lib.exe -ErrorAction SilentlyContinue) {
                $HasMsvc = $true
            }
        }
    }
}

if ($HasMsvc) {
    # Use dumpbin + lib.exe (best quality import lib)
    $DllPath = Join-Path $LibDir "libmpv-2.dll"
    $DefFile = Join-Path $LibDir "libmpv-2.def"

    $DumpOutput = & dumpbin /exports $DllPath
    $Exports = $DumpOutput | Where-Object {
        $_ -match "^\s+\d+\s+[A-F0-9]+\s+[A-F0-9]+\s+(\w+)"
    } | ForEach-Object {
        if ($_ -match "^\s+\d+\s+[A-F0-9]+\s+[A-F0-9]+\s+(\w+)") { $matches[1] }
    }

    if ($Exports.Count -gt 0) {
        $DefContent = "LIBRARY libmpv-2`nEXPORTS`n"
        $Exports | ForEach-Object { $DefContent += "    $_`n" }
        Set-Content -Path $DefFile -Value $DefContent

        Push-Location $LibDir
        & lib.exe /def:libmpv-2.def /out:mpv.lib /MACHINE:$LibMachine 2>&1 | Out-Null
        Pop-Location

        if (Test-Path $OutputLib) {
            Write-Host "Generated mpv.lib ($($Exports.Count) exports)" -ForegroundColor Green
        } else {
            Write-Host "lib.exe failed to generate import library" -ForegroundColor Red
            exit 1
        }
    } else {
        Write-Host "No exports found in DLL" -ForegroundColor Red
        exit 1
    }
} else {
    # Fallback: use gendef + dlltool from MSYS2
    Write-Host "MSVC not available, using MSYS2 tools..." -ForegroundColor Yellow
    $MsysLibDir = ConvertTo-MsysPath $LibDir
    Invoke-Msys2 "cd '$MsysLibDir' && gendef libmpv-2.dll && dlltool -d libmpv-2.def -l mpv.lib" `
        -Description "Generating import library with dlltool"

    if (Test-Path $OutputLib) {
        Write-Host "Generated mpv.lib (via dlltool)" -ForegroundColor Green
    } else {
        Write-Host "Failed to generate import library" -ForegroundColor Red
        exit 1
    }
}

# Collect runtime DLL dependencies recursively from MSYS2
Write-Host ""
Write-Host "=== Collecting runtime dependencies ===" -ForegroundColor Cyan
$MsysBinDir = Join-Path $MsysPath "$MsysEnv\bin"
$MsysEnvLower = $MsysEnv.ToLower()
$MsysLibDir = ConvertTo-MsysPath $LibDir

# mpv loads VSScript dynamically, so it does not appear in libmpv's import
# table. Stage it explicitly under the Windows name mpv probes, then let the
# dependency walker collect its Python and C++ runtime dependencies.
$VsScriptSource = Join-Path $MsysBinDir "libvapoursynth-script-0.dll"
$VsCoreSource = Join-Path $MsysBinDir "libvapoursynth.dll"
if (-not (Test-Path $VsScriptSource) -or -not (Test-Path $VsCoreSource)) {
    throw "VapourSynth R65 runtime DLLs are missing from $MsysBinDir"
}
Copy-Item $VsScriptSource (Join-Path $LibDir "VSScript.dll")
Copy-Item $VsCoreSource (Join-Path $LibDir "libvapoursynth.dll")

# Write a helper script to resolve deps recursively, then run it
$DepScript = @"
#!/bin/bash
MSYS_BIN=/$MsysEnvLower/bin
OUT_DIR='$MsysLibDir'
declare -A seen

resolve_deps() {
    local dll=`"`$1`"
    local path=`"`$2`"
    [ -n `"`${seen[`$dll]}`" ] && return
    seen[`$dll]=1
    while read -r dep; do
        if [ -f `"`$MSYS_BIN/`$dep`" ] && [ -z `"`${seen[`$dep]}`" ]; then
            cp -v `"`$MSYS_BIN/`$dep`" `"`$OUT_DIR/`"
            resolve_deps `"`$dep`" `"`$MSYS_BIN/`$dep`"
        fi
    done < <(objdump -p `"`$path`" 2>/dev/null | awk '/DLL Name/ {print `$3}')
}

resolve_deps libmpv-2.dll `"`$OUT_DIR/libmpv-2.dll`"
resolve_deps VSScript.dll `"`$OUT_DIR/VSScript.dll`"
resolve_deps libvapoursynth.dll `"`$OUT_DIR/libvapoursynth.dll`"
"@

$DepScriptPath = Join-Path $MesonBuildDir "resolve_deps.sh"
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText($DepScriptPath, $DepScript, $Utf8NoBom)
$MsysDepScript = ConvertTo-MsysPath $DepScriptPath

Invoke-Msys2 "bash '$MsysDepScript'" -Description "Copying MSYS2 runtime dependencies"

# Count what we copied
$DllCount = (Get-ChildItem $LibDir -Filter "*.dll").Count
Write-Host "Collected $DllCount DLLs total" -ForegroundColor Green

# Generate avcodec.lib import library so MSVC can link libavcodec at build
# time. The avcodec-NN.dll was pulled in by the runtime-dep walker above.
Write-Host "Generating avcodec import library..."
$AvcodecDll = Get-ChildItem $LibDir -Filter "avcodec-*.dll" | Select-Object -First 1
if (-not $AvcodecDll) {
    Write-Host "avcodec-*.dll not found in $LibDir — runtime collection failed" -ForegroundColor Red
    exit 1
}
$AvcodecBase = [System.IO.Path]::GetFileNameWithoutExtension($AvcodecDll.Name)
if ($HasMsvc) {
    $DefFile = Join-Path $LibDir "$AvcodecBase.def"
    $DumpOutput = & dumpbin /exports $AvcodecDll.FullName
    $Exports = $DumpOutput | ForEach-Object {
        if ($_ -match "^\s+\d+\s+[A-F0-9]+\s+[A-F0-9]+\s+(\w+)") { $matches[1] }
    }
    if ($Exports.Count -eq 0) {
        Write-Host "No exports found in $($AvcodecDll.Name)" -ForegroundColor Red
        exit 1
    }
    $DefContent = "LIBRARY $AvcodecBase`nEXPORTS`n"
    $Exports | ForEach-Object { $DefContent += "    $_`n" }
    Set-Content -Path $DefFile -Value $DefContent
    Push-Location $LibDir
    & lib.exe /def:"$AvcodecBase.def" /out:avcodec.lib /MACHINE:$LibMachine 2>&1 | Out-Null
    Pop-Location
} else {
    $MsysLibDir = ConvertTo-MsysPath $LibDir
    Invoke-Msys2 "cd '$MsysLibDir' && gendef '$($AvcodecDll.Name)' && dlltool -d '$AvcodecBase.def' -l avcodec.lib" `
        -Description "Generating avcodec.lib with dlltool"
}
if (-not (Test-Path (Join-Path $LibDir "avcodec.lib"))) {
    Write-Host "Failed to generate avcodec.lib" -ForegroundColor Red
    exit 1
}
Write-Host "Generated avcodec.lib" -ForegroundColor Green

Write-Host ""
Write-Host "=== Build complete ===" -ForegroundColor Green
Write-Host "Output: $OutputDir"
Write-Host ""
Write-Host "Contents:"
Get-ChildItem $OutputDir -Recurse -File | ForEach-Object {
    Write-Host "  $($_.FullName.Substring($OutputDir.Length + 1))"
}
} finally {
    if ($RifeRuntimeHeaderCreated -and (Test-Path -LiteralPath $RifeRuntimeHeaderDestination)) {
        Remove-Item -LiteralPath $RifeRuntimeHeaderDestination -Force
    }
    if ($NvofMemcSourceCreated -and (Test-Path -LiteralPath $NvofMemcDestination)) {
        Remove-Item -LiteralPath $NvofMemcDestination -Force
    }
    if ($NvofMemcPatchApplied) {
        & git -C $MpvSourceDir apply --reverse $NvofMemcPatch
        if ($LASTEXITCODE -ne 0) {
            Write-Error "Failed to restore the mpv source after the NVOF MEMC build"
        }
    }
    if ($D3d11DevicePatchApplied) {
        & git -C $MpvSourceDir apply --reverse $D3d11DevicePatch
        if ($LASTEXITCODE -ne 0) {
            Write-Error "Failed to restore the mpv source after the D3D11 device access build"
        }
    }
}
