# Windows 视频组件管理与接入

2026-10-11，分支 `codex/mediastation-windows-spike`，本地实现与验证记录。

侧栏新增“组件管理”，原设置中的 RIFE 安装、卸载、待安装取消与重启确认均迁入。
四项组件共用同一个按用户安装的事务实现，各自有独立目录、下载状态和缓存。
RTX Video、NVIDIA VFG 与 DLSSNR 均已接入 mpv 原生播放，核心参数可配置。
安装不会自动开启增强，安装和卸载在退出全部实例并重启后生效。

| 组件 | 安装管理 | 播放能力 | 当前边界 |
| --- | --- | --- | --- |
| RIFE | 沿用既有组件，增加本地导入入口 | 原有三档与固定 2 倍插帧 | ABI 8、模型、TensorRT profile 与 HDR 路径保持原合同 |
| RTX Video | 原版 `nvngx_vsr.dll` 1.6.0.0 | D3D11 RTX Video SDK，质量 1–4 与 2x/QHD/4K–8K 目标尺寸 | 渐进 SDR / HDR10；HDR 使用代理推理和原图残差合成 |
| NVIDIA VFG | Maxine VFX 1.3.0 五个原始 DLL | Low/Medium/High，2–8 倍帧率，可配置自动切镜检测 | 渐进 SDR / HDR10；HDR 原生 RGB10A2/PQ；与 RIFE 互斥 |
| DLSSNR | NVIDIA 原版 310.8.0.0，可安装、取消、卸载 | D3D12 Feature 18 实际推理，模型参数与处理尺寸可配置 | 渐进 SDR / HDR10；HDR 使用代理推理和原图残差合成 |

## 使用

1. 打开侧栏“组件管理”，点击所需组件的“从本地安装”，选择对应组件 ZIP。
2. 校验并排队完成后，确认重启；也可稍后重启或取消待安装。
3. 播放渐进 SDR 或 HDR10 视频，在播放器“画面增强”菜单开启所需组件，展开参数后应用。
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
- 主程序携带自写 ABI 3 的 `video_enhancement.dll` 桥和 `nvngx.dll` NR 适配器，不携带上述可选 NVIDIA DLL。
  安装成功只说明文件经过校验；GPU/驱动支持在实际创建 SDK feature 时验证。
- SDR 链路为 `D3D11 解码 → d3d11vpp BGRA8 → 可选 DLSSNR → 可选 VFG → 可选 VSR →
  d3d11vpp NV12 → 原有 VO`。BGRA→NV12 是输出格式合同的一部分。
  生产帧保持在 GPU，不经过 CPU 读回；GPU 探针中的读回仅用于检查结果。
- mpv 继续拥有 PTS、音轨、字幕、播放速度和 EOF。VFG 第 i 个中间帧 PTS 为 prev + (current - prev) × i / multiplier；
  seek、EOF、分辨率或静态格式变化清空历史并释放 SDK 会话。VFG 要求源 PTS 递增，
  相邻间隔不超过 250 ms。
- SDK 初始化、执行和释放均隔离共享 D3D11 上下文状态，VFG 使用与解码设备匹配的
  CUDA 设备。NGX 参数在 Shutdown 前释放；释放异常输出明确诊断。
- 加载请求附带明确的增强选择，旧前端缺省为关闭。播放中切换复用原有重载流程，
  保留进度、片源版本、播放/暂停意图；切集保留增强选择。
- HDR10 链路为 `D3D11 P010 → 可选 DLSSNR → 可选 VSR → 可选 VFG → P010 → 原有 VO`。
  HDR 顺序将 VFG 放在超分与色彩恢复之后；不会经过 BGRA/NV12 的普通视频转换。
- 预加载拒绝已知 HLG、HDR10+ 和 Dolby Vision；原生滤镜继续检查每帧的格式、
  BT.2020 NCL/PQ、明确量化范围、色度位置、隔行与动态元数据。未知格式明确失败。
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
当前未接 NR 光流、多层模型或参考项目额外的残差/防闪烁参数。

## HDR10 处理

主包桥 ABI 升为 3，与自定义 mpv 同步构建；可选 NVIDIA 运行库及三个组件 ZIP 不变。
HDR 输入与输出均为 P010，支持 Limited/Full 和 left/top-left/center 色度位置。
BT.2020 NCL 转 RGB、ST.2084 EOTF/OETF 和色度重建在 GPU 执行；P010 使用整数
UAV 将十位码值写入高十位，避免 UNORM 写入的舍入使低六位不再为零。

VFG 使用 SDK 的 RGB10A2/P32 编码。RGB10A2 位布局放在 R32_UINT 纹理中，与
CUDA 互操作后直接交给 VFG，不将 HDR 转为 SDR。所有中间输出再转换为 P010，
原有色彩属性和静态 HDR 元数据通过 `mp_image_copy_attributes` 传递。

