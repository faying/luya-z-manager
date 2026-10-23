# What u60d took from datad

Base: data-service `68416eb` (the datad build on the device since 2026-10-07; MIT). The files listed here were rewritten on 2026-10-21 (`b8268904`); this page is kept as history.

Moved in three steps: `8605c28` first (Phase 2 step 1), then `68416eb` on top (USB attach mode:
the `typec` and `powerbank` blocks in `block.rs`, the multi-event listener in `ubus/listen.rs`,
`tests/mock_ubus.sh`). Phase 3 step 1 (2026-10-08) moved the rest, HTTP and the write path,
from the same `68416eb`: every module datad's `main.rs` declares is now in u60d, and u60d can
stand in for datad (serve mode, below).

u60 platform Phase 2 (`docs/designs/u60-platform.md`) moves the datad read path into u60d.
datad keeps running until Phase 3; in between u60d reads in shadow mode and
the outputs are compared. File names are the same as in datad on purpose, so
a datad fix can be carried over with a plain diff:

```
# from crates/luyaz-core, one file at a time
diff <(git -C ../../../data-service show 68416eb:rust/src/state.rs) src/state.rs
```

Expect the deliberate changes listed below (`blocking()`, the typed projection, the SMS
session switch, `u60d:` log prefixes) in that diff.

## Moved (`src/`)

Read path (Phase 2): `block.rs`, `cell_window.rs`, `command.rs`, `executor.rs` (+ `executor/tests.rs`),
`model.rs`, `project.rs` + `project/` (snapshot and screen projections, pure),
`qos.rs`, `sms.rs`, `state.rs`, `uci.rs`, `ubus/` (client, socket and CLI
backends, compare, SMS event listener). Test fixtures are under `tests/`.
Entry point: `read.rs` (`u60d --once`, the same output as `datad --once`).

