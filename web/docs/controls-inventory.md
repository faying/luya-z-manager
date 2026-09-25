# 管理网页控件与数据清单（改版前基线）

## 说明

**用途**：管理网页（`source/manager/web`）要整体改版。动任何页面之前，先把 43 个路由上的每个控件、每项数据列全，改版后对着这份清单逐项核对，保证一样不丢。它也是后面 e2e 测试（按可访问名称找控件）和「安全白名单」（哪些接口可以放心自动调用）的输入。

**怎么来的**：2026-09-24 从代码逐页读出来，没有连设备，也没有跑页面。
- 页面：`web/src/app/(panel)/**/page.tsx` 共 43 个（`find web/src/app -name page.tsx | wc -l` = 43），外加它们用到的 `web/src/components/admin/*`（AdminShell、AlertBanner、StatusStrip、LangToggle、Button/Toggle/Input、Help）。
- 中文文案：`web/src/lib/i18n/zh.ts` + `zh-pages.ts`。表里写中文；没有中文翻译的写英文默认值并标「(无中文)」。
- 接口行为：`zte-agent/src/server.rs` 的路由表，以及各 handler（`handlers.rs`、`router.rs`、`cell.rs`、`modem_ext.rs`、`wifi.rs`、`wifi_radio.rs`、`homemode.rs`、`esim.rs`、`scenario.rs`、`sms.rs`、`sms_forward.rs`、`alerts.rs`、`scheduler.rs`、`services.rs`、`public.rs`、`device_ext.rs`、`usb.rs`、`sim.rs`、`telephony.rs`、`speedtest.rs`、`health.rs`、`at_terminal.rs`、`qos.rs`、`network_ext.rs`、`system.rs`）。
- 路径里的 `(panel)` 是 Next.js 路由组，不出现在网址里；下文 URL 一律不带它。

### 表格各列怎么读

**数据项**：显示内容｜接口（页面里写的原样路径）｜字段（页面读的 JSON 路径，`data.` 已由 `apiFetch` 拆掉，直接从 `data` 里面写起）｜刷新间隔（SWR `refreshInterval` 毫秒；`setInterval` 轮询另注；只在挂载或操作后拉一次写「无」）｜假成功风险。

**控件**：控件（屏幕上的中文字）｜可访问名称（e2e 用 `getByRole(…, { name })` 能找到的名字；代码里没有能用的名字时给出建议，标「建议」）｜接口 + 方法 + 请求体要点｜步骤（一次点击发出多个写请求时，按顺序列出）｜读回（哪个 GET 的哪个字段能证明改动生效；本来就没有状态可读的写「无读回」）｜现有确认（`window.confirm` / `confirm` 的原文，没有就写「无」）｜档位。

**假成功风险**的判法（看 agent handler，不看页面）：
- **是**：handler 把下游失败吞掉仍回 `ok:true`（`let _ =`、`unwrap_or(默认值)`、写文件失败不报、先回成功再在后台做事等），后面附 `文件:行`。
- **否·透传**：handler 只看 `ubus` 进程的退出码，退出码为 0 就把固件返回体原样放进 `data` 回 `ok:true`，**不检查返回体里的 `result` 失败码**。固件确实会这样：`sms.rs:64-67` 注释写明 `zwrt_wms_delete_sms` 返回 `{"result":3}` 却什么都没删。所以「否·透传」不等于可靠。表里会顺带写页面自己有没有检查 `data.result`；没写就是页面也不检查。
- **否**：handler 自己校验结果，失败时回 4xx/5xx。
- **不适用（不经 agent）**：请求不走 zte-agent。
- **未查**：没追到 handler 的具体行为。

**档位规则**（本次改版定的）：
- **第一档**（封闭清单，只有这些）：DNS/DoH 上游、QoS 参数、短信标为已读、告警标为已读、界面设置（深浅色、语言）。
- 其余一切**至少第二档**。
- 现在每个 `window.confirm()` 的地方都是**第三档**。另外这些也是第三档：选网方式、锁频/锁小区（含恢复默认）、飞行模式、重启、定时重启开启、恢复出厂、eSIM 切换/启用/删除、APN 修改/切换/删除、USB 模式、STC 白名单重置、结束进程/一键结束、开 ADB、AT 危险命令、删除短信、删除短信转发规则、清空转发日志。
- **二·远程三**（本地第二档，远程访问时第三档）：Wi-Fi 开关、Wi-Fi 名称/密码、LAN/DHCP 地址、关 Tailscale、防火墙规则增删改、VPN 直通开关。
- **始终第二档**：情景固定/取消固定。
- 纯页面内的操作（展开/收起、筛选、切标签、只改表单草稿、复制到剪贴板、跳转链接）不写设备，档位写「—（本地）」。

**改版后的标记（2026-09-24，E9 自动核对）**：`tests/e2e/inventory.spec.ts` 逐页按「可访问名称」一列去页面上找控件（`getByRole` 按角色 + 名称精确匹配，表单控件也认 `<label>`；1440 和 390 两种宽度任一处找到就算）。改版后名字变了、控件还在的行，**原文不删**，在可访问名称一格末尾加标记：
- 〔现名：role「新名字」（说明）〕——改名；核对只用这里的名字，原来的名字留作旧名。
- 〔交互后：说明；步骤〕——要先操作才出现（标签页、表单、对话框、选中、扫描结果、读取失败……）。写了步骤（`点 button「…」`、`填 textbox「…」=值`、`场景 down`、`载入后场景 down`）的，测试会照做再找；没写步骤的只列出来，不核对。
- 〔缺失：说明〕——页面上确实没有了，汇总里单独列出。
名字里的 `{…}` 匹配任意文字，单独的 `N` 匹配数字。汇总表在跑完后打印，并写到 `web/test-results/inventory-summary.md`。

**不计入的**：`Help` 组件的「?」说明气泡（只显示解释文字，每页都有，不逐个列）；侧栏、底部标签栏的普通导航链接。

---

## 全局（每个登录后页面都有）

来源：`components/admin/AdminShell.tsx`、`AlertBanner.tsx`、`StatusStrip.tsx`、`LangToggle.tsx`；`app/(panel)/layout.tsx`（`/login` 不套 AdminShell）。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 顶栏状态条·移动网络图标（已连接/制式/信号弱/WAN 断开） | `/api/public/status` | `network.connected`、`network.type`、`network.rsrp`（≤ −110 算信号弱） | 10000 | 是：public.rs:31、43 各 ubus 调用失败时 `unwrap_or(json!({}))`，照样回 ok，连接状态会显示成「断开」而不是「读不到」 |
| 顶栏状态条·Wi-Fi 图标 | `/api/public/status` | `wifi.on` | 10000 | 是：public.rs:53 `zwrt_wlan report` 失败时当作关 |
| 顶栏状态条·Tailscale 图标 | `/api/public/status` | `services.tailscale.installed`、`services.tailscale.running` | 10000 | 否（本地 `pidof`/文件存在判断） |
| 顶栏状态条·短信图标 + 未读角标 | `/api/public/status` | `sms.unread` | 10000 | 是：public.rs:107 容量查询失败时未读数为 0 |
| 顶栏页面标题 | 无（按当前路由查 NAV 的 `tKey`） | — | — | — |
| 顶栏 agent 地址（`host:port`，小屏隐藏） | 无（`getApiBase()`，localStorage `u60.agent_url` 或同源） | — | — | — |
| 告警横幅：「N 条新告警」+ 最新一条种类 + 正文 + 短信状态提示（「短信告警未配置。」/「上一条告警短信发送失败。」） | `/api/alerts` | `unread`、`events[].unread/kind/text`、`sms.configured`、`sms.recent[0].result`；在 `/alerts` 页不显示 | 30000 | 否（alerts.rs:262 读本地文件） |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 侧栏分组标题（概览/移动网络/…，点了展开收起） | 分组名，如 button「概览」 〔缺失：新导航里侧栏分组是固定展开的静态小标题（「网络与频段」等），不再能点着展开/收起；各组入口改由「功能」「系统」等枢纽页列出〕 | 无 | — | — | 无 | —（本地） |
| 侧栏关闭「×」（小屏） | button「Close menu」（英文写死；建议改为「关闭菜单」） 〔缺失：小屏不再有侧栏抽屉（改为底部 4 个标签 + 枢纽页），所以也没有「关闭菜单」按钮〕 | 无 | — | — | 无 | —（本地） |
| 底部标签栏「更多」（小屏，打开侧栏抽屉） | button「更多」 〔现名：link「功能」（原 button「更多」打开侧栏抽屉；现在底部标签栏是 首页 / 图表 / 功能 / 系统，「功能」「系统」枢纽页列出全部页面）〕 | 无 | — | — | 无 | —（本地） |
| 语言切换「中 / EN」 | group「Language」内 button「中」、button「EN」（`aria-pressed`；建议 group 名改「语言」） 〔现名：radiogroup「语言」内 radio「中」「EN」（按了就切换的单选组；设置页里也有同名的一组）〕 | 无（`setLang`，写 localStorage `u60_lang`） | — | `<html lang>` / localStorage `u60_lang` | 无 | 一（界面设置·语言） |
| 退出登录 | button「退出登录」（小屏只剩图标，`title`=「退出登录」） | 无（清 localStorage `u60.token`，不调接口） | — | 跳到 `/login` | 无 | —（本地） |
| 告警横幅「知道了」 | button「知道了」 | POST `/api/alerts/read` `{seq: events[0].seq}` | 1 个写请求；之后重拉 `/api/alerts`，并让 `/api/public/status` 失效重拉 | `/api/alerts` `unread` = 0 | 无 | 一（告警标为已读） |
| 告警横幅「查看」 | link「查看」→ `/alerts` | 无（导航） | — | — | 无 | —（本地） |

说明：
- 深浅色切换：**当前代码里没有**（grep `theme`/`dark` 在 web 端只找到 CSS 变量，没有切换控件）。第一档里的「界面设置·深浅色」目前没有对应控件，改版若新增要记为新功能。
- 401 处理：任一请求 401 时 `apiFetch` 清掉 token、派发 `u60:unauthorized`，AuthGate 跳 `/login?next=…`。

---

## /login 登录

文件：`app/(panel)/login/page.tsx`。不套 AdminShell，没有全局状态条和告警横幅。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 网络·连接（已连接/离线） | `/api/public/status`（`noAuth`，免登录） | `network.connected` | `setInterval` 10000（非 SWR） | 是：public.rs:43 `get_wwaniface` 失败时当作未连接 |
| 网络·信号（`bar/5` + RSRP dBm） | `/api/public/status` | `network.bar`（<0 显示「—」）、`network.rsrp` | `setInterval` 10000 | 是：public.rs:31 读不到时 bar=−1、rsrp=0 |
| 网络·网络制式 + 运营商 | `/api/public/status` | `network.type`、`network.operator` | `setInterval` 10000 | 是：public.rs:31 |
| 设备·Wi-Fi（SSID 或「关」） | `/api/public/status` | `wifi.on`、`wifi.ssid` | `setInterval` 10000 | 是：public.rs:53 |
| 设备·电池（% + 充电中） | `/api/public/status` | `battery.percent`（<0 显示「—」）、`battery.charging` | `setInterval` 10000 | 否（sysfs，读不到为 −1） |
| 服务·Tailscale（节点名/已停止/未安装） | `/api/public/status` | `services.tailscale.running/installed/node` | `setInterval` 10000 | 否 |
| 服务·回家模式（未安装/已暂停/已启用 · Wi-Fi 关/已启用 · Wi-Fi 开） | `/api/public/status` | `services.home_mode.present/enabled/mode` | `setInterval` 10000 | 否（读本地文件） |
| 服务·短信（N 条未读 / 无未读） | `/api/public/status` | `sms.unread` | `setInterval` 10000 | 是：public.rs:107 |
| 登录失败提示 | `/api/auth/login` 的错误 | `error`（如 `invalid password`） | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 密码输入框（标签「Agent 密码」） | 无（`<label>` 没有和输入框关联）；建议 textbox「Agent 密码」 | 无（草稿） | — | — | 无 | —（本地） |
| 「高级」/「隐藏」 | button「高级」/「隐藏」 | 无（展开 Agent 地址） | — | — | 无 | —（本地） |
| Agent 地址输入框（标签「Agent 地址」，占位 `http://192.168.0.1:9090`） | 无；建议 textbox「Agent 地址」 〔交互后：点 button「高级」〕 | 无；提交时 `setApiBase()` 写 localStorage `u60.agent_url` | — | — | 无 | —（本地） |
| 「登录」 | button「登录」 | POST `/api/auth/login` `{password}`（免 token） | 1；成功后 token 写 localStorage `u60.token`，跳 `?next=` 或 `/` | 进入后台（后续请求不再 401） | 无 | 二（登录本身不在第一档清单） |

---

## / 仪表盘

文件：`app/(panel)/page.tsx`。

**关于「今日 / 本月流量」**：当前首页**不显示**任何流量用量。页面只调 `/api/device/system`、`/api/network/signal`、`/api/device/battery-info`、`/api/network/speed`、`/api/wifi/status`、`/api/public/status`，没有调 `/api/data-usage`，也没有调 `/api/network/traffic`。整个 `web/src` 里都没有页面调 `/api/data-usage`。agent 里这个接口是有的（`handlers.rs:136`，读 `zwrt_data_commit.wwancid1dst` 的 `day_* / month_* / total_*`，返回 `data.day / data.month / data.total` 的 `tx_bytes / rx_bytes / time_secs / tx_packets / rx_packets`；读不到的项为 `null`）。改版如果要在首页放今日/本月流量，应接 `/api/data-usage`，不要用 `/api/network/traffic`（那是网卡开机以来的累计计数）。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 信号柱（0–5 格，按 RSRP 分档） | `/api/network/signal` | NR 时 `nr5g_rsrp`，否则 `lte_rsrp`（`network_type` 含 SA 或 `nr5g_rsrp≠0` 算 NR） | 2000 | 否·透传（handlers.rs:109） |
| 链路评价（很好/良好/一般/差/无信号）+ 通俗说明 | `/api/network/signal` | 同上 RSRP | 2000 | 否·透传 |
| 「实时 / 重连中」小点 | `/api/network/signal` | SWR `error` 是否存在 | 2000 | — |
| 摘要行：RSRP dBm · 运营商 · 制式 · Band | `/api/network/signal` | `rsrp`、`network_provider_fullname`‖`network_provider`、`network_type`、`nr5g_action_band` | 2000 | 否·透传 |
| SINR | `/api/network/signal` | NR 时 `nr5g_snr`，否则 `lte_snr` | 2000 | 否·透传 |
| RSRQ | `/api/network/signal` | NR 时 `nr5g_rsrq`，否则 `lte_rsrq` | 2000 | 否·透传 |
| Bars `/5` | `/api/network/signal` | `signalbar` | 2000 | 否·透传 |
| 下载速率 | `/api/network/speed` | `rx_speed`（字节/秒，页面 ×8 显示 bit/s） | 1000 | 否（agent 内部计数，handlers.rs:123） |
| 上传速率 | `/api/network/speed` | `tx_speed` | 1000 | 否 |
| 电池 % + 充电中/已接电源/使用电池 | `/api/device/battery-info` | `battery_capacity`、`battery_online`、`battery_time_to_full`（≥0 且 online=1 算充电） | 5000 | 否·透传（network_ext.rs:55） |
| 已连设备数 | `/api/wifi/status` | `clients_total` | 10000 | 是：wifi.rs:55-67 `iw station dump` 失败时计 0 |
| Wi-Fi 开/关 + 5G 加密方式 | `/api/wifi/status` | `wifi_onoff`（"1" 为开）、`encryption_5g` | 10000 | 是：wifi.rs:84-103 读不到时 `wifi_onoff` 兜底为 "1"（显示为开） |
| 运行时间 | `/api/device/system` | `uptime` | 5000 | 否·透传（device_ext.rs:22） |
| 服务·Tailscale（节点名/已停止/未安装） | `/api/public/status` | `services.tailscale.running/node/installed` | 10000 | 否 |
| 服务·回家模式 | `/api/public/status` | `services.home_mode.present/enabled/mode` | 10000 | 否 |
| 服务·短信（N 条未读/无，可点） | `/api/public/status` | `sms.unread` | 10000 | 是：public.rs:107 |
| 小区卡：Cell ID / PCI / EARFCN / Band / Bandwidth / Net Select | `/api/network/signal` | `nr5g_cell_id`、`nr5g_pci`、`nr5g_action_channel`、`nr5g_action_band`、`nr5g_bandwidth`（加 MHz）、`net_select_mode` | 2000 | 否·透传 |
| Wi-Fi 卡：状态 / 2.4G SSID / 2.4G 信道 / 5G SSID / 5G 信道 / 5G 频宽 / 已连设备 | `/api/wifi/status` | `wifi_onoff`、`ssid_2g`、`actual_channel_2g`、`ssid_5g`、`actual_channel_5g`、`actual_bw_5g`、`clients_total` | 10000 | 是：wifi.rs:12-18 `uci get` 失败一律空串 |

说明：接口里有但首页没显示的：`actual_bw_2g`（接口声明了但没渲染）、`lte_rsrp/lte_rsrq` 之外的 LTE 小区信息。

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 服务卡「短信」行（整行链接到 `/sms`） | link「N 条未读」/「无」 〔现名：link「N 条未读短信」（首页头部的短信角标链接；原服务卡「短信」行）〕 | 无（导航） | — | — | 无 | —（本地） |

首页没有任何写操作。

---

## /alerts 告警

文件：`app/(panel)/alerts/page.tsx`。本页不显示全局告警横幅。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 记录列表：时间（MM-DD HH:mm，或「开机后 N 分钟」） | `/api/alerts` | `events[].time`（null 时用 `events[].uptime`） | 30000 | 否（alerts.rs:262 读本地文件） |
| 记录列表：未读标记 ▲ + 种类文字 | `/api/alerts` | `events[].unread`、`events[].kind`（`lib/alerts.ts` `kindLabel` 翻成中文） | 30000 | 否 |
| 记录列表：正文 | `/api/alerts` | `events[].text` | 30000 | 否 |
| 空状态「没有出过问题…」 | `/api/alerts` | `events.length === 0` | 30000 | 否 |
| 「设备时钟还没校准…」提示 | `/api/alerts` | `clock_set === false` | 30000 | 否 |
| 短信告警状态（已开启：发到 {号码} / 未配置） | `/api/alerts` | `sms.configured`、`sms.number` | 30000 | 否 |
| 手机号输入框初值 | `/api/alerts` | `sms.number` | 30000 | 否 |
| 「在国外也发送」开关状态 | `/api/alerts` | `sms.abroad_allowed` | 30000 | 否 |
| 「设备当前处于国外情景，短信告警暂不发送。」 | `/api/alerts` | `sms.abroad_now && !sms.abroad_allowed` | 30000 | 否 |
| 最近的短信 · 24 小时内已发 N 条 | `/api/alerts` | `sms.sent_24h` | 30000 | 否 |
| 最近短信列表：时间 / 种类 / 结果（已发送/失败/超限未发/在国外未发/未设号码未发） | `/api/alerts` | `sms.recent[].time`（<2024-01-01 视为未校准）、`.kind`、`.result` | 30000 | 否 |
| 加载错误横幅 | `/api/alerts` | SWR `error` | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「全部标为已读」（有未读时才出现） | button「全部标为已读」 | POST `/api/alerts/read` `{seq: events[0].seq}` | 1；之后重拉 `/api/alerts`、`/api/public/status` | `/api/alerts` `unread` = 0 | 无 | 一（告警标为已读） |
| 手机号输入框 | textbox「手机号」（`<label>` 包住输入框，有名称） | 无（草稿） | — | — | 无 | —（本地） |
| 「保存」（号码不变时禁用；清空后保存 = 关闭短信告警） | button「保存」 | PUT `/api/alerts/sms` `{number}`（空串=关闭） | 1 | `/api/alerts` `sms.number`、`sms.configured` | 无 | 二 |
| 「在国外也发送（可能产生漫游费）」开关 | switch「在国外也发送（可能产生漫游费）」 | PUT `/api/alerts/sms` `{abroad: bool}` | 1 | `/api/alerts` `sms.abroad_allowed` | 无 | 二 |

---

## /bandlock 频段锁定

文件：`app/(panel)/bandlock/page.tsx`。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 「当前 NR 频段：」 | `/api/network/signal` | `nr5g_band`（**未确认**：首页读的是 `nr5g_action_band`，`nwinfo_get_netinfo` 是否有 `nr5g_band` 字段没在代码里找到；字段不存在时这一行整段不显示） | 无 | 否·透传 |
| 「LTE 频段：」 | `/api/network/signal` | `lte_band`（**未确认**，同上） | 无 | 否·透传 |
| 操作结果提示（成功绿条/错误横幅） | 各写接口的返回 | — | — | — |

页面**不读取当前锁频配置**：NR/LTE 勾选框初始全不选，与设备上实际锁定的频段无关。agent 也没有锁频状态的 GET 接口。

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「重置 / 全部解锁」（页头，红色） | button「重置 / 全部解锁」 | POST `/api/cell/band/reset`（无请求体；ubus `nwinfo_rest_band_rat`） | 1 | 无读回（agent 无锁频状态 GET） | 「重置所有频段锁定？设备将切换为自动选择频段。」 | 三 |
| NR 模式单选「NSA」/「SA」 | radio「NSA」、radio「SA」 | 无——**选了也不影响提交**：`applyNR` 固定先发 nsa 再发 sa，`nrMode` 没被用到 | — | — | 无 | —（本地） |
| NR 频段勾选框 n1 n3 n5 n7 n8 n28 n38 n40 n41 n66 n71 n77 n78 n79（14 个） | checkbox「n1」…「n79」 〔现名：button「n1」「n79」（toolbar「NR (5G) 频段」里的开关按钮，aria-pressed）〕 | 无（草稿） | — | — | 无 | —（本地） |
| 「应用 NR 锁定」（未选时禁用） | button「应用 NR 锁定」 | POST `/api/cell/band/nr` `{nr5g_type, nr5g_band:"78,41"}`（去掉 n 前缀逗号拼接；ubus `nwinfo_set_nrbandlock`） | **2 步**：① `{nr5g_type:"nsa", nr5g_band}` ② `{nr5g_type:"sa", nr5g_band}`；第 ① 步成功第 ② 步失败时 NSA 已锁、SA 没锁，页面只报错不回滚 | 无读回 | 无 | 三（锁频） |
| LTE 频段勾选框 B1 B2 B3 B4 B5 B7 B8 B12 B17 B20 B28 B38 B40 B41（14 个） | checkbox「B1」…「B41」 〔现名：button「B1」「B41」（toolbar「LTE 频段」里的开关按钮，aria-pressed）〕 | 无（草稿） | — | — | 无 | —（本地） |
| 「应用 LTE 锁定」（未选时禁用） | button「应用 LTE 锁定」 | POST `/api/cell/band/lte` `{is_lte_band:"1", lte_band_mask:"1,3", is_gw_band:"0", gw_band_mask:""}`（ubus `nwinfo_set_gwl_bandlock`） | 1 | 无读回 | 无 | 三（锁频） |

写接口假成功风险：三个都是「否·透传」（cell.rs:56-83），页面不看 `data.result`。

---

## /clients 已连设备

