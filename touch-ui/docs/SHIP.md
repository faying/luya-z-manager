# luyaz ship 设备端：事务日志、暂存目录和清单的格式

`luyaz ship` 在 U60 Pro（MU5250）上的设备端是两个脚本：

| 脚本 | 设备上的位置 | 谁更新它 |
|---|---|---|
| `scripts/u60-ship.sh` | `/data/luyaz/scripts/luyaz-ship.sh`（`datad-trial.sh` 在同一目录，是它的壳；Luya-Z 改名 D4 前是 `/data/u60-guard/u60-ship.sh`） | 每次 ship 随上传一起带上；换之前先在设备上 `sh -n` + `u60-ship.sh selftest` |
| `scripts/u60-recover.sh` | `/data/luyaz/ship/luyaz-recover.sh`（D4 前 `/data/u60-ship/u60-recover.sh`，`move-d4` 后那里只剩转发的薄壳） | **不随 ship 更新**；换它是单独一步（`sh -n` + `u60-recover.sh selftest` + 先问用户），和改 rc.local 一样 |

本文件是两者和 Mac 端 `luyaz`（T7；`luyaz ship <组件>`、`luyaz rollback`、`luyaz status` 等，是维护者自己工作区的工具，不在本仓库里，没有它的人用装机包 `onboard/`）、guard 兜底（T4）、doctor（T6）之间的约定。改格式要升版本号（下面每种文件都有 `v`），
并且旧版 u60-recover.sh 必须仍能安全地处理（它看不懂的版本一律不动文件）。

## 设备上的文件

| 路径 | 内容 | 写法 |
|---|---|---|
| `/data/luyaz/ship/stage/<事务号>/` | Mac 上传的 `meta` 和产物 | Mac 写；`stage` 核对后把产物拷到旁路名并删掉暂存副本，只留 `meta`；事务结束时整个目录删掉 |
| `/data/luyaz/ship/txn` | 当前（或最近一次）事务的事务日志 | 每次改：临时文件 → sync → mv → sync |
| `/data/luyaz/ship/ship.log` | 执行器、stage、recover-live 的人看日志 | 追加 |
| `/data/luyaz/ship/recover.log` | u60-recover.sh 的日志（只在它动手或看不懂时写） | 追加 |
| `/data/luyaz/ship/test.log` | 试跑时测试版的标准输出/错误 | 追加 |
| `/data/luyaz/ship/lock` | `stage` 用的 flock 文件（防两个 stage 同时进来） | — |
| `/data/luyaz/ship/manifest.jsonl` | 设备清单，一行一条 | 整个文件重写：临时文件 → sync → mv → sync |
| `/tmp/luyaz/ship/heartbeat` | 执行器心跳（内存盘，重启就没了） | 临时文件 → mv |
| `/tmp/luyaz/ship/executor.pid` | detach 时用的 pid 文件名前缀（`-p <它>.none`） | — |
| `<正式文件>.test` | 旁路名：`stage` 放在正式文件同一目录，转正时 mv 过去 | — |
| `<正式文件>.prev-<事务号>` | 转正前的上一版（程序和状态文件都有）；每个文件只留 ship 生成的最近 3 份 | cp → sync |

所有设备路径在测试里都可以整体加前缀（`U60S_ROOT`、`U60S_TMP`、`U60R_ROOT`），测试不会碰容器里真的 `/data`、`/tmp/luyaz/ship`。

这些路径只有新的一套（de-old，10-16）：`/data/luyaz/ship/.migrated` 不在（设备没跑过 `u60-ship.sh move-d4`）时 `stage` 和 `prepare-rollback` 一律拒绝，提示先 `move-d4`；`move-d4` 自己、`status`、`selftest` 照常。旧路径只有 `move-d4` 还认。

## 事务号

`YYYYMMDD-HHMMSS-<组件>`（Mac 上的 **UTC** 时间——设备全球用、Mac 跟着换时区，当地时间会往回走，而清理 `.prev` 靠字典序排新旧；组件名小写字母数字，1–12 个字符），例如 `20260930-143000-datad`。
正则：`^[0-9]{8}-[0-9]{6}-[a-z0-9]{1,12}$`。

`.prev-<事务号>` 因此一定以 8 位数字开头；手工留的备份（`zte-agent.prev-esim-20260926`、`zwrt-datad.prev-cache-20260923`、
`*.orig-backup`、`admin.prev-*.tgz` 等）都不符合这个格式，清理时一律不碰。按事务号的字典序排新旧（前面是时间）。

## 组件

| 组件 | 期 | 状态 | 正式文件 | 状态文件（精确清单，不按目录） | 试跑 / 转正后检查 |
|---|---|---|---|---|---|
| `core`（别名 `datad`；Luya-Z D4c 起事务号和清单写 `core`，旧的 `datad` 行照样算同一个） | 一 | 可用 | 核心 `/data/luyaz/luyaz-core` | 无（10-06 起；下面格式示例里的 cooling.conf 只是举例） | 旁路顶替 180 s / 120 s（datad-trial 的全部检查） |
| `agent` | 一 | **8d-2c 删除**：zte-agent 退役，接口归核心 luyaz-core；stage / record-kit / prepare-rollback 按名字拒绝（「已删除（8d-2c）」），`luyaz` 构建前就拒 | （原 `/data/zte-agent`，留在设备上不动） | — | — |
| `touch` | 二 | 可用（10-15 起只编 `rs/`） | `/data/luyaz/ui/luyaz-ui`、`/data/plugins/u60pro-devui/start.sh` | `/data/luyaz/ui/devui.conf` | 旁路顶替 60 s / 120 s（下面「第二期」的 touch 检查） |
| `uid` | 二 | 5f 后退役：rc.local 起哨兵时 stage 拒绝（`luyaz` 构建前也拒），`luyaz rollback sentinel` 回旧 rc.local 后才又能用；10-15 设备上的 u60-uid 程序和 `/data/u60-uid/` 已删 | `/data/plugins/u60pro-devui/u60-uid` | — | direct，— / 120 s（同 touch 的转正后检查） |
| `web` | 二 | **8d-2c 删除**，同 agent；管理网页归 luyaz-web（`luyaz ship lweb`） | （原目录 `/data/admin`，留在设备上不动） | — | — |
| `guard` | 二 | 5f 后退役，同 uid；`u60-guard.sh` 冻结在 c2b5ee8、只作退回目标，它的容器测试已删（场景归哨兵和核心的 Rust 测试，manager u60-sentinel.md §21），别再改它 | `/data/u60-guard/` 下 `GUARD_FILES` + `/etc/init.d/` 下我们的 4 个 init 脚本 `GUARD_INITS`（txn v=2） | — | direct，— / 300 s（下面「第二期」的 guard 检查） |
| `scripts` | 五 | 可用（5f 前装哨兵的启动脚本；10-12 起也带 luyaz-web 的，要 formats 4，先 install-recover） | `/data/luyaz/scripts/` 下 `SCRIPTS_FILES`（Luya-Z 改名 D1 起；`/data/u60-guard/` 的旧副本已删，D1、D4 的下限无条件生效；第一次往新目录 ship 时 stage 建目录；`u60-fallback.sh` 10-16 改名 `luyaz-fallback.sh`：第一次带新名字的 ship 把它当新文件放上去，旧的 `u60-fallback.sh` 不在清单里、留在原处不动（没人再读）；ship 和 scripts 的退回都删不掉 `luyaz-fallback.sh`，`touch` 在它不在时拒绝（触屏只找这个名字）；GUARD_FILES 去掉 guard 自己，8d-2c 起也去掉 `zte-agent.init`，Luya-Z 改名 E3 起也去掉 `zwrt-datad.init`）+ `/etc/init.d/{luyaz-core,luyaz-sentinel,luyaz-web}`（`SCRIPTS_INITS`；txn v=4/5；`/etc/init.d/zte-agent` 8d-2c 起、`/etc/init.d/zwrt-datad` 和 scripts 目录里的 `zwrt-datad.init` E3 起不再管，留在设备上不动，旧 init 在新布局下拒绝启动） | — | direct，— / 60 s（md5 + sh -n） |
| `sentinel` | 五 | 可用（转换要用户同意） | `/data/luyaz/luyaz-sentinel`；第一次（`--convert`）另加 `/etc/rc.local`，在设备上从现有文件改出来（txn v=3） | — | direct，— / 300 s（下面「哨兵」的检查 + 核心的判定，两者都要过） |
| `lweb` | 8c | 可用（10-13 上机；第一次要先 install-recover、ship scripts，rc.local 加一行都问用户） | `/data/luyaz/luyaz-web`（manager crates/luyaz-web，`release`：unwind；8d-2c 起 `0.0.0.0:9090`） | — | 旁路顶替 120 s / 120 s（下面「luyaz-web」的检查） |
| `selftest` | — | 只在设了 `U60S_ROOT`（沙盒）时存在，供 `selftest` 和测试用 | 沙盒里的两个文件 | 沙盒里的两个文件 | 没有旁路（direct），检查 2 s |

