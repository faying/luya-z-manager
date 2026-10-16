# Development notes

**English** · [中文](DEVELOPMENT.zh-CN.md)

The current touch UI is the **Rust version** in `rs/` (Rust + LVGL v9.5 + FreeType; device program `luyaz-ui`). The C/LVGL version (`src/`, `include/`, `make`, `scripts/build-docker.sh`)
was deleted on 2026-10-15 and is kept at git tag `legacy-c-touch-ui`; the older litehtml version is at tag `legacy-litehtml`.

## Code layout

| File | Purpose |
|---|---|
| `rs/devui/src/main.rs`, `lib.rs` | Entry point: DRM, touch, keys and backlight; main loop |
| `rs/devui/src/ui/*.rs`, `kit.rs`, `theme.rs` | Layout of the 5 tabs and their subpages (one file per page), shared widgets, light/dark themes and font loading |
| `rs/devui/src/logic.rs`, `exec.rs` | Decision logic with no LVGL dependency (testable on its own); `exec.rs` re-execs the UI for a theme or language switch |
| `rs/devui/src/data/datad.rs`, `json.rs` | Read data from the core (`luyaz-core`, `/v2/state` + `/v2/events` on `127.0.0.1:9460`); writes go through its `/control`, and the emergency script `/data/u60-guard/u60-fallback.sh` runs only when the core cannot be reached |
| `rs/devui/src/data/screen_feed.rs`, `net_view.rs`, `op_view.rs` | The home card / status bar conclusions and the write-transaction row come ready-made from the core's `GET /v2/screen` (rules in manager `crates/luyaz-core`); this code only parses them |
| `rs/devui/src/data/core_api.rs` | Requests to the core's loopback API `127.0.0.1:9460/v2/api/...` (no token, no login); used by every feature module |
| `rs/devui/src/data/{esim,tailscale,speedtest,netinfo,scenario,alerts}.rs` | Feature backends: eSIM / APN and other writes go through the core |
| `rs/plat/` | Hardware interfaces (DRM, touch, keys, backlight), see [HARDWARE.md](HARDWARE.md) |
| `rs/lvgl-sys/`, `rs/devui-model/` | LVGL/FreeType bindings and build; the model types the pages draw |
| `rs/devui/src/conf.rs` | `devui.conf` (`/data/plugins/u60pro-devui/devui.conf`) |
| `patches/` | Small patch for LVGL v9.5.0 (synthetic bold in FreeType bitmap mode), applied by `rs/setup-src.sh` |
| `scripts/` | Device-side scripts: `*.init` (procd), `u60-ship.sh`, `u60-recover.sh`, `alert-lib.sh`, `doctor.sh` (a thin shim over `<core> doctor`, `doctor-legacy.sh` as fallback), `config-backup.sh`, `chaos.sh`, etc. |

The screen owner is not here: it is manager's `crates/luyaz-sentinel` (before 10-15: `u60-uid`, `src/uid.c`).

The UI design rules (hierarchy, color, tap feedback, confirmation style) are in the manager repo's `docs/DESIGN.md` §4.

## Build

See [README](../README.md#build). Key points:

- `rs/setup-src.sh` once: puts the patched LVGL v9.5.0 and FreeType sources into a docker volume.
- `rs/build.sh device [FILE]` builds the static aarch64-musl `luyaz-ui` (cargo zigbuild in the shared build image; the crates are members of the manager Cargo workspace).
- On the device: `tools/u60 ship touch` (it builds only `rs/`).

## Fonts

The load order is in `rs/devui/src/theme.rs`:

| Use | First choice | Fallback |
|---|---|---|
| Chinese and body text | The device's own `/usr/ui/fonts/ZTEZhengYuan.ttf` (loaded at runtime, not in the repo) | `/data/luyaz/ui/fonts/u60-cjk-fallback.ttf` (the old `/data/plugins/u60pro-devui/fonts/` if the new one is missing) → LVGL's built-in Montserrat (no Chinese) |
| Numerals | `fonts/Nunito-600/700/800.ttf` (OFL) | The device's own Roboto → the Chinese font |

All bold weights are synthetic. The startup log line `ui: fonts cjk=… numerals=…` shows which set was actually used.
The Chinese fallback font is cut from Resource Han Rounded (OFL) by `scripts/fonts/build-cjk-fallback.sh <output dir>`.
No font files go into the repo; the install kit (`DEVUI_FONTS_DIR`) carries them to the device.

## Tests

```sh
scripts/test/docker.sh                 # device-side shell scripts, run in a busybox container with every command stubbed
rs/build.sh test                       # Rust unit tests (logic, data parsing, render checks)
rs/smoke.sh                            # off-screen end-to-end run of the device binary against a fake core
rs/parity.sh MODEL_DIR REF_DIR OUT_DIR # pixel parity against the C render test (from a checkout of tag legacy-c-touch-ui)
```

Copy the device fonts from `/usr/ui/fonts/` on the device yourself; they don't go into the repo.

## Layout on the device

| Path | Contents |
|---|---|
| `/data/luyaz/ui/luyaz-ui` | UI binary (live slot; process name `luyaz-ui`) |
| `/data/luyaz/ui/fonts/`, `/data/luyaz/ui/touchsim` | Nunito and the Chinese fallback font; the touch simulator |
| `/data/plugins/u60pro-devui/` | Only `start.sh`, `devui.conf`, `esim.conf`, `operator-logos/` and `*.orig-backup` |
| `/data/luyaz/luyaz-sentinel`, `/etc/init.d/luyaz-sentinel` | Screen owner and watchdog; screen state in `/data/luyaz/screen/` |
| `/data/luyaz/luyaz-core`, `/etc/init.d/luyaz-core` | The core (data service; state in `/data/luyaz/core/`) |
| `/data/u60-guard/` | Device scripts (`doctor.sh`, `u60-ship.sh`, …) |

Autostart goes only through `/etc/init.d/<name> start` lines in `/etc/rc.local`; `enable` is not used.

Controlling the screen: `echo vendor > /tmp/luyaz-screen.ctl` hands it to the stock UI, `echo devui > /tmp/luyaz-screen.ctl` takes it back
(the old `/tmp/u60-uid.ctl` is gone). If the sentinel fails to start the UI twice in a row, it gives up and hands the screen back to the stock UI;
the counters are in `/data/luyaz/screen/attempts` and `/data/luyaz/screen/gave-up`.

## Trying a new build on a real device

Two hard constraints: **a UI that crashes on startup gets escalated by the firmware into a whole-device reboot loop**; **if the screen has no UI at all for a few minutes, the firmware also reboots the whole device**.
So try a new build under a different file name first and only move it to the live slot once it is stable; two UIs also must not fight over `/dev/dri/card0`.

Use `tools/u60 ship touch` (see [SHIP.md](SHIP.md)): it does exactly this (sentinel `hold`, side-by-side `luyaz-ui.test`, checks, atomic replace, `release`, rollback). Regular users use the install kit's `./install.sh devui`.

Notes:

- Find the test process with `pidof` (by process name), not `pgrep -f`: the latter matches the shell of this SSH command itself and kills it, so the remaining steps never run and the screen stays blank.
- Don't write to `/tmp/luyaz-screen.ctl` while the sentinel is not running; it will hang.
- The device has no scp/sftp; transfer files with `ssh … 'cat > path' < local-file`.
