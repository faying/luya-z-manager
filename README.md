<div align="center">

# Luya-Z Manager

**A replacement admin web, touch screen UI and on-device core for a 5G portable hotspot.**

Clear signal readouts, band and cell locking, eSIM, SMS, health checks and a watchdog, on the device's front panel and in your browser.

[![License: PolyForm Noncommercial](https://img.shields.io/badge/license-PolyForm%20Noncommercial-blue.svg)](LICENSE)
[![Latest release](https://img.shields.io/github/v/release/faying/luya-z-manager?sort=date&display_name=tag)](https://github.com/faying/luya-z-manager/releases)
![Rust](https://img.shields.io/badge/Rust-1.99-orange.svg)
![Target](https://img.shields.io/badge/target-aarch64--musl-lightgrey.svg)
![UI](https://img.shields.io/badge/UI-English%20%C2%B7%20%E4%B8%AD%E6%96%87-green.svg)

**English** · [中文](README.zh-CN.md)

<img src="docs/images/web-home-desktop.png" width="600" alt="Admin web home page: signal summary, bandwidth, RSRP/SINR, speeds, battery">&nbsp;&nbsp;<img src="docs/images/touch-home.png" width="200" alt="Touch UI home screen">

</div>

> [!NOTE]
> **Hardware:** runs on the ZTE U60 Pro (MU5250). Community project, not affiliated with ZTE. Use at your own risk.

> [!IMPORTANT]
> **Can I install it?** A fresh install works on firmware **B31 or earlier**. On B27 or earlier the installer gets in by itself; from B28 on ZTE removed that interface, so you need working SSH on the device first — how you open it is outside the scope of this project. Versions after B31 are untested. Check your version first ([how](docs/GETTING-STARTED.md#2-prerequisites)) and **do not update the firmware before installing**.

## Contents

- [Why](#why) · [Screenshots](#screenshots) · [Features](#features) · [Requirements](#requirements) · [Quick start](#quick-start)
- [What it changes, and how to undo it](#what-it-changes-and-how-to-undo-it) · [Architecture](#architecture) · [Development](#development)
- [Versions and updates](#versions-and-updates) · [Project status](#project-status) · [Documentation](#documentation) · [Credits](#credits) · [License](#license-and-disclaimer)

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
| **Proxy (CHILL)** | Optional transparent proxy on native mihomo: a generic template (one subscription, a small rule set), a panel behind the admin web, an exit page on the touch screen; installed separately with `./install.sh chill` ([docs/PROXY.md](docs/PROXY.md)) |
| **Install kit** | One `./install.sh` on a new device sets up SSH, the admin web, the touch UI and eSIM, and turns off automatic firmware updates |

The proxy is not part of the full install. It ships with no subscription or personal rules: you add your own subscription URL in `chill.env` on the device.

## Requirements

| | |
|---|---|
| Device | ZTE U60 Pro (MU5250) |
| Firmware for a fresh install | **B31 or earlier**; B28 – B31 need SSH opened first (see the note at the top) |
| Build machine | Docker (macOS or Linux); the kit builds everything inside containers, no local toolchain needed |
| Install | A computer on the device's Wi-Fi, a USB-C data cable and the router admin password |
| Binaries | None published yet: you build them yourself, the guide has every step |

## Quick start

**Starting from scratch? Read [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md)**: what you need, how to build the install kit, how to install, check and roll back.

```sh
./onboard/build-kit.sh                 # → onboard/dist/luyaz-kit-YYYYMMDD.tar.gz
# unpack the kit; connect the computer to the device's Wi-Fi, then:
./install.sh                           # first install; plug in USB-C when it asks
./install.sh doctor                    # read-only health check
```

The device is expected at `192.168.0.1`; if yours uses another address, run `GATEWAY=192.168.x.1 ./install.sh`. When it is done, open `http://192.168.0.1:9090` in a browser on the device's network.

## What it changes, and how to undo it

- Programs and data go to `/data`, which firmware upgrades leave alone. Autostart is a few lines added to `/etc/rc.local`; the original is saved first.
- No stock service is disabled or modified, and the stock screen interface stays on the device as a fallback.
- The installer turns **off** ZTE's automatic firmware updates, so an update cannot wipe the install.
- Every component can be rolled back on its own ([DEPLOY.md](DEPLOY.md)), and [onboard/README.md](onboard/README.md#restore-stock) has the steps to remove everything and return to stock.

## Architecture

```
luyaz-core :9460 ──▶ luyaz-ui (touch screen)
       ▲  └──▶ lpac (/data/esim) ──▶ eUICC card
browser ──▶ luyaz-web :9090 (admin web) ──▶ luyaz-core
luyaz-sentinel: screen owner + watchdog
```

- **luyaz-core**: the only program that talks to the modem; turns ubus, AT commands and sysfs into one local API.
- **luyaz-web**: the admin web, LAN only.
- **luyaz-ui**: the touch UI (Rust + LVGL) on the front panel.
- **luyaz-sentinel**: keeps the touch UI running and hands the screen back to the stock interface if it cannot.

Paths on the device, startup and supervision: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Development

Everything builds in Docker. The simplest path is the whole install kit:

```sh
./onboard/build-kit.sh                 # → onboard/dist/luyaz-kit-YYYYMMDD.tar.gz
```

<details>
<summary>Building single components</summary>

```sh
# Core and admin web (aarch64 musl, needs cargo-zigbuild if built outside Docker)
cargo zigbuild --profile release-device --target aarch64-unknown-linux-musl -p luyaz-core
cargo zigbuild --release --target aarch64-unknown-linux-musl -p luyaz-web

# Touch UI (Docker)
(cd touch-ui && rs/setup-src.sh && rs/build.sh device)

# Admin web browser checks (mock core in a container)
(cd crates/luyaz-web && npm ci && e2e/run.sh)
```

</details>

To update a single component on a device that is already set up, see [DEPLOY.md](DEPLOY.md).

<details>
<summary>Repository layout</summary>

```
crates/        luyaz-core (core), luyaz-web (admin web), luyaz-sentinel (screen owner + watchdog), luyaz-* libraries
touch-ui/      touch UI: rs/ (Rust + LVGL), scripts/ (device scripts), docs/
onboard/       install kit: build-kit.sh (build), install.sh (install), device/ (on-device scripts), test/ (sandbox tests)
scripts/       esim/ (lpac toolkit), tailscale/ (boot start for Tailscale, see its README)
docs/          GETTING-STARTED.md, SCREENSHOTS.md, ARCHITECTURE.md, DESIGN.md (UI design spec), RELIABILITY.md (reliability contract)
```

</details>

## Versions and updates

The five parts — core, admin web, sentinel, touch UI and device scripts — each have their own version ([SemVer](https://semver.org)); an update bumps only the part that changed.

- Current versions: [`versions.json`](versions.json). What changed in each: [CHANGELOG](CHANGELOG.md), and [Releases](https://github.com/faying/luya-z-manager/releases) (tags like `core-v1.1.0`).
- On the device: admin web **Device info → Component versions**, or the touch screen **System → Component versions**. A newer version shows as `1.0.0 → 1.1.0`.
- The device looks at `versions.json` here once a day (not while roaming). It only shows what is new; it never installs anything by itself.

## Project status

Maintained by one person and tested on one device (CN firmware). It is used daily, but expect rough edges on other firmware variants and regions. Bug reports with `./install.sh doctor` output are welcome.

**There is no official seller.** Luya-Z is free. If you paid for a device with Luya-Z installed, or for an install service, the price and any support are between you and the seller; Luya-Z has nothing to do with the sale. Devices bought that way get the same help as anyone else: open an issue with `./install.sh doctor` output.

## Documentation

Each doc links to its Chinese copy (`*.zh-CN.md`).

| Doc | For |
|---|---|
| [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md) | Quick start (begin here) |
| [docs/SCREENSHOTS.md](docs/SCREENSHOTS.md) | Feature tour: every page of the admin web and the touch UI |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | How the programs fit together and where they live on the device |
| [onboard/README.md](onboard/README.md) | The install kit: what goes where, daily use, restoring stock, FAQ |
| [DEPLOY.md](DEPLOY.md) | Updating a device that is already set up |
| [docs/RELIABILITY.md](docs/RELIABILITY.md) | File contract between the core, the sentinel and the scripts |
| [docs/DESIGN.md](docs/DESIGN.md) | Design spec for the admin web and the touch UI |
| [touch-ui/README.md](touch-ui/README.md), [touch-ui/docs/](touch-ui/docs/) | Touch UI build, hardware, eSIM, speed test |
| [CLAUDE.md](CLAUDE.md) | Device rules for AI coding assistants (humans should read the "don'ts" too) |

Contributions are welcome: see [CONTRIBUTING.md](CONTRIBUTING.md) and [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Security issues: [SECURITY.md](SECURITY.md).

## Credits

- [faying](https://github.com/faying): core (luyaz-core), admin web (luyaz-web), sentinel, Rust touch UI, install kit, eSIM, Tailscale.

This project learned from and grew out of these open-source projects; thanks to their authors for the ideas and groundwork:

- [Jesther Silvestre](https://github.com/jesther-ai): this project started from [open-u60-pro](https://github.com/jesther-ai/open-u60-pro).
- [33333s](https://github.com/33333s): [zwrt-datad](https://github.com/33333s/zwrt-datad) shaped the core's device layer, [u60pro-devui](https://github.com/33333s/u60pro-devui) the touch UI.
- Wei REN: the earlier fork this work continued from (install kit, eSIM probe).

The code itself was rewritten in October 2026; see [NOTICE](NOTICE).

## License and disclaimer

Source available under the [PolyForm Noncommercial License 1.0.0](LICENSE): free for personal and other noncommercial use. Selling it, preinstalling it on devices you sell, or using it in a business needs a separate licence; ask via a GitHub issue. Versions and Releases published before 2026-10-21 stay under the MIT License they were released with. Third-party components are listed in [NOTICE](NOTICE).

Not affiliated with or endorsed by ZTE Corporation. Use only on your own device. Reverse engineering was done solely for interoperability and learning; the repo contains no ZTE proprietary source code.
