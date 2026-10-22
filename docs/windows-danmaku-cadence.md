# Windows 弹幕帧时间修复与实测

2026-10-22，本地实现、验证与提交，标准 Release 构建已更新，未推送或发布。

## 问题和实现

JS 绘制达到 152 FPS，并不意味着桌面中的弹幕每次刷新都更新。原实现的基线在
152 Hz 屏幕上只有约 128–130 个新弹幕帧/秒，存在大量约 13 ms 的间隔。

本次保留前一阶段的共享物理行防碰撞、文字图像缓存和播放时钟平滑修复，继续处理
CEF 共享纹理到 DirectComposition 的交付：

- 在 CEF 回调内将共享纹理复制到当前 surface 持有的纹理，并用 D3D11 EVENT query
  等待 GPU 读取完成。CEF 可以在回调返回后复用源纹理；只发出 CopyResource 或 Flush
  不代表读取已经完成。在呈现前完成这次复制，避免直接等待仍被显示器使用的后缓冲。
- 使用同步间隔 1，并将交换链从两个缓冲改为三个缓冲，为异步到达的 CEF 帧留出余量。
  Present、Commit 和 GPU 查询失败均明确报错；主 surface 返回真实成功状态。
- 弹幕 Canvas 使用正常合成路径，关闭 desynchronized 提示，使内容随 CEF 合成视图交付。

