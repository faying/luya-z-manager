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
- Both writers (`luyaz-alerts`, `alert-lib.sh`) number past the larger of `seq` and the highest seq in the queue, so an empty or lagging `seq` never reuses a number; a last line left without a newline by a killed writer is ended first, so the new event is never glued onto it.
- Wall-clock under `1704067200` (2024-01-01) means "not calibrated"; the web shows 「开机后 N 秒」.
- Text: printable ASCII, ≤ 120 characters, never any configuration (numbers, passwords, SSIDs). Malformed lines are skipped.
- Kinds emitted now:
  - sentinel: `agent-silent`, `wifi-takeover`, `wifi-restore-failed`, `dropbear-down`, `dropbear-restarted`, `ui-crash`, `ui-gave-up`
  - core svcwatch: `core-crash` (the core itself; `core-crash` before rename F4, old rows still read), `web-crash`, `sentinel-crash`
  - core sentinel watch: `sentinel-silent`, `screen-empty` (only if the screen-guard switch is on)
  - core engines: `home-exit-down`, `home-exit-check-failed`, `sms-forward-stuck`, `sms-forward-queue`
  - touch UI: `ui-theme-paused`

### SMS

- One implementation, `Alerts::sms_round`, two possible senders, never both:
  - **core** (`engines/guard_duties.rs`, every 30 s) when `/tmp/luyaz/handover` names `sms` and writes are on — the normal case;
  - **sentinel** only while the core is unhealthy (2 bad `/healthz` rounds) and not being replaced by ship.
  The `sms-sending` claim keeps a handover between them from sending twice.
- Rules: `sms-failed` and `ui-theme-paused` are never sent; no number → `no-number`; `abroad` without `sms-abroad` → `suppressed-abroad`; at most 1 per kind per hour and 5 per 24 h → else `suppressed-rate`. Nothing is sent and the cursor does not advance until the clock verdict (section 9) is trusted.
- Regress and point events (section 12): a crash row texts its 1st, 2nd, 4th and 8th (`suppressed-repeat` otherwise), the 2nd/4th/8th and regress escalations outside the hourly limit but inside the daily one; a regress or point-event alert the device refuses is retried 5, 10, 15 min after the first refusal before it is `failed`; `rg-…-close` alerts are inbox only; `/data/luyaz/core/regress-sms-off` holds every regress kind back (`suppressed-off`); `wifi-takeover`, `regress-cellular`, `regress-blind` are never texted. Every other kind goes exactly as before.
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
- `/data/luyaz/screen/attempts`: incremented and fsynced before each start, reset after 10 min of stable running; at 2 it writes `/data/luyaz/screen/gave-up`, alerts `ui-gave-up` and hands back to the stock UI. Gave-up survives reboots; cleared by the long press or `ui`. A corrupt file counts as 2.
- `/tmp/luyaz/screen.want` starting with `vendor` = stock UI chosen; a reboot restores ours.
- `/tmp/luyaz-screen.ctl` (FIFO, only while the sentinel runs): `vendor`, `ui`, `hold [s]` (10–3600, default 600), `release` (the last two for ship).
- Unexpected exits of a UI it started go to `/data/crashlog/luyaz-ui/` (5 kept) and alert `ui-crash`; adopted processes give no exit code and are not counted.
- Sentinel heartbeat `/tmp/luyaz-sentinel.hb`, events `/data/luyaz/screen/events{,.seq}` (200 kept), log `/tmp/luyaz-sentinel.log`.
- **If the screen shows no UI for a few minutes, the firmware reboots the whole device** (reason code 1185). Any path that stops one UI must start the other in the same step.

## 7. Health check and backup

- `luyaz-core doctor` (process name `luyaz-doctor`; `/data/luyaz/scripts/doctor.sh` is a thin shim). Read-only. Output: human text, `--tsv` (`<ok|warn|bad>\t<id>\t<名称>\t<说明>`), `--tsv2` (header `#tsv2`, English); exit 1 if any row is bad. Also `--manifest`, `--tree-fp`, `--ledger-selftest`.
- `--calibrate-standby` writes the standby baseline (section 8); `--report 24h|7d [--summary]` outputs the longterm scorecard (`scorecard::live::card`, ported in Core 1.3.0 / `7637b1b1e`). `doctor.sh` only runs `/data/luyaz/luyaz-core doctor`; without a core it exits 2 (the shell fallback `doctor-legacy.sh` was removed in Luya-Z rename F4).
- The core runs the checks itself (first 20 s after start, then periodically) and serves the snapshot at `/api/health`.
- `touch-ui/scripts/config-backup.sh`: configuration only (list at the top of the script), Tailscale identity only with `--with-tailscale`. Backups live only on the computer (`./install.sh backup`); `restore` plans first, leaves `.pre-restore`, does not reload Wi-Fi.

