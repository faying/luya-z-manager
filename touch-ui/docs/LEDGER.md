# 设备账本（ledger）：格式与约定

> Maintainer notes, Chinese only.

设备上的跨重启事件账：整机重启、基带崩溃和恢复、我们自己的程序退出和重启、每小时的耗电和资源。
写的一方是核心 luyaz-core 的长期层（manager `crates/luyaz-core/src/longterm.rs`、`longterm/`，崩溃观察在 `crashwatch.rs`、`crashwatch/`；
`LUYAZ_LONGTERM=1` 才开，设备上在 `core.env` 里开）。读的一方是 `<核心> doctor --report 24h|7d`（`/data/luyaz/scripts/doctor.sh` 是薄壳；
成绩单在 `crates/luyaz-core/src/scorecard*`），按「稳 / 好用 / 省」三类给出成绩单。

这份文件是长期层和成绩单之间的契约。改格式先改这里，并升 `v`。

## 1. 基本规矩

- **只有一个写的。** 往 `/data/luyaz/core/long/` 追加的只有核心的长期层。崩溃观察、每分钟的查看（`longterm/watch.rs`）、
  校准（`calib`）产生的事件都交给长期层写。
- **账本工作不拖累核心的本职。** 长期层是纯计算（`Fold`，快照进、行出），读写文件在 IO 一侧；写不进只记日志。
- **先落盘，后推进。** 「补记到哪了」（key.log 锚点）、「看过哪些 crashlog」（`crashlog.seen`）都等对应的行写进去并 fsync 之后才更新；
  中途被杀，下次同一事件编号相同，读的一方去重。
- **没覆盖就是没测。** 数据源没在工作的时段不算「0 次」，报告写「没测」并列出缺口。

## 2. 文件

持久（`/data/luyaz/core/long/`，`longterm::DIR`）：

| 路径 | 内容 |
|---|---|
| `hours.jsonl` | 每小时三行（`hour`、`hour_power`、`hour_proc`），留 90 天（按 `tt`）。 |
| `events.jsonl` | 重要事件，留 30 天、最多 1 MB。 |
| `open.json` | 当前没写完的那一小时的累计副本，每次检查重写：重启后上一次开机最后那不满一小时按它最后保存的时刻收尾，不随 tmpfs 丢掉。 |
| `keylog.anchor` | key.log 补记锚点（第 10 节）。 |
| `init-done` | 第一次上线的基准已经建好（没有它时不补历史，只设锚点）。 |
| `crashlog.seen` | 已入账的崩溃文件。 |
| `cutover` | 旧账本停写的时刻（epoch）：成绩单在这之前读旧账本，之后读 `long/`；没有这个文件（新装的设备）时成绩单只读 `long/`。 |
| `incidents.json` | regress 事故（「变差才通知」，第 15 节）：开着的和 8 天内结束的、告警队列读到哪、每个事故收了哪些告警、最近的演练（实机测试开关 `/tmp/luyaz-test-rx-quiet` 打开的时刻，最多 20 个）。事故开/关/又一次时写（fsync）。 |
| `baseline.json` | regress 基线：内存（heap、anon）和待机耗电（按在家/国外）的 7 天中位数与锚点，每天重算。 |
| `regress-limits.json` | 主人调过的绝对门槛（`<core> regress limit`），没有就是起始值。 |
| `incident-<id>.json` | 一个事故的证据：开始前 6 小时的小时摘要（内存、两个出口、待机耗电）、开始那次开机的原因码、可能相关的上机和演练，结束时再写一次（结束方式、恢复用时、收的告警）；≤ 8 KB，留 30 天、最多 50 份。8 天清单放掉之后 `/v2/incidents/<id>` 从它读回（`archived`）。 |
| `memsnap-<id>.txt` | 内存事故开始时的 smaps_rollup、线程数、堆计数，≤ 4 KB，最多 10 份。 |

`/data/luyaz/core/planned-reboot`：人为重启标记（第 4 节 `boot.planned`）。

只读的旧账本 `/data/ledger/`（`boot-*.jsonl` 分段等，不再追加）：成绩单读 `cutover` 之前的部分，经验库 `places.json` 开头从它回填一次，
doctor 默认输出末尾不再照抄它的 `summary`（10-24 起：那份停在 10-19，会误导）。`/data/ledger/state/wall.last` 仍由哨兵写（第 7 节）。

易失（`/tmp`，重新开机清空，核心重启后接着用）：
- `/tmp/luyaz/core/longterm.json`：同一次开机里两个核心进程之间交接的累计状态（含每分钟查看的状态：各程序 pid 和启动时间、`ver` 的 md5、屏幕日志读到第几行）。
- `/tmp/luyaz/ledger/`：`fp`（被测程序的 md5，第 12 节）、`fp-changed`。
- `/tmp/luyaz/clock-ok`：时钟结论，哨兵写，核心和 doctor 读（第 7 节）。
- `/tmp/luyaz/recovery-set`：哨兵记的 recovery 由我们设过。
- `/tmp/luyaz/handover`：交接文件，写着 `crashcap`、`standby`、`fp` 这些职责已归核心。
- `/tmp/luyaz-screen.log`：屏幕日志，哨兵写，`uid` 事件读它（改名后的 `.old` 接着数）。

