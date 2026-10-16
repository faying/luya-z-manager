# Luya-Z 管理后台

[English](README.md) · **中文**

**Luya-Z** 的一部分：给 5G 随身 Wi-Fi 用的设备端核心（`luyaz-core`）、浏览器管理网页（`luyaz-web`）、屏幕主人兼看门狗（`luyaz-sentinel`）、前面板触屏界面（`touch-ui/`）和**一键装机包**。全部在这一个仓库里。

> **适用硬件：** 中兴 U60 Pro（MU5250）。社区项目，和中兴（ZTE）没有关系，风险自负。

| 管理网页 | 触屏界面 |
|---|---|
| <img src="docs/images/web-home-desktop.png" width="560" alt="管理网页首页"> | <img src="docs/images/touch-home.png" width="200" alt="触屏首页"> |

## 各部分怎么配合

| 组件 | 在设备上 |
|---|---|
| `crates/luyaz-core` | 核心：`/data/luyaz/luyaz-core`，API 在 `127.0.0.1:9460`（`/v2/state`、`/v2/events`、`/v2/screen`、控制接口），由 procd 监督（`/etc/init.d/luyaz-core`）；`luyaz-core doctor` 是体检 |
| `crates/luyaz-web` | 管理网页，端口 `:9090`（只对局域网，Rust），密码文件 `/data/luyaz/web.env` |
| `crates/luyaz-sentinel` | `/data/luyaz/luyaz-sentinel`：屏幕主人 + 看门狗（Wi-Fi 兜底、告警） |
| `touch-ui/` | 触屏界面（Rust + LVGL，`touch-ui/rs/`），设备上是 `/data/luyaz/ui/luyaz-ui`；设备端脚本在 `touch-ui/scripts/` |
| `crates/luyaz-*`（其余） | 核心用的库：策略、情景、Wi-Fi、告警、eSIM、测速等 |
| `onboard/` | 装机包 |

```
luyaz-core :9460 ──▶ luyaz-ui（触屏）
       ▲  └──▶ lpac（/data/esim）──▶ eUICC 卡
浏览器 ──▶ luyaz-web :9090（管理网页）──▶ luyaz-core
luyaz-sentinel：屏幕主人 + 看门狗
```

## 功能

- **luyaz-core**：一个 Rust 程序，把 ubus、AT 命令、sysfs 整理成一套本机 API：设备/电池/温度、信号与载波、锁频锁小区、SIM/短信、APN、DNS/DHCP/防火墙、Wi-Fi、USB 模式、测速、情景等。
- **管理网页**（`crates/luyaz-web`，Rust 服务端渲染 + Datastar）：替代原厂网页，手机和电脑都能用，浅色/深色，中英文。
- **触屏界面**（`touch-ui/`）：3.5 寸前面板上按任务分的 5 个标签，见 [touch-ui/README.zh-CN.md](touch-ui/README.zh-CN.md)。
- **eSIM**：配合可插拔 eUICC 卡（5ber、eSTK.me 这类）下载、切换、删除 profile。
- **代理**：不带。想自己装，按 [docs/PROXY.zh-CN.md](docs/PROXY.zh-CN.md) 从官方来源自己配。
- **可靠性**：核心、管理网页、哨兵由 procd 监督；Wi-Fi 兜底看门狗；告警横幅、「健康」页、可选告警短信；
  `./install.sh doctor` 只读体检，`backup` / `restore` 备份配置。约定见 [docs/RELIABILITY.md](docs/RELIABILITY.md)。
- **装机包**（`onboard/`）：新设备一条 `./install.sh` 装好 SSH、管理网页、触屏、eSIM，并关掉固件自动升级。

## 快速开始

**从零开始请看 [docs/GETTING-STARTED.zh-CN.md](docs/GETTING-STARTED.zh-CN.md)**：准备什么、怎么编出装机包、怎么装、怎么检查和退回。

只有固件 **B27 及更早** 能从零装（B28 起中兴删了开 ADB 的接口）。**装之前别升级固件。**
目前不发布编译好的二进制，需要自己编，指南里有完整步骤。

## 开发与构建

```sh
# 核心和管理网页（aarch64 musl；onboard/build-kit.sh 会在 Docker 里编）
cargo zigbuild --profile release-device --target aarch64-unknown-linux-musl -p luyaz-core
cargo zigbuild --release --target aarch64-unknown-linux-musl -p luyaz-web

# 触屏界面（Docker）
(cd touch-ui && rs/setup-src.sh && rs/build.sh device)

# 管理网页浏览器检查（容器里跑假核心）：见 crates/luyaz-web/e2e/run.sh
(cd crates/luyaz-web && npm ci && e2e/run.sh)

# 装机包 → onboard/dist/luyaz-kit-YYYYMMDD.tar.gz
./onboard/build-kit.sh
```

已经装好的设备更新单个组件，见 [DEPLOY.zh-CN.md](DEPLOY.zh-CN.md)。

## 目录

```
crates/        luyaz-core（核心）、luyaz-web（管理网页）、luyaz-sentinel（屏幕主人 + 看门狗）、luyaz-* 库
touch-ui/      触屏界面：rs/（Rust + LVGL）、scripts/（设备端脚本）、docs/
onboard/       装机包：build-kit.sh（打包）、install.sh（装机）、device/（设备端脚本）、test/（沙盒测试）
scripts/       esim/（lpac 工具包）、tailscale/（Tailscale 开机启动，用法见其 README）
docs/          GETTING-STARTED.md、DESIGN.md（界面设计规范）、RELIABILITY.md（可靠性约定）
```

## 文档

- [docs/GETTING-STARTED.zh-CN.md](docs/GETTING-STARTED.zh-CN.md)：快速上手（新手从这里开始）
- [onboard/README.zh-CN.md](onboard/README.zh-CN.md)：装机包说明（装到哪里、日常用法、恢复原厂、常见问题）
- [DEPLOY.zh-CN.md](DEPLOY.zh-CN.md)：更新已装好的设备
- [docs/RELIABILITY.md](docs/RELIABILITY.md)：核心、哨兵和脚本之间的文件约定
- [docs/DESIGN.md](docs/DESIGN.md)：管理网页和触屏的设计规范
- [touch-ui/README.zh-CN.md](touch-ui/README.zh-CN.md)、[touch-ui/docs/](touch-ui/docs/)：触屏的构建、硬件、eSIM、测速
- [CLAUDE.md](CLAUDE.md)：给 AI 编程助手看的设备规则（人也值得读一遍「不能做的事」）

## 致谢

- Wei REN：核心（luyaz-core）、管理网页（luyaz-web）、哨兵、Rust 触屏、装机包、eSIM、Tailscale。
- [33333s](https://github.com/33333s)：核心读写设备的那一层来自 [zwrt-datad](https://github.com/33333s/zwrt-datad)（MIT）；[u60pro-devui](https://github.com/33333s/u60pro-devui) 是触屏界面的起点。
- [Jesther Silvestre](https://github.com/jesther-ai)：本项目从 [open-u60-pro](https://github.com/jesther-ai/open-u60-pro) 起步，其中的 agent 和网页现已全部换掉。

## 许可证与免责声明

[MIT](LICENSE)。来源、删减内容和第三方组件见 [NOTICE](NOTICE) 和 [touch-ui/NOTICE](touch-ui/NOTICE)。

和中兴通讯没有关系，也没有得到其认可。只在你自己的设备上使用。逆向只为互通和学习，仓库里没有中兴的专有源码。