## 8. Standby records

- Writer: core `longterm/standby.rs`, only while `/tmp/luyaz/handover` names `standby`. While the backlight is off, one line per round to `/tmp/standby.stat`: `<uptime> <cellular pkts/min> <Tailscale tunnel pkts/min> <wakeups/s per program>`, last 60 kept; a program whose pid changed is `-`; no line if the previous round is more than 150 s ago.
- The program columns are fixed by position (doctor reads by position) : `tailscaled mihomo luyaz-ui luyaz-core`.
- Fingerprints `/tmp/luyaz/ledger/fp`, `fp-changed`. Baseline `/data/luyaz/scripts/standby.baseline` (v2 with `fp` lines); written by `luyaz-core doctor --calibrate-standby` (≥ 30 records; per column median and MAD, then the fingerprints; one `calib` event via `POST /v2/longterm/calib`), judged by doctor's 「待机」 row.

## 9. Clock verdict, crash records, `.migrated`

- **Clock** `/tmp/luyaz/clock-ok`: line 1 is `<1|0> <wall−uptime> <uptime> <how>` (`vendor|jump|last_wall|year|revoked|none`); line 2 (since `94fec16ec`) is v2 `ClockEvidence` JSON (`raw_wall`, `observed_at` with `boot_id`/BOOTTIME, `monotonic`, `source`, `sync_state`), written by the sentinel and read by the SMS path, the core's longterm store, the scheduler and doctor. v2 readers require both lines to match the current boot within 180 s and ≤ 5 s drift relative to BOOTTIME (until the sentinel is upgraded past `ea7e1e0af` to write line 2, v2 readers treat a single-line file as `Unknown` and hold SMS/calendar actions). The sentinel keeps `/data/ledger/state/wall.last` (at most hourly) as evidence across reboots.
- **Service crashes**: core `svcwatch.rs` writes `/data/crashlog/<name>/` (5 per program, 1 MB total) and alerts per section 4; a requested stop or restart is not a crash. The core's own crash is detected by the marker `/data/luyaz/core/core.running`.
- **Modem crashes**: core `crashwatch.rs` (kmsg + netlink), state `/tmp/luyaz/core/crashwatch.json`, events into the longterm store `/data/luyaz/core/long`.
- **`.migrated`**: `/tmp/luyaz/.migrated` is written last by the sentinel, only after `clock-ok` exists and the handover file equals the core's full duty list; ship removes it before starting a sentinel. `/data/luyaz/core/.migrated` is the core layout switch. doctor flags a directory without it as a half-done move.

## 10. Finding probe budget (P0-02 revision, committed in `94fec16ec`, deployed in Core `5db284761`)

- Core `api/finding/budget.rs` owns `/data/luyaz/core/findings.probe-budget.json`: `{"version":1,"last":{"boot_id":"…","seconds":600}}`. It is independent of the finding store; SIM changes and resolving/dismissing findings do not reset it. BOOTTIME includes suspend; wall time is never used for admission.
- Before any Finding HTTP/DNS request, claim the shared 600-second budget under a nonblocking exclusive lock on `findings.probe-budget.lock`. Re-read the budget under the lock, write `findings.probe-budget.tmp`, fsync the file, rename, and fsync the directory. Any failure prevents packets; the in-memory retry deadline still advances by 600 seconds. No completion write or per-sample budget write. A crash after reservation keeps the budget consumed.
- Same-boot Core restarts retain the reservation. Missing, malformed, unknown-version or future records receive a 600-second hold from Core startup; an unavailable boot clock denies probes. A valid record from another device boot receives a hold until the current boot reaches 600 seconds. A quarantined future record can be replaced after its hold; a newer reservation from another instance cannot be overwritten.
- The existing five-second sampler feeds Finding's passive admission. A gap over 10 seconds, missing/stale required reads, unknown status/counter, counter reset, or a changed SIM/interfaces generation breaks the 60-second observation window. Suspend counts toward budget expiry but does not prove continuous fault observation. Quiet traffic only admits a bounded probe; it does not prove an internet outage.
- `Hub::observations` supplies latest internal data with BOOTTIME and change generations under one lock; `/v2` wire timestamps, revision and publication policy remain unchanged. Every request and final result recheck connection identity/freshness. Changes invalidate the entire result; a saved report's `fresh` also becomes false. Observed A→B→A changes invalidate old context. Transitions never seen by the collector remain unverified; this is not a kernel session identity or complete route proof.
- Only the Finding pilot uses this admission; legacy deep diagnosis, general netwatch status and transaction probes retain their existing behavior. Rolling back to an older binary loses these guards; retaining this file alone does not protect an old binary. Device persistence, suspend and power impact still require separate validation.

## 11. What fault injection covers (offline)

