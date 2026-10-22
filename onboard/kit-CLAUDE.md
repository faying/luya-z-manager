# Luya-Z 装机包：Claude 操作手册

这个目录是 **Luya-Z 装机包**，目标设备是 ZTE U60 Pro（MU5250）。设备主人请你帮忙装机或排错时，照这份手册做。
`README.md` 是写给人看的同一套内容；这份更侧重操作步骤、判断条件和出错时的处理。

## 零、绝对不能做的事（先读）

这台设备**没有公开的救砖工具**，下面任何一条做错都可能开不了机：

- **固件自动升级必须一直关着。** 关着的样子：`zwrt_zte_dm.dm_update.dm_update_mode` 为 `0`，`zwrt_zte_dm.dm_update.TURNOFFPOLLING` 为 `1`。
  任何脚本、页面、自动化都不许把它打开。装 `ssh` 组件时脚本会把两项都设好（只往关的方向写）并读回核对——
  因为升级会覆盖开机自启，新固件又开不了 ADB。
- **不许 `/etc/init.d/<服务> disable` 原厂服务。** 主守护进程 `zte_topsw_daemon` 要等下面这些全部就绪才放行开机，
  关掉任何一个，设备就停在 ZTE logo、不拨号、触屏没反应：
  `zte_topsw_mc zte_router zte_topsw_data zte_topsw_nwinfo zte_topsw_mdm zte_topsw_sleep_faw zte_topsw_apn zte_topsw_wms zte_topsw_key zte_topsw_led zte_topsw_tr098db zte_dm zte_topsw_fota_result zte_topsw_devui zte_topsw_wlan zte_smart_manage`。
  开机后用 `ubus call zwrt_topsw_daemon.sync get_sync_info '{}'` 检查，正常是 `"noSyncModuleName": "sync success"`。
  要停原厂服务用 `/etc/init.d/<名字> stop`（stop 可以，disable 不行）；`kill -9` 原厂进程可能马上被 procd 拉起来。
- 不写分区、不对块设备 `dd`、不用 `abctl` 切 A/B 槽、不刷固件、不点系统升级。
- 不恢复出厂设置，除非用户明确要求并且知道后果（本包装的东西和开机自启全部丢失）。
- 手动改 `/etc/rc.local` 之前先备份，改完跑 `sh -n /etc/rc.local` 查语法。
- **屏幕上几分钟没有任何界面，固件会整机重启**：永远不要只停掉一个界面而不起另一个，换界面交给哨兵（见第六节）。

**必须先问用户**：重启；切换/启用/删除 eSIM profile（切到没流量的 profile，远程连着就断了）；改 APN、网络模式、锁频；
改 Wi-Fi；恢复原厂；改 `~/.ssh/config`；给电脑装软件；`./install.sh restore`；装 `recover`（会改 rc.local）；启动或停止 CHILL。

**不用问，可以直接做**：`./install.sh status`、`./install.sh doctor`（只读体检）、`./install.sh backup`、读日志和配置、
`lpac.sh chip info` / `profile list` 这类只读查询。

## 一、组件

| 组件 | 装上什么 |
|---|---|
| `ssh` | dropbear，端口 2222，只认密钥，开机自启。脚本先起 dropbear，10 秒内确认进程在、2222 在监听，才往 rc.local 写那一行；没起来就不碰 rc.local。电脑端 SSH 检查失败时会分清是：没有密钥 / 连接被拒 / 认证失败 / 超时。顺带关闭固件自动升级（见第零节） |
| `admin` | 只有高级后台 = 管理网页 luyaz-web，地址 `http://<设备>:9090/`。它的数据和改动都经过核心 luyaz-core，核心随 `ui` 装 |
| `ui` | 前面板触屏界面 + 核心 luyaz-core（`/data/luyaz/luyaz-core`，procd 服务 `/etc/init.d/luyaz-core`）+ 哨兵 luyaz-sentinel（管屏幕和看门狗）。rc.local 里 `/etc/init.d/luyaz-core start` 之后只留一行 `/etc/init.d/luyaz-sentinel start   # u60 ship sentinel: …`。起哨兵后最多等 60 秒，核对三点：哨兵在 procd 下、心跳第 3 栏是 `ours`、界面进程的 exe md5 等于刚装的文件 |
| `esim` | lpac，配可插拔 eUICC 卡管理和切换 profile |
| `recover`（可选，不在全套里） | u60 ship 的开机收尾：设备上自检通过后放上 `/data/luyaz/ship/luyaz-recover.sh`，并在 `/etc/rc.local` 的各服务启动行之前加一行调用它。**会改 rc.local，先问用户**。`admin`/`ui` 只在设备上还没有这个脚本时放一份，不碰 rc.local |
| `chill`（可选，不在全套里） | CHILL 透明代理：mihomo + 规则集 + zashboard 面板，装在 `/data/chill`。第一次装**不启动**：订阅地址要用户自己写进 `/data/chill/chill.env`（你别替他填、也别读出来），然后 `chill.sh safe-start`，5 分钟内 `confirm` |

