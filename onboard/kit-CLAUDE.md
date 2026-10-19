# Luya-Z 装机包 — 给 Claude 的部署说明

你现在所在的目录是一个 **Luya-Z 装机包**（给 ZTE U60 Pro（MU5250）用）。用户是设备主人，他请你帮忙时按这份说明做。
给人看的说明在 `README.md`，内容一致，这份更偏操作和排错。

这个包会给设备装上：

| 组件 | 内容 |
|---|---|
| `ssh` | dropbear，端口 2222，只认密钥，开机自启（先起 dropbear、10 秒内确认进程在且 2222 在监听，才写 rc.local 那一行；没起来 rc.local 不动）。电脑端 SSH 检查失败会说是哪种：没有密钥 / 连接被拒 / 认证失败 / 超时 |
| `admin` | 只装高级后台 = 管理网页 luyaz-web，`http://<设备>:9090/`（数据和改动都经核心 luyaz-core，它随 `devui` 装在 `/data/luyaz/luyaz-core`，见下表） |
| `devui` | 前面板触屏界面 + 核心 luyaz-core（`/data/luyaz/luyaz-core`，procd 服务 `/etc/init.d/luyaz-core`）+ 哨兵 luyaz-sentinel（管屏幕和看门狗）。rc.local 在 `/etc/init.d/luyaz-core start` 后面只留一行 `/etc/init.d/luyaz-sentinel start   # u60 ship sentinel: …`。只装现在的布局：还有 `/data/u60d`、`/data/u60-uid`、`/data/u60-ship`、`/data/plugins/u60pro-devui`、`/data/plugins/zwrt-datad` 的设备拒装（先用旧版装机包升级）。起哨兵后最多等 60 秒，核对：哨兵在 procd 下运行、心跳第 3 栏是 `ours`、界面进程 exe md5 = 刚装的文件。设备清单记 touch、sentinel、core 三条（`admin` 记 lweb 一条） |
| `esim` | lpac，配合可插拔 eUICC 卡管理/切换 profile |
| `recover`（可选，不在全套里） | u60 ship 的开机收尾：换上 `/data/luyaz/ship/luyaz-recover.sh`（设备上自检过才换），并在 `/etc/rc.local` 各服务启动行之前加一行调用它。**改 rc.local，先问用户**。`admin`/`devui` 只在设备上还没有这个脚本时放一份，不碰 rc.local |
| `chill`（可选，不在全套里） | CHILL 透明代理：mihomo + 规则集 + zashboard 面板（`/data/chill`）。第一次装**不启动**：要用户自己写订阅地址（`/data/chill/chill.env`，别替用户填、别读出来），再 `chill.sh safe-start` + 5 分钟内 `confirm` |

装 `ssh` 组件时还会关闭固件自动升级（升级会覆盖开机自启，新固件又开不了 ADB）：`dm_update_mode=0` 和 `TURNOFFPOLLING=1` 两项都设（只往关的方向改），并读回核对。
**全局要求：固件自动升级永远保持关闭**，任何脚本、页面、自动化都不能把它打开（`zwrt_zte_dm.dm_update.dm_update_mode` 必须是 `0`，且 `zwrt_zte_dm.dm_update.TURNOFFPOLLING` 必须是 `1`）。

## 一、先检查，不满足就停下来告诉用户

1. **工具**：`command -v adb ssh ssh-keygen curl tar`。
   缺 adb 时告诉用户怎么装（macOS `brew install android-platform-tools`；Linux `sudo apt install adb`；
   Windows 下载 Google SDK Platform-Tools 加进 PATH）。替用户装软件前先征得同意。
