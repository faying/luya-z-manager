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

## Unreleased

- scripts: doctor-legacy.sh 的重复实例检查标签和 core 1.2.1 一样。
- scripts: `VERSION` 随脚本放到 `/data/luyaz/scripts/`，设备上能读到脚本的版本。
- scripts: ship 检查界面异常时，触屏崩溃那一行新旧两种名字都认（`u60pro-devui pid … ended:` 和 `luyaz-ui pid … ended:`）。要先于 sentinel 1.0.2 上机。

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
