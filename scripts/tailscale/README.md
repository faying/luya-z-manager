# Tailscale on the U60 Pro (MU5250)

English · [中文](README.zh-CN.md)

`start.sh` starts `tailscaled` at boot from `/etc/rc.local` and brings the node up. `apply.sh` switches tuning (`tuning.env`) and puts the previous one back if Tailscale does not come back healthy.

| File on the device | What it is |
|---|---|
| `/data/tailscale/tailscaled`, `/data/tailscale/tailscale` | the official static arm64 build (`tailscale_<version>_arm64.tgz` from pkgs.tailscale.com) |
| `/data/tailscale/start.sh` | this directory's `start.sh` |
| `/data/tailscale/apply.sh` | this directory's `apply.sh` (optional) |
| `/data/tailscale/tuning.env` | your settings (optional, see below) |
| `/data/tailscale/state/` | the node's identity: back it up, never share it |
| `/data/tailscale/boot.log` | what each start decided (caps itself at 64 KB) |
| `/data/tailscaled.log` | tailscaled's own log (appended; rotate it yourself) |

## Install

The device has no `scp`; copy files with `ssh … 'cat > file' < file`. Replace `<device>` with your router's address. The examples assume SSH to the device works.

```sh
# 1. binaries (any recent stable version)
curl -fLO https://pkgs.tailscale.com/stable/tailscale_1.102.4_arm64.tgz
tar xzf tailscale_1.102.4_arm64.tgz
ssh root@<device> 'mkdir -p /data/tailscale'
for f in tailscaled tailscale; do
  ssh root@<device> "cat > /data/tailscale/$f && chmod 755 /data/tailscale/$f" < tailscale_1.102.4_arm64/$f
done

# 2. scripts
ssh root@<device> 'cat > /data/tailscale/start.sh' < scripts/tailscale/start.sh
ssh root@<device> 'cat > /data/tailscale/apply.sh && chmod 755 /data/tailscale/apply.sh' < scripts/tailscale/apply.sh

# 3. check before enabling anything at boot (starts nothing, changes no network state)
ssh root@<device> 'sh /data/tailscale/start.sh check'

# 4. start it and log in once (open the printed URL; or use --auth-key=tskey-…)
ssh root@<device> 'sh /data/tailscale/start.sh; sleep 10; /data/tailscale/tailscale --socket=/tmp/tailscaled.sock login'
```

When `check` passes and the node shows up in your tailnet, start it at boot. Back up `/etc/rc.local` first, add this line **before** `exit 0`, then check the syntax:

```sh
ssh root@<device> 'cp /etc/rc.local /data/rc.local.bak && sed -i "/^exit 0/i sh /data/tailscale/start.sh" /etc/rc.local && sh -n /etc/rc.local && grep -n tailscale /etc/rc.local'
```

Do not use `/etc/init.d/… enable` for this. The vendor boot barrier only lets boot finish once its own daemons have registered, and anything else added there can hang the boot at the logo.

No auth key is ever stored on the device. Once logged in, the identity in `state/` is enough.

## Settings: `/data/tailscale/tuning.env`

All settings are optional. Each one is a shell assignment, one per line. The file is checked with `sh -n` and a trial load first; if either fails, it is ignored and the defaults are used (see `boot.log`).

| Key | Default | Meaning |
|---|---|---|
| `TS_HOSTNAME` | `u60pro` | node name |
| `TS_ROUTES` | the `br-lan` /24 | subnet to advertise, e.g. `192.168.0.0/24`; your LAN becomes reachable from the tailnet |
| `TS_ACCEPT_ROUTES` | `true` | use subnet routes other nodes advertise |
| `TS_ACCEPT_DNS` | `false` | let Tailscale manage DNS |
| `TS_EXIT_NODE` | none | send internet traffic through this node (IP or name); LAN access stays allowed |
| `TS_MODE` | `tun` | `tun` = kernel networking (`tailscale0`); `userspace` = no kernel routes or firewall rules |
| `TS_TAILSCALED_FLAGS` | none | extra tailscaled flags, e.g. `--no-logs-no-support` |
| `TS_TAILSCALED_ENV` | none | extra environment, e.g. `TS_DISABLE_PORTMAPPER=1 GOMEMLIMIT=192MiB` |
| `TS_TAILSCALED_BIN` | `/data/tailscale/tailscaled` | another tailscaled build (the file must be named `tailscaled`); if it is missing, the default is used |
| `TS_WAN_WAIT` / `TS_STABLE` / `TS_MAX_STRIKES` / `TS_SAFE_RETRY` | 180 / 600 / 3 / 3600 | see "Boot safety" |

