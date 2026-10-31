# Windows 播放链路日志

日志属于 Windows MediaStationGo/Jellium 客户端，默认文件为：

```text
%LOCALAPPDATA%\jellium-desktop\Logs\jellium-desktop.log
```

`[CEF]` 的 `mediastation playback ...` 记录播放准备阶段；`[mpv]` 的
`mediastation resolve probe ...` 和 `mediastation stream ...` 来自 Rust/ureq
取链探测和自定义媒体读取层。后者并非 libmpv 自身打印的 HTTP 日志。
这些事件使用 INFO/WARN 级别，正常启用 INFO 文件日志即可采集。

## 对齐一次播放

1. 从 `playback load started` 的 `media_id` 找到对应的 `playback resolved`。
   `info ready`、`preference ready`、`resolve started` 和 `native load queued`
   分别定位 PlaybackInfo、偏好读取、取链和提交给播放器之前的耗时。
2. `playback resolved` 的 `link_id` 对应后续媒体读取；同一直链重播时 ID 相同。
   每次新建自定义媒体流分配独立 `stream_id`，流内每次 Range 尝试递增
   `request_seq`。ID 只用于当前程序版本的诊断，不是持久缓存键。
3. `reused=true` 表示复用客户端直链缓存；此时 `resolve_ms=0`，日志里的
   探测状态和重定向次数来自原缓存，不代表这次重新访问了服务器或 CDN。
   `expires_in_ms` 是客户端缓存的剩余有效期，已经包含现有安全余量。
4. `resolve probe request` 记录每一跳实际构造的 UA；`playback resolved`
   对比本次请求 UA 和缓存中携带的播放 UA；`stream opened` 和 `range request`
   记录媒体读取实际设置的 UA。`ua_matches` 只比较前两者，不能单独作为
   CDN 已判定 UA 不匹配的证据。

## Range 和错误

- `range request/response`：偏移、重试次数、响应状态、目标主机、响应头耗时、
  Content-Range、上一次位置、旧 reader 是否仍存在。
- `first bytes`：该 Range 第一次成功读取的字节数，以及从发起请求起的耗时。
  这是媒体字节到达读取层的时间，不是视频首帧显示时间；
  `native load queued` 也只代表播放命令提交成功。
- `seek scheduled`：逻辑读取位置已更新并释放旧 reader，尚未执行 HTTP 请求。
  连续 seek 可覆盖未读取的位置；只有之后的 `range request` 才计入请求数。
- `playback startup ready`：每次加载首次进入 Started 的毫秒耗时，包括准备、
  提交原生加载到 Started，以及总耗时。这是播放状态机就绪，不能等同于显示器
  扫出首帧；计时从 Rust `execute_load` 开始，不包括前端点击和等待绘制的时间。
  选轨前移版对视频还要求 `video-frame-info` 已有 VO 当前帧；旧版可能在没有
  选中视频时提前记录 Started，不能直接用旧版这个值比较实际黑屏时间。
- `HTTP failure`：HTTP 错误状态、服务端日期、Server、Content-Type、Retry-After、
  OSS request ID，以及 JSON/XML 白名单中的错误码、message、request_id、EC。
  不保存原始错误正文、Cookie、完整直链或签名查询参数。
- 错误正文最多读取 4 KiB，使用 100 ms 采集预算；超时或截断会明确记录
  `body_state`。无法解析时为 `error_format=unrecognized`，不把缺少原因当作成功。
  这个预算只用于错误正文，不限制正常媒体读取时长。
- 网络连接失败、Range 校验失败、断流恢复、重试耗尽和关闭流也记录关联 ID。
  日志不逐帧或逐次普通读取输出。

`old_reader_open=true` 只说明客户端旧 reader 尚未释放，不能证明 CDN 仍计为一条
活跃连接；`proxy_configured=true` 只说明 Agent 配置了代理，不能证明实际请求
没有受到 NO_PROXY 等规则影响。

埋点复用正常播放已有请求，不增加探针，不触发 OpenList refresh，也不增加取链或
媒体读取的重试次数。安装或启动包含本次修改的程序后才会产生这些记录。

## 2026-10-24 实测与本地因果验证

以下时间均为香港时间。结论是：已确认客户端会把 seek 拒绝放大为长时间顺序读取；
尚未唯一定位 115 最初拒绝 Range 的触发条件。诊断当时尚未改入修复，后续本地
实现和验证见下文对应章节。

### 实际故障链

22:12:40–41，同一条新建立的客户端播放会话先后成功读取文件头、文件尾、
偏移 9177，随后跳转到偏移 1154284931 时连续收到三次 HTTP 403。
三次错误正文均为 `115 pmt 3-2`，请求头 UA 均为
`MediaStationGoWindows/1.1.0`，取链和 CDN 探测 UA 也一致。

