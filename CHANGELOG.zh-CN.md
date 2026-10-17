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

- core: `GET /v2/versions` 列出每个组件装着的版本和本仓库 `versions.json` 里的最新版本；核心每天查一次（真漫游时不查；`LUYAZ_VERSION_CHECK=0` 关掉），`POST /v2/versions/check` 立刻查。不会自动安装。
- lweb: 设备信息页加「组件版本」一组和「检查更新」按钮。
- touch: 系统页加「组件版本」卡片；「检查更新」让核心立刻去查。
- scripts: `VERSION` 随脚本放到 `/data/luyaz/scripts/`，设备上能读到脚本的版本。

## core 1.0.0、lweb 1.0.0、sentinel 1.0.0、touch 1.0.0、scripts 1.0.0 — 2026-10-17

基线：第一次编号。到这里为止的全部内容（一个 Cargo workspace、Luya-Z 命名、设备上 `/data/luyaz/` 的布局）
对每个组件都算 1.0.0。
