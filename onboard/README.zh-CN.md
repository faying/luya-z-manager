# Luya-Z 装机包（ZTE U60 Pro / MU5250）

*[English](README.md) · 中文*。在你自己已经配好的 SSH 登录上跑一次 `./install.sh`，把 ZTE U60 Pro（MU5250）变成 Luya-Z 设备。

## 会装上什么

| 组件 | 装好以后 |
|:--|:--|
| **SSH** | 用 `ssh -p 2222 root@192.168.0.1` 凭密钥登录（密码登录一直关着），重启后照样能连 |
| **高级后台** | 浏览器开 `http://192.168.0.1:9090/`：锁频锁小区、短信、APN、防火墙、Wi-Fi 等 |
| **ui 触屏界面** | 前面板换成新界面，按任务分 5 个标签（首页 · 蜂窝 · Wi-Fi · 出口 · 系统）：信号结论和载波、Wi-Fi 和连着的设备、CHILL/Tailscale 出口、eSIM 切换 |
| **eSIM** | 插可插拔 eUICC 卡（5ber、eSTK.me 之类）后，在后台下载、切换、删除 profile；屏幕上也能切 |
| **CHILL**（可选，点名才装） | 设备上的透明代理（mihomo），带 zashboard 面板，按规则分流；订阅地址要你自己的 |

装 **SSH** 时顺带做两件事：

