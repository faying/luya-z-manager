# ZTE U60 Pro（MU5250）管理后台

给中兴 U60 Pro（MU5250）5G 随身 Wi-Fi 用的设备端 REST API（`zte-agent`）、浏览器高级后台和**一键装机包**。

> 社区项目，和中兴（ZTE）没有关系，风险自负。

| 管理网页 | 触屏界面（配套仓库） |
|---|---|
| <img src="docs/images/web-home-desktop.png" width="560" alt="管理网页首页"> | <img src="docs/images/touch-home.png" width="200" alt="触屏首页"> |

## 三个仓库一起用

| 仓库 | 设备上的角色 |
|---|---|
| **[manager](https://github.com/faying/zte-u60-pro-mu5250-manager)**（本仓库） | `zte-agent`（:9090）+ 管理网页 + 装机包 |
| [touch-ui](https://github.com/faying/zte-u60-pro-mu5250-touch-ui) | 前面板触屏界面、屏幕守护进程、进程监督与 Wi-Fi 兜底脚本 |
| [data-service](https://github.com/faying/zte-u60-pro-mu5250-data-service) | `zwrt-datad`：本机数据服务（`127.0.0.1:9460` 的 `/state` + SSE） |

```
zwrt-datad :9460 ──▶ 触屏界面 ──(eSIM 页)──▶ zte-agent :9090 ──▶ lpac ──▶ eUICC 卡
浏览器 ──▶ zte-agent :9090（API + 管理网页）
```

装机包在本仓库的 `onboard/` 里打，打包时从另外两个仓库取触屏程序和数据服务。

## 功能

- **zte-agent**：一个 Rust 程序（端口 9090，只对局域网），把 ubus、AT 命令、sysfs 整理成 REST API：
  设备/电池/温度、信号与载波、锁频锁小区、SIM/短信、APN、DNS/DHCP/防火墙、Wi-Fi、USB 模式、测速、定时任务等。
- **管理网页**（`web/`，Next.js 静态导出）：替代原厂网页，手机和电脑都能用，浅色/深色，中英文。
- **eSIM**：配合可插拔 eUICC 卡（5ber、eSTK.me 这类）下载、切换、删除 profile。
- **代理**：不带。想在设备上跑透明代理，按 [docs/PROXY.md](docs/PROXY.md) 从官方来源自己编内核、配面板。
- **可靠性**：zte-agent、数据服务、看门狗由 procd 监督；Wi-Fi 兜底看门狗；告警横幅、「健康」页、可选告警短信；
  `./install.sh doctor` 只读体检，`backup` / `restore` 备份配置。约定见 [docs/RELIABILITY.md](docs/RELIABILITY.md)。
- **装机包**（`onboard/`）：新设备一条 `./install.sh` 装好 SSH、后台、触屏、eSIM，并关掉固件自动升级。

## 快速开始

**从零开始请看 [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md)**：准备什么、怎么编出装机包、怎么装、怎么检查和退回。

只适用于固件 **B27 及更早**（B28 起中兴删了开 ADB 的接口）。**装之前别升级固件。**
目前不发布编译好的二进制，需要自己编，指南里有完整步骤。

## 开发与构建

```sh
# zte-agent（aarch64 musl；没有 cargo-zigbuild 时 onboard/build-kit.sh 会改用 Docker）
cargo zigbuild --release --target aarch64-unknown-linux-musl -p zte-agent

# 管理网页 → web/out/
cd web && npm ci && npm run build

# 不连设备开发网页：假数据 agent + 开发服务器，见 web/README.md
node scripts/mock-agent/server.ts & npm run dev

# 装机包 → onboard/dist/u60-kit-YYYYMMDD.tar.gz
./onboard/build-kit.sh
```

已经装好的设备更新单个组件，见 [DEPLOY.md](DEPLOY.md)。

## 目录

```
zte-agent/     设备端 REST API（Rust）
web/           管理网页（Next.js），由 agent 在 :9090 提供
onboard/       装机包：build-kit.sh（打包）、install.sh（装机）、device/（设备端脚本）、test/（沙盒测试）
scripts/       esim/（lpac 工具包）、tailscale/、homemode.sh、monitor.sh 等
docs/          GETTING-STARTED.md、DESIGN.md（界面设计规范）、RELIABILITY.md（可靠性约定）
```

根目录的 `setup.sh`、`install.sh`、`deploy.sh` 来自上游 open-u60-pro，用密码 SSH 和旧的启动方式；新设备请用 `onboard/` 的装机包。

## 文档

- [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md)：快速上手（新手从这里开始）
- [onboard/README.md](onboard/README.md)：装机包说明（装到哪里、日常用法、恢复原厂、常见问题）
- [DEPLOY.md](DEPLOY.md)：更新已装好的设备
- [docs/RELIABILITY.md](docs/RELIABILITY.md)：zte-agent 与看门狗之间的文件约定
- [docs/DESIGN.md](docs/DESIGN.md)：管理网页和触屏的设计规范
- [web/README.md](web/README.md)：网页开发
- [CLAUDE.md](CLAUDE.md)：给 AI 编程助手看的设备规则（人也值得读一遍「不能做的事」）

## 致谢

- [Jesther Silvestre](https://github.com/jesther-ai)：原始项目 [open-u60-pro](https://github.com/jesther-ai/open-u60-pro)（agent、第一版网页）。
- Wei REN：装机包、eSIM、Tailscale、回家模式、网页改版。
- [33333s](https://github.com/33333s)：感谢 [u60pro-devui](https://github.com/33333s/u60pro-devui)（触屏界面的起点）和 [zwrt-datad](https://github.com/33333s/zwrt-datad)（本机数据服务）这两个参考仓库。

## 许可证与免责声明

[MIT](LICENSE)。来源、删减内容和第三方组件见 [NOTICE](NOTICE)。

和中兴通讯没有关系，也没有得到其认可。只在你自己的设备上使用。逆向只为互通和学习，仓库里没有中兴的专有源码。
