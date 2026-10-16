# 设备账本（ledger）：格式与约定

**现状（2026-10-15）**：u60-guard 已退役（10c 删）。下文「u60-guard / guard」的账本任务、待机记录现在由核心 luyaz-core（manager `crates/luyaz-core/src/longterm/`）承担，读的一方是 `<核心> doctor`（`/data/u60-guard/doctor.sh` 是薄壳）；u60-uid 的屏幕事由哨兵 luyaz-sentinel 承担（日志 10-16 E2c 起叫 `/tmp/luyaz-screen.log`，哨兵启动时把旧名改过来）；迁移后的设备上 `/tmp/u60-guard/…` 是 `/tmp/luyaz/…`。账本仍在 `/data/ledger/`；下文保留当时的写法作记录，按这里换主语读。

u60-guard 在设备上记一本跨重启的事件账：整机重启、基带崩溃和恢复、我们自己的程序退出、datad 降级、
每小时的耗电和资源。`doctor.sh --report 24h|7d` 读它，按「稳 / 好用 / 省」三类给出成绩单。

这份文件是写的一方（`scripts/u60-guard.sh`）和读的一方（`scripts/doctor.sh`）之间的契约。
两边的实现、测试都以这里为准；改格式先改这里，并升 `v`。

## 1. 基本规矩

- **只有一个写的。** 往账本追加的只有 guard 的账本任务，并且同一时间只有一个：任务一开始就对
  `/tmp/u60-guard/ledger/writer.lock` 拿 `flock -n`，拿不到立刻退出（另一个还在写）。其他部分（崩溃观察循环、
  doctor 校准，以后的 agent、触屏、网页）每个事件写一个暂存文件，账本任务收进来（第 5 节）。
- **账本工作绝不拖累 guard 的本职。** 账本任务在后台子 shell 里跑，guard 主循环不等它；上一个任务还在，
  这一轮就不起新的，记一段覆盖缺口。子 shell 里出了致命错误（比如算术碰到坏数字），只死子 shell。
- **观察循环不碰 kmsg 落盘管道。** 崩溃由单独的观察循环读落盘文件发现（第 9 节）；落盘读取进程退出后由 guard 主循环有限次地重起（第 2 节 `crashcap-starts`）。
- **先落盘，后推进。** 任何「读到哪了」「补记到哪了」「暂存删不删」，都等对应的账本行完整写进去并 fsync 之后才推进；
  中途被杀，下次靠事件编号去重（第 5 节）。
- **没覆盖就是没测。** 数据源没在工作的时段不算「0 次」，报告写「没测」并列出缺口。

## 2. 文件

持久（`/data/ledger/`，入口 `GUARD_LEDGER_DIR` / `DOC_LEDGER_DIR`）：

| 路径 | 内容 |
|---|---|
| `boot-<seq>-<boot>-<part>.jsonl` | 账本分段。`<seq>` 6 位开机序号，`<boot>` boot_id 前 8 位，`<part>` 3 位段号。每段 ≤ 256 KB，写满换下一段。按文件名排序就是时间顺序；不看 mtime（对时前结束的开机 mtime 不可信）。 |
| `seq` | 最后分配出去的开机序号。 |
| `bootmap` | 每次开机一行 `<seq> <完整 boot_id> <up>`（账本初始化时追加并 fsync）。 |
| `state/init-done` | 第一次上线的基准已经建好（没有它时按第一次上线处理：不补历史、crashlog 全记作基准）。 |
| `state/keylog.anchor` | key.log 补记锚点：`<inode> <行号> <md5>`（第 10 节）。 |
| `state/crashlog.seen` | 已入账的崩溃文件：每行 `<目录>/<文件名> <大小> <md5>`。 |
| `state/started.log` | guard 每次启动追加一行 `<完整 boot_id> <up>`（第一轮之前写），只留最后 50 行。 |
| `state/wall.last` | 上次可信的墙钟秒数，可信时每小时更新（第 7 节）。 |
| `state/watch-init` | 观察循环第一次上线已经把当时的落盘内容当作「已看过」（第 9 节）。 |
| `state/datad.open` | 开着的 datad 降级：`<since> <开始那次开机的 boot8>`；重启后由下次开机补 `end how=reboot`（第 11 节）。 |
| `state/net.last` | 上次 `net_change` 时的 `<net_select> <服务网 MCC>`，跨开机比较用。 |
| `state/acc.last` | 本小时累计的副本（每轮写，不 fsync），第一行 `boot <boot8> <seq>`。重启后下次开机用它把上一次开机最后那不满一小时写出来（`cut:1`）。 |
| `spool/` | 要挺过重启的暂存事件（第 5 节）；`spool/bad/` 放格式不对的，最多 20 个。 |
| `summary` | 3 行小汇总：每写完一小时，主循环另起一个后台任务（不拿写锁）跑 `doctor.sh --report 7d --summary` 重写它；doctor 默认输出末尾照抄。 |
| `.selftest` | selftest 的临时文件，测完删。 |

易失（`/tmp`，重新开机清空，guard 重启后接着用）：
- `/tmp/u60-guard/ledger/`：`seq`（本次开机的序号）、`n`（行号）、`seg`（当前段）、`writer.lock`、`job.pid`、`boot.done`、
  `net`（网络缓存，账本任务写：一行 `<开机秒数> <rat> <band> <nrband> <服务网 MCC-MNC> <SIM 归属 MCC-MNC>`，不知道的写 `-`）、
  `acc.*`（每小时累计）、`cov`（覆盖记录）、`procs`（datad/agent/u60-uid 的 pid 和启动时间）、`fp`（被测程序的 md5 缓存，第 12 节）、
  `budget`（当天已写字节）、`cl.new`（这一轮有新 crashlog 的程序）、`uid.pos`（`<当前 u60-uid 日志读到第几行> <之前的日志一共几行>`；日志读完且超过 64 KB 就改名成 `.old`，从新文件接着数）、
  `datad.ep`（开着的降级 `<since> <开始的开机秒数>`）、`datad.seen`（本次开机开始过的 since）、`clock.last`（上次记的时钟结论）、
  `datad.read`（最近一次读到 datad `/state` 的开机秒数，第 8 节的 `datad` 数据源）、`rounds`（主循环每轮一行，第 6 节）、
  `acc`（本小时累计，第 6 节）、`cov`（没在工作的数据源和开始时刻）、`cpumax.<policy>`（本次开机见过的最高 `scaling_max_freq`）、
  `ledger.slow`（跑超 10 秒已经记过日志的任务 pid），以及观察循环的（第 9 节）：
  - `watcher.pid`：`<pid> <启动时间> <u60-guard.sh 的 md5 前 8 位>`；`watcher.lock`：保证只有一个观察循环。
  - `w.pos`：`<已读到的字节位置> <最后处理的 kmsg 序号> <已见过的截短计数>`；`w.up`：最近一次读完的开机秒数（观察循环的心跳）。
  - `w.ssr`：打开着的崩溃（编号、发现时刻、第一行的内核秒数、down/up 时刻、rx、最近采样、缺口秒数、resumed、slept、no_drop 已定）；
    `w.ssrlast`：最近打开的崩溃的 kmsg 序号和内核秒数（重读时不重开）。
  - `w.dump`：转储窗口 `<ssr 编号> <结束的开机秒数>`；`w.dbase`、`w.dsz`、`w.drep`：基准文件名、见过的大小、已报的。
  - `w.link`：`<默认路由 0|1> <本次开机见过默认路由 0|1>`；`w.pk`：`<开机秒数> <包数> <每分钟包数>`；`w.cnt`：`<link 计数> <sleep 计数>`。
  - `w.slept`：有证据的休眠 `<from> <to> <pm|stats>`，每小时摘要的 `asleep` 只数这些（C14）。
