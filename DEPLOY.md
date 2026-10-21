# Luya-Z: updating an installed device

**English** · [中文](DEPLOY.zh-CN.md)

For the first install, see [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md). This page covers updating after that.

## Recommended: use a new install kit

```sh
./onboard/build-kit.sh                       # → onboard/dist/luyaz-kit-YYYYMMDD.tar.gz
tar xzf onboard/dist/luyaz-kit-*.tar.gz -C /tmp && cd /tmp/luyaz-kit
./install.sh admin                           # luyaz-web, the admin web (over SSH, no cable needed)
./install.sh devui                           # touch UI + core (luyaz-core) + luyaz-sentinel
./install.sh esim                            # eSIM tools
./install.sh chill                           # CHILL proxy (optional; put chill.env on the device first, see docs/PROXY.md)
./install.sh status                          # show status
./install.sh doctor                          # read-only health check
./install.sh reboot                          # reboot and confirm every component comes back by itself
./install.sh backup                          # back up the configuration to this computer (BACKUP_DIR, default ./backups)
./install.sh restore <backup.tgz>            # lists what it would change, writes only after you type yes
./install.sh recover                         # ship's boot-time cleanup (/data/luyaz/ship/luyaz-recover.sh + one rc.local line); only when named
```

If the device is not at `192.168.0.1`, add `GATEWAY=…`; if your SSH key is not `~/.ssh/id_ed25519`, add `SSH_KEY=…`.
If the device has an unfinished ship transaction (see [touch-ui/docs/SHIP.md](touch-ui/docs/SHIP.md)), `admin`, `devui` and `recover` refuse to install until it is finished or rolled back.

## Building the kit

- `build-kit.sh` only needs this repository plus Docker to build a complete kit (core, admin web, sentinel, touch UI, eSIM and fonts are all built or generated fresh, nothing is pulled from any device); see section 3 of [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md).
- `build-kit.sh` variables (`DATAD_BIN`, `DEVUI_BIN`, `SENTINEL_BIN`, `ESIM_TGZ`, `DEVUI_FONTS_DIR`, `FONTS=0`, `DEVUI_REPO`, etc.) are listed at the top of the script; you can also put them in `onboard/kit.local.env` (not tracked by git).
- The touch program must be the Rust build and the core must be the Rust `luyaz-core` without external update sources; otherwise `build-kit.sh` stops.
- The eSIM tools are built fresh by `scripts/esim/build-esim-bundle.sh --out`, with Alpine package versions and sha256 pinned in `scripts/esim/alpine.lock`. When an Alpine security update replaces a pinned package, run `python3 scripts/esim/alpine_closure.py --relock`, rebuild, and verify `lpac.sh chip info` on a real device first.
- Bundled third-party binaries (dropbear, lpac and its libraries) are not in this repo; when you give the kit to someone else, include each one's license.
- Vendor names for connected devices need an OUI table, which this repo does not ship: build it from an IEEE `oui.txt` you download yourself with `scripts/lanid/make-oui.sh oui.txt oui.tsv`, then pass `OUI_TSV=oui.tsv` to `build-kit.sh` or copy it to `/data/luyaz/lanid/oui.tsv` on the device. Without it only the vendor is missing.
- After changing `onboard/install.sh` or `onboard/device/install.sh`, rebuild the kit, then run the full install flow in a sandbox on a real device with
  `HOST=<ssh alias> GATEWAY=<device address> SSH_KEY=<key> onboard/test/sandbox.sh run`
  (device-side writes are redirected to `/data/local/tmp/kit-sb`; afterwards it checks that device files and processes are unchanged; the touch UI component is not run in the sandbox).

## Updating a single program by hand (during development)

A device set up with the install kit only accepts SSH keys. There is no scp/sftp on the device, so transfer through a pipe.
Upload to a temporary name, keep the previous version, replace atomically with `mv`, then let procd restart the service:

```sh
# admin web (luyaz-web)
cargo zigbuild --release --target aarch64-unknown-linux-musl -p luyaz-web
ssh -p 2222 root@192.168.0.1 'cat > /data/luyaz/luyaz-web.new && chmod 755 /data/luyaz/luyaz-web.new \
  && cp -p /data/luyaz/luyaz-web /data/luyaz/luyaz-web.prev && mv /data/luyaz/luyaz-web.new /data/luyaz/luyaz-web \
  && /etc/init.d/luyaz-web restart' < target/aarch64-unknown-linux-musl/release/luyaz-web

# verify on the device (with a proxy TUN running on the computer, reaching :9090 directly from the computer often fails)
ssh -p 2222 root@192.168.0.1 'wget -q -O- http://127.0.0.1:9090/ | head -c 200'
```

The core (`/data/luyaz/luyaz-core`, `/etc/init.d/luyaz-core`) works the same way, but the screen and the admin web depend on it; prefer `./install.sh devui`.

Notes:

- Always restart services with `/etc/init.d/<name> restart`; don't start a second copy by hand with `nohup`.
- **Don't** overwrite the touch UI binary (`/data/luyaz/ui/luyaz-ui`) or the sentinel this way: a build that crashes on startup puts the device into a reboot loop, and a screen with no UI for a few minutes also triggers a full reboot. Use `./install.sh devui`, or follow [touch-ui/docs/DEVELOPMENT.md](touch-ui/docs/DEVELOPMENT.md) and do a trial run under a different file name first.
- dropbear refuses reconnections that come too fast; combine multiple commands into one `ssh`.
