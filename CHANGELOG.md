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

## lweb 1.13.0 — 2026-10-21

- home shows a banner while a device keeps polling the admin ports over Tailscale (who, which port, how long, about 0.3–0.6 W more with the screen off) with 「It's mine」 (allowed 24 h) and 「Got it」; Settings lists the allow list with 「Make permanent」 and 「Delete」. Requires: core 1.15.0.

## core 1.15.0 — 2026-10-21

- `power.tailnet_poller` (docs/designs/tailnet-poller.md): with the screen off and not charging, each long-term check (60 s) reads conntrack and watches inbound :80 / :9090 from tailnet peers and from peers behind a subnet router; 10 minutes in a row at ≥ 20 packets a minute from one peer opens one event and one `tailnet-poller` alert. `GET /api/poller` (also `/api/diagnose` `tailnet_poller`); `/control` `poller.allow` (24 h by default, `forever`), `poller.unallow`, `poller.ack`; allow list in `/data/luyaz/core/poller-allow.json`.
- the scenario engine picks up a hand-edited `/data/scenario/scenarios.json` on its next tick (changed mtime or length): a valid file replaces the loaded configuration and logs once; a missing, unreadable or invalid file keeps the loaded one and logs once. The web's own writes are not taken for a hand edit.

## Unreleased

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