文件：`app/(panel)/clients/page.tsx`。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 「共 N 台 · M 个有效租约」 | `/api/network/clients` | `dhcp_leases.length`；有效 = `dhcp_leases[].expires` > 设备当前时间（`deviceNow(useDeviceOffset())`，偏移来自 `/api/public/status` `clock.utc_offset`） | 5000 | 是：network_ext.rs:28-32 两个 luci-rpc 调用失败时分别给 `null`/省略，照样 ok，页面显示「暂无设备」 |
| 「实时 / 重连中」 | `/api/network/clients` | SWR `error` | 5000 | — |
| 设备行：名称（租约主机名 → hosts 表按 MAC 查 → 「未知设备」） | `/api/network/clients` | `dhcp_leases[].hostname`、`hosts[mac]` | 5000 | 同上 |
| 设备行：IP · MAC | `/api/network/clients` | `dhcp_leases[].ipaddr`、`dhcp_leases[].macaddr` | 5000 | 同上 |
| 设备行：连接（「5 GHz · Wi-Fi 6 · 2402 Mbps · 信号很好」；租约有效但不在 Wi-Fi 上时「没连 Wi-Fi（网线、USB，或已经离开）」；悬停看 dBm 和信道） | `/api/network/clients` | `wifi[]` 按 MAC 对上：`band`、`wifi_gen`、`link_down_mbps`、`signal`、`channel`（agent 读 `iw dev <ap> info` 和 `station dump`，2026-09-25 加；旧 agent 没有这个字段就不显示） | 5000 | 否：读不到时列表为空，页面只是不显示这一行 |
| 设备行：租约剩余（`Xh Ym`，过期显示「已过期」） | `/api/network/clients` | `dhcp_leases[].expires` 减设备当前时间 | 5000 | 同上 |
| 空状态「暂无设备」 | `/api/network/clients` | `dhcp_leases` 为空 | 5000 | 同上 |
| 错误横幅（带重试） | `/api/network/clients` | SWR `error` | — | — |

说明：租约剩余时间过期时，`fmtLease` 返回英文 `Expired`（写死，未翻译），但过期行实际走的是「已过期」分支，所以只在边界上可能露出英文。

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「刷新」（页头） | button「刷新」 | GET `/api/network/clients`（SWR `mutate` 重拉） | — | — | 无 | —（本地，只读） |
| 错误横幅「Retry」 | button「Retry」（ErrorBanner 里写死英文，未翻译；建议「重试」） 〔现名：button「重试」〕〔交互后：读取失败时（错误状态）；场景 down〕 | 同上 | — | — | 无 | —（本地，只读） |
| 「显示已过期 (N)」/「隐藏已过期 (N)」（有过期租约时出现） | button「显示已过期 (N)」 | 无（筛选） | — | — | 无 | —（本地） |

---

## /config 配置工具

文件：`app/(panel)/config/page.tsx`。**完全在浏览器里运行，不调任何接口**：读本地选的文件，解析文件头，显示十六进制。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 选中的文件名 | 无（浏览器 FileReader） | — | — | 不适用（不经 agent） |
| 文件头：Magic / 负载类型 / 签名 / 负载偏移 / 文件大小 | 无（`parseHeader` 解析本地文件） | — | — | 不适用 |
| 十六进制预览（前 256 字节） | 无 | — | — | 不适用 |
| 「无法识别的 ZXHN 配置文件」错误 | 无 | — | — | 不适用 |
| 已知解密密钥表（5 行：描述 + 密钥 hex，写死在页面里） | 无 | — | — | 不适用 |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 拖放区（点击打开文件选择，接受 `.zxhn,.bin,.cfg,.conf`） | 无（是个可点的 `div`，键盘无法聚焦）；建议 button「选择配置文件」 | 无 | — | — | 无 | —（本地） |
| 隐藏的文件输入框 | 无（`className="hidden"`） 〔交互后：隐藏的 input 本来就不可见，由「选择配置文件」按钮打开，无法单独定位〕 | 无 | — | — | 无 | —（本地） |
| 「下载十六进制转储」 | button「下载十六进制转储」 〔交互后：选了配置文件之后才出现（要上传文件，未自动验证）〕 | 无（浏览器生成 `文件名.hex.txt` 下载） | — | — | 无 | —（本地） |

---

## /device-info 设备信息

文件：`app/(panel)/device-info/page.tsx`。全部只读，所有请求只在进页面时拉一次。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| IMEI | `/api/sim/imei` | `imei` | 无 | 否·透传（sim.rs:13） |
| IMSI | `/api/sim/info` | `sim_imsi` | 无 | 否·透传（sim.rs:6） |
| ICCID | `/api/sim/info` | `sim_iccid` | 无 | 否·透传 |
| SIM 状态 | `/api/sim/info` | `sim_states` | 无 | 否·透传 |
| 运营商 | `/api/network/signal` | `network_provider_fullname` ?? `network_provider` | 无 | 否·透传 |
| 网络制式 | `/api/network/signal` | `network_type` | 无 | 否·透传 |
| IPv4 (WAN) | `/api/network/wan` | `["ipv4-address"][0].address` | 无 | 否·透传（network_ext.rs:6） |
| 网关 | `/api/network/wan` | `route[0].nexthop` | 无 | 否·透传 |
| DNS | `/api/network/wan` | `["dns-server"][0]` | 无 | 否·透传 |
| IPv6 (WAN) | `/api/network/wan6` | `["ipv6-address"][0].address` | 无 | 否·透传（network_ext.rs:13） |
| LAN IP | `/api/network/lan-status` | `["ipv4-address"][0].address` | 无 | 否·透传（network_ext.rs:20） |
| 主机名 | `/api/device/system` | `hostname`（**未确认**：该接口是 `ubus call system info`，OpenWrt 的 `system info` 通常不含 `hostname`/`kernel`，那两项在 `system board` 里；很可能恒显示「—」） | 无 | 否·透传（device_ext.rs:22） |
| 内核 | `/api/device/system` | `kernel`（同上，**未确认**） | 无 | 否·透传 |
| 运行时间 | `/api/device/system` | `uptime` | 无 | 否·透传 |
| 内存总量 / 空闲 / 可用 | `/api/device/system` | `memory.total`、`memory.free`、`memory.available`（字节，显示为 MB） | 无 | 否·透传 |
| SIM 读取错误横幅 | `/api/sim/info` | SWR `error`（只显示 SIM 这一个接口的错误） | — | — |

#### 控件

本页没有控件。

---

## /health 健康

文件：`app/(panel)/health/page.tsx`。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 总结（一切正常 / N 项异常，M 项需要注意）+「HH:mm 检查」 | `/api/health` | `bad`、`warn`、`checked_at`（设备时钟，`fmtDevice`） | 60000 | 否：检查跑不起来时 ok 仍为 true，但带 `error` 字段，页面会显示（health.rs:184-206） |
| 「检查没能运行：{错误}」 | `/api/health` | `error` | 60000 | 否 |
| 「管理后台启动约 20 秒后做第一次检查。」 | `/api/health` | `checked_at == null` | 60000 | 否 |
| 检查项列表：级别符号（● ▲ ■）/ 名称 / 详情 | `/api/health` | `checks[].level/label/detail`（`id` 作 key） | 60000 | 否 |
| 崩溃记录列表：时间 / 程序 / 状态 | `/api/health` | `crashlogs[].time/program/status`（`file` 用于取正文） | 60000 | 否 |
| 崩溃日志正文（展开后） | `/api/health/crashlog?program=…&file=…` | `text`（最多 256 KB） | 无（点开时拉一次） | 否 |
| 「没有崩溃记录。…」 | `/api/health` | `crashlogs.length === 0` | 60000 | 否 |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「现在检查」（页头） | button「现在检查」 | GET `/api/health?refresh=1`（同步跑一遍设备上的 `/data/u60-guard/doctor.sh --tsv`，最长 20 秒） | 1 | 返回体即新结果（`checked_at` 更新） | 无 | 二（GET 但会在设备上起进程，见汇总「有副作用的 GET」） |
| 崩溃记录行（整行是按钮，「查看」/「收起」） | button，名称为整行文字（时间 + 程序 + 状态 +「查看」）；建议加 `aria-label`「查看 {程序} {时间} 的崩溃日志」 | GET `/api/health/crashlog?program=&file=` | — | — | 无 | —（本地，只读） |

---

## /router/apn APN 设置

文件：`app/(panel)/router/apn/page.tsx`（含 `WanIpv6Card`）。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| APN 模式（手动/自动 + 说明） | `/api/router/apn/mode` | `apn_mode`（1=手动） | 无 | 否·透传（router.rs:252） |
| 手动配置列表：名称 +「使用中」（只在手动模式；自动模式下被选中的那条写「手动模式时用这条」，2026-09-25：手动列表的 `isEnable` 只表示手动模式会用哪条） | `/api/router/apn/profiles` + `/api/router/apn/mode` | `apnListArray[].profilename`、`.isEnable`、`apn_mode` | 无 | 否·透传（router.rs:270） |
| 手动配置列表：APN · PDP 类型 · 认证方式（none 不显示） | `/api/router/apn/profiles` | `apnListArray[].wanapn`、`.pdpType`、`.pppAuthMode` | 无 | 否·透传 |
| 手动配置 id（编辑/删除/启用时用） | `/api/router/apn/profiles` | `apnListArray[].cid`（转字符串当 `profileId`；**未确认** `cid` 是否就是固件要的 `profileId`） | 无 | 否·透传 |
| 「暂无手动配置」空状态 | `/api/router/apn/profiles` | 列表为空 | 无 | 否·透传 |
| 自动检测的配置列表（只读）：名称 +「使用中」（只在自动模式）+ APN · PDP | `/api/router/apn/auto-profiles` | `apnListArray[].profilename/isEnable/wanapn/pdpType` | 无 | 否·透传（router.rs:299） |
| 编辑表单预填：名称/APN/用户名/密码/PDP/认证/设为使用中 | `/api/router/apn/profiles` | 对应条目的 `profilename/wanapn/username/password/pdpType/pppAuthMode/isEnable` | 无 | 否·透传 |
| 运营商 IPv6(WAN)：向运营商请求 IPv6 / 仅 IPv4 | `/api/router/wan-ipv6` | `ipv6_enabled` | 无 | 否（读 APN 失败回 503，router.rs:333-336） |
| 运营商 IPv6：PDP 类型 · 当前有/无 IPv6 | `/api/router/wan-ipv6` | `pdp_type`（3=IPv4v6、2=IPv6、其他=IPv4）、`wan_has_ipv6` | 无 | 是：router.rs:338-345 `get_wwaniface` 失败时 `wan_has_ipv6` 为 false，显示「当前无 IPv6」 |
| 加载错误横幅（模式或列表出错，带 Retry）/「读取 WAN IPv6 状态失败。」 | 上述接口 | SWR `error` | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| APN 模式开关「手动」 | switch「手动」 | PUT `/api/router/apn/mode` `{apn_mode: 1 或 0}`（ubus `set_apn_mode`） | 1 | `/api/router/apn/mode` `apn_mode` | 无 | 三（APN 切换） |
| 运营商 IPv6(WAN) 开关 | switch「运营商 IPv6(WAN)」 | PUT `/api/router/wan-ipv6` `{enabled: bool}` | 1 个 HTTP 请求；agent 内部 3 步：读 cid1 APN → `set_apn_at_cid` 改 `pdpType/roamingPdpType`（开=3，关=1）→ `set_qcliiface` 开/关 IPv6 支路（这一步失败被忽略，router.rs:400） | `/api/router/wan-ipv6` `pdp_type`（配置）；`wan_has_ipv6`（实际是否有 IPv6，可能滞后） | 无 | 三（APN 修改） |
| 「添加配置」（卡片右上） | button「添加配置」 | 无（打开表单弹层） | — | — | 无 | —（本地） |
| 条目「启用」（非使用中才有） | button「启用」（每行同名；建议 `aria-label`「启用 {名称}」） | POST `/api/router/apn/profiles/activate` `{profileId}`（ubus `enable_manu_apn_id`） | 1 | `/api/router/apn/profiles` 对应条目 `isEnable` | 无 | 三（APN 切换） |
| 条目铅笔图标 | button「编辑 {名称}」 | 无（打开编辑表单） | — | — | 无 | —（本地） |
| 条目垃圾桶图标（使用中的禁用） | button「删除 {名称}」 | POST `/api/router/apn/profiles/delete` `{profileId}`（ubus `delete_manu_apn`） | 1 | `/api/router/apn/profiles` 条目消失 | 「删除 APN "{名称}"?」 | 三 |
| 表单·配置名称 * | 无（`Field` 的 `<label>` 未关联）；建议 textbox「配置名称」 〔现名：textbox「配置名称 *」〕〔交互后：点 button「添加配置」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 表单·APN * | 无；建议 textbox「APN」 〔现名：textbox「APN *」「APN *{帮助}」〕〔交互后：点 button「添加配置」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 表单·用户名 | 无；建议 textbox「用户名」 〔交互后：点 button「添加配置」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 表单·密码 | 无；建议「密码」 〔交互后：点 button「添加配置」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 表单·PDP 类型下拉（IPv4 / IPv6 / IPv4v6） | 无；建议 combobox「PDP 类型」 〔交互后：点 button「添加配置」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 表单·认证方式下拉（none / PAP / CHAP） | 无；建议 combobox「认证方式」 〔交互后：点 button「添加配置」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 表单·「设为使用中的配置」开关 | 无（`Toggle` 没传 `label`，文字在旁边的 span 里）；建议 switch「设为使用中的配置」 〔交互后：点 button「添加配置」〕 | 无（草稿；只在**编辑**时生效，新增时被忽略） | — | — | 无 | —（本地） |
| 表单「取消」 | button「取消」 〔交互后：点 button「添加配置」〕 | 无 | — | — | 无 | —（本地） |
| 表单「添加配置」（新增时） | button「添加配置」（与卡片按钮同名；建议弹层按钮改「保存」或加 dialog 作用域） | POST `/api/router/apn/profiles` `{profilename, wanapn, pdpType, pppAuthMode, username, password}`（ubus `add_manu_apn`） | 1（页面先查同名重复、必填） | `/api/router/apn/profiles` 出现新条目 | 无 | 三（APN 修改） |
| 表单「保存修改」（编辑时） | button「保存修改」 〔交互后：编辑已有配置时；点 button「编辑 China Mobile」〕 | PUT `/api/router/apn/profiles` `{profileId, profilename, wanapn, pdpType, pppAuthMode, username, password}`（ubus `modify_manu_apn`） | **最多 2 步**：① PUT `/api/router/apn/profiles` ② 若勾了「设为使用中」→ POST `/api/router/apn/profiles/activate` `{profileId}`；① 成功 ② 失败时改动已保存但没切换 | `/api/router/apn/profiles` 条目字段、`isEnable` | 无 | 三（APN 修改/切换） |
| 错误横幅「Retry」 | button「Retry」 〔现名：button「重试」〕〔交互后：读取失败时（错误状态）；场景 down〕 | 重拉三个 GET | — | — | 无 | —（本地，只读） |

写接口假成功风险：模式、增删改、启用都是「否·透传」（router.rs:259-326），页面不看 `data.result`；`/api/router/wan-ipv6` PUT 为「是」（router.rs:400）。

说明：页面显示的 PDP 类型是字符串（`IPv4`/`IPv6`/`IPv4v6`），而 `/api/router/wan-ipv6` 里 agent 读到的 `pdpType` 是数字（1/2/3）。表单提交的也是字符串。**未确认**固件 `add_manu_apn`/`modify_manu_apn` 接受哪种。

---

## /router/celllock 小区锁定

文件：`app/(panel)/router/celllock/page.tsx`。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 当前小区·PCI | `/api/network/signal` | NR 时 `nr5g_pci`，否则 `lte_pci`（NR 判断同首页） | 5000 | 否·透传 |
| 当前小区·EARFCN | `/api/network/signal` | NR 时 `nr5g_action_channel`，否则 `lte_earfcn` | 5000 | 否·透传 |
| 当前小区·Band | `/api/network/signal` | NR 时 `nr5g_action_band`，否则 `lte_band` | 5000 | 否·透传 |
| 当前小区·Cell ID | `/api/network/signal` | NR 时 `nr5g_cell_id`，否则 `lte_cell_id` | 5000 | 否·透传 |
| NR 邻区表（列 = 返回对象的所有键，动态生成） | `/api/cell/neighbors/nr` | 页面猜结构：`cells[]` 或 `list[]`，都没有时把整个对象当一行（**未确认**固件真实结构） | 无（扫描后拉一次） | 否·透传（cell.rs:42） |
| LTE 邻区表 | `/api/cell/neighbors/lte` | 同上 | 无 | 否·透传（cell.rs:49） |
| 「点击“扫描邻区”以发现附近的小区。」 | — | 两张表都为空 | — | — |
| 操作结果提示 | 写接口返回 | — | — | — |

页面**不读取当前锁小区配置**，表单初始为空；agent 也没有锁小区状态的 GET。「当前小区」显示的是正在驻留的小区，不是锁定设置。

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「全部解锁」（页头，红色） | button「全部解锁」 | POST `/api/cell/lock/reset`（ubus `nwinfo_reset_band_cell_setting`；从名字看会同时重置频段和小区设置，**未确认**） | 1 | 无读回（agent 无锁小区状态 GET）；间接看 `/api/network/signal` 的驻留小区 | 「重置所有小区锁定？」 | 三 |
| NR·PCI * 输入框 | 无（label 未关联）；建议 textbox「NR PCI」 | 无（草稿） | — | — | 无 | —（本地） |
| NR·EARFCN * 输入框 | 无；建议 textbox「NR EARFCN」 | 无（草稿） | — | — | 无 | —（本地） |
| NR·频段（可选）输入框 | 无；建议 textbox「NR 频段」 〔现名：textbox「NR 频段（可选）」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 「锁定 NR」 | button「锁定 NR」 | POST `/api/cell/lock/nr` `{pci, earfcn, band?}`（字符串；ubus `nwinfo_lock_nr_cell`） | 1 | 无读回；间接：`/api/network/signal` `nr5g_pci`/`nr5g_action_channel` 等于所填值 | 无 | 三（锁小区） |
| LTE·PCI * 输入框 | 无；建议 textbox「LTE PCI」 | 无（草稿） | — | — | 无 | —（本地） |
| LTE·EARFCN * 输入框 | 无；建议 textbox「LTE EARFCN」 | 无（草稿） | — | — | 无 | —（本地） |
| 「锁定 LTE」 | button「锁定 LTE」 | POST `/api/cell/lock/lte` `{pci, earfcn}`（ubus `nwinfo_lock_lte_cell`） | 1 | 无读回；间接：`/api/network/signal` `lte_pci`/`lte_earfcn` | 无 | 三（锁小区） |
| 「扫描邻区」/「扫描中…」 | button「扫描邻区」（`/api/netinfo` 的 `neighbors.state` 为 `unsupported` 时置灰，下方写原因） | POST `/api/cell/neighbors/scan`（agent 固定回 410「原厂扫描会断网且拿不到数据，已停用」，不再调 ubus `nwinfo_scan_nbr`） | **3 个请求**：① POST 扫描 → 页面等 3 秒 → ② GET `/api/cell/neighbors/nr` ③ GET `/api/cell/neighbors/lte`（②③ 失败被静默忽略，仍提示「扫描完成」） | 邻区表有行 | 无 | 二 |
| 邻区表每行「选择」 | button「选择」（每行同名；建议 `aria-label`「用 PCI {pci} 填表」） 〔现名：button「用 PCI {pci} 填表」〕〔缺失：2026-09-25 起邻区扫描停用（原厂扫描会断移动数据且拿不到小区，agent 回 410，按钮置灰并说明原因），结果行不会出现〕 | 无（把该行 pci/earfcn/band 填进上面表单） | — | — | 无 | —（本地） |

写接口假成功风险：锁 NR / 锁 LTE / 重置 / 扫描都是「否·透传」（cell.rs:6-40），页面不看 `data.result`。

---

## /router/device 设备控制

文件：`app/(panel)/router/device/page.tsx`。重启、恢复出厂用的是页面内的两段式按钮确认（自定义确认，非 `window.confirm`）。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 「当前已停止充电（硬件强制）。」 | `/api/device/charge-control` | `charging_stopped` | 无 | 是：device_ext.rs:57-60 `zwrt_bsp.charger list` 失败时当作 false |
| 充电限制开关初值 | `/api/device/charge-control` | `charge_limit_enabled` | 无 | 否（agent 内存状态） |
| 充电上限 % 滑块初值 | `/api/device/charge-control` | `charge_limit` | 无 | 否 |
| 回差 % 滑块初值 | `/api/device/charge-control` | `hysteresis` | 无 | 否 |
| 「当前电量：N% — 状态」 | `/api/device/charge-control` | `capacity`、`battery_status`（sysfs） | 无 | 是：device_ext.rs:45-55 sysfs 读不到时 capacity=0、status 空 |
| 省电模式开关初值 | `/api/device/power-save`（**POST** 读取） | 请求 `{deviceInfoList:["power_saver_mode"]}`，读 `power_saver_mode`（"1"=开）；失败被静默吞掉，开关保持禁用 | 无 | 否·透传（device_ext.rs:126） |
| 快速启动开关初值 | `/api/device/fast-boot` | `fast_boot`（"1"=开） | 无 | 否·透传（device_ext.rs:156，读不到键时当 "0"） |
| 错误横幅 | charge-control / fast-boot | SWR `error` | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「启用充电限制」开关 | switch「已启用」/「已禁用」（名称随状态变，**不适合做测试定位**）；建议 switch「启用充电限制」 | 无（草稿，等「应用」） | — | — | 无 | —（本地） |
| 充电上限 % 滑块（50–100） | 无（文字在 span 里，`<input type=range>` 无标签）；建议 slider「充电上限 %」 〔交互后：打开「启用充电限制」后出现；点 switch「启用充电限制」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 回差 % 滑块（1–10） | 无；建议 slider「回差 %」 〔交互后：打开「启用充电限制」后出现；点 switch「启用充电限制」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 「应用」 | button「应用」 | PUT `/api/device/charge-control` `{charge_limit_enabled, charge_limit, hysteresis}` | 1 | 返回体就是重读的状态；GET `/api/device/charge-control` `charge_limit_enabled/charge_limit/hysteresis` | 无 | 二 |
| 省电模式开关 | switch「开」/「关」（随状态变）；建议 switch「省电模式」 | PUT `/api/device/power-save` `{deviceInfoList:[{power_saver_mode:"1"/"0"}]}`（ubus `set_device_info`） | 1（先乐观翻转，失败再翻回） | POST `/api/device/power-save` 读 `power_saver_mode` | 无 | 二 |
| 快速启动开关 | switch「开」/「关」（随状态变）；建议 switch「快速启动」 | PUT `/api/device/fast-boot` `{fast_boot:"1"/"0"}` | 1（乐观翻转） | `/api/device/fast-boot` `fast_boot` | 无 | 二 |
| 「重启设备」 | button「重启设备」 | 无（进入确认态） | — | — | 无 | —（本地） |
| 「确认重启」 | button「确认重启」 〔交互后：确认改为对话框；点 button「重启设备」〕 | POST `/api/device/reboot`（ubus `system reboot`） | 1 | 无读回（设备断线；可观察 `/api/device/system` `uptime` 变小） | 自定义确认（非 window.confirm）：「路由器将重启并暂时无法连接。」 | 三（重启） |
| 重启确认态「取消」 | button「取消」 〔交互后：确认对话框里；点 button「重启设备」〕 | 无 | — | — | 无 | —（本地） |
| 「恢复出厂设置」 | button「恢复出厂设置」 | 无（进入第一段确认） | — | — | 无 | —（本地） |
| 「确定」（第一段） | button「确定」 〔现名：textbox「{前}恢复出厂{后}」（两段确认合成一个对话框：第一段「确定」换成输入确认词「恢复出厂」）〕〔交互后：点 button「恢复出厂设置」〕 | 无（进入第二段确认） | — | — | 自定义确认：「这将清除所有设置。确定吗？」 | —（本地） |
| 「立即重置」（第二段） | button「立即重置」 〔交互后：确认对话框里，输入确认词前是禁用的；点 button「恢复出厂设置」〕 | POST `/api/device/factory-reset`（ubus `zwrt_bsp.power factory_reset`） | 1 | 无读回 | 自定义两段确认：「这将清除所有设置。确定吗？」→「最终确认——此操作无法撤销。」 | 三（恢复出厂） |
| 恢复出厂各段「取消」 | button「取消」 〔交互后：确认对话框里；点 button「恢复出厂设置」〕 | 无 | — | — | 无 | —（本地） |

