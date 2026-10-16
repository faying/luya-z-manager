# 测速

[English](SPEEDTEST.md) · **中文**

触屏的测速页用的是 core 的测速引擎（manager 仓库 `crates/luyaz-speedtest`，管理网页也用它；10-15 之前是 zte-agent 的）；屏幕这边是 `rs/devui/src/data/speedtest.rs` 和 `rs/devui/src/ui/speed.rs`。设备上不用另装东西。

- 开始 / 停止：`POST /api/speedtest/start`、`POST /api/speedtest/stop`（core 在后台线程里跑）。
- 进度：`GET /api/speedtest/progress`，页面开着时大约每秒读一次。
- 服务器：`GET /api/speedtest/servers`（缓存 5 分钟）；第一行是「自动」（core 挑最好的）。
- 请求发到 core 的本机端口 `127.0.0.1:9460/v2/api/...`，不带 token（`rs/devui/src/data/core_api.rs`）。连不上时页面直接说明，不发起测速。

旧的可选插件 `better-speedtest`（litehtml 时代）从没在真机上装过，已不再支持；见 tag `legacy-litehtml`。
