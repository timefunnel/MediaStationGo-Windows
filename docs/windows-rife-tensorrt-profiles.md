# Windows RIFE TensorRT Profile 设计结论

本文记录 Windows MediaStationGo 播放器中 RIFE 模型、输入对齐和
TensorRT optimization profile 之间的边界。本文用于后续 Engine 设计、
性能基准和产品档位讨论，不把 TensorRT profile 当成产品档位。

## 当前任务顺序

以下三项是当前 Windows 播放器后续工作的固定顺序。前一项没有完成验收前，
不得用其未稳定的数据推进后一项的最终设计或基准结论。

### 进度台账

最近更新：2026-08-05，分支 `codex/mediastation-windows-spike`。均衡档精度图
实现检查点 `aa760f2`、验证记录 `db88468` 和画质门禁 `95a4cff` 已推送；
任务 1 完成。任务 2 的 profile 集合冻结、通用合同实现、正式 mpv/Release
重建和真实 4K HDR10 播放器验收均已完成，实现提交为 `fdda0a4`。任务 3 停止交付：
NVIDIA 下 `d3d11va` 直通会阻断 VRR，而正式 RIFE 链路必须保留 D3D11 P010
零拷贝；`d3d11va-copy` 的 4K 实测吞吐不合格，软件解码也不满足产品约束。
固定显示模式硬切会黑屏且不属于 VRR，已连同实验性 VRR patch、协议和设置页入口
一起撤回；撤回后的自定义 mpv 与 Release 已干净重建并通过启动烟测。后续只在
驱动/上游解决直通 VRR，或出现经验证的零拷贝替代链路时重启。
质量档 HDR10 偶发花屏仍在独立排查中；该问题不改变已冻结的 RIFE 三档、
Engine/profile 或场景阈值，但在同片长测和视觉验收通过前不得宣称质量档问题已关闭。
播放退出后的 UI 帧率问题已完成生命周期修复、无 RIFE 隔离验证和正式 RIFE
实机验收；真实 4K HDR10 P010 均衡档退出后，首页 152 Hz rAF 无 `>10 ms`
间隔，本问题已关闭。

| 任务 | 状态 | 已完成 | 下一步 |
| --- | --- | --- | --- |
| `scale=0.5` FP32 性能 | 完成：实现、数值、探针、真实播放和三档逐中间帧 A/B 均通过 | 已淘汰两个失败候选；最终图只为 5 组最终坐标 `Add + Transpose + GridSample` 保留 FP32；三层数值验证、3840x2160 HDR10 真实播放和 23 个同片段中间帧检查通过，TensorRT 探针为 `26.342 ms` | 冻结本检查点，不再修改另外两档；后续变化必须重新经过相同质量门禁 |
| TensorRT profile 优化 | 完成：合同、构建、缓存、切换、真实播放和回归均通过 | schema 4 / runtime ABI 7 / metadata schema 3 已同步；三档各一个 Engine，profile 数为 `4/3/4`；正式 Release 在同一 3840x2160 HDR10 P010 影片中命中质量 `3`、均衡 `2`、Lite `3`，模型与 scale 均和用户选择一致 | 冻结本检查点；后续 profile 或 ABI 变化必须重新执行原生 probe、正式构建和三档真实播放验收 |
| 窗口刷新率自匹配 | 阻塞并停止交付：当前 NVIDIA + RIFE 零拷贝合同下没有可用实现 | 最小 libmpv 软件源 60 fps 可 `active=yes`；正式 `d3d11va` 直通为 `active=no`。`d3d11va-copy` 同一 4K 样本由 `Dropped: 10` 恶化为 `Dropped: 265`；固定模式硬切会黑屏 | 不保留未完成的生产接线。只有 NVIDIA/上游解决 `d3d11va` 直通 VRR，或新的 D3D11 P010 零拷贝解码互操作通过性能与画质验收后才重启 |

当前不得重复或误判的结论：

- 质量优先 RIFE v4.26 `scale=1.0` 已完成主计算图 FP16 转换；当前
  3840x2160 probe 为 TensorRT `30.373 ms`、端到端 `31.516 ms`。
- 流畅优先 RIFE v4.25 Lite 已完成主计算图 FP16 转换；当前 3840x2160
  probe 为 TensorRT `22.687 ms`、端到端 `23.820 ms`。
- 均衡优先 `scale=0.5` 的新图为 `fp16_compute_fp32_grid_final`；3840x2160
  probe 为 TensorRT `26.342 ms`、端到端 `27.498 ms`。导出、数值、原生探针、
  真实播放和同片段逐中间帧 A/B 均已通过，任务 1 已完成。
- 上述数据来自 RTX 5070 Ti、TensorRT-RTX 1.4.0.76、当前缓存 Engine、
  预热 30 次和测量 100 次。它们是问题定位基线，不是最终跨设备结论。
- 最终 TensorRT profile 集合已实现并通过正式播放器验证；VRR 已在独立 mpv
  前台无边框全屏的 60 fps 软件源中实际激活，但正式播放器的 `d3d11va` 直通
  窗口和全屏均未激活，不能开放为能力，也不能声称已证明物理刷新节奏或 LFC。
  `47.952 fps` 本轮最小探针也未激活，进一步证明不同显示器的 VRR 下限必须实测。

### 进度更新规则

- 每完成一个可复现步骤，立即更新本台账的状态、证据、下一步和对应 commit。
- “已诊断”“已实现”“已验证”“已提交/推送”必须分开记录；没有完成验收
  不得标记为完成。
- 运行大型构建或长基准前，先检查本台账、Git 状态和现有产物，避免重复执行。
- 基准必须记录模型 hash、Engine/profile、输入和 padded shape、GPU/驱动、
  TensorRT 版本、warmup、迭代次数、runtime cache 和后台负载。
- 每个小步验证通过后提交并推送当前分支；失败也要记录真实错误和仍待解决项，
  不得用隐藏回退或伪成功推进台账。
- 后续任务在上下文压缩或干净续接后，应先读取本文和当前工作区真实状态，
  不重新推断已经有证据的结论。

### 2026-08-05 播放退出后的 UI 帧率修复

- 已诊断：首页在 152 Hz 下连续 3 秒得到 `456` 帧，rAF p95/max 均约
  `6.7 ms`，没有 `>10 ms` 间隔；强制重绘缓存首页约 `5.3 ms`，也没有掉帧。
  公开 Sintel 样片在不启用 RIFE 时，普通 mpv/VO 停止前后仍稳定为 152 Hz。
- 根因边界位于退出生命周期：旧 `finishPlayer(true)` 发出 `playerStop` 后立即
  清空前端 player、恢复首页轮播和 UI 动画，但 mpv 文件、RIFE filter 与 D3D11
  资源仍在异步卸载；随后到达的 `canceled` 因 player 已为空而被忽略。
- 已实现：新增显式 `player.exiting` 状态。主动退出只发送一次 `playerStop`，
  保持 player mode 和首页轮播暂停，显示“正在退出播放”，并锁住播放、seek、
  全屏、插帧、音轨和信息操作；只有收到 `finished`、`canceled` 或终止 `error`
  后才清理 player 并恢复首页。没有超时兜底，也不在未收到终止事件时伪装成功。
- 静态与构建验证：`node --check src/web/mediastation.js`、`git diff --check`、
  `cargo fmt --check` 均通过；Windows Release 增量构建成功，产物 SHA-256 为
  `42e39680286994cc0ee99c2d459970bdc814003535e9a4bd9205a5bea9fb2038`。
  workspace Clippy 未通过的是既有 `src/mpv/src/stream_cb.rs:167`
  `clippy::not_unsafe_ptr_arg_deref`，与本次 JS 改动无关，未越界修改。
- 无 RIFE 隔离验证：公开 Sintel 播放开始后连续触发两次退出，实际只调用一次
  `playerStop`；stop 同步返回时 player layer 仍可见且保持 player mode，约
  `2-6 ms` 后收到 `canceled` 才恢复首页。首帧前退出也已覆盖：退出遮罩立即
  生效，等 `started` 确认可停止后只发送一次 stop，再由 `canceled` 收尾。
  恢复后 3 秒 rAF 为 `456` 帧，
  p95/p99/max 均约 `6.7 ms`，`>10 ms` 和 `>16.7 ms` 间隔均为 `0`。
- 正式 RIFE 实机验收：MediaStation 会话已从 Windows Credential Manager 恢复，
  真实《黑衣人2》以 CDN 直连、Range `206` 播放 3840x2160 HEVC Main 10 HDR10。
  日志确认 `RIFE frame interpolation started`，实际为 RIFE v4.26 `scale=0.5`、
  D3D11 P010、TensorRT-RTX 1.4.0.76、`d3d11va` 和 profile `2`；退出时 runtime
  summary 为 `pairs=500 inferred=499 scene-cuts=1 failures=0`。
- 正式 RIFE 主动退出验收：连续两次触发退出实际只调用一次 `playerStop`；
  stop 返回时 player layer 仍可见、保持 player mode 并显示“正在退出播放”。
  原生日志先记录 filter `Frame interpolation shutdown` 和 `RIFE runtime summary`，
  然后 mpv `end-file=Canceled`，最后前端收到 `canceled` 才恢复首页。
- 返回首页后立即采集 3 秒 rAF：`3006.1 ms` 内 `457` 帧，p95/p99/max
  均约 `6.7 ms`，`>10 ms` 和 `>16.7 ms` 间隔均为 `0`。当前状态为
  “已诊断、已实现、无 RIFE 隔离验证通过、正式 RIFE 实机验收通过”；
  实现与隔离验证提交 `8ccf79e` 已推送，正式验收证据随本次台账更新提交。

### 1. 解决 `scale=0.5` 的 FP32 性能问题

- 只修改 RIFE v4.26 `scale=0.5` 的导出图、精度路径、Engine 元数据以及
  直接相关的测试和 probe。
