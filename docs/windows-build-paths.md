# Windows 本地构建路径约定

本地构建前先为当前仓库核对或建立短盘符别名，并始终通过同一别名访问
`build/cargo-target`。产物仍保存在当前仓库的标准 `build` 目录。
`dev/windows/env.ps1` 负责 MSVC、libclang 和 mpv 环境，不会自动缩短路径。

## 原因和已验证结果

2026-10-23，在 `D:\stronger\movieService\.codex-mediastation-windows-spike\jellium-desktop`
直接构建，先遇到 CMake 缓存记录的 `V:/build/...` 与当前 `D:/.../build/...`
不一致；重新配置后，CEF 包装层又报 `fatal error C1083`，无法生成编译器文件，
错误为 `Invalid argument`。因此，路径全是 ASCII 也不能排除路径长度问题。

核对 `V:` 指向同一仓库，清除报错指向的旧 `CMakeCache.txt`，通过 `V:` 重建后成功，
release 构建耗时 1 分 27 秒。已核对标准目录内的 exe 与编译产物一致，并包含本次前端代码。

## 每次构建的固定步骤

在仓库根目录的 PowerShell 中执行。已有 `V:` 必须指向当前仓库；若被其他目录或磁盘
占用，应停止并选择合适的空闲短盘符，不得覆盖现有映射。

```powershell
$repoPath = & git rev-parse --show-toplevel
if ($LASTEXITCODE -ne 0) { throw '请在目标 Git 仓库内执行' }
$repoRoot = [IO.Path]::GetFullPath($repoPath.Trim()).TrimEnd('\')
$mapping = & subst | Where-Object { $_ -match '^V:\\: => ' }
if ($mapping) {
    $mappedRoot = [IO.Path]::GetFullPath(($mapping -replace '^V:\\: => ', '')).TrimEnd('\')
    if ($mappedRoot -ine $repoRoot) { throw 'V: 已映射到其他目录，不得覆盖' }
} elseif (Test-Path -LiteralPath 'V:\') {
    throw 'V: 已被占用，请选择其他空闲短盘符'
} else {
    & subst V: $repoRoot
    if ($LASTEXITCODE -ne 0) { throw '创建构建路径映射失败' }
}
Set-Location V:\
. .\dev\windows\env.ps1
$env:CARGO_TARGET_DIR = 'V:\build\cargo-target'
$env:EXTERNAL_MPV_DIR = 'V:\third_party\mpv-install'
cargo xtask build --external-mpv="$env:EXTERNAL_MPV_DIR"
if ($LASTEXITCODE -ne 0) { throw 'Windows 构建失败' }
```

若选择其他盘符，以上所有 `V:` 必须一致替换，后续构建保持一致。
这只是同一目录的路径别名，不是隔离输出；最终程序仍是仓库内的
`build/jellium-desktop.exe`。普通 UI 修改使用上述构建入口，不需要重建 RIFE 等可选组件。

## 首次切换已有缓存

若 CMake 明确报告 `CMakeCache.txt directory ... is different than ...`，先核对
错误中的缓存文件确实位于当前仓库的 `build/cargo-target/.../out/build/` 内，再仅删除
该错误指向的 `CMakeCache.txt`，随后通过固定短路径重建。不要清空整个 `build`、
切换隔离输出，或反复交替使用长路径与短路径重试。

保留并复用正确的盘符映射；不要在收尾时删除其他任务或用户创建的映射。
