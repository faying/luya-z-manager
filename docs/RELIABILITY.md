# Reliability contract: file conventions between programs on the device

**English** · [中文](RELIABILITY.zh-CN.md)

Programs on the device learn about each other only through the files below. Change any one of them and both the reader and the writer must change together.

Current programs (all under procd, one `/etc/init.d/<name> start` line each in rc.local):

| Program | Binary | Owns |
|---|---|---|
| core (u60d) | `/data/luyaz/luyaz-core`, state `/data/luyaz/core/` | scenario engine, alert SMS (normal path), service crash watch, modem crash watch, standby records, doctor, `/v2` API on :9460 |
| sentinel | `/data/luyaz/luyaz-sentinel`, state `/data/luyaz/screen/` | screen ownership, Wi-Fi fallback watchdog, clock verdict, dropbear watch, alert SMS while the core is unhealthy |
| web | luyaz-web :9090, password `/data/luyaz/web.env` | admin pages; talks only to the core |
| touch UI | `/data/luyaz/ui/luyaz-ui` (comm `luyaz-ui`) | started and adopted only by the sentinel |

The contract as written for the retired zte-agent, u60-guard, u60-uid and supervise.sh is in [`archive/RELIABILITY-pre-phase10.md`](archive/RELIABILITY-pre-phase10.md) (record only).

## 1. Wi-Fi lock `/tmp/luyaz-wifi.lock`

Every program that writes the `wireless` uci package holds it (exclusive `flock`) from the first `uci set` until the reload has been verified.

- Library: `crates/luyaz-wifi` (`MAX_HOLD` 120 s, `ENGINE_WAIT` 150 s, `HTTP_WAIT` 5 s). The file is opened with `O_CLOEXEC` on each acquire, so uci/ubus children never inherit it; the holder writes its pid into it; the file is never deleted.
- Core `/control` takes `/var/run/luyaz-write.lock` first, then the Wi-Fi lock (`ops/wifi_lock.rs`; 150 s wait, 2 tries, then `503 busy`; no wait if the holder pid is the caller or one of its ancestors).
- Sentinel (`luyaz-sentinel/src/wifi.rs`) waits up to 150 s. If it cannot get the lock the round fails and is retried later.
- **Nobody kills a holder.** There is no stuck-holder kill any more.
- Shell writers: `( flock 9; echo $$ > /tmp/luyaz-wifi.lock; … ) 9>>/tmp/luyaz-wifi.lock`. Open with `>>`, never `>`. busybox `flock` has no `-w`; poll `flock -n` against a deadline.

## 2. Scenario heartbeat `/tmp/scenario.heartbeat`

- Writer: the scenario engine inside the core (`crates/luyaz-scenario`), once before bootsafe, then once per loop, even when disabled. Content: whole seconds of `/proc/uptime` plus a newline, temp file + rename. It proves the engine thread is alive, not that it is switching.
- Judge: the sentinel (`worker.rs` guard round, every 60 s). No verdict in the first 300 s after boot; once seen, older than 300 s = lost; never seen 900 s after boot = lost. Two rounds more than 150 s apart mean the device slept, and the engine gets a full 300 s again.
- On loss the sentinel turns Wi-Fi back on (section 3) and alerts `agent-silent` (kind name kept). Restore retries back off 60, 120, 240, 480 s.
- doctor also reads it.

## 3. Takeover marker `/tmp/luyaz/wifiguard.took-over`

- Written by the sentinel when it takes over Wi-Fi, **once per takeover**, content = its uptime at that moment (alert `wifi-takeover`; restore failure alerts `wifi-restore-failed`).
- While it exists the scenario engine does no detection, ignores pins, and goes to and stays in 「外出」 (away).
- After 600 s of steady running with the same content the engine deletes it and takes control back; new content restarts the timer. Only the engine deletes it (or a reboot).
- Also read by the core status/netinfo APIs and doctor.

## 4. Alert directory `/data/alerts/`

On `/data`, mode 700: a reboot loop must neither wipe alerts nor reset the SMS quota. All access goes through `crates/luyaz-alerts`.

