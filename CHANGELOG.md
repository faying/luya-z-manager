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

- scripts: doctor-legacy.sh labels its duplicate-instance checks like core 1.2.1.
- scripts: `VERSION` is shipped to `/data/luyaz/scripts/` so the scripts' version can be read on the device.
- scripts: ship's UI-trouble check matches the touch UI's crash line under both names (`u60pro-devui pid … ended:` and `luyaz-ui pid … ended:`). Ship this before sentinel 1.0.2.

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