- `/tmp/u60-guard/crashcap-cuts`：guard 原地截短落盘文件的次数（第 9 节）。
- `/tmp/u60-guard/crashcap-starts`、`crashcap-started`：这次开机 kmsg 落盘读取进程启动过几次、最近一次在开机第几秒。读取进程退出（如 `cat /dev/kmsg` 读得太慢被内核覆盖、返回 EPIPE）后，guard 主循环过 300 秒再起一次，一次开机最多再起 5 次（`crashcap_keep`）；新起的只追加文件里还没有的记录（按 kmsg 序号），续行跟着它的记录走。
- `/tmp/u60-guard/clock-ok`：时钟结论，guard 和 doctor 共用（第 7 节）。
- `/tmp/ledger-spool/`：丢了无妨的暂存事件。

**保留**：总量（段 + spool + state）≤ 16 MB、≤ 30 天，从最老的开机整段删；更早的开机删完还超（一次开机连着跑了几周），就从本次开机最老的段删起；绝不改写或删除本次开机正在写的段（也不删本次开机编号最大的段）。每小时换小时时查一次；
「30 天」按那次开机最后一行的墙钟 `t` 算（null 的只在超量时删）。
**预算**：每天写入 ≤ 1 MB（只算账本段；`state/acc.last` 每轮重写一次、约 1–2 KB、一天约 1440 次，是状态不是账本行，不计入）。超了以后只写关键事件（第 4 节标 fsync 的）和每小时的 `hour*` 三行（`hour` 里 `budget:1`），其余丢弃。
**空间**：`/data` 剩余不到 100 MB：账本只写 `boot` 行；生产方改写 `/tmp/ledger-spool/`（事件里带 `lowspace:1`）；`/data/ledger/spool/`
里超过 200 个文件时生产方同样改写 `/tmp`。doctor 把这两种情况标出来。

## 3. 行格式

每行一个扁平 JSON 对象，键序固定，一行 ≤ 512 字节：

```
{"v":1,"seq":<开机序号>,"n":<行号>,"up":<开机秒数>,"t":<墙钟秒数或 null>,"k":"<种类>"[,"id":"<事件编号>"]<,种类字段…>}
```

- `seq`：事件所属的那次开机。上一次开机没来得及收的暂存事件会写进本次开机的段里，`seq` 仍是上一次的；读的一方按行里的 `seq` 归属，不按文件。
- `n`：写入顺序，本次开机内严格递增（跨段连续）。
- `up`：事件发生时的 `/proc/uptime`，一位小数。暂存事件用生产方记下的时刻。
- `t`：写入时时钟可信、并且事件属于本次开机，就写 `up + offset`（整数），否则 `null`（上一次开机的暂存事件、补写的小时都是 null）；
  读的一方用那次开机自己的 `clock` 事件补算（第 7 节）。
