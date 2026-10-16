<div align="center">

# Luya-Z Manager

**A replacement admin web, touch screen UI and on-device core for a 5G portable hotspot.**

Clear signal readouts, band and cell locking, eSIM, SMS, health checks and a watchdog, on the device's front panel and in your browser.

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/Rust-1.99-orange.svg)
![Target](https://img.shields.io/badge/target-aarch64--musl-lightgrey.svg)
![UI](https://img.shields.io/badge/UI-English%20%C2%B7%20%E4%B8%AD%E6%96%87-green.svg)

**English** · [中文](README.zh-CN.md)

<img src="docs/images/web-home-desktop.png" width="600" alt="Admin web home page: signal summary, bandwidth, RSRP/SINR, speeds, battery">&nbsp;&nbsp;<img src="docs/images/touch-home.png" width="200" alt="Touch UI home screen">

</div>

> [!NOTE]
> **Hardware:** runs on the ZTE U60 Pro (MU5250). Community project, not affiliated with ZTE. Use at your own risk.

## Contents

- [Why](#why) · [Screenshots](#screenshots) · [Features](#features) · [Requirements](#requirements) · [Quick start](#quick-start)
- [Architecture](#architecture) · [Development](#development) · [Documentation](#documentation) · [Credits](#credits) · [License](#license-and-disclaimer)

## Why

The stock firmware hides most of what the modem knows behind a slow web UI and a front panel that shows little more than bars. Luya-Z puts one local API in front of the device (`luyaz-core`) and builds two interfaces on it:

- an **admin web** for phone and desktop that tells you *why* the connection is slow, not just that it is;
- a **touch UI** for the 3.5-inch front panel, organized by task, so the common jobs need no computer at all.

Everything is installed with **one command** from a kit you build yourself, and can be rolled back.

## Screenshots

All screenshots use the English UI with made-up data. **[Feature tour](docs/SCREENSHOTS.md)**: what every page does, with screenshots.

### Admin web

| Signal | Clients |
|---|---|
| <img src="docs/images/web-signal.png" width="420" alt="Signal page: RSRP/SINR trend and serving cells"> | <img src="docs/images/web-clients.png" width="420" alt="Connected devices with traffic"> |
| **Last 5 minutes** | **Band lock** |
| <img src="docs/images/web-trends.png" width="420" alt="Speeds, CPU, memory and battery over 5 minutes"> | <img src="docs/images/web-bandlock.png" width="420" alt="LTE and 5G band lock"> |
| **SMS** | **Change log** |
| <img src="docs/images/web-sms.png" width="420" alt="SMS inbox"> | <img src="docs/images/web-changes.png" width="420" alt="Change log with transactions"> |

### Touch UI (front panel)

| Home (dark) | Cellular | Wi-Fi | Band lock | System |
|---|---|---|---|---|
| <img src="docs/images/touch-home-dark.png" width="150" alt="Home, dark theme"> | <img src="docs/images/touch-cellular.png" width="150" alt="Cellular tab"> | <img src="docs/images/touch-wifi.png" width="150" alt="Wi-Fi tab"> | <img src="docs/images/touch-bandlock.png" width="150" alt="Band lock"> | <img src="docs/images/touch-system.png" width="150" alt="System tab"> |

## Features

| Area | What you get |
|---|---|
| **Signal and network** | RSRP/RSRQ/SINR with plain-language verdicts, carrier aggregation, serving cells, trends, band and cell locking, network mode, APN |
| **Admin web** | Replaces the stock web UI. Server-rendered Rust with Datastar; phone and desktop, light and dark, English and Chinese |
| **Touch UI** | Five task-based tabs (Home, Cellular, Wi-Fi, Route, System) on the front panel; see [touch-ui/README.md](touch-ui/README.md) |
| **SIM, eSIM, SMS** | SIM/PIN, SMS inbox and sending, SMS forwarding; download, switch and delete profiles on a removable eUICC card (5ber, eSTK.me and similar) |
| **LAN and Wi-Fi** | Wi-Fi and guest Wi-Fi, LAN/DHCP, DNS, firewall, QoS, USB mode, clients |
| **Reliability** | procd supervision for the core, the admin web and the sentinel; Wi-Fi fallback watchdog; alert banners, a Health page and optional alert SMS; a change log of every setting change; a network-mode change that cannot connect is reverted automatically |
| **Operations** | `./install.sh doctor` read-only health check, `backup` / `restore` for configuration, per-component updates with rollback ([DEPLOY.md](DEPLOY.md)) |
| **Install kit** | One `./install.sh` on a new device sets up SSH, the admin web, the touch UI and eSIM, and turns off automatic firmware updates |

A proxy is **not** included; [docs/PROXY.md](docs/PROXY.md) explains how to set one up yourself from official sources.

## Requirements

| | |
|---|---|
| Device | ZTE U60 Pro (MU5250) |
| Firmware for a fresh install | **B27 or earlier** (from B28 ZTE removed the interface used to enable ADB). **Do not update the firmware before installing.** |
| Build machine | Docker (macOS or Linux); the kit builds everything inside containers |
| Binaries | None published yet: you build them yourself, the guide has every step |

## Quick start

**Starting from scratch? Read [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md)**: what you need, how to build the install kit, how to install, check and roll back.

```sh
./onboard/build-kit.sh                 # → onboard/dist/luyaz-kit-YYYYMMDD.tar.gz
# unpack the kit, then on the computer connected to the device:
./install.sh                           # first install
./install.sh doctor                    # read-only health check
```

Then open `http://<device>:9090` in a browser on the device's network.

## Architecture

```
luyaz-core :9460 ──▶ luyaz-ui (touch screen)
       ▲  └──▶ lpac (/data/esim) ──▶ eUICC card
browser ──▶ luyaz-web :9090 (admin web) ──▶ luyaz-core
luyaz-sentinel: screen owner + watchdog
```

| Component | On the device |
|---|---|
| `crates/luyaz-core` | The core: `/data/luyaz/luyaz-core`, API on `127.0.0.1:9460` (`/v2/state`, `/v2/events`, `/v2/screen`, control API), supervised by procd (`/etc/init.d/luyaz-core`); turns ubus, AT commands and sysfs into one local API; `luyaz-core doctor` is the health check |
| `crates/luyaz-web` | Admin web on `:9090` (LAN only, Rust), password file `/data/luyaz/web.env` |
| `crates/luyaz-sentinel` | `/data/luyaz/luyaz-sentinel`: owns the screen and is the watchdog (Wi-Fi fallback, alerts) |
| `touch-ui/` | Touch UI (Rust + LVGL, `touch-ui/rs/`), device program `/data/luyaz/ui/luyaz-ui`; device scripts in `touch-ui/scripts/` |
| `crates/luyaz-*` (others) | Libraries used by the core: policy, scenarios, Wi-Fi, alerts, eSIM, speed test, … |
| `onboard/` | The install kit |

## Development

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

<details>
<summary>Repository layout</summary>

```
crates/        luyaz-core (core), luyaz-web (admin web), luyaz-sentinel (screen owner + watchdog), luyaz-* libraries
touch-ui/      touch UI: rs/ (Rust + LVGL), scripts/ (device scripts), docs/
onboard/       install kit: build-kit.sh (build), install.sh (install), device/ (on-device scripts), test/ (sandbox tests)
scripts/       esim/ (lpac toolkit), tailscale/ (boot start for Tailscale, see its README)
docs/          GETTING-STARTED.md, SCREENSHOTS.md, DESIGN.md (UI design spec), RELIABILITY.md (reliability contract)
```

</details>

## Documentation

Each doc links to its Chinese copy (`*.zh-CN.md`). RELIABILITY.md and DESIGN.md are Chinese only for now.

| Doc | For |
|---|---|
| [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md) | Quick start (begin here) |
| [docs/SCREENSHOTS.md](docs/SCREENSHOTS.md) | Feature tour: every page of the admin web and the touch UI |
| [onboard/README.md](onboard/README.md) | The install kit: what goes where, daily use, restoring stock, FAQ |
| [DEPLOY.md](DEPLOY.md) | Updating a device that is already set up |
| [docs/RELIABILITY.md](docs/RELIABILITY.md) | File contract between the core, the sentinel and the scripts (Chinese) |
| [docs/DESIGN.md](docs/DESIGN.md) | Design spec for the admin web and the touch UI (Chinese) |
| [touch-ui/README.md](touch-ui/README.md), [touch-ui/docs/](touch-ui/docs/) | Touch UI build, hardware, eSIM, speed test |
| [CLAUDE.md](CLAUDE.md) | Device rules for AI coding assistants (humans should read the "don'ts" too) |

Contributions are welcome: see [CONTRIBUTING.md](CONTRIBUTING.md) and [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Security issues: [SECURITY.md](SECURITY.md).

## Credits

- [faying](https://github.com/faying): core (luyaz-core), admin web (luyaz-web), sentinel, Rust touch UI, install kit, eSIM, Tailscale.
- Wei REN: the earlier fork this work continued from; parts of the install kit and the eSIM probe come from it.
- [33333s](https://github.com/33333s): the core's device read and write layer comes from [zwrt-datad](https://github.com/33333s/zwrt-datad) (MIT); [u60pro-devui](https://github.com/33333s/u60pro-devui) was the starting point of the touch UI.
- [Jesther Silvestre](https://github.com/jesther-ai): this project started from [open-u60-pro](https://github.com/jesther-ai/open-u60-pro). Its agent and web UI have since been replaced.

## License and disclaimer

[MIT](LICENSE). Sources, removed content and third-party components are listed in [NOTICE](NOTICE) and [touch-ui/NOTICE](touch-ui/NOTICE).

Not affiliated with or endorsed by ZTE Corporation. Use only on your own device. Reverse engineering was done solely for interoperability and learning; the repo contains no ZTE proprietary source code.
