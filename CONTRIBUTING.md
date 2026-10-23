# How to contribute

[中文版](CONTRIBUTING.zh-CN.md) | English

Issues and pull requests are both fine.

## Before you touch a device

Two documents first: [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md) explains how the
workspace is laid out, and the device rules in [CLAUDE.md](CLAUDE.md) list the mistakes that can
leave a U60 unable to boot (for example disabling a stock daemon or writing a partition).

## Build

All binaries are `aarch64-unknown-linux-musl`, built with `cargo-zigbuild` (installed on your
machine, or from the image `onboard/build.Dockerfile` describes):

```sh
cargo zigbuild --target aarch64-unknown-linux-musl --profile release-device -p luyaz-core
cargo zigbuild --target aarch64-unknown-linux-musl --release -p luyaz-web
```

The touch UI builds with `touch-ui/rs/build.sh device`. [DEPLOY.md](DEPLOY.md) covers putting a
build on a device.

## Pull request checklist

1. No `cargo fmt` or `cargo clippy` complaints in the crates you changed.
2. `cargo test -p <crate>` passes; web page changes also run the browser checks in `crates/luyaz-web/e2e`.
3. Install script changes: `onboard/test/sandbox.sh run` against a device of your own.
4. The description says what changed and how you tested it, on real hardware if possible.

## Reporting a problem

Use the issue template: firmware version (such as `B31`), component versions, and logs or the
output of `./install.sh doctor`. **Strip IMEI, ICCID, phone numbers and public IP addresses before
pasting.**

Please also follow the [code of conduct](CODE_OF_CONDUCT.md).
