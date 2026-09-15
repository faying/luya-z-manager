# Deploy — ZTE U60 Pro (MU5250) Manager

## TL;DR

```sh
./scripts/deploy.sh web      # build web/ → push to /data/admin (the :9090 LAN UI)
./scripts/deploy.sh agent    # cross-compile + push + restart zte-agent
./scripts/deploy.sh all      # both
./scripts/deploy.sh verify   # just re-check pages respond
```

One-time setup on a new machine:

```sh
cp scripts/.deploy.env.example scripts/.deploy.env   # then put the SSH password in it
brew install sshpass zig ; cargo install cargo-zigbuild   # agent build only
```

`scripts/.deploy.env` holds the device IP + SSH password and is **git-ignored** — the password is never committed.

## What each target does

- **web** — `cd web && npm run build` (static export → `web/out/`), tar it, and unpack into `/data/admin` on the device. That's the whole admin UI served at `http://<device>:9090/`.
- **agent** — cross-compile `zte-agent` for aarch64-musl with zig (`cargo zigbuild --release --target aarch64-unknown-linux-musl -p zte-agent`), upload to `/data/zte-agent`, kill the old pid, relaunch via `/data/local/tmp/start_zte_agent.sh`. Only needed when Rust code changed.

The script auto-detects the device IP (tries `192.168.0.1`, `192.168.1.1`, plus `DEVICE_HOST`) by probing SSH.

## Gotchas (why the script does what it does)

- **Verify happens ON the device.** If your computer runs a proxy in TUN / enhanced mode, `curl <device>:9090` from the computer often fails while SSH still works, so the script checks pages with on-device `wget http://127.0.0.1:9090/...`. Add a direct-route rule for `192.168.0.0/16` in the proxy if you want to reach it from a browser.
- **SSH rate-limits** rapid reconnects (dropbear). The script uses one connection per phase; if you see "Permission denied", wait a few seconds and retry.
- **Agent build needs zig** (`cargo zigbuild`); Homebrew's musl-cross linker is unreliable on macOS.
- **Never disable daemons in `/etc/config/zte_topsw_daemon.conf` via init.d** — see CLAUDE.md. Deploys here don't touch daemons, but keep it in mind.
- The device is read-only rootfs; `/data` is writable. The UI lives in `/data/admin`, the agent binary in `/data/zte-agent`.

## 装机包（onboard/）

```sh
./onboard/build-kit.sh                   # → onboard/dist/u60-kit-YYYYMMDD.tar.gz（约 15 MB）
FLEET_HOST=<ssh 别名> REFRESH_FLEET=1 ./onboard/build-kit.sh   # 重新从你的设备拉 zwrt-datad + /data/esim
```

- 包里：dropbear（OpenWrt 官方 ipk，sha256 钉死）、HEAD 现编的 zte-agent + web、`../zte-u60-pro-mu5250-touch-ui`（`DEVUI_REPO` 可改）里已构建的 `u60pro-devui.stripped` + `ui/`（去掉 CHILL 页）、从 `FLEET_HOST` 设备上拉的 zwrt-datad 和 `/data/esim`（缓存在 `onboard/cache/`，已 gitignore）。
- datad/eSIM 默认用设备上验证过的二进制，不重编：上游 datad 新版体积和接口都变了；lpac 依赖 Alpine edge，重编会漂。**这些二进制不在本仓库里，分发前要自己附上各自的许可证。**
- 对方只要 `adb` + `ssh`，跑 `./install.sh`。只适用 CN 固件 **B27 及以下**（B28+ 删了 `zwrt_bsp.usb set`）。
- 对方也可以在包目录里开 Claude Code 让它装：`onboard/kit-CLAUDE.md` 打包时改名成 `CLAUDE.md`。`install.sh` 支持无终端运行：密码走 `ROUTER_PASSWORD` / `AGENT_PASSWORD` 或包目录里的 `u60.env`（环境变量优先），无终端时不自动重启，重启验证单独 `./install.sh reboot`。改了 install.sh 的参数或提示文字，记得同步 kit-CLAUDE.md 和 README.md。
- 装机包的设备布局：dropbear 在 `/data/ssh/`（公钥和 host key 正本也在那，开机同步到 `/etc/dropbear/`）。
- 改了 `onboard/install.sh` 或 `onboard/device/install.sh` 以后，重打包再跑 `HOST=<ssh 别名> GATEWAY=<设备IP> SSH_KEY=<密钥> onboard/test/sandbox.sh run`：假 adb 转 ssh 到设备，设备端脚本改写到 `/data/local/tmp/kit-sb`（dropbear 2223、agent 127.0.0.1:19090、FOTA/`killall` 打桩、devui 禁用），跑 ADB 全新安装 → SSH 重跑 → status，最后自动清理并核对设备文件和进程没变。改写没覆盖到新代码时它会拒绝运行。devui 组件没法沙盒跑（会抢屏幕）。

## Manual fallback (if the script can't run)

```sh
# web
cd web && npm run build && tar czf /tmp/admin.tgz -C out .
sshpass -p '<pass>' ssh -o StrictHostKeyChecking=no -p 2222 root@192.168.0.1 \
  'rm -rf /data/admin && mkdir -p /data/admin && cd /data/admin && tar xzf -' < /tmp/admin.tgz
# verify (on device)
sshpass -p '<pass>' ssh -p 2222 root@192.168.0.1 'wget -q -O- http://127.0.0.1:9090/ | head -c 200'
```