HTTP and write path (Phase 3 step 1, base `68416eb`): `server.rs` + `server/write.rs`,
`conn.rs`, `auth.rs`, `v2.rs` (+ `v2/tests.rs`), `legacy_hits.rs` (removed in Phase 10, X3; its
`/proc` caller lookup is `peer_pid.rs`), `control.rs`, `wifi.rs`,
`ops/` (all of it, with the `tests.rs` subdirectories), `at.rs`, `watchdog.rs`. Entry point:
`serve.rs` (datad's `main.rs`). All 345 of datad's unit tests run in u60d.
Not moved: `rust/tests/legacy_golden.rs` (it needs `CARGO_BIN_EXE_datad` and data-service's
`tests/`); a local parity script (workspace tooling, not in this repo) runs those harnesses instead.

Changes against the base, all deliberate:

- edition 2024 for the whole crate (datad already was).
- `block::NoSink` is no longer test-only (`--once` uses it).
- `model::DatadVersion` reports `u60d` and its own version (`/version`, `datad` in the
  `--once` snapshot; `server.rs`'s `version_shape` test says `u60d`); log lines say `u60d:`.
  Nothing on the device, in the touch UI, agent or guard reads that field (grep 10-08).
- `sms.rs`, `state.rs`, `project/snapshot.rs`: the typed projection (`contract::` types),
  (The shadow mode's `readonly.rs` checks were removed in Phase 10, X2.)
- The 4 state tests and `lock_and_at_waits_count_as_progress` that ran on a multi-thread
  runtime run on `current_thread` (E2 item 7: the per-round bound and the lock/AT waits
  must not depend on a second worker thread; they pass).
- `executor/tests.rs`: the source scan also reads `read.rs`; it and `server.rs` each build
  the ubus backend once (they never run in the same process).
- Test fixture paths: `tests/…` in the crate instead of `../tests/…`.
- `state.rs`: `blocking()` is `pub(crate)`; the SMS session switch (below).
- `control.rs` (E2 item 2): `wifi.power_save`'s hotplug script write (create dir, write,
  chmod, rename, remove the old `psm`) goes through `blocking()`; same order, same errors.
- `serve.rs` against datad's `main.rs`: `current_thread` runtime (built in `main.rs`) instead of
  `#[tokio::main]`; `Args::parse_from(argv)`; `--version` prints u60d's version
  (`datad 0.1.0`; nothing calls it); the listener test moved here, plus argv[0] and
  `Args` tests.
- `state.rs`: synchronous file reads in the collection round (E2 item 1: `/etc/config`
  parsing, thermal zones, `/proc` and `/sys` runtime, `key.log` tails, leases, power
  rails, the MU5252 TCP/ICG reads) go through `blocking()` (`spawn_blocking`), so on
  u60d's single-thread runtime they no longer stop other tasks. Same order as before;
  the parity gate checks it.
- `state.rs`: the SMS session key is registered in serve mode (`state::SMS_SESSION`, set by
  `serve.rs`: u60d then replaces datad, so there is no other key to displace), and in the
  other modes only with `LUYAZ_SMS_SESSION=1` (off by default; the parity gate sets it).
  Shadow mode never registers it. See the hazard below.
- T22 (D23, 2026-10-08): an E4 write has two identities, `source` (who proposed it; wire name
  unchanged) and `approved_by` (who approved it: screen or web when a person confirmed someone
  else's proposal, otherwise the same as `source`). New `Source::Mcp` (`"mcp"`, automatic by itself).
  Optional top-level `approved_by` in `/control` bodies; it needs a `source`, and differing from it
  it must be screen or web, else 400 `invalid_parameter`. Every user/override/rollback/owner decision
  reads `approved_by` (`ops::txn::Who`). `approved_by` is written only when it differs from
  `source` (op block, `/control` replies, busy `doing`, journal lines, `owners.json`,
  `pending.json`, `last.json`), and a missing one reads as `source`: today's callers never send it,
  so every output and file is byte-identical (parity gate green). A rollback to datad reads these
  files as long as nothing sent `source:"mcp"` (datad does not know that source and would set a
  `pending.json`/`last.json` holding it aside as unreadable).
- D3 (2026-10-08, `ops/wifi_lock.rs`): `wifi.apply`, `wifi.reload`, `wifi.set_module` (the writers
  of the `wireless` uci package) take the write lock and then `/tmp/luyaz-wifi.lock`, the flock guard
  and agent use (same file, `flock(LOCK_EX|LOCK_NB)` polled, pid written in; guard/agent unchanged).
  Each try is one executor task (write lock → one non-blocking Wi-Fi try → write); when the Wi-Fi
  lock is busy both are released and the wait (poll 1 s) happens outside the executor, so neither
  the collection rounds nor the write lock are held for 150 s, and a reverse-order holder gets the
  write lock in the gap. 150 s without it: a `lock_wait` journal line, start again; after 2 rounds
  a `failed`/`wifi_lock` line and 503 `busy`. The wait runs in its own task, so a caller that hangs
  up (touch-ui closes after 5 s) does not cancel it: the write still happens later, as executor
  jobs always did. Guard and agent call these actions while already
  holding the Wi-Fi lock, so when the lock file's pid is the local caller or its ancestor
  (`/proc` lookup of the TCP peer, done only when the lock is busy) u60d does not wait for it;
  if the caller's process can't be found, or the write lock itself timed out, it writes as before
  D3. Knobs: `LUYAZ_WIFI_LOCK` (empty = off), `LUYAZ_WIFI_LOCK_WAIT_MS` (150000),
  `LUYAZ_WIFI_LOCK_POLL_MS` (1000), `LUYAZ_WIFI_LOCK_TRIES` (2). `wifi.power_save` and
  `nfc.set` do not write `wireless` and are not covered (the agent doesn't lock for them either).
- Batch 4 step 4-0b (2026-10-13, `ops/wifi.rs`, `ops/wifi_dev.rs`): new transaction actions
  `wifi.settings` (item `wifi.main`, body = agent PUT /api/wifi/settings) and `wifi.guest` (item
  `wifi.guest`, body = PUT /api/wifi/guest), source required (no legacy queue). Value = the `wifi`
  block flattened (`band_2g.ssid`, `on`, `guest.on_5g`…), passwords only as `*.key_fp` (HMAC with
  `apn.key`; old/new keys in `wifi-vault.json`, 0600); no-op → `{"status":"ok","note":"no changes"}`
  without a txn; Wi-Fi 6 change → 409 `refused`; Wi-Fi lock taken before submit (5 s, else 503
  `busy`), held until the background `reload_and_verify` ends; `Confirm::Wifi` = readback + APs up,
  no data/DNS check, deadline `LUYAZ_DEADLINE_WIFI_MS` (90000). datad has neither action.
- Env names were datad's `DATAD_*` so one mock harness drove both programs; since the Luya-Z
  rename D3 they are `LUYAZ_*`, old names read as a fallback (`src/env.rs`).

## Serve mode (`serve.rs`)

`u60d serve <datad arguments>`, or any argv[0] whose file name starts with `datad` or
`luyaz-core` (since Phase 10 the device runs it as `/data/luyaz/luyaz-core`, init
`/etc/init.d/luyaz-core`, state `/data/luyaz/core/`; before that `/data/plugins/datad/datad`;
the side-by-side `.test` names that `luyaz ship` (the maintainer's Mac-side workspace tool, not part of this repo; it drives `touch-ui/scripts/u60-ship.sh`, installed as `/data/luyaz/scripts/luyaz-ship.sh`, format in `touch-ui/docs/SHIP.md`; without it, use the install kit `onboard/`) and `luyaz-fallback.sh` recognise by prefix): the whole command line is datad's, parsed by
datad's `Args` (clap; `-i`, `-b/--bind`, `-p/--port`, `--lan-bind`, `--lan-port`,
`--auth-token-file`, `--data-dir`/`LUYAZ_DATA_DIR` default `/data/datad`, `--once`, hidden
`--webshell`/`--neighbor`, raw `--ubus-compare` and `--compare-state-shape`). Same ports, pid file
(`/var/run/datad.pid`), data directory, ops directory, write lock; `BUILD_MARKER`
(`LUYAZ_CORE_BUILD_SELF_CONTAINED`; `DATAD_FORK_RUST_SELF_CONTAINED` before D3) is kept for `build-kit.sh`. `datad --once` here goes
through datad's `App` like datad; `u60d --once` keeps using `read.rs`. The other u60d modes
(ledger, `--probe`, `--once`, `--evlog-stats`, `--ubus-compare`) are unchanged. When serving ends the process exits (P2-4), as in datad.

Runtime: one `current_thread` runtime. Checked against the E2 list (u60-platform.md):
no `block_in_place`, no `rt-multi-thread` anywhere in the feature graph (`cargo tree -e features`);
AT commands (`at.rs`) and the ops journal writer (`ops/record.rs`) have their own threads, the
watchdog is its own system thread; the write lock and AT lock waits poll with `try_lock` +
sleep (tokio sleep for the write lock, the AT thread's own sleep for AT), script and `iw` runs
are `tokio::process` (forked on the runtime thread, so pdeathsig follows the process' main thread).
Sync fs left on the runtime thread, small or rare:
- `ops/pending.rs` (`pending.json`, `last.json`, `notice.json`: write + fsync + rename + fsync dir;
  `takeover` remove): called inside the E4 engine's `std::sync::Mutex` from synchronous code, a
  few times per E4 write and every 10 s while one is being verified. On flash an fsync can take
  tens of ms; HTTP/SSE waits that long. Not moved on purpose: the write order under the engine
  lock is what keeps `pending.json` right (a `save` that dropped the lock to await could land
  after a later `clear` and leave a stale transaction that is resumed after a restart). Way out,
  not done (changes E4 durability timing, needs a decision): one ordered writer thread, jobs
  queued under the engine lock, the caller awaiting the ack after releasing it and before the
  device write.
- `ops/mod.rs` `now_ms` / `boot_id` (`/proc/uptime`, `boot_id`), `auth.rs` 24 bytes of
  `/dev/urandom`: procfs reads, microseconds.
- start-up only: `server::write_pid_file`, `ops::pending::Store::open`,
  the engine's `load`/`load_last`/`take_takeover`, the auth token file.
`peer_pid::find_pid`'s `/proc` scan for the caller already runs in `spawn_blocking`.

Release build: `cargo zigbuild --profile release-device --target aarch64-unknown-linux-musl -p luyaz-core`
(workspace root profile `release-device`: datad's settings, opt-level "s", panic=abort, lto,
codegen-units 1, strip; other crates keep the default `release`). 2026-10-08: 4.22 MB
(datad 68416eb 3.50 MB).

## Phase 4: policies (`plugin.rs`, `plugin/`)

Not from datad: the policy host and the policies moved from the old agent (u60-platform.md Phase 4).
Shadow only while the agent runs: computed and shown on `GET /debug/policy`, never acted on;
`/v2` and every other endpoint are unchanged (the parity gate's 171 GET replies still match datad).
`LUYAZ_POLICIES=0` turns them off.
- datad files touched: `block.rs` (`Hub::set_after_read`, called after a successful block read,
  outside the hub lock; `Hub::view` no longer test-only), `state.rs` (`host_path` is `pub(crate)`),
  `server.rs` (install on the hub, the `/debug/policy` route), `main.rs` (`--plugin-check [dir]`),
  `server/write.rs` (`handle` wraps the old body as `handle_inner` and, after a successful
  `power.direct_supply.set`, tells `plugin/charge.rs`), `control.rs` (`direct_supply_enabled` is
  `pub(crate)`).
- charge_policy (agent `src/charge_policy.rs`, step 3) runs after every battery and charger block
  read: proposals instead of writes, paired on `/debug/policy` with the agent's own writes
  (source auto) within 30 s. The agent's manual override is mirrored from successful web/screen
  charge writes and cleared when the limit file's mtime changes; a web/screen write within 2 s of a
  file change is the settings save's own enforce, not a manual stop (`plugin/charge.rs`).
  `LUYAZ_POLICIES=0` turns off both the hub hook and the `/control` observation.
- battery_eta (agent `src/battery_eta.rs`, policy code in crates/luyaz-policy) runs in the
  `after_read` of the `battery` block, on the runtime thread: four sysfs reads, the agent's
  `charge_limit.json`, one policy call (built-in about 10 µs; component about 50 µs, capped by fuel
  at about 10 ms) every 5 s. The agent's manual charge override is in its memory only and counts as
  mirrored by `plugin/charge.rs` since step 3.
- Components: `LUYAZ_PLUGINS` (default `<state dir>/plugins`: `/data/luyaz/core/plugins`, formerly `/data/u60d/plugins`), `plugins.json` + `.cwasm` from
  crates/luyaz-plugins/build.sh. wasmtime 48.0.5 runtime only (`runtime`, `component-model`, `std`);
  the Config is `plugin/config.rs`, shared with `plugc`. 2026-10-08: the release binary went from
  4.24 MB to 4.77 MB (+528 KB; the limit is 3 MB).

## Not moved yet / left for later steps

- The 19 std `Mutex`/`OnceLock` statics (E2 item 6) and the `Send` bounds (item 7) are as in datad.
- `ops/pending.rs` sync writes (above): the task said to move them; left, needs a decision.
- `state::SMS_SESSION` is one more static (an `AtomicBool`), outside the 19.
- Nothing deploys serve mode yet: build-kit.sh / `luyaz ship datad` still take datad's
  binary, and they would need the `release-device` output path.

## datad commits after the base still to carry over

None (2026-10-08; `5fb965c` only fixes `tests/rust_control_integration.sh`, which the gate takes from it).

## Check

Removed in Phase 10 (X2, 2026-10-14) with the shadow mode: datad no longer runs on the device,
so there is nothing left to compare with. What it did: a local parity script (workspace tooling, not in this repo): builds datad at the base and
u60d, runs every mock case (80 at 68416eb) through both `--once`, and requires byte-identical
snapshots and line-identical ubus/uci call logs. Then the contract check, the shadow cases, and
serve mode: the base commit's own harnesses (`golden.sh check`, `control_golden.sh check` with
`control_contract.py`, SSE limit and lifecycle, legacy hits, direct supply, listen orphan,
control integration, version test) on datad and on u60d copied as `datad`, and every GET
reply (`/v2/screen` included) per case, byte for byte. Masked: `{name,version}` and the caller
pids/times in `/debug/legacy-hits`; golden files are not edited.

## Shadow run on the device (removed)

`u60d --shadow`, `src/shadow.rs` and its read-only gate `readonly.rs` ran u60d next to datad
and compared the two (Phase 2 step 6). Removed in Phase 10 (X2, 2026-10-14); in git history.

## Shadow run on the device: hazards found while moving

- `sms::prepare` registers a fresh session key with the vendor
  (`zwrt_web web_http_enstr_set`). Read from the code, not verified on the device:
  a second process doing that would replace datad's key, and datad caches its key
  until restart, so datad (and the `sms_list` block that agent SMS forwarding reads)
  would stop decrypting. Hence `LUYAZ_SMS_SESSION`, off by default. The mock has no
  certificate, so this write never shows up in the parity call logs.
- AT (`at.rs`) is only reached from `/control` actions, which shadow mode does not serve;
  shadow mode must not open the modem AT port (`readonly.rs` does not cover device files).

## Read path calls

Every distinct call in the 80 parity cases (`cut -f1,2` of the call logs, 10-07).
All are reads (the shadow mode's allow-list until Phase 10).

- ubus: `system board|info`; `network.interface.{lan,zte_wan,zte_wan6,waneth,zte_mwan2,
  zte_mwan3,zte_mwan3_6,zte_mwan4,zte_mwan4_6} status`; `mwan3 status`;
  `zte_nwinfo_api nwinfo_get_netinfo|nwinfo_get_msim_netinfo`;
  `zwrt_bsp.{battery,charger,usb,typec} list`, `zwrt_bsp.powerbank get`, `zwrt_bsp.thermal get_cpu_temp`;
  `zwrt_data get_wwandst|get_wwandst_clearday|get_wwandst_monthlimit|get_wwaniface`;
  `zwrt_nfc zwrt_nfc_wifi_get`;
  `zwrt_apn_object get_apn_mode / get_apn_at_cid {cid:1} / get_manu_apn_list / get_auto_apn_list` (APN block, 3-3);
  `zwrt_router.api router_get_lan_info|router_get_status_no_auth|router_lan_access_list|
  router_wireless_access_list`;
  `zwrt_router.api router_get_firewall_para / router_get_upnp / router_get_portforward_rule /
  router_get_macipport_filter_rule / router_get_alg_para / router_get_domainfilter_rule` and
  `uci get zwrt_smart_mng.{smart_qos.mode,smart_mng.xdpi_support}` (router block, 4-0);
  `zwrt_wlan wlan_get_guest_access_left_time` (wifi block, 4-0, only while guest Wi-Fi is on);
  `zwrt_wms zte_libwms_get_sms_data|zwrt_wms_get_wms_capacity`;
  `zwrt_zte_mdm.api get_imei|get_sim_info|get_v3t_sim_info|get_zwrt_common_info`;
  `uci get`; with `LUYAZ_SMS_SESSION=1` also `zwrt_web web_crt_get` (+ the
  `web_http_enstr_set` write on a real device).
- uci CLI: `uci -q show <package>` and `uci -q get <path>` (only when the
  package can't be parsed directly).
- Not in the logs: `iw` (Wi-Fi survey, read-only) and MU5252's `adb`.
