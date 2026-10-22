# 更新记录

每个组件有自己的版本号（[语义化版本](https://semver.org/lang/zh-CN/)）：
补丁号 = 修错；次版本号 = 新功能（旧设置和接口照样能用）；主版本号 = 要迁移设置，或者几个组件要一起换。

| 组件 | 是什么 | 版本号写在 |
|---|---|---|
| `core` | `luyaz-core`，核心服务（`:9460`） | `crates/luyaz-core/Cargo.toml` |
| `lweb` | `luyaz-web`，管理网页（`:9090`） | `crates/luyaz-web/Cargo.toml` |
| `sentinel` | `luyaz-sentinel`，屏幕主人和看门狗 | `crates/luyaz-sentinel/Cargo.toml` |
| `touch` | `luyaz-ui`，触屏界面 | `touch-ui/rs/ui/Cargo.toml` |
| `scripts` | 设备脚本（体检、启动脚本等） | `touch-ui/scripts/VERSION` |

每个程序用 `--version` 报版本，例如 `luyaz-core 1.0.0 (6e606ec)`。
平时写在 **Unreleased** 下，格式 `- <组件>: 内容`（例如 `- touch: …`）；`tools/luyaz bump <组件> <patch|minor|major>` 会把它们挪成新的版本段。`Requires:` 写需要的其他组件最低版本。

英文版是 [CHANGELOG.md](CHANGELOG.md)。

## Unreleased

- core：采集以外读 SIM 信息（写前检查、短信切槽等待、网页 SIM 页）都走同一个入口；读到的结果给下一轮采集直接用，不再多问一次 modem。
- kit: 给了 `OUI_TSV=`（`scripts/lanid/make-oui.sh` 生成的表）就装到 `/data/luyaz/lanid/oui.tsv`，用来显示厂商；公开构建没有这张表。

- kit: 装机包带上 lanid-parse 组件（已连设备识别），装到 `/data/luyaz/core/plugins/`，只有这一个组件；没有时核心照常跑、识别关着。`LANID_CWASM=` 指定现成的。

## core 1.33.7 — 2026-10-22

- doctor 按清单里写的名字读记录项；改名前的旧名（`devui-fonts`、`devui-logos`、`u60d-plugins`、`u60-recover.sh`）不再对应（两台都没有了）。

## core 1.33.6, sentinel 1.0.11, touch 1.10.7, scripts 1.0.9 — 2026-10-22

不再认旧的 devui 名字（devui 清理 B5）；两台设备上一轮已迁移。
- touch：只读 `ui.conf`，不再从 `devui.conf` 拷。
- sentinel：控制 FIFO 只认 `ui`；语言只看 ui.conf。
- core：告警按记录的名字读（不再把 `devui-*` 对应到新名）；短信语言只看 ui.conf。
- scripts：ship 和 config-backup 不再带 devui.conf。

## core 1.33.5, lweb 1.24.1, sentinel 1.0.10, touch 1.10.6, scripts 1.0.8 — 2026-10-22

设备上的名字去掉 devui（devui 清理 B2）；旧名字下一轮之前照认。
- 触屏：设置改叫 `/data/luyaz/ui/ui.conf`；第一次启动把 `devui.conf` 拷过去，旧文件留着备退回。深浅色告警叫 `ui-theme-paused`。
- 哨兵：控制 FIFO 认 `ui`（`devui` 照认）；告警叫 `ui-crash`、`ui-gave-up`；语言先读 ui.conf，没有再读 devui.conf。
- 核心：短信语言先读 ui.conf，没有再读 devui.conf；体检提示改成 `echo ui > /tmp/luyaz-screen.ctl`。
- 核心、网页、哨兵：记成 `devui-*` 的旧告警按新名 `ui-*` 显示（说明、短信内容、不发短信的名单）。
- 脚本：上机试跑触屏时 ui.conf、devui.conf 都备份；配置备份加上 ui.conf。
- 触屏、装机包（devui 清理 B3）：crate 改为 `touch-ui/rs/ui`（包名 `luyaz-ui`）、`touch-ui/rs/ui-model`；装机包组件和包内目录叫 `ui`（`./install.sh ssh admin ui esim`）；装机包变量 `TOUCH_REPO`、`TOUCH_BIN`、`UI_FONTS_DIR`；触屏日志前缀 `ui:`；调试变量 `LUYAZ_UI_FORCE_HSR`；编译卷 `u60-ui-rs-target`。

## core 1.33.4 — 2026-10-22

- 局域网设备名：mDNS 名字只认代表整台设备的服务（AirPlay、SMB、SSH、打印机等），App 自己广播的服务不算；安卓手机 DHCP 名字排在 mDNS 名字前面（要配新版 lanid-parse 插件）。

## touch 1.10.5 — 2026-10-22

- 触屏：不再到已退役的 /data/plugins/u60pro-devui 找字体。
- 恢复脚本：luyaz-recover.sh 不管有没有残留旧文件，一律不删 luyaz-ui、不还原起 u60-uid/u60-guard 的 rc.local（以前先看旧程序在不在）。

## sentinel 1.0.9, scripts 1.0.7 — 2026-10-22

- 哨兵：10-22 ship 写进 `/tmp/luyaz-screen.ctl` 的 `hold` 有三个小时没到哨兵手里（心跳照常，重启后好了，原因没查到）。现在每一轮都核对这个路径还是不是自己在读的那个 FIFO，不是就新建一个（读到文件结尾也新建），读出错（不是「没东西」）就记日志。
- 脚本：哨兵 10 秒内没放下屏幕时，`luyaz-ship.sh` 把现场记进日志（心跳和它多久前写的、FIFO 的 inode 和哨兵打开的 fd 对比、每个线程的状态/wchan/syscall），不重启哨兵，留住现场。

## core 1.33.3, touch 1.10.4 — 2026-10-22

- core: 网络页的 APN 读取（`/api/netinfo?apn=1`）重用 60 秒，切换 APN 时立刻作废：触屏蜂窝页每 5 秒读一次，固件每次往 key.log 写约 8 行，几小时就把拨号时那几行（QCI/AMBR）挤出日志，载波卡的「上限」随之消失。
- core: 日志里一条承载都没有时，`qos.bearer` 按当前制式填（SA = `nr_pdu`），不再留空让屏幕和网页把 SA 的 5QI 写成 QCI。
- touch: 载波卡有 QCI/5QI 但不知道 AMBR 时写「上限 未知」，不再只剩半句。

## core 1.33.2 — 2026-10-22

- 局域网设备名：iOS 的 `<MAC>@fe80::…` mDNS 实例名是 id 不是名字，手机重新显示为 `iPhone`。

## core 1.33.1 — 2026-10-22

- 诊断探测预算拿锁失败时隔 10 ms 再试几次才放弃：进程里别处 fork 时子进程会短暂拿着锁的副本。测试：修掉两个偶发失败（预算锁、刚写好的测试脚本 ETXTBSY）。

## core 1.33.0, lweb 1.24.0, mcp 0.4.0 — 2026-10-22

- core: 判断「数据通」不再只看固件的 `connect_status`（B31 上没有承载也会说已连接）：注册守卫、等数据、短信转发都要求有经 `rmnet_data0` 的默认路由（`/proc/net/route`，IPv6 也算）。
- core: `/api/public/status` 新增 `network.health`：承载、当前问题、被底层闸门暂停的修复、下一步（等待 / 检查 APN）和一句给网页、触屏共用的话。故意关数据时不说话；暂停的原因不在了就清掉。
- core: CHILL 端到端探测：CHILL 开着且有客户端时，最多 10 分钟一次（独立预算，不占网络诊断的）经 mihomo 请求一次；API 活着却连两次不通，`network.health` 提示「代理转发不通」。只提示、不重启。`LUYAZ_CHILL_PROBE=0` 关闭。
- lweb: 首页提示改读 `network.health` 的那句话（自动恢复失败时链到 APN，只有代理不通时链到 CHILL）；旧核心照旧显示问题描述。Agent API `/agent-api/network-health`。
- mcp: 只读工具 `get_network_health`。
- kit: `onboard/README` 改成 B27～B31（B28 起先自己开 SSH）；`sandbox.sh` 示例改用 `HOST=root@<网关>`。CI：`.github/workflows/test.yml`（cargo test + aarch64 构建）。

## core 1.32.6 — 2026-10-22

- 带 UUID 的局域网设备名（安卓「快速分享」的 `nearby-presence-nsd-<uuid>`）不再当设备名显示，回到 DHCP 名字（如 `OnePlus-Ace-3-Pro`）。

## core 1.32.5 — 2026-10-22

- eSIM 切换：SIM 身份比 45 秒稍慢不再重启设备。超过 45 秒后再等最多 75 秒，身份和数据连接都还没回来才重启（u60g 10-22：身份晚几秒回来、数据正常，却已经因此重启了一次）。

## touch 1.10.0 — 2026-10-22

- eSIM 切换的状态行在进行中和结束时都用核心给的共用短文案（`say_short`），和管理网页一样；自己没在切换时，任何 SIM 变化期间 eSIM 页显示 `sim` 块的文案；任务最多读 2 KB。旧核心照旧。Requires: core 1.32.0（新文案）。

## lweb 1.23.0 — 2026-10-22

- eSIM 切换结果开头是核心给的共用文案（`say`）；已知原因的失败显示这句和该怎么做，不再显示 lpac 原始消息（写进日志）；切换进行中 eSIM 页状态显示任务的文案（如「安全重试中」），任何 SIM 变化期间显示 `sim` 块的文案。旧核心照旧。

## touch 1.10.3 — 2026-10-22

- 双卡：待机卡写出漫游到哪（如「漫游到中国移动」），用核心给的说法，首页和蜂窝页都有。Requires: core 1.32.4。

## lweb 1.23.1 — 2026-10-22

- 双卡：首页两张卡显示在哪（如「4G · 漫游到中国移动」），logo 用核心的选法（CMLink）；双卡页在运营商后面加上同一句。Requires: core 1.32.4。

## core 1.32.4 — 2026-10-22

- 双卡：每个卡槽带 `place`/`place_en`，用上网卡同样的说法说卡在哪（本地 / 漫游到中国移动 / 漫游）；待机卡的网络名称来自它的 QMI 服务网络读取。

## touch 1.10.2 — 2026-10-22

- 双卡：首页待机卡那一行、蜂窝页两张卡都显示「本地」或「漫游」，和上网卡那一行一样。Requires: core 1.32.3。

## core 1.32.3 — 2026-10-22

- 双卡：每个卡槽带 `serving_mcc` 和 `true_roaming`（和 `signal.true_roaming` 同一条国家规则）；待机卡所在的网络在读信号时顺带用 QMI NAS Get Serving System 读。

## touch 1.10.1 — 2026-10-22

- 双卡：每个卡槽一行用核心给的 `logo`，和单卡同一套（如 CMLink、CMHK）；旧核心照旧只认大陆三家。Requires: core 1.32.2。

## core 1.32.2 — 2026-10-22

- 双卡：每个卡槽带 `logo`，和单卡同一套选法（先 SPN、再 PLMN），港澳台和其他地方的卡也有 logo，不再只有大陆三家。

## core 1.32.1 — 2026-10-22

- 双卡：固件把卡槽 1 的运营商留空时（B31、454-12 卡待机时见到），卡槽 1 改显示卡的 SPN 名称，不再空着。

## core 1.32.0 — 2026-10-22

- SIM/eSIM 切换的文案只有一张表（`sim_say`）：`sim` 块（按 SIM 身份 phase）和 `/api/esim/job`（切换任务，按 status/reason）带 `say`/`say_en`（全文）、`say_short`/`say_short_en`（触屏放得下）和 `say_id`：正在切换 / 安全重试中 / 已恢复 / 可能要查 APN / 自动恢复未成功、设置没被再改。旧字段不变。

## lweb 1.22.0 — 2026-10-22

- eSIM 页不再每 10 秒读配置列表、每 30 秒读通知（每次都跑 lpac 读卡）；改为只在打开页面（标签页隐藏时流会断开）、点刷新/重试或操作结束、SIM 身份 epoch/phase 变化时读，任务进行中或列表还在稳定时每 10 秒读一次，结束后再读 40 秒。

## core 1.31.0 — 2026-10-22

- 短信转发等换卡结束再发（SIM 身份 phase 不是 online 时暂停）：online 满 30 秒后接着发，卡住超过 10 分钟也照发；不丢任何短信。

## core 1.30.0 — 2026-10-22

- APN 监视改为在 SIM 身份 epoch/phase 变化时立即查看，兜底轮询 60 秒（原 10 秒）；SIM 切换处于 leaving/changed 时不再提议重连。

## core 1.29.0 — 2026-10-22

- 新警报种类 `sim-changed`、`sim-stalled`、`esim-failed`、`esim-rebooted`（eSIM 切换要求的自动重启，下次开机再报），每个 SIM 身份 epoch、每次开机只报一次。
- 换卡时 CHILL 关着或空闲暂停，日志写明不重载。

## core 1.28.0 — 2026-10-22

- eSIM 身份逾时自动重启可以关掉：有 `/data/luyaz/core/esim-auto-reboot-off` 这个文件时，卡住的切换以 `identity_stuck` 结束（手动重启），不再自动重启；默认开着。

## core 1.27.7 — 2026-10-22

- 更正 eSIM 切换：1.27.6 比对错了字段（`get_sim_info` 的 `sim_*` 固定是卡槽 1，也就是 eSIM 卡）。现在改成：卡槽 1 关着、或数据在卡槽 2 时，只在卡上切换（`reason: "inactive_slot"`）。

## core 1.27.6 — 2026-10-22

- 双卡设备上的 eSIM 切换：数据机用的是另一张卡（不是 eSIM 卡）时，只在卡上切换设定，任务以 `reason: "inactive_slot"` 结束——不重启数据机、不等身分、不自动重开（身分永远不会变，以前会白白重开）。

## core 1.25.0 — 2026-10-22

- 没拿到跨进程写锁就不写：别的写者占着满 15 秒、或锁文件打不开，回绝这次写（HTTP 503，`busy` 或 `lock_error`），设备上什么都不动。等锁不再拖住数据采集。
- Wi-Fi 写：只有能证明自己拿着 Wi-Fi 锁的调用方（它自己或它的父进程）不用等；其余的都要等，锁一直被占或打不开就回绝。
- 启动时待确认的改动一直等到拿到写锁才动，不再不拿锁就做。
- 排队的旧请求因为锁被回绝时放回队列，不再当成已做完。

## scripts 1.0.6 — 2026-10-22

- `luyaz-fallback.sh`：写锁文件打不开时 exit 5，什么都不写（以前是不拿锁照写）。

## core 1.24.0 — 2026-10-22

- eSIM 切换：「数据回来」改看蜂窝接口有地址、且上网路由从它出去（IPv4 或 IPv6 任一），不再看固件自己的拨号状态。
- eSIM 切换：卡回「忙」时自动重试最多 4 分钟（每次前先重读卡上列表），`POST /api/esim/cancel` 可停；任务多了 `attempts`、`deadline_s`。
- eSIM 切换：调制解调器一直认不出新配置时的自动重启，24 小时内最多一次，重启前先记成「有人要的」；之后任务以 `identity_stuck` 结束，请用户手动重启。
- `sim` 块多了 `epoch`、`phase`（online / leaving / changed / reloaded / stalled）：SIM 身份变化走到哪一步。

## lweb 1.21.0 — 2026-10-22

- eSIM：按切换任务自己报的时长等；分得清「重试后卡还是忙」「已停止重试」「卡已换，重启设备完成切换」。

Requires: core 1.24.0（更早的 core 照旧等 240 秒）。

## touch 1.9.0 — 2026-10-22

- eSIM：自动重试时显示「卡忙，自动重试中」，按切换任务报的时长等，需要重启才能完成时会说。

## core 1.23.0 — 2026-10-22

- 家里出口：蜂窝本身断了时探测家里失败不再计入，不会因为断网 15 分钟就关掉「其余都走家里」。
- CHILL（`chill.sh`，只记录的文件）：运行中每分钟查一次自己的路由规则，被删了就重启代理核心重建（10 分钟内超过 3 次就回直连并退避）；WAN 换到别的口才回直连，重拨不会。

## lweb 1.20.0 — 2026-10-22

- 已连设备：沿用了别名的设备标「新地址，沿用别名」，详情里显示原来的 MAC。

## core 1.22.0 — 2026-10-22

- 已连设备：换了新随机 MAC 回来、自己没有别名的设备，如果正好和一台不在线的设备对得上（同一个 mDNS 名字，或者同一个具体的 DHCP 主机名加同样的选项 55 顺序；只有主机名不算，对上两台也不算），就沿用那台的别名；从没见过的设备连上 90 秒后记一条告警「新设备连上了热点」（不发短信）。更新后第一次运行时，当时已有的设备都算认识，不告警。

## core 1.21.4 — 2026-10-21

- 双卡：QoS 查询用上网卡的 IMSI（上网卡在卡 2 时用卡 2 的）。

## lweb 1.19.1 — 2026-10-21

- 设备信息：上网卡在卡 2、号码为空时显示「卡 2 不提供号码」，不再说「卡里没存」。

## touch 1.8.2 — 2026-10-21

- SIM 页：上网卡在卡 2、号码为空时显示「卡 2 不提供号码」；PLMN 460-13 算中国移动。

## core 1.27.5 — 2026-10-22

- APN 块从每分钟读一次改成 10 分钟一次，写过之后、数据卡换了（ICCID 变）都立即重读：厂商每读一次往 key.log 写约 30 行。

## core 1.27.4 — 2026-10-22

- 换卡或换核心网后 QCI/5QI 和 AMBR 不再变空：厂商的 key.log 几小时就被别的日志冲掉，核心现在自己保留最近 400 条 `[DATA]` 行（`/data/luyaz/core/qos-lines.log`，有新行才写）。地址或核心网变了，这些旧值照样标成过时。

## core 1.27.3 — 2026-10-22

- 已连设备的名字一旦学到可读的就固定下来，同一台设备后来广播的 UUID 或 NetBIOS 名不再覆盖它（曾有 Mac 从 AirPlay 名变成 `MAC-…`）。

## touch 1.9.1 — 2026-10-22

- 已连接设备的厂商和类型单独一行，「连上以来」总流量不再被截掉。

## core 1.27.2 — 2026-10-22

- 已连设备取名：8 位以上的十六进制编号（安卓 adb/投屏）不再盖掉 DHCP 主机名；Mac 没有别的名字时用 mDNS 型号；电脑类服务优先于 AirPlay/投屏接收（Mac 曾被认成音箱）；OUI 厂商名去掉公司后缀（`Xiaomi Communications Co Ltd` → `Xiaomi`）。

## core 1.27.1 — 2026-10-22

- 修正（1.25.0 起）：情景引擎自己的 Wi-Fi 切换被回绝。它在核心里拿着 Wi-Fi 锁、经回环调核心的 `/control`；现在核心认得调用方是自己，不再等自己的锁 5 分钟。

## core 1.27.0 — 2026-10-22

- 换卡（eSIM 切换或固件自己切卡槽）后数据回来时，CHILL 重载一次（只在它开着时，已有 CHILL 工作在跑就不再叠一次），家里出口的探测失败数清零并马上探一次，旧卡上的失败不会再把「其余都走家里」关掉。

## core 1.26.1 — 2026-10-22

- 手动重拨（`/control` 的 `cellular.redial`，网页和触屏）先等正在跑的诊断或 Finding 探测结束（最多 4 秒），不再把它打断；正在改网络设置时回 409 忙。

## core 1.26.0 — 2026-10-22

- 下层闸门：代理以下的层坏着时（数据口没地址：一直；有更低层的确定诊断：最多 15 分钟），home-exit 不再累计家里探测失败，manual-first 也不再换走手选节点。已知故障的修复（进程死、规则丢）不受影响。

## lweb 1.19.0 — 2026-10-21

- 已连设备显示每台设备的最佳名字（别名优先）、厂商、类型和「私有地址」标记，详情里列出每个来源的值；可在详情里设置或删除别名（页内确认，写后读回）。Requires: core 1.21.0。

## touch 1.8.1 — 2026-10-21

- 修复：1.8.0 在设备上找不到触屏和电源键（重写的输入代码把 EVIOCGBIT 返回的字节数当成失败）。

## touch 1.8.0 — 2026-10-21

- 已连设备列表优先显示别名，并显示厂商、设备类型和「随机地址」提示；带新字段的设备不再被跳过。Requires: core 1.21.0。

## core 1.21.3 — 2026-10-21

- 屏幕 logo：PLMN 460-13 算中国移动（1.21.2 起 `sim` 区块用卡 2 的 IMSI，没有这条 logo 就是空的）。

## core 1.21.2 — 2026-10-21

- 双卡：上网卡在卡 2 时，`sim` 区块（imsi、iccid、状态、PIN）改为卡 2 的；固件的 `sim_*` 永远是卡 1，屏幕因此显示卡 1 的运营商 logo。卡 2 不上报号码和 SPN，留空。

## core 1.21.1 — 2026-10-21

- lanid 不再把 ID 当名字显示（iOS 有的 mDNS 服务用 UUID 当实例名）；`_apple-mobdev2`/`_remotepairing` 判定为 Apple 手机或平板。

## core 1.21.0 — 2026-10-21

- 已连设备识别（`docs/designs/lan-client-id.md`）：`lanid` 线程在 br-lan 上被动听 DHCP 请求、mDNS、SSDP、NetBIOS（内核 BPF，不加组播组，不发包），由新的 wasm 组件 `lanid-parse` 解析，没有内置退路：没有组件时只显示 DHCP 名字。`clients` 多了 `alias`、`vendor`、`kind`、`private`；`GET /api/lanid` 列出每台设备各来源的值，`POST /api/lanid/alias` 设别名。学到的名字存在 `/data/luyaz/lanid/clients.json`（只在内容变化时写，最多每小时一次）。doctor 多一行「设备识别」。

## core 1.20.0 — 2026-10-21

- lanid 录包开关文件可以写 `mb=N`（环形两个文件各多大，1–32，默认 2），长时间录包不会只留几小时的 mDNS；`status.json` 多了 `file_mb`。

## touch 1.7.0 — 2026-10-21

- 摆放模式（蜂窝 › 排查）加近 30 秒 SINR 曲线；「这次最好」SINR 相同时比 RSRP，显示成「17.7 · -82 dBm · 2 分钟前」。首页在 core 记下新国家后 24 小时内显示落地卡（`GET /v2/usage/landing`）：「到了日本 · 2 小时前」「从中国来 · 落地以来 110MB」。Requires: core 1.18.2。

## core 1.19.0 — 2026-10-21

- 已连设备识别第 0 步录包（默认关）：启动时有 `/data/luyaz/core/lanid-capture` 才开，`lanid` 线程把 br-lan 上的 DHCP 请求、mDNS、SSDP、NetBIOS 帧（内核过滤、不解析）录进 `/tmp/luyaz/lanid/` 的 4 MB pcap 环形文件，48 h 后自停（`docs/designs/lan-client-id.md`）。

## lweb 1.13.0 — 2026-10-21

- 有设备经 Tailscale 持续访问管理接口时，首页出一条横幅（谁、哪个口、多久、息屏约多耗 0.3–0.6 W），带「是我自己的」（放行 24 小时）和「知道了」；设置页列出白名单，可「改成永久」「删除」。Requires: core 1.15.0。

## core 1.15.0 — 2026-10-21

- `power.tailnet_poller`（docs/designs/tailnet-poller.md）：息屏不充电时，长期层每次检查（60 秒）读一次 conntrack，看 tailnet 对端和子网路由后面的对端进到 :80 / :9090 的流；同一对端连续 10 分钟每分钟 ≥ 20 包就开一次事件、出一条 `tailnet-poller` 告警。`GET /api/poller`（`/api/diagnose` 里也有 `tailnet_poller`）；`/control` `poller.allow`（默认 24 小时，`forever` 永久）、`poller.unallow`、`poller.ack`；白名单 `/data/luyaz/core/poller-allow.json`。
- 情景引擎下一轮会读到手改的 `/data/scenario/scenarios.json`（看修改时间和长度）：读得懂、验证通过就换上并记一行；文件不见、读不了或验证不过就保留原来的设置，只记一次。网页自己写的不算手改。

## Unreleased

- kit: 重写 install.sh、device/install.sh、build-kit.sh 里沿用下来的部分；参数、输出和装上的文件都不变。

## core 1.18.3 — 2026-10-21

- 把状态、短信、控制、服务、QoS、登录、型号和命令执行里沿用原数据服务的部分重写；行为、路由和 JSON 都不变。

## core 1.18.2 — 2026-10-21

- 落地记录真正读到所在国家了：之前读的是 `net.serving_mcc`，但旧 `net` 块里没有这个字段（只有 `/v2` 的 signal 块有），所以从来没记过；现在读 `net.mcc`（要是能对上一个国家）。

## lweb 1.18.0 — 2026-10-21

- 首页落地卡片（docs/designs/travel-assist.md §5）：到新国家后 24 小时内显示国家和从哪来、网络（运营商、漫游还是当地卡、制式）、当地时间（用网络给的时区；没有 NITZ 时用单时区国家的时区并标「估计」）、家里出口和落地以来的流量；「收起」可以隐藏。需要 core 1.18.2。

## core 1.18.1 — 2026-10-21

- 落地按国家判断，不按 MCC：310 和 311（都是美国）、440 和 441（日本）之类不会算一次落地。

## core 1.18.0 — 2026-10-21

- 出行落地记录（docs/designs/travel-assist.md §5）：连续 3 轮看到新的服务国家（`net.serving_mcc`）就新开一条记录，存在 `/data/luyaz/core/usage/landing.json`，含之前的国家、落地时间和之后用的流量；`GET /v2/usage/landing` → `{mcc, from, at, rx, tx}`（`from` 为 null：第一条记录，不算落地）。`uci_device_info` 增加网络时区 `nitz_timezone`、`nitz_time_dst`、`nitz_sync_flag`。

## lweb 1.17.0 — 2026-10-21

- /trends 新增「信号与网速」图：蜂窝下行和主载波 SINR 画在一起，SINR 用右侧 −10…30 dB 刻度。SINR 稳而网速掉，多半是基站忙；两条一起掉，是信号差。
- 首页载波行补上每个载波的 RSRQ。

## lweb 1.16.0 — 2026-10-21

- 新页面 /usage（流量，在「图表」里）：近 30 天每天用了多少，一天一根柱、按 SIM 卡分色，下面是按卡合计和逐日数字；首页「今天」那一行点进来。Requires: core 1.17.0。

## core 1.17.0 — 2026-10-21

- 按天、按卡的流量（docs/designs/travel-assist.md §4）：每 60 秒把本次连接的流量增量记给当前数据卡（原厂计数不分卡）；`GET /v2/usage/days?n=1..400`；文件在 `/data/luyaz/core/usage/`（`today.json`、`days.jsonl`，保留 400 天）。

## touch 1.6.1 — 2026-10-21

- 双卡切换提示改写断网约 10 秒（10-21 实测约 11 秒），不再写 10–25 秒。

## lweb 1.15.1 — 2026-10-21

- 双卡切换确认框改写断网约 10 秒（10-21 实测约 11 秒），不再写 10–25 秒。

## lweb 1.15.0 — 2026-10-21

- /signal 双卡时加「换卡记录」：过去 14 天的换卡，带推测原因（无服务、只有 2G/3G、信号差且另一张好）。Requires: 带 `/v2/dual/switches` 的 core。

## core 1.16.0 — 2026-10-21

- 双卡：被动记录，双卡模式下每 30 s 记一次上网卡和两张卡的信号，写 `/data/luyaz/core/dsds/YYYY-MM-DD.jsonl`（留 14 天，约 1 MB/天）；韧体换上网卡时记一条 `switch` 事件；`GET /v2/dual/switches?days=14`。

## lweb 1.14.2 — 2026-10-21

- 首页 Tailscale 轮询横幅的「知道了」同时把对应的 `tailnet-poller` 告警在告警条里标成已读（它下面没有别的未读告警时）。Requires: core 1.15.1。

## core 1.15.1 — 2026-10-21

- `power.tailnet_poller` 的事件和「知道了」存进 `/data/luyaz/core/poller-state.json`（只在事件开始、结束、并回、知道了、放行时写）：核心重启后，已点过「知道了」、还在持续的事件不会再告警一次。文件没有或读不懂就从头开始。

## lweb 1.14.1 — 2026-10-21

- 双卡待机：上网卡那行的 5G SA / 5G NSA / 4G 改按当前服务网络判断，不再用采样的制式。

## lweb 1.14.0 — 2026-10-21

- 双卡待机：首页「双卡」组移到顶部，与触屏同一套说法：上网卡排第一，用 logo + 名称代替「卡N」，右侧上网中 / · 自动切换 / 切换中 / 待机，待机卡变淡、信号弱时显示「待机 · 信号弱」；去掉逐卡 dBm/dB 和未经验证的智能切换门槛说明。RSRP/SINR 读数标上网卡运营商。中国移动 460-13 补上 logo。

## touch 1.6.0 — 2026-10-21

- 双卡待机：首页去掉「卡N」和 dBm/dB 两行；上网卡固定在第一行（上网中，智能切换开时加「· 自动切换」，切换时「切换中」），待机卡整行变淡、写 5G SA/4G，信号弱时显示「待机 · 信号弱」；结论行恢复与单卡一致，只说上网卡。蜂窝页新增「双卡」卡片，逐卡显示信号强度 / 信号质量。

## touch 1.5.4 — 2026-10-21

- 双卡待机：两张卡那两行改写文字解读（上网 · 信号强 / 待机 · 信号中）；大字结论小一号，右边两行小字放两张卡的 RSRP · SINR，「干扰 · 负载」移到结论下面。

## touch 1.5.3 — 2026-10-21

- 双卡待机：上网卡那行放不下时「漫游」永远保留，「本地」可以省掉。

## touch 1.5.2 — 2026-10-21

- 双卡待机：每张卡那行都带自己的运营商 logo（没有 logo 时写中文名，如「中国移动」而不是 CMCC），和单卡顶行一样。

## touch 1.5.1 — 2026-10-21

- 双卡待机：两张卡的两行改放在状态块顶部（浅绿底）里，取代原来那一行，不再另起一块。

## lweb 1.12.0 — 2026-10-21

- 首页：双卡模式下多一组「双卡」，两张卡各一行（运营商、制式、上网/待机、RSRP · SINR、读数时间，超过 60 秒变灰），智能切换开着时写门限；顶部 RSRP/SINR 标明是哪张卡。单卡设备和单卡模式不变。

## touch 1.5.0 — 2026-10-21

- 双卡待机（全球版）：状态栏信号点变成上下两排（上排上网卡、颜色照旧，下排待机卡、中性灰）；首页状态块下面每张卡一行：卡号、运营商、制式、角色和 RSRP · SINR（卡不就绪时写状态）。单卡设备不变。读 core 1.14 的 `sim.dual` 卡槽字段。

## core 1.14.0 — 2026-10-21

- 双卡：每张卡报 `state` 和 `role`，`sim.dual.smart_rule` 带智能切换门限，卡上多了可选的 `signal`（上网卡取 nwinfo，待机卡每 30 秒用只读的 QMI NAS 连接读一次；有待机卡才启动）。新增 `luyaz-core qmi-signal`。

## touch 1.4.0 — 2026-10-20

- 网络诊断：蜂窝设置和这张卡在这个国家上次能用时不同时，多一个「恢复上次能用的设置」按钮（按两下）；进度和结果显示在按钮下面。

## core 1.13.2 — 2026-10-20

- 崩溃记录：同一时钟刻度内连写几份时，修剪一定留下最新那份、每个程序正好 5 份（之前重用 `.1` 文件名会多留一份）。

## touch 1.3.1 — 2026-10-20

- 双卡：点第一下后的漫游提醒改短（320 px 屏上原来显示不全）。

## touch 1.3.0 — 2026-10-20

- 「SIM 与 eSIM」页加双卡（全球版，core 报两个卡槽时才显示）：两个卡槽各插着什么，五个选项（只用卡槽 1、只用卡槽 2、双卡主卡 1 或 2、双卡智能切换）；点两次发 core 的 `sim.dual` 事务，进度和倒计时显示在顶上那行，卡可能漫游时会提醒。Requires: core 1.13.1。

## lweb 1.11.0 — 2026-10-20

- 新页「双卡」（`/router/dualsim`，双卡设备的 SIM / PIN 页里有入口）：两个卡槽、单卡/双卡、主卡、智能切换；三级确认框，卡可能漫游（或看不出来）时提醒；走 u60d 的 `sim.dual` 事务。

## core 1.13.1 — 2026-10-20

- `sim.dual` 的进度和值在 op 块、改动记录、触屏上用文字（「双卡 · 主卡 卡槽 2」），不再显示原始 JSON。

## core 1.13.0 — 2026-10-20

- 双卡（全球版）：`/v2` `sim` 块加两槽的 `dual` 视图；新写 `sim.dual`（单卡/双卡、主卡、智能切换）走事务：读回、数据通确认、到点退回；`support_dual_sim` 不是 1 的机器上不出现。

## core 1.12.0 — 2026-10-20

- core: 「恢复上次能用的连线」第一部分（还没有按钮）。你按深查之后，多跑一次绑蜂窝的 HTTPS + DNS 检查（约 20 KB），通过就按「卡 × 国家」记下当时能用的蜂窝设置（锁小区、频段、制式、APN）。`GET /api/verified` 显示现在哪些不一样；`/control verified.restore`（只认触屏和网页，代理不能用）逐项用普通事务改回去，哪一步没确认就停。

## lweb 1.10.0 — 2026-10-20

- lweb：深查页加「恢复上次能用的设置」（要 core 1.12.0）：列出和这张卡在这个国家上次确认能用的那套哪里不一样、每项能不能自动退回、改 APN 要另外打勾，以及逐项进度。
- lweb：新增只读页「系统 → AI 代理权限」（/tools/agent）：代理的令牌（只显示名字和只读/可改动，不显示令牌本身）、可以读取和可以改动的地址、能改的范围（APN、CHILL）及每 30 分钟一次的限速、上次改动时间、都不能自动退回，权限文件上次修改时间。页上不能改。

## lweb 1.9.2 — 2026-10-20

- lweb: 改动记录里 AI 代理那几行写清授权从哪来：它自己的 act 授权，还是你在网页/触屏批准了它的提议（另外照旧写能不能自动回滚）。

## core 1.11.4, touch 1.2.1 — 2026-10-20

- core: 只读场景给触屏的短句（`say`）：`slow_signal` 放在 `/api/diagnose`，`hot_drain` / `long_idle` 放在 `/api/health` 的 `notices`（不算检查、不发告警）。
- touch: 网络诊断页在重拨那行下面显示「信号强但网速慢」一句；健康页在检查后面列出核心的提示（● 标记，点开看全文）。核心是旧版就什么都不显示。

## core 1.11.3、lweb 1.9.1 — 2026-10-20

- core: `perf.slow_strong_signal` 多了被动入口：信号强（按深度检测自己的信号规则看实时读数）、下行 ≥ 1 Mbit/s 持续 60 秒、又没有深度检测可读时，建议跑一次。从不判定「慢」，也不发任何东西。
- lweb：诊断页显示这条建议。

## core 1.11.2 — 2026-10-20

- 小时账只在电量计电流为正时才算充电秒数（这台设备放电时电池块的 `charging` 也会置上），和小时功耗同一规则。

## lweb 1.9.0 — 2026-10-20

- `/agent-api` 的每个回应都带 `device_id`（`/data/luyaz/agent.id` 里的 12 位随机十六进制，0600，第一次回应时生成），供 `luyaz-mcp enroll` 钉住设备（多台 U60，u60-mcp.md R2）。

## core 1.11.1、lweb 1.8.1 — 2026-10-20

- core: `slow_signal` 按深度检测自己的制式规则认服务小区（NR/5G/SA_* 和 ENDC 不再被当成 `cell_changed`）。
- lweb：`long_idle` 的说明改成提示重启核心。

## core 1.11.0、lweb 1.8.0 — 2026-10-20

- core: 决策层三个只读场景（§4.11），证据在 `/api/diagnose`：`power.hot_drain`（本次开机最后一个完整小时对比小时账里条件相同的小时）、`usage.long_idle`（在家、熄屏、无终端的两个安静小时）、`perf.slow_strong_signal`（基于用户自己的深度检测）。从不执行动作、不发探测。
- lweb：诊断页解释这三个场景。

## core 1.10.0 — 2026-10-20

- 告警短信可以在签名后带设备名（`/data/alerts/sms-name`，或 PUT /api/alerts/sms 的 `"name"`）：【Luya-Z】U60Pro全球版：… ；超过 70 字的作为一条长短信发出

## core 1.9.0、lweb 1.7.0 — 2026-10-20

- core: `radio.band_flap` 证据，来自 rmnet 上 rtnetlink 的地址掉线（频段/小区/SINR 取信号块，SSR 取 crashwatch），在 `/api/diagnose` 的 `data.band_flap`。只读。
- lweb：诊断页显示 band_flap 说明；Wi-Fi 确认框按来源带 `remote.wifi_change` 风险提示（先查回滚，再看终端数）。

## core 1.8.0、lweb 1.6.0、scripts 1.0.5 — 2026-10-20

- core: R18-P0 一批：`expect_device_id`、代理权限、DNS 存活检测绑定蜂窝接口（不走默认路由）、事务探测三态（本地 socket 失败算未知）、锁外探测结果只有 15 秒内重读到同一 SIM、接口、连接才采用；核心 LAN 监听（:9461，没有调用方）改绑 127.0.0.1。
- lweb：配合 R18-P0 的改动。
- scripts：体检只在读不到网页设备 ID 文件时才显示一行。

## core 1.7.2 — 2026-10-20

- 退回 ENDC 后保留的 NR 会话不再显示成旧的。
- 决策闭环：人批准的重拨事务确认、绑定蜂窝口的 HTTPS 复查也通过（记为已恢复）时，提议对应的那条问题立刻以 `resolved` 关闭，不再等两次被动检查；不加 24 小时抑制。其他结果照旧等被动检查。

## core 1.7.1 — 2026-10-20

- 决策闭环：启动时把重启前没结束的重拨尝试跟到结束（记为不明、不复查），不会一直卡在 `attempt_running`。

## core 1.7.0、lweb 1.5.0、touch 1.2.0 — 2026-10-20

- core: `/control` `loop.approve {proposal_id, params_digest}`：屏幕或网页上的人批准当前显示的重拨提议；不是同一条或已用过的拒绝（409 `proposal_changed` / `already_used`），智能体永远不能批准。写前现读一次制式，事务结束后做绑定 HTTPS 复查并记录结果。
- lweb：诊断页未关闭的问题下面加批准卡片（「重拨」，按两次）。
- touch：诊断页加重拨按钮（点两次）。
- 需要：lweb 1.5.0、touch 1.2.0 需要 core 1.7.0。

## core 1.6.11 — 2026-10-20

- eSIM：切换 profile 后等数据 90 秒（B31 上漫游 profile 要 55–60 秒）。
- LTE 锁小区真机测过，标成 `captured_and_proven`；制式事务时限 180 秒；PCI 检查范围。

## core 1.6.10 — 2026-10-20

- `data.reconnect`（重拨）只给决策闭环用；自动执行要过白名单，白名单出厂为空。

## core 1.6.9 — 2026-10-20

- 内容相同的 rmnet `RTM_NEWLINK`（驱动每秒重复好几次）不再改变连接代次；真的链路、标志、地址、路由变化照样会。

## core 1.6.1–1.6.7 — 2026-10-19/20

- 由 netlink 和 ubus 订阅得出连接、SIM、无线代次；O2 复查：绑定蜂窝口的多目标 HTTPS、20 秒内经运营商 DNS 解析、时钟要两家不同机构一致才可信；新增 `o2-check` 子命令。

## lweb 1.4.1 — 2026-10-19

- 远端 Wi-Fi 回执把 `op_id` 放进 `params`，核心才读得到（第一次真机远端测试因此以 `no_receipt` 退回）。

## core 1.6.0、lweb 1.4.0 — 2026-10-19

- core: 远端 Wi-Fi 回执：从局域网外改 Wi-Fi，读回对上后等页面的 `op.receipt` 60 秒，没收到就退回原设置（不看全局自动退回开关）。
- lweb：经 Tailscale、子网路由或来源不明时，Wi-Fi 写入要求回执、页面自动送出，确认框会说明。

## core 1.6.8 — 2026-10-20

- 5G 锁小区（`cell.lock_nr`）真机测过（锁到不存在的小区，到时限经原厂恢复默认退回），标成 `captured_and_proven`：闭环来源的 5G 锁小区也会自动退回。LTE 锁小区没测，仍是 `unknown`。

## core 1.6.5 — 2026-10-20

- 锁频（`band.set_lte`、`band.set_nr`、`band.reset`）真机测过（好的切换确认、锁到没有的频段到时限退回），标成 `captured_and_proven`：闭环来源的锁频也会自动退回了。制式、锁小区仍是 `unknown`。

## core 1.5.8 — 2026-10-19

- 制式：三个「自动」取值（`WL_AND_5G`、`TCHGWL_5G`、`NETWORK_auto`）在事务里算同一个值。写 `TCHGWL_5G` 固件读回 `WL_AND_5G`，以前好的切换或退回会被判成 `manual_change` / `not_applied`。

## lweb 1.3.1 — 2026-10-19

- 确认框里的远程警告按动作说会不会自动退回：网络模式、锁频、锁小区跟着设备的自动退回设置说，其他写入说不会自动退回。

## core 1.5.7 — 2026-10-19

- 触屏来的写入在事务闸门前最多等 4 秒，触屏（对 `/control` 只等 5 秒）能看到「忙」的回答。

## lweb 1.3.0 — 2026-10-19

- 按连接路径给远程警告（R18-P0-04）：经别的 Tailscale 节点子网路由进来的浏览器不再算本地；顶栏分「经 Tailscale 远程访问」「经子网路由远程访问」「来源不明」，可能断网的确认框写明没有自动退回时怎么在本地接管。
- 错设备保护：每页带上这台设备的随机 ID（`/data/luyaz/web-device-id`）；从另一台 U60 打开的页面发来的写入会被拒绝，提示「页面来自另一台设备」。

## touch 1.1.4 — 2026-10-19

- eSIM 核心给的是上次的列表时显示「卡片还在稳定，列表稍后更新」，拿到新列表前不能切换。

## lweb 1.2.4 — 2026-10-19

- eSIM 卡片还在稳定时，列表显示「这是 N 秒前的列表，正在更新…」并锁住；这种列表不算写入的回读。

## sentinel 1.0.8 — 2026-10-19

- recover-live 上限的判断和核心共用（`luyaz_wifi::check_slot`），有了测试。

## core 1.5.6 — 2026-10-19

- doctor 有命令超时时多一行 `timeout`，写明哪些命令；recover-live 上限的判断和哨兵共用（`luyaz_wifi::check_slot`）。

## scripts 1.0.4 — 2026-10-19

- `luyaz-ship.sh recover-live` 先拿 `recover.lock`（`flock -n`），第二个回 3；后台起的程序不再继承锁的 fd。（R9）
- `web-auth.sh` 只读 `LUYAZ_WEB_PASSWORD`（`ZTE_AGENT_PASSWORD` 的退路删了；config-backup 还原旧备份时照样改写成新键）。`luyaz-recover.sh` 不管有没有 `.migrated`，都不放回旧路径的核心程序和起 `zwrt-datad` 的 rc.local（10-19 已用 `install-recover` 装到设备）。

## touch 1.1.3 — 2026-10-19

- eSIM 切换后数据没回来显示「已切换，数据还没恢复」；等切换的上限 240 s。（R5）
- `start.sh` 退役，touch 上机只带 `luyaz-ui`。下一次 touch 上机前先 ship scripts（在那之前执行器还要 `start.sh`）。

## sentinel 1.0.7 — 2026-10-19

- 命令超时杀整个进程组；自己起的 recover-live 跑过 15 分钟就杀掉。（R8、R9）

## core 1.5.5 — 2026-10-19

- eSIM 卡上同时只有一个调用（轮询读取被占时回上次的列表，不排队）；lpac 跑在自己的进程组里，输出上限 1 MB，stderr 最后一段拼进错误；切换后卡还没稳时给上次的列表（`stale`）；切换按 卡 → 身份（45 s）→ 数据（60 s）检查，数据没回来是警告、不重启。doctor 每条外部命令 10 s 截止。自己起的 recover-live 跑过 15 分钟就杀掉。（韧性审计 R2–R5、R8–R10）

## lweb 1.2.3 — 2026-10-19

- USB 页「快速充电宝」一行：能力只是尚未验证时，原来的提示（「先在 USB-C 口插上手机」「18W 快充…」）照常显示，「尚未验证」另起一行；1427ef5d9 起这句说明把原提示盖掉了。
- 只从 `/data/luyaz/web.env` 读 `LUYAZ_WEB_PASSWORD`（旧键退路删了；设备上的文件 F7 已改写）。

## core 1.5.4 — 2026-10-19

- 外部命令不会再卡住核心：eSIM 探针、`simreset`、ubus、modem 重启都有截止时间，超时整组杀掉；`iw` 读终端限 2 s、同一时间只跑一份（忙时回上次结果）；`chill.sh` 超时（任务 5 分钟、home-exit 重载 60 s）后放开 CHILL 工作槽。见 docs/designs/resilience-audit-2026-10-19.md。

## scripts 1.0.3 — 2026-10-19

- 仓库里的文件名跟设备一致（改名 F7）：`luyaz-ship.sh`、`luyaz-recover.sh`、`core-trial.sh`、`web-auth.sh`（原 `agent-auth.sh`；scripts 组件带 `web-auth.sh`）。`/data/luyaz/web.env` 里的密码键是 `LUYAZ_WEB_PASSWORD`；没有新键时 `web-auth.sh`、装机包和 config-backup 仍认 `ZTE_AGENT_PASSWORD`，恢复旧备份写新键。装机包里的目录叫 `scripts/`（原 `guard/`）。

## touch 1.1.2 — 2026-10-19

- 只是内部名字：和核心通信的数据模块叫 `core_link`（原 `datad`），渲染场景 `core-down`（原 `datad-down`）。

## lweb 1.2.2 — 2026-10-19

- 从 `/data/luyaz/web.env` 读 `LUYAZ_WEB_PASSWORD`；设备上的文件改写之前，没有新键时还读 `ZTE_AGENT_PASSWORD`。

## sentinel 1.0.6 — 2026-10-19

- 交接清单写 `core`，不再写 `datad`（核心两个都不读：做的事不变）。

## core 1.5.3 — 2026-10-19

- 最后一批旧名去掉（改名 F7）：快照的版本块叫 `core`（原 `datad`），`/control` 的回复是 `"busy":"core"` / `"core":"down"`，netwatch 事件 `core_lost` / `core_back`。长期层不再写 `g_datad`（它一直和 `g_core` 相同），写 `cpu_core`（原 `cpu_datad`）；成绩单只认 `g_core`、`cpu_core`、`core_degraded`、`rss_core`、`rss_ui`：上机后把设备上的历史文件改写成新名。

## scripts 1.0.2 — 2026-10-19

- 上机执行器、init、chaos、agent-auth、fallback、config-backup 只认新布局和新组件名（`core`；`datad`、`guard`、`uid`、`--convert`、`move-d4` 都删了）。上机前设备清单先改写成 `core`。

## lweb 1.2.1 — 2026-10-19

- 不再退回读 `/data/zte-agent.env`。

## sentinel 1.0.5 — 2026-10-19

- 不再写 `/tmp/luyaz/.migrated`，也不认旧日志名和 `comp=datad` 的上机事务。

## core 1.5.2 — 2026-10-19

- 只认现在的名字（Luya-Z 改名 F5/6）：组件叫 `core`、程序 `luyaz-core`，设置在 `/data/luyaz/core/core.env`（原 `datad.env`）。待机帐本去掉已退役的 `zte-agent` 一栏；longterm 不再盯 `u60-uid`、旧 guard 脚本和 `datad_degraded`。

## core 1.5.1 — 2026-10-19

- doctor 检查项 id 换成现在的名字：`core-http`、`web-http`、`dup-luyaz-core`、`dup-luyaz-ui`（原 `datad-http`、`agent-http`、`dup-zwrt-datad`、`dup-u60pro-devui`）。核心自己的崩溃告警叫 `core-crash`；旧的 `datad-crash` 记录照样显示原来的文字和短信（Luya-Z 改名 F4）。
- doctor 不再检查已退役的东西：`svc-zte-agent`、`svc-zwrt-datad`、`svc-u60-guard`、`svc-u60-uid`、core-move、tmp-move 和旧进程名。已退役告警（`agent-*`、`datad-degraded`）的文字、短信和 `datad-crash` 旧名对照都删了；设备上的旧记录在上机时改写成 `core-crash`。

## scripts 1.0.1 — 2026-10-19

- 删除 `doctor-legacy.sh`；`doctor.sh` 只转给核心的 doctor。
- doctor-legacy.sh 用和核心一样的新检查项 id；`chaos.sh` 认 `core-crash`。
- 装机包不再往设备放 `start.sh` 和它的 rc.local 钩子，并退役 `/data/plugins/u60pro-devui/`（原厂界面备份对过 md5 后搬到 `/data/luyaz/ui/vendor-devui.orig-backup`，开机记录一起搬）。
- doctor-legacy.sh 的重复实例检查标签和 core 1.2.1 一样。
- `VERSION` 随脚本放到 `/data/luyaz/scripts/`，设备上能读到脚本的版本。
- ship 检查界面异常时，触屏崩溃那一行新旧两种名字都认（`u60pro-devui pid … ended:` 和 `luyaz-ui pid … ended:`）。要先于 sentinel 1.0.2 上机。

## sentinel 1.0.4 — 2026-10-19

- 开机记录由哨兵自己写（`/data/luyaz/ui/boot-trace.log`，格式同 `start.sh prep`），不再跑 `/data/plugins/u60pro-devui/start.sh`。

## sentinel 1.0.3 — 2026-10-18

- 触屏的崩溃记录写到 `/data/crashlog/luyaz-ui`。

## core 1.5.0 — 2026-10-18

- 帐本和报表的键改用新名（`luyaz-core`、`luyaz-ui`；RSS 列 `rss_core`、`rss_ui`），旧记录（`zwrt-datad`、`u60pro-devui`、`rss_datad`、`rss_devui`）照算。核心的崩溃记录写到 `/data/crashlog/luyaz-core`，并把 `/data/crashlog/u60pro-devui`、`zwrt-datad` 搬进新目录（每轮检查都搬，不会重算）。

## core 1.4.4 — 2026-10-18

- 深查各行改成大白话：信号行只说强弱和干扰（原来写不下），限速行列出基站给的载波，其他行去掉网卡名和协议词。

## touch 1.1.1 — 2026-10-18

- 蜂窝页的测速行在没打开过测速页时不再显示「服务不可用」。

## core 1.4.3 — 2026-10-18

- 设备上显示「测不了」的深查行现在有结果：CHILL 开着时外网、DNS 照样探测蜂窝出口；设备时间用 HTTP `Date` 核对；只有 QCI 没有 AMBR 时说「没报限速」；原因代码改成文字。

## core 1.4.2 — 2026-10-18

- 深查的 eSIM 通道检查在设备上能跑了（原来用了设备上没有的 `timeout`，结果总是「不知道」）。

## core 1.4.1 — 2026-10-18

- 深查的 SIM 行改读 modem 状态；设备上卡已就绪、数据也通时它说「SIM 卡还没就绪」。

## touch 1.1.0 — 2026-10-18

- 诊断页显示新增的几行和灰色「没测」样式。

## lweb 1.2.0 — 2026-10-18

- 诊断页显示新增的几行和灰色「没测」样式。

## core 1.4.0 — 2026-10-18

- 深查第二版：新增 SIM（eUICC 上另查 eSIM 通道是否占满）、承载（地址、路由、rp_filter）、经设备自己 dnsmasq 的 DNS、端到端请求（用 Finding 路径的探测）和时钟几行；前面的行不通时，后面依赖它的行不发包、显示灰色「没测」，主句就是第一个真正的原因。Finding 探测在跑时不再被当成「正在诊断」。

## core 1.3.4 — 2026-10-18

- 被看门狗杀掉的 lpac 留下的 eSIM ISD-R 通道会被关回去：modem 不会自己回收（实测两次超时就占满所有空通道，之后 eSIM 操作全部失败，要重启才好）。需要带 `close` 的 `qmi_uim_probe`（`scripts/qmi_uim_probe.c`）；旧版 probe 照旧不关。

## core 1.3.3 — 2026-10-18

- 改 Wi-Fi 设置要等正在跑的 hostapd 广播的名称和 uci 一致才算确认，不再只看 ENABLED：原厂重载晚约 40 秒生效，旧 hostapd 还以旧名称报 ENABLED，网页在改动生效前就报完成。

## core 1.3.2 — 2026-10-18

- 只是核心重启（开机超过 5 分钟）、且当前情景（如在家）本来就关了 AP 时，情景引擎的 bootsafe 不再把 Wi-Fi 打开——以前每次上机都会开约 100 秒。整机开机照旧修复。

## core 1.3.1 — 2026-10-18

- 编辑没在用的 APN 配置、密码留空时不再把密码清掉：`modify_manu_apn` 整条覆盖，核心现在把当前密码一起送回去（10-18 真机发现）。

## lweb 1.1.4 — 2026-10-18

- 暂不开放更改局域网设置（/router/lan）：在设备上 `router_set_lan_para` 会整机重启，而且值只进 `zwrt_router.dhcp`、不进 `dhcp.lan`。确认框改成「U60 会整机重启」。`LUYAZ_WEB_LAN_WRITES=1` 可重新打开（浏览器检查用）。

## touch 1.0.2 — 2026-10-18

- LTE-NSA 下 NR 那一路没有 SINR 时，摆放模式改用 LTE 锚点的 SINR，不再显示「--」（[#4](https://github.com/faying/luya-z-manager/issues/4)）。

## lweb 1.1.3 — 2026-10-18

- 登录页用回网站的无衬线字体（之前字体变量只有别的页面设，登录页退成衬线字）。
- 首页载波行在 1280 px 以上不再重叠（小区 ID 一律另起一行）。
- QCI 页打开只显示核心的 `qos` 块；承载表（经 AT 读，约 12 秒）按「读取承载表」才读，页面开着不再轮询。
- `:9090` 请求头 10 秒内没收完就断开，每个地址同时最多 64 条连接。

## core 1.3.0 — 2026-10-18

- `doctor --report 24h|7d [--summary]` 由核心自己出（和 `/v2/scorecard` 同一张成绩单）；doctor.sh 薄壳不再为它转给 doctor-legacy.sh。

## core 1.2.1 — 2026-10-18

- doctor 的重复实例检查改叫「核心实例数」「触屏实例数」，不再显示旧程序名；检查项 id（`dup-zwrt-datad`、`dup-u60pro-devui`）不变。

## sentinel 1.0.2 — 2026-10-18

- 日志、告警和崩溃记录里的触屏名字从 `u60pro-devui` 改成 `luyaz-ui`。Requires: ship 检查认两种崩溃行名字的那版 scripts（旧的 ship 脚本认不出新的崩溃行）。

## touch 1.0.1 — 2026-10-17

- 「载波」卡片的 QoS 行按核心的 `bearer` 标 QCI 或 5QI（不看当前制式），AMBR 写成上限、带单位（「上限 1668/1008 Mbps」），`stale` 时标「（旧）」。
- 系统页加「组件版本」卡片；「检查更新」让核心立刻去查。

## lweb 1.1.2 — 2026-10-17

- 信号页和 QCI 页按 `qos` 块的 `bearer` 写标签（NR PDU 会话写 5QI，EPS 承载写 QCI）；信号页在 stale 时标「旧承载」。

## core 1.2.0 — 2026-10-17

- 找 AT 端口时跳过被别的进程打开着的节点、不往里写；回答以整行的终结码结束（`OK`、`ERROR`、`+CME ERROR:`、`+CMS ERROR:`）；AT 出错带 `kind`（`busy`、`held`、`timeout`、`error`、`no-port`）。AT 终端拒发 `ATD…`、`ATA`、`AT+CHUP`。
- QoS 按 WAN 的 IPv4 和 key.log 的 `CONNECTED … addr=` 行对上当前连接：别的连接的承载、或当前连接已不在读到的那段日志里，都标 `stale`；有几个数据 DNN 时取当前连接的。只有数字的会话 AMBR 单位码也能解（TS 24.501）。`qos` 块 stale 时诊断不下「限速」结论、不记 `qos_change`；诊断里 5QI 标成 5QI。
- `qos` 块的 QCI/5QI 用厂商的当前值（`zwrt_data_tmp.wwaniface1.qci`，走 ubus 读）；读不到才用 key.log 里的。

## core 1.1.3 — 2026-10-17

- CHILL 的退回读回原值就算完成（不再等探测；闲置暂停期间的退回不再记成 `rollback_failed`）。

## core 1.1.2 — 2026-10-17

- 闲置暂停期间（息屏、没人连）Agent 改 CHILL 回 `chill_off`，不再重启 mihomo 进一个必然测不通的试用。

## lweb 1.1.1 — 2026-10-17

- `chill_off` 给 Agent 的是「拒绝：CHILL 关着，别重试」。

## core 1.1.1 — 2026-10-17

- CHILL 没在跑时 Agent 改 CHILL 回 `chill_off`（不再是 `busy`）。

## sentinel 1.0.1 — 2026-10-17

- `agent-change` 告警不发短信。

## lweb 1.1.0 — 2026-10-17

- 设备信息页加「组件版本」一组和「检查更新」按钮。
- `agent.token` 第三列 `read|act`、`agent.act-allow`、`/agent-api/act/*` 和 `/agent-api/action-url`；APN 页「Agent 与漫游」按卡设置。

## core 1.1.0 — 2026-10-17

- 「已注册但不通」：被动读数连续断满 60 秒才去探测（试点 OQ2：7.5 天信号日志，正常重连 ≤10 秒）；短暂中断不再用掉 10 分钟的探测冷却。
- `state.set_interval` 算只读：亮屏、灭屏不再清掉全部缓存、全量重读。
- `GET /v2/versions` 列出每个组件装着的版本和本仓库 `versions.json` 里的最新版本；核心每天查一次（真漫游时不查；`LUYAZ_VERSION_CHECK=0` 关掉），`POST /v2/versions/check` 立刻查。不会自动安装。
- Agent 改动（MCP 第二期）：`approved_by=mcp` 只能改 `apn.*` 和 `chill.profile`/`chill.exit`；一律自动退回；APN、CHILL 各 30 分钟一次；漫游中且没标「不算贵」的卡只给网页链接；人的写会取消 Agent 的试用；每次 Agent 改动记一条 `agent-change` 告警（不发短信）。新增 `GET/PUT /api/agent/roam-cheap`。
- 退回途中核心重启不再丢掉退回：读回还不是原值就重发，有时限。
- `/api/netinfo/apn` 的切换改成一个 `apn.use` 事务、按真正的调用方写；按卡记住的 APN 等试用通过才写。

## core 1.0.0、lweb 1.0.0、sentinel 1.0.0、touch 1.0.0、scripts 1.0.0 — 2026-10-17

基线：第一次编号。到这里为止的全部内容（一个 Cargo workspace、Luya-Z 命名、设备上 `/data/luyaz/` 的布局）
对每个组件都算 1.0.0。