- 不带参数的 `./install.sh` = `ssh admin ui esim`；`recover`、`chill` 要单独点名。
- `admin` 在设备清单里记 lweb 一条；`ui` 记 touch、sentinel、core 三条。
- 只装现在的布局：设备上还有 `/data/u60d`、`/data/u60-uid`、`/data/u60-ship`、`/data/plugins/u60pro-devui`、`/data/plugins/zwrt-datad` 任何一个时拒装，要先用旧版装机包升级。

## 二、动手前检查（不满足就停下来告诉用户）

1. **电脑上的工具**：`command -v adb ssh ssh-keygen curl tar`。
   缺 adb 就告诉用户怎么装：macOS `brew install android-platform-tools`；Linux `sudo apt install adb`；
   Windows 下载 Google SDK Platform-Tools 并加进 PATH。要替用户装软件，先征得同意。
2. **电脑经 Wi-Fi 连着 U60**，设备默认地址 `192.168.0.1`。不能只靠 USB 网络共享：开 ADB 后 USB 口进调试模式，那条网就断了。
   用匿名接口探一下：
   ```sh
   curl -s -m 5 --noproxy '*' http://192.168.0.1/ubus/ -H 'Content-Type: application/json' \
     -H 'Referer: http://192.168.0.1/' -H 'Origin: http://192.168.0.1' \
     -d '[{"jsonrpc":"2.0","id":1,"method":"call","params":["00000000000000000000000000000000","zwrt_web","web_login_info",{}]}]'
   ```
   - 返回里有 `zte_web_sault` = 通了。`login_fail_num` 是网页密码还能试几次；`login_fail_lock_lefttime` 大于 0 表示正在锁定。
   - 不通：请用户确认连的是 U60 的 Wi-Fi。**不要让用户关代理**（你自己可能就靠它上网）；
     代理开着 TUN/增强模式（Surge、Clash 等）时，请用户把 `192.168.0.0/16` 设成直连。
   - 地址不是 192.168.0.1：后面每条命令前面加 `GATEWAY=<地址>`。
3. **固件**：只支持 CN 固件 **B27 及更早**。用户说不清也没关系，脚本登录后会打印版本；
   B28 以后会停在「开 ADB 被拒绝」，那就**到此为止**，不要尝试任何别的解锁办法。
4. **数据线**：USB-C 线已经把 U60 接到这台电脑（开 ADB 后脚本最多等 90 秒），而且电脑上**没有接别的安卓设备**。

**判断是不是已经装过**：下面这条成功，说明 SSH 已经通了，直接 `./install.sh status` 看现状，不需要密码和数据线。

```sh
ssh -p 2222 -i ~/.ssh/id_ed25519 -o IdentitiesOnly=yes -o BatchMode=yes -o ConnectTimeout=6 \
  -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=~/.ssh/known_hosts_u60 root@192.168.0.1 true && echo SSH_OK
```

`~/.ssh/id_ed25519` 是脚本默认用的密钥（本手册所有 ssh 命令都按它写）。如果用 `SSH_KEY=` 换了别的密钥，
以后每次跑 `./install.sh`（含 `status`、`reboot`）和手动 ssh 都得带同一把，否则会被当成 SSH 不通。

## 三、安装流程

### 1. 密码

需要两个：

