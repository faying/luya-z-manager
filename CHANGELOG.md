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

- touch: System has a "Component versions" card; "Check for updates" asks the core to look now.
- scripts: `VERSION` is shipped to `/data/luyaz/scripts/` so the scripts' version can be read on the device.

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
