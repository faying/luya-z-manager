# Speed test

**English** · [中文](SPEEDTEST.zh-CN.md)

The touch UI's speed test page drives the core's speed test engine (manager repo, `crates/luyaz-speedtest`, the same one the admin web page uses); the screen side is `rs/devui/src/data/speedtest.rs` and `rs/devui/src/ui/speed.rs`. Nothing extra is installed on the device.

- Start / stop: `POST /api/speedtest/start`, `POST /api/speedtest/stop` (the core runs the test in a background thread).
- Progress: `GET /api/speedtest/progress`, polled about once a second while the page is open.
- Servers: `GET /api/speedtest/servers` (cached for 5 minutes); the first row is 自动 (the core picks the best server).
- Requests go to the core's loopback port `127.0.0.1:9460/v2/api/...` with no token (`rs/devui/src/data/core_api.rs`). When it cannot be reached, the page says so instead of starting.

The old optional `better-speedtest` plugin (litehtml era) was never installed on real devices and is no longer supported; see tag `legacy-litehtml`.