内核日志落盘：`/data/crashcap/kmsg-<boot8>.log`，由核心的崩溃观察写（`crashwatch/kmsgfile.rs`），doctor 和成绩单读。

**保留**：`hours.jsonl` 90 天、`events.jsonl` 30 天且不超过 1 MB，追加时顺带剪。

## 3. 行格式

核心写的每行一个扁平 JSON 对象：

```
事件：{"t":<墙钟秒数或 null>,"raw_wall":<墙钟原值>,"clock_status":"<…>","up":<开机秒数>,"boot":"<boot8>","k":"<种类>"[,"id":"<事件编号>"]<,种类字段…>}
小时：{"k":"hour|hour_power|hour_proc","id":"<编号>","boot":"<boot8>","from":…,"to":…,"ft":…,"tt":…<,种类字段…>}
```

- `up`：CLOCK_BOOTTIME 秒数（含休眠）。
- `t`：时钟可信时写墙钟，否则 `null`。
- `id`：有稳定编号的事件才有，用来去重。编号规则见各种类。
- 缺值写 `null`，不写 0。运营商一律写 MCC-MNC 数字（如 `440-10`）。IMSI、ICCID、号码、MAC 一律不进账本。
- 读的一方：成绩单先把核心的行转成旧账本的行（`scorecard/core.rs`：`{"v":1,"seq":…,"n":…,"up":…,"t":…,"k":…}`）再算；
  解析不了的行跳过并计数；不认识的 `k` 跳过；同一 `id` 只算一次。

## 4. 事件种类（v1）

「fsync」标 ✔ 的是关键事件（核心每行追加都 fsync，这一栏只表示成绩单依赖它）。

