# 设计：信号历史记录（24 小时曲线 + 事件）

由 /office-hours 生成，2026-09-25 · Status: 已并入 `slow-diagnosis.md` 作为数据底座（用户 9-25 指出单纯记录对移动场景没用）；该文第 5 节的修订（被动存活判断、stall 事件、曲线降为次要）优先于本文 · Mode: Builder
涉及仓库：source/manager（zte-agent + web）；触屏以后接同一个接口

## 问题
弱信号场景下（电梯、地下室、山里、高铁）说不清到底发生了什么：信号掉到多少、有没有换小区或换频段、断网持续了多久、放在哪里更好。现在设备上**没有任何信号历史**：网页信号页的曲线只是浏览器内存里的约 2 分钟（`web/src/components/signal/TrendChart.tsx`），关页面就没了。
这也是后面几件事的前提：判断"是设备问题还是网络没开某项功能"、弱信号自动恢复、摆放位置指导。

## 范围边界
- 这一轮**只读、只记**。不改射频、频段、制式、锁频、APN，不写基带分区，不调 `nwinfo_scan_nbr`（会断数据）。
- 发射功率、基带 NV 参数不在本项目范围内。

## 现状（本地代码核对）
- **datad 一直在读信号，不管有没有人看**：`data-service/rust/src/server.rs:111` 的采样循环常驻；`nwinfo_get_netinfo` 走 `ubus_ttl(0, …)`，不缓存（`state.rs:948`）。节奏由触屏设置：亮屏 1 s、息屏 5 s（`touch-ui/src/data.c:907`）。
- **真机 `/state` 实测（2026-09-25，设备上的 datad `6bbf6ea6`）**：不带 token 返回 200。`net` 里有 `type`（SA/NSA/…）、`band`、`nr_band`、`nr_bw`、`nr_rsrp`、`nr_rsrq`、`nr_snr`（字符串 `"20.1"`）、`nr_pci`、`nr_cell_id`、`nr_channel`、`lte_rsrp/rsrq/rssi/pci/cell_id/channel`、`lte_snr`（字符串）、`nrca`、`lteca`（当前载波聚合，分号分隔的逗号串）、`HSR`、`wan_status`（`"ipv4_ipv6_connected"`）、`lte_supported_bands`、`nr_sa_supported_bands`、`nr_nsa_supported_bands`、`operator`、`roaming`、`mcc/mnc`。本地源码 `state.rs:1417` 比设备上的版本字段少，**以真机为准**。
- 陷阱：SA 下 `lte_rsrp=0` 但 `lte_snr` 还是 `"-5.0"`，是上次 LTE 残留的旧值。**LTE 各项只在 `lte_rsrp` 不为 0 时有效**，NR 同理（`nr_rsrp` 不为 0）。
- zte-agent 现在自己调 ubus 读信号（`handlers.rs:111`），不经过 datad；已有 `ureq`，也有后台线程的写法（`scheduler.rs:173`）。
- 「网络观测台」因为担心采样耗电被推迟（`TODOS.md:303`）。按上面第一条，信号这部分不新增基带读取，这个顾虑不成立；新增开销只有本机 HTTP 和写盘。

## 已定的决定
- **方案 A**：记录放在 zte-agent，从 datad `/state` 取数，不新增基带查询。（D1，2026-09-25 用户选定）
- 不采用：B 放进 datad（分支离上游越来越远，只能在 x86/Docker 里编，换 datad 风险更大）；C 用 u60-guard shell 每分钟写 CSV（没有界面，分钟粒度抓不到短时间掉线）。
- 内存保留 24 小时，磁盘保留 7 天，每 10 分钟写一次盘。
- 时间按设备时钟原样存（当地时间标成 UTC），网页用 `deviceClock.ts` 显示，不做时区换算。

## 设计

### 采样（zte-agent 新模块 `signal_log.rs`）
- 后台线程每 **5 s** `GET http://127.0.0.1:9460/state`（不需要 token，已实测），超时 1 s。与 datad 息屏节奏对齐：再快也只会读到同一份数据。
- 取不到（datad 重启、超时）记为缺测，**不回退去调 ubus**：否则 datad 挂掉时 agent 反而加重基带负担。
- 数据连接以 `net.wan_status` 为准：以 `connected` 结尾、并且不包含 `disconnect` 才算已连接，和 `netinfo.rs:568` 的 `data_connected()` 判断方式一致。
- 字段缺失或是字符串（`"--"`、空串）就当作缺测，不当 0。

### 每分钟汇总（一条记录）
| 字段 | 说明 |
|---|---|
| `t` | 分钟起点（设备时钟秒） |
| `rat` | 这一分钟里占多数的制式：SA / NSA / LTE / 无服务 |
| `band`, `pci`, `cell` | 分钟末的服务小区 |
| `nr_rsrp_min/avg/max`, `nr_sinr_min/avg`, `nr_n` | NR 一组，只统计有效样本 |
| `lte_rsrp_min/avg/max`, `lte_sinr_min/avg`, `lte_n` | LTE 一组，分开存；NSA 下 NR 时有时无，混在一组会把两种值平均到一起 |
| `nrca`, `lteca` | 分钟末的聚合载波数（解析自同名字段），看聚合什么时候掉 |
| `hsr` | 这一分钟里是否出现 `HSR=true`（高铁模式） |
| `n` | 取到 `/state` 的次数（最多 12），用来区分"信号差"和"没采到" |
| `down_s` | 这一分钟里 `wan_status` 不是已连接的秒数 |

