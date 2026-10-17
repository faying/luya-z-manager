# Changelog

Each component has its own version ([Semantic Versioning](https://semver.org/)):
patch = fixes, minor = new features (old settings and APIs still work), major = something
must be migrated or several components have to be updated together.

| Component | What it is | Version from |
|---|---|---|
| `core` | `luyaz-core`, the core service (`:9460`) | `crates/luyaz-core/Cargo.toml` |
| `lweb` | `luyaz-web`, the admin web (`:9090`) | `crates/luyaz-web/Cargo.toml` |
| `sentinel` | `luyaz-sentinel`, screen owner and watchdog | `crates/luyaz-sentinel/Cargo.toml` |
| `touch` | `luyaz-ui`, the touch screen UI | `touch-ui/rs/devui/Cargo.toml` |
| `scripts` | device scripts (doctor, init scripts, …) | `touch-ui/scripts/VERSION` |

Each program prints its version with `--version`, e.g. `luyaz-core 1.0.0 (6e606ec)`.
Entries are written under **Unreleased** as `- <component>: text` (e.g. `- touch: …`);
`tools/luyaz bump <component> <patch|minor|major>` moves them into a new version section. `Requires:` lists the minimum versions
of other components.

A Chinese copy is in [CHANGELOG.zh-CN.md](CHANGELOG.zh-CN.md).

## Unreleased

- core: `GET /v2/versions` lists each component's installed version and the newest in this repo's `versions.json`; the core looks once a day (not while roaming; `LUYAZ_VERSION_CHECK=0` turns it off), `POST /v2/versions/check` looks now. Nothing is installed automatically.
- lweb: Device info has a "Component versions" group with a "Check for updates" button.
- touch: System has a "Component versions" card; "Check for updates" asks the core to look now.
- scripts: `VERSION` is shipped to `/data/luyaz/scripts/` so the scripts' version can be read on the device.

## core 1.0.0, lweb 1.0.0, sentinel 1.0.0, touch 1.0.0, scripts 1.0.0 — 2026-10-17

Baseline: the first numbered versions. Everything up to here (one Cargo workspace, the
Luya-Z names and device layout under `/data/luyaz/`) counts as 1.0.0 for every component.
