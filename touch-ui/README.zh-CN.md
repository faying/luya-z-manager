# Luya-Z 触屏界面

[English](README.md) · **中文**

**Luya-Z** 的一部分：给 5G 随身 Wi-Fi 前面板 3.5 寸触摸屏用的开源替代界面：Rust + LVGL + FreeType 直接画到 DRM/KMS，编成一个静态 aarch64 程序（`luyaz-ui`），另带设备端脚本。
它在 [Luya-Z 管理后台](../README.zh-CN.md) 仓库的 `touch-ui/` 目录里，和那边一起编、一起装。

> **适用硬件：** 中兴 U60 Pro（MU5250）。社区项目，和中兴（ZTE）没有关系，风险自负。

| 首页 | 蜂窝 | 出口（深色） | 系统 |
|---|---|---|---|
| <img src="docs/images/touch-home.png" width="180" alt="首页"> | <img src="docs/images/touch-cellular.png" width="180" alt="蜂窝页"> | <img src="docs/images/touch-exit-dark.png" width="180" alt="出口页，深色"> | <img src="docs/images/touch-system.png" width="180" alt="系统页"> |

（截图由离屏渲染测试用假数据生成。）

屏幕文字目前只有中文，计划加英文选项。

## 怎么配合

```
luyaz-core :9460 ──▶ luyaz-ui（触屏）
       ▲  └──▶ lpac（/data/esim）──▶ eUICC 卡
浏览器 ──▶ luyaz-web :9090（管理网页）──▶ luyaz-core
luyaz-sentinel：屏幕主人 + 看门狗
```

触屏界面从核心 `luyaz-core` 读实时数据（`127.0.0.1:9460` 的 `/v2/state` + `/v2/events`；首页卡片和状态栏的结论由 `/v2/screen` 直接给出），eSIM、APN 等操作交给核心的控制接口。
屏幕主人是 `luyaz-sentinel`（manager 的 `crates/luyaz-sentinel`）。manager 的装机包把它们一起装到设备上。

## 功能

- 底部 5 个标签，按「我想做什么」分：**首页 · 蜂窝 · Wi-Fi · 出口 · 系统**。首页大字是一句结论（网络正常 / 信号偏弱 / 漫游中）。
- 蜂窝：载波、信号、网络模式、APN、SIM 与 eSIM 切换（经核心调 lpac）。
- 出口：出口 IP 与归属地；Tailscale。不带代理功能，要的话见 manager 的 [docs/PROXY.zh-CN.md](../docs/PROXY.zh-CN.md)。
- 系统：亮度、息屏、浅色 / 深色 / 自动，电池与负载、告警详情。
- USB-C 口插上手机：和原厂屏幕一样问用来做什么——充电 + 上网（手机得到 USB 网卡）、18W 快速充电宝、网口配件——可以记住选择（系统 › 插入手机等设备时）。
- 每次点击都有即时反馈；要确认的操作按两次。
- 屏幕主人（`luyaz-sentinel`）：拉起或接管界面，崩了自动拉起并记告警；连续两次起不来就交还原厂界面（避免被固件升级成整机重启），长按屏幕右下角 3 秒回来。
- `scripts/`：procd 启动脚本（`luyaz-core.init`、`luyaz-web.init`、`luyaz-sentinel.init`）、只读体检（`doctor.sh`，转给 `luyaz-core doctor` 的薄壳）、告警工具、配置备份（`config-backup.sh`）、ship 执行器和开机恢复（见 [docs/SHIP.md](docs/SHIP.md)）等，约定见 manager 的 [docs/RELIABILITY.md](../docs/RELIABILITY.md)。

## 快速开始

装到设备上请看 manager 的 **[快速上手](../docs/GETTING-STARTED.zh-CN.md)**：触屏界面由那边的装机包带上设备，已装好的设备用 `./install.sh devui` 更新。

## 构建

用 Docker（共用的 Rust 构建镜像，本机不用装工具链）：

```sh
rs/setup-src.sh              # 一次：把打过补丁的 LVGL v9.5.0 和 FreeType 2.13.3 源码放进 docker 卷
rs/build.sh device out/luyaz-ui   # 静态 aarch64-musl 的 luyaz-ui（cargo zigbuild）
```

`rs/` 里的 crate 是 manager Cargo workspace 的成员。manager 的 `onboard/build-kit.sh` 没给现成程序时会自己编触屏。
触屏字体（Nunito、中文兜底）用 `scripts/fonts/build-nunito.sh`、`scripts/fonts/build-cjk-fallback.sh` 生成（公开下载按 sha256 固定），装机包会自动调。

测试：`scripts/test/docker.sh`（设备端脚本，busybox 容器里全部打桩）、`rs/build.sh test`（Rust 单元测试）、
`rs/smoke.sh`（对着假核心离屏跑完整程序）。开发细节见 [docs/DEVELOPMENT.zh-CN.md](docs/DEVELOPMENT.zh-CN.md)。

> **新构建不要直接覆盖设备上的正式程序。** 一启动就崩的版本会被固件升级成整机重启循环；屏幕上几分钟没有界面，固件也会整机重启。
> 先用别的文件名试跑，步骤见 [docs/DEVELOPMENT.zh-CN.md](docs/DEVELOPMENT.zh-CN.md#在真机上试新版本)。

## 文档

- [docs/DEVELOPMENT.zh-CN.md](docs/DEVELOPMENT.zh-CN.md)：代码结构、构建、测试、真机试跑
- [docs/HARDWARE.zh-CN.md](docs/HARDWARE.zh-CN.md)：屏幕、触摸、按键、背光等硬件接口
- [docs/ESIM.zh-CN.md](docs/ESIM.zh-CN.md)：eSIM 切换页与核心接口
- [docs/SPEEDTEST.zh-CN.md](docs/SPEEDTEST.zh-CN.md)：可选测速后端
- 设计规范：manager 的 [docs/DESIGN.md](../docs/DESIGN.md) §4

## 致谢

- [33333s](https://github.com/33333s)：感谢作为本触屏界面和本机数据服务起点的两个参考仓库。
- Wei REN：LVGL 与 Rust 重写、屏幕主人、eSIM、Tailscale、可靠性脚本。
- [Jesther Silvestre](https://github.com/jesther-ai)：[open-u60-pro](https://github.com/jesther-ai/open-u60-pro)，manager 的起点。

## 许可证与免责声明

[MIT](LICENSE)。LVGL（MIT）、FreeType（FTL / GPLv2）按各自许可证使用，详见 [NOTICE](NOTICE)。
仓库不含任何中兴字体或厂商二进制。和中兴通讯没有关系，只在你自己的设备上使用。
