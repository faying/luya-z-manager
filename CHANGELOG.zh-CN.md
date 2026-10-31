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

- kit: 写任何东西之前先查空间（`/data` ≥ 载荷 + 2 × 快照 + 16 MB，`/etc` ≥ 2 MB）。第一次装机（没有进度或 `KIT_FRESH=1`）把装机包要写的东西快照到 `/data/luyaz/kit/snap`（记下哪些原来就有，状态按临时文件 → sync → mv 写）；第一份快照一直留到被 undo。新增 `./install.sh undo [--ssh]` 按快照退回：原来有的放回、原来没有的删掉，`rc.local` 先 `sh -n`，中途断了可以重跑；默认保留 SSH，`--ssh` 才一起退；只换文件（不停不起服务，不碰自动升级、基带、eSIM）。不自动：默认仍是续装。离线测试 `onboard/test/kit-undo.sh`。

- core：eSIM 切换后数据回来了，再做一次只读的真上网确认（ext-1030 §3.1）：绑 `rmnet_data0` 和它的源地址、用运营商 DNS（不经 CHILL TUN、不经 mihomo），明文 HTTP 取 connect.rom.miui.com 和 www.gstatic.com 的 `generate_204`，连接 2 秒、总共 4 秒；隔约 6 秒连续 3 次通才算 `ok`，失败重新计，30 秒后不再开始新的探测。job 加 `net_check`（`ok` / `no_internet` / 没有源地址时 `skipped`——不知道，不算正常）和 `net_attempts`；`no_internet` 时 `reason` 也是 `no_internet`，job 仍是 `done`。不写 APN、数据开关、IMS、基带；切换以外不探测。`deadline_s` 加 35 秒。
- core：采样少读（ext-1030 §3-A–C）。`uci show` 和采集用的 `uci get` 按包记住结果，`/etc/config/<包>` 和 `/tmp/.uci/<包>` 的（mtime、纳秒、大小、inode）不变就复用——有未保存改动的包以前每轮都 fork `uci show`，现在也不用了；子进程失败不记。温区扫描 5 秒内复用（温控照旧自己读）。`/proc/net/tcp` 每轮读一次，原来两次。
- chill: 读 core 的请求 `/tmp/luyaz/heat`（`hot` = eco，`critical` = 直连，跟自己的 `CHILL_TEMP_WARM`/`CHILL_TEMP_HOT` 一样处理）；请求撤掉再过 `COOL_HOLD` 才回档。
- chill: 订阅不能再指定出口网卡（ext-1030 第 14 条）：渲染结果里 `proxy-providers` 下的 `interface-name` / `routing-mark` 一律删掉，每个 provider 补上 `override: interface-name: "", routing-mark: 0`，mihomo 下载回来的订阅也一并清空；模板自己的家里出口（`tailscale0`）不动。起核心前（ext-1030 第 16 条）：`chill0` 已经存在，或 127.0.0.1/0.0.0.0/:: 上的 9999、53（UDP）、7894、7895 被别的程序监听，就停在直连，理由 `port_busy` / `tun_busy`（隔 2 秒看两次），每 30 秒再试，不算崩溃。
- kit: 型号检查不再要求 `MU` 前面有底线（`BD_CNMU5250V1.0.0B31`、`BD_XCBZHKMU5250V1.0.0B03`），region 取 `MU` 前面那段；矩阵外的固件照样拒装。
- chill：情景关掉 Wi-Fi 时，`rules_drift` 不再每分钟报一次。原厂按接口加 `iif wlanN failed_policy` 规则，接口关了就删，接口回来时优先级跟着新的 ifindex 变。现在绑在接口上的规则（`iif`/`oif`）只有在接口在跑、而且找不到同样内容的规则时才算消失；其他规则消失照旧报。

- kit: `firmware-capabilities.matrix.json` 取代 `device/compat.list`：型号 / 地区 / 固件 → 每个组件（ssh、admin、ui、esim、chill、recover）`verified`、`code-only` 或 `unsupported`。`install.sh` 按组件各自检查：`code-only` 仍要 `LUYAZ_ALLOW_UNVERIFIED=1`，`unsupported` 跳过并说明、其余照装，一个都不剩才停。矩阵打进装机包、列在 MANIFEST.txt 里。

- kit: 装机进度按组件记在 `/data/luyaz/kit/progress`（包 id = MANIFEST.txt 的 sha256 前 16 位、组件、步骤；写临时文件再改名）。同一个包断了再跑，已完成的组件跳过、从第一个没完成的续；换包、上次已装完或 `KIT_FRESH=1` 从头来。`./install.sh status` 能看到。

- kit: `backup` 旁存 `<备份>.sha256`（`.system.tgz` 也有一份）。`restore` 推送之前先比对：不符一律停；缺旁存也停，除非 `LUYAZ_RESTORE_NO_SHA256=1`（旧备份）；推到设备后 verify、plan、restore 前再比一次。

- chill: AI 闸门（chill.env 里 `CHILL_AI_STRICT=1`）。AI 只走 🤖 AI 选的地区：CHILL 一停（崩溃、卡死、过热、暂停、息屏暂停、总开关关），在交还 DNS 的同一次 dnsmasq 重启里 AI 网域就解析不到，直到核心确认蜂窝出口就在那个地区。`ai_domain` 改用文本清单（`ai_domain.list`，同一来源），dnsmasq 的封锁文件和 mihomo 的规则出自同一份；另有内置的短清单兜底。封锁变化时 dnsmasq 用 restart 而不是 reload（reload 不重读 conf-dir）。

- kit: 给了 `OUI_TSV=`（`scripts/lanid/make-oui.sh` 生成的表）就装到 `/data/luyaz/lanid/oui.tsv`，用来显示厂商；公开构建没有这张表。

- kit: 装机包带上 lanid-parse 组件（已连设备识别），装到 `/data/luyaz/core/plugins/`，只有这一个组件；没有时核心照常跑、识别关着。`LANID_CWASM=` 指定现成的。

## touch 1.28.1 — 2026-10-31

- 内存分配器改用 mimalloc（跟 core 一样）。musl 的 mallocng 下每收一次 core 推送内存多约 13 KB（设备上每小时 3–8 MB，最高约 190 MB），要等深浅色切换重启才回落：已释放的快照缓冲区占着页不还。同样负载下现在不涨。

## lweb 1.50.0 — 2026-10-31

- 没装 CHILL（没有 `/data/chill/chill.sh`，跟 `install.sh status` 同一个判断）时，首页不显示 CHILL 卡片和落地卡的「家里出口」行，侧栏、分类页和 ⌘K 搜索也不列 CHILL；其余内容顺次补上。打开页面时判断；只有「文件不存在」才算没装。新增 `--chill-sh` / `LUYAZ_WEB_CHILL_SH`；有密码文件（设备上）默认 `/data/chill/chill.sh`，没有就不查（照常显示 CHILL）。

## touch 1.28.0 — 2026-10-31

- 没装 CHILL（同一个文件，每 30 秒看一次）时，首页情景卡没有 CHILL 一栏（Tailscale 占满整行），出口页没有 CHILL 卡片。系统页「语言 / Language」一行下面的小字改成只用当前语言（行名和两个选项仍是双语）。离屏渲染的 Wi-Fi 开关可由 `LUYAZ_RENDER_AUX` 给（截图用）。
- 温控停充、CHILL 降档时横幅说明（热 / 过热）；系统页电池行写「因为热暂停充电」。core 没有 `heat` 块时照旧看电池 ≥50 °C。

## lweb 1.49.0 — 2026-10-31

- 改动记录把连续相同的记录合成最新一条（「之前还有 N 条相同」）。APN：IPv6 一行和漫游开关写成句子（PDP 类型放括号、卡尾号放旁边）。情景：「固定为」只在固定时显示（选择器已显示「自动」）。设备信息：内核只显示版本号。网络模式：「（使用中）」代替「· 使用中」。

## lweb 1.48.0 — 2026-10-31

- 各页状态标记只给符号上色，文字用墨色。已连设备：「没连 Wi-Fi」去掉长说明（改成悬停提示），租约为空不再显示「—」。Tailscale：节点数只在状态里说一次，「状态」去掉 ⓘ。CHILL：去掉 PID 一格；息屏/手动暂停/停用时的「直连」改成中性色，不再琥珀；中文原因用全角标点。USB：状态不再带模式（选择器已显示），去掉充电器类型/充电状态原始值两行，直接供电写成文字。健康：概要里不写 0 项。

## lweb 1.47.0 — 2026-10-31

- 句子不再用中点串：信号状态副标题写成「5G SA，China Unicom，2 个 NR 载波」（去掉格数，已有单独读数），告警条「2 条新告警：…」，电池「使用电池，约可用…」，Tailscale「，中继 hkg」，情景「，已固定」。「频段」「带宽」「选网方式」去掉 ⓘ（平常词），术语保留。规则写进 DESIGN.md §5。

## lweb 1.46.1 — 2026-10-31

- 首页读数字号降一级（24/28 px），状态句（28/36 px）保持最大。

## lweb 1.46.0 — 2026-10-31

- 首页先说结论：状态句是页面最大的字，读数改为六格等大（聚合带宽不再是 64 px 大字；下载和上传合成一格「速率」，两个方向都低于 0.1 Mbps 时写「空闲」，数字放下一行）。宽屏时「详情」移到右栏。未读短信只在顶栏计一次，首页标题旁不再重复。告警条去掉核心事件编号前缀（「[fa717fe4-22962] …」）。隐藏的行不再留下 44 px 空行（没有 CHILL 时公网 IP 下方）。Wi-Fi 关闭时不显示 2.4/5 GHz 两行。