- **路由器管理密码**（登录 `http://192.168.0.1` 用的），只有开 ADB 时才用到。
- **高级后台密码**，可以和路由器密码相同；**不能有引号、反斜杠、空格**。

让用户选一种给法：

- **直接告诉你**：你通过环境变量传给脚本。先提醒用户：密码会留在这台电脑的对话记录里。
- **自己写文件**：请用户 `cp u60.env.example u60.env` 然后自己编辑。脚本启动时自动读取。
  这种情况下**你不要读 `u60.env` 的内容**，只确认文件存在。

两种都有时，环境变量优先。

`AGENT_PASSWORD` 分三种情况，注意区分：

| 写法 | 结果 |
|---|---|
| 不设 | 用 `ROUTER_PASSWORD` 的值；`ROUTER_PASSWORD` 也没给时，沿用设备上已有的后台密码 |
| 设成空（`AGENT_PASSWORD=''`） | 沿用设备上已有的后台密码 |
| 设成具体值 | 就用这个值 |

**第一次装 `admin` 最好明确给 `AGENT_PASSWORD`。** 如果 ADB 已经开着，脚本不会再去网页登录，也就不需要 `ROUTER_PASSWORD`；
这时两个都没给、设备上又从没装过后台，脚本会在推送前报「需要高级后台密码」。

网页登录连续输错会被锁一段时间。**登录失败时不要换着密码再试**：把剩余次数告诉用户，请用户核对密码。

### 2. 运行

Bash 工具的 timeout 设成 600000。

```sh
ROUTER_PASSWORD='…' AGENT_PASSWORD='…' ./install.sh      # 密码走环境变量
./install.sh                                              # 密码在 u60.env 里
```

只装部分组件就把名字写在后面，例如 `./install.sh ssh admin`。脚本可以重复跑：失败了，修好原因后重跑同一条命令。

看输出前缀判断进度：

- `[*]` 进行中，`[+]` 成功，`[!]` 警告
- `[-]` 致命错误，脚本以非 0 退出
- `[device] 失败:` 设备端出错，后面会跟「设备端安装没有完成」

### 3. 重启验证（先问用户：会断网约 2 分钟）

```sh
./install.sh reboot        # timeout 同样设成 600000
```

它会重启设备、等 SSH 回来、打印各组件状态。

- 只有上一步输出里出现「SSH 公钥登录验证通过」才做这一步。SSH 没通就重启，ADB 也会跟着没了。
- 没终端时脚本不会自己重启，会提示「没有重启验证」，要重启就单独跑上面这条。
- 电脑没自动重连 U60 的 Wi-Fi 会导致超时：请用户手动连上，再跑 `./install.sh status`。
- 如果这台电脑只靠 U60 上网，重启期间你也会断线，事先跟用户说。

### 4. 收尾

- 把脚本最后打印的 SSH 登录方式和后台地址转告用户。
- 脚本会打印一段 `~/.ssh/config`，**问过用户才**写进去。
- 用了 `u60.env` 的，建议用户装完删掉。

## 四、装好之后怎么操作设备

```sh
ssh -p 2222 -i ~/.ssh/id_ed25519 -o UserKnownHostsFile=~/.ssh/known_hosts_u60 root@192.168.0.1 '<命令>'
adb shell '<命令>'        # 仅限 ADB 还通的时候（重启后 ADB 就没了）
```

- 连续连接后偶尔会突然 `Permission denied`：等 20～30 秒再试，别急着断定密钥坏了。多条查询尽量合并成一次 ssh。
- 设备上没有 scp/sftp：传文件用 `ssh … 'cat > /path' < 本地文件` 或 tar 管道。
- 设备上的 `wget` 不支持 `--header`，要带请求头就用 `/usr/bin/curl`。`/tmp` 是内存盘，别放大文件。