- **datad 的检查**（试跑看测试版，转正后看正式版，每 10 秒）：`/v2/state` 的 live 块 30 秒不变；进程不在；屏幕日志（哨兵写 `/tmp/luyaz-screen.log`；E2c 前叫 `/tmp/u60-uid.log`）出现放弃 /
  交还原厂界面 / 界面意外退出，或界面 `luyaz-ui` 不在。任何一项 → 试跑中止 / 转正后退回。（原来的第四项 zte-agent netwatch 计数 8d-2c 随 agent 删掉：核心不写这个计数。）
  屏幕日志读 `/tmp/luyaz-screen.log.old` + `/tmp/luyaz-screen.log`（到 64 KB 被 mv 成 `.old`；以前是 guard，现在是哨兵）。计数跟着轮转走：每次记下两份的 inode
  和各自的坏行数，上次见过、这次没了的那份（被轮转换掉的旧 `.old`）把它的坏行数累加进「已消失」，所以轮转前刚写的一行照样算新的，
  旧 `.old` 里原有的坏行被换掉也不会让计数变小、吃掉新的一行。只有一次轮询之间轮转两次（64 KB × 2），中间那份里的行看不到。
  touch / uid 的界面检查同样。测试用 `DT_LOGREAD` 时是那份输出的简单计数。
- 试跑启动的命令和 init 脚本 `start_service` 一样（init 由 procd 直接拉起，试跑不经 procd，崩了不被拉起；supervise.sh 10-15 已退役）：datad、touch、lweb 见 `print-launch <组件>`。测试里各有一个
  「和 init 脚本对不上就失败」的用例。
- 只追加的日志和账本（`/data/scenario/log`、`/data/u60-guard/ledger` 等）不是状态文件，退回时不会被覆盖。
- datad 的 `/data/zwrt-datad/wifi` 是运行时目录，不进状态文件；状态文件只能是普通文件（或者不存在）。
- 组件描述在 `u60-ship.sh` 的 `comp_load` 里：`C_KIND`（`trial` 旁路顶替 / `direct` 直接转正）、`C_FILES`、`C_DIRS`（整个换的目录）、
  `C_STATE_ALLOW`、`C_TRIAL`、`C_CHECK`、`C_ALLOW_NEW`（正式文件可以原来没有：guard）、`C_PRE`（direct 转正前先跑 `pre` 钩子，
  不过 = 中止：guard 的旧 doctor）、`C_NOTREADY`，外加每个组件一组 `c_<组件>_*` 钩子：`pre`、`stop`、`start`（起来并确认）、
  `launch_test`、`trial_watch`、`kill_test`、`check`。

## 暂存目录和 meta（Mac → 设备）

Mac 把产物和 `meta` 放进 `/data/luyaz/ship/stage/<事务号>/`，产物的文件名 = 正式文件的文件名（核心是 `luyaz-core`）。
`meta` 是 `key=value`，一行一个，不 eval、不 `.`：

```
v=1
txn=20260930-143000-datad
comp=datad
commit=7e8d223                 7–40 位十六进制
mac_time=1790000000            Mac 的 Unix 时间（设备时钟开机后可能没对上）
format=1                       这个版本的数据格式版本（仓库里声明，T8）
trial=180                      可选，试跑秒数 60–86400（--trial）
note=requires-ignored          可选：requires-ignored（--ignore-requires）、rollback（luyaz rollback）、rollback+requires-ignored；照抄进清单行
file=luyaz-core 0123…cdef      产物文件名 + md5，组件的每个正式文件一行
state=/data/zwrt-datad/cooling.conf   可选，可重复；必须在组件的状态文件清单里；一行都没有 = 用组件的整张清单
```

`u60-ship.sh stage <事务号>` 拒绝（退出码 1，正式位置和状态文件都不动，旁路文件清掉）的情况：

- 事务号、组件名、提交号、时间、格式版本、试跑秒数、md5 任何一个格式不对；meta 里有看不懂的键；少了某个正式文件的 `file=`；
- 组件未知、`C_NOTREADY`；状态文件不在清单里、不是普通文件、路径越界（只允许 `/data/…` 下和我们 4 个 init 脚本，
  拒绝 `..`、`//`、空格、`|` 等字符）；
- 产物 md5 不对（上传没完成）、正式文件不存在（第一次安装走装机包；guard 例外：目录在、文件原来没有 = 新文件）；
  脚本产物（`*.sh`、`*.init`、`/etc/init.d/*`）`sh -n` 不过；
- 目录产物：tgz 的 md5 不对、包里有链接 / 特殊文件 / 绝对路径 / `..`（busybox tar 会自己去掉开头的 `../` 并只在标准错误说一句，这一句也算）、
  解开以后指纹不对、正式目录不在或算不了指纹；要写 v=2 而设备上的 u60-recover.sh 读不了 v=2；
- 有事务在进行（非终态）或停在「failed」；datad-trial 的守护在跑；另一个 stage 拿着 flock；
- `/data` 可用 < 200 MB；
- 格式版本和设备上的不同：设备上的版本 = 清单里这个组件最后一条 ship 记录的 `format`，没有记录按 1 算。
  **高了**按 R14 拒绝（要单独写迁移和退回方案）；**低了**也拒绝（报告没覆盖，按「宁可中止」处理：旧程序读新格式的数据不可预期）。
- 上一次事务停在「清单待补」时，stage 先补完清单（`manifest-finish`）再往下查。