2. **电脑通过 Wi-Fi 连着 U60**，设备默认地址 `192.168.0.1`。
   别只靠 USB 网络共享：开 ADB 后 USB 口切成调试模式，那条网就没了。
   ```sh
   curl -s -m 5 --noproxy '*' http://192.168.0.1/ubus/ -H 'Content-Type: application/json' \
     -H 'Referer: http://192.168.0.1/' -H 'Origin: http://192.168.0.1' \
     -d '[{"jsonrpc":"2.0","id":1,"method":"call","params":["00000000000000000000000000000000","zwrt_web","web_login_info",{}]}]'
   ```
   返回里有 `zte_web_sault` 就对了；`login_fail_num` 是网页密码还能试几次，`login_fail_lock_lefttime` > 0 表示被锁。
   连不上：让用户确认连的是 U60 的 Wi-Fi。**不要让用户关代理**（你自己可能就靠它联网）；
   代理开着 TUN/增强模式（Surge、Clash 等）时，请用户把 `192.168.0.0/16` 设成直连。
   地址不是 192.168.0.1 的话，后面所有命令前加 `GATEWAY=<地址>`。
3. **固件**：只支持 CN 固件 **B27 及以下**。用户不确定也没关系，脚本登录后会打印版本；
   B28+ 会在「开 ADB 被拒绝」那一步停下，这时**到此为止**，不要尝试任何别的解锁办法。
4. **USB-C 数据线**已经把 U60 接到这台电脑（开 ADB 后脚本最多等 90 秒），而且电脑上**没有接别的安卓设备**。

**是不是已经装过**：下面这条能返回就说明 SSH 已通，直接 `./install.sh status` 看现状，不需要密码和数据线。
```sh
ssh -p 2222 -i ~/.ssh/id_ed25519 -o IdentitiesOnly=yes -o BatchMode=yes -o ConnectTimeout=6 \
  -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=~/.ssh/known_hosts_u60 root@192.168.0.1 true && echo SSH_OK
```
这里和后面所有 ssh 命令里的 `~/.ssh/id_ed25519` 是脚本的默认密钥。用 `SSH_KEY=` 换过密钥的话，
之后每次跑 `./install.sh`（包括 `status`、`reboot`）和手动 ssh 都要带同一把，不然会被当成 SSH 不通。

## 二、部署

**1. 拿到两个密码**

- 路由器管理密码（登录 `http://192.168.0.1` 用的）。只在开 ADB 时需要。
- 高级后台密码。可以和路由器密码一样；**不能含引号、反斜杠、空格**。

问用户用哪种方式给：
- **直接告诉你**：你用环境变量传给脚本（密码会留在这台电脑的对话记录里，先跟用户说明）。
- **自己填文件**：让用户 `cp u60.env.example u60.env` 后自己编辑填好。脚本会自动读取。
  这种方式下**不要去读 `u60.env` 的内容**，只确认文件存在。
  两种都给时，环境变量优先。

`AGENT_PASSWORD` 有三种状态，别弄混：
- **不设**：用 `ROUTER_PASSWORD` 的值；连 `ROUTER_PASSWORD` 也没给时，沿用设备上已有的后台密码。
- **设成空**（`AGENT_PASSWORD=''`）：沿用设备上已有的后台密码。
- **设成具体值**：用这个值。

**第一次装 `admin` 时最稳的做法是明确传 `AGENT_PASSWORD`。** ADB 已经开着的话，脚本不会再去网页登录，也就不需要 `ROUTER_PASSWORD`。
这时如果两个都没给、设备上又没装过后台，脚本会在推送前报「需要高级后台密码」。

网页登录连续输错会锁一段时间：**登录失败时不要换着密码重试**，把剩余次数告诉用户，让用户确认密码。

**2. 运行安装**（Bash 工具的 timeout 设成 600000）

```sh
ROUTER_PASSWORD='…' AGENT_PASSWORD='…' ./install.sh      # 用环境变量
./install.sh                                              # 用 u60.env
```

只装部分组件就把名字跟在后面，例如 `./install.sh ssh admin`。脚本可以反复跑，失败后修好原因重跑同一条命令即可。

输出前缀：`[*]` 进行中、`[+]` 成功、`[!]` 警告、`[-]` 致命错误（脚本退出码非 0）、
`[device] 失败:` 设备端出错（后面会接「设备端安装没有完成」）。

**3. 重启验证**（先问用户：会断网约 2 分钟）

```sh
./install.sh reboot        # timeout 设成 600000
```