- `id`：有稳定编号的事件才有（暂存事件、补记、crashlog），用来去重。编号规则见各种类。
- 字段值只有三种：整数、一位小数、字符串。开关写 `0`/`1`，缺值写 `null`。
- 字符串清洗（`ledger_clean`）：制表、换行变空格，只留可打印 ASCII，去掉 `"` 和 `\`，最长 160 字符。运营商一律写 MCC-MNC 数字
  （如 `440-10`）。IMSI、ICCID、号码、MAC 一律不进账本。
- 读的一方：解析不了的行跳过并计数；不认识的 `k` 跳过；`v` 大于自己认识的版本就整行跳过；同一 `id` 出现多次只算一次。

## 4. 事件种类（v1）

「fsync」标 ✔ 的是关键事件：写完立刻 fsync（`dd if=/dev/null of=<段> conv=notrunc,fsync`）。

| k | 谁写 | 什么时候 | 编号 `id` | 字段（按顺序） | fsync |
|---|---|---|---|---|---|
| `boot` | 账本任务 | 每次开机一次 | — | `boot` 完整 boot_id；`code` 本次原因码（key.log 最后一个 `reboot_reason_code=`，没有写 null）；`mode` 开机模式；`fw` 固件版本（`zwrt_web device_info` 的 `wa_inner_version`，读不到用 tr069 的 SoftwareVersion）；`net_select`（uci `zte_nwinfo` 里的制式偏好）；`rb_weekly`、`rb_cutoff`、`rb_connfail` 原厂三个自动重启的配置（`选项=值` 空格分隔，不含节头和包名、节名，截到 60 字节）；`pon` 开机原因（`ubus call zwrt_bsp.pm list` 的 `power_on_reason` 原值，整数，读不到写 null；10-04 加，旧行没有这个字段，没升 `v`） | ✔ |
| `ver` | 账本任务 | 紧跟 `boot`，每个程序一行；之后被测程序 pid 变了且 md5 也变了再记 | — | `prog`；`md5` 前 8 位；`how`：`exe` / `file`；`pid`；`extra`（datad 写 `ubus=cli` 或 `ubus=socket`） | |
| `boot_backfill` | 账本任务 | 写 `boot` 时 | `kl-<inode>-<行号>` | `code`；`at` key.log 里那行的时间文字；`started`（0/1/null）；`gap`（0/1） | ✔ |
| `guard_start` | 账本任务 | guard 每次启动（按 `state/started.log` 补写） | `gs-<boot8>-<up>` | `requested`（0/1）；`by`；`why` | |
| `recovery_seen` | 账本任务 | 每次启动第一次读到；之后被改回 disabled 再记 | — | `value`；`set_by_guard`（0/1） | |
| `clock` | 账本任务 | 本次开机第一次判可信；之后可信被撤销或偏移变了 1 天以上再记 | — | `offset` = 墙钟 − 开机秒数（撤销时 null）；`how`：`vendor` / `jump` / `last_wall` / `year` / `revoked` | |
| `net_change` | 账本任务 | `net_select`、服务网国家（MCC）和 `state/net.last` 记的不一样（第一次读到也记） | — | `net_select`；`net`；`home` | |
| `ssr` | 观察循环 | 一次基带崩溃（第 9 节） | `ssr-<boot8>-<kmsg 序号>` | `kseq`；`line` 断言原文；`rat`、`band`、`nrband`；`net` 服务网 MCC-MNC；`home` SIM 归属；`net_age` 网络缓存有多旧（秒）；`wan_ppm` | ✔ |
| `ssr_result` | 观察循环 | 这次崩溃跟随结束 | `res-<ssr 编号>` | `ssr`；`result`：`ok` / `no_drop` / `merged` / `timeout`；`recovered_s`；`rx_seen_s`；`down_seen`；`resumed`（0/1）；`gap_s`；`slept`（0/1） | ✔ |
| `ssr_dump` | 观察循环 | 崩溃后 90 秒内新出现的转储文件 | `dump-<boot8>-<文件名的 md5 前 8 位>` | `ssr`；`file`；`size` | ✔ |
| `link` | 观察循环 | 蜂窝口 down/up 变化 | `link-<boot8>-<计数>` | `state`；`cause`（`ssr` / `other`）；`res` 分辨率秒数 | |
| `sleep` | 观察循环 | 一段有证据的休眠结束（第 8 节） | `sleep-<boot8>-<计数>` | `from`、`to` 开机秒数；`ev`：`pm` / `stats` / `inferred` | |
| `oom` | 观察循环 | kmsg 里的内存不足行 | `oom-<boot8>-<kmsg 序号>` | `line` | ✔ |
| `thermal` | 观察循环 | kmsg 里原厂过热等级变化 | `th-<boot8>-<kmsg 序号>` | `level`（如 `0x3`）；`line`。每次开机约 150 秒时原厂会设一次初始等级，读的一方只数比本次开机第一条更高的 | |
| `svc_exit` | 账本任务 | crashlog 目录里出现新文件 | `cl-<文件 md5 前 12 位>` | `prog`；`file`；`status`（文件第 2 行）；`found`：`round` / `boot_init`；`crash_up` 文件名里的开机秒数 | ✔ |
| `proc_restart` | 账本任务 | datad、agent、u60-uid 的 pid 或进程启动时间变了（不在了先不记，回来时记） | `pr-<boot8>-<新 pid>-<启动时间>` | `prog`；`old`、`new` pid；`crashlog`（这一轮看到过它的新 crashlog 没有，0/1）；`ship`（1 = 上机事务重启的：`/data/u60-ship/txn` 是本次开机、对应组件（u60-uid 对应 touch 或 uid）、还在跑或结束不到 10 分钟；10-04 加，旧行没有） | ✔ |
| `uid` | 账本任务 | u60-uid 日志里的放弃（`giving up`）、交还（`starting the vendor UI`）、人为请求（`request:`、`corner long-press:`，记作 `other`） | `uid-<boot8>-<日志行号>`（本次开机累计的行号：日志改名成 `.old` 以后接着往下数，不从 1 重来） | `what`：`handback` / `gave_up` / `other`；`detail`（那一行原文） | ✔ |
| `datad_degraded` | 账本任务 | 一次降级开始、结束（第 11 节） | 开始 `dds-<boot8>-<since>`，结束 `dde-<开始那次开机的 boot8>-<since>` | `state`：`start` / `end`；`since` 标记第一行；`reason`；`dur_s`（`end` 时，按本次开机的开机秒数算；不知道写 null）；`how`（`end` 时：`gone` / `new_episode` / `stale` / `reboot`） | ✔ |
| `gap` | 账本任务 | 醒着时 watcher、capture、uidlog、crashlog、datad 某个数据源没在工作，恢复时写（第 8 节） | — | `src`；`from`（最后一次还在工作的那一轮）、`to` 开机秒数 | |
| `calib` | doctor（暂存） | `--calibrate-standby` 写了新基线 | `calib-<boot8>-<开机秒数>` | `rows` | ✔ |
| `hour` | 账本任务 | 每个整点（对时前按开机满一小时） | `h-<boot8>-<from>` | `from`、`to` 开机秒数；`ft`、`tt` 墙钟（不可信写 null）；`awake`、`asleep`（只算有证据的休眠）秒；`rounds`、`skipped`（主循环的轮数和其中跳过账本任务的）；`max_gap`；`net`、`home`（整点那一刻）、`abroad`（在国外的醒着秒数）；`free_mb`；`dump_mb`；`budget`；`g_guard`、`g_job`、`g_watcher`、`g_capture`、`g_uidlog`、`g_crashlog`、`g_datad`：各数据源这一小时没在工作的醒着秒数（第 8 节） | |
| `hour_power` | 账本任务 | 紧跟 `hour` | `hp-<boot8>-<from>` | `src`：`counter` / `vi`（读不到写 null）；`e` 总能量 mWh；`on_home_s`、`on_home_e`、`on_abroad_s`、`on_abroad_e`、`off_home_s`、`off_home_e`、`off_abroad_s`、`off_abroad_e`；`chg_s`；`batt_from`、`batt_to` %；`batt_tmax` 0.1 °C；`zone_tmax` m°C（只取主芯片侧的温度区：`cpuss-*`、`aoss-*`、`sys-therm-*`、`xo-therm`、`pm*_tz`、`battery`；基带侧的 `sdr*`、`mmw*`、`epm*`、`mdm*` 读一次就是一个发给基带的 QMI 请求，不读）；`throttle_s`（没有 cpufreq 写 null）；`e_partial`（能量缺了一部分：V×I 下有过休眠，或有读不到的轮）；核心起（manager `docs/designs/u60-radio.md` §4、D9/D10）：`off_home_quiet_pm`、`off_abroad_quiet_pm` 息屏不充电时蜂窝安静的千分比（连续 0 包 ≥10 s 的整段，跟诊断档 quiet10 同定义，约 5 s 粒度，休眠不算）、`off_home_wan_pkts`、`off_abroad_wan_pkts` 这段时间的蜂窝包数（厂商计数，含 IPA 转发；没比到写 null）；`off_home_cli_s` 息屏在家时有客户端的秒数（客户端数未知也算有） | |
| `hour_proc` | 账本任务 | 紧跟 `hour_power` | `hq-<boot8>-<from>` | `rss_agent`、`rss_datad`、`rss_devui`、`rss_ts` kB（整点那一刻，不在跑写 null）；`cpu_datad` 本小时平均千分比（同一个进程从头跑到尾才算，否则 null）；`idle_rows` 息屏且空闲的分钟数、`idle_wan` 这些分钟蜂窝包数的中位数（没有基线写 null）；`ts_on_s`、`ts_chk_s`、`ts_ok_s`（第 6 节「出口」） | |

以后的种类（第二步起）：`reboot_request`（`by`、`why`，我们的每条重启路径发出前写，要挺过重启）、`session`（设备占用锁）、`deploy`（E1）。

## 5. 暂存协议（spool）

- **放哪**：要挺过重启的写 `/data/ledger/spool/`：`ssr`、`ssr_result`、`ssr_dump`、`oom`、`calib`，以后的 `reboot_request`；
  丢了无妨的写 `/tmp/ledger-spool/`：`link`、`sleep`、`thermal`。空间不够时一律改写 `/tmp`（第 2 节）；往 `/data` 写失败也改写 `/tmp`，
  同样带 `lowspace:1`（本次开机仍能进账本，好过丢掉）。
- **文件名**：`<事件编号>.ev`。编号由生产方按第 4 节的规则生成；按 kmsg 序号、文件 md5 生成的编号是确定的，同一事件重复生成会得到同一个文件名，已经存在就不再写。
  用计数的编号，计数存 `/tmp/u60-guard/ledger/`，观察循环重起后接着数。
- **内容两行**：第 1 行 `<编号>\t<完整 boot_id>\t<up>\t<k>`；第 2 行是清洗好的种类字段片段，以 `,` 开头（没有字段就空行）。
  片段只能是可打印 ASCII，不含换行，≤ 400 字节。
- **生产方写法**：写 `<编号>.tmp` → fsync 这个文件（`/data` 下的）→ `mv` 成 `.ev` → `/data` 下的再 `sync`（让改名落盘）。
- **收取**（账本任务，每轮和启动时）：按文件名排序逐个处理——
  1. 头格式不对、片段不合法：挪到 `spool/bad/`，记一行 guard 日志。
  2. boot_id 换成序号：本次开机用当前序号，其他查 `bootmap`，查不到写 `seq:0`。
  3. **去重**：这个编号已经在账本里（查事件所属那次开机及之后的段），直接删暂存文件、不重记。
  4. 追加前先看当前段末尾是不是换行，不是就先补一个换行（前面那半行成了坏行，读的一方跳过）。
  5. 追加，关键事件 fsync；检查写入和 fsync 都成功、段的最后一行就是刚写的这行，**然后**才删暂存文件。任何一步失败：保留暂存文件，下一轮再来。
- 被杀在追加后、删除前：下次第 3 步去重。被杀在追加前：暂存文件还在，下次照收。

## 6. 后台任务

- guard 每轮在本职（自恢复、Wi-Fi 兜底、RTC、datad 标记、短信、日志上限、kmsg 截短、待机、IPv6）之后，看 `job.pid` 里的任务还在不在
  （pid 在，并且 `/proc/<pid>/stat` 的启动时间和记下的一致）：
  - 不在：起 `( ledger_job ) </dev/null 2>>日志 &`，写 `job.pid`（pid 和启动时间）；
  - 在：不起新的，`skipped` 加一，记 `gap src=job`；它已经跑了 10 秒以上就记一行日志（每个任务只记一次）。
- `ledger_job` 先拿 `writer.lock`（`flock -n`，拿不到就退出，不做任何事）。本次开机还没有 `boot.done` 就先做开机初始化
  （第 10 节，每一步可重入：重复执行不会重复分配序号，也不会重复写 `boot`），然后做本轮的事：收暂存、采样、需要时写每小时摘要和保留。
- 一轮采样的外部读取合计 ≤ 3 秒：任务开始时定截止时刻（`GUARD_LEDGER_READ_BUDGET`），每次外部读取前看一眼，不够就不读、记一行日志，
  这一轮这个数据源算缺测；wget、curl、ubus 都带 2 秒超时。
- **网络缓存**：每轮读一次 datad `/state`（本机 9460 端口不要令牌）：`rat` = `net.type`，`band` = `net.band`，`nrband` = `net.nr_band`，
  服务网 = `net.mcc`、`net.mnc`（整数，按 MCC 补成 3 位或 2 位 MNC：302、310–316、334 等用 3 位，和 datad、agent 同一张表），
  SIM 归属 = IMSI 的前 5–6 位（同一张表）。IMSI 只在管道里取前 6 位，读完的 `/state` 文件立刻删掉，不留在 `/tmp`，也不进账本。
- **wall.last**：时钟可信时，账本任务每小时写一次（临时名、fsync、改名）。
- **主循环的轮次**：每轮末尾往 `/tmp/u60-guard/ledger/rounds` 追加 `<开机秒数> started|skipped`（账本任务这一轮起了还是因为上一个还在而跳过），
  主循环是唯一的写方，每 60 轮剪回最近 120 行（不超过 180 行）。账本任务从它算每小时的 `rounds`、`skipped`、`max_gap` 和 guard、job 两个数据源的覆盖。
- **每小时摘要**：每一轮把这一段的量加进 `/tmp/u60-guard/ledger/acc`（账本任务是唯一的写方），换小时（时钟可信后按墙钟整点，之前按开机满一小时；
  可信与否变了也算换）就写 `hour`、`hour_power`、`hour_proc` 三行，从零再来。三行按起点编号，重跑不重记；写不进（空间不够等）记日志，照样换下一小时。
  读不到的值一律 null，不写 0。每轮把累计另存一份 `state/acc.last`：重启时那不满一小时不会丢，下次开机按上一次开机的序号写出来，
  `to` 是它最后一轮，`hour` 带 `cut:1`，只能当场读的（主循环轮数、内存、CPU、剩余空间、整点时的网络）写 null。
  - **能量**：有电量计的累计量（`battery/charge_counter`，µAh）时，按相邻两轮的差 × 电压算，跨过休眠也对，算进后一轮所在的格子，格子的秒数也含休眠；
    没有时每轮用 V×I 只积醒着的那一段，本小时有过休眠就标 `e_partial`。充电（`status` 是 Charging/Full，或电流为正）的时间进 `chg_s`，不进格子。
  - **降频**：某个 cpufreq policy 的 `scaling_max_freq` 低于它本次开机见过的最高值，这一轮算降频（不和 `cpuinfo_max_freq` 比：原厂可能一直压着）。
  - **出口（S8）Tailscale**：算「开着」= 蜂窝口有默认路由且 `rc.local` 会起它。开着的每一轮，账本任务问一次 tailscaled 的 LocalAPI：
    `curl --unix-socket /tmp/tailscaled.sock http://local-tailscaled.sock/localapi/v0/status`（和触屏卡片同一个路径、同一个 Host），
    限时取这一轮剩下的读取时间、最多 2 秒，剩下不到 1 秒就不问。「正常」= `BackendState` 是 `Running` 并且 `Self.Online` 为真
    （`apply.sh` 的 relay 模式也看这两样；它另外核对子网路由重启后还是主路由，那是比较前后两个时刻，这里不用）。结果分三种：
    - 正常；
    - 不正常：socket 不在、连不上、超时、5xx，或者不是 `Running`、不在线；
    - 没查到：没有 curl 或它不支持 unix socket、4xx（是请求的问题，不是 tailscaled 的）、回答里没有 `BackendState`、这一轮没有读取时间了。
      原因写一次日志，原因变了或者又查到过以后再写。

    上一轮到这一轮醒着的秒数记进 `ts_on_s`；查到了再记进 `ts_chk_s`，正常再记进 `ts_ok_s`。没查到的秒数只算没覆盖（第 8 节），不算不正常；
    也不拿「进程在」冒充正常。