写接口假成功风险：charge-control PUT 为「否」（ubus 失败回 500，device_ext.rs:91-96）；power-save、fast-boot、reboot、factory-reset 为「否·透传」。

---

## /router/dns DNS 设置

文件：`app/(panel)/router/dns/page.tsx`。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| DNS 模式单选初值（自动/手动/DoH） | `/api/router/dns`、`/api/doh/status` | `config.enabled`（DoH 为真时选 DoH）否则 `dns_mode`（agent 已去掉固件的 `wan_` 前缀） | 无（只初始化一次，`initDone`） | 否·透传（router.rs:6-41） |
| 主 DNS / 备用 DNS 输入框初值 | `/api/router/dns` | `prefer_dns_manual`、`standby_dns_manual`（手动模式下为空时 agent 用 `uci network.wan.dns` 补） | 无 | 否·透传 |
| DoH 上游 URL 输入框初值 | `/api/doh/status` | `config.upstream_url` | 无 | 否（agent 内存状态） |
| 「缓存条目：N」「查询次数：N」 | `/api/doh/status` | `stats.cache_entries`、`stats.queries_total` | 无 | 否 |
| DoH 缓存表（名称 / 类型 / TTL）+「N 条」 | `/api/doh/cache` | 页面读 `[].name`、`[].type`、`[].ttl`；**agent 实际返回 `domain` 不是 `name`**（doh/mod.rs:106），所以「名称」列恒为「—」 | 无（DoH 开着时进页拉一次） | 否 |
| 加载错误横幅 | dns / doh status | SWR `error` | — | — |

接口里有、页面没显示的：`/api/doh/status` 的 `running`、`stats.cache_hits/cache_misses`、`config.cache_enabled/cache_max_entries/listen_addr/timeout_ms`；`/api/router/dns` 的 `prefer_dns_auto/standby_dns_auto`（运营商下发的 DNS）。

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| DNS 模式单选：「自动（运营商分配）」/「手动」/「DNS-over-HTTPS (DoH)」 | radio「自动（运营商分配）」「手动」「DNS-over-HTTPS (DoH)」 | 无（草稿，等「应用」） | — | — | 无 | —（本地） |
| 主 DNS 输入框（手动时） | 无（label 未关联）；建议 textbox「主 DNS」 〔交互后：DNS 模式选「手动」后；点 radio「手动」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 备用 DNS（可选）输入框 | 无；建议 textbox「备用 DNS」 〔现名：textbox「备用 DNS（可选）」〕〔交互后：DNS 模式选「手动」后；点 radio「手动」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 上游 URL 输入框（DoH 时） | 无；建议 textbox「上游 URL」 〔交互后：DNS 模式选 DoH 后；点 radio「DNS-over-HTTPS (DoH)」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 「应用」——模式=自动 | button「应用」 | 见步骤 | **2 步**：① PUT `/api/router/dns` `{dns_mode:"auto", prefer_dns_manual:"", standby_dns_manual:""}`（ubus `router_set_wan_dns`）② POST `/api/doh/disable` `{}`（停 DoH、删 dnsmasq drop-in、`uci delete`+commit、重启 dnsmasq） | `/api/router/dns` `dns_mode`；`/api/doh/status` `config.enabled=false` | 无 | 一（DNS 上游） |
| 「应用」——模式=手动 | button「应用」 | 见步骤（页面先校验 IPv4） | **2 步**：① PUT `/api/router/dns` `{dns_mode:"manual", prefer_dns_manual, standby_dns_manual}` ② POST `/api/doh/disable` `{}` | `/api/router/dns` `dns_mode`、`prefer_dns_manual`、`standby_dns_manual` | 无 | 一（DNS 上游） |
| 「应用」——模式=DoH | button「应用」 | 见步骤 | **2 步**：① PUT `/api/doh/config` `{upstreams:[url]}` ② POST `/api/doh/enable` `{}`（起 DoH 代理、写 `/tmp/dnsmasq.d/doh.conf`、重启 dnsmasq）。**缺陷**：agent 的补丁结构只认 `upstream_url`（doh/config.rs:29-35），`upstreams` 被 serde 忽略，所以第 ① 步永远不改上游，却回 ok | `/api/doh/status` `config.enabled`、`config.upstream_url`（可验证上游到底改没改） | 无 | 一（DoH 上游）；② 开 DoH 本身严格说不在第一档清单，见汇总 |
| 「清除缓存」（DoH 开着时） | button「清除缓存」 〔交互后：只在设备上的 DoH 代理正在运行时出现（模拟数据里 DoH 没开，未自动验证）〕 | POST `/api/doh/cache/clear` `{}` | 1 | `/api/doh/status` `stats.cache_entries` = 0 | 无 | 二 |

写接口假成功风险：
- PUT `/api/router/dns`：否·透传（router.rs:43-52）。
- PUT `/api/doh/config`：**是**（页面字段名不对，请求等于空补丁，doh/mod.rs:87-95 照样保存回 ok）。
- POST `/api/doh/enable`：**是**，server.rs:449-452 写 drop-in 和重启 dnsmasq 的结果都被 `let _ =` 丢掉；doh/mod.rs:119 保存 enabled 失败也不报。
- POST `/api/doh/disable`：**是**，server.rs:458-471 恢复 dnsmasq 的命令结果被丢掉。

---

## /router/esim eSIM

文件：`app/(panel)/router/esim/page.tsx`（`ProfilesSection`、`NotificationsSection`）。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 未部署 lpac 提示（「设备上未部署 lpac」） | `/api/esim/status` | `installed === false` | 无 | 否 |
| EID（显示末 8 位，悬停/副标题显示全文） | `/api/esim/status` | `eid` | 无 | 否（lpac 出错回 502，esim.rs:265-284） |
| 剩余空间 | `/api/esim/status` | `free_nvm`（字节） | 无 | 否 |
| SGP.22 规范版本 | `/api/esim/status` | `sgp22_version` | 无 | 否 |
| 芯片固件 + 根 SM-DS 地址 | `/api/esim/status` | `firmware_version`、`root_smds` | 无 | 否 |
| （接口有、页面没显示）默认 SM-DP+ | `/api/esim/status` | `default_smdp` | — | — |
| 配置列表：名称（昵称 → 运营商名 → 配置名）+「使用中」 | `/api/esim/profiles` | `profiles[].profileNickname/serviceProviderName/profileName`、`profileState === "enabled"` | 10000 | 否（esim.rs:287-300；有任务在跑时回 `busy:true, profiles:null`，页面保留旧列表） |
| 配置列表：运营商名 · 配置名 · ICCID | `/api/esim/profiles` | `profiles[].serviceProviderName/profileName/iccid` | 10000 | 否 |
| 「卡上还没有配置文件——在下方添加。」 | `/api/esim/profiles` | 列表空且不忙 | 10000 | 否 |
| 忙碌状态标签（切换中…/下载中…/删除中…/重启中…/处理中…） | `/api/esim/job`（有任务时） + `/api/esim/profiles` `busy` | `status`、本地 `opLabel` | job 1500（仅在有任务时） | 否 |
| 任务结果提示（完成 / 失败原因） | `/api/esim/job` | `id`（对上本次 `job_id`）、`status`（done/error）、`message`、`rebooting` | 1500 | 是：esim.rs:436-449 切换没收敛时任务标记 `done`（「switched — rebooting to finish」）然后重启，并没有验证新配置已生效 |
| 重启横幅「设备正在重启并切换到新配置…」+ 自动重连 | `/api/public/status`（`noAuth`、`raw`） | 只看能否连上 | `setInterval` 4000（仅重启中） | — |
| 待发送通知列表：#序号 · 操作类型 · 地址 · ICCID（列表空时整卡隐藏） | `/api/esim/notifications` | `[].seqNumber/profileManagementOperation/notificationAddress/iccid` | 30000 | 否 |
| 各类加载错误（读取 eUICC 失败 / 加载配置失败） | status / profiles | SWR `error` | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「刷新」（页头） | button「刷新」 | GET `/api/esim/status` 重拉 | — | — | 无 | —（本地，只读） |
| 错误横幅「Retry」 | button「Retry」 〔现名：button「重试」〕〔交互后：读取失败时（错误状态）；场景 down〕 | 重拉 status 或 profiles | — | — | 无 | —（本地，只读） |
| 条目铅笔图标（改名） | 无（只有 `title`=「改名」，无 aria-label；`title` 可作为兜底名称）；建议 button「给 {名称} 改名」 | 无（进入编辑态） | — | — | 无 | —（本地） |
| 改名输入框（编辑态，回车保存） | 无（只有 placeholder=运营商名）；建议 textbox「昵称」 〔现名：textbox「{名称} 的昵称」〕〔交互后：点 button「给 台湾旅行 改名」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 「保存」（编辑态） | button「保存」 〔交互后：改名编辑态；点 button「给 台湾旅行 改名」〕 | POST `/api/esim/nickname` `{iccid, nickname}`（空串=清除；同步 lpac） | 1（昵称未变不发） | `/api/esim/profiles` `profiles[].profileNickname` | 无 | 二 |
| 条目「切换」（非使用中才有） | button「切换」（每行同名；建议 `aria-label`「切换到 {名称}」） | POST `/api/esim/switch` `{iccid}` → 返回 `job_id` | 1 个写请求，之后轮询 `/api/esim/job`；agent 内部：lpac enable → `qmi simreset` → 重启 `zte_topsw_mdm` → 最多 30 秒等身份收敛，否则 **2 秒后重启整机**。距上次切换 5 分钟内会被 429 拒绝 | `/api/esim/job` `status=done` 且 `/api/esim/profiles` 该条 `profileState="enabled"`；重启路径下以设备回来后的 profiles 为准 | 「切换到“{名称}”?\n\n基带将以新配置重新注网——预计短暂断网(通常约 40 秒,无需重启)。」 | 三（eSIM 切换/启用） |
| 条目垃圾桶图标（非使用中才有） | 无 aria-label（`title`=「删除」）；建议 button「删除 {名称}」 | POST `/api/esim/delete` `{iccid}` → `job_id`（agent 先查不能删使用中的） | 1，之后轮询 job | `/api/esim/job` `status=done`；`/api/esim/profiles` 该条消失 | 「从卡上永久删除配置“{名称}”?此操作不可撤销。」 | 三（eSIM 删除） |
| 激活码输入框（占位 `LPA:1$smdp.example.com$XXXX-XXXX-XXXX`） | 无；建议 textbox「激活码」 | 无（草稿；页面校验以 `LPA:1$` 或 `1$` 开头） | — | — | 无 | —（本地） |
| 确认码(可选) 输入框 | 无（只有 placeholder）；建议 textbox「确认码」 〔现名：textbox「确认码(可选)」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 「下载」 | button「下载」 | POST `/api/esim/download` `{code, confirmation_code?}` → `job_id`（只下载不启用） | 1，之后轮询 job；agent 内部下载后再补发通知（失败忽略） | `/api/esim/job` `status=done`；`/api/esim/profiles` 出现新条目 | 无 | 二（下载新配置不在第三档清单里的「切换/启用/删除」；但会占用卡上空间且联网，建议评审时再定） |
| 待发送通知「全部发送」 | button「全部发送」 | POST `/api/esim/notifications/process` `{}` → `job_id` | 1，之后页面内每 2 秒拉 `/api/esim/job`，最多 60 次 | `/api/esim/job` `status`；`/api/esim/notifications` 变空 | 无 | 二 |

写接口假成功风险：nickname 否（esim.rs:336-356）；download / delete / notifications 否（结果在 job 里，失败为 `error`）；switch **是**（见上，esim.rs:436-449）。

---

## /router/firewall 防火墙

文件：`app/(panel)/router/firewall/page.tsx`。所有 GET 都是 ubus 透传，页面对返回结构的假设（端口转发、过滤规则直接当数组）**没有在代码里得到验证**：固件 `router_get_portforward_rule` / `router_get_macipport_filter_rule` 多半返回对象而不是数组，若如此两张表会恒为空。以下字段名均按页面代码记录，标「未确认」。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 防火墙开关状态 | `/api/router/firewall` | `firewall_enable`（"1"=开，未确认） | 无 | 否·透传（router.rs:93） |
| 防火墙级别「当前：X」+ 三个级别按钮高亮 | `/api/router/firewall` | `level`（缺省按 medium 显示，未确认） | 无 | 否·透传 |
| NAT 开关状态 | `/api/router/firewall` | `nat_enable`（未确认） | 无 | 否·透传 |
| 端口转发总开关状态 | `/api/router/firewall` | `portforward_enable`（未确认） | 无 | 否·透传 |
| DMZ 开关 + 主机 IP 初值 | `/api/router/firewall` | `dmz_enable`、`dmz_ip` ?? `dmz_hostname`（未确认） | 无 | 否·透传 |
| UPnP 开关状态 | `/api/router/firewall/upnp` | `upnp_switch`（未确认） | 无 | 否·透传（router.rs:144） |
| 「N 条规则」+ 端口转发表（名称/协议/WAN/LAN IP/LAN 端口） | `/api/router/firewall/port-forward` | 当数组读：`[].id/name/protocol/wan_port/lan_ip/lan_port`（未确认） | 无 | 否·透传（router.rs:162） |
| 「暂无端口转发规则。」 | 同上 | 数组为空 | 无 | 否·透传 |
| 过滤规则（只读）表：名称/协议/源 IP/目标 IP/目标端口/动作 | `/api/router/firewall/filter-rules` | 当数组读：`[].name/protocol/src_ip/dst_ip/dst_port/action`（未确认） | 无 | 否·透传（router.rs:191） |
| 「未配置过滤规则。」 | 同上 | 为空 | 无 | 否·透传 |
| 接口里有、页面没显示 | `/api/router/firewall` | `wan_ping_enable`、`remote_web_access_enable`（页面声明了类型但没渲染） | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「防火墙」开关 | 无（`Toggle` 没传 label，文字在旁边 span）；建议 switch「防火墙」 | PUT `/api/router/firewall/switch` `{firewall_switch:"1"/"0"}`（ubus `router_set_firewall_switch`） | 1 | `/api/router/firewall` `firewall_enable` | 无 | 二·远程三 |
| 防火墙级别「低」「中」「高」 | button「低」「中」「高」（当前级别用 primary 样式区分，无 `aria-pressed`；建议加） 〔现名：radio「低」「中」「高」（radiogroup「防火墙级别」）〕 | PUT `/api/router/firewall/level` `{firewall_level:"low"/"medium"/"high"}` | 1 | `/api/router/firewall` `level` | 无 | 二·远程三 |
| 「NAT」开关 | 无；建议 switch「NAT」 | PUT `/api/router/firewall/nat` `{nat_switch:"1"/"0"}` | 1 | `/api/router/firewall` `nat_enable` | 无 | 二·远程三 |
| 「UPnP」开关 | 无；建议 switch「UPnP」 | PUT `/api/router/firewall/upnp` `{upnp_switch:"1"/"0"}` | 1 | `/api/router/firewall/upnp` `upnp_switch` | 无 | 二·远程三 |
| 「端口转发」总开关 | 无；建议 switch「端口转发」 | PUT `/api/router/firewall/port-forward/switch` `{port_forward_switch:"1"/"0"}` | 1 | `/api/router/firewall` `portforward_enable` | 无 | 二·远程三 |
| DMZ 开关（打开时弹出页内确认） | switch「已启用」/「已禁用」（随状态变）；建议 switch「DMZ」 | 无（打开只进入确认态；关闭只改草稿，要再点「应用」） | — | — | 无 | —（本地） |
| DMZ 页内确认「启用 DMZ」 | button「启用 DMZ」 〔现名：button「确认：{动作}」（DMZ 改为开关 +「应用」，页内确认是统一的「确认：…」）〕〔交互后：点 switch「DMZ」，填 textbox「DMZ 主机 IP」=192.168.0.120，点 button「应用」〕 | PUT `/api/router/firewall/dmz` `{dmz_enabled:"1", dmz_ip}` | 1 | `/api/router/firewall` `dmz_enable`、`dmz_ip` | 自定义确认（非 window.confirm）：「通过 DMZ 暴露主机将绕过防火墙保护。是否继续？」 | 二·远程三 |
| DMZ 页内确认「取消」 | button「取消」 〔交互后：DMZ 页内确认里；点 switch「DMZ」，填 textbox「DMZ 主机 IP」=192.168.0.120，点 button「应用」〕 | 无（开关退回关） | — | — | 无 | —（本地） |
| DMZ 主机 IP 输入框（DMZ 关时禁用） | 无（label 未关联）；建议 textbox「DMZ 主机 IP」 | 无（草稿） | — | — | 无 | —（本地） |
| DMZ「应用」 | button「应用」 | PUT `/api/router/firewall/dmz` `{dmz_enabled:"1"/"0", dmz_ip}`（开着时页面校验 IPv4） | 1 | `/api/router/firewall` `dmz_enable`、`dmz_ip` | 无 | 二·远程三 |
| 「添加规则」/「取消」（切换表单） | button「添加规则」/「取消」 | 无 | — | — | 无 | —（本地） |
| 规则表单·名称 / WAN 端口 / LAN IP / LAN 端口 输入框 | 无（label 未关联）；建议 textbox「名称」「WAN 端口」「LAN IP」「LAN 端口」 〔交互后：点 button「添加规则」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 规则表单·协议下拉（TCP / UDP / 两者） | 无；建议 combobox「协议」 〔交互后：点 button「添加规则」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 规则表单·「已启用」开关 | switch「已启用」 〔交互后：点 button「添加规则」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 「保存规则」 | button「保存规则」 〔交互后：点 button「添加规则」〕 | POST `/api/router/firewall/port-forward` `{action:"add", name, protocol, wan_port, lan_ip, lan_port, enabled:"1"/"0"}`（ubus `router_set_portforward`；**未确认**固件接受这个 `action` 字段格式） | 1 | `/api/router/firewall/port-forward` 出现新行 | 无 | 二·远程三 |
| 规则行垃圾桶图标 | button「删除规则」（每行同名；建议加规则名） 〔现名：button「删除规则{规则名}」（带规则名，如「删除规则「NAS HTTPS」」）〕 | 无（进入页内确认） | — | — | 无 | —（本地） |
| 删除页内确认「删除」 | button「删除」 〔现名：button「确认：{动作}」〕〔交互后：点 button「删除规则{规则名}」〕 | POST `/api/router/firewall/port-forward` `{action:"delete", id}` | 1 | `/api/router/firewall/port-forward` 该行消失 | 自定义确认：「删除规则 "{名称}"？」 | 二·远程三 |
| 删除页内确认「取消」 | button「取消」 〔交互后：删除的页内确认里；点 button「删除规则{规则名}」〕 | 无 | — | — | 无 | —（本地） |

写接口假成功风险：全部「否·透传」（router.rs:100-189），页面不看 `data.result`。

---

## /router/home-mode 在家模式（回家模式）

文件：`app/(panel)/router/home-mode/page.tsx`。**这个路由不在侧栏 NAV 里**（NAV 里只有「情景模式」`/router/scenario`；`nav.homeMode` 这个键只用在首页和登录页的服务行标题上），只能直接输网址进入。页面顶部写着「在家模式已被情景模式取代，两者不能同时运行。」

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 页头状态（已暂停 / Wi-Fi 已关闭 — 附近有家庭网络 / Wi-Fi 已开启） | `/api/homemode` | `enabled`、`wifi_off`（= `mode == "home"`，读 `/data/homemode/state`） | 15000 | 否（homemode.rs:103，读本地文件） |
| 页头开关状态 | `/api/homemode` | `enabled`（`/data/homemode/disabled` 不存在） | 15000 | 否 |
| 「正在使用内置默认值…」 | `/api/homemode` | `using_default` | 15000 | 否 |
| 家庭 SSID 列表 | `/api/homemode` | `ssids[]` | 15000 | 否 |
| 「未配置任何 SSID — 在家模式将永不触发。」 | `/api/homemode` | `ssids` 为空 | 15000 | 否 |
| 复查间隔 / 恢复前错过次数 输入框初值 | `/api/homemode` | `check_every`、`exit_misses`（只在第一次拿到数据时填入） | 15000 | 否 |
| 「离开后 Wi-Fi 大约会在 N 分钟…」 | 本地计算 | `every × misses` | — | — |
| 附近网络列表：SSID + 信号 dBm（已在列表里的隐藏） | `/api/homemode/scan`（GET，**会动 Wi-Fi**，见汇总） | `networks[].ssid`、`networks[].signal`；页面还读 `note`，**agent 不返回 `note`**（返回的是 `woke_radio`，页面没用） | 无（点「扫描」时） | 否（homemode.rs:235-303） |
| 「正在唤醒 2.4 GHz 射频以扫描…」 | `/api/homemode` | 扫描中且 `wifi_off` | — | — |
| 切换事件日志（最新在前） | `/api/homemode/log` | `events`（`/data/log/homemode.log` 末 200 行） | 10000 | 否 |
| 扫描活动日志 | `/api/homemode/log` | `scans` | 10000 | 否 |
| 接口里有、页面没显示 | `/api/homemode` | `default_ssids`、`mode` | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 页头开关（开/关） | switch「开」/「关」（随状态变）；建议 switch「在家模式」 | PUT `/api/homemode` `{enabled: bool}`（写/删 `/data/homemode/disabled`） | 1 | 返回体即新状态；`/api/homemode` `enabled` | 无 | 二·远程三（开启后设备会在检测到家时自己关 Wi-Fi，等同 Wi-Fi 开关） |
| 「前往情景模式 →」 | link「前往情景模式 →」 〔现名：link「前往情景模式 →{说明}」（链接名带后面的说明文字）〕 | 无（导航） | — | — | 无 | —（本地） |
| 添加 SSID 输入框（回车提交） | textbox「添加家庭 SSID」（有 `aria-label`） | 无（草稿） | — | — | 无 | —（本地） |
| 「添加」 | button「添加」 | PUT `/api/homemode` `{ssids:[...原列表, 新]}`（整表覆盖写 `/data/homemode/ssids`） | 1 | `/api/homemode` `ssids` | 无 | 二 |
| SSID 行垃圾桶 | button「移除 {SSID}」 | PUT `/api/homemode` `{ssids: 去掉该项}` | 1 | `/api/homemode` `ssids` | 无（事后给「撤销」） | 二 |
| 提示条「撤销」 | button「撤销」 〔交互后：只在刚增删一个家庭 SSID 之后出现（要先写一次，未自动验证）〕 | PUT `/api/homemode` `{ssids: 原列表}` | 1 | `/api/homemode` `ssids` | 无 | 二 |
| 「扫描」/「扫描中…」/「正在唤醒 Wi-Fi…」 | button「扫描」 | GET `/api/homemode/scan`；Wi-Fi 被在家模式关着时，agent 会临时 `uci set wireless.wifi0.disabled=0` + commit + `zwrt_wlan reload`，扫完再改回并 reload | 1 个 GET，agent 内部最多 2 次 uci 写 + 2 次 reload | 无读回 | 无 | 二（GET 但改设备状态） |
| 附近网络行「+」 | button「添加 {SSID}」 〔交互后：扫描之后的结果行；点 button「扫描」，点 button「确认：{x}」〕 | PUT `/api/homemode` `{ssids:[..., 该 SSID]}` | 1 | `/api/homemode` `ssids` | 无 | 二 |
| 复查间隔（分钟）数字框 | spinbutton「复查间隔」（`<label>` 包住，有名称） 〔现名：spinbutton「复查间隔 分钟 · 1–60」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 恢复 Wi-Fi 前的错过次数 数字框 | spinbutton「恢复 Wi-Fi 前的错过次数」 〔现名：spinbutton「恢复 Wi-Fi 前的错过次数 次复查 · 1–30」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 检测设置「保存」 | button「保存」 | PUT `/api/homemode` `{check_every(1–60), exit_misses(1–30)}` | 1 | `/api/homemode` `check_every`、`exit_misses` | 无 | 二 |
| 切换事件「刷新」 | button「刷新」 | GET `/api/homemode/log` | — | — | 无 | —（本地，只读） |

