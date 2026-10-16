# 架构

[English](ARCHITECTURE.md) · **中文**

设备上的各个程序怎么配合、各自放在哪里。程序之间的文件约定（锁、状态文件、FIFO）见 [RELIABILITY.md](RELIABILITY.md)。

## 总览

```
luyaz-core :9460 ──▶ luyaz-ui（触屏）
       ▲  └──▶ lpac（/data/esim）──▶ eUICC 卡
浏览器 ──▶ luyaz-web :9090（管理网页）──▶ luyaz-core
luyaz-sentinel：屏幕主人 + 看门狗
```

`luyaz-core` 是唯一和调制解调器、原厂服务打交道的程序，把 ubus、AT 命令和 sysfs 整理成一套本机 API。管理网页和触屏界面都是这套 API 的客户端，都不直接碰硬件。

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

## 东西放在哪

- 程序和要保留的数据：`/data`（固件升级不会清掉）。
- 日志、锁和运行时状态：`/tmp`、`/var/run`（内存盘，重启就没了）。核心日志 `/tmp/luyaz-core.log`，哨兵日志 `/tmp/luyaz-sentinel.log`，界面日志 `/tmp/luyaz-ui.log`。
- 首次安装前的原厂 `rc.local` 备份：`/data/luyaz/kit/rc.local.orig`（旧装机包装的设备在 `/data/u60-kit/`）。

完整的安装位置和恢复原厂的方法见 [onboard/README.zh-CN.md](../onboard/README.zh-CN.md)。
