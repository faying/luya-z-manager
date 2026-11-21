# Proxy (CHILL)

**English** · [中文](PROXY.zh-CN.md)

CHILL runs native Mihomo (Clash.Meta) as a transparent gateway. Manage configurations and modes under Services → CHILL in the admin web. Tap the Mihomo status on the touchscreen home page to open CHILL details. Install CHILL by name.

What ships is a generic setup, not anyone's personal one:

- `scripts/chill/template.yaml`: one subscription (`SUB_URL`), region groups (TW / JP / SG / US), and a small rule set:
  LAN and captive-portal checks direct, mainland China direct, AI, Telegram, YouTube / Netflix / Spotify, Apple, everything else through the proxy.
  Node URL profiles use this template. Rule sets come from [MetaCubeX/meta-rules-dat](https://github.com/MetaCubeX/meta-rules-dat).
- `scripts/chill/chill.env.example` documents advanced device settings and the legacy single-subscription setup.

## Install

From the install kit: `GATEWAY=<device IP> ./install.sh chill`. From a checkout: `scripts/chill/install-chill.sh` (dry run by default; `--apply` to do it).
Import a full configuration URL, a node subscription URL or a local YAML file in the admin web. You can keep several profiles and switch between them. A failed switch while CHILL is running restores the previous working profile. For the first start, run `sh /data/chill/chill.sh safe-start`. Check the network, then run `sh /data/chill/chill.sh confirm` within five minutes. If the timer expires, CHILL turns off and restores DNS.

Notes in the comments of `template.yaml` and `chill.sh` explain the device-specific choices (no `auto-redirect`, DNS through dnsmasq, CN routes).
The touch-screen side is described in [touch-ui/docs/CHILL.md](../touch-ui/docs/CHILL.md).

## Rules

- Autostart goes only through `/etc/rc.local` (the installer backs it up and checks it with `sh -n`); **never** `/etc/init.d/<stock service> disable`.
- A transparent proxy takes over the whole home network: always start with `safe-start`, and keep SSH access to stop it.
- The control interface (`:9999`) listens on localhost only by default; the panel goes through the admin web.
- Rule mode follows the profile. Global uses Mihomo's `GLOBAL` group. Direct keeps the gateway running with a direct exit. Turning CHILL off restores stock routing and DNS. Open zashboard for detailed node and connection controls.
- Full profiles keep their DNS policies and traffic rules. The device creates a gateway runtime copy and stores each original and its separate cache under `/data/chill/profiles/`. The device keeps subscription URLs and secrets private.
