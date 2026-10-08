param([string]$BuildDir = 'build')

$ErrorActionPreference = 'Stop'
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
if (-not [System.IO.Path]::IsPathRooted($BuildDir)) { $BuildDir = Join-Path $RepoRoot $BuildDir }
$BuildDir = (Resolve-Path -LiteralPath $BuildDir).Path
$TestRoot = Join-Path $RepoRoot ('build\rife-base-test-' + [guid]::NewGuid().ToString('N'))
$BuildPrefix = [System.IO.Path]::GetFullPath((Join-Path $RepoRoot 'build')).TrimEnd('\') + '\'
if (-not [System.IO.Path]::GetFullPath($TestRoot).StartsWith($BuildPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw 'Base staging test directory must remain below repository build'
}
. (Join-Path $PSScriptRoot 'env.ps1')
$Forbidden = @('rife_runtime.dll', 'cudart64_12.dll', 'tensorrt_rtx_1_4.dll',
    'tensorrt_onnxparser_rtx_1_4.dll', 'rife_engine_builder.exe', 'tensorrt_rtx.exe',
    'rife_v4.26_fp16_io.onnx', 'rife_v4.26_scale0.5.onnx', 'rife_v4.25_lite_fp16_io.onnx')
try {
    & cargo xtask install --skip-build --external-mpv $env:EXTERNAL_MPV_DIR --out $BuildDir --prefix $TestRoot
    if ($LASTEXITCODE -ne 0) { throw 'Base installation staging failed' }
    foreach ($Directory in @($BuildDir, $TestRoot)) {
        $Found = Get-ChildItem -LiteralPath $Directory -Recurse -File |
            Where-Object { $_.Name -in $Forbidden -and ($Directory -eq $TestRoot -or $_.DirectoryName -eq $BuildDir -or $_.FullName.StartsWith((Join-Path $BuildDir 'frame-interpolation'))) }
        if ($Found) { throw "Base payload includes optional components: $($Found.FullName -join ', ')" }
    }
    foreach ($Name in @('jellium-desktop.exe', 'mediastation-portable-updater.exe', 'libmpv-2.dll', 'libcef.dll')) {
        if (-not (Test-Path -LiteralPath (Join-Path $TestRoot $Name) -PathType Leaf)) { throw "Base payload missing: $Name" }
    }
    & (Join-Path $PSScriptRoot 'stage_public_release_licenses.ps1') -OutputDir $TestRoot -SourceArchiveName 'MediaStationGo-local-validation-source.zip'
    $Process = Start-Process -FilePath (Join-Path $TestRoot 'jellium-desktop.exe') -ArgumentList '--version' -WindowStyle Hidden -Wait -PassThru
    if ($Process.ExitCode -ne 0) { throw "Staged app smoke failed: $($Process.ExitCode)" }
    Write-Host "RIFE_BASE_INSTALL_TEST_OK component-payload=absent required-runtime=present licenses=staged smoke=0"
} finally {
    if (Test-Path -LiteralPath $TestRoot) { Remove-Item -LiteralPath $TestRoot -Recurse -Force }
}
