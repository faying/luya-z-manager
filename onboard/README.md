# U60 Pro 装机包

给**刚到手的 ZTE U60 Pro（MU5250）**一键装上：

| 组件 | 装完是什么样 |
|---|---|
| **SSH** | `ssh -p 2222 root@192.168.0.1` 用你电脑上的密钥直接登录（只认密钥、不接受密码），重启后自动恢复 |
| **高级后台** | 浏览器打开 `http://192.168.0.1:9090/`，锁频/锁小区、短信、APN、防火墙、Wi-Fi 等全在里面 |
| **devui 触屏界面** | 前面板换成新界面：多载波信号卡片、图表、短信、锁频、eSIM 切换 |
| **eSIM** | 配合可插拔 eUICC 卡（5ber、eSTK.me 这类），在后台下载/切换/删除 profile，屏幕上也能切 |

装 SSH 时会顺手**关掉固件自动升级**（原因见「注意事项」）。

## 装之前确认

1. **固件版本 B27 或更早**（网页后台首页或「设备信息」里，形如 `…MU5250V1.0.0B27`）。
   B28 起中兴删掉了开 ADB 的接口，这个包就用不了了，**千万别先升级**。
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
tar xzf u60-kit-*.tar.gz
cd u60-kit
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
cd u60-kit
claude
```

然后说「帮我把 U60 装好」。它会先检查 adb、网络和固件，再问你要密码——不想告诉它的话，自己 `cp u60.env.example u60.env` 填好，跟它说「密码在 u60.env 里」就行。插 USB 线、确认要不要重启这类事它会停下来问你。

只想要其中几样也可以：

```sh
./install.sh ssh               # 只开 ADB + 持久化 SSH
./install.sh ssh admin devui   # 不要 eSIM
./install.sh status            # 看各组件状态
./install.sh reboot            # 重启一次，确认各组件开机自己起来
```

以后重装或更新某个组件，拿到新的装机包后直接 `./install.sh admin`（或 `devui` / `esim`），会走 SSH，不用再插线。

U60 地址不是 `192.168.0.1` 时：`GATEWAY=192.168.x.1 ./install.sh`。

## 装了什么、在哪

程序和数据都在 `/data`（固件升级也不会清）。开机自启只加了 `/etc/rc.local` 里的三行，没有改任何原厂服务。

| 路径 | 用途 |
|---|---|
| `/data/ssh/` | dropbear、host key、`authorized_keys`（**公钥正本**） |
| `/data/local/tmp/start_dropbear.sh` | 开机把公钥同步到 `/etc/dropbear/` 再起 dropbear（:2222） |
| `/data/zte-agent`、`/data/admin/` | 高级后台程序和网页 |
| `/data/local/tmp/start_zte_agent.sh` | 后台启动脚本，**后台密码写在这里** |
| `/data/plugins/u60pro-devui/`、`/data/plugins/zwrt-datad/` | 触屏界面和它的数据后端 |
| `/data/esim/` | lpac（eSIM 读写卡工具） |
| `/data/u60-kit/rc.local.orig` | 第一次安装前的原厂 `rc.local` 备份 |

## 日常用法

- **加一台电脑的 SSH 公钥**：把公钥追加到 `/data/ssh/authorized_keys`，然后 `sh /data/local/tmp/start_dropbear.sh`（改 `/etc/dropbear/` 里那份没用，开机会被覆盖）。
- **改后台密码**：改 `/data/local/tmp/start_zte_agent.sh` 里那一行，然后 `killall zte-agent; sh /data/local/tmp/start_zte_agent.sh`。或者重跑 `./install.sh admin`。
- **eSIM**：后台「移动网络 → eSIM」管理 profile；屏幕「更多功能 → eSIM」点两下切换，大约 10 秒生效，一般不用重启。
- **临时要 ADB**：SSH 进去跑 `ubus call zwrt_bsp.usb set '{"mode":"debug"}'`；`./install.sh status` 里的「USB 模式」显示 `user` 就是普通模式。

## 注意事项

- **别升级固件**。升级会覆盖 `rc.local`（SSH、后台、屏幕全部不再自启），而新固件又开不了 ADB，回不去了。装 SSH 时已经关了自动升级；手机 App 或网页提示升级也别点。
- **别用 `/etc/init.d/<服务> disable` 关原厂服务**。U60 的主守护进程要等一串服务全部就绪才放行开机，关掉其中一个会导致屏幕卡在 ZTE logo、不拨号。想精简服务先问清楚。
- **eSIM 别切到没流量的 profile 再远程操作**。如果你是通过这台 U60 自己的网络远程连进来的，切过去就断了，只能到设备跟前在屏幕上切回来。
- 这是非官方改装，风险自负。

## 恢复原厂

SSH 登录后：

```sh
cp /data/u60-kit/rc.local.orig /etc/rc.local
rm -rf /data/zte-agent /data/admin /data/plugins/u60pro-devui /data/plugins/zwrt-datad /data/esim \
       /data/local/tmp/start_zte_agent.sh
# 连 SSH 也不要的话再加：rm -rf /data/ssh /data/local/tmp/start_dropbear.sh
reboot
```

重启后屏幕回到原厂界面。自动升级需要的话到网页后台重新打开。

## 常见问题

**登录失败 / 被锁**：网页后台连续输错 5 次会锁一段时间，脚本会显示剩余次数和解锁倒计时，别连着乱试。

**一直等不到 ADB 设备**：换根线、换个 USB 口（别用扩展坞）。Windows 上打开设备管理器，如果有带黄色感叹号的设备，右键更新驱动 → 浏览我的电脑 → 从列表选「Android ADB Interface」（需要先装 Google USB Driver）。`adb devices` 能看到一行 `device` 就说明通了，重跑 `./install.sh` 会从这里接着装。

**提示 SSH 连不上**：设备上已经装好了，多半是电脑没连 U60 的 Wi-Fi，或者代理软件的 TUN/增强模式把 `192.168.0.1` 劫走了（在代理里把 `192.168.0.0/16` 设成直连，不用整个关掉）。再试：`ssh -p 2222 -i ~/.ssh/id_ed25519 root@192.168.0.1`。

**屏幕界面没数据**：SSH 进去看 `cat /tmp/zwrt-datad.log`；重启一次通常就好。

**装到一半失败**：脚本可以反复跑，已经装好的部分会跳过或覆盖。把终端输出整段发给你装机包的人。