- 首次 403 响应头耗时 72 ms，后两次均为 37 ms；既有重试等待合计 450 ms。
- 失败时旧 reader 为请求 3，位置 53792；三次失败没有替换或关闭它。
- 随后 mpv 打印 seek failed、MKV resync。22:16:32 的下一次 Range 日志仍显示
  旧 reader 为请求 3，但位置已经变成 1233633824。这证明失败后同一旧流仍被
  向前读取，不能把这段等待仅归为一次 HTTP 请求慢或解码器初始化慢。
- 约 43 秒后出现硬解初始化；当前记录不能据此给出准确黑屏/首帧时长，也不能
  把四分钟后的读取位置全部算入这 43 秒。
- 22:17 再播时，同一 `link_id` 正常返回 206。现有证据不支持永久失效、固定
  文件损坏或空 UA 的结论。

最初错误的 CDN request_id 为 `6ADCBCD826FEF63139CE9FD1`，后两次为
`6ADCBCD83E0FD83739ADFF89`、`6ADCBCD85F95CB3636E6A8CE`。
这三个 ID 可用于上游按时间定位，不包含签名直链。

### 本地验证

使用程序实际配套的 `libmpv-2.dll`，自报 `mpv v0.41.0-UNKNOWN`，SHA256 为
`5265643476E609E7DD6E0483DEA767A8A50ABF15FC225AF85087DD3D9CD77E23`。
合成 120 秒、320×180、10 fps 的 H.264/MKV，索引置于文件前部；
通过自定义 stream callback 从片头跳到 100 秒，在该次 seek 注入失败。
读取限为每次最多 4096 字节并加 2 ms 延迟，输出使用 `vo=null`。
这里测量的是解复用/解码恢复事件，不是窗口真实首帧。

| 对照 | seek 后行为 | 结果 |
| --- | --- | --- |
| 正常 seek | 直接跳转 | 约 94 ms 后恢复，时间为 100 秒 |
| seek 失败、保留旧流 | 继续顺序读取旧位置 | 额外读取 9688168 字节，约 6.4 秒后到达目标 |
| seek 失败、后续读取返回错误 | 停止读取旧位置 | 额外读取 0 字节，但 mpv 最终报告 `end-file reason=EOF,error=0` |

另用实际 Rust/ureq 读取代码和本地 TCP 服务端验证：新 Range 到达时，旧 reader
保留则旧 TCP 连接仍开放；先 drop 旧 reader 则服务端观察到连接关闭。
模拟服务端按旧连接是否开放决定是否拒绝，只验证连接生命周期，不能证明
115 使用同一判定规则。

`demux_mkv.c` 的 `seek_with_cues` 未检查 `stream_seek` 返回值，随后可能从旧位置
继续解析；本地运行结果与这个路径一致。只关闭 reader 并返回负数还不完整：
必须把传输层错误明确交给播放状态机，否则 EOF 会掩盖实际失败。

### 获准的真实 CDN 对照

22:43:18 开始：一次 HEAD 取链返回 302，耗时 4094 ms；随后使用 ureq 3.3.0/
rustls、相同 UA、相同直链做两组各 4 次 Range，组间隔 3 秒。A 组先建立新请求
再释放旧 reader；B 组先释放旧 reader 再建立新请求。偏移顺序均为
`0 → 1316702601 → 9177 → 1154284931`，对应应用层读取量为
`11850 / 383718 / 44615 / 4096` 字节。

- 8 次均返回 206，Content-Range 均符合请求偏移和已知文件大小。
- A 组第 4 次请求在组开始后 520 ms 发出，仍正常；该时序已接近故障时 495 ms。
- 实际读取合计 888558 字节；CDN 阶段 4292 ms，含取链共 8391 ms。
- 最多保留两条测试读取连接；无自动重试，无主动 refresh，无配置变更。
- 使用的是本次 HEAD 返回的 URL，不能确认它与 22:12 的 URL 完全相同。
  对照只能说明这次未复现，不能排除偶发策略、节点状态或组合条件。

此前一次 Python 取链尝试以 URLError 结束，未进入 CDN 对照；经用户另行批准后
才补做上述 Schannel HEAD。实际 115 控制 API 调用量没有取得服务端审计证据，
不能将业务取链次数直接等同为 DownURL 调用次数。

### 修复边界与尚缺证据

下述最小修复已在本地实现，验证情况见下一节：

1. 切换 Range 前释放旧 reader，避免持续保留已废弃媒体连接。
2. seek 最终失败后禁止从旧位置继续读；保持真实失败语义，避免顺序扫描到目标。
3. 将 Range 的真实错误状态和安全摘要传给现有播放状态机，不能仅依赖 libmpv
   的 end-file reason；明确区分传输失败与自然 EOF。

