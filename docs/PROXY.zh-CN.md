# 代理（CHILL）

[English](PROXY.md) · **中文**

CHILL 是本项目的透明代理：设备上跑原生 mihomo（Clash.Meta），面板经管理网页反代（`:9090/chill-ui/`），触屏上有「出口」页。
它**不在全套安装里**，要单独点名装。

仓库里带的是通用配置，不是谁的个人配置：

- `scripts/chill/template.yaml`：一个订阅（`SUB_URL`）、地区组（台湾 / 日本 / 新加坡 / 美国）和一套精简规则：
  局域网和联网检测直连、国内直连、AI、Telegram、YouTube / Netflix / Spotify、Apple，其余走代理。
  自己的订阅来源和规则加在这里；规则集来自 [MetaCubeX/meta-rules-dat](https://github.com/MetaCubeX/meta-rules-dat)。
- `scripts/chill/chill.env.example`：设备端设置。复制到设备的 `/data/chill/chill.env`，填上 `SUB_URL`，`chmod 600`。

## 安装

用装机包：`GATEWAY=<设备地址> ./install.sh chill`；用源码：`scripts/chill/install-chill.sh`（默认干跑，加 `--apply` 才真做）。
装完在设备上 `chill.sh safe-start`，确认家里网络正常，5 分钟内 `chill.sh confirm`；不确认会自己停掉，DNS 和防火墙恢复原样。

`template.yaml` 和 `chill.sh` 的注释里写了这台设备上的特殊做法（不用 `auto-redirect`、DNS 经 dnsmasq 转发、国内路由）。
触屏这边见 [touch-ui/docs/CHILL.md](../touch-ui/docs/CHILL.md)。

## 规则

- 开机自启只走 `/etc/rc.local`（安装脚本会先备份、改完 `sh -n` 检查）；**绝不** `/etc/init.d/<原厂服务> disable`。
- 透明代理接管的是整个家里的网络：一律先 `safe-start`，并保证 SSH 能连上把它停掉。
- 控制口（`:9999`）默认只听本机，面板经管理网页走。
- 订阅地址和密钥只放设备上的 `chill.env`，永远不要提交进任何仓库。
