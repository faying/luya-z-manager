# 代理（CHILL）

[English](PROXY.md) · **中文**

CHILL 是本项目的透明代理。设备上跑原生 Mihomo（Clash.Meta），在管理网页「服务 → CHILL」管理配置和模式。触屏首页轻点 Mihomo 状态即可打开 CHILL 详情。
它**不在全套安装里**，要单独点名装。

仓库里带的是通用配置，不是谁的个人配置：

- `scripts/chill/template.yaml`：一个订阅（`SUB_URL`）、地区组（台湾 / 日本 / 新加坡 / 美国）和一套精简规则：
  局域网和联网检测直连、国内直连、AI、Telegram、YouTube / Netflix / Spotify、Apple，其余走代理。
  规则集来自 [MetaCubeX/meta-rules-dat](https://github.com/MetaCubeX/meta-rules-dat)。节点订阅档案使用这个模板。
- `scripts/chill/chill.env.example`：设备端高级设置和旧版单订阅入口。

## 安装

用装机包：`GATEWAY=<设备地址> ./install.sh chill`；用源码：`scripts/chill/install-chill.sh`（默认干跑，加 `--apply` 才真做）。
安装后在管理网页导入完整配置链接、节点订阅链接或本地 YAML。可保留多份档案并切换，运行中的切换失败会恢复上一份可用档案。首启用 `sh /data/chill/chill.sh safe-start`，确认网络正常后在 5 分钟内执行 `sh /data/chill/chill.sh confirm`。超时会停用 CHILL 并还原 DNS。

`template.yaml` 和 `chill.sh` 的注释里写了这台设备上的特殊做法（不用 `auto-redirect`、DNS 经 dnsmasq 转发、国内路由）。
触屏这边见 [touch-ui/docs/CHILL.md](../touch-ui/docs/CHILL.md)。

## 规则

- 开机自启只走 `/etc/rc.local`（安装脚本会先备份、改完 `sh -n` 检查）；**绝不** `/etc/init.d/<原厂服务> disable`。
- 透明代理接管的是整个家里的网络：一律先 `safe-start`，并保证 SSH 能连上把它停掉。
- 控制口（`:9999`）默认只听本机，面板经管理网页走。
- 「规则」按档案分流，「全局」使用 Mihomo 的 `GLOBAL` 组，「直连」保留网关并让流量直出。关闭 CHILL 会还原设备 DNS 和路由。zashboard 用于详细的节点和连接管理。
- 完整配置保留原本的规则和 DNS 策略。设备只生成自己的网关运行副本，各档案的源文件和缓存独立保存在 `/data/chill/profiles/`。订阅链接和密钥仅存在设备端。