- 测试入口：`GUARD_LEDGER_FG=1` 让任务在前台同步跑；另有用例专门测后台跳过和写锁。

## 7. 时钟

- **可信**：年份 ≥ 2026，并且满足任一：
  1. 原厂对时状态表明已对时（字段在设备上核实后填进 `GUARD_SNTP_OK_CMD`；没核实前这一条不用）；
  2. 本次开机里观察到「墙钟 − 开机秒数」向前跳了 1 天以上（`jump`）；
  3. 墙钟不早于 `state/wall.last` 减 1 天（`last_wall`）；
  4. `state/wall.last` 还不存在（第一次部署）时，只看年份（`year`）。
- **撤销**：已经可信之后，「墙钟 − 开机秒数」又往回跳了 1 天以上，或者墙钟早于 `wall.last` 减 1 天：判不可信，记 `clock how=revoked`，
  直到再次满足上面的条件（再记一条 `clock`）。
- guard 每轮更新 `/tmp/u60-guard/clock-ok`：`<0|1> <offset> <写入时的开机秒数> <how>`。可信时账本任务每小时写一次 `state/wall.last`
  （主循环不碰 `/data`）。
- guard 里所有「时钟可信吗」都用同一个函数：短信的对时门槛（原来的 `CLOCK_SANE_AFTER`，u60-guard.sh 的 sms_round）、datad 降级标记的计时
  （datad_round）、账本的 `t`。旧常量 `CLOCK_SANE_AFTER=2024-01-01` 作废（对时前设备时钟是 2025-01-04，它会被当成可信）。