- 不修改质量优先的 RIFE v4.26 `scale=1.0`，也不修改流畅优先的
  RIFE v4.25 Lite；不得改变这两档的 ONNX、Engine 契约、模型映射或产品行为。
- 修复前的 `scale=0.5` 图虽然是 FP16 IO，但仍保留 129 个 FP32 精度节点、
  4 个 FP32 编码器卷积和 5 个 FP32 GridSample。manifest 的 `fp16` 标签不能
  代替内部计算精度检查；该旧图只作为修复前基线，不得再描述成当前实现。
- 必须先定位 FP32 islands 对 1080p、2K、UHD 4K、DCI 4K 和超宽输入的实际
  耗时，再缩小或消除性能关键的 FP32 路径。不能为了提速而跳过数值一致性。
- 至少验证：官方 vs-rife PyTorch 输出对 ONNX、ONNX 对 TensorRT Engine、
  TensorRT probe 对播放器真实输出。复杂运动、遮挡、细纹理和切镜必须纳入
  真实影片逐帧检查。
- 失败必须显式，不得回退到 Lite、`scale=1.0`、NVOF 或透传路径。
- 只有数值、画质、任意合法分辨率和性能均通过后，才能冻结
  `scale=0.5` 的基准数据并开始最终 profile 优化。

#### 2026-08-01 逐层耗时归因检查点

- 输入 ONNX SHA-256：
  `dda9c05402da61a383f3ce24d4df66ea7d0006418c98e831f55442a1b6e295ac`；
  当前缓存 Engine SHA-256：
  `b081b039b93f8b24153e75e4dbbade16dfe00a5aade7bac744cb1f022f98c9f6`。
- 使用 TensorRT-RTX 1.4.0.76 按产品相同的两套 profile 临时构建
  `profilingVerbosity=detailed` 诊断 Engine；选择 profile 1，实际输入为
  `FP16 [1,11,2176,3840]`，加载现有 4K runtime cache 的临时副本。
- 独立逐层 profile 共 35 次，图内总时间均值 `33.0143 ms`。5 个层名中含
  `Grid` 的融合层均值依次为 `1.2169`、`0.6294`、`2.0523`、`7.1017`、
  `3.6783 ms`，合计 `14.6786 ms`，占 `44.46%`。ONNX 静态类型审计同时
  确认这 5 个 `GridSample` 输出仍为 FP32，因此采样/坐标路径是第一性能目标。
- 逐层 profiler 会插入同步并禁用 CUDA Graph，本组总时间只用于图内成本归因，
  不替代播放器 probe 的正式 TensorRT/端到端基准。正式性能结论仍使用相同
  warmup、迭代次数、runtime cache 和后台负载条件下的非 profiler A/B 数据。

#### 2026-08-01 均衡档精度图实现与验证检查点

- 全 FP16 候选通过 PyTorch/ONNX 比较，但在 `256x384` 的 ONNX/TensorRT 比较中
  得到 `max=0.05957`、`mean=0.00234`，超过数值合同，已显式淘汰。
- “图像输入 FP16、GridSample 坐标 FP32”候选符合 ONNX 16 类型规则，但
  TensorRT-RTX 要求 GridSample 两个输入类型一致，解析时显式失败，已淘汰。
  没有删除 vs-mlrt 对非 1.0 scale 的限制，也没有把 vs-rife/PyTorch 引入
  播放器运行时。
- 最终图只为每次 warp 的最终 `Add + Transpose + GridSample` 保持 FP32，共
  `15` 个精度保护节点：`Add 5 + Transpose 5 + GridSample 5`。FP32 卷积为
  `0`；静态类型审计得到 `72` 个 FP32 输出节点：`Cast 43 + Resize 14 +
  Add 5 + Transpose 5 + GridSample 5`。其余主计算使用 FP16，IO 合同仍为
  `FP16 [1,11,H,W] -> [1,3,H,W]`。
- 正式 ONNX SHA-256 为
  `212696b5befa040ab1989003dcc89b90903fbbdce21f46e0883cd5ab1bf91ca4`。
  PyTorch/ONNX 为 `max=0.002815`、`mean=0.0001885`；动态 FP32 ONNX/新 ONNX
  为 `max=0.006071`、`mean=0.0001869`；ONNX/TensorRT 为 `max=0.004394`、
  `mean=0.0002193`。三层数值比较均通过既定阈值。
- 3840x2160 原生 D3D11 P010 探针选择 `3840x2176` 固定 profile：TensorRT
  `26.342 ms`、端到端 `27.498 ms`、`36.36 qps`，相对旧图分别改善约
  `9.93%`、`9.53%` 和 `10.55%`。探针无推理失败、模型切换或回退。
- 新图在 universal profile 下的多 shape 结果如下。所有输入均显式运行成功，
  但这些数字是 profile 优化的待解决证据，不是多 shape 已优化的结论。

| 原始输入 | 实际 padded shape | TensorRT 平均 | 吞吐 |
| --- | --- | ---: | ---: |
| `1920x1080` | `1920x1152` | `16.509 ms` | `53.76 qps` |
| `2560x1440` | `2560x1536` | `28.777 ms` | `31.51 qps` |
| `3840x1600` | `3840x1664` | `46.606 ms` | `19.50 qps` |
| `4096x2160` | `4096x2176` | `65.418 ms` | `13.91 qps` |

- 质量档 ONNX hash 仍为
  `534aeae1a47bd7585defc6902fd6135b71545f85522626197eb109888ab7dfc2`，Lite
  档仍为 `9b9209ebce65b666c1f24ba9ee203bcacb1b3054b0bcc7617fbea7b6736a19b4`；
  本轮没有修改这两档。
- 刷新 hash 后启动 `build/jellium-desktop.exe` 时只识别到另外两档的既有
  Engine；选择均衡档后，UI 明确显示“正在加载均衡优先插帧”，并为新 hash
  构建独立 Engine。Engine key 为
  `919db79501b058b8cd5939c889c143cf0108537249eeeb3554740c3633694c32`，
  SHA-256 为
  `a94bac99a667033525f2734928640759ee849d4cbe242a44e5cdf8b827539525`。
- 真实播放使用《阿丽塔：战斗天使》3840x2160 HDR10、
  `24000/1001 -> 47.952 fps`、D3D11 P010 和固定 profile 1。冷启动 summary
  为 `5272` 对帧、`5233` 次推理、`39` 次切镜复制、`0` 次失败、`2` 次
  seek reset，runtime 推理平均 `27.819 ms`、p95 `29.425 ms`；没有模型切换
  或回退。
- 原生窗口屏幕采样覆盖机械臂遮挡、皮肤/机械细纹理及切镜，未发现空白输出、
  整帧破坏或切镜混合。该 250 ms 屏幕采样只作为播放烟测，逐中间帧结论以下方
  同源 P010 原生 runtime A/B 为准。

#### 2026-08-01 同一 HDR10 影片逐中间帧 A/B 检查点

- 输入为当前播放器实际解析的《阿丽塔：战斗天使》3840x2160 HDR10 片源，
  从 `187.270 s` 开始提取连续 `24` 个 `24000/1001` P010 源帧，原始字节数为
  `597196800`。解析使用播放器相同 User-Agent，经 `2` 次跳转到支持 Range 的
  CDN；不使用旧公园素材、桌面录屏或另一分辨率代替。
- 三档均使用当前正式缓存 Engine 和 profile 1，顺序执行原生 D3D11 P010
  sequence probe；每档得到 `23` 个中间帧，其中 `22` 次 RIFE 推理、`1` 次
  hard-cut F0 复制、`0` 次失败。质量、均衡、Lite 的 Engine key 分别为
  `2911120d5bc92304903687ac70879313baa05f4df1aaf1386e1fba86b92b499c`、
  `919db79501b058b8cd5939c889c143cf0108537249eeeb3554740c3633694c32`、
  `ead9de84b37fc092d730d4343fb810d66221a18ed45c974d5779ba07fc6a8a58`；Engine
  SHA-256 分别为 `f942c9b8835e23fdca1296b71302773e408dacefd3eeb46284b7fe1f1f363713`、
  `a94bac99a667033525f2734928640759ee849d4cbe242a44e5cdf8b827539525`、
  `dbf843d4e94f179766f16a145208d4880bba5bedbff43a916c81fa799c7645a3`。
- hard-cut 输出与源 F0 的首帧 SHA-256 在三档中均为
  `1abcd66183abc52dbcfb10ffe98d727b82bc20207f2a00bf85674308cc6bc225`，
  确认没有跨切镜混合。22 个推理帧的平均 temporal imbalance 为：质量
  `0.011926`、均衡 `0.010537`、Lite `0.031713`。
- 对全部 23 个中间帧做一致的 BT.2020/PQ 到 BT.709 tone-map 后逐帧检查。
  片段同时覆盖切镜、快速移动的白色袖臂和手部、前景遮挡，以及控制台按键和
  机械结构细纹理。均衡档未发现轮廓破碎、遮挡泄漏、整块错误纹理或切镜混合；
  相对质量档的整段 P010 比较为 `PSNR 47.137526 dB`、`SSIM 0.995136`，可见
  差异主要是轻微平滑。Lite 在相同运动区域出现持续的袖臂/手部轮廓破碎，说明
  该片段能够暴露真实插帧缺陷，不是弱门禁。
- 本次逐帧 A/B 与此前 PyTorch/ONNX、ONNX/TensorRT、原生 probe 和播放器
  长片段烟测共同关闭任务 1。测试期间播放器保持暂停，probe 开启 stage profiler；
  该次 profiler 耗时只用于执行确认，不替代前述正式非 profiler 性能基线。

### 2. 完成 TensorRT profile 优化

