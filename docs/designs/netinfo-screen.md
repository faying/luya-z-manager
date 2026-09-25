# 设计：触屏「网络」页 — IP 归属地、运营商/漫游、手动选网、客户端流量、邻区

由 /office-hours 生成，2026-09-24 · Status: 已上机（2026-09-25，见文末「真机验证」） · Mode: Builder
涉及仓库：source/manager（zte-agent + web）、source/touch-ui

## 问题
屏幕上缺这些信息：当前 IP 的归属地、原始运营商和注册运营商的对照、漫游状态、手动选网、每台已连设备的流量、邻区和调度信息。其中邻区和调度现在读不到，原因是 `ui.c:1882` 依赖的 `/modem/*` 接口只存在于原作者的私有版本，Rust 版 datad 没有这些接口。

## 现状（本地代码核对）
- 注册运营商、PLMN、漫游：datad `/state.net.operator/plmn/roaming` 已有（`rust/src/state.rs:281-283,542`）；漫游没上屏。
- 原始运营商：只有 IMSI、`mdm_mcc/mnc`，没有运营商名；`touch-ui/src/data.c:112` 有中国大陆的 MCC/MNC 对照表。
- 手动选网：zte-agent 已有 `/api/modem/scan`、`/scan/status`、`/scan/results`、`/register`、`/register/result`（`zte-agent/src/modem_ext.rs:50-88`）；管理网页「移动网络」页在用。
- 邻区：zte-agent 已有 `/api/cell/neighbors/scan|nr|lte`（`cell.rs:35-54`，走 ubus `nwinfo_scan_nbr`）；datad 的 diag 邻区模块默认关着。
- 客户端流量：没有采集。候选来源 `zwrt_router.api router_get_clients_traffic`（未实测），兜底用 `iw station dump`。
- 公网 IP 和归属地：没有。
- 调度（MCS/RB/BLER）：原厂 ubus 没有这类接口，只能靠 diag 日志解析。

## 已定的决定
- **两个 IP 都显示**：蜂窝直连出口一行，CHILL 出口一行；CHILL 关着时合并成一行。（D1）
- **归属地只查外部 IP 查询接口**：只查 IP 本身，IP 变化或每 10 分钟查一次，息屏时不查，失败显示「—」，不重试风暴。
- **原始运营商**用本地 MCC/MNC 对照表来查（扩展到台、日、新、美等地区），不发 AT 指令读 SPN。
- **手动选网**：屏幕上先确认「搜索会断网 1–3 分钟」；注册失败自动回到自动选网；始终保留「恢复自动」按钮。
- **客户端流量**：先在真机实测 `router_get_clients_traffic`，等耗电测试结束再做。
- **邻区**：用原厂接口手动扫描，不开 diag 常驻采集。
- **调度信息这次不做**：记进 TODOS，放到稳定性与耗电专场之后。（D2）
- **实现方案 B**：统一放进 zte-agent，触屏和管理网页共用。（D3）
- 不采用：A 屏幕自己取（网页以后要重做，C 写网络请求太脆）；C 放进 datad（违背 datad 不加外部依赖的原则）。

## 推荐方案（B）
**zte-agent 新模块 `netinfo`**
- `GET /api/netinfo`，返回：
  - `direct:{ip,geo,isp}`：agent 直接查询即可。CHILL 的 TUN 只接管 `br-lan`（`include-interface`），本机自己的连接本来就不进 mihomo，不需要绑定网口。
  - `proxy:{ip,geo,node}`：经 CHILL 模板新增的本机入口（`listeners` 里的 `netinfo`，127.0.0.1:7894，直接交给「🚀 节点选择」）查询；CHILL 关着时为 null。
  - `home_operator`：由 IMSI 前缀和对照表得出。
  - `serving_operator`、`plmn`、`roaming`：从 datad `/state` 读。
  - `clients[]`：mac、名称、rx/tx 字节数、速率。
  - 各字段带 `fetched_at`，标明数据是什么时候取的。
- 后台缓存加刷新策略：IP 变化或每 10 分钟刷新，屏幕熄灭时暂停；外部请求超时 5 秒；接口地址可配置，便于更换，不写死单一来源。
- 选网保护：`POST /api/modem/register` 之后轮询结果，失败或超时就用 `AT+COPS=0` 切回自动；另加 `POST /api/modem/netselect/auto`。（原稿写的 `nwinfo_set_netselect` 是错的：它只收 `net_select`，改的是制式偏好。）
- 邻区：包一层现有 `/api/cell/neighbors/*`，把最近一次扫描结果缓存下来给触屏用。

