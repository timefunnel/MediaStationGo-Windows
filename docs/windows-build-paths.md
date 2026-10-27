# Windows 本地构建路径约定

直接从当前仓库的真实路径构建，不创建或依赖 `subst` 虚拟盘符。
编译缓存保存在标准 `build/cargo-target`，最终程序仍为
`build/jellium-desktop.exe`。

## 路径长度处理

Windows CEF 构建要求 **CMake 4.2 或更新版本**。
`.cargo/config.toml` 为 Windows MSVC 目标指定
`dev/windows/cef-toolchain.cmake`，启用
`CMAKE_INTERMEDIATE_DIR_STRATEGY=SHORT`，缩短中间目录和对象文件名。
同时将 `CMAKE_OBJECT_PATH_MAX` 限制为 240，覆盖 `cef-dll-sys` 原先的 500。
旧版 CMake 会明确报错，不会回退到虚拟盘符或其他缓存目录。

机制参见 [CMake 官方文档](https://cmake.org/cmake/help/v4.2/variable/CMAKE_INTERMEDIATE_DIR_STRATEGY.html)。

`dev/windows/env.ps1` 加载 MSVC、libclang 和 mpv 环境，并保留加载 MSVC 前
PATH 中选中的 CMake；也可以通过 `CMAKE` 环境变量显式指定其可执行文件。
Visual Studio 自带的 CMake 若低于 4.2，需要另行安装新版。
Windows CI 显式安装 CMake 4.4.4，并设置 `CMAKE` 指向其可执行文件，避免
加载 MSVC 环境后选到 Visual Studio 自带的旧版本。

## 固定构建步骤

在当前仓库根目录的 PowerShell 中执行：

```powershell
$repoPath = & git rev-parse --show-toplevel
if ($LASTEXITCODE -ne 0) { throw '请在目标 Git 仓库内执行' }
$repoRoot = [IO.Path]::GetFullPath($repoPath.Trim()).TrimEnd('\')
Set-Location -LiteralPath $repoRoot
. .\dev\windows\env.ps1
$env:CARGO_TARGET_DIR = Join-Path $repoRoot 'build\cargo-target'
$env:EXTERNAL_MPV_DIR = Join-Path $repoRoot 'third_party\mpv-install'
cargo xtask build --external-mpv="$env:EXTERNAL_MPV_DIR"
if ($LASTEXITCODE -ne 0) { throw 'Windows 构建失败' }
```

普通 UI 修改使用上述入口，不需要重建 RIFE 等可选组件。

## 从旧虚拟盘符缓存迁移

2026-10-23 曾使用 V: 绕过 CEF 长路径问题。该方案现已取消：旧缓存不仅包含
CMake 绝对路径，已编译的本地构建脚本也可能嵌入 V: 资源路径。若直接切回真实
路径，可能出现 CMake 缓存位置不一致或资源文件路径不存在的错误。

首次迁移时，只清理受影响包在标准缓存内的构建产物，再执行上述构建：

```powershell
$targetDir = Join-Path $repoRoot 'build\cargo-target'
foreach ($profile in @('dev', 'release')) {
    cargo clean --manifest-path src/Cargo.toml --target-dir "$targetDir" `
        --profile $profile -p cef-dll-sys -p jfn-rust -p jfn-cef -p xtask -p jfn-mediastation
    if ($LASTEXITCODE -ne 0) { throw '旧路径构建缓存清理失败' }
}
```

不要清空整个 `build`，不要改用其他虚拟盘符、目录映射或隔离输出目录。
无需在每次构建前清理；只在确认使用了旧路径缓存时执行这一步。

## 已验证结果

2026-10-26，使用 CMake 4.4.4，在没有 V: 映射的情况下，从
`D:\stronger\movieService\.codex-mediastation-windows-spike\jellium-desktop`
完成标准 Windows x64 release 构建，Cargo release 阶段耗时 1 分 34 秒。
CEF 缓存使用真实 D: 路径，`SHORT` 和 240 字符上限均生效；生成的对象文件
绝对路径最长为 200 字符，当前 `CMakeCache.txt` 和 `build.ninja` 无 V: 引用。
已同时验证旧版 CMake 4.1 会给出明确的版本错误。
