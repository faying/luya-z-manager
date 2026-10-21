# Luya-Z install kit (ZTE U60 Pro / MU5250)

**English** · [中文](README.zh-CN.md)

This kit turns a **ZTE U60 Pro (MU5250) straight out of the box** into a Luya-Z device in one run of `./install.sh`.

## What it adds

| Component | After installing |
|---|---|
| **SSH** | Key-only login with `ssh -p 2222 root@192.168.0.1` (password login stays off). Survives reboots |
| **Advanced admin web** | `http://192.168.0.1:9090/` in a browser: band and cell locking, SMS, APN, firewall, Wi-Fi and more |
| **devui touch UI** | A new front-panel interface with 5 task tabs (Home · Cellular · Wi-Fi · Exit · System): signal verdict and carriers, Wi-Fi and clients, CHILL/Tailscale exit, eSIM switching |
| **eSIM** | Download, switch and delete profiles on a removable eUICC card (5ber, eSTK.me and the like) from the admin web; switching works on the screen too |
| **CHILL** (optional; only when you name it) | A transparent proxy on the device (mihomo) with the zashboard panel and rule-based routing. Bring your own subscription URL |

Two things happen as part of **SSH**:

- **Firmware auto-update is switched off**: `zwrt_zte_dm.dm_update.dm_update_mode=0` and `TURNOFFPOLLING=1` are both set and then read back. [Things to avoid](#things-to-avoid) explains why.
- dropbear is started first and must be listening on 2222 before its line goes into `rc.local`. If it does not come up, `rc.local` is not touched.

## What you need

1. **Firmware B27 or older.** The version is on the web admin home page or under 「设备信息」 (Device information) and ends in something like `…MU5250V1.0.0B27`. Later firmware removed the call the kit uses to switch on ADB (`zwrt_bsp.usb set`: missing on CN B31, reported missing from B28 on), so the kit cannot get in there. **Do not upgrade before installing.**
2. The **router admin password**, i.e. what you type at `http://192.168.0.1`.
3. A **USB-C cable that carries data**. Some cables only charge.
4. **adb** on the computer:
   - macOS: `brew install android-platform-tools`
   - Linux: `sudo apt install adb`
   - Windows: get Google's [SDK Platform-Tools](https://developer.android.com/tools/releases/platform-tools), unzip, put the folder on PATH.
     Also install [Git for Windows](https://git-scm.com/download/win) and type the commands below into **Git Bash**.
5. For eSIM, a removable eUICC card. An ordinary SIM is fine for installing; the eSIM screens will simply report that there is no card.

## Installing

```sh
tar xzf luyaz-kit-*.tar.gz
cd luyaz-kit
./install.sh
```

What the script walks you through:

1. Join the U60's **Wi-Fi** and type the router admin password. The script uses the web interface to turn on USB debugging. (Don't depend on USB tethering alone: that link goes away when debugging mode is switched on.)
2. When it says it is waiting for the ADB device, plug the U60 into the computer with the USB-C cable.
3. Pick a login password for the **advanced admin web**. Enter alone reuses the router admin password. Quotes, backslashes and spaces are not allowed.
4. Upload, install and checks then run by themselves for 1–2 minutes. The screen flickers once and comes back with the new interface.
5. Answer yes to **reboot and verify**. After the restart the script checks that SSH, the admin web and the screen came back without help.

The last thing it prints is a block for `~/.ssh/config`; with that added, `ssh u60` logs you in.

### Having Claude Code do it

There is a `CLAUDE.md` in the kit describing the steps, what to do when something fails and what must never be touched on the device. Claude Code picks it up when started in the kit folder:

```sh
cd luyaz-kit
claude
```

Ask it to "set up my U60". It checks adb, the network and the firmware before asking for passwords. To keep the passwords out of the chat, run `cp u60.env.example u60.env`, fill the file in yourself and tell it "the passwords are in u60.env". It will pause and ask you whenever a person is needed, e.g. to plug in the cable or to agree to a reboot.

### Picking components, and other commands

```sh
./install.sh ssh               # ADB, then SSH that survives reboots; nothing else
./install.sh ssh admin devui   # everything except eSIM
./install.sh chill             # CHILL proxy (not in the default set; first install does not start it, see Everyday use)
./install.sh recover           # boot-time clean-up for `u60 ship`: installs /data/luyaz/ship/luyaz-recover.sh once it passes
                               #   a self-test on the device, and adds a line calling it to rc.local ahead of the service lines.
                               #   Not in the default set: use it only when you intend to change rc.local
./install.sh status            # state of each component
./install.sh doctor            # read-only check (boot sync, auto-update, services, heartbeat, Wi-Fi, alerts …), marked ●▲■
./install.sh backup            # copy the device config to ./backups/ on this computer (holds passwords, keep it private);
                               #   also writes config-…system.tgz with rc.local, our init.d scripts, the dropbear host key,
                               #   authorized_keys and start_dropbear.sh, for reference before a firmware upgrade (restore skips it)
./install.sh restore 备份.tgz  # shows which files would change and writes only after you type yes
./install.sh reboot            # restart once and confirm everything comes back on its own
```

To update or reinstall a component later, unpack the newer kit and run e.g. `./install.sh admin` (or `devui` / `esim`). That goes over SSH; no cable needed.

U60 at a different address: `GATEWAY=192.168.x.1 ./install.sh`.

## What ends up on the device

Everything lives under `/data`, which firmware upgrades leave alone. Autostart is a handful of lines in `/etc/rc.local`; stock services are not changed.

- The core luyaz-core, the admin web luyaz-web and the sentinel luyaz-sentinel run under procd. A crash gets them restarted and leaves an alert (SMS notification can be turned on in the admin web under 「系统 → 告警」 (System → Alerts)).
- `admin` installs luyaz-web only. `devui` installs the touch UI, luyaz-core and luyaz-sentinel; in `rc.local`, `/etc/init.d/luyaz-sentinel start` comes right after `/etc/init.d/luyaz-core start`. Once the sentinel is started the script waits up to 60 s for three things: the sentinel runs under procd, its heartbeat says `ours`, and the touch UI that is running is the file just installed.
- Both `admin` and `devui` record what they installed (kit date, commit) in the device manifest `/data/luyaz/ship/manifest.jsonl`, one line per component (lweb; touch, sentinel, core). `doctor` checks the files against it.
- Only the current layout is supported. If `/data/u60d`, `/data/u60-uid`, `/data/u60-ship`, `/data/plugins/u60pro-devui` or `/data/plugins/zwrt-datad` still exists, the kit refuses to install; bring the device forward with an older kit first.

| Path | What it is |
|---|---|
| `/data/ssh/` | dropbear, its host key, and `authorized_keys`: **the copy of your public keys that counts** |
| `/data/local/tmp/start_dropbear.sh` | Runs at boot: copies the public keys to `/etc/dropbear/`, starts dropbear on :2222 |
| `/data/luyaz/luyaz-core`, `/data/luyaz/core/` | The core (:9460, local only). Every read and change from the touch UI and the admin web goes through it. State in `/data/luyaz/core/`, log `/tmp/luyaz-core.log` |
| `/data/luyaz/luyaz-web`, `/data/luyaz/operator-logos/` | The advanced admin web (luyaz-web, :9090) |
| `/data/luyaz/web.env` | **The admin web password**, readable only by root |
| `/data/luyaz/luyaz-sentinel`, `/data/luyaz/screen/` | The sentinel. It owns the screen and restarts the touch UI after a crash; after two failed starts in a row it puts the stock interface back and raises an alert (hold the bottom-right corner for 3 s to return). It is also the Wi-Fi fallback watchdog and sends alert SMS. Control FIFO `/tmp/luyaz-screen.ctl`, log `/tmp/luyaz-sentinel.log`, heartbeat `/tmp/luyaz-sentinel.hb` |
| `/data/luyaz/ui/` | The touch UI `luyaz-ui` with its fonts and operator logos, the boot trace `boot-trace.log`, and `vendor-devui.orig-backup` (copy of the stock UI) |
| `/data/luyaz/scripts/`, `/etc/init.d/{luyaz-core,luyaz-sentinel,luyaz-web}` | Device scripts (doctor.sh, which hands over to `luyaz-core doctor`; alert-lib.sh …; the ship executor luyaz-ship.sh and core-trial.sh) and the procd services. They start from `/etc/init.d/… start` lines in `rc.local`, not through `enable` |
| `/data/luyaz/ship/` | `u60 ship` transactions, the manifest `manifest.jsonl`, the boot clean-up `luyaz-recover.sh` |
| `/data/alerts/`, `/data/crashlog/` | Alert history, crash logs |
| `/data/esim/` | lpac, the tool that reads and writes the eSIM card |
| `/data/chill/`, `/etc/init.d/chill` | CHILL: mihomo, rule sets, zashboard, `chill.sh`. The subscription URL goes in `/data/chill/chill.env` (written by you, readable only by root) |
| `/data/luyaz/kit/rc.local.orig` | The stock `rc.local` as it was before the first install |

Leftovers from older kits (`/data/zte-agent*`, `/data/admin/`, `/data/plugins/zwrt-datad/`, `/data/u60-guard/`, `/data/u60-uid/`) are no longer used and can be deleted. While the directories of the last two layouts are present, this kit will not install.

## Everyday use

- **Let another computer in over SSH**: add its public key to `/data/ssh/authorized_keys` and run `sh /data/local/tmp/start_dropbear.sh`. Editing `/etc/dropbear/` has no lasting effect; it is rewritten at every boot.
- **New admin web password**: run `./install.sh admin` again and give a new one. Or, over SSH, change the line in `/data/luyaz/web.env` (no quotes, backslashes or spaces) and log in at `:9090` again. luyaz-web reads the file on each login, so nothing needs restarting.
- **eSIM**: profiles are managed in the admin web under 「移动网络 → eSIM」 (Mobile network → eSIM). On the screen it is 「更多功能 → eSIM」 (More → eSIM); tap a profile twice to switch. The switch takes about 10 seconds and normally needs no reboot.
- **CHILL**: the first `./install.sh chill` installs it but leaves it stopped.
  1. Over SSH, copy `/data/chill/chill.env.example` to `/data/chill/chill.env`, put your subscription URL in it and `chmod 600` it. Static node snapshots go in `/data/chill/providers/`.
  2. Run `sh /data/chill/chill.sh safe-start`. Check that the whole network still works, then run `sh /data/chill/chill.sh confirm` within 5 minutes. Without the confirm it shuts itself off again, so a bad config cannot lock you out.
  3. From then on, switch it on and off and pick the exit in the admin web under 「服务 → CHILL」 (Services → CHILL). The button there opens zashboard, which the admin web serves behind its own login at `:9090/chill-ui/` (also reachable over Tailscale).

  A later `./install.sh chill` is an update. It keeps the subscription URL and cache; if CHILL was running, the core is restarted, and a new core that fails to start is swapped back for the old one automatically.
- **ADB for a while**: over SSH, `ubus call zwrt_bsp.usb set '{"mode":"debug"}'`. When 「USB 模式」 (USB mode) in `./install.sh status` reads `user`, the port is back to normal. This only works on firmware where `ubus -v list zwrt_bsp.usb` still shows `set`; CN B31 shows only `list` and the call fails with `Method not found`.

## Things to avoid

- **Upgrading the firmware.** An upgrade replaces `rc.local`, so SSH, the admin web and the screen no longer start at boot, and the new firmware has no way to turn on ADB, so you cannot reinstall. Auto-update was already switched off with SSH; also ignore upgrade prompts in the phone app and the web page.
- **`/etc/init.d/<service> disable` on stock services.** The U60's main daemon holds the boot until a fixed group of services has reported in. Disable one of them and the screen sits on the ZTE logo and the modem never dials. Ask before trimming services.
- **Switching eSIM to a profile without data while you are remote.** If your remote session runs over this U60's own connection, the switch cuts it, and the only way back is switching on the screen in person.
- This is an unofficial modification and you use it at your own risk.

## Going back to stock

Log in over SSH, then:

```sh
echo vendor > /tmp/luyaz-screen.ctl; sleep 8     # stock interface takes the screen first
[ -x /data/chill/chill.sh ] && /data/chill/chill.sh stop; rm -f /etc/init.d/chill   # stopping CHILL restores the DNS settings
for s in luyaz-sentinel luyaz-web luyaz-core u60-uid u60-guard zte-agent zwrt-datad; do [ -x /etc/init.d/$s ] && /etc/init.d/$s stop; rm -f /etc/init.d/$s; done
for f in /data/luyaz/kit/rc.local.orig /data/u60-kit/rc.local.orig; do [ -s $f ] && cp $f /etc/rc.local && break; done
rm -rf /data/luyaz /data/plugins/u60pro-devui /data/esim /data/chill /data/alerts /data/crashlog /data/power \
       /data/u60d /data/u60-kit /data/u60-ship /data/u60-guard /data/u60-uid /data/zte-agent /data/zte-agent.env /data/admin \
       /data/plugins/zwrt-datad /data/local/tmp/start_zte_agent.sh    # from the second line on: what older kits left behind
# To remove SSH as well: rm -rf /data/ssh /data/local/tmp/start_dropbear.sh
reboot
```

The stock interface is back after the restart. If you want auto-update again, turn it on in the web admin.

## Troubleshooting

**Login fails or is locked**: five wrong passwords in a row lock the web admin for a while. The script prints how many tries are left and when the lock ends; stop guessing and check the password.

**No ADB device appears**: try a different cable and a USB port directly on the computer (no hub or dock). On Windows, look in Device Manager for an entry with a yellow exclamation mark: install the Google USB Driver, then right-click → Update driver → Browse my computer → choose "Android ADB Interface". Once `adb devices` lists a line ending in `device`, run `./install.sh` again and it carries on.

**SSH does not connect**: the device side is already done; the message says whether there was no key, the connection was refused, authentication failed, or it timed out. The usual causes are a computer that is not on the U60's Wi-Fi, or a proxy app in TUN/enhanced mode grabbing `192.168.0.1` (add `192.168.0.0/16` as a direct rule; the proxy can stay on). Then try `ssh -p 2222 -i ~/.ssh/id_ed25519 root@192.168.0.1`.

**No data on the screen**: over SSH, `tail -n 50 /tmp/luyaz-core.log` (and `/tmp/luyaz-sentinel.log` if the screen itself misbehaves), or run `./install.sh doctor`. A reboot usually clears it.

**Install stopped halfway**: just run it again; finished parts are skipped or overwritten. If it keeps failing, send the full terminal output to whoever gave you the kit.