通过后：产物拷到 `<正式文件>.test`（一律 `chmod 755`，不看上传时的权限）→ sync → 核 md5 → 删暂存副本 → 写事务日志 `phase=staged`。

## 事务日志 `/data/luyaz/ship/txn`

`key=value`，一行一个；`file=`、`state=` 可重复，字段用 `|` 分开（路径里不允许 `|`）：

```
v=1
txn=20260930-143000-datad
comp=datad
phase=check
reason=
boot_id=0b7c…（stage 时的 /proc/sys/kernel/random/boot_id）
commit=7e8d223
mac_time=1790000000
format=1
trial=180
check=120
exec_pid=1234
t_stage=5321                    stage 时的 uptime（秒）
t_phase=5530                    这次改 phase 时的 uptime
file=/data/luyaz/luyaz-core|<旧 md5>|<新 md5>
statelist=/data/zwrt-datad/cooling.conf
statelist=/data/zwrt-datad/neighbor.json
state=/data/zwrt-datad/cooling.conf|<旧 md5>
state=/data/zwrt-datad/neighbor.json|-          （- 表示原来不存在，退回时删掉）
end=1                           必须是最后一行：没有它就算日志不完整
```

- `end=1` 缺了（写到一半、被截断）：u60-ship.sh 当作看不懂（挡住 stage），u60-recover.sh 不动文件、只记一行。

- 旁路名、上一版的名字**不写进日志**，由正式路径推出来：`<路径>.test`、`<路径>.prev-<事务号>`（u60-recover.sh 不信日志里的任意路径）。
- `statelist=<路径>`（可重复）是 stage 时定下的状态文件清单（只有路径）；`state=` 在停掉正式版、把状态文件拷成 `.prev-<事务号>` 并 sync 之后才写进日志；那之前断电不需要还原状态文件（正式版停着，文件没动）。
- `reason` 是给人看的一句话（去掉换行和 `|`）。

### 阶段

| phase | 中文 | 终态 | 挡住下一次 stage |
|---|---|---|---|
| `staged` | 已暂存 | 否 | 是 |
| `trial` | 试跑中 | 否 | 是 |
| `promote` | 转正中 | 否 | 是 |
| `check` | 检查中 | 否 | 是 |
| `manifest` | 写清单中 | 否 | 是 |
| `rollback` | 退回中 | 否 | 是 |
| `done` | 完成 | 是 | 否 |
| `rolledback` | 已退回 | 是 | 否 |
| `aborted` | 已中止 | 是 | 否 |
| `manifest_pending` | 清单待补 | 是（文件已是新版且通过检查） | 否：stage 先补完清单 |
| `failed` | 要人处理（杀不掉测试版、退回时 .prev 缺失或不对） | 是 | **是**：先看 ship.log 手工处理，再把日志改成终态或删掉 |

`failed` 是定稿表之外新加的终态：退回做不完时不能假装「已退回」，也不能让下一次 ship 在半退回的设备上继续。

```
staged → trial → promote → check → manifest → done
           │        │         │        └─ 清单写不上 → manifest_pending
           │        └────┬────┘→ 不过 / 执行器意外退出 → rollback → rolledback（或 failed）
           └─ 不过 → 杀测试版、还原状态文件、起正式（旧）→ aborted（杀不掉 → failed）
```

落盘顺序：每次阶段转换先写日志（临时文件 → sync → mv → sync）再动文件；转正每个文件：cp 正式 → `.prev-<事务号>` → sync →
核 `.prev` 的 md5 = 旧 md5 → 核旁路的 md5 = 新 md5 → mv 旁路 → 正式 → sync。退回每个文件：正式的 md5 已是旧的就跳过；
否则 `.prev` 的 md5 必须等于旧 md5，才 cp 到同目录临时名 → sync → mv → sync；`.prev` 不对就不碰正式文件、记下来、结果为 `failed`。
原来不存在的状态文件（`-`）退回时删掉。

### 断电后开机（u60-recover.sh）和同一次开机的兜底（recover-live）

| 断电时的阶段 | 开机时 u60-recover.sh | 同一次开机 `u60-ship.sh recover-live` |
|---|---|---|
| `staged`、`trial` | 程序不动；状态文件换回试跑前的 `.prev`（R14，用户 10-01 定；同样按 md5，换不回 → `failed`）→ `aborted`，reason「开机时已中止（断电时在 X…）」 | 杀测试版、还原状态文件（只在 trial）、起正式版 → `aborted` |
| `promote`、`check`、`rollback` | 程序和状态文件全部换回 `.prev`（按 md5，已是旧版就跳过，可重复执行）→ `rolledback`，reason「开机时已退回」；有换不回的 → `failed` | 停服务、同左换回、起服务 → `rolledback` / `failed` |
| `manifest` | 不动文件 → `manifest_pending` | 补完清单 → `done` |
| 终态 | 什么都不做（≤ 1 秒，不写任何文件） | 什么都不做 |

第二期组件的 recover-live 照同一张表，停 / 起服务用组件自己的钩子：touch、uid 的「起」是 `uid-restart`（试跑中的 touch 先杀测试版）；
（8d-2c 删掉的 web 没有停 / 起，只换目录）；guard 整组换回并重启 guard（guard 每轮的兜底不对 guard 自己的事务起 recover-live，
由 `luyaz status` 调）。

u60-recover.sh 只在「日志里的开机号 ≠ 现在的开机号」时动手（同一次开机归执行器和 guard）；日志的 `v` 看不懂、没有 `end=1`、
字段不合格式、路径越界、程序文件被标成「原来不存在」时一律不动文件（整份日志里只要有一行不对就全部不动），只在 recover.log 记一行。它不做算术、不 `.` 任何文件、不联网。

## 执行器心跳 `/tmp/luyaz/ship/heartbeat`（给 T4 的 guard 兜底用）

一行：`<uptime 秒> <执行器 pid> <事务号> <phase>`。试跑时每秒写一次，其余阶段至少每 5 秒一次（执行器里所有 sleep 都经过心跳）。

**每一步的超时（T4）**：执行器里会卡住的命令（init.d stop/start、sync）都经 `step` 放到后台跑，前台每 0.2 秒看一眼、每秒写一次心跳；
init.d 超过 25 秒就 kill -9 并当作失败（之后照常按「停不下来 / 起不来」处理）。sync 不设超时、只保持心跳——放弃一个没做完的 sync
会破坏「先落盘再改名」的顺序。curl 自带 `-m`，等端口、等进程退出的循环都用带心跳的 sleep。所以执行器正常干活时心跳不会超过 30 秒。

**guard 兜底（`u60-guard.sh` 的 `ship_round`，每轮在子 shell 里）**：事务日志在 staged/trial/promote/check/manifest/rollback，
而且心跳超过 30 秒、或心跳里的 pid 已不是 `u60-ship.sh`、或没有心跳 → 在后台起 `sh /data/u60-guard/u60-ship.sh recover-live`
（关掉 7、8、9 号 fd，日志进 guard 的日志）。例外：组件是 guard 自己时不管；`staged` 且还没有心跳、暂存不到 300 秒时不管（Mac 马上会 start）；
设备上没有 u60-ship.sh 时不管。guard 一轮约 60 秒，所以执行器被杀后最迟约 90 秒有人接手。
`recover-live` 自己再查一遍：执行器还活着（pid 在、cmdline 里有 `u60-ship.sh`）且心跳 ≤ 30 秒 → 退出码 3、不动；`--force` 先 kill -9 执行器再做。
它干活时也写心跳，所以下一轮 guard 不会再起第二个。

