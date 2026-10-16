# Contributing

**English** · [中文](CONTRIBUTING.zh-CN.md)

Issues and PRs are welcome. Start with [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md) to see how the three repositories fit together, then read the on-device "don'ts" in [CLAUDE.md](CLAUDE.md).

## Development environment

- Rust stable + `cargo-zigbuild` (or Docker), target `aarch64-unknown-linux-musl`
- Node.js + npm (only for the luyaz-web e2e tests: uses the `web/` node_modules)
- A ZTE U60 Pro (MU5250), firmware B27 or earlier, with SSH set up by the install kit

```sh
cargo zigbuild --profile release-device --target aarch64-unknown-linux-musl -p luyaz-core   # data service
cargo zigbuild --release --target aarch64-unknown-linux-musl -p luyaz-web           # admin web UI (:9090)
```

To update a device, see [DEPLOY.md](DEPLOY.md).

## Before submitting

- Rust: `cargo fmt`, `cargo clippy` with no warnings
- Web: `cargo test -p luyaz-web` (also run `crates/luyaz-web/e2e` when page behavior changes)
- Install scripts changed: run `onboard/test/sandbox.sh run` in a sandbox on a real device (see DEPLOY.md)
- Verify on a real device when possible; in the PR, say what changed and how you tested it

## Filing issues

Use the issue template. Include the firmware version (e.g. `B27`), the luyaz-core / luyaz-web commit, and relevant logs or the output of `./install.sh doctor`.
**Remove IMEI, ICCID, phone numbers, public IPs and other personal data before posting logs.**

## Code of conduct

Follow the [Contributor Covenant](CODE_OF_CONDUCT.md).
