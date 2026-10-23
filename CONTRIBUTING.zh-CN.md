# 怎么参与

中文 | [English](CONTRIBUTING.md)

提 issue、发 PR 都可以。

## 动设备之前

先读两份：[docs/GETTING-STARTED.md](docs/GETTING-STARTED.md) 讲工作区怎么组织；[CLAUDE.md](CLAUDE.md) 的设备规则列出会让 U60 开不了机的操作（比如停用原厂守护进程、写分区）。

## 编译

所有程序的目标平台是 `aarch64-unknown-linux-musl`，用 `cargo-zigbuild` 编（装在本机，或者用 `onboard/build.Dockerfile` 建的镜像）：

```sh
cargo zigbuild --target aarch64-unknown-linux-musl --profile release-device -p luyaz-core
cargo zigbuild --target aarch64-unknown-linux-musl --release -p luyaz-web
```

触屏界面用 `touch-ui/rs/build.sh device` 编。编好的程序怎么装上设备，看 [DEPLOY.md](DEPLOY.md)。

## PR 自查

1. 改过的 crate，`cargo fmt`、`cargo clippy` 都没有意见。
2. `cargo test -p <crate>` 通过；改了网页再跑 `crates/luyaz-web/e2e` 里的浏览器检查。
3. 改了装机脚本：用自己的设备跑一遍 `onboard/test/sandbox.sh run`。
4. 说明里写清改了什么、怎么测的，能在真机上测最好。

## 报问题

照 issue 模板填：固件版本（如 `B31`）、各组件版本、日志或 `./install.sh doctor` 的输出。**贴之前删掉 IMEI、ICCID、手机号和公网 IP。**

也请遵守[行为准则](CODE_OF_CONDUCT.md)。
