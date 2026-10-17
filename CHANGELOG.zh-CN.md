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

- touch: 系统页加「组件版本」卡片；「检查更新」让核心立刻去查。
- scripts: `VERSION` 随脚本放到 `/data/luyaz/scripts/`，设备上能读到脚本的版本。

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