## lweb 1.45.0 — 2026-10-31

- 颜色只表示状态：正常的状态块是普通卡片（注意/出错仍是琥珀/珊瑚）；首页 CHILL/情景/Tailscale 卡和导航当前项不再有分类色；首页深绿色带去掉（登录页保留）。分组标题改成正式标题（正文字体、墨色、15–16 px），不再是灰色等宽小字；IP、PCI 等数据改用正文字体加等宽数字，等宽字体只留给终端、AT、十六进制和原始查询。
- 电池卡加「温控」行（档位、暂停充电、CHILL 省电/直连、几点起、原厂保护）。

## lweb 1.44.0 — 2026-10-31

- eSIM 改名超过 64 字节（约 21 个汉字）或有控制/文字方向字元时，问之前就说明；送出时带上当时看到的名字，期间被别处改过就直接说明。

## core 1.80.0 — 2026-10-31

- eSIM 下载出错时（lpac 的 stderr 尾部或服务器回答），激活码和确认码先换成 `[code]` 再进任务记录、网页回复和日志。加了事件日志、流量账本视图和 eSIM 回复的金丝雀测试。
- eSIM 改名也拒绝行/段分隔符和其余文字方向字元（U+061C、U+200E/200F、U+2028/2029）；带 `expect`（问的人当时看到的名字）的改名，卡上已经是别的名字时拒绝，回 409 `snapshot_changed`。
- `modems[]` 每项带 `traffic_per_module: false`（get_wwandst 不认 `subid`）。
- 设备档案的 `components[]` 加上原厂程序（`stock:zte_topsw_mdm`、`stock:zte_topsw_daemon`、`stock:diag-router`、`stock:ipacm`、`stock:qcrilnr`）的 sha256：每次开机在固件版本读到后算一次，按版本串缓存到 `/data/luyaz/core/stock-components.json`（大小和修改时间不变就直接用缓存；读不到的文件省略）。版本串只用来选缓存，能力仍靠运行时探测，指纹不变。

## sentinel 1.2.2 — 2026-10-31

- 按名字找进程时记住上次的 pid，先核对 comm 和启动时间，对不上才整个扫 /proc。

## lweb 1.43.1 — 2026-10-31

- 图表页速率改用 core 的厂商计数（`live.traffic`），跟首页、触屏一致；原来加总 rmnet 计数，漏掉 IPA 硬件转发的热点流量（直连客户端大流量时严重偏低）。

## core 1.79.1 — 2026-10-31

- 用户吞吐最后的退路只读 `rmnet_data0`（没有再读 `rmnet_ipa0`），不再两者相加（同一份流量算两次），方向也改对（WAN 侧 rx 就是下行，之前跟 LAN 侧一样对调了）。
- 待机卡信号采样在没有待机卡时停住，不再每 30 秒醒一次。
- eSIM 改名拒绝控制字元和文字方向覆盖字元，卡上没有的 ICCID 直接拒绝（404）；写完从卡上读回，名字不对或其他 profile 变了就报失败。

## lweb 1.43.0 — 2026-10-30

- lweb：`:9090` 加第二道门（调研 backlog C3）：只有连到的本机地址属于 `lo`、`br-lan` 或 `tailscale0` 的连接才服务（`--ifaces` / `LUYAZ_WEB_IFACES`，`*` = 不限；本机回环一律放行）。蜂窝那一侧（`rmnet*`）即使防火墙漏了也直接关掉、不回任何内容；Tailscale（从 `tailscale0` 进来，或经子网路由连 `br-lan` 的地址）照常。读不到网卡列表时拒绝。被拒的连接每分钟最多记一行日志（带次数）。`luyaz-web mock` 不受限。

## core 1.79.0 / lweb 1.42.0 / touch 1.27.0 — 2026-10-30

- core：漫游时也能完整测速（net-performance R11）。`/api/speedtest/start` 收 `roaming_ok: true`（默认 false）；漫游时带了就跑完整测试，结果标 `roaming_full: true`（中途停止也保留）；不带照旧降成粗测（`roaming_capped`）。只有管理网页和触屏两步确认后才带，而且只管这一次。
- core：网络记录仪第二批（net-performance 2a）。每行加 `rmnet_data0`、`chill0` 的 root qdisc 计数（`q_rmnet` / `q_chill0`：drops、overlimits、requeues、backlog、qlen；每行 `d_q_*_drops`），一次 `RTM_GETQDISC` 取得；再加 WAN 侧 ESTABLISHED 连接的 `tcp` 摘要，来自 sock_diag `INET_DIAG_INFO`（连接数、活跃数、按 socket cookie 算的重传和发送段数差值、RTT 中位数/最大、最小 min_rtt、cwnd 和对端窗口、扫描耗时）。TCP 只在 `rmnet_data0` 有流量时扫，最多 5 秒一次（上次超过 5 ms 就 10 秒）。10 秒摘要加丢包、积压和 TCP 合计。记录仪关着时一样都不读。每行变成 240 B（2 MiB 环形约 2.4 小时）；文件头改为 `NETREC2`，旧的环形从头开始。
- lweb：漫游时开始测速的确认框多「仍要完整测速」，要再确认一次（走漫游流量，最多约 750 MB）；结果标「漫游中完整测速」。
- touch：漫游时开始按钮下多「仍要完整测速」；第一下再问一次（5 秒内），第二下才开始；结果行标明。

## core 1.78.0 — 2026-10-30

- 温控（`docs/designs/thermal-care.md`）。搭充电策略的 tick 读电池和最热的 CPU zone（只读主芯片侧，不碰基带侧传感器）（每 30 秒，不另开计时器）。偏热（插电时电池 ≥44 °C）暂停充电；热（电池 ≥50 °C 或芯片 ≥80 °C）再请 CHILL 降到 eco；过热（电池 ≥55 °C 或芯片 ≥90 °C）请 CHILL 直连并开 `regress-heat` 事故（发短信）。连续两次读到才进档，降到退出线（40/46·73/51·83 °C）以下 5 分钟才退。停充走充电执行器（唯一写充电开关的地方）；手动恢复充电优先到拔电，过热档除外；电量 ≤30% 时偏热/热档不停充。偏热、热只进收件箱（`heat-protect`，不发短信）。新 `heat` 块；doctor「温度」栏。

## core 1.77.3 — 2026-10-30

- 内核日志落盘：Wi-Fi 驱动的 err 级记录（约 8 秒一条）照写，但不再立即同步。

## core 1.77.2 — 2026-10-30

- 内核日志落盘（`/data/crashcap`）只在这批有 err 及以上级别的记录、或离上次同步满 30 秒时才同步到闪存（原来每批都同步：日志一天 5–7 MB，闪存写入却约 150 MB），并去掉 Wi-Fi 驱动每秒刷的 `wifi_bs: ==========`、`wlan_hdd_fill_rate_info` 记录。

## core 1.77.1 — 2026-10-30

- `wifi_dfs` 块在 5G 信道守护两次查看之间（300 秒）不再显示过期：最长有效期改为 1200 秒，不再按三轮采集算。

## core 1.77.0 / lweb 1.41.0 / touch 1.26.0 — 2026-10-30

- core：5G 信道守护（`docs/designs/wifi-dfs-guard.md`）。5G 热点是自动选信道 + 160 MHz，落在 36 时占到 DFS 信道 52–64，那里遇到雷达会被停。core 现在看结果、不看日志字句：5G 该开却没在 ENABLED，或换了信道/频宽。开关开着（预设开；建 `/data/luyaz/core/wifi-dfs-off` 关掉）、停了 90 秒、上次的信道碰到 DFS：在 Wi-Fi 锁里经 `wifi.apply` 把 `wifi1` 换到 36 信道 80 MHz（同一种 htmode），原来的设置记在 `/data/luyaz/core/wifi-dfs.json`；30 分钟后（雷达禁用期）、没有 Wi-Fi 设备连着、也没在通话时换回去（5G 关着时只改 uci）。停了 5 分钟还没恢复：`wifi-5g-down`（发短信）；换信道本身只进收件箱（`wifi-5g-fallback`）。平时每 5 分钟看一次（停了或换过时 30 秒），内核出现雷达/DFS/CSA 字样马上看。新 `wifi_dfs` 块。还没遇过真雷达。
- lweb：Wi-Fi 页说明 5G 在 DFS 信道、已停、或暂时避开雷达（以及什么时候换回）。
- touch：Wi-Fi 热点卡片加「5G 信道」一行（信道 · 频宽、DFS、已停、暂避雷达）。

## touch 1.25.2 — 2026-10-29

- touch：被「...」截断的标签不再每次刷新都重画。LVGL 把截断后的字存在标签里，「同样的字就不重设」的比对永远对不上，每次都重设、重画（插普通 SIM 时 eSIM 页的空白提示每秒两次）。CHILL 节点页里自动选的那个组的标签也不再每次刷新来回变色。
- touch：eSIM 页的空白提示改成换行，卡片跟着变高：插普通 SIM 时那句话原来在「没有 eSIM 配置」后面就被截掉了。
- touch：`rs/still.sh` 也检查每个子页，不只五个分页（性能测试页豁免：它的跑分本来就一直重画）；温控和 Wi-Fi 连接详情之前的视图模型又能读了（`heat_*` 当作没有这块，`link_up` 当作 0）。







## core 1.74.0 — 2026-10-29

