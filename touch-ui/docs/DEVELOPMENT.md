# Development notes

**English** · [中文](DEVELOPMENT.zh-CN.md)

The touch UI is written in Rust on top of LVGL 9.5 and FreeType; it all lives in `rs/` and builds into one static program, `luyaz-ui`. Earlier C and litehtml versions exist only as git tags (`legacy-c-touch-ui`, `legacy-litehtml`).

## Where things are

**Crates**

| Path | What it is |
|---|---|
| `rs/devui/` | The UI program |
| `rs/plat/` | Hardware access: DRM scanout, touch, power key, backlight ([HARDWARE.md](HARDWARE.md)) |
| `rs/lvgl-sys/` | LVGL and FreeType compiled from source plus generated bindings; settings in `../lv_conf.h` |
| `rs/devui-model/` | Plain data types that the pages render |

**Inside `rs/devui/src/`**

| Path | What it is |
|---|---|
| `main.rs`, `lib.rs` | Start-up and the main loop |
| `ui/` | One file per page: five tabs (home, cellular, Wi-Fi, exit, system) and their subpages |
| `kit.rs`, `theme.rs` | Shared widgets; light and dark themes; font loading |
| `logic.rs` | Decisions that do not touch LVGL, so they can be unit-tested |
| `exec.rs` | Restarts the program in place after a theme or language change |
| `conf.rs` | Reads and writes `/data/luyaz/ui/devui.conf` |
| `data/core_link.rs` | Live state from the core: `/v2/state` and the `/v2/events` stream on `127.0.0.1:9460` |
| `data/screen_feed.rs`, `net_view.rs`, `op_view.rs` | Parse `GET /v2/screen`, where the core hands over finished conclusions for the home card, status bar and write-in-progress row |
| `data/core_api.rs` | Requests to the core's loopback API (`/v2/api/...`, no token); every feature module goes through it |
| `data/esim.rs`, `chill.rs`, `tailscale.rs`, `speedtest.rs`, `netinfo.rs`, `scenario.rs`, `alerts.rs`, … | Feature modules. All writes go through the core, except CHILL's group and node picks, which go straight to mihomo ([CHILL.md](CHILL.md)) |

If the core cannot be reached for a write, the UI falls back to the device script `/data/luyaz/scripts/luyaz-fallback.sh`.

**Elsewhere in `touch-ui/`**

| Path | What it is |
|---|---|
| `patches/` | One LVGL 9.5.0 patch (synthetic bold for FreeType bitmap fonts), applied by `rs/setup-src.sh` |
| `scripts/` | Device-side scripts: procd `*.init` files, `luyaz-ship.sh` ([SHIP.md](SHIP.md)), `luyaz-recover.sh`, `luyaz-fallback.sh`, `alert-lib.sh`, `doctor.sh` (forwards to `luyaz-core doctor`), `config-backup.sh`, `chaos.sh` and others |
| `tools/touchsim.c` | Injects taps, swipes and key presses for remote testing ([CHILL.md](CHILL.md#不在屏幕前也能验证)) |

Who owns the screen is not decided here: that is `crates/luyaz-sentinel`. Visual rules (hierarchy, colour, tap feedback, confirmations) are in the manager repo's `docs/DESIGN.md` §4.

## Building

Details in the [README](../README.md#构建); in short:

1. `rs/setup-src.sh`, once: puts the patched LVGL 9.5.0 and FreeType sources into a Docker volume.
2. `rs/build.sh device [FILE]`: builds the static aarch64-musl `luyaz-ui` with cargo-zigbuild in the shared build image (the crates are members of the manager Cargo workspace).
3. To put it on a device, use `luyaz ship touch`, which builds only `rs/`. That command is the maintainer's own workstation tool and is not in this repo; the device half it drives is `scripts/luyaz-ship.sh` (installed as `/data/luyaz/scripts/luyaz-ship.sh`). Everyone else installs with the kit in `onboard/`.

## Fonts

Chosen at start-up in `rs/devui/src/theme.rs`:

| For | Tried first | Then |
|---|---|---|
| Chinese and body text | The device's `/usr/ui/fonts/ZTEZhengYuan.ttf` | `/data/luyaz/ui/fonts/u60-cjk-fallback.ttf` (or the same file in the pre-rename `/data/plugins/u60pro-devui/fonts/`), then LVGL's built-in Montserrat, which has no Chinese |
| Numerals | `Nunito-600/700/800.ttf` in the font directory (OFL) | The device's Roboto, then the Chinese font |

Bold is always synthesised. The log line `ui: fonts cjk=… numerals=…` tells you what was picked. `scripts/fonts/build-cjk-fallback.sh <output dir>` cuts the fallback from Resource Han Rounded (OFL). Font files are never committed; the install kit (`DEVUI_FONTS_DIR`) brings them to the device.

## Tests

```sh
scripts/test/docker.sh                 # device shell scripts in a busybox container, every command stubbed
rs/build.sh test                       # Rust unit tests: logic, parsing, render checks
rs/smoke.sh                            # runs the device binary off-screen against a fake core
rs/parity.sh MODEL_DIR REF_DIR OUT_DIR # pixel comparison with renders from tag legacy-c-touch-ui
```

Tests that need the device fonts expect you to copy them from `/usr/ui/fonts/` on your own device.

## On the device

| Path | Contents |
|---|---|
| `/data/luyaz/ui/` | `luyaz-ui` (process name `luyaz-ui`), `fonts/`, `touchsim`, `devui.conf`, `chill.conf`, `operator-logos/`, `boot-trace.log`, `vendor-devui.orig-backup` (the stock UI, kept) |
| `/data/luyaz/luyaz-sentinel` | Screen owner and watchdog (`/etc/init.d/luyaz-sentinel`; state in `/data/luyaz/screen/`) |
| `/data/luyaz/luyaz-core` | The core (`/etc/init.d/luyaz-core`; state in `/data/luyaz/core/`) |
| `/data/luyaz/scripts/` | `doctor.sh`, `luyaz-ship.sh`, `core-trial.sh`, `luyaz-fallback.sh`, … |

The old `/data/plugins/u60pro-devui/` directory is retired. Services start from `/etc/init.d/<name> start` lines in `/etc/rc.local`; nothing uses `enable`.

The sentinel decides which UI is on screen. `echo vendor > /tmp/luyaz-screen.ctl` gives the screen to the stock UI, `echo devui > /tmp/luyaz-screen.ctl` takes it back. If `luyaz-ui` fails to come up twice in a row, the sentinel gives up and returns the screen to the stock UI; see `/data/luyaz/screen/attempts` and `/data/luyaz/screen/gave-up`.

## Trying a new build on real hardware

Two firmware behaviours make this risky:

- a UI that crashes at start-up is escalated by the firmware into a reboot loop of the whole device;
- a screen with no UI for a few minutes also gets the whole device rebooted.

So a new build first runs under a different file name, and only moves to the live slot once it has proven stable. Two UIs must never hold `/dev/dri/card0` at the same time.

`luyaz ship touch` does all of this ([SHIP.md](SHIP.md)): sentinel `hold`, start `luyaz-ui.test` beside the live one, check, replace atomically, `release`, roll back on failure. Users of the install kit run `./install.sh devui`.

Pitfalls:

- Find the test process with `pidof` (it matches the process name). `pgrep -f` also matches the SSH shell running your command and kills it, so the remaining steps never run and the screen stays empty.
- Writing to `/tmp/luyaz-screen.ctl` blocks if the sentinel is not running.
- The device has no scp or sftp; copy with `ssh <device> 'cat > PATH' < FILE`.
