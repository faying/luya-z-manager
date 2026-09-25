# CLAUDE.md — ZTE U60 Pro Project Guidelines

## Global requirement — ZTE firmware auto-update stays OFF, always

**The owner's hard rule, and a design requirement for everything in this
project: ZTE's firmware auto-update (FOTA) is never turned on.** Not by code,
not by a script, not by a UI toggle, not "just to test".

- Why: a firmware upgrade overwrites `/etc/rc.local` (every autostart in this
  project is gone), and from B28 on ZTE closed the interface used to enable
  ADB — upgrade once and there is no way back in.
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
- Not the same thing as the data service's own updater (zwrt-datad OTA): that
  one is removed from our fork for a separate reason (no external dependencies).

## Device Overview
- **Model**: ZTE U60 Pro 5G CPE router
- **OS**: OpenWrt 23.05.4 on Qualcomm SDX75 (aarch64, musl libc)
- **SSH**: Port 2222 at 192.168.0.1
- **Filesystem**: Read-only rootfs, writable overlay at `/zteoverlay/`, writable `/data`

## Architecture
- `zte-agent/` — Rust HTTP agent on device (tiny_http, port 9090, LAN-only)
- `web/` — Next.js web dashboard

---

## CRITICAL: ZTE Daemon Sync Barrier

`zte_topsw_daemon` is the master daemon. It reads `/etc/config/zte_topsw_daemon.conf` and **waits for ALL listed daemons to register** before releasing the boot sequence.

### If you disable a daemon listed in daemon.conf via init.d, the device will:
- **Display stuck on ZTE boot logo** (UI never renders)
- **WAN never connects** (mobile data call never initiated)
- **Touchscreen unresponsive** (mtdev2tuio bridge never starts)

All other daemons appear to run fine — making the root cause very hard to diagnose.

### Daemons in daemon.conf (NEVER disable via init.d):
```
zte_topsw_mc, zte_router, zte_topsw_data, zte_topsw_nwinfo,
zte_topsw_mdm, zte_topsw_sleep_faw, zte_topsw_apn, zte_topsw_wms,
zte_topsw_key, zte_topsw_led, zte_topsw_tr098db, zte_dm,
zte_topsw_fota_result, zte_topsw_devui, zte_topsw_wlan, zte_smart_manage
```

### Safe to disable via init.d (NOT in daemon.conf):
```
zte_topsw_diag, zte_topsw_samba, zte_topsw_nfc, zte_topsw_get_brand,
zte_topsw_jwxk_query, zte_topsw_tr069_sub, zte_mqtt_sdk_st,
zte_topsw_dua, zte-topsw-tunnel
```

### How to properly disable a daemon.conf daemon:
Comment it out in `/etc/config/zte_topsw_daemon.conf` (prefix with `#`). Do NOT use `/etc/init.d/<name> disable`.

### Overlay Whiteouts
`/etc/init.d/<name> disable` creates **whiteout character devices** in `/zteoverlay/etc-upper_a/rc.d/` that silently delete the ROM symlinks. These are invisible in normal `ls /etc/rc.d/` but persist across reboots.

- **Check**: `ls -la /zteoverlay/etc-upper_a/rc.d/ | grep '^c'`
- **Fix**: `rm /zteoverlay/etc-upper_a/rc.d/<whiteout_file>`

### Verify sync status after boot:
```sh
ubus call zwrt_topsw_daemon.sync get_sync_info '{}'
# Should return: "noSyncModuleName": "sync success"
```

---

## Recovery Commands

### Display stuck on logo
```sh
# Start touchscreen driver + bridge
sh /usr/bin/mtdev2tuio.sh
# Restart UI daemon
kill -9 $(pidof zte_topsw_devui); /usr/bin/zte_topsw_devui &
```

### WAN not connecting
```sh
# Enable IPv4 data call
ubus call zwrt_qcmap_cli set_qcliiface '{"source_module":"zte_topsw_data","type":1,"enable":1,"sub_id":1}'
# Enable IPv6
ubus call zwrt_qcmap_cli set_qcliiface '{"source_module":"zte_topsw_data","type":2,"enable":1,"sub_id":1}'
```

