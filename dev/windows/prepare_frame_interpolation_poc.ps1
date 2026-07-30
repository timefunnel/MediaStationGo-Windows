param(
    [Parameter(Mandatory = $true)]
    [string]$TensorRtRtxArchive,
    [string]$CudaRuntimeDll,
    [switch]$AcceptNvidiaLicense,
    [string]$MsysPath = "C:\msys64",
    [string]$RuntimeDir,
    [switch]$Force
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
$ThirdPartyRoot = Join-Path $RepoRoot "third_party"
if (-not $RuntimeDir) {
    $RuntimeDir = Join-Path $ThirdPartyRoot "frame-interpolation-runtime"
}
$RuntimeDir = [System.IO.Path]::GetFullPath($RuntimeDir)
$AllowedRoot = [System.IO.Path]::GetFullPath($ThirdPartyRoot) + [System.IO.Path]::DirectorySeparatorChar
if (-not $RuntimeDir.StartsWith($AllowedRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "RuntimeDir must stay under $ThirdPartyRoot"
}
if (-not $AcceptNvidiaLicense) {
    throw "Review and accept the NVIDIA TensorRT-RTX license, then rerun with -AcceptNvidiaLicense"
}

$TensorRtRtxArchive = (Resolve-Path -LiteralPath $TensorRtRtxArchive).Path
if ($CudaRuntimeDll) {
    $CudaRuntimeDll = (Resolve-Path -LiteralPath $CudaRuntimeDll).Path
    if ([System.IO.Path]::GetFileName($CudaRuntimeDll) -notmatch '^cudart64_[0-9]+\.dll$') {
        throw "CudaRuntimeDll must be an NVIDIA CUDA Runtime DLL named cudart64_<version>.dll"
    }
}
$SevenZip = "C:\Program Files\7-Zip\7z.exe"
if (-not (Test-Path $SevenZip)) {
    throw "7-Zip is required at $SevenZip"
}
$MsysBash = Join-Path $MsysPath "usr\bin\bash.exe"
$MsysBin = Join-Path $MsysPath "clang64\bin"
$MsysPythonLib = Join-Path $MsysPath "clang64\lib\python3.14"
if (-not (Test-Path $MsysBash)) {
    throw "MSYS2 is required at $MsysPath"
}

$PinnedAssets = @(
    @{
        Name = "VSTRT-RTX-Windows-x64.v15.16.7z"
        Url = "https://github.com/AmusementClub/vs-mlrt/releases/download/v15.16/VSTRT-RTX-Windows-x64.v15.16.7z"
        Sha256 = "d2d311b09635d6681285aa4eb30030b953ed243b49c6d842c490d32c338ca303"
    },
    @{
        Name = "scripts.v15.16.7z"
        Url = "https://github.com/AmusementClub/vs-mlrt/releases/download/v15.16/scripts.v15.16.7z"
        Sha256 = "d07dae0a00cb8dbf4f00358f640f630ff5d933de44d27050e7acce4f31cc3560"
    },
    @{
        Name = "rife_v4.26.7z"
        Url = "https://github.com/AmusementClub/vs-mlrt/releases/download/external-models/rife_v4.26.7z"
        Sha256 = "dfdabd84a2a3db773f87604b8cc255e94a6a72f13550d910ccd3b4ee2606cd4f"
    },
    @{
        Name = "onnxconverter_common-1.16.0-py2.py3-none-any.whl"
        Url = "https://files.pythonhosted.org/packages/4a/67/8dca1868a6e226f8d3f7d666cb6a48b79a60aad5267b16b24627cd8d9eb8/onnxconverter_common-1.16.0-py2.py3-none-any.whl"
        Sha256 = "df39ee96f17fff119dff10dd245467651b60b9e8a96020eb93402239794852f7"
    }
)

function Assert-FileHash {
    param([string]$Path, [string]$Expected)
    $Actual = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($Actual -ne $Expected) {
        throw "SHA-256 mismatch for $Path. Expected $Expected, got $Actual"
    }
}

function Get-PinnedAsset {
    param([hashtable]$Asset, [string]$Destination)
    if (Test-Path -LiteralPath $Destination) {
        try {
            Assert-FileHash $Destination $Asset.Sha256
            return
        } catch {
            Remove-Item -LiteralPath $Destination -Force
        }
    }
    $SharedCache = Join-Path $ThirdPartyRoot $Asset.Name
    if (Test-Path -LiteralPath $SharedCache) {
        Assert-FileHash $SharedCache $Asset.Sha256
        Copy-Item -LiteralPath $SharedCache -Destination $Destination
        return
    }
    $CurlArgs = @("--fail", "--location", "--retry", "3", "--output", $Destination)
    $Proxy = git config --get https.proxy
    if ($Proxy) {
        $CurlArgs += @("--proxy", $Proxy)
    }
    $CurlArgs += $Asset.Url
    & curl.exe @CurlArgs
    if ($LASTEXITCODE -ne 0) {
        throw "Download failed: $($Asset.Url)"
    }
    Assert-FileHash $Destination $Asset.Sha256
}

function Invoke-Msys2 {
    param([string]$Command, [string]$Description)
    Write-Host "$Description..." -ForegroundColor Cyan
    $env:MSYSTEM = "CLANG64"
    $env:CHERE_INVOKING = "1"
    & $MsysBash -l -c $Command
    if ($LASTEXITCODE -ne 0) {
        throw "Failed: $Description"
    }
}

$GpuOutput = & nvidia-smi --query-gpu=name --format=csv,noheader 2>$null
$NvidiaExitCode = $LASTEXITCODE
$Gpu = $GpuOutput | Select-Object -First 1
if ($NvidiaExitCode -ne 0 -or $Gpu -notmatch "NVIDIA GeForce RTX") {
    throw "An NVIDIA GeForce RTX GPU and working NVIDIA driver are required"
}

Invoke-Msys2 @"
pacman -S --needed --noconfirm \
    mingw-w64-clang-x86_64-vapoursynth \
    mingw-w64-clang-x86_64-python-numpy \
    mingw-w64-clang-x86_64-python-onnx
"@ -Description "Installing pinned VapourSynth R65 and Python dependencies"

if ($Force -and (Test-Path -LiteralPath $RuntimeDir)) {
    Remove-Item -LiteralPath $RuntimeDir -Recurse -Force
}
if (Test-Path -LiteralPath $RuntimeDir) {
    throw "$RuntimeDir already exists. Use -Force to rebuild it"
}

$ArchiveDir = Join-Path $RuntimeDir ".archives"
$BinDir = Join-Path $RuntimeDir "bin"
$PythonLibDir = Join-Path $RuntimeDir "lib\python3.14"
$SitePackagesDir = Join-Path $PythonLibDir "site-packages"
$PluginDir = Join-Path $RuntimeDir "vapoursynth\plugins"
$VsMlrtCudaDir = Join-Path $PluginDir "vsmlrt-cuda"
$ModelDir = Join-Path $PluginDir "models\rife"
$ScriptDir = Join-Path $RuntimeDir "scripts"
$EngineDir = Join-Path $RuntimeDir "engines"
$LicenseDir = Join-Path $RuntimeDir "licenses"
foreach ($Directory in @($ArchiveDir, $BinDir, $PythonLibDir, $SitePackagesDir, $PluginDir, $VsMlrtCudaDir, $ModelDir, $ScriptDir, $EngineDir, $LicenseDir)) {
    New-Item -ItemType Directory -Path $Directory -Force | Out-Null
}

foreach ($Asset in $PinnedAssets) {
    Get-PinnedAsset $Asset (Join-Path $ArchiveDir $Asset.Name)
}

$TensorRtHash = "0a050b10158bbe286c90b55b23dffbd3d5096c626b2ee45eccf51322795a3c29"
Assert-FileHash $TensorRtRtxArchive $TensorRtHash
$TensorRtExtract = Join-Path $RuntimeDir ".tensorrt"
& $SevenZip x -y "-o$TensorRtExtract" $TensorRtRtxArchive | Out-Null
$TensorRtRoot = Get-ChildItem -LiteralPath $TensorRtExtract -Directory | Select-Object -First 1
if (-not $TensorRtRoot) {
    throw "TensorRT-RTX archive has no root directory"
}
Copy-Item (Join-Path $TensorRtRoot.FullName "bin\*") $BinDir -Force
Copy-Item (Join-Path $TensorRtRoot.FullName "bin\*") $VsMlrtCudaDir -Force
if ($CudaRuntimeDll) {
    Copy-Item -LiteralPath $CudaRuntimeDll -Destination $BinDir -Force
}

& $SevenZip x -y "-o$PluginDir" (Join-Path $ArchiveDir "VSTRT-RTX-Windows-x64.v15.16.7z") | Out-Null
& $SevenZip x -y "-o$ScriptDir" (Join-Path $ArchiveDir "scripts.v15.16.7z") | Out-Null
$ModelExtract = Join-Path $RuntimeDir ".model"
& $SevenZip x -y "-o$ModelExtract" (Join-Path $ArchiveDir "rife_v4.26.7z") | Out-Null
Copy-Item (Join-Path $ModelExtract "rife\rife_v4.26.onnx") $ModelDir
& $SevenZip x -y "-o$SitePackagesDir" (Join-Path $ArchiveDir "onnxconverter_common-1.16.0-py2.py3-none-any.whl") | Out-Null

Copy-Item (Join-Path $MsysBin "python.exe") $BinDir
Copy-Item (Join-Path $MsysBin "vspipe.exe") $BinDir
Copy-Item (Join-Path $MsysBin "libvapoursynth.dll") $BinDir
Copy-Item (Join-Path $MsysBin "libvapoursynth-script-0.dll") (Join-Path $BinDir "VSScript.dll")
Copy-Item (Join-Path $MsysPythonLib "*") $PythonLibDir -Recurse -Force

$Objdump = Join-Path $MsysBin "objdump.exe"
$Queue = [System.Collections.Generic.Queue[string]]::new()
$Seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
Get-ChildItem -LiteralPath $BinDir -File | ForEach-Object { $Queue.Enqueue($_.FullName) }
Get-ChildItem -LiteralPath $PythonLibDir -Filter "*.pyd" -File -Recurse | ForEach-Object { $Queue.Enqueue($_.FullName) }
while ($Queue.Count -gt 0) {
    $Binary = $Queue.Dequeue()
    if (-not $Seen.Add($Binary)) {
        continue
    }
    $Dependencies = & $Objdump -p $Binary 2>$null | ForEach-Object {
        if ($_ -match "DLL Name:\s*(\S+)") { $matches[1] }
    }
    foreach ($Dependency in $Dependencies) {
        $Source = Join-Path $MsysBin $Dependency
        $Destination = Join-Path $BinDir $Dependency
        if ((Test-Path -LiteralPath $Source) -and -not (Test-Path -LiteralPath $Destination)) {
            Copy-Item $Source $Destination
            $Queue.Enqueue($Destination)
        }
    }
}

$Manifest = [ordered]@{
    schema = 1
    gpu = $Gpu.Trim()
    vapoursynth = "R65"
    python = "3.14"
    vsMlrt = "v15.16"
    backend = "TensorRT-RTX 1.4.0.76"
    cudaRuntime = if ($CudaRuntimeDll) { [System.IO.Path]::GetFileName($CudaRuntimeDll) } else { $null }
    cudaRuntimeSha256 = if ($CudaRuntimeDll) { (Get-FileHash -LiteralPath $CudaRuntimeDll -Algorithm SHA256).Hash.ToLowerInvariant() } else { $null }
    model = "RIFE v4.26"
    modelSha256 = (Get-FileHash (Join-Path $ModelDir "rife_v4.26.onnx") -Algorithm SHA256).Hash.ToLowerInvariant()
    fp16 = $true
    cudaGraph = $true
    streams = 1
    redistributable = $false
}
$ManifestJson = $Manifest | ConvertTo-Json
$ManifestEncoding = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllText(
    (Join-Path $RuntimeDir "runtime-manifest.json"),
    $ManifestJson,
    $ManifestEncoding
)

Write-Host "Frame interpolation PoC runtime prepared at $RuntimeDir" -ForegroundColor Green
Write-Host "This directory is local-only and must not be published before NVIDIA redistribution review." -ForegroundColor Yellow
