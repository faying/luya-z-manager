# 功能导览

[English](SCREENSHOTS.md) · **中文**

截图是英文界面、假数据（示例运营商、文档专用 IP 段）；两个界面都有中文版，也都有浅色和深色主题。

## 管理网页（`luyaz-web`，端口 9090）

### 首页

首页先回答“网络正常吗”：一行结论（信号、制式、运营商、载波、格数），然后是聚合带宽、RSRP/SINR、实时上下行速度、温度，以及电量和预计剩余时间。往下是已连设备、今天和本月流量、开机时长、公网 IP、SIM 和注册的运营商、是否漫游、两个 Wi-Fi 频段和正在用的载波。电脑上是侧边栏，手机上换成底部标签（首页 / 图表 / 功能 / 系统）。

<img src="images/web-home-desktop.png" width="720" alt="首页（电脑）">

| | |
|---|---|
| <img src="images/web-home-phone.png" width="260" alt="首页（手机）"> | <img src="images/web-home-phone-dark.png" width="260" alt="首页（手机，深色）"> |
| 手机，浅色 | 手机，深色 |

### 信号和趋势

信号页列出 RSRP、SINR、RSRQ 和格数，每项都配一句好坏判断，还有最近两分钟左右的走势图和服务小区信息（小区 ID、PCI、ARFCN、频段、带宽）。旁边是数据漫游开关，以及去锁频段、信号探测的入口。“最近 5 分钟”画出移动网速、CPU、内存和电池；只有打开这类页面时才采点，不在后台常驻。

<img src="images/web-signal.png" width="720" alt="信号">

<img src="images/web-trends.png" width="720" alt="最近 5 分钟">

### 已连设备

列出每台连着的设备：IP、MAC、连了多久。Wi-Fi 设备还显示频段、Wi-Fi 代数、连接速率、信号好坏、当前网速和连上以来用的流量，一眼看出谁在占带宽。过期的 DHCP 租约收在“显示已过期”里。

<img src="images/web-clients.png" width="720" alt="已连设备">

### 锁频段

先显示当前在用的频段和已锁定的频段，再让你勾选 LTE 和 NR（5G）频段；NR 只选一次，SA 和 NSA 同时生效。应用前会弹框确认，之后设备在约两分钟内核对：读回锁定结果、确认已注册、数据能通，结果直接显示在页面上。「自动选频」下的「清除全部锁定」把频段锁和小区锁一起清掉，交还基带自己选。

<img src="images/web-bandlock.png" width="720" alt="锁频段">

### SIM 和 eSIM

SIM 页显示卡状态、IMSI、ICCID、PIN 锁和 PIN/PUK 剩余次数，可以开启 PIN 锁、改 PIN、网络解锁。U60 没有内置 eSIM，但 SIM 卡槽里插一张可拆卸的 eUICC 卡（eSTK.me、5ber 之类）后，eSIM 页能列出卡上的配置文件，改名、切换、删除，也能用激活码下载新的。切换一般约 40 秒；新配置没接上网时设备会重启。页面还列出待发给运营商的通知和卡片信息（EID、剩余空间）。

<img src="images/web-sim.png" width="720" alt="SIM / PIN">

<img src="images/web-esim.png" width="720" alt="eSIM 配置文件">

### 短信

存在 SIM 卡和设备里的短信都在这里，带存储用量、未读标记、筛选（全部 / SIM / 设备）、批量选择和写短信按钮。运营商发来的漫游流量包开通之类的短信，不用把卡插到手机上也能看。

<img src="images/web-sms.png" width="720" alt="短信">

### Wi-Fi

顶部是两个网络的概况（信道、带宽、已连设备数），下面分频段列出设置：名称、加密方式、信道、带宽、发射功率、是否隐藏。访客 Wi-Fi 有单独的页面。最下面是 Wi-Fi 省电和 NFC 碰一碰连接两个开关，各有一句说明利弊。

<img src="images/web-wifi.png" width="720" alt="Wi-Fi">

### USB 模式