Design and full table: `docs/designs/fault-injection.md`. Each test was checked to fail with its guard removed.

| Fault | Test |
|---|---|
| Wi-Fi lock holder killed -9; child left behind by a holder | `crates/luyaz-wifi/src/tests.rs` |
| Alert queue line torn mid-write; `seq` lost | `crates/luyaz-alerts` tests, `touch-ui/scripts/test/alert-lib/run.sh` |
| Screen: burst of opposite requests, two writers at once (1185 rule) | `crates/luyaz-sentinel/test/real.sh` |
| Kit rc.local write: cat / sync / mv fails; stale temp file | `onboard/test/kit-rc.sh` |
| Ship / update crash at every step, trial and check failure | `touch-ui/scripts/test/luyaz-ship*/` |

Not covered: real power cuts and real Wi-Fi reloads on the device (`touch-ui/scripts/chaos.sh` only kills services, each run with the owner's consent).

## 12. Regress alerts: what each means and what to do

Detectors in the core's long-term thread (`crates/luyaz-core/src/longterm/regress/`, files in `touch-ui/docs/LEDGER.md` §15) and two in the sentinel. An incident opens with one alert (an SMS unless noted) and closes with one inbox-only alert; `tools/luyaz status` lists the incidents opened within an hour of a ship.

| Kind | Opens | Look at | Then |
|---|---|---|---|
| `regress-reboot` | a boot without the planned-reboot mark whose reason code is not manual (1150/1152) or the weekly reboot (1112); 2nd/4th/8th within 6 h escalate | `/v2/incidents/<id>` (code, previous build, ship, drill), `long/events.jsonl` `boot` | a ship named "may be related" → `tools/luyaz rollback <comp>` |
| `regress-mem-abs` | heap > 32 MiB 15 min or RssAnon > 96 MiB 30 min | `long/memsnap-<id>.txt`, `hours.jsonl` `hour_proc` | a leak → rollback core; a legitimate new level → `<core> regress limit mem <heap> <anon>` |
| `regress-mem` | 30 min after core start: heap > 1.5× or RssAnon > 2× the 7-day baseline | same | open 3 days, the inbox asks once (`rg-<id>-ask`, never an SMS); the new level is fine → `<core> regress accept regress-mem` (or the MCP tool on the owner's word) |
| `regress-cellular` (inbox only) | no service, or registered without a data route, 5 min, with SIM ready, data on, data roaming on when roaming | `/v2/device/no-service-history` | the stock firmware recovers or not; nothing to roll back |
| `regress-egress` | CHILL (process down or forwarding broken) or Tailscale failing 5 min while meant on | CHILL page, `tailscale status` | a CHILL/Tailscale change → roll it back |
| `regress-power-abs` | two qualifying standby hours > 600 mW | `hours.jsonl` `hour_power` | find what keeps it awake; a legitimate level → `<core> regress limit power <mW>` |
| `regress-power` | two qualifying hours > max(1.3× baseline, baseline + 100 mW) | same | as `regress-mem` (`accept regress-power`) |
| `regress-recorder` (sentinel) | `long/open.json` unchanged 10 min while the core answers | `/tmp/luyaz-core.log` `[longterm]` | restart the core (`/etc/init.d/luyaz-core restart`) |
| `regress-core-down` (sentinel) | `/healthz` failing 10 min, not during a ship | `/tmp/luyaz-core.log`, `logread` | procd should restart it; if not, rollback core |
| `regress-blind` (inbox only) | a kind could not be judged for 3 days (power: unplugged 6 h in a week and no qualifying hour) | — | nothing; it is a notice that silence there means nothing |

- `<core> regress sms off` (or `tools/luyaz regress sms off`) keeps detecting and filling the inbox, without SMS; `on` reverses it. Both senders read the flag. While it is off, doctor's 短信告警 row is a warning and `tools/luyaz status` says so.
- **Minimum compatible versions, roll back in pairs:** the flag, the 1/2/4/8 rule and the regress texts live in luyaz-alerts, built into both core ≥ 1.45.0 and sentinel ≥ 1.1.0. Rolling one of them back to an older build makes that sender ignore the flag and text regress kinds with the generic sentence: roll core and sentinel back together.
- **Drills:** while the decision loop's test switch `/tmp/luyaz-test-rx-quiet` is on, the core records a drill; an incident starting within the hour after it (same boot), or early in the next boot, names it 「可能相关：演练」. A reboot after a drill is usually the stock cut-off protection (1155), not a regression.
- **Evidence:** `long/incident-<id>.json` is written when an incident opens and again when it ends (end, recovery time, its alerts); `/v2/incidents/<id>` reads it back for 30 days, after the 8-day list let it go (`archived`).
- The core's `/v2/incidents` and the touch screen's `regress` block are read-only; nothing here changes the device.