这些措施修复已复现的等待放大和错误丢失，不代表已经消除 CDN 的偶发 403。
对 403 的盲目重试、重新取链、刷新 OpenList 或扩大超时均没有获得本次证据支持。

公开的 [rclone/Alist 一手报告](https://forum.rclone.org/t/failed-to-write-to-cache-file/46285)
有同样的间歇性 `115 pmt 3-2`，但没有可靠的错误码释义或已验证修复。
当前 OpenList 的
[115 Open 驱动](https://github.com/OpenListTeam/OpenList/blob/a92d59d35c6c24930fba95a24e10d949480efe67/drivers/115_open/driver.go#L126)
将请求 UA 传入 DownURL 并返回相同 UA。不能从
[#2510](https://github.com/OpenListTeam/OpenList/pull/2510)
的空 UA 案例推出本次一定同因，也不能沿用旧注释中的 TLS 指纹归因作为确证。

服务端相同时段的后台探测、其他读取者、CDN 实际节点和 `pmt 3-2` 的上游解释
尚缺证据；本次生产日志 SSH 读取被自动审批以 `blocked by policy` 拒绝。

## 2026-10-24/25 客户端修复与回归

切换 Range 时先丢弃旧 reader，再发出替代请求。最终 seek 或读取恢复失败后，
该流进入终止状态：后续 read 返回错误、seek 返回 `MPV_ERROR_GENERIC`，不再读取
旧位置，也不再追加恢复请求。成功的有限重试仍可从目标偏移继续读取。

每次原生加载使用独立的 `session_id`，即使重播复用了相同直链。读取回调只向
既有 coordinator 队列投递结构化错误，不调用 libmpv API。状态机核对 session，
只发出一次 Error，并忽略该次失败后的 EOF/FILE_LOADED；新加载清除终止状态。
Runtime 在加载使用的同一把锁下核对 session 后提交 stop，避免旧流错误停止重播。
如果 FILE_LOADED 和当前流错误位于同一批队列，coordinator 同时撤销该批中先前
生成的播放动作，避免失败后仍执行选轨与播放；旧 session 错误不会撤销新流动作。
播放失败页面显示 HTTP 状态和脱敏后的 CDN message，保留现有手动重试入口。

UA、代理、取链缓存和取链策略未改变。原有 seek 最多 3 次、读取恢复最多 2 次，
以及 150/300 ms 等待均保留；本次没有主动访问 115，也没有刷新 OpenList。

验证：

- `jfn-mpv` 34 项与 `jfn-playback` 59 项单测通过。包括真实 loopback TCP
  观察旧连接关闭先于替代 Range、失败后不再读旧数据或追加请求、有限重试
  成功后从目标位置读、旧 session 错误不影响同片重播，以及同批 FILE_LOADED
  生成的播放动作被当前流错误撤销。后者先以失败测试复现，再验证修复通过。
- `jfn-cef` 的 3 项播放事件 payload 测试通过：HTTP 403 和安全摘要被保留，
  原始 native error、签名 URL 和非白名单字段不会透传。
- `src/playback/tests/stream_failure.rs` 使用实际配套 libmpv、已有的本地 H.264
  MP4 测试素材和仅监听 `127.0.0.1` 的 HTTP 服务。FileLoaded 后向时长 80%
  跳转并注入 3 次 403，连续 3 次回归均在约 460–467 ms 后结束；typed HTTP 403 先进入状态机，
  libmpv 后续终止事件未再生成 Finished。输出为 `vo=null`，硬解和音频关闭，
  测试 stream buffer 为 4096 字节；这不是窗口首帧或真实 CDN 的测量。
- `node --check src/web/mediastation.js` 通过。
- 修改的播放核心 Rust 文件通过 rustfmt 检查，`git diff --check` 通过。
- 播放核心 `jfn-mpv`、`jfn-playback` 的 Clippy（`-D warnings`）通过。
  加入 `jfn-cef` 的检查被 3 处原有 lint 阻塞：弹幕设置的 `collapsible_if`
  和 `preferred_string` 的 2 处 `redundant_closure`；已对照 HEAD 确认原有代码，
  本次没有修改这些无关逻辑，不能把该检查记为整体通过。

原生测试服务的 listener 使用轮询，响应 worker 显式使用 blocking I/O 加 2 秒
读写超时。复测遇到的 `WouldBlock` 在明确连接模式后通过上述 3 次回归；没有通过
忽略该错误让测试成功，也没有访问真实 CDN 来替代本地验证。

2026-10-25 00:40（UTC+8），经核对 V: 指向当前 checkout 后，使用标准
`cargo xtask build --external-mpv="$env:EXTERNAL_MPV_DIR"` 完成 Windows release
构建。`build/jellium-desktop.exe` 与 `build/cargo-target/release/jellium-desktop.exe`
均为 6124032 字节，SHA-256 一致：

```text
9A84B26F8D472D1323F5B272FDCDDF57C7FE2F0C6F069F83097846F61D70FAC6
```

`D:\softwares\MediaStationGo` 中的安装版仍为旧文件，未被覆盖；本次保留本地改动，
未提交、推送或部署。测试结束后没有残留的测试进程，也未创建额外媒体测试素材。

本地原生回归需要显式提供已有测试素材，不会取链或下载：

```powershell
# 先按 windows-build-paths.md 从仓库真实路径加载构建环境。
$env:CEF_PATH = Join-Path (Get-Location).Path '.cache\cef'
$env:MPV_STREAM_TEST_FIXTURE = '<已有的本地可跳转视频文件>'
cargo test --manifest-path src/Cargo.toml -p jfn-playback --test stream_failure -- --ignored --nocapture
```

这些修复处理已证实的失败放大和错误丢失。尚未确定 `115 pmt 3-2` 的触发原因，
不能把本地回归成功写成 CDN 403 已根治。

## 2026-10-25 起播优化：避免未读取的 Range

本轮检查 `%LOCALAPPDATA%\jellium-desktop\Logs\jellium-desktop.log` 中
01:21:40 的起播。提交给播放器前用时 3961 ms：PlaybackInfo 305 ms，偏好读取
62 ms，解析 3592 ms；解析中等待服务端 302 用时 3464 ms，CDN 单字节验证
127 ms。本轮未进行服务端 SSH 或真实取链/115 对照请求，不能进一步分解 3464 ms。

原生加载的前 5 次 Range 为 `0 → 1316702601 → 9177 → 274654549 → 273335141`。
第 4 次响应头耗时 93 ms，但没有 `first bytes`，在任何读取发生前已被第 5 次
seek 替代。此请求没有向解复用器交付媒体数据。

改动仅在现有客户端读取层：seek 更新逻辑游标并立即释放旧 reader，读取时才
建立对应 Range。多次连续 seek 只保留最后一个读取位置；同一有效 reader 的
当前位置 seek 直接复用连接。没有新增预取、缓存、副本、后台探针或自动取链。
这不改变源数据或选轨顺序，也没有降低缓冲量和错误校验。

逻辑 seek 的返回值只表示游标定位；HTTP 状态和 Content-Range 校验在随后的
read 中执行。打开目标 Range 仍最多 3 次、间隔 150/300 ms；若最终失败，read
返回 -1，并发出原有的结构化终止错误。之后的 read/seek 不再产生 HTTP 请求。
普通断流的最多 2 次恢复保持不变，403 不会被当成自然 EOF 或重试成功。

本地验证（均未调用真实云服务）：

- 先通过失败测试复现 eager seek 提前发送请求；改动后连续逻辑 seek 到真正
  read 之间请求数为 0，首次 read 只请求最终偏移。
- 模拟片头、片尾、回片头、一次未读取跳转和最终续播的真实 TCP 读取序列；
  每个响应延迟 80 ms。旧 eager-open 参考行为 5 次请求、408.882 ms；新行为
  4 次请求、329.345 ms，返回字节一致。测试断言请求与字节，不用易抖动的
  耗时阈值判定成功。这是该序列的约 80 ms 收益，不能代表整次实际起播提速。
- `jfn-mpv` 37 项、`jfn-playback` 59 项、CEF 播放 payload 3 项，共 99 项通过。
- 实际配套 libmpv 加现有本地 MP4 的 403 注入回归通过：3 次拒绝，约 458 ms，
  结构化错误保留，未误报 Finished。此测试不测窗口首帧或真实 CDN 状态。
- 播放核心 Clippy（`-D warnings`）通过。CEF 检查仍为上一轮相同的 3 处原有
  lint，未将其写成整体通过，也未修改无关代码或在源码中屏蔽检查。

本轮同时增加每次加载首次 Started 的 `startup ready` INFO 埋点，以便下次播放
区分准备等待和原生启动耗时。当前实测中，冷取链的 3464 ms 仍占主要部分；
本轮优化没有消除该段服务端等待。

01:35（UTC+8）标准 Windows release 构建完成。`build/jellium-desktop.exe` 与
`build/cargo-target/release/jellium-desktop.exe` 均为 6130176 字节，SHA-256 一致：

```text
4DC98901EC5FD8C3650A89A45F47B34C6915592B4943C90E6E15398904F2890B
```

读取层 rustfmt 和 `git diff --check` 通过。用户退出后才替换 build 产物；
没有改写安装目录、提交或推送，也未自动启动程序产生新的真实播放请求。

## 2026-10-25 播放器端：选轨前移与首帧就绪

01:39 的客户端日志显示，准备阶段为 2501 ms，原生加载后 290 ms 就记录了
`startup ready`，但随后仍有 3 条 `Track switched`，AO 才在 01:39:37 出现，
VO 到 01:39:38 才初始化。因此这次旧版 Started 不是实际视频就绪的证明。
读取层还实际请求了 264117667 和 273335141 两个相邻定位位置；上一轮只合并
尚未读取的 seek，不能阻止选轨交错期间已经开始的读取。

本轮只改播放器启动顺序：

- 使用配套 mpv 的 `on_preloaded` hook。在容器轨道目录可读、默认选轨和解码器
  初始化之前，恢复现有内嵌字幕偏好，写入最终 vid/aid/sid，并添加已准备好的
  外置轨道。FILE_LOADED 后只解除暂停，避免先空轨加载再补选造成的重复初始化。
- 显式轨道按原生目录校验，已有同类轨道但请求 ID 不存在时明确失败，不让
  mpv 静默改选其他轨道。没有相应媒体类型时保持禁用；未探测音频仍仅按原有
  `defer_audio_to_mpv` 条件交给 mpv 默认选择。字幕关闭仍明确传 `no`。
- 起点为 0 时使用 mpv 的 `start=none`，避免无意义的初始 seek；非零续播点保留。
- 视频 Started 要求 VO 已有当前帧，未知元数据也不能绕过此条件。容器确认的
  纯音频不需要视频帧。初始 pause/unpause、playback-restart、缓存清空均遵守此门槛。
- hook 按当前加载路径核对，旧加载的 hook 仍会释放；错误批次只撤销播放动作，
  保留 hook 释放动作。选轨失败会记录错误并停止，晚到的 FILE_LOADED/EOF/cancel
  不会把失败改成成功或正常结束。

顺序依据为当前配套源码 `third_party/mpv/player/loadfile.c` 和
`third_party/mpv/DOCS/man/input.rst` 的 `on_preloaded` 说明；未改变 libmpv DLL。

验证使用现有 0.96 秒本地 H.264 MP4、vo=null/ao=null，未访问服务端或 115：

- 真实 libmpv 的旧流程有 3 条加载后切轨日志，新流程为 0；续播 0.4 秒时，
  新流程首帧位置为 0.44 秒，旧流程首次就绪位置为 0.04 秒。
- 新流程从头播放 seek=0，非零续播 seek=1。字幕关闭和本地外置 SRT 选择通过；
  外置字幕同样没有加载后切轨。测试临时字幕已自动删除。
- `jfn-mpv` 38 项、`jfn-playback` 64 项、CEF 载荷/轨道目录/字幕偏好 20 项，
  共 122 项单元测试通过。原生选轨对照与原生 403 注入两项独立回归通过；
  后者仅请求 127.0.0.1，3 次拒绝约 460 ms，未误报 Finished。
- 播放核心 Clippy（`-D warnings`）通过；真实播放集成测试已完成编译检查，
  未调用真实片源。需要 ffmpeg 生成双字幕 MKV 的原生 CEF 测试本轮未执行，
  内嵌字幕偏好由现有目录/偏好单元测试覆盖。

本地 null 输出没有测 Windows GPU/音频设备初始化、窗口实际显示、真实 MKV
首帧耗时或 CDN 请求数量变化，不能把上述对照换算为实片节省秒数。

02:05:51（UTC+8）标准 Windows release 构建完成，耗时 46.92 秒。
`build/jellium-desktop.exe` 与 `build/cargo-target/release/jellium-desktop.exe`
均为 6138880 字节，SHA-256 一致：

```text
F2D295DFC9DBB7BDE4EC61B38AF58F27798E7E76FBD74F2D0AEABE3D82CD200C
```

用户退出后替换了标准 build 产物。libmpv DLL 哈希保持
`5265643476E609E7DD6E0483DEA767A8A50ABF15FC225AF85087DD3D9CD77E23`。
修改文件的 rustfmt 与 `git diff --check` 通过；测试临时字幕已清理，未自动启动
播放器，未修改安装目录，未提交或推送。

## 2026-10-25 播放器端：复用取链校验的文件头

02:09:47（UTC+8）实测的 `resolveMs=1880` 只计取链；Rust 加载到视频就绪
共 3837 ms：PlaybackInfo 271 ms、偏好 59 ms、取链 1880 ms，交给原生播放器
前共 2211 ms，原生阶段 1625 ms。四次 Range 头等待共 639 ms，其余 986 ms
包括读包、容器处理、续播定位和音视频初始化，尚不能归因于其中某一项。

读取顺序为 `0 → 1316702601 → 9177 → 1035457134`，前后两次文件头解析
实际读到偏移 11850 和 53792。原先 CDN 校验只读取 1 字节，随后 mpv 又
请求文件头；跳转尾部元数据之后再次回头联网读取。这两次头等待为
198 ms 和 81 ms。

本次仅改客户端取流与交接：

- 自定义 ureq 播放路径在原有跨域 CDN 校验中请求 `bytes=0-65535`，最多
  64 KiB；同源取链和标准 Emby/mpv HTTP 路径仍使用原来的 1 字节校验。
- 校验响应的 Content-Range 起点、范围上界、总长度和 Content-Length，
  完整读取声明的头部后才交接；编码响应、截断响应明确失败。
- 文件头与本次播放完整 URI 绑定，包括 URL、UA、大小、代理模式和唯一会话。
  媒体数据保留在内存 Arc 中，不加入 URI、不写日志、不落盘。
- mpv seek 到缓存范围时直接读相同响应的字节；越过范围才从准确偏移请求
  新 Range。终止错误先于缓存检查，403 后不能靠旧字节继续或误报 EOF。
- 取链缓存只为最近一个新解析的片源保留文件头；待打开流最多保留一份，
  下一次原生加载会替换它，流打开时消费。已打开流独立持有自己的只读前缀。
  UA 变化也会使既有取链缓存失效。

本地验证不访问服务端、OpenList 或 115：

- 记录偏移序列的逐字节对照，原生阶段 4 次 Range 降为 2 次。每次本地响应
  固定延迟 80 ms 时，331.1831 ms 降为 164.4982 ms；此阶段从文件头已取得
  开始计时，未计多读校验响应的成本，也不是实片首帧测量。
- 使用已有 85807 字节 MP4 和实际 libmpv，交接 64 KiB 后只请求一次
  `bytes=65536-`；0.4 秒续播成功输出视频帧，位置 0.44 秒。
- 覆盖缓存边界的连续读取、小文件完整缓存/EOF、重复回读、完整 URI 绑定、
  缓存数量、UA 变化、错误偏移、超限范围、短响应和缓存后的 403。
- 原生 403 注入回归仍为结构化错误，3 次拒绝，约 461 ms，未误报 Finished。

本次真实日志的两次头等待合计 279 ms，但这只是可减少的旧请求等待，
必须扣除新增文件头传输成本。不能据此承诺整次播放固定减少 279 ms。
MKV 索引、起点探测、续播准确性、选轨及 libmpv DLL 均未修改。

验证汇总：mediastation 83 项、mpv 43 项、playback 64 项、CEF runtime 58 项，
共 248 项单元测试通过；原生文件头续播与 403 注入两项独立回归通过。
三项播放核心库的 Clippy（`-D warnings`）、改动 Rust 文件的 rustfmt 及
`git diff --check` 通过。CEF 两个 URL 测试同步包含之前已加入的唯一会话 ID。

02:28:41（UTC+8）标准 release 构建完成，Rust 构建耗时 42.30 秒。
`build/jellium-desktop.exe` 和 `build/cargo-target/release/jellium-desktop.exe`
均为 6146560 字节，SHA-256 一致：

```text
8E6F935CA36F0D14C00FCAC0E7CE56BCE401C89E21AE3235480ACC50512DFE36
```

产物中已核对 `resolve initial head read`、`stream initial head reused` 和
`playback startup ready` 三条日志字符串。用户退出后已替换标准 build 产物，
未自动启动播放器；未修改安装目录，未提交、推送或访问真实片源。
测试没有生成媒体文件，测试临时字幕目录核对无残留。

## 2026-10-25 网络停流后的取消与退出卡死

20:25:33 的现场转储与 20:27:46 的实时堆栈显示相同等待链：应用主线程
在 `mp_destroy_client/mp_thread_join` 等待 mpv core，core 在
`kill_demuxers_reentrant` 等待 demux，demux 在自定义读取的 socket `recv`
内阻塞。运行内存中的 `mp_cancel.triggered=1`，但实际注册的 `cancel_fn`
只有一条 `ret`。20:23:46 的关闭请求已经进入清理，仍无法结束这个读取。
这些证据确认了持续阻塞和退出卡死的客户端原因，未确定网络最初停流的上游原因。

修复只涉及自定义媒体读取生命周期：

- 每条流共享独立的原子取消标记。`cancel_cb` 不获取读取锁、不调用 libmpv；
  正在等待响应头或正文的 transport 每 100 ms 检查取消，观察到后终止读取。
  已取消流后续的 read/seek 同样失败，不再交付缓存文件头或追加 Range。
- 沿用现有 ureq 默认 connector、TLS 和代理行为。轮询在同一个读取内部进行，
  只消耗这 100 ms 的轮询超时；真实 EOF、网络错误继续进入原有处理流程。
- 正常媒体每次等待新数据最多 15 秒。持续供数会开始下一次独立等待，
  没有对整部影片的响应生命周期设置总超时。原有最多两次断流恢复保留，
  全部失败后继续上报结构化传输错误，不能当作自然 EOF。
- DNS、连接、初始发送、响应头分别设置 5/5/10/15 秒边界。
  DNS/连接建立及初始 TLS 发送阶段使用这些边界，不承诺它们能在 100 ms 内取消；
  100 ms 是已建立连接的接收轮询间隔，也不是所有 Windows 调度下的硬实时保证。
- 主动停止、换片和退出造成的取消不生成播放失败事件。错误正文仍使用原有
  100 ms 采集预算；ureq 包装的读取超时保留明确的 `timeout` 原因。

本地验证未访问真实片源、115、OpenList 或生产 SSH：

- 两项真实 loopback TCP 失败回归先复现空取消回调：等待响应头与收到部分
  206 正文后停流，都无法在一秒内取消。修复后两项约 90–99 ms 返回，
  请求数保持一次，没有恢复重试或终止错误。
- 覆盖正常 206 正文的无数据超时、既有两次恢复耗尽后保留终止错误、取消后
  缓存和新 Range 均不可读。持续供数的响应总时长超过单次 idle 边界仍完整成功。
- Rustls HTTPS 服务在测试进程内仅监听 `127.0.0.1`；证书与私钥是公开测试
  fixture，仅由这个测试客户端信任，不修改系统证书或生产客户端配置。
  多次 180 ms 的供数间隔跨越轮询超时后仍逐字节完整读取同一响应；
  另一条停流 TLS 正文约 58 ms 响应取消。
- 使用实际配套 libmpv、已有 85807 字节 MP4、`vo=null/ao=null`，
  在 seek 后注入保持连接但不再供数的 206 响应。停止、换片、直接销毁
  三种场景连同 mpv 销毁分别约 181/185/180 ms 完成；服务端确认客户端
  关闭旧连接，未依赖测试端提前发送 EOF，也没有再打开旧流 Range。
- 原生 403 回归仍为三次拒绝后报告结构化 HTTP 403，约 464 ms 结束，
  未误报 Finished。原生选轨与文件头续播两项回归同样通过。
- `jfn-mpv` 50 项、`jfn-playback` 66 项单元测试通过；上述 4 项原生回归
  通过。两项核心 crate 的 `cargo clippy --all-targets -- -D warnings` 通过。

本轮测试没有测量真实 CDN 的恢复率、GPU 渲染或实际窗口首帧；修复目标是
不让一次停流变成不可取消的永久读取。原始现场保存在
`%LOCALAPPDATA%/jellium-desktop/Diagnostics/hang-20261025-202533/`。

21:12:17（UTC+8）标准 Windows release 构建完成，Rust 构建耗时 47.90 秒。
已先核对 V: 对应当前 checkout，并在原始转储留存后结束原卡死 PID 12848，
再替换标准 `build/jellium-desktop.exe`。该文件与
`build/cargo-target/release/jellium-desktop.exe` 均为 6151168 字节，SHA-256 一致：

```text
39EAB2FDF1030EE4B81A482313D218C0A97C6E95F677270908322D64C81AE8DD
```

配套 libmpv DLL 哈希仍为
`5265643476E609E7DD6E0483DEA767A8A50ABF15FC225AF85087DD3D9CD77E23`。
没有修改安装目录或自动启动新的真实播放，未提交或推送。

## 2026-10-29 标准 Emby HTTP 首个响应复用

现场 v1.2.0 日志中，17:01:17 的 media_id=2862785 从 load 到首帧共
14816 ms；准备阶段 3243 ms，其中 1 字节 Range 预检 3027 ms；native
加载到首帧另需 11573 ms。该次为 `standard_emby_stream`，没有经过已有的
115/CDN 文件头缓存路径。预检结束后约 7 秒才进入 `on_preloaded`，说明
只看界面的 resolveMs 会漏掉 native 打开媒体和容器解析的等待；尚不能据此
把这些时间全部归因于代理、解码器或服务器。

本次仅调整标准 Emby HTTP 的客户端读取：

- 继续由同一个 PlaybackSessionResolver 校验来源、逐跳解析 Location、
  限制跳转次数并禁止 HTTPS 降级；同源携带 X-Emby-*，跨源移除凭据，UA
  和代理模式沿用当前会话。标准读取发送 `Range: bytes=0-`，不再发送
  `bytes=0-0` 后丢弃响应、交由 FFmpeg 重新请求。
- 媒体 transport 保留校验成功的响应正文，连同 agent 和取消控制直接移交
  native stream；URI 只包含本次 load 的唯一 ID。沿用同一份已解析链接
  缓存，命中时直接向有效目标打开新的响应，不重复经过返链接口；UA、
  来源、鉴权、代理或过期条件变化仍按原规则失效。活连接不进入链接缓存，
  同一响应只能由匹配的 load
  消费一次。新 load、停止和 loadfile 入队失败会释放未消费的响应。
- 后续 Range 复用现有读取、延迟 seek、有限恢复和结构化失败逻辑；标准
  HTTP 的自动跳转始终关闭，后续响应不能携带旧凭据自动跳到其他来源。
  200 响应按不可 seek 的顺序流处理，206 校验范围及长度，拒绝编码后的
  媒体正文和已知长度变化。HTTP 错误诊断会移除请求中的服务端凭据。
- callback 流本身没有 mpv 的网络标记，因此仅为本次标准 HTTP 文件把
  `cache=auto` 映射为 `cache=yes`，保留默认网络预读；显式 yes/no 不变。
  既有 DirectStreamUrl/115 的 64 KiB 前缀缓存、UA、代理、取消及选轨路径
  不启用这项映射。没有缩小探测大小、索引范围或解码缓存。

新增 `playback startup stage` 的 `preloaded`、`file_loaded` 两个阶段，
首帧日志补充 `native_to_preloaded_ms`、`preloaded_to_loaded_ms` 和
`loaded_to_frame_ms`。这些阶段分别覆盖 native 打开和容器解析、选轨及初始
续播加载、加载完成到实际首帧；外部字幕准备也可能计入相邻阶段，不能把
其中某段直接等同于纯解码耗时。Range 日志继续记录响应头和首批数据时间。

本地验证均使用 127.0.0.1 和已有 85807 字节 MP4，不访问真实片源：

- 88 项 mediastation、54 项 mpv、66 项 playback、60 项 CEF runtime
  单元测试通过。真实 loopback 验证了同源鉴权、跨源移除私密头、UA 一致、
  首次响应直接读取、200 顺序流、无效 Range、403 和一次性 load 绑定，
  同片缓存重播省去返链请求、但每次新开媒体响应，UA 变化重新解析。
- libmpv 首帧对照给每次响应头增加 80 ms：默认网络缓存下从头播放
  177 ms → 91 ms，续播 0.4 秒为 174 ms → 94 ms；请求均从 2 次降为
  1 次。显式 cache=no/yes 同样只有 1 次请求，续播位置为 0.44 秒。
  这是验证去掉重复请求的本地对照，不是实际远端视频的加速承诺。
- 原有 64 KiB 前缀仅请求剩余数据并到达 0.44 秒首帧；提前选轨仍为
  0 次加载后切轨，零起点 0 次 seek，非零续播 1 次 seek，外部字幕通过。
- 停流后的 stop/switch/destroy 含 mpv 销毁约 181/186/181 ms。403 注入
  仍只拒绝 3 次 Range，约 460 ms 上报结构化错误，不误报自然结束。
  这两项集成测试共享全局错误回调，必须以 `--test-threads=1` 串行运行；
  首次并行运行失败，单独及串行重跑均通过，未修改其生产逻辑来绕过测试。

mpv/playback 的 Clippy 严格检查通过。mediastation 的严格 Clippy 被其他
未提交的 `src/mediastation/src/api.rs:676` 的 manual_inspect 问题阻断，
仅临时禁用该 lint 后通过。CEF Clippy 另有既存的 collapsible_if 一处与
redundant_closure 两处，位于未改动的弹幕和元数据代码；临时排除这些
既存 lint 后通过。没有修改其他任务代码或永久降低仓库 lint 级别。

17:57:51（UTC+8）最终标准 Windows release 构建完成，Rust 阶段耗时
47.02 秒；从仓库真实 D: 路径构建，CMake 4.4.4，使用现有 external mpv，
没有创建盘符映射或隔离构建目录。两个 exe 均为 6257152 字节：

```text
build/jellium-desktop.exe
build/cargo-target/release/jellium-desktop.exe
SHA256 32AEEEE124A5C9A06B4E239994020B0465BC0EF14B243BAFCA75C9EBC5772A78
```

配套 libmpv-2.dll 与测试前一致：
`4F85C5904481C30817FE41A9805B059540EB0A029DBE870DDBCDB30E44FF1469`。
改动 Rust 文件的 rustfmt、git diff --check 通过；测试临时字幕无残留。
未自动播放真实片源、替换安装目录、提交或推送。当前运行的安装版仍为
16:53 的旧构建，实测本次优化需启动新的 build 目录程序。
