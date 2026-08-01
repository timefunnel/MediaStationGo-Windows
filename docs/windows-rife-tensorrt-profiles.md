# Windows RIFE TensorRT Profile 设计结论

本文记录 Windows MediaStationGo 播放器中 RIFE 模型、输入对齐和
TensorRT optimization profile 之间的边界。本文用于后续 Engine 设计、
性能基准和产品档位讨论，不把 TensorRT profile 当成产品档位。

## 当前任务顺序

以下三项是当前 Windows 播放器后续工作的固定顺序。前一项没有完成验收前，
不得用其未稳定的数据推进后一项的最终设计或基准结论。

### 进度台账

最近更新：2026-08-01，分支 `codex/mediastation-windows-spike`。均衡档精度图
实现检查点 `aa760f2` 已推送；本节同步记录其验证结果和仍未完成的画质验收。

| 任务 | 状态 | 已完成 | 下一步 |
| --- | --- | --- | --- |
| `scale=0.5` FP32 性能 | 实现、数值、探针和真实播放烟测通过，等待三档逐中间帧 A/B | 已淘汰两个失败候选；最终图只为 5 组最终坐标 `Add + Transpose + GridSample` 保留 FP32，三层数值验证和 3840x2160 HDR10 真实播放通过，TensorRT 探针降至 `26.342 ms` | 对同一真实影片基准片段导出三档的逐中间帧结果，检查复杂运动、遮挡、细纹理和切镜并完成主观 A/B |
| TensorRT profile 优化 | 未开始，等待任务 1 最终画质验收 | 已明确当前 universal + `3840x2176` fixed profile 的真实语义、限制和 A/B 规则；新图已取得四种 universal shape 的可用性/性能证据 | 任务 1 三档逐中间帧 A/B 通过后，针对 universal profile 在 1440p、超宽 4K、DCI 4K 的性能设计并实测候选 profile |
| 窗口刷新率自匹配 | 未开始 | 已记录 VRR / G-SYNC Compatible 目标、诊断字段和验收场景 | 完成前两项后，审计 mpv、DXGI、DWM 和窗口呈现链路，先取得 VRR supported/active 的真实证据 |

当前不得重复或误判的结论：

- 质量优先 RIFE v4.26 `scale=1.0` 已完成主计算图 FP16 转换；当前
  3840x2160 probe 为 TensorRT `30.373 ms`、端到端 `31.516 ms`。
- 流畅优先 RIFE v4.25 Lite 已完成主计算图 FP16 转换；当前 3840x2160
  probe 为 TensorRT `22.687 ms`、端到端 `23.820 ms`。
- 均衡优先 `scale=0.5` 的新图为 `fp16_compute_fp32_grid_final`；3840x2160
  probe 为 TensorRT `26.342 ms`、端到端 `27.498 ms`。导出、数值和原生探针
  已通过，但真实影片逐帧画质仍未验收，因此任务 1 尚未完成。
- 上述数据来自 RTX 5070 Ti、TensorRT-RTX 1.4.0.76、当前缓存 Engine、
  预热 30 次和测量 100 次。它们是问题定位基线，不是最终跨设备结论。
- 当前尚未完成真实影片逐帧主观画质验收，也尚未验证 VRR 实际激活。

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
  整帧破坏或切镜混合。但屏幕采样间隔为 250 ms，不能锁定每个 RIFE 中间帧，
  也不能替代三档相同输入的逐中间帧 A/B。
- 尚未完成同一真实片段下 v4.26 `scale=1.0`、v4.26 `scale=0.5` 和 Lite 的
  逐中间帧主观画质验收。完成之前，不得把任务 1 标记完成，也不得据此开放
  最终 profile 设计。

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
- 当前的“两套 profile”是可运行的最小设计，不是充分的多分辨率优化设计：
  它只对 `3840x2176` 提供固定 shape 优化，其余尺寸主要依赖动态 profile。

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

当前每个模型一个 Engine，Engine 内有两套 profile：

```text
profile 0 universal:
  min = alignment x alignment
  opt = 3840 x 2176
  max = 16384 x 16384

profile 1 optimized:
  min = opt = max = 3840 x 2176
```

运行流程为：

```text
选择产品档位
  -> 加载该模型 Engine
  -> 按该模型 alignment 计算 padded shape
  -> padded shape 精确匹配固定 profile 时选择它
  -> 否则选择 universal profile
  -> 设置实际 input shape 并推理
```

因此当前结构的准确描述是：

> 一个模型一个 Engine；Engine 内有动态通用 profile，以及一个针对
> `3840x2160` UHD 补齐形状 `3840x2176` 的固定优化 profile。

它能保证任意合法分辨率运行，但不能称为覆盖所有 4K 形状的优化方案。
`4096x2160`、`3840x1600`、`3840x2048` 和其他超宽尺寸通常会走
universal profile。

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

当前 profile 0 的 `opt` 已经是 `3840x2176`，与 profile 1 的固定 shape
相同；再加上 TensorRT-RTX 的动态 shape specialization，profile 1 不保证
一定明显快于 profile 0，必须做同一 Engine、同一 shape 的 A/B 测试。

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

以下是当前 RTX 5070 Ti、TensorRT-RTX 1.4.0.76、当前缓存 Engine、
`profile=1`、3840x2160、预热 30 次、测量 100 次的 probe 结果。数值用于
说明当前基线，不代表所有 NVIDIA GPU 的固定性能：

| 模型 | TensorRT 平均 | 端到端平均 | 吞吐 |
| --- | ---: | ---: | ---: |
| RIFE v4.26 | `30.373 ms` | `31.516 ms` | `31.72 qps` |
| RIFE v4.26 `scale=0.5` | `26.342 ms` | `27.498 ms` | `36.36 qps` |
| RIFE v4.25 Lite | `22.687 ms` | `23.820 ms` | `41.97 qps` |

新 `scale=0.5` 图的 4K TensorRT 时间相对旧图下降约 `9.93%`，并已快于
标准 v4.26；但真实影片画质尚未验收，且 universal profile 在 1440p、超宽
4K 和 DCI 4K 的结果仍明显不足。因此这些数据只能作为当前实现检查点，不能
提前当作最终的 profile 最优点或跨设备结论。

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
