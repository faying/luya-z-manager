# 更新已装好的设备

第一次装机见 [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md)。这里讲装好以后怎么更新。

## 推荐：用新的装机包

```sh
./onboard/build-kit.sh                       # → onboard/dist/u60-kit-YYYYMMDD.tar.gz
tar xzf onboard/dist/u60-kit-*.tar.gz -C /tmp && cd /tmp/u60-kit
./install.sh admin                           # zte-agent + 管理网页（走 SSH，不用插线）
./install.sh devui                           # 触屏界面 + zwrt-datad + 看门狗脚本
./install.sh esim                            # eSIM 工具
./install.sh status                          # 看状态
./install.sh doctor                          # 只读体检
```

设备地址不是 `192.168.0.1` 时加 `GATEWAY=…`；SSH 密钥不是 `~/.ssh/id_ed25519` 时加 `SSH_KEY=…`。

## 打包相关

- `build-kit.sh` 的变量（`DATAD_BIN`、`DEVUI_BIN`、`UID_BIN`、`DEVUI_FONTS_DIR`、`DEVUI_REPO`、`CHILL=0` 等）写在脚本开头，也可以写进 `onboard/kit.local.env`（不进 git）。
- 触屏程序必须是 LVGL 版，`zwrt-datad` 必须是不带外部更新源的 Rust 版，否则 `build-kit.sh` 会停下。
- `FLEET_HOST=<ssh 别名> REFRESH_FLEET=1 ./onboard/build-kit.sh`：从一台已装好的设备重新拉 eSIM 工具（和 `zwrt-datad`）到 `onboard/cache/`。
- 打包的二进制（dropbear、lpac 及其库、zwrt-datad）不在本仓库里；把装机包给别人时，要附上它们各自的许可证。
- 改了 `onboard/install.sh` 或 `onboard/device/install.sh` 以后，重新打包，再用
  `HOST=<ssh 别名> GATEWAY=<设备地址> SSH_KEY=<密钥> onboard/test/sandbox.sh run` 在真机沙盒里跑一遍完整装机流程
  （设备端改写到 `/data/local/tmp/kit-sb`，结束后核对设备文件和进程没变；触屏组件不在沙盒里跑）。

## 手动更新单个程序（开发时）

用装机包装过的设备只认 SSH 密钥。设备上没有 scp/sftp，用管道传：

```sh
# 管理网页
cd web && npm run build && tar czf /tmp/admin.tgz -C out . && cd ..
ssh -p 2222 root@192.168.0.1 'rm -rf /data/admin.new && mkdir /data/admin.new && tar xzf - -C /data/admin.new \
  && rm -rf /data/admin.old && mv /data/admin /data/admin.old && mv /data/admin.new /data/admin' < /tmp/admin.tgz

# zte-agent：先传到临时名，再原子替换，最后由 procd 重启
cargo zigbuild --release --target aarch64-unknown-linux-musl -p zte-agent
ssh -p 2222 root@192.168.0.1 'cat > /data/zte-agent.new && chmod 755 /data/zte-agent.new \
  && cp -p /data/zte-agent /data/zte-agent.prev && mv /data/zte-agent.new /data/zte-agent \
  && /etc/init.d/zte-agent restart' < target/aarch64-unknown-linux-musl/release/zte-agent

# 在设备上验证（电脑开着代理 TUN 时，从电脑直接访问 :9090 常常不通）
ssh -p 2222 root@192.168.0.1 'wget -q -O- http://127.0.0.1:9090/ | head -c 200'
```

注意：

- 重启服务一律 `/etc/init.d/<名字> restart`，不要再手动 `nohup` 起第二份。
- 触屏程序**不要**这样直接覆盖：一启动就崩的版本会让设备进入重启循环。用 `./install.sh devui`，或按 touch-ui 仓库的开发说明先用别的文件名试跑。
- dropbear 对太快的重连会拒绝，多条命令合并到一次 `ssh` 里。

`scripts/deploy.sh` 是上游留下的脚本，用 `sshpass` 和密码登录，只适用于用上游 `setup.sh` 装、开着密码登录的设备。
