# Reliability contract: file conventions between programs on the device

**English** · [中文](RELIABILITY.zh-CN.md)

Programs on the device learn about each other only through the files below. Change any one of them and both the reader and the writer must change together.

**Current state (2026-10-15, Phase 10 done)**:

| Written below as (at the time) | Now handled by |
|---|---|
| agent (zte-agent, removed in 8d-2c) | core `crates/luyaz-core` (`/data/luyaz/luyaz-core`, `/etc/init.d/luyaz-core`, state `/data/luyaz/core/`); engines in the shared `crates/luyaz-*`: Wi-Fi lock `luyaz-wifi`, scenario heartbeat `luyaz-scenario`, alert files and SMS `luyaz-alerts` |
| u60-guard (retired 10-10, removed in 10c) | watchdog, takeover, clock verdict: sentinel `crates/luyaz-sentinel` (`/data/luyaz/luyaz-sentinel`); alert SMS, LAN IPv6: core `engines/guard_duties.rs`; standby records, ledger: core `longterm/` |
| u60-uid (removed in 10c) | sentinel `screen.rs` (section 6) |
| supervise.sh (removed in X9) | core `svcwatch.rs` (`datad-crash`/`web-crash`/`sentinel-crash`) |
| doctor.sh | `<core> doctor`; `/data/luyaz/scripts/doctor.sh` (before D1 `/data/u60-guard/`; readers use the new one if present) is a thin shim, fallback `doctor-legacy.sh` |
| `/tmp/u60-guard/…` | `/tmp/luyaz/…` (`.migrated` rule, u60-phase10.md §9 10d) |

Sections 1–5 and 7–9 keep the wording of the time as a record (`zte-agent/src/…` paths, `ZTE_AGENT_SUPERVISED`, `agent-*` alert kinds);
the file names and formats themselves are unchanged. Read them with the subjects swapped per the table above. The admin web is luyaz-web (:9090), password file `/data/luyaz/web.env`.

## 1. Wi-Fi lock `/tmp/u60-wifi.lock`

Every program that writes the `wireless` uci package must hold it (exclusive `flock`) from the first `uci set` until the reload has been verified.

- agent: `wifi_radio::lock()` (in-process lock + flock), see `zte-agent/src/wifi_radio.rs`.
- shell: `( flock 9; echo $$ > /tmp/u60-wifi.lock; … ) 9>>/tmp/u60-wifi.lock`.
  Open with `>>`, not `>`: waiting for the lock must not wipe the pid the holder wrote.
- The device's busybox `flock` **has no `-w`**. For a time limit, poll `flock -n` against a deadline.
- After getting the lock the holder writes its pid into the file (the device has no `fuser`). The file is not cleared on release,
  so the pid in it may be stale or reused: before any `kill`, confirm `/proc/<pid>/comm` starts with `zte-agent`.
- The agent's longest legitimate hold is `MAX_HOLD` = **120 seconds**. u60-guard must wait longer before deciding "agent stuck holding the lock" (150 seconds suggested).
- The file is never deleted.
- Long-lived child processes must never inherit this lock's file descriptor: a flock lock lives on the open file, and stays held as long as any process has it open.
  u60-guard closes fd 7, 8, 9 when it starts the crash watcher loop (section 9); anyone starting a long-lived child does the same.

## 2. Heartbeat `/tmp/scenario.heartbeat`

- Written by the agent's engine thread: once before bootsafe, then once per loop (15-second interval plus the time a tick takes).
  Written even when the engine is disabled or unconfigured. It proves the agent is alive, not that the engine is switching.
- Content: whole seconds of `/proc/uptime` plus a newline, written to a temp file and renamed.
- Verdict: no judgement in the first 5 minutes after boot. Once a heartbeat has been seen, more than 5 minutes without an update means lost; **never having seen** a heartbeat 15 minutes after boot also means lost (the agent never came up). A very long tick (e.g. lock contention) can go over 5 minutes without a heartbeat;
  a false verdict then just turns Wi-Fi on once more, which is the safe direction.
