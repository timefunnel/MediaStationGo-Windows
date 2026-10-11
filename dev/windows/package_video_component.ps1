param([Parameter(Mandatory)][ValidateSet('rtx-video', 'dlssnr', 'nvidia-vfg')][string]$Component, [string]$OutputDir = 'dist\components')
$ErrorActionPreference = 'Stop'
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
$Dependencies = Join-Path $RepoRoot 'third_party\video-enhancement-deps'
$Veyra = Join-Path $Dependencies 'Veyra-2.0.6-NVIDIA'
if (-not [IO.Path]::IsPathRooted($OutputDir)) { $OutputDir = Join-Path $RepoRoot $OutputDir }
$OutputDir = [IO.Path]::GetFullPath($OutputDir)
$DistRoot = [IO.Path]::GetFullPath((Join-Path $RepoRoot 'dist')) + '\'
if (-not $OutputDir.StartsWith($DistRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Component output must be under this workspace dist directory' }
$Sources = [ordered]@{}
$Hashes = @{}
switch ($Component) {
    'rtx-video' {
        $Version = '1.6.0.0'
        $Sources['nvngx_vsr.dll'] = Join-Path $Dependencies 'RTX-Video-SDK-1.1.0\bin\Windows\x64\rel\nvngx_vsr.dll'
        $Hashes['nvngx_vsr.dll'] = 'c3d88eea5ff7a548edefa66414cf6e77464d0947277c904f324dd23abf58a1ed'
        $Sources['licenses/NVIDIA-LICENSE.pdf'] = Join-Path $Veyra 'licenses\NVIDIA_RTX_VIDEO_SDK_LICENSE.pdf'
    }
    'dlssnr' {
        $Version = '310.8.0.0'
        $Sources['nvngx_dlssnr.dll'] = Join-Path $Veyra 'runtime\experimental\nr-original\nvngx_dlssnr.dll'
        $Hashes['nvngx_dlssnr.dll'] = 'e16bcf15e16e13f527491cdf7845b2fe6521a738d8f7c9c721866a8496e1fc8e'
        $Sources['licenses/NVIDIA-LICENSE.txt'] = Join-Path $Veyra 'licenses\NVIDIA_RTX_SDK_LICENSE.txt'
    }
    'nvidia-vfg' {
        $Version = '1.3.0'
        $Hashes = @{
            'NVVideoEffects.dll' = 'c6b042fc57bdb6bea88c9c52cb951582377d3f1643acf52a52d197195a1ea8f4'
            'NVCVImage.dll' = '689a64586c804468127647b7ed24acc36a9a9780502ffe95ad11aa4fe3286a39'
            'nvVFXVideoFrameGeneration.dll' = '270cf4fff9329908f9770d306cb51b910ee04323af5520cd02a98a2e22aba795'
            'nvngxruntime.dll' = 'd2dbd1993000a2eed2c7ad8cd417ffc738ea180003daacf190e737121869d637'
            'cudart64_12.dll' = '663b5fe65aa6c654b8b085a879f49deb19f2cb033b8f2ff8dffd06cdefe0d1db'
        }
        foreach ($Name in ($Hashes.Keys | Sort-Object)) { $Sources[$Name] = Join-Path $Veyra "runtime\nvidia-vfg\$Name" }
        $Sources['licenses/NVIDIA-Software-License-Agreement.pdf'] = Join-Path $Veyra 'licenses\nvidia-vfg\NVIDIA-Software-License-Agreement-2025.05.05.pdf'
        $Sources['licenses/NVIDIA-Open-Model-License.pdf'] = Join-Path $Veyra 'licenses\nvidia-vfg\NVIDIA-Open-Model-License-Agreements-24-10-2025.pdf'
        $Sources['licenses/NVIDIA-AI-Product-Terms.pdf'] = Join-Path $Veyra 'licenses\nvidia-vfg\product-specific-terms-for-nvidia-ai-products-2025.05.05.pdf'
        $Sources['licenses/THIRD_PARTY.md'] = Join-Path $Veyra 'licenses\nvidia-vfg\THIRD_PARTY.md'
        $Sources['licenses/ThirdPartyLicenses_VFX_SDK.txt'] = Join-Path $Veyra 'licenses\nvidia-vfg\ThirdPartyLicenses_VFX_SDK.txt'
    }
}
$Files = @()
foreach ($Entry in $Sources.GetEnumerator()) {
    $Info = Get-Item -LiteralPath $Entry.Value
    if ($Info.PSIsContainer -or ($Info.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $Info.Length -le 0) { throw "Not a nonempty regular component file: $($Entry.Value)" }
    $Sha = (Get-FileHash -LiteralPath $Entry.Value -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($Hashes.ContainsKey($Entry.Key)) {
        if ($Sha -ne $Hashes[$Entry.Key]) { throw "Runtime is not the pinned NVIDIA original: $($Entry.Key)" }
        $Signature = Get-AuthenticodeSignature -LiteralPath $Entry.Value
        if ($Signature.Status -ne 'Valid' -or $Signature.SignerCertificate.Subject -notmatch 'NVIDIA') { throw "NVIDIA signature invalid: $($Entry.Key)" }
    }
    $Files += [ordered]@{ path = $Entry.Key; bytes = $Info.Length; sha256 = $Sha }
}
if (($Files | Measure-Object bytes -Sum).Sum -gt 768MB) { throw 'Unpacked component exceeds limit' }
$Manifest = [ordered]@{ schema = 1; component = $Component; version = '1'; platform = 'windows-x64'; runtimeAbi = 1; providerVersion = $Version; files = $Files }
$Utf8 = [Text.UTF8Encoding]::new($false)
$ManifestBytes = $Utf8.GetBytes(($Manifest | ConvertTo-Json -Depth 6))
New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
$Destination = Join-Path $OutputDir "MediaStationGo-$Component-x64-v1.zip"
if (Test-Path -LiteralPath $Destination) { throw "Refusing to overwrite existing component: $Destination" }
$Partial = Join-Path $OutputDir ([guid]::NewGuid().ToString('N') + '.partial')
try {
    $Stream = [IO.File]::Open($Partial, [IO.FileMode]::CreateNew)
    $Zip = [IO.Compression.ZipArchive]::new($Stream, [IO.Compression.ZipArchiveMode]::Create)
    try {
        $Out = $Zip.CreateEntry('component-manifest.json').Open()
        try { $Out.Write($ManifestBytes) } finally { $Out.Dispose() }
        foreach ($File in $Sources.GetEnumerator()) {
            $Entry = $Zip.CreateEntry($File.Key, [IO.Compression.CompressionLevel]::Optimal)
            $Input = [IO.File]::OpenRead($File.Value); $Out = $Entry.Open()
            try { $Input.CopyTo($Out) } finally { $Input.Dispose(); $Out.Dispose() }
        }
    } finally { $Zip.Dispose(); $Stream.Dispose() }
    if ((Get-Item -LiteralPath $Partial).Length -gt 512MB) { throw 'Compressed component exceeds limit' }
    Move-Item -LiteralPath $Partial -Destination $Destination
} finally {
    $Absolute = [IO.Path]::GetFullPath($Partial)
    if (-not $Absolute.StartsWith($DistRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Unexpected partial cleanup path' }
    if (Test-Path -LiteralPath $Absolute) { Remove-Item -LiteralPath $Absolute -Force }
}
Write-Host "VIDEO_COMPONENT_PACKAGE_OK component=$Component path=$Destination sha256=$((Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash.ToLowerInvariant())"
Write-Host 'Local package only; no release uploaded. DLSSNR is experimental and does not enable playback.'
