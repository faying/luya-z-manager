# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

- **Primary: the device owner.** Technical, comfortable with dense data and radio terms (band, ARFCN, PCI, RSRP/SINR, APN, QCI). Uses the admin mostly on a phone.
- **Secondary: other ZTE U60 Pro (MU5250) owners** who installed the open-source install kit. They should understand a page on first open and not change something risky by accident.

## Product Purpose

The admin web for a ZTE U60 Pro (MU5250) 5G pocket router running this project's on-device agent. It replaces the stock ZTE management page with one that shows what the router is doing and exposes controls the stock UI hides.

The same device also has a front touchscreen UI from this project. The web and the touchscreen are **one product on two screens**. They share the same status meanings, words and structure (Home / Charts / Functions / System). The web adds the depth a 320×480 screen cannot hold.

Success: the owner opens it on a phone, sees within a glance whether signal, throughput, CHILL and the current scenario are fine, and switches a CHILL node or exit without hunting.

## Operating Context

- Served by the on-device agent at `http://<device>:9090/` as a static export. No internet is required to load it; fonts and assets are bundled at build time.
- Reached from the LAN, and remotely over Tailscale. Remote use means a slow or high-latency link and no one at the device to recover from a mistake.
- Most frequent jobs, in order:
  1. switch the CHILL node or exit;
  2. glance at status on a phone;
  3. manage remotely over Tailscale (eSIM, SMS, "is it still online").
- Devices: iPhone and Android phones first, iPad (portrait and landscape, touch and keyboard), then desktop browsers.
- Login: one admin password; the session token expires after an hour.

## Capabilities and Constraints

- **43 pages today**, each backed by the agent's REST API, including:
  - dashboard, signal, clients, device info, alerts, health;
  - mobile network, network mode, APN, SIM/PIN, eSIM, QCI;
  - band lock, cell lock, signal detect, STC;
  - Wi-Fi, guest Wi-Fi, scenarios, LAN/DHCP, DNS/DoH, firewall, QoS, VPN passthrough, telemetry block;
  - SMS, SMS forwarding, STK/USSD;
  - Tailscale, CHILL (on-device mihomo proxy: exit, regions, profiles, subscriptions, device bypass, dashboard);
  - AT terminal, speed test, processes, enable ADB;
  - device control, scheduled reboot, scheduler, USB mode, config tool, settings.
- **No existing control or data item may be removed.** Structure and placement can change. A redesign keeps an inventory of every control and field and checks it before and after.
- **Controls that change device state must be hard to trigger by accident.** This covers reboot, factory reset, eSIM switch, APN, network mode, band/cell lock, turning CHILL off, and deleting SMS. The device may be someone's only uplink.
- **ZTE firmware auto-update (FOTA) is never offered as a control.**
- Device clock is local time labelled UTC. Times are shown as device-local (`deviceClock.ts`).
- Stale or unreachable data must stay visible, marked with when it was last fresh. It must not be blanked or shown as "no signal".
- Stack: Next.js (static export) + React 19 + Tailwind CSS 4 + react-i18next, calling the agent through `lib/api` (`useApi`, `apiFetch`). HeroUI v3 is the owner's preferred component library for the redesign.
- The public GitHub repository may not contain binaries (fonts, images): fonts come from `next/font` at build time.

## Brand Commitments

- Device name is always written with both **U60 Pro** and **MU5250**.
- The proxy feature is named **CHILL** everywhere; the name RELAX must never appear.
- The touchscreen's "new design" is the visual source of truth the web follows. The owner gave the Cohere and Figma DESIGN.md files (getdesign.md) as references for making the web more expressive. They are references for technique, not brands to copy.
- The redesign is called "新设计 / new design"; do not use the name of the reference OS style in code or docs.

## Evidence on Hand

- Touchscreen design system and implementation:
  - `../docs/DESIGN.md` §4;
  - `touch-ui/include/ui_theme.h` (light/dark palettes, contrast-checked) and `ui_kit.h` (component sizes);
  - touch render-test screenshots.
- The touch UI's own design notes and the Cohere / Figma reference DESIGN.md files are kept outside this repository (maintainer's working notes).
- The live agent API and its data shapes (`lib/api`, `zte-agent/src`).
- No user research, analytics or testimonials exist; do not invent any.

## Product Principles

1. **Status and its reason come first**, then what can be changed, then detail (traffic, connections, logs).
2. **Same meaning on both screens.** Normal / warning / stopped / neutral look and read the same as on the touchscreen, never color alone.
3. **Nothing is taken away.** Depth moves behind navigation, search and links; it is not deleted.
4. **Safe at a distance.** Every risky action says what it will do and needs a second, deliberate step, because the owner may be on the other side of a Tailscale link.
5. **Honest data.** Show freshness, and say why something is missing instead of drawing an empty shell.

## Accessibility & Inclusion

- WCAG 2.2 AA: text contrast ≥ 4.5:1 in both light and dark, visible focus, full keyboard use (including iPad with a keyboard), labelled controls, touch targets ≥ 44 px.
- Chinese is the primary language; English stays available and layouts must fit its longer strings.
- Respect `prefers-reduced-motion` and the OS light/dark preference.