它会重启设备、等 SSH 回来、打印各组件状态。
- 电脑没有自动重连 U60 的 Wi-Fi 时会超时，让用户手动连上后跑 `./install.sh status`。
- 如果这台电脑只靠 U60 上网，重启期间你也会断开，先跟用户说一声。
- 只有上一步输出里有「SSH 公钥登录验证通过」才做重启验证。SSH 没通时重启，ADB 也就没了。

**4. 收尾**

- 把脚本最后打印的 SSH 登录方式和后台地址告诉用户。
- 脚本会打印一段 `~/.ssh/config`，**问过用户再**写进去。
- 用了 `u60.env` 的话，建议用户装完删掉它。

## 三、装完以后怎么操作设备

```sh
ssh -p 2222 -i ~/.ssh/id_ed25519 -o UserKnownHostsFile=~/.ssh/known_hosts_u60 root@192.168.0.1 '<命令>'
adb shell '<命令>'        # 只有 ADB 通的时候（ADB 重启后就没了）
```

- 偶尔遇到连续连接后突然 `Permission denied`，先等 20～30 秒再试，别急着判断密钥坏了。自己查东西时尽量把多条命令合并到一次 ssh 里。
- 设备上没有 scp/sftp，传文件用 `ssh … 'cat > /path' < 本地文件` 或 tar 管道。
- 设备上的 `wget` 不支持 `--header`，要带请求头用 `/usr/bin/curl`；`/tmp` 是内存盘，别往里放大文件。

| 路径 | 用途 |
|---|---|
| `/data/ssh/` | dropbear、host key、`authorized_keys` 正本（开机同步到 `/etc/dropbear/`） |
| `/data/local/tmp/start_dropbear.sh` | SSH 的开机启动脚本 |
| `/data/luyaz/web.env` | 后台密码（`LUYAZ_WEB_PASSWORD=…`，600；luyaz-web 每次登录现读，命令行里只有路径） |
| `/data/luyaz/luyaz-sentinel`、`/etc/init.d/luyaz-sentinel` | 哨兵：触屏界面唯一的主人（拉起/接管、连续 2 次没稳住就交还原厂界面并告警、长按右下角 3 秒回来），兼 Wi-Fi 兜底看门狗 + 告警短信（原 u60-guard）。状态在 `/data/luyaz/screen/`，控制 FIFO `/tmp/luyaz-screen.ctl`（10-15 之前是 `/tmp/u60-uid.ctl`，已不再听）：`vendor` / `devui` / `hold` / `release`（只在它运行时用）；心跳 `/tmp/luyaz-sentinel.hb` 第 3 栏是屏幕上是谁（ours/vendor/none/held）。**屏幕上几分钟没有任何界面，固件会整机重启**：永远不要只停一个界面而不起另一个 |
| `/data/luyaz/scripts/`、`/etc/init.d/{luyaz-core,luyaz-sentinel,luyaz-web}` | 设备脚本（`doctor.sh`（`<core> doctor` 的薄壳）、`alert-lib.sh` 等；执行器 `luyaz-ship.sh`、`core-trial.sh` 也在这里；不再有 `u60-guard.sh`、`supervise.sh`）和 procd 监督（崩溃拉起）；**不要 `enable`/`disable`**（overlay whiteout），开机靠 rc.local 里的 `start` 行 |
| `/data/alerts/`、`/data/crashlog/` | 告警事件与短信记录、崩溃日志 |
| `/data/luyaz/luyaz-web`、`/data/luyaz/operator-logos/` | 高级后台；`/data/luyaz/core/` 是核心 luyaz-core 的状态（设置在 `core.env`），`/data/luyaz/screen/` 是哨兵的 |
| `/data/luyaz/ui/` | 触屏界面 `luyaz-ui`（进程名同）、字体、`touchsim`、`devui.conf`、`operator-logos/`、开机记录 `boot-trace.log`、原厂界面备份 `vendor-devui.orig-backup` |
| `/data/esim/` | lpac（`/data/esim/lpac.sh chip info` / `profile list`） |
| `/data/chill/`、`/etc/init.d/chill` | CHILL（`sh /data/chill/chill.sh status`）。改全屋网络：启动、停止、`CHILL_API_LAN`、防火墙区 `chill` 都**先问用户**。停它用 `chill.sh stop`（会把 dnsmasq 换回原样），别 kill mihomo |
| `/etc/rc.local` | 只加了几行自启；原厂版本备份在 `/data/luyaz/kit/rc.local.orig` |

