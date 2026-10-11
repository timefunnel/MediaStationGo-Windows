# Windows 视频组件管理与接入

2026-10-11，分支 `codex/mediastation-windows-spike`，本地实现与验证记录。

侧栏新增“组件管理”，原设置中的 RIFE 安装、卸载、待安装取消与重启确认均迁入。
四项组件共用同一个按用户安装的事务实现，各自有独立目录、下载状态和缓存。
RTX Video、NVIDIA VFG 与 DLSSNR 均已接入 mpv 原生播放，核心参数可配置。
安装不会自动开启增强，安装和卸载在退出全部实例并重启后生效。

| 组件 | 安装管理 | 播放能力 | 当前边界 |
| --- | --- | --- | --- |
| RIFE | 沿用既有组件，增加本地导入入口 | 原有三档与固定 2 倍插帧 | ABI 8、模型、TensorRT profile 与 HDR 路径保持原合同 |
| RTX Video | 原版 `nvngx_vsr.dll` 1.6.0.0 | D3D11 RTX Video SDK，质量 1–4 与 2x/QHD/4K–8K 目标尺寸 | 渐进 SDR；本机 RTX 5070 Ti 验证通过 |
| NVIDIA VFG | Maxine VFX 1.3.0 五个原始 DLL | Low/Medium/High，2–8 倍帧率，可配置自动切镜检测 | 渐进 SDR；与 RIFE 互斥；本机 RTX 5070 Ti 验证通过 |
| DLSSNR | NVIDIA 原版 310.8.0.0，可安装、取消、卸载 | D3D12 Feature 18 实际推理，模型参数与处理尺寸可配置 | 渐进 SDR；本机 RTX 5070 Ti 的原生与 mpv 验证通过 |

## 使用

1. 打开侧栏“组件管理”，点击所需组件的“从本地安装”，选择对应组件 ZIP。
2. 校验并排队完成后，确认重启；也可稍后重启或取消待安装。
3. 播放渐进 SDR 视频，在播放器“画面增强”菜单开启所需组件，展开参数后应用。
   NR、VFG 与超分可叠加；开启 RIFE 会关闭 VFG。关闭插帧保留 NR 与超分选择。

本次三个新组件包仅生成于 `dist/components`，尚未发布 GitHub Release。
“下载并安装”仍使用既有受校验的 Release 下载链路，未发布时明确提示使用本地包，
不会改从第三方镜像寻找同名 DLL。组件包文件名如下：

| 组件包 | SHA-256 |
| --- | --- |
| `MediaStationGo-rtx-video-x64-v1.zip` | `6f8c7ca16634437f83f8890433060a93daaca29cdb6bbb361829002d4c431e25` |
| `MediaStationGo-dlssnr-x64-v1.zip` | `c7daf9e2a8287b228c11055fdc1f598e92697948eef44c7729d113ba8f5ed686` |
| `MediaStationGo-nvidia-vfg-x64-v1.zip` | `13c2e0493b1aaa6bb8d3b93f9b6a70d60359db78c672d6963dfcea50987f1cdb` |

## 实现边界

- `ComponentKind` 固定组件身份、平台、运行库版本与精确文件集。解包仍拒绝路径穿越、
  重解析点、额外文件、重复文件和不匹配的哈希，复用既有排队、激活、取消、卸载、
  归属检查和跨进程租约。RIFE 路径没有迁移。
- 用户 cache 下 `components/{rife,rtx-video,dlssnr,nvidia-vfg}/{active,pending}`
  各自存放运行库；专属缓存为 `frame-interpolation`、`rtx-video`、`dlssnr`、`nvidia-vfg`。
  卸载清理仅针对选中组件及其专属缓存。
- 主程序携带自写 ABI 2 的 `video_enhancement.dll` 桥和 `nvngx.dll` NR 适配器，不携带上述可选 NVIDIA DLL。
  安装成功只说明文件经过校验；GPU/驱动支持在实际创建 SDK feature 时验证。
- mpv 链路为 `D3D11 解码 → d3d11vpp BGRA8 → 可选 DLSSNR → 可选 VFG → 可选 VSR →
  d3d11vpp NV12 → 原有 VO`。BGRA→NV12 是输出格式合同的一部分。
  生产帧保持在 GPU，不经过 CPU 读回；GPU 探针中的读回仅用于检查结果。
- mpv 继续拥有 PTS、音轨、字幕、播放速度和 EOF。VFG 第 i 个中间帧 PTS 为 prev + (current - prev) × i / multiplier；
  seek、EOF、分辨率或静态格式变化清空历史并释放 SDK 会话。VFG 要求源 PTS 递增，
  相邻间隔不超过 250 ms。
- SDK 初始化、执行和释放均隔离共享 D3D11 上下文状态，VFG 使用与解码设备匹配的
  CUDA 设备。NGX 参数在 Shutdown 前释放；释放异常输出明确诊断。
- 加载请求附带明确的增强选择，旧前端缺省为关闭。播放中切换复用原有重载流程，
  保留进度、片源版本、播放/暂停意图；切集保留增强选择。