显示 USB-C 口的状态（是否插线、CC、模式、USB 转网口适配器）和充电情况。可以切换 USB 模式（DEBUG / MTP / RNDIS），切换会让 USB 接口重新连接；USB-C 口插着手机时还可以打开快充。

<img src="images/web-usb.png" width="720" alt="USB 模式">

### 体检和告警

只读的设备体检，每分钟重跑一次：开机同步、中兴自动升级是否关闭、各服务是否有人看着且能连上、时钟、Wi-Fi、屏幕界面、未读告警、告警短信是否设好、崩溃日志、`/data` 剩余空间、待机时的流量。同一套检查也能经 SSH 用 `./install.sh doctor` 跑，管理网页挂了也照样能查。页面底部可以展开最近的崩溃日志。

<img src="images/web-health.png" width="720" alt="体检">

### 改动记录

经设备改过的每一项设置都在这里，新的在前：改了什么、什么时候、从哪里改的（网页、屏幕或情景）、有没有生效。点开一行看详情，能撤销的可以直接撤销。只保留最近 50 条。

<img src="images/web-changes.png" width="720" alt="改动记录">

### 设备信息

身份信息（IMEI、IMSI、ICCID、电话号码、运营商、制式）、WAN 和 LAN 地址、DNS，以及系统信息（主机名、内核、开机时长、内存）集中在一页，找客服或运营商要报号码时很方便。

<img src="images/web-device-info.png" width="720" alt="设备信息">

### 登录

管理网页只能从局域网访问，登录只要一个管理员密码。

<img src="images/web-login-phone.png" width="260" alt="登录（手机）">

## 触屏界面（机身屏幕）

### 首页

屏幕首页先给结论（“All good”）、信号强弱、干扰和负载，然后是载波、Wi-Fi 和设备数、今天和本月流量；状态栏显示实时网速和电量。第一次会有一条提示：改网络模式后连不上会自动退回；电量低时底部会出现提醒。

| | | | |
|---|---|---|---|
| <img src="images/touch-home.png" width="180" alt="首页"> | <img src="images/touch-home-dark.png" width="180" alt="首页（深色）"> | <img src="images/touch-notice.png" width="180" alt="首页（带提示）"> | <img src="images/touch-lowbat.png" width="180" alt="电量低"> |
| 首页 | 深色 | 自动退回提示 | 电量低 |

### 蜂窝和 Wi-Fi

| | |
|---|---|
| <img src="images/touch-cellular.png" width="220" alt="蜂窝"> | <img src="images/touch-wifi.png" width="220" alt="Wi-Fi"> |
| **蜂窝：**APN、移动数据和数据漫游开关、网络模式（自动、5G NSA、5G SA、4G+3G、4G）。可能断网或产生漫游费的操作要点两下确认。 | **Wi-Fi：**热点名称、加密方式和密码一眼可见（给客人连很方便），Wi-Fi 总开关、各频段、省电、NFC 开关，再往下是已连设备。 |

### 锁频段和体检

| | |
|---|---|
| <img src="images/touch-bandlock.png" width="220" alt="锁频段"> | <img src="images/touch-alerts.png" width="220" alt="体检和告警"> |
| **锁频段：**直接在机身上选 5G SA、5G NSA 和 LTE 频段再应用。 | **体检和告警：**需要留意的体检项和告警记录（比如 Wi-Fi 看门狗把 Wi-Fi 恢复了），点一行看全文。 |

### 系统

| | |
|---|---|
| <img src="images/touch-system.png" width="220" alt="系统"> | <img src="images/touch-system-charts.png" width="220" alt="系统：图表"> |
| 语言（告警短信也用它）、亮度、熄屏时间、浅色/深色/自动主题，还有电池详情和剩余时间预估、充电器、CPU。 | 内存和开机时长，CPU、内存、电池的 5 分钟图表，以及固件版本。 |

重拍方法见 [scripts/screenshots/](../scripts/screenshots/README.md)（`web.sh`、`touch.sh`）。
