# CLAUDE.md — ZTE U60 Pro Project Guidelines

## Device Overview
- **Model**: ZTE U60 Pro 5G CPE router
- **OS**: OpenWrt 23.05.4 on Qualcomm SDX75 (aarch64, musl libc)
- **SSH**: Port 2222 at 192.168.0.1
- **Filesystem**: Read-only rootfs, writable overlay at `/zteoverlay/`, writable `/data`

## Architecture
- `zte-agent/` — Rust HTTP agent on device (Axum, port 9090, LAN-only)
- `mobile/ios/` — SwiftUI companion app
- `mobile/android/` — Jetpack Compose companion app
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

## Other Device Notes

- **Airplane mode bug**: `nwinfo_set_mode ONLINE` does NOT recover modem from LPM. Only fix: reboot.
- **Charge policy bug**: Wall mode `enable` STOPS charging, `disable` STARTS charging (inverted).
- **procd respawn**: `kill -9` may trigger procd respawn. Use `/etc/init.d/<name> stop` instead.
- **Touchscreen**: Sitronix at I2C `1-0055`, kernel module `sitronix-ts.ko` loaded by `/usr/bin/mtdev2tuio.sh`.
- **eSIM**: no embedded eUICC, but a removable eUICC card (5ber / eSTK.me) in the SIM slot works via lpac under `/data/esim` (`scripts/esim/`, agent `esim.rs`).
- **New devices**: `onboard/` is the prebuilt kit for other people's U60s (ADB → SSH → agent/web → devui → eSIM). Build with `onboard/build-kit.sh`; see DEPLOY.md.
- **IMEI**: Hardware-locked (QFPROM fused), not modifiable.

---

## Design Context (Admin Web)

Full version in `.impeccable.md`. Summary for `web/` (Next.js admin served by the agent on `:9090`, LAN-only):

- **Users**: device owner (technical, dense data OK) **and shared with other U60 owners** — stay approachable. Often used on **phone/tablet on the LAN**, not just desktop.
- **Aesthetic**: **"Cool Tech"** system in `web/src/app/(panel)/admin.css` — **light**, clean minimalist-tech. Cool near-white canvas `#f6f8fb`, white cards, cool-slate neutrals, hairline borders, Outfit (display) + DM Sans (body), tabular numerals. **Electric-cyan accent**: deep `#0e7490` for contrast-bearing UI (buttons/links/active text, AA on white); bright `#22d3ee` (`--admin-glow`) for tints/rings/active dots ONLY (never text/fills on white). No gradients/glassmorphism/shimmer.
- **Mobile-first**: desktop sidebar; mobile = fixed **bottom tab bar** (Home/Signal/Wi-Fi/Services/More) + drawer. UI served at site **root** (`http://<ip>:9090/`).
- **Principles**: (1) tokens never hard-codes — use `.admin-theme` vars / Tailwind utilities (`bg-bg-card`, `text-accent`, `text-text-dim`); a raw hex is a defect. (2) density with rhythm, not walls of identical cards. (3) mobile is a real target — no h-scroll, ≥44px touch targets, bottom-tab reachable. (4) **WCAG AA floor** (≥4.5:1, labelled controls, visible focus, keyboard). (5) quiet confidence — deep cyan as the single sparing accent.