| File | Content |
|---|---|
| `lock` | flock held for any read → modify → write here |
| `seq` | last sequence number handed out |
| `queue` | events, `<seq>\t<wall s>\t<uptime s>\t<kind>\t<text>` |
| `read` | read cursor (web) |
| `sms-to` | alert SMS number; missing or empty = SMS off |
| `sms-abroad` | present = also send SMS when abroad |
| `abroad` | present = current scenario matched by SIM country code (written by the scenario engine) |
| `sms-done` | highest sequence number already handled for SMS |
| `sms-sending` | send claim `<seq>\t<sender>\t<pid>\t<boot_id>\t<uptime>\t<wall>`; treated as uncertain after 120 s, from another boot, or if the pid is dead |
| `sms-log` | `<wall s>\t<seq>\t<kind>\t<result>`, last 100 lines |

- `add` allocates the sequence number under `lock`, appends, and trims past 300 lines to the last 200. `add_once` (sentinel events) prefixes the text with `[<bootid8>-<n>] `. Cursors store sequence numbers, never line numbers.
- Wall-clock under `1704067200` (2024-01-01) means "not calibrated"; the web shows 「开机后 N 秒」.
- Text: printable ASCII, ≤ 120 characters, never any configuration (numbers, passwords, SSIDs). Malformed lines are skipped.
- Kinds emitted now:
  - sentinel: `agent-silent`, `wifi-takeover`, `wifi-restore-failed`, `dropbear-down`, `dropbear-restarted`, `devui-crash`, `devui-gave-up`
  - core svcwatch: `datad-crash` (the core itself), `web-crash`, `sentinel-crash`
  - core sentinel watch: `sentinel-silent`, `screen-empty` (only if the screen-guard switch is on)
  - core engines: `home-exit-down`, `home-exit-check-failed`, `sms-forward-stuck`
  - touch UI: `devui-theme-paused`

### SMS

- One implementation, `Alerts::sms_round`, two possible senders, never both:
  - **core** (`engines/guard_duties.rs`, every 30 s) when `/tmp/luyaz/handover` names `sms` and writes are on — the normal case;
  - **sentinel** only while the core is unhealthy (2 bad `/healthz` rounds) and not being replaced by ship.
  The `sms-sending` claim keeps a handover between them from sending twice.
- Rules: `sms-failed` and `devui-theme-paused` are never sent; no number → `no-number`; `abroad` without `sms-abroad` → `suppressed-abroad`; at most 1 per kind per hour and 5 per 24 h → else `suppressed-rate`. Nothing is sent and the cursor does not advance until the clock verdict (section 9) is trusted.
- Send: `ubus call zwrt_wms zte_libwms_send_sms` (`number`, `message_body` uppercase UCS-2 hex, `encode_type` `UNICODE`, `sms_time` `YY;MM;DD;HH;MM;SS;+TZ`, `id` `-1`). Success if `result` is 3 or missing; the sent copy is then deleted from the SMS store. Failure logs `failed` and appends `sms-failed`; no retry.
- Text: one plain-Chinese sentence per kind, `【Luya-Z】…（MM-DD HH:MM）`, ≤ 70 characters; unknown kinds point to the web.

## 5. Alert API (core)

Served by the core on loopback as `/v2/api/…`; luyaz-web (`alerts_page.rs`) is the only UI and calls these.

- `GET /api/alerts`: events, unread count, SMS settings, recent results. The number appears only here.
- `POST /api/alerts/read` `{"seq": N}`, `PUT /api/alerts/sms` `{"number"?, "abroad"?}`: registered only when writes are on.
- `GET /api/health`, `GET /api/health/crashlog?program=&file=`.
- `POST /v2/sentinel/event`: the sentinel's events into the core.
- A non-loopback listener needs `--auth-token-file` (`/data/luyaz/core/auth.token`) or an explicit `--lan-bind`.

## 6. Screen (sentinel, `crates/luyaz-sentinel/src/screen.rs`, `run.rs`)

