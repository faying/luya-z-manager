# 可靠性契约：zte-agent 与 u60-guard 之间的文件约定

zte-agent（本仓库）与 u60-guard（touch-ui 仓库 `scripts/u60-guard.sh`，Wi-Fi 兜底看门狗兼告警发送方）
只通过下面这些文件互相了解。两边的代码都以本文件为准；改其中任何一条，两边要同时改。

## 1. Wi-Fi 锁 `/tmp/u60-wifi.lock`

所有写 `wireless` 这个 uci 包的程序，从第一次 `uci set` 到 reload 回验结束都要持有它（`flock` 排他锁）。

- agent：`wifi_radio::lock()`（进程内锁 + flock），见 `zte-agent/src/wifi_radio.rs`。
- shell：`( flock 9; echo $$ > /tmp/u60-wifi.lock; … ) 9>>/tmp/u60-wifi.lock`。
  用 `>>` 打开，不要用 `>`：等锁时不能清掉持锁者写的 pid。
- 设备的 busybox `flock` **没有 `-w`**。要限时就用 `flock -n` 轮询加截止时间。
- 持锁者在拿到锁后把自己的 pid 写进文件（设备上没有 `fuser`）。锁释放后文件不清空，
  所以里面的 pid 可能过期或已被复用：`kill` 之前必须确认 `/proc/<pid>/comm` 以 `zte-agent` 开头。
- agent 单次合法持锁最长 `MAX_HOLD` = **120 秒**。u60-guard 判定「agent 持锁卡死」的等待必须更长（建议 150 秒）。
- 文件永不删除。

## 2. 心跳 `/tmp/scenario.heartbeat`

- agent 的引擎线程写：bootsafe 之前一次，之后每轮循环一次（间隔 15 秒加上一轮 tick 的耗时）。
  引擎停用、未配置时照写。它证明 agent 活着，不代表引擎在切换。
- 内容：`/proc/uptime` 的整秒数加换行，先写临时文件再改名。
- 判定：开机 5 分钟内不判断。见过心跳后，心跳超过 5 分钟没更新算失联；开机 15 分钟仍**从未见过**心跳也算失联（agent 根本没起来）。一轮 tick 很长时（例如锁争用）可能超过 5 分钟没有心跳，
  这时误判的后果是多开一次 Wi-Fi，属于安全方向。
- 引擎线程 panic 时心跳会停。受 procd 监督（环境变量 `ZTE_AGENT_SUPERVISED=1`）的 agent 会随之退出，交给 procd 重启。

## 3. 接管标记 `/tmp/u60-wifiguard.took-over`

- u60-guard 接管 Wi-Fi 时写入，**每次接管只写一次**，内容在这次接管里唯一（写当时的 uptime）。
  每轮检查都重写会让 agent 的计时一直归零，控制权永远交不回来。
- 标记存在期间 agent 不做识别、不理 pin，回到并停留在「外出」情景。
- agent 看着同一份内容稳定运行满 **10 分钟**后删除它，交回控制权。内容变了（又一次接管）就重新计时。
- 只有 agent 删除它（或者整机重启，因为在 `/tmp`）。

## 4. 告警目录 `/data/alerts/`

放在 `/data` 上：重启循环不能清空告警，也不能重置短信额度。目录权限 700。

| 文件 | 谁写 | 内容 |
|---|---|---|
| `lock` | 所有写入方 | 本目录内任何「读 → 改 → 写」都要持有它的 flock |
| `seq` | 事件写入方 | 最后分配出去的事件序号（十进制） |
| `queue` | 事件写入方 | 事件，一行一条，见下 |
| `read` | agent | 网页上已读到的序号 |
| `sms-to` | agent | 告警短信号码；没有这个文件就是没配置 |
| `sms-abroad` | agent | 存在 = 在国外也发短信（默认不发，避免漫游费） |
| `abroad` | agent | 存在 = 当前情景是按 SIM 国家码匹配出来的（国外） |
| `sms-done` | u60-guard | 已经做过短信处理的最大序号 |
| `sms-log` | u60-guard | 短信发送记录，一行一条，见下 |

### 事件 `queue`

```
<序号>\t<墙钟秒>\t<uptime 秒>\t<类别>\t<说明>
```

- **序号**在 `lock` 内分配：读 `seq`、加 1、写回，再追加这一行。游标（`read`、`sms-done`）都存序号，
  所以裁剪文件不影响游标。不用行号、字节偏移或墙钟做编号。
- **墙钟不可信**：设备 RTC 在网络对时之前是 1971 年左右。墙钟秒 < `1704067200`（2024-01-01）的一律当作「时间未校准」，
  网页显示「开机后 N 秒」。
- 类别：`[a-z0-9-]`，现有：`agent-crash`、`datad-crash`（supervise.sh）；`devui-crash`、`devui-gave-up`（u60-uid）；`agent-silent`、`agent-hung`、`wifi-takeover`、`wifi-restore-failed`、`sms-failed`（u60-guard）。
- 说明：只能是可打印 ASCII，去掉 tab 和换行，最多 120 个字符。给网页告警列表看（短信正文按类别另写），不能带任何配置内容（号码、密码、SSID）。
- 写入方统一用 touch-ui `scripts/alert-lib.sh` 里的写入函数（supervise.sh 和 u60-guard 共用），它负责清洗、截断和裁剪：
  超过 300 行时保留最后 200 行。