| 路径 | 是什么 |
|---|---|
| `/data/ssh/` | dropbear、host key、`authorized_keys` 正本（开机同步到 `/etc/dropbear/`） |
| `/data/local/tmp/start_dropbear.sh` | SSH 的开机启动脚本 |
| `/data/luyaz/web.env` | 后台密码（`LUYAZ_WEB_PASSWORD=…`，权限 600；luyaz-web 每次登录现读，命令行里只出现路径） |
| `/data/luyaz/luyaz-sentinel`、`/etc/init.d/luyaz-sentinel` | 哨兵，触屏界面唯一的主人：负责拉起/接管界面；连续 2 次没稳住就交还原厂界面并告警；长按右下角 3 秒回来。兼任 Wi-Fi 兜底看门狗和告警短信。状态在 `/data/luyaz/screen/`；控制 FIFO `/tmp/luyaz-screen.ctl` 接受 `vendor` / `ui` / `hold` / `release`（只在哨兵运行时用；10-15 之前的 `/tmp/u60-uid.ctl` 已经不听了）；心跳 `/tmp/luyaz-sentinel.hb` 第 3 栏表示屏幕上是谁（ours/vendor/none/held） |
| `/data/luyaz/scripts/`、`/etc/init.d/{luyaz-core,luyaz-sentinel,luyaz-web}` | 设备脚本：`doctor.sh`（`<core> doctor` 的薄壳）、`alert-lib.sh` 等，执行器 `luyaz-ship.sh`、`core-trial.sh` 也在这里（已经没有 `u60-guard.sh`、`supervise.sh`）。init 脚本是 procd 监督（崩了拉起）；**别 `enable`/`disable`**（会在 overlay 里留 whiteout），开机靠 rc.local 里的 `start` 行 |
| `/data/alerts/`、`/data/crashlog/` | 告警事件和短信记录、崩溃日志 |
| `/data/luyaz/luyaz-web`、`/data/luyaz/operator-logos/` | 高级后台。`/data/luyaz/core/` 是核心 luyaz-core 的状态（设置在 `core.env`），`/data/luyaz/screen/` 是哨兵的 |
| `/data/luyaz/ui/` | 触屏界面 `luyaz-ui`（进程名相同）、字体、`touchsim`、`ui.conf`、`operator-logos/`、开机记录 `boot-trace.log`、原厂界面备份 `vendor-devui.orig-backup` |
| `/data/esim/` | lpac（`/data/esim/lpac.sh chip info` / `profile list`） |
| `/data/chill/`、`/etc/init.d/chill` | CHILL（`sh /data/chill/chill.sh status`）。它改的是全屋网络：启动、停止、`CHILL_API_LAN`、防火墙区 `chill` 都**先问用户**。要停就用 `chill.sh stop`（会把 dnsmasq 恢复原样），别 kill mihomo |
| `/etc/rc.local` | 只加了几行自启；原厂版本备份在 `/data/luyaz/kit/rc.local.orig` |

日志：`/tmp/luyaz-web.log`、`/tmp/luyaz-ui.log`、`/tmp/luyaz-core.log`、`/tmp/luyaz-sentinel.log`、`logread`（procd），
以及 `/data/luyaz/ui/boot-trace.log`（哨兵每次启动写一段）。

`./install.sh backup` 把配置备份到电脑，旁边另存 `config-…system.tgz`（rc.local、我们的 init.d 脚本、dropbear host key、
authorized_keys、start_dropbear.sh），供升固件前参考，`restore` 不恢复它。
`./install.sh restore <备份>` 会改设备配置：先问用户；它自己也会先列出要改的文件、要求输入 yes。

## 五、出错了怎么办