VSR/NR 保留线性 BT.2020 FP16 原图，另生成固定 203 尼特参考的 BT.709/sRGB 8 位
代理图供现有模型推理。代理映射采用 `x/(1+x)`，增强前后差值在线性空间转换回
BT.2020 后叠加到原图；保护接近黑位和压缩高光，残差为零时保持原图。
参考 [Veyra HDR 代理与恢复思路](https://github.com/Likely7/Veyra-NRVideo/blob/4ceb2df78ebcefef8c2b564afb54eed66b81d595/shaders/HdrColor.hlsli)，
本项目自写 shader，代理曲线与残差保护并非 Veyra 逐像素同一实现，也不加载 RenoDX 插件。
这是应用层 HDR 适配，不能称为 NR 模型原生高精度 HDR 推理。

滤镜元数据新增 `dynamic-range`、`sdk-encoding`、`hdr-processing`，区分
`rgb10a2-pq / native-10bit` 与 `bgra8 / proxy-residual`，可审计实际执行路径。
显示器是否用 HDR 输出仍由 mpv/Windows 的原有显示链路决定，本次不修改全局显示设置。

播放“画面增强”内展开各组件参数，点击“应用参数并启用”。VSR 质量 1–4；目标尺寸为
2 倍、QHD、4K、5K、6K、7K、8K 的 16:9 外框，保持原比例且不缩小源；VFG 质量
0–2、倍率 2–8、自动切镜；NR 风格 0–2，强度/局部色调/局部结构 0–1，皮肤结构 -1
或 0–2，自动遮罩/UI 修正开关与处理尺寸。参数只随当前播放及切集保留，未新增全局偏好。
前后端拒绝未知字段、无效类型、非有限值、越界参数和错误的返回参数，不自动降档。

## 验证记录与复现

### HDR10 追加验收（2026-10-11）

RTX 5070 Ti / 596.49 上，`/W4 /WX` 编译和 137 项常规 Rust 单测通过
（CEF 104、组件/RIFE 33）；JS 回归与 scoped Clippy 通过。浅色/深色参数截图已更新，
仍是离线真实渲染函数的布局检查，不是用户组件安装或实体 HDR 显示的证据。

`test_hdr_video_enhancement.cpp` 使用 0、1、100、203、1000、4000 尼特灰阶与
BT.2020 彩色块：Limited/Full × 三种色度位置 × 代理/10 位路径共 12 组往返，
灰阶码值误差均为 0，彩色误差至多 1，所有 P010 字的低六位均为零。三项真实
provider 各连续执行 6 次，VFG/NR 分别改变 34434/234581 个 P010 字；VSR 输出
1280×720。NR 允许改变未保护的中间调，最大灰阶码值变化为 8；黑位和 4000 尼特
采样误差为 0，不把模型产生的色调变化称作无损恢复。

播放使用 4 秒、24 fps HEVC Main10 + AAC 合成源，先用 ffprobe 核验 VUI，再在正式
libmpv 的隐藏 HWND / gpu-next / d3d11va 下运行。初次生成时，本机 x265 未从 FFmpeg
通用选项写出 PQ/原色标记；已显式设置编码器 `colorprim=9:transfer=16:colormatrix=9`，
测试源不满足这些元数据时直接失败，没有解除滤镜格式保护。

`build/video-enhancement/hdr-playback-only.log` 六条链路均通过精确 seek 和 EOF；
输出检查 P010、PQ、BT.2020，并逐项断言 min/max luminance、MaxCLL、MaxFALL
与输入相同。HDR VFG 使用 Low 2x；超分目标为 720p，NR 默认 1080p 上限，
1920×1080 样本按 720p 推理再合成回源尺寸。结果如下：

| HDR10 链路 | VSR / VFG / NR 调用数 | 输出 | 最大 A/V 偏差（秒） |
| --- | --- | --- | --- |
| VSR | 96 / 0 / 0 | 1280×720 | 0.0417 |
| VFG | 0 / 95 / 0 | 640×360 | 0.0169 |
| VSR → VFG | 96 / 94 / 0 | 1280×720 | 0.0208 |
| NR | 0 / 0 / 96 | 640×360 | 0.0417 |
| NR → VSR → VFG | 96 / 94 / 96 | 1280×720 | 0.0208 |
| NR（1080p 源 / 720p 推理） | 0 / 0 / 96 | 1920×1080 | 0.0417 |

本轮同时复核七条原 SDR 播放链路；High VFG 8x 仍有约 0.824 秒 A/V 偏差，
仅通过功能性 seek/EOF 检查，未通过实时流畅验收。测试使用 `ao=null`，后台有
普通桌面应用运行；这些短片结果不代表 4K 长片、实体声音/HDR 显示、长期显存
或跨 GPU 验收。HDR10+、HLG、Dolby Vision 仍明确拒绝。测试临时媒体、缓存和
浏览器 profile 清理后只保留脱敏日志和文档截图。

原生数值日志为 `build/video-enhancement/hdr-gpu-acceptance.log`；
`test_video_enhancement.ps1 -HdrOnly` 可单独复现 HDR 播放，省略该开关同时跑 SDR。

标准 Release 代码已编译通过（`hdr-release-build.log`），但复制到 `build` 时，
正在运行的旧播放器锁住 EXE，返回 Windows `os error 32`。当前 `build` 的播放器
仍为此前 SDR 版本；用户确认退出后再完成同目录产物更新和新版本启动/IPC 检查。
这项最终更新尚未完成，不能将已编译的缓存 EXE 当成已更新的运行版本。

### 原 SDR 接入记录

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

此前 SDR 标准构建 SHA-256：应用 `703774f670bc752bb9cf1403e2bd45db0727133fc05e939f471185fd1c9db692`，
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