**触屏**
- 新增「网络」页：两行 IP 和归属地、原始/注册运营商、漫游标记、「选网」入口（搜索 → 列表 → 确认 → 结果，外加「恢复自动」）、客户端流量列表、邻区（点一下扫描）。
- 按 DESIGN.md 和新设计的样式做，不拿掉已有的内容。
- 调度信息那一栏的提示改成「本机数据服务未提供（规划中）」。

**管理网页**：首页加「网络身份」卡，内容是两个 IP、运营商和漫游；客户端页加流量列。

## 待确认的问题
- 用哪家归属地接口：国内直连用 myip.ipip.net 还是 ip-api，要在设备上两个都测。
- `router_get_clients_traffic` 实际返回什么字段，是累计值还是速率。
- 手动扫描邻区会不会打断数据连接。
- 注册到外网 PLMN 以后，`scenario.rs` 的出国逻辑会不会被误触发。

## 成功标准
- CHILL 开着时，屏幕上两行 IP 的归属地不同且都正确；关着时只显示一行。
- 插外国 SIM 或漫游时，原始运营商和注册运营商都显示正确，漫游标记亮。
- 手动选网注册失败后，60 秒内自动恢复成自动选网，数据连接恢复。
- 新模块在空闲、亮屏时，平均每分钟外部请求少于 1 次，耗电专场的基线没有明显变化。

## 依赖
- 耗电测试会话结束后才能碰设备。
- zte-agent 在 Docker 里交叉编译；触屏二进制按旁路测试流程上机。

## 下一步行动
等耗电测试结束后，在设备上跑一次只读探测，把结果存进 `source/manager/docs/designs/netinfo-probe.txt`，给 `/plan-eng-review` 用。探测内容：
- `ubus call zwrt_router.api router_get_clients_traffic`
- `ubus call zwrt_wlan get_assoc_info`
- 分别直连和经 mihomo `curl` 一次归属地接口

## 实现记录（2026-09-24，本地，未上机）

编码时对照代码，纠正了原稿的三处：
1. **恢复自动选网用 `AT+COPS=0`**，不用 `nwinfo_set_netselect`（原因见上）。ubus 没有「自动选择运营商」的调用。选网方式用 `AT+COPS?` 读（0 自动、1/4 手动）。
2. **直连出口不绑网口**；**CHILL 出口靠模板新增的本机入口**。模板原来没有 mixed 端口，没有它就查不到。已用 mihomo 实测：配置校验通过；把规则设成全部拒绝，经这个入口仍然走「🚀 节点选择」。注意：`mihomo -t` 不检查 listener 里写的组名存不存在。
3. **zte-agent 只有 2 个 HTTP 线程**：所有慢的事（归属地查询、AT、搜网、邻区扫描）都放在一次性后台线程里做，GET 直接返回缓存。只有有人来问时才刷新（最多 20 秒一轮），所以息屏时不会发出任何请求。

选网保护判断成功的依据是「注册网络的 PLMN 等于目标，并且数据连接已通」，不看结果字符串（它的取值没有确认过）。明确失败时马上回到自动，45 秒超时也回到自动。最坏情况是 45 + 3（一次轮询）+ 8（`AT+COPS=0`）+ 2（读回）= 58 秒，满足「60 秒内恢复」。