## 命令（u60-ship.sh）

| 命令 | 作用 | 退出码 |
|---|---|---|
| `stage <事务号>` | 核对暂存目录（上面的拒绝条件），放好旁路文件，写 `staged` | 0 / 1 拒绝 |
| `start <事务号>` | 脱离 ssh 起执行器（`run`），5 秒内看到心跳才算起来；没起来 → `aborted` | 0 / 1 |
| `run <事务号>` | 执行器本身（前台；测试直接调） | 0 完成 / 1 退回或中止 / 2 清单待补 |
| `status` | `key=value`：phase、txn、comp、reason、hb_age、exec_alive、observe_left（完成后 1 小时观察期还剩几秒） | 0 |
| `wait <秒>` | 等到终态或超时，然后同 `status` | 0 终态 / 4 超时 |
| `abort` | 给执行器发 TERM（它按阶段自己收尾） | 0 / 1 没有执行器 |
| `recover-live [--force]` | 见上 | 0 / 3 执行器还活着 |
| `manifest-finish` | 把 `manifest_pending`（或停在 `manifest` 且执行器已死）补成 `done` | 0 / 1 |
| `print-launch datad\|touch\|lweb` | 打印试跑会启动的命令（测试拿它和 init 脚本比） | 0 |
| `record <名\|all> <Mac 时间> [原因]` | 只记录的文件按现在的 md5 记一行 | 0 / 1 拒绝 |
| `record-kit <组件> <装机包日期> <提交号> <格式> <Mac 时间> [dirty]` | 装机包装完写一条 `kind=kit`；有事务在进行时拒绝 | 0 / 1 |
| `prepare-rollback <组件> <事务号> <Mac 时间>` | 把组件最后一次上机的 `.prev-<那次事务号>` 拷进 `stage/<事务号>/` 并写好 meta（提交号、格式取再上一次上机的；没有就是 `0000000`、现在的格式；`note=rollback`），之后照常 stage / start：退回也走试跑、转正、检查 | 0 / 1 拒绝 |
| `selftest` | 在 /tmp 的沙盒里用 `selftest` 组件把完整事务走一遍（通过、检查不过退回），再查需要的 applet | 0 PASS / 1 |

## 清单 `/data/luyaz/ship/manifest.jsonl`

一行一个 JSON 对象，只有受控字段（十六进制、数字、已检查过的路径），不含密钥：

```json
{"v":1,"kind":"ship","comp":"core","txn":"20260930-143000-core","commit":"7e8d223","format":1,"mac_time":1790000000,"boot_id":"0b7c…","uptime":5600,"files":[{"path":"/data/luyaz/luyaz-core","md5":"…","prev":"/data/luyaz/luyaz-core.prev-20260930-143000-core"}],"state":["/data/zwrt-datad/cooling.conf"]}
```

- 同一组件以最后一条 `kind=ship` 或 `kind=kit` 为设备上的现状（格式版本、doctor 的比对、luyaz 的依赖检查都这样算）。
- **装机包**（T9）：`onboard/device/install.sh` 装完 admin 后调 `u60-ship.sh record-kit <组件> <装机包日期> <提交号> <格式版本> <Mac 时间> [dirty]`：
  ```json
  {"v":1,"kind":"kit","comp":"core","kit":"20261001","commit":"abc1234","format":1,"mac_time":1790000000,"boot_id":"…","uptime":80,"files":[{"path":"/data/luyaz/luyaz-core","md5":"…"}],"state":[],"note":"kit-dirty"}
  ```
  没有 `prev`：装机包不留上一版，所以最后一条是 kit 时 `prepare-rollback` 拒绝。`note:"kit-dirty"` = 打包时那个仓库有未提交改动。
- **只记录的文件**（R1/D10）：`u60-ship.sh record <名|all> <Mac 时间> [原因]` 写 `kind=record` 行，同一名字以最后一条为准：
  ```json
  {"v":1,"kind":"record","name":"rc.local","path":"/etc/rc.local","md5":"…","mac_time":1790000000,"boot_id":"…","uptime":5600,"why":"手工改了"}
  ```
  名字固定：`tailscale-start.sh`、`tuning.env`、`tailscaled`、`tailscaled-nofight`；
  `init.d/<核心>`（迁移后 `init.d/luyaz-core`，旧布局 `init.d/zwrt-datad`）、`init.d/tailscale`；`rc.local`；`luyaz-recover.sh`；目录 `ui-fonts`（`/data/luyaz/ui/fonts`）、`ui-logos`、`core-plugins`（Luya-Z D4c 前叫 `u60-recover.sh`、`devui-fonts`、`devui-logos`、`u60d-plugins`：清单里的旧名照样算，`record` 也认旧名、写新名）。
  `init.d/zte-agent`、`init.d/u60-guard`、`init.d/u60-uid` 已由 `retire` 行退役（10c）。文件不存在记 `"md5":"-"`。
  运行中会被正常改写的设置文件不进清单。原因去掉引号、反斜杠和控制字符，最长 120 字节。有事务在进行时拒绝。
  doctor.sh 里有同一张名字表，测试检查两边一致。
- 只有 `done` 才写一条；试跑不过、转正后退回，清单都不变。
- 写入：旧内容 + 新的一行 → 同目录临时文件 → sync → mv → sync；写不上 → 事务停在 `manifest_pending`，`run` 退出码 2。
- 补清单是幂等的：清单里已经有这个事务号的行就不再加。
- 时间用 Mac 传来的 `mac_time`（设备时钟是当地时间标成 UTC，而且刚开机可能没对上），另记设备的开机号和 uptime。

## doctor 的清单行（T6，R9/D11）

- 人看的 `doctor.sh` 第一行、`doctor.sh --tsv` 最后一行：`<ok|warn>\tmanifest\t清单\t<说明>`，其余各行不变。说明按先后：
  | 情况 | 级别 | 说明 |
  |---|---|---|
  | 事务在进行、执行器活着 | ok | 正在上机：X（阶段） |
  | 事务停在半路、执行器没了 | warn | 上次上机停在半路：X（阶段），guard 或 luyaz status 会收尾 |
  | `failed` | warn | 上次上机停在半路，要人处理：X（原因） |
  | `manifest_pending` | warn | 清单待补：X 已转正并通过检查，清单还没写上 |
  | 没有清单文件 | ok | 还没有清单 |
  | 清单里一条能用的都没有 | warn | 清单读不懂 |
  | 有文件和清单不同 | warn | 不一致的是 <文件名>（清单 <md5 前 8 位>，实际 <md5 前 8 位>）[ 等 N 项] |
  | 刚完成、同一次开机、不到 1 小时 | ok | 观察中（还剩 N 分钟）：X 刚上机；一致（N 项） |
  | 其余 | ok | 一致（N 项）[；上次上机已退回 / 已中止：X（原因）] |