To change settings while staying reachable, use `apply.sh`. It restarts Tailscale, waits up to 5 minutes, and puts the old file back if Tailscale does not come back healthy:

```sh
sh /data/tailscale/apply.sh /data/tailscale/my-variant.env relay
```

## Commands

```sh
sh /data/tailscale/start.sh            # start (what rc.local runs)
sh /data/tailscale/start.sh status     # mode, safe mode, boot strikes, disabled
sh /data/tailscale/start.sh check      # preflight
sh /data/tailscale/start.sh stop       # stop, and remove its routes and firewall rules
sh /data/tailscale/start.sh disable    # stop, and do not start at boot (rc.local unchanged)
sh /data/tailscale/start.sh enable     # undo disable and start
sh /data/tailscale/start.sh clear-safe # leave safe mode now
```

## Boot safety

- **rc.local is not held up.** rc.local runs inside the boot sequence, and later boot steps wait for it, including the one that marks a freshly installed firmware slot as good. The part rc.local waits for only checks a few files and returns. Waiting for the network, starting tailscaled and `tailscale up` run in the background, and each step has a time limit.
- **Off switch.** If `/data/tailscale/disabled` exists, nothing is started (`disable` / `enable` manage it). The kill switch never needs an rc.local edit.
- **Unsafe kernel networking → userspace mode.** In `tun` mode, tailscaled drops packets from `100.64.0.0/10` arriving on any other interface. Some carriers use that range for the WAN address, gateway or DNS, which would cut off the router's own DNS. If any of them is in that range, or there is no TUN device, this boot uses userspace mode. Userspace mode adds no routes or firewall rules. The tailnet can still reach the router's own services that listen on all addresses (SSH, admin web), but LAN devices cannot reach the tailnet through the router.
- **Boot strikes → safe mode.** If 3 boots in a row start in `tun` mode and the device does not stay up 10 minutes, safe mode (`/data/tailscale/safe-mode`) switches to userspace mode. After 1 hour up in safe mode, the next boot tries `tun` once more. Reboots for other reasons (a modem crash, for example) also count, which is why this only degrades and never switches Tailscale off. Restarts by `apply.sh` in the same boot do not count.
- **Bad settings don't stop it.** A broken `tuning.env` is ignored. If the `TS_TAILSCALED_BIN` build is missing, it falls back to the default binary.
- **No restart loop.** Nothing respawns tailscaled. If it exits, it stays down until the next start or reboot.

## Moving over from `scripts/tailscale-start.sh`

The old script is deprecated (it now only prints a notice). To switch:

1. Remove its rc.local line: `sed -i '\#tailscale-start.sh#d' /etc/rc.local && sh -n /etc/rc.local`.
2. Install `start.sh` as above and add its rc.local line.
3. Settings: copy `TS_ROUTES`, `TS_HOSTNAME`, `TS_ACCEPT_ROUTES`, `TS_ACCEPT_DNS` and `TS_EXIT_NODE` from `/data/tailscale/tsconfig` into `tuning.env`. **Leave out `TS_AUTHKEY`**, then delete `tsconfig`, because it holds your auth key.
4. Identity: if yours is in `/data/tailscale/tailscaled.state`, or in a file named `/data/tailscale/state`, `start.sh` moves it to `/data/tailscale/state/tailscaled.state` the first time it starts. `check` tells you when this will happen.
5. The old script did things this one deliberately does not do:
   - set the clock (the firmware's own time service does that);
   - add MSS-clamp, FORWARD and SNAT iptables rules (the stock firewall already has a `tailscale` zone, and tailscaled masquerades subnet-route traffic itself);
   - mwan3 policy-routing workarounds;
   - run a second dropbear on the tailnet address (SSH listening on all addresses is reachable over the tailnet);
   - start ShellCrash.

   If you need any of these, run it from its own script, not from this one.

## Uninstall

```sh
sh /data/tailscale/start.sh stop
sed -i '\#/data/tailscale/start.sh#d' /etc/rc.local && sh -n /etc/rc.local
# optional: log the node out first with /data/tailscale/tailscale --socket=/tmp/tailscaled.sock logout
rm -rf /data/tailscale /data/tailscaled.log*
```

## Tests

```sh
docker run --rm -v "$PWD":/w busybox sh /w/scripts/tailscale/test/start.sh   # start.sh
docker run --rm -v "$PWD":/w busybox sh /w/scripts/tailscale/test/run.sh     # apply.sh
```
