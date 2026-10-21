# Speed test

**English** · [中文](SPEEDTEST.zh-CN.md)

The speed test page has no engine of its own. It asks the core to run a test and shows what the core reports; the engine is `crates/luyaz-speedtest`, shared with the admin web. Screen code: `rs/devui/src/data/speedtest.rs` (requests) and `rs/devui/src/ui/speed.rs` (page). Nothing extra needs to be installed on the device.

| What | Request (core loopback, `127.0.0.1:9460/v2/api/...`, no token) |
|---|---|
| Server list | `GET /api/speedtest/servers`; kept for 5 minutes. The first row, 自动 (auto), lets the core choose. |
| Start / stop | `POST /api/speedtest/start`, `POST /api/speedtest/stop`; the test runs in the core's background thread. |
| Progress | `GET /api/speedtest/progress`, about once a second while the page is open. |

If the core does not answer, the page says so and does not start a test.
