# Proxy (CHILL)

**English** · [中文](PROXY.zh-CN.md)

CHILL is this project's transparent proxy: native mihomo (Clash.Meta) on the device, a panel behind the admin web
(`:9090/chill-ui/`), and an exit page on the touch screen. It is **not** in the full install; you install it by name.

What ships is a generic setup, not anyone's personal one:

- `scripts/chill/template.yaml`: one subscription (`SUB_URL`), region groups (TW / JP / SG / US), and a small rule set:
  LAN and captive-portal checks direct, mainland China direct, AI, Telegram, YouTube / Netflix / Spotify, Apple, everything else through the proxy.
  Add your own providers and rules there; rule sets come from [MetaCubeX/meta-rules-dat](https://github.com/MetaCubeX/meta-rules-dat).
- `scripts/chill/chill.env.example`: the device-side settings. Copy it to `/data/chill/chill.env` on the device, fill in `SUB_URL`, `chmod 600`.

## Install

From the install kit: `GATEWAY=<device IP> ./install.sh chill`. From a checkout: `scripts/chill/install-chill.sh` (dry run by default; `--apply` to do it).
Then on the device: `chill.sh safe-start`, check that the home network works, and `chill.sh confirm` within 5 minutes,
otherwise it stops by itself and restores DNS and the firewall.

Notes in the comments of `template.yaml` and `chill.sh` explain the device-specific choices (no `auto-redirect`, DNS through dnsmasq, CN routes).
The touch-screen side is described in [touch-ui/docs/CHILL.md](../touch-ui/docs/CHILL.md).

## Rules

- Autostart goes only through `/etc/rc.local` (the installer backs it up and checks it with `sh -n`); **never** `/etc/init.d/<stock service> disable`.
- A transparent proxy takes over the whole home network: always start with `safe-start`, and keep SSH access to stop it.
- The control interface (`:9999`) listens on localhost only by default; the panel goes through the admin web.
- Keep subscription URLs and secrets only in `chill.env` on the device; never commit them to any repository.