- `doctor.sh --manifest`：每条一行 `<same|differs|unrecorded>\t<comp|record>:<名>\t<路径>\t<清单 md5>\t<实际 md5>`，最后一行 `state\t<级别>\t<说明>`。
- md5 缓存在 `/tmp/luyaz/doctor/md5`，键是路径 + 大小 + inode + 修改/变更时间（到纳秒）；没变不重算（28 MB 文件第二次 `--tsv` ≤ 2 秒，测试里量）。

## 第二期（T12–T16）的约定（10-01 定，实现照这里写）

### 组件的内容

| 组件 | 正式内容 | 来源（luyaz 在提交 X 的 worktree 里） | 方式 | 试跑 / 检查 | txn `v` |
|---|---|---|---|---|---|
| `touch` | `/data/luyaz/ui/luyaz-ui`、`/data/plugins/u60pro-devui/start.sh` | `rs/build.sh device` 编的 `luyaz-ui`（C 版和 `scripts/build-docker.sh` 10-15 删除，tag `legacy-c-touch-ui`；没有 `rs/` 的提交 luyaz 拒绝）；`scripts/start.sh` | 旁路顶替 | 60 s / 120 s | 1 |
| `uid`（退役） | `/data/plugins/u60pro-devui/u60-uid` | 原 C 版 `out/u60-uid`（已删） | direct | — / 120 s | 1 |
| `guard` | `/data/u60-guard/` 下 `GUARD_FILES` 每个文件 + `/etc/init.d/<名>`（`GUARD_INITS`） | `git show X:scripts/<名>`；init.d 的 = `scripts/<名>.init` | direct，停止标记 + 只重启 guard | — / 300 s | 2（有 `/etc` 路径，总是） |

- （guard 已退役，下面是它的历史约定；现在的脚本走 `scripts` 组件的 `SCRIPTS_FILES` + `SCRIPTS_INITS`。）`GUARD_FILES`（u60-ship.sh 里一张表，build-kit 的 guard 清单去掉下面三个后和它一致，测试比对）：`alert-lib.sh u60-guard.sh
  agent-auth.sh chaos.sh doctor.sh doctor-legacy.sh config-backup.sh wan-sources.sh u60-fallback.sh zte-agent.init zwrt-datad.init u60-guard.init`（`u60-fallback.sh` 是 E4 的应急直写脚本，触屏在核心连不上时调 `/data/u60-guard/u60-fallback.sh`），
  加 `/etc/init.d/` 下的 `GUARD_INITS="u60-guard zte-agent zwrt-datad u60-uid"`（内容 = 同一提交的 `scripts/<名>.init`；u60-ship.sh 和 luyaz 各一张同样的表，
  上传顺序 = `GUARD_FILES` 再 `GUARD_INITS`）。设备目录里其余文件（日志、账本、`lan-ipv6-off`、`standby.baseline`、手工备份）一律不碰。
  从清单里拿掉的文件 ship 不会删（10-06 拿掉了耗电测试用的 `power-sample.sh`、`wifi-ab.sh`），留在设备上没人调用，要删手工删。
- **init 脚本归 guard**（10-04 起；之前只换 `/etc/init.d/u60-guard`，另外三个改了要手工换再 `luyaz record`）：agent、datad、uid 带不了自己的——
  产物按正式文件的文件名命名，`/data/zte-agent` 和 `/etc/init.d/zte-agent` 同名（datad、uid 一样）。guard 只重启 guard：
  新的 `zte-agent`、`zwrt-datad`、`u60-uid` 脚本要等那个服务下次启动（ship 那个组件、手工 restart 或重启设备）才生效；
  luyaz ship guard 会逐个说出哪几个会换。原来没有的 init 脚本照「新文件」处理（`-`，退回时删掉）；没被 procd 启用的脚本放进去也不会开机自启。
- **清单的只记录条目跟着更新**：事务的文件里有只记录名单上的（就是这 4 个 `init.d/<名>`），`done` 时同一次写清单在 ship 行**前面**各写一条
  `kind=record`（md5 = 新的，`why` = `ship <事务号>`），doctor 不再报它们「不一致」；退回、中止不写；`luyaz rollback guard` 本身是一次 ship，写回旧的 md5。
- **谁管哪个脚本**：`u60-ship.sh`、`datad-trial.sh` 只随每次 ship 上传（K4），不进 guard 组件，guard 退回也不会把它们换回旧版；
  `u60-recover.sh` 只由 `install-recover` 换（先问用户）。
- **字体 `fonts/`、运营商 logo `operator-logos/` 不进 touch**：它们不是 touch-ui 某个提交的产物（字体是钉死 sha256 的公开下载，logo 由 manager 的 SVG 生成），
  按 R1 作只记录的目录条目：`record` 名字 `ui-fonts`、`ui-logos`，记目录指纹。换它们照旧走装机包。
- **核心 luyaz-core 的策略组件 `/data/luyaz/core/plugins/`（旧布局 `/data/u60d/plugins/`）**（manager `crates/luyaz-plugins/build.sh` 编的 `plugins.json` + `.cwasm`）也是只记录的目录条目 `core-plugins`：
  手工放好（暂存目录 + `mv`）后 `luyaz record core-plugins`；核心只在启动时读，放好后要等下一次 ship datad 或重启核心才生效；清单或 sha256 不对就用内置实现。
- 旧做法的 `ui/` 模板目录现在的触屏程序不读，不进任何组件。

### 上机顺序（第一次用第二期之前）

1. `luyaz install-recover`（先问用户）：之后 guard（txn v=2）才能 stage；
2. ship guard：带上新的 doctor.sh（认目录条目、`key.log.0`）。设备上旧的 doctor 把 `"tree"` 条目读成空 md5，第一行会报 warn；
3. 之后才 `luyaz record ui-fonts`、`ui-logos`（或 `record all`），以及 ship touch、uid。

### 目录指纹（只记录的目录，以及 8d-2c 删掉的 web；Mac 和设备算法一样）

8d-2c 起没有组件再用 `dir=` / `C_DIRS`（只有 web 用过）：机制留着，旧的 v=2 日志照样能读、能收尾（u60-recover 的测试覆盖）。


`cd <目录> && find . -type f` → 去掉开头 `./` → `LC_ALL=C sort` → 每个文件一行 `<md5> <相对路径>`（一个空格）→ 整段（每行带换行）的 md5。
目录里有普通文件和目录以外的东西（链接、设备文件）、或者文件名带换行、`|`，算失败（stage 拒绝、doctor 报 warn）。空目录 = 空串的 md5。
两边各有一个用例用同一个夹具（`scripts/test/fixtures/tree/` 和它的期望值）对答案：Mac 端 bash 3.2 + `md5 -q`、设备 busybox `md5sum`。
期望值在 `scripts/test/fixtures/tree.expected`（`4cf3547a1a7205c976ec74f0dd968146`），被算 md5 的原文在 `tree.listing`（对不上时 diff 它）。
设备上三个脚本各有入口：`u60-ship.sh tree-fp <目录>`、`u60-recover.sh tree-fp <目录>`、`doctor.sh --tree-fp <目录>`（算不了退出 1、不打印）。

### 暂存和 meta（新增的行）

```
dir=admin <目录指纹> <tgz 的 md5>      产物是 stage/<事务号>/admin.tgz（gzip tar，COPYFILE_DISABLE=1，路径不带 ./ 之外的前缀）
```

