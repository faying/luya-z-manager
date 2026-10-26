# Changelog

Each component has its own version ([Semantic Versioning](https://semver.org/)):
patch = fixes, minor = new features (old settings and APIs still work), major = something
must be migrated or several components have to be updated together.

| Component | What it is | Version from |
|---|---|---|
| `core` | `luyaz-core`, the core service (`:9460`) | `crates/luyaz-core/Cargo.toml` |
| `lweb` | `luyaz-web`, the admin web (`:9090`) | `crates/luyaz-web/Cargo.toml` |
| `sentinel` | `luyaz-sentinel`, screen owner and watchdog | `crates/luyaz-sentinel/Cargo.toml` |
| `touch` | `luyaz-ui`, the touch screen UI | `touch-ui/rs/ui/Cargo.toml` |
| `scripts` | device scripts (doctor, init scripts, …) | `touch-ui/scripts/VERSION` |

Each program prints its version with `--version`, e.g. `luyaz-core 1.0.0 (6e606ec)`.
Entries are written under **Unreleased** as `- <component>: text` (e.g. `- touch: …`);
`tools/luyaz bump <component> <patch|minor|major>` moves them into a new version section. `Requires:` lists the minimum versions
of other components.

A Chinese copy is in [CHANGELOG.zh-CN.md](CHANGELOG.zh-CN.md).

## Unreleased

- chill: `rules_drift` no longer fires every minute when a scenario turns Wi-Fi off. The stock firmware adds `iif wlanN failed_policy` rules per interface and drops them with the interface; when it comes back their priority follows the new ifindex. A rule bound to an interface (`iif`/`oif`) now only counts as gone while that interface is up and no rule with the same text is left; any other rule that disappears is reported as before.

- kit: `firmware-capabilities.matrix.json` replaces `device/compat.list`: model / region / build → each component (ssh, admin, ui, esim, chill, recover) `verified`, `code-only` or `unsupported`. `install.sh` checks each picked component on its own: `code-only` still needs `LUYAZ_ALLOW_UNVERIFIED=1`, `unsupported` is skipped with a message and the rest is installed; it stops only when nothing is left. The matrix is in the kit and in MANIFEST.txt.

- kit: install progress per component in `/data/luyaz/kit/progress` (kit id = first 16 hex of MANIFEST.txt's sha256, component, step; written to a temp file and renamed). Running the same kit after an interruption skips the finished components and resumes at the first unfinished one; another kit, a finished run or `KIT_FRESH=1` starts over. `./install.sh status` shows it.

- kit: `backup` writes `<backup>.sha256` (and one for the `.system.tgz`). `restore` checks it before sending anything: a mismatch always stops, a missing one stops unless `LUYAZ_RESTORE_NO_SHA256=1` (older backups); the copy is checked again on the device before verify, plan and restore.

- chill: AI gate (`CHILL_AI_STRICT=1` in chill.env). AI only ever leaves through the region 🤖 AI is set to: whenever CHILL stops (crash, hang, heat, pause, idle, switched off) the AI domains stop resolving in the same dnsmasq restart that hands DNS back, and stay that way until the core checks the cellular exit is in that region. `ai_domain` is now the text list (`ai_domain.list`, same source) so dnsmasq's block file and mihomo's rule come from one list; a built-in short list backs it up. dnsmasq is restarted, not reloaded, when the block changes (reload does not re-read conf-dir).

- kit: `OUI_TSV=` (a table from `scripts/lanid/make-oui.sh`) is installed to `/data/luyaz/lanid/oui.tsv` for vendor names; the public build has none.

- kit: carries the lanid-parse component (LAN client identification) and installs it to `/data/luyaz/core/plugins/` as the only component; without it the core runs with identification off. `LANID_CWASM=` uses a ready-made one.

## core 1.49.1 — 2026-10-26

- keeps the sentinel's `regress-disk` incidents ("Storage low"); alerts: its SMS and labels.

## sentinel 1.2.0 — 2026-10-26

- restarts the core (`/etc/init.d/luyaz-core restart`) when it has been down 10 min (`regress-core-down` opens), then after 10, 20, 40 min and hourly while it stays down; never while a ship replaces it. procd gives up after 5 crashes and nothing else brought it back (10-26: down 50 min after an ENOSPC crash loop). The incident's close says how many restarts it took.
- watches /data and /tmp every round: below a twentieth of the space (at least 32 MB) or of the inodes it opens `regress-disk` (SMS) with the room left and the five biggest entries two levels down; it closes at twice that. In the sentinel, not the core: a full /data is what stops the core.

## scripts 1.3.3 — 2026-10-26

- scripts: `luyaz-core.init` takes `LUYAZ_NETREC` from core.env (the recorder's switch, core 1.49.0).

## sentinel 1.1.3 — 2026-10-26

- the screen log and the worker log are also kept on flash, `/data/luyaz/screen/log` (`<boot id 8> <screen|worker> <line>`, each line synced, 64 KB then one `.old`), so the lines before an unexpected reboot can still be read after it.

## scripts 1.3.2 — 2026-10-26

- when `luyaz-ship.sh` cannot hand the screen back (the live UI does not come up, or the sentinel is down), it no longer leaves the panel dark until the firmware reboots (1185, u60g 2026-10-25): with no UI on screen it has the sentinel put up the vendor UI (release, then vendor), or starts a sentinel that is down so it launches a UI; a UI already on screen is left alone and the ship still fails with the original reason. The next release sends `ui` first when that vendor request is still in force.

## core 1.49.0 — 2026-10-26

- netrec, the network recorder (2a, first batch; docs/designs/net-performance.md「2a 记录仪细节设计」). Off unless `LUYAZ_NETREC=1` in core.env. It rides the collect round with what that round already read (vendor `get_wwandst` counters, `rmnet_data0`, LAN + Wi-Fi client lists, nwinfo): no timer, thread or ubus of its own, and an idle round does no extra reads or writes. A session starts after 3 rounds ≥ 1 Mbps with clients (a failed client read counts as clients) and stops after 60 s < 0.5 Mbps, when the clients are gone, or at 2 h. Device-local traffic does not count: `local = Δrmnet_data0 − Δchill0`, `chill0` is read only in a session or when the vendor rate alone reaches 1 Mbps. Each round in a session → a 160-byte row in `/tmp/luyaz-netrec.ring` (2 MiB, wraps, resumes after a restart); every 10 s a summary line (rate p50/max, direct/proxy/local bytes, SNR range, cell/band/CA events, CPU) plus start/end lines → `/data/luyaz/core/netrec/YYYYMMDD.jsonl`, written at 4 KiB or session end with one fsync; ≤ 1 MiB a day (past it only start/end, the session marked `trimmed`), 8 MiB in all (oldest day first). No addresses stored; not in the event log or the shareable bundle. API: `GET /v2/netrec/sessions`, `GET /v2/netrec/summary?since=&cursor=&limit=` (JSONL, `x-netrec-next`), `GET /v2/netrec/ring?session=&offset=&limit=`. Doctor: a 记录仪 row (on/off, KB written today) when it is on or has files.

## sentinel 1.1.2 — 2026-10-26

- the same SMS fixes (time only on a settled clock, Chinese regress texts without English details); agent-silent is not raised while the core is down (regress-core-down says it; the Wi-Fi rescue still runs).

## core 1.48.3 — 2026-10-26

- alert SMS fixes from the 10-26 real-device tests. The time is printed only once the clock has stepped past the wall the device booted with (`last_wall`); before that the SMS has no time (a reboot SMS said 01:58 at 09:58). A Chinese regress SMS no longer carries the English details (codes, builds, /healthz): only "a ship just before it, may be related"; the English SMS keeps them. agent-silent has its own words.

## core 1.48.2 — 2026-10-26

- an incident's evidence gives `recovery_s` only when a signal said it was over (`end: recovered`); a crash or reboot whose window just passed (`quiet`) no longer shows the 6-hour wait as a recovery time. Evidence already on the device is rewritten at start.

## scripts 1.3.1 — 2026-10-25

- `luyaz-ship.sh` waits up to 20 s again for the live UI after the sentinel takes the screen back. The rename in c622e515 dropped `UID_UP`, so the wait was 0 s: a UI that was one second late failed the touch ship and its rollback (u60g, 2026-10-25, followed by an unexpected reboot).

## core 1.48.1 — 2026-10-25

- speed test verdict: lag under load names the stage (「下载时」/「上传时」) and also counts a stage whose probes were too slow to reach 20 samples when at least 5 are already 100 ms over idle (first device run: upload probes took ~1.8 s each and the row read "ok"). SINR rounding never shows "-0".

## lweb 1.34.1 — 2026-10-25

- Speed test page English follows the glossary (§17): the layer section is "Layers", too few samples reads "Too few samples; rough figure".

## touch 1.15.0 — 2026-10-25

- Speed test page for speed test v2 (net-performance DR1–DR13): the big number is the multi-connection stable rate with a conclusion line under it, a layer section of five rows below the card (the proxy and weak-radio rows open the exit and placement pages), step-numbered stage text while running, a grey number for too few samples, a rough-test line when coarse, a roaming hint with tap-again, an "also via CHILL" switch (off by default, back off after each run, grey when CHILL is not running or while roaming), the data cost before and the data used after. Against an older core the page is unchanged.

## lweb 1.34.0 — 2026-10-25

- /tools/speedtest reads the v2 result (net-performance T10b): the state says the step (「3/5 · 多连接下载」) and the measuring method while running, and the conclusion when done (「完成 · ▲ 慢在单连接」); only these are announced, not the percent. The fourth readout is latency while downloading (jitter moved to the details); a 「分层」 list (single connection, proxy, lag under load, radio, cell and Wi-Fi) follows the readouts, the proxy row links to CHILL when slow; the details add the whole-run average, jitter, connections and 内核调参：开 / 关 (read only, as at the test). A short sample greys the download number (「▲ 样本不足，数字只供参考」); a coarse run says 「● 粗测 · 漫游中只测 5 秒」. New switch 「也测经 CHILL」 (once per start, off while roaming or with CHILL not running); the start confirm states the new cost (about 30 s / 750 MB, 40 s / 1.15 GB with CHILL, 5 s / 30 MB while roaming). With a core before 1.48.0 the page looks as before. The full layout needs core 1.48.0.

## core 1.48.0 — 2026-10-25

- speed test v2 (net-performance 1a, method 2): idle latency 2 s, one long single-connection download 8 s, a 6-connection download 10 s with a latency probe every 200 ms on its own kept-alive connection, and a 6-connection upload 10 s of fixed-size POSTs (only acknowledged ones count). Each stage drops its first 2 s; `download_mbps`/`upload_mbps` are now the multi-connection stable rates, beside `*_whole_mbps`, `single_mbps`, `idle_ms`/`loaded_*_ms`, `cpu_peak_pct`. Request timeouts follow the stage (no 3 s reconnects); every byte is reserved before it is read: 200/400/150 MB per stage, 750 MB in all, cut upload → single → multi when short. A stable part under 3 s (1 s coarse) is `sample_short`. Auto picks among the 3 lowest-latency nodes by a 1 s, 2-connection trial and drops nodes still at 0 bytes after 2 s; the diagnosis' capped run (`mode:"coarse"`) reuses that node on the same cell for 30 min. `verdict` (single connection / proxy / lag under load / radio, Chinese and English) when the run is full and long enough.
- `POST /api/speedtest/start` takes `path:"chill"`: the same node is measured again through CHILL's 7894 listener right after the direct download (+400 MB); `chill_region` is only the main exit's region (「直连」 on DIRECT). CHILL not running or stopping midway is reported (`chill_not_running`/`chill_stopped`), never 0 Mbps and never retried direct. A roaming device (the signal block's `roaming`, as ship's gate reads it) runs the capped test (`roaming_capped:true`). `net_tune` and `radio` record the state at the start. Old fields keep their names and types; `phase` keeps its values (the new sub-stage is `stage`, `step`/`steps`).
- `GET/PUT /api/net-tune` `{"on": bool}`: writes or deletes `/data/luyaz/net-tune.want` and runs `net-tune.sh apply`/`restore` (child process, 8 s cap over the script's own 5 s lock wait), replying with its output; GET answers `net-tune.sh show`.

## scripts 1.3.0 — 2026-10-25

- `net-tune.sh apply [iface]|restore|show` (net-performance 1b): CPU-path network tuning, on while `/data/luyaz/net-tune.want` exists — `tcp_slow_start_after_idle=0`, `tcp_rmem`/`tcp_wmem` max 16 MB, `rmem_max`/`wmem_max` 8 MB, `tcp_notsent_lowat=131072`, `netdev_max_backlog=4096`, RPS mask `e` on br-lan/chill0/tailscale0. Each item's original value is recorded the first time it is seen (`/data/luyaz/net-tune.baseline`, checked against the stock values; a different value is refused); `restore` puts back every recorded item; one `flock -n` lock, 5 s. `luyaz-core.init` runs `apply` before the core (subprocess, 5 s cap); `doctor.sh` (no arguments) prints `show`.

## core 1.47.1 — 2026-10-25

- APN page follows the data card on a dual-SIM unit: with data on slot 2, `get_auto_apn_list` is read with `slotId` (without it the firmware answers slot 1's list), and in automatic mode a dialled profile that is in neither of this card's lists (`get_apn_at_cid` keeps the old card's) gives way to the card's enabled candidate. The ICCID fallback for the per-card pick reads `sim2_iccid` on slot 2.

## sentinel 1.1.1 — 2026-10-25

- built with luyaz-alerts that keeps `rg-…-ask` inbox-only like `rg-…-close`. Requires: core ≥ 1.47.0 for the asks themselves.

## lweb 1.33.0 — 2026-10-25

- /alerts incident rows say 「可能相关：演练」 for a drill in the hour before (same boot); the details have 可能相关的演练; the Agent API passes `drill`, `archived`, `recovery_s`, `boot_code`.

## core 1.47.0 — 2026-10-25

- regress incident evidence finished (design 「變差才通知」 X5): `long/incident-<id>.json` is written again when an incident ends (end, recovery time, its alerts), names the boot's reason code, a drill and the exits (CHILL / Tailscale seconds on and fine per hour) beside memory and standby power; an incident begun before the clock was trusted gets its 6 hours once it is. `/v2/incidents/<id>` reads one the 8-day list let go back from its evidence for 30 days (`archived`, `recovery_s`, `boot_code`). Evidence written by an older core only when it opened is written once more at start.
- drills: while the decision loop's test switch `/tmp/luyaz-test-rx-quiet` is on, a drill is recorded (`incidents.json` `drills`, the last 20); an incident within the hour after it in the same boot, or early in the next boot, carries `drill` (like `ship`).
- a `regress-mem` / `regress-power` incident open 3 days asks once in the inbox whether its level is the new normal (`rg-<id>-ask`: builds and level, `regress accept <kind>`); never an SMS, never accepted by itself.
- `doctor --ledger-selftest` checks the core's long-term directory, not the retired `/data/ledger`.

## core 1.46.2 — 2026-10-25

- the deep diagnosis reads only the `diagnose` and `speed` events of its 7 days of signal history, line by line; it used to read the week's minute records as well and drop them, about 20 MB of heap for a second (u60g 8 → 28 MB).
- QoS keeps only the `[DATA]` lines while reading key.log's 2 MB tails, instead of both tails as text every 30 s.

## scripts 1.2.1 — 2026-10-25

- `luyaz-ship.sh` keeps `ship.log` and `test.log` under 256 KB each: at stage (under the lock, nothing running) one over it moves to `<log>.old`, the one before is dropped (`U60S_LOG_MAX`).

## core 1.46.1 — 2026-10-25

- doctor's 短信告警 row warns when regress SMS is off (`/data/luyaz/core/regress-sms-off`; inbox only, `tools/luyaz regress sms on`); a warning, the exit code stays 0.

## core 1.46.0 — 2026-10-25

- the heap allocator is mimalloc (still under the heap counters). On musl's own allocator the core's RssAnon went 33 → 82 MB in 8 h while the live heap stayed at 8 MB: freed memory stayed resident. Third-party list: mimalloc (MIT).

## lweb 1.32.0 — 2026-10-25

- /alerts: 「事故 · 7 天」 between the status and the event list — the core's incidents (open ones first, then those ended in 7 days, at most 50), each opening its details (what else was seen, start/end, a ship that may be related, the memory snapshot, its alerts). An alert of an incident drawn there leaves 「记录」 and marks its incident unread; nothing is hidden when incidents cannot be read or the core is older. The status block leads with open incidents.
- `/agent-api/incidents`, `/agent-api/incidents/<id>`; `/agent-api/act/regress-accept` (only with `instruction: owner`).
- Requires: core 1.45.0 (older cores: 「core 版本太旧，不支持事故」).

## touch 1.14.2 — 2026-10-25

- Home: the status card's speed wave also shows in dual standby (it was hidden there for lack of room); it stays at the body's foot and may overlap the speed line slightly.

## touch 1.14.1 — 2026-10-25

- 摆放模式: a cell is its PCI (and 4G / 5G), not band · PCI: with carrier aggregation the modem files the PCI under whichever carrier's band, so one cell read as n78·603, n5·603 and n1·603 and counted as returns.
- 摆放模式: 「卡在两个基站之间」 counts at the current spot only (taking a place starts it again; walking between places is not switching back and forth) and headlines only while the score is under 60 (良好): at a good score the switching costs nothing worth moving for. The line names the two cells: 「在小区 603、615 之间来回切了 5 次，往一边挪。」
- 摆放模式: the cell in use is on the chart (「近 3 分钟 · 小区 603」), the cell-change note names the new one, and a place on another cell shows its PCI instead of 「另一小区」.
- 摆放模式: places tied on score go to the later one, the one last called best (it had said 「就放这里」 at place 3, then 「放回第 2 处」 at place 4 with 2 and 3 equal).

## touch 1.14.0 — 2026-10-25

- The top bar's ▲ also lights for an incident the core says is open (fresh data only); its tap box is 44×44 beside the rate's, never over it. 健康与告警 starts with 事故: open ones (≤5), those ended in 24 h (≤3), what else was seen; stale, never read and an older core are said, not hidden.
- 网络诊断's 再查一次 moved from the title bar into the page (the ▲ would cover it).
- `luyaz-ui --hitcheck` (rs/hitcheck.sh): every page, nothing tappable under the ▲.
- Requires: core 1.45.0 for incidents (older cores show 「core 版本太旧，不支持事故」).

## core 1.45.0 — 2026-10-24

- Tell me when it gets worse (regress): incidents from what the core already reads, in `long/incidents.json` — memory (against a 7-day baseline and absolute: heap 32 MiB / RssAnon 96 MiB), cellular down 5 min (inbox only), CHILL / Tailscale failing 5 min, standby power (two qualifying hours over 600 mW or over the baseline), unexpected reboots (no planned mark, not a manual or weekly code), and crash rows merged from the alert queue. Each opening is one alert (an SMS with the details), each recovery one inbox-only alert; point incidents end "quiet" after their window.
- `/v2/incidents`, `/v2/incidents/<id>` (alerts, evidence, memory snapshot), `/api/alerts` events carry `incident`, a `regress` block in `/v2/state` (open ≤ 5, ended in 24 h ≤ 3).
- `<core> regress accept regress-mem|regress-power`, `regress limit mem|power …`, `regress sms on|off`; `regress.accept` on `/control` (an agent only with `instruction: owner`).
- The scorecard reads `long/` alone when there is no `cutover` (newer devices no longer say "no ledger records"); doctor no longer prints the old ledger's summary.
- Requires: sentinel 1.1.0 (roll back together: both carry the SMS rules).

## sentinel 1.1.0 — 2026-10-24

- `regress-core-down`: `/healthz` failing 10 min (not during a ship) is queued and texted by the sentinel; `regress-recorder`: `long/open.json` unchanged 10 min while the core answers. Both close with an inbox-only alert.
- SMS rules shared with the core: a crash row texts its 1st/2nd/4th/8th, refused regress alerts are retried, `regress-sms-off` holds regress kinds back, `wifi-takeover` is no longer texted.
- Requires: core 1.45.0.

## scripts 1.2.0 — 2026-10-24

- `luyaz-ship.sh` writes `act_boot` / `act_uptime` (when the new version went live) into the transaction and the manifest's ship line; the core names ships by it.

## touch 1.13.5 — 2026-10-25

- 摆放模式: 「卡在两个基站之间」 counts only returns to a cell left in the last 2 minutes (3GPP's ping-pong), not every change: walking through new cells no longer trips it, and readings mid-handover (PCI 65535, no band) are ignored. One walk on u60 logged 30 changes, many of them band and PCI updating a snapshot apart. Returns are marked `back` in the log.
- 摆放模式: the score card is five columns, each a number, its name and what it means: 总分 (the big number, with its tier), SINR (dB, its points), 频宽 (MHz, what it takes off), 信号强度 (RSRP, 正常 / 弱), 基站负载 (RSRQ, 不忙 / 忙). The total = SINR's points − the width's.
- Home: the speed wave leaves idle out (at or under its 10 KB/s floor) as a gap; a flat green line along the bottom read like a broken screen.

## touch 1.13.4 — 2026-10-24

- 摆放模式: the score is 3GPP's SINR-to-throughput curve (TR 36.942 Annex A.2: 0.6·log2(1 + SINR), from −10 dB, capped at 4.4 b/s/Hz ≈ 22 dB) × the primary carrier's width ratio (throughput is linear in bandwidth, TS 38.306 §4.1.2). RSRP and RSRQ are no longer multiplied in: SINR already has a weak signal's noise and the interference in it, and no published model turns them into a throughput factor; they name the reason instead (RSRP ≤ −105 weak, RSRQ ≤ −15 busy). The live SINR is smoothed (time constant ≈ 3 s), so the score glides instead of jumping when the modem steps a reading. The equation card: SINR score − width = the big number, then strength and load as words (正常 / 弱, 不忙 / 忙) with their readings.
- Home: each carrier under the status card carries its width ("n5 15M"), and the bars are drawn against 100 MHz (or the total, when wider), so n5 15 MHz is a short bar and n78 100 MHz a full one; the width stays visible when 查原因 › takes the right side.

## touch 1.13.3 — 2026-10-24

- 摆放模式: full marks at SINR 20 dB instead of 25 (indoor spots topped out in the 40s and barely moved): 13 dB ≈ 60, about 6–7 points per dB; tiers 80 / 60 / 40, places within 8 points are the same, 「放哪都不行」 under 30. A second line says why: the biggest deduction with its reading (「差在干扰：SINR 9.6 dB，转个方向或靠窗」); while walking, 「比 5 秒前 +8」. Record cells name what holds a place down (「低 48 · 干扰」).

## touch 1.13.2 — 2026-10-24

- 摆放模式: the width is the primary carrier's own (SA the NR cell, 4G the LTE cell, NSA anchor + NR leg); secondary carriers no longer count, since the base station adds them only for a download. NSA reads quality from the LTE anchor only, so the score no longer jumps when the NR leg's SINR comes and goes. The equation card shows points taken off (「55 · 0 · −6 · 0」) that add up to the big number, which is now this second's score.

## touch 1.13.1 — 2026-10-24

- 摆放模式: a cell change counts only when the new band · PCI holds for two readings and has a PCI (a one-off reading no longer reports 「卡在两个基站之间」); each counted change is logged.

## touch 1.13.0 — 2026-10-24

- 摆放模式 (Placement) redone: a 0–100 score = the share of this link's fastest speed (SINR's ceiling × strength, load and relative-bandwidth discounts, shown as an equation), the score's last 3 minutes on the status card with recorded places pinned on it, and up to three places compared: go back to spot N, they're all the same, no spot works (try another room or SIM), or not a placement issue (busy cell; opens the speed test). Switching back and forth between cells is called out first. Places are 「第 N 处」 with their time: the device has no compass or motion sensor.

## touch 1.12.6 — 2026-10-24

- System: the 5-minute speed chart reads like the status bar too (numbers, unit setting; axis marks 80Mb / 800Kb in bits).

## touch 1.12.5 — 2026-10-24

- Home: the status card's speed reads like the status bar: same scale and unit setting (112 Kbps, not 0.1 Mbps), and upload has its own unit.

## touch 1.12.4 — 2026-10-24

- Home: the signal word and 干扰 · 负载 are two small lines beside the verdict (no service: how long). The 5-minute speed chart moved from Home's second screen to System (近 5 分钟), above CPU and memory; Home keeps the status card's wave.

## touch 1.12.3 — 2026-10-24

- Home: 信号 · 干扰 · 负载 sit beside the verdict (the space right of 顺畅 was empty); the load drops first when it is short of room, then the noise. The speed wave takes the freed line and is taller.

## touch 1.12.2 — 2026-10-24

- Home: the signal word leads the noise · load line (信号强 · 干扰中 · 负载正常) and the SINR ring sits centred; under the ring only while a hint takes that line. Home's scenario card and cells lose the dark theme's 1 px top highlight (it read as a stray white line).
- status bar: the upload no longer drops out first when the ▲ is up: a tighter form (no space, no unit letter) comes before 「↓ only」, and the ▲ takes only its own width. The short form keeps one decimal under 10 M, so it reads as Home's speed (2.3M, not 2M).
- the speed chart's upload line is solid (dashed, it vanished under the download fill) and the axis starts at ~1.6 KB/s (the upload sat on the floor).

## touch 1.12.1 — 2026-10-24

- Home's battery cell said 「充电中」 on battery: it now goes by the charger, as System does.

## touch 1.12.0 — 2026-10-24

- Home answers three questions on one screen (DESIGN.md §4): a status card tinted by the verdict (SIM line, or two in dual standby; speed with a wave, or the hint in up to 3 lines; SINR ring; carriers with their bandwidth, or what to do: 查原因 / 详情 / 换这张上网 / 去启用), a scenario card (scenario, mode, why; CHILL with one line per traffic class and its latency; Tailscale with the path to the home exit) and a bottom row (Wi-Fi · today · battery to the minute, or a notice over the first two cells). Fixed slots: nothing moves when content changes. The speed chart, landing and Tailscale details are below the fold. The home exit state and the battery estimate are now read while Home is lit.

## core 1.44.2 — 2026-10-24

- the journal merges identical repeated writes that change no undoable item: the first is written, repeats are counted and summed into one line (`repeat`, `since`) when something else is written or once an hour. The scenario engine's every-15-s `enableAutoSleep` refresh had filled the 512 KiB journal in about a day and a half, pushing real changes and `took_ms` samples out. That call also no longer clears the whole state cache.

## scripts 1.1.0 — 2026-10-24

- `config-backup.sh` verify / plan / restore refuse the archive when `CB_EXPECT_SHA256` is set and the archive's sha256 differs.

## core 1.44.1 — 2026-10-24

- `/api/netinfo/neighbors/scan`: reads the whole `manual_scan` section; with the option unset (SA) `uci get` of the option answers nothing, which came back as a 502 instead of `not_reported`.

## core 1.44.0 — 2026-10-24

- SMS forwarding: a persistent delivery queue (`/data/luyaz/core/sms-forward-queue.json`, saved with fsync on every change): one job per SMS × rule with its own state, tries and next try (30 s doubling to 15 min). A rule whose target is down no longer holds back the other rules or later SMS (the cause of "only the first one arrives": one transient failure used to stop everything behind it for up to 6 h). Pending jobs resume after a restart, a rule that got an SMS never gets it again, and an SMS is known by sender + date + text, so one renumbered by a SIM re-read is not sent twice. Given up after 24 h online or when the queue is full (alert `sms-forward-stuck`); mark-read / delete only after every rule succeeded and only if the id still holds that SMS. `GET /api/sms/forward/config` adds `queue` (pending / sent / failed / next_at).
- `/api/netinfo/neighbors/scan` (GET or POST) reads the LTE neighbours the modem already reports (uci `zte_nwinfo.manual_scan.lteg_nbr_content`) instead of answering 410: `{ok, status: "ok" | "not_reported", cells: [{pci, earfcn, band, rsrp, rsrq}]}`, the serving and CA cells left out, values out of range `null`. No scan, no write; empty in SA.
- Wi-Fi master switch (`wifi.set_module`) reads back `wifi_onoff_by_user` and fails when it does not show the wanted value within 15 s (the stock call can answer success without moving it; on B31 switching on takes ~6 s).

## core 1.43.1 — 2026-10-24

- AI gate: switching 🤖 AI to another region drops the old verdict (a stop would otherwise have trusted it); a mode switch from the CHILL panel (which does not write the mode file) is written to it once seen, so chill.sh and the core agree.

## core 1.43.0 — 2026-10-24

- heap counters for the slow RSS growth between restarts: a counting wrapper over the system allocator; each new 4 MB high mark writes `[mem] live … kB; thread …; last <path> …s ago` to the core log, and `hour_proc` gains `anon_core` (RssAnon), `heap_live` and `heap_peak` (kB). No new thread or timer.

## core 1.42.1 — 2026-10-24

- the AI gate tells "AI goes through mihomo" from dnsmasq's running config (`server=127.0.0.1`) instead of chill.state, and does not close after its lookups when CHILL took DNS over meanwhile (on u60g a start was closed again for up to a minute).

## touch 1.11.1 — 2026-10-24

- Home's exit row is two lines tall while the AI gate's line shows (it was hidden under 流量).

## touch 1.11.0 — 2026-10-24

- Home's exit row says when AI is paused (exit not in the AI group's region) or checked.

## lweb 1.31.0 — 2026-10-24

- Home's CHILL card says when AI is paused (exit not in the AI group's region) or checked.

## core 1.42.0 — 2026-10-24

- AI gate engine: watches whether AI goes through 🤖 AI (CHILL running in rule mode); otherwise (CHILL down, mode `direct` or `global`) keeps the AI domains blocked until three country lookups (Cloudflare trace, ip-api, ipinfo; at least two answering, all the region) place the exit in 🤖 AI's region, tied to this boot and the cellular route (a redial or another SIM checks again). Lookups only with somebody connected, backing off. `services.chill.ai_gate` shows it.

## core 1.41.0 — 2026-10-24

- USB epoch producer: a kernel uevent socket (NETLINK_KOBJECT_UEVENT); the USB epoch is minted after a `/sys/class/udc/*/state` snapshot taken once the socket is bound, and renewed on every UDC/USB/Type-C/charger uevent. A SEQNUM gap, a lost message or a read error revokes it until a new snapshot; the vendor Type-C/power-bank events still revoke and renew it. Still not authoritative.

## core 1.40.0 — 2026-10-24

- ubus epoch producer: the listener also follows ubusd `ubus.object.add`/`ubus.object.remove`; the ubus epoch is minted once the probe confirms the subscription and renewed on every add/remove of an object a ubus-bound capability uses. Still not authoritative.

## lweb 1.30.1 — 2026-10-24

- the top bar and the mobile network page no longer show "connected" when the modem claims connected without an IPv4 address (B31 keeps claiming it after a drop); they show it as unconfirmed, in neutral.

## core 1.39.0 — 2026-10-24

- turning direct supply on or off also sets `zwrt_deviceui.Device.direct_power_mode_switch` ("1"/"0", only when it differs), so the stock UI shows the same state; a failed sync is a warning.
- SMS forwarding no longer stops after the first message: when the hub SMS summary has not changed it reads the inbox directly; a message dated more than 2 days ahead no longer blocks newer ones.
- new capability item `wwaniface_trustworthy` (Unknown on B31: `get_wwaniface` keeps saying connected after a drop); the long-term link log counts "connected" without an IPv4 address as down.

## core 1.38.1 — 2026-10-24

- a Finding (no-internet check) could never open while a device was connected: the LAN traffic counters in the interfaces block counted as a connection change and restarted the 60 s hold on every refresh. Counters no longer count; any other change, changing back, or a failed read still does.

## lweb 1.30.0 — 2026-10-23

- the eSIM page says when pending receipts will be sent again automatically, or that automatic re-sending has stopped and they need sending by hand. Requires: core 1.38.0.

## core 1.38.0 — 2026-10-23

- eSIM receipts (operator notifications) are sent one at a time, so a dead server no longer holds back the others; whatever is left is re-sent on its own 2 min after a trigger (core start, profile switch, download/delete, the page's button), then after 10 min, 30 min, 2 h and 6 h, then it stops until the next trigger. Nothing pending means no timer and no card reads. `GET /api/esim/notifications` adds `resend {next_s, gave_up}`.
- alerts: a queue line left half-written by a killed writer no longer swallows the next event, and an empty `seq` file no longer restarts numbering at 1 (same fix as sentinel 1.0.12).

## core 1.37.1 — 2026-10-23

- decision loop test switch: while `/tmp/luyaz-test-rx-quiet` exists and is under 10 minutes old, moving receive counters do not count as "still receiving", so a real-device test can stage an outage (deleting the data address alone leaves the counters moving). RAM disk, self-expiring; nothing else changes.

## sentinel 1.0.12 — 2026-10-23

- alerts: a queue line left half-written by a killed writer no longer swallows the next event, and an empty `seq` file no longer restarts numbering at 1 (the SMS and read cursors skipped the new events).

## scripts 1.0.10 — 2026-10-23

- `alert-lib.sh`: same fix as sentinel 1.0.12 (torn last line, lost `seq`).

## lweb 1.29.1 — 2026-10-23

- small buttons keep a 44 px tap area (36 px to look at); reduced motion stops only what moves (switch thumb, progress bars) and keeps colour fades; the ⌘K search field shows focus.

## touch 1.10.10 — 2026-10-23

- Screen off: redraws while dark no longer go to the panel; the black frame from 1.10.9 stays until the screen lights up.

## touch 1.10.9 — 2026-10-23

- Screen off: the panel gets one black frame after the backlight goes off (before, only the backlight went dark and the last screen stayed faintly readable in bright light); lighting up redraws the whole screen before the backlight comes on, also when the vendor key daemon lit it.

## lweb 1.29.0 — 2026-10-23

- `/agent-api/sim` (MCP `get_sim_identity`, luyaz-mcp 0.8.0): whether the SIM card changed (`epoch`, `phase`), its state, PIN status, network name and, on a dual-SIM unit, each slot; never the ICCID, IMSI, phone number or ICCID tail.

## core 1.37.0 — 2026-10-23

- the deep diagnosis also runs bound multi-target HTTPS (verified TLS over the cellular IPv4 path, beside the path probe, 16 s cap). Internet warns when plain HTTP answers but no HTTPS site opens (a sign-in page or interception) or certificates do not verify; Device clock first uses the verified `Date` headers of two sites.

## lweb 1.28.1 — 2026-10-23

- /tools/agent says band and cell locks are not done while roaming or abroad.

## core 1.36.1 — 2026-10-23

- band and cell lock changes by the AI agent are also sensitive abroad (the registered network's MCC `rmcc` is not 460, a local SIM included) or when it cannot be read.

## lweb 1.28.0 — 2026-10-23

- `/agent-api/act/link`: band and cell lock changes from the AI agent, passed to the core with the owner's-instruction marker; change rows label them 「Agent 执行 · 来源：你的指令（未验证）」; /tools/agent lists the band & cell lock scope and how it rolls back.

## core 1.36.0 — 2026-10-23

- the AI agent may run band locks, band reset and cell locks (the actions whose rollback is proven on the device) only on the owner's instruction (`instruction: "owner"`, otherwise 403 `needs_instruction`); refused while roaming or when roaming cannot be read (sensitive, web page), refused when Tailscale is not online beforehand, one per 30 minutes for the whole class (layer `link`), always a trial that is kept only when the device itself sees mobile data (DNS) and Tailscale online; the agent-change event says 「来源：你的指令（未验证）」 (u60-mcp.md D-g).

## lweb 1.27.1 — 2026-10-23

- `/agent-api/no-service-history` reaches the core (the path was missing from the core allow-list, so it answered "core down").

## core 1.35.0, lweb 1.27.0, mcp 0.6.0 — 2026-10-23

- core: read-only stock-firmware anomaly checks on the existing minute tick (R18-P2-01, docs/designs/stock-anomaly-recovery.md). `wifi-post-stuck`: ZTE's `zte_post_wifi.sh` alive over 30 s with enabled main*/guest* ≠ running wps `hostapd_cli`, two ticks in a row (never killed). No-service ledger `/data/luyaz/core/no-service.jsonl` (200 rows): each outage with what the stock firmware was seen doing (lpm_cycle / sim_reset / redial / unknown); `no-service-long` when an outage passes 5 min with nothing seen. Both alerts go to the inbox only, no SMS. `/v2/device/capabilities` gains `stock_anomalies`; new `/v2/device/no-service-history`.
- lweb: `/agent-api/capabilities` passes `stock_anomalies` field by field; new `/agent-api/no-service-history`. Requires core 1.35.0.
- mcp: new read-only tool `get_no_service_history`. Requires lweb 1.27.0, core 1.35.0.

## lweb 1.26.1 — 2026-10-23

- SMS forwarding rule form names the missing field instead of "fill in the destination fields"; a Telegram Chat ID must be a number or @channel and a forward number digits with an optional +; the ntfy server field opens the URL keyboard.

## touch 1.10.8 — 2026-10-23

- Dark screen: the periodic refresh (and the background reads it starts) runs every 5 s instead of every second; lit again, it goes back to 1 s and refreshes at once. The core's pushes still paint at once (R18-P1-04).
- Periodic reads and core reconnects run on bounded background workers; a finished change wakes the UI; HTTP reads have a total deadline.
- CHILL: empty and unavailable states, long labels and touch feedback polished.
- Theme: every text colour is checked for 4.5:1 contrast on its surfaces in both themes (test); glass rims drawn by one helper (no visible change).

## core 1.34.1 — 2026-10-23

R18-P1-04: fewer idle wakeups in the core (about 2.6 timer wakeups/s less); no behaviour change.
- scheduler, health: sleep until the next tick / due run instead of a 2 s / 1 s look.
- charge: the policies' switch is applied once at start (it cannot change while the core runs).
- scenario watchdog: looks every 30 s instead of 2 s (a wedge is 15 min).
- SMS forwarding: woken when the `sms`, `signal` or `interfaces` block changes; otherwise every 10 s (bearer, SIM switch hold) instead of 2 s.

## lweb 1.26.0 — 2026-10-23

- lweb: /router/esim shows a switch that is waiting out a busy eUICC (catBusy retries, core P0-3): tries so far, the next try's time and roughly when it gives up (device local time), with a **Stop retrying** button (tier 1: it only ends the waiting, the card keeps its old profile; `POST /api/esim/cancel`). The press answers at once ("Stopping the retries…"); the switch's reply then ends with "retries stopped". Too late (the card already took the profile) or nothing to stop is said as such. Requires: core 1.24.0 (`attempts`, `/api/esim/cancel`).

## core 1.34.0, lweb 1.25.0, mcp 0.5.0 — 2026-10-22

R18-P1-07: two more read-only MCP tools, from what the device already has (no new probe).
- core: `GET /v2/recovery/actions`, the action table in its own words: per action the confirm rule, completion evidence, rollback capability and when it rolls back (loop execution, agent write, person write), who may approve it (person, the agent's layer and 30-minute limit, the loop's allow-list), plus every other `/control` write as `direct` (no transaction, no rollback), `limitations` (the web confirm tiers are per page, not here), the allow-list itself (empty) and the loop's missing entry conditions. Built from `ops::spec`, `api::execute::ALLOW` and `ops::agent`; static.
- lweb: agent API `/agent-api/capabilities[?capability=<prefix>]` (the passive firmware capability report cut to a field whitelist, under 16 KB with `truncated`; an undecodable report is `unverified`) and `/agent-api/recovery-actions` (an older core: `not_described`).
- mcp: tools `get_firmware_capabilities` and `explain_recovery_action`; a good answer (state ok) is reused for 30 s per device (`gateway_cache_age_s`); core down / unverified / not described always ask again. Requires: lweb 1.25.0, core 1.34.0.

## core 1.33.7 — 2026-10-22

- doctor reads ship-manifest record names as written; the pre-rename names (`devui-fonts`, `devui-logos`, `u60d-plugins`, `u60-recover.sh`) are no longer mapped (neither device has them).

## core 1.33.6, sentinel 1.0.11, touch 1.10.7, scripts 1.0.9 — 2026-10-22

The old "devui" names are no longer read (devui cleanup B5); both devices were migrated with the previous round.
- touch: reads only `ui.conf`; no copy from `devui.conf`.
- sentinel: the control FIFO takes `ui` only; the language comes from ui.conf only.
- core: alerts are read under their recorded name only (no `devui-*` mapping); the SMS language from ui.conf only.
- scripts: ship and config-backup no longer carry devui.conf.

## core 1.33.5, lweb 1.24.1, sentinel 1.0.10, touch 1.10.6, scripts 1.0.8 — 2026-10-22

Device names without "devui" (devui cleanup B2); the old names are still read until the next round.
- touch: settings are `/data/luyaz/ui/ui.conf`; the first start copies `devui.conf` over and leaves it for a rollback. The theme alert is `ui-theme-paused`.
- sentinel: the control FIFO takes `ui` (`devui` still works); alerts are `ui-crash` and `ui-gave-up`; the language comes from ui.conf, else devui.conf.
- core: the SMS language from ui.conf, else devui.conf; doctor says `echo ui > /tmp/luyaz-screen.ctl`.
- core, lweb, sentinel: an alert recorded as `devui-*` reads as its `ui-*` name (labels, SMS text, never-SMS list).
- scripts: ship backs up both ui.conf and devui.conf with a touch trial; config-backup takes ui.conf.
- touch, kit (devui cleanup B3): the crates are `touch-ui/rs/ui` (package `luyaz-ui`) and `touch-ui/rs/ui-model`; the install kit's component and payload directory are `ui` (`./install.sh ssh admin ui esim`); kit variables `TOUCH_REPO`, `TOUCH_BIN`, `UI_FONTS_DIR`; the touch log says `ui:`; debug variable `LUYAZ_UI_FORCE_HSR`; build volume `u60-ui-rs-target`.

## core 1.33.4 — 2026-10-22

- SIM info reads outside the snapshot (write checks, SMS slot wait, web SIM page) go through one entry; a good read is reused by the next snapshot round instead of asking the modem again.
- LAN client names: an mDNS name only counts from services that name the whole device (AirPlay, SMB, SSH, printers, …), not from any app's own service; an Android phone's DHCP name comes before its mDNS name (needs the new lanid-parse plugin).

## touch 1.10.5 — 2026-10-22

- touch: no font directory fallback to the retired /data/plugins/u60pro-devui.
- recover: luyaz-recover.sh refuses to delete luyaz-ui, or to restore an rc.local that starts u60-uid/u60-guard, whatever stray old files exist (it used to look for the retired programs first).

## sentinel 1.0.9, scripts 1.0.7 — 2026-10-22

- sentinel: on 10-22 ship's `hold` went into `/tmp/luyaz-screen.ctl` for three hours and never reached the sentinel (its heartbeat kept going; a restart cured it; cause not found). Every pass it now checks that the path is still the FIFO it reads and makes a new one if not (or if its reader hits end of file), and logs read errors other than "nothing there".
- scripts: when the sentinel does not put the screen down within 10 s, `luyaz-ship.sh` logs what is left to look at (heartbeat and its age, the FIFO's inode against the sentinel's fds, each thread's state/wchan/syscall) and leaves the sentinel running so that evidence survives.

## core 1.33.3, touch 1.10.4 — 2026-10-22

- core: the 网络 page's APN reads (`/api/netinfo?apn=1`) are reused for 60 s and dropped at once by an APN switch: the touch 蜂窝 tab read them every 5 s, each read logs about 8 lines to key.log, and within hours the connection's bearer lines (QCI/AMBR) rolled out of the log, taking the carrier card's ceiling with them.
- core: with no bearer at all in the logs, `qos.bearer` is the current core's (SA = `nr_pdu`) instead of empty, so the screen and the web no longer call an SA 5QI a QCI.
- touch: the carrier card says 「上限 未知」 (ceiling unknown) when it has the QCI/5QI but no AMBR, instead of half a line.

## core 1.33.2 — 2026-10-22

- LAN client names: an iOS `<MAC>@fe80::…` mDNS instance is an id, not a name; the phone shows as `iPhone` again.

## core 1.33.1 — 2026-10-22

- the diagnosis probe budget tries its lock a few times (10 ms apart) before giving up: a fork elsewhere in the process briefly holds a copy of the lock. Tests: two flaky ones fixed (budget lock, ETXTBSY on a freshly written test script).

## core 1.33.0, lweb 1.24.0, mcp 0.4.0 — 2026-10-22

- core: "data is up" no longer trusts the firmware's `connect_status` alone (B31 can say connected with no bearer): the register guard, the data-up wait and SMS forwarding also want a default route out of `rmnet_data0` (`/proc/net/route`, IPv6 too).
- core: `network.health` in `/api/public/status`: the bearer, the open finding, the repairs the underlay gate is holding, what to do next (wait / check the APN) and one sentence for web and touch. Data off on purpose says nothing; held repairs clear once their reason is gone.
- core: CHILL end to end: with CHILL on and clients present, at most every 10 min (own budget, not the diagnosis's) one request through mihomo; two misses while its API answers show "proxy not getting through" in `network.health`. Detection only, no restart. `LUYAZ_CHILL_PROBE=0` turns it off.
- lweb: the home notice reads `network.health`'s sentence (APN link after a failed automatic recovery, CHILL link when only the proxy fails); older cores keep the finding's symptom. Agent API `/agent-api/network-health`.
- mcp: read-only tool `get_network_health`.
- kit: `onboard/README` says B27–B31 (B28 on: open SSH first); `sandbox.sh`'s example uses `HOST=root@<gateway>`. CI: `.github/workflows/test.yml` (cargo test + aarch64 build).

## core 1.32.6 — 2026-10-22

- a LAN client name with a UUID in it (Android Quick Share `nearby-presence-nsd-<uuid>`) is no longer shown as the device name; the DHCP name (e.g. `OnePlus-Ace-3-Pro`) wins again.

## core 1.32.5 — 2026-10-22

- eSIM switch: identity a little slower than 45 s no longer reboots the device. Past 45 s the switch waits up to 75 s more and reboots only if both the identity and the data call are still missing (u60g 10-22: identity came back a few seconds late with data fine, and the device had rebooted for it).

## touch 1.10.0 — 2026-10-22

- the eSIM switch status line uses the core's shared short word (`say_short`) while running and when it ends, the same words as the admin web; with no switch of its own the eSIM page shows the `sim` block's word during any SIM change; reads up to 2 KB of the job. Older cores: as before. Requires: core 1.32.0 for the new words.

## lweb 1.23.0 — 2026-10-22

- an eSIM switch's result starts with the core's shared word (`say`); a known failure cause shows that word and what to do instead of the raw lpac message (logged); the eSIM page's status shows the job's word while a switch runs (e.g. Retrying safely) and the `sim` block's word during any SIM change. Older cores: as before.

## touch 1.10.3 — 2026-10-22

- dual SIM: the standby card says where it roams to (e.g. Roaming on China Mobile), in the core's words, on Home and Cellular. Requires: core 1.32.4.

## lweb 1.23.1 — 2026-10-22

- dual SIM: the Home card rows show where each card is (e.g. 4G · Roaming on China Mobile) and use the core's logo pick (CMLink); the Dual SIM page adds it after the carrier. Requires: core 1.32.4.

## core 1.32.4 — 2026-10-22

- dual SIM: each slot carries `place`/`place_en`, where the card is in the data card's words (Local / Roaming on China Mobile / Roaming); the standby slot's network name comes from its QMI serving-system read.

## touch 1.10.2 — 2026-10-22

- dual SIM: the standby card's line on Home and both card rows on Cellular say Local or Roaming, like the data card's line. Requires: core 1.32.3.

## core 1.32.3 — 2026-10-22

- dual SIM: each slot carries `serving_mcc` and `true_roaming` (the same country rule as `signal.true_roaming`); the standby slot's network comes from a QMI NAS Get Serving System read next to its signal read.

## touch 1.10.1 — 2026-10-22

- dual SIM: each slot row uses the core's per-slot `logo`, the same logos as single SIM (e.g. CMLink, CMHK); older cores: mainland three only, as before. Requires: core 1.32.2.

## core 1.32.2 — 2026-10-22

- dual SIM: each slot carries a `logo`, picked the same way as the single-card logo (SPN, then PLMN), so Hong Kong, Macau, Taiwan and other cards get their logo too, not only the mainland three.

## core 1.32.1 — 2026-10-22

- dual SIM: when the firmware leaves slot 1's operator empty (seen on B31 with a 454-12 card on standby), slot 1 shows its SPN name instead of nothing.

## core 1.32.0 — 2026-10-22

- one table of words for a SIM/eSIM switch (`sim_say`): the `sim` block (by SIM identity phase) and `/api/esim/job` (switch jobs, by status/reason) carry `say`/`say_en` (full), `say_short`/`say_short_en` (fits the touch screen) and `say_id`: Switching / Retrying safely / Back online / May need an APN check / Auto-recovery failed; settings left as they were. Old fields unchanged.

## lweb 1.22.0 — 2026-10-22

- the eSIM page no longer reads the card every 10 s (profiles) and 30 s (notifications); it reads on opening (the stream closes while the tab is hidden), on Refresh/Retry or a finished write, when the SIM identity epoch/phase changes, and every 10 s only while a job runs or the list is settling, plus 40 s after.

## core 1.31.0 — 2026-10-22

- SMS forwarding waits out a SIM switch (identity phase not online): sends resume once the phase has been online 30 s, or after 10 min if it stays stuck; nothing is dropped.

## core 1.30.0 — 2026-10-22

- the APN watch now looks when the SIM identity epoch/phase moves, with a 60 s fallback poll (was 10 s); no reconnect proposal while a SIM switch is leaving/changed.

## core 1.29.0 — 2026-10-22

- new alert kinds `sim-changed`, `sim-stalled`, `esim-failed` and `esim-rebooted` (the reboot an eSIM switch asked for, told on the next boot), each once per SIM identity epoch and boot.
- on a SIM change with CHILL off or idle-paused, the log says it is not reloaded.

## core 1.28.0 — 2026-10-22

- the eSIM identity-timeout reboot can be switched off: with `/data/luyaz/core/esim-auto-reboot-off` present a stuck switch ends `identity_stuck` (restart by hand) instead of rebooting; on by default.

## core 1.27.7 — 2026-10-22

- eSIM switch, corrected: 1.27.6 compared against the wrong field (`get_sim_info`'s `sim_*` fields are always slot 1, the eSIM card's). Now the card-only switch (`reason: "inactive_slot"`) happens when slot 1 is switched off or data is on slot 2.

## core 1.27.6 — 2026-10-22

- eSIM switch on a dual-SIM device: when the modem is on another SIM than the eSIM card, the switch only changes the card's profile and ends `reason: "inactive_slot"` — no modem restart, no identity wait and no automatic reboot (the identity would never change, so it used to reboot for nothing).

## core 1.25.0 — 2026-10-22

- Writes never go ahead without the cross-process write lock: when another writer holds it for 15 s, or the lock file cannot be opened, the write is refused (HTTP 503, `busy` or `lock_error`) and nothing is sent to the device. Waiting for the lock no longer holds up data collection.
- Wi-Fi writes: only a caller that provably holds the Wi-Fi lock (its process, or a parent of it) skips waiting for it; anything else waits, and is refused if the lock stays busy or cannot be opened.
- At start, pending changes wait for the write lock as long as it takes instead of going ahead without it.
- Queued legacy requests refused for the lock go back to the queue instead of counting as done.

## scripts 1.0.6 — 2026-10-22

- `luyaz-fallback.sh`: a write lock that cannot be opened exits 5 and writes nothing (it used to write without the lock).

## core 1.24.0 — 2026-10-22

- eSIM switch: "data is back" now means an address on the cellular interface and the internet route through it (IPv4 or IPv6), not the firmware's own data-call flag.
- eSIM switch: a card that answers "busy" is tried again for up to 4 minutes (the card's list re-read before each try); `POST /api/esim/cancel` stops the waiting. Jobs report `attempts` and `deadline_s`.
- eSIM switch: the reboot after the modem never picks up the new profile happens at most once per 24 hours, marked as planned first; after that the job ends `identity_stuck` and asks for a manual restart.
- The `sim` block has `epoch` and `phase` (online / leaving / changed / reloaded / stalled): where a change of SIM identity stands.

## lweb 1.21.0 — 2026-10-22

- eSIM: waits as long as the switch says it may take; tells apart "card busy after the retries", "retries stopped" and "card switched, restart the device to finish".

Requires: core 1.24.0 for the longer wait (older cores: 240 s as before).

## touch 1.9.0 — 2026-10-22

- eSIM: shows 「卡忙，自动重试中」 while the card is retried, waits as long as the switch says it may take, and says when a restart is needed to finish.

## core 1.23.0 — 2026-10-22

- Home exit: a probe home that fails while the cellular link itself is down no longer counts toward the 15 minutes that turn 「Everything else via home」 off.
- CHILL (`chill.sh`, record-only file): while running, checks its routing rules once a minute and restarts the proxy core when they were deleted (more than 3 times in 10 minutes: direct, with backoff); the WAN moving to another interface switches to direct, a redial does not.

## lweb 1.20.0 — 2026-10-22

- Connected devices: a device that took over an alias is tagged 「New address, alias kept」 and its details show the old MAC.

## core 1.22.0 — 2026-10-22

- Connected devices: a device that comes back on a new random MAC (and has no alias of its own) takes over the alias of the one offline device it matches (same mDNS name, or same specific DHCP host name and option 55 list; never a host name alone, never two candidates), and a device never seen before raises one alert 「A new device joined the hotspot」 after 90 s (no SMS). The first run after the update takes every current device as known, silently.

## core 1.21.4 — 2026-10-21

- Dual SIM: the QoS query reads the data card's IMSI (slot 2 when data is on slot 2).

## lweb 1.19.1 — 2026-10-21

- Device info: with data on SIM 2 an empty phone number reads 「SIM 2 reports no number」 instead of 「Not on the SIM」.

## touch 1.8.2 — 2026-10-21

- SIM page: with data on SIM 2 an empty number reads 「SIM 2 reports no number」; PLMN 460-13 counts as China Mobile.

## core 1.27.5 — 2026-10-22

- the APN block is read every 10 minutes instead of every minute, and at once after a write or when the data SIM changes (ICCID): each read made the vendor log about 30 lines to key.log.

## core 1.27.4 — 2026-10-22

- the QCI/5QI and AMBR no longer go blank after a SIM or core change: the vendor's key.log fills with other chatter within hours, so the core now keeps its own copy of the newest 400 `[DATA]` lines (`/data/luyaz/core/qos-lines.log`, written only when new ones appear). Values from that copy are still marked stale when the address or core moved.

## core 1.27.3 — 2026-10-22

- a learned device name, once readable, sticks: a later UUID or NetBIOS name from the same device no longer replaces it (a Mac flipped from its AirPlay name to `MAC-…`).
- after a SIM change with CHILL off or paused while idle, the log says the reload was skipped.

## touch 1.9.1 — 2026-10-22

- connected devices show vendor and type on their own line, so the totals are no longer cut off.

## core 1.27.2 — 2026-10-22

- LAN client names: hex ids of 8+ digits (Android adb/cast) no longer hide the DHCP hostname; a Mac's mDNS model names it when nothing else does; computer services beat AirPlay/cast receiver types (a Mac was shown as a speaker); OUI vendors lose legal tails (`Xiaomi Communications Co Ltd` → `Xiaomi`).

## core 1.27.1 — 2026-10-22

- Fix (1.25.0): the scenario engine's own Wi-Fi switches were refused. It holds the Wi-Fi lock inside the core and calls the core's `/control` over loopback; the core now recognises itself as that caller instead of waiting for its own lock for 5 minutes.

## core 1.27.0 — 2026-10-22

- after a SIM change (eSIM switch or the firmware's own slot switch) is back online, CHILL is reloaded once (only when it runs, and not on top of a CHILL job already running), and home-exit starts its missed-probe count over and probes home at once, so misses on the old card can no longer turn off "rest via home".

## core 1.26.1 — 2026-10-22

- a manual redial (`/control` `cellular.redial`, web and touch screen) waits for a running diagnosis or Finding probe to finish (at most 4 s) instead of cutting it off, and answers 409 busy while a network setting is being applied.

## core 1.26.0 — 2026-10-22

- underlay gate: home-exit no longer counts a missed home probe, and manual-first no longer leaves the hand-picked node, while the layer below the proxy is down (no address on the data interface: for as long as it lasts; a lower sure finding: at most 15 min). Repairs of known breakage (process died, rule lost) are never held.

## lweb 1.19.0 — 2026-10-21

- Connected devices shows each device's best name (your alias first), maker, type and a "Private address" tag, plus a details view listing every source's value; set or remove an alias there (inline confirm, read back). Requires: core 1.21.0.

## touch 1.8.1 — 2026-10-21

- Fix: 1.8.0 found no touchscreen and no power button on the device (the rewritten input code treated EVIOCGBIT's byte count as a failure).

## touch 1.8.0 — 2026-10-21

- the clients list shows a device's alias first, plus its vendor, type and a "Private address" hint; client entries carrying the new fields are no longer dropped. Requires: core 1.21.0.

## core 1.21.3 — 2026-10-21

- Screen logo: PLMN 460-13 is China Mobile (missing since 1.21.2 put slot 2's IMSI in the `sim` block, the logo went blank).

## core 1.21.2 — 2026-10-21

- Dual SIM: with data on slot 2 the `sim` block (imsi, iccid, state, PIN) now describes slot 2; the firmware's `sim_*` fields are always slot 1, so the screen showed the slot 1 operator logo. Slot 2 reports no number or SPN; those stay blank.

## core 1.21.1 — 2026-10-21

- lanid no longer shows ids as names (iOS announces some mDNS services under a UUID); `_apple-mobdev2`/`_remotepairing` mark an Apple phone or tablet.

## core 1.21.0 — 2026-10-21

- LAN client identification (`docs/designs/lan-client-id.md`): a `lanid` thread listens passively on br-lan for DHCP requests, mDNS, SSDP and NetBIOS (kernel BPF, no multicast join, nothing sent); a new wasm component `lanid-parse` parses them, with no built-in fallback: without the component only DHCP names show. `clients` gains `alias`, `vendor`, `kind`, `private`; `GET /api/lanid` lists every source per device; `POST /api/lanid/alias` sets an alias. Learned names are kept in `/data/luyaz/lanid/clients.json` (written at most hourly, only on change). doctor shows a 「设备识别」 row.

## core 1.20.0 — 2026-10-21

- the lanid capture flag takes `mb=N` (size of each of the two ring files, 1–32, default 2), so a long capture can hold more than a few hours of mDNS; `status.json` shows `file_mb`.

## touch 1.7.0 — 2026-10-21

- Placement (Cellular › troubleshooting) gets a 30 s SINR curve; the best so far breaks SINR ties on RSRP and reads `17.7 · -82 dBm · 2 min ago`. Home shows a landing card for 24 hours after the core records a new country (`GET /v2/usage/landing`): 「Arrived in Japan · 2 h ago」, 「From China · 110MB since landing」. Requires: core 1.18.2.

## core 1.19.0 — 2026-10-21

- opt-in step-0 capture for LAN client identification: with `/data/luyaz/core/lanid-capture` present at start, a `lanid` thread records DHCP requests, mDNS, SSDP and NetBIOS frames from br-lan (filtered in the kernel, not parsed) into a 4 MB pcap ring in `/tmp/luyaz/lanid/` for 48 h (`docs/designs/lan-client-id.md`). Off by default.

## lweb 1.13.0 — 2026-10-21

- home shows a banner while a device keeps polling the admin ports over Tailscale (who, which port, how long, about 0.3–0.6 W more with the screen off) with 「It's mine」 (allowed 24 h) and 「Got it」; Settings lists the allow list with 「Make permanent」 and 「Delete」. Requires: core 1.15.0.

## core 1.15.0 — 2026-10-21

- `power.tailnet_poller` (docs/designs/tailnet-poller.md): with the screen off and not charging, each long-term check (60 s) reads conntrack and watches inbound :80 / :9090 from tailnet peers and from peers behind a subnet router; 10 minutes in a row at ≥ 20 packets a minute from one peer opens one event and one `tailnet-poller` alert. `GET /api/poller` (also `/api/diagnose` `tailnet_poller`); `/control` `poller.allow` (24 h by default, `forever`), `poller.unallow`, `poller.ack`; allow list in `/data/luyaz/core/poller-allow.json`.
- the scenario engine picks up a hand-edited `/data/scenario/scenarios.json` on its next tick (changed mtime or length): a valid file replaces the loaded configuration and logs once; a missing, unreadable or invalid file keeps the loaded one and logs once. The web's own writes are not taken for a hand edit.

## Unreleased

- kit: rewrote the inherited parts of install.sh, device/install.sh and build-kit.sh; options, output and installed files are unchanged.

## core 1.18.3 — 2026-10-21

- rewrote the parts of state, SMS, control, server, QoS, auth, model and command handling that came from the original data service; behavior, routes and JSON are unchanged.

## core 1.18.2 — 2026-10-21

- the landing record now actually reads the serving country: it took `net.serving_mcc`, which the old `net` block never carries (only the `/v2` signal block does), so no record was ever made; now `net.mcc` when it names a country.

## lweb 1.18.0 — 2026-10-21

- home page landing card (docs/designs/travel-assist.md §5): for 24 hours after arriving in a new country, the country and where from, the network (operator, roaming or local SIM, RAT), local time from the network's time zone (a single-zone country's own zone marked 「estimate」 without NITZ), the home exit and the data used since landing; 「Dismiss」 hides it. Requires: core 1.18.2.

## core 1.18.1 — 2026-10-21

- a landing is a new country, not a new MCC: 310 and 311 (both the US), 440 and 441 (Japan) and the like never start a landing record.

## core 1.18.0 — 2026-10-21

- travel landing record (docs/designs/travel-assist.md §5): the serving country (`net.serving_mcc`) seen 3 rounds in a row starts a new record in `/data/luyaz/core/usage/landing.json` with the country before, the time and the bytes used since; `GET /v2/usage/landing` → `{mcc, from, at, rx, tx}` (`from` null: the first record, not a landing). `uci_device_info` gains the network time zone `nitz_timezone`, `nitz_time_dst`, `nitz_sync_flag`.

## lweb 1.17.0 — 2026-10-21

- /trends: new chart 「Signal vs speed」: mobile download with the serving-cell SINR over it on its own −10…30 dB scale (right labels). SINR steady while speed falls points at a busy cell; both falling, at the signal.
- home carrier rows also show each carrier's RSRQ.

## lweb 1.16.0 — 2026-10-21

- new page /usage (Data Usage, in Charts): data used per day for the last 30 days as one bar a day stacked by SIM card, per-card totals and a day list; the home page's 「Today」 row opens it. Requires: core 1.17.0.

## core 1.17.0 — 2026-10-21

- data usage per device date and per card (docs/designs/travel-assist.md §4): every 60 s the session counters are booked to the current data card (the firmware's counter does not split by card); `GET /v2/usage/days?n=1..400`; files in `/data/luyaz/core/usage/` (`today.json`, `days.jsonl`, 400 days).

## touch 1.6.1 — 2026-10-21

- dual SIM switch hint says ~10 s offline (measured ≈11 s on 10-21), not 10–25 s.

## lweb 1.15.1 — 2026-10-21

- dual SIM dialog says data drops for about 10 seconds (measured ≈11 s on 10-21), not 10–25.

## lweb 1.15.0 — 2026-10-21

- /signal, dual mode: 「换卡记录」 lists the data-card switches of the past 14 days with the likely reason (no service, 2G/3G, weak signal while the other card was good). Requires: core with `/v2/dual/switches`.

## core 1.16.0 — 2026-10-21

- dual SIM: passive log of the data card and both slots' signal every 30 s in `/data/luyaz/core/dsds/YYYY-MM-DD.jsonl` (dual mode only, 14 days, ~1 MB a day), with a `switch` event when the firmware moves the data card; `GET /v2/dual/switches?days=14`.

## lweb 1.14.2 — 2026-10-21

- 「Got it」 on the home Tailscale-polling banner also marks the matching `tailnet-poller` alert read in the alert bar (only when no other unread alert sits below it). Requires: core 1.15.1.

## core 1.15.1 — 2026-10-21

- `power.tailnet_poller` events and their acks are kept in `/data/luyaz/core/poller-state.json` (written only when an event opens, ends, comes back, is acked or allowed): a core restart no longer tells an acked, still running event again. A missing or broken file is a fresh start.

## lweb 1.14.1 — 2026-10-21

- dual standby: the data card's row says 5G SA / 5G NSA / 4G from the serving network, not the sampler's RAT.

## lweb 1.14.0 — 2026-10-21

- dual standby: the 双卡 group moves to the top of Home and reads like the touch screen: data card first, logo and name instead of SIM N, 上网中 / · 自动切换 / 切换中 / 待机, standby dimmed and 待机 · 信号弱 when weak; per-slot dBm/dB and the unverified smart-rule sentence are gone. The RSRP/SINR readouts are tagged with the data card's operator. China Mobile 460-13 now has its logo.

## touch 1.6.0 — 2026-10-21

- dual standby: Home drops the 卡N labels and the dBm/dB lines; the data card is always the top row (上网中, · 自动切换 when smart is on, 切换中 while switching), the standby row is dimmed with 5G SA/4G and turns to 待机 · 信号弱 when weak; the verdict row is the single-card one again, about the data card. 蜂窝 gains a 双卡 card with 信号强度 / 信号质量 per slot.

## touch 1.5.4 — 2026-10-21

- dual standby: the slot lines read in words (上网 · 信号强 / 待机 · 信号中); the verdict is one size smaller with both slots' RSRP · SINR beside it, and 干扰 · 负载 under it.

## touch 1.5.3 — 2026-10-21

- dual standby: the data slot's line never drops 「漫游」 (roaming); 「本地」 may still go when space is short.

## touch 1.5.2 — 2026-10-21

- dual standby: each slot line carries its own operator logo (or the localized name, e.g. 中国移动 instead of CMCC), like the single-card top line.

## touch 1.5.1 — 2026-10-21

- dual standby: the two slot lines now sit in the status block's top (the green wash), replacing the single top line, instead of a block under it.

## lweb 1.12.0 — 2026-10-21

- home: in dual-SIM mode a 「Dual SIM」 group shows both slots (operator, RAT, data/standby, RSRP · SINR, reading age; greyed after 60 s) and the smart-switch rule; the top RSRP/SINR readouts name the data slot. Single-slot devices and single mode are unchanged.

## touch 1.5.0 — 2026-10-21

- dual standby (global edition): the status bar shows two rows of signal dots (data slot on top in its usual colours, standby slot below in neutral grey), and Home shows one line per slot under the status block: card, operator, RAT, role and RSRP · SINR (or the card's state). Single-slot devices are unchanged. Reads the core 1.14 `sim.dual` slot fields.

## core 1.14.0 — 2026-10-21

- dual SIM: each slot reports `state` and `role`, `sim.dual.smart_rule` carries the smart-switch thresholds, and slots carry an optional `signal` (data slot from nwinfo, standby slot read every 30 s over a read-only QMI NAS client; only started when a standby slot exists). New `luyaz-core qmi-signal`.

## touch 1.4.0 — 2026-10-20

- Network diagnosis: a two-tap 「Restore the last working setup」 button when the cellular settings differ from the last verified connection for this card and country; progress and the result show under it.

## core 1.13.2 — 2026-10-20

- crash logs: when several are written within one clock tick, pruning now always keeps the newest and exactly 5 per program (a reused `.1` name could leave 6).

## touch 1.3.1 — 2026-10-20

- Dual SIM: the armed row's roaming warning is shorter (it was cut off on the 320 px screen).

## touch 1.3.0 — 2026-10-20

- Dual SIM on the SIM & eSIM page (global edition, shown only when core reports two slots): what each slot holds and five choices (slot 1 only, slot 2 only, dual with main slot 1 or 2, dual with smart switching); two taps send core's `sim.dual` transaction, whose words and countdown replace the line on top, with a roaming warning when the card may roam here. Requires: core 1.13.1.

## lweb 1.11.0 — 2026-10-20

- new Dual SIM page (`/router/dualsim`, linked from SIM / PIN on a dual-SIM device): both slots, single/dual, main card, smart switching; tier-3 dialog with a roaming note when the card may roam (or that can't be told); runs u60d's `sim.dual` transaction.

## core 1.13.1 — 2026-10-20

- `sim.dual` progress and values read as words ("Dual SIM · main slot 2") in the op block, the change log and the touch screen, instead of raw JSON.

## core 1.13.0 — 2026-10-20

- dual SIM (global edition): `/v2` `sim.dual` view of both slots, and a `sim.dual` change (single/dual, main slot, smart switching) run as a transaction with readback, data check and rollback; hidden where `support_dual_sim` is not 1.

## core 1.12.0 — 2026-10-20

- core: restore the last verified connection, first part (no button yet). After a deep check you start, one bound HTTPS + DNS check (~20 KB) may record the cellular settings that worked (cell locks, bands, mode, APN) per card and country. `GET /api/verified` shows what differs now; `/control verified.restore` (screen or web only, never the agent) sets them back one ordinary transaction at a time and stops at the first that doesn't confirm.

## lweb 1.10.0 — 2026-10-20

- lweb: the deep check page gets 「Restore the last settings that worked」 (needs core 1.12.0): what differs from the last verified set for this card and country, whether each item rolls back by itself, a separate tick when the APN changes, and the step-by-step progress.
- lweb: new read-only page System → AI Agent Permissions (/tools/agent): the agent's tokens (names and read/change, never the token), the addresses allowed to read and to change, what it may change (APN, CHILL) with the once-per-30-minutes limit, the last change and that neither rolls back by itself, and when the permission files last changed. Nothing can be changed there.

## lweb 1.9.2 — 2026-10-20

- lweb: change log rows made through the AI agent say where the permission came from: the agent's own act permission, or you approving its proposal on the web page or the screen (plus whether it rolls back by itself).

## core 1.11.4, touch 1.2.1 — 2026-10-20

- core: short touch-screen words for the read-only scenarios (`say`): `slow_signal` in `/api/diagnose`, and `hot_drain` / `long_idle` as `/api/health` `notices` (never checks, never alerts).
- touch: the diagnose page shows the strong-but-slow line under the redial row; the health page lists the core's notices after the checks (● mark, tap for the full text). Older cores just show nothing.

## core 1.11.3, lweb 1.9.1 — 2026-10-20

- core: `perf.slow_strong_signal` has a passive way in: strong signal (the deep check's own signal rule on the live reading) with downlink ≥ 1 Mbit/s for 60 s and no deep check to read → suggests running one. Never judges slow, never sends anything.
- lweb: the diagnosis page shows that suggestion.

## core 1.11.2 — 2026-10-20

- the hour ledger counts charging seconds only while the fuel gauge's current is positive (the battery block's `charging` flag is set on this device while the battery drains), the same rule as the hour power figure.

## lweb 1.9.0 — 2026-10-20

- every `/agent-api` answer carries `device_id` (random 12 hex digits in `/data/luyaz/agent.id`, 0600, made on the first answer) so `luyaz-mcp enroll` can pin the device (multiple U60s, u60-mcp.md R2).

## core 1.11.1, lweb 1.8.1 — 2026-10-20

- core: `slow_signal` reads the serving cell with the deep check's own RAT rule (NR/5G/SA_* and ENDC no longer read as `cell_changed`).
- lweb: the `long_idle` explanation says to restart the core.

## core 1.11.0, lweb 1.8.0 — 2026-10-20

- core: three read-only decision-loop scenarios (§4.11) with evidence on `/api/diagnose`: `power.hot_drain` (this boot's last closed hour against matched hours from the hour ledger), `usage.long_idle` (two quiet covered hours at home, screen off, no clients) and `perf.slow_strong_signal` (over the person's own deep check). Never an action, never a probe.
- lweb: the diagnosis page explains all three.

## core 1.10.0 — 2026-10-20

- alert SMS can carry a device name after the signature (`/data/alerts/sms-name`, or `"name"` in PUT /api/alerts/sms): 【Luya-Z】U60Pro全球版：… ; longer texts go out as one long SMS

## core 1.9.0, lweb 1.7.0 — 2026-10-20

- core: `radio.band_flap` evidence from rtnetlink address drops on rmnet (band/cell/SINR from the signal block, SSR from crashwatch), on `/api/diagnose` as `data.band_flap`. Read-only.
- lweb: the diagnosis page shows the band_flap explanation; the Wi-Fi confirms carry a `remote.wifi_change` risk line per origin (revert checks come before the client count).

## core 1.8.0, lweb 1.6.0, scripts 1.0.5 — 2026-10-20

- core: R18-P0 batch: `expect_device_id`, agent authority, DNS liveness bound to the cellular interface (never the default route), transaction probe tri-state (a local socket failure is unknown), an off-lock probe result kept only if a fresh read within 15 s shows the same SIM, interface and connection; the core LAN listener (:9461, no callers) binds to 127.0.0.1.
- lweb: matching changes for the R18-P0 batch.
- scripts: doctor shows a row only when the web device id file can't be read.

## core 1.7.2 — 2026-10-20

- an NR session kept after the radio falls back to ENDC is no longer shown as old.
- decision loop: when a person-approved redial is confirmed and the bound HTTPS recheck passes (recorded as recovered), the finding it was proposed for closes as `resolved` right away instead of waiting for two passive checks. No 24 h suppression; other outcomes still wait for the passive checks.

## core 1.7.1 — 2026-10-20

- decision loop: at startup the core follows every redial attempt a restart left open to its end (recorded as unknown, no recheck), so an attempt can no longer stay `attempt_running` for good.

## core 1.7.0, lweb 1.5.0, touch 1.2.0 — 2026-10-20

- core: `/control` `loop.approve {proposal_id, params_digest}`: a person on the screen or the web approves the redial proposal shown; a different or spent proposal is refused (409 `proposal_changed` / `already_used`), the agent can never approve. A fresh mode read before the write; after the transaction a bound HTTPS recheck and the result is recorded.
- lweb: an approval card under the open finding on the diagnosis page (Redial, press twice).
- touch: a redial button on the diagnosis page (two taps).
- Requires: core 1.7.0 for lweb 1.5.0 and touch 1.2.0.

## core 1.6.11 — 2026-10-20

- eSIM: the wait for data after a profile switch is 90 s (a roaming profile took 55–60 s on B31).
- LTE cell lock is marked `captured_and_proven` after the device test; network-mode transactions get a 180 s deadline; PCI values are range-checked.

## core 1.6.10 — 2026-10-20

- `data.reconnect` (redial) exists for the decision loop only; automatic execution goes through an allow-list that ships empty.

## core 1.6.9 — 2026-10-20

- an identical rmnet `RTM_NEWLINK` (the driver repeats it several times a second) no longer changes the connection epoch; real link, flag, address and route changes still do.

## core 1.6.1–1.6.7 — 2026-10-19/20

- connection, SIM and radio epochs from the netlink and ubus subscriptions; O2 recheck: bound multi-target HTTPS over the cellular interface, operator DNS within a 20 s budget, clock trust needs two agreeing organisations; `o2-check` subcommand.

## lweb 1.4.1 — 2026-10-19

- the remote Wi-Fi receipt sends its `op_id` inside `params`, where the core reads it (the first real remote test rolled back with `no_receipt`).

## core 1.6.0, lweb 1.4.0 — 2026-10-19

- core: remote Wi-Fi receipt: a Wi-Fi change made from off the LAN waits 60 s for the page's `op.receipt` after the read-back, and puts the old settings back without one (whatever the global auto-revert switch says).
- lweb: from Tailscale, a subnet route or an unknown path, Wi-Fi writes ask for the receipt and the page sends it by itself; the confirm says so.

## core 1.6.8 — 2026-10-20

- 5G cell lock (`cell.lock_nr`) is marked `captured_and_proven` after the real-device test (a lock to a missing cell rolls back at the deadline through the stock reset), so closed-loop 5G cell locks auto-revert too. LTE cell lock stays `unknown` (not tested).

## core 1.6.5 — 2026-10-20

- band lock (`band.set_lte`, `band.set_nr`, `band.reset`) is marked `captured_and_proven` after the real-device test (good switches confirm, a lock to an absent band rolls back at the deadline), so closed-loop band writes now auto-revert too. Network mode and cell lock stay `unknown`.

## core 1.5.8 — 2026-10-19

- network mode: the three "auto" values (`WL_AND_5G`, `TCHGWL_5G`, `NETWORK_auto`) count as one in a transaction. Writing `TCHGWL_5G` reads back `WL_AND_5G`, which used to end a good switch or a rollback as `manual_change` / `not_applied`.

## lweb 1.3.1 — 2026-10-19

- the remote warning in a confirm now says whether this step reverts by itself: network mode, band lock and cell lock follow the device's auto-revert setting; every other write says it does not revert.

## core 1.5.7 — 2026-10-19

- a write from the touch screen waits at most 4 s at the transaction gate, so the screen (which gives up on `/control` after 5 s) sees the busy answer.

## lweb 1.3.0 — 2026-10-19

- Remote warning by path (R18-P0-04): a browser behind another Tailscale node's subnet route no longer reads as local; the top bar says "Remote via Tailscale", "Remote via a subnet route" or "Unknown source", and a confirm that can cut the uplink says how to take over locally if nothing reverts it.
- Wrong-device guard: each page carries this device's random ID (`/data/luyaz/web-device-id`); a write from a page opened on another U60 is refused with "Opened on another device".

## touch 1.1.4 — 2026-10-19

- eSIM: "Card settling, list updates soon" while the core serves its last list; no switch until a fresh list.

## lweb 1.2.4 — 2026-10-19

- eSIM: while the card settles the list shows as "from N s ago, updating…" and stays locked; a settling list never counts as a write's readback.

## sentinel 1.0.8 — 2026-10-19

- the recover-live cap check is shared with the core (`luyaz_wifi::check_slot`), now tested.

## core 1.5.6 — 2026-10-19

- doctor adds a `timeout` row naming the commands that ran out of time; the recover-live cap check is shared with the sentinel (`luyaz_wifi::check_slot`).

## scripts 1.0.4 — 2026-10-19

- `luyaz-ship.sh recover-live` takes `recover.lock` (`flock -n`): a second one refuses with 3; detached programs no longer inherit the lock fds. (R9)
- `web-auth.sh` reads only `LUYAZ_WEB_PASSWORD` (the `ZTE_AGENT_PASSWORD` fallback is gone; config-backup still rewrites it when restoring an old archive). `luyaz-recover.sh` never puts back the old-path core program or an rc.local starting `zwrt-datad`, whether or not `.migrated` is there (installed on the device 10-19 with `install-recover`).

## touch 1.1.3 — 2026-10-19

- eSIM: "switched, data not back yet" after a switch without data; waits up to 240 s for a switch. (R5)
- `start.sh` is retired; a touch ship carries only `luyaz-ui`. Ship scripts before the next touch ship (the executor still expects `start.sh` until then).

## sentinel 1.0.7 — 2026-10-19

- command timeouts kill the whole process group; a recover-live it started is killed after 15 min. (R8, R9)

## core 1.5.5 — 2026-10-19

- eSIM: one call on the card at a time (polling reads answer from the last list instead of queueing); lpac runs in its own process group with a 1 MB output cap, its stderr tail goes into the error; while the card settles after a switch the list is the last good one (`stale`); a switch waits card → identity (45 s) → data (60 s), no data back is a warning, not a reboot. doctor: every external command has a 10 s deadline. A recover-live it started is killed after 15 min. (resilience R2–R5, R8–R10)

## lweb 1.2.3 — 2026-10-19

- the USB page's fast-charging row keeps its hint ("plug a phone in first", "18 W fast charging…") while the capability is only unverified; the "unverified" note goes beside it. Since 1427ef5d9 the note replaced the hint.
- reads only `LUYAZ_WEB_PASSWORD` from `/data/luyaz/web.env` (the old-key fallback is gone; the device file was rewritten in F7).

## core 1.5.4 — 2026-10-19

- external commands can no longer hang the core: eSIM probes, `simreset`, ubus and modem restart run with a deadline and are killed as a process group; `iw` station reads have a 2 s deadline and run one at a time (a busy read returns the last result); `chill.sh` calls time out (jobs 5 min, home-exit reload 60 s) and free the CHILL slot. See docs/designs/resilience-audit-2026-10-19.md.

## scripts 1.0.3 — 2026-10-19

- repo names follow the device (rename F7): `luyaz-ship.sh`, `luyaz-recover.sh`, `core-trial.sh`, `web-auth.sh` (was `agent-auth.sh`; the scripts component carries `web-auth.sh`). The password in `/data/luyaz/web.env` is `LUYAZ_WEB_PASSWORD`; `web-auth.sh`, the kit and config-backup still read `ZTE_AGENT_PASSWORD` when the new key is missing, and restoring an old archive writes the new key. The kit's payload directory is `scripts/` (was `guard/`).

## touch 1.1.2 — 2026-10-19

- internal names only: the data module talking to the core is `core_link` (was `datad`), the render scene `core-down` (was `datad-down`).

## lweb 1.2.2 — 2026-10-19

- reads the password as `LUYAZ_WEB_PASSWORD` from `/data/luyaz/web.env`, falling back to `ZTE_AGENT_PASSWORD` until the device file is rewritten.

## sentinel 1.0.6 — 2026-10-19

- the handover list names `core` instead of `datad` (the core reads neither: nothing changes in what it does).

## core 1.5.3 — 2026-10-19

- the last old names are gone (rename F7): the snapshot's version block is `core` (was `datad`), `/control` replies say `"busy":"core"` / `"core":"down"`, netwatch events `core_lost` / `core_back`. The long-term layer no longer writes `g_datad` (it always equalled `g_core`) and writes `cpu_core` (was `cpu_datad`); the scorecard reads only `g_core`, `cpu_core`, `core_degraded`, `rss_core`, `rss_ui`: the device's history files are rewritten to the new names after this ships.

## scripts 1.0.2 — 2026-10-19

- the ship executor, init, chaos, agent-auth, fallback and config-backup know only the new layout and component names (`core`; `datad`, `guard`, `uid`, `--convert`, `move-d4` are gone). The device manifest is rewritten to `core` before this ships.

## lweb 1.2.1 — 2026-10-19

- no fallback to `/data/zte-agent.env`.

## sentinel 1.0.5 — 2026-10-19

- no more `/tmp/luyaz/.migrated`, old log names or `comp=datad` ship transactions.

## core 1.5.2 — 2026-10-19

- only the current names are known (Luya-Z rename F5/6): the component is `core`, the program `luyaz-core`, settings in `/data/luyaz/core/core.env` (was `datad.env`). The standby ledger drops the retired `zte-agent` column; longterm no longer watches `u60-uid`, the old guard scripts or `datad_degraded`.

## core 1.5.1 — 2026-10-19

- doctor check ids use the current names: `core-http`, `web-http`, `dup-luyaz-core`, `dup-luyaz-ui` (were `datad-http`, `agent-http`, `dup-zwrt-datad`, `dup-u60pro-devui`). The core's own crash alert is `core-crash`; old `datad-crash` rows keep their label and SMS text (Luya-Z rename F4).
- doctor no longer checks retired things: `svc-zte-agent`, `svc-zwrt-datad`, `svc-u60-guard`, `svc-u60-uid`, core-move, tmp-move, and the old process names. Alert labels and SMS texts for retired kinds (`agent-*`, `datad-degraded`) and the `datad-crash` alias are gone; old rows on the device are rewritten to `core-crash` when this ships.

## scripts 1.0.1 — 2026-10-19

- `doctor-legacy.sh` is deleted; `doctor.sh` only runs the core's doctor.
- doctor-legacy.sh uses the same new check ids as the core; `chaos.sh` expects `core-crash`.
- the install kit no longer puts `start.sh` or its rc.local hook on the device and retires `/data/plugins/u60pro-devui/` (stock UI backup → `/data/luyaz/ui/vendor-devui.orig-backup` after an md5 check, boot trace along).
- doctor-legacy.sh labels its duplicate-instance checks like core 1.2.1.
- `VERSION` is shipped to `/data/luyaz/scripts/` so the scripts' version can be read on the device.
- ship's UI-trouble check matches the touch UI's crash line under both names (`u60pro-devui pid … ended:` and `luyaz-ui pid … ended:`). Ship this before sentinel 1.0.2.

## sentinel 1.0.4 — 2026-10-19

- writes the boot trace itself (`/data/luyaz/ui/boot-trace.log`, same format as `start.sh prep`) and no longer runs `/data/plugins/u60pro-devui/start.sh`.

## sentinel 1.0.3 — 2026-10-18

- the touch UI's crash records go to `/data/crashlog/luyaz-ui`.

## core 1.5.0 — 2026-10-18

- ledger and report keys use the new names (`luyaz-core`, `luyaz-ui`; RSS columns `rss_core`, `rss_ui`); older records under `zwrt-datad`/`u60pro-devui`/`rss_datad`/`rss_devui` still count. The core's crash records go to `/data/crashlog/luyaz-core`, and the core moves `/data/crashlog/u60pro-devui` and `zwrt-datad` into the new directories (each check, without counting them again).

## core 1.4.4 — 2026-10-18

- Diagnosis rows in plain words: the signal row says how strong and how noisy without numbers (it was cut off), the cap row lists the carriers the cell gives, and the other rows drop interface names and protocol words.

## touch 1.1.1 — 2026-10-18

- The cellular page's speed-test row no longer says "Unavailable" before the speed page was opened.

## core 1.4.3 — 2026-10-18

- Diagnosis rows that said "not checked" on the device now answer: the internet and DNS rows probe the cellular path with CHILL on, the clock row compares with an HTTP `Date`, a QCI without an AMBR reads "no cap reported", and reason codes are in words.

## core 1.4.2 — 2026-10-18

- The diagnosis eSIM channel check runs on the device (it used `timeout`, which the device lacks, and always said unknown).

## core 1.4.1 — 2026-10-18

- The diagnosis SIM row reads the modem state; on the device it said "SIM not ready yet" with the card ready and data up.

## touch 1.1.0 — 2026-10-18

- The diagnose page shows the new rows and the grey "not checked" style.

## lweb 1.2.0 — 2026-10-18

- The diagnose page shows the new rows and the grey "not checked" style.

## core 1.4.0 — 2026-10-18

- Deep diagnosis v2: new rows for the SIM (and a full eSIM channel table on an eUICC), the data bearer (address, route, rp_filter), DNS through the device's own dnsmasq, an end-to-end request (the Finding path's probe) and the clock; a row whose earlier row failed is not probed and shows grey ("not checked"), so the headline is the first real cause. A running Finding probe no longer looks like a running diagnosis.

## core 1.3.4 — 2026-10-18

- The eSIM ISD-R channel a killed lpac leaves open is closed again: the modem never reclaimed it (measured: two watchdog kills used up every free channel and every eSIM call failed until a reboot). Needs the `qmi_uim_probe` with `close` (`scripts/qmi_uim_probe.c`); an older probe just leaves the channel as before.

## core 1.3.3 — 2026-10-18

- A Wi-Fi settings change is confirmed only once the running hostapd broadcasts the name uci now holds, not merely ENABLED: the vendor reload lands ~40 s late and the old hostapd answered ENABLED with the old name, so the page said done before the change had happened.

## core 1.3.2 — 2026-10-18

- The scenario engine's bootsafe no longer turns Wi-Fi on when only the core restarted (up past 5 min) and the applied scenario (e.g. home) had turned the APs off; it brought Wi-Fi up for ~100 s on every ship. A boot still repairs as before.

## core 1.3.1 — 2026-10-18

- Editing an APN profile that is not in use without typing a password no longer clears its password: `modify_manu_apn` overwrites the whole profile, so the core now sends the current password along (found on the device 10-18).

## lweb 1.1.4 — 2026-10-18

- Changing LAN settings (/router/lan) is off: on the device `router_set_lan_para` reboots the whole U60 and the values only reach `zwrt_router.dhcp`, not `dhcp.lan`. The confirm now says the U60 reboots. `LUYAZ_WEB_LAN_WRITES=1` turns it back on (browser checks).

## touch 1.0.2 — 2026-10-18

- Placement (摆放模式) on LTE-NSA uses the LTE anchor's SINR when the NR leg reports none, instead of showing "--" ([#4](https://github.com/faying/luya-z-manager/issues/4)).

## lweb 1.1.3 — 2026-10-18

- the login page uses the site's sans-serif font (it fell back to a serif font: the font variable was only set by the other pages).
- Home's carrier rows no longer overlap at widths of 1280 px and up (the cell IDs always go on their own line).
- the QCI page shows the core's `qos` block on open; the bearer table (an AT read, about 12 s) is read only when you press "Read bearer table", not polled while the page is open.
- `:9090` drops a connection whose request headers do not arrive within 10 s and serves at most 64 connections per address at a time.

## core 1.3.0 — 2026-10-18

- `doctor --report 24h|7d [--summary]` is answered by the core itself (the same card `/v2/scorecard` gives); the doctor.sh shim no longer needs doctor-legacy.sh for it.

## core 1.2.1 — 2026-10-18

- doctor's duplicate-instance checks are labelled "Core instances" and "Touch UI instances" instead of the old program names; the check ids (`dup-zwrt-datad`, `dup-u60pro-devui`) stay.

## sentinel 1.0.2 — 2026-10-18

- log lines, alerts and crash records name the touch UI `luyaz-ui` instead of `u60pro-devui`. Requires: the scripts release whose ship check matches both crash-line names (an older ship script misses the new line).

## touch 1.0.1 — 2026-10-17

- The Carrier card's QoS line labels the value QCI or 5QI by the core's `bearer` (not the radio mode), shows AMBR as a ceiling in Mbps ("max 1668/1008 Mbps") and marks stale values "(old)".
- System has a "Component versions" card; "Check for updates" asks the core to look now.

## lweb 1.1.2 — 2026-10-17

- The signal page and QCI page label the class by the `qos` block's `bearer` (5QI for an NR PDU session, QCI for an EPS bearer); the signal page marks a stale block "earlier bearer".

## core 1.2.0 — 2026-10-17

- The AT port pick skips a node another process holds open and never writes to it; an answer ends at a whole-line final result code (`OK`, `ERROR`, `+CME ERROR:`, `+CMS ERROR:`); AT errors carry a `kind` (`busy`, `held`, `timeout`, `error`, `no-port`). The AT terminal refuses `ATD…`, `ATA` and `AT+CHUP`.
- QoS is matched to the current connection by the WAN IPv4 and key.log's `CONNECTED … addr=` lines: a bearer of another connection, or a current connection no longer in the part of the log read, is `stale`; with several data DNNs the current connection's wins. Session-AMBR unit codes without a name are decoded (TS 24.501). A stale `qos` block gives no "capped" verdict in diagnosis and no `qos_change` event; diagnosis labels a 5QI as 5QI.
- The `qos` block's QCI/5QI is the vendor's value now (`zwrt_data_tmp.wwaniface1.qci`, read over ubus); key.log's value only when that can't be read.

## core 1.1.3 — 2026-10-17

- a CHILL rollback is done once the old value reads back (it no longer waits for a probe, so a rollback during the idle pause no longer ends as `rollback_failed`).

## core 1.1.2 — 2026-10-17

- an agent CHILL change during the idle pause (screen off, nobody connected) is refused as `chill_off` instead of restarting mihomo into a trial that cannot pass.

## lweb 1.1.1 — 2026-10-17

- `chill_off` reaches the agent as "refused: CHILL is off, do not retry".

## core 1.1.1 — 2026-10-17

- an agent CHILL change while CHILL is off is refused as `chill_off` (not `busy`).

## sentinel 1.0.1 — 2026-10-17

- `agent-change` alerts are never sent by SMS.

## lweb 1.1.0 — 2026-10-17

- Device info has a "Component versions" group with a "Check for updates" button.
- `agent.token` third column `read|act`, `agent.act-allow`, `/agent-api/act/*` and `/agent-api/action-url`; APN page "Agent and roaming" per-card setting.

## core 1.1.0 — 2026-10-17

- "Registered but no internet": the probe runs only after 60 s of passive downs (pilot OQ2: 7.5 days of signal-log, normal reconnects ≤10 s); short drops no longer use up the 10-minute probe cool-down.
- `state.set_interval` counts as read-only: screen on/off no longer clears every cache and re-reads everything.
- `GET /v2/versions` lists each component's installed version and the newest in this repo's `versions.json`; the core looks once a day (not while roaming; `LUYAZ_VERSION_CHECK=0` turns it off), `POST /v2/versions/check` looks now. Nothing is installed automatically.
- Agent changes (MCP phase 2): CHILL profile/exit and APN writes from `approved_by=mcp`, only `apn.*` and `chill.profile`/`chill.exit`; always auto-rollback; one change per layer (APN, CHILL) per 30 minutes; roaming cards not marked cheap get only a page link; a person's write cancels a running agent trial; each agent change is an `agent-change` alert (never SMSed). `GET/PUT /api/agent/roam-cheap`.
- a restart during a rollback no longer drops it: the rollback is re-sent unless the readback already shows the old value, with a deadline.
- `/api/netinfo/apn` switches go through one `apn.use` transaction written as the real caller; the per-card APN is remembered only after the trial passes.

## core 1.0.0, lweb 1.0.0, sentinel 1.0.0, touch 1.0.0, scripts 1.0.0 — 2026-10-17

Baseline: the first numbered versions. Everything up to here (one Cargo workspace, the
Luya-Z names and device layout under `/data/luyaz/`) counts as 1.0.0 for every component.
