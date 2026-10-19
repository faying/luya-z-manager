# Changelog

Each component has its own version ([Semantic Versioning](https://semver.org/)):
patch = fixes, minor = new features (old settings and APIs still work), major = something
must be migrated or several components have to be updated together.

| Component | What it is | Version from |
|---|---|---|
| `core` | `luyaz-core`, the core service (`:9460`) | `crates/luyaz-core/Cargo.toml` |
| `lweb` | `luyaz-web`, the admin web (`:9090`) | `crates/luyaz-web/Cargo.toml` |
| `sentinel` | `luyaz-sentinel`, screen owner and watchdog | `crates/luyaz-sentinel/Cargo.toml` |
| `touch` | `luyaz-ui`, the touch screen UI | `touch-ui/rs/devui/Cargo.toml` |
| `scripts` | device scripts (doctor, init scripts, …) | `touch-ui/scripts/VERSION` |

Each program prints its version with `--version`, e.g. `luyaz-core 1.0.0 (6e606ec)`.
Entries are written under **Unreleased** as `- <component>: text` (e.g. `- touch: …`);
`tools/luyaz bump <component> <patch|minor|major>` moves them into a new version section. `Requires:` lists the minimum versions
of other components.

A Chinese copy is in [CHANGELOG.zh-CN.md](CHANGELOG.zh-CN.md).

## Unreleased

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
