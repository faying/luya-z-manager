# 架构

[English](ARCHITECTURE.md) · **中文**

设备上的各个程序怎么配合、各自放在哪里。程序之间的文件约定（锁、状态文件、FIFO）见 [RELIABILITY.zh-CN.md](RELIABILITY.zh-CN.md)。

## 总览

```
luyaz-core :9460 ──▶ luyaz-ui（触屏）
       ▲  └──▶ lpac（/data/esim）──▶ eUICC 卡
浏览器 ──▶ luyaz-web :9090（管理网页）──▶ luyaz-core
luyaz-sentinel：屏幕主人 + 看门狗
```

`luyaz-core` 是正常情况下唯一和调制解调器、原厂服务打交道的程序，把 ubus、AT 命令和 sysfs 整理成一套本机 API（`/v2` 与 `/control`）。管理网页和触屏界面都是这套 API 的客户端；只有哨兵以及触屏在核心失联时的有界紧急兜底（见下）会直接操作原厂接口。

## 组件

| 源码 | 在设备上 | 做什么 |
|---|---|---|
| `crates/luyaz-core` | `/data/luyaz/luyaz-core`、`/etc/init.d/luyaz-core`，状态在 `/data/luyaz/core/` | 核心。API 在 `127.0.0.1:9460`：`/v2/state`、`/v2/events`（SSE）、`/v2/screen` 和控制接口。`luyaz-core doctor` 是体检 |
| `crates/luyaz-web` | `/data/luyaz/luyaz-web`、`/etc/init.d/luyaz-web`，密码文件 `/data/luyaz/web.env` | 管理网页，端口 `:9090`，只对局域网。Rust 服务端渲染 + Datastar |
| `crates/luyaz-sentinel` | `/data/luyaz/luyaz-sentinel`、`/etc/init.d/luyaz-sentinel`，控制 FIFO `/tmp/luyaz-screen.ctl` | 屏幕主人：拉起触屏、崩了重启、连续两次起不来就把屏幕交还原厂界面。也是 Wi-Fi 兜底看门狗 |
| `touch-ui/rs/` | `/data/luyaz/ui/luyaz-ui`（字体和 logo 在同一目录） | 触屏界面，Rust + LVGL，直接画到 DRM/KMS |
| `crates/luyaz-*`（其他） | 链接进核心 | 库：策略、情景、Wi-Fi、告警、eSIM、短信转发、测速 |
| `scripts/esim/` | `/data/esim/` | 可插拔 eUICC 卡用的 lpac 工具 |
| `onboard/` | 不装到设备 | 装机包，把上面这些装到设备上 |

## 启动与监督

- 全部从 `/etc/rc.local` 里的几行启动，每个服务一行 `/etc/init.d/<名字> start`。不停用、不改任何原厂服务：原厂的开机守护进程要等它清单里的服务全部就位，少一个就卡在 logo。
- 核心、管理网页、哨兵退出后由 procd 重启；触屏界面由哨兵重启。
- 屏幕不能长时间空着：几分钟没有任何界面，固件会整机重启。所以哨兵起不来触屏时交还原厂界面，而不是无限重试。

## 触屏紧急兜底边界

正常运行时 `luyaz-ui` 从 `127.0.0.1:9460` 读 `/v2/screen`（`/v2/state` + `/v2/events`），所有写操作发往 `POST /control`。为保证 `luyaz-core` 挂掉或卡死时拿在手里的设备仍能自救，`luyaz-ui` 保留四条有界的本地兜底路径（`touch-ui/rs/ui/src/ui/power.rs`、`src/data/live.rs`、`src/data/core.rs`）：

- **电源菜单兜底**（`power_fallback` / `power_last_resort`）：重启、关机先调 `POST /control`（`device.reboot` / `device.poweroff`）。核心连不上时，触屏直接 `sync` 并调 `ubus -t 10 call zwrt_mc.device.manager device_reboot|device_poweroff '{"moduleName":"zte_topsw_devui"}'`，失败退回 `reboot`/`poweroff`；确认后 40 秒仍未关机/重启，则由 `power_last_resort` 直接调系统 `reboot`/`poweroff`。
- **核心不在线时的控制兜底**（`core.rs::control_fb` → `/data/luyaz/scripts/luyaz-fallback.sh`）：只在 `127.0.0.1:9460` TCP 连接失败时触发（核心回 409/503 或请求超时绝不触发，以免核心其实在执行却被绕过）。脚本先用 `pidof` 再次确认 `luyaz-core` 没在跑，再直接执行断网自救必需的开关（`wifi.radio`、`cellular.data`、`cellular.roaming`、`cellular.rat`、`apn.mode`、`sim.slot`、`usb.mode`、`charge.inhibit`、`fast.boot`）。
- **切回原厂界面兜底**（`request_vendor` / `vendor_fallback`）：优先往 `/tmp/luyaz-screen.ctl` 写 `vendor\n`；若哨兵没在听 FIFO，则后台延时拉起 `/etc/init.d/zte_topsw_devui start` 并给自己发 `SIGTERM` 让出 DRM master（`/dev/dri/card0`）。
- **本地硬件读写**：背光（`/sys/class/backlight`）、电源键（`/dev/input`）与辅助 Wi-Fi 状态（`/sys/class/net/wlan*/operstate`、`iw`）。

## 东西放在哪

- 程序和要保留的数据：`/data`（固件升级不会清掉）。
- 日志、锁和运行时状态：`/tmp`、`/var/run`（内存盘，重启就没了）。核心日志 `/tmp/luyaz-core.log`，哨兵日志 `/tmp/luyaz-sentinel.log`，界面日志 `/tmp/luyaz-ui.log`。
- 首次安装前的原厂 `rc.local` 备份：`/data/luyaz/kit/rc.local.orig`（旧装机包装的设备在 `/data/u60-kit/`）。

完整的安装位置和恢复原厂的方法见 [onboard/README.zh-CN.md](../onboard/README.zh-CN.md)。