依据：[CEF 共享纹理约定](https://raw.githubusercontent.com/chromiumembedded/cef/master/include/cef_render_handler.h)、
[CEF D3D11 示例](https://raw.githubusercontent.com/chromiumembedded/cef/master/tests/shared/browser/osr_renderer_d3d11_win.cc)、
[DXGI Present 语义](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/nf-dxgi-idxgiswapchain-present)、
[后缓冲等待](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/for-best-performance--use-dxgi-flip-model)、
[Canvas 低延迟路径](https://developer.chrome.com/blog/desynchronized)。这些说明支持实现约束；
下列实测评价组合改动的结果，不把每一项都单独认定为全部卡顿的原因。

## 本机实测

RTX 5070 Ti / 驱动 32.0.15.9649，1920×1200、152 Hz，DPR 1.25；
默认弹幕样式，原生播放本地 1080p、23.976 fps H.264，视频增强关闭。
两组分别每秒输入 12 / 80 条混合方向、中文、英文和 emoji 弹幕，峰值可见 88 / 117 个图像。
每组采集 65 秒，排除前 5 秒。窗口和全屏也改变了密度，不用两组差异推断单一因素。

| 版本与场景 | 桌面新弹幕 FPS | 间隔 P99 | 超过 9.868 ms 的占比 | 已知间隔覆盖 |
|---|---:|---:|---:|---:|
| 基线 / 窗口 | 127.99 | 14.30 ms | 18.75% | 100% |
| 基线 / 全屏 | 129.63 | 14.02 ms | 17.26% | 100% |
| 修复 / 窗口 | 151.81 | 8.00 ms | 0.121% | 99.956% |
| 修复 / 全屏 | 151.92 | 8.20 ms | 0.055% | 100% |
| 复测 / 窗口 | 152.00 | 7.61 ms | 0% | 100% |
| 复测 / 全屏 | 151.68 | 8.90 ms | 0.209% | 100% |

152 Hz 的理想周期为 6.579 ms。修复组仍有少量约 13 ms 的长间隔，不宣称完全没有卡顿。
首轮窗口发生两次采集累计，相关两个未知内容间隔已排除，不补造时间；条码有效率与前台覆盖均为 100%。

两轮共约 240 秒有效测量，四组门禁均通过。复测窗口的帧编号连续递增、无重复和跳号；
复测全屏仍有 19 个确认跳过的源帧。四组最大已知间隔为 13.669 ms。

2026-10-22 合并 `v1.0.7` 发布线后，以合并构建（EXE SHA-256
`ed3f4ee60260dab541ac30056b6495842c8d01b92160e96e782b62e702bb6a4f`）复测：窗口
151.82 FPS / P99 8.79 ms / 已知间隔覆盖 99.943%；全屏 151.79 FPS / P99 8.00 ms /
覆盖 100%；两场景标记与前台覆盖均为 100%，无原生错误，门禁通过。结果与合并前
151.68–152.00 FPS、P99 7.61–8.90 ms 一致，确认三缓冲、回调内纹理复制、同步间隔 1
和 Canvas 正常合成未在合并中丢失。原始记录为
`build/danmaku-present-20261022/retest2-*`；测试媒体、采集程序和隔离配置已清理。

验收要求为两个场景各至少 59 秒稳态数据、已知间隔覆盖不少于 99%、条码与前台覆盖
100%、无原生播放错误、新帧率不少于屏幕刷新率的 99%、P99 不超过 1.5 个刷新周期。
`dev/windows/test_danmaku_cadence.cjs` 执行这组门禁；基线失败，两轮修复组通过。

此前只改 Present(1) 的短测接近 152 FPS，但 60 秒全屏组下降到 109.11 FPS；
直接在后缓冲上等待复制完成也导致吞吐明显下降。自有纹理配合两个交换链缓冲的长测
仍约 149–152 FPS，窗口 P99 约 13.42 ms，未通过门禁。这些候选均未作为最终通过结果。

## 方法与边界

通过 CDP 读取运行 EXE 内嵌 JS，断言与本地源码一致，提取真实 createDanmakuRenderer，
在正式 `.player-danmaku-layer` 样式的容器内、原生视频上运行；测试副本在同一 Canvas
追加带反码校验的帧编号。DXGI Desktop Duplication 读取桌面图像里的编号，使用
LastPresentTime 的 QPC 计算内容更新间隔，记录 AccumulatedFrames，排除未知缺口。
源帧回调和 rAF 时间戳分别记录，不用回调计数冒充桌面新帧率。

这是桌面采集开启条件下的合成弹幕负载测试，未测用户具体剧集，也未做光学测量。
Desktop Duplication 可能影响合成模式；桌面图像更新时间不等于面板扫描或像素发光时间。
少量未知间隔之外的分位数也不代表全部物理屏幕输出。

验证：五项 JS 行为回归，740×420、1920×1080、3840×2160/DPR 2 的真实 Canvas
共检查 5,885,207 对可见字形矩形而无重叠，文字缓存检查通过；28 项 compositor-core
测试通过。正式 EXE 上暂停停止弹幕更新、暂停跳转到 15 秒、继续播放、全屏切换与
Canvas 物理尺寸匹配检查通过。受影响的 `jfn-windows` 在短路径编译缓存下通过 Release
`--no-deps` Clippy，warnings、unwrap、expect、panic 均禁止。
初次长路径检查触发 CEF 包装层 C1083；包含依赖的检查另有现存 mpv raw-pointer lint。
本次未修无关依赖，也未将全量 Clippy 记为通过，相关失败日志保留。

标准程序 `build/jellium-desktop.exe` 的 SHA-256：
`5f1e7fcbfd046c09fb01da55594ad0173ff82fe757d2806e1c50b53c7ffe018a`。
mpv、RIFE/TensorRT、视频增强、屏幕模式与音频速率未改。

收尾已关闭诊断实例，清理测试媒体、配置/缓存、采集二进制与校准临时文件；保留逐帧证据。
标准程序已无参数重新打开（PID 4300），窗口存在且响应正常；运行文件 hash 与两轮测量一致，
实际加载标准目录的 mpv / CEF。核验记录为 `build/danmaku-present-20261022/delivered-audit.json`。

## 证据与复现

生成文件不入库。原始记录保存在 `build/danmaku-present-20261022/`：

- `triple-{summary.json,window-normal-desktop.csv,fullscreen-dense-desktop.csv}`、对应源帧 JSON、运行 manifest 和 `triple-frame-times.svg`。
- `repeat-*` 保留第二轮完整数据、manifest、汇总与帧时间曲线。
- `triple-controls-smoke-manifest.json`、`renderer-tests.log`、`compositor-core-tests.log`、`short-path-clippy.log`、`triple-build.log`。
- `delivered-audit.json` 记录交付 EXE、已打开的普通实例及实际加载的 mpv / CEF 路径与 hash。
- 候选失败记录的 `final-*`、`owned-long-*`、`release-*`，以及旧基线 `build/danmaku-cadence-20261022/`。
- `capture.cpp`、`run.cjs`、`analyze.py` 保留测量源码；正式 EXE 没有条码、统计 CSV 或硬编码诊断路径。

复测需要重新编译 capture.cpp，在脚本同目录生成至少 75 秒的 1080p/23.976 本地 H.264
`background.mp4`；以独立 config/cache、`--remote-debug-port=9240` 启动标准程序。
执行 `node build/danmaku-present-20261022/run.cjs <主进程PID> 65 <标签前缀>`，
再执行该目录的 `analyze.py <标签前缀>`，最后将 `<标签前缀>summary.json` 传给门禁。
`CADENCE_CONTROLS=1` 额外检查播放操作，`CADENCE_SYNC=1` 仅供旧 Canvas 参数的诊断对照，
正式长测不设置实验覆盖。测试媒体、诊断配置/缓存、采集二进制和校准图片在收尾清理。