- 双卡（全球版）：固定一张电话卡，默认卡槽 1（`LUYAZ_VOICE_SLOT=2` 改用卡槽 2）。两槽都开着时，短信记录归电话卡，不归上网卡；原厂短信不带卡槽，双卡时一律算电话卡的。
- 双卡：通话中拒绝 `sim.dual`；双卡开着时 `sms.send_raw` 指定 `sim1`/`sim2` 不再把另一槽关掉，改为拒绝。


## core 1.73.0 — 2026-10-29

- core：插件操作 `power.now` 给电量和是否插着电，取上一次读电池的结果（不另外读）。

## core 1.72.0 — 2026-10-29

- core：`/v2/versions` 的组件来自一张表，触屏和网页都照它列，新组件两边都会出现。

## lweb 1.40.1 — 2026-10-29

- lweb：版本列表不认识的组件用自己的行 id，不再占用设备脚本那一行。

## touch 1.23.0 — 2026-10-29

- touch：「组件版本」卡片按核心列出的组件一行一个（卡片跟着变高）。

## core 1.71.1 — 2026-10-29

- 双卡（全球版）：切过单卡再重启后，原厂可能报两槽都开着，但没开的那槽其实没上电（`sim undetected`）；这时把上网卡切过去会超时退回，屏幕也显示假的「双卡」。现在看不到卡的槽按没开报，切过去会重下卡槽旗标。

## core 1.71.0 — 2026-10-29

- 插件操作 `sim.identity` 新增 `number`：卡里存的本机号码（MSISDN），取上一轮采集的快照、不读基带；卡里没存或不合法就不带。


## core 1.70.1 — 2026-10-29

- `net.now` 改读快照里的 `net` 栏位（信号块不在快照栏位里），插件才拿得到网络，不再总是空的。

## core 1.70.0 — 2026-10-29

- 插件操作 `net.now`：现在的制式、频段、信号格数（`{"type":"SA","band":"n78","bars":4}`；还没采集过或没服务回 `null`），取上一轮采集的快照，不读基带。


## lweb 1.40.0 — 2026-10-28

- 通用的插件设置页（`/settings/plugin`；装了声明设置的插件时，设置页里有入口）。按插件的描述显示分组、开关、值、状态行和子页，经 `/control plugin.settings.get` 读、经 `plugin.settings.set` 带着看到的版本号写。插件给的文字只当文本显示，不进任何属性。版本冲突时重读并显示现在的值；插件没及时确认的结果先读回再下结论；插件没在运行时只读，标出最后确认的时间。mock core 带一个示范插件，供浏览器检查 `e2e/plugin-settings.mjs` 使用。

## core 1.68.2 — 2026-10-28

- 管理网页「写短信」能发了。`sms.send_raw` 原来用固件会话金钥加密号码和内容，B31 固件一秒内就拒（寄件匣里没有）；现在和告警短信、插件 `sms.send`（1.57.1）一样送明文，固件回覆拒收就直接报失败，不再空等状态。

## core 1.68.1 — 2026-10-28

- 短信转发队列档案坏了不再把旧短信再转发一次。原来读坏档当成空队列（已转发短信的指纹全丢，固件重编号的短信会再发一遍），也没有告警。现在坏档改名 `.corrupt-<秒>`，改读上一份（每次存档都留 `.prev`）；两份都不能用就重建，设备上已有的短信只记指纹、不发。两种情况都发一次 `sms-forward-queue` 告警。

















## core 1.59.0 — 2026-10-27

- 插件契约升到 v1；hello 里写 v0 的插件照样连得上，v0 的操作不变。
- AT 口被命令用过一次就一直开着，上面一条读取线程在 `poll(2)` 里睡着等（基带不说话时不醒）。主动送来的行（RING、+CLIP、+CMTI、IMS 注册、通话结束）不会再在下一个命令前被清掉，而是在 core 里广播出去，夹在别的命令回复中间也照样分出来。命令没等到最终码就结束时，口记为「不干净」，下一个命令先送一个带标记的 `AT+CMEE?`，丢掉它回复之前的所有东西，迟到的 OK 不会被当成下一个命令的。口被挂断时，正在用它的命令马上失败，下一个命令重新开口。锁、25 秒总时限、卡住提示、拒绝 CFUN、终端拒绝 ATD/ATA/+CHUP、选口规则都不变。
- 响铃或通话中，其他 AT 命令（终端、USSD、STK、QoS 读取、选网）一律回「通话中，稍后再试」；通话自己的命令排在等待的命令前面。

## core 1.58.0 — 2026-10-27

- 插件 socket：`sms.remove {items}` 从固件两个库里删短信（iPhone 上删是全局的）。收到的按寄件人、时间、文字找，发出的找固件自己留的副本（tag 2：收件人和文字一样、时间 2 分钟内最近的）；先走原厂删除，剩下的再用 `sms.db_delete`，和管理网页一样。一次最多 200 条，回 `{removed, left}`。
- 插件 socket：`sms.reports {after}` 给网络回的送达报告（`{id, number, ok, at}`），从固件的 `sms_report` 表只读地读：原厂 `zwrt_wms_get_status_rpt_data` 读一次就把报告清掉。固件的 `status_report_on` 是 1 才有报告（默认关）。

## core 1.57.2 — 2026-10-27

- 短信日志：只收收到的短信（tag 0/1）。固件把这台设备发出的每条短信（tag 2）也留在同一个列表里，日志把它们当成收到的收了进去，插件发出的短信又被当成新短信送回来。发送失败（3）和草稿（4）也不收。

## core 1.57.1 — 2026-10-27

- 插件 socket：`sms.send` 改用明文的 `zte_libwms_send_sms` 参数，和告警短信同一条路；`sms.send_raw` 的加密参数固件不收。被拒时日志记固件的结果码（不记号码和内容）。

## core 1.57.0 — 2026-10-27

- 插件 socket：`sms.send {to, text}` 从现在的卡发一条短信（最多 1024 个 UTF-16 单位）。固件说发出了回 `{result:"submitted"}`，说失败回错 `send_failed`，轮询状态逾时回 `{result:"unknown"}`（可能已发出，插件不要自己重发）。错误说明里不放号码和内容。

## touch 1.21.0 — 2026-10-27

- 公开版的屏幕没有变化（采样登记照旧）。

## core 1.56.0 — 2026-10-27

- 插件 socket：`display.show {id, kind, code, seconds}` / `display.dismiss {id, result}`（插件请触屏显示的卡片，回答以 `display` 事件送回）和 `clock.utc`（设备时钟比真实 UTC 快几秒；时钟还没对过回 `clock_unset`）。屏幕没登记会显示卡片就拒绝；公开版的触屏不登记，所以 `display.show` 在公开版总是回 `screen_unavailable`。

## core 1.55.1 — 2026-10-27

- 短信日志自己的整库扫一页取 50 条（`sms.list_after` 的上限）；之前要 200 条，每次都被拒，日志只靠短信转发的读取进来。

## core 1.55.0 — 2026-10-27

- 每条新短信先写进有上限的短信日志（`/data/luyaz/core/sms-journal.json`，2000 条或 2 MB，上一份留作 `.prev`），短信转发才看得到；短信按寄件人、时间、内容认，重读卡重新编号也不重不漏。每条记的是加盐的卡身份键（`sim-salt`），不存 ICCID。插件 socket（`/var/run/luyaz/core.sock`）只在有插件用户 `phone` 时才开：核对对端 uid、契约 v0、单帧 64 KB、在途 4 笔；`plugin.settings.get`/`set` 给管理网页转送插件设置（MCP 调不到）。

## touch 1.20.4 — 2026-10-27

- 「流量」页改用「系统」页图表的样式：四张半宽卡（今天 · 昨天 / 前天 · 本月，各带右上小注），近 14 天蓝色柱、日均线、选中那天下面画线，读数在细线下面；按卡每行「用量 · 占比」。

## touch 1.20.3 — 2026-10-27

- 「流量」页：点柱子选中的就是那根（以前右边那根的放大点击区盖住了它一半）。

## touch 1.20.2 — 2026-10-27

- 「流量」页：柱图不再有压住读数行的刻度字；按卡一行写「运营商 · 尾号四位」不截断；占比不到 1% 写 <1%。

## touch 1.20.1 — 2026-10-27

- 「流量」页重做成一眼能看：今天大字＋近 7 天日均，昨天 · 前天 · 本月三格；近 14 天柱图按卡叠色、日均一条线（点柱子看那天日期、总量、比日均、分卡）；近 30 天按卡合计和占比。没记录的那天没有柱子。

## touch 1.20.0 — 2026-10-27

- 点首页「今日」格打开新的「流量」页：今天、昨天、前天、本月。昨天、前天来自 core 按卡记的账（`GET /v2/usage/days`），按设备日期对；那天没记录就写「没有记录」，不当 0；那天用过两张以上卡就分卡列（运营商 · 尾号）。今天和本月是首页同一个原厂计数。只在页面开着时读。Requires: core ≥ 1.17.0（更旧的 core：两天都显示没有记录）。

## core 1.54.1 — 2026-10-27

- `/api/netinfo/neighbors/scan` 清单为空时也读 netinfo，SA 下 `reason` 写的是 SA 那句（和 `/api/netinfo` 一样）。

## touch 1.19.0 — 2026-10-27

- 小区信息的「邻小区」改成排行：左边「名次 频段 PCI EARFCN」，右边「RSRP / RSRQ」，强的在前，标题行写个数；没有时写 core 给的原因。「扫描」按钮和它的确认去掉了（不再扫描）。读 `band`（旧 core 没有时显示制式）。

## lweb 1.38.0 — 2026-10-27

