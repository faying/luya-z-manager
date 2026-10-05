# U60 Pro（MU5250）上的 Tailscale

[English](README.md) · 中文

`start.sh` 由 `/etc/rc.local` 在开机时调用，负责启动 `tailscaled` 并让节点上线。`apply.sh` 用来切换调参文件（`tuning.env`）：切换后 Tailscale 没恢复健康，就自动换回原来那份。

| 设备上的文件 | 是什么 |
|---|---|
| `/data/tailscale/tailscaled`、`/data/tailscale/tailscale` | 官方静态 arm64 版（pkgs.tailscale.com 的 `tailscale_<版本>_arm64.tgz`） |
| `/data/tailscale/start.sh` | 本目录的 `start.sh` |
| `/data/tailscale/apply.sh` | 本目录的 `apply.sh`（可选） |
| `/data/tailscale/tuning.env` | 你的设置（可选，见下） |
| `/data/tailscale/state/` | 节点身份：要备份，别给别人 |
| `/data/tailscale/boot.log` | 每次启动做了什么决定（超过 64 KB 自动截短） |
| `/data/tailscaled.log` | tailscaled 自己的日志（追加写；需要自己轮转） |

## 安装

设备上没有 `scp`，用 `ssh … 'cat > 文件' < 文件` 传。下面的 `<device>` 换成你路由器的地址，并且假定 SSH 已经能连上。

```sh
# 1. 程序（任一较新的稳定版）
curl -fLO https://pkgs.tailscale.com/stable/tailscale_1.102.4_arm64.tgz
tar xzf tailscale_1.102.4_arm64.tgz
ssh root@<device> 'mkdir -p /data/tailscale'
for f in tailscaled tailscale; do
  ssh root@<device> "cat > /data/tailscale/$f && chmod 755 /data/tailscale/$f" < tailscale_1.102.4_arm64/$f
done

# 2. 脚本
ssh root@<device> 'cat > /data/tailscale/start.sh' < scripts/tailscale/start.sh
ssh root@<device> 'cat > /data/tailscale/apply.sh && chmod 755 /data/tailscale/apply.sh' < scripts/tailscale/apply.sh

# 3. 开机自启之前先检查（不启动任何东西、不改网络）
ssh root@<device> 'sh /data/tailscale/start.sh check'

# 4. 启动并登录一次（打开打印出来的链接；也可以用 --auth-key=tskey-…）
ssh root@<device> 'sh /data/tailscale/start.sh; sleep 10; /data/tailscale/tailscale --socket=/tmp/tailscaled.sock login'
```

`check` 通过、节点也已经出现在你的 tailnet 里，再设开机自启：先备份 `/etc/rc.local`，在 `exit 0` **之前**加一行，然后检查语法：

```sh
ssh root@<device> 'cp /etc/rc.local /data/rc.local.bak && sed -i "/^exit 0/i sh /data/tailscale/start.sh" /etc/rc.local && sh -n /etc/rc.local && grep -n tailscale /etc/rc.local'
```

别用 `/etc/init.d/… enable` 做自启。原厂有一道开机关卡，要等它自己的守护进程全部注册才让开机走完，往那里加别的东西可能让开机卡在 logo。

设备上从不保存 auth key。登录过一次以后，`state/` 里的身份就够用了。

## 设置：`/data/tailscale/tuning.env`

所有设置都是可选的，每行一个 shell 赋值。文件会先用 `sh -n` 查语法、再试读一次，任何一步失败就整个忽略、用默认值（`boot.log` 里会写明）。

| 键 | 默认 | 含义 |
|---|---|---|
| `TS_HOSTNAME` | `u60pro` | 节点名 |
| `TS_ROUTES` | `br-lan` 的 /24 | 要广播的网段，如 `192.168.0.0/24`；广播后 tailnet 里能访问你的局域网 |
| `TS_ACCEPT_ROUTES` | `true` | 使用其他节点广播的网段 |
| `TS_ACCEPT_DNS` | `false` | 让 Tailscale 管 DNS |
| `TS_EXIT_NODE` | 无 | 上网流量走这个节点（IP 或名字）；仍然可以访问局域网 |
| `TS_MODE` | `tun` | `tun` = 内核网络（`tailscale0`）；`userspace` = 不加内核路由和防火墙规则 |
| `TS_TAILSCALED_FLAGS` | 无 | 额外的 tailscaled 参数，如 `--no-logs-no-support` |
| `TS_TAILSCALED_ENV` | 无 | 额外的环境变量，如 `TS_DISABLE_PORTMAPPER=1 GOMEMLIMIT=192MiB` |
| `TS_TAILSCALED_BIN` | `/data/tailscale/tailscaled` | 另一份 tailscaled（文件名必须是 `tailscaled`）；找不到就用默认那份 |
| `TS_WAN_WAIT` / `TS_STABLE` / `TS_MAX_STRIKES` / `TS_SAFE_RETRY` | 180 / 600 / 3 / 3600 | 见「开机安全」 |

想在改设置的同时保证远程不断，用 `apply.sh`：它重启 Tailscale，最多等 5 分钟，没恢复健康就换回旧文件：

```sh
sh /data/tailscale/apply.sh /data/tailscale/my-variant.env relay
```

## 命令

