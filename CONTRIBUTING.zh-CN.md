# 参与贡献

[English](CONTRIBUTING.md) · **中文**

欢迎报 bug、提 PR。

先看 [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md) 了解工作区的结构；在 U60 上动手之前，先读 [CLAUDE.md](CLAUDE.md) 里的设备规则——有些错误（停用原厂守护进程、写分区）会让设备开不了机。

## 编译

目标都是 `aarch64-unknown-linux-musl`，用 `cargo-zigbuild` 编，本机装或者用 `onboard/build.Dockerfile` 的镜像都行。

```sh
cargo zigbuild --profile release-device --target aarch64-unknown-linux-musl -p luyaz-core
cargo zigbuild --release --target aarch64-unknown-linux-musl -p luyaz-web
```

触屏界面有自己的脚本：`touch-ui/rs/build.sh device`。
怎么装到设备上见 [DEPLOY.md](DEPLOY.md)。

## 提 PR 前检查

- 改到的 crate 跑过 `cargo fmt`、`cargo clippy`，没有警告。
- `cargo test -p <crate>`；改了 luyaz-web 页面再跑 `crates/luyaz-web/e2e`。
- 改了装机脚本：对着自己的设备跑 `onboard/test/sandbox.sh run`。
- PR 里写清改了什么、怎么测的（能上真机最好）。

## 提 issue

按模板填：固件版本（如 `B31`）、各组件版本、日志或 `./install.sh doctor` 的输出。
**贴出来的内容先去掉 IMEI、ICCID、手机号、公网 IP。**

另见[行为准则](CODE_OF_CONDUCT.md)。