### Check data call status
```sh
ubus call zwrt_data get_wwaniface '{"source_module":"zte_topsw_data","cid":1}'
# Look for: "enable": 1, "connect_status": "connected"
```

---

## Scenario Engine — standing authorisation, and its limits

`zte-agent/src/scenario.rs` changes Wi-Fi by itself when the surroundings
change. The device owner authorised that specific behaviour on 2026-09-22.
Treat it as a narrow exception to the "ask before changing Wi-Fi" rule below,
not as a general licence:

- **Authorised, no prompt:** the engine enabling/disabling the AP interfaces
  (`wireless.main_2g/main_5g.disabled`) via `wifi_radio::apply`
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
  time. Any code that writes uci, reloads and assumes success is wrong —
  `scripts/homemode.sh`'s `apply_wifi()` is written that way. Poll until the
  change is observable.
- **`iw list`'s interface-combination table is advisory on this chip**, and the
  phy index changes on every reload. Discover the phy each time; never cache it,
  and never reason from that table.

## Other Device Notes

- **The clock is local time labelled UTC.** ZTE's SNTP (`zwrt_zte_sntp`, `time_from_utc='8.00'`) sets the system clock to local wall time and leaves TZ=UTC, so `date` says "14:34 UTC" at 14:34 Beijing time and every device epoch/ISO-"Z" string is 8 h ahead of real UTC. Don't "fix" the firmware. On the device, format with localtime (right digits). Across to a browser/phone, use `clock.utc_offset` from `/api/public/status` (`zte-agent/src/clock.rs`, `web/src/lib/deviceClock.ts`): show device times with UTC formatting, compare with browser time via the offset.
- **Airplane mode bug**: `nwinfo_set_mode ONLINE` does NOT recover modem from LPM. Only fix: reboot.
- **Charge policy bug**: Wall mode `enable` STOPS charging, `disable` STARTS charging (inverted).
- **procd respawn**: `kill -9` may trigger procd respawn. Use `/etc/init.d/<name> stop` instead.
- **Touchscreen**: Sitronix at I2C `1-0055`, kernel module `sitronix-ts.ko` loaded by `/usr/bin/mtdev2tuio.sh`.
- **eSIM**: no embedded eUICC, but a removable eUICC card (5ber / eSTK.me) in the SIM slot works via lpac under `/data/esim` (`scripts/esim/`, agent `esim.rs`).
- **New devices**: `onboard/` is the prebuilt kit for other people's U60s (ADB → SSH → agent/web → devui → eSIM). Build with `onboard/build-kit.sh`; see DEPLOY.md.
- **IMEI**: Hardware-locked (QFPROM fused), not modifiable.

---

## Design Context (Admin Web)

Single source of truth: `docs/DESIGN.md` §5. Summary for `web/` (Next.js static export served by the agent on `:9090`, LAN-only):

- **Users**: device owner (technical, dense data OK) **and other U60 owners** — stay approachable. Often used on **phone/tablet on the LAN**.
- **Aesthetic**: new design in the **Cohere + Figma** language — warm paper canvas, hairline-ruled white cards, ink text, one near-black primary surface, flat colour blocks per route family, Cohere deep-green band on home/login. Geist + Geist Mono, platform CJK face. Light + dark. Tokens: `web/src/app/newdesign.css`; components: `web/src/components/nd/`.
- **Navigation**: <640 bottom tabs (Home / Charts / Functions / System), 640–1023 icon rail, ≥1024 sidebar; ⌘K search. Route table: `web/src/lib/routes.ts`.
- **Writes**: three confirm tiers via `useWriteOp` (direct / inline / dialog), readback after every write, multi-step writes tracked per step. Inventory: `web/docs/controls-inventory.md`.
- **Principles**: tokens never hard-codes; mobile is a real target (no h-scroll, ≥44px targets, inputs ≥16px); WCAG AA floor (`web/docs/contrast.md` must pass); never remove an existing control without asking.