- 以本文和 `docs/frame-interpolation-dependencies.md` 为设计与历史基准资料。
- 保持一个模型一个 Engine；profile 是同一个 Engine 内的优化配置，不为
  每个分辨率生成独立 Engine。
- 每个模型保留 universal profile，以支持任意合法 padded shape；只为实测
  高频且确有收益的形状增加固定或窄范围 profile。
- 候选范围至少覆盖 1080p、1440p/2K、UHD 4K、DCI 4K 和常见超宽 4K，
  但 profile 集合必须按每个模型的 alignment 和实际性能分别决定。
- 对每个候选记录 Engine 构建时间、文件大小、显存、runtime cache、
  TensorRT 时间、端到端时间和 p50/p95/p99。没有稳定收益的 profile 不保留。
- profile 不匹配时使用 universal profile；不得切换模型、修改 `scale` 或
  隐藏失败。Engine/profile 合同变化必须同步 manifest schema、ABI 和缓存键。

#### 2026-08-01 三档三 profile 临时候选检查点

- 当前正式 Engine 仍保持两套 profile，本检查点没有修改播放器代码、manifest、
  ABI 或正式缓存。先使用 TensorRT-RTX 自带的 `--useProfile` 对同一 ONNX 的
  临时 Engine 做纯 TensorRT 对照，避免在候选未证实前重建三档正式 Engine。
- 临时候选仍是一个模型一个 Engine，只包含三套 profile：profile 0 保留当前
  `128x128 + 3840x2176 + 16384x16384` universal；profile 1 为
  `128x128 + 2560x1536 + 2560x1536` 中低分辨率范围；profile 2 为
  `128x128 + 3840x2176 + 4096x2176` 4K 范围。它不按分辨率切换模型，超出两个
  有界范围的合法 shape 仍由 universal 显式覆盖。
- 临时均衡档 Engine 使用正式
  `212696b5befa040ab1989003dcc89b90903fbbdce21f46e0883cd5ab1bf91ca4`
  ONNX，在 RTX 5070 Ti / TensorRT-RTX 1.4.0.76 上构建耗时 `6.997 s`，大小
  `37574124` 字节；当前正式两 profile Engine 为 `42980052` 字节。本结果只说明
  本机 Builder 成本，没有把构建时间或大小写成跨设备常量。
- A/B 固定为同一 GPU 和后台状态、无 H2D/D2H、runtime allocation、eager dynamic
  shape specialization、每个进程预热 `5000 ms`、测量 `50` 次。baseline 对非
  UHD shape 使用当前 universal，对 UHD 使用当前 fixed；candidate 使用相应的
  中低分辨率或 4K 范围 profile。未加载现有 runtime cache。

| padded shape | 当前 profile | 当前均值 | 候选 profile | 候选均值 | 改善 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `1920x1152` | 0 | `17.1750 ms` | 1 | `8.9090 ms` | `48.128%` |
| `2560x1536` | 0 | `30.1737 ms` | 1 | `13.8066 ms` | `54.243%` |
| `3840x1664` | 0 | `48.7440 ms` | 2 | `25.1553 ms` | `48.393%` |
| `3840x2176` | 1 | `31.6467 ms` | 2 | `31.6523 ms` | `-0.018%` |
| `4096x2176` | 0 | `68.8217 ms` | 2 | `35.7948 ms` | `47.989%` |

- 这组结果确认当前超大 universal 是均衡档 1440p、超宽 4K 和 DCI 4K 的主要
  profile 性能问题；为所有分辨率增加独立 fixed profile 没有必要。候选在 UHD
  保持现有性能，同时让一个有界 4K profile 覆盖 UHD、超宽 4K 和 DCI 4K。
- 质量档候选使用正式
  `534aeae1a47bd7585defc6902fd6135b71545f85522626197eb109888ab7dfc2`
  ONNX。profile 0 为 alignment `64` 的 universal；profile 1 为
  `64x64 + 2560x1472 + 2560x1472` 中低分辨率范围；profile 2 为
  `64x64 + 3840x2176 + 4096x2176` 4K 范围。临时 Engine 构建耗时
  `5.896 s`、大小 `34925692` 字节、SHA-256 为
  `9b4bc92100fe998c6066491f4152acab5cd712ef925d371e8d42128aa75a0c31`。

| 质量档 padded shape | 当前 profile | 当前均值 | 候选 profile | 候选均值 | 改善 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `1920x1088` | 0 | `18.6833 ms` | 1 | `9.2799 ms` | `50.331%` |
| `2560x1472` | 0 | `34.5677 ms` | 1 | `15.9550 ms` | `53.844%` |
| `3840x1600` | 0 | `56.7175 ms` | 2 | `29.6202 ms` | `47.776%` |
| `3840x2176` | 1 | `37.0605 ms` | 2 | `37.8356 ms` | `-2.091%` |
| `4096x2176` | 0 | `83.7807 ms` | 2 | `44.3481 ms` | `47.066%` |

- Lite 候选使用正式
  `9b9209ebce65b666c1f24ba9ee203bcacb1b3054b0bcc7617fbea7b6736a19b4`
  ONNX。profile 0 为 alignment `128` 的 universal；profile 1 为
  `128x128 + 2560x1536 + 2560x1536` 中低分辨率范围；profile 2 为
  `128x128 + 3840x2176 + 4096x2176` 4K 范围。临时 Engine 构建耗时
  `5.060 s`、大小 `30771292` 字节、SHA-256 为
  `332292db800c72e6f9bdc28629554a6262bd915e476906ca25e634eafcc2d4db`。

| Lite padded shape | 当前 profile | 当前均值 | 候选 profile | 候选均值 | 改善 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `1920x1152` | 0 | `13.6771 ms` | 1 | `7.1275 ms` | `47.887%` |
| `2560x1536` | 0 | `26.3967 ms` | 1 | `14.2348 ms` | `46.074%` |
| `3840x1664` | 0 | `42.7912 ms` | 2 | `23.4620 ms` | `45.171%` |
| `3840x2176` | 1 | `31.1893 ms` | 2 | `31.9233 ms` | `-2.353%` |
| `4096x2176` | 0 | `61.6007 ms` | 2 | `34.0347 ms` | `44.749%` |

- 三档均证明中低范围和有界 4K 范围值得保留，但质量档与 Lite 的 UHD 范围
  profile 相比既有 UHD fixed 分别回归 `2.091%`、`2.353%`，不能直接采用
  三 profile 候选。下一步只为存在回归的模型构建第四套
  `min=opt=max=3840x2176` fixed profile，确认能否同时保留 UHD 基线和其他
  shape 收益；每个模型的最终 profile 数量由各自数据决定，不强制一致。
- 本检查点尚不是产品验收：冻结最终集合并实现通用 profile 合同后，仍必须运行
  原生 P010 probe、任意合法 shape universal 覆盖、Engine 构建/缓存、模型切换
  和真实播放验证。若任一层不成立，正式集合不得切换到该候选。

#### 2026-08-01 UHD fixed 四 profile 候选检查点

- 只为三 profile 候选在 UHD 有回归的质量档和 Lite 增加 profile 3：
  `min=opt=max=3840x2176`。均衡档不增加该 profile，因为其 profile 2 在 UHD
  仅相差 `-0.018%`。三档仍各自只有一个 Engine，profile 数量无需相同。
- 质量档四 profile 临时 Engine 构建耗时 `6.578 s`、大小 `73702764` 字节，
  SHA-256 为
  `b7f793e460db6af7a08fd99593c52bf88de53b98917b0af0fef1ec0e50812005`。
  相同无传输、runtime allocation、eager specialization、`5000 ms` 预热和
  `50` 次测量条件下，正式 fixed 与候选 fixed 的配对均值分别为
  `37.4265 ms`、`37.2966 ms`，候选快 `0.347%`，属于持平区间。
- Lite 四 profile 临时 Engine 构建耗时 `5.645 s`、大小 `68838988` 字节，
  SHA-256 为
  `2bed63abaeb505d009c7877100719edca1b0b439d2b21f3e93c4b89baa138836`。
  首轮正式 fixed 出现短暂的 `29.4278 ms`，但交替复测后正式 fixed 与候选 fixed
  分别稳定为 `31.1110 ms`、`31.1117 ms`，差异 `0.002%`，不能把首轮 GPU
  时钟/负载漂移写成 profile 收益或回归。
- 最终候选集合冻结为：均衡档 universal + 中低范围 + 4K 范围；质量档和 Lite
  在相同三套 profile 后追加 UHD fixed。与当前正式三档 Engine 总大小
  `156990956` 字节相比，候选总大小为 `180115876` 字节，增加 `23124920` 字节
  （`14.73%`）。该成本用于同时保留 UHD 基线和其他 shape 的大幅收益，不会产生
  按分辨率拆分的大量 Engine。
- 本检查点只冻结候选结构，不代表产品已切换。实现时 manifest 必须描述每个
  profile 的完整 min/opt/max 合同；runtime 必须按实际 padded shape 选择最窄的
  匹配 profile，并显式保留 universal 覆盖，不得切换模型、修改 `scale` 或隐藏
  Engine/profile 失败。

#### 2026-08-01 通用 profile 合同实现检查点

- manifest 从单个 `profile` 升级为有序 `profiles[]`，schema 升至 `4`；原生
  runtime ABI 升至 `7`，Engine metadata schema 升至 `3`。Rust 校验三档冻结
  集合，并把每套 profile 的 index、purpose 和完整 min/opt/max 写入 Engine
  缓存键、TensorRT Builder 参数和 metadata，不保留旧合同的并行解析路径。
- 原生 runtime 要求 Engine 恰好包含 `3` 或 `4` 套有序 profile，逐套校验 shape
  合同，并在所有能覆盖实际 padded shape 的专用 profile 中选择范围最窄者；无
  专用匹配时只选择 profile 0 universal，不切换模型或 `scale`。旧两-profile
  Engine 被显式拒绝，错误为 `received 2`，没有隐藏兜底。