| 现象 | 处理 |
|---|---|
| 连不上 `http://192.168.0.1` | 电脑没连 U60 的 Wi-Fi；或代理 TUN 劫持（设直连，别关代理）；或地址不同（用 GATEWAY） |
| 需要路由器管理密码 | 没传 `ROUTER_PASSWORD`，`u60.env` 又不存在或这一项是空的（你不读那个文件，请用户自己看）。回到「三、1. 密码」 |
| 需要高级后台密码 | 设备上没装过后台，又没给后台密码。传 `AGENT_PASSWORD` 重跑 |
| 登录失败、被锁 | 密码不对。别重试，问用户；被锁就等提示的秒数 |
| 开 ADB 被拒绝 | 固件是 B28 以后，本包不适用，停止 |
| 90 秒等不到 ADB | 换数据线或 USB 口，别用扩展坞；Windows 设备管理器里有黄色感叹号时装 Google USB Driver 并手动选「Android ADB Interface」。`adb devices` 出现 `device` 后重跑 |
| 多个 ADB 设备 / 「ADB 连着的设备不是 U60」 | 请用户拔掉手机等其他安卓设备（未授权、离线的也算）；确实要留着就设 `ANDROID_SERIAL` 指定 U60 |
| 文件损坏 / 缺文件 | 包不完整：请用户重新解压或重新要包。**不要改包里的文件**（有 sha256 校验） |
| `[device] 失败: …` | 按提示去看对应日志；修好原因后重跑同一条命令 |
| SSH 验证失败、但设备端成功 | 先别重启（ADB 还在，方便查）。常见原因：电脑不在 U60 的 Wi-Fi 里；代理 TUN 劫持；`~/.ssh/id_ed25519` 有密码短语（脚本用 BatchMode，没加进 ssh-agent 就登不上，可以 `SSH_KEY=` 指定一把无密码的密钥重跑 `./install.sh ssh`） |
| 屏幕没数据 | 看 `/tmp/luyaz-core.log`；通常重启一次就好 |
| 屏幕卡住、黑屏 | 先退回原厂界面（第六节第一条），再把日志给用户 |

脚本本身有 bug，或遇到这里没写到的情况：把完整输出整理好，请用户发给给他装机包的人。**不要自己改脚本硬装。**

## 六、退回与恢复

**只在这次开机换回原厂界面**：`echo vendor > /tmp/luyaz-screen.ctl`。哨兵会停掉触屏界面、起原厂界面；
长按右下角 3 秒或 `echo ui > /tmp/luyaz-screen.ctl` 换回来，重启后也会自动回来。

**永久用原厂屏幕界面**（其他组件保留）：

```sh
mkdir -p /data/luyaz/kit && cp /etc/rc.local /data/luyaz/kit/rc.local.before-revert
grep -v -e '/etc/init.d/luyaz-sentinel start' /data/luyaz/kit/rc.local.before-revert > /tmp/rc.new \
  && sh -n /tmp/rc.new && cat /tmp/rc.new > /etc/rc.local
echo vendor > /tmp/luyaz-screen.ctl   # 让哨兵换界面：它会同时起原厂界面，屏幕不会空着
sleep 8; /etc/init.d/luyaz-sentinel stop  # 从此不再管屏幕（原厂界面服务一直是 enable 的，开机自己会起）
# 注意：哨兵同时是 Wi-Fi 兜底看门狗和告警短信，停掉它这两样也停了
```

**全部恢复原厂**（先确认用户真要这样）：

```sh
echo vendor > /tmp/luyaz-screen.ctl; sleep 8     # 先把屏幕交给原厂界面
[ -x /data/chill/chill.sh ] && /data/chill/chill.sh stop; rm -f /etc/init.d/chill   # 先停 CHILL，它会把 DNS 设置恢复原样
for s in luyaz-sentinel luyaz-web luyaz-core u60-uid u60-guard zte-agent zwrt-datad; do [ -x /etc/init.d/$s ] && /etc/init.d/$s stop; rm -f /etc/init.d/$s; done
for f in /data/luyaz/kit/rc.local.orig /data/u60-kit/rc.local.orig; do [ -s $f ] && cp $f /etc/rc.local && break; done
rm -rf /data/luyaz /data/plugins/u60pro-devui /data/esim /data/chill /data/alerts /data/crashlog /data/power \
       /data/u60d /data/u60-kit /data/u60-ship /data/u60-guard /data/u60-uid /data/zte-agent /data/zte-agent.env /data/admin \
       /data/plugins/zwrt-datad /data/local/tmp/start_zte_agent.sh    # 第二行起是旧版装机包的残留
# SSH 也不要的话：rm -rf /data/ssh /data/local/tmp/start_dropbear.sh
reboot
```

**原厂系统卡 logo / 不拨号的急救**（在 ADB 或 SSH 里执行）：

```sh
sh /usr/bin/mtdev2tuio.sh; kill -9 $(pidof zte_topsw_devui); /usr/bin/zte_topsw_devui &   # 屏幕/触摸没起来
ubus call zwrt_qcmap_cli set_qcliiface '{"source_module":"zte_topsw_data","type":1,"enable":1,"sub_id":1}'   # IPv4 没拨号
```
