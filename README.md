# Luya-Z Manager

**English** · [中文](README.zh-CN.md)

Part of **Luya-Z**: the on-device core (`luyaz-core`), a browser admin web (`luyaz-web`), a screen owner and watchdog (`luyaz-sentinel`), the front-panel touch UI (`touch-ui/`) and a **one-command install kit** for a 5G portable hotspot. Everything is in this one repository.

> **Hardware:** runs on the ZTE U60 Pro (MU5250). Community project, not affiliated with ZTE. Use at your own risk.

| Admin web | Touch UI |
|---|---|
| <img src="docs/images/web-home-desktop.png" width="560" alt="Admin web home page"> | <img src="docs/images/touch-home.png" width="200" alt="Touch UI home screen"> |

## How the pieces fit

| Component | On the device |
|---|---|
| `crates/luyaz-core` | The core: `/data/luyaz/luyaz-core`, API on `127.0.0.1:9460` (`/v2/state`, `/v2/events`, `/v2/screen`, control API), supervised by procd (`/etc/init.d/luyaz-core`); `luyaz-core doctor` is the health check |
| `crates/luyaz-web` | Admin web on `:9090` (LAN only, Rust), password file `/data/luyaz/web.env` |
| `crates/luyaz-sentinel` | `/data/luyaz/luyaz-sentinel`: owns the screen and is the watchdog (Wi-Fi fallback, alerts) |
| `touch-ui/` | Touch UI (Rust + LVGL, `touch-ui/rs/`), device program `/data/luyaz/ui/luyaz-ui`; device scripts in `touch-ui/scripts/` |
| `crates/luyaz-*` (others) | Libraries used by the core: policy, scenarios, Wi-Fi, alerts, eSIM, speed test, … |
| `onboard/` | The install kit |

```
luyaz-core :9460 ──▶ luyaz-ui (touch screen)
       ▲  └──▶ lpac (/data/esim) ──▶ eUICC card
browser ──▶ luyaz-web :9090 (admin web) ──▶ luyaz-core
luyaz-sentinel: screen owner + watchdog
```

## Features

- **luyaz-core**: a Rust daemon that turns ubus, AT commands and sysfs into one local API: device, battery and temperature; signal and carriers; band and cell locking; SIM and SMS; APN; DNS, DHCP and firewall; Wi-Fi; USB mode; speed test; scenarios; and more.
- **Admin web** (`crates/luyaz-web`, server-rendered Rust with Datastar): replaces the stock web UI. Works on phone and desktop, light and dark, Chinese and English.
- **Touch UI** (`touch-ui/`): five task-based tabs on the 3.5-inch front panel; see [touch-ui/README.md](touch-ui/README.md).
- **eSIM**: download, switch and delete profiles on a removable eUICC card (5ber, eSTK.me and similar).
- **Proxy**: not included. [docs/PROXY.md](docs/PROXY.md) explains how to set one up yourself from official sources.
- **Reliability**: the core, the admin web and the sentinel are supervised by procd; Wi-Fi fallback watchdog; alert banners, a Health page and optional alert SMS;
  `./install.sh doctor` for a read-only health check, `backup` / `restore` for configuration. The contract is in [docs/RELIABILITY.md](docs/RELIABILITY.md) (Chinese).
- **Install kit** (`onboard/`): one `./install.sh` on a new device sets up SSH, the admin web, the touch UI and eSIM, and turns off automatic firmware updates.

## Quick start

**Starting from scratch? Read [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md)**: what you need, how to build the install kit, how to install, check and roll back.

Only firmware **B27 and earlier** can be installed from scratch (from B28 ZTE removed the interface used to enable ADB). **Do not update the firmware before installing.**
No prebuilt binaries are published yet; you build them yourself, and the guide has every step.

## Development and build

```sh
# Core and admin web (aarch64 musl; onboard/build-kit.sh builds them in Docker)
cargo zigbuild --profile release-device --target aarch64-unknown-linux-musl -p luyaz-core
cargo zigbuild --release --target aarch64-unknown-linux-musl -p luyaz-web

# Touch UI (Docker)
(cd touch-ui && rs/setup-src.sh && rs/build.sh device)

# Admin web browser checks (mock core in a container): see crates/luyaz-web/e2e/run.sh
(cd crates/luyaz-web && npm ci && e2e/run.sh)

# Install kit → onboard/dist/luyaz-kit-YYYYMMDD.tar.gz
./onboard/build-kit.sh
```

To update a single component on a device that is already set up, see [DEPLOY.md](DEPLOY.md).

## Layout

```
crates/        luyaz-core (core), luyaz-web (admin web), luyaz-sentinel (screen owner + watchdog), luyaz-* libraries
touch-ui/      touch UI: rs/ (Rust + LVGL), scripts/ (device scripts), docs/
onboard/       install kit: build-kit.sh (build), install.sh (install), device/ (on-device scripts), test/ (sandbox tests)
scripts/       esim/ (lpac toolkit), tailscale/ (boot start for Tailscale, see its README)
docs/          GETTING-STARTED.md, DESIGN.md (UI design spec), RELIABILITY.md (reliability contract)
```

## Documentation

Each doc links to its Chinese copy (`*.zh-CN.md`). RELIABILITY.md and DESIGN.md are Chinese only for now.

- [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md): quick start (begin here)
- [onboard/README.md](onboard/README.md): the install kit (what goes where, daily use, restoring stock, FAQ)
- [DEPLOY.md](DEPLOY.md): updating a device that is already set up
- [docs/RELIABILITY.md](docs/RELIABILITY.md): file contract between the core, the sentinel and the scripts (Chinese)
- [docs/DESIGN.md](docs/DESIGN.md): design spec for the admin web and the touch UI (Chinese)
- [touch-ui/README.md](touch-ui/README.md), [touch-ui/docs/](touch-ui/docs/): touch UI build, hardware, eSIM, speed test
- [CLAUDE.md](CLAUDE.md): device rules for AI coding assistants (humans should read the "don'ts" too)

## Credits

- [Jesther Silvestre](https://github.com/jesther-ai): the original project [open-u60-pro](https://github.com/jesther-ai/open-u60-pro) (first agent and web UI).
- Wei REN: core, admin web, touch UI rewrite, install kit, eSIM, Tailscale.
- [33333s](https://github.com/33333s): thanks for the reference repos that were the starting points of the touch UI and the local data service.

## License and disclaimer

[MIT](LICENSE). Sources, removed content and third-party components are listed in [NOTICE](NOTICE) and [touch-ui/NOTICE](touch-ui/NOTICE).

Not affiliated with or endorsed by ZTE Corporation. Use only on your own device. Reverse engineering was done solely for interoperability and learning; the repo contains no ZTE proprietary source code.