stage 对 `dir=`：核 tgz 的 md5 → `rm -rf <正式目录>.test` → 解到 `<正式目录>.test` → 核目录指纹 → sync → 删 tgz。正式目录不存在 → 拒绝（第一次安装走装机包）。
guard 的新文件（正式位置原来没有）照常 `file=`，stage 记旧 md5 为 `-`。
产物文件名照旧 = 正式文件的文件名：`/etc/init.d/u60-guard` 的产物叫 `u60-guard`（内容 = `scripts/u60-guard.init`，和 `/data/u60-guard/u60-guard.init` 一样），
meta 里是 `file=u60-guard <md5>`；`zte-agent`、`zwrt-datad`、`u60-uid` 同理。meta 的 `v` 仍是 1（`dir=` 是新加的键，不升 meta 版本）。

### 事务日志 v=2

v=1 的所有规则不变，另加：

```
v=2
dir=/data/admin|<旧指纹>|<新指纹>
file=/data/u60-guard/<新文件>|-|<新 md5>     程序文件原来不存在（v=1 不允许）：退回时删掉
file=/etc/init.d/u60-guard|<旧>|<新>
```

- **只有用到上面任何一项时才写 v=2**；datad、agent、touch、uid 照旧 v=1，已装的 u60-recover.sh（cabc78d3，只懂 v=1）照常处理，
  看到 v=2 一律不动（它本来就这样）。
- **stage 前置**：要写 v=2 时，设备上的 `/data/luyaz/ship/luyaz-recover.sh formats` 必须打印含 `2` 的一行；旧版没有 `formats` 命令 → 拒绝，
  原因「先装新版 u60-recover.sh（luyaz install-recover，要用户同意）」。
- **`/etc/` 下文件的上一版不放在 `/etc/init.d`**（免得多一个看起来像服务的文件）：`/etc/init.d/u60-guard`（其余 3 个同理）的上一版是
  `/data/luyaz/ship/prev/etc.init.d.u60-guard.prev-<事务号>`。规则写死在两个脚本里：`/etc/init.d/<名>` → `$SHIP_DIR/prev/etc.init.d.<名>.prev-<事务号>`，
  其余路径照旧 `<路径>.prev-<事务号>`。
- **目录转正**：`mv <正式> <正式>.prev-<事务号>` → sync → `mv <正式>.test <正式>` → sync（两次改名之间断电 = 没有正式目录，按日志退回）。
- **目录退回**（幂等，可重复）：`<正式>` 的指纹已是旧的 → 跳过；否则 `<正式>.prev-<事务号>` 的指纹必须是旧的（不对 → 不碰，`failed`）→
  `rm -rf <正式>.rec-tmp` → `cp -a` 上一版到 `<正式>.rec-tmp` → sync → 核指纹 → `<正式>` 在就 `mv` 成 `<正式>.bad-<事务号>` →
  `mv <正式>.rec-tmp <正式>` → sync → `rm -rf <正式>.bad-<事务号>`。
- `.prev` 清理对目录同样只留 ship 生成的最近 3 份（只认事务号格式的名字；`admin.prev-*.tgz`、`admin.old-*` 这些手工备份不碰）。


### 事务日志 v=3（哨兵，manager docs/designs/u60-sentinel.md 5d-0）

v=2 的所有规则不变，另外只有 v=3 的日志可以写这两个文件（v=1、v=2 的日志里出现 → u60-recover.sh 一律不动）：

```
v=3
file=/etc/init.d/luyaz-sentinel|-|<新 md5>
file=/etc/rc.local|<旧>|<新>
```

- `/etc/rc.local` 的上一版是 `/data/luyaz/ship/prev/etc.rc.local.prev-<事务号>`；`/etc/init.d/luyaz-sentinel` 同 init 脚本规则。
- **stage 前置**：写 v=3 时设备上 `u60-recover.sh formats` 必须含 `3`（10-10 起的版本打印 `1 2 3`）；装新版走 `luyaz install-recover`（要用户同意）。
- 写 v=3 的：带 rc.local 的 sentinel 事务（转换）、scripts（带哨兵的启动脚本）。

### 事务日志 v=4（luyaz-web，manager docs/designs/u60-web.md 第 6 步）

v=3 的所有规则不变，另外只有 v=4 的日志可以写 `/etc/init.d/luyaz-web`（v=1～3 的日志里出现 → u60-recover.sh 一律不动）。
- **stage 前置**：写 v=4 时设备上 `u60-recover.sh formats` 必须含 `4`（10-12 起的版本打印 `1 2 3 4`）；装新版走 `luyaz install-recover`（要用户同意）。
- 写 v=4 的：scripts（带 luyaz-web 的启动脚本）。所以 10-12 以后的 **每一次 `ship scripts` 都要设备上先有 formats 4 的 u60-recover.sh**。
- 日志版本取所有条目要的最高值（`txn_v`；之前新文件、`/etc` 路径会把已经算出的 3 压回 2，带哨兵 init 时只是因为它排在最后才没出事）。

### luyaz-web（`lweb`，manager docs/designs/u60-web.md 第 6 步）

**第一次的顺序**（每步都先问用户）：`luyaz install-recover`（formats 要有 4）→ `luyaz ship scripts`（装 `/etc/init.d/luyaz-web`；没有程序时它拒绝启动，所以放着没事）→
`luyaz ship lweb`（`ship/lweb/requires` 里那行 `agent 0bab76b8` 8d-2c 起不再查：`luyaz` 跳过已删除组件的依赖，两个方向都跳，在 stderr 说一句）→ 手工在 rc.local 里 datad 那行后面加
`/etc/init.d/luyaz-web start`（在 u60-recover.sh 那行之后；`luyaz` 的前置检查和 `after-upgrade` 都认这一行）。之后升级只要 `luyaz ship lweb`。

- **init**：procd 直接起 `/data/luyaz/luyaz-web serve --bind 0.0.0.0:9090 --upstream http://127.0.0.1:9460 --password-file /data/zte-agent.env`（luyaz-web 先读 `/data/luyaz/web.env`，迁移后密码在那里；supervise.sh 10-15 退役），
  respawn 300 5 5、term_timeout 10；日志 `/tmp/luyaz-web.log`（哨兵按 256 KB 截），崩溃留 `/data/crashlog/luyaz-web/`，告警类别 `web-crash`（luyaz-alerts 里有它的说法）。
- **试跑**：和 datad 一样旁路顶替（引擎试跑前本来就停正式版，换端口没有好处）：停正式版 → `nohup /data/luyaz/luyaz-web.test serve …`（参数同 init，
  `print-launch lweb`；测试比对 init 的那一行）→ 每 10 s：进程在、`/health` 200 且有 `link`（没人看时是 `idle`，不要求 `live`）、`/login` 200 且有内容（luyaz-web 自己渲染、不要会话的页）、`/proc/net/tcp` 里有 `00000000:1F90` 在听
  （init 丢了 `--bind` 时只听本机，本机 curl 照样通）。**检查**再加：同一个 pid、跑的是这一版。
