# 2026-10-22 首页启动与滚动修复

用户报告启动时软件首页黑屏、首次加载后滑动滞涩，并提示清缓存可能更容易复现。
用户另行确认将同目录中的 VFG 和播放面板改动一并交付到本机。

## 启动后持续滚动迟滞的补充修复

前述 wheel/布局检查通过后，用户仍必现启动后十余秒不跟手，随后恢复正常。
改为由用户自行启动、手动滚动，从页面约 0.29 秒开始采集；基线在约 1.63 秒
收到首次滚轮，共记录 335 次真实 wheel。最初两次主线程长任务分别为 79/72 ms，
不足以解释持续十余秒的迟滞。原生绘制日志则显示前约 26 秒 GPU 复制等待中位数
5.266 ms，rAF 约 304 Hz；此后等待降至约 0.266 ms，rAF 恢复约 152 Hz。
这里的 rAF 和复制耗时均不是物理上屏延迟测量。

根因在 `paint_scheduler.rs`：首次绘制会初始化 `now_ns()` 的单调时钟起点，
此时 `now_ns=0`。旧逻辑先记住 resize generation，再要求已经过一个刷新周期
才设定结束重绘的帧数；首次因尚未经过周期而跳过，后续 generation 相同也不再设定。
因此启动时的双倍帧率及持续 Invalidate 一直运行到 1000 tick 保护退出，
在 GPU/呈现背压下这轮实际持续约 26 秒。

修复确保首个 resize generation 在计时起点也设定完成预算；连续拖动窗口时仍按
刷新周期限制重置，保留尺寸切换时的跳帧和稳定机制。没有通过改变滚轮平滑动画、
缩短超时或取消 GPU 纹理读取完成同步来掩盖问题。可选 `scroll_latency=trace`
记录 Win32 滚轮分发及 native paint 的锁等待、准备、复制与 Present 耗时，默认关闭。
初始化重绘的开始、完成预算和恢复帧率使用既有 CEF debug 日志。

修复版同样由用户启动并滚动，首次输入约 2.90 秒，共采到 78 次 wheel；日志明确记录
`generation=1 fps=152 now_ns=0`，约 1.315 秒就恢复 152 Hz（135 tick），
1–10 秒复制等待中位数 0.293 ms，10–30 秒为 0.279 ms。用户确认“前十秒也正常跟手”。
最初一秒仍包含初始化和 GPU 等待，不宣称所有首帧等待或掉帧为零。

两项回归覆盖计时起点为零时能按预算结束，以及同一刷新周期内连续 resize 不会反复
跳过绘制。CEF 单测 112 通过/4 忽略，Windows 单测 11 通过；Windows 严格 Clippy
通过，CEF 按既有 collapsible_if/redundant_closure 两类豁免检查通过。完整严格
Clippy 另有既有 `mediastation_runtime.rs` 的 `expect` 限制，不将其写成全部通过。
原有首页、VFG、播放面板修复继续保留。

采集入口为 `dev/windows/profile_scroll_latency.cjs`，使用本机显式开启的调试端口。
可先运行采集器再由用户启动；输入 stop 或在报告目录创建 stop 文件结束采集，
不会关闭播放器。默认只保存事件与原生日志；`SCROLL_TRACE=1` 才额外采集体积较大的
Chromium trace。事件日志省略资源 URL，原始 trace 可能含 URL，仅保存在本机 build。
基线、修复和复核记录位于 `build/scroll-latency/`，其中基线原生日志已固定为
`manual-20261022/baseline-app.log`，避免后续启动日志轮转导致对错样本。
修复版记录在 `manual-fixed-20261022/`，本机更新指纹以 `delivery-audit.json` 为准。

## 证据和实现

- 原启动日志中，CEF 初始化后约 11 秒才开始创建浏览器，同时记录原生账号恢复
  `code=transport`。`business_web::jfn_web_init` 在创建浏览器之前同步恢复登录并查询
  服务端能力，网络等待直接挡住首页。恢复改由首个 `session_status` 请求的工作线程
  执行，`OnceLock` 保证每次进程只恢复一次；失败有日志、错误码及账号重连提示。
  未增加重试、并行账号源或自动换账号。