- 小区锁定页的「邻区」改成只读表格（#、PCI、频段、EARFCN、RSRP、RSRQ），来自 `/api/netinfo?lite=1`，每 15 秒更新，强的在前；没有时写 core 给的原因。置灰的「扫描邻区」按钮去掉了。

## core 1.54.0 — 2026-10-27

- `/api/netinfo` 的 `neighbors` 带上基带自己报的 LTE 邻区（uci `zte_nwinfo.manual_scan.lteg_nbr_content`，随 netinfo 每轮读一次；不扫描、不写）：`state` `ok` 时 `cells` `[{rat, band, pci, arfcn, rsrp, rsrq, sinr: null}]` 按 RSRP 由强到弱（再按 RSRQ；最多 16 个），否则 `not_reported`，原因在 `error`/`error_en`（SA 下「5G 独立组网（SA）下基带不报邻区」，其他「基带现在没报邻区」）。不再有 `unsupported`。第一项不是当前服务小区的清单（之前那个小区留下的）和 SA 下的任何清单都算没报。`/api/netinfo/neighbors/scan` 用同一套判断和排序，并多了 `reason`。

## touch 1.18.0 — 2026-10-27

- Wi-Fi 设备行加一行连接信息「协商 ↓2402 ↑1201 Mbps · Wi-Fi 6 · -39 dBm」，iw 没给的项写「读不到」（频段也是）。从 netinfo 读 `link_up_mbps`。

## lweb 1.37.0 — 2026-10-27

- `/clients` 详情面板显示每台 Wi-Fi 设备的连接：协商速率下行 / 上行（Mbps）、频段（信道、热点频宽）、信号强度（dBm 和等级）、Wi-Fi 协议。四项一直列出，驱动没给的写「读不到」，不猜。行内摘要改为同时显示 ↓ 和 ↑ 协商速率。

## core 1.53.1 — 2026-10-27

- Wi-Fi 扫描再补扫一遍已启用的被动/DFS 5 GHz 信道（5150-5730 MHz，`iw dev <iface> scan freq …`）并合并结果；普通扫描在这些信道上从来扫不到网络。

## lweb 1.36.0 — 2026-10-27

- 情景「反应快慢」表单加上三个离开参数（离开信号、闪断信号、闪断等待）；旧版 core 没有这些字段时显示默认值。

## core 1.53.0 — 2026-10-27

- 离开靠 Wi-Fi 判定的场景按信号走。一轮扫描里命中网络的最好信号弱于 `leave_rssi_dbm`（-72）就算一次未命中，家里 Wi-Fi 变弱时提前切到外出；最后看到时不弱于 `blip_strong_dbm`（-60）的网络突然消失，按路由器闪断保留 `blip_hold_secs`（300 秒）后才开始算未命中。`/api/scenario` 配置新增这三个参数（缺省用默认值，有校验）；`state.json` 新增 `last_seen_at`/`last_seen_dbm`（进入时由触发切换的那次扫描写入，刚进入就闪断也会保留）。引擎日志写明原因（`"home" weak: …`、`"home" not seen, held as a blip: …`）。

## core 1.52.0 — 2026-10-27

- 在队列里等了超过 2 分钟才转发的短信，正文会注明（「late: forwarded 3 h 05 min after it came in」）；webhook 带 `late_secs`。转发队列状态新增 `oldest_pending_secs`。
- lpac 被杀后，先重新探一次，确认它的通道还被占着才关；已经空出来的通道（或猜错的）不会被关。
- `state.set_interval` 可带 `{client, milliseconds, ttl_s}`（会过期的登记，默认 180 秒）或 `{client, release: true}`。采样节奏取未过期登记里最快的，没有就是 5 秒。触屏亮屏时崩溃不会再让核心一直每秒采；不带 `client` 的旧调用照旧。

## lweb 1.35.0 — 2026-10-27

- 只接受外人拿不到的名字（IP、单段名、`.lan`/`.local`/`.home.arpa`/`.internal`、`*.ts.net`、`LUYAZ_WEB_HOSTS`），其他回 421，堵住能绕过 Origin 检查的 DNS rebinding。
- 密码错了，同一地址下次要等（1 秒起翻倍，最多 5 分钟，另有总量上限）；输对就清零。
- 一个地址最多 16 条连接，最后 8 个名额留给本机和 tailnet；空闲的 keep-alive 连接 10 秒后关。
- 有页面开着时请核心按 1 秒采（暗屏也一样），最后一个页面关掉就撤回。

## touch 1.17.0 — 2026-10-27

- 核心节奏（亮屏 1 秒、暗屏 5 秒）改成每分钟续一次的登记，退出或交出屏幕时撤回。

## core 1.51.0 — 2026-10-27

- eSIM 下载、删除时 lpac 报错或超时，改为回读卡上的 profile 列表判定：其实已装上/已删掉就算成功，激活码不会被重试。core 在写入中途重启时，下次启动用同样方法判定。
- `wifi_post_stuck` 另在原厂 Wi-Fi 脚本启动时被唤醒（inotify，不轮询），35 秒、65 秒各看一次，空转约 1 分钟就报。仍然只读。

## touch 1.16.2 — 2026-10-27

- 情景卡「为什么」按 DESIGN §4：已固定写「已固定 · 你 10:20 手动选的」，国外写「· 到了 2 小时」（取最大整单位），不再写「· HH:MM 起」。

## touch 1.16.1 — 2026-10-27

- clippy 豁免只留在 place.rs 两处原有代码上；行为不变。

## core 1.50.2 — 2026-10-27

- `/v2/latency/direct` 和 `/v2/timeline/today` 的测量函数和长期层来源改成启动时放进服务状态，不再每次请求时读；回复不变。两个接口加了经过路由的测试，双网址测延迟的竞速也加了测试。

## core 1.50.1 — 2026-10-26

- 直连出口延迟（`/v2/latency/direct`、深查的直连探测）同时问国内的 204 和 gstatic，取先回来的；只问 gstatic 在国内直连永远不通，首页一直显示「—」。

## touch 1.16.0 — 2026-10-26

- 首页接上 core 1.50 的数据。情景卡写出是哪个 Wi-Fi、从几点起（「因为看到家里 Wi-Fi Home-5G · 08:41 起」，SSID 太长两头留），扫描指向别的情景时胶囊显示「判断中 ●●○」、为什么那行写「可能切到外出：家里 Wi-Fi N 次没看到」（`GET /api/scenario`，只在亮屏首页、最多一分钟一次）。状态卡的操作按 `story.fix` 走：「摆放模式 ›」开摆放模式，「去打开 ›」去蜂窝，diag:<层> 开诊断并滚到那一层、短暂高亮；换卡提示和没有 `fix` 时的旧规则不变。直连那几行显示直连延迟（`GET /v2/latency/direct`，亮屏首页一分钟一次；失败写「—」）。Tailscale 下面新加「今天」卡（`GET /v2/timeline/today`，亮屏首页 5 分钟一次）：24 个小时格按平均 SINR 上色、制式变化处标出，下面是最新 8 条事件和「还有 N 条」；时钟没校准时写「设备时间还没校准，时间线暂停」。旧 core 下一切照旧。Requires: core 1.50.0（新的部分）。

## core 1.50.0 — 2026-10-26

- 触屏首页要的数据。`GET /api/scenario`（要登录）加 `matched`（`{id, ssid, since}`：最近一次扫描对上的 Wi-Fi，和同一个网络从什么时候起一直对得上；不是按 Wi-Fi 判的就是 null）、`enter_hits`、`exit_misses`（和 `candidate`、`hits`、`misses` 一起画「判断中 ●●○」）；公开状态里照旧没有 SSID。`story.fix` 说首页底部按钮去哪（`sim_enable`、`cell`、`placement`、`diag:data|internet|dns|limit|crowd|link`、`op`；正常时没有这个字段），只在触屏原来有按钮的地方有。`GET /v2/latency/direct`：直连出口延迟（深查自己的 `generate_204` 探测，3 秒），只在有人问时测，60 秒内所有调用共用一次，带 `roaming`。`GET /v2/timeline/today`：当地零点以来的情景切换、蜂窝断开/恢复、开机（带原因码）、事件开始/结束、开始/停止充电、自动修复，加每小时一个点（`sinr_avg`、`rat`）；时钟不可信时 `clock_trusted:false`，长期层关着时 `available:false`。长期层新写 `charge`（保持 5 分钟才算）、`scenario`、`repair` 事件，小时行加 `sinr_avg`、`rat`。

## core 1.49.1 — 2026-10-26

- 收下哨兵的 `regress-disk` 事件（「存储快满」）；alerts：它的短信和名称。

## sentinel 1.2.0 — 2026-10-26

- core 停了 10 分钟（`regress-core-down` 打开）就重启它（`/etc/init.d/luyaz-core restart`），还没好就在 10、20、40 分钟后再试，之后每小时一次；ship 在换 core 时不动。procd 崩 5 次就放弃，之前没人把它拉回来（10-26：/data 写满崩溃循环后停了 50 分钟）。事件关闭时写明重启了几次。
- 每轮看 /data 和 /tmp：剩余空间低于二十分之一（至少 32 MB）或 inode 低于二十分之一，就打开 `regress-disk`（发短信），记下剩余和往下两层最大的五项；回到两倍以上才关闭。放在哨兵而不是 core：/data 满了停掉的正是 core。

## scripts 1.3.3 — 2026-10-26

- scripts: `luyaz-core.init` 从 core.env 读 `LUYAZ_NETREC`（记录仪开关，core 1.49.0）。

## sentinel 1.1.3 — 2026-10-26

