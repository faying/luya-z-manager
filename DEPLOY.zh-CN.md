# Luya-Z：更新已装好的设备

[English](DEPLOY.md) · **中文**

第一次装机见 [docs/GETTING-STARTED.zh-CN.md](docs/GETTING-STARTED.zh-CN.md)。这里讲装好以后怎么更新。

## 推荐：用新的装机包

```sh
./onboard/build-kit.sh                       # → onboard/dist/luyaz-kit-YYYYMMDD.tar.gz
tar xzf onboard/dist/luyaz-kit-*.tar.gz -C /tmp && cd /tmp/luyaz-kit
./install.sh admin                           # luyaz-web 管理网页（走 SSH，不用插线）
./install.sh devui                           # 触屏界面 + 核心（luyaz-core）+ luyaz-sentinel
./install.sh esim                            # eSIM 工具
./install.sh chill                           # CHILL 代理（可选；先在设备上放好 chill.env，见 docs/PROXY.zh-CN.md）
./install.sh status                          # 看状态
./install.sh doctor                          # 只读体检
./install.sh reboot                          # 重启设备并确认各组件开机自己起来
./install.sh backup                          # 把配置备份到这台电脑（BACKUP_DIR，默认 ./backups）
./install.sh restore <备份.tgz>              # 先列出会改哪些文件，输入 yes 才写
./install.sh recover                         # ship 的开机收尾（/data/luyaz/ship/luyaz-recover.sh + rc.local 一行）；要单独点名
```

设备地址不是 `192.168.0.1` 时加 `GATEWAY=…`；SSH 密钥不是 `~/.ssh/id_ed25519` 时加 `SSH_KEY=…`。
设备上有没做完的 ship 事务（见 [touch-ui/docs/SHIP.md](touch-ui/docs/SHIP.md)）时，`admin`、`devui`、`recover` 会拒绝安装，先做完或退回。

## 打装机包

- `build-kit.sh` 只要本仓库加 Docker 就能打出完整装机包（核心、管理网页、哨兵、触屏、eSIM、字体全部现编或现生成，不从任何设备拉文件），见 [docs/GETTING-STARTED.zh-CN.md](docs/GETTING-STARTED.zh-CN.md) 第 3 节。
- `build-kit.sh` 的变量（`DATAD_BIN`、`DEVUI_BIN`、`SENTINEL_BIN`、`ESIM_TGZ`、`DEVUI_FONTS_DIR`、`FONTS=0`、`DEVUI_REPO` 等）写在脚本开头；也可以写进 `onboard/kit.local.env`（不进 git）。
- 触屏程序必须是 Rust 版，核心必须是不带外部更新源的 Rust 版 `luyaz-core`，否则 `build-kit.sh` 会停下。
- eSIM 工具由 `scripts/esim/build-esim-bundle.sh --out` 现打，Alpine 包的版本和 sha256 固定在 `scripts/esim/alpine.lock`。Alpine 安全更新替换了固定的包时，跑 `python3 scripts/esim/alpine_closure.py --relock`，重编后先在真机上验证 `lpac.sh chip info`。
- 装机包里的第三方二进制（dropbear、lpac 及其库）不在本仓库里；把装机包给别人时，带上各自的许可证。
- 改了 `onboard/install.sh` 或 `onboard/device/install.sh` 后，重打装机包，再用
  `HOST=<ssh 别名> GATEWAY=<设备地址> SSH_KEY=<密钥> onboard/test/sandbox.sh run`
  在真机沙盒里跑一遍完整装机流程（设备端写入都重定向到 `/data/local/tmp/kit-sb`；跑完核对设备文件和进程没变；沙盒里不跑触屏组件）。

## 手工更新单个程序（开发时）

用装机包装好的设备只认 SSH 密钥。设备上没有 scp/sftp，用管道传。
先传到临时名、留一份上一版、用 `mv` 原子替换，再让 procd 重启服务：

```sh
# 管理网页（luyaz-web）
cargo zigbuild --release --target aarch64-unknown-linux-musl -p luyaz-web
ssh -p 2222 root@192.168.0.1 'cat > /data/luyaz/luyaz-web.new && chmod 755 /data/luyaz/luyaz-web.new \
  && cp -p /data/luyaz/luyaz-web /data/luyaz/luyaz-web.prev && mv /data/luyaz/luyaz-web.new /data/luyaz/luyaz-web \
  && /etc/init.d/luyaz-web restart' < target/aarch64-unknown-linux-musl/release/luyaz-web

# 在设备上验证（电脑开着代理 TUN 时，从电脑直连 :9090 常常不通）
ssh -p 2222 root@192.168.0.1 'wget -q -O- http://127.0.0.1:9090/ | head -c 200'
```

核心（`/data/luyaz/luyaz-core`、`/etc/init.d/luyaz-core`）做法一样，但屏幕和管理网页都靠它；尽量用 `./install.sh devui`。

注意：

- 重启服务一律用 `/etc/init.d/<名字> restart`，别用 `nohup` 手工再起一份。
- **别**用这种方式覆盖触屏程序（`/data/luyaz/ui/luyaz-ui`）或哨兵：一启动就崩的构建会让整机进入重启循环，屏幕上几分钟没有界面也会整机重启。用 `./install.sh devui`，或按 [touch-ui/docs/DEVELOPMENT.zh-CN.md](touch-ui/docs/DEVELOPMENT.zh-CN.md) 先用别的文件名试跑。
- dropbear 会拒绝连得太快的重连，多条命令合进一次 `ssh`。
