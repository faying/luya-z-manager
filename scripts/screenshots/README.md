# Screenshots

Regenerates the English screenshots in `docs/images/` (gallery: [docs/SCREENSHOTS.md](../../docs/SCREENSHOTS.md)) on made-up data: no device, no recording.

| Script | Makes | How |
|---|---|---|
| `web.sh [OUT_DIR] [name…]` | `web-*.png` | Builds `luyaz-web` in Docker, replays a synthetic event stream (`gen-fake-sse.py`) with `luyaz-web mock`, puts the typed fake agent (`crates/luyaz-web/e2e/fake-agent.mjs`) behind `rewrite-agent.mjs` (one made-up operator, English eSIM names), shoots every page with Playwright (`shoot-web.mjs`, cookie `u60_lang=en`) |
| `touch.sh [OUT_DIR]` | `touch-*.png` | Renders the touch UI off-screen (`luyaz-ui --render`) on the view models in `MODELS`, made by `touch-models.py SRC_MODEL OUT_DIR` from one dumped view model (made-up texts, CHILL not installed, a series that fills the charts; the Wi-Fi switches from `LUYAZ_RENDER_AUX`) |

Output goes to `scripts/screenshots/out/` (not committed). Look at every image, then copy the ones you want into `docs/images/`.

```sh
# once
(cd crates/luyaz-web && npm ci)
touch-ui/rs/build.sh native

scripts/screenshots/web.sh                       # all pages; or: web.sh out/web home signal
python3 scripts/screenshots/touch-models.py <a dumped model>.json /tmp/models
LUYAZ_DEVICE_FONTS=<dir> LUYAZ_NUNITO_DIR=<dir> MODELS=/tmp/models scripts/screenshots/touch.sh
```

- **Shoot the public tree.** `web.sh` builds the checkout it lives in. The public export has the private proxy pages stripped; a private tree would show them.
- Fake data only: operator "Example Mobile" (MCC/MNC 001/01), IPs from RFC 5737 / RFC 3849 ranges, `+15550100`, IMEI `3500…`, ICCID `8900…`. Never feed a recording from a real device into public screenshots.
- `shoot-web.mjs` prints any Chinese text it finds on a page (`ZH: …`); the language switch's 「中文」 is expected.
- Both shoot a device without CHILL: `web.sh` points `LUYAZ_WEB_CHILL_SH` at a missing file, the touch models say `chill_absent`.
- Env: `IMAGE` (Rust build image, default `u60-rust-ci:1.99.0`), `CHROME` (browser path; default Playwright's own, else the system Google Chrome), `PORT_BASE` (default 28000), `WARMUP` (seconds before shooting, default 60, so the charts have data).
- The device fonts for `touch.sh` are not in the repo (ZTEZhengYuan is proprietary): copy them from `/usr/ui/fonts` on a U60 Pro. Nunito (OFL) is on Google Fonts.
- Known: the desktop home page's carriers row overlaps at ≥1280 px, so `web-home-desktop.png` is currently cropped above it by hand.