- 屏幕日志和后台日志另存一份到 flash：`/data/luyaz/screen/log`（`<开机 id 前 8 位> <screen|worker> <原行>`，每行落盘，满 64 KB 留一份 `.old`），意外重启后还能看到重启前的记录。

## scripts 1.3.2 — 2026-10-26

- `luyaz-ship.sh` 交不回屏幕时（正式界面没起来，或哨兵没在跑），不再让屏幕一直空着、等固件整机重启（1185，u60g 2026-10-25）：屏幕上没有界面就让哨兵换上原厂界面（先 release 再 vendor），哨兵没在跑就把它起来、由它拉起界面；屏幕上已有界面就不动，上机照样按原来的原因判失败。下次 release 时如果那个原厂界面的请求还在，先发 `ui`。

## core 1.49.0 — 2026-10-26

- 记录仪 netrec（2a 第一批，docs/designs/net-performance.md「2a 记录仪细节设计」）。core.env 里 `LUYAZ_NETREC=1` 才开。挂在采集轮上，只用这一轮已读到的厂商 `get_wwandst` 计数、`rmnet_data0`、有线 + Wi-Fi 客户端表、nwinfo：不加定时器、线程和 ubus，空闲轮不多读不多写。有客户端（读不到算有）且连续 3 轮 ≥ 1 Mbps 开一次会话；连续 60 s < 0.5 Mbps、客户端变 0 或满 2 小时关。设备本机流量不计入：本机 = Δ`rmnet_data0` − Δ`chill0`，`chill0` 只在会话中或厂商速率本身到 1 Mbps 时才读。会话里每轮一行 160 字节进 `/tmp/luyaz-netrec.ring`（2 MiB，回绕，重启续写）；每 10 s 一行摘要（速率 p50/最大、直连/代理/本机字节、SNR 范围、换小区/频段/CA 事件、CPU）加开始/结束行写 `/data/luyaz/core/netrec/YYYYMMDD.jsonl`，攒到 4 KiB 或会话结束一次 fsync；每天 ≤ 1 MiB（超过只写开始/结束并标 `trimmed`），总量 8 MiB（先删最旧的一天）。不存地址；不进事件日志、不进可分享资料包。接口：`GET /v2/netrec/sessions`、`GET /v2/netrec/summary?since=&cursor=&limit=`（JSONL，`x-netrec-next`）、`GET /v2/netrec/ring?session=&offset=&limit=`。体检：开着或有文件时多一行「记录仪」（开/关、今天写了多少 KB）。

## sentinel 1.1.2 — 2026-10-26

- 同样的短信修正（时钟校好才写时间、中文 regress 短信不带英文细节）；core 停着时不再报 agent-silent（由 regress-core-down 报，Wi-Fi 救援照做）。

## core 1.48.3 — 2026-10-26

- 10-26 实机测试后的告警短信修正。时钟还停在开机时的时间（`last_wall`、SNTP 还没校）时短信不写时间（重启短信曾在 09:58 写 01:58）；中文 regress 短信不再带英文细节（原因码、版本、/healthz），只说「之前刚换过某组件，可能有关」，英文短信照旧；agent-silent 有了自己的话。

## core 1.48.2 — 2026-10-26

- 事故证据只在有信号说已恢复（`end: recovered`）时写 `recovery_s`；崩溃、重启这类窗口过完就结束（`quiet`）的不再把 6 小时等待当成恢复用时。设备上已有的证据在启动时重写。

## scripts 1.3.1 — 2026-10-25

- 哨兵收回屏幕后，`luyaz-ship.sh` 重新最多等 20 秒让正式界面起来。改名提交 c622e515 删掉了 `UID_UP`，等待变成 0 秒：界面晚起一秒，touch 上机和它的退回就都判失败（u60g，2026-10-25，随后一次意外重启）。

## core 1.48.1 — 2026-10-25

- 测速结论：「下载时卡」写明是下载时还是上传时；探测太慢、凑不满 20 个样本的阶段，只要已有至少 5 个比空闲高 100 ms 以上，也照样算（第一次实机：上传时每个探测约 1.8 秒，这一行却显示正常）。SINR 四舍五入不再显示「-0」。

## lweb 1.34.1 — 2026-10-25

- 测速页英文按 glossary §17 统一：分层区叫 "Layers"，样本不足写 "Too few samples; rough figure"。

## touch 1.15.0 — 2026-10-25

- 测速页接上测速 v2（net-performance DR1–DR13）：大数字是多连接稳定段，下面一行结论（「▲ 慢在…」/「● 没发现瓶颈」）；卡片下面加「分层」区，五行层行（代理、信号差的无线两行可点，去出口页和找位置页）；测着时阶段字带步数；样本不足时数字变灰；粗测时写「粗测」；漫游时多一行提示、要再按一次；「也测经 CHILL」开关（默认关，每次测完回到关，CHILL 没在跑或漫游时变灰）；测前写流量上限，测后写用掉多少。接旧 core 时页面不变。

## lweb 1.34.0 — 2026-10-25

- /tools/speedtest 读测速 v2 的结果（net-performance T10b）：进行中状态写步数（「3/5 · 多连接下载」）、原因行写测法，测完写结论（「完成 · ▲ 慢在单连接」），读屏只广播这两种，不广播百分比。结果墙第 4 格换成「下载时延迟」（抖动挪进详情）；结果墙下加「分层」（单连接、代理、下载时卡、无线、基站和 Wi-Fi），代理慢时整行链到 CHILL；详情加全程均值、抖动、连接数、内核调参：开 / 关（只读，测速当时的状态）。样本不足时下载数字变灰（「▲ 样本不足，数字只供参考」），粗测写「● 粗测 · 漫游中只测 5 秒」。新开关「也测经 CHILL」（每次开始后回到关，漫游或 CHILL 没在跑时不能开）；「开始」的确认写新费用（约 30 秒 / 750 MB，经 CHILL 约 40 秒 / 1.15 GB，漫游 5 秒 / 30 MB）。接 1.48.0 以前的 core 时照旧显示。完整显示要 core 1.48.0。

## core 1.48.0 — 2026-10-25

- 测速 v2（net-performance 1a，测法 2）：空闲延迟 2 秒、单连接长下载 8 秒、6 条连接下载 10 秒（同时每 200 ms 在一条专用保持连接上测延迟）、6 条连接上传 10 秒（每条循环发固定大小的 POST，只算服务器确认过的）。每段丢掉前 2 秒；`download_mbps`/`upload_mbps` 改放多连接稳定段，另有 `*_whole_mbps`、`single_mbps`、`idle_ms`/`loaded_*_ms`、`cpu_peak_pct`。请求超时跟着阶段剩余时间走（不再 3 秒重连）；每个字节先预留再读：各段 200/400/150 MB、总共 750 MB，不够时依次砍上传、单连接、多连接。稳定段不到 3 秒（粗测 1 秒）标 `sample_short`。自动选节点：延迟最低的 3 个各试下载 1 秒（2 条连接），2 秒内 0 字节的排除，选最快的；诊断的粗测（`mode:"coarse"`）30 分钟内在同一小区沿用这个节点。完整且样本够时给 `verdict`（单连接 / 代理 / 下载时卡 / 无线，中英两种）。
- `POST /api/speedtest/start` 接受 `path:"chill"`：直连多连接之后，同一节点再经 CHILL 的 7894 监听测一次（多 400 MB）；`chill_region` 只给主出口的地区名（DIRECT 时为「直连」）。CHILL 没在运行、中途停了都如实报告（`chill_not_running`/`chill_stopped`），不当成 0 Mbps，也不悄悄改走直连。漫游时（信号块的 `roaming`，与 ship 漫游闸门同一读法）自动改粗测，标 `roaming_capped:true`。`net_tune`、`radio` 记下开始时的状态。旧字段名字、类型不变；`phase` 仍是原来的取值（新子阶段在 `stage`、`step`/`steps`）。
- `GET/PUT /api/net-tune` `{"on": bool}`：写入或删除 `/data/luyaz/net-tune.want`，再跑 `net-tune.sh apply`/`restore`（子进程，8 秒上限，脚本自己等锁最多 5 秒），回复带上它的输出；GET 回 `net-tune.sh show`。

## scripts 1.3.0 — 2026-10-25

- `net-tune.sh apply [网卡]|restore|show`（net-performance 1b）：CPU 路径网络参数，`/data/luyaz/net-tune.want` 存在时生效——`tcp_slow_start_after_idle=0`、`tcp_rmem`/`tcp_wmem` 上限 16 MB、`rmem_max`/`wmem_max` 8 MB、`tcp_notsent_lowat=131072`、`netdev_max_backlog=4096`、br-lan/chill0/tailscale0 的 RPS 掩码 `e`。每项第一次见到时记原值（`/data/luyaz/net-tune.baseline`，先跟原厂值核对，不一致就不改并报告）；`restore` 把记过的项全部恢复；apply/restore 共用一把 `flock -n` 锁，最多等 5 秒。`luyaz-core.init` 在 core 起来之前跑 `apply`（子进程，5 秒上限）；`doctor.sh`（不带参数）附上 `show`。

## core 1.47.1 — 2026-10-25

- 双卡机的 APN 页跟着上网卡走：数据在卡 2 时 `get_auto_apn_list` 带 `slotId` 读（不带的话原厂回的是卡 1 的列表）；自动模式下「正在用」若不在这张卡的任何列表里（`get_apn_at_cid` 会留着上一张卡的），改用这张卡启用的候选。按卡记住候选用的 ICCID 备用读法在卡 2 时读 `sim2_iccid`。

## sentinel 1.1.1 — 2026-10-25