| k | 谁写 | 什么时候 | 编号 `id` | 字段（按顺序） | fsync |
|---|---|---|---|---|---|
| `boot` | 长期层 | 每次开机一次 | — | `boot_id` 完整 boot_id；`code` 本次原因码（key.log 最后一个 `reboot_reason_code=`，没有写 null）；`mode` 开机模式；`fw` 固件版本；`net_select`（uci `zte_nwinfo` 里的制式偏好）；`rb_weekly`、`rb_cutoff`、`rb_connfail` 原厂三个自动重启的配置（`选项=值` 空格分隔，截短）；`pon` 开机原因（`ubus call zwrt_bsp.pm list` 的 `power_on_reason` 原值，读不到写 null）；`planned` 这次重启是有人要的（网页/触屏/定时经 `/control` 的重启、关机、恢复出厂，或 shell 里先跑 `<核心> plan-reboot <原因>` 再 `reboot`；标记 `/data/luyaz/core/planned-reboot` 记着写它的 boot，下一次开机的 boot 行带上原因再删；写了 10 分钟还在同一次开机里就作废）。CN B31 上 shell `reboot` 也记 1134（和基带崩溃同码），所以成绩单有 `planned` 就算人为，不算 S1/S2a，也不算 D12 的崩溃 | ✔ |
| `ver` | 长期层（每分钟查看） | 每次开机每个程序一行；之后程序 pid 变了且 md5 也变了再记 | — | `prog`（`luyaz-core`、`luyaz-ui`、`tailscaled`、`mihomo`；脚本 `doctor.sh`、`alert-lib.sh`）；`md5` 前 8 位；`how`：`exe` / `file`；`pid`；`extra`（核心写 `ubus=<后端>`）。另有核心自己的 `ver`（`prog=u60d`、`version`） | |
| `boot_backfill` | 长期层 | 写 `boot` 时 | `kl-<inode>-<行号>` | `code`；`at` key.log 里那行的时间文字；`started`（null）；`gap`（0/1） | ✔ |
| `guard_start` | —（只在旧账本里） | — | `gs-<boot8>-<up>` | `requested`（0/1）；`by`；`why` | |
| `recovery_seen` | 长期层（每分钟查看） | 每次开机第一次读到；之后被改回 disabled 再记 | — | `value`；`set_by_guard`（0/1，看 `/tmp/luyaz/recovery-set`） | |
| `clock` | 长期层 | 本次开机第一次判可信；之后可信被撤销或偏移变了再记 | — | `offset` = 墙钟 − 开机秒数（撤销时 null）；`how` | |
| `net_change` | 长期层 | `net_select`、服务网国家（MCC）和上一次记的不一样（第一次读到也记） | — | `net_select`；`net`；`home` | |
| `ssr` | 崩溃观察 | 一次基带崩溃（第 9 节） | `ssr-<boot8>-<kmsg 序号>` | `kseq`；`line` 断言原文；`rat`、`band`、`nrband`；`net` 服务网 MCC-MNC；`home` SIM 归属；`net_age` 网络缓存有多旧（秒）；`wan_ppm` | ✔ |
| `ssr_result` | 崩溃观察 | 这次崩溃跟随结束 | `res-<ssr 编号>` | `ssr`；`result`：`ok` / `no_drop` / `merged` / `timeout`；`recovered_s`；`rx_seen_s`；`down_seen`；`resumed`（0/1）；`gap_s`；`slept`（0/1） | ✔ |
| `ssr_dump` | 崩溃观察 | 崩溃后 90 秒内新出现的转储文件 | `dump-<boot8>-<文件名的 md5 前 8 位>` | `ssr`；`file`；`size` | ✔ |
| `link` | 崩溃观察 | 蜂窝口默认路由来了、没了 | `link-<boot8>-<计数>` | `state`；`cause`（`ssr` / `other`）；`res` 分辨率秒数（rtnetlink 订阅正常时 0） | |
| `sleep` | 崩溃观察 | 一段休眠结束（第 8 节） | `sleep-<boot8>-<计数>` | `from`、`to` 开机秒数；`ev`：`pm` / `stats` / `clock` | |
| `oom` | 崩溃观察 | kmsg 里的内存不足行 | `oom-<boot8>-<kmsg 序号>` | `line` | ✔ |
| `thermal` | 崩溃观察 | kmsg 里原厂过热等级变化 | `th-<boot8>-<kmsg 序号>` | `level`（如 `0x3`）；`line`。每次开机约 150 秒时原厂会设一次初始等级，读的一方只数比本次开机第一条更高的 | |
| `svc_exit` | 长期层（每分钟查看） | `/data/crashlog/<程序>/` 里出现新文件或文件变了 | `cl-<文件 md5 前 12 位>` | `prog`；`file`；`status`（文件第 2 行）；`found`：`round` / `boot_init`；`crash_up` 文件名里的开机秒数 | ✔ |
| `proc_restart` | 长期层（每分钟查看） | `luyaz-core` 的 pid 或进程启动时间变了（不在了先不记，回来时记） | `pr-<boot8>-<新 pid>-<启动时间>` | `prog`；`old`、`new` pid；`crashlog`（这一次查看看到过它的新 crashlog 没有，0/1）；`ship`（1 = 上机事务重启的：`/data/luyaz/ship/txn` 是本次开机、组件 `core`、还在跑或结束不到 10 分钟） | ✔ |
| `uid` | 长期层（每分钟查看） | 屏幕日志 `/tmp/luyaz-screen.log` 里的放弃（`giving up`）、交还（`starting the vendor UI`）、人为请求（`request:`、`corner long-press:`，记作 `other`） | `uid-<boot8>-<日志行号>`（本次开机累计的行号：日志改名成 `.old` 以后接着往下数） | `what`：`handback` / `gave_up` / `other`；`detail`（那一行原文） | ✔ |
| `core_degraded` | —（只在旧账本里） | — | 开始 `dds-<boot8>-<since>`，结束 `dde-<开始那次开机的 boot8>-<since>` | `state`：`start` / `end`；`since`；`reason`；`dur_s`；`how` | ✔ |
| `gap` | 长期层 | 某个数据源（第 8 节）连续没在工作超过 10 分钟 | — | `src`；`from`、`to` 开机秒数；核心重启跨了小时时另有 `no_hour=1` | |
| `calib` | 长期层 | `--calibrate-standby` 写了新基线 | `calib-<boot8>-<开机秒数>` | `rows` | ✔ |
| `hour` | 长期层 | 每个整点（对时前按开机满一小时） | `h-<boot8>-<from>` | `from`、`to` 开机秒数；`ft`、`tt` 墙钟（不可信写 null）；`awake`、`asleep` 秒；`net`（整点那一刻）、`home_mcc`、`abroad`（在国外的醒着秒数）；`batt_from`、`batt_to`、`batt_tmax`、`chg_s`；`link_down`；`stale_flips`；`g_core`、`g_watcher`、`g_capture`、`g_link`、`g_crashlog`、`g_uidlog`：各数据源这一小时没在工作的醒着秒数（第 8 节）；`free_mb`；`dump_mb`；`budget`（核心没有写入预算，总是 null） | |
| `hour_power` | 长期层 | 紧跟 `hour` | `hp-<boot8>-<from>` | `src`：`counter` / `vi`（读不到写 null）；`e` 总能量 mWh；`on_home_s`、`on_home_e`、`on_abroad_s`、`on_abroad_e`、`off_home_s`、`off_home_e`、`off_abroad_s`、`off_abroad_e`；`chg_s`；`batt_from`、`batt_to` %；`batt_tmax` 0.1 °C；`zone_tmax` m°C（只取主芯片侧的温度区：`cpuss-*`、`aoss-*`、`sys-therm-*`、`xo-therm`、`pm*_tz`、`battery`；基带侧的 `sdr*`、`mmw*`、`epm*`、`mdm*` 读一次就是一个发给基带的 QMI 请求，不读）；`throttle_s`（没有 cpufreq 写 null）；`e_partial`（能量缺了一部分：V×I 下有过休眠，或有读不到的检查）；manager `docs/designs/u60-radio.md` §4、D9/D10：`off_home_quiet_pm`、`off_abroad_quiet_pm` 息屏不充电时蜂窝安静的千分比（连续 0 包 ≥10 s 的整段，跟诊断档 quiet10 同定义，休眠不算）、`off_home_wan_pkts`、`off_abroad_wan_pkts` 这段时间的蜂窝包数（厂商计数，含 IPA 转发；没比到写 null）；`off_home_cli_s` 息屏在家时有客户端的秒数（客户端数未知也算有）；`chill_profile` 这小时 CHILL 的实际档位（`eco`/`standard`/`perf`，CHILL 关着或暂停写 `off`，换过写 `mixed`，没有 CHILL 写 null） | |
| `hour_proc` | 长期层 | 紧跟 `hour_power` | `hq-<boot8>-<from>` | `rss_agent`、`rss_core`、`rss_ui`、`rss_mihomo`、`rss_ts` kB（这一小时最后一次检查时，不在跑写 null）；`cpu_core` 本小时平均千分比（同一个进程从头跑到尾才算，否则 null）；`idle_rows`、`idle_wan`（null）；`activity`；`chill_on_s`、`chill_ok_s`、`ts_on_s`、`ts_chk_s`、`ts_ok_s`（第 6 节「出口」） | |

