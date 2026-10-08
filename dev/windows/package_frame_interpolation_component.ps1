param(
    [string]$RuntimeLibDir = "",
    [string]$OutputDir = "dist\components"
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
if (-not $RuntimeLibDir) { $RuntimeLibDir = Join-Path $RepoRoot "third_party\mpv-install\lib" }
$RuntimeLibDir = (Resolve-Path -LiteralPath $RuntimeLibDir).Path
if (-not [System.IO.Path]::IsPathRooted($OutputDir)) { $OutputDir = Join-Path $RepoRoot $OutputDir }
$OutputDir = [System.IO.Path]::GetFullPath($OutputDir)
$DistPrefix = [System.IO.Path]::GetFullPath((Join-Path $RepoRoot "dist")).TrimEnd('\') + '\'
if (-not $OutputDir.StartsWith($DistPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Component output must remain below the repository dist directory"
}
$AssetName = 'MediaStationGo-frame-interpolation-x64-v1.zip'
$Destination = Join-Path $OutputDir $AssetName
if (Test-Path -LiteralPath $Destination) { throw "Refusing to overwrite an existing component package: $Destination" }
$RuntimeManifest = Get-Content -LiteralPath (Join-Path $RuntimeLibDir 'frame-interpolation\runtime-manifest.json') -Raw -Encoding utf8 | ConvertFrom-Json
if ($RuntimeManifest.schema -ne 4 -or $RuntimeManifest.runtimeAbi -ne 8 -or
    $RuntimeManifest.tensorRtVersion -ne '1.4.0.76' -or $RuntimeManifest.engineBuilder -ne 'rife_engine_builder.exe') {
    throw "Component input does not match the validated RIFE runtime tuple"
}
$FrozenModels = @{
    'rife_v4.26_fp16_io.onnx' = '534aeae1a47bd7585defc6902fd6135b71545f85522626197eb109888ab7dfc2'
    'rife_v4.26_scale0.5.onnx' = '212696b5befa040ab1989003dcc89b90903fbbdce21f46e0883cd5ab1bf91ca4'
    'rife_v4.25_lite_fp16_io.onnx' = '9b9209ebce65b666c1f24ba9ee203bcacb1b3054b0bcc7617fbea7b6736a19b4'
}
if ($RuntimeManifest.models.Count -ne 3) { throw "Component must contain the three validated models" }
foreach ($Model in $RuntimeManifest.models) {
    if ($FrozenModels[$Model.onnxFile] -ne $Model.onnxSha256) { throw "Unexpected model or model SHA: $($Model.onnxFile)" }
}
$Sources = [ordered]@{}
foreach ($Name in @('rife_runtime.dll', 'cudart64_12.dll', 'tensorrt_rtx_1_4.dll',
    'tensorrt_onnxparser_rtx_1_4.dll', 'rife_engine_builder.exe', 'frame-interpolation/runtime-manifest.json')) {
    $Sources[$Name] = Join-Path $RuntimeLibDir $Name
}
foreach ($Name in $FrozenModels.Keys | Sort-Object) { $Sources["frame-interpolation/models/$Name"] = Join-Path $RuntimeLibDir "frame-interpolation\models\$Name" }
$Sources['licenses/MediaStationGo-GPL-2.0.txt'] = Join-Path $RepoRoot 'LICENSE'
foreach ($Name in @('RIFE-LICENSE.txt', 'RIFE-ENGINE-BUILDER-MIT.txt', 'NVIDIA-RUNTIME-NOTICE.txt')) {
    $Sources["licenses/$Name"] = Join-Path $RepoRoot "resources\win\licenses\$Name"
}
$TensorRtRoot = Join-Path $RepoRoot 'third_party\frame-interpolation-runtime\.tensorrt\TensorRT-RTX-1.4.0.76'
$Sources['licenses/TensorRT-RTX-Acknowledgements.txt'] = Join-Path $TensorRtRoot 'doc\Acknowledgements.txt'
$Sources['licenses/TensorRT-RTX-README.txt'] = Join-Path $TensorRtRoot 'doc\README.txt'
$Files = @()
foreach ($Entry in $Sources.GetEnumerator()) {
    $Info = Get-Item -LiteralPath $Entry.Value
    if ($Info.PSIsContainer -or ($Info.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -or $Info.Length -le 0) {
        throw "Component input must be a nonempty regular file: $($Entry.Value)"
    }
    $Sha = (Get-FileHash -LiteralPath $Entry.Value -Algorithm SHA256).Hash.ToLowerInvariant()
    $Name = Split-Path -Leaf $Entry.Key
    if ($FrozenModels.ContainsKey($Name) -and $Sha -ne $FrozenModels[$Name]) { throw "Model integrity failed: $Name" }
    $Files += [ordered]@{ path = $Entry.Key; bytes = $Info.Length; sha256 = $Sha }
}
if (($Files | Measure-Object -Property bytes -Sum).Sum -gt 768MB) { throw "Component exceeds unpacked size limit" }
$Manifest = [ordered]@{ schema = 1; component = 'rife'; version = '1'; platform = 'windows-x64'; runtimeAbi = 8; tensorRtVersion = '1.4.0.76'; files = $Files }
$ManifestBytes = [System.Text.UTF8Encoding]::new($false).GetBytes(($Manifest | ConvertTo-Json -Depth 8))
New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
$Partial = Join-Path $OutputDir ("component-" + [guid]::NewGuid().ToString('N') + '.partial')
try {
    $Stream = [System.IO.File]::Open($Partial, [System.IO.FileMode]::CreateNew)
    $Zip = [System.IO.Compression.ZipArchive]::new($Stream, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        $Entry = $Zip.CreateEntry('component-manifest.json', [System.IO.Compression.CompressionLevel]::Optimal)
        $Out = $Entry.Open()
        try { $Out.Write($ManifestBytes) } finally { $Out.Dispose() }
        foreach ($File in $Sources.GetEnumerator()) {
            $Entry = $Zip.CreateEntry($File.Key, [System.IO.Compression.CompressionLevel]::Optimal)
            $InputFile = [System.IO.File]::OpenRead($File.Value)
            $Out = $Entry.Open()
            try { $InputFile.CopyTo($Out) } finally { $Out.Dispose(); $InputFile.Dispose() }
        }
    } finally { $Zip.Dispose(); $Stream.Dispose() }
    if ((Get-Item -LiteralPath $Partial).Length -gt 512MB) { throw "Component ZIP exceeds download size limit" }
    Move-Item -LiteralPath $Partial -Destination $Destination
} finally {
    # Only the unique partial file created by this invocation, under validated dist.
    if (Test-Path -LiteralPath $Partial) { Remove-Item -LiteralPath $Partial -Force }
}
$Hash = (Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash.ToLowerInvariant()
Write-Host "RIFE_COMPONENT_PACKAGE_OK path=$Destination bytes=$((Get-Item -LiteralPath $Destination).Length) sha256=$Hash files=$($Sources.Count + 1)"
Write-Host "Publish only as prerelease tag rife-component-x64-v1, with the matching GPL source archive. No upload was performed."