- The heartbeat stops if the engine thread panics. An agent under procd (env `ZTE_AGENT_SUPERVISED=1`) exits with it and procd restarts it.

## 3. Takeover marker `/tmp/luyaz/wifiguard.took-over` (before Luya-Z rename E2: `/tmp/u60-wifiguard.took-over`)

- Written by u60-guard when it takes over Wi-Fi, **only once per takeover**, with content unique to that takeover (the uptime at the time).
  Rewriting it every check would keep resetting the agent's timer, and control would never come back.
- While the marker exists the agent does no detection, ignores pins, and goes to and stays in the 「外出」 (away) scenario.
- After running stably for **10 minutes** with the same content, the agent deletes it and takes control back. If the content changes (another takeover), the timer restarts.
- Only the agent deletes it (or a reboot, since it is in `/tmp`).

## 4. Alert directory `/data/alerts/`

On `/data`: a reboot loop must neither wipe alerts nor reset the SMS quota. Directory mode 700.

| File | Written by | Content |
|---|---|---|
| `lock` | all writers | hold its flock for any read → modify → write in this directory |
| `seq` | event writers | last event sequence number handed out (decimal) |
| `queue` | event writers | events, one per line, see below |
| `read` | agent | sequence number read up to on the web |
| `sms-to` | agent | alert SMS number; no file = not configured |
| `sms-abroad` | agent | present = also send SMS when abroad (off by default, to avoid roaming charges) |
| `abroad` | agent | present = the current scenario was matched by SIM country code (abroad) |
| `sms-done` | u60-guard | highest sequence number already handled for SMS |
| `sms-log` | u60-guard | SMS send records, one per line, see below |

### Events `queue`

```
<序号>\t<墙钟秒>\t<uptime 秒>\t<类别>\t<说明>
```

(Fields: sequence number, wall-clock seconds, uptime seconds, kind, description.)

- **Sequence numbers** are allocated under `lock`: read `seq`, add 1, write it back, then append the line. Cursors (`read`, `sms-done`) store sequence numbers,
  so trimming the file does not affect them. No line numbers, byte offsets or wall-clock as IDs.
- **Wall-clock is untrusted**: before network time sync the device RTC is around 1971. Wall-clock seconds < `1704067200` (2024-01-01) always mean "time not calibrated";
  the web shows 「开机后 N 秒」 (N seconds after boot).