- The sentinel is the only owner of the touch UI: starts it, adopts a running one by comm `luyaz-ui` (never starts a second), switches to the stock UI (`zte_topsw_devui`), and while the stock UI is up detects a 3 s long press in the bottom-right corner to come back.
- `/data/luyaz/screen/attempts`: incremented and fsynced before each start, reset after 10 min of stable running; at 2 it writes `/data/luyaz/screen/gave-up`, alerts `devui-gave-up` and hands back to the stock UI. Gave-up survives reboots; cleared by the long press or `devui`. A corrupt file counts as 2.
- `/tmp/luyaz/screen.want` starting with `vendor` = stock UI chosen; a reboot restores ours.
- `/tmp/luyaz-screen.ctl` (FIFO, only while the sentinel runs): `vendor`, `devui`, `hold [s]` (10–3600, default 600), `release` (the last two for ship).
- Unexpected exits of a UI it started go to `/data/crashlog/u60pro-devui/` (5 kept) and alert `devui-crash`; adopted processes give no exit code and are not counted.
- Sentinel heartbeat `/tmp/luyaz-sentinel.hb`, events `/data/luyaz/screen/events{,.seq}` (200 kept), log `/tmp/luyaz-sentinel.log`.
- **If the screen shows no UI for a few minutes, the firmware reboots the whole device** (reason code 1185). Any path that stops one UI must start the other in the same step.

## 7. Health check and backup

- `luyaz-core doctor` (process name `luyaz-doctor`; `/data/luyaz/scripts/doctor.sh` is a thin shim). Read-only. Output: human text, `--tsv` (`<ok|warn|bad>\t<id>\t<名称>\t<说明>`), `--tsv2` (header `#tsv2`, English); exit 1 if any row is bad. Also `--manifest`, `--tree-fp`, `--ledger-selftest`.
- `--calibrate-standby` writes the standby baseline (section 8). `--report` is not in the core (exit 64); the shim falls back to `doctor-legacy.sh` for it.
- The core runs the checks itself (first 20 s after start, then periodically) and serves the snapshot at `/api/health`.
- `touch-ui/scripts/config-backup.sh`: configuration only (list at the top of the script), Tailscale identity only with `--with-tailscale`. Backups live only on the computer (`./install.sh backup`); `restore` plans first, leaves `.pre-restore`, does not reload Wi-Fi.

## 8. Standby records

- Writer: core `longterm/standby.rs`, only while `/tmp/luyaz/handover` names `standby`. While the backlight is off, one line per round to `/tmp/standby.stat`: `<uptime> <cellular pkts/min> <Tailscale tunnel pkts/min> <wakeups/s per program>`, last 60 kept; a program whose pid changed is `-`; no line if the previous round is more than 150 s ago.
- The program columns are fixed by position (doctor reads by position) and still carry the pre-rename labels `tailscaled mihomo u60pro-devui zwrt-datad zte-agent`.
- Fingerprints `/tmp/luyaz/ledger/fp`, `fp-changed`. Baseline `/data/luyaz/scripts/standby.baseline` (v2 with `fp` lines); written by `luyaz-core doctor --calibrate-standby` (≥ 30 records; per column median and MAD, then the fingerprints; one `calib` event via `POST /v2/longterm/calib`), judged by doctor's 「待机」 row.

## 9. Clock verdict, crash records, `.migrated`

- **Clock** `/tmp/luyaz/clock-ok`: `<1|0> <wall−uptime> <uptime> <how>` (`vendor|jump|last_wall|year|revoked|none`), written by the sentinel; read by the SMS path, the core's longterm store and doctor. The sentinel keeps `/data/ledger/state/wall.last` (at most hourly) as evidence across reboots.
- **Service crashes**: core `svcwatch.rs` writes `/data/crashlog/<name>/` (5 per program, 1 MB total) and alerts per section 4; a requested stop or restart is not a crash. The core's own crash is detected by the marker `/data/luyaz/core/core.running`.
- **Modem crashes**: core `crashwatch.rs` (kmsg + netlink), state `/tmp/luyaz/core/crashwatch.json`, events into the longterm store `/data/luyaz/core/long`.
- **`.migrated`**: `/tmp/luyaz/.migrated` is written last by the sentinel, only after `clock-ok` exists and the handover file equals the core's full duty list; ship removes it before starting a sentinel. `/data/luyaz/core/.migrated` is the core layout switch. doctor flags a directory without it as a half-done move.