- staging 真实生成结果为：质量档 `4` 套
  `universal,mid-range,4k-range,uhd-fixed`；均衡档 `3` 套
  `universal,mid-range,4k-range`；Lite `4` 套，与冻结候选一致。旧单一
  `profile` 字段不存在。
- ABI 7 原生 DLL 和 probe 已在 MSVC `/W4 /WX` 下编译通过；Rust 定向单测
  `16/16` 通过。候选 Engine 的原生 D3D11 P010 probe 共验证 `11` 条路径：质量
  `1/2/3/0`、均衡 `1/2/0`、Lite `1/2/3/0`，实际 profile 均符合预期，全部输出
  `p010-packed=yes` 且缓存重开命中。
- `build_mpv_source.ps1` 增加 mpv 源码合同哈希戳。哈希匹配的早退路径仍会重建
  runtime DLL 并刷新 schema 4 三模型 manifest；ABI 头或 filter 源变化时则自动
  完整重建，避免 ABI 7 runtime 与 ABI 6 mpv filter 被拼装到同一输出目录。
- 本检查点尚未完成正式缓存迁移、Release 播放器构建和真实影片验证，因此任务 2
  仍为进行中。后续失败必须停在真实错误，不得恢复旧 Engine、Lite 或 universal
  之外的隐藏路径。

#### 2026-08-01 正式播放器验收与任务 2 关闭

- `build_mpv_source.ps1` 已按 ABI 7 合同完整重建自定义 mpv，正式 Release
  `build/jellium-desktop.exe` 基于 `fdda0a4` 启动成功。启动时旧 ABI 6 缓存未被
  误认，设置页明确显示三个模型、TensorRT-RTX 1.4.0.76 和 `0 个 Engine`。
- 真实影片使用《阿丽塔：战斗天使》3840x2160、23.976 fps、HDR10、10-bit、
  75.5 Mbps，解码和输出链路为 D3D11VA + gpu-next P010。每次新播放均以插帧
  关闭开始；即使模型偏好已写入 Rust `settings.json`，也不会自动开启插帧。
- 通过真实 UI 依次选择质量、均衡和 Lite 时，播放器分别显示“正在加载质量优先
  插帧”“正在加载均衡优先插帧”和“正在加载流畅优先插帧”，首次构建与播放重载
  总耗时约为 `24.3 s`、`31.4 s` 和 `19.7 s`。没有静默回退、模型自动切换或
  状态伪成功。
- 最终缓存恰好包含三个 Engine、三份 schema 3 metadata 和三份
  `3840x2176.runtime-cache`，没有 `.building` 残留：

| 档位 | Engine key | Engine 字节数 | profiles | 真实 UHD profile |
| --- | --- | ---: | ---: | ---: |
| 质量优先 | `d445a7b3cfdebb33ea054fda9ae22a50dfc6778be0aa6bf175e1b966b1525e95` | `73704708` | 4 | 3 |
| 均衡优先 | `fbd72157394d108ed7cf13d6f90b62c79adc41e6b9d1b03667a694e1403edd4d` | `37574124` | 3 | 2 |
| 流畅优先 | `54f191f14d4cf42fa385c124e1e925b1cd3fadaff59f736ebfc7a428302c0f61` | `68841444` | 4 | 3 |

- 播放信息逐档确认实际模型分别为 `rife-v4.26`、
  `rife-v4.26-scale0.5` 和 `rife-v4.25-lite`，精度/scale 分别为
  `fp16/1.0`、`fp16/0.5` 和 `fp16/1.0`，后端始终为 TensorRT-RTX D3D11
  P010，输出始终为 47.952 fps，解码丢帧为 0。
- 三档 Engine 已存在后，再次选择质量档只需约 `2.5 s` 完成播放重载，Engine
  数量保持 3。关闭后立即重开相同质量档时，原生日志明确为
  `profile=3 cache=hit init-ms=0.003 reuses=1`，确认进程内 runtime 复用生效。
- 任务 2 至此关闭。三档继续支持任意合法分辨率；超出专用 profile 范围时显式
  使用各自 Engine 的 universal profile，不切模型、不改 scale、不生成按分辨率
  拆分的大量 Engine。

### 3. 实现窗口播放的刷新率自匹配

#### 2026-08-01 第一阶段审计证据

- 正式播放器没有设置 `d3d11-output-mode=composition`。mpv 保持默认 `auto`，
  Windows 宿主也从 `window-id` 取得并使用 mpv HWND，因此视频链路实际通过
  `CreateSwapChainForHwnd` 创建 flip-model swapchain；
  `src/windows/src/compositor.rs` 的 `CreateSwapChainForComposition` 只用于 CEF
  透明界面，不是 mpv 视频 swapchain。
- 本机 `IDXGIFactory5::CheckFeatureSupport(DXGI_FEATURE_PRESENT_ALLOW_TEARING)`
  返回 `S_OK` 且结果为 `TRUE`。这只证明操作系统、驱动和 DXGI factory 支持
  标准窗口化 tearing 请求，不证明当前应用已请求或实际进入 VRR。
- 当前活动输出为 `G28XR`，模式是 `3840x2160 @ 152 Hz`。EDID range-limits
  descriptor 声明的垂直扫描范围为 `48-152 Hz`；它是显示器能力线索，不单独
  证明 Windows/NVIDIA 已激活 VRR。`47.952 fps` 略低于该下限，真实呈现若要
  保持该节奏需要驱动 LFC 或其他经验证机制，不能假定自动成立。
- 当前 mpv swapchain 描述未包含 `DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING`，呈现调用
  仍为 `Present(1, 0)`。当前状态必须准确记录为
  `supported=true / requested=false / active=unknown`。
- mpv HWND 上方还存在透明 DirectComposition CEF visual。该覆盖是否阻止
  Independent Flip 或 VRR 必须通过 PresentMon/ETW 实测，不能只根据 flip-model
  或 G-SYNC Compatible 标志推断。
- PresentMon `2.5.1` 便携版已用于尝试基线采集，SHA-256 为
  `9bec3083069f58f911e6a512f4806db51a27bd096103087bc1d05ef54c80a191`；
  当前非管理员会话启动 ETW trace 被系统以 `access denied` 拒绝。后续先在 mpv
  swapchain 内采集 `IDXGISwapChainMedia` 帧统计、composition mode 和 Present
  间隔；若仍不足以证明 active，再执行有权限的 PresentMon 验收，缺失证据期间
  不得标记 VRR active。

#### 2026-08-01 第二阶段原型证据

- mpv D3D11 原型已请求 `DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING`，并在 NVAPI 确认
  当前 primary surface 具备 G-SYNC 能力后使用
  `Present(0, DXGI_PRESENT_ALLOW_TEARING)`。`ResizeBuffers` 和格式切换继续保留
  原 swapchain flags。
- NVAPI capability/active 查询在 `Present` 前取得当前 backbuffer 的
  `NVDX_ObjectHandle`，`Present` 完成后查询同一个 surface。active 为 false 时
  继续保持 requested 状态并按秒复查，不再只请求一个 tearing frame 后永久
  退回固定刷新呈现。
- 早期合成源测试虽然得到 `capable=yes / active=no`，但测试 HWND 实际不是系统
  前台窗口；NVIDIA 官方接口明确 active 结果只在应用处于前台时有效。通过 Win32
  前台 PID 校验后，无边框全屏 60 fps 和 `48000/1001` fps 均稳定得到
  `requested=yes / capable=yes / active=yes`。临时使用
  `NvAPI_D3D_GetSleepStatus` 交叉检查时也得到 `fs-vrr=yes`；该临时查询不进入
  正式控制状态机。
- 使用 D3D11 device 和 immediate context 查询均能工作；正式原型保留更简单的
  device 调用。进程退出正常，不调用会导致当前驱动退出崩溃的 `NvAPI_Unload`。
- 前台普通窗口、最大化窗口和最大化无边框窗口在 60 fps 与 `48000/1001` fps
  下仍为 `capable=yes / active=no`。因此当前只能确认无边框全屏 VRR 激活，不能
  宣称窗口刷新率同步已经完成。
- `48000/1001` fps 略低于 EDID 的 48 Hz 下限。NVAPI 的 `active=yes` 证明驱动
  已启用 G-SYNC Compatible 路径，但尚不能单独证明物理面板使用了哪一种 LFC
  倍频节奏；最终 LFC 结论仍需有权限的 ETW/PresentMon 或等价物理刷新证据。

#### 2026-08-01 实验性正式播放器集成（已撤回）

- 实验分支曾将 `d3d11-vrr=yes` 设为 Windows 必需选项；若正式 mpv 缺少
  该选项，初始化会显式失败，不会静默退回旧呈现路径。
- CEF 协议曾读取 mpv 的 state、requested、supported、capable、active、
  sync interval、提交次数与平均提交间隔，并以同一个 VRR JSON 对象提供给设置页
  与播放信息面板。提交测量只有在至少两次 Present 且间隔为有效正数时才成立。
- 设置页与播放信息面板曾接入只读 VRR 诊断；`inactive` 明确显示为当前窗口路径
  未激活，不能等同于 supported、requested 或 capable。
- 该实验代码曾通过 `cargo check -p jfn-cef` 和前端语法检查；正式 mpv 与 Release
  重建后，正式窗口播放仍为 `composed / capable=yes / active=no`。
- 最终确认正式 RIFE 必需的 NVIDIA `d3d11va` 直通与 VRR 不兼容后，mpv patch、
  Rust 启动选项、CEF 协议、设置页入口和失败探针全部撤回，不作为产品代码提交。

#### 2026-08-01 正式 CEF/DComp 全屏实机证据

