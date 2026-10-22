# 播放器本地弹幕模块

该模块由播放器随包启动，通过一次 stdin JSON 请求和 stdout JSON 响应工作，不监听端口，不依赖额外安装的 Node.js，也不接收 MSG 令牌、媒体文件路径或播放直链。

`local.mjs` 负责匹配策略、磁盘缓存、清洗与偏移；`sources.mjs`、`youku.mjs`、`risk.mjs` 复用原项目源适配规则。源顺序为腾讯视频、爱奇艺、优酷。共享记录直接保存源站节目页面 URL，没有数字节目编号注册表。匹配存在歧义时不会猜测；已选源失败不会自动改选。

构建使用 Node.js **25.6.0**：`npm ci --ignore-scripts --no-audit --no-fund`、`npm test`、`npm run build`。标准 Windows `cargo xtask build` 会执行依赖安装、打包，并将运行时放到播放器旁的 `danmaku/`。

上游源适配器固定于 `huangxd-/danmu_api` 提交 `afc8b8119f981492a5caee52f1e1ebf756bd0d41`。请以上游仓库实际 `LICENSE` 的 **AGPL-3.0** 为准，其 package.json 的 ISC 标记与该文件不一致。本目录的适配与入口代码使用 AGPL-3.0-only，作为独立进程随包提供，主程序通过 JSON IPC 调用。分发内容包括未压缩的 worker、可编辑模块及上游源代码、锁定依赖清单和许可证。Node.js 及其他依赖许可证随包放在 `NODE-LICENSE.txt`、`DEPENDENCY-LICENSES.txt`，构建产物的 `source/` 用于查看和重建该模块。

所有缓存位于播放器用户缓存目录的 `danmaku/`，搜索缓存一天，非空弹幕缓存七天，空结果一小时，风控冷却三十分钟。缓存文件名的哈希只用于磁盘寻址，不参与节目关联或跨设备同步。
