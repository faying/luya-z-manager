# 可靠性契约：设备上各程序之间的文件约定

[English](RELIABILITY.md) · **中文**

设备上的程序彼此只通过下面这些文件了解对方。改其中任何一个，读的一方和写的一方必须一起改。

现行程序（都由 procd 监督，rc.local 里各一行 `/etc/init.d/<名字> start`）：

| 程序 | 位置 | 负责 |
|---|---|---|
| 核心（u60d） | `/data/luyaz/luyaz-core`，状态 `/data/luyaz/core/` | 情景引擎、告警短信（正常路径）、服务崩溃监视、基带崩溃监视、待机记录、doctor、:9460 的 `/v2` 接口 |
| 哨兵 | `/data/luyaz/luyaz-sentinel`，状态 `/data/luyaz/screen/` | 屏幕主人、Wi-Fi 兜底看门狗、时钟判定、dropbear 监视、核心不健康时代发告警短信 |
| 管理网页 | luyaz-web :9090，密码 `/data/luyaz/web.env` | 管理页面；只跟核心说话 |
| 触屏 | `/data/luyaz/ui/luyaz-ui`（comm `luyaz-ui`） | 只由哨兵拉起和接管 |

为已退役的 zte-agent、u60-guard、u60-uid、supervise.sh 写的旧契约在 [`archive/RELIABILITY-pre-phase10.zh-CN.md`](archive/RELIABILITY-pre-phase10.zh-CN.md)（只作记录）。

## 1. Wi-Fi 锁 `/tmp/luyaz-wifi.lock`

所有写 `wireless` 这个 uci 包的程序，从第一次 `uci set` 到 reload 回验结束都持有它（`flock` 排他锁）。

- 库：`crates/luyaz-wifi`（`MAX_HOLD` 120 秒、`ENGINE_WAIT` 150 秒、`HTTP_WAIT` 5 秒）。每次拿锁都以 `O_CLOEXEC` 打开，uci/ubus 子进程不会继承；持锁者把 pid 写进去；文件从不删除。
- 核心 `/control` 先拿 `/var/run/luyaz-write.lock` 再拿 Wi-Fi 锁（`ops/wifi_lock.rs`；等 150 秒、试 2 次，不行回 `503 busy`；持锁 pid 是调用者自己或其祖先时不等）。
- 哨兵（`luyaz-sentinel/src/wifi.rs`）最多等 150 秒，拿不到这一轮算失败，下轮再试。
- **谁都不杀持锁者。**「卡住就杀」已经没有了。
- shell 写者：`( flock 9; echo $$ > /tmp/luyaz-wifi.lock; … ) 9>>/tmp/luyaz-wifi.lock`。用 `>>` 打开，不要用 `>`。busybox `flock` 没有 `-w`，限时用 `flock -n` 轮询加截止时间。

## 2. 情景心跳 `/tmp/scenario.heartbeat`

- 写者：核心里的情景引擎（`crates/luyaz-scenario`），bootsafe 前写一次，之后每轮一次，引擎关着也写。内容：`/proc/uptime` 的整数秒加换行，临时文件 + rename。它证明引擎线程活着，不证明在切换。
- 判定：哨兵（`worker.rs` 守护轮，每 60 秒）。开机 300 秒内不判；见过之后超过 300 秒没更新算丢；开机 900 秒还从没见过也算丢。两轮间隔超过 150 秒说明设备睡过，引擎重新获得完整的 300 秒。
- 丢了哨兵就把 Wi-Fi 打开（第 3 节）并告警 `agent-silent`（类别名沿用）。恢复重试按 60、120、240、480 秒退避。
- doctor 也读它。

## 3. 接管标记 `/tmp/luyaz/wifiguard.took-over`

- 哨兵接管 Wi-Fi 时写，**每次接管只写一次**，内容是当时的开机秒数（告警 `wifi-takeover`；恢复失败告警 `wifi-restore-failed`）。
- 标记在时，情景引擎不做检测、不理会固定，进入并停在「外出」。
- 内容不变、稳定运行 600 秒后，引擎删掉它收回控制；内容变了重新计时。只有引擎删（或重启）。
- 核心的 status/netinfo 接口和 doctor 也读它。

## 4. 告警目录 `/data/alerts/`

放在 `/data`、权限 700：重启循环既不能清掉告警，也不能重置短信额度。所有读写都经 `crates/luyaz-alerts`。

