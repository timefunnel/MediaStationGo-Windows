param([string]$HelperPath = 'build\mediastation-portable-updater.exe')

$ErrorActionPreference = 'Stop'
$helperSource = (Resolve-Path -LiteralPath $HelperPath).Path
$taskTempParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
$taskLeaf = 'mediastation-restart-test-' + [Guid]::NewGuid().ToString('N')
$taskRoot = Join-Path $taskTempParent $taskLeaf
$parent = $null
$helperProcess = $null
try {
    New-Item -ItemType Directory -Path $taskRoot | Out-Null
    $workingDirectory = Join-Path $taskRoot '工作 目录'
    New-Item -ItemType Directory -Path $workingDirectory | Out-Null
    $helper = Join-Path $taskRoot 'mediastation-portable-updater.exe'
    Copy-Item -LiteralPath $helperSource -Destination $helper
    $probe = Join-Path $taskRoot 'jellium-desktop.exe'
    & rustc (Join-Path $PSScriptRoot 'fixtures\restart_probe.rs') -o $probe
    if ($LASTEXITCODE -ne 0) { throw 'Restart probe compilation failed' }
    $result = Join-Path $taskRoot 'probe-result.txt'
    $probeArgs = @('--config-dir', 'D:\用户 配置', '--log-file', '', '--hwdec=d3d11va')

    # Only this short-lived, hidden fixture is a parent; never touch the player.
    $parent = Start-Process -FilePath "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" -WindowStyle Hidden -PassThru -ArgumentList @('-NoProfile', '-NonInteractive', '-Command', 'Start-Sleep -Seconds 6')
    $start = [Diagnostics.ProcessStartInfo]::new($helper)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in (@('--restart-only', '--parent-pid', $parent.Id.ToString(), '--working-dir', $workingDirectory, '--', $result) + $probeArgs)) {
        $start.ArgumentList.Add($argument)
    }
    $helperProcess = [Diagnostics.Process]::Start($start)
    $ready = $helperProcess.StandardOutput.ReadLineAsync()
    if (-not $ready.Wait(5000) -or $ready.Result -ne 'MEDIASTATION_RESTART_READY') {
        throw 'Restart helper did not acknowledge readiness'
    }
    $parent.Refresh()
    if ($parent.HasExited) { throw 'Parent exited before wait ordering could be checked' }
    Start-Sleep -Milliseconds 200
    if (Test-Path -LiteralPath $result) { throw 'Application launched before parent exit' }
    if (-not $parent.WaitForExit(10000)) { throw 'Fixture parent did not exit' }
    if (-not $helperProcess.WaitForExit(5000)) { throw 'Restart helper did not finish' }
    if ($helperProcess.ExitCode -ne 0) { throw "Restart helper failed: $($helperProcess.StandardError.ReadToEnd())" }
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    while (-not (Test-Path -LiteralPath $result) -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 50
    }
    if (-not (Test-Path -LiteralPath $result)) { throw 'Restarted application did not produce evidence' }
    $expected = (@($workingDirectory) + $probeArgs) -join "`n"
    if ([IO.File]::ReadAllText($result) -cne $expected) { throw 'Restart did not preserve directory or arguments (including Unicode, spaces and empty values)' }
    Write-Output 'APP_RESTART_SMOKE_OK ready/parent-exit-before-launch/original-args/working-directory'
} finally {
    foreach ($process in @($helperProcess, $parent)) {
        if ($null -ne $process) {
            if (-not $process.HasExited) { $process.Kill(); $process.WaitForExit() }
            $process.Dispose()
        }
    }
    if (Test-Path -LiteralPath $taskRoot) {
        $resolved = (Resolve-Path -LiteralPath $taskRoot).Path
        if ([IO.Path]::GetDirectoryName($resolved) -ne $taskTempParent -or [IO.Path]::GetFileName($resolved) -ne $taskLeaf -or (Get-Item -LiteralPath $resolved).Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) {
            throw 'Refusing cleanup outside the exact owned temporary directory'
        }
        Remove-Item -LiteralPath $resolved -Recurse -Force
        if (Test-Path -LiteralPath $resolved) { throw 'Restart probe cleanup failed' }
    }
}