- **关掉固件自动升级**：`zwrt_zte_dm.dm_update.dm_update_mode=0` 和 `TURNOFFPOLLING=1` 都设上，再读回来核对。原因见[别做的事](#别做的事)。
- 先起 dropbear，确认 2222 在监听，才往 `rc.local` 写那一行；起不来就不碰 `rc.local`。

## 需要准备

1. **SSH 已经能用。** 装机包只经你自己已经配好的 SSH 登录工作：`root`、端口 2222（dropbear）、你的密钥（`SSH_KEY`，默认 `~/.ssh/id_ed25519`），自己不会去开任何进入设备的方式。SSH 怎么来的不在本项目范围内。可以先用 `ssh -p 2222 -i ~/.ssh/id_ed25519 root@192.168.0.1 true` 试一下。
2. **固件 B27～B31。** 版本号在网页后台首页或「设备信息」里，结尾类似 `…MU5250V1.0.0B31`。B31 之后的版本没测过：**装之前别升级。**`install.sh` 动手之前会先读设备的型号和固件，按 `firmware-capabilities.matrix.json`（型号 / 地区 / 固件 → 每个组件 `verified`、`code-only` 或 `unsupported`）逐个组件对照：国行 B27、B31 在真机上装过；B28～B30 和 MU5252 上各组件是 `code-only`，要加 `LUYAZ_ALLOW_UNVERIFIED=1` 才装；`unsupported` 的组件（以及更新的固件、别的地区、别的机型上的全部组件）跳过并说明，其余照装；一个都不剩才停。
3. 电脑上有 **ssh** 和 **tar**。Windows 电脑装 [Git for Windows](https://git-scm.com/download/win)，下面的命令都在 **Git Bash** 里输入。
4. 要用 eSIM 就准备一张可插拔 eUICC 卡。普通 SIM 卡也能完成安装，只是 eSIM 页面会显示没有卡。

## 安装

```sh
tar xzf luyaz-kit-*.tar.gz
cd luyaz-kit
./install.sh        # 开始引导式安装
```

脚本会一步步带你：

1. 先确认 SSH 能登录（登不上就停，什么都不改）。
2. 给**高级后台**定一个登录密码。直接回车 = 沿用设备上已有的（第一次装必须输入，或设 `AGENT_PASSWORD`）；不能有引号、反斜杠、空格。
3. 接下来4. 接下来上传、安装、检查自动进行，约 1～2 分钟；屏幕闪一下，回来就是新界面。
4. 问要不要**重启验证**时选是：重启后脚本会确认 SSH、高级后台和屏幕都自己起来了。

### 装完以后

最后会打印一段 `~/.ssh/config`，加进去以后 `ssh u60` 就能登录。

### 让 Claude Code 代装

包里有一份 `CLAUDE.md`，写了安装步骤、出错时怎么办、设备上哪些东西绝不能碰。在包目录里启动 Claude Code，它会自己读：

```sh
cd luyaz-kit
claude              # Claude Code 会读这里的 CLAUDE.md
```

跟它说「帮我装 U60」。它会先确认 SSH 能登录，再问高级后台密码。不想把密码交给它，就 `cp u60.env.example u60.env`，自己填好，然后告诉它「密码在 u60.env 里」。需要人动手的地方（同不同意重启等）它会停下来问你。

### 只装一部分，以及其他命令

```sh
./install.sh ssh               # 只把 SSH 登录装成重启不丢（dropbear 2222 端口，你的密钥）
./install.sh ssh admin ui   # 除了 eSIM 都装
./install.sh chill             # CHILL 代理（不在默认组件里；第一次装完不启动，见「日常用法」）
./install.sh recover           # u60 ship 的开机收尾：在设备上自检通过后放上 /data/luyaz/ship/luyaz-recover.sh，
                               #   并在 rc.local 的各服务行前面加一行调用它。
                               #   不在默认组件里：确实要改 rc.local 时才用
./install.sh status            # 看各组件状态
./install.sh doctor            # 只读体检（开机同步、自动升级、服务、心跳、Wi-Fi、告警……），用 ●▲■ 标出
./install.sh backup            # 把设备配置备份到这台电脑的 ./backups/（里面有密码，别外传）；
                               #   另存一份 config-…system.tgz（rc.local、我们的 init.d 脚本、dropbear host key、
                               #   authorized_keys、start_dropbear.sh），供升固件前参考，restore 不恢复它
./install.sh restore 备份.tgz  # 先和 backup 旁存的 备份.tgz.sha256 比对（不符一律不恢复；没有它的旧备份要
                               #   LUYAZ_RESTORE_NO_SHA256=1），推到设备后再比一次，再列出会改哪些文件，输入 yes 才写
./install.sh reboot            # 重启一次，确认各组件都能自己起来
```

装到一半断了（连接掉了、某个组件失败）：用同一个包再跑一次。设备上 `/data/luyaz/kit/progress` 按组件记着进度（`./install.sh status` 能看到），已装完的组件跳过，从第一个没完成的续；换了包或加 `KIT_FRESH=1` 就从头来。

以后要更新或重装某个组件，解开新的装机包跑 `./install.sh admin`（或 `ui` / `esim`）即可，走 SSH，不用插线。

U60 不在默认地址时：`GATEWAY=192.168.x.1 ./install.sh`。

## 设备上多了什么

东西全放在 `/data`，升级固件不会清掉它。开机自启只是 `/etc/rc.local` 里的几行，原厂服务一个没改。

- 核心 luyaz-core、高级后台 luyaz-web、哨兵 luyaz-sentinel 都在 procd 下运行：崩了会被拉起，并留一条告警（短信通知在后台「系统 → 告警」里开）。
- `admin` 只装 luyaz-web。`ui` 装触屏界面、luyaz-core 和 luyaz-sentinel；`rc.local` 里 `/etc/init.d/luyaz-core start` 后面紧跟 `/etc/init.d/luyaz-sentinel start`。起哨兵后脚本最多等 60 秒，确认三件事：哨兵在 procd 下运行、心跳显示 `ours`、正在运行的触屏界面就是刚装的那个文件。
- `admin` 和 `ui` 都会把装了什么（装机包日期、提交号）记进设备清单 `/data/luyaz/ship/manifest.jsonl`，每个组件一行（lweb；touch、sentinel、core），`doctor` 拿它和文件核对。
- 只支持现在的目录布局；设备上还是旧布局时拒绝安装，先用旧版装机包升级。

| 路径 | 是什么 |
|:--|:--|
| `/data/ssh/` | dropbear、host key、`authorized_keys`（**以它为准的公钥清单**） |
| `/data/local/tmp/start_dropbear.sh` | 开机时运行：把公钥复制到 `/etc/dropbear/`，在 :2222 起 dropbear |
| `/data/luyaz/luyaz-core`、`/data/luyaz/core/` | 核心（:9460，只对本机）。触屏界面和高级后台的读数、改动都经过它。状态在 `/data/luyaz/core/`，日志 `/tmp/luyaz-core.log` |
| `/data/luyaz/luyaz-web`、`/data/luyaz/operator-logos/` | 高级后台（luyaz-web，:9090） |
| `/data/luyaz/web.env` | **高级后台密码**，只有 root 能读 |
| `/data/luyaz/luyaz-sentinel`、`/data/luyaz/screen/` | 哨兵。屏幕归它管：触屏界面崩了它重启；连续两次起不来就换回原厂界面并告警（按住右下角 3 秒回来）。它也是 Wi-Fi 兜底看门狗、负责发告警短信。控制 FIFO `/tmp/luyaz-screen.ctl`，日志 `/tmp/luyaz-sentinel.log`，心跳 `/tmp/luyaz-sentinel.hb` |
| `/data/luyaz/ui/` | 触屏界面 `luyaz-ui` 及其字体和运营商 logo、开机记录 `boot-trace.log`、原厂界面副本 `vendor-*.orig-backup` |
| `/data/luyaz/scripts/`、`/etc/init.d/{luyaz-core,luyaz-sentinel,luyaz-web}` | 设备脚本（doctor.sh，转给 `luyaz-core doctor`；alert-lib.sh……；ship 执行器 luyaz-ship.sh 和 core-trial.sh）和 procd 服务。开机靠 `rc.local` 里的 `/etc/init.d/… start` 行起，不用 `enable` |
| `/data/luyaz/ship/` | `u60 ship` 事务、清单 `manifest.jsonl`、开机收尾 `luyaz-recover.sh` |
| `/data/alerts/`、`/data/crashlog/` | 告警记录、崩溃日志 |
| `/data/esim/` | lpac，读写 eSIM 卡的工具 |
| `/data/chill/`、`/etc/init.d/chill` | CHILL：mihomo、规则集、zashboard、`chill.sh`。订阅地址写在 `/data/chill/chill.env`（你自己写，只有 root 能读） |
| `/data/luyaz/kit/rc.local.orig` | 第一次安装前的原厂 `rc.local` |

## 日常用法

- **让另一台电脑也能 SSH 进来**：把它的公钥加进 `/data/ssh/authorized_keys`，再跑 `sh /data/local/tmp/start_dropbear.sh`。改 `/etc/dropbear/` 里的没用，每次开机都会被覆盖。
- **换高级后台密码**：再跑一次 `./install.sh admin`，给新密码。或者 SSH 进去改 `/data/luyaz/web.env` 里那一行（不能有引号、反斜杠、空格），再到 `:9090` 重新登录；luyaz-web 每次登录都现读这个文件，不用重启。
- **eSIM**：在后台「移动网络 → eSIM」管理 profile；屏幕上是「更多功能 → eSIM」，点两下切换。切换约 10 秒生效，一般不用重启。
- **CHILL**：第一次 `./install.sh chill` 只装不启动。
  1. SSH 进去，把 `/data/chill/chill.env.example` 复制成 `/data/chill/chill.env`，填上订阅地址，`chmod 600`。静态节点快照放 `/data/chill/providers/`。
  2. 跑 `sh /data/chill/chill.sh safe-start`；确认全屋网络正常后，5 分钟内跑 `sh /data/chill/chill.sh confirm`。不确认它会自己停掉，配错了也不会把你锁在外面。
  3. 以后在后台「服务 → CHILL」里开关、选出口。那一页的按钮打开 zashboard，由高级后台在自己的登录后面提供（`:9090/chill-ui/`，经 Tailscale 也能用）。

  之后再跑 `./install.sh chill` 就是更新：订阅地址和缓存保留；原来在运行的话会重启内核，新内核起不来会自动换回旧的。

## 别做的事

- **别升级固件。** 升级会换掉 `rc.local`，SSH、高级后台、屏幕开机都不再自己起来，还可能丢掉装机包重装要用的 SSH 登录。装 SSH 时已经关了自动升级，手机 App 和网页里的升级提示也别点。
- **别用 `/etc/init.d/<服务> disable` 关原厂服务。** U60 的主守护进程要等一组固定的服务全部报到才放行开机；关掉其中一个，屏幕就停在 ZTE logo，模块也不拨号。想精简服务先问清楚。
- **人在远程时别把 eSIM 切到没流量的 profile。** 如果远程连接走的就是这台 U60 的网络，一切换就断了，只能到设备跟前在屏幕上切回来。
- 这是非官方改装，风险自负。

## 恢复原厂

SSH 登录后：

```sh
echo vendor > /tmp/luyaz-screen.ctl; sleep 8     # 先让原厂界面接管屏幕
[ -x /data/chill/chill.sh ] && /data/chill/chill.sh stop; rm -f /etc/init.d/chill   # 停 CHILL 时会把 DNS 设置换回原样
for s in luyaz-sentinel luyaz-web luyaz-core; do [ -x /etc/init.d/$s ] && /etc/init.d/$s stop; rm -f /etc/init.d/$s; done
for f in /data/luyaz/kit/rc.local.orig /data/u60-kit/rc.local.orig; do [ -s $f ] && cp $f /etc/rc.local && break; done
rm -rf /data/luyaz /data/esim /data/chill /data/alerts /data/crashlog /data/power \
       /data/u60d /data/u60-kit /data/u60-ship /data/admin    # 第二行起是旧布局的残留
# SSH 也不要的话再加：rm -rf /data/ssh /data/local/tmp/start_dropbear.sh
reboot   # 重启后生效
```

重启后屏幕就是原厂界面。需要自动升级的话，自己到网页后台打开。

## 出问题时

- **装之前 SSH 就登不上**：脚本会停下，什么都不改。核对 `GATEWAY`、`SSH_KEY`，以及代理软件的 TUN/增强模式有没有劫持 `192.168.0.1`（设成直连，不用整个关掉）。SSH 本身怎么开通不在本项目范围内。
- **提示 SSH 连不上**：设备那边已经装好了，提示会说是没有密钥、连接被拒、认证失败还是超时。常见原因是电脑没连 U60 的 Wi-Fi，或者代理软件的 TUN/增强模式劫持了 `192.168.0.1`（在代理里把 `192.168.0.0/16` 设成直连，不用整个关掉）。然后再试 `ssh -p 2222 -i ~/.ssh/id_ed25519 root@192.168.0.1`。
- **屏幕上没数据**：SSH 进去看 `tail -n 50 /tmp/luyaz-core.log`（屏幕本身不对再看 `/tmp/luyaz-sentinel.log`），或者跑 `./install.sh doctor`。重启一次一般就好。
- **装到一半失败**：直接再跑一遍，装好的部分会跳过或覆盖。一直失败的话，把终端的完整输出发给给你装机包的人。