- Kinds: `[a-z0-9-]`; current ones: `agent-crash` (supervise.sh), `datad-crash`, `web-crash`, `sentinel-crash` (core luyaz-core svcwatch, from Phase 10 X9; before that datad-crash was written by supervise.sh); `devui-crash`, `devui-gave-up` (u60-uid); `devui-theme-paused` (the touch UI itself: auto light/dark switched 3 times within 1 hour — normally only 2 a day — paused until next boot; no SMS); `agent-silent`, `agent-hung`, `wifi-takeover`, `wifi-restore-failed`, `sms-failed`, `datad-degraded` (u60-guard); `tailscale-gave-up` (the watcher in Tailscale's `start.sh`: tailscaled still dies after 6 starts within 24 hours, paused until those 24 hours pass; u60-guard has no dedicated SMS text for it yet and uses the generic one).
- `datad-degraded` watches the marker file `/data/u60-guard/datad-degraded`: line 1 = epoch seconds when the fallback began, line 2 = reason (one line of printable ASCII, ≤120 characters).
  zte-agent (`zte-agent/src/datad_feed.rs`) is the only writer and deleter: written when datad `/v2` stops answering and it reads ubus itself, rewritten every 60 seconds during the fallback (start time unchanged, mtime refreshed),
  deleted on recovery; on start the agent deletes any old marker and re-evaluates. u60-guard only reads: only if mtime is under 3 minutes old and the start time is more than 5 minutes ago does it `alert_once` by start time;
  a stale mtime (agent no longer refreshing) sends nothing — that is the heartbeat alert's job; not checked during the boot grace period.
- Description: printable ASCII only, tabs and newlines stripped, at most 120 characters. For the web alert list (SMS text is written per kind separately); must never contain any configuration (numbers, passwords, SSIDs).
- Writers all use the write function in touch-ui `scripts/alert-lib.sh` (shared by supervise.sh and u60-guard), which handles cleaning, truncation and trimming:
  over 300 lines, keep the last 200.
- The agent only reads. Malformed lines are skipped without error.

### SMS

- **u60-guard is the only sender** (long-running, under procd, one round every 60 seconds). The agent sends no alert SMS; it only sets the number and shows results.
  That way cursor, rate limit, abroad suppression, encoding and result checking have a single implementation.
- Each round handles events after `sms-done` in sequence order. `sms-log` and `sms-done` are written only by u60-guard, and `queue` is replaced whole when trimmed, so this step does not take `lock`:
  - Kind `sms-failed` is not sent (otherwise a failure would trigger new failure SMS).
  - Kind `devui-theme-paused` is not sent (just touch-UI looks, not worth an SMS).
  - No `sms-to` (or invalid number): not sent, logged `no-number`.
  - `abroad` present and no `sms-abroad`: skipped, logged `suppressed-abroad`.
  - Rate limit: at most 1 per kind per hour, at most 5 total per 24 hours, counted from `sent` records in `sms-log`.
    Over the limit logs `suppressed-rate`. With the wall-clock uncalibrated, **nothing is sent and the cursor does not advance**; handled after calibration.
    Records in `sms-log` timestamped later than now (clock jumped back) always count as "just sent".
  - Sending: `ubus call zwrt_wms zte_libwms_send_sms`, fields as in `sms_forward.rs`
    (`number`, `message_body` as uppercase UCS-2 hex, `encode_type` = `UNICODE`, `sms_time` = `YY;MM;DD;HH;MM;SS;+TZ`, `id` = `-1`).
    Success only if `result` in the reply is 3 or there is no `result`. Failure logs `failed`, does not count against the quota, and appends an `sms-failed` event; no retry.
  - The number is validated again before building the JSON: optional `+`, then digits only, total length at most 20.
- SMS text: one plain-Chinese sentence written per kind (what happened, whether internet is affected, whether to act), `【Luya-Z】…（MM-DD HH:MM）`,
  within one SMS (≤70 characters); unknown kinds point to the web. The event's ASCII description stays only in the web alert list. u60-guard decodes UTF-8 to UCS-2 itself (busybox has no iconv).

### SMS log `sms-log`

```
<墙钟秒>\t<序号>\t<类别>\t<结果>
```

(Fields: wall-clock seconds, sequence number, kind, result.)

Result is one of `sent`, `failed`, `suppressed-rate`, `suppressed-abroad`, `no-number`. u60-guard trims to the last 100 lines when writing.

## 5. External API (agent)

- `GET /api/alerts` (login required): event list, unread count, SMS settings and recent send results. The number appears only here.
- `POST /api/alerts/read` (login required): `{"seq": N}` marks as read.
- `PUT /api/alerts/sms` (login required): `{"number": "+86…" | "", "abroad": bool}`.
- `alerts.unread` in `GET /api/public/status`: unread count only, for the touch UI's red dot. No event content.

## 6. Screen: sentinel (`crates/luyaz-sentinel/src/screen.rs`, under procd)

- The sentinel is the sole owner of the touch UI (Rust, `/data/luyaz/ui/luyaz-ui`, comm `luyaz-ui`): starts it, adopts an already-running one by comm
  (never starts a second), switches to the stock UI (`zte_topsw_devui`), and while the stock UI is on screen detects a 3-second long press in the bottom-right corner to come back.
  `/data/plugins/u60pro-devui/start.sh` only does boot chores (`start.sh prep`), it does not start a UI.
- `/data/luyaz/screen/attempts`: incremented and fsynced before each start, reset after 10 minutes of stable running; at 2 it stops starting, writes
  `/data/luyaz/screen/gave-up`, alerts `devui-gave-up`, and hands back to the stock UI. The gave-up state survives reboots; cleared by the long press or
  `echo devui > /tmp/luyaz-screen.ctl`. A corrupt file counts as 2.
- "Stock UI chosen" lives in `/tmp/luyaz/screen.want` (before Luya-Z rename E2: `/tmp/u60-uid.want`); a reboot restores our UI.
- `/tmp/luyaz-screen.ctl` is a FIFO, present only while the sentinel runs: `vendor` / `devui` / `hold` / `release` (the last two for ship).
  The old `/tmp/u60-uid.ctl` is gone since 10h. The touch UI's 「切回原厂」 (switch back to stock) button writes non-blocking and falls back to switching itself if the write fails.
- Unexpected exits (non-zero, killed by a signal, and not a stop the sentinel asked for) are written to `/data/crashlog/u60pro-devui/` (name unchanged) and alert `devui-crash`;
  adopted processes give no exit code and are not counted.
- **If the screen shows no UI for a few minutes, the firmware reboots the whole device** (measured 2026-09-23, `reboot_reason_code` 1185). So any path that stops one UI
  must start the other in the same step.

## 7. Health check and backup (touch-ui `scripts/doctor.sh`, `scripts/config-backup.sh`)

- `doctor.sh`: read-only, one line per item `<ok|warn|bad>\t<id>\t<名称>\t<说明>` (id, name, description; `--tsv`). The agent runs it every 60 seconds for `/api/health`
  and `/api/public/status.health` (counts only); the install kit's `./install.sh doctor` pushes it to `/tmp` and runs it directly, so it works even with the agent down.
  `--tsv` does not read the ledger; row ids and order are unchanged; only the 「待机」 (standby) and 「时钟」 (clock) rows had their verdict logic changed to fix bugs (sections 8, 9).
- `doctor.sh --report 24h|7d`: device scorecard; reads `/data/ledger` and gives values and grades for stable / usable / frugal item by item (not measured / failing / watch / passing),
  changing nothing. The default human output ends with the 3 lines of `/data/ledger/summary` copied verbatim (rewritten hourly by u60-guard).
- `doctor.sh --ledger-selftest`: whether the ledger can work on this device, item by item PASS/FAIL (directory writable, key.log reason codes, self-recovery switch,
  kmsg persisted to disk, and the device tools u60-guard depends on). Writes only one test file, deleted after the test, and one kmsg line.
- `config-backup.sh`: backs up configuration only (list at the top of the script), no runtime state; Tailscale identity needs `--with-tailscale`.
  Backups are stored only on the computer (install kit `./install.sh backup`); `restore` does `plan` before writing, leaves `.pre-restore` before writing, and does not reload Wi-Fi.

## 8. Standby sentinel (recorded by u60-guard, judged by doctor.sh)

- While the screen is off, u60-guard writes one line per round (60 seconds) to `/tmp/standby.stat`: `<uptime> <蜂窝包/分> <Tailscale 隧道包/分> <tailscaled 代理核心 u60pro-devui zwrt-datad zte-agent 的唤醒/秒>` (cellular packets/min, Tailscale tunnel packets/min, wakeups/sec of tailscaled, the proxy core, u60pro-devui, zwrt-datad, zte-agent). A cell for a program that restarted mid-interval (pid changed) is written as `-`; nothing is written when the screen is on, when the gap between rounds is too long (device slept), or when a counter goes backwards. Only 60 lines are kept, on the RAM disk.
- Tunnel traffic is recorded separately, not subtracted from the cellular packet count: another tailnet device constantly hitting this one (say, an open admin web) is exactly the waste the sentinel is meant to catch.
- `doctor.sh --calibrate-standby` uses these records (at least 30 lines) to compute each column's median and MAD, written to `/data/u60-guard/standby.baseline`. Calibrate after all power-saving changes are live, with the screen off for 30–60 minutes.
- The 「待机」 (standby) health row: with no baseline it shows 「未校准」 (not calibrated; counts as fine); **only idle minutes are judged**: a line whose cellular packet count exceeds "baseline median + 3×MAD" means someone is using the network
  and is not judged; if most lines in the last 15 minutes have traffic it writes 「有流量在用，不判待机」 (traffic in use, standby not judged); fewer than 8 idle lines, no verdict; a column whose median exceeds "baseline + 3×MAD" and is over 1.5× baseline
  (and at least 1 more) is reported ▲, stating which column, the current value and the baseline. Display only, no SMS.
- **Baseline v2 carries fingerprints** (from 2026-09-28): first line `v2`, and per column `fp <列> <依赖的 md5>` (column, md5 of its dependencies) — the md5 of the measured program's actual running file (computed by u60-guard when the pid changes,
  stored in `/tmp/u60-guard/ledger/fp`) and of related config files (Tailscale's `tuning.env` etc.). If a column's dependencies change it writes 「基线过期，不判」 (baseline stale, not judged);
  an old baseline without `v2` gets a whole-row 「基线格式旧，请在家自然空闲时重新校准」 (old baseline format, recalibrate at home while naturally idle). Records from before a program version change (`fp-changed`) are not used.
  Number of columns per record = 3 + number of measured programs; calibration and verdict both count this way.

## 9. Device ledger (written by u60-guard, read by doctor.sh; full contract in touch-ui `docs/LEDGER.md`)

- **Exactly one writer**: u60-guard's ledger task (background subshell, once per round, takes `flock -n` on `/tmp/u60-guard/ledger/writer.lock`,
  and skips the round if it can't). The ledger is `/data/ledger/boot-<序号>-<boot>-<段>.jsonl` (sequence, boot, segment), one flat JSON event per line, append-only.
  The main loop never waits for it: no ledger problem can touch the Wi-Fi fallback or SMS.
- **Other programs record events via spool files**: `/data/ledger/spool/<编号>.ev` (must survive reboot) or `/tmp/ledger-spool/<编号>.ev` (编号 = ID),
  two lines: `<编号>\t<完整 boot_id>\t<开机秒数>\t<种类>` (ID, full boot_id, seconds since boot, kind) and a field fragment starting with `,`; write a temp name, fsync, rename. The ledger task collects them, deduplicating by ID.
  Current writers: u60-guard's crash watcher loop, `doctor.sh --calibrate-standby`; from step two the agent, touch UI and web also write this format
  (e.g. a `reboot_request` before every reboot path fires).
- **Crash watcher loop** `u60-guard.sh watcher`: each round u60-guard checks it is running and current, starts it if not, replaces it if the version is wrong; when starting it closes
  fd 7/8/9 (section 1). Every 2 seconds it reads new content from the persisted kmsg file, recognizes modem crashes, follows the cellular interface until recovery, and writes only to the spool. If `u60-guard.sh` on disk changes,
  it exits by itself within a minute.
- **Before a deliberate u60-guard restart** (ship, manual restart) write `/tmp/u60-guard/stop-requested` first, one line `<谁> <为什么>` (who, why); the next start records it as
  deliberate (`guard_start requested=1`) and deletes it. A second start without this marker counts as an unexpected exit.
- **Clock verdict** `/tmp/u60-guard/clock-ok`: `<0|1> <偏移> <开机秒数> <依据>` (offset, seconds since boot, basis), written by u60-guard every round, used by both doctor and the ledger.
  Before time sync the device clock reads 2025-01-04; it is trusted only when the year is ≥ 2026 and there is sync evidence (a forward jump of more than a day, or not earlier than the last trusted wall-clock minus one day).
