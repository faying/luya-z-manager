# Luya-Z 装机包（ZTE U60 Pro / MU5250）

[English](README.md) · **中文**

给**刚到手的 ZTE U60 Pro（MU5250）**一键装上：

| 组件 | 装完是什么样 |
|---|---|
| **SSH** | `ssh -p 2222 root@192.168.0.1` 用你电脑上的密钥直接登录（只认密钥、不接受密码），重启后自动恢复 |
| **高级后台** | 浏览器打开 `http://192.168.0.1:9090/`，锁频/锁小区、短信、APN、防火墙、Wi-Fi 等全在里面 |
| **devui 触屏界面** | 前面板换成新界面：按任务分 5 个标签（首页 · 蜂窝 · Wi-Fi · 出口 · 系统）：信号结论与载波、Wi-Fi 与设备、CHILL/Tailscale 出口、eSIM 切换 |
| **eSIM** | 配合可插拔 eUICC 卡（5ber、eSTK.me 这类），在后台下载/切换/删除 profile，屏幕上也能切 |
| **CHILL**（可选，要单独点名） | 设备上的透明代理（mihomo）+ zashboard 面板，按规则分流；要你自己的订阅地址 |

装 SSH 时会顺手**关掉固件自动升级**（`zwrt_zte_dm.dm_update.dm_update_mode=0` 和 `TURNOFFPOLLING=1` 两项都设，并读回核对；原因见「注意事项」）。
它先起 dropbear、确认 2222 在监听，才在 `rc.local` 加那一行；没起来 `rc.local` 不动。

## 装之前确认

1. **固件版本 B27 或更早**（网页后台首页或「设备信息」里，形如 `…MU5250V1.0.0B27`）。
   新固件删掉了开 ADB 的接口（`zwrt_bsp.usb set`：国行 B31 实测没有，据称 B28 起就删了），这个包就用不了了，**千万别先升级**。