| 文件 | 内容 |
|---|---|
| `lock` | 在这个目录里做「读 → 改 → 写」都持有它的 flock |
| `seq` | 最后分出去的序号 |
| `queue` | 事件，`<序号>\t<墙钟秒>\t<开机秒>\t<类别>\t<说明>` |
| `read` | 已读游标（网页） |
| `sms-to` | 告警短信号码；没有或为空 = 不发 |
| `sms-abroad` | 存在 = 在国外也发 |
| `abroad` | 存在 = 当前情景按 SIM 国家码判定为国外（情景引擎写） |
| `sms-done` | 短信已处理到的最大序号 |
| `sms-sending` | 发送认领 `<序号>\t<发送者>\t<pid>\t<boot_id>\t<开机秒>\t<墙钟秒>`；超过 120 秒、来自上次开机、或 pid 已死，算「不确定」 |
| `sms-log` | `<墙钟秒>\t<序号>\t<类别>\t<结果>`，保留最后 100 行 |

- `add` 在 `lock` 下分配序号、追加，超过 300 行剪到最后 200 行。`add_once`（哨兵事件）在说明前加 `[<bootid8>-<n>] `。游标存序号，不存行号。
- 墙钟秒小于 `1704067200`（2024-01-01）表示「时间没校准」，网页显示「开机后 N 秒」。
- 说明：只用可打印 ASCII，≤ 120 字符，绝不含任何配置（号码、密码、SSID）。格式不对的行跳过。
- 现在会产生的类别：
  - 哨兵：`agent-silent`、`wifi-takeover`、`wifi-restore-failed`、`dropbear-down`、`dropbear-restarted`、`devui-crash`、`devui-gave-up`
  - 核心 svcwatch：`datad-crash`（核心自己）、`web-crash`、`sentinel-crash`
  - 核心哨兵监视：`sentinel-silent`、`screen-empty`（只在空屏兜底开关打开时）
  - 核心引擎：`home-exit-down`、`home-exit-check-failed`、`sms-forward-stuck`
  - 触屏：`devui-theme-paused`

### 短信

- 一份实现 `Alerts::sms_round`，两个可能的发送者，不会同时发：
  - **核心**（`engines/guard_duties.rs`，每 30 秒）：`/tmp/luyaz/handover` 里列了 `sms` 且写开着时——正常情况；
  - **哨兵**：只在核心不健康（连续 2 轮 `/healthz` 不通）且没在被 ship 替换时。
  `sms-sending` 认领保证两者交接时不重复发。
- 规则：`sms-failed`、`devui-theme-paused` 永远不发；没有号码记 `no-number`；有 `abroad` 没 `sms-abroad` 记 `suppressed-abroad`；每类每小时最多 1 条、24 小时最多 5 条，超了记 `suppressed-rate`。时钟判定（第 9 节）可信之前什么都不发、游标不前进。
- 发送：`ubus call zwrt_wms zte_libwms_send_sms`（`number`、`message_body` 大写 UCS-2 十六进制、`encode_type` `UNICODE`、`sms_time` `YY;MM;DD;HH;MM;SS;+TZ`、`id` `-1`）。回复里 `result` 为 3 或没有 `result` 算成功，之后把已发副本从短信库删掉。失败记 `failed` 并追加 `sms-failed`，不重试。
- 正文：每个类别一句大白话，`【Luya-Z】…（MM-DD HH:MM）`，≤ 70 字；未知类别指向网页。

## 5. 告警接口（核心）

核心在本机回环上以 `/v2/api/…` 提供；luyaz-web（`alerts_page.rs`）是唯一界面，调的就是这些。

- `GET /api/alerts`：事件、未读数、短信设置、最近结果。号码只在这里出现。
- `POST /api/alerts/read` `{"seq": N}`、`PUT /api/alerts/sms` `{"number"?, "abroad"?}`：只在写开着时注册。
- `GET /api/health`、`GET /api/health/crashlog?program=&file=`。
- `POST /v2/sentinel/event`：哨兵把事件交给核心。
- 非回环监听必须带 `--auth-token-file`（`/data/luyaz/core/auth.token`）或明确的 `--lan-bind`。

## 6. 屏幕（哨兵，`crates/luyaz-sentinel/src/screen.rs`、`run.rs`）

