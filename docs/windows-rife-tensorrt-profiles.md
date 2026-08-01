# Windows RIFE TensorRT Profile 设计结论

本文记录 Windows MediaStationGo 播放器中 RIFE 模型、输入对齐和
TensorRT optimization profile 之间的边界。本文用于后续 Engine 设计、
性能基准和产品档位讨论，不把 TensorRT profile 当成产品档位。

## 当前任务顺序

以下三项是当前 Windows 播放器后续工作的固定顺序。前一项没有完成验收前，
不得用其未稳定的数据推进后一项的最终设计或基准结论。

### 进度台账

最近更新：2026-08-01，分支 `codex/mediastation-windows-spike`。均衡档精度图
实现检查点 `aa760f2`、验证记录 `db88468` 和画质门禁 `95a4cff` 已推送；
任务 1 完成。任务 2 的 profile 集合冻结、通用合同实现、正式 mpv/Release
重建和真实 4K HDR10 播放器验收均已完成，实现提交为 `fdda0a4`。下一步只进入
任务 3 的 VRR / 刷新率自匹配审计，不再修改三档精度图或 profile 集合。

| 任务 | 状态 | 已完成 | 下一步 |
| --- | --- | --- | --- |
| `scale=0.5` FP32 性能 | 完成：实现、数值、探针、真实播放和三档逐中间帧 A/B 均通过 | 已淘汰两个失败候选；最终图只为 5 组最终坐标 `Add + Transpose + GridSample` 保留 FP32；三层数值验证、3840x2160 HDR10 真实播放和 23 个同片段中间帧检查通过，TensorRT 探针为 `26.342 ms` | 冻结本检查点，不再修改另外两档；后续变化必须重新经过相同质量门禁 |
| TensorRT profile 优化 | 完成：合同、构建、缓存、切换、真实播放和回归均通过 | schema 4 / runtime ABI 7 / metadata schema 3 已同步；三档各一个 Engine，profile 数为 `4/3/4`；正式 Release 在同一 3840x2160 HDR10 P010 影片中命中质量 `3`、均衡 `2`、Lite `3`，模型与 scale 均和用户选择一致 | 冻结本检查点；后续 profile 或 ABI 变化必须重新执行原生 probe、正式构建和三档真实播放验收 |
| 窗口刷新率自匹配 | 未开始 | 已记录 VRR / G-SYNC Compatible 目标、诊断字段和验收场景 | 完成前两项后，审计 mpv、DXGI、DWM 和窗口呈现链路，先取得 VRR supported/active 的真实证据 |

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
- 最终 TensorRT profile 集合已实现并通过正式播放器验证；VRR 实际激活仍未验证。

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

- 当前目标显示器支持 VRR / G-SYNC Compatible。首选方向是让显示器的实际
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