内存：1440 条 × 约 120 字节 ≈ 170 KB。

### 事件（精确到 5 s，单独一个列表）
- `cell_change`：PCI 或频段变化（前后值都记下来）
- `rat_change`：SA、NSA、LTE、无服务之间切换
- `data_down` / `data_up`：`wan_status` 变化，恢复时写入持续时间
- `ca_change`：聚合载波数变化
- `weak_enter` / `weak_exit`：当前主用制式（SA 取 NR，LTE/NSA 取 LTE）的 RSRP 连续 30 s 低于 −110 dBm / 连续 30 s 高于 −105 dBm（留 5 dB 回差，防止来回抖）
- 24 小时最多保留 2000 条，满了丢最旧的。

### 存储
- 目录 `/data/signal-log/`（`/data/zte-agent` 是二进制文件本身；和 `/data/alerts`、`/data/crashlog` 一样放在 `/data` 顶层），按天一个文件 `YYYYMMDD.jsonl`；每 10 分钟追加一次，只写新增部分。
- 启动时读回最近 24 小时进内存；超过 7 天的文件删掉。
- 写入量估算：每天约 170 KB 加事件，每 10 分钟写一次，对闪存可以忽略。
- 时钟没同步前（年份早于 2024）的数据只放内存，不写盘；时钟同步后第一次写盘时丢掉这些数据，不去猜时间。

### 接口
- `GET /api/network/signal/history?hours=24`：返回 `{minutes:[…], events:[…], gaps:[…]}`；`hours` 可取 1 到 168，超过 24 小时的部分从磁盘读。
- 和其他 `/api/network/*` 一样要登录。

### 网页（信号页）
- 保留现有 2 分钟实时曲线。下面加「过去 24 小时」：RSRP 画成 min 到 max 的色带加平均线，沿用 TrendChart 的固定 −130 到 −70 刻度和好、中、弱三段底色；掉线时段用红色竖条标出；缺测的地方断开，不连线。
- 曲线下是事件列表，最新的在最上面，用大白话写：「14:02 换到 n78 小区 PCI 312」「14:05 断网 38 秒」「14:10 信号偏弱（−114 dBm）持续 6 分钟」。
- 可以切 1 小时 / 6 小时 / 24 小时 / 7 天。
- 视觉按 `docs/DESIGN.md`，不用彩色左边框卡片。

## 失败模式
| 情况 | 表现 |
|---|---|
| datad 不在 | 曲线缺测断开；事件列表写一条「数据服务不可用」 |
| agent 重启 | 最多丢 10 分钟内存数据；启动后从盘上读回 |
| 磁盘写失败 | 只在内存里继续记，健康检查里出一条 warn |
| 时钟跳变（开机同步） | 按上文，没同步前不写盘 |

## 测试
- 单元测试（Rust，放在模块里）：分钟汇总（NR 和 LTE 分组、NSA 下 NR 时有时无、缺测、`n` 计数）、弱信号迟滞（−110/−105，30 s）、事件列表上限、跨天切文件、7 天清理、时钟没同步时不写盘。
- 单元测试补：SA 下 LTE 残留值被忽略、`"--"` 按缺测处理、`nrca` 解析。
- 本机：假 `/state` 服务器喂一段录下来的数据，跑 `/history`。
- 真机：旁路跑新 agent 30 分钟，对比网页曲线和触屏读数；等一次自然掉线（电梯、地下车库），确认出现 `data_down`/`data_up` 和持续时间。**不用飞行模式**：`nwinfo_set_mode ONLINE` 拉不回 LPM，只能重启（`source/manager/CLAUDE.md`）。
- 耗电：不单独做 A/B（用户 9-25 说耗电测试已结束）；上机后看一眼 agent 的 CPU 占用和唤醒次数，与上一版对比。

## 上机
- 走 agent 的常规流程：zigbuild 编译 → 旁路文件名跑 → 确认没问题后 `/etc/init.d/zte-agent restart` 换正式位置；上一版留作 `/data/zte-agent.prev-siglog-YYYYMMDD`，本地同 md5 放 `releases/device-backups/`。
- 网页随 `main` 一起编译推送。

## 以后（这一轮不做）
1. **弱信号自动恢复**：直接用这里的 `weak_enter` 和 `data_down` 事件。持续断网超过 N 分钟时做一次 `AT+COPS=0`（安全的重新搜网方式），并写进事件列表。要先和用户定 N，并确认不会和 u60-guard 抢着操作。
2. **能力核对**：第一层不用断网：`/state` 已经给出 LTE / NR SA / NR NSA 的支持频段和当前聚合状态，可以直接做成「设备声明支持 vs 实际用上」的对照。第二层（聚合组合、调制方式）原厂 ubus 没有只读接口（只有改频段的写接口），只能在入网时用 diag 日志抓 `UECapabilityInformation`，要重新注册，会断一次网，必须先问用户。可以复用 datad `neighbor_manager.rs` 的 `diag_mdlog` 调用框架，抓一次就停。
3. 触屏显示 24 小时迷你曲线：读同一个接口。
4. 「网络观测台」的网速、服务状态两条线：信号这条跑稳之后再说。

## 下一步
用 `/plan-eng-review` 过一遍这份文档，然后在 manager 开分支 `signal-history` 实现。