日志：`/tmp/luyaz-web.log`、`/tmp/luyaz-ui.log`、`/tmp/luyaz-core.log`、`/tmp/luyaz-sentinel.log`、`logread`（procd）、
`/data/luyaz/ui/boot-trace.log`（哨兵每次启动写一段）。

## 四、常见失败

| 现象 | 处理 |
|---|---|
| 连不上 `http://192.168.0.1` | 电脑没连 U60 的 Wi-Fi / 代理 TUN 劫持（设直连，别关代理）/ 地址不同（用 GATEWAY） |
| 需要路由器管理密码 | 没传 `ROUTER_PASSWORD`，而 `u60.env` 不存在或里面这项留空（你不读那个文件，请用户自己检查）。回到「拿到两个密码」 |
| 需要高级后台密码 | 设备上没装过后台，又没给后台密码。传 `AGENT_PASSWORD` 重跑 |
| 登录失败、被锁 | 密码不对。别重试，问用户；被锁就等提示的秒数 |
| 开 ADB 被拒绝 | 固件 B28+，本包不适用，停止 |
| 90 秒等不到 ADB | 换数据线/USB 口，别用扩展坞；Windows 设备管理器里有黄色感叹号时装 Google USB Driver 并手动选「Android ADB Interface」。`adb devices` 出现 `device` 后重跑 |
| 多个 ADB 设备 / 「ADB 连着的设备不是 U60」 | 让用户拔掉手机等其他安卓设备（未授权、离线的也算）；确实要保留就设 `ANDROID_SERIAL` 指定 U60 |
| 文件损坏 / 缺文件 | 包不完整，让用户重新解压或重新要包。**不要改包里的文件**（有 sha256 校验） |
| `[device] 失败: …` | 按提示读对应日志；原因修好后重跑同一条命令 |
| SSH 验证失败但设备端成功 | 先别重启（ADB 还在，方便排查）。常见原因：电脑不在 U60 的 Wi-Fi 里；代理 TUN 劫持；`~/.ssh/id_ed25519` 带密码（脚本用 BatchMode，没加进 ssh-agent 就登不上，可以用 `SSH_KEY=` 指定一把无密码的密钥重跑 `./install.sh ssh`） |
| 屏幕没数据 | 看 `/tmp/luyaz-core.log`；重启一次通常就好 |
| 屏幕卡住/黑屏 | 退回原厂界面（见第六节第一段），再把日志给用户 |

脚本本身有 bug、或者这里没覆盖的情况：整理好完整输出，让用户发给给他装机包的人，不要自己改脚本硬装。

## 五、设备上的硬性规则

违反这些可能导致开不了机或变砖，而这台设备**没有公开的救砖工具**：

- **绝不**用 `/etc/init.d/<服务> disable` 关原厂服务。主守护进程 `zte_topsw_daemon` 要等下面这些全部就绪才放行开机，
  关掉任何一个都会卡在 ZTE logo、不拨号、触屏无响应：
  `zte_topsw_mc zte_router zte_topsw_data zte_topsw_nwinfo zte_topsw_mdm zte_topsw_sleep_faw zte_topsw_apn zte_topsw_wms zte_topsw_key zte_topsw_led zte_topsw_tr098db zte_dm zte_topsw_fota_result zte_topsw_devui zte_topsw_wlan zte_smart_manage`。
  开机后可以用 `ubus call zwrt_topsw_daemon.sync get_sync_info '{}'` 检查，应为 `"noSyncModuleName": "sync success"`。