以后的种类：`reboot_request`（`by`、`why`，我们的每条重启路径发出前写，要挺过重启）、`session`（设备占用锁）、`deploy`（E1）。

## 5. 暂存协议（spool）

核心不用暂存文件：崩溃观察的事件在内存里排队（`Watch::pending`），写进 `events.jsonl` 才出队；编号稳定，被杀后重写的是同一行，
读的一方按 `id` 去重。旧账本的 `/data/ledger/spool/`、`/tmp/ledger-spool/` 不再读也不再写。

## 6. 后台任务

- 长期层随核心常驻：区块（`signal`、`battery`、`interfaces`、`sim`、`clients`）每次发布都喂给它；每 60 秒（`PROBE_S`）一次检查：
  探一次崩溃观察、记覆盖、做一次每分钟查看（`longterm/watch.rs`）、累计 `hour_power`/`hour_proc`（`longterm/power.rs`），并保存状态。
- **网络缓存**：取自核心自己的 `signal`、`sim` 区块：`rat`、`band`、`nrband`、服务网 MCC-MNC（按 MCC 补成 3 位或 2 位 MNC）、
  SIM 归属 = IMSI 的前缀（IMSI 本身不留）。
- **每小时摘要**：换小时（时钟可信后按墙钟整点，之前按开机满一小时；可信与否变了也算换）就写 `hour`、`hour_power`、`hour_proc` 三行，从零再来。
  三行按起点编号，重跑不重记。读不到的值一律 null，不写 0。累计每次检查另存 `open.json`：重启时那不满一小时下次开机按上一次开机写出来。
  跨整点的一段算进它结束的那一小时。
  - **能量**：有电量计的累计量（`battery/charge_counter`，µAh）时，按相邻两次的差 × 电压算，跨过休眠也对；
    没有时用 V×I 只积醒着的那一段，本小时有过休眠就标 `e_partial`。充电的时间进 `chg_s`，不进格子。
  - **降频**：某个 cpufreq policy 的 `scaling_max_freq` 低于它本次开机见过的最高值，算降频（不和 `cpuinfo_max_freq` 比：原厂可能一直压着）。
  - **出口（S8）CHILL**：算「开着」= 蜂窝口有默认路由、`/data/chill/` 下没有 `disabled`、不在 `pause_until` 之内；「正常」= 另外 `chill.sh` 的
    `/tmp/chill.state` 150 秒内写过、状态是 `running`、它记的 `core_pid` 还是 mihomo。
  - **出口（S8）Tailscale**：算「开着」= 蜂窝口有默认路由且 `rc.local` 会起它（`/data/tailscale/start.sh`）。开着的每次检查，问一次 tailscaled 的 LocalAPI
    （`/tmp/tailscaled.sock` 上手写 HTTP 请求 `/localapi/v0/status`，不起 `tailscale` 进程），最多 2 秒。「正常」= `BackendState` 是 `Running` 并且 `Self.Online` 为真。结果分三种：
    - 正常；
    - 不正常：socket 不在、连不上、超时、5xx，或者不是 `Running`、不在线；
    - 没查到：4xx（是请求的问题，不是 tailscaled 的）、回答里没有 `BackendState`。

    上一次到这一次醒着的秒数记进 `ts_on_s`；查到了再记进 `ts_chk_s`，正常再记进 `ts_ok_s`。没查到的秒数只算没覆盖（第 8 节），不算不正常；
    也不拿「进程在」冒充正常。

## 7. 时钟

时钟判定由哨兵和核心共用 `luyaz-alerts::clock::ClockEvidence`；年份等只算 estimated，不证明已同步或是 UTC。

- 哨兵写 `/tmp/luyaz/clock-ok`：第一行 `<0|1> <offset> <写入时的开机秒数> <how>`，后面附 boot 绑定的 JSON；可信时更新 `/data/ledger/state/wall.last`。
- 读的一方（核心长期层、短信、doctor）检查 ≤180 秒新鲜度、同一次开机、5 秒连续性；大幅跳变撤销可信，跨哨兵重启保留。
  doctor 不用年份回退宣称「已对时」。具体范围见 `../../docs/designs/u60-mcp.md`「P0-01 第二批」。
- 长期层的 `t`、`ft`、`tt` 只在可信时写墙钟；`clock` 事件记下每次判可信、撤销。
- 读的一方：`t` 为 null 的行，用同一次开机最近一条有效 `clock` 的偏移补算 `t = up + offset`；这次开机没有有效 `clock` 就一直 null。