写接口假成功风险：PUT `/api/homemode` **是**——`enabled` 的写/删标志文件结果被丢掉（homemode.rs:168-173），失败也回 ok；SSID 和检测参数写失败会报 500。

---

## /router/lan 局域网 / DHCP

文件：`app/(panel)/router/lan/page.tsx`。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| LAN IP 地址初值 | `/api/router/lan` | `lan_ipaddr`（`uci network.lan.ipaddr`） | 无 | 是：router.rs:55-60 每个 `uci get` 失败都变空串，照样 ok |
| 子网掩码初值 | `/api/router/lan` | `lan_netmask` | 无 | 是：同上 |
| DHCP 开关初值 | `/api/router/lan` | `dhcp_enable`（`dhcp.lan.ignore` 为 "1" 时为 "0"） | 无 | 是：同上（读不到 ignore 时显示为开） |
| 起始地址初值 | `/api/router/lan` | `dhcp_start`——agent 返回的是 `uci dhcp.lan.start` **原值（主机号，如 "100"）**，页面占位符却是完整 IP `192.168.0.100` | 无 | 是：同上 |
| 结束地址初值 | `/api/router/lan` | `dhcp_end`——agent **算出来的完整 IP**（前三段 + start+limit−1，router.rs:70-80） | 无 | 是：读不到时按 start=100、limit=50 算 |
| 租约时间（秒）初值 | `/api/router/lan` | `dhcp_lease_time`——`uci dhcp.lan.leasetime` 原值（OpenWrt 常见 "12h" 这种带单位的写法；若如此页面校验「必须为正整数」会拦住提交，**未确认**设备上的实际值） | 无 | 是：同上 |
| 加载错误 / 表单校验错误横幅 | — | — | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| LAN IP 地址输入框 | 无（label 未关联）；建议 textbox「LAN IP 地址」 | 无（草稿） | — | — | 无 | —（本地） |
| 子网掩码输入框 | 无；建议 textbox「子网掩码」 | 无（草稿） | — | — | 无 | —（本地） |
| DHCP 服务器开关 | switch「已启用」/「已禁用」（随状态变）；建议 switch「DHCP 服务器」 | 无（草稿） | — | — | 无 | —（本地） |
| 起始地址 / 结束地址 / 租约时间（秒）输入框（DHCP 关时禁用） | 无；建议 textbox「起始地址」「结束地址」「租约时间（秒）」 | 无（草稿） | — | — | 无 | —（本地） |
| 「应用」 | button「应用」 | 无（页面校验 IP/掩码/租约，然后弹出页内确认） | — | — | 无 | —（本地） |
| 页内确认「仍然应用」 | button「仍然应用」 〔现名：button「确认：应用局域网设置」（统一的页内确认）〕〔交互后：改了设置再按「应用」；填 textbox「租约时间」=12h，点 button「应用」〕 | PUT `/api/router/lan` `{lan_ipaddr, lan_netmask, dhcp_enable:"1"/"0", dhcp_start, dhcp_end, dhcp_lease_time}`（ubus `zwrt_router.api router_set_lan_para`，原样透传） | 1 | `/api/router/lan` 各字段（注意读和写的 `dhcp_start` 格式可能不一致，见上） | 自定义确认（非 window.confirm）：「更改 LAN 设置将断开客户端连接。是否继续？」 | 二·远程三 |
| 页内确认「取消」 | button「取消」 〔交互后：页内确认里；填 textbox「租约时间」=12h，点 button「应用」〕 | 无 | — | — | 无 | —（本地） |

写接口假成功风险：PUT `/api/router/lan` 否·透传（router.rs:82-91）。

---

## /router/mobile-network 移动网络

文件：`app/(panel)/router/mobile-network/page.tsx`。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 连接·状态（含 connected 时绿色） | `/api/modem/data` | `connect_status`（ubus `zwrt_data get_wwaniface {cid:1}`） | 5000 | 否·透传（modem_ext.rs:6） |
| 移动数据开关状态 | `/api/modem/data` | `enable` | 5000 | 否·透传 |
| 漫游开关状态 | `/api/modem/data` | `roam_enable` | 5000 | 否·透传 |
| （写入时回填用）连接方式 | `/api/modem/data` | `connect_mode` | 5000 | 否·透传 |
| 飞行模式开关状态 | `/api/modem/status` | `operate_mode`（不是 `"ONLINE"` 即视为飞行模式开；`uci zte_nwinfo.sys_info.operate_mode`） | 5000 | 否（uci 失败回 503，handlers.rs:129） |
| 「注意：关闭飞行模式后如果调制解调器无法恢复…」 | — | 飞行模式关时显示 | — | — |
| 运营商扫描结果表：运营商 / MCC/MNC / 制式 / 当前·禁止 | `/api/netinfo` | `scan.operators[].name/plmn/rat/status`（agent 解析模组字符串 `状态,名字,PLMN,制式;`，2026-09-25 实测：状态 1 可用 2 当前 3 禁止，制式 2/7/11 = 3G/4G/5G） | 扫描进行中每 3000 | 否·agent 任务（netinfo.rs operator_scan） |
| 扫描进度判断 | `/api/netinfo` | `scan.state`（scanning / done / error；模组状态 `manual_selecting` → `manual_selected`，实测约 110 秒） | 页面循环 3000，最多 80 次 | 否·agent 任务 |
| 注册结果（已注册到 X / 没注册上已回到自动 / 注册超时） | `/api/modem/register/guard` | `phase`（registering → ok / reverting → reverted）、`reason`；失败时模组 `m_netselect_result` 为 "0"（实测） | 页面循环 3000，最多 40 次 | 否·agent 保护任务 |
| 数据加载错误横幅 | `/api/modem/data` | SWR `error` | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「移动数据」开关 | 无（`Toggle` 没传 label）；建议 switch「移动数据」 | PUT `/api/modem/data` `{cid:1, connect_mode, roam_enable, enable:1/0}`，关闭时多带 `connect_status:"disconnected"`（ubus `zwrt_data set_wwaniface`） | 1 | `/api/modem/data` `enable`、`connect_status` | 无 | 二（远程访问时关掉会断开远程连接，建议评审是否按「二·远程三」处理） |
| 「漫游」开关 | 无；建议 switch「漫游」 | PUT `/api/modem/data` `{cid:1, connect_mode, roam_enable:1/0, enable}` | 1 | `/api/modem/data` `roam_enable` | 无 | 二 |
| 「飞行模式」开关——打开 | 无；建议 switch「飞行模式」 | POST `/api/modem/airplane` `{operate_mode:"LPM"}`（ubus `nwinfo_set_mode`） | 1 | `/api/modem/status` `operate_mode` ≠ "ONLINE" | 「启用飞行模式？这将关闭蜂窝无线电。」 | 三（飞行模式） |
| 「飞行模式」开关——关闭 | 同上 〔现名：switch「飞行模式」（开、关是同一个开关）〕 | POST `/api/modem/online`（agent 发 `AT+CFUN=1`，回复不含 OK 回 500） | **最多 2 步**：① POST `/api/modem/online` ② ① 失败时等 3 秒再发一次 | `/api/modem/status` `operate_mode` = "ONLINE" | 无（关闭方向不确认） | 三（飞行模式） |
| 「扫描运营商」/「扫描中…」 | button「扫描运营商」 | POST `/api/netinfo/scan`（旧 `/api/modem/scan` 也走同一任务；ubus `nwinfo_manual_scan`） | **多请求**：① POST 扫描 → 每 3 秒 GET `/api/netinfo` 看 `scan` 最多 80 次 | 结果表有行 | 无 | 二（搜索期间移动数据断开约 110 秒，实测） |
| 结果行「选择」（注册到该运营商） | button「选择」（每行同名；`aria-label`「注册到 {运营商}」） 〔现名：button「注册到 {运营商}」〕〔交互后：扫描运营商之后的结果行；点 button「扫描运营商」，点 button「确认：{x}」〕 | POST `/api/modem/register` `{m_mcc_mnc, m_rat}`（202；agent 先写保护标记，再调 ubus `nwinfo_manual_register`） | **多请求**：① POST 注册 → 每 3 秒 GET `/api/modem/register/guard` 最多 40 次 | 保护的 `phase`；注册不上时 agent 用 `AT+COPS=0` 回到自动（实测 44 秒） | 无 | 三（选网方式：手动选网） |
| 「重启」（设备控制卡） | button「重启」 | POST `/api/device/reboot` | 1 | 无读回 | 「现在重启路由器？」 | 三（重启） |

写接口假成功风险：`/api/modem/data` PUT、`/api/modem/airplane`（LPM）、scan、register、reboot 为「否·透传」；`/api/modem/online` 为「否」（检查 AT 回复里的 OK，handlers.rs:171-178）。

---

## /router/network-mode 网络模式

文件：`app/(panel)/router/network-mode/page.tsx`。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 页头「当前:{模式}」+ 选项上的「使用中」标记 + 单选初值 | `/api/network/signal` | `net_select`（制式偏好，B27 实测 `WL_AND_5G` = 自动；9-25 起本页读它。`net_select_mode` 是选网方式 `auto_select` / `manual_select`，手动时页头下方加一句说明） | 5000 | 否·透传 |
| 错误横幅 | `/api/network/signal` | SWR `error` | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 模式单选 6 项，顺序和名字照原厂网页（`/usr/zte_web/web/js/config/ufi/U60Pro/config.js` 的 `AUTO_MODES`）：「5G/4G/3G（自动）」「只用 5G NSA」「只用 5G SA」「4G/3G」「只用 4G」「只用 3G」（值 `WL_AND_5G` / `LTE_AND_5G` / `Only_5G` / `WCDMA_AND_LTE` / `Only_LTE` / `Only_WCDMA`，与原厂相同；原来的 `auto_select`、`5G_only` 这类值模组不认） | radio，名称=标签 〔现名：radio「5G/4G/3G（自动）」「只用 5G NSA」「只用 5G SA」「4G/3G」「只用 4G」「只用 3G」（名称只用标签）〕 | 无（草稿） | — | — | 无 | —（本地） |
| 「应用」（未改动时禁用） | button「应用」 | PUT `/api/modem/network-mode` `{net_select}`（agent 只收固件的 14 个值，其他回 400；ubus `nwinfo_set_netselect`。确认弹窗里「只用 5G SA」「只用 3G」另有一段加粗的国外提醒：很多国家没有 SA / 已关 3G，设备又没有 2G） | 1 个写请求，之后每 2 秒 GET `/api/network/signal` 最多 5 次确认 | `/api/network/signal` `net_select` = 所选值（页面自带读回；超时提示「已下发 —— 路由器可能仍在切换」） | 无 | 三（选网方式） |

写接口假成功风险：否·透传（modem_ext.rs `modem_network_mode_set`，先校验取值），但页面有自己的读回轮询。

---

## /router/qci QCI / 承载

文件：`app/(panel)/router/qci/page.tsx`。只读。注意 `/api/network/qos` 每次都会在 AT 口上发 `AT+CGCONTRDP` 以及每个 cid 一条 `AT+CGEQOSRDP=<cid>`（qos.rs:18-70），本页每 5 秒轮询一次，会持续占用 AT 口。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 承载表·CID | `/api/network/qos` | `contexts[].cid` | 5000 | 否（CGCONTRDP 失败回 503，qos.rs:19-22） |
| 承载表·承载 | `/api/network/qos` | `contexts[].bearer_id` | 5000 | 否 |
| 承载表·APN | `/api/network/qos` | `contexts[].apn` | 5000 | 否 |
| 承载表·QCI / 5QI + 含义 | `/api/network/qos` | `contexts[].qci` ?? `qci_device` ?? `qci_inferred`；含义查页面内置表（1–9、65、66、69、70、79、80） | 5000 | 否 |
| 承载表·来源（实测 / 来自调制解调器 / 根据 APN 推断） | `/api/network/qos` | `qci != null` → 实测；`qci_device != null` → 来自调制解调器；否则推断 | 5000 | 否 |
| 承载表·DL · UL GBR | `/api/network/qos` | `contexts[].dl_gbr_kbps`、`ul_gbr_kbps`（MBR 字段有但页面没显示） | 5000 | 否 |
| 表下说明文字 | `/api/network/qos` | `note`（agent 写死的英文）+ 页面补充说明 | 5000 | 否 |
| 原始 AT+CGCONTRDP 输出（展开后） | `/api/network/qos` | `raw_cgcontrdp` | 5000 | 否 |
| 「没有活动的 PDP 上下文。」/ 错误条 | `/api/network/qos` | `contexts` 为空 / SWR `error` | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「显示原始 AT+CGCONTRDP」/「隐藏原始 AT+CGCONTRDP」 | button「显示原始 AT+CGCONTRDP」 | 无（展开） | — | — | 无 | —（本地） |

---

## /router/qos QoS

文件：`app/(panel)/router/qos/page.tsx`。2026-09-25 起只读：原厂「带宽分配」除极速外都靠 xdpi 识别应用，xdpi 为省 CPU 已关，用户决定不做切换（manager `docs/designs/touch-menu-tabs.md`「不做」）。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 「带宽分配：极速（不区分应用）」等 | `/api/router/qos` | `smart_qos_mode`（uci `zwrt_smart_mng.smart_qos.mode`，agent 加的） | 30000 | — |
| 应用识别（xdpi）已关 / 已开 | `/api/router/qos` | `xdpi_support`（uci `zwrt_smart_mng.smart_mng.xdpi_support`） | 30000 | — |
| 「这个固件不支持」（旧 agent） | `/api/router/qos` | 503 Method not found | 不重试 | — |

#### 控件

无（只读）。旧的 QoS 开关（`router_set_qos_switch`）B27 上不存在，已去掉。

---

## /router/scenario 情景模式

文件：`app/(panel)/router/scenario/page.tsx`。

> **重要（从代码推断，未在设备上验证）：本页所有写请求都是双重 JSON 编码。** 页面写的是 `apiFetch(path, { body: JSON.stringify(x) })`，而 `apiFetch` 自己又会 `JSON.stringify(opts.body)`（`lib/api/client.ts:66`），所以发出去的请求体是一个 **JSON 字符串字面量**（如 `"{\"id\":\"home\"}"`），不是对象。agent 端的后果：
> - PUT `/api/scenario`：`serde_json::from_slice::<Config>` 解析字符串失败 → 回 400 `invalid config: …`。**保存 SSID、参数、每个情景的 Wi-Fi/节点、创建默认情景、添加国外情景都会报错，改不进去。**
> - POST `/api/scenario/pin`：解析成 `Value::String`，`.get("id")` 为 None → 走「取消固定」分支（scenario.rs:1666）。**点任何「固定」按钮实际效果都是取消固定**，回 ok，页面还提示「已固定」。
> - PUT `/api/scenario/enabled`：`.get("enabled")` 为 None → `unwrap_or(true)` → **永远是打开引擎**（scenario.rs:1684）。关引擎时页面提示「引擎已关闭 — Wi-Fi 已恢复」，实际引擎仍开着。这是典型假成功。
> 改版时要修掉（去掉页面里的 `JSON.stringify`），并在 e2e 里用读回验证。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 页头「引擎开启」开关状态 | `/api/scenario` | `enabled` | 10000 | 否（scenario.rs:1614，内存 + 标志文件） |
| 当前状态·情景（名称，在家时 accent 色） | `/api/scenario` | `current`（查 `config.scenarios[].name`） | 10000 | 否 |
| 当前状态·上次切换 / 上次扫描 | `/api/scenario` | `last_switch`、`last_scan`（设备时钟，`fmtDevice`） | 10000 | 否 |
| 当前状态·SIM 归属地代码 | `/api/scenario` | `sim_mcc`（null 显示「未知」） | 10000 | 否 |
| 当前状态·固定为（情景 id，点了取消固定；未固定显示「自动」） | `/api/scenario` | `pin` | 10000 | 否 |
| 「看到了「X」— 已确认 N / M 次。」 | `/api/scenario` | `candidate`、`hits`、`config.params.enter_hits` | 10000 | 否 |
| 看门狗接管提示 | `/api/scenario` | `guard_takeover` | 10000 | 否 |
| 最近错误 | `/api/scenario` | `last_error` | 10000 | 否 |
| 固定按钮列（每个情景一个，当前固定的高亮） | `/api/scenario` | `config.scenarios[].id/name`、`pin` | 10000 | 否 |
| 代表「在家」的网络列表：SSID + BSSID（无则「任何使用这个名称的热点」） | `/api/scenario` | `config.scenarios[id=home].detect.entries[].ssid/bssid` | 10000 | 否 |
| 只按名称匹配的警告（N 条） | `/api/scenario` | 上述 entries 中没有 `bssid` 的条数 | 10000 | 否 |
| 附近的网络：SSID / BSSID / 信号 dBm（已在列表里的隐藏） | `/api/scenario/scan`（GET，会临时建虚拟网卡，见汇总） | `networks[].ssid/bssid/signal` | 无（点「扫描」时） | 否（失败回 503，scenario.rs:1731-1740） |
| 国外情景列表：名称 + MCC 列表 / 「所有外国 SIM」 | `/api/scenario` | `config.scenarios[]` 中 `detect.type` 为 `mcc`/`abroad` 的；`detect.mccs` | 10000 | 否 |
| 每个情景做什么：名称 / Wi-Fi 开关状态（外出=「始终开」） | `/api/scenario` | `config.scenarios[].actions[]` 中 `path=/api/wifi/radio` 的 `body.ap_2g`；`detect.type=fallback` | 10000 | 否 |
| 反应快慢 6 个输入框初值 | `/api/scenario` | `config.params.scan_interval_away_charging_secs/scan_interval_away_battery_secs/scan_interval_home_secs/enter_hits/exit_misses/min_rssi_dbm`（只填一次） | 10000 | 否 |
| 运行记录 | `/api/scenario/log` | `log`（末若干行） | 15000 | 否 |
| 「还没设置」卡（没有任何情景时整页只显示这张） | `/api/scenario` | `config.scenarios.length === 0` | 10000 | 否 |
| 整页错误（带 Retry） | `/api/scenario` | SWR `error` | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 页头「引擎开启」开关 | switch「引擎开启」 | PUT `/api/scenario/enabled` `{enabled}`——**双重编码，agent 永远当 true** | 1；agent 关引擎时还会同步跑 `bootsafe` + `run_restores`（恢复外出情景、开 Wi-Fi） | `/api/scenario` `enabled` | 无 | 二 |
| 「创建默认情景」（未设置时） | button「创建默认情景」 〔交互后：只在还没有任何情景时出现（模拟数据已配置好情景，未自动验证）〕 | 见步骤 | **2 请求**：① GET `/api/scenario/template` ② PUT `/api/scenario` 整份模板（双重编码 → 400） | `/api/scenario` `config.scenarios` 非空 | 无 | 二 |
| 当前状态「固定为」里的已固定情景（点了取消固定） | button「{情景 id}」（只有图标 + id 文本；建议 `aria-label`「取消固定」） | POST `/api/scenario/pin` `{id:null}`（双重编码，但结果恰好也是取消固定） | 1 | `/api/scenario` `pin` = null | 无 | 二（情景固定/取消固定） |
| 固定按钮（每个情景一个，再点一次取消） | button「{情景名}」（图标 + 名称；当前固定的用 primary 样式，无 `aria-pressed`） | POST `/api/scenario/pin` `{id}` 或 `{id:null}`——**双重编码，实际永远取消固定** | 1 | `/api/scenario` `pin` | 无 | 二（情景固定/取消固定） |
| 在家网络行垃圾桶 | button「移除 {SSID}」 | PUT `/api/scenario` 整份 config（去掉该 entry；双重编码 → 400） | 1 | `/api/scenario` `config.scenarios[home].detect.entries` | 无 | 二 |
| 附近的网络「扫描」/「扫描中…」 | button「扫描」 | GET `/api/scenario/scan` | 1 | — | 无 | 二（GET 但动网卡） |
| 附近网络行「+」 | button「添加 {SSID}」 〔交互后：扫描之后的结果行；点 button「扫描」，点 button「确认：{x}」〕 | PUT `/api/scenario` 整份 config（加 `{ssid, bssid}`；双重编码 → 400） | 1 | `/api/scenario` 在家 entries | 无 | 二 |
| 「添加国外情景」（缺国外情景时） | button「添加国外情景」 〔交互后：只在缺国外情景时出现（模拟数据里已有，未自动验证）〕 | 见步骤 | **2 请求**：① GET `/api/scenario/template` ② PUT `/api/scenario`（原 config + 模板里缺的国外情景；双重编码 → 400） | `/api/scenario` 出现 `detect.type=mcc/abroad` 的情景 | 无 | 二 |
| 每个情景的「Wi-Fi」开关（外出情景没有，显示「始终开」） | switch「开」/「关」（随状态变，且每行同名）；建议 switch「{情景名} 的 Wi-Fi」 〔现名：switch「{前}的 Wi-Fi」（如「「在家」的 Wi-Fi」）〕 | PUT `/api/scenario` 整份 config，把该情景的 `/api/wifi/radio` 动作改成 `{ap_2g:on, ap_5g:on}` 并放到第一位（双重编码 → 400） | 1 | `/api/scenario` 该情景 actions | 无 | 二·远程三（改的是「进入该情景时开/关 Wi-Fi」，不是立即开关；但会导致之后自动关 Wi-Fi） |
| 反应快慢 6 个输入框：充电时扫描间隔（秒）/ 用电池时扫描间隔（秒）/ 在家时扫描间隔（秒）/ 进入需确认次数 / 离开需确认次数 / 无硬件地址时的信号下限（dBm） | textbox，名称即各自标签（`<label>` 包住） 〔现名：textbox「充电时扫描间隔（秒）」「用电池时扫描间隔（秒）」「在家时扫描间隔（秒）」「进入需确认次数」「离开需确认次数」「无硬件地址时的信号下限（dBm）」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 反应快慢「保存」 | button「保存」 | PUT `/api/scenario` 整份 config 换 `params`（页面先校验都是数字、确认次数 ≥1；双重编码 → 400） | 1 | `/api/scenario` `config.params` | 无 | 二 |
| 运行记录刷新图标 | 无（只有图标）；建议 button「刷新记录」 | GET `/api/scenario/log` | — | — | 无 | —（本地，只读） |
| 整页错误「Retry」 | button「Retry」 〔现名：button「重试」〕〔交互后：读取失败时（错误状态）；场景 down〕 | 重拉 `/api/scenario` | — | — | 无 | —（本地，只读） |