- 改用把 `rg-…-ask` 当成只进收件箱（同 `rg-…-close`）的 luyaz-alerts 重编。Requires: core ≥ 1.47.0（提问本身）。

## lweb 1.33.0 — 2026-10-25

- /alerts 的事故列在同一次开机、前一小时有演练时写「可能相关：演练」；详情加「可能相关的演练」；Agent API 多给 `drill`、`archived`、`recovery_s`、`boot_code`。

## core 1.47.0 — 2026-10-25

- regress 事故证据补完（设计「變差才通知」X5）：`long/incident-<id>.json` 在事故结束时再写一次（结束方式、恢复用时、收的告警），并写开机原因码、演练和两个出口（每小时 CHILL / Tailscale 开着与正常的秒数），和内存、待机耗电并列；时钟可信之前开始的事故，等时钟可信后补上前 6 小时。`/v2/incidents/<id>` 对 8 天清单放掉的事故，30 天内从证据读回（`archived`、`recovery_s`、`boot_code`）。旧 core 只在开始时写的证据，启动时补写一次。
- 演练：决策层的实机测试开关 `/tmp/luyaz-test-rx-quiet` 开着时记一次演练（`incidents.json` `drills`，最多 20 个）；同一次开机、之后一小时内开始的事故，或下一次开机初期的事故，带 `drill`（规则同 `ship`）。
- `regress-mem` / `regress-power` 事故开满 3 天，收件箱问一次「新水位要不要接受」（`rg-<id>-ask`：前后 build 和数值、`regress accept <kind>`）；从不发短信，也不会自己接受。
- `doctor --ledger-selftest` 改查 core 的长期记录目录，不再查已退役的 `/data/ledger`。

## core 1.46.2 — 2026-10-25

- 深度诊断只逐行读 7 天信号历史里的 `diagnose`、`speed` 事件；原来连这 7 天的每分钟记录也读进来再丢掉，一下子多用约 20 MB 堆（u60g 8 → 28 MB）。
- QoS 读 key.log 末尾 2 MB 时只留 `[DATA]` 行，不再每 30 秒把两份末尾整段读成文字。

## scripts 1.2.1 — 2026-10-25

- `luyaz-ship.sh` 让 `ship.log`、`test.log` 各自不超过 256 KB：暂存时（拿着锁、没有别的事务）超过的改名成 `<日志>.old`，再前一份丢掉（`U60S_LOG_MAX`）。

## core 1.46.1 — 2026-10-25

- 体检的「短信告警」在「变差」类短信关着时（`/data/luyaz/core/regress-sms-off`）给「需要注意」：只进收件箱，`tools/luyaz regress sms on` 打开；只是提醒，退出码仍是 0。

## core 1.46.0 — 2026-10-25

- 堆分配器换成 mimalloc（堆计数照旧包在外层）。用 musl 自带分配器时，8 小时内核心 RssAnon 从 33 涨到 82 MB，实际在用的堆一直是 8 MB：释放的内存没还给系统。第三方清单多了 mimalloc（MIT）。

## lweb 1.32.0 — 2026-10-25

- 告警页：状态区块和「记录」之间新增「事故 · 7 天」——core 的事故（开着的在最上，然后 7 天内结束的，最多 50 条），点开看详情（同时观察到、开始/结束、可能相关的上机、内存快照、归入的告警）。事故画在上面时，它的告警从「记录」移走，未读标记跟到事故列；读不到事故或 core 太旧时一条都不藏。状态区块有开着的事故时先讲事故。
- `/agent-api/incidents`、`/agent-api/incidents/<id>`；`/agent-api/act/regress-accept`（必须带 `instruction: owner`）。
- 需要：core 1.45.0（更旧的 core 显示「core 版本太旧，不支持事故」）。

## touch 1.14.2 — 2026-10-25

- 首页：双卡待机时状态卡也显示下载速度曲线（之前因为放不下而隐藏）；曲线留在原位，可能和速度那行稍有重叠。

## touch 1.14.1 — 2026-10-25

- 摆放模式：小区按 PCI（加 4G／5G）认，不再用「频段 · PCI」：载波聚合时基带会把 PCI 报在任一载波的频段下，同一个小区被读成 n78·603、n5·603、n1·603，还被算成「切回」。
- 摆放模式：「卡在两个基站之间」只算当前这一处（记一处就重新算，在几处之间走动不算来回切），并且分数低于 60（良好）才提示：分数够好时切换不值得为它挪。提示写出是哪两个小区：「在小区 603、615 之间来回切了 5 次，往一边挪。」
- 摆放模式：曲线上标出在用的小区（「近 3 分钟 · 小区 603」），换小区的提示写出新小区，另一小区记下的那处显示它的 PCI，不再写「另一小区」。
- 摆放模式：分数一样的几处选后记的那处，也就是上次说最好的那处（原来在第 3 处说「就放这里」，到第 4 处时第 2、3 处同分，又说「放回第 2 处」）。

## touch 1.14.0 — 2026-10-25

- 顶栏 ▲ 在 core 说有事故开着时也亮（只认新鲜数据）；点击范围 44×44，贴在速率旁边、不压速率。「健康与告警」最上面是「事故」：开着的（≤5）、24 小时内结束的（≤3）、同时观察到；数据过期、从没读到、core 太旧都写出来，不隐藏。
- 「网络诊断」的「再查一次」从标题栏移到页面里（▲ 会盖住它）。
- `luyaz-ui --hitcheck`（rs/hitcheck.sh）：逐页检查 ▲ 底下没有可点的东西。
- 需要：core 1.45.0 才有事故（更旧的 core 显示「core 版本太旧，不支持事故」）。

## core 1.45.0 — 2026-10-24

- 变差才通知（regress）：只用核心已经在读的数据判断事故，存在 `long/incidents.json`：内存（对 7 天基线的相对门槛，和绝对门槛 heap 32 MiB / RssAnon 96 MiB）、蜂窝断线 5 分钟（只进收件箱）、CHILL / Tailscale 不通 5 分钟、待机耗电（连续两个合格小时超过 600 mW 或超过基线）、意外重启（没有计划重启标记、原因码不是手动或每周定时）、告警队列里的崩溃合并成一个事故。开始发一则（短信带细节），恢复一则只进收件箱；点事件型过了窗口记「已结束」。
- `/v2/incidents`、`/v2/incidents/<id>`（告警、证据、内存快照）；`/api/alerts` 的事件带 `incident`；`/v2/state` 新增 `regress` 块（开着 ≤ 5、24 小时内结束 ≤ 3）。
- `<core> regress accept regress-mem|regress-power`、`regress limit mem|power …`、`regress sms on|off`；`/control` 新增 `regress.accept`（Agent 必须带 `instruction: owner`）。
- 成绩单在没有 `cutover` 时只读 `long/`（新装的设备不再说「账本还没有记录」）；doctor 不再照抄旧账本的 summary。
- 需要：sentinel 1.1.0（两个一起回退：短信规则在两边都有）。

## sentinel 1.1.0 — 2026-10-24

- `regress-core-down`：`/healthz` 连续 10 分钟不通（上机替换期间不算），哨兵入队并发短信；`regress-recorder`：核心在答话、`long/open.json` 却 10 分钟没更新。两者恢复时各一则只进收件箱。
- 和核心共用的短信规则：崩溃连发只发第 1/2/4/8 次、regress 告警被基带拒绝会重试、`regress-sms-off` 挡住 regress 短信、`wifi-takeover` 不再发短信。
- 需要：core 1.45.0。

## scripts 1.2.0 — 2026-10-24

- `luyaz-ship.sh` 转正那一刻把 `act_boot` / `act_uptime` 写进事务和清单的 ship 行，核心按它判断哪次上机可能相关。

## touch 1.13.5 — 2026-10-25

- 摆放模式：「卡在两个基站之间」只算 2 分钟内切回刚离开的基站（3GPP 说的乒乓切换），不再每次换小区都算：一路走过新基站不会触发，切换中途的读数（PCI 65535、没有频段）也不算。主力机一次实走记了 30 次换小区，不少是频段和 PCI 前后差一个快照更新造成的。日志里切回会标 `back`。
- 摆放模式：分数卡改成五格，每格都是「数字 · 名称 · 意思」：总分（大字，带分级）、SINR（dB，得几分）、频宽（MHz，扣几分）、信号强度（RSRP，正常／弱）、基站负载（RSRQ，不忙／忙）。总分 = SINR 得分 − 频宽扣。
- 首页：速度线在空闲时（10 KB/s 以下，也就是线的底）不画，留空；原来贴着底的一条绿线像屏幕坏了。

## touch 1.13.4 — 2026-10-24

- 摆放模式：分数改用 3GPP 的 SINR→速度曲线（TR 36.942 附录 A.2：0.6·log2(1 + SINR)，−10 dB 起算，4.4 b/s/Hz 封顶，约 22 dB 满分）× 主载波频宽比例（速度跟频宽成正比，TS 38.306 §4.1.2）。RSRP、RSRQ 不再乘进分数：SINR 已经包含信号弱带来的噪声和干扰，也没有公开模型能把它们换成速度折扣；改为用来说原因（RSRP ≤ −105 算弱，RSRQ ≤ −15 算忙）。SINR 先平滑（约 3 秒），基带读数一跳，分数是滑过去而不是跳。算式卡：SINR 得分 − 频宽扣 = 大字，后面两格用字写强度和负载（正常 / 弱、不忙 / 忙）和读数。
- 首页：状态卡下每个载波都带频宽（「n5 15M」），条按 100 MHz 画（加起来更宽就按总数），n5 15 MHz 是短条、n78 100 MHz 是满条；右边换成「查原因 ›」时频宽也还在。

