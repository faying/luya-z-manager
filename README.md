<div align="center">

# ZTE U60 Pro (MU5250) Manager

**On-device REST agent, admin web UI, transparent-proxy (CHILL) control and install kit for the ZTE U60 Pro (MU5250) 5G mobile router.**

[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/device-ZTE%20U60%20Pro%20(MU5250)-orange.svg)]()

</div>

> **Community project, not affiliated with ZTE.** Based on
> [jesther-ai/open-u60-pro](https://github.com/jesther-ai/open-u60-pro) (MIT) plus later changes
> by Wei REN (install kit, eSIM, Tailscale, CHILL proxy, admin web redesign). This repository
> starts from a cleaned snapshot instead of the original git history; see [NOTICE](NOTICE) for
> sources and what was removed. The front-panel touchscreen UI lives in the companion repository
> [zte-u60-pro-mu5250-touch-ui](https://github.com/faying/zte-u60-pro-mu5250-touch-ui).

## Device

| | |
|---|---|
| **Model** | ZTE U60 Pro (MU5250), `MU5250_HW1.0` |
| **Chipset** | Qualcomm Snapdragon X75 (SDX75), 4x Cortex-A55 @ 2.2 GHz, 1.6 GB RAM |
| **Modem** | 5G-A Sub-6 + mmWave, Cat 22 LTE |
| **WiFi** | WiFi 7 (802.11be), 2x2 MIMO, EHT160 (Qualcomm WCN7851) |
| **Display** | 3.5" IPS LCD, 320x480, DRM/KMS |
| **OS** | ZWRT (OpenWrt 23.05.4), Linux 5.15, aarch64, read-only rootfs + writable `/data` |
| **SIM** | Single nano-SIM; no embedded eSIM, but removable eUICC cards work via lpac |

Full hardware inventory (bands, PMICs, I2C addresses, battery/charging) is in the touchscreen
repo's [docs/HARDWARE.md](https://github.com/faying/zte-u60-pro-mu5250-touch-ui/blob/main/docs/HARDWARE.md).

## What's Included

### `zte-agent` — on-device REST API

A single Rust binary (~2.3 MB, port 9090, LAN-only) that turns ubus calls, AT commands, and sysfs
reads into a typed REST API: device/battery/thermal, network and signal, modem and cell/band
locking, SIM/SMS, router settings (DNS/DHCP/firewall/NAT/QoS/APN), WiFi, USB mode, speed test,
scheduler, and **CHILL** — control for a native `mihomo` transparent proxy (TUN mode, no
ShellCrash). See [zte-agent/src/](zte-agent/src/) for the full endpoint list, one module per area.

```sh
cargo build --release --target aarch64-unknown-linux-musl -p zte-agent
```

### `web/` — admin UI

Next.js app served by the agent at the device root (`http://<device>:9090/`), replacing the stock
ZTE management page. Mobile-first (bottom tab bar on phones), design system in
[docs/DESIGN.md](docs/DESIGN.md).

### Reliability — supervision, Wi-Fi safety net, alerts

The device is often its owner's only uplink, so the agent, the data service and the touch UI run
under procd with crash capture, and a small watchdog (`u60-guard`) forces Wi-Fi back on if the
agent stops heartbeating while the access points are down. Failures show up as an alert banner and
a **Health** page in the web UI, on the touch screen, and — optionally — as a rate-limited SMS from
the device's own SIM. The install kit adds `./install.sh doctor` (read-only check) and
`./install.sh backup` / `restore` (configuration only, stored on your computer). The file-level
contract between the agent and the shell side is in [docs/RELIABILITY.md](docs/RELIABILITY.md); the
scripts themselves live in the
[touch-ui repo](https://github.com/faying/zte-u60-pro-mu5250-touch-ui) under `scripts/`.

Note: ZTE's firmware keeps the system clock on *local* wall time with the time zone set to UTC.
The agent reports the gap as `clock.utc_offset` in `/api/public/status`; the web UI shows device
times as device-local time (`web/src/lib/deviceClock.ts`).

### `mobile/` — native companion apps

SwiftUI (iOS 16+) and Jetpack Compose (Android 8+) apps that talk to `zte-agent` directly over
WiFi. See [mobile/README.md](mobile/README.md).

## Why Use This Instead of the Official ZTE App?

The stock firmware runs ~44 proprietary daemons (TR-069 remote management, MQTT telemetry, Samba,
NFC, diagnostics — several phoning home to ZTE servers) for ~225 MB RAM. `zte-agent` replaces the
management surface with one binary using well under 1 MB RSS, and ships an open-source,
non-Chinese-only mobile app.

## Quick Start

### Prerequisites

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add aarch64-unknown-linux-musl
brew install filosottile/musl-cross/musl-cross android-platform-tools   # macOS
# or: sudo apt install musl-tools gcc-aarch64-linux-gnu android-tools-adb
```

### First-time setup (factory-fresh device)

```sh
./setup.sh <router-password> <agent-password>
```

Enables ADB, builds and pushes the agent, creates boot scripts, optionally sets up key-only SSH.

For setting up **someone else's** device, use the prebuilt kit instead — see
[onboard/README.md](onboard/README.md) (Chinese) and `./onboard/build-kit.sh`.

### All-in-one installer

```sh
GATEWAY=192.168.0.1 ./install.sh
```

Interactive menu over SSH (falls back to password + `sshpass` if you have no key yet). All boot
persistence goes through `/etc/rc.local` and cron, never `init.d` — see CLAUDE.md for why.

```
╔══ Open U60 Pro installer ══╗
  1) Agent + Admin Web + SSH
  2) Tailscale
  3) Home Mode           (Wi-Fi auto-off at home)
  4) System monitor       (temp/signal log + forensics)
  5) Crash recovery (SSR)
  6) Status
  7) Uninstall a component
