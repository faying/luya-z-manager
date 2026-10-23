# CLAUDE.md — Luya-Z (ZTE U60 Pro) Project Guidelines

## Global requirement — ZTE firmware auto-update stays OFF, always

**The owner's hard rule, and a design requirement for everything in this
project: ZTE's firmware auto-update (FOTA) is never turned on.** Not by code,
not by a script, not by a UI toggle, not "just to test".

- Why: a firmware upgrade overwrites `/etc/rc.local` (every autostart in this
  project is gone), and newer firmware drops the interface used to enable
  ADB (`zwrt_bsp.usb set`: gone on CN B31, reported gone from B28) — upgrade once
  and the kit's way in is gone.
- The kit already enforces it: `onboard/device/install.sh` → `fota_off()` sets
  `dm_update_mode=0`; `install.sh status` reports it as 「自动升级: 已关闭」.
- "Off" looks like: `uci get zwrt_zte_dm.dm_update.dm_update_mode` = `0` and
  `zwrt_zte_dm.dm_update.TURNOFFPOLLING` = `1`.
- Never add anything that can flip it back: no `set_update_mode` with a
  non-zero mode, no `confirm_download`/`confirm_install`, no admin-web
  control for it, no scenario action that reaches it (it is not, and must not
  be, in `scenario.rs`'s `ALLOWED_PATHS`).
- `zte_dm` itself keeps running — it is in the boot sync barrier below. Off
  means the mode, not the daemon.
- Applies to the install kit too: nothing in `onboard/` may ever turn it on.
  `fota_off()` turning it OFF is exactly what the owner wants and stays.
- If you ever find it on, tell the owner. Do not "fix" it silently either way.
- The core (`crates/luyaz-core`) has no updater of its own (no external dependencies).

## The hardware in one table

| | |
|---|---|
| Device | ZTE U60 Pro, a 5G CPE router |
| System | OpenWrt 23.05.4, Qualcomm SDX75, aarch64 with musl libc |
| SSH | 192.168.0.1, port 2222 |
| Storage | rootfs read-only; `/zteoverlay/` is a writable overlay; `/data` is writable |

## What lives where

A single Cargo workspace holds everything:

- `crates/luyaz-core` — the core: `/data/luyaz/luyaz-core`, `/etc/init.d/luyaz-core`,
  state `/data/luyaz/core/`, API on `:9460`; `<core> doctor` is the health check
  (`/data/luyaz/scripts/doctor.sh` is a shim over it)
- `crates/luyaz-web` — admin web on `:9090` (LAN-only), password file `/data/luyaz/web.env`
- `crates/luyaz-sentinel` — `/data/luyaz/luyaz-sentinel`, owns the screen and the
  watchdog; control FIFO `/tmp/luyaz-screen.ctl`, state `/data/luyaz/screen/`
- `touch-ui/` — touch UI (Rust, `touch-ui/rs/`), device program `/data/luyaz/ui/luyaz-ui`
  (comm `luyaz-ui`, fonts + touchsim beside it); device scripts in `touch-ui/scripts/`
- `crates/luyaz-*` — libraries used by the core (policy, scenario, wifi, alerts, …)
- `onboard/` — install kit; `scripts/` — CHILL, Tailscale

## Boot gate: never `init.d … disable` a stock daemon

Boot is held by `zte_topsw_daemon`. It reads `/etc/config/zte_topsw_daemon.conf` and only lets the
system finish starting once **every daemon named there has registered with it**. Take one of them
away through init.d and the result is:

| Symptom | Why |
|---|---|
| **Screen frozen on the ZTE logo** | the UI is never drawn |
| **No WAN** | the mobile data call is never started |
| **Touch does nothing** | the mtdev2tuio bridge is never launched |

Everything else keeps running normally, which is what makes this so hard to track down.
**Listed in daemon.conf — never disable through init.d** (alphabetical):
`zte_dm`, `zte_router`, `zte_smart_manage`, `zte_topsw_apn`, `zte_topsw_data`,
`zte_topsw_devui`, `zte_topsw_fota_result`, `zte_topsw_key`, `zte_topsw_led`, `zte_topsw_mc`,
`zte_topsw_mdm`, `zte_topsw_nwinfo`, `zte_topsw_sleep_faw`, `zte_topsw_tr098db`,
`zte_topsw_wlan`, `zte_topsw_wms`.
**Not in daemon.conf — init.d disable is fine** (alphabetical):
`zte-topsw-tunnel`, `zte_mqtt_sdk_st`, `zte_topsw_diag`, `zte_topsw_dua`, `zte_topsw_get_brand`,
`zte_topsw_jwxk_query`, `zte_topsw_nfc`, `zte_topsw_samba`, `zte_topsw_tr069_sub`.
To switch off one of the daemon.conf entries, put `#` in front of its line in
`/etc/config/zte_topsw_daemon.conf`. `/etc/init.d/<name> disable` is not the way.
**Whiteouts.** `init.d … disable` also leaves **whiteout character devices** in
`/zteoverlay/etc-upper_a/rc.d/`, which quietly hide the ROM's start symlinks. Find them with
`ls -la /zteoverlay/etc-upper_a/rc.d/ | grep '^c'` and remove one with
`rm /zteoverlay/etc-upper_a/rc.d/<whiteout_file>`.
**After a boot**, `ubus call zwrt_topsw_daemon.sync get_sync_info '{}'` must answer with
`"noSyncModuleName": "sync success"`.

## When something is stuck

| Problem | Commands |
|---|---|
| Screen stays on the logo | `sh /usr/bin/mtdev2tuio.sh` (touch driver + bridge), then restart the UI daemon: `kill -9 $(pidof zte_topsw_devui); /usr/bin/zte_topsw_devui &` |
| WAN does not come up | IPv4 data call: `ubus call zwrt_qcmap_cli set_qcliiface '{"source_module":"zte_topsw_data","type":1,"enable":1,"sub_id":1}'`; IPv6: the same with `"type":2` |
| Is the data call up? | `ubus call zwrt_data get_wwaniface '{"source_module":"zte_topsw_data","cid":1}'` — want `"enable": 1` and `"connect_status": "connected"` |

## Scenario Engine — standing authorisation, and its limits

The scenario engine (`crates/luyaz-scenario`, run by the core) changes Wi-Fi by itself when the surroundings
change. The device owner authorised that specific behaviour on 2026-09-22.
Treat it as a narrow exception to the "ask before changing Wi-Fi" rule below,
not as a general licence:

- **Authorised, no prompt:** the engine enabling/disabling the AP interfaces
  (`wireless.main_2g/main_5g.disabled`) via `wifi_radio::apply`
  and the CHILL region/on-off actions,
  when driven by a scenario the owner configured.
- **Still requires asking, every time:** eSIM profile switches, APN changes,
  network-mode/band locking, reboots, and edits to `/etc/rc.local`. The engine's
  `ALLOWED_PATHS` allow-list enforces most of this in code — eSIM is absent from
  it on purpose, because `esim.rs` reboots the device when a profile switch does
  not converge and this device is someone's only uplink.
- **Never:** widening `ALLOWED_PATHS` to reach `/api/device/reboot`,
  `/api/device/factory-reset`, `/api/system/kill-bloat` or the eSIM endpoints.
  A human typing a scheduler job is a different risk class from something that
  fires on its own when you walk into a room.

Two device facts the engine is built around, both measured, both easy to get
wrong again:

- **`ubus call zwrt_wlan reload` is not reliable.** The same uci write took
  effect in 8 seconds once and did nothing at all for a full 60 seconds another
  time. Any code that writes uci, reloads and assumes success is wrong. Poll
  until the change is observable.
- **`iw list`'s interface-combination table is advisory on this chip**, and the
  phy index changes on every reload. Discover the phy each time; never cache it,
  and never reason from that table.

## More device facts

- **The clock is local time labelled UTC.** ZTE's SNTP (`zwrt_zte_sntp`, `time_from_utc='8.00'`) sets the system clock to local wall time and leaves TZ=UTC, so `date` says "14:34 UTC" at 14:34 Beijing time and every device epoch/ISO-"Z" string is 8 h ahead of real UTC. Don't "fix" the firmware. On the device, format with localtime (right digits). Across to a browser/phone, use `clock.utc_offset` from `/api/public/status` (`crates/luyaz-api/src/clock.rs`): show device times with UTC formatting, compare with browser time via the offset.
- **Airplane mode bug**: `nwinfo_set_mode ONLINE` does NOT recover modem from LPM. Only fix: reboot. Re-checked on B31 (2026-10-05): vendor ONLINE flips to OFFLINE and falls back to LOW_POWER; `AT+CFUN=1` blocks on the AT port (no answer). Never put the device in LPM to test anything. The stock UI has no airplane switch at all.
- **Charging switch is backwards**: in wall mode, `enable` stops charging and `disable` starts it.
- **procd restarts what you kill**: a `kill -9` can just bring the process back; stop services with `/etc/init.d/<name> stop`.
- **Touch controller**: Sitronix on I2C `1-0055`; `/usr/bin/mtdev2tuio.sh` loads its module `sitronix-ts.ko`.
- **eSIM**: no embedded eUICC, but a removable eUICC card (5ber / eSTK.me) in the SIM slot works via lpac under `/data/esim` (`scripts/esim/`, `crates/luyaz-esim`, web `crates/luyaz-web/src/esim.rs`).
- **New devices**: `onboard/` is the prebuilt kit for other people's U60s (ADB → SSH → core/web/sentinel → touch UI → eSIM). Build with `onboard/build-kit.sh`; see DEPLOY.md.
- **IMEI** cannot be changed: it is fused into QFPROM.

## Admin web design, in brief

`docs/DESIGN.md` §5 decides; `.impeccable.md` holds the implementation notes. For `crates/luyaz-web` (Rust, server-rendered, `:9090`, LAN only):

- **Users**: device owner (technical, dense data OK) **and other U60 owners** — stay approachable. Often used on **phone/tablet on the LAN**.
- **Aesthetic**: new design in the **Cohere + Figma** language — warm paper canvas, hairline-ruled white cards, ink text, one near-black primary surface, flat colour blocks per route family, Cohere deep-green band on home/login. Geist + Geist Mono, platform CJK face. Light + dark. Tokens/CSS: `crates/luyaz-web/static/nd/`; templates: `crates/luyaz-web/templates/`.
- **Navigation**: <640 bottom tabs (Home / Charts / Functions / System), 640–1023 icon rail, ≥1024 sidebar; ⌘K search. Route table: `crates/luyaz-web/src/routes.rs`.
- **Writes**: three confirm tiers (direct / inline / dialog), readback after every write, multi-step writes tracked per step. Inventory: `docs/web/controls-inventory.md`.
- **Principles**: tokens never hard-codes; mobile is a real target (no h-scroll, ≥44px targets, inputs ≥16px); WCAG AA floor (`docs/web/contrast.md` must pass); never remove an existing control without asking.