## touch 1.13.3 — 2026-10-24

- 摆放模式：SINR 20 dB 就满分（原来 25 dB，室内最高 40 多分、走动几乎不变）：13 dB ≈ 60 分，每 1 dB 约 6–7 分；分级 80 / 60 / 40，差 8 分以内算一样，最好的不到 30 才「放哪都不行」。多一行说原因：扣分最多的一项和读数（「差在干扰：SINR 9.6 dB，转个方向或靠窗」）；走动时显示「比 5 秒前 +8」。记录卡写每处差在哪（「低 48 · 干扰」）。

## touch 1.13.2 — 2026-10-24

- 摆放模式：频宽只算主载波自己的（SA 是 NR 小区，4G 是 LTE 小区，NSA 是锚点 + 5G 那条）；辅载波不再算，它只在下载时才由基站加上。NSA 的信号质量只读 4G 锚点，5G 那条 SINR 时有时无时分数不再跳。算式卡改成扣分（「55 · 0 · −6 · 0」），加起来就是大字，大字改为这一秒的分数。

## touch 1.13.1 — 2026-10-24

- 摆放模式：换小区只在新的「频段 · PCI」连续两次读到、且有 PCI 时才算（偶发读数不再报「卡在两个基站之间」）；每次算到的切换写一行日志。

## touch 1.13.0 — 2026-10-24

- 摆放模式重做：0–100 分 = 这条链路最快速度的几成（SINR 定的上限 × 强度、负载、相对频宽三个折扣，用算式写出来）；状态卡底部是近 3 分钟分数走势，记下的地方钉在线上；最多比 3 处，直接说放回第 N 处、放哪都差不多、放哪都不行（换房间或换卡）、不是摆放的问题（基站忙，打开测速）。来回切基站优先提示。地方只叫「第 N 处」加时间：设备没有罗盘和运动传感器。

## touch 1.12.6 — 2026-10-24

- 系统：5 分钟网速图的读数也跟顶栏同一套写法和单位设置（按位元时刻度写 80Mb / 800Kb）。

## touch 1.12.5 — 2026-10-24

- 首页：状态卡的网速跟顶栏同一套写法，单位随设置自动换（112 Kbps，不再是 0.1 Mbps），上传也带自己的单位。

## touch 1.12.4 — 2026-10-24

- 首页：信号评语和「干扰 · 负载」改成结论右边上下两行小字（没服务时下行是多久）。5 分钟网速图从首页第二屏挪到「系统 → 近 5 分钟」，在 CPU、内存上面；首页留状态卡的波形。

## touch 1.12.3 — 2026-10-24

- 首页：信号 · 干扰 · 负载放到结论右边（「顺畅」右边原来空着）；放不下先省负载，再省干扰。腾出的一行给网速波形，波形更高。

## touch 1.12.2 — 2026-10-24

- 首页：信号词放到「干扰 · 负载」那行最前面（信号强 · 干扰中 · 负载正常），SINR 圆环居中；只有提示占了那行时信号词才放圆环下面。首页情景卡和三格去掉深色主题的 1px 顶部高光（看着像一道白线）。
- 状态栏：有 ▲ 时上传不再先被挤掉：先换成更紧的写法（不留空格、不带单位字母），最后才只留下载；▲ 只占自己的宽度。短写法在 10M 以下留一位小数，和首页一致（2.3M，不是 2M）。
- 网速图上传线改实线（虚线时被下载的填色盖住），纵轴下限降到约 1.6 KB/s（上传常贴在底线上）。

## touch 1.12.1 — 2026-10-24

- 首页电池格用电池时也写「充电中」：改按充电器判断，和「系统」一致。

## touch 1.12.0 — 2026-10-24

- 首页一屏回答三件事（DESIGN.md §4）：按结论染色的状态卡（SIM 行，双卡两行；网速加波形，或最多 3 行提示；SINR 圆环；载波和带宽，或要做的事：查原因 / 详情 / 换这张上网 / 去启用）、情景卡（情景、模式、原因；CHILL 每类流量一行带延迟；Tailscale 和到家里出口的路径）和底排（Wi-Fi · 今日 · 电池剩余到分钟，或通知占前两格）。固定版位：内容变化不推动别的块。网速图、落地和 Tailscale 明细在第二屏。首页亮着时也读家里出口状态和电池预估。

## core 1.44.2 — 2026-10-24

- 流水账合并一模一样的重复写（不占可撤销项的）：第一次照记，之后只计数，换了别的行或满一小时补一行汇总（`repeat`、`since`）。情景引擎每 15 秒保一次 `enableAutoSleep`，原来一天半就把 512 KiB 的流水账写满、挤掉真改动和 `took_ms` 样本。这个调用也不再清全部状态缓存。

## scripts 1.1.0 — 2026-10-24

- `config-backup.sh` 设了 `CB_EXPECT_SHA256` 时，归档的 sha256 不符则 verify / plan / restore 都拒绝。

## core 1.44.1 — 2026-10-24

- `/api/netinfo/neighbors/scan`：改读整个 `manual_scan` 段；该项没设（SA）时单读这一项 `uci get` 什么都不回，之前因此回 502 而不是 `not_reported`。

## core 1.44.0 — 2026-10-24

- 短信转发：持久投递队列（`/data/luyaz/core/sms-forward-queue.json`，每次变化都 fsync 保存）：每条短信×每条规则一项，各自记状态、次数和下次重试（30 秒起翻倍，最多 15 分钟）。某条规则的目标不通，不再挡住其他规则和后面的短信（「只收到一次」的原因：一次临时失败会把后面全部卡住最多 6 小时）。重启后接着发，已成功的规则不重发；短信按发件人+日期+内容认，SIM 重读改了编号也不会再发一次。在线 24 小时还没发出或队列满了就放弃（告警 `sms-forward-stuck`）；标已读/删除要等所有规则都成功，且该编号还是那条短信。`GET /api/sms/forward/config` 多了 `queue`（pending / sent / failed / next_at）。
- `/api/netinfo/neighbors/scan`（GET 或 POST）改为读调制解调器自己报的 LTE 邻区（uci `zte_nwinfo.manual_scan.lteg_nbr_content`），不再回 410：`{ok, status: "ok" | "not_reported", cells: [{pci, earfcn, band, rsrp, rsrq}]}`，去掉服务小区和载波聚合小区，超出范围的值为 `null`。不扫描、不写；SA 下为空。
- Wi-Fi 总开关（`wifi.set_module`）写完读回 `wifi_onoff_by_user`，15 秒内没变成要的值就回失败（原厂有时回成功却没动，B31 实测打开约 6 秒）。

## core 1.43.1 — 2026-10-24

- AI 闸门：🤖 AI 换地区时删掉旧的确认（否则下次 CHILL 停下会照旧放行）；在 CHILL 面板切的模式（不写模式文件）看到后写进模式文件，chill.sh 和核心看到的一致。

## core 1.43.0 — 2026-10-24

- 为两次重启之间 RSS 慢涨加堆计数：系统分配器外包一层计数；每创 4 MB 新高往核心日志写一行 `[mem] live … kB; thread …; last <路径> …s ago`，`hour_proc` 多 `anon_core`（RssAnon）、`heap_live`、`heap_peak`（kB）。不加线程、不加定时器。

## core 1.42.1 — 2026-10-24

- AI 闸门按 dnsmasq 实际生效的配置（`server=127.0.0.1`）判断「AI 经过 mihomo」，不再看 chill.state；查询期间 CHILL 已接管 DNS 时，查完不再关闸（u60g 上一次启动后被重新关上近一分钟）。

## touch 1.11.1 — 2026-10-24

- 首页「出口」行显示 AI 闸门那行时撑成两行（之前被「流量」行盖住）。

## touch 1.11.0 — 2026-10-24

- 首页「出口」行显示 AI 已暂停（出口不在 AI 策略组的地区）或已确认。

## lweb 1.31.0 — 2026-10-24

- 首页 CHILL 卡片显示 AI 已暂停（出口不在 AI 策略组的地区）或已确认。

## core 1.42.0 — 2026-10-24

- AI 闸门引擎：看 AI 是否经过 🤖 AI（CHILL 在跑且是规则模式）；不是的时候（CHILL 停了、模式是 `direct` 或 `global`）一直封着 AI 网域，直到三个国家查询（Cloudflare trace、ip-api、ipinfo；至少两个回答，且全是该地区）确认出口在 🤖 AI 的地区；确认绑定这次开机和这条蜂窝路由（重拨、换卡就重查）。只在有设备连着时查，失败退避。`services.chill.ai_gate` 显示状态。

## core 1.41.0 — 2026-10-24

- USB 代次有了来源：内核 uevent 套接字（NETLINK_KOBJECT_UEVENT）；绑定后读一次 `/sys/class/udc/*/state` 当快照才产生 USB 代次，之后每条 UDC/USB/Type-C/充电相关的 uevent 都换新值。SEQNUM 断号、丢消息或读取出错就撤销，等重新快照；原厂 Type-C/充电宝事件仍会撤销并换新值。仍不算权威。

## core 1.40.0 — 2026-10-24

- ubus 代次有了来源：监听多听 ubusd 的 `ubus.object.add`/`ubus.object.remove`；探针确认订阅后产生 ubus 代次，用到 ubus 的能力相关的对象每次登记/注销都换新值。仍不算权威。

## lweb 1.30.1 — 2026-10-24

- 调制解调器说已连接但没有 IPv4 地址时（B31 断线后还会一直这么说），顶栏和移动网络页不再显示「已连接」，改成中性的「未确认」。