## 8. 覆盖和休眠

- **各数据源的存活判据**（每 60 秒检查一次，按 CLOCK_MONOTONIC 计，休眠既不算覆盖也不算缺口）：

  | src | 算在工作的条件 |
  |---|---|
  | `core` | 核心在跑：同一次开机里核心重启，离开的秒数记给所有数据源；跨了小时的，旧小时在最后保存处收尾，另写一条 `gap src=core no_hour=1` |
  | `watcher`、`capture`、`link` | 崩溃观察的三条路（kmsg、转储目录的 inotify、rtnetlink）：探测没被应答 = 三条都从它最后一次醒来起算缺；丢了记录（EPIPE、ENOBUFS、IN_Q_OVERFLOW）= 那一条从最后一次干净的读取起算缺 |
  | `uidlog` | `/tmp/luyaz-screen.log` 最后一行能解析（以时间开头）；它刚被改名或是空的时看 `.old` 的最后一行 |
  | `crashlog` | crashlog 目录在、能读 |

  没在工作的醒着秒数记进 `hour` 行对应的 `g_*`（每个不超过这一小时的醒着秒数）；一段连续超过 10 分钟的另写一条 `gap`。
  **没有 `hour` 行的醒着时间一律算没覆盖**。

- **休眠**：两次醒来之间 CLOCK_BOOTTIME 比 CLOCK_MONOTONIC 多走了 > 10 秒（只有挂起会这样）就是一段休眠。证据：内核挂起、唤醒行
  （`PM: suspend`、`PM: resume` 开头）写 `ev=pm`，`/sys/power/suspend_stats/success` 增加了写 `ev=stats`；6 秒内两样都没有写 `ev=clock`
  （两个时钟本身就是证据，照样算休眠）。`from` 是 `to` 减去挂起的时长。
- **不算缺口**：休眠；关机充电的开机；开机头约 45 秒（只归 S2a 的空档一栏）。
- **窗口的起点**：账本上线（第一条 `boot`）之前的时间，一律算覆盖不完整。
- **每项指标依赖哪些数据源**见第 11 节的表；缺口按这些数据源合并（取并集）。
- **完整**：窗口里醒着的时间，合并后的缺口合计 ≤ 5%，并且没有一段超过 10 分钟。否则该项写「没测」并列出缺口。
- **Tailscale 自己的覆盖**（S8 的 Tailscale 一半）：Tailscale 开着的时间里，没查到的（`ts_on_s` − `ts_chk_s`）合计 ≤ 5%，
  并且没有哪一小时里超过 10 分钟（只有每小时的合计，所以按小时判，跨整点的一段不合并）。

## 9. 崩溃观察循环

- **在哪**：核心里的崩溃观察（`crashwatch.rs`，IO 在 `crashwatch/io.rs`），靠事件唤醒，不轮询：
  `/dev/kmsg` 有新记录时内核唤醒它（一阵记录最多每 2 秒读一次）；rtnetlink 路由、地址消息在链路可能变了时唤醒它；
  只有跟随崩溃、转储窗口开着、或停顿在等证据时才每 2 秒定时一次。状态可序列化，核心重启后接着跟（`resumed=1`、`gap_s`）。
- **落盘**：拿到交接（`/tmp/luyaz/handover` 连续两次写着 `crashcap`）后，核心把读到的内核记录写进 `/data/crashcap/kmsg-<boot8>.log`
  （`crashwatch/kmsgfile.rs`，格式同 `/dev/kmsg`：`<优先级>,<序号>,<时间戳 µs>,<标志>[,caller=…];<正文>`）。
- **认崩溃**（不区分大小写）：`fatal error received`、`watchdog received`、`crash detected`、`rproc recovery`、`recovering …remoteproc`、
  `subsystem restart`、`err_fatal`。设备上一次崩溃依次是：
  `qcom_q6v5_pas 4080000.remoteproc-mss: fatal error received: <断言>`、`… rproc recovery state: enabled and kick reovery process`
  （恢复关着时是 `disabled and lead to device crash`）、`remoteproc remoteproc0: crash detected in …: type fatal error`、
  `remoteproc remoteproc0: recovering …`。
  - 第一行的内核秒数（时间戳）起 60 秒内的崩溃行，都算同一次崩溃，不另起。
  - 超过 60 秒、还没恢复：当前这次结果 `merged`，另开一次新的，从新崩溃重新计时。
  - 已经恢复、还在等流量，或者已判 `no_drop` 还在等流量：当前这次按已有的结果结束（`ok` / `no_drop`，没等到的 `rx_seen_s` 写 null），另开一次新的。
  - 打开时立刻出 `ssr`：`line` 是 `fatal error received: ` 之后的断言（没有就是整行），带崩溃前最后已知的网络信息（写明有多旧）
    和 `wan_ppm`（最近一对相隔至少一分钟的 `/proc/net/dev` 读数算出的每分钟包数；安静时这一对可能隔得更久）。
