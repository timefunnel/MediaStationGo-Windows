param(
    [string]$HelperPath = 'build\mediastation-portable-updater.exe',
    [string]$ScreenshotPath
)

$ErrorActionPreference = 'Stop'
$helperSource = (Resolve-Path -LiteralPath $HelperPath).Path
$taskTempParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
$taskLeaf = 'mediastation-update-test-' + [Guid]::NewGuid().ToString('N')
$taskRoot = Join-Path $taskTempParent $taskLeaf
$parent = $null
$helperProcess = $null
Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class UpdateProgressProbe {
    public delegate bool EnumProc(IntPtr window, IntPtr data);
    [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr parent, EnumProc callback, IntPtr data);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr window, StringBuilder text, int count);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr window, StringBuilder text, int count);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] public static extern IntPtr SendMessageTimeout(IntPtr window, uint message, UIntPtr wparam, IntPtr lparam, uint flags, uint timeout, out UIntPtr result);
    [DllImport("user32.dll")] public static extern IntPtr GetSystemMenu(IntPtr window, bool revert);
    [DllImport("user32.dll")] public static extern uint GetMenuState(IntPtr menu, uint item, uint flags);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr window, IntPtr dc, uint flags);
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr window, out Rect rect);
}
'@
try {
    New-Item -ItemType Directory -Path $taskRoot | Out-Null
    $install = Join-Path $taskRoot '便携 应用'
    $package = Join-Path $taskRoot 'package'
    New-Item -ItemType Directory -Path $install, $package | Out-Null
    $helper = Join-Path $taskRoot 'mediastation-portable-updater.exe'
    Copy-Item -LiteralPath $helperSource -Destination $helper
    $managed = @('jellium-desktop.exe', 'mediastation-portable-updater.exe', '.mediastation-portable', '.mediastation-portable-files.txt')
    [IO.File]::WriteAllText((Join-Path $install '.mediastation-portable-files.txt'), (($managed + 'stale.dll') -join "`n"))
    foreach ($name in @('jellium-desktop.exe', 'mediastation-portable-updater.exe', 'stale.dll')) {
        [IO.File]::WriteAllText((Join-Path $install $name), 'old')
    }
    [IO.File]::WriteAllText((Join-Path $install '.mediastation-portable'), '1.0.0')
    [IO.File]::WriteAllText((Join-Path $install 'user-note.txt'), 'preserve me')
    $probeSource = Join-Path $taskRoot 'probe.rs'
    [IO.File]::WriteAllText($probeSource, @'
#![windows_subsystem = "windows"]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::current_exe()?.parent().ok_or("missing parent")?.to_path_buf();
    std::fs::write(root.join("launched-version.txt"), std::fs::read(root.join(".mediastation-portable"))?)?;
    Ok(())
}
'@)
    & rustc $probeSource -o (Join-Path $package 'jellium-desktop.exe')
    if ($LASTEXITCODE -ne 0) { throw 'Update probe compilation failed' }
    [IO.File]::WriteAllText((Join-Path $package 'mediastation-portable-updater.exe'), 'new helper fixture')
    [IO.File]::WriteAllText((Join-Path $package '.mediastation-portable'), '1.1.0')
    [IO.File]::WriteAllBytes((Join-Path $package 'payload.bin'), [byte[]]::new(16MB))
    $packageFiles = @(Get-ChildItem -LiteralPath $package -File -Force | Select-Object -ExpandProperty Name) + '.mediastation-portable-files.txt'
    [IO.File]::WriteAllText((Join-Path $package '.mediastation-portable-files.txt'), ($packageFiles -join "`n"))
    $archive = Join-Path $taskRoot 'update.zip'
    [IO.Compression.ZipFile]::CreateFromDirectory($package, $archive)
    $sha = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()

    $parent = Start-Process -FilePath "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" -WindowStyle Hidden -PassThru -ArgumentList @('-NoProfile', '-NonInteractive', '-Command', 'Start-Sleep -Seconds 30')
    $start = [Diagnostics.ProcessStartInfo]::new($helper)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    foreach ($argument in @('--parent-pid', $parent.Id.ToString(), '--archive', $archive, '--install-dir', $install, '--expected-version', '1.1.0', '--expected-sha256', $sha, '--log-file', (Join-Path $taskRoot 'update.log'))) {
        $start.ArgumentList.Add($argument)
    }
    $helperProcess = [Diagnostics.Process]::Start($start)
    $window = [IntPtr]::Zero
    $texts = [Collections.Generic.List[string]]::new()
    $bars = [Collections.Generic.List[IntPtr]]::new()
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        Start-Sleep -Milliseconds 50
        $helperProcess.Refresh()
        if ($helperProcess.HasExited) { throw 'Updater exited before showing progress' }
        $window = $helperProcess.MainWindowHandle
        if ($window -ne [IntPtr]::Zero) {
            $texts.Clear(); $bars.Clear()
            [UpdateProgressProbe]::EnumChildWindows($window, {
                param($child, $data)
                $text = [Text.StringBuilder]::new(512)
                $class = [Text.StringBuilder]::new(128)
                [void][UpdateProgressProbe]::GetWindowText($child, $text, 512)
                [void][UpdateProgressProbe]::GetClassName($child, $class, 128)
                $texts.Add($text.ToString())
                if ($class.ToString() -eq 'msctls_progress32') { $bars.Add($child) }
                return $true
            }, [IntPtr]::Zero) | Out-Null
        }
    } while (-not ($texts -match '正在等待应用关闭') -and [DateTime]::UtcNow -lt $deadline)
    if ($window -eq [IntPtr]::Zero -or -not [UpdateProgressProbe]::IsWindowVisible($window) -or $bars.Count -ne 1 -or -not ($texts -match '正在等待应用关闭')) { throw "Native progress window/bar/stage missing: $texts" }
    if ([IO.File]::ReadAllText((Join-Path $install '.mediastation-portable')) -ne '1.0.0') { throw 'Files changed before parent exit' }
    $reply = [UIntPtr]::Zero
    if ([UpdateProgressProbe]::SendMessageTimeout($window, 0, [UIntPtr]::Zero, [IntPtr]::Zero, 2, 500, [ref]$reply) -eq [IntPtr]::Zero) { throw 'Progress window is unresponsive while worker waits' }
    if ([UpdateProgressProbe]::SendMessageTimeout($bars[0], 0x40A, [UIntPtr]::new(1), [IntPtr]::new(30), 2, 500, [ref]$reply) -eq [IntPtr]::Zero -or $reply -ne [UIntPtr]::new(1)) { throw 'Native marquee animation did not start (common controls v6 required)' }
    $menu = [UpdateProgressProbe]::GetSystemMenu($window, $false)
    if (([UpdateProgressProbe]::GetMenuState($menu, 0xF060, 0) -band 3) -eq 0) { throw 'Close must be disabled while applying an update' }
    [void][UpdateProgressProbe]::SendMessageTimeout($window, 0x10, [UIntPtr]::Zero, [IntPtr]::Zero, 2, 500, [ref]$reply)
    if (-not [UpdateProgressProbe]::IsWindowVisible($window)) { throw 'Closing the window hid active update progress' }
    if ($ScreenshotPath) {
        # Capture after the first native marquee timer tick has painted.
        Start-Sleep -Milliseconds 250
        Add-Type -AssemblyName System.Drawing
        $priorDpi = [UpdateProgressProbe]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
        try {
            $rect = [UpdateProgressProbe+Rect]::new()
            if (-not [UpdateProgressProbe]::GetWindowRect($window, [ref]$rect)) { throw 'Cannot read progress window bounds' }
            $bitmap = [Drawing.Bitmap]::new($rect.Right - $rect.Left, $rect.Bottom - $rect.Top)
            try {
                $graphics = [Drawing.Graphics]::FromImage($bitmap)
                $dc = $graphics.GetHdc()
                try {
                    if (-not [UpdateProgressProbe]::PrintWindow($window, $dc, 0)) { throw 'Cannot capture progress window' }
                } finally { $graphics.ReleaseHdc($dc); $graphics.Dispose() }
                $bitmap.Save([IO.Path]::GetFullPath($ScreenshotPath))
            } finally { $bitmap.Dispose() }
        } finally { [void][UpdateProgressProbe]::SetThreadDpiAwarenessContext($priorDpi) }
    }
    $parent.Kill(); $parent.WaitForExit()
    if (-not $helperProcess.WaitForExit(15000) -or $helperProcess.ExitCode -ne 0) { throw 'Portable update failed to finish' }
    $result = Join-Path $install 'launched-version.txt'
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    while (-not (Test-Path -LiteralPath $result) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 50 }
    if (-not (Test-Path -LiteralPath $result) -or [IO.File]::ReadAllText($result) -ne '1.1.0') { throw 'Updated application did not restart with the new version' }
    if ((Test-Path -LiteralPath (Join-Path $install 'stale.dll')) -or [IO.File]::ReadAllText((Join-Path $install 'user-note.txt')) -ne 'preserve me') { throw 'Managed update did not preserve user files or remove stale files' }
    if (Test-Path -LiteralPath $archive) { throw 'Completed update did not clean its archive' }
    Write-Output 'APP_UPDATE_PROGRESS_SMOKE_OK native-window/progress/responsive/parent-wait/managed-update/automatic-restart'
} finally {
    foreach ($process in @($helperProcess, $parent)) {
        if ($null -ne $process) {
            if (-not $process.HasExited) { $process.Kill(); $process.WaitForExit() }
            $process.Dispose()
        }
    }
    if (Test-Path -LiteralPath $taskRoot) {
        $resolved = (Resolve-Path -LiteralPath $taskRoot).Path
        if ([IO.Path]::GetDirectoryName($resolved) -ne $taskTempParent -or [IO.Path]::GetFileName($resolved) -ne $taskLeaf -or (Get-Item -LiteralPath $resolved).Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) { throw 'Refusing cleanup outside the exact owned temporary directory' }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
