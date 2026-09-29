# Proxy (do it yourself)

**English** · [中文](PROXY.zh-CN.md)

This repository and the install kit **do not include** any proxy functionality: no proxy core, no panel, no rule sets, and no proxy pages in the admin web or touch UI.
To run a transparent proxy on the U60 Pro (MU5250), set it up yourself directly from the official sources:

- Core: mihomo (Clash.Meta)
  - Source and release packages: <https://github.com/MetaCubeX/mihomo> (pick `linux-arm64` under releases)
  - Documentation: <https://wiki.metacubex.one/>
- Panel (pick any official-style web panel that talks to mihomo's `external-controller`):
  - metacubexd: <https://github.com/MetaCubeX/metacubexd>
  - zashboard: <https://github.com/Zephyruso/zashboard>

Rules to follow when setting it up on this device (same as section 8 of [GETTING-STARTED.md](GETTING-STARTED.md)):

- The device runs OpenWrt 23.05 on aarch64 with musl libc; put programs and configuration in `/data`; `/tmp` is a RAM disk.
- Autostart goes only through `/etc/rc.local`; back it up before changing it and check it with `sh -n` afterwards; **do not** run `/etc/init.d/<stock service> disable`.
- A transparent proxy takes over the whole home network. Before the first start, have a way back (be able to stop it over SSH and restore DNS / firewall to their original state),
  and have the control interface (`external-controller`) listen only on localhost or the LAN, never exposed to the cellular network.
- Keep subscription URLs and secrets only on the device, readable only by root, and never commit them to any repository.
