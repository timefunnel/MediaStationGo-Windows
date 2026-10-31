# Windows 后台恢复后的页面尺寸同步

## 2026-10-29 现场与修复

用户报告窗口放在后台后，页面自行缩到左上角，右侧与底部留白。
现场只读测量得到客户区 `1818×1146`、窗口 DPI `144`（150%），
启动日志记录 `display-hidpi-scale=1.25`，保存尺寸为 `1515×955`。
两组物理尺寸相差 1.2 倍，对应相同的 `1212×764` 逻辑尺寸。
触发后台 DPI 变化的具体系统事件尚未确定。

此前 Windows 的 CEF 视口和输入坐标读取 mpv OSD 尺寸缓存；
`WM_SIZE` 只更新输入层与合成器，CEF 仍等待 mpv 属性通知。
后台迟到的 OSD 尺寸可以重新缩小网页视口。

现在 Windows 的 `WindowSource` 直接读取 `GetClientRect` 与
`GetDpiForWindow`。输入层读取同一来源，删除 mpv 尺寸和缩放缓存分支。
窗口尺寸、DPI、位置变化和恢复焦点时主动通知消费者重新读取原生尺寸。
最小化不生成零尺寸；恢复后立即同步。启动等待窗口时仅从 mpv
获取 HWND，尺寸仍读取 Win32，避免平台初始化与窗口就绪互相等待。
没有新增轮询、CSS 强制重排或纹理拉伸。

## 验证方式

- `cargo test --manifest-path src/Cargo.toml -p jfn-windows --lib platform::tests`
  验证实际故障尺寸、仅 DPI 变化、输入坐标与 CEF 同源，以及无效尺寸。
- `cargo test --manifest-path src/Cargo.toml -p jfn-cef --lib window_sync::tests`
  验证尺寸换算和通知丢失后的最终一致性。
- 标准 Windows 构建后，用无账号的独立配置启动
  `build/jellium-desktop.exe --config-dir=<测试配置> --cache-dir=<测试缓存>
  --remote-debug-port=<本地端口>`。
- 另行测量测试窗口 Win32 客户区和 DPI，运行：
  `node dev/windows/test_native_window_geometry.cjs <端口> <物理宽> <物理高> <DPI> --fixture`。
  此参数会将该测试实例的页面替换为四边框与右下角点击验证页，
  不应对用户正在使用的实例执行。
- 最大化、还原、最小化静置后恢复，再用相同命令去掉 `--fixture`
  比较 CEF 的 `innerWidth/innerHeight/devicePixelRatio` 与 Win32 实测值，
  并检查四边绘制及右下角点击命中。

已实测初始及还原 `1575×1050 → 1050×700 @1.5`、
最大化 `3840×2066 → 2560×1377 @1.5`，以及最小化到任务栏静置后恢复，
视口、四边绘制和右下角输入命中均正确。8 项单元测试与 Windows Clippy
（warnings、unwrap、expect、panic 均禁止）通过，标准 release 构建通过。
没有修改系统显示缩放，125%→150% 的故障尺寸由单元测试覆盖；
长时间后台自发复现仍需实际使用观察。