- 不写分区、不 `dd` 块设备、不用 `abctl` 切 A/B 槽、不刷固件、不点系统升级。
- 不恢复出厂设置，除非用户明确要求并知道后果（会丢掉本包装的全部东西和开机自启）。
- 手改 `/etc/rc.local` 前先备份，改完 `sh -n /etc/rc.local` 检查语法。
- `kill -9` 原厂进程可能被 procd 立刻拉起，停原厂服务用 `/etc/init.d/<名字> stop`（stop 可以，disable 不行）。

**先问用户再做**：重启、切换/启用/删除 eSIM profile（切到没流量的 profile，远程连着的话就断了）、
改 APN/网络模式/锁频、改 Wi-Fi、恢复原厂、改 `~/.ssh/config`、给电脑装软件。

**可以直接做**：`./install.sh status`、`./install.sh doctor`（只读体检）、`./install.sh backup`（配置备份到电脑，旁边另存 `config-…system.tgz`：rc.local、我们的 init.d 脚本、dropbear host key、authorized_keys、start_dropbear.sh，供升固件前参考，`restore` 不恢复它）、读日志和配置、`lpac.sh chip info` / `profile list` 这类只读查询。
`./install.sh restore <备份>` 会改设备配置：先问用户，它自己也会先列出改动、要输入 yes。

## 六、恢复

**只是这次开机换成原厂界面**：`echo vendor > /tmp/luyaz-screen.ctl`（哨兵停掉触屏、起原厂界面；长按右下角 3 秒或
`echo devui > /tmp/luyaz-screen.ctl` 回来，重启后也会回来）。

**永久退回原厂屏幕界面**（保留其它组件）：

```sh
mkdir -p /data/luyaz/kit && cp /etc/rc.local /data/luyaz/kit/rc.local.before-revert
grep -v -e '/etc/init.d/luyaz-sentinel start' /data/luyaz/kit/rc.local.before-revert > /tmp/rc.new \
  && sh -n /tmp/rc.new && cat /tmp/rc.new > /etc/rc.local
echo vendor > /tmp/luyaz-screen.ctl   # 由哨兵换界面：它在同一步里起原厂界面，屏幕不会空着
sleep 8; /etc/init.d/luyaz-sentinel stop  # 之后不再管屏幕（原厂界面服务一直是 enable 的，开机自己起）
# 注意：哨兵也是 Wi-Fi 兜底看门狗和告警短信，停掉它这两样也一起停了
```

**全部恢复原厂**（先确认用户要这么做）：

```sh
echo vendor > /tmp/luyaz-screen.ctl; sleep 8     # 先把屏幕交给原厂界面
[ -x /data/chill/chill.sh ] && /data/chill/chill.sh stop; rm -f /etc/init.d/chill   # CHILL：先停，它会把 DNS 设置换回原样
for s in luyaz-sentinel luyaz-web luyaz-core u60-uid u60-guard zte-agent zwrt-datad; do [ -x /etc/init.d/$s ] && /etc/init.d/$s stop; rm -f /etc/init.d/$s; done
for f in /data/luyaz/kit/rc.local.orig /data/u60-kit/rc.local.orig; do [ -s $f ] && cp $f /etc/rc.local && break; done
rm -rf /data/luyaz /data/plugins/u60pro-devui /data/esim /data/chill /data/alerts /data/crashlog /data/power \
       /data/u60d /data/u60-kit /data/u60-ship /data/u60-guard /data/u60-uid /data/zte-agent /data/zte-agent.env /data/admin \
       /data/plugins/zwrt-datad /data/local/tmp/start_zte_agent.sh    # 第二行起是旧装机包的残留
# 连 SSH 也不要：rm -rf /data/ssh /data/local/tmp/start_dropbear.sh
reboot
```

原厂的卡 logo / 不拨号急救（ADB 或 SSH 里执行）：

```sh
sh /usr/bin/mtdev2tuio.sh; kill -9 $(pidof zte_topsw_devui); /usr/bin/zte_topsw_devui &   # 屏幕/触摸没起来
ubus call zwrt_qcmap_cli set_qcliiface '{"source_module":"zte_topsw_data","type":1,"enable":1,"sub_id":1}'   # IPv4 没拨号
```