- **链路**：up = main 表的默认路由走蜂窝口。每次醒来都看；默认路由来了、没了各记一条 `link`，开着崩溃时 `cause=ssr`。
- **跟随**（打开期间每 2 秒采样一次：地址、默认路由、rx_bytes）：
  - 先看到 down 再看到 up 才算恢复，`recovered_s` 从发现崩溃算起；恢复后 60 秒内等 rx_bytes 第一次增长，`rx_seen_s` 也从发现崩溃算起，
    没等到写 null（空闲时正常，不算没恢复）；
  - 30 秒内一直没看到 down：`no_drop`，`rx_seen_s` 是发现崩溃以来 rx_bytes 第一次增长；30 秒时还没有就再等到 90 秒；
  - 看到 down、10 分钟没恢复：`timeout`（P95 里按 ≥ 600 秒）；
  - 跟随期间有第 8 节的休眠：`slept=1`；
  - 相邻两次采样隔了 10 秒以上：多出来的秒数记进 `gap_s`；核心重启时有没结果的崩溃：接着跟，`resumed=1`。结果仍是上面四种之一。
- **转储**：`/data/vendor/ramdump/`（设备上是 `ipa_driver_<时刻>.elf`）。窗口外转储目录有变化（inotify）时重新记基准，没有 inotify 时
  醒来距上次列目录满 60 秒再记。崩溃打开一个 90 秒的窗口（下一次崩溃会提前结束上一个窗口），基准里没有的新文件，相邻两次大小不变
  （或窗口到期）时写一条 `ssr_dump`，每个文件只写一次。恢复了也看满 90 秒。
- 顺带认：`Out of memory: Kill…`（含 memcg）→ `oom`；`zte_thermal_update_throttling_level() to 0x…` → `thermal`；内核挂起行（第 8 节）。

## 10. 开机初始化和补记

每一步都可重入，并且「先写账本、后推进检查点」：

1. **开机事件**：本次开机还没有 `boot` 行才写 `boot`（前面先写 `boot_backfill`）；`ver` 由每分钟查看在本次开机第一次查看时写。
2. **key.log 补记**：`/data/logfs/key.log`，锚点 `long/keylog.anchor` 是上次入账的原因码行。从锚点之后到本次开机那一行之前，
   每个 `reboot_reason_code=` 各记一条 `boot_backfill`（编号按所在文件的 inode 和行号，重复执行不会重记）。写好之后才推进锚点。
   找不到锚点：只补能看到的、标 `gap=1`。`init-done` 不存在（第一次上线）：不补历史，只设锚点。
3. **crashlog**：`long/crashlog.seen` 记已入账的文件。第一次上线把现有文件全记作基准、不计数。之后每次查看扫 `/data/crashlog/<程序>/`，
   名字、大小、md5 任一对不上就是新文件，写 `svc_exit`（编号按文件内容的 md5），写好之后才更新 `crashlog.seen`。
   触屏崩溃以 `/data/crashlog/luyaz-ui/` 为准，屏幕日志只补交还、放弃这类 crashlog 里没有的。
- **人为重启**：见第 4 节 `boot.planned`。

## 11. 读的一方（doctor --report）

- 成绩单读两份：`cutover` 之前读旧账本 `/data/ledger/` 的分段，之后读 `long/` 并转成同样的行；按行里的开机归属；跳过解析不了的行并计数；同一 `id` 只算一次。
- **时间**：每次开机的「墙钟 − 开机秒数」取它自己可信的行（`t` 或 `hour` 的 `tt`）；`t` 为 null 的行用它补算，这次开机一行可信的都没有就算时间不明、
  不进任何窗口。`hour_power`、`hour_proc` 按编号里的起点挂到对应的 `hour`。
- **报告截至**最后一条 `hour` 的结束时刻（当前没满的那一小时不算缺口）；窗口起点也不早于账本第一条 `hour`。
- **在家 / 国外**：崩溃按 `ssr` 自己的服务网和 SIM 归属；重启按上一次开机最后一小时 `abroad` 过半与否。
- **开机之间的空洞**：上一次开机最后一小时结束到下一次开机第一小时开始超过 5 分钟，并且下一次开机不是人按的（1150、1152）、也不是充电开机，
  这段算没覆盖（设备开着但没人记）。
- **崩溃按发生顺序**排。
- **原因码分类**：1150、1152 人按的（不算意外）；1112 原厂定时（单列）；1155 断网保护、1185 屏幕没界面、1134、1132 基带（算意外）；
  1182 要判别：上一次开机的最后一条 `ssr` 没有 `ssr_result`，或者崩溃观察没来得及看到、而上一次开机的落盘文件以崩溃行结尾、后面再没有别的记录
  （看 `/data/crashcap/kmsg-<boot8>.log` 最后一条记录行）→ 基带；上一次开机账本最后有我们的 `reboot_request` → 我们发起的；
  上一次开机的落盘文件已经没了、或以上都不是 →「来源不明」（算意外）。`boot.planned` 的算人为。
- **窗口**：每项 = 「要求的窗口 W」和「这项最近一次相关改动之后」取短的；改动前的违反另列一行。改动：自恢复第一次上线
  （2026-09-26 21:43，固定记录）→ S1、S2a；某程序 `ver` 的 md5 变了 → 它的 S4、P4、P5；哨兵接管屏幕
  （2026-09-23，固定记录）→ S5a、S5b；`net_change` → S2b、S3、P2；`calib` → P3。