- doctor：`clock-ok` 存在且是 5 分钟内写的，就用它；否则自己判：年份 ≥ 2026，并且（`wall.last` 不存在，或墙钟不早于它减 1 天）。
- 读的一方：`t` 为 null 的行，用同一次开机最近一条有效 `clock` 的偏移补算 `t = up + offset`；这次开机没有有效 `clock` 就一直 null。

## 8. 覆盖和休眠

- **各数据源的存活判据**（账本任务每轮检查，缺了就记 `gap`，同一段缺口只记一次、结束时写 `to`）：

  | src | 算在工作的条件 |
  |---|---|
  | `guard` | 醒着时相邻两轮的开始时刻相差 ≤ 180 秒（扣掉有证据的休眠）；超出的部分减去正常的一轮计入缺口 |
  | `job` | 账本任务没有被跳过（每跳过一轮计一轮的秒数） |
  | `watcher` | 观察循环进程在，并且它的心跳 `w.up` 离现在 ≤ 10 秒 |
  | `capture` | kmsg 落盘读取进程的 pid 在（不看文件有没有变大） |
  | `uidlog` | `/tmp/luyaz-screen.log`（E2c 前 `/tmp/u60-uid.log`）最后一行能解析（以时间开头）；它刚被改名或截短、是空的时看 `.old` 的最后一行 |
  | `crashlog` | crashlog 目录在、能读 |
  | `datad` | 这一轮 `/state` 读到了 |

  每一轮哪些数据源没在工作，这一轮醒着的秒数就记进 `hour` 行对应的 `g_*`；后五个在恢复工作时另写一条 `gap`（从最后一次还在工作的那一轮到恢复的那一轮）。
  重启时还没写出的那一小时没有 `hour` 行：**没有 `hour` 行的醒着时间一律算没覆盖**。

- **休眠只认证据**：观察循环某一次循环发现开机秒数比上一次多了 > 10 秒（一段停顿），并且有证据：这段停顿前一次、当次或之后 3 次循环里
  读到了内核挂起、唤醒行（`PM: suspend`、`PM: resume` 开头，如 `PM: suspend entry`、`PM: suspend exit`），或者
  `/sys/power/suspend_stats/success` 增加了：写 `sleep`（`ev=pm` / `stats`）。区间 `from` 是本该有下一次循环的时刻（上一次 + 2 秒），
  `to` 是迟到的这一次。只豁免这一段本身；一条证据只用一次。3 次循环内没等到证据的写 `sleep ev=inferred`，**不豁免**。
  观察循环刚起（或重起）的第一次循环不判停顿：那段时间它根本不在。
  局限：同一段里观察循环也卡住了、又恰好挂起过，会多豁免一些；报告里写明。9-26 的落盘文件里一条挂起行都没有，
  设备上有没有 `suspend_stats` 待核实；两样都没有的话，夜里的停顿全是 `inferred`，相关指标会写「没测」。
- **不算缺口**：有证据的休眠；关机充电的开机（guard 不起）；开机头约 45 秒（只归 S2a 的空档一栏）。
- **窗口的起点**：账本上线（第一条 `boot`）之前的时间，一律算覆盖不完整。
- **每项指标依赖哪些数据源**见第 11 节的表；缺口按这些数据源合并（取并集）。
- **完整**：窗口里醒着的时间，合并后的缺口合计 ≤ 5%，并且没有一段超过 10 分钟。否则该项写「没测」并列出缺口。
- **Tailscale 自己的覆盖**（S8 的 Tailscale 一半）：Tailscale 开着的时间里，没查到的（`ts_on_s` − `ts_chk_s`）合计 ≤ 5%，
  并且没有哪一小时里超过 10 分钟（只有每小时的合计，所以按小时判，跨整点的一段不合并）。没有 `ts_chk_s` 的 `hour_proc`
  是有这项检查之前写的，开着的时间全算没查到。

## 9. 崩溃观察循环

- **谁起它**：guard 每轮（`watcher_round`）看 `watcher.pid` 里的进程在不在（pid 加启动时间）、版本（`u60-guard.sh` 的 md5 前 8 位）对不对；
  不在就起 `sh u60-guard.sh watcher`，版本不对就结束旧的、起新的。起的时候关掉 fd 7、8、9，stdin/stdout 接 /dev/null：
  常驻的子进程要是继承了 fd 9，会一直占着 agent 的 Wi-Fi 锁（RELIABILITY.md §1）。观察循环自己每 30 次循环看一次磁盘上的
  `u60-guard.sh`，变了就退出（回退到旧版 guard 时它也会自己走）。`watcher.lock`（`flock -n`，最多等 5 秒）保证只有一个。
