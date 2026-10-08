param(
    [string]$Archive = 'dist\components\MediaStationGo-frame-interpolation-x64-v1.zip',
    [ValidateRange(1, 1000)][int]$Warmup = 3,
    [ValidateRange(1, 10000)][int]$Iterations = 10
)

$ErrorActionPreference = 'Stop'
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
if (-not [System.IO.Path]::IsPathRooted($Archive)) { $Archive = Join-Path $RepoRoot $Archive }
$Archive = (Resolve-Path -LiteralPath $Archive).Path
. (Join-Path $PSScriptRoot 'env.ps1')
$PreviousArchive = $env:MSGO_RIFE_COMPONENT_TEST_ARCHIVE
$TestRoot = Join-Path $RepoRoot ('build\rife-component-test-' + [guid]::NewGuid().ToString('N'))
$BuildPrefix = [System.IO.Path]::GetFullPath((Join-Path $RepoRoot 'build')).TrimEnd('\') + '\'
if (-not [System.IO.Path]::GetFullPath($TestRoot).StartsWith($BuildPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw 'Component test directory must remain below repository build'
}
try {
    $env:MSGO_RIFE_COMPONENT_TEST_ARCHIVE = $Archive
    & cargo test --manifest-path (Join-Path $RepoRoot 'src\Cargo.toml') -p jfn-frame-interpolation `
        real_component_installs_loads_native_abi_and_uninstalls -- --ignored --nocapture
    if ($LASTEXITCODE -ne 0) { throw 'Real component lifecycle/native ABI test failed' }
    # This ZIP is the same artifact validated by Rust above. Expand only into a
    # unique test directory, never the actual user's component/cache directory.
    [System.IO.Compression.ZipFile]::ExtractToDirectory($Archive, $TestRoot)
    & (Join-Path $PSScriptRoot 'test_rife_engine_builder.ps1') -RuntimeLibDir $TestRoot -Warmup $Warmup -Iterations $Iterations
    if ($LASTEXITCODE -ne 0) { throw 'Relocated component Engine/P010 validation failed' }
    Write-Host "RIFE_COMPONENT_TEST_OK archive=$Archive sha256=$((Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash.ToLowerInvariant()) models=3"
} finally {
    $env:MSGO_RIFE_COMPONENT_TEST_ARCHIVE = $PreviousArchive
    if (Test-Path -LiteralPath $TestRoot) { Remove-Item -LiteralPath $TestRoot -Recurse -Force }
}