- agent 只读。遇到格式不对的行跳过，不报错。

### 短信

- **发送方只有 u60-guard**（常驻、受 procd 监督，每 60 秒一轮）。agent 不发告警短信，只负责号码设置和展示结果。
  这样游标、限流、国外抑制、编码、结果判定只有一份实现。
- 每轮按序号处理 `sms-done` 之后的事件。`sms-log`、`sms-done` 只有 u60-guard 写，`queue` 裁剪时整文件替换，所以这一步不拿 `lock`：
  - `sms-failed` 类别不发（否则失败会引出新的失败短信）。
  - 没有 `sms-to`（或号码不合法）：不发，记 `no-number`。
  - 有 `abroad` 且没有 `sms-abroad`：跳过，记 `suppressed-abroad`。
  - 限流：同一类别 1 小时内最多 1 条，24 小时内总共最多 5 条，按 `sms-log` 里的 `sent` 记录算。
    超出记 `suppressed-rate`。墙钟未校准时**不发、也不推进游标**，等校准后再处理。
    `sms-log` 里时间戳比现在还晚的记录（时钟往回跳过）一律当作「刚发过」。
  - 发送：`ubus call zwrt_wms zte_libwms_send_sms`，字段与 `sms_forward.rs` 一致
    （`number`、`message_body` 为 UCS-2 十六进制大写、`encode_type` 为 `UNICODE`、`sms_time` 为 `YY;MM;DD;HH;MM;SS;+TZ`、`id` 为 `-1`）。
    返回体里 `result` 是 3 或没有 `result` 才算成功。失败记 `failed`，不计入额度，并追加一条 `sms-failed` 事件；不重试。
  - 号码在拼 JSON 之前再校验一次：`+` 可有可无，后面只能是数字，总长不超过 20。
- 短信正文：按类别写好的一句中文大白话（发生了什么、影不影响上网、要不要动手），`【U60】…（MM-DD HH:MM）`，
  一条短信以内（≤70 字）；不认识的类别提示去网页看。事件的 ASCII 说明只留在网页告警列表里。u60-guard 自己把 UTF-8 解成 UCS-2（busybox 没有 iconv）。

### 短信记录 `sms-log`

```
<墙钟秒>\t<序号>\t<类别>\t<结果>
```

结果为 `sent`、`failed`、`suppressed-rate`、`suppressed-abroad`、`no-number` 之一。u60-guard 写入时裁剪到最后 100 行。

## 5. 对外接口（agent）

- `GET /api/alerts`（需登录）：事件列表、未读数、短信设置和最近的发送结果。号码只在这里出现。
- `POST /api/alerts/read`（需登录）：`{"seq": N}` 标记已读。
- `PUT /api/alerts/sms`（需登录）：`{"number": "+86…" | "", "abroad": bool}`。
- `GET /api/public/status` 的 `alerts.unread`：只有未读数，给触屏红点用。不带任何事件内容。

## 6. 屏幕：u60-uid（touch-ui `src/uid.c`，procd 监督）

- u60-uid 是 u60pro-devui 唯一的主人：拉起、按 comm 前缀 `u60pro-devui` 接管已在跑的（绝不起第二份）、
  切原厂界面（`zte_topsw_devui`）、原厂界面在屏时识别长按右下角 3 秒回来。corner-wake 已退役，start.sh 在
  rc.local 里有 `/etc/init.d/u60-uid start` 时只做开机杂务（`start.sh prep`），不再起界面。
- `/data/u60-uid/attempts`：拉起前加 1 并 fsync，稳定运行 10 分钟清零；达到 2 就不再拉起，写
  `/data/u60-uid/gave-up`、告警 `devui-gave-up`、交还原厂界面。放弃状态跨重启保留，长按或
  `echo devui > /tmp/u60-uid.ctl` 清除。文件损坏按 2 处理。
- 「选了原厂界面」在 `/tmp/u60-uid.want`，重启即恢复为我们的界面。
- `/tmp/u60-uid.ctl` 是 FIFO，只在 u60-uid 运行时存在（它退出时删掉）：`vendor` / `devui`。
  触屏的「切回原厂」按钮用非阻塞写，写不进去就回落为自己切。
- 非预期退出（非 0、被信号杀，且不是 u60-uid 要求的停止）写 `/data/crashlog/u60pro-devui/` 并告警 `devui-crash`；
  接管来的进程拿不到退出码，不计。
- **屏幕上几分钟没有任何界面，固件会整机重启**（2026-09-23 实测，`reboot_reason_code` 1185）。所以任何停掉一个界面的
  路径都要在同一步里起另一个。

## 7. 体检与备份（touch-ui `scripts/doctor.sh`、`scripts/config-backup.sh`）

- `doctor.sh`：只读，每项一行 `<ok|warn|bad>\t<id>\t<名称>\t<说明>`（`--tsv`）。agent 每 60 秒跑一次给 `/api/health`
  和 `/api/public/status.health`（只有计数）；装机包 `./install.sh doctor` 推到 `/tmp` 直接跑，agent 挂了也能用。
- `config-backup.sh`：只备份配置（清单在脚本开头），不含运行状态；Tailscale 身份要 `--with-tailscale`。
  备份只存电脑（装机包 `./install.sh backup`），`restore` 先 `plan` 再写、写前留 `.pre-restore`、不重载 Wi-Fi。
