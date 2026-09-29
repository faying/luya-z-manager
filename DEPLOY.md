# Updating an installed U60 Pro (MU5250)

**English** · [中文](DEPLOY.zh-CN.md)

For the first install, see [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md). This page covers updating after that.

## Recommended: use a new install kit

```sh
./onboard/build-kit.sh                       # → onboard/dist/u60-kit-YYYYMMDD.tar.gz
tar xzf onboard/dist/u60-kit-*.tar.gz -C /tmp && cd /tmp/u60-kit
./install.sh admin                           # zte-agent + admin web (over SSH, no cable needed)
./install.sh devui                           # touch UI + zwrt-datad + watchdog scripts
./install.sh esim                            # eSIM tools
./install.sh status                          # show status
./install.sh doctor                          # read-only health check
```

If the device is not at `192.168.0.1`, add `GATEWAY=…`; if your SSH key is not `~/.ssh/id_ed25519`, add `SSH_KEY=…`.

## Building the kit

- `build-kit.sh` only needs the three public repos checked out side by side plus Docker to build a complete kit (touch UI, zwrt-datad, eSIM and fonts are all built or generated fresh, nothing is pulled from any device); see section 3 of [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md).
- `build-kit.sh` variables (`DATAD_BIN`, `DATAD_REPO`, `DEVUI_BIN`, `UID_BIN`, `ESIM_TGZ`, `DEVUI_FONTS_DIR`, `FONTS=0`, `DEVUI_REPO`, etc.) are listed at the top of the script; you can also put them in `onboard/kit.local.env` (not tracked by git).
- The touch UI binary must be the LVGL version, and `zwrt-datad` must be the Rust version without external update sources; otherwise `build-kit.sh` stops.
- The eSIM tools are built fresh by `scripts/esim/build-esim-bundle.sh --out`, with Alpine package versions and sha256 pinned in `scripts/esim/alpine.lock`. When an Alpine security update replaces a pinned package, run `python3 scripts/esim/alpine_closure.py --relock`, rebuild, and verify `lpac.sh chip info` on a real device first.
- The bundled binaries (dropbear, lpac and its libraries, zwrt-datad) are not in this repo; when you give the kit to someone else, include each one's license.
- After changing `onboard/install.sh` or `onboard/device/install.sh`, rebuild the kit, then run the full install flow in a sandbox on a real device with
  `HOST=<ssh alias> GATEWAY=<device address> SSH_KEY=<key> onboard/test/sandbox.sh run`
  (device-side writes are redirected to `/data/local/tmp/kit-sb`; afterwards it checks that device files and processes are unchanged; the touch UI component is not run in the sandbox).

## Updating a single program by hand (during development)

A device set up with the install kit only accepts SSH keys. There is no scp/sftp on the device, so transfer through a pipe:

```sh
# admin web
cd web && npm run build && tar czf /tmp/admin.tgz -C out . && cd ..
ssh -p 2222 root@192.168.0.1 'rm -rf /data/admin.new && mkdir /data/admin.new && tar xzf - -C /data/admin.new \
  && rm -rf /data/admin.old && mv /data/admin /data/admin.old && mv /data/admin.new /data/admin' < /tmp/admin.tgz

# zte-agent: upload to a temporary name, replace atomically, then let procd restart it
cargo zigbuild --release --target aarch64-unknown-linux-musl -p zte-agent
ssh -p 2222 root@192.168.0.1 'cat > /data/zte-agent.new && chmod 755 /data/zte-agent.new \
  && cp -p /data/zte-agent /data/zte-agent.prev && mv /data/zte-agent.new /data/zte-agent \
  && /etc/init.d/zte-agent restart' < target/aarch64-unknown-linux-musl/release/zte-agent

# verify on the device (with a proxy TUN running on the computer, reaching :9090 directly from the computer often fails)
ssh -p 2222 root@192.168.0.1 'wget -q -O- http://127.0.0.1:9090/ | head -c 200'
```

Notes:

- Always restart services with `/etc/init.d/<name> restart`; don't start a second copy by hand with `nohup`.
- **Don't** overwrite the touch UI binary this way: a build that crashes on startup puts the device into a reboot loop. Use `./install.sh devui`, or follow the touch-ui repo's development notes and do a trial run under a different file name first.
- dropbear refuses reconnections that come too fast; combine multiple commands into one `ssh`.

`scripts/deploy.sh` is left over from upstream. It uses `sshpass` and password login, and only works on devices installed with upstream's `setup.sh` that still have password login enabled.
