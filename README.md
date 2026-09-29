# ZTE U60 Pro (MU5250) Manager

**English** · [中文](README.zh-CN.md)

An on-device REST API (`zte-agent`), a browser admin UI and a **one-command install kit** for the ZTE U60 Pro (MU5250) 5G portable hotspot.

> Community project. Not affiliated with ZTE. Use at your own risk.

| Admin web | Touch UI (companion repo) |
|---|---|
| <img src="docs/images/web-home-desktop.png" width="560" alt="Admin web home page"> | <img src="docs/images/touch-home.png" width="200" alt="Touch UI home screen"> |

## Part of a three-repo set

| Repo | Role on the device |
|---|---|
| **[manager](https://github.com/faying/zte-u60-pro-mu5250-manager)** (this repo) | `zte-agent` (:9090), admin web, install kit |
| [touch-ui](https://github.com/faying/zte-u60-pro-mu5250-touch-ui) | Front-panel touch UI, screen owner daemon, supervision and Wi-Fi fallback scripts |
| [data-service](https://github.com/faying/zte-u60-pro-mu5250-data-service) | `zwrt-datad`: local data service (`/state` + SSE on `127.0.0.1:9460`) |

```
zwrt-datad :9460 ──▶ touch UI ──(eSIM page)──▶ zte-agent :9090 ──▶ lpac ──▶ eUICC card
browser ──▶ zte-agent :9090 (API + admin web)
```

The install kit is built from `onboard/` in this repo; it pulls the touch UI and data service from the other two repos at build time.

## Features

- **zte-agent**: a Rust daemon (port 9090, LAN only) that turns ubus, AT commands and sysfs into a REST API:
  device, battery and temperature; signal and carriers; band and cell locking; SIM and SMS; APN; DNS, DHCP and firewall; Wi-Fi; USB mode; speed test; scheduled tasks; and more.
- **Admin web** (`web/`, Next.js static export): replaces the stock web UI. Works on phone and desktop, light and dark, Chinese and English.
- **eSIM**: download, switch and delete profiles on a removable eUICC card (5ber, eSTK.me and similar).
- **Proxy**: not included. If you want a transparent proxy on the device, [docs/PROXY.md](docs/PROXY.md) explains how to build and configure one yourself from official sources.
- **Reliability**: zte-agent, the data service and the watchdog are supervised by procd; a Wi-Fi fallback watchdog; alert banners, a Health page and optional alert SMS;
  `./install.sh doctor` for a read-only health check, `backup` / `restore` for configuration. The file contract is in [docs/RELIABILITY.md](docs/RELIABILITY.md) (Chinese).
- **Install kit** (`onboard/`): one `./install.sh` on a new device sets up SSH, the admin web, the touch UI and eSIM, and turns off automatic firmware updates.

## Quick start

**Starting from scratch? Read [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md)**: what you need, how to build the install kit, how to install, check and roll back.

Only firmware **B27 and earlier** is supported (from B28 ZTE removed the interface used to enable ADB). **Do not update the firmware before installing.**
No prebuilt binaries are published yet; you build them yourself, and the guide has every step.

## Development and build

```sh
# zte-agent (aarch64 musl; onboard/build-kit.sh falls back to Docker when cargo-zigbuild is missing)
cargo zigbuild --release --target aarch64-unknown-linux-musl -p zte-agent

# Admin web → web/out/
cd web && npm ci && npm run build

# Web development without a device: mock agent + dev server, see web/README.md
node scripts/mock-agent/server.ts & npm run dev

# Install kit → onboard/dist/u60-kit-YYYYMMDD.tar.gz
./onboard/build-kit.sh
```

To update a single component on a device that is already set up, see [DEPLOY.md](DEPLOY.md).

## Layout

```
zte-agent/     on-device REST API (Rust)
web/           admin web (Next.js), served by the agent on :9090
onboard/       install kit: build-kit.sh (build), install.sh (install), device/ (on-device scripts), test/ (sandbox tests)
scripts/       esim/ (lpac toolkit), tailscale/, homemode.sh, monitor.sh, etc.
docs/          GETTING-STARTED.md, DESIGN.md (UI design spec), RELIABILITY.md (reliability contract)
```

The top-level `setup.sh`, `install.sh` and `deploy.sh` come from upstream open-u60-pro and use password SSH and the old startup method; for new devices use the install kit in `onboard/`.

## Documentation

Each doc links to its Chinese copy (`*.zh-CN.md`). RELIABILITY.md and DESIGN.md are Chinese only for now.

- [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md): quick start (begin here)
- [onboard/README.md](onboard/README.md): the install kit (what goes where, daily use, restoring stock, FAQ)
- [DEPLOY.md](DEPLOY.md): updating a device that is already set up
- [docs/RELIABILITY.md](docs/RELIABILITY.md): file contract between zte-agent and the watchdog (Chinese)
- [docs/DESIGN.md](docs/DESIGN.md): design spec for the admin web and the touch UI (Chinese)
- [web/README.md](web/README.md): web development
- [CLAUDE.md](CLAUDE.md): device rules for AI coding assistants (humans should read the "don'ts" too)

## Credits

- [Jesther Silvestre](https://github.com/jesther-ai): the original project [open-u60-pro](https://github.com/jesther-ai/open-u60-pro) (agent, first web UI).
- Wei REN: install kit, eSIM, Tailscale, home mode, web redesign.
- [33333s](https://github.com/33333s): thanks for the reference repos [u60pro-devui](https://github.com/33333s/u60pro-devui) (starting point of the touch UI) and [zwrt-datad](https://github.com/33333s/zwrt-datad) (local data service).

## License and disclaimer

[MIT](LICENSE). Sources, removed content and third-party components are listed in [NOTICE](NOTICE).

Not affiliated with or endorsed by ZTE Corporation. Use only on your own device. Reverse engineering was done solely for interoperability and learning; the repo contains no ZTE proprietary source code.