- 正式 `build/jellium-desktop.exe` 播放《黑衣人2》3840x2160 HDR10，质量档实际
  保持 RIFE v4.26 `scale=1.0`、D3D11 P010、47.952 fps、FP16 profile 3；推理
  初始化与持续出帧正常，没有切模型或回退。
- 全屏 HWND 实际为 `0,0,3840,2160`、style `0x140A0000`，不是最大化伪全屏。
  CEF DComp target 和 DWM transparency 释放后，DXGI 只从 `composed` 变为
  `overlay`，NVAPI 仍为 `capable=yes / active=no`。一像素尺寸往返触发
  ResizeBuffers 后结果不变。
- 一次性完整 VO 重建实验确实执行了 `uninit_video_out -> reinit_video_chain`，
  新 swapchain 仍为 `overlay / active=no`。同时 RIFE 重建显式失败：
  `cudaD3D11SetDirect3DDevice rejected: this process is already bound to a different D3D11 device`。
  该实验不能解决 VRR 且会破坏插帧，相关命令和 CEF 门控已删除，不进入正式实现。
- CEF DComp target 动态拆装同样只改变了 composition mode，没有激活 VRR；最终
  范围审计已删除 `setOsdVisible` 接线和 DComp/DWM 动态拆装，正式合成生命周期
  保持原实现。该失败实验仅保留本节证据，不作为生产功能提交。
- PresentMon 仍因当前会话缺少管理员 ETW 权限而不可用。本轮证据只证明 NVIDIA
  API 报告的 capable/active 和 DXGI composition mode，不证明物理面板刷新节奏
  或 LFC；后续不得把该缺失证据补写成已通过。

#### 2026-08-01 50 fps 阈值与无 RIFE 对照

- 《低智商犯罪》25 fps 片源选择 Lite 后，正式播放器输出稳定约
  `50.058 fps`；窗口处于真实全屏，控件隐藏且 CEF DComp target 已释放，NVAPI
  仍为 `capable=yes / active=no`。该结果已高于显示器 EDID 声明的约 48 Hz 下限。
- 同一片源关闭 RIFE、临时以 2 倍速度播放时，实测输出约 `49.966 fps`；全屏
  overlay 呈现仍为 `active=no`。因此不能把正式播放器未激活归因于 RIFE GPU
  负载、质量档性能余量或插帧滤镜本身。
- 2026-08-01 最小 libmpv 探针在同一显示器约 `47.952 fps` 曾得到 `active=yes`。正式
  窗口与探针 HWND 的 style、ex-style、父窗口、owner 和全屏尺寸一致；隐藏输入
  子窗口、关闭 CEF GPU 合成、先隐藏 OSD 再全屏、释放 DComp target 和移除
  `WS_CLIPSIBLINGS` 均未使正式播放器激活。
- 2026-08-02 在桌面保持 `152 Hz` 的复测中，同一最小探针的 60 fps 软件源进入
  `active=yes`，`47.952 fps` 软件源则为 `active=no`。这说明低帧率下限或驱动状态
  会随显示器和运行环境变化，不能从单次探针外推跨设备结论。
- CEF 覆盖层、输入子窗口和 HWND 样式均已排除为单一根因；但在确认 NVIDIA
  `d3d11va` 直通阻塞后，不再继续隔离进程初始化或 mpv boot 参数。禁止恢复会破坏
  CUDA-D3D11 绑定的播放中 VO 重建。

#### 2026-08-01 D3D11VA copy 与 RIFE 上传实验