- 已知 HDR/Dolby Vision 元数据在预加载时拒绝；原生滤镜也拒绝 PQ、HLG 和隔行输入。
  SDK 失败或请求的滤镜失效会触发明确播放错误并停止，不静默退回普通处理。

## 来源与 DLSSNR 实现

运行库下载来源为 [Veyra 2.0.6 NVIDIA 便携包](https://github.com/Likely7/Veyra-NRVideo/releases/tag/v2.0.6)
与 [NVIDIA RTX Video SDK](https://developer.nvidia.com/rtx-video-sdk)。VFG 接口依据
[NVIDIA Maxine Video Frame Generation 文档](https://docs.nvidia.com/maxine/vfx/latest/Filters/VideoFrameGeneration.html)
与公开头文件，自写桥接与 mpv 滤镜没有复制 Veyra 实现。

便携包 SHA-256 `ddc4c3a74508ceac19a9f829eff918d8b23fa43db5db8e4936effe7f9212cf6b`
已匹配发布资产 digest。NGC 的 `RTX_Video_SDK_v1.1.0.zip` 本地 SHA-256 为
`abf4f34e2b5a618e355b0d5a0365d8ecc3db4396e756e4c850a867e1ae2ed69e`，
未提供可比对的发布者 digest。实际打包的 DLL 逐文件匹配固定 SHA-256，
Authenticode 均为 Valid/NVIDIA；各包带对应许可证与第三方声明。

DLSSNR 采用 `runtime/experimental/nr-original/nvngx_dlssnr.dll`，SHA-256
`e16bcf15e16e13f527491cdf7845b2fe6521a738d8f7c9c721866a8496e1fc8e`。
已验证 Init_Ext、Feature 18 创建、EvaluateFeature 推理、释放与共享纹理互操作。
调用合同参考固定源码 [4ceb2df 的参数与评估调用](https://github.com/Likely7/Veyra-NRVideo/blob/4ceb2df78ebcefef8c2b564afb54eed66b81d595/src/pipeline/EnhanceGraph.cpp)，
不复制参考项目实现。本项目自写独立 `nr_adapter.cpp`，编译为真实文件名 `nvngx.dll`，
满足运行库的调用者名称合同；没有修改原版 DLL、挂钩系统 API 或伪造 GPU 架构。

NR 使用同一 DXGI 适配器上的 D3D12 设备、共享 NT 纹理与 D3D11/D3D12 fence。
D3D11 计算着色器将 SDR BGRA 帧转换为 RGBA8 推理输入，NR 在新的 D3D12 命令列表执行，
结果差量在 GPU 上合成回原始尺寸。默认处理上限为 1920×1080，可选原尺寸、480p、
720p、900p、1440p；始终保持比例且不放大低分辨率输入。运动与深度输入使用明确的零图，
当前未接 NR 光流、HDR/RenoDX 色彩恢复、多层模型或参考项目额外的残差/防闪烁参数。

播放“画面增强”内展开各组件参数，点击“应用参数并启用”。VSR 质量 1–4；目标尺寸为
2 倍、QHD、4K、5K、6K、7K、8K 的 16:9 外框，保持原比例且不缩小源；VFG 质量
0–2、倍率 2–8、自动切镜；NR 风格 0–2，强度/局部色调/局部结构 0–1，皮肤结构 -1
或 0–2，自动遮罩/UI 修正开关与处理尺寸。参数只随当前播放及切集保留，未新增全局偏好。
前后端拒绝未知字段、无效类型、非有限值、越界参数和错误的返回参数，不自动降档。

## 验证记录与复现

环境：RTX 5070 Ti，驱动 596.49（Win32 32.0.15.9649），MSVC 14.50，
mpv 子模块 `9347cbc4d4333daf1b9d6d9ff292f2c99dd2c380`，RTX Video SDK 1.1.0。
开发依赖位于忽略目录 `third_party/video-enhancement-deps`，SDK 与 Veyra 下载解包
目录分别为 `RTX-Video-SDK-1.1.0` 和 `Veyra-2.0.6-NVIDIA`。

```powershell
. ./dev/windows/env.ps1
$env:CEF_PATH = (Resolve-Path '.cache/cef/150.0.10/cef_windows_x86_64').Path
cargo test --manifest-path src/Cargo.toml -p jfn-frame-interpolation -p jfn-cef --lib
node ./dev/windows/test_frame_interpolation_ui.cjs
node ./dev/windows/test_modern_cinema_ui.cjs
node ./dev/windows/test_video_components_ui.cjs
./dev/windows/build_mpv_source.ps1
./dev/windows/test_video_enhancement.ps1
./dev/windows/build.ps1
```

- 136 项常规 Rust 单测通过（CEF 103、组件/RIFE 33）；5 项依赖账户、硬件或网络的
  测试默认忽略。另显式执行新组件实包安装→重启激活→逐文件校验→卸载测试通过，
  使用临时目录，没有修改真实用户安装目录。
- 三个 JS 脚本通过，包括原 RIFE 重启确认/取消/故障恢复，新组件独立状态、损坏待安装、
  NR 可播放状态、状态查询重试、完整参数与错误返回验证，以及实际嵌入重载函数的进度/版本保留和 RIFE/VFG 互斥。
- 正式应用使用独立测试 config/cache 启动，原版 NR 包展开到测试 active 目录后，真实
  CEF 页面经原生 IPC 返回 installed/runtimeVerified/playbackAvailable 均为 true，
  默认处理上限 1080，experimental 为 false。复核脚本为
  `test_video_components_app.cjs`，依赖测试启动时生成的 `build/video-enhancement/smoke.json`；
  该检查只验证启动与状态，不替代安装事务或推理验证。应用已退出，临时状态已清理，
  脱敏结果保留在 `build/video-enhancement/app-smoke-validation.json`。
- `/W4 /WX` 原生桥、NR 适配器与探针编译通过。640×360 合成 BGRA 输入：VSR 得到
  1280×720，VFG 输出 640×360 且不同于两个输入，NR 改变 663107 个 RGB 分量，平均
  绝对差 11.7717/255；三个 provider 各连续 6 次调用成功。NR 的三个参数组合分别
  连续 6 次调用，风格 0/1/2、强度 0/0.7 的输出均与默认输出存在实际 RGB 差异。
- 使用隐藏原生 HWND、正式自定义 libmpv、4 秒 24 fps H.264 合成源及 AAC 音轨，
  分别验证 VSR、VFG、VFG+VSR、NR、NR+VFG+VSR、High VFG 8x，以及 1080p NR
  片源按 720p 处理后合成回原尺寸；达到 12 次调用后精确跳到 1 秒并播放至 EOF。
  最终复测（2026-10-11，`build/video-enhancement/playback-validation-final.log`）结果为：

```text
mode=1  vsr=96  vfg=0   nr=0   output=2560x1440 max-avsync=0.0417
mode=2  vsr=0   vfg=182 nr=0   output=640x360   max-avsync=0.0112
mode=3  vsr=272 vfg=180 nr=0   output=2560x1440 max-avsync=0.0112
mode=4  vsr=0   vfg=0   nr=96  output=640x360   max-avsync=0.0417
mode=7  vsr=272 vfg=180 nr=93  output=2560x1440 max-avsync=0.0112
mode=8  vsr=0   vfg=623 nr=0   output=640x360   max-avsync=0.8243
mode=12 vsr=0   vfg=0   nr=101 output=1920x1080 max-avsync=0.0417
# 各项 resets=3, seek=1, eof=1；mode=2/3/7 为 Medium VFG 3x。
# mode=12 实际 NR 内部尺寸 1280x720；平均调用耗时 4.712 ms。
```

计数包含 SDK 自动切镜时的输出，不能视为每帧都新增推理的证明，也不是帧率基准。
`ao=null` 保留音频计时，未验证实体音频输出、长片音画同步、4K 性能、字幕渲染、
长期显存占用或其他 GPU/驱动。High VFG 8x 平均调用耗时 6.946 ms，超出 24×8 fps
的实时预算，实测出现音画不同步警告，不能标记为实时流畅验收通过。界面保留明确
的用户参数与耗时提示，不隐藏降档。测试脚本生成的媒体、探针与临时缓存会自动清理。

初次 mpv 验证遇到 BGRA 输出与 VO 纹理格式不匹配及共享上下文状态污染；已通过明确
NV12 输出转换与 SDK 全生命周期上下文隔离修复，修复后三条链路均无上述错误。
后续复核补上 NGX 参数释放：Shutdown 后释放会返回 `0xbad00007`，调整至 Shutdown
前释放后，SDK 与 mpv 复测没有再报释放错误。

常规 Windows Release 在标准 `build` 目录生成。新代码 scoped Clippy 使用
`--no-deps -- -D warnings -A clippy::redundant_closure` 通过；完整严格检查仍被既有
`src/mpv/src/stream_cb.rs:339` 的 `not_unsafe_ptr_arg_deref` 与
`mediastation_runtime.rs` 的两处既有 `redundant_closure` 阻挡，本任务未修改无关代码。
未发布组件 Release、推送或部署；提交状态以本地 Git 历史为准。

当前标准构建 SHA-256：应用 `703774f670bc752bb9cf1403e2bd45db0727133fc05e939f471185fd1c9db692`，
视频桥 `e3dbb4b5fc97fb2bc40644afad8a4488efe548e47be3460ea1463c20ef6cabd0`，
NR 适配器 `5d075b793d9d9e11ed09641a77b20212137fe6f4153532b206ede35730e36f5d`。

下图为实际 HTML/CSS 的离线布局检查，状态文字为静态测试输入；行为验证以上述 JS
和 Rust 测试为准，不把图片当成真实用户组件已安装的证据。

![组件管理浅色布局](images/video-components-light.png)
![组件管理深色布局](images/video-components-dark.png)

以下参数面板由真实渲染函数与 CSS 生成离线测试状态；NR 安装状态为测试输入，
不代表真实用户组件已安装。可复现 `node dev/windows/render_video_parameters_ui.cjs`。

![DLSSNR 参数浅色布局](images/video-parameters-light.png)
![DLSSNR 参数深色布局](images/video-parameters-dark.png)