- **档位**：没测（覆盖不完整，或没有数据源）/ 不达标（窗口内有违反，含已知事实）/ 注意（覆盖完整、没违反、窗口比要求的短，写出已有多长）/ 达标。
- **核心降级**（`core_degraded`）：只在旧账本里有；一次降级按 `since` 区分，时长只用同一次开机里的开机秒数算。
- **恢复用时 P95**：`timeout` 按 ≥ 600 秒；`merged` 按「到下一次崩溃仍未恢复」，一串连着的算一次长断网，时长取到最后恢复为止；
  `no_drop` 用 `rx_seen_s`，单列；`resumed=1` 时，缺口里可能已恢复就按缺口结束时刻算；`slept=1` 的样本单列，不能用来判「达标」。
- 每项都分在家、国外报（服务网和 SIM 归属的 MCC 是否相同）；S2b、P1、P2、P3 另分空闲和有流量（`wan_ppm` 或当小时的空闲判定）。
- 告警直接读 `/data/alerts/queue` 和 `sms-log`，排除 `sms-test`；队列最老一条比窗口起点晚，U6 标覆盖不完整。

**指标计算表**（W 是要求的窗口；「源」是第 8 节的数据源，覆盖不完整就是「没测」）：

| 项 | 怎么算 | 目标 | W | 源 |
|---|---|---|---|---|
| S1 | 窗口内算意外的重启次数（`boot.code` 和 `boot_backfill.code` 按上面的分类） | 0 | 7 天 | key.log 补记（锚点丢了 `gap=1` 算不完整） |
| S2a | 基带崩溃后整机重启的次数：原因码归为基带、且上一次开机账本在记；上一次开机账本没起来的单列成「开机空档」 | 0 | 7 天 | 同 S1，外加 capture、watcher |
| S2b | `ssr_result` 的恢复用时 P95（上面的规则），分在家、国外，分空闲、有流量 | ≤ 45 秒 | 7 天 | watcher、capture |
| S3 | `ssr` 次数，按服务网国家、`rat`、`band` 分 | 在家 0；国外列出并写对策（人工） | 7 天 | watcher、capture |
| S4 | 每个程序：`svc_exit`（非 0 退出）+ 没有 crashlog 的 `proc_restart`；`ship=1` 的（上机事务自己重启的）不算，手工重启仍算 | 0 | 7 天 | crashlog、core |
| S5a | 原因码 1185 的次数 | 0 | 7 天 | 同 S1 |
| S5b | `uid what=gave_up` 次数（再对照 `/data/luyaz/screen/gave-up`、告警 `ui-gave-up`） | 0 | 7 天 | uidlog |
| S6 | `core_degraded` 次数和总分钟数 | 每天 0 | 1 天 | core |
| S7 | E1 前靠已知事实（版本漂移） | — | — | — |
| S8 | CHILL：`chill_ok_s` ÷ `chill_on_s`；Tailscale：`ts_ok_s` ÷ `ts_chk_s`，覆盖另按第 8 节「Tailscale 自己的覆盖」；把没查到的全算正常也不到 99% 就直接不达标。两个都没开过写没测 | ≥ 99% | 7 天 | core |
| U1–U5 | 第二步前没测，或靠已知事实（写明） | — | — | — |
| U6 | 列出窗口内全部告警（排除 sms-test）和当前体检里的 ▲，人工判 | — | 7 天 | 告警队列 |
| P1 | `hour_power.e` 折成平均功率和按当前电量的续航；先不定目标，攒够 7 天 | 待定 | 7 天 | core |
| P2 | 四格（亮屏/息屏 × 在家/国外）的平均功率，剔除充电分钟；每格样本少于 6 小时写「样本不足」 | 待定 | 7 天 | core |
| P3 | 当前待机判定是否只在空闲时做、有没有过期的列 | 规则成立 | — | 待机记录 |
| P4a | `hour_proc.cpu_core` 的平均 | ≤ 2% | 1 天 | core |
| P4b | 核心 `ver.extra` 是否 `ubus=socket` 或 `ubus=auto`（socket，连不上 ubusd 时临时退回 CLI） | socket / auto | — | — |
| P5 | 连续运行 ≥ 24 小时的开机：以开机满 1 小时后第一条 `hour_proc` 为基准，RSS 增长 ≤ 10%；`oom` 为 0 | ≤ 10%、0 | 7 天 | core、watcher |
| P6 | `hour_power.throttle_s` 合计（null 的小时算没测）、`thermal` 里比本次开机第一条更高的等级出现的次数 | 0 | 7 天 | core、watcher |

- 输出：每项一行，写数值、档位、已有窗口 / 要求窗口、覆盖、判定方式（自动 / 人工 / 靠已知事实 / 第 X 步前没测），细项另起一行缩进。
  `--report 24h` 把所有窗口压到 24 小时（7 天的项因此最多「注意」）。S7、U1–U5 在对应步骤之前按已知事实固定写不达标；P1、P2 目标待定，
  只给数值；P2 的小时记录不分空闲和有流量。P5 没有内存记录时写「没有内存记录」（没测），不说「都在 10% 以内」。

