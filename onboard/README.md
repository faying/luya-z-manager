# U60 Pro (MU5250) install kit

**English** · [中文](README.zh-CN.md)

One-step setup for a **freshly unboxed ZTE U60 Pro (MU5250)**:

| Component | What you get after install |
|---|---|
| **SSH** | `ssh -p 2222 root@192.168.0.1` logs you in with the key on your computer (key only, no passwords); comes back by itself after a reboot |
| **Advanced admin web** | Open `http://192.168.0.1:9090/` in a browser: band/cell locking, SMS, APN, firewall, Wi-Fi and more |
| **devui touch UI** | The front panel gets a new interface, organized by task into 5 tabs (Home · Cellular · Wi-Fi · Exit · System): signal verdict and carriers, Wi-Fi and clients, Tailscale and exit, eSIM switching |
| **eSIM** | With a removable eUICC card (5ber, eSTK.me and similar), download/switch/delete profiles in the admin web; switching also works from the screen |

Installing SSH also **turns off firmware auto-update** (see [Caveats](#caveats) for why).

## Before you start

1. **Firmware B27 or earlier** (shown on the web admin home page or under 「设备信息」 (Device information), looks like `…MU5250V1.0.0B27`).
   From B28 on, ZTE removed the interface used to enable ADB, so this kit no longer works. **Do not upgrade first.**
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
tar xzf u60-kit-*.tar.gz
cd u60-kit
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
cd u60-kit
claude
```

Then say "set up my U60". It checks adb, the network and the firmware first, then asks for passwords. If you'd rather not give them to it, run `cp u60.env.example u60.env`, fill it in yourself and tell it "the passwords are in u60.env". It stops and asks you for things like plugging in the USB cable and whether to reboot.

You can also install only some of the pieces:

```sh
./install.sh ssh               # ADB + persistent SSH only
./install.sh ssh admin devui   # no eSIM
./install.sh status            # show component status
./install.sh doctor            # read-only health check: boot sync, auto-update, services, heartbeat, Wi-Fi, alerts... each item ●▲■
./install.sh backup            # back up the device config to this computer (./backups/, contains passwords, don't share)
./install.sh restore 备份.tgz  # lists the files it would change first, writes only after you type yes
./install.sh reboot            # reboot once and confirm every component starts on its own
```

Later, to reinstall or update a component, get the new kit and run `./install.sh admin` (or `devui` / `esim`). It goes over SSH, no cable needed.

If the U60 is not at `192.168.0.1`: `GATEWAY=192.168.x.1 ./install.sh`.

## What gets installed, and where

Programs and data all live in `/data` (firmware upgrades don't wipe it). Autostart is only a few lines added to `/etc/rc.local`; no stock service is changed.
The advanced admin web, the screen's data backend and the Wi-Fi fallback watchdog are supervised by the system's procd: if one crashes it is restarted automatically and an alert is logged (SMS notification can be set up in the admin web under 「系统 → 告警」 (System → Alerts)).

| Path | Purpose |
|---|---|
| `/data/ssh/` | dropbear, host key, `authorized_keys` (**the master copy of your public keys**) |
| `/data/local/tmp/start_dropbear.sh` | At boot, syncs the public keys to `/etc/dropbear/` and starts dropbear (:2222) |
| `/data/zte-agent`, `/data/admin/` | Advanced admin web program and pages |
| `/data/zte-agent.env` | **Admin web password** (one line `ZTE_AGENT_PASSWORD=…`, readable by root only) |
| `/data/plugins/u60pro-devui/u60-uid` | Screen supervisor: restarts the touch UI if it crashes; after two failed starts in a row it switches back to the stock interface (long-press the bottom-right corner of the screen for 3 seconds to come back) |
| `/data/u60-guard/`, `/etc/init.d/{zte-agent,zwrt-datad,u60-guard}` | Process supervision, Wi-Fi fallback watchdog, alert SMS (started at boot by `/etc/init.d/… start` lines in `rc.local`, not via `enable`) |
| `/data/alerts/`, `/data/crashlog/` | Alert records, logs from program crashes |
| `/data/plugins/u60pro-devui/`, `/data/plugins/zwrt-datad/` | Touch UI and its data backend |
| `/data/esim/` | lpac (eSIM card read/write tool) |
| `/data/u60-kit/rc.local.orig` | Backup of the stock `rc.local` from before the first install |

## Everyday use

- **Add another computer's SSH public key**: append it to `/data/ssh/authorized_keys`, then run `sh /data/local/tmp/start_dropbear.sh` (editing the copy in `/etc/dropbear/` does nothing; it is overwritten at boot).
- **Change the admin web password**: rerun `./install.sh admin` (it asks for a new password). Or over SSH edit the line in `/data/zte-agent.env` (no quotes, backslashes or spaces), then `/etc/init.d/zte-agent restart`, then `sh /data/u60-guard/agent-auth.sh verify` to confirm.
- **eSIM**: manage profiles in the admin web under 「移动网络 → eSIM」 (Mobile network → eSIM); on the screen, 「更多功能 → eSIM」 (More → eSIM), tap twice to switch. Takes effect in about 10 seconds, usually no reboot needed.
- **Proxy**: the install kit does not include a proxy; if you want one, set it up yourself from official sources following `docs/PROXY.md` in the manager repo.
- **Need ADB temporarily**: SSH in and run `ubus call zwrt_bsp.usb set '{"mode":"debug"}'`; when 「USB 模式」 (USB mode) in `./install.sh status` shows `user`, it is back in normal mode.

## Caveats

- **Don't upgrade the firmware.** An upgrade overwrites `rc.local` (SSH, admin web and screen all stop autostarting), and the new firmware can't enable ADB, so there is no way back. Installing SSH already turned off auto-update; don't tap upgrade prompts in the phone app or web page either.
- **Don't turn off stock services with `/etc/init.d/<service> disable`.** The U60's main daemon waits for a whole set of services to be ready before letting boot continue; disabling one leaves the screen stuck on the ZTE logo and the modem never dials. Ask first if you want to trim services.
- **Don't switch eSIM to a profile with no data and then keep working remotely.** If you are connected remotely through this U60's own network, switching cuts you off, and the only way back is to switch on the screen in front of the device.
- This is an unofficial modification. Use at your own risk.

## Restore stock

After logging in over SSH:

```sh
echo vendor > /tmp/u60-uid.ctl; sleep 8     # hand the screen back to the stock interface first
for s in u60-uid u60-guard zte-agent zwrt-datad; do /etc/init.d/$s stop; rm -f /etc/init.d/$s; done
cp /data/u60-kit/rc.local.orig /etc/rc.local
rm -rf /data/zte-agent /data/zte-agent.env /data/admin /data/plugins/u60pro-devui /data/plugins/zwrt-datad /data/esim \
       /data/u60-guard /data/u60-uid /data/alerts /data/crashlog /data/power /data/local/tmp/start_zte_agent.sh
# If you don't want SSH either, also run: rm -rf /data/ssh /data/local/tmp/start_dropbear.sh
reboot
```

After the reboot the screen is back to the stock interface. Turn auto-update back on in the web admin if you need it.

## FAQ

**Login fails / locked out**: the web admin locks you out for a while after 5 wrong attempts in a row. The script shows the remaining attempts and the unlock countdown; don't keep guessing.

**The ADB device never shows up**: try another cable or another USB port (not a hub/dock). On Windows, open Device Manager; if a device has a yellow exclamation mark, right-click → Update driver → Browse my computer → pick "Android ADB Interface" from the list (install the Google USB Driver first). If `adb devices` shows a line ending in `device`, the connection works; rerunning `./install.sh` continues from there.

**It says SSH can't connect**: the device side is already installed. Usually the computer isn't on the U60's Wi-Fi, or a proxy app's TUN/enhanced mode is hijacking `192.168.0.1` (set `192.168.0.0/16` to direct in the proxy; no need to turn it off entirely). Then try again: `ssh -p 2222 -i ~/.ssh/id_ed25519 root@192.168.0.1`.

**The screen shows no data**: SSH in and look at `cat /tmp/zwrt-datad.log`; a reboot usually fixes it.

**Install fails halfway**: the script can be run again and again; parts already installed are skipped or overwritten. Send the whole terminal output to whoever gave you the kit.
