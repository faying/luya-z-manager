# Architecture

**English** · [中文](ARCHITECTURE.zh-CN.md)

How the programs on the device fit together, and where each one lives. For the file contract between them (locks, state files, FIFOs), see [RELIABILITY.md](RELIABILITY.md).

## Overview

```
luyaz-core :9460 ──▶ luyaz-ui (touch screen)
       ▲  └──▶ lpac (/data/esim) ──▶ eUICC card
browser ──▶ luyaz-web :9090 (admin web) ──▶ luyaz-core
luyaz-sentinel: screen owner + watchdog
```

`luyaz-core` is the only program that talks to the modem and the stock services. It turns ubus calls, AT commands and sysfs into one local API. The admin web and the touch UI are both clients of that API; neither touches the hardware directly.

## Components

| Source | On the device | What it does |
|---|---|---|
| `crates/luyaz-core` | `/data/luyaz/luyaz-core`, `/etc/init.d/luyaz-core`, state in `/data/luyaz/core/` | The core. API on `127.0.0.1:9460`: `/v2/state`, `/v2/events` (SSE), `/v2/screen`, plus the control API. `luyaz-core doctor` is the health check |
| `crates/luyaz-web` | `/data/luyaz/luyaz-web`, `/etc/init.d/luyaz-web`, password file `/data/luyaz/web.env` | Admin web on `:9090`, LAN only. Server-rendered Rust with Datastar |
| `crates/luyaz-sentinel` | `/data/luyaz/luyaz-sentinel`, `/etc/init.d/luyaz-sentinel`, control FIFO `/tmp/luyaz-screen.ctl` | Owns the screen: starts the touch UI, restarts it if it crashes, hands the screen back to the stock interface after two failed starts. Also the Wi-Fi fallback watchdog |
| `touch-ui/rs/` | `/data/luyaz/ui/luyaz-ui` (fonts and logos in the same directory) | Touch UI, Rust + LVGL, draws straight to DRM/KMS |
| `crates/luyaz-*` (others) | linked into the core | Libraries: policy, scenarios, Wi-Fi, alerts, eSIM, SMS forwarding, speed test |
| `scripts/esim/` | `/data/esim/` | lpac toolkit for removable eUICC cards |
| `onboard/` | not installed | The install kit that puts all of the above on a device |

## Startup and supervision

- Everything starts from a few lines in `/etc/rc.local`, one `/etc/init.d/<name> start` per service. No stock service is disabled or changed: the stock boot daemon waits for its whole service list, and removing one leaves the device stuck on the logo.
- procd restarts the core, the admin web and the sentinel if they exit. The sentinel in turn restarts the touch UI.
- The screen must never stay empty: if no interface is on it for a few minutes, the firmware reboots the whole device. This is why the sentinel falls back to the stock interface instead of retrying forever.

## Where things live

- Programs and persistent data: `/data` (survives firmware upgrades).
- Logs, locks and runtime state: `/tmp` and `/var/run` (RAM, cleared on reboot). Core log `/tmp/luyaz-core.log`, sentinel log `/tmp/luyaz-sentinel.log`, UI log `/tmp/luyaz-ui.log`.
- Install backup of the stock `rc.local`: `/data/luyaz/kit/rc.local.orig` (`/data/u60-kit/` on devices installed by older kits).

See [onboard/README.md](../onboard/README.md) for the full install layout and how to restore stock.
