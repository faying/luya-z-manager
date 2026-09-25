# 代理（自己动手）

这个仓库和装机包**不带**任何代理功能：没有代理内核、没有面板、没有规则集，后台和触屏上也没有代理页面。
想在 U60 Pro（MU5250）上跑透明代理，请直接按官方来源自己搭：

- 内核：mihomo（Clash.Meta）
  - 源码与发布包：<https://github.com/MetaCubeX/mihomo>（releases 里选 `linux-arm64`）
  - 文档：<https://wiki.metacubex.one/>
- 面板（任选一个官方风格的网页面板，接 mihomo 的 `external-controller`）：
  - metacubexd：<https://github.com/MetaCubeX/metacubexd>
  - zashboard：<https://github.com/Zephyruso/zashboard>

在这台设备上自己搭时要守的规矩（和 [GETTING-STARTED.md](GETTING-STARTED.md) 第 8 节一致）：

- 设备是 aarch64、musl libc 的 OpenWrt 23.05；程序和配置放 `/data`，`/tmp` 是内存盘。
- 开机自启只走 `/etc/rc.local`，改之前备份，改完 `sh -n` 检查；**不要** `/etc/init.d/<原厂服务> disable`。
- 透明代理会接管全屋网络。第一次启动前准备好退路（能通过 SSH 停掉它、把 DNS / 防火墙改回原样），
  控制接口（`external-controller`）只监听本机或局域网，不要暴露到蜂窝网。
- 订阅地址和密钥只放设备上、只给 root 读，不要提交进任何仓库。
