# 更新记录

每个组件有自己的版本号（[语义化版本](https://semver.org/lang/zh-CN/)）：
补丁号 = 修错；次版本号 = 新功能（旧设置和接口照样能用）；主版本号 = 要迁移设置，或者几个组件要一起换。

| 组件 | 是什么 | 版本号写在 |
|---|---|---|
| `core` | `luyaz-core`，核心服务（`:9460`） | `crates/luyaz-core/Cargo.toml` |
| `lweb` | `luyaz-web`，管理网页（`:9090`） | `crates/luyaz-web/Cargo.toml` |
| `sentinel` | `luyaz-sentinel`，屏幕主人和看门狗 | `crates/luyaz-sentinel/Cargo.toml` |
| `touch` | `luyaz-ui`，触屏界面 | `touch-ui/rs/devui/Cargo.toml` |
| `scripts` | 设备脚本（体检、启动脚本等） | `touch-ui/scripts/VERSION` |

每个程序用 `--version` 报版本，例如 `luyaz-core 1.0.0 (6e606ec)`。
平时写在 **Unreleased** 下，格式 `- <组件>: 内容`（例如 `- touch: …`）；`tools/luyaz bump <组件> <patch|minor|major>` 会把它们挪成新的版本段。`Requires:` 写需要的其他组件最低版本。

英文版是 [CHANGELOG.md](CHANGELOG.md)。

## lweb 1.13.0 — 2026-10-21

- 有设备经 Tailscale 持续访问管理接口时，首页出一条横幅（谁、哪个口、多久、息屏约多耗 0.3–0.6 W），带「是我自己的」（放行 24 小时）和「知道了」；设置页列出白名单，可「改成永久」「删除」。Requires: core 1.15.0。

## core 1.15.0 — 2026-10-21

- `power.tailnet_poller`（docs/designs/tailnet-poller.md）：息屏不充电时，长期层每次检查（60 秒）读一次 conntrack，看 tailnet 对端和子网路由后面的对端进到 :80 / :9090 的流；同一对端连续 10 分钟每分钟 ≥ 20 包就开一次事件、出一条 `tailnet-poller` 告警。`GET /api/poller`（`/api/diagnose` 里也有 `tailnet_poller`）；`/control` `poller.allow`（默认 24 小时，`forever` 永久）、`poller.unallow`、`poller.ack`；白名单 `/data/luyaz/core/poller-allow.json`。
- 情景引擎下一轮会读到手改的 `/data/scenario/scenarios.json`（看修改时间和长度）：读得懂、验证通过就换上并记一行；文件不见、读不了或验证不过就保留原来的设置，只记一次。网页自己写的不算手改。

## Unreleased

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

- core：「恢复上次能用的连线」第一部分（还没有按钮）。你按深查之后，多跑一次绑蜂窝的 HTTPS + DNS 检查（约 20 KB），通过就按「卡 × 国家」记下当时能用的蜂窝设置（锁小区、频段、制式、APN）。`GET /api/verified` 显示现在哪些不一样；`/control verified.restore`（只认触屏和网页，代理不能用）逐项用普通事务改回去，哪一步没确认就停。

## lweb 1.10.0 — 2026-10-20

- lweb：深查页加「恢复上次能用的设置」（要 core 1.12.0）：列出和这张卡在这个国家上次确认能用的那套哪里不一样、每项能不能自动退回、改 APN 要另外打勾，以及逐项进度。
- lweb：新增只读页「系统 → AI 代理权限」（/tools/agent）：代理的令牌（只显示名字和只读/可改动，不显示令牌本身）、可以读取和可以改动的地址、能改的范围（APN、CHILL）及每 30 分钟一次的限速、上次改动时间、都不能自动退回，权限文件上次修改时间。页上不能改。

## lweb 1.9.2 — 2026-10-20

- lweb: 改动记录里 AI 代理那几行写清授权从哪来：它自己的 act 授权，还是你在网页/触屏批准了它的提议（另外照旧写能不能自动回滚）。

## core 1.11.4, touch 1.2.1 — 2026-10-20

- core: 只读场景给触屏的短句（`say`）：`slow_signal` 放在 `/api/diagnose`，`hot_drain` / `long_idle` 放在 `/api/health` 的 `notices`（不算检查、不发告警）。
- touch: 网络诊断页在重拨那行下面显示「信号强但网速慢」一句；健康页在检查后面列出核心的提示（● 标记，点开看全文）。核心是旧版就什么都不显示。

## core 1.11.3、lweb 1.9.1 — 2026-10-20

- core：`perf.slow_strong_signal` 多了被动入口：信号强（按深度检测自己的信号规则看实时读数）、下行 ≥ 1 Mbit/s 持续 60 秒、又没有深度检测可读时，建议跑一次。从不判定「慢」，也不发任何东西。
- lweb：诊断页显示这条建议。

## core 1.11.2 — 2026-10-20

- 小时账只在电量计电流为正时才算充电秒数（这台设备放电时电池块的 `charging` 也会置上），和小时功耗同一规则。

## lweb 1.9.0 — 2026-10-20

- `/agent-api` 的每个回应都带 `device_id`（`/data/luyaz/agent.id` 里的 12 位随机十六进制，0600，第一次回应时生成），供 `luyaz-mcp enroll` 钉住设备（多台 U60，u60-mcp.md R2）。

## core 1.11.1、lweb 1.8.1 — 2026-10-20

- core：`slow_signal` 按深度检测自己的制式规则认服务小区（NR/5G/SA_* 和 ENDC 不再被当成 `cell_changed`）。
- lweb：`long_idle` 的说明改成提示重启核心。

## core 1.11.0、lweb 1.8.0 — 2026-10-20

- core：决策层三个只读场景（§4.11），证据在 `/api/diagnose`：`power.hot_drain`（本次开机最后一个完整小时对比小时账里条件相同的小时）、`usage.long_idle`（在家、熄屏、无终端的两个安静小时）、`perf.slow_strong_signal`（基于用户自己的深度检测）。从不执行动作、不发探测。
- lweb：诊断页解释这三个场景。

## core 1.10.0 — 2026-10-20

- 告警短信可以在签名后带设备名（`/data/alerts/sms-name`，或 PUT /api/alerts/sms 的 `"name"`）：【Luya-Z】U60Pro全球版：… ；超过 70 字的作为一条长短信发出

## core 1.9.0、lweb 1.7.0 — 2026-10-20

- core：`radio.band_flap` 证据，来自 rmnet 上 rtnetlink 的地址掉线（频段/小区/SINR 取信号块，SSR 取 crashwatch），在 `/api/diagnose` 的 `data.band_flap`。只读。
- lweb：诊断页显示 band_flap 说明；Wi-Fi 确认框按来源带 `remote.wifi_change` 风险提示（先查回滚，再看终端数）。

## core 1.8.0、lweb 1.6.0、scripts 1.0.5 — 2026-10-20

- core：R18-P0 一批：`expect_device_id`、代理权限、DNS 存活检测绑定蜂窝接口（不走默认路由）、事务探测三态（本地 socket 失败算未知）、锁外探测结果只有 15 秒内重读到同一 SIM、接口、连接才采用；核心 LAN 监听（:9461，没有调用方）改绑 127.0.0.1。
- lweb：配合 R18-P0 的改动。
- scripts：体检只在读不到网页设备 ID 文件时才显示一行。

## core 1.7.2 — 2026-10-20

- 退回 ENDC 后保留的 NR 会话不再显示成旧的。
- 决策闭环：人批准的重拨事务确认、绑定蜂窝口的 HTTPS 复查也通过（记为已恢复）时，提议对应的那条问题立刻以 `resolved` 关闭，不再等两次被动检查；不加 24 小时抑制。其他结果照旧等被动检查。

## core 1.7.1 — 2026-10-20

- 决策闭环：启动时把重启前没结束的重拨尝试跟到结束（记为不明、不复查），不会一直卡在 `attempt_running`。

## core 1.7.0、lweb 1.5.0、touch 1.2.0 — 2026-10-20

- core：`/control` `loop.approve {proposal_id, params_digest}`：屏幕或网页上的人批准当前显示的重拨提议；不是同一条或已用过的拒绝（409 `proposal_changed` / `already_used`），智能体永远不能批准。写前现读一次制式，事务结束后做绑定 HTTPS 复查并记录结果。
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

- core：远端 Wi-Fi 回执：从局域网外改 Wi-Fi，读回对上后等页面的 `op.receipt` 60 秒，没收到就退回原设置（不看全局自动退回开关）。
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