2. 知道**路由器管理密码**（登录 `http://192.168.0.1` 用的那个）。
3. 一根**能传数据的 USB-C 线**（有些充电线只能充电）。
4. 电脑装好 **adb**：
   - macOS：`brew install android-platform-tools`
   - Linux：`sudo apt install adb`
   - Windows：下载 Google 的 [SDK Platform-Tools](https://developer.android.com/tools/releases/platform-tools)，解压后把目录加进 PATH；
     再装 [Git for Windows](https://git-scm.com/download/win)，下面的命令在 **Git Bash** 里跑
5. 要用 eSIM 的话，准备一张可插拔 eUICC 卡。插普通 SIM 也能装，只是 eSIM 功能读不到卡。

## 安装

```sh
tar xzf luyaz-kit-*.tar.gz
cd luyaz-kit
./install.sh
```

照提示做就行：

1. 电脑连上 U60 的 **Wi-Fi**（别只靠 USB 网络共享，开调试后那条网会断），输入路由器管理密码 → 脚本通过网页接口打开 USB 调试
2. 提示「等 ADB 设备出现」时，用 USB-C 线把 U60 接到电脑
3. 设一个**高级后台登录密码**（直接回车就和路由器管理密码一样；不能含引号、反斜杠、空格）
4. 自动推送、安装、验证，全程 1～2 分钟，屏幕会闪一下换成新界面
5. 问要不要**重启验证**时建议选是：重启后脚本会确认 SSH、后台、屏幕都自己起来了

装完会打印一段 `~/.ssh/config`，加进去以后 `ssh u60` 就能登录。

### 让 Claude Code 帮你装

包里带了一份 `CLAUDE.md`，写着部署步骤、报错怎么处理和设备上不能碰的东西。在包目录里打开 Claude Code 就会自动读到：

```sh
cd luyaz-kit
claude
```

然后说「帮我把 U60 装好」。它会先检查 adb、网络和固件，再问你要密码——不想告诉它的话，自己 `cp u60.env.example u60.env` 填好，跟它说「密码在 u60.env 里」就行。插 USB 线、确认要不要重启这类事它会停下来问你。

只想要其中几样也可以：

```sh
./install.sh ssh               # 只开 ADB + 持久化 SSH
./install.sh ssh admin devui   # 不要 eSIM
./install.sh chill             # 加装 CHILL 代理（不在全套里；第一次装不会启动，见下面「CHILL」）
./install.sh recover           # u60 ship 的开机收尾：放好 /data/luyaz/ship/luyaz-recover.sh（先在设备上自检），并在 rc.local
                               #   各服务启动行之前加一行调用它。不在全套里：确定要改 rc.local 时才单独跑
./install.sh status            # 看各组件状态
./install.sh doctor            # 只读体检：开机同步、自动升级、各服务、心跳、Wi-Fi、告警……逐项 ●▲■
./install.sh backup            # 把设备配置备份到这台电脑（./backups/，含密码，别外传）；
                               # 另存 config-…system.tgz（rc.local、我们的 init.d 脚本、dropbear host key、authorized_keys、
                               # start_dropbear.sh），供升固件前参考，restore 不恢复它
./install.sh restore 备份.tgz  # 先列出会改哪些文件，输入 yes 才写
./install.sh reboot            # 重启一次，确认各组件开机自己起来
```

以后重装或更新某个组件，拿到新的装机包后直接 `./install.sh admin`（或 `devui` / `esim`），会走 SSH，不用再插线。

U60 地址不是 `192.168.0.1` 时：`GATEWAY=192.168.x.1 ./install.sh`。

## 装了什么、在哪

程序和数据都在 `/data`（固件升级也不会清）。开机自启只在 `/etc/rc.local` 里加了几行，没有改任何原厂服务。
核心 luyaz-core、高级后台 luyaz-web 和哨兵 luyaz-sentinel 由系统的 procd 监督：崩溃会被自动拉起，并记一条告警（后台「系统 → 告警」可以设短信通知）。
`admin`、`devui` 装完会把装了什么（装机包日期、提交号）写进设备清单 `/data/luyaz/ship/manifest.jsonl`，每个组件一行（lweb；touch、sentinel、datad = 核心），`doctor` 拿它和文件比对。
`devui` 装触屏界面、luyaz-core 和 luyaz-sentinel。`rc.local` 里 `/etc/init.d/luyaz-core start` 后面跟一行 `/etc/init.d/luyaz-sentinel start`；旧装机包留下的行（`zwrt-datad`、start.sh、`u60-uid`、`u60-guard`）会被换掉，这些服务也会停掉（不会 disable）。起哨兵后最多等 60 秒，确认它在 procd 下运行、心跳写着 `ours`、跑着的触屏程序就是刚装的那个。`admin` 只装 luyaz-web。
装机包直接装成现在的布局。还是旧布局的设备（有 `/data/u60d`、`/data/u60-uid` 或 `/data/u60-ship` 但没有新的 `.migrated` 标记，或者只有 `/data/u60-kit` 没有 `/data/luyaz/kit`）会被拒装，并提示先搬什么。

| 路径 | 用途 |
|---|---|
| `/data/ssh/` | dropbear、host key、`authorized_keys`（**公钥正本**） |
| `/data/local/tmp/start_dropbear.sh` | 开机时把公钥同步到 `/etc/dropbear/` 并启动 dropbear（:2222） |
| `/data/luyaz/luyaz-core`、`/data/luyaz/core/` | 核心（:9460，只听本机）：触屏和后台的数据、所有改动都经过它。状态在 `/data/luyaz/core/`，日志 `/tmp/luyaz-core.log` |
| `/data/luyaz/luyaz-web`、`/data/luyaz/operator-logos/` | 高级后台（luyaz-web，:9090）。zte-agent 2026-10-14 起已删：装机包会停掉旧的、去掉它在 `rc.local` 里那一行 |
| `/data/luyaz/web.env` | **后台密码**（只有 root 可读；旧装机包放在 `/data/zte-agent.env`） |
| `/data/luyaz/luyaz-sentinel`、`/data/luyaz/screen/` | 哨兵（取代 u60-uid 和 u60-guard）：管屏幕，触屏程序崩了拉起，连续两次起不来就交还原厂界面并告警（长按右下角 3 秒回来）；也负责 Wi-Fi 兜底看门狗和告警短信。控制 FIFO `/tmp/luyaz-screen.ctl`，日志 `/tmp/luyaz-sentinel.log`，心跳 `/tmp/luyaz-sentinel.hb` |
| `/data/luyaz/ui/` | 触屏程序 `luyaz-ui`、字体和运营商 logo。`/data/plugins/u60pro-devui/` 只剩 `start.sh` 和备份 |
| `/data/luyaz/scripts/`、`/etc/init.d/{luyaz-core,luyaz-sentinel,luyaz-web}` | 设备端脚本（doctor.sh，是 `luyaz-core doctor` 的薄壳；alert-lib.sh 等；上机执行器 luyaz-ship.sh、datad-trial.sh 也在这）和进程监督（开机由 `rc.local` 里的 `/etc/init.d/… start` 行启动，不走 `enable`） |
| `/data/luyaz/ship/` | `u60 ship` 的事务、清单 `manifest.jsonl`、开机收尾脚本 `luyaz-recover.sh` |
| `/data/alerts/`、`/data/crashlog/` | 告警记录、程序崩溃时的日志 |
| `/data/esim/` | lpac（eSIM 卡读写工具） |
| `/data/chill/`、`/etc/init.d/chill` | CHILL：mihomo、规则集、zashboard、`chill.sh`；订阅地址在 `/data/chill/chill.env`（你自己写，只有 root 可读） |
| `/data/luyaz/kit/rc.local.orig` | 第一次安装前原厂 `rc.local` 的备份 |

旧装机包留下的 `/data/zte-agent*`、`/data/admin/`、`/data/plugins/zwrt-datad/`、`/data/u60-guard/`、`/data/u60-uid/` 已经没人用，可以删。

## 日常用法

- **加一台电脑的 SSH 公钥**：把公钥追加到 `/data/ssh/authorized_keys`，然后 `sh /data/local/tmp/start_dropbear.sh`（改 `/etc/dropbear/` 里那份没用，开机会被覆盖）。
- **改后台密码**：重跑 `./install.sh admin`（会问新密码）。或者 SSH 里改 `/data/luyaz/web.env` 那一行（不能含引号、反斜杠、空格），然后在 `:9090` 重新登录（luyaz-web 每次登录都现读这个文件，不用重启）。
- **eSIM**：后台「移动网络 → eSIM」管理 profile；屏幕「更多功能 → eSIM」点两下切换，大约 10 秒生效，一般不用重启。
- **CHILL**：第一次 `./install.sh chill` 只装不启动。SSH 进去把订阅地址写进 `/data/chill/chill.env`（从同目录的 `chill.env.example` 复制改，`chmod 600`；静态节点快照放 `/data/chill/providers/`），
  然后 `sh /data/chill/chill.sh safe-start`，确认全屋网络正常后 5 分钟内 `sh /data/chill/chill.sh confirm`（不确认会自动停掉，免得把自己锁在外面）。
  之后在后台「服务 → CHILL」开关、换出口；面板 zashboard 从那一页的按钮进（后台自己在 `:9090/chill-ui/` 出面板，要先登录后台，Tailscale 下也能用）。
  以后再跑 `./install.sh chill` 是更新：订阅地址和缓存不动，原来在跑就重启核心，起不来自动换回旧版。
- **临时要 ADB**：SSH 进去跑 `ubus call zwrt_bsp.usb set '{"mode":"debug"}'`；`./install.sh status` 里的「USB 模式」显示 `user` 就是普通模式。只在 `ubus -v list zwrt_bsp.usb` 还列着 `set` 的固件上有用：国行 B31 只剩 `list`，这条会报 `Method not found`。

## 注意事项

- **别升级固件**。升级会覆盖 `rc.local`（SSH、后台、屏幕全部不再自启），而新固件又开不了 ADB，回不去了。装 SSH 时已经关了自动升级；手机 App 或网页提示升级也别点。
- **别用 `/etc/init.d/<服务> disable` 关原厂服务**。U60 的主守护进程要等一串服务全部就绪才放行开机，关掉其中一个会导致屏幕卡在 ZTE logo、不拨号。想精简服务先问清楚。
- **eSIM 别切到没流量的 profile 再远程操作**。如果你是通过这台 U60 自己的网络远程连进来的，切过去就断了，只能到设备跟前在屏幕上切回来。
- 这是非官方改装，风险自负。

## 恢复原厂

SSH 登录后：

```sh
echo vendor > /tmp/luyaz-screen.ctl; sleep 8     # 先把屏幕交给原厂界面
[ -x /data/chill/chill.sh ] && /data/chill/chill.sh stop; rm -f /etc/init.d/chill   # CHILL：先停，它会把 DNS 设置换回原样
for s in luyaz-sentinel luyaz-web luyaz-core u60-uid u60-guard zte-agent zwrt-datad; do [ -x /etc/init.d/$s ] && /etc/init.d/$s stop; rm -f /etc/init.d/$s; done
for f in /data/luyaz/kit/rc.local.orig /data/u60-kit/rc.local.orig; do [ -s $f ] && cp $f /etc/rc.local && break; done
rm -rf /data/luyaz /data/plugins/u60pro-devui /data/esim /data/chill /data/alerts /data/crashlog /data/power \
       /data/u60d /data/u60-kit /data/u60-ship /data/u60-guard /data/u60-uid /data/zte-agent /data/zte-agent.env /data/admin \
       /data/plugins/zwrt-datad /data/local/tmp/start_zte_agent.sh    # 第二行起是旧装机包的残留
# 连 SSH 也不要的话再加：rm -rf /data/ssh /data/local/tmp/start_dropbear.sh
reboot
```

重启后屏幕回到原厂界面。自动升级需要的话到网页后台重新打开。

## 常见问题

**登录失败 / 被锁**：网页后台连续输错 5 次会锁一段时间，脚本会显示剩余次数和解锁倒计时，别连着乱试。

**一直等不到 ADB 设备**：换根线、换个 USB 口（别用扩展坞）。Windows 上打开设备管理器，如果有带黄色感叹号的设备，右键更新驱动 → 浏览我的电脑 → 从列表选「Android ADB Interface」（需要先装 Google USB Driver）。`adb devices` 能看到一行 `device` 就说明通了，重跑 `./install.sh` 会从这里接着装。

**提示 SSH 连不上**：设备上已经装好了。提示会说是哪种：没有密钥 / 连接被拒 / 认证失败 / 超时。多半是电脑没连 U60 的 Wi-Fi，或者代理软件的 TUN/增强模式把 `192.168.0.1` 劫走了（在代理里把 `192.168.0.0/16` 设成直连，不用整个关掉）。再试：`ssh -p 2222 -i ~/.ssh/id_ed25519 root@192.168.0.1`。

**屏幕界面没数据**：SSH 进去看 `tail -n 50 /tmp/luyaz-core.log`（屏幕本身不对再看 `/tmp/luyaz-sentinel.log`），或跑 `./install.sh doctor`；重启一次通常就好。

**装到一半失败**：脚本可以反复跑，已经装好的部分会跳过或覆盖。把终端输出整段发给你装机包的人。