- 首页根节点原有 `passive:false` 滚轮监听，纵向事件也需等待渲染线程，横向分支
  调用 `preventDefault()` 却没有执行滚动。删除该监听，复用 Chromium 原生滚动及
  既有 CSS `overflow-x:auto`、`overscroll-behavior-x:contain`。
  [Chrome 官方说明](https://developer.chrome.com/blog/scrolling-intervention-2)
  解释了阻塞监听与滚动等待的关系。
- 后台首页快照刷新总是重建整页。现在只忽略响应中的缓存状态元数据比较实际内容；
  无变化时更新权威数据引用并保留 DOM，有变化时照常重绘并恢复滚动与焦点。
  播放进度、媒体库及最近添加变化均保留刷新。
- 主图初次宽度在插入卡片前读取，避免同一请求回调立即强制新页面布局。
  卡片行初始化和 resize 改为同一帧分阶段读取/更新各行尺寸，保留原有像素对齐、
  分页、行尾补齐、选集初始定位与隐藏播放器外壳保护。

## 验证及边界

使用独立临时 config/cache；未清理用户真实缓存。Windows 活跃凭据仍来自既有凭据
管理器，隔离目录本身不会隔离凭据。首页检查只读目录，不加载实际云端媒体。

| 检查 | 结果 |
| --- | --- |
| 同一非转发代理阻塞登录恢复 8 秒 | 旧版页面出现 8098 ms；最终完整安装版 1301 ms，等待期间 splash 可完成连续 rAF，断开后明确提示并显示账号列表 |
| 再次请求登录状态 | 不再次访问恢复网络；返回明确的 `session_restore_failed` |
| 真实 CSS 浏览器 24 行初始化 | 原版 38 次布局，修复版 3 次；翻页/末页对齐、缩放、隐藏恢复、移除待布局行及选集定位通过 |
| 真实浏览器滚轮 | 纵横向原生滚动通过，根节点阻塞 wheel 监听为零；原版横向检查超时失败 |
| 实际 CEF 空缓存及缓存命中首页 | 均加载 263 张卡片；`blockingWheelListeners=0`，通过 Debugger 获取的嵌入脚本与本地源码一致 |
| 刷新和播放前端回归 | 首页数据变化/不变、播放面板竞态、音轨元数据、视频组件及现代影院 UI 回归通过 |
| Rust | CEF 单测 110 通过，4 项依赖外部环境的测试忽略 |
| 视频桥及原生播放 | 三档固定倍率/任意 timestep 72 输出对照、HDR 三 provider；SDR/PQ/HLG 自动时间轴、seek/EOF，SDR 音轨/字幕/暂停跳转通过 |

真实 CEF 的首次页面排布仍有约 120–151 ms 的单次长任务；没有把布局次数减少当作
首次加载完全无停顿的证明。rAF P99 约 6.7 ms 仅描述回调，不代表物理上屏帧率。
缓存命中后的同内容刷新在最终观测中没有再次重建首页。原片 VFG 随机回退仍未复现，
不能把合成对照通过写成原片症状已完全消失。播放实现和 4K 联合验证详见
[VFG 交互修复记录](windows-video-components.md#2026-10-22-vfg-交互延迟与同步修复)。

复核脚本：

```powershell
node dev/windows/test_startup_session.cjs build/jellium-desktop.exe
node dev/windows/test_home_refresh.cjs
node dev/windows/test_home_scroll_browser.cjs
node dev/windows/test_home_carousel_layout.cjs
node dev/windows/profile_home_startup.cjs build/jellium-desktop.exe final-cold
```

浏览器脚本需要 Playwright。诊断脚本启动其自己的进程并只结束该测试进程；startup
脚本自动清理临时目录，profile 脚本保留私有缓存供下一次 warm 采样，验证完成后清理。
实际 CEF 核验使用 Debugger 读取脚本，不通过不支持的 `app://` fetch 测试钩子取源码。
startup 断网检查等待异步账号列表完成后再检查错误提示，避免将列表加载竞态误判为
产品缺少错误提示。

## 本机交付

已更新 `D:\softwares\MediaStationGo` 的 EXE 和视频桥，并从安装目录以私有 config/cache
启动完成断网回归。未启动默认账号实例；下次从安装目录启动即加载本轮版本。
最终核对发现原安装 mpv 及其依赖与标准测试包不同，因此同时备份并对齐了 22 个
运行 DLL（其中 3 个原来不存在），全部 47 个随包 DLL 均与标准构建逐项匹配。
mpv SHA-256 为 `042c21f14efc9df594948e0a4ccd908cc586935ef3e77665ac4cf7fa943c93c2`；
没有修改 mpv 子模块源码或用户播放设置。新增运行文件在审计 `before` 中以空 hash
标明，原有文件的备份与旧 hash 均保留，避免只恢复 EXE 而遗漏依赖。
标准 Release 产物、安装文件和备份的最终 SHA-256 以
`build/video-enhancement/vfg-black-screen/install-audit.json` 为准。
合并前已验证的 EXE 为 `7b38bb724d6a52c4c70664cf52c5ad281864da8c907734f1a7738729a5b750a0`，
视频桥为 `97ffb5fe4d9d8a61ddc28cb0cfe411234fc6bc0661bee54a9b4bfdec41a9808a`。
安装目录为 `D:\softwares\MediaStationGo`，原 EXE、视频桥及被替换的运行依赖保留在
`build/video-enhancement/vfg-black-screen/installed-backup-20261022-223254`。

原始报告保存在 `build/video-enhancement/vfg-black-screen/`：
`startup-{baseline,regression}.json`、`home-final-batched-{cold,warm}.json`、
`delivered-gpu.log`、`playback-final.log`、`rust-final.log`、`home-layout-build.log`。
保留测量与回滚产物。startup 检查已自动清理其测试目录；profile 私有缓存的后续
递归清理被自动审批以策略限制拒绝，因此保留在 `cleanup-plan.json` 清单中的路径，
没有改用其他工具绕过删除。常规 build 测试页面和临时 EXE 也保留，不删用户缓存或媒体。
本次本机交付包含首页、VFG 和播放面板修复；未推送或发布。

## 两个会话的合并核对

用户明确要求将本会话 `01a163c6-bc9a-7e40-9635-fe0d6ac447fd` 与“排查 VFG 插帧播放异常”
`01a163e3-5bdd-7441-9afa-120ff97f6712` 的修复完整合并。两个会话使用同一分支、同一
工作目录，修复已共存在工作树；本次逐项核对后统一纳入本地提交，没有再覆盖另一套补丁。

| 来源及修复 | 实现文件 | 随本地提交保留的验证 |
| --- | --- | --- |
| 首页：启动账号恢复移出浏览器创建流程 | `business_web.rs`、`mediastation_runtime.rs`、`mediastation.js` | `test_startup_session.cjs`，断网时页面响应及显式失败、不重复恢复 |
| 首页：快照内容不变时保留 DOM，主图先测量再插入 | `mediastation.js` | `test_home_refresh.cjs`，数据变化及滚动/焦点恢复 |
| 首页：原生双向滚动，各行分批计算尺寸 | `mediastation.js` | `test_home_scroll_browser.cjs`、`test_home_carousel_layout.cjs` |
| VFG：每个新帧对重绑输入并重置 ShotChange | `dev/windows/mpv/video_enhancement.cpp` | `test_video_enhancement.cpp` 的 72 输出对照，既有 GPU/原生播放脚本 |
| VFG：会话自有纹理、持续 CUDA 注册，仅在纯推理等待时释放 D3D11 锁 | 同上 | SDR/PQ/HLG、seek/EOF、4K25 双目标验证；map/unmap 和传输完成同步保留 |
| 播放：原生诊断在后台执行，面板立即显示并隔离过期响应 | `mediastation_runtime.rs`、`mediastation.js` | `test_player_panel_async.cjs`，关闭、切换、同类重开及错误；既有音轨/组件回归 |
| 随包桥与安装运行依赖 | `resources/win/video-bridge/video_enhancement.dll` 及其 README | 源码 blob 与 DLL hash；标准包和安装目录的 EXE、mpv、视频桥及全部 47 个 DLL 对照 |
| 可重复排查及台账 | `profile_home_startup.cjs`、`test_vfg_cadence.ps1`、两份 Windows 视频文档和本文 | 原生基准保留可选 `SourceRate=25`，默认源帧率不变；CEF 首页 profile 保留空缓存与缓存命中核验 |

所有六个新增回归/诊断脚本及 GPU 测试扩展均纳入提交。联合检查重新运行首页、面板、
播放元数据、组件/插帧及影院 UI 回归，结果保留在
`build/video-enhancement/combined-fixes-regression.log`。两个会话的原生 GPU、110 项 Rust
单测及 4K/CEF 测量记录沿用相同源码与产物指纹，安装记录覆盖较早的“未替换安装版”状态。
临时捕获实验和日志保留为 build 证据，不混入产品源码。未将随机回退、实际弹幕上屏
流畅度或首次排布停顿写成已完全解决。

合并提交后的标准重建、安装文件指纹及文件清单记录在
`build/video-enhancement/combined-fixes-audit.json`；当前 EXE 指纹以该审计和
`vfg-black-screen/install-audit.json` 为准，避免将合并前候选误当作后续重建产物。