- **第一次上机没有旧版**：stop 没东西停；start 看到没有程序就不起、算成功，所以试跑不过 → aborted、转正后起不来 → rolledback，都回到「没有 luyaz-web」，不会 failed。
- **stop 总是走 init**（没在跑也调）：新版在两次 respawn 之间时 pidof 是空的，但 procd 实例还在；第一次上机退回时不停它，procd 会一直拉一个已删掉的程序。
- 没有 `/etc/init.d/luyaz-web` 时 stage 拒绝（先 ship scripts）。`luyaz rollback lweb` 和其它组件一样；**第一次上机之后的 rollback 不删程序**（ship 不删程序，「上一版」就是现在这版，等于空事务），
  真要去掉：问过用户后 `/etc/init.d/luyaz-web stop`、删 rc.local 那一行。
- doctor：程序在或 rc.local 有它那一行时查 `svc-luyaz-web`（程序在、在跑、rc.local 没那一行 = 警告「重启后不会起来」）。

### 哨兵（manager docs/designs/u60-sentinel.md 5f，T7；Codex 10-10 复核过设计）

**5f 的顺序**：`luyaz install-recover`（formats 要有 3）→ `luyaz ship scripts`（装 `/etc/init.d/luyaz-sentinel`）→ `luyaz ship sentinel --convert`（都先问用户）。之后升级哨兵只要 `luyaz ship sentinel`（只换程序，rc.local 不动）。

- **rc.local 只认两种样子**（`rc_mode`）：旧（恰好一行 `/etc/init.d/u60-uid start`，紧跟恰好一行 `/etc/init.d/u60-guard start`，没有哨兵行）或已转换（恰好一行 `/etc/init.d/luyaz-sentinel start`，两行旧的都没有）；两种都要 `u60-recover.sh` 那行恰好一次、在前面，`zwrt-datad start` 在屏幕主人之前，`exit 0` 在后面，三个名字不能在别处（条件、复合命令）出现。行尾 `   # 注释` 可以。其余一律拒绝。
- **转换在设备上改**（`rc_derive`）：u60-uid 那行换成哨兵行、u60-guard 那行删掉、恢复那行（新旧路径都认）变成 `sh /data/luyaz/ship/luyaz-recover.sh; [ $? = 10 ] && exec /bin/sh /etc/rc.local …`，其余字节不动；改前改后都 `sh -n`，改法记进 ship.log。旧 rc.local 要 `--convert` 才改（meta `note=convert`），已转换的再带 `--convert` 拒绝。
- **开机恢复退回 rc.local 时返回 10**：正在跑的 shell 拿着新 rc.local，不退出会接着起一个已经没有的哨兵；新 rc.local 那一行看到 10 就重新执行换回的旧文件（旧文件不认 10，不会绕圈）。
- **停**：先杀影子哨兵（`--shadow`），再停哨兵（init stop，5 s 后 TERM），再 u60-uid stop（界面留在屏上），再 guard stop；任何一个停不下就失败。**起**：按盘上的文件决定起哪一套（rc.local 是哨兵的样子且 init 和程序都在 → 起哨兵并等它接管界面；否则 u60-uid 再 guard），**还有屏幕主人在跑就拒绝起**（不会有两个主人；停失败的回滚停在那里，人来处理）。
- **检查 300 s**：同一个哨兵进程、跑的是这一版；心跳至少动 2 次、从不超过 150 s 不动；u60-uid 不能出现；屏幕空不超过 20 s；**核心的判定**（`/data/luyaz/ship/verdict`，行首是本事务号）是 fail 立刻回滚，窗口结束时不是 ok 也回滚（D5：没有判定也不转正）。
- **D4 一次只换一个**：暂存 datad 或 sentinel 时，按清单历史看另一个上次上机是否还在 1 小时观察期里（同一次开机），在就拒绝。
- **哨兵管屏幕时 ship touch**：不停哨兵，往 `/tmp/luyaz-screen.ctl`（10-15 起唯一的 FIFO；旧 `/tmp/u60-uid.ctl` 不再听）写 `hold <试跑+检查+300 s>`（5 s 写不进算失败），等心跳第 3 栏变成 `held` 再杀界面；完了写 `release`，等哨兵拉起这一版界面、心跳回到 `ours`。检查窗口结束时也要核心判定 ok。
### 清单

目录条目：`{"path":"/data/admin","tree":"<指纹>","prev":"/data/admin.prev-<事务号>"}`（没有 `md5`）。doctor `--manifest` 对它算指纹比较（不走 md5 缓存，
每次现算）。只记录的目录：`{"v":1,"kind":"record","name":"ui-fonts","path":"/data/luyaz/ui/fonts","tree":"…",…}`。

### u60-recover.sh（新版）

- 多认 v=2（上面的目录、`-` 程序文件、`/etc/init.d` 上一版的位置）；`sh u60-recover.sh formats` 打印 `1 2`。
- 开机时它在所有服务之前跑，所以只换文件、不重启任何服务（和现在一样）。
- 换它：`u60-ship.sh install-recover <事务号>`（事务号以 `-recover` 结尾；暂存目录 `stage/<事务号>/` 里放 `u60-recover.sh` 和 `meta`：
  `v=1`、`txn=`、`comp=recover`、`commit=`、`mac_time=`、`file=u60-recover.sh <md5>`，别的键都拒绝）：`sh -n` → 用暂存的那份跑 `selftest` → 临时名 → sync → mv → sync →
  写 `kind=record` 名 `luyaz-recover.sh`（D4c 前叫 `u60-recover.sh`）。有事务在进行时拒绝。Mac 端 `luyaz install-recover [--commit X]`，每次都先问用户。

### 各组件的钩子要点

- **touch**（T12）：stop = 让屏幕主人放手（哨兵：`hold`，见上；只有旧设备上才是 `u60-uid stop`），再按名字杀正式界面 `luyaz-ui`（`pidof`，绝不 `pgrep -f`）并等它真的退出（两个界面抢 `/dev/dri/card0` = 整机重启）；
  清 `/data/luyaz/screen/` 和 `/data/u60-uid/` 的 `attempts`、`gave-up`。launch_test = 和哨兵起界面一样（`chdir` 到界面目录再 exec；命令见 `print-launch touch`：
  `nohup sh -c 'cd "$1" && exec "$2"' sh /data/luyaz/ui /data/luyaz/ui/luyaz-ui.test`）起 `luyaz-ui.test`（`pidof luyaz-ui` 和 `pidof luyaz-ui.test` 互不认对方）。
  trial_watch：前 30 秒每秒查、之后每 2 秒；测试版没了 → 立刻 `uid-restart`（正式版），
  屏幕空着的总时长（正式界面退出 → 下一个界面进程出现）超过 20 秒也算失败。start = `uid-restart`（核 exe md5 = 新版）。check：屏幕主人和界面在、
  屏幕日志没有放弃 / 交还原厂 / 界面意外退出、界面进程的 exe md5 不变（没被重启过）。
- **uid-restart**（R8/D9，命令 `u60-ship.sh uid-restart [<界面 md5> [<u60-uid md5>]]`，装机包也调；哨兵在跑时 = 写 `release` 让哨兵拉起界面并核 md5，下面的 u60-uid 流程只在旧设备上走；不给 md5 = 用磁盘上正式文件
  `/data/luyaz/ui/luyaz-ui`、`/data/plugins/u60pro-devui/u60-uid` 现在的 md5；退出码 0 好了 / 1 没好（原因打印出来）；有事务在进行时拒绝）：u60-uid 在跑就 `stop` → 清 attempts、gave-up →
  `start` → 等两个进程都在且 `/proc/<pid>/exe` 的 md5 等于给出的值（10 秒）→ 不对就再 `start` 一次 → 还不对退出 1。