写接口假成功风险：
- PUT `/api/scenario`：**是**——scenario.rs:1631 `write_json` 写盘失败不报（scenario.rs:586-593 只在 `fs::write` 成功时 rename，失败静默），内存已改、回 ok，重启后丢。
- POST `/api/scenario/pin`：**是**——scenario.rs:1662、1666 写/删 pin 文件结果被丢掉。
- PUT `/api/scenario/enabled`：**是**——scenario.rs:1687、1690 标志文件写/删结果被丢掉；再加上本页的双重编码问题。

---

## /router/schedule 定时重启

文件：`app/(panel)/router/schedule/page.tsx`。只是一张说明卡，指向 `/scheduler`；不调任何接口（代码注释：`/api/modem/schedule-reboot` 不存在）。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 说明文字「周期性的定时重启通过计划任务进行管理…POST /api/device/reboot…」 | 无（静态） | — | — | 不适用 |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「打开计划任务」 | link「打开计划任务」→ `/scheduler` | 无（导航） | — | — | 无 | —（本地） |

---

## /router/signal-detect 信号检测

文件：`app/(panel)/router/signal-detect/page.tsx`。进页面不拉任何数据。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 进度 N% + 进度条（检测中） | `/api/cell/signal-detect/progress` | `progress`（数字或字符串，≥100 视为完成） | `setInterval` 2000（检测中） | 否·透传（cell.rs:152） |
| 检测结果表（列 = 返回对象的键，动态生成） | `/api/cell/signal-detect/results` | 页面猜结构：`cells[]`/`results[]`/`list[]`，都没有时整个对象当一行（**未确认**） | 无（完成或停止后拉一次；失败静默） | 否·透传（cell.rs:145） |
| 「暂无可用结果。」 | 同上 | 为空 | — | — |
| 操作提示（检测已开始/已停止/完成） | 写接口返回 | — | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「开始检测」 | button「开始检测」 | POST `/api/cell/signal-detect/start`（ubus `nwinfo_start_detect_signal_quality`） | 1，之后每 2 秒 GET progress | `/api/cell/signal-detect/progress` `progress` 开始变化 | 无 | 二 |
| 「停止检测」（检测中） | button「停止检测」 〔交互后：检测进行中才有；点 button「开始检测」，点 button「确认：{x}」〕 | POST `/api/cell/signal-detect/stop` | **2 请求**：① POST stop ② GET `/api/cell/signal-detect/results` | 结果表 | 无 | 二 |

写接口假成功风险：start / stop 否·透传（cell.rs:131-143）。

---

## /router/sim SIM / PIN

文件：`app/(panel)/router/sim/page.tsx`。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 状态 | `/api/sim/info` | `sim_states` | 无 | 否·透传（sim.rs:6） |
| 调制解调器状态 | `/api/sim/info` | `modem_main_state` | 无 | 否·透传 |
| PIN 锁（已启用/已禁用/原值） | `/api/sim/info` | `pin_status`（"1"/"0"） | 无 | 否·透传 |
| IMSI / ICCID | `/api/sim/info` | `sim_imsi`、`sim_iccid` | 无 | 否·透传 |
| PIN 剩余尝试次数 / PUK 剩余尝试次数（有值才显示） | `/api/sim/info` | `pinnumber`、`puknumber` | 无 | 否·透传 |
| 剩余尝试次数（SIM 锁 / NCK） | `/api/sim/lock-trials` | `available_trials` | 无 | 否·透传（sim.rs:64） |
| 「SIM 已被 PIN 锁定…」/「SIM 已被 PUK 锁定…」警告 | `/api/sim/info` | `sim_states` 或 `modem_main_state` 为 `wait pin`/`modem_waitpin`（PUK 同理） | 无 | 否·透传 |
| 加载错误横幅 | info / lock-trials | SWR `error` | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「刷新」（页头） | button「刷新」 | 重拉 `/api/sim/info`、`/api/sim/lock-trials` | — | — | 无 | —（本地，只读） |
| 「验证 PIN」（PIN 锁定时） | button「验证 PIN」 〔交互后：只在 SIM 被 PIN 锁住时出现（模拟 SIM 就绪，未自动验证）〕 | 无（打开面板） | — | — | 无 | —（本地） |
| 「验证 PUK」（PUK 锁定时） | button「验证 PUK」 〔交互后：只在 SIM 被 PUK 锁住时出现（模拟 SIM 就绪，未自动验证）〕 | 无（打开面板） | — | — | 无 | —（本地） |
| 「开启 PIN 锁」/「关闭 PIN 锁」（未锁定时） | button「开启 PIN 锁」/「关闭 PIN 锁」 | 无（打开面板） | — | — | 无 | —（本地） |
| 「修改 PIN」（未锁定时，打开面板） | button「修改 PIN」 | 无（打开面板） | — | — | 无 | —（本地） |
| 「网络解锁」 | button「网络解锁」 | 无（打开面板） | — | — | 无 | —（本地） |
| 各面板 PIN / PUK / 新 PIN / 当前 PIN / 确认当前 PIN 输入框（密码型，只收数字） | 无（label 未关联）；建议 textbox 用各自标签名 〔现名：textbox「当前 PIN」「新 PIN」「确认当前 PIN」「PIN」「PUK」〕〔交互后：点 button「修改 PIN」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 验证 PIN 面板「提交」 | button「提交」 〔交互后：只在 SIM 被 PIN 锁住时出现（未自动验证）〕 | POST `/api/sim/pin/verify` `{pin_num, puk_num:"", pin_encode_flag:"0"}`（ubus `sim_verify_pin_puk`） | 1，之后重拉 info | `/api/sim/info` `sim_states` 不再是 wait pin | 无 | 二（输错会消耗尝试次数，建议复核） |
| 验证 PUK 面板「提交」 | button「提交」 〔交互后：只在 SIM 被 PUK 锁住时出现（未自动验证）〕 | POST `/api/sim/pin/verify` `{pin_num:新PIN, puk_num, pin_encode_flag:"0"}` | 1 | `/api/sim/info` `sim_states` | 无 | 二（建议复核：PUK 输错次数用完 SIM 永久锁死） |
| 修改 PIN 面板「修改 PIN」 | button「修改 PIN」（与打开面板的按钮同名） | POST `/api/sim/pin/change` `{pin_num:旧, new_pin_num:新, pin_encode_flag:"0"}` | 1（成功后**不**重拉 info） | 无直接读回（PIN 值不可读） | 无 | 二 |
| PIN 锁面板「确认」 | button「确认」 〔交互后：点 button「开启 PIN 锁」〕 | POST `/api/sim/pin/mode` `{pin_num_m, pin_mode:"1"/"0", pin_encode_flag:"0"}` | 1 | `/api/sim/info` `pin_status` | 无 | 二 |
| NCK 面板·解锁码输入框 | 无；建议 textbox「解锁码 (NCK)」 〔交互后：点 button「网络解锁」〕 | 无（草稿） | — | — | 无 | —（本地） |
| NCK 面板「解锁 SIM」 | button「解锁 SIM」 〔交互后：点 button「网络解锁」〕 | POST `/api/sim/unlock` `{nck}`（ubus `set_simlock_nck`） | 1 | `/api/sim/lock-trials` `available_trials` / SIM 状态 | 无（只有面板里的警告文字「此操作不可逆…」） | 二（建议复核升三：页面自己写着不可逆） |
| 各面板「取消」 | button「取消」 〔交互后：点 button「修改 PIN」〕 | 无 | — | — | 无 | —（本地） |

写接口假成功风险：全部「否·透传」（sim.rs:20-62），页面不看 `data.result`——PIN 输错时固件可能返回失败码而 agent 仍回 ok，页面会提示「PIN 已验证」。

---

## /router/stc STC