- **落盘文件的行**：`<优先级>,<序号>,<时间戳 µs>,<标志>[,caller=…];<正文>`（设备上实际带 `caller=`），以空格开头的是上一条的续行。
  只处理序号比 `w.pos` 里的最后序号大的记录行；续行、截短留下的半行一律跳过。/dev/kmsg 把可打印 ASCII 以外的字节都转义了，
  所以按字节数推进位置是准的。
- **每 2 秒一次循环**：
  1. 用 `ls -ln` 取文件大小（busybox 的 `wc -c` 会把 8 MB 读一遍），比位置大才读：`dd iflag=skip_bytes` 从位置读到末尾，
     只处理以换行结尾的行，最后没写完的半行留到下一次。
  2. **截短**：guard 在 8 MB 原地截短后把 `/tmp/u60-guard/crashcap-cuts` 加一；计数变了、或者文件比位置还短，就从头重读，
     靠序号跳过已处理的。
  3. 对新行做匹配、写暂存；**暂存都写好之后**才更新 `w.pos`（先落盘，后推进）。被杀在中间：重读时编号相同，暂存和账本都不会重复；
     `w.ssrlast` 记着最近打开的崩溃，重读到它不会再开一次。
  4. 读完并且都写好了（或者没有新内容）才写心跳 `w.up`；文件比位置大却什么也没读到，记一次日志。第 8 节的 `watcher` 数据源看的就是这个心跳。
- **第一次上线**：`state/watch-init` 不存在时，第一次读只把位置推到文件末尾，不出任何事件，然后建这个文件：上线那天落盘文件里已经有
  这次开机几个小时的内容，不能都按「现在」记。之后每次开机，观察循环起来后第一次读到的（开机头几十秒、或重起前没读的）照常记；
  其中的崩溃标 `resumed=1`，`gap_s` 是离上一个观察循环最后一次循环（本次开机第一个观察循环就是离开机）的秒数：那段时间没人跟。
- **认崩溃**（不区分大小写）：`fatal error received`、`watchdog received`、`crash detected`、`rproc recovery`、`recovering …remoteproc`、
  `subsystem restart`、`err_fatal`。设备上一次崩溃依次是（9-26 的落盘文件）：
  `qcom_q6v5_pas 4080000.remoteproc-mss: fatal error received: <断言>`、`… rproc recovery state: enabled and kick reovery process`
  （恢复关着时是 `disabled and lead to device crash`）、`remoteproc remoteproc0: crash detected in …: type fatal error`、
  `remoteproc remoteproc0: recovering …`。
  - 第一行的内核秒数（时间戳）起 60 秒内的崩溃行，都算同一次崩溃，不另起。用内核时间而不是循环时刻：补读时一次读进好几次崩溃也分得开。
  - 超过 60 秒、还没恢复：当前这次结果 `merged`，另开一次新的，从新崩溃重新计时。
  - 已经恢复、还在等流量，或者已判 `no_drop` 还在等流量：当前这次按已有的结果结束（`ok` / `no_drop`，没等到的 `rx_seen_s` 写 null），另开一次新的。
  - 打开时立刻写 `ssr` 暂存：`line` 是 `fatal error received: ` 之后的断言（没有就是整行），带崩溃前最后已知的网络信息（`net` 缓存，
    写明有多旧）和 `wan_ppm`（观察循环每分钟读一次 `/proc/net/dev`，取最近一整分钟两个方向的包数）。
- **链路**：up = 蜂窝口有 IPv4 地址（`ip -4 -o addr show dev`），并且 main 表的默认路由走它（`/proc/net/route`）；这次开机从没见过
  走它的默认路由时只看地址。`link` 事件每次循环都看（只读 `/proc/net/route`，不起进程）：默认路由来了、没了各记一条，
  开着崩溃时 `cause=ssr`，`res` 是离上一次采样的秒数。
- **跟随**（打开期间每次循环采样一次：地址、默认路由、rx_bytes）：
  - 先看到 down 再看到 up 才算恢复，`recovered_s` 从发现崩溃算起；恢复后 60 秒内等 rx_bytes 第一次增长，`rx_seen_s` 也从发现崩溃算起，
    没等到写 null（空闲时正常，不算没恢复）；
  - 30 秒内一直没看到 down：`no_drop`，`rx_seen_s` 是发现崩溃以来 rx_bytes 第一次增长；30 秒时还没有就再等到 90 秒；
  - 看到 down、10 分钟没恢复：`timeout`（P95 里按 ≥ 600 秒）；
  - 跟随期间有第 8 节的休眠证据：`slept=1`（只有证据才标）；
  - 相邻两次采样隔了 10 秒以上：多出来的秒数记进 `gap_s`；观察循环重起时有没结果的崩溃：接着跟，`resumed=1`。结果仍是上面四种之一。
- **转储**：`/data/vendor/ramdump/`（设备上是 `ipa_driver_<时刻>.elf`）。观察循环起来时、没有窗口时每 60 秒、窗口结束时记下当时的
  文件名作基准。崩溃打开一个 90 秒的窗口（下一次崩溃会提前结束上一个窗口），基准里没有的新文件，相邻两次循环大小不变
  （或窗口到期）时写一条 `ssr_dump`，每个文件只写一次。恢复了也看满 90 秒。
- 顺带认：`Out of memory: Kill…`（含 memcg）→ `oom`；`zte_thermal_update_throttling_level() to 0x…` → `thermal`；内核挂起行（第 8 节）。
- 测试入口：`GUARD_WATCHER`（0 = 不起）、`GUARD_WATCH_MAX` 限制循环次数、`GUARD_WATCH_SLEEP` 替换 sleep、`GUARD_WATCH_INTERVAL`、
  `GUARD_WATCH_GAP`、`GUARD_DUMP_DIR`、`GUARD_ROUTE`、`GUARD_IP`、`GUARD_NETDEV`、`GUARD_SUSPEND_STATS`。

## 10. 开机初始化和补记

每一步都可重入，并且「先写账本、后推进检查点」：

1. **序号**：`bootmap` 最后一行的 boot_id 就是本次开机，沿用它的序号；否则 `seq` +1（写临时名、fsync、mv；文件坏了就用段文件名和 `bootmap`
   里最大的序号 +1），再追加 `bootmap` 并 fsync。结果写 `/tmp/u60-guard/ledger/seq`。