```

CHILL (the transparent-proxy panel) installs separately — `scripts/chill/install-chill.sh` — since
it needs your own outbound proxy subscription.

### Deploy agent + web (subsequent updates)

```sh
./scripts/deploy.sh all      # or: web / agent / verify
```

See [DEPLOY.md](DEPLOY.md) for what each target does and manual fallback commands.

### Connect the mobile app

Connect to the router's WiFi, open the app, set the agent URL to `http://192.168.0.1:9090` and
enter the agent password from setup.

## Project Structure

```
zte-agent/        On-device REST API server (Rust)
web/               Next.js admin UI, served by the agent
mobile/            iOS (SwiftUI) + Android (Jetpack Compose) companion apps
install.sh         Interactive all-in-one installer
setup.sh           First-time agent + UI + SSH setup
scripts/
├── deploy.sh       Rebuild & redeploy agent/web
├── chill/          CHILL (mihomo) install + init script
├── esim/           lpac bundle for removable eUICC cards
├── homemode.sh      Wi-Fi auto-off near a home network
├── monitor.sh       Temp/signal logger + crash forensics
├── recovery_setup.sh   Per-subsystem (SSR) recovery
└── tailscale-start.sh  Tailscale boot bring-up
onboard/           Prebuilt install kit for other people's devices — see its own README
docs/DESIGN.md     Admin web design system
```

## Credits

- [Jesther Silvestre](https://github.com/jesther-ai) — original [open-u60-pro](https://github.com/jesther-ai/open-u60-pro): agent, mobile apps, first web dashboard.
- Wei REN — install kit, eSIM, Tailscale, CHILL proxy, Home Mode, admin web redesign.
- Touchscreen UI: [zte-u60-pro-mu5250-touch-ui](https://github.com/faying/zte-u60-pro-mu5250-touch-ui) (fork of [33333s/u60pro-devui](https://github.com/33333s/u60pro-devui)); its data backend is [33333s/zwrt-datad](https://github.com/33333s/zwrt-datad).

## License & Disclaimers

[MIT License](LICENSE). See [NOTICE](NOTICE) for attribution and third-party components.

**Use at your own risk.** Not affiliated with ZTE Corporation. Intended for devices you personally
own. Reverse engineering was performed solely for interoperability and educational purposes; no
proprietary ZTE source code is included.
