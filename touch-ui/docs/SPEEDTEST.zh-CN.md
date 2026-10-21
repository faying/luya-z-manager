# 测速

[English](SPEEDTEST.md) · **中文**

测速页自己不带测速引擎，只是请 core 测、把 core 报回来的结果画出来；引擎是 `crates/luyaz-speedtest`，和管理网页共用。屏幕这边的代码：`rs/devui/src/data/speedtest.rs`（发请求）和 `rs/devui/src/ui/speed.rs`（页面）。设备上不用另装东西。

| 做什么 | 请求（core 本机端口 `127.0.0.1:9460/v2/api/...`，不带 token） |
|---|---|
| 服务器列表 | `GET /api/speedtest/servers`，缓存 5 分钟。第一行「自动」让 core 自己挑。 |
| 开始 / 停止 | `POST /api/speedtest/start`、`POST /api/speedtest/stop`；测试在 core 的后台线程里跑。 |
| 进度 | `GET /api/speedtest/progress`，页面开着时大约每秒一次。 |

core 没有回应时，页面直接提示，不开始测速。