2. **开机事件**：本次开机的段里还没有 `boot` 行才写 `boot`、`ver`、`guard_start`、`recovery_seen`。之后每次 guard 启动只写 `guard_start`。
3. **key.log 补记**：锚点是上次入账的原因码行（inode、行号、它和前面 20 行的 md5；busybox 的 grep 没有 -b，行号和字节位置一样在追加、改名轮转时不变）。先在 key.log 找（inode 相同且那 21 行对得上），
   再到 key.log.0 找；从锚点之后到本次开机那一行之前，每个 `reboot_reason_code=` 各记一条 `boot_backfill`（编号按所在文件的 inode 和行号，
   重复执行不会重记）。全部写好并 fsync 之后才把锚点推进到本次开机那一行。两处都找不到锚点：只补 key.log 里能看到的、标 `gap=1`，
   不重补 key.log.0（避免把旧的重启再算一次）。`state/init-done` 不存在（第一次上线）：不补历史，只设锚点。
4. `boot_backfill.started`：那次开机有没有 `bootmap` 记录或 `state/started.log` 里的记录（原因码行不带 boot_id，按顺序对应，对不上写 null）。
5. **crashlog**：`state/crashlog.seen` 记已入账的文件。第一次上线把现有文件全记作基准、不计数。之后每轮和开机时扫 `/data/crashlog/<程序>/`，
   名字、大小、md5 任一对不上就是新文件，写 `svc_exit`（编号按文件内容的 md5），写好之后才更新 `crashlog.seen`。
   触屏崩溃以 `/data/crashlog/u60pro-devui/` 为准，u60-uid 日志只补交还、放弃这类 crashlog 里没有的。
6. 第一次上线的基准建好之后写 `state/init-done`；整个初始化做完写 `/tmp/u60-guard/ledger/boot.done`。
- **人为重启**：上机或手动重启 guard 前先写 `/tmp/u60-guard/stop-requested`（`<谁> <为什么>`）；guard 启动时读到就记 `requested=1` 并删掉它。

## 11. 读的一方（doctor --report）

- 按文件名顺序读全部段，按行里的 `seq` 归属开机；跳过解析不了的行并计数；同一 `id` 只算一次。
- **时间**：每次开机的「墙钟 − 开机秒数」取它自己可信的行（`t` 或 `hour` 的 `tt`）；`t` 为 null 的行用它补算，这次开机一行可信的都没有就算时间不明、
  不进任何窗口。`hour_power`、`hour_proc` 按编号里的起点挂到对应的 `hour`。
- **报告截至**最后一条 `hour` 的结束时刻（当前没满的那一小时不算缺口）；窗口起点也不早于账本第一条 `hour`。
- **在家 / 国外**：崩溃按 `ssr` 自己的服务网和 SIM 归属；重启按上一次开机最后一小时 `abroad` 过半与否。
- **开机之间的空洞**：上一次开机最后一小时结束到下一次开机第一小时开始超过 5 分钟，并且下一次开机不是人按的（1150、1152）、也不是充电开机，
  这段算没覆盖（设备开着但没人记）。
- **崩溃按发生顺序**排（暂存按文件名收取，不是时间顺序）。
- **原因码分类**：1150、1152 人按的（不算意外）；1112 原厂定时（单列）；1155 断网保护、1185 屏幕没界面、1134、1132 基带（算意外）；
  1182 要判别：上一次开机的最后一条 `ssr` 没有 `ssr_result`，或者观察循环没来得及看到、而上一次开机的落盘文件以崩溃行结尾、后面再没有别的记录
  （doctor 看 `/data/crashcap/kmsg-<boot8>.log` 最后一条记录行）→ 基带；上一次开机账本最后有我们的 `reboot_request` → 我们发起的（第二步起）；
  上一次开机的落盘文件已经没了、或以上都不是 →「来源不明」（算意外）。
- **窗口**：每项 = 「要求的窗口 W」和「这项最近一次相关改动之后」取短的；改动前的违反另列一行。改动：自恢复第一次上线
  （2026-09-26 21:43，固定记录；每次开机 guard 重新打开不算）→ S1、S2a；某程序 `ver` 的 md5 变了 → 它的 S4、P4、P5；u60-uid 接管屏幕
  （2026-09-23，固定记录）→ S5a、S5b；`net_change` → S2b、S3、P2；`calib` → P3。
- **档位**：没测（覆盖不完整，或没有数据源）/ 不达标（窗口内有违反，含已知事实）/ 注意（覆盖完整、没违反、窗口比要求的短，写出已有多长）/ 达标。
- **datad 降级**：一次降级按标记第一行的 `since` 区分。开机 5 分钟后、看到新鲜的标记（mtime 在 3 分钟内）并且 `since` 是新的 → `start`；
  标记消失 → `end how=gone`；`since` 换了 → 旧的 `end how=new_episode`、新的 `start`；标记变陈旧（agent 没了）→ `end how=stale`，时长不知道；
  重启时还开着的 → 下次开机补一条 `end how=reboot`，时长不知道（开始时写 `state/datad.open`，结束时删）。时长只用同一次开机里的开机秒数算，
  不跨对时相减。同一个 `since` 在一次开机里只开始一次（陈旧后又新鲜不算新的一次）。
- **恢复用时 P95**：`timeout` 按 ≥ 600 秒；`merged` 按「到下一次崩溃仍未恢复」，一串连着的算一次长断网，时长取到最后恢复为止；
  `no_drop` 用 `rx_seen_s`，单列；`resumed=1` 时，缺口里可能已恢复就按缺口结束时刻算；`slept=1` 的样本单列，不能用来判「达标」。
- 每项都分在家、国外报（服务网和 SIM 归属的 MCC 是否相同）；S2b、P1、P2、P3 另分空闲和有流量（`wan_ppm` 或当小时的空闲判定）。
- 告警直接读 `/data/alerts/queue` 和 `sms-log`，排除 `sms-test`；队列最老一条比窗口起点晚，U6 标覆盖不完整。

**指标计算表**（W 是要求的窗口；「源」是第 8 节的数据源，覆盖不完整就是「没测」）：