- **uid**（T13）：stop = `u60-uid stop`；start = `uid-restart`；check 同 touch。退回只用替身测试。
- **guard**（T15，已退役，历史）：转正前用**旧** doctor 出一份 `--tsv`（跑不完 = 不转正，中止）；stop = 写 `/tmp/u60-guard/stop-requested` 并 `/etc/init.d/u60-guard stop`；
  start = `/etc/init.d/u60-guard start`；check 300 秒：进程在（`/proc/*/cmdline` 正好是 `/bin/sh /data/u60-guard/u60-guard.sh`）、
  只有一条 kmsg 落盘管道（正好是 `cat /dev/kmsg` 的进程只有一个；它外面那层 `sh -c` 不算）、`u60-guard.sh` 自检 PASS
  （u60-guard.sh 没有自检命令：做法是第一个 10 秒后对每个上机的脚本 `sh -n`，stage 时也先做一遍）、`doctor.sh --ledger-selftest` PASS、
  每个新文件 md5 对、新 doctor 的 `--tsv` 按行 id 比级别和旧的一样（排除 standby、clock、manifest 三行；旧的 ok 新的不 ok 才算变坏）。
  退回 = 整组换回再重启 guard。guard 自己的事务 guard 兜底不管（同一次开机执行器死了，由 `luyaz status` 调 recover-live 收尾）。
  `doctor --ledger-selftest` 的 key.log 原因码项改成连 `key.log.0` 一起查（10-01 的 P3，不修第一次 ship guard 就会自己退回）。

## 测试

- `scripts/test/u60-ship/run.sh`：stage 的每种拒绝、datad 正常上机、试跑不过、检查不过、清单写不上和补完、`.prev` 只留 3 份
  （夹具里放着真实的手工备份名）、每个断点 kill -9 后跑 u60-recover.sh 按 md5 断言、撕裂状态（0 字节正式文件、截断或不对的 `.prev`）、
  心跳间隔（试跑 ≤ 1 秒、其余 ≤ 5 秒）、recover-live、datad-trial 与事务互斥、每个用例后真实 `/data`、`/tmp/luyaz/ship` 没有被写。
- `scripts/test/u60-recover/run.sh`：正常开机三种情况 ≤ 1 秒且整棵目录逐字节不变、每个阶段的开机结果、看不懂的日志不动、重复执行结果一样、`selftest`。
- `scripts/test/datad-trial/run.sh`：原 218 个用例原样通过（8d-2c 删了 netwatch 那一组，换成「计数上升、没有 wan_status 都不中止」一条）。
- `scripts/test/u60-recover/run.sh` 另有 v=2：每个阶段、两次改名之间断电（没有正式目录）、目录还原中途断电（正式在 `.bad`、`.rec-tmp` 半截）、
  目录 `.prev` 不对或有链接、只认 `/data/admin`、v=1 里出现 v=2 的东西不动；**旧版 u60-recover.sh**（`scripts/test/fixtures/u60-recover.v1-2bb17b1.sh`，
  = 设备上的 cabc78d3）看到每个阶段的 v=2 日志都不动；`formats`。
- `scripts/test/u60-ship-p2/run.sh`（第二期）：
  - touch：正常上机（试跑心跳每秒、测试版从插件目录起、清启动计数）；测试版 5 秒内崩 → ≤ 1 秒拉起正式版；测试版一直不出现 → 屏幕空 20 秒判失败、
    正式版拉回；10 分钟内连上三次 u60-uid 不放弃（替身照 uid.c 计数，不清计数时第三次放弃作对照）；检查中界面被重启 / u60-uid 放弃 → 退回；
    新界面起不来 → 退回；测试版杀不掉 → failed 且不起正式版；旧界面杀不掉 → 中止；真 busybox `pidof` 两个名字互不认（真进程）；断电和 recover-live。
  - uid-restart：默认比磁盘上的 md5、给定 md5、版本不对 start 两次后退出 1、放弃后清掉再起、u60-uid 起来马上又没了（9-26）→ 再 start、有事务时拒绝。
  - uid：正常（界面被接管、没重启）、新 u60-uid 起不来、检查中 u60-uid 没了、断电、prepare-rollback。
  - web（T14）：8d-2c 随组件删掉。
  - guard：正常（新文件、`/etc/init.d` 的上一版在 u60-ship/prev、停止标记、300 秒检查）、doctor 按行 id 比（排除三行、只算 ok→不 ok、顺序和增减行不算）、
    旧 doctor 跑不完 → 中止且什么都没动、ledger 自检不过、两条 kmsg 管道、新 guard 起不来、检查中文件被改、旧版 u60-recover.sh 时拒绝、语法错误、
    断电（新旧 u60-recover.sh，含换 `zwrt-datad`、`u60-uid` init 时断电）、recover-live 整组换回并重启、prepare-rollback（新加的文件保持现状；
    上一次 ship 还没带另外 3 个 init 时它们保持现状；退回后 record 行是旧 md5）、record-kit（装机包没装的文件记 `-`）；
    4 个 init 都换、各有 u60-ship/prev 里的上一版、u60-uid 的 init.d 一次没调、清单 4 条 record 在 ship 行前；原来没有的 init 脚本放进去、退回时删掉；
    退回时清单不变。
  - u60-uid 日志轮转：检查中写了放弃再马上被 mv 成 `.old` → 照样退回（旧 `.old` 里本来有放弃行时也一样）；轮转换掉一份有旧放弃的 `.old` → 不误报；
    旧 `.old` 被删、再轮转两次 → 不误报。
  - install-recover：正常（换上、留 `.prev`、记清单、之后 v=2 能 stage）、md5 / 语法 / 自检 / meta / 有事务 / 事务号各种拒绝；record-kit touch、uid；旁路文件一律 755。
  - lweb：第一次上机（v=2、只 start 一次、试跑命令 = init、下次开机安静）、升级（stop start、留 `.prev`）、第一次试跑起不来 → aborted 且不留程序、
    测试版只听本机 → aborted 说原因、检查中被重启 → 退回、第一次转正后起不来 → 退回到没有且 procd 实例停掉、升级后 rollback 回旧版、
    第一次上机后 rollback 是空事务、没有 init → 拒绝、试跑命令和 `luyaz-web.init` 一致；
    scripts 带 luyaz-web 的 init 是 v=4、设备 u60-recover.sh 没有 formats 4 时拒绝。
- `scripts/test/tree-fp/run.sh`：三个脚本对夹具得 `tree.expected`，空目录、链接、带 `|` 或换行的名字、fifo 结果一致。
- `scripts/test/cross-repo/run.sh`（在主机上跑，容器里只有 /scripts 时跳过）：`GUARD_FILES` = manager `onboard/build-kit.sh` 的 guard 清单去掉
  u60-ship.sh、datad-trial.sh、u60-recover.sh（完全一致）；touch 试跑的启动方式 = src/uid.c 起界面的方式。
