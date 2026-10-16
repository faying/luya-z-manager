# 参与贡献

欢迎提 issue 和 PR。先读 [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md) 了解三个仓库怎么配合，再读 [CLAUDE.md](CLAUDE.md) 里设备上「不能做的事」。

## 开发环境

- Rust stable + `cargo-zigbuild`（或 Docker），目标 `aarch64-unknown-linux-musl`
- Node.js + npm（只在跑 luyaz-web 的 e2e 时要：用 `web/` 的 node_modules）
- 一台 ZTE U60 Pro（MU5250），固件 B27 或更早，用装机包装好 SSH

```sh
cargo zigbuild --profile release-device --target aarch64-unknown-linux-musl -p luyaz-core   # 数据服务
cargo zigbuild --release --target aarch64-unknown-linux-musl -p luyaz-web           # 管理网页（:9090）
```

更新到设备见 [DEPLOY.md](DEPLOY.md)。

## 提交前

- Rust：`cargo fmt`、`cargo clippy` 没有警告
- 网页：`cargo test -p luyaz-web`（涉及页面行为时再跑 `crates/luyaz-web/e2e`）
- 改了装机脚本：在真机沙盒里跑 `onboard/test/sandbox.sh run`（见 DEPLOY.md）
- 尽量在真机上验证；PR 里写清改了什么、怎么测的

## 提 issue

用 issue 模板，写上固件版本（如 `B27`）、luyaz-core / luyaz-web 的提交号，附上相关日志或 `./install.sh doctor` 的输出。
**贴日志前去掉 IMEI、ICCID、手机号、公网 IP 等个人信息。**

## 行为准则

遵守 [Contributor Covenant](CODE_OF_CONDUCT.md)。