## core 1.39.0 — 2026-10-24

- 开关直供时同步写 `zwrt_deviceui.Device.direct_power_mode_switch`（"1"/"0"，不同才写），原厂界面显示一致；同步失败只给警告。
- 短信转发不再只发第一条：汇总没变时直接读收件箱；日期比现在晚 2 天以上的短信不再挡住后面的新短信。
- 新增能力项 `wwaniface_trustworthy`（B31 为 Unknown：断线后 `get_wwaniface` 仍报已连接）；长期链路记录把「已连接但没有 IPv4 地址」算作断开。

## core 1.38.1 — 2026-10-24

- 有设备连着时 Finding（断网检查）永远开不起来：interfaces 块里的 LAN 流量计数被当成连接变化，每次刷新都把 60 秒计时重来。计数不再算；其他任何变化、变回原值、读失败照旧算。

## lweb 1.30.0 — 2026-10-23

- eSIM 页写明待送回执约几分钟后自动重送，或已停止自动重送、要手动发送。Requires: core 1.38.0。

## core 1.38.0 — 2026-10-23

- eSIM 运营商回执逐条发送，一台服务器不通不再拖住其他；没送出的自己重送：触发后（核心启动、切换、下载删除、网页按钮）2 分钟，之后 10 分钟、30 分钟、2 小时、6 小时，再停到下次触发。没有待送就不设定时器、不读卡。`GET /api/esim/notifications` 多了 `resend {next_s, gave_up}`。
- 告警：被杀掉的写入者留下的半行不再吞掉下一条事件，空的 `seq` 文件也不再让编号从 1 重来（与 sentinel 1.0.12 同一修复）。

## core 1.37.1 — 2026-10-23

- 决策闭环测试开关：`/tmp/luyaz-test-rx-quiet` 存在且不到 10 分钟时，收包计数在涨也不算「还在收」，真机测试才造得出断网（只删位址时计数照涨）。在内存盘上、到时自动失效；其他不变。

## sentinel 1.0.12 — 2026-10-23

- 告警：写入方被杀留下的半行不再吞掉下一条事件；`seq` 文件空了也不再从 1 重新发号（之前短信和已读游标会跳过新事件）。

## scripts 1.0.10 — 2026-10-23

- `alert-lib.sh`：同 sentinel 1.0.12 的修复（半行、`seq` 丢失）。

## lweb 1.29.1 — 2026-10-23

- 小按钮点击区保持 44 px（看起来仍是 36 px）；减少动态效果只停会移动的（开关滑块、进度条），颜色渐变保留；⌘K 搜索框聚焦时有提示。

## touch 1.10.10 — 2026-10-23

- 熄屏：熄屏期间的重画不再送到面板，1.10.9 的黑帧一直保持到亮屏。

## touch 1.10.9 — 2026-10-23

- 熄屏：关背光后给面板送一帧全黑（以前只关背光，上一屏内容在强光下还隐约看得见）；亮屏时先整屏重画再开背光，原厂按键守护进程点亮的也一样。

## lweb 1.29.0 — 2026-10-23

- `/agent-api/sim`（MCP `get_sim_identity`，luyaz-mcp 0.8.0）：卡换没换（`epoch`、`phase`）、卡状态、PIN 状态、网络名，双卡机还有每个卡槽；绝不给 ICCID、IMSI、手机号和 ICCID 尾号。

## core 1.37.0 — 2026-10-23

- 深度诊断加一次绑定蜂窝 IPv4 的多目标 HTTPS（校验证书，和路径探测并行，上限 16 秒）。「外网」在 HTTP 通、加密网站都打不开（要网页登录或被拦截）或证书校验不过时给 warn；「设备时间」先用两家 HTTPS 网站的 `Date` 判断。

## lweb 1.28.1 — 2026-10-23

- /tools/agent 写明锁频、锁小区在漫游或人在国外时不做。

## core 1.36.1 — 2026-10-23

- AI 代理的锁频、锁小区在人在国外（注册网络国家码 `rmcc` 不是 460，插当地卡也算）或读不到时也算敏感。

## lweb 1.28.0 — 2026-10-23

- `/agent-api/act/link`：AI 代理的锁频、锁小区改动，带着「主人的指令」标记转给核心；改动记录里这类改动标「Agent 执行 · 来源：你的指令（未验证）」；/tools/agent 列出「锁频 / 锁小区」范围和怎么退回。

## core 1.36.0 — 2026-10-23

- AI 代理按主人的指令（`instruction: "owner"`，没带回 403 `needs_instruction`）可以做锁频、恢复默认频段、锁小区（设备上退回已实测的动作）；漫游中或读不到漫游状态不做（敏感，给网页地址），改之前 Tailscale 不在线不做，这一类整体 30 分钟一次（层 `link`），一律试用，设备自己测到蜂窝（DNS）和 Tailscale 都在线才保留；改动记录写「来源：你的指令（未验证）」（u60-mcp.md D-g）。

## lweb 1.27.1 — 2026-10-23

- `/agent-api/no-service-history` 能到达 core 了（路径没进 core 白名单，之前回「core 没回应」）。

## core 1.35.0、lweb 1.27.0、mcp 0.6.0 — 2026-10-23

- core：原厂异常只读检测，挂在已有的分钟采样上（R18-P2-01，docs/designs/stock-anomaly-recovery.md）。`wifi-post-stuck`：ZTE 的 `zte_post_wifi.sh` 活过 30 秒且启用的 main*/guest* 数 ≠ 在跑的 wps `hostapd_cli` 数，连续两次（从不杀它）。无服务账本 `/data/luyaz/core/no-service.jsonl`（200 行）：每次失去服务记一行和期间看到的原厂动作（lpm_cycle / sim_reset / redial / unknown）；超过 5 分钟且没看到任何动作时开 `no-service-long`。两个告警都只进收件箱，不发短信。`/v2/device/capabilities` 加 `stock_anomalies`；新增 `/v2/device/no-service-history`。
- lweb：`/agent-api/capabilities` 逐字段带出 `stock_anomalies`；新增 `/agent-api/no-service-history`。需要 core 1.35.0。
- mcp：新只读工具 `get_no_service_history`。需要 lweb 1.27.0、core 1.35.0。

## lweb 1.26.1 — 2026-10-23

- 短信转发规则表单缺什么就说哪一栏（不再只说「把转发目标填完整」）；Telegram Chat ID 要是数字或 @频道名，转发号码只能是数字、前面可带 +；ntfy 服务器地址栏弹网址键盘。

## touch 1.10.8 — 2026-10-23

- 暗屏：定时刷新（和它带起的后台读取）从每秒一次改成每 5 秒一次；亮屏后回到每秒一次并马上刷新一次。core 推来的数据照样马上画（R18-P1-04）。
- 定时读取和重连 core 放到有上限的后台线程；读完有变化就叫醒界面；HTTP 读取有总时限。
- CHILL：空状态、不可用状态、长标签和点按反馈打磨。
- 主题：测试保证每种文字颜色在它所在的底色上、两种主题下对比度都 ≥ 4.5:1；玻璃描边统一由一个函数画（外观不变）。

## core 1.34.1 — 2026-10-23

R18-P1-04：core 空闲时少醒（每秒约少 2.6 次定时唤醒）；行为不变。
- 计划任务、体检：一觉睡到下次该跑，不再每 2 秒 / 1 秒看一眼。
- 充电策略：开关启动时定一次（core 运行期间不会变）。
- 情景引擎看门狗：30 秒看一次，原来 2 秒（卡死判定是 15 分钟）。
- 短信转发：`sms`、`signal`、`interfaces` 块有变化时马上醒；其余每 10 秒看一次（数据承载、换卡等待），原来 2 秒。

## lweb 1.26.0 — 2026-10-23

- lweb：/router/esim 上，切换遇到 eUICC 卡忙在重试时（core P0-3 的 catBusy 重试）显示已试几次、下次重试的时间和大约什么时候放弃（设备当地时间），并有「停止重试」按钮（第一档：只是不再等，卡上保持原来的配置；`POST /api/esim/cancel`）。按下当场回应「正在停止重试…」，随后切换的结果显示「已停止重试」。来不及（卡已经换上）或没有可停的重试都会照实说。需要：core 1.24.0（`attempts`、`/api/esim/cancel`）。

## core 1.34.0, lweb 1.25.0, mcp 0.5.0 — 2026-10-22

R18-P1-07：MCP 多两个只读工具，只用设备已有的数据（不新探测）。
- core：`GET /v2/recovery/actions`，动作表自己说：每个动作的确认规则、完成证据、退回能力和什么时候退回（闭环执行、Agent 写、人写）、谁能批准（人、Agent 的层和 30 分钟限制、闭环白名单），其余 `/control` 写记为 `direct`（不走事务、不退回），`limitations`（网页确认档位按页面定，不在表里），以及白名单本身（空）和闭环还缺的进入条件。由 `ops::spec`、`api::execute::ALLOW`、`ops::agent` 生成，静态。
- lweb：Agent 接口 `/agent-api/capabilities[?capability=<前缀>]`（被动固件能力报告按字段白名单裁剪，16 KB 以内，超了带 `truncated`；解不开的报告是 `unverified`）和 `/agent-api/recovery-actions`（旧 core：`not_described`）。
- mcp：工具 `get_firmware_capabilities`、`explain_recovery_action`；正常回答（state ok）在网关按设备复用 30 秒（`gateway_cache_age_s`）；core 挂了、unverified、not_described 每次重新问。需要：lweb 1.25.0、core 1.34.0。

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
