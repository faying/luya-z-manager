<div align="center">

# Luya-Z 管理后台

**给 5G 随身 Wi-Fi 换上的管理网页、触屏界面和设备端核心。**

信号看得懂、能锁频锁小区、管 eSIM 和短信、有体检和看门狗——在设备前面板上，也在浏览器里。

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Latest release](https://img.shields.io/github/v/release/faying/luya-z-manager?sort=date&display_name=tag)](https://github.com/faying/luya-z-manager/releases)
![Rust](https://img.shields.io/badge/Rust-1.99-orange.svg)
![Target](https://img.shields.io/badge/target-aarch64--musl-lightgrey.svg)
![UI](https://img.shields.io/badge/UI-English%20%C2%B7%20%E4%B8%AD%E6%96%87-green.svg)

[English](README.md) · **中文**

<img src="docs/images/web-home-desktop.png" width="600" alt="管理网页首页：信号结论、带宽、RSRP/SINR、速率、电池">&nbsp;&nbsp;<img src="docs/images/touch-home.png" width="200" alt="触屏首页">

</div>

> [!NOTE]
> **适用硬件：** 中兴 U60 Pro（MU5250）。社区项目，和中兴（ZTE）没有关系，风险自负。

> [!IMPORTANT]
> **我的设备能装吗？** 从零安装需要固件 **B27 及更早**。B28 起中兴删了安装程序进设备用的接口，目前没有验证过的办法。先查自己的版本（[怎么查](docs/GETTING-STARTED.zh-CN.md#2-准备)），**装之前别升级固件**。

## 目录

- [为什么做](#为什么做) · [截图](#截图) · [功能](#功能) · [前提条件](#前提条件) · [快速开始](#快速开始)
- [会改动设备哪些地方、怎么退回](#会改动设备哪些地方怎么退回) · [架构](#架构) · [开发与构建](#开发与构建)
- [版本与更新](#版本与更新) · [项目状态](#项目状态) · [文档](#文档) · [致谢](#致谢) · [许可证](#许可证与免责声明)

## 为什么做

原厂固件把调制解调器知道的大部分信息藏在一个很慢的网页后面，前面板也只显示几格信号。Luya-Z 在设备上放一套本机 API（`luyaz-core`），在它上面做了两个界面：

- **管理网页**：手机和电脑都能用，不只告诉你网慢，还告诉你**为什么**慢；
- **触屏界面**：3.5 寸前面板上按任务分页，常用操作不用开电脑。

全部用你自己编出来的装机包**一条命令**装好，也能退回。

## 截图

截图都是英文界面、假数据。**[功能导览](docs/SCREENSHOTS.zh-CN.md)**：每一页做什么，配截图。

### 管理网页

| 信号 | 已连设备 |
|---|---|
| <img src="docs/images/web-signal.png" width="420" alt="信号页：RSRP/SINR 趋势和服务小区"> | <img src="docs/images/web-clients.png" width="420" alt="已连设备和流量"> |
| **最近 5 分钟** | **锁频** |
| <img src="docs/images/web-trends.png" width="420" alt="5 分钟内的速率、CPU、内存、电池"> | <img src="docs/images/web-bandlock.png" width="420" alt="LTE 和 5G 锁频"> |
| **短信** | **变更记录** |
| <img src="docs/images/web-sms.png" width="420" alt="短信收件箱"> | <img src="docs/images/web-changes.png" width="420" alt="变更记录"> |

### 触屏界面（前面板）

| 首页（深色） | 蜂窝 | Wi-Fi | 锁频 | 系统 |
|---|---|---|---|---|
| <img src="docs/images/touch-home-dark.png" width="150" alt="首页，深色"> | <img src="docs/images/touch-cellular.png" width="150" alt="蜂窝页"> | <img src="docs/images/touch-wifi.png" width="150" alt="Wi-Fi 页"> | <img src="docs/images/touch-bandlock.png" width="150" alt="锁频"> | <img src="docs/images/touch-system.png" width="150" alt="系统页"> |

## 功能

| 方面 | 能做什么 |
|---|---|
| **信号与网络** | RSRP/RSRQ/SINR 加白话结论、载波聚合、服务小区、趋势、锁频锁小区、网络模式、APN |
| **管理网页** | 替代原厂网页。Rust 服务端渲染 + Datastar；手机和电脑、浅色和深色、中文和英文 |
| **触屏界面** | 前面板上按任务分的 5 个标签（首页、蜂窝、Wi-Fi、出口、系统），见 [touch-ui/README.zh-CN.md](touch-ui/README.zh-CN.md) |
| **SIM、eSIM、短信** | SIM/PIN、收发短信、短信转发；配合可插拔 eUICC 卡（5ber、eSTK.me 这类）下载、切换、删除 profile |
| **局域网与 Wi-Fi** | Wi-Fi 和访客 Wi-Fi、LAN/DHCP、DNS、防火墙、QoS、USB 模式、已连设备 |
| **可靠性** | 核心、管理网页、哨兵由 procd 监督；Wi-Fi 兜底看门狗；告警横幅、「健康」页、可选告警短信；每次改设置都记进变更记录；改网络模式后连不上会自动退回 |
| **运维** | `./install.sh doctor` 只读体检，`backup` / `restore` 备份配置，单个组件更新和回退（[DEPLOY.zh-CN.md](DEPLOY.zh-CN.md)） |
| **代理（CHILL）** | 可选的透明代理，基于原生 mihomo：通用模板（一个订阅、精简规则）、经管理网页的面板、触屏「出口」页；用 `./install.sh chill` 单独装（[docs/PROXY.zh-CN.md](docs/PROXY.zh-CN.md)） |
| **装机包** | 新设备一条 `./install.sh` 装好 SSH、管理网页、触屏、eSIM，并关掉固件自动升级 |

代理不在全套安装里，也不带任何订阅和个人规则：订阅地址由你自己写进设备上的 `chill.env`。

## 前提条件

| | |
|---|---|
| 设备 | 中兴 U60 Pro（MU5250） |
| 从零安装的固件 | **B27 及更早**（见顶部说明） |
| 编译用的电脑 | 装好 Docker（macOS 或 Linux）；装机包在容器里编全部程序，本机不用装工具链 |
| 安装 | 一台连着设备 Wi-Fi 的电脑、一根能传数据的 USB-C 线、路由器管理密码 |
| 二进制 | 目前不发布，需要自己编，指南里有完整步骤 |

## 快速开始

**从零开始请看 [docs/GETTING-STARTED.zh-CN.md](docs/GETTING-STARTED.zh-CN.md)**：准备什么、怎么编出装机包、怎么装、怎么检查和退回。

```sh
./onboard/build-kit.sh                 # → onboard/dist/luyaz-kit-YYYYMMDD.tar.gz
# 解开装机包，电脑连上设备的 Wi-Fi，然后：
./install.sh                           # 首次安装；提示时插上 USB-C 线
./install.sh doctor                    # 只读体检
```

默认设备地址是 `192.168.0.1`；不是的话用 `GATEWAY=192.168.x.1 ./install.sh`。装好后在设备的网络里用浏览器打开 `http://192.168.0.1:9090`。

## 会改动设备哪些地方、怎么退回

- 程序和数据都放在 `/data`，固件升级不会清掉。开机自启只是在 `/etc/rc.local` 里加几行，改之前先备份原文件。
- 不停用、不改任何原厂服务；原厂屏幕界面也还在设备上，作为兜底。
- 安装时会**关掉**中兴的固件自动升级，免得一次升级把装好的东西冲掉。
- 每个组件都能单独回退（[DEPLOY.zh-CN.md](DEPLOY.zh-CN.md)）；全部删掉、恢复原厂的步骤见 [onboard/README.zh-CN.md](onboard/README.zh-CN.md#恢复原厂)。

## 架构

```
luyaz-core :9460 ──▶ luyaz-ui（触屏）
       ▲  └──▶ lpac（/data/esim）──▶ eUICC 卡
浏览器 ──▶ luyaz-web :9090（管理网页）──▶ luyaz-core
luyaz-sentinel：屏幕主人 + 看门狗
```

- **luyaz-core**：唯一和调制解调器打交道的程序，把 ubus、AT 命令、sysfs 整理成一套本机 API。
- **luyaz-web**：管理网页，只对局域网。
- **luyaz-ui**：前面板上的触屏界面（Rust + LVGL）。
- **luyaz-sentinel**：让触屏一直在跑，起不来就把屏幕交还原厂界面。

设备上的路径、启动和监督方式见 [docs/ARCHITECTURE.zh-CN.md](docs/ARCHITECTURE.zh-CN.md)。

## 开发与构建

全部在 Docker 里编。最简单的是直接编整个装机包：

```sh
./onboard/build-kit.sh                 # → onboard/dist/luyaz-kit-YYYYMMDD.tar.gz
```

<details>
<summary>单独编某个组件</summary>

```sh
# 核心和管理网页（aarch64 musl；不在 Docker 里编要装 cargo-zigbuild）
cargo zigbuild --profile release-device --target aarch64-unknown-linux-musl -p luyaz-core
cargo zigbuild --release --target aarch64-unknown-linux-musl -p luyaz-web

# 触屏（Docker）
(cd touch-ui && rs/setup-src.sh && rs/build.sh device)

# 管理网页浏览器检查（容器里跑模拟核心）
(cd crates/luyaz-web && npm ci && e2e/run.sh)
```

</details>

更新已装好设备上的单个组件，见 [DEPLOY.zh-CN.md](DEPLOY.zh-CN.md)。

<details>
<summary>目录结构</summary>

```
crates/        luyaz-core（核心）、luyaz-web（管理网页）、luyaz-sentinel（屏幕主人 + 看门狗）、luyaz-* 库
touch-ui/      触屏：rs/（Rust + LVGL）、scripts/（设备脚本）、docs/
onboard/       装机包：build-kit.sh（编）、install.sh（装）、device/（设备端脚本）、test/（沙盒测试）
scripts/       esim/（lpac 工具）、tailscale/（Tailscale 开机启动，见其 README）
docs/          GETTING-STARTED、SCREENSHOTS、ARCHITECTURE、DESIGN.md（界面设计规范）、RELIABILITY.md（可靠性约定）
```

</details>

## 版本与更新

核心、管理网页、哨兵、触屏界面、设备脚本五个部分各有自己的版本号（[语义化版本](https://semver.org/lang/zh-CN/)），改了哪个只升哪个。

- 当前版本：[`versions.json`](versions.json)。每次改了什么：[更新说明](CHANGELOG.zh-CN.md)，以及 [Releases](https://github.com/faying/luya-z-manager/releases)（标签形如 `core-v1.1.0`）。
- 在设备上看：管理网页「设备信息 → 组件版本」，或触屏「系统 → 组件版本」。有新版会显示 `1.0.0 → 1.1.0`。
- 设备每天来这里看一次 `versions.json`（漫游时不看），只显示有没有新版，不会自己安装。

## 项目状态

一个人维护，只在一台设备（国内版固件）上测过。每天在用，但换了固件版本或地区难免有毛病。报问题时附上 `./install.sh doctor` 的输出最好。

## 文档

| 文档 | 内容 |
|---|---|
| [docs/GETTING-STARTED.zh-CN.md](docs/GETTING-STARTED.zh-CN.md) | 快速上手（新手从这里开始） |
| [docs/SCREENSHOTS.zh-CN.md](docs/SCREENSHOTS.zh-CN.md) | 功能导览：管理网页和触屏的每一页 |
| [docs/ARCHITECTURE.zh-CN.md](docs/ARCHITECTURE.zh-CN.md) | 各个程序怎么配合、在设备上放在哪 |
| [onboard/README.zh-CN.md](onboard/README.zh-CN.md) | 装机包说明：装到哪里、日常用法、恢复原厂、常见问题 |
| [DEPLOY.zh-CN.md](DEPLOY.zh-CN.md) | 更新已装好的设备 |
| [docs/RELIABILITY.zh-CN.md](docs/RELIABILITY.zh-CN.md) | 核心、哨兵和脚本之间的文件约定 |
| [docs/DESIGN.zh-CN.md](docs/DESIGN.zh-CN.md) | 管理网页和触屏的设计规范 |
| [touch-ui/README.zh-CN.md](touch-ui/README.zh-CN.md)、[touch-ui/docs/](touch-ui/docs/) | 触屏的构建、硬件、eSIM、测速 |
| [CLAUDE.md](CLAUDE.md) | 给 AI 编程助手看的设备规则（人也值得读一遍「不能做的事」） |

欢迎贡献：见 [CONTRIBUTING.md](CONTRIBUTING.md) 和 [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)。安全问题见 [SECURITY.md](SECURITY.md)。

## 致谢

- [faying](https://github.com/faying)：核心（luyaz-core）、管理网页（luyaz-web）、哨兵、Rust 触屏、装机包、eSIM、Tailscale。
- Wei REN：本项目接手的上一版分支；装机包和 eSIM 探测程序的一部分来自它。
- [33333s](https://github.com/33333s)：核心读写设备的那一层来自 [zwrt-datad](https://github.com/33333s/zwrt-datad)（MIT）；[u60pro-devui](https://github.com/33333s/u60pro-devui) 是触屏界面的起点。
- [Jesther Silvestre](https://github.com/jesther-ai)：本项目从 [open-u60-pro](https://github.com/jesther-ai/open-u60-pro) 起步，其中的 agent 和网页现已全部换掉。

## 许可证与免责声明

[MIT](LICENSE)。来源、删减内容和第三方组件见 [NOTICE](NOTICE) 和 [touch-ui/NOTICE](touch-ui/NOTICE)。

和中兴通讯没有关系，也没有得到其认可。只在你自己的设备上使用。逆向只为互通和学习，仓库里没有中兴的专有源码。