## 12. doctor --tsv 的约定

- `--tsv` / `--tsv2` 的结果上触屏和网页的健康行（`/api/health`）。`--tsv` **不加行、不读账本**，行的 id 和顺序不变。
- 只有两行为修错而变：
  - `standby`：只判空闲的息屏分钟（蜂窝包数 ≤ 基线中位数 + 3×MAD），有流量写「有流量，不判」；指纹对不上的列写「基线过期」、不报 ▲。
  - `clock`：按第 7 节的判据；开机 5 分钟内还没对时写「刚开机，还在对时」算 ok，之后还没对时才 warn。
- **待机指纹**：
  - 核心（交接文件写着 `fp` 时）在开机和被测程序 pid 变化时算好程序的 md5，写 `/tmp/luyaz/ledger/fp`（每行 `<程序> <pid> <md5>`），并在
    `/tmp/luyaz/ledger/fp-changed` 记下最近一次变化时的开机秒数。
  - 基线文件（`/data/luyaz/scripts/standby.baseline`，第 2 版）：`<列> <中位数> <MAD>` 行之外，有 `fp <列> <依赖的 md5 串>` 行和第一行 `v2`。
    每列的依赖：2 蜂窝包（无）；3 隧道包（tailscaled、`tuning.env`）；4 tailscaled 唤醒（tailscaled、`tuning.env`）；5 mihomo 唤醒（mihomo、CHILL 模板、`chill.env`）；
    6 触屏唤醒（luyaz-ui）；7 核心唤醒（luyaz-core）。配置文件的 md5 每次现算（都很小），程序的 md5 读 `fp`。
  - 某列依赖的 md5 对不上：那一列写「基线过期」、不判。基线没有 `v2`：整行写「基线格式旧，请在家自然空闲时重新校准」，不判。
  - 只用 `fp-changed` 之后的待机记录来判。
- 默认（给人看的）输出末尾照抄旧账本的 `summary`；`summary` 不存在就不写。`--tsv` 不读账本也不读 `summary`。

## 13. selftest

`doctor --ledger-selftest`（`crates/luyaz-core/src/doctor/selftest.rs`）：不碰正式账本和 recovery。逐项写 PASS/FAIL：
目录可写（写 `/data/ledger/.selftest` 一行 512 字节、fsync、读回、删除）；key.log 能解析出最后一个原因码；能读 recovery；
往 `/dev/kmsg` 写一行带随机数的 `u60-ledger-selftest` 测试行，6 秒内在落盘文件（`/data/crashcap/`）里出现。
测试行不计入任何指标。另外按原样用法试一遍设备工具，哪个工具设备上没有或行为不同，在这里先暴露。全部 PASS 才退出 0。

## 14. 公开导出

CHILL（mihomo）相关的字段随 CHILL 一起公开；没装 CHILL 的设备上这些字段不写，读的一方按缺字段处理。

## 15. regress 事故（`longterm/regress/`）

设计：本机设计稿（「变差才通知」）及其实作用整合规格。长期层每分钟检查时顺带判断，不加读取、不加唤醒：

- **区间型**（`regress-mem`、`regress-mem-abs`、`regress-cellular`、`regress-egress`、`regress-power`、`regress-power-abs`；哨兵的 `regress-recorder`、`regress-core-down`；`wifi-restore-failed`）：开一则、恢复一则（`end: recovered`，只进收件匣）；整机重启时没关的记 `end: reboot`。
- **点事件型**（`core-crash`、`ui-crash`、`web-crash`、`sentinel-crash`、`ui-gave-up`、`sentinel-silent`，窗口 1 小时；`regress-reboot`，窗口 6 小时）：同类在窗口内再来就并进同一事故（`count`），窗口过了没再来就 `end: quiet`（不是「已恢复」）。
- 每次转换是一则告警，id `rg-<事故 id>-<open|esc<n>|close>`：先把事故连同「待入队」写进 `incidents.json`，再 `add_once`，再去掉待入队标记，所以中途重启只会补入队一次。
- 告警队列每次检查读回（`ingested_seq`）：转换告警和点事件告警记在事故的 `alert_seqs`（最多 50，超过只计数），`/api/alerts` 每则带 `incident`；队列被剪掉还没读的部分记 `gap_at`（`/v2/incidents` 的 `ingest_gap`），不猜内容。第一次运行把读到哪设成当时的队列尾、当前开机记为已判断（旧告警不变成事故，装上新核心那次开机不报重启）。
- 基线：`hour_proc` 里整小时同一个核心进程的 heap_live / anon_core，`hour_power` 里合格小时（整小时熄屏、没充电、该格蜂窝包 ≤ 15000、计量覆盖 ≥ 95%）的待机 mW；排除事故区间；第一次的值做锚点，之后不超过锚点 ×1.25。固件或 SIM 归属网络变了，耗电基线清空。
- 读的一方：`/v2/incidents`（开着的 + 7 天内结束的，每个带 `unread`）、`/v2/incidents/<id>`（加 `alerts[]`、证据小时、内存快照）、`/v2/state` 的 `regress` 块（开着 ≤ 5 + 24 小时内结束的 ≤ 3；跨开机只按可信墙钟算 24 小时）。