| 位置 | 内容 |
|---|---|
| `zte-agent/src/netinfo.rs` | 模块本体和 15 个单测（全量 68 个测试全过，交叉编译通过） |
| 选网保护能跨过重启 | 注册前写 `/data/netinfo.guard`（目标加开始时间），结束时删掉；agent 启动时如果它还在，就接着做保护（`resume_guard`） |
| `ubus.rs` | `call_with_timeout`：netinfo 的 ubus 调用都带 `-t 3`，防止模组搜网时卡住，把线程占满 30 秒 |
| `at_cmd.rs` | 全进程一把锁，同一时间只发一条 AT 指令。以前两个 `cat` 同时读同一个 tty，会互相吃掉对方的回复 |
| GET 不再起子进程 | 本地状态（3 次 ubus 调用）也挪进后台刷新，最多 10 秒一轮 |
| 错误文字截到 60 字 | C 端的缓冲区是定长的 |
| `zte-agent/src/chill.rs` | `running()`、`main_exit_chain()`（沿主组逐级找到实际节点） |
| `server.rs` | `/api/netinfo`、`/api/netinfo/scan`、`/api/netinfo/neighbors/scan`、`/api/modem/register`（改为经保护）、`/api/modem/register/guard`、`/api/modem/netselect/auto` |
| `scripts/chill/template.yaml` | `listeners: netinfo`（127.0.0.1:7894） |
| touch-ui `src/netinfo.c`、`include/netinfo.h` | 客户端；`tests/netinfo_test.c` 用 agent 格式的 JSON 测解析，在 ASan/UBSan 下 26 项全过（`scripts/test/netinfo/run.sh`） |
| touch-ui `src/ui.c` | 功能页新增「网络」磁贴（第 5 行，性能测试挪到它后面），新增「网络」子页；「信令读取」的说明改成调度「本机数据服务未提供（规划中）」 |
| touch-ui `tests/render` | 假数据、expect、基准图（只有 func/cell/net 三页有变化） |
| web 首页 | 「网络身份」卡（两个出口、原始/注册运营商、漫游、选网），30 秒轮询 |

邻区扫描在触屏上也是两步确认，因为它会不会打断数据还没实测。CHILL 的 `lint.sh` 和 `chill-unit.sh`（88 项）都通过。

还没做的：
- 客户端流量：等探测结果。
- 网页客户端页的流量列：同上。
- 网页截图验证：当时本机还没有浏览器驱动；后来改用 Playwright e2e（连假 agent）截图和验证。

上机时要注意：
- 上机顺序是：zte-agent（procd 重启）、CHILL 模板、触屏（按旁路测试流程）。
- 模板换了以后必须 stop 再 start CHILL，然后观察两轮健康检查。只换文件不会重新加载。

## 上机前的探测（只读，结果存 netinfo-probe.txt）
- `ubus call zte_nwinfo_api nwinfo_get_netinfo`：确认字段 `rplmn_num` 或 `rmcc`/`rmnc`、`simcard_roam` 的取值、`network_provider_fullname`。
- `ubus call zwrt_zte_mdm.api get_sim_info`：确认 `sim_imsi`、`sim_states`。
- `AT+COPS?` 的原样输出。
- `ubus call zwrt_router.api router_get_clients_traffic`、`ubus call zwrt_wlan get_assoc_info`。
- `nwinfo_m_netselect_status`、`nwinfo_m_netselect_contents`、`nwinfo_m_netselect_result` 在空闲时的输出（不发起搜网）。
- `nwinfo_get_nr5g_nbr_contents`、`nwinfo_get_lte_nbr_contents` 在空闲时的输出。
- 模组搜网时 `nwinfo_get_netinfo` 要多久（决定 3 秒的 ubus 超时是否够用）。
- 在设备上直连 curl `https://myip.ipip.net/json` 和 ip-api；新模板上机后，再经 127.0.0.1:7894 curl 一次。
- **需要用户点头才做的**：搜一次网（会断网 1–3 分钟）；扫一次邻区（看会不会断数据）；手动注册一次，验证失败后能回到自动。

## 第二轮（2026-09-24 晚，按用户反馈）

用户的要求：
- 运营商、IP 归属地、漫游在首页就显示；
- 网络相关内容放到情景下面；
- 情景能手动选。

用户的选择：
- 做「情景页 + 保留网络磁贴」；
- 手动选的情景一直保持，点「自动」才恢复。这和情景引擎现有的固定（pin）行为一致，包括重启后保持、看门狗接管时暂停。

改动：
- **首页新增「网络」卡**，放在信号卡下面：
  - 注册运营商和漫游来自 datad，不多发请求；
  - IP 和归属地来自 agent 缓存，30 秒读一次，并带 `?lite=1`，这样 agent 不会为首页去读选网方式（AT 指令）；
  - 点这张卡进网络部分。
- **情景卡随时都能点**，进「情景 · 网络」页顶部。
  - 以前只有在国外时能点，点开是 CHILL 出口面板。这个面板现在挪到页里「CHILL 出口」这一行，功能没少。
  - 卡片上的提示从「点这里改」改成「点开可改」。