- 哨兵是触屏唯一的主人：拉起它，按 comm `luyaz-ui` 接管已在跑的（绝不起第二份），切到原厂界面（`zte_topsw_devui`），原厂界面在屏上时识别右下角长按 3 秒切回来。
- `/data/luyaz/screen/attempts`：每次拉起前加一并 fsync，稳定 10 分钟清零；到 2 写 `/data/luyaz/screen/gave-up`、告警 `devui-gave-up`、交还原厂界面。放弃状态跨重启保留，长按或 `devui` 清除。文件坏了按 2 算。
- `/tmp/luyaz/screen.want` 以 `vendor` 开头 = 选了原厂界面；重启后回到我们的。
- `/tmp/luyaz-screen.ctl`（FIFO，哨兵运行时才有）：`vendor`、`devui`、`hold [秒]`（10–3600，默认 600）、`release`（后两个给 ship 用）。
- 自己拉起的界面意外退出写进 `/data/crashlog/u60pro-devui/`（留 5 个）并告警 `devui-crash`；接管来的进程拿不到退出码，不计。
- 哨兵心跳 `/tmp/luyaz-sentinel.hb`，事件 `/data/luyaz/screen/events{,.seq}`（留 200 条），日志 `/tmp/luyaz-sentinel.log`。
- **屏幕上几分钟没有任何界面，固件会整机重启**（原因码 1185）。任何停掉一个界面的路径，必须在同一步里拉起另一个。

## 7. 体检和备份

- `luyaz-core doctor`（进程名 `luyaz-doctor`；`/data/luyaz/scripts/doctor.sh` 是薄壳）。只读。输出：给人看的文字、`--tsv`（`<ok|warn|bad>\t<id>\t<名称>\t<说明>`）、`--tsv2`（头 `#tsv2`，英文）；有 bad 行退出码 1。另有 `--manifest`、`--tree-fp`、`--ledger-selftest`。
- `--calibrate-standby` 写待机基线（第 8 节）。`--report` 核心不做（退出 64），薄壳转给 `doctor-legacy.sh`。
- 核心自己跑体检（启动后 20 秒第一次，之后定时），快照由 `/api/health` 提供。
- `touch-ui/scripts/config-backup.sh`：只备份配置（清单在脚本开头），Tailscale 身份要 `--with-tailscale`。备份只存在电脑上（`./install.sh backup`）；`restore` 先 plan、写前留 `.pre-restore`、不 reload Wi-Fi。

## 8. 待机记录

- 写者：核心 `longterm/standby.rs`，只在 `/tmp/luyaz/handover` 列了 `standby` 时。背光关着时每轮往 `/tmp/standby.stat` 写一行：`<开机秒> <蜂窝包/分> <Tailscale 隧道包/分> <各程序唤醒/秒>`，留 60 行；中途换了 pid 的程序写 `-`；离上一轮超过 150 秒不写。
- 程序列按位置固定（doctor 按位置读），列名仍是改名前的 `tailscaled mihomo u60pro-devui zwrt-datad zte-agent`。
- 指纹 `/tmp/luyaz/ledger/fp`、`fp-changed`。基线 `/data/luyaz/scripts/standby.baseline`（v2，带 `fp` 行），由 `luyaz-core doctor --calibrate-standby` 写（至少 30 行记录；每列中位数和 MAD，再加指纹；经 `POST /v2/longterm/calib` 记一条 `calib` 事件），由 doctor 的「待机」行判定。

## 9. 时钟判定、崩溃记录、`.migrated`

- **时钟** `/tmp/luyaz/clock-ok`：`<1|0> <墙钟−开机秒> <开机秒> <依据>`（`vendor|jump|last_wall|year|revoked|none`），哨兵写；短信路径、核心长期存储、doctor 读。哨兵把 `/data/ledger/state/wall.last`（最多每小时一次）留作跨重启的依据。
- **服务崩溃**：核心 `svcwatch.rs` 写 `/data/crashlog/<名字>/`（每个程序 5 个、合计 1 MB），按第 4 节告警；主动停止或重启不算崩溃。核心自己的崩溃靠标记 `/data/luyaz/core/core.running` 发现。
- **基带崩溃**：核心 `crashwatch.rs`（kmsg + netlink），状态 `/tmp/luyaz/core/crashwatch.json`，事件进长期存储 `/data/luyaz/core/long`。
- **`.migrated`**：`/tmp/luyaz/.migrated` 由哨兵最后写，前提是 `clock-ok` 已有、handover 文件等于核心的全部职责；ship 拉起哨兵前先删它。`/data/luyaz/core/.migrated` 是核心新布局的开关。目录在却没有它，doctor 报「搬了一半」。