- mpv 上游问题 [`#13304`](https://github.com/mpv-player/mpv/issues/13304) 与本机
  现象一致：NVIDIA 下 `hwdec=d3d11va` 直通会阻断
  VRR；本轮不再重查 CEF、DComp、窗口样式、帧率下限、RIFE 负载或 VO 重建。
- 自定义 RIFE filter 已隔离接入 mpv 标准 `mp_autoconvert`，目标严格限定为
  `IMGFMT_D3D11 + IMGFMT_P010`。同一 3840x2160、10-bit、24000/1001 短片使用
  `d3d11va-copy` 时，日志确认 CPU `p010` 经 `HW-uploading to d3d11` 转为
  `d3d11[p010]`；质量档保持 RIFE v4.26 `scale=1.0` 并命中 profile 3。
- 功能链路通过：`191/191` 对帧完成推理、`0` 次失败，平均推理 `33.524 ms`、
  p95 `35.050 ms`，mpv 正常播放到 EOF。没有切换模型、改写 `scale`、禁用滤镜、
  NVOF 或透传回退。
- 实时性能未通过：同样本零拷贝质量档累计 `Dropped: 10`，copy 加上传累计
  `Dropped: 265`；无 RIFE 的 copy 对照无丢帧。无节奏完整吞吐 A/B 中，两条链路
  均完成 `191/191`，copy 的 TensorRT 平均仅比直通多约 `0.6 ms`，因此额外压力
  来自 D3D11VA 解码纹理的 GPU 到 CPU 回读及随后 CPU 到 GPU 上传，而不是模型、
  Engine 或 profile 退化。
- 本轮 copy 探针仍为 `capable=yes / active=no`。NVAPI 状态已知会随 NVIDIA/Windows
  运行环境漂移，单次结果不能证明跨设备 VRR；本次更不能宣称刷新率自匹配已完成。
- 当前决定：`d3d11va-copy` 只保留为已验证的失败证据，正式源码已移除为该实验
  增加的 `mp_autoconvert` 自动上传接线。正式路径继续严格使用 `d3d11va` 的
  D3D11 P010 输入；不把软件解码加上传隐藏成可用路径，也不以 Lite、关闭插帧或
  其他静默回退掩盖失败。不得再用固定显示模式切换冒充 VRR。
- 同一样本继续否决了当前 mpv 内三个硬件候选。`nvdec` 和 `d3d12va` 均先报告
  `Could not create device`、`DR failed - disabling`，实际退化为软件 P010 后再上传；
  其最终 RIFE 成功不代表硬件链路成立。`dxva2` 能保持 `dxva2_vld[p010]` 硬解码，
  但 `dxva2_vld -> d3d11[p010]` 设备间上传失败，mpv 随后自动禁用 RIFE。
- mpv 现有 DXVA2 到 D3D11 互操作只供 VO 渲染，输出为 `BGR0`，不满足 RIFE 的
  P010 输入合同；标准硬件映射表也没有 CUDA/D3D12 到 D3D11 P010 的零拷贝映射。
  因此不继续手写未经成熟项目验证的跨 API 同步和资源共享层。解码替代候选和
  当前 VRR 交付路线同时关闭，不再继续调整宿主或解码参数。

#### 2026-08-02 固定显示模式方案淘汰

- `SetDisplayConfig` 将桌面从 `152 Hz` 切到 `100 Hz` 时会触发显示链路重新同步和黑屏；
  这属于固定显示模式切换，不是 VRR 的无缝扫描节奏调整。
- 该方案虽然曾通过切换、恢复和生命周期测试，但不再计入任务 3 的验收，相关
  Windows 实现、Platform ABI、CEF 协调、设置页字段和测试均从正式代码撤回。
- 实验性 DXGI/NVIDIA VRR 请求与 `requested/supported/capable/active` 诊断也已撤回，
  不在设置页暴露无法交付的功能。禁止用固定模式切换或其他旁路改写为成功。
- 若未来重启，实机验收仍要求桌面显示模式保持不变、没有黑屏，并由正式 Release
  的 `d3d11va` 零拷贝路径报告 `active=yes`。

#### 2026-08-02 VRR 撤回后的最终产物重建

- 撤回实验代码后重新执行 `dev/windows/build_mpv_source.ps1`，自定义 mpv 构建成功；
  `third_party/mpv-install/lib/libmpv-2.dll` 的 SHA-256 为
  `c6b78c30fede6262338045d898ad476b0f8c0b5a13c840894d7f6a795b9f490f`，不再包含
  `d3d11-vrr`、`display-vrr-state` 或 `VRR state=` 字符串。
- 随后执行 `dev/windows/build.ps1 -Clean`，完整 Release 重建成功。最终
  `build/libmpv-2.dll` 与 staged DLL 的 SHA-256 完全相同；manifest 为 schema 4、
  runtime ABI 7、TensorRT-RTX 1.4.0.76，三个 ONNX 的实际 hash 均与 manifest
  一致，三档 profile 数仍为 `4/3/4`。
- 最终 `build/jellium-desktop.exe` 启动烟测日志标识提交 `2111dac`，并报告
  `RIFE v4.26`、`RIFE v4.26 (scale=0.5)`、`RIFE v4.25 Lite`、`engines=3`、
  `TensorRT-RTX D3D11 P010`，主循环和本地 MediaStation 页面均成功就绪。
- 烟测期间 NVIDIA 输出始终为 `152 Hz`，Release 日志没有 VRR、固定显示模式切换
  或撤回实验字段。该项只证明没有再次硬切显示模式，不替代真实 VRR 或黑屏视觉验收。
- 自动化 `CloseMainWindow()` 不会关闭该自绘/CEF 主窗口；测试进程已按 PID 清理并
  确认无残留。因此本轮不伪报自动化正常退出，启动与运行组件烟测结论不受影响。

#### 2026-08-02 质量档偶发花屏闪烁诊断

- 《黑衣人2》3840x2160、23.976 fps、HEVC Main 10 在质量优先档命中 RIFE v4.26
  profile 3。用户明确报告片内约 `13:50` 闪烁；日志在片内 `828.95` 秒同一时刻
  连续三次报告 libplacebo `Peak detection usage error`，并明确提示输出可能错误。
- 随后的多次主观闪烁期间，日志又在 `02:50:57`、`02:51:26`、`02:52:01` 和
  `02:52:05` 逐组报告相同错误。对应时段没有 RIFE 推理失败、Engine/profile
  切换或 D3D11 P010 输入错误，不能把问题归因于质量模型吞吐或用降档掩盖。
- 原 Release 打包的是 libplacebo `7.360.1`。首次固定到
  `82224764a98164ce9d2d9a10e4fefca934e475fb`、关闭 delayed peak detection 并重建
  libplacebo `7.362.0`、mpv 和 Release 后，同一影片仍在持续播放一段时间后成组报告
  `Peak detection usage error`，因此 mpv `#17685` 不能作为本轮 HEVC HDR10 场景的
  已确认根因，`8222476` 也不是完整修复。
- libplacebo 后继提交 `c43baa7a0cff623ef416c60d5b926342903279e6` 确实修复了
  `8222476` 引入的 raster pass UAV 重复偏移；但固定并打包该提交、完整重建
  libplacebo/mpv/Release 后，真实播放《黑衣人2》质量档仍在 `15:13:49`、`15:13:54`、
  `15:14:07`、`15:14:35`、`15:14:42`、`15:15:07`、`15:19:10`、`15:20:47` 和
  `15:23:46` 成组报告相同 peak detection usage error。实际合同为 3840x2160
  HEVC Main 10 HDR10、23.976 fps、RIFE v4.26 scale=1.0、profile=3、D3D11 P010、
  TensorRT-RTX FP16；同期没有 TensorRT 推理失败、D3D11 device lost 或 gpu-next
  render failure。因此 `c43baa7` 也不是完整修复。
- `c43baa7` 同时包含 libplacebo `27aa71a97f4daed84916936572fa6a2e1c3eedb7`
  的第一版线性光帧混合。mpv 上游问题
  [`#17847`](https://github.com/mpv-player/mpv/issues/17847) 明确记录该版本会在播放中
  随机亮闪，问题 [`#17899`](https://github.com/mpv-player/mpv/issues/17899) 也将闪烁
  回归指向 `27aa71a9`。上游先回退该实现，再由
  `1733c8601edec161b714e4a799c72a9f5e5aa2f0` 以不缓存线性光帧的第二版实现修复，
  该提交明确关闭 `#17847/#17899`。
- 固定 libplacebo `1733c860`、版本/ABI `7.364.0` 并完整重建后，旧修复版在
  `15:36:55`、`15:36:58`、`15:38:24` 和 `15:38:32` 各出现 3 条 peak usage
  error，共 12 条；因此它只解决上游 `27aa71a9` 的随机亮闪回归，不能单独关闭
  本轮问题。
- 继续叠加上游 `2d0979fb54e025e904c7372666fffbf5dae40f66` 后，已确认补丁真实
  应用于源码并重建 libplacebo、mpv 和 Release；同片质量档从 `16:17:38`
  开始实播，在 `16:18:02` 和 `16:18:10` 各出现 3 条相同错误。该提交修复的是
  线性缩放时 peak detection 读取错误颜色状态，不是当前未执行 peak 缓冲区的完整修复。
- 把非延迟 peak detection 立即落到独立 FBO 的候选已完整重建，但质量档从
  `16:39:18` 开始播放后仍在 `16:40:37` 连续出现 3 条相同错误，因此该候选已证伪并
  撤回。检测 pass 确实单独提交，错误仍表示其 SSBO 计数偶发为 0，不能继续把问题
  简化为 libplacebo 缺少 FBO 边界。
- 当前根因候选转向共享 D3D11 immediate context 的管线状态竞争。libplacebo 的一次
  compute pass 由多条 `CSSet*`、`Dispatch` 和解绑调用组成；`ID3D10Multithread` 的
  内建保护只逐 API 串行化，而 RIFE 原有的显式 `Enter/Leave` 只能保证 RIFE 自己的
  命令块不被打断，仍可能完整插入 libplacebo 的绑定与 `Dispatch` 之间并改写 compute
  状态。Windows 插帧滤镜现已实现独立 `ID3DDeviceContextState`，在每个 RIFE D3D11
  命令块前后原子交换并恢复调用方状态；不支持 D3D11.1 状态隔离时显式初始化失败。
- 首轮状态隔离 Release 在 `17:01:20` 确认启用
  `D3D11 context state isolation`，命中 RIFE v4.26 `scale=1.0`、profile 3；从片内
  `3960` 秒连续播放约 5 分 30 秒，`Peak detection usage error`、RIFE/TensorRT
  failure、D3D11 device lost、gpu-next/render failure 均为 0。源码审计随后发现动态
  `caller_context_state` 断言必须位于 `ID3D10Multithread_Enter` 之后，避免并发调用者
  在等待锁前误触断言；修正后重新完整构建 mpv 并打包 Release。
- 最终滤镜源码 SHA-256 为
  `AB113262220F60E7A34A995498C62F469007BA123D7491064F65A22F17DBD02F`，编译副本一致；
  `libmpv-2.dll` 为
  `183CCF83B0A39DDC59256DEB734322B00BDEE83AD3D0104579FD2DCC824F5A92`，打包副本一致；
  `libplacebo-364.dll` 仍为
  `E5BA3D8A9F35ECA86CD2B283051ECBC96BA914197DF5DECC41C7B9BDAD53CD01`。
  规范化补丁空白前的等价 Release 在 `17:10:50` 再次确认状态隔离、质量模型和
  profile 3，`17:11:00` 跳到片内 `3960` 秒后连续覆盖至 `17:14:38`，上述六类目标
  错误仍全部为 0。补丁只移除无语义尾随空格，但源码合同按设计触发 libplacebo、
  mpv 和 Release 干净重建；最终精确产物在 `17:26:04` 再次确认 3840x2160 P010、
  状态隔离、RIFE v4.26 `scale=1.0` 和 profile 3，从片内 `3960` 秒继续播放至
  `17:28:07`，目标错误仍全部为 0。
- 该候选已通过此前稳定复现强度下的构建和日志门禁，但尚未完成用户视觉验收，不能
  宣称偶发花屏已彻底关闭。仍不采用关闭 peak detection、禁用 film grain、切换
  Lite、关闭插帧或启用 delayed 掩盖问题；下一步由用户在同片质量档确认画面，若仍
  闪烁则继续按真实时间点取证。

#### 2026-08-02 插帧播放卡顿与全屏重复驱动诊断

- 调试日志实际记录了两次全屏切换。每次都先执行按钮触发的
  `cycle fullscreen`，然后 mpv `fullscreen` 属性回调又写入同一个
  `fullscreen=true/false`。这证明播放事件层在状态消化前调用平台同步，
  Windows 因而将 mpv 已经完成的属性变化再次写回。
- 同一复现中，质量档为 3840x2160 P010、RIFE v4.26 `scale=1.0`、
  profile 3；无 TensorRT failure、D3D11 device lost 或 gpu-next render failure。第二段
  `413` 对帧的推理均值为 `33.622 ms`、P95 `44.350 ms`、最大 `57.739 ms`，
  其中尾延迟已超过 23.976 fps 每对帧约 `41.708 ms` 的实时预算。这是独立的
  持续卡顿候选，不得用全屏重复驱动一项直接解释全部现象。
- 本轮修复只调整事件顺序：先把 mpv 属性落到 `IngestState`，再调用平台同步。
  Windows 将在已提交状态上判定值未变，不再回写 mpv；未修改模型、Engine、
  profile、scale、解码或刷新率。`jfn-playback` 54 项测试、定向严格 Clippy、增量
  Release 构建均已通过；新构建实播确认每次真实动作只出现一次
  `Set property: fullscreen -> 1`，不再出现旧版后续属性回写。该项已关闭。
- 独立卡顿在同一新构建上完整复现：3840x2160 P010、RIFE v4.26 `scale=1.0`、
  profile 3 共处理 `6099` 对帧，推理 `6062` 次、failure `0`；平均
  `35.249 ms`、P95 `44.925 ms`、最大 `235.334 ms`。现场 GPU 利用率约
  `83-97%`、核心约 `2.79-2.805 GHz`、`66-72 C`，没有热降频；日志仍没有
  TensorRT failure、D3D11 device lost 或 gpu-next render failure。
- 源码审计确认 `write_rife_frame()` 在整个 `rife.process()` 期间持有共享
  `ID3D10Multithread` 锁；约 `35-45 ms` 的 TensorRT/CUDA 计算因此会阻塞同一
  immediate context 上的 libplacebo 呈现线程。修复严格限于同步边界：runtime 为
  场景检测、输入转换至 `cudaGraphicsMapResources`、输出转换分别获取锁并切换独立
  `ID3DDeviceContextState`；TensorRT/CUDA 计算和 unmap 不持 D3D11 锁，滤镜删除
  包住整个 `rife.process()` 的外层锁。runtime ABI 升为 8，旧 DLL 将显式不兼容。
- ABI 8 runtime 与 probe 已通过 MSVC `/W4 /WX` 编译。真实质量档 4K Engine 的
  8 次探针测量为平均 `35.72 ms`、P95 `36.05 ms`，profile 3、P010 输出有效、
  caller CS context state 每次均恢复；未修改模型、Engine、profile、scale、解码或
  回退规则。当前状态为“runtime/probe 已验证，待完整重建 mpv、Release 和同片实播
  长测”，不得提前宣称视觉卡顿已关闭。

#### 2026-08-02 19:28 卡死现场与 D3D11 查询锁修复

- 质量档再次播放到片内 `3491.008` 秒后日志停在 `19:28:07`，没有 runtime
  summary、TensorRT failure、D3D11 device lost 或 gpu-next failure。主进程 PID `8676`
  仍被 Windows 标记为可响应，但播放管线已停止前进。
- 在 `19:32:23` 和 `19:42:52` 保存了两份轻量原生转储，两次栈一致。mpv
  `core` 线程 TID `0x53c0` 停在
  `d3d11!CContext::TID3D11DeviceContext_GetData_<2> -> rife_runtime+0xf98b ->`
  `rife_runtime+0xdb23 -> write_rife_frame`，对应源码中的
  `ID3D11DeviceContext::GetData` 轮询。该线程 5 秒内消耗 `4812.5 ms` CPU，
  确认为持续自旋，不是一次短暂尾延迟。
- 同时 mpv `vo` 线程 TID `0x3704` 停在
  `RtlEnterCriticalSection -> d3d11!CDevice::CondObjectLock -> DiscardView ->`
  `libplacebo -> draw_frame`。根因是 GPU 查询轮询位于 `D3D11ContextBlock` 内：
  `core` 线程在 `GetData` 不返回时始终持有共享 `ID3D10Multithread` 锁，
  呈现线程因此永久无法进入 D3D11。这次卡死与模型、Engine、profile 或
  算力不足无关。
- 修复后，显式 D3D11 锁只覆盖 compute 状态设置、命令提交和 caller context
  state 恢复。场景读回 `Map`、输入转换查询、`cudaGraphicsMapResources` 和
  输出转换查询全部在锁外等待。D3D11 命令块只显式 `Flush` 一次；
  `GetData` 使用 `D3D11_ASYNC_GETDATA_DONOTFLUSH`，读回 `Map` 使用
  `D3D11_MAP_FLAG_DO_NOT_WAIT`，两者都有 `2000 ms` 硬超时并返回真实错误。
  没有自动切换模型、降到 Lite、改 `scale` 或继续播放的隐藏回退。接口未变，
  runtime ABI 保持 `8`。
- 修复后 runtime 和 probe 通过 MSVC `/W4 /WX`。真实质量档 4K Engine 先跑
  `30 + 100` 次，再跑 `30 + 1000` 次压力探针；后者平均 `36.17 ms`、
  P95 `36.99 ms`、最大 `37.87 ms`，profile 3、P010 和 caller context state 均正确，
  无卡死或超时。mpv 早退路径已重编 runtime 并刷新 schema 4 / ABI 8 三模型
  manifest，Release 已重新打包；三处 runtime DLL 的 SHA-256 均为
  `57478937D6C2E46A80AEDCA0F1548B3205DBEA417DEF2E1B6B84EDEBB09959B3`。
  `jfn-frame-interpolation` `16/16`、`jfn-playback` `54/54` 和两包严格 Clippy
  全部通过。
- 转储证据已闭合卡死的因果链，但修复后尚未完成同片真实长时播放，
  不得把压力探针写成实播或视觉验收。

#### 2026-08-03 质量档 HDR10 实播 GPU TDR 四次复现（同步修复后）

- 在 `c1d98a16`（输入转换同步修复）和 `df08c5c6`（硬切同步修复）之后，
  质量档《黑衣人2》3840x2160 HDR10、23.976 fps、RIFE v4.26 `scale=1.0`、
  profile 3、D3D11 P010 实播仍稳定触发 GPU TDR，四次独立复现：

| 复现 | 启动时刻 | 挂起时刻 | 播放时长 | 挂起时片内位置 | 触发特征 |
| --- | --- | --- | ---: | ---: | --- |
| 1 | 15:49:51 | 首帧后 | ~10 s | ~748 s | 输入转换竞态（`c1d98a16` 已修） |
| 2 | 16:00:48 | 16:04:34 | ~3 m 46 s | ~3725 s | 密集硬切段 + Peak detection WARN |
| 3 | 16:28:42 | 16:32:17 | ~3 m 35 s | ~3580 s | 无硬切、无 Peak detection WARN |
| 4 | 16:56:47 | 16:59:48 | ~3 m 01 s | 未知 | 无硬切、无 Peak detection WARN |

- 四次挂起的错误链首见点都是 `vo/gpu-next/libplacebo: Device lost!`，
  RIFE 随后报 `RIFE output conversion wait failed (hr=0x887a0005)`
  （`DEVICE_REMOVED`），filter 被禁用，进程停滞但主界面存活。
  系统日志 `nvlddmkm` Provider 在每次挂起时刻都有 Id=153 内核事件，
  确认为 NVIDIA 驱动检测到 GPU 命令队列错误，不是纯应用层 D3D11 检测。
- 系统日志无标准 TDR 事件（Event 4101 / Display provider），说明本次
  未触发驱动级 TDR 重置，而是 D3D11 命令队列层面的停滞。
- 挂起后转储分析（132 线程）显示 124 个在 `ntdll.dll`、8 个在
  `win32u.dll`，零个在 mpv/rife/d3d11/tensorrt/libplacebo——所有 mpv
  播放线程已退出，进程只剩 CEF 主界面线程，这是“画面定格但进程存活”
  的直接原因。与 19:28 的 GetData 自旋死锁（线程卡在 rife_runtime 内）
  本质不同。
- 关键归因实验（关插帧对照）：同一《黑衣人2》关闭插帧后播放超过
  `6.5 分钟` GPU 完全稳定，显存 `2388 -> 2369 MiB` 平稳，无任何错误；
  开插帧必在 `3.0-3.8 分钟` 挂起。因此挂起根因在 RIFE 插帧路径，
  与 libplacebo 呈现/HDR tone-mapping/解码路径无关。
- 开插帧显存趋势（每 30 s 采样）：挂起前 `4547 -> 4541 MiB` 完全平稳，
  无单调增长，排除显存或资源句柄泄漏。GPU 利用率持续 `95%`（满载）、
  温度 `70-73 C`，无过热。挂起后 GPU util 骤降至 `12%`。
- 推断：显存平稳但必在满载 ~3 分钟挂起，指向 RIFE 每帧在 D3D11 命令
  队列/状态层的累积（每帧创建 6 个视图：`create_input_views` x2 +
  `create_output_views`；以及 ABI 8 引入的多次 `SwapDeviceContextState`），
  而非资源分配泄漏。ABI 7 曾实播 6099 对帧（约 4 分 14 秒）无 device
  lost 但严重卡顿（P95 44.9 ms），ABI 8 锁重构消除卡顿后却引入或暴露了
  该时间累积挂起。
- 本问题不改变已冻结的 RIFE 三档、Engine/profile 或场景阈值，但同片
  长测和视觉验收仍未通过，不得宣称质量档问题已关闭。

#### 2026-08-03 map/unmap 加锁修复与 11 分钟实播验证（GPU TDR 根因关闭）

- 根因确定为 CUDA 互操作竞态：`cudaGraphicsMapResources` 和
  `cudaGraphicsUnmapResources` 内部会使用 D3D11 immediate context 但
  不加锁（NVIDIA 社区实践验证，
  [forums.developer.nvidia.com](https://forums.developer.nvidia.com/t/d3d11-device-context-in-a-separate-thread-gets-corrupted-when-cuda-graphics-resource-mapping-is-used/232326/2)）。
  ABI 8 锁重构把 map/unmap 移出锁块后，libplacebo 呈现线程与 CUDA 映射
  并发访问同一 immediate context，长时间运行损坏 context 状态，触发
  GPU 挂起（nvlddmkm Id=153 / `DXGI_ERROR_DEVICE_HUNG`）。
- 修复（DLL SHA-256 `588f125b9d1586a51a654ed5a0f0c1d7ebf65875e5a221fa0c6141165fe92cae`）：
  把 `cudaGraphicsMapResources` 和 `cudaGraphicsUnmapResources` 放回
  `D3D11ContextBlock` 锁内，与呈现线程串行化；TensorRT 推理（约 33 ms）
  保持锁外，不重新阻塞 libplacebo 呈现。错误路径中的 unmap 也在锁内执行。
- 修复后同一《黑衣人2》3840x2160 HDR10 质量档实播 **11 分 11 秒** 无任何
  `Device lost` / `DEVICE_HUNG` / `DEVICE_REMOVED` / `Disabling filter` /
  `Failed presenting`；GPU 利用率持续 73-89%（满载）、温度 68-73 C、
  显存 4534-4577 MiB 平稳。此前四次挂起全部在 3.0-3.8 分钟，本次稳定
  播放时长为修复前的约 3.2 倍。
- 至此，质量档开插帧 GPU TDR 的根因（CUDA map/unmap 与 D3D11 immediate
  context 并发竞争）已修复并经实播验证。输入转换同步（`c1d98a16`）和
  硬切同步（`df08c5c6`）修复保持有效。仍不改变已冻结的 RIFE 三档、
  Engine/profile 或场景阈值。

#### 未来重启条件

- 若未来重启任务 3，首选方向仍是让显示器的实际
  刷新节奏跟随播放器提交帧的节奏，而不是把视频补帧到一个错误的固定整数帧率。
- 使用真实源帧率和插帧后的输出帧率驱动呈现节奏，例如
  `23.976 -> 47.952`、`24 -> 48`、`25 -> 50`、`29.97 -> 59.94`。
- 必须检测实际输出显示器、窗口状态、标称刷新率、VRR 能力/范围和呈现路径；
  不能仅凭驱动控制面板显示 “Compatible” 就宣称 VRR 已生效。
- 在窗口化或无边框窗口路径中验证 DXGI/DWM 呈现是否真正进入可变刷新节奏，
  并记录 Present 间隔、抖动、重复帧、丢帧以及音视频同步。
- 运行时诊断至少暴露：源帧率、插帧输出帧率、目标显示器、当前显示模式、
  VRR supported/active 状态和实测呈现节奏。
- VRR 不可用、超出范围或激活失败时必须显式报告真实状态，不得显示伪成功；
  同时不能静默改变 RIFE 模型、`scale` 或插帧开关。
- 验收覆盖窗口化、无边框、跨显示器移动、暂停/恢复、seek、切换模型和
  插帧关闭，并检查 HDR/P010、字幕与音视频同步没有回归。

## 结论摘要

- 产品档位决定使用哪个 RIFE 模型；TensorRT profile 只决定该模型在某个
  输入 shape 上采用哪套执行策略。
- profile 不改变模型权重、`scale`、输出分辨率或产品画质，也不会把视频
  缩放到某个 profile 的尺寸。
- 输入尺寸先按模型自身的 shape alignment 向上补齐，再选择能覆盖该
  padded shape 的 profile。
- 一个动态通用 profile 可以保证任意合法分辨率可用；固定 profile 只在
  TensorRT 对该 shape 实测有收益时才有必要。
- 增加 profile 不会自动带来性能收益。真正起作用的是 TensorRT Builder
  的 tactic 选择、TensorRT-RTX 的 shape specialization，以及最终运行的
  CUDA kernel。
- 最终实现仍保持一个模型一个 Engine；质量和 Lite 各有 4 套 profile，均衡有
  3 套。所有模型都保留 universal，并用中低范围和 4K 范围覆盖已验证的常用
  shape；质量和 Lite 额外保留 UHD fixed 以避免 3840x2176 性能回归。
- 刷新率自匹配未交付：固定显示模式硬切和实验性 VRR 接线均已撤回。当前
  NVIDIA `d3d11va` 直通与 VRR 的冲突没有满足 RIFE 零拷贝及实时性能合同的解法。

## 四个概念

### 产品档位和模型

当前三个产品档位对应三个模型：

| 产品语义 | 模型 | `scale` |
| --- | --- | --- |
| 质量优先 | RIFE v4.26 | `1.0` |
| 均衡优先 | RIFE v4.26 | `0.5` |
| 流畅优先 | RIFE v4.25 Lite | `1.0` |

档位选择决定模型图、模型权重和计算量。档位不会根据分辨率自动切换，
也不应因为 profile 不匹配而静默切换到另一个模型。

### `scale`

`scale` 是模型图的配置，不是 TensorRT profile。`scale=0.5` 不表示把
视频缩小到某个固定输出分辨率；它是 RIFE v4.26 模型内部的推理配置，
输出仍满足播放器的 `[1,3,H,W]` 契约。

### Shape alignment 和 padding

alignment 是模型输入 shape 的约束：播放器把原始宽高向上补齐到 alignment
的整数倍，得到 TensorRT 实际处理的 padded shape，推理后再裁回原始尺寸。

当前约束：

- RIFE v4.26 `scale=1.0`：alignment `64`。
- RIFE v4.26 `scale=0.5`：alignment `128`。
- RIFE v4.25 Lite：alignment `128`。

示例：

| 原始尺寸 | alignment 64 | alignment 128 |
| --- | ---: | ---: |
| `1920x1080` | `1920x1088` | `1920x1152` |
| `2560x1440` | `2560x1472` | `2560x1536` |
| `3840x1600` | `3840x1600` | `3840x1664` |
| `3840x2160` | `3840x2176` | `3840x2176` |
| `4096x2160` | `4096x2176` | `4096x2176` |

profile 不会把 `4096x2176` 改成 `3840x2176`，也不会缩放视频来命中
固定 profile。

### TensorRT optimization profile

TensorRT profile 是动态输入的形状约束和优化目标：

```text
min shape：允许的最小输入形状
opt shape：Builder 重点测速和选 tactic 的形状
max shape：允许的最大输入形状
```

profile 编号没有产品含义。`profile=1` 不代表质量更高，`profile=0` 也不
代表性能更低。

## 当前 Engine 结构

当前每个模型一个 Engine。质量优先和 Lite 的 Engine 内有四套 profile：

```text
profile 0 universal:
  min = alignment x alignment
  opt = 3840 x 2176
  max = 16384 x 16384

profile 1 mid-range:
  min = alignment x alignment
  opt = max = 2560 x 1472 (质量) / 2560 x 1536 (Lite)

profile 2 4k-range:
  min = alignment x alignment
  opt = 3840 x 2176
  max = 4096 x 2176

profile 3 uhd-fixed:
  min = opt = max = 3840 x 2176
```

均衡优先使用相同的 universal、mid-range 和 4k-range，但不包含 profile 3，
因为其实测 4k-range 在 UHD 与旧 fixed 持平。

运行流程为：

```text
选择产品档位
  -> 加载该模型 Engine
  -> 按该模型 alignment 计算 padded shape
  -> 从所有覆盖 padded shape 的专用 profile 中选择范围最窄者
  -> 没有专用匹配时选择 universal profile
  -> 设置实际 input shape 并推理
```

因此当前结构的准确描述是：

> 一个模型一个 Engine；Engine 内始终有动态通用 profile，并按各模型实测结果
> 增加中低范围、4K 范围，以及必要时的 `3840x2176` UHD fixed profile。

它能保证任意合法分辨率运行。`4096x2160`、`3840x1600` 和常见超宽 4K
由 4k-range 覆盖；超出专用范围的合法 shape 仍显式走 universal。

## 性能收益由谁产生

profile 本身不减少像素数，也不减少模型理论 FLOPs。性能收益来自以下
TensorRT 组件：

1. TensorRT Builder 根据 profile 的 `opt shape` 搜索卷积、采样、Resize
   等算子的 tactic。
2. 对固定 shape，Builder 可能使用只适合该 shape 的 Tensor Core tile、
   kernel、融合和内存规划。
3. TensorRT-RTX Runtime 还可能针对实际动态 shape 做 JIT specialization，
   并将结果写入 runtime cache。
4. CUDA 和 GPU 只执行已经选择的最终 kernel；播放器只负责选择 profile、
   设置 shape 和处理错误。

因此，增加 profile 只有在以下条件成立时才有意义：

- TensorRT 为新 profile 选择了不同且更快的 tactic；或
- 新 profile 允许动态范围内不可用的固定 shape 优化；并且
- 端到端测量确认收益稳定覆盖额外的 Engine 构建时间、文件大小和显存
  成本。

旧两-profile 设计中，profile 0 的 `opt` 已经是 `3840x2176`，与 profile 1
的固定 shape 相同；因此最终集合是经过同一 Engine、同一 shape A/B 后冻结，
不是仅凭 profile 名称或数量推断收益。

## 当前精度基线

profile 性能不能脱离模型精度路径比较。当前三档的精度状态不同：

- v4.26 `scale=1.0`：主计算图已转换为 FP16，仅保留少量 Resize 形状/标量
  Cast；不再是整图 FP32 计算。
- v4.25 Lite：主计算图已转换为 FP16，同样只保留少量非图像数据 Cast。
- v4.26 `scale=0.5`：FP16 IO 和主计算，只为 5 次 warp 的最终坐标 Add、
  Transpose 和 GridSample 保留 FP32；没有 FP32 卷积。

播放器当前检查的是 Engine 输入输出为 FP16，不等于检查 Engine 内部每一层
都是 FP16。因此不能只依据 manifest 的 `precision=fp16` 判断完整计算精度。

## 已有性能证据

以下是任务 1 结束时旧两-profile Engine 在 RTX 5070 Ti、TensorRT-RTX
1.4.0.76、`profile=1`、3840x2160、预热 30 次、测量 100 次的 probe 结果。
它们保留为精度图性能基线，不代表当前最终 profile 编号或所有 NVIDIA GPU：

| 模型 | TensorRT 平均 | 端到端平均 | 吞吐 |
| --- | ---: | ---: | ---: |
| RIFE v4.26 | `30.373 ms` | `31.516 ms` | `31.72 qps` |
| RIFE v4.26 `scale=0.5` | `26.342 ms` | `27.498 ms` | `36.36 qps` |
| RIFE v4.25 Lite | `22.687 ms` | `23.820 ms` | `41.97 qps` |

新 `scale=0.5` 图的 4K TensorRT 时间相对旧图下降约 `9.93%`，并已快于
标准 v4.26。其真实影片画质门禁和最终 profile 验收均已通过；本表仍只作为
旧两-profile 精度图基线，最终多 shape 收益以前述候选 A/B 和正式播放器验证
为准，不能写成跨设备固定性能。

## 后续 profile 设计规则

- 保留一个 universal profile，保证任意合法 padded shape 可用。
- 只为实际使用频率高、且基准确认有收益的 padded shape 增加专用 profile。
- 候选形状应按每个模型的 alignment 分别计算，不能直接复用另一模型的
  原始分辨率表。
- 不要为所有分辨率建立固定 profile；profile 数量过多会增加构建时间、
  Engine 大小和显存/策略数据成本，收益会递减。
- 固定 profile 或窄范围 profile 都必须以实测为准；profile 不匹配时使用
  universal profile，不切换模型、不修改 `scale`、不隐藏失败。
- 每个模型的 profile 集合可以不同。模型结构、精度路径和实际工作负载
  不同，不要求三个 Engine 使用相同数量或相同形状的 profile。

推荐的候选分辨率族包括 1080p、1440p/2K、超宽 4K、3840x2160 UHD 和
4096x2160 DCI 4K，但最终是否加入必须通过真实影片或代表性输入验证。

## Profile A/B 基准要求

比较 profile 时必须固定：

- 同一模型、同一 ONNX hash 和同一 Engine。
- 同一 FP16/混合 FP32 精度路径。
- 同一 GPU、驱动、TensorRT-RTX 版本和 Builder 版本。
- 同一个原始输入尺寸及其实际 padded shape。
- 相同 warmup、迭代次数、CUDA Graph、runtime cache 状态和后台负载。

至少记录：

- 选中的 profile 编号和实际 padded shape。
- TensorRT kernel 时间、端到端时间、p50/p95/p99 和吞吐。
- Engine 文件大小、构建时间、显存占用和 runtime cache 状态。
- 播放器真实输出是否成功，是否出现推理失败或隐藏回退。

只有同一模型、同一 shape 的专用 profile 相对 universal profile 获得稳定
收益，才应保留该 profile。模型档位之间的速度差异不能单独证明 profile
有效。