- **「情景 · 网络」页**（原来的「网络」页）：
  - 最上面是情景：「自动」加上每个情景，点两次固定；
  - 在国外时多一行「CHILL 出口」；
  - 看门狗接管或引擎停用时，有文字说明；
  - 下面是原来的网络内容。

  「网络」磁贴直接跳到网络部分。
- **agent**：
  - `/api/netinfo` 多了 `scenes` 段（`scenario::picker`），包含情景列表、当前情景、固定的情景，以及是否被看门狗接管；
  - 固定情景直接用现有的 `POST /api/scenario/pin`；
  - `?lite=1` 给首页用。管理网页首页也改成 lite，并去掉了「选网」那一行。
- **「✓」在设备字体里没有**，会显示成方框，已经改成只用强调色表示。渲染测试的缺字检查没发现这个问题，以后别用这个字符。

## 真机验证（2026-09-25）

### 已上机
- zte-agent（`/api/netinfo`、选网保护、搜网）
- CHILL 模板（7894 本机入口，热重载生效）
- 触屏（首页新层级、「情景 · 网络」页）
- 管理网页（`netinfo-web` 分支，建在 `newdesign-web` 之上）

真机上两个出口的实测：蜂窝直连在「中国 上海 · 电信」，CHILL 出口在「台湾 新北市」，节点名里的国旗已转成国家码。

### 搜网
- 格式：`m_netselect_contents` 是字符串，形如 `状态,名字,PLMN,制式;…`，例如 `2,CT,46011,11;3,CMCC,46000,7;`。
  - 状态：1 可用，2 当前，3 禁止。
  - 制式：2 = 3G，7 = 4G，11 = 5G。
- 过程：搜网时状态是 `manual_selecting`，搜完变成 `manual_selected`。整个过程约 110 秒都断网，断完模组会自己拨回来。

### 选网保护
测试方法：用电信卡手动注册到移动（46000），这注定会失败。
- 第 6 秒识别到失败（结果为 `"0"`）；
- 第 38 秒回到自动选网，第 41 秒数据拨上；
- 共 44 秒，满足「60 秒内恢复」。

失败期间 `network_type` 会变成 `LIMITED_SERVICE_SA`。触屏已按「受限服务」处理，不会显示成 5G。

之后（06:25）再读，制式偏好 `net_select` 是 `TCHGWL_5G`；早上探测时是 `WL_AND_5G`。中间只做过这次手动注册和 `AT+COPS=0`，推测是注册时带的制式改了它，没有再确认。`TCHGWL_5G` 是「全部制式 + 5G」，也算自动，网络正常，就没改回去。固件 `/usr/bin/zte_topsw_nwinfo` 里一共 14 个取值，agent 只收这 14 个；网页和触屏把两个自动值都显示成「自动」。

### 邻区：停用
原厂的 `nwinfo_scan_nbr` 一调用，2 秒内数据就断了，而且模组不会自己重拨。当时 `roll_connect_status` 停在 `init`，要手动调 `set_qcliiface` 才恢复，总共断了约 10 分钟。扫完什么数据都拿不到，两个邻区字段都是空字符串。

处理：
- `/api/netinfo/neighbors/scan` 和旧的 `/api/cell/neighbors/scan` 都返回 410；
- 触屏上写明「原厂扫描会断网且拿不到数据，已停用」；
- 邻区以后要换数据来源，比如 datad 的 diag 模块。

### 每台设备的流量
`router_get_clients_traffic` 在这台固件上调用失败。改成读 `iw dev <ap> station dump`，设备名从 `/tmp/dhcp.leases` 查。
- 只统计 Wi-Fi 设备，网线和 USB 连的设备不在里面；
- 只在「情景 · 网络」页打开时才读；
- 速率是两次读数的差。

验证时处于「在家」情景，Wi-Fi 是关的，所以还没见过真实数据，只有单测。

### codex 对抗审查（两轮）
第一轮报了 13 条（6 条 P1），都已修复：
- 搜网、注册、恢复自动改成同一个操作状态，用一把锁完成「检查并占用」；
- 注册先写保护标记，写不进去就拒绝；
- 请求只要可能已经发出，就一直保护到底；
- 恢复自动会无限退避重试，并且把「要恢复自动」这个意图写进标记，重启后能接着做；
- 重拨合并成一次，并要求连续两次读到「已连接」才算恢复；
- HTTP 线程里不再等待外部进程；
- AT 端口探测也走串行锁。
