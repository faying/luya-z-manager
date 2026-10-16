# 截图

[English](SCREENSHOTS.md) · **中文**

截图是英文界面、假数据（示例运营商、文档专用 IP 段）。两个界面都有中文版和浅色/深色主题。

## 管理网页（`luyaz-web`，端口 9090）

**首页（桌面）**

<img src="images/web-home-desktop.png" width="720" alt="首页（桌面）">

**首页（手机）**

<img src="images/web-home-phone.png" width="300" alt="首页（手机）">

**首页（手机，深色）**

<img src="images/web-home-phone-dark.png" width="300" alt="首页（手机，深色）">

**信号**

<img src="images/web-signal.png" width="720" alt="信号">

**已连设备**

<img src="images/web-clients.png" width="720" alt="已连设备">

**最近 5 分钟**

<img src="images/web-trends.png" width="720" alt="最近 5 分钟">

**健康**

<img src="images/web-health.png" width="720" alt="健康">

**变更记录**

<img src="images/web-changes.png" width="720" alt="变更记录">

**短信**

<img src="images/web-sms.png" width="720" alt="短信">

**设备信息**

<img src="images/web-device-info.png" width="720" alt="设备信息">

**锁频**

<img src="images/web-bandlock.png" width="720" alt="锁频">

**Wi-Fi**

<img src="images/web-wifi.png" width="720" alt="Wi-Fi">

**SIM / PIN**

<img src="images/web-sim.png" width="720" alt="SIM / PIN">

**eSIM 配置文件**

<img src="images/web-esim.png" width="720" alt="eSIM 配置文件">

**USB 模式**

<img src="images/web-usb.png" width="720" alt="USB 模式">

**登录（手机）**

<img src="images/web-login-phone.png" width="300" alt="登录（手机）">

## 触屏（320×480 前面板）

| | | | |
|---|---|---|---|
| <img src="images/touch-home.png" width="180" alt="首页"> | <img src="images/touch-home-dark.png" width="180" alt="首页（深色）"> | <img src="images/touch-notice.png" width="180" alt="首页带通知"> | <img src="images/touch-lowbat.png" width="180" alt="电量低"> |
| 首页 | 首页（深色） | 首页带通知 | 电量低 |
| <img src="images/touch-cellular.png" width="180" alt="蜂窝"> | <img src="images/touch-wifi.png" width="180" alt="Wi-Fi"> | <img src="images/touch-bandlock.png" width="180" alt="锁频"> | <img src="images/touch-alerts.png" width="180" alt="健康与告警"> |
| 蜂窝 | Wi-Fi | 锁频 | 健康与告警 |
| <img src="images/touch-system.png" width="180" alt="系统"> | <img src="images/touch-system-charts.png" width="180" alt="系统：图表"> |
| 系统 | 系统：图表 |

触屏截图由离屏渲染（`luyaz-ui --render`，见 `touch-ui/rs/devui/src/render/mod.rs`）生成；管理网页截图用 `luyaz-web mock` 加合成的事件流拍摄。