```sh
sh /data/tailscale/start.sh            # 启动（rc.local 跑的就是这个）
sh /data/tailscale/start.sh status     # 当前模式、安全模式、开机失败计数、是否关掉
sh /data/tailscale/start.sh check      # 启动前检查
sh /data/tailscale/start.sh stop       # 停止，并撤掉它加的路由和防火墙规则（下次启动之前不会被拉起）
sh /data/tailscale/start.sh disable    # 停止，以后开机也不启动（不用改 rc.local）
sh /data/tailscale/start.sh enable     # 取消 disable 并启动
sh /data/tailscale/start.sh clear-safe # 立即退出安全模式
```

## 开机安全

- **不拖住 rc.local。** rc.local 是在开机流程里同步跑的，后面的开机步骤都要等它，其中包括「把刚刷进去的固件槽标记为正常」那一步。rc.local 等待的那部分只看几个文件就返回；等网络、启动 tailscaled、`tailscale up` 都在后台做，每一步都有时间上限。
- **关断开关。** `/data/tailscale/disabled` 存在就什么都不启动（用 `disable` / `enable` 管理）。关掉 Tailscale 永远不需要改 rc.local。
- **内核网络不安全时改用 userspace 模式。** `tun` 模式下，tailscaled 会丢掉从其他接口进来、来源是 `100.64.0.0/10` 的包。有些运营商的 WAN 地址、网关或 DNS 就在这个网段里，路由器自己的 DNS 就会断。只要其中任何一个落在这个网段，或者没有 TUN 设备，这次开机就用 userspace 模式。userspace 模式不加路由和防火墙规则；tailnet 仍然能访问路由器上监听所有地址的服务（SSH、管理网页），但局域网设备不能再经路由器访问 tailnet。
- **开机失败计数 → 安全模式。** 连续 3 次开机都用 `tun` 模式启动、设备却没撑过 10 分钟，就进入安全模式（`/data/tailscale/safe-mode`），改用 userspace 模式。安全模式下撑过 1 小时，下次开机再试一次 `tun`。别的原因引起的重启（比如基带崩溃）也会算进去，所以这里只降级、绝不关掉 Tailscale。同一次开机里 `apply.sh` 引起的重启不计数。
- **设置坏了也照样起。** `tuning.env` 写坏了会被忽略；`TS_TAILSCALED_BIN` 指的程序不在，就退回默认那份。
- **挂了会拉起，但有上限。** 启动之后有一个很小的后台看守，每分钟看一次 tailscaled 还在不在。不在了，就先撤掉它留下的路由和防火墙规则，再按开机时的同一套规则重新启动（关断开关、安全模式、userspace 检查都照样生效）。第一次拉起前等 5 分钟，第二次等 15 分钟，之后每次等 1 小时；tailscaled 撑过 10 分钟就从头算。24 小时内拉起 6 次还不行，就暂停到这 24 小时过完，记进 `boot.log`，设备上装了告警队列的话再加一条 `tailscale-gave-up` 告警。它只看进程在不在，不看 Tailscale 健不健康。关断开关开着、`stop` 之后（到下次启动为止）、`apply.sh` 正在切换时，都不会拉起。`tuning.env` 里写 `TS_WATCH=0` 可以关掉它；`TS_WATCH_POLL`、`TS_REVIVE_DELAYS`、`TS_REVIVE_MAX`、`TS_REVIVE_WINDOW` 改这些数。

## 从 `scripts/tailscale-start.sh` 迁移过来

旧脚本已废弃（现在运行它只会打印一段说明）。迁移步骤：

1. 删掉它在 rc.local 里的那一行：`sed -i '\#tailscale-start.sh#d' /etc/rc.local && sh -n /etc/rc.local`。
2. 按上面的步骤装好 `start.sh`，加上它的 rc.local 行。
3. 设置：把 `/data/tailscale/tsconfig` 里的 `TS_ROUTES`、`TS_HOSTNAME`、`TS_ACCEPT_ROUTES`、`TS_ACCEPT_DNS`、`TS_EXIT_NODE` 抄进 `tuning.env`，**不要抄 `TS_AUTHKEY`**。然后删掉 `tsconfig`，因为里面有你的 auth key。
4. 身份：如果你的身份文件在 `/data/tailscale/tailscaled.state`，或者是一个名叫 `/data/tailscale/state` 的文件，`start.sh` 第一次启动时会把它挪到 `/data/tailscale/state/tailscaled.state`。`check` 会提前告诉你。
5. 旧脚本做的这些事，新脚本故意不做：
   - 校时（固件自己的时间服务会做）；
   - 加 MSS 钳制、FORWARD、SNAT 这些 iptables 规则（原厂防火墙已经有 `tailscale` 区域，子网路由的地址转换 tailscaled 自己会做）；
   - mwan3 策略路由的绕行；
   - 在 tailnet 地址上再起一个 dropbear（监听所有地址的 SSH 从 tailnet 也能连）；
   - 启动 ShellCrash。

   确实需要其中某一项，请放到单独的脚本里跑，别放进这个脚本。

## 卸载

```sh
sh /data/tailscale/start.sh stop
sed -i '\#/data/tailscale/start.sh#d' /etc/rc.local && sh -n /etc/rc.local
# 可选：先用 /data/tailscale/tailscale --socket=/tmp/tailscaled.sock logout 让节点下线
rm -rf /data/tailscale /data/tailscaled.log*
```

## 测试

```sh
docker run --rm -v "$PWD":/w busybox sh /w/scripts/tailscale/test/start.sh   # start.sh
docker run --rm -v "$PWD":/w busybox sh /w/scripts/tailscale/test/run.sh     # apply.sh
```