文件：`app/(panel)/router/stc/page.tsx`。页面注释写着「/api/cell/stc/* returns 503 currently」。agent 背后的 ubus 方法名是 `nwinfo_stc_cell_lock_enable/disable/reset`、`nwinfo_*_stc_white_list_*`，即**按白名单锁小区**。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 「STC 已启用」开关状态 + 当前状态（已启用/已禁用） | `/api/cell/stc/status` | `enabled`（布尔 / "1" / "true" / "enabled" / 非 0 数字都算开，**未确认**固件真实字段名） | 无 | 否·透传（cell.rs:103） |
| LTE 采集定时器 / NR-SA 采集定时器 / LTE 白名单上限 / NR-SA 白名单上限 初值 | `/api/cell/stc/params` | `lte_collect_timer`、`nrsa_collect_timer`、`lte_whitelist_max`、`nrsa_whitelist_max`（未确认） | 无 | 否·透传（cell.rs:85） |
| 「STC 服务当前不可用（503）。控件已禁用。」 | status / params | SWR `error` | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「重置白名单」（页头，红色；服务不可用时**不**禁用） | button「重置白名单」 | POST `/api/cell/stc/reset`（ubus `nwinfo_stc_cell_lock_reset`） | 1 | `/api/cell/stc/status`（白名单本身无读取接口） | 「重置 STC 白名单？此操作无法撤销。」 | 三（STC 白名单重置） |
| 「STC 已启用」开关 | 无（`Toggle` 没传 label）；建议 switch「STC」 | POST `/api/cell/stc/enable` 或 `/api/cell/stc/disable`（无请求体） | 1 | `/api/cell/stc/status` `enabled` | 无 | 三（按白名单锁小区） |
| 4 个参数输入框 | 无（label 未关联）；建议 textbox「LTE 采集定时器」「NR-SA 采集定时器」「LTE 白名单上限」「NR-SA 白名单上限」 | 无（草稿） | — | — | 无 | —（本地） |
| 「应用参数」 | button「应用参数」 | PUT `/api/cell/stc/params` `{lte_collect_timer, nrsa_collect_timer, lte_whitelist_max, nrsa_whitelist_max}`（字符串） | 1 | `/api/cell/stc/params` 各字段 | 无 | 二 |

写接口假成功风险：全部「否·透传」（cell.rs:92-129）。

---

## /router/stk USSD 与 SIM 工具包

文件：`app/(panel)/router/stk/page.tsx`（`USSDSection`、`STKSection`）。进页面不拉数据。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| USSD 回复正文 | `/api/ussd/send`、`/api/ussd/respond` 的返回 | `response`（agent 已按 DCS 解码） | — | send 否（回复含 ERROR 时 500，telephony.rs:395-397）；respond **是**（telephony.rs:430-441 不检查 ERROR，原样当回复返回 ok） |
| USSD 状态 + 「会话进行中」 | 同上 | `status`、`session_active` | — | 同上 |
| STK 菜单标题 + 面包屑 | `/api/stk/menu`、`/api/stk/select` | `title`（选项返回 `type:"menu"` 时是子菜单） | 无（点「加载 STK 菜单」时） | 否 |
| STK 菜单项列表 | 同上 | `items[].id`、`items[].label` | — | 否 |
| STK 选择后的显示文本 | `/api/stk/select` | `type:"display"` 时的 `data` | — | 否 |
| 「此 SIM 卡不支持 STK。」 | `/api/stk/menu` / select | `supported === false` | — | 否 |
| （接口有、页面没显示）STK 诊断 | `/api/stk/menu` | `reason`、`diagnostics[]`、`source` | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 标签「USSD」/「STK 菜单」 | button「USSD」、button「STK 菜单」（无 `role=tab`/`aria-selected`；建议改成 tablist） 〔现名：tab「USSD」「STK 菜单」（tablist「USSD 或 STK」）〕 | 无（切标签） | — | — | 无 | —（本地） |
| USSD 代码输入框（占位 `*#100#`，回车发送） | 无（只有 placeholder）；建议 textbox「USSD 代码」 | 无（草稿） | — | — | 无 | —（本地） |
| 「发送」 | button「发送」 | POST `/api/ussd/send` `{code}`（agent 只保留数字和 `*#+`，发 `AT+CUSD=1,"…",15`） | 1 | 无读回（回复即结果） | 无 | 二（可能触发运营商计费业务） |
| 回复输入框（会话进行中，回车回复） | 无；建议 textbox「USSD 回复」 〔交互后：USSD 会话进行中（运营商回了菜单）才有；填 textbox「USSD 代码」=*100#，点 button「发送」，点 button「确认：{x}」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 「回复」 | button「回复」 〔交互后：USSD 会话进行中才有；填 textbox「USSD 代码」=*100#，点 button「发送」，点 button「确认：{x}」〕 | POST `/api/ussd/respond` `{reply}` | 1 | 无读回 | 无 | 二 |
| 结束会话（红色 × 图标） | 无（只有图标，无 aria-label）；建议 button「结束 USSD 会话」 〔交互后：USSD 会话进行中才有；填 textbox「USSD 代码」=*100#，点 button「发送」，点 button「确认：{x}」〕 | POST `/api/ussd/cancel` `{}`（`AT+CUSD=2`） | 1 | 无读回 | 无 | 二 |
| 「新建查询」（会话结束后） | button「新建查询」 〔交互后：USSD 会话结束后才有（未自动验证）〕 | 无（清空） | — | — | 无 | —（本地） |
| 「加载 STK 菜单」 | button「加载 STK 菜单」 〔交互后：在「STK 菜单」标签里；点 tab「STK 菜单」〕 | GET `/api/stk/menu`（agent 会发 `AT+CUAD`、`AT+STIN?`、`AT+CUSATD=1`、`AT+STGI=…`，其中 `AT+CUSATD=1` 可能改变调制解调器 STK 状态，见汇总） | 1 | — | 无 | 二（GET 但可能有副作用） |
| STK「返回」（子菜单时） | button「返回」 〔交互后：进了 STK 子菜单才有（未自动验证）〕 | 无（本地回上一级） | — | — | 无 | —（本地） |
| STK 重新加载图标 | 无（只有图标）；建议 button「重新加载菜单」 〔交互后：加载 STK 菜单之后；点 tab「STK 菜单」，点 button「加载 STK 菜单」，点 button「确认：{x}」〕 | GET `/api/stk/menu` | 1 | — | 无 | 二（同「加载 STK 菜单」） |
| STK 菜单项（每项一个按钮） | button「{菜单项文字} ›」 〔交互后：加载 STK 菜单之后的菜单项（名称是菜单文字，未自动验证）〕 | POST `/api/stk/select` `{item_id}`（agent 发 `AT+CUSATE="D30782020181900101{id}"` 菜单选择信封） | 1 | 无读回（返回子菜单或显示文本） | 无 | 二（可能触发 SIM 卡上的业务） |

---

## /router/telemetry 遥测拦截

文件：`app/(panel)/router/telemetry/page.tsx`。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 已拦截域名列表 | `/api/router/domain-filter` | `blocked_domains[]`（页面注释称实际形状为 `{action_response, blocked_domains}`，未在 agent 代码里核对，agent 只是透传 ubus `router_get_domainfilter_rule`） | 无 | 否·透传（router.rs:234） |
| 「暂无已拦截的域名。」 | 同上 | 为空 | 无 | 否·透传 |
| 「已知 ZTE 遥测：iot.zte.com.cn, ztems.com, zte.com.cn, ztemt.com.cn」 | 无（页面写死） | — | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「拦截已知遥测」 | button「拦截已知遥测」 | PUT `/api/router/domain-filter` `{action:"add", domain}`（ubus `router_set_domain_filter`） | **最多 4 步**：对 4 个写死域名中还没拦截的，逐个 PUT；单个失败被页面吞掉继续下一个，最后照样提示成功（页面层面的假成功） | `/api/router/domain-filter` `blocked_domains` 包含这 4 个 | 无 | 二·远程三（按「防火墙规则增删改」处理） |
| 添加域名输入框（占位 `example.com`，回车添加） | 无；建议 textbox「域名」 | 无（草稿） | — | — | 无 | —（本地） |
| 「添加」 | button「添加」 | PUT `/api/router/domain-filter` `{action:"add", domain}` | 1 | `/api/router/domain-filter` `blocked_domains` | 无 | 二·远程三 |
| 域名行垃圾桶 | button「移除域名」（每行同名；建议带域名） 〔现名：button「移除 {域名}」〕 | 无（进入页内确认） | — | — | 无 | —（本地） |
| 页内确认「移除」 | button「移除」 〔现名：button「确认：{动作}」〕〔交互后：点 button「移除 {域名}」〕 | PUT `/api/router/domain-filter` `{action:"delete", domain}` | 1 | `/api/router/domain-filter` `blocked_domains` 不再含该域名 | 自定义确认（非 window.confirm）：「确定移除 "{域名}"？」 | 二·远程三 |
| 页内确认「取消」 | button「取消」 〔交互后：页内确认里；点 button「移除 {域名}」〕 | 无 | — | — | 无 | —（本地） |

写接口假成功风险：否·透传（router.rs:241-250）。

---

## /router/vpn VPN 穿透

文件：`app/(panel)/router/vpn/page.tsx`。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| L2TP 穿透开关状态 | `/api/router/vpn` | `l2tp_passthrough`（"1"=开，未确认） | 无 | 否·透传（router.rs:198，ubus `router_get_alg_para`） |
| PPTP 穿透开关状态 | `/api/router/vpn` | `pptp_passthrough` | 无 | 否·透传 |
| IPSec 穿透开关状态 | `/api/router/vpn` | `ipsec_passthrough` | 无 | 否·透传 |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「L2TP 穿透」开关 | 无（`Toggle` 没传 label）；建议 switch「L2TP 穿透」 | PUT `/api/router/vpn` `{l2tp_passthrough:"1"/"0"}`（ubus `router_set_alg_switch`） | 1 | `/api/router/vpn` `l2tp_passthrough` | 无 | 二·远程三 |
| 「PPTP 穿透」开关 | 无；建议 switch「PPTP 穿透」 | PUT `/api/router/vpn` `{pptp_passthrough}` | 1 | `/api/router/vpn` `pptp_passthrough` | 无 | 二·远程三 |
| 「IPSec 穿透」开关 | 无；建议 switch「IPSec 穿透」 | PUT `/api/router/vpn` `{ipsec_passthrough}` | 1 | `/api/router/vpn` `ipsec_passthrough` | 无 | 二·远程三 |

写接口假成功风险：否·透传（router.rs:205-214）。

---

## /router/wifi Wi-Fi 设置

文件：`app/(panel)/router/wifi/page.tsx`。整页是一个大表单：所有控件只改草稿，页头「应用」→ 页内确认「确认」时**一次性**把全部字段 PUT 到 `/api/wifi/settings`。

**Wi-Fi 总开关走的是 `/api/wifi/settings` 的 `wifi_onoff`，不是 `/api/wifi/radio`。** 这条路径是全站最严重的假成功（见本节末尾）。`/api/wifi/radio`（会等 hostapd 真正起来/停掉才回 ok，wifi_radio.rs:283-326）目前只有情景引擎在用，管理网页没有任何页面调它。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| Wi-Fi 开关初值 | `/api/wifi/status` | `wifi_onoff`（≠"0" 即开；`uci zte_mbb.wifi.wifi_onoff`，读不到再看 `zwrt_wlan report`，再读不到兜底 "1"） | 无 | 是：wifi.rs:84-103，读失败会显示为「开」 |
| Wi-Fi 6 开关初值 | `/api/wifi/status` | `wifi6_switch` | 无 | 是：同上，兜底 "0" |
| 区域下拉初值 | `/api/wifi/status` | `country_code`（`wireless.wifi0.country`，空时按 CN；不在内置列表里的值会被加到列表最前） | 无 | 是：wifi.rs:12-14 `uci get` 失败为空串 |
| 2.4G / 5G「无线电开启」初值 | `/api/wifi/status` | `radio2_disabled`、`radio5_disabled`（`wireless.wifi0/wifi1.disabled`） | 无 | 是：同上 |
| 2.4G / 5G SSID 初值 | `/api/wifi/status` | `ssid_2g`、`ssid_5g`（兼容 `ssid`） | 无 | 是：同上 |
| 2.4G / 5G 密码初值（**明文从接口返回**） | `/api/wifi/status` | `key_2g`、`key_5g`（兼容 `password`/`key`） | 无 | 是：同上 |
| 2.4G / 5G 加密方式初值 | `/api/wifi/status` | `encryption_2g`、`encryption_5g` | 无 | 是：同上 |
| 2.4G / 5G 信道初值 | `/api/wifi/status` | `channel_2g`、`channel_5g`（配置值，不是实际信道） | 无 | 是：同上 |
| 2.4G / 5G 带宽初值 | `/api/wifi/status` | `htmode_2g`、`htmode_5g` | 无 | 是：同上 |
| 2.4G / 5G 发射功率初值 | `/api/wifi/status` | `txpower_2g`、`txpower_5g`（`txpowerpercent`） | 无 | 是：同上 |
| 2.4G / 5G 隐藏 SSID 初值 | `/api/wifi/status` | `hidden_2g`、`hidden_5g` | 无 | 是：同上 |
| 加载错误横幅（带 Retry） | `/api/wifi/status` | SWR `error` | — | — |

接口里有、本页没显示：`actual_channel_*`、`actual_bw_*`、`clients_*`、`guest_*`（首页用了一部分）。

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「应用」（页头） | button「应用」 | 无（弹出页内确认） | — | — | 无 | —（本地） |
| 页内确认「确认」 | button「确认」 〔现名：button「确认：应用 Wi-Fi 设置」〕〔交互后：改了设置再按「应用」；填 textbox「2.4 GHz SSID(网络名)」=U60-e2e，点 button「应用」〕 | PUT `/api/wifi/settings`，一次带全部字段：`{ssid_2g, ssid_5g, key_2g, key_5g, channel_2g, channel_5g, txpower_2g, txpower_5g, encryption_2g, encryption_5g, hidden_2g, hidden_5g, wifi_onoff, radio2_disabled, radio5_disabled, wifi6_switch, htmode_2g, htmode_5g, country}` | 1 个 HTTP 请求；agent 内部：逐项和 uci 比较，变了才 `uci set` → commit `wireless` / `zte_mbb` → 只改发射功率时 `iw … set txpower` 热生效，否则**另起线程** `zwrt_wlan reload` 并轮询验证（HTTP 已先回 ok） | `/api/wifi/status` 各字段（**只是 uci 配置，不代表已生效**）；是否真在广播要看 `/api/wifi/radio` `beaconing`（本页不读） | 自定义确认（非 window.confirm）：「Wi-Fi 将短暂重启,确认?」 | 二·远程三（含 Wi-Fi 开关、名称/密码） |
| 页内确认「取消」 | button「取消」 〔交互后：页内确认里；填 textbox「2.4 GHz SSID(网络名)」=U60-e2e，点 button「应用」〕 | 无 | — | — | 无 | —（本地） |
| 「Wi-Fi 开关」 | switch「开」/「关」（随状态变，且与下面两个「无线电开启」同名）；建议 switch「Wi-Fi 开关」 | 无（草稿 → `wifi_onoff`） | — | — | 无 | —（本地；随「确认」提交，二·远程三） |
| 「Wi-Fi 6 (802.11ax)」开关 | switch「已启用」/「已禁用」（随状态变）；建议 switch「Wi-Fi 6」 〔现名：switch「Wi-Fi 6 (802.11ax)」〕 | 无（草稿 → `wifi6_switch`） | — | — | 无 | —（本地；随「确认」提交） |
| 「区域」下拉（中国/美国/香港/台湾/日本/韩国/新加坡/澳大利亚/英国/德国/加拿大） | 无（FieldRow 的文字是 span，不是 label）；建议 combobox「区域」 | 无（草稿 → `country`；选 US 时若 2.4G 信道是 12/13 自动改回自动） | — | — | 无 | —（本地；随「确认」提交，改区域会整机 Wi-Fi 重载） |
| 2.4 GHz「无线电开启」开关 | switch「开」/「关」；建议 switch「2.4 GHz 无线电」 | 无（草稿 → `radio2_disabled`） | — | — | 无 | —（本地；随「确认」提交，二·远程三） |
| 2.4 GHz SSID(网络名) 输入框 | 无；建议 textbox「2.4 GHz SSID」 〔现名：textbox「2.4 GHz SSID(网络名)」〕 | 无（草稿） | — | — | 无 | —（本地；随「确认」提交，二·远程三） |
| 2.4 GHz 密码输入框（加密=开放时禁用） | 无；建议「2.4 GHz 密码」 | 无（草稿） | — | — | 无 | —（本地；随「确认」提交，二·远程三） |
| 2.4 GHz 眼睛图标（显示/隐藏密码） | button「显示密码」/「隐藏密码」（与 5G 的同名） 〔现名：button「显示 2.4 GHz 密码」/「隐藏 2.4 GHz 密码」〕 | 无 | — | — | 无 | —（本地） |
| 2.4 GHz 加密方式下拉（WPA2 / WPA3 / WPA2/WPA3 Mixed / 开放(无密码)） | 无；建议 combobox「2.4 GHz 加密方式」 | 无（草稿） | — | — | 无 | —（本地；随「确认」提交） |
| 2.4 GHz 信道下拉（自动、1–13；US 去掉 12、13） | 无；建议 combobox「2.4 GHz 信道」 | 无（草稿） | — | — | 无 | —（本地；随「确认」提交） |
| 2.4 GHz 带宽下拉（自动 / HT20 / HT40 / VHT80 / VHT160） | 无；建议 combobox「2.4 GHz 带宽」 | 无（草稿） | — | — | 无 | —（本地；随「确认」提交） |
| 2.4 GHz 发射功率下拉（低 25 / 中 50 / 高 100） | 无；建议 combobox「2.4 GHz 发射功率」 | 无（草稿） | — | — | 无 | —（本地；随「确认」提交） |
| 2.4 GHz 隐藏 SSID 开关 | 无（`Toggle` 没传 label）；建议 switch「2.4 GHz 隐藏 SSID」 〔现名：switch「隐藏 2.4 GHz SSID」〕 | 无（草稿） | — | — | 无 | —（本地；随「确认」提交） |
| 5 GHz「无线电开启」开关 | switch「开」/「关」（与 2.4 GHz、Wi-Fi 开关同名）；建议 switch「5 GHz 无线电」 | 无（草稿 → `radio5_disabled`） | — | — | 无 | —（本地；随「确认」提交，二·远程三） |
| 5 GHz SSID(网络名) 输入框 | 无；建议 textbox「5 GHz SSID」 〔现名：textbox「5 GHz SSID(网络名)」〕 | 无（草稿 → `ssid_5g`） | — | — | 无 | —（本地；随「确认」提交，二·远程三） |
| 5 GHz 密码输入框（加密=开放时禁用） | 无；建议「5 GHz 密码」 | 无（草稿 → `key_5g`） | — | — | 无 | —（本地；随「确认」提交，二·远程三） |
| 5 GHz 眼睛图标（显示/隐藏密码） | button「显示密码」/「隐藏密码」（与 2.4 GHz 同名）；建议加「5 GHz」 〔现名：button「显示 5 GHz 密码」/「隐藏 5 GHz 密码」〕 | 无 | — | — | 无 | —（本地） |
| 5 GHz 加密方式下拉（WPA2 / WPA3 / WPA2/WPA3 Mixed / 开放(无密码)） | 无；建议 combobox「5 GHz 加密方式」 | 无（草稿 → `encryption_5g`） | — | — | 无 | —（本地；随「确认」提交） |
| 5 GHz 信道下拉（自动、36 40 44 48 52 56 60 64 100 104 108 112 116 132 136 140 149 153 157 161 165） | 无；建议 combobox「5 GHz 信道」 | 无（草稿 → `channel_5g`） | — | — | 无 | —（本地；随「确认」提交） |
| 5 GHz 带宽下拉（自动 / HT20 / HT40 / VHT80 / VHT160） | 无；建议 combobox「5 GHz 带宽」 | 无（草稿 → `htmode_5g`） | — | — | 无 | —（本地；随「确认」提交） |
| 5 GHz 发射功率下拉（低 25 / 中 50 / 高 100） | 无；建议 combobox「5 GHz 发射功率」 | 无（草稿 → `txpower_5g`） | — | — | 无 | —（本地；随「确认」提交） |
| 5 GHz 隐藏 SSID 开关 | 无（`Toggle` 没传 label）；建议 switch「5 GHz 隐藏 SSID」 〔现名：switch「隐藏 5 GHz SSID」〕 | 无（草稿 → `hidden_5g`） | — | — | 无 | —（本地；随「确认」提交） |
| 错误横幅「Retry」 | button「Retry」 〔现名：button「重试」〕〔交互后：读取失败时（错误状态）；场景 down〕 | 重拉 `/api/wifi/status` | — | — | 无 | —（本地，只读） |

写接口假成功风险：PUT `/api/wifi/settings` **是**，而且有三处：
1. wifi.rs:251-259：`wifi_onoff` / `wifi6_switch` 属于 `zte_mbb` 包，`uci set` 失败被「skip silently」，照样回 ok。
2. wifi.rs:264-300：如果这次只改了 `wifi_onoff`（或 `wifi6_switch`），`wireless_changed` 为 false，**根本不会 reload**，只 commit 了 `zte_mbb` 就回 ok——Wi-Fi 开关在页面上显示已切换，实际无线电不一定动（**未确认**固件是否另有进程监听 `zte_mbb.wifi.wifi_onoff`）。
3. wifi.rs:294-298：需要重载时放到后台线程 `finish_in_background`，HTTP 先回 ok；重载失败只写 agent 日志，页面看不到。另外只改发射功率时 `iw … set txpower` 的结果被丢掉（wifi.rs:280-291）。

---

## /router/wifi-guest 访客 Wi-Fi

文件：`app/(panel)/router/wifi-guest/page.tsx`。同样是整表提交：页头「应用」一次 PUT 全部字段，**没有**确认。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 启用 2.4 GHz / 启用 5 GHz 初值 | `/api/wifi/guest` | `disabled_2g`、`disabled_5g`（≠"1" 即开；页面先找 `guest_disabled_*`，agent 不返回这个名字，落到 `disabled_*`） | 无 | 是：wifi.rs:12-14 读不到为空串 → 显示为「开」 |
| SSID 初值 | `/api/wifi/guest` | `ssid`（`wireless.guest_2g.ssid`） | 无 | 是：同上 |
| 密码初值（明文） | `/api/wifi/guest` | `key` | 无 | 是：同上 |
| 加密方式初值 | `/api/wifi/guest` | `encryption` | 无 | 是：同上 |
| 隐藏 SSID 初值 | `/api/wifi/guest` | 页面读 `guest_hidden` ?? `hide`；**agent 返回的键叫 `hidden`**（wifi.rs:315），所以这个开关永远显示为关 | 无 | 是：同上 |
| AP 隔离初值 | `/api/wifi/guest` | `isolate`（≠"0" 即开） | 无 | 是：同上 |
| 有效时长初值 | `/api/wifi/guest` | `guest_active_time` | 无 | 是：同上 |
| 剩余时间倒计时（剩余 X小时 Y分 Z秒；开着、有效时长>0 且剩余>0 时显示） | `/api/wifi/guest` | `remaining_seconds`（`zwrt_wlan wlan_get_guest_access_left_time`，失败为 −1），之后页面每秒本地减 1 | 本地 1000 倒计时；接口本身无轮询 | 是：wifi.rs:320-324 失败时 −1，倒计时不显示 |
| 加载错误横幅 | `/api/wifi/guest` | SWR `error` | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「应用」（页头） | button「应用」 | PUT `/api/wifi/guest` `{guest_ssid, guest_key, guest_encryption, guest_disabled_2g, guest_disabled_5g, guest_hidden, guest_isolate, guest_active_time}` | 1 个 HTTP 请求；agent 对每个键写 2.4G+5G 两个 uci 段（段不存在则静默跳过），commit 后**后台** reload | `/api/wifi/guest` 各字段（配置，不代表已生效） | 无 | 二·远程三（访客网络的开关与名称/密码） |
| 「启用 2.4 GHz」开关 | switch「开」/「关」（随状态变，与 5 GHz 同名）；建议 switch「访客 2.4 GHz」 | 无（草稿） | — | — | 无 | —（本地；随「应用」提交） |
| 「启用 5 GHz」开关 | switch「开」/「关」；建议 switch「访客 5 GHz」 | 无（草稿） | — | — | 无 | —（本地；随「应用」提交） |
| SSID 输入框（占位「访客 SSID」） | 无；建议 textbox「访客 SSID」 | 无（草稿） | — | — | 无 | —（本地；随「应用」提交） |
| 密码输入框（加密=开放时禁用） | 无；建议「访客密码」 | 无（草稿） | — | — | 无 | —（本地；随「应用」提交） |
| 眼睛图标（显示/隐藏密码） | 无（**没有 aria-label**，与主 Wi-Fi 页不同）；建议 button「显示密码」 〔现名：button「显示访客密码」/「隐藏访客密码」〕 | 无 | — | — | 无 | —（本地） |
| 加密方式下拉（WPA2 / WPA3 / WPA2/WPA3 混合 / 开放） | 无；建议 combobox「访客加密方式」 | 无（草稿） | — | — | 无 | —（本地；随「应用」提交） |
| 隐藏 SSID 开关 | 无（`Toggle` 没传 label）；建议 switch「隐藏访客 SSID」 | 无（草稿） | — | — | 无 | —（本地；随「应用」提交） |
| AP 隔离开关 | switch「已启用」/「已禁用」（随状态变）；建议 switch「AP 隔离」 | 无（草稿） | — | — | 无 | —（本地；随「应用」提交） |
| 有效时长（分钟，0 = 不限）数字框（0–1440） | 无；建议 spinbutton「有效时长（分钟）」 | 无（草稿） | — | — | 无 | —（本地；随「应用」提交） |

写接口假成功风险：PUT `/api/wifi/guest` **是**——wifi.rs:370-378 不存在的段写失败静默跳过；wifi.rs:389 reload 在后台，HTTP 先回 ok。

---

## /scheduler 定时任务（计划任务）

文件：`app/(panel)/scheduler/page.tsx`。可以定时调用**任意** `/api/*`（agent 只拒绝 `/api/scheduler/*` 和 `/api/auth/*`，scheduler.rs:113-129），包括重启、恢复出厂、eSIM 切换等。「定时重启」就是在这里建一个 `POST /api/device/reboot` 的任务。

> **星期错位（代码确认）**：页面把 0 当周日、6 当周六（`DAYS = ["Sun",…,"Sat"]`）；agent 把 0 当周一、6 当周日（scheduler.rs:88 `dow = (tm_wday + 6) % 7`，校验提示「0-6 (Mon-Sun)」）。页面上勾「周日」，实际在周一执行，整体错一天。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 任务行：启用开关状态 | `/api/scheduler/jobs` | `[].enabled` | 无 | 否（agent 内存） |
| 任务行：名称 | `/api/scheduler/jobs` | `[].name` | 无 | 否 |
| 任务行：计划（`HH:mm — 周一, 周二…` / 「于 X 执行一次」/「每天」） | `/api/scheduler/jobs` | `[].schedule.type/time/days/at`（`at` 为设备时钟，`fmtDevice`） | 无 | 否 |
| 任务行：`方法 路径` | `/api/scheduler/jobs` | `[].action.method`、`[].action.path` | 无 | 否 |
| 任务行：上次错误（红字）/「上次：时间」 | `/api/scheduler/jobs` | `[].last_error`、`[].last_run` | 无 | 否 |
| 「暂无定时任务。请在上方创建。」 | `/api/scheduler/jobs` | 为空 | 无 | 否 |
| 编辑表单预填 | `/api/scheduler/jobs` | `name`、`action.method/path/body`、`schedule.*` | 无 | 否 |
| （接口有、页面没显示/没编辑）恢复动作 | `/api/scheduler/jobs` | `restore`、`last_status`、`last_restore_*`（表单里 `restoreEnabled/restoreTime` 字段存在但**没有控件**，也不提交） | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「新建任务」（页头） | button「新建任务」 | 无（打开表单） | — | — | 无 | —（本地） |
| 表单·任务名称 | 无（label 未关联）；建议 textbox「任务名称」 〔交互后：点 button「新建任务」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 表单·方法下拉（GET / POST / PUT / DELETE） | 无；建议 combobox「方法」 〔交互后：点 button「新建任务」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 表单·路径（占位 `/api/device/reboot`） | 无；建议 textbox「路径」 〔交互后：点 button「新建任务」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 表单·请求体（JSON，可选）多行框 | 无；建议 textbox「请求体」 〔现名：textbox「请求体（JSON，可选）」〕〔交互后：点 button「新建任务」〕 | 无（草稿；**JSON 解析失败被静默忽略，请求体直接丢掉**） | — | — | 无 | —（本地） |
| 表单·计划类型单选「循环」/「一次性」 | radio「循环」「一次性」 〔交互后：点 button「新建任务」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 表单·时间（HH:mm） | 无；建议「时间」 〔现名：textbox「时间（HH:mm）」〕〔交互后：点 button「新建任务」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 表单·星期 7 个按钮（周日…周六，可多选） | button「周日」…「周六」（选中只靠颜色，无 `aria-pressed`；建议加） 〔交互后：点 button「新建任务」〕 | 无（草稿；见上方星期错位） | — | — | 无 | —（本地） |
| 表单·日期/时间（一次性） | 无；建议「日期/时间」 〔交互后：计划类型选「一次性」后；点 button「新建任务」，点 radio「一次性」〕 | 无（草稿；按设备墙上时间换算，`fromWallInput`） | — | — | 无 | —（本地） |
| 表单「创建」 | button「创建」 〔交互后：点 button「新建任务」〕 | POST `/api/scheduler/jobs` `{name, action:{method, path, body?}, schedule:{type:"recurring", time, days} 或 {type:"once", at}}` | 1 | `/api/scheduler/jobs` 出现新任务 | 无 | 二；目标是第三档接口时（如 `/api/device/reboot` = 定时重启开启）为三 |
| 表单「更新」（编辑时） | button「更新」 〔交互后：编辑已有任务时；点 button「编辑 夜间省电」〕 | PUT `/api/scheduler/jobs` `{id, name, enabled:true, action, schedule}`（**编辑一定会把任务重新启用**；原有 `restore` 会被清掉） | 1 | `/api/scheduler/jobs` 该任务字段 | 无 | 同「创建」 |
| 表单「取消」 | button「取消」 〔交互后：点 button「新建任务」〕 | 无 | — | — | 无 | —（本地） |
| 任务行启用开关 | 无（`Toggle` 没传 label，每行一个）；建议 switch「启用 {任务名}」 | PUT `/api/scheduler/jobs/toggle` `{id, enabled:!enabled}` | 1 | `/api/scheduler/jobs` `[].enabled` | 无 | 二；开启重启类任务时三（定时重启开启） |
| 任务行铅笔图标 | 无（只有图标，无 aria-label）；建议 button「编辑 {任务名}」 | 无（进入行内编辑） | — | — | 无 | —（本地） |
| 任务行垃圾桶图标 | 无（只有图标）；建议 button「删除 {任务名}」 | DELETE `/api/scheduler/jobs` `{id}` | 1（**无任何确认**） | `/api/scheduler/jobs` 该任务消失 | 无 | 二 |

写接口假成功风险：创建 / 更新 / 删除 / 启停 **是**——scheduler.rs:154-158 `save()` 写 `/data/…` 失败被 `let _ =` 丢掉，内存已改、回 ok，重启后丢失。

---

## /services/tailscale Tailscale

文件：`app/(panel)/services/tailscale/page.tsx`。**只读**：agent 的 `/api/services/tailscale` 只有 GET（services.rs 注释「Endpoints never mutate state」）。**关 Tailscale：当前无控件**（档位规则里的「关 Tailscale（二·远程三）」在现有页面上没有对应控件，改版若新增要按新功能处理）。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 页头状态（未安装 / 已停止 / 需要认证 / 运行中 / 需要登录 / 原始 backend_state） | `/api/services/tailscale` | `installed`、`running`（socket 存在 + `pidof tailscaled`）、`auth_url`、`backend_state` | 5000 | 是：services.rs:52-86 `tailscale status --json` 执行失败或解析失败时仍回 ok，只多一个 `error` 字段（页面会显示成红条） |
| 「Tailscale 未安装」卡 | 同上 | `installed === false` | 5000 | 否 |
| 「Tailscale: {错误}」红条 | 同上 | `error` | 5000 | 同上 |
| 需要登录卡 + 认证链接 | 同上 | `auth_url` | 5000 | 否 |
| 本节点：主机名 / Tailscale 名称 / 状态 / IP (v4) / IP (v6) / DERP 中继 / 版本 / 可作出口节点 | 同上 | `self.hostname`、`self.dns_name`、`backend_state`、`self.ips[]`（按是否含 `:` 分 v4/v6）、`self.relay`、`version`、`self.exit_node_option` | 5000 | 否 |
| 网格：对等节点数 / 在线数 | 同上 | `peer_count`、`peer_online` | 5000 | 否 |
| 正在使用的出口节点：主机名 · IP · 在线/离线（无则「未选择出口节点。」） | 同上 | `exit_node.hostname/ips[0]/online` | 5000 | 否 |
| 对等节点列表：主机名 +「exit node」标记 · IP · 系统 · ↑发送 ↓接收 · 最近握手（相对时间）· 在线/离线（最多 32 个） | 同上 | `peers[].hostname/exit_node/ips[0]/os/tx_bytes/rx_bytes/last_handshake/online` | 5000 | 否 |
| 守护进程日志（末 200 行） | `/api/services/tailscale/log?lines=200` | `lines[]`（`/data/tailscaled.log`） | 4000（可暂停） | 否 |
| 加载错误横幅 | `/api/services/tailscale` | SWR `error` | — | — |

接口里有、页面没显示：`peers[].dns_name`、`peers[].last_seen`、`peers[].id`（作 key）。

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 页头「刷新」 | button「刷新」（与日志卡同名） | 重拉 `/api/services/tailscale` | — | — | 无 | —（本地，只读） |
| 错误横幅「Retry」 | button「Retry」 〔现名：button「重试」〕〔交互后：读取失败时（错误状态）；场景 down〕 | 同上 | — | — | 无 | —（本地，只读） |
| 认证链接 | link「{auth_url}」（新窗口） | 无（导航到 Tailscale 登录） | — | — | 无 | —（本地） |
| 日志「暂停」/「继续」 | button「暂停」/「继续」 | 无 | — | — | 无 | —（本地） |
| 日志「刷新」 | button「刷新」 | GET `/api/services/tailscale/log?lines=200` | — | — | 无 | —（本地，只读） |

---

## /settings 设置

文件：`app/(panel)/settings/page.tsx`。全部是浏览器本地设置，只有「测试连通性」会发请求。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 代理 URL 输入框初值 | 无（`getApiBase()`：localStorage `u60.agent_url` 或同源） | — | — | 不适用 |
| 轮询间隔（秒）初值 | 无（localStorage `u60.poll_interval`，默认 2）；**这个值整个 web 端没有任何地方读取**，改了不生效 | — | — | 不适用 |
| 连通性结果（可访问 / 不可访问） | `/api/auth/login` | 见下 | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 代理 URL 输入框 | 无（FieldRow 文字不是 label）；建议 textbox「代理 URL」 | 无（草稿） | — | — | 无 | —（本地） |
| 「保存」/「已保存！」（代理连接卡） | button「保存」 | 无（写 localStorage `u60.agent_url` 和 `u60.poll_interval`） | — | — | 无 | —（本地） |
| 「测试连通性」 | button「测试连通性」 | POST `/api/auth/login` `{password:""}`（`noAuth`、`raw`） | 1。**缺陷**：设了密码时 agent 回 401，`apiFetch` 遇 401 会先清掉 token 并派发登出事件（client.ts:77-80），所以点这个按钮会**把自己登出**，页面还显示「不可访问」 | 无读回 | 无 | 二（会产生一次失败登录；建议改成调 `/api/public/status`） |
| 轮询间隔（秒）数字框（1–60） | 无；建议 spinbutton「轮询间隔（秒）」 | 无（草稿） | — | — | 无 | —（本地） |
| 「保存偏好」/「已保存！」 | button「保存偏好」 | 无（同上，写 localStorage，但没人读） | — | — | 无 | —（本地） |
| 「退出登录」 | button「退出登录」（与顶栏同名） | 无（清 token，跳 `/login`） | — | — | 无 | —（本地） |
| 「清除已存储的 Token」 | button「清除已存储的 Token」 | 无（删 localStorage `u60.token`，跳 `/login`） | — | — | 无 | —（本地） |
| 「清除已存储的代理 URL」 | button「清除已存储的代理 URL」 | 无（删 localStorage `u60.agent_url`） | — | — | 无 | —（本地） |

说明：本页没有深浅色切换；语言切换在顶栏（见「全局」）。

---

## /signal 信号

文件：`app/(panel)/signal/page.tsx`。只读。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 信号柱 + 链路评价 + 通俗说明（同首页） | `/api/network/signal` | NR 时 `nr5g_rsrp`，否则 `lte_rsrp` | 2000 | 否·透传 |
| 「实时 / 重连中」 | 同上 | SWR `error` | 2000 | — |
| 摘要行：RSRP dBm · 运营商 · 制式 · Band | 同上 | `rsrp`、`network_provider_fullname`‖`network_provider`、`network_type`、NR 时 `nr5g_action_band` 否则 `lte_band` | 2000 | 否·透传 |
| SINR / RSRQ / Bars | 同上 | NR 时 `nr5g_snr`/`nr5g_rsrq`，否则 `lte_snr`/`lte_rsrq`；`signalbar` | 2000 | 否·透传 |
| 信号趋势折线（约 2 分钟，最多 60 个点，最小/最大 dBm） | 同上 | RSRP 历史（页面内存，刷新页面就清空） | 2000 | 否·透传 |
| 服务小区：Cell ID / PCI / EARFCN / Band | 同上 | NR 时 `nr5g_cell_id`/`nr5g_pci`/`nr5g_action_channel`/`nr5g_action_band`，否则 `lte_cell_id`/`lte_pci`/`lte_earfcn`/`lte_band` | 2000 | 否·透传 |
| 服务小区：Bandwidth / Net Select | 同上 | `nr5g_bandwidth`（加 MHz；LTE 时也读 NR 字段）、`net_select_mode` | 2000 | 否·透传 |
| 错误横幅（带 Retry） | 同上 | SWR `error` | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 错误横幅「Retry」 | button「Retry」 〔现名：button「立即重试」〕〔交互后：读到过数据、之后连不上设备时（旧数据标为过期）；载入后场景 down〕 | 重拉 `/api/network/signal` | — | — | 无 | —（本地，只读） |

---

## /sms 短信收件箱

文件：`app/(panel)/sms/page.tsx`。列表是用 **POST** 读的（`useApi` 带 `method:"POST"`），SWR 键是 `/api/sms/list?store=…`；agent 路由去掉查询串后就是 `POST /api/sms/list`。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| SIM 已用（N / 总数） | `/api/sms/capacity` | `sim_used`、`sim_total`（**未确认**：固件 `zwrt_wms_get_wms_capacity` 的真实键名没在代码里核对；`public.rs` 用的是 `sms_dev_unread_num`/`sms_sim_unread_num` 这类名字，若键名不对这张卡不显示） | 10000 | 否·透传（sms.rs:42） |
| 设备已用 | `/api/sms/capacity` | `device_used`、`device_total`（未确认） | 10000 | 否·透传 |
| 总已用 | `/api/sms/capacity` | `used`、`total`（未确认） | 10000 | 否·透传 |
| 未读（本页列表里 `tag≠0` 的条数） | POST `/api/sms/list` | `messages[].tag` | 10000 | 否·透传（sms.rs:10-40） |
| 列表：来自（未读加粗） | POST `/api/sms/list` | `messages[].number`、`tag` | 10000 | 否·透传 |
| 列表：内容（UCS2 解码，两行截断） | 同上 | `messages[].content`（`decodeSms`） | 10000 | 否·透传 |
| 列表：日期 | 同上 | `messages[].date`（`YY,MM,DD,HH,MM,SS,+TZ` → `formatSmsDate`） | 10000 | 否·透传 |
| 列表：存储 | 同上 | `messages[].mem_store` | 10000 | 否·透传 |
| 「暂无短信。」 | 同上 | 为空 | 10000 | 否·透传 |
| 详情弹层：来自 / 日期 · 存储 /「已读」/ 正文 | 同上（本地已有数据） | 同上字段 | — | — |
| 操作错误横幅 | 写接口 | — | — | — |

说明：筛选「全部」和「设备」发给 agent 的 `mem_store` 都会被换成 1（sms.rs:21-28，只有 "sim" 变 2），所以这两个筛选**看到的是同一份列表**。

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「写短信」（页头） | button「写短信」 | 无（跳 `/sms/compose`） | — | — | 无 | —（本地） |
| 存储筛选「全部」「SIM」「设备」 | button「全部」「SIM」「设备」（选中只靠颜色，无 `aria-pressed`；建议做成 radiogroup） 〔现名：radio「全部」「SIM」「设备」（radiogroup「存储」）〕 | POST `/api/sms/list` `{page:0, data_per_page:500, mem_store, tags:10, order_by}`（读） | — | — | 无 | —（本地，只读） |
| 刷新图标 | 无（只有图标）；建议 button「刷新短信」 | 同上重拉 | — | — | 无 | —（本地，只读） |
| 表头全选勾选框 | 无；建议 checkbox「全选」 | 无（本地选择） | — | — | 无 | —（本地） |
| 每行勾选框 | 无；建议 checkbox「选择来自 {号码} 的短信」 | 无（本地选择） | — | — | 无 | —（本地） |
| 「标为已读 (N)」（有选择时） | button「标为已读 (N)」 〔交互后：选中短信之后；点 checkbox「选择来自 +886912000451 的短信」〕 | POST `/api/sms/read` `{id:"1;2;3;", tag:0}`（ubus `zwrt_wms_modify_tag`） | 1 | POST `/api/sms/list` 对应条 `tag`=0；`/api/public/status` `sms.unread` | 无 | 一（短信标为已读） |
| 「删除 (N)」（有选择时） | button「删除 (N)」 〔交互后：选中短信之后；点 checkbox「选择来自 +886912000451 的短信」〕 | POST `/api/sms/delete` `{id:"1;2;3;"}`（ubus 删，残留的再直接 sqlite 删） | 1 | POST `/api/sms/list` 这些条消失 | 「删除 {N} 条短信?」 | 三（删除短信） |
| 行内号码 / 内容单元格（点开详情） | 无（`<td onClick>`，不可聚焦、无角色）；建议整行改成 button「打开来自 {号码} 的短信」 | 未读时后台 POST `/api/sms/read` `{id:"{id};", tag:0}`（失败静默） | 1（仅未读时） | 同「标为已读」 | 无 | 一（短信标为已读） |
| 详情弹层关闭「×」/ 点遮罩 | button「关闭」 〔交互后：打开一条短信之后；点 button「打开来自 +886912000451 的短信」〕 | 无 | — | — | 无 | —（本地） |

写接口假成功风险：
- `/api/sms/read`：否·透传（sms.rs:178-187）。
- `/api/sms/delete`：**是**——sms.rs:86 sqlite 核对失败时直接按 ubus 结果回 ok（附 `warning`），而注释里写明 ubus 对 SIM 存储的短信会「返回 result:3 却不删」。

---

## /sms/compose 写短信

文件：`app/(panel)/sms/compose/page.tsx`。不拉任何数据。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 「N 个字符 · M 条」 | 无（本地计算：含非 ASCII 按 70 字/条，否则 160） | — | — | 不适用 |
| 「Unicode (UCS2) 编码」提示 | 无（本地） | — | — | 不适用 |
| 发送错误横幅 | `/api/sms/send` 的错误 | `error` | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「返回」（页头） | button「返回」 | 无（跳 `/sms/`） | — | — | 无 | —（本地） |
| 收件人（电话号码）输入框 | 无（label 未关联）；建议 textbox「收件人」 〔现名：textbox「收件人（电话号码）」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 内容多行框 | 无；建议 textbox「内容」 | 无（草稿） | — | — | 无 | —（本地） |
| 「发送」（收件人或内容为空时禁用） | button「发送」 | POST `/api/sms/send` `{number, sms_time:"YYYYMMDDHHmmss"（浏览器本地时间）, message_body（UCS2 时为 hex）, id:"-1", encode_type:"ucs2"/"gsm7"}`（ubus `zte_libwms_send_sms`） | 1；成功后跳回 `/sms/` | 无读回（发件箱不在收件箱列表里） | 无 | 二 |
| 「取消」 | button「取消」 | 无（跳 `/sms/`） | — | — | 无 | —（本地） |

写接口假成功风险：否·透传（sms.rs:49-58）——固件发送失败但 ubus 退出码为 0 时页面照样跳回收件箱，没有任何失败提示。

---

## /sms/forward 短信转发

文件：`app/(panel)/sms/forward/page.tsx`（`RuleForm`）。三个标签：设置 / 规则 / 日志。

> **两个按钮必失败（代码确认）**：
> - 规则行「测试规则」（▶）发的是 `{rule_id}`，agent 要的是 `{destination:{…}}`（sms_forward.rs:1255-1264），serde 解析失败回 400。
> - 日志「重试失败项」发的是 `{}`，agent 要的是 `{index}`（sms_forward.rs:1299-1308），回 400。页面失败时会显示错误，不是假成功，但功能不可用。
> 另：设置里轮询间隔输入框 `min=5`，agent 要求 ≥10（sms_forward.rs:1119-1121），填 5–9 会被拒。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 设置·启用短信转发 | `/api/sms/forward/config` | `config.enabled` | 无 | 否（agent 内存） |
| 设置·轮询间隔(秒) | 同上 | `config.poll_interval_secs` | 无 | 否 |
| 设置·转发后标为已读 / 转发后删除 | 同上 | `config.mark_read_after_forward`、`config.delete_after_forward` | 无 | 否 |
| 规则·「N 条规则」 | 同上 | `config.rules.length` | 无 | 否 |
| 规则行：启用开关 / 名称 / 筛选：类型 · 目标：类型 | 同上 | `config.rules[].enabled/name/filter.type/destination.type` | 无 | 否 |
| 规则编辑表单预填 | 同上 | `rules[].filter.patterns/keywords`、`destination.*` | 无 | 否 |
| （接口有、页面没显示）上次转发到的短信 id | 同上 | `last_forwarded_id` | — | — |
| 日志表：时间 / 发件人 / 预览 / 规则 / 目标 / 状态（成功/失败，悬停看错误） | `/api/sms/forward/log`（切到日志标签才拉） | `[].timestamp`（设备时钟）、`sender`、`content_preview`（UCS2 解码）、`rule_name`、`destination_type`、`success`、`error` | 无 | 否 |
| 「暂无日志。」/「暂无规则…」 | 同上 | 为空 | — | — |
| 操作结果提示（成功绿条 / 错误横幅） | 写接口 | — | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 标签「设置」「规则」「日志」 | button「设置」「规则」「日志」（无 tab 角色；建议 tablist） 〔现名：tab「设置」「规则」「日志」（tablist「短信转发分区」）〕 | 无（切标签；日志标签会触发 GET log） | — | — | 无 | —（本地） |
| 设置·「启用短信转发」开关 | switch「启用短信转发」 | 无（草稿） | — | — | 无 | —（本地） |
| 设置·轮询间隔(秒) 数字框 | 无（label 未关联）；建议 spinbutton「轮询间隔(秒)」 | 无（草稿） | — | — | 无 | —（本地） |
| 设置·「转发后标为已读」开关 | switch「转发后标为已读」 | 无（草稿） | — | — | 无 | —（本地） |
| 设置·「转发后删除」开关 | switch「转发后删除」 | 无（草稿） | — | — | 无 | —（本地） |
| 「保存设置」 | button「保存设置」 | PUT `/api/sms/forward/config` `{enabled, poll_interval_secs, mark_read_after_forward, delete_after_forward}` | 1 | `/api/sms/forward/config` `config.*` | 无 | 二（「转发后删除」打开后会自动删短信，建议评审） |
| 「添加规则」 | button「添加规则」 〔交互后：在「规则」标签里；点 tab「规则」〕 | 无（打开表单） | — | — | 无 | —（本地） |
| 规则行启用开关 | 无（`Toggle` 没传 label）；建议 switch「启用规则 {名称}」 〔现名：switch「启用规则{规则名}」〕〔交互后：在「规则」标签里；点 tab「规则」〕 | PUT `/api/sms/forward/rules/toggle` `{id, enabled}` | 1 | `/api/sms/forward/config` `config.rules[].enabled` | 无 | 二 |
| 规则行 ▶（测试规则） | 无 aria-label（`title`=「测试规则」）；建议 button「测试规则 {名称}」 〔现名：button「测试规则{规则名}」〕〔交互后：在「规则」标签里；点 tab「规则」〕 | POST `/api/sms/forward/test` `{rule_id}`——**字段不对，必回 400**（见上） | 1 | 无读回 | 无 | 二 |
| 规则行铅笔 | 无（只有图标）；建议 button「编辑规则 {名称}」 〔现名：button「编辑规则{规则名}」〕〔交互后：在「规则」标签里；点 tab「规则」〕 | 无（打开编辑表单） | — | — | 无 | —（本地） |
| 规则行垃圾桶 | 无（只有图标）；建议 button「删除规则 {名称}」 〔现名：button「删除规则{规则名}」〕〔交互后：在「规则」标签里；点 tab「规则」〕 | DELETE `/api/sms/forward/rules` `{id}` | 1 | `/api/sms/forward/config` 该规则消失 | 「删除此规则?」 | 三（删除短信转发规则） |
| 规则表单·规则名称 | 无；建议 textbox「规则名称」 〔交互后：规则表单；点 tab「规则」，点 button「添加规则」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 规则表单·「已启用」开关 | switch「已启用」 〔交互后：规则表单；点 tab「规则」，点 button「添加规则」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 规则表单·筛选条件下拉（全部短信 / 按发件人(模式) / 按内容(关键词) / 发件人 + 内容） | 无；建议 combobox「筛选条件」 〔交互后：规则表单；点 tab「规则」，点 button「添加规则」〕 | 无（草稿；切换类型会清空模式/关键词，不填就保存会因缺字段被 agent 拒绝） | — | — | 无 | —（本地） |
| 规则表单·发件人模式(逗号分隔) / 关键词(逗号分隔) | 无；建议 textbox「发件人模式」「关键词」 〔现名：textbox「发件人模式(逗号分隔)」「关键词(逗号分隔)」〕〔交互后：规则表单里、筛选条件选了发件人/内容时（未自动验证）〕 | 无（草稿） | — | — | 无 | —（本地） |
| 规则表单·转发目标下拉（Telegram / Webhook / 短信转发 / Ntfy / Discord / Slack） | 无；建议 combobox「转发目标」 〔交互后：规则表单；点 tab「规则」，点 button「添加规则」〕 | 无（草稿；切换会清空目标字段） | — | — | 无 | —（本地） |
| 规则表单·目标字段：Bot Token、Chat ID、「静默通知」开关（Telegram）；URL、方法下拉 POST/PUT/GET（Webhook）；转发到号码（短信）；服务器地址、主题、令牌(可选)（Ntfy）；Webhook 地址（Discord/Slack） | 输入框均无名称（label 未关联）；开关为 switch「静默通知」 〔交互后：规则表单、目标选 Telegram 时；点 tab「规则」，点 button「添加规则」〕 | 无（草稿） | — | — | 无 | —（本地） |
| 规则表单「创建」（新建时） | button「创建」 〔交互后：规则表单；点 tab「规则」，点 button「添加规则」〕 | POST `/api/sms/forward/rules` `{name, enabled, filter:{type, patterns?, keywords?}, destination:{type, …}}` | 1 | `/api/sms/forward/config` 出现新规则 | 无 | 二 |
| 规则表单「更新」（编辑时） | button「更新」 〔交互后：编辑规则时；点 tab「规则」，点 button「编辑规则{规则名}」〕 | PUT `/api/sms/forward/rules` `{id, name, enabled, filter, destination}` | 1 | `/api/sms/forward/config` 该规则字段 | 无 | 二 |
| 规则表单「取消」 | button「取消」 〔交互后：规则表单；点 tab「规则」，点 button「添加规则」〕 | 无 | — | — | 无 | —（本地） |
| 日志「刷新」 | button「刷新」 〔交互后：在「日志」标签里；点 tab「日志」〕 | GET `/api/sms/forward/log` | — | — | 无 | —（本地，只读） |
| 日志「重试失败项」 | button「重试失败项」 〔现名：button「重试失败项 (N)」〕〔交互后：在「日志」标签里；点 tab「日志」〕 | POST `/api/sms/forward/retry` `{}`——**缺 `index`，必回 400**（见上） | 1 | 无 | 无 | 二 |
| 日志「清空日志」 | button「清空日志」 〔交互后：在「日志」标签里；点 tab「日志」〕 | POST `/api/sms/forward/log/clear` `{}` | 1 | `/api/sms/forward/log` 为空 | 「清空所有日志?」 | 三（清空转发日志） |

写接口假成功风险：config / rules 增改删 / toggle / log/clear 全部 **是**——sms_forward.rs:1064-1068 `save_config`、1070-1074 `save_state` 写文件失败被 `let _ =` 丢掉，内存已改、回 ok，重启后丢失。test / retry 为「否」（真正发出去才回 ok）。

---

## /tools/at AT 终端

文件：`app/(panel)/tools/at/page.tsx`。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 页头端口状态（端口名 — 可用/不可用） | `/api/at/port` | `port`、`available` | 无 | 否（at_terminal.rs:63） |
| 响应区：`> 命令` + 响应原文 + 耗时 ms（本次会话的历史，刷新页面清空） | `/api/at/send` 的返回 | `command`（agent 过滤掉 `'`$;|&` 后的实际命令）、`response`、`elapsed_ms` | — | 否（原样返回调制解调器回复，含 ERROR 也照样显示；at_terminal.rs:43-59） |
| 「上次响应耗时 N ms」 | 同上 | `elapsed_ms` | — | — |
| 历史记录列表（本地） | 无 | — | — | — |
| 「命令必须以 AT 开头」/ 发送错误横幅 | 本地校验 / `/api/at/send` 错误 | — | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 命令输入框（占位 `AT+CGMR`；回车发送；↑↓ 调历史） | 无（只有 placeholder）；建议 textbox「AT 命令」 | 无（草稿） | — | — | 无 | —（本地） |
| 「发送」——普通命令 | button「发送」 | POST `/api/at/send` `{command, timeout:3}` | 1 | 无读回（响应即结果） | 无 | 二 |
| 「发送」——危险命令（匹配 `CFUN=0`、`+CRESET`、`&F`、`+NVWR`、`+QPOWD`、`+COPS=`） | button「发送」 | 同上 | 1 | 无读回 | 「警告：{原因}\n\n确定要发送：{命令} 吗？」 | 三（AT 危险命令） |
| 历史记录里的每条命令（点了填回输入框） | button「{命令}」 | 无 | — | — | 无 | —（本地） |

说明：危险命令的判断只有这 6 个正则。`AT+CFUN=4`（飞行）、`AT+CFUN=1,1`（重启调制解调器）、锁频锁小区类私有命令、`AT+CGDCONT=`（改 APN）等**不会**触发确认，改版时应扩充或改成白名单。

---

## /tools/enable-adb 启用 ADB

文件：`app/(panel)/tools/enable-adb/page.tsx`。走 agent 的 `/api/usb/mode`（`web/src/lib/ubus.ts`、`lib/adb.ts`、`lib/crypto.ts` 里那套「直连 `http://网关/ubus` 登录再切 USB」和 WebUSB ADB 客户端**没有任何页面引用**，是死代码）。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 成功提示「ADB 调试模式已启用。通过 USB 连接并运行 adb devices。」 | `/api/usb/mode` 的返回 | 只看有无报错 | — | 否·透传（usb.rs:13-22） |
| 错误横幅 | 同上 | `error` | — | — |

页面**不读取当前 USB 模式**（可以读的 `/api/usb/status` 只在 `/usb` 页用）。

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「启用 ADB 调试 USB 模式」/「重新启用 ADB」 | button「启用 ADB 调试 USB 模式」 | PUT `/api/usb/mode` `{mode:"debug"}`（ubus `zwrt_bsp.usb set`） | 1 | 无读回（本页）；可用 `/api/usb/status` 看模式 | 「启用 ADB 调试 USB 模式？\n\n这将通过 USB 暴露 ADB，允许对设备进行完整的 shell 访问。仅在你拥有物理访问权限并信任所处环境时才执行此操作。」 | 三（开 ADB） |

---

## /tools/processes 进程监控

文件：`app/(panel)/tools/processes/page.tsx`。

> **风险提示（代码确认）**：agent 的「冗余进程」名单（system.rs:389-407 `BLOAT_DAEMONS`）里有 `zte_topsw_mc`、`zte_dm`、`zte_topsw_wms`、`zte_topsw_sleep_faw`、`zte_topsw_tr098db`、`zte_topsw_fota_result`、`zte_smart_manage`——这几个都在 `zte_topsw_daemon.conf` 的开机同步名单里（见 `source/manager/CLAUDE.md`），`zte_topsw_wms` 还是短信服务。「清除全部冗余进程」会对它们发 SIGKILL。改版时建议复核这个名单。
> **显示缺陷**：agent 返回的 `killed` 是 `[{pid, name}]`，页面当成数字数组 `join(", ")`，成功提示会显示成「[object Object]」。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 进程表：名称 / PID / CPU% / RSS / 冗余标记（冗余行底色） | `/api/system/top` | `processes[].name/pid/cpu_pct/rss_kb/is_bloat` | 3000 | 否（读 /proc） |
| 页头「清除全部冗余进程（N）」里的 N | 同上 | `processes[]` 中 `is_bloat` 的条数（接口另有 `bloat_count`，页面没用） | 3000 | 否 |
| 「加载中…」/「未找到进程」 | 同上 | 为空 | — | — |
| 操作结果（已终止 PID：… / 已终止 N 个进程：…） | `/api/system/kill-bloat` 的返回 | `killed`（见上方显示缺陷） | — | — |
| 接口里有、页面没显示 | `/api/system/top` | `processes[].state`、`total_count`、`bloat_cpu_pct`、`bloat_rss_kb`；kill 返回的 `skipped`、`freed_rss_kb` | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 「清除全部冗余进程（N）」（页头，N=0 时禁用） | button「清除全部冗余进程（N）」 | POST `/api/system/kill-bloat` `{all:true}`（agent 扫 /proc，对名单内进程发 SIGKILL） | 1 | 无读回（`/api/system/top` 里进程可能被 procd 重新拉起） | 「终止全部 {N} 个冗余进程？」 | 三（一键结束） |
| 表头「名称」「PID」「CPU%」「RSS」（点了排序） | 无（`<th onClick>`，不可聚焦）；建议 button「按名称排序」等 〔现名：radiogroup「排序」（表头排序改为分段选择：CPU / 内存 / 名称 / PID，旁边一个升降序按钮）〕 | 无（本地排序） | — | — | 无 | —（本地） |
| 冗余行「终止」 | button「终止」（每行同名；建议 `aria-label`「终止 {进程名} ({PID})」） 〔现名：button「终止 {进程名}」（如「终止 zte_dua（802）」）〕 | POST `/api/system/kill-bloat` `{pids:[pid]}`（agent 只杀名单内的，否则放进 `skipped`） | 1 | 无读回 | 「终止进程 PID {pid}？」 | 三（结束进程） |

写接口假成功风险：否（kill 结果逐条写在 `killed`/`skipped` 里；但页面不显示 `skipped`，被跳过的会被当成成功，页面层面属于假成功）。

---

## /tools/speedtest 测速

文件：`app/(panel)/tools/speedtest/page.tsx`。只做 WAN 测速（agent 从外网拉 speedtest 服务器列表，在设备上跑下载/上传）。agent 另有局域网测速接口 `/api/lan/ping|download|upload`，**web 端没有任何页面调用**。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 服务器下拉选项（运营方 — 名称, 国家） | `/api/speedtest/servers`（GET，**会从设备访问外网**拉列表，有缓存） | `[].id/sponsor/name/country`（`host`、`url` 不显示） | 无（进页拉一次） | 否（拉取失败回 503，speedtest.rs:355-360） |
| 服务器列表加载错误（下拉下方红字） | 同上 | `error` | — | — |
| 进度：阶段（空闲/正在测量延迟…/正在测试下载…/正在测试上传…/完成/已取消/错误）+ 百分比 + 进度条 | `/api/speedtest/progress` | `phase`、`progress`（0–100 整数） | 页面 `setTimeout` 链 1000（运行中） | 否（agent 内存状态） |
| 实时速率 Mbps | 同上 | `live_speed_mbps` | 1000 | 否 |
| 错误原因 | 同上 | `phase=error` 时的 `error` | 1000 | 否 |
| 结果卡：延迟 / 抖动 / 下载 / 上传 | 同上 | `ping_ms`、`jitter_ms`、`download_mbps`、`upload_mbps`（完成时） | — | 否 |
| （接口有、页面没显示） | 同上 | `server`、`download_bytes`、`upload_bytes` | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 服务器下拉（自动（最佳服务器）+ 列表） | 无（label 未关联）；建议 combobox「服务器」 | 无（草稿） | — | — | 无 | —（本地） |
| 「开始」 | button「开始」 | POST `/api/speedtest/start` `{}` 或 `{server_id}` | 1，之后每秒 GET progress 直到 complete/cancelled/error | 无读回（结果即进度接口的数值） | 无 | 二（消耗蜂窝流量） |
| 「停止」（运行中） | button「停止」 〔交互后：测速进行中才有；点 button「开始」，点 button「确认：{x}」〕 | POST `/api/speedtest/stop` `{}`（错误被页面忽略） | 1 | `/api/speedtest/progress` `phase=cancelled` | 无 | 二 |

---

## /usb USB 模式

文件：`app/(panel)/usb/page.tsx`。

#### 数据项

| 显示内容 | 接口 | 字段 | 刷新间隔 | 假成功风险 |
|---|---|---|---|---|
| 线缆（已连接/已断开） | `/api/usb/status` | `connect` | 3000 | 否·透传（usb.rs:6，ubus `zwrt_bsp.usb list`） |
| USB-C CC | `/api/usb/status` | `typec_cc` | 3000 | 否·透传 |
| 模式 + 模式按钮高亮 | `/api/usb/status` | `mode`（debug/mtp/rndis） | 3000 | 否·透传 |
| 充电器类型 | `/api/device/charger` | `charger_type`（原始数字） | 5000 | 否·透传（device_ext.rs:15） |
| 直接供电：X | `/api/device/charger` | `direct_power_supply_mode`（注意 CLAUDE.md：enable=停充、disable=充电，反的） | 5000 | 否·透传 |
| 充电宝开关状态 | `/api/device/charger` | `otg_powerbank_state` | 5000 | 否·透传 |
| 接口里有、页面没显示 | `/api/usb/status`、`/api/device/charger` | `usb2rj45`、`charge_status`、`charger_connect` | — | — |

#### 控件

| 控件 | 可访问名称 | 接口 + 方法 + 请求体要点 | 步骤 | 读回 | 现有确认 | 档位 |
|---|---|---|---|---|---|---|
| 充电宝开关 | switch「充电宝已开启」/「充电宝已关闭」（随状态变）；建议 switch「充电宝模式」 | PUT `/api/usb/powerbank` `{state:1/0}`（ubus `zwrt_bsp.powerbank set`） | 1 | `/api/device/charger` `otg_powerbank_state` | 无 | 二 |
| 模式按钮「DEBUG」「MTP」「RNDIS」 | button「DEBUG」「MTP」「RNDIS」（当前模式只靠样式，无 `aria-pressed`） 〔现名：radio「DEBUG」「MTP」「RNDIS」〕 | PUT `/api/usb/mode` `{mode:"debug"/"mtp"/"rndis"}` | 1 | `/api/usb/status` `mode` | 「将 USB 模式更改为“{模式}”？这将断开当前会话。」 | 三（USB 模式；选 DEBUG 等同开 ADB） |

写接口假成功风险：两者都是「否·透传」（usb.rs:13-33）。

---

## 汇总

### 1. 路由数

`find web/src/app -name page.tsx | wc -l` = **43**，本文 43 个路由各有一节（另加一节「全局」）。已用脚本核对：本文 `## /…` 标题与 `app/(panel)` 下的 `page.tsx` 一一对应，没有遗漏或多出。

不在侧栏 NAV 里的路由：`/login`（登录页）、`/sms/compose`（从 `/sms` 的「写短信」进）、`/router/home-mode`（已被情景模式取代，只能输网址进入）。

### 2. `window.confirm()` 调用点：16 处，12 个页面

| # | 页面 | 位置 | 触发的控件 | 确认文案（中文） |
|---|---|---|---|---|
| 1 | /bandlock | `bandlock/page.tsx:85` | 「重置 / 全部解锁」 | 重置所有频段锁定？设备将切换为自动选择频段。 |
| 2 | /router/apn | `router/apn/page.tsx:216` | 条目删除 | 删除 APN "{名称}"? |
| 3 | /router/celllock | `router/celllock/page.tsx:119` | 「全部解锁」 | 重置所有小区锁定？ |
| 4 | /router/esim | `router/esim/page.tsx:233` | 条目「切换」 | 切换到“{名称}”?…预计短暂断网(通常约 40 秒,无需重启)。 |
| 5 | /router/esim | `router/esim/page.tsx:264` | 条目删除 | 从卡上永久删除配置“{名称}”?此操作不可撤销。 |
| 6 | /router/mobile-network | `router/mobile-network/page.tsx:129` | 飞行模式开关（仅打开时） | 启用飞行模式？这将关闭蜂窝无线电。 |
| 7 | /router/mobile-network | `router/mobile-network/page.tsx:217` | 「重启」 | 现在重启路由器？ |
| 8 | /router/stc | `router/stc/page.tsx:98` | 「重置白名单」 | 重置 STC 白名单？此操作无法撤销。 |
| 9 | /sms | `sms/page.tsx:190` | 「删除 (N)」 | 删除 {N} 条短信? |
| 10 | /sms/forward | `sms/forward/page.tsx:375` | 规则删除 | 删除此规则? |
| 11 | /sms/forward | `sms/forward/page.tsx:419` | 「清空日志」 | 清空所有日志? |
| 12 | /tools/at | `tools/at/page.tsx:79` | 「发送」危险命令 | 警告：{原因}…确定要发送：{命令} 吗？ |
| 13 | /tools/enable-adb | `tools/enable-adb/page.tsx:19` | 「启用 ADB 调试 USB 模式」 | 启用 ADB 调试 USB 模式？… |
| 14 | /tools/processes | `tools/processes/page.tsx:78` | 行「终止」 | 终止进程 PID {pid}？ |
| 15 | /tools/processes | `tools/processes/page.tsx:103` | 「清除全部冗余进程」 | 终止全部 {N} 个冗余进程？ |
| 16 | /usb | `usb/page.tsx:74` | 模式按钮 | 将 USB 模式更改为“{模式}”？这将断开当前会话。 |

其中 7 处写的是裸 `confirm(`（bandlock、celllock、stc、sms、sms/forward×2、usb），其余是 `window.confirm(`，行为相同。以上 16 处全部定为第三档。没有任何第一档控件带 `window.confirm`，所以不存在「第一档却要第三档确认」的冲突。

**页面内自定义确认（非 window.confirm，未按规则自动升第三档）**：/router/device「确认重启」「恢复出厂」两段式；/router/firewall DMZ 启用、端口转发规则删除；/router/lan「仍然应用」；/router/wifi「确认」；/router/telemetry 域名移除。

### 3. 一次操作发出多个请求的地方

| 页面 | 控件 | 请求顺序 |
|---|---|---|
| /bandlock | 应用 NR 锁定 | ① POST `/api/cell/band/nr` `{nr5g_type:"nsa"}` → ② POST `/api/cell/band/nr` `{nr5g_type:"sa"}`（页面上的 NSA/SA 单选不起作用） |
| /router/apn | 保存修改（勾了「设为使用中」） | ① PUT `/api/router/apn/profiles` → ② POST `/api/router/apn/profiles/activate` |
| /router/apn | 运营商 IPv6(WAN) 开关 | 1 个请求；agent 内部：读 APN → `set_apn_at_cid` → `set_qcliiface`（最后一步失败被忽略） |
| /router/dns | 应用（自动 / 手动） | ① PUT `/api/router/dns` → ② POST `/api/doh/disable` |
| /router/dns | 应用（DoH） | ① PUT `/api/doh/config`（字段名错，实际不改上游）→ ② POST `/api/doh/enable` |
| /router/celllock | 扫描邻区 | ① POST `/api/cell/neighbors/scan` → 等 3 秒 → ② GET `/api/cell/neighbors/nr` → ③ GET `/api/cell/neighbors/lte` |
| /router/mobile-network | 关闭飞行模式 | ① POST `/api/modem/online` → 失败时等 3 秒 ② 再 POST 一次 |
| /router/mobile-network | 扫描运营商 | ① POST `/api/netinfo/scan` → 每 3 秒 GET `/api/netinfo` 看 `scan`（≤80 次） |
| /router/mobile-network | 选择（注册运营商） | ① POST `/api/modem/register` → 每 3 秒 GET `/api/modem/register/guard`（≤40 次） |
| /router/mobile-network | 回到自动选网 | ① POST `/api/modem/netselect/auto` → 每 3 秒 GET `/api/modem/register/guard`（≤40 次） |
| /router/network-mode | 应用 | ① PUT `/api/modem/network-mode` → 每 2 秒 GET `/api/network/signal`（≤5 次读回） |
| /router/scenario | 创建默认情景 / 添加国外情景 | ① GET `/api/scenario/template` → ② PUT `/api/scenario` |
| /router/scenario | 引擎开关（关） | 1 个请求；agent 内部还会同步执行 `bootsafe` + `run_restores`（恢复外出情景、开 Wi-Fi 等）——但因双重编码 agent 永远当作「开」 |
| /router/signal-detect | 开始检测 / 停止检测 | 开始：POST start → 每 2 秒 GET progress；停止：① POST stop → ② GET results |
| /router/telemetry | 拦截已知遥测 | 最多 4 次 PUT `/api/router/domain-filter`（逐个域名，失败静默） |
| /router/wifi | 确认 | 1 个请求；agent 内部：uci set ×N → commit `wireless`/`zte_mbb` → 后台线程 reload + 验证（HTTP 已先回） |
| /router/wifi-guest | 应用 | 1 个请求；agent 内部同上（后台 reload） |
| /router/home-mode | 扫描 | 1 个 GET；Wi-Fi 被关着时 agent 内部：开 2.4G（uci+commit+reload）→ 扫描 → 关回（uci+commit+reload+验证） |
| /router/esim | 切换 | POST `/api/esim/switch` → 每 1.5 秒 GET `/api/esim/job`；agent 内部：lpac enable → qmi simreset → 重启 `zte_topsw_mdm` → 最多 30 秒等收敛 → 否则重启整机；重启后每 4 秒 GET `/api/public/status` 直到回来 |
| /router/esim | 删除 / 下载 | POST → 轮询 `/api/esim/job`；agent 内部删/下载后再补发通知 |
| /router/esim | 全部发送（通知） | POST `/api/esim/notifications/process` → 每 2 秒 GET `/api/esim/job`（≤60 次） |
| /tools/speedtest | 开始 | POST `/api/speedtest/start` → 每秒 GET `/api/speedtest/progress` |
| 全局 / /alerts | 知道了 / 全部标为已读 | POST `/api/alerts/read` → 重拉 `/api/alerts` + `/api/public/status`（读） |

### 4. 假成功风险 = 是 的接口

**写接口（agent 吞掉失败仍回 ok）**

| 接口 | 位置 | 问题 |
|---|---|---|
| PUT `/api/wifi/settings` | wifi.rs:251-259、264-300、280-291、294-298 | `zte_mbb` 键（含 `wifi_onoff` 总开关、`wifi6_switch`）写失败静默跳过；只改这两个键时根本不 reload；需要 reload 时放后台，HTTP 先回 ok；发射功率热生效结果被丢掉 |
| PUT `/api/wifi/guest` | wifi.rs:370-378、389 | 段不存在时静默跳过；reload 放后台 |
| PUT `/api/homemode` | homemode.rs:168-173 | 启停标志文件写/删结果被丢掉 |
| PUT `/api/router/wan-ipv6` | router.rs:400 | 实时开关 IPv6 支路 `set_qcliiface` 结果被丢掉 |
| PUT `/api/doh/config` | doh/config.rs:29-35 + doh/mod.rs:87-95 | 页面发 `upstreams`，agent 只认 `upstream_url`，等于空补丁仍回 ok（页面字段 × agent 宽松解析） |
| POST `/api/doh/enable` | server.rs:449-454、doh/mod.rs:119 | 写 dnsmasq drop-in、重启 dnsmasq、保存 enabled 的结果都被丢掉 |
| POST `/api/doh/disable` | server.rs:458-471 | 恢复 dnsmasq 的 shell 命令结果被丢掉 |
| POST `/api/sms/delete` | sms.rs:86 | sqlite 核对失败时直接信任 ubus（而 ubus 对 SIM 短信会「返回 result:3 却不删」） |
| POST/PUT/DELETE `/api/scheduler/jobs`、PUT `/api/scheduler/jobs/toggle` | scheduler.rs:154-158 | `save()` 写盘失败被丢掉，重启后丢任务 |
| PUT `/api/sms/forward/config`、POST/PUT/DELETE `/api/sms/forward/rules`、PUT `/api/sms/forward/rules/toggle`、POST `/api/sms/forward/log/clear` | sms_forward.rs:1064-1074 | `save_config`/`save_state` 写盘失败被丢掉 |
| PUT `/api/scenario` | scenario.rs:586-593（调用处 1631） | `write_json` 写盘失败静默 |
| POST `/api/scenario/pin` | scenario.rs:1662、1666 | pin 文件写/删结果被丢掉 |
| PUT `/api/scenario/enabled` | scenario.rs:1684、1687、1690 | 标志文件写/删结果被丢掉；缺 `enabled` 字段时默认 true |
| POST `/api/esim/switch` | esim.rs:436-449 | 身份没收敛时任务照样标 `done` 然后重启整机 |
| POST `/api/ussd/respond` | telephony.rs:430-441 | 不检查 `ERROR`，原样当回复返回 ok |

**读接口（下游读失败时回 ok + 默认值，页面会把「读不到」显示成一个确定的状态）**

| 接口 | 位置 | 表现 |
|---|---|---|
| GET `/api/public/status` | public.rs:31、43、53、107 | 读不到时显示为：未连接、Wi-Fi 关、0 条未读 |
| GET `/api/wifi/status` | wifi.rs:12-18、55-67、84-103 | uci 读不到为空串；`wifi_onoff` 兜底 "1"（显示为开）；连接数兜底 0 |
| GET `/api/wifi/guest` | wifi.rs:12-14、320-324 | 同上；剩余时间兜底 −1 |
| GET `/api/network/clients` | network_ext.rs:28-32 | luci-rpc 失败时列表为空 |
| GET `/api/router/lan` | router.rs:55-60 | 每项 `uci get` 失败为空串 |
| GET `/api/router/wan-ipv6` | router.rs:338-345 | `wan_has_ipv6` 读不到当 false |
| GET `/api/device/charge-control` | device_ext.rs:45-60 | 充电器读不到当「未停充」、电量 0 |
| GET `/api/services/tailscale` | services.rs:52-86 | 执行/解析失败时 ok + `error` 字段（页面会显示） |

**页面层面的假成功（agent 正常报错或没问题，但页面没反映出来）**

- /router/scenario：双重 JSON 编码，「固定」实际永远是取消固定、「关引擎」实际是开引擎，页面都提示成功。
- /router/dns：DoH 上游改不进去却提示「DoH 已启用」。
- /router/telemetry「拦截已知遥测」：单个失败被吞，最后照样提示成功。
- /tools/processes：agent 把跳过的进程放 `skipped`，页面不显示。
- /sms/compose：发送固件失败（ubus 退出码 0）时照样跳回收件箱。

**否·透传（只看 ubus 退出码，不看返回体 `result`；页面也都不看）**——这些写接口不列为「是」，但安全白名单要单独决定怎么对待：
`/api/router/dns`、`/api/router/lan`、`/api/router/firewall/{switch,level,nat,dmz,upnp,port-forward,port-forward/switch}`、`/api/router/vpn`、`/api/router/qos`、`/api/router/domain-filter`、`/api/router/apn/{mode,profiles,profiles/delete,profiles/activate}`、`/api/cell/{band/nr,band/lte,band/reset,lock/nr,lock/lte,lock/reset,neighbors/scan,stc/params,stc/enable,stc/disable,stc/reset,signal-detect/start,signal-detect/stop}`、`/api/modem/{data,airplane(LPM),network-mode,scan,register}`、`/api/sim/{pin/verify,pin/change,pin/mode,unlock}`、`/api/usb/{mode,powerbank}`、`/api/device/{reboot,factory-reset,power-save,fast-boot}`、`/api/sms/{send,read}`。其中只有 /router/network-mode（轮询 `net_select`）和运营商注册（轮询 `register/result`）页面自己做了读回。

### 5. 有副作用的 GET（供安全白名单用，从严）

| 接口 | 判定 | 依据 |
|---|---|---|
| GET `/api/homemode/scan` | **有** | homemode.rs:235-303：Wi-Fi 被在家模式关着时，`uci set wireless.wifi0.disabled=0` + commit + `zwrt_wlan reload` 叫醒 2.4G，扫完再 `disabled=1` + commit + reload + 验证；期间持有 Wi-Fi 锁，最长十几秒 |
| GET `/api/scenario/scan` | **有** | wifi_scan.rs:187-214：没有可用接口时 `iw phy … interface add scen-scan0` + `ip link set … up`，扫完删除；扫描本身占用射频 |
| GET `/api/health?refresh=1` | **可能有** | health.rs:184-188：同步执行 `/data/u60-guard/doctor.sh --tsv`（最长 20 秒，占用 agent 工作线程）；grep 该脚本未见 uci set/commit/restart，但它是外部脚本，不保证 |
| GET `/api/stk/menu` | **可能有** | telephony.rs:256-278、457-521：发 `AT+CUAD`、`AT+STIN?`、`AT+CUSATD=1`、`AT+STGI=…`；`AT+CUSATD=1` 是 USAT 激活类命令，可能改变调制解调器 STK 状态 |
| GET `/api/network/qos` | 可能有（占用 AT 口） | qos.rs:18-70：每次发 `AT+CGCONTRDP` + 每 cid 一条 `AT+CGEQOSRDP`，命令本身只读；/router/qci 每 5 秒轮询 |
| GET `/api/speedtest/servers` | 可能有（外网请求） | speedtest.rs:131-143：缓存过期时从设备访问外网拉服务器列表，不改设备状态 |
| GET `/api/esim/status`、`/api/esim/profiles`、`/api/esim/notifications` | 可能有（卡片 APDU） | esim.rs:265-315：跑 lpac 读卡，代码注释称只读、不计入 eSTK.me 的 catBusy 冷却 |
| GET `/api/at/port` | 可能有（占用 AT 口） | at_cmd.rs:23-53：端口未缓存时，对每个候选串口 `cat` 并写入 `AT\r` 探测（每口约 1.3 秒）；探到后缓存，之后只读缓存。/tools/at 进页时调用 |
| GET `/api/call/status` | 可能有（占用 AT 口） | telephony.rs:334：发 AT 查询（web 端没有页面调用） |

**名义上是 POST、实际是读取的接口**（按方法做白名单时会被误判为写）：POST `/api/sms/list`（/sms 每 10 秒轮询）、POST `/api/device/power-save`（/router/device 读省电模式）。另：POST `/api/auth/login` 被 /settings「测试连通性」当探活用。

其余 GET（`/api/public/status`、`/api/wifi/status`、`/api/network/*`、`/api/router/*` 各 GET、`/api/sim/*` GET、`/api/cell/*` GET、`/api/modem/*` GET、`/api/alerts`、`/api/scenario`、`/api/scheduler/jobs` 等）在代码里只做 ubus/uci 读、读文件、或更新 agent 内存里的采样状态（`/api/cpu`、`/api/network/speed`、`/api/system/top`），未发现写设备状态。

### 6. 读回缺口（改版做「写后读回」时要注意）

- 锁频、锁小区、STC 白名单：agent **没有**读取当前锁定配置的 GET，只能间接看 `/api/network/signal` 的驻留小区/频段。
- Wi-Fi：`/api/wifi/status` 只是 uci 配置；「是否真的在广播」要看 `/api/wifi/radio` 的 `beaconing`（目前没有页面读它）。
- 修改 PIN、订阅链接、USSD/STK、AT、短信发送、测速、结束进程、开 ADB、重启、恢复出厂：无读回。
- eSIM 切换：读回是 job 的 `status` + 最终状态字段。

### 7. 代码里发现、改版时要一并处理的缺陷

1. /router/scenario 全部写请求双重 JSON 编码（见该节）。
2. /router/dns DoH 上游字段名 `upstreams` ≠ `upstream_url`；DoH 缓存表读 `name`，agent 给的是 `domain`。
3. /scheduler 星期编号错一天（页面 0=周日，agent 0=周一）。
4. /sms/forward「测试规则」「重试失败项」请求体与 agent 不符，必回 400；轮询间隔下限页面 5、agent 10。
6. /settings「测试连通性」会把自己登出；「轮询间隔」没人读。
7. /router/wifi-guest「隐藏 SSID」读的键名不对（agent 给 `hidden`）。
8. /tools/processes 成功提示显示「[object Object]」；冗余名单含开机同步名单里的守护进程。
9. /bandlock NSA/SA 单选无效。
10. /sms「全部」和「设备」筛选结果相同。
11. Wi-Fi 总开关走 `/api/wifi/settings` 的 `wifi_onoff`（见第 4 节第一行）。
12. `web/src/lib/ubus.ts`、`lib/adb.ts`、`lib/crypto.ts` 无人引用。
13. 大量输入框的 `<label>` 没有和输入框关联（没有 `htmlFor`/包裹），e2e 用 `getByLabel`/`getByRole(name)` 找不到；很多开关的名称随状态变（「开/关」「已启用/已禁用」），不适合做定位。本文「建议」列给了替代名。

### 8. 建议评审时复核的档位

以下按规则落在第二档，但风险接近第三档，建议定档时再看：`/router/mobile-network` 移动数据开关（远程时关掉会断开自己）、运营商扫描；`/router/sim` PUK 验证与 NCK 网络解锁（页面自称不可逆）；`/router/esim` 下载新配置；`/scheduler` 任意接口的定时任务（本文按「目标接口的档位」处理）；`/sms/forward`「转发后删除」；`/router/home-mode` 启用（会自动关 Wi-Fi，已按二·远程三）；`/router/stc` 启用/关闭（按锁小区记为三）；`/router/scenario` 每个情景的 Wi-Fi 开关（已按二·远程三）。
/router/dns「应用」按第一档（DNS/DoH 上游）记录，但它同时会调 POST `/api/doh/enable` 或 `/api/doh/disable`——这两个会改写 dnsmasq 配置（drop-in 文件、`uci delete dhcp.lan_dns.*` + commit）并重启 dnsmasq，全局 DNS 会短暂中断。如果安全白名单按接口定档，这两个接口需要单独决定（本文倾向第二档）。
另：第一档里的「界面设置·深浅色」「关 Tailscale（二·远程三）」目前没有任何对应控件。

### 9. 没能确定的

- ubus 透传接口返回体的真实字段名（固件文档不在仓库里）：`/api/network/signal` 的 `nr5g_band`/`lte_band`、`net_select_mode` 的取值含义；`/api/device/system` 是否含 `hostname`/`kernel`；`/api/sms/capacity` 键名；`/api/router/firewall` 及其子接口、`/api/router/vpn`、`/api/router/qos`、`/api/router/domain-filter`、`/api/cell/stc/*`、`/api/cell/neighbors/*`、`/api/cell/signal-detect/*`、`/api/modem/scan/*`、`/api/modem/register/result` 的结构；APN 的 `cid` 能否当 `profileId`、PDP 类型传字符串还是数字；LAN 的 `dhcp_start` 和租约时间格式。以上在表里都标了「未确认」，需要在设备上抓一次真实返回再定。
- 只改 `zte_mbb.wifi.wifi_onoff` 时固件是否有别的进程监听并实际开关 Wi-Fi。
- `nwinfo_reset_band_cell_setting`（小区锁定页「全部解锁」）是否也会重置频段锁定。
- 手动搜网（`nwinfo_manual_scan`）期间数据连接是否中断。
- `AT+CUSATD=1`（STK 菜单 GET）对调制解调器的实际影响。
- `doctor.sh` 是否绝对只读（只做了 grep）。

## 9. 清单整理后已修的问题（2026-09-24，本地提交，未上机）

上面各表记的是修之前的代码。下面这些已改，迁移时按修好的行为搬：

| 提交 | 位置 | 改了什么 |
|---|---|---|
| `fb4aedc` | `zte-agent/src/system.rs` | 「一键结束」列表去掉 daemon.conf 里的 7 个守护进程，另列 `SYNC_BARRIER_DAEMONS`，单元测试保证两份列表不重叠 |
| `7546a4d` | `/router/scenario` | 5 处写请求不再二次 `JSON.stringify` |
| `c6a7825` | `/router/dns` | DoH 上游发 `upstream_url` |
| `1a1fc45` | `/scheduler` | 星期按 agent 的 0 = 周一编号 |
| `2c612db` | `/sms/forward` | 测试规则发 `destination`；重试失败项逐条发 `{ index }` |
| `8d84645` | `/settings` | 测试连通性改用 `/api/public/status`，不再用空密码登录 |
| `8bbc946` | `/router/wifi-guest` | 隐藏 SSID 读 `hidden` |

**没改、等上机再定：** `/router/wifi` 总开关写 `zte_mbb.wifi.wifi_onoff`，只改这一项时 agent 不重载，也不确认生效（`wifi.rs:264-300`）。要先在设备上确认这个 uci 键单独改是否起作用，再决定改用 `/api/wifi/radio`，还是让 agent 在这种情况下重载并读回。
