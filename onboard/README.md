# Luya-Z install kit (ZTE U60 Pro / MU5250)

**English** · [中文](README.zh-CN.md)

One-step setup for a **freshly unboxed ZTE U60 Pro (MU5250)**:

| Component | What you get after install |
|---|---|
| **SSH** | `ssh -p 2222 root@192.168.0.1` logs you in with the key on your computer (key only, no passwords); comes back by itself after a reboot |
| **Advanced admin web** | Open `http://192.168.0.1:9090/` in a browser: band/cell locking, SMS, APN, firewall, Wi-Fi and more |
| **devui touch UI** | The front panel gets a new interface, organized by task into 5 tabs (Home · Cellular · Wi-Fi · Exit · System): signal verdict and carriers, Wi-Fi and clients, CHILL/Tailscale exit, eSIM switching |
| **eSIM** | With a removable eUICC card (5ber, eSTK.me and similar), download/switch/delete profiles in the admin web; switching also works from the screen |
| **CHILL** (optional, must be asked for by name) | Transparent proxy on the device (mihomo) + zashboard panel, rule-based routing; needs your own subscription URL |

Installing SSH also **turns off firmware auto-update** (both `zwrt_zte_dm.dm_update.dm_update_mode=0` and `TURNOFFPOLLING=1`, read back afterwards; see [Caveats](#caveats) for why).
It starts dropbear and confirms it is listening on 2222 before adding its `rc.local` line; if that fails, `rc.local` is left unchanged.

## Before you start

1. **Firmware B27 or earlier** (shown on the web admin home page or under 「设备信息」 (Device information), looks like `…MU5250V1.0.0B27`).
   Newer firmware drops the interface used to enable ADB (`zwrt_bsp.usb set`: gone on CN B31, reported gone from B28), so this kit no longer works there. **Do not upgrade first.**
2. You know the **router admin password** (the one you use to log in at `http://192.168.0.1`).
3. A **USB-C cable that carries data** (some charging cables only charge).
4. **adb** installed on your computer:
   - macOS: `brew install android-platform-tools`
   - Linux: `sudo apt install adb`
   - Windows: download Google's [SDK Platform-Tools](https://developer.android.com/tools/releases/platform-tools), unzip it and add the folder to PATH;
     then install [Git for Windows](https://git-scm.com/download/win) and run the commands below in **Git Bash**
5. For eSIM, have a removable eUICC card ready. A regular SIM also works for the install; the eSIM features just won't find a card.

## Install

```sh
tar xzf luyaz-kit-*.tar.gz
cd luyaz-kit
./install.sh
```

Follow the prompts:

1. Connect your computer to the U60's **Wi-Fi** (don't rely on USB tethering alone; that link drops once debugging is turned on) and enter the router admin password → the script turns on USB debugging through the web interface
2. When it says it is waiting for the ADB device, connect the U60 to your computer with the USB-C cable
3. Set an **advanced admin web login password** (press Enter to reuse the router admin password; no quotes, backslashes or spaces)
4. Push, install and verify run automatically and take 1–2 minutes; the screen flickers once and switches to the new interface
5. When asked whether to **reboot and verify**, say yes: after the reboot the script confirms that SSH, the admin web and the screen all come up on their own

At the end it prints a `~/.ssh/config` snippet; add it and `ssh u60` will log you in.

### Let Claude Code install it for you

The kit includes a `CLAUDE.md` with the deployment steps, how to handle errors, and what must not be touched on the device. Open Claude Code in the kit directory and it reads the file automatically:

```sh
cd luyaz-kit
claude
```

Then say "set up my U60". It checks adb, the network and the firmware first, then asks for passwords. If you'd rather not give them to it, run `cp u60.env.example u60.env`, fill it in yourself and tell it "the passwords are in u60.env". It stops and asks you for things like plugging in the USB cable and whether to reboot.

You can also install only some of the pieces:

```sh
./install.sh ssh               # ADB + persistent SSH only
./install.sh ssh admin devui   # no eSIM
./install.sh chill             # add the CHILL proxy (not part of the full set; not started on first install, see "CHILL" below)
./install.sh recover           # boot-time clean-up for `u60 ship`: puts /data/luyaz/ship/luyaz-recover.sh in place (after its
                               #   selftest on the device) and adds one line calling it to rc.local, before the service lines.
                               #   Not part of the full set: run it only when you mean to change rc.local
./install.sh status            # show component status
./install.sh doctor            # read-only health check: boot sync, auto-update, services, heartbeat, Wi-Fi, alerts... each item ●▲■
./install.sh backup            # back up the device config to this computer (./backups/, contains passwords, don't share);
                               # also saves config-…system.tgz (rc.local, our init.d scripts, dropbear host key, authorized_keys,
                               # start_dropbear.sh) for reference before a firmware upgrade; restore does not restore it
./install.sh restore 备份.tgz  # lists the files it would change first, writes only after you type yes
./install.sh reboot            # reboot once and confirm every component starts on its own
```

Later, to reinstall or update a component, get the new kit and run `./install.sh admin` (or `devui` / `esim`). It goes over SSH, no cable needed.

If the U60 is not at `192.168.0.1`: `GATEWAY=192.168.x.1 ./install.sh`.

## What gets installed, and where

Programs and data all live in `/data` (firmware upgrades don't wipe it). Autostart is only a few lines added to `/etc/rc.local`; no stock service is changed.
The core luyaz-core, the advanced admin web luyaz-web and the sentinel luyaz-sentinel are supervised by the system's procd: if one crashes it is restarted automatically and an alert is logged (SMS notification can be set up in the admin web under 「系统 → 告警」 (System → Alerts)).
After `admin` and `devui`, the kit writes what it installed (kit date, commit) into the device manifest `/data/luyaz/ship/manifest.jsonl`, one line per component (lweb; touch, sentinel, core), which `doctor` compares with the files.
`devui` installs the touch UI, luyaz-core and luyaz-sentinel. In `rc.local` the line `/etc/init.d/luyaz-core start` is followed by `/etc/init.d/luyaz-sentinel start`. After starting the sentinel it waits up to 60 s until it runs under procd, its heartbeat says `ours`, and the running touch UI is the file just installed. `admin` installs only luyaz-web.
The kit installs the current layout only. A device with an older layout (`/data/u60d`, `/data/u60-uid`, `/data/u60-ship`, `/data/plugins/u60pro-devui` or `/data/plugins/zwrt-datad` still there) is refused: upgrade it with an older kit first.

| Path | Purpose |
|---|---|
| `/data/ssh/` | dropbear, host key, `authorized_keys` (**the master copy of your public keys**) |
| `/data/local/tmp/start_dropbear.sh` | At boot, syncs the public keys to `/etc/dropbear/` and starts dropbear (:2222) |
| `/data/luyaz/luyaz-core`, `/data/luyaz/core/` | Core (:9460, local only): the touch UI's and admin web's data, and every change, go through it. State in `/data/luyaz/core/`, log `/tmp/luyaz-core.log` |
| `/data/luyaz/luyaz-web`, `/data/luyaz/operator-logos/` | Advanced admin web (luyaz-web on :9090) |
| `/data/luyaz/web.env` | **Admin web password** (readable by root only) |
| `/data/luyaz/luyaz-sentinel`, `/data/luyaz/screen/` | Sentinel: owns the screen, restarts the touch UI if it crashes, after two failed starts in a row switches back to the stock interface with an alert (long-press the bottom-right corner for 3 seconds to come back); also the Wi-Fi fallback watchdog and alert SMS. Control FIFO `/tmp/luyaz-screen.ctl`, log `/tmp/luyaz-sentinel.log`, heartbeat `/tmp/luyaz-sentinel.hb` |
| `/data/luyaz/ui/` | Touch UI `luyaz-ui`, its fonts and operator logos, the boot trace `boot-trace.log` and the stock UI backup `vendor-devui.orig-backup` |
| `/data/luyaz/scripts/`, `/etc/init.d/{luyaz-core,luyaz-sentinel,luyaz-web}` | Device scripts (doctor.sh, a shim over `luyaz-core doctor`; alert-lib.sh …; the ship executor luyaz-ship.sh and core-trial.sh too) and process supervision (started at boot by `/etc/init.d/… start` lines in `rc.local`, not via `enable`) |
| `/data/luyaz/ship/` | `u60 ship` transactions, manifest `manifest.jsonl`, boot clean-up `luyaz-recover.sh` |
| `/data/alerts/`, `/data/crashlog/` | Alert records, logs from program crashes |
| `/data/esim/` | lpac (eSIM card read/write tool) |
| `/data/chill/`, `/etc/init.d/chill` | CHILL: mihomo, rule sets, zashboard, `chill.sh`; subscription URL in `/data/chill/chill.env` (you write it, readable by root only) |
| `/data/luyaz/kit/rc.local.orig` | Backup of the stock `rc.local` from before the first install |

Older kits left `/data/zte-agent*`, `/data/admin/`, `/data/plugins/zwrt-datad/`, `/data/u60-guard/`, `/data/u60-uid/` behind; nothing uses them now and they are safe to delete (this kit refuses to install while the last two layouts' directories are there).

## Everyday use

- **Add another computer's SSH public key**: append it to `/data/ssh/authorized_keys`, then run `sh /data/local/tmp/start_dropbear.sh` (editing the copy in `/etc/dropbear/` does nothing; it is overwritten at boot).
- **Change the admin web password**: rerun `./install.sh admin` (it asks for a new password). Or over SSH edit the line in `/data/luyaz/web.env` (no quotes, backslashes or spaces), then log in again at `:9090` (luyaz-web reads the file at every login, no restart needed).
- **eSIM**: manage profiles in the admin web under 「移动网络 → eSIM」 (Mobile network → eSIM); on the screen, 「更多功能 → eSIM」 (More → eSIM), tap twice to switch. Takes effect in about 10 seconds, usually no reboot needed.
- **CHILL**: the first `./install.sh chill` installs without starting. SSH in and write your subscription URL into `/data/chill/chill.env` (copy `chill.env.example` in the same directory and edit it, `chmod 600`; static node snapshots go in `/data/chill/providers/`),
  then run `sh /data/chill/chill.sh safe-start`, and once you've confirmed the whole network works, run `sh /data/chill/chill.sh confirm` within 5 minutes (without confirmation it stops itself, so you can't lock yourself out).
  After that, turn it on/off and change the exit in the admin web under 「服务 → CHILL」 (Services → CHILL); the zashboard panel opens from the button on that page (served by the admin web itself at `:9090/chill-ui/` behind its login; also works over Tailscale).
  Running `./install.sh chill` again later is an update: the subscription URL and cache are left alone; if it was running, the core restarts, and if the new one fails to start it switches back to the old version automatically.
- **Need ADB temporarily**: SSH in and run `ubus call zwrt_bsp.usb set '{"mode":"debug"}'`; when 「USB 模式」 (USB mode) in `./install.sh status` shows `user`, it is back in normal mode. Only on firmware whose `ubus -v list zwrt_bsp.usb` still lists `set`: CN B31 lists only `list`, and the call fails with `Method not found`.

## Caveats

- **Don't upgrade the firmware.** An upgrade overwrites `rc.local` (SSH, admin web and screen all stop autostarting), and the new firmware can't enable ADB, so there is no way back. Installing SSH already turned off auto-update; don't tap upgrade prompts in the phone app or web page either.
- **Don't turn off stock services with `/etc/init.d/<service> disable`.** The U60's main daemon waits for a whole set of services to be ready before letting boot continue; disabling one leaves the screen stuck on the ZTE logo and the modem never dials. Ask first if you want to trim services.
- **Don't switch eSIM to a profile with no data and then keep working remotely.** If you are connected remotely through this U60's own network, switching cuts you off, and the only way back is to switch on the screen in front of the device.
- This is an unofficial modification. Use at your own risk.

## Restore stock

After logging in over SSH:

```sh
echo vendor > /tmp/luyaz-screen.ctl; sleep 8     # hand the screen back to the stock interface first
[ -x /data/chill/chill.sh ] && /data/chill/chill.sh stop; rm -f /etc/init.d/chill   # CHILL: stop it first; it puts the DNS settings back the way they were
for s in luyaz-sentinel luyaz-web luyaz-core u60-uid u60-guard zte-agent zwrt-datad; do [ -x /etc/init.d/$s ] && /etc/init.d/$s stop; rm -f /etc/init.d/$s; done
for f in /data/luyaz/kit/rc.local.orig /data/u60-kit/rc.local.orig; do [ -s $f ] && cp $f /etc/rc.local && break; done
rm -rf /data/luyaz /data/plugins/u60pro-devui /data/esim /data/chill /data/alerts /data/crashlog /data/power \
       /data/u60d /data/u60-kit /data/u60-ship /data/u60-guard /data/u60-uid /data/zte-agent /data/zte-agent.env /data/admin \
       /data/plugins/zwrt-datad /data/local/tmp/start_zte_agent.sh    # second line: leftovers from older kits
# If you don't want SSH either, also run: rm -rf /data/ssh /data/local/tmp/start_dropbear.sh
reboot
```

After the reboot the screen is back to the stock interface. Turn auto-update back on in the web admin if you need it.

## FAQ

**Login fails / locked out**: the web admin locks you out for a while after 5 wrong attempts in a row. The script shows the remaining attempts and the unlock countdown; don't keep guessing.

**The ADB device never shows up**: try another cable or another USB port (not a hub/dock). On Windows, open Device Manager; if a device has a yellow exclamation mark, right-click → Update driver → Browse my computer → pick "Android ADB Interface" from the list (install the Google USB Driver first). If `adb devices` shows a line ending in `device`, the connection works; rerunning `./install.sh` continues from there.

**It says SSH can't connect**: the device side is already installed. The message says which: no key / connection refused / auth failed / timeout. Usually the computer isn't on the U60's Wi-Fi, or a proxy app's TUN/enhanced mode is hijacking `192.168.0.1` (set `192.168.0.0/16` to direct in the proxy; no need to turn it off entirely). Then try again: `ssh -p 2222 -i ~/.ssh/id_ed25519 root@192.168.0.1`.

**The screen shows no data**: SSH in and look at `tail -n 50 /tmp/luyaz-core.log` (and `/tmp/luyaz-sentinel.log` if the screen itself is wrong), or run `./install.sh doctor`; a reboot usually fixes it.

**Install fails halfway**: the script can be run again and again; parts already installed are skipped or overwritten. Send the whole terminal output to whoever gave you the kit.
