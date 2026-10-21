# Contributing

**English** · [中文](CONTRIBUTING.zh-CN.md)

Bug reports and pull requests are welcome.

Read [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md) for the layout of the
workspace, and the device rules in [CLAUDE.md](CLAUDE.md) before running
anything on a U60: some mistakes there (disabling stock daemons, writing
partitions) can stop the device from booting.

## Building

Everything targets `aarch64-unknown-linux-musl` and is built with
`cargo-zigbuild`, either installed locally or through the image in
`onboard/build.Dockerfile`.

```sh
cargo zigbuild --profile release-device --target aarch64-unknown-linux-musl -p luyaz-core
cargo zigbuild --release --target aarch64-unknown-linux-musl -p luyaz-web
```

The touch UI has its own script: `touch-ui/rs/build.sh device`.
Getting a build onto a device is described in [DEPLOY.md](DEPLOY.md).

## Checks before a pull request

- `cargo fmt` and `cargo clippy` are clean for the crates you touched.
- `cargo test -p <crate>`; for luyaz-web page changes also `crates/luyaz-web/e2e`.
- Changes to the install scripts: run `onboard/test/sandbox.sh run` against
  your own device.
- Say in the pull request what changed and how you tested it (on a device if
  you could).

## Issues

Fill in the template: firmware version (for example `B31`), the component
versions, and logs or `./install.sh doctor` output.
**Remove IMEI, ICCID, phone numbers and public IPs from anything you paste.**

See also the [code of conduct](CODE_OF_CONDUCT.md).