| 项 | 怎么算 | 目标 | W | 源 |
|---|---|---|---|---|
| S1 | 窗口内算意外的重启次数（`boot.code` 和 `boot_backfill.code` 按上面的分类） | 0 | 7 天 | key.log 补记（锚点丢了 `gap=1` 算不完整） |
| S2a | 基带崩溃后整机重启的次数：原因码归为基带、且上一次开机 guard 起来过；上一次开机 guard 没起来的单列成「开机空档」 | 0 | 7 天 | 同 S1，外加 capture、watcher |
| S2b | `ssr_result` 的恢复用时 P95（上面的规则），分在家、国外，分空闲、有流量 | ≤ 45 秒 | 7 天 | watcher、capture |
| S3 | `ssr` 次数，按服务网国家、`rat`、`band` 分 | 在家 0；国外列出并写对策（人工） | 7 天 | watcher、capture |
| S4 | 每个程序：`svc_exit`（非 0 退出）+ 没有 crashlog 的 `proc_restart`；guard：同一次开机里没有 `requested` 的第二次及以后的 `guard_start`；`ship=1` 的（上机事务自己重启的）不算，手工重启仍算 | 0 | 7 天 | crashlog、guard |
| S5a | 原因码 1185 的次数 | 0 | 7 天 | 同 S1 |
| S5b | `uid what=gave_up` 次数（再对照 `/data/u60-uid/gave-up`、告警 `devui-gave-up`） | 0 | 7 天 | uidlog |
| S6 | `datad_degraded` 次数和总分钟数 | 每天 0 | 1 天 | guard、datad |
| S7 | E1 前靠已知事实（版本漂移） | — | — | — |
| S8 | Tailscale：`ts_ok_s` ÷ `ts_chk_s`，覆盖另按第 8 节「Tailscale 自己的覆盖」；把没查到的全算正常也不到 99% 就直接不达标。没开过写没测 | ≥ 99% | 7 天 | guard |
| U1–U5 | 第二步前没测，或靠已知事实（写明） | — | — | — |
| U6 | 列出窗口内全部告警（排除 sms-test）和当前体检里的 ▲，人工判 | — | 7 天 | 告警队列 |
| P1 | `hour_power.e` 折成平均功率和按当前电量的续航；先不定目标，攒够 7 天 | 待定 | 7 天 | guard |
| P2 | 四格（亮屏/息屏 × 在家/国外）的平均功率，剔除充电分钟；每格样本少于 6 小时写「样本不足」 | 待定 | 7 天 | guard |
| P3 | 当前待机判定是否只在空闲时做、有没有过期的列 | 规则成立 | — | 待机记录 |
| P4a | `hour_proc.cpu_datad` 的平均 | ≤ 2% | 1 天 | guard |
| P4b | datad `ver.extra` 是否 `ubus=socket` 或 `ubus=auto`（socket，连不上 ubusd 时临时退回 CLI） | socket / auto | — | — |
| P5 | 连续运行 ≥ 24 小时的开机：以开机满 1 小时后第一条 `hour_proc` 为基准，RSS 增长 ≤ 10%；`oom` 为 0 | ≤ 10%、0 | 7 天 | guard、watcher |
| P6 | `hour_power.throttle_s` 合计（null 的小时算没测）、`thermal` 里比本次开机第一条更高的等级出现的次数 | 0 | 7 天 | guard、watcher |

- 输出：每项一行，写数值、档位、已有窗口 / 要求窗口、覆盖、判定方式（自动 / 人工 / 靠已知事实 / 第 X 步前没测），细项另起一行缩进。
  `--report 24h` 把所有窗口压到 24 小时（7 天的项因此最多「注意」）。S7、U1–U5 在对应步骤之前按已知事实固定写不达标；P1、P2 目标待定，
  只给数值；P2 的小时记录不分空闲和有流量，这一拆分第二步再做。P5 没有内存记录时写「没有内存记录」（没测），不说「都在 10% 以内」。
- **耗时**：16 MB（保留上限）的假账本，容器里 `--report 7d` 约 1～2 秒；设备慢几倍，按需跑和每小时后台跑都可以接受，上机后实测补上。

## 12. doctor --tsv 的约定

- agent 每 60 秒以 `--tsv` 调 doctor，结果原样上触屏和网页的健康行。`--tsv` **不加行、不读账本**，行的 id 和顺序不变。
- 只有两行为修错而变：
  - `standby`：只判空闲的息屏分钟（蜂窝包数 ≤ 基线中位数 + 3×MAD），有流量写「有流量，不判」；指纹对不上的列写「基线过期」、不报 ▲。
  - `clock`：按第 7 节的判据；开机 5 分钟内还没对时写「刚开机，还在对时」算 ok，之后还没对时才 warn。
- **待机指纹**：
  - guard 在开机和被测程序 pid 变化时算好程序的 md5，写 `/tmp/u60-guard/ledger/fp`（每行 `<程序> <pid> <md5>`），并在
    `/tmp/u60-guard/ledger/fp-changed` 记下最近一次变化时的开机秒数。
  - 基线文件（`standby.baseline`，第 2 版）：原来的 `<列> <中位数> <MAD>` 行之外，加 `fp <列> <依赖的 md5 串>` 行和第一行 `v2`。
    每列的依赖：2 蜂窝包（无）；3 隧道包（tailscaled、`tuning.env`）；4 tailscaled 唤醒（tailscaled、`tuning.env`）；
    5 触屏唤醒（u60pro-devui）；6 datad 唤醒（zwrt-datad）；7 agent 唤醒（zte-agent）。配置文件的 md5 每次现算（都很小），程序的 md5 读 `fp`。
  - 某列依赖的 md5 对不上：那一列写「基线过期」、不判。基线是旧格式（没有 `v2`）：整行写「基线格式旧，请在家自然空闲时重新校准」，不判。
  - 只用 `fp-changed` 之后的待机记录来判（变化之前的记录属于旧版本）。
- 默认（给人看的）输出末尾多 3 行，照抄 `summary`；`summary` 不存在就不写。`--tsv` 不读账本也不读 `summary`。

## 13. selftest

`doctor.sh --ledger-selftest`：不碰正式账本和 recovery，不起读取进程和观察循环。逐项写 PASS/FAIL：
目录可写（写 `/data/ledger/.selftest` 一行 512 字节、fsync、读回、删除）；key.log 能解析出最后一个原因码；能读 recovery；
往 `/dev/kmsg` 写一行带随机数的 `u60-ledger-selftest` 测试行，6 秒内在落盘文件里出现（观察循环在跑时，也要在它的已读位置之前）。
测试行不计入任何指标。另外按 guard 的原样用法试一遍它依赖的设备工具：`dd iflag=skip_bytes`、`ls -ln` 第 5 列、awk 的 `mktime`/`strftime`
和括起来的 `?:`、`flock -n`、jsonfilter 多个 `-e`（中间一个路径不存在）、`wget -T 2` 读 datad `/state`，以及观察循环判链路的两样：
`ip -4 -o addr show dev rmnet_data0` 看得到 `inet `、main 表的默认路由走 `rmnet_data0`（此刻没联网也会 FAIL，要在联网时跑）。上机前在设备上先跑它：
哪个工具设备上没有或行为不同，在这里先暴露。全部 PASS 才退出 0。

## 14. 公开导出

公开版不含私有扩展的字段和代码；读的一方按缺字段处理。
