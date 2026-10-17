# DESIGN.md — Luya-Z UI design single source of truth (device ZTE U60 Pro / MU5250)

**English** · [中文](DESIGN.zh-CN.md)

This file is the **only** design baseline for the admin web and the touch UI. Where another document disagrees with it, this file wins.

Source: `/plan-design-review` 2026-09-16 (decisions 1A / 2A / 3A / 4A / 5A / 6A / 7A / 7B / 7C).

---

## 1. General principles (both platforms)

1. **State and reason always come first**, things you can change second, detail (traffic, connection counts, logs) last.
2. **State is never color alone.** Every state carries a symbol and text: `●` normal, `▲` warning, `■` stopped.
3. **Empty states are a feature.** Never just "no data"; always give the reason and the next action.
4. **Subtract first.** Every element must earn its pixels; if it doesn't fit, cut it — don't fill space with decoration.
5. **Use a card only when the card itself is the interaction.** Display-only groups use a heading plus a divider.
6. **English wording** (set 2026-10-01 in L2; one set for touch UI, web, datad, agent and alert SMS; words follow `docs/ui-glossary.md`):
   - Sentence case; page names are proper nouns and use Title Case like the web menu (Band Lock, Speed Test). Proper names as-is: Wi-Fi, eSIM, Tailscale.
   - No period on labels and state words. Hint lines are one sentence, start with a verb when there's an action, no "Please", no trailing period.
   - Name a page by its name, unquoted, exactly as in the glossary (Turn on mobile data in Cellular).
   - Unit case never changes: Mbps, MHz, dBm, GB, n78, B3; one space between number and unit; in tight spots like the top bar, follow what the Chinese currently does. Web English eyebrows are uppercased, so no units in eyebrows.
   - 24-hour time `14:32`; durations `14h 51m`, `5 min`; relative `3 min ago`; stale `last update 14:32`; dates `26 Sep` (SMS fixed width `01 Oct 14:32`).
   - Prefer counts that need no plural (`Devices 6`, `6 online`); use `TRN` only in natural sentences.
   - English alert SMS: `[Luya-Z] body (01 Oct 14:32)`, pure ASCII, ≤70 characters.
   - Tests check: no trailing period, no Please, correct unit case, no Chinese (except whitelisted SSIDs, raw carrier names, etc.).

---

## 2. State semantics (cross-platform, semantics fixed)

| Meaning | Definition | Symbol | Touch value | Web token |
|---|---|---|---|---|
| Normal | Proxy running | `●` | `.ok` `#5ec85e` | `--ds-success` |
| Warning | Degraded but network usable, or a deviation worth noting | `▲` | `.warn` `#d98a00` | `--ds-warning` |
| Stopped | Proxy not running, needs a human | `■` | `.bad` `#d9534f` | `--ds-error` |
| Neutral | User turned it off, paused, or upstream unsuitable to take over | `●` grey | theme default text color | `--ds-text-dim` |

**Color values may vary per platform; the semantic mapping may not.** If the LVGL touch UI changes palette it must keep these four meanings one-to-one, or two generations of UI on the same device will contradict each other.

Contrast: any state text ≥4.5:1 on its background. The touch UI must be checked on both `body.dark` (`#15161a`) and `body.light` (`#eceef1`).

The full 15-row state copy table (including API unreachable, empty list, subscription failed, switch succeeded) is in the design doc section "UI design decisions · 2A"; implement from that table.

**State history keeps 20 entries** (decision 3A), one data set shared by both platforms; past 20 the oldest is dropped. The touch UI bottom shows only the latest one; the web expands all 20 under "Advanced" (time, state, reason, key values). With empty history the row isn't rendered — no "no records yet".

Data source is `/data/chill/events.log` (on `/data`, survives reboot; `chill.sh` appends a line per state transition and truncates to the last 20 on write):

```
2026-09-16T03:12:41 overheat cpuss=86 -> direct
2026-09-16T03:29:07 recover  cpuss=63 -> running
```

The touch UI bottom line renders it as `最近：03:12 过热暂停 · 03:29 已恢复` ("Recent: 03:12 paused for heat · 03:29 recovered").
This history is also the basis for calibrating thermal thresholds — run a few days, count real triggers, then fill in `CHILL_TEMP_HOT_*` — so don't change the format casually.

---

## 3. Touch UI · phase 1 (litehtml, legacy, not the default build)

> The live touch UI is the Rust one in section 4. This section is history: the litehtml renderer (`src/htmlmain.c` + `ui/*.html`) has been removed from touch-ui and survives only in tag `legacy-litehtml`.

**Hard constraints** (break them and it either won't draw or shows tofu boxes):

| Allowed | Not allowed |
|---|---|
| `block` / `inline-block` / `float` / `table` layout | CSS Grid, JavaScript, CSS animations and transitions |
| colors, border radius, borders, padding, margin | images (neither `<img>` nor `background-image` draws) |
| `position: absolute / relative` | `var()` custom properties (unreliable) |
| whole-page vertical scrolling | scroll containers inside elements |

- Screen 320×480. Pages live in `ui/functions/`, hot-reloaded, no compile.
- **Symbol whitelist** (confirmed to render in the device font): `● · ↑ ↓ ▲ ▼ ℃ •`. Verify any other symbol on the device first; when unsure, use Chinese text or draw it in CSS.
- **Copy length cap 14 CJK characters** (decision 2A) — the limit for a single 320px line; longer truncates or wraps and breaks hierarchy. Every touch string in the state table is written to this.
- **Tile copy** (decision 7C): `chill.html` keeps `<title>` as `CHILL`, `<meta name="description">` is `线路与出口` ("routes and exit"). It's the subtitle on the tile wall, same style as 「网络测速」 ("Speed Test") and 「eSIM」; no English slogans.
- Themes are two rule sets, `body.dark` / `body.light`; every new color class is written twice.
- Pages contain only the inner `<body>`: no `<style>`, no shell besides `{{STATUSBAR}}`, no home-made back bar.
- Polling: only call the API while the CHILL page is visible and the screen is on; the home card reads only `/tmp/chill.state`, never the API; rates use cumulative fields, the full connection list isn't fetched by default; latency tests only on tap.

**CHILL page hierarchy** (top to bottom): status line (state + exit region + latency, large) → mode buttons → groups → node list → bottom line with traffic and connection count → bottom line with latest event (not rendered when there are none).

---

## 4. Touch UI · phase 3 (LVGL, new design since 2026-09-24)

Rust (`touch-ui/rs/`, LVGL 9.5 via `lvgl-sys`; the old C version is deleted, tag `legacy-c-touch-ui`), 320×480, FreeType for CJK. Inherits section 2's state semantics (values changed, meanings map one-to-one: normal → okT / green, warning → warnT / orange, stopped → badT / red, neutral → t3) and section 1's principles. The full spec (palette and contrast, type scale, every component's size, every page layout, state table) is in the new design doc; the only implementation in code is touch-ui's `include/ui_kit.h` (components) and `include/ui_theme.h` (palette, font roles). Pages may only use these; they don't set their own colors, radii or font sizes.

**Theme**: light / dark / auto (`appearance` in `devui.conf`; auto = dark 19:00–07:00 device local time, switched only while the screen is off). One process uses one theme; switching = exec itself (same pid, same name). All colors are quantized to 565 and contrast-checked one by one; dim things with t3, never with lower opacity; vivid colors (green / orange / red / blue) only where there's no text, white text sits on fill colors. **Switch exception**: an on switch's vivid green track is only 2.2:1 on white; on/off is carried by the knob position (common practice), color is only a hint.

**Fonts**: digits Nunito (600/700/800, with CJK fallback attached; falls back to device Roboto if missing); Chinese uses the device's ZTEZhengYuan (regular 11–15, synthetic bold 15/17/20/22/24, needs an LVGL patch). Largest size 36 (speed test result); the home status card's verdict uses cj24b. Smallest: battery digits 10, value labels 11, secondary info 12.

**Components** (sizes per `ui_kit.h`): top bar 26 (time · 5 signal dots · RAT · rate · ▲ · battery; when the rate doesn't fit, step through shorter forms); floating tab bar 44 (only on the five top-level pages); sub-page title bar 62 (includes top bar, solid, round ‹ back); card radius 20, outer margin 10; list row 40 / two-line 50, divider starting at x=14; segmented button 30; switch 44×26; tile 145×78, always white, "on" (CHILL running, WiFi on) = a green dot before the subtitle + green text, never a full fill (a full blue tile looks permanently selected); status block 72 (top of a card, radius 20 plus a square patch underneath); bottom sheet radius 22. Press: one step darker (one step lighter in dark theme), restore on release; transparent list rows lay a `track` background while pressed (darkening a transparent background shows nothing, 2026-09-25). Only two animations: bottom sheet rises in 180 ms, sub-page pushes in 150 ms; `UI_ANIM=0` turns both off. Hit areas no smaller than 40×40.

**While the screen is dark, a finger hits nothing**: if the power key turned it off, touch does nothing and only the power key wakes it; if it auto-dimmed, a double tap wakes it (single taps, swipes, or two taps too far apart don't count — pocket protection). The waking gesture (the double tap, or a finger already down when the power key is pressed) is not a tap, and taps are ignored for 300 ms after wake. Rule lives in touch-ui `ui_logic.c` `ui_tgate_step`, with unit and render tests.

**Long text**: values shown in full; names take the remaining width and end with `…`; IP, ICCID, IMEI are never truncated. LVGL 9's `…` mode needs a fixed height (`uk_label_w` handles it), otherwise it wraps.

**State**: every colored state also has a text cue (signal good / weak / none, running / off, direct / relay / offline, ▲). Charging only turns the battery bright green (user's choice, matches the mock, no bolt). Empty and error states use specific copy and a next step (state table in the design doc). Data service down ≠ no signal: the status block turns grey, says 「数字停在 hh:mm」 ("numbers frozen at hh:mm"), values dimmed but kept.

**Home hierarchy** (finalized in the 2026-09-24 review: verdict first + two tiles): picking up the device you first want "can I get online, is it good, will it cost extra", so aggregate bandwidth is no longer the hero. Top to bottom:
- **Status card**:
  - Top line: carrier · RAT · home/roaming.
  - Large text: one verdict, from the "Home verdict table" below (English: state word only).
  - Right: SINR (colored by quality) and RSRP.
  - One extra next-step hint line when something's wrong.
  - Four rows below (after the 2026-09-25 task-based tabs): Cellular (carrier reading + active bands · aggregate bandwidth), Wi-Fi name · device count, Exit (where CHILL exits when on; direct-IP location when off) — these three jump to the top of their tab; today/month traffic is read-only, no ›.
- **Scenario tile**: full width, opens the 「情景」 (Scenarios) page.
- **Speed chart** (formerly the first chart on the charts page).
- Carrier detail is at the top of the 「蜂窝」 (Cellular) tab; CHILL detail (rule → node) and Tailscale detail are in the 「出口」 (Exit) tab, below the exit IP.

**Navigation** (2026-09-25): five tabs 首页 · 蜂窝 · Wi-Fi · 出口 · 系统 (Home · Cellular · Wi-Fi · Exit · System), split by "what do I want to do"; every function has exactly one home. Scenarios are a separate module that only drives Wi-Fi / CHILL / Tailscale; no cellular setting ever appears on its pages.
- A row with **›** always opens a page (or jumps to a tab); rows without › aren't tappable and have no pressed state.
- **Switches** take effect immediately; **buttons** are only for one-off actions (speed test, network search, mark all read, restore defaults); any choice that can drop the connection uses the two-step confirm below.
- **Back**: tap ‹, or swipe right from the left edge (x < 24). Once the swipe registers, that press is void and the row under the finger isn't tapped.
- Sub-pages always open at the top; jumping to a tab from a home summary also lands at the top; tapping the current tab again scrolls to top, tapping another tab keeps its previous position.
- Sub-page title = the text on its entry.

With no signal or no data, the exit IP and CHILL rates are dimmed so nobody takes them as live.

**Interpret first, parameters second** (2026-09-24, user: "don't expose raw parameters; put an understandable reading on top"):
- The status card's verdict, hint, RAT wording, carrier wording and signal rating are all decided by datad `rust/src/screen.rs` `story()`/`say()` (moved from touch `ui_net_story()` on 2026-09-26), every branch unit-tested. The touch UI only displays them.
- Parameters (SINR, RSRP, RSSI, band, bandwidth) are small supporting text only.

**Home verdict table** (set 2026-10-01 in L2, datad `say()` is authoritative; full Chinese/English hint lines in `docs/ui-glossary.md` §5):

| Priority | story.state | Large text (Chinese) | Large text (English, ≤150px) | Tone |
|---|---|---|---|---|
| 1 | (touch: no data yet) | 读取中… | Loading… | neutral |
| 2 | (touch: datad down) | 读不到数据 | No update | neutral, values greyed but kept |
| 3 | nosim | 无 SIM | No SIM | bad |
| 4 | airplane | 移动网络已关 | Airplane | neutral |
| 5 | changing | 正在换制式 / 正在确认 / 正在退回 | Switching / Checking / Reverting | neutral |
| 6 | revert_fail | 退回也没通 | Failed | bad |
| 7 | sos | 只能紧急呼叫 | SOS only | bad |
| 8 | nosvc | 无服务 | No service | bad |
| 9 | nodata | 没连上网 | Offline | bad |
| 10 | stall | 连上了但不通 | No traffic | bad |
| 11 | hot | 慢：过热限速 | Slow | warn |
| 12 | limit / weak / noise / crowd | 慢：限速 / 信号弱 / 干扰大 / 疑似拥挤 | Slow | warn |
| 13 | only2g / only3g | 只有 2G / 只有 3G | 2G only / 3G only | warn |
| 14 | narrow | 慢：载波窄 | Slow | warn |
| 15 | ok | 顺畅 | All good | ok |

changing and revert_fail are overlaid by write operations (E4, datad `screen.rs` `with_op()`, only in `net.home` of `/v2/screen`; `net.story` is always just the network verdict): switching RAT often passes through no-service, so it ranks above no-service to avoid a run of red; not overlaid on no SIM or mobile network off. The hint line carries the countdown sentence (「1:42 后没通就退回到自动」, "if not connected in 1:42, revert to auto") or, on revert failure, the current and last confirmed values. Other persistent results don't take the large text; they add one sentence at the end of the hint line.

English "slow" is just Slow; the reason goes to the right column (signal, Noise, Load) and the hint line. hot = the firmware itself flagged thermal throttling (uci `zwrt_zte_mc_tmp.cpe.hightemp_datalimit_status` non-zero, datad `/state` `thermal.hightemp_limit`) — a fact, not an inference, so it ranks above limit and signal checks; unreadable counts as absent. stall = dialed, and in 30 seconds `rmnet_data0` sent ≥ 20 packets but received none (window kept by the datad sampling loop, `docs/designs/slow-diagnosis.md` §4.1). Roaming is never a verdict by itself; the hint line gets 「；漫游中 / ; roaming」 appended.

**English length budget** (real Nunito sizes; datad and agent tests approximate by characters, the touch render checks text stays inside its parent and the screen and siblings don't overlap):

| Place | Size | Limit | If over |
|---|---|---|---|
| Home large verdict | 24b | 150px (~10 chars) | change the word only |
| Right column signal word | 17b | 120px | change the word |
| Right column noise · load | 12 | 150px | change the word |
| Hint line | 13 | 2 lines (width 292) | change the word, no 3rd line |
| Top line (carrier · RAT · home/roaming) | 12 | 292px | carrier name `…` per the end of this section |
| Top bar RAT word | 13b | no wider than 5G UW (46px) | use the glossary's short form (No service = No svc) |
| List row label | 15 | 110px | change the word |
| List row value (names) | 15 | remaining width | names `…`, numbers in full |
| Three-way segment, each | 14 | segment width minus 16 | change the word |
| Tile subtitle | 12 | 117px | change the word |
| Bottom tab label | 14 | 60px | change the word |
| Whole alert SMS | — | 70 characters | change the word |

Wording of each part:
- **Network label**: as short as a phone status bar, used in both the top bar and the status card top line. Computed by `ui_net_label()` from the raw RAT and the number of active carriers:
  - **2G**: GSM, GPRS, EDGE, CDMA 1X.
  - **3G**: WCDMA, TD-SCDMA, CDMA2000 (EV-DO, eHRPD), HSPA. **3G+**: HSPA+, DC-HSPA+.
  - **4G**: LTE, 4G, FDD-LTE, TD-LTE, single carrier. **4G+**: LTE-A, LTE_CA, 4G+, or ≥2 active LTE carriers.
  - **5G**: SA or NSA, 1 NR carrier. **5G+**: 2 active NR carriers. **5G-A**: ≥3 active NR carriers, or the raw value itself says 5G-A. The modem has no explicit 5G-Advanced flag; this is inferred from the three-carrier aggregation common with Chinese carriers.
  - The **status card top line** doesn't repeat the top bar shorthand but goes one level finer: 「5G SA」「5G NSA」「4G LTE」「3G WCDMA / HSPA+ / CDMA2000 / TD-SCDMA」「2G EDGE / GPRS / GSM / CDMA 1X」 (`ui_rat_long()`).
- **Carriers**: the reading first, then a line with band and bandwidth for each active carrier. Total bandwidth counts active carriers only. Network architecture is already in the top line, not repeated here. Readings:
  - 5G SA and 4G: 「N 条载波聚合 · 带宽很宽/充足/一般/偏窄」 ("N carriers aggregated · bandwidth very wide/ample/fair/narrow"); with one carrier 「单载波 · …」 ("single carrier · …");
  - NSA: 「4G 锚点 + 5G，N 条载波 · …」 ("4G anchor + 5G, N carriers · …");
  - 3G and 2G: 「这个制式没有载波聚合」 ("this RAT has no carrier aggregation"). The speed impact goes in the hint line, not repeated in the carrier block.
- **Signal rating**: 「信号很好 / 良好 / 一般 / 较差」 (very good / good / fair / poor), mainly from SINR, one step lower when RSRP is too low.
- **Bands**: written B3 / n78. Things like 「GSM 900」「DCS 1800」 are frequencies, kept as-is.
- **4G without carrier aggregation**: the serving cell is only in the `lte_*` fields and must be filled in from there — never show "no carrier".

Render test scenes: good, weak, nosignal, datad-down, loading, nosim, abroad, lowbat, full-charging, long-names, empty, nsa, lte, 3g, nodata, 5ga, edge, each in dark and light theme.

**Home carriers** (carrier detail card): serving cell first (`nr_band/nr_bw/nr_rsrp/nr_pci/nr_channel`), then every `nrca`/`lteca` group appended as-is (the serving cell appears again inside them with a -140 floor value — that's the modem's configured carrier, not deduplicated); inactive carriers collapse to one t3 line.

**Every control that changes device state uses a two-step confirm**: the first tap turns it fillOrange 「再按一次确认」 ("tap again to confirm"); only a second tap within 5 seconds sends it (network selection, apply band lock, restore defaults, eSIM switch, switch to stock UI, delete SMS, turn off CHILL). The WiFi switch is the exception.

**Every tap must be visible on the spot** (2026-09-25, user tapped on 「情景 · 网络」 and nothing happened):
- **The first tap** redraws right in the callback, without waiting for the 1-second refresh. Buttons turn fillOrange; list rows get a full-row `washW` pale yellow background, 「再点一次确认」 ("tap again to confirm") on the right, and one sentence under the card saying what will happen (e.g. 「切到「外出」：Wi-Fi 开着 / 之后一直固定，点「自动」才恢复」, "switch to Away: Wi-Fi on / stays fixed until you tap Auto"); break the line deliberately when it spans two lines so no single character drops to line two.
- **Single-choice lists** (scenarios, network selection, etc.) use radio circles: filled = the option actually in effect, not "the one just tapped".
- **Tapping the option already in effect** says something (「现在已经是自动 · 判断为「在家」」, "already Auto · detected Home"), never silently ignored; tapping while busy also says something (「正在切换，稍等」, "switching, hold on").
- **After sending**, draw 「切换中… / 已发送」 ("switching… / sent") first, then send the request (it's synchronous; the UI pauses briefly), and keep it until the device has really changed; on timeout (scenarios: 45 seconds) state the reason. Success shows one green result line that fades after a few seconds.
- **Rejection** reasons are written in that card, not only at the top of the page.

**Features without a data source say what's missing**: neighbor cells / MIMO / BLER aren't available on this device (no `/modem/latest-signals`); the page states why instead of drawing an empty shell.

**Transactions (write-op layer, E4, design `docs/designs/write-op-layer.md` DD3, DD5, DD9, DD12, DD13, DD16)**: for every write that goes through datad (currently network mode), all text is sent by datad in the `op` block (`say/next/note/what/source/old/target`, each with `_zh/_en`); the touch UI only lays it out; vocabulary in `docs/ui-glossary.md` §13.
- Home and the matching settings page (Cellular › Network mode) show it in the existing status block, no extra progress card; other tabs and sub-pages get a **transaction row** under the title bar 「正在换制式 · 1:42 ›」 ("switching RAT · 1:42 ›", whole row ≥40 tall), which opens the **transaction page**.
- Transaction page, five layers: verdict → 「旧 → 新 · 来源 时间」 ("old → new · source time") → three progress lines (setting applied / registered / data, ✓ or …) → countdown (「1:42 后没通就退回到 X」 "revert to X if not connected in 1:42"; with auto-revert off 「还剩 0:48 · 自动退回没开」 "0:48 left · auto-revert off") → buttons. If datad pushes nothing for 20 seconds: countdown greys out as 「停在 1:42 · 等设备响应」 ("stopped at 1:42 · waiting for device") and stops ticking.
- Buttons name concrete values: 「退回 X」 ("revert to X") primary on the left, 「保留 Y」 ("keep Y") secondary on the right; both two-step confirm, only one pending at a time, pending = fillOrange plus one consequence sentence. At expiry the device decides and the pending confirm is void; while reverting, only progress shows, no buttons.
- When done, the transaction row becomes a result row (「▲ 没通 · 已退回自动 ›」, "▲ not connected · reverted to auto ›"); 3-second kinds (switched, reverted, no confirm needed) only show if their end was actually witnessed; persistent kinds stay until 「知道了」 ("got it") is tapped (only recorded via datad; touch and web dismiss together). Revert also failed (DD9): red takes the status block, shows current and last confirmed values, buttons 「再试一次退回到 X」 ("try reverting to X again") and 「重启设备」 ("reboot device").
- System › Change log: one entry per two lines (「制式 · 自动 → 只用 4G」 / 「10-03 14:32 · 网页 · 已切到只用 4G」, "RAT · auto → 4G only" / "… · web · switched to 4G only"), tap for detail; undo/redo live in the detail; un-undoable entries are greyed with a reason.
- Data service stuck (20 s without movement): controls that write via datad grey out early with 「数据服务没响应 · 暂时不能改设置」 ("data service not responding · can't change settings right now"). Busy (another write running) doesn't grey out early; on tap, show the sentence datad returns.

**Regression tests**: `touch-ui/rs/build.sh test` (unit tests, including render checks on text/glyphs/geometry); after changing pages also run `touch-ui/rs/parity.sh`: replays the scenes recorded by the C render test (11 kinds × dark/light, from tag `legacy-c-touch-ui` `scripts/test/render/render.sh --model/--png`; the C version was deleted in Phase 10 X5) on the Rust touch UI, compares pixel by pixel, and asserts every string expected in `rs/devui/testdata/expect.txt` is present (widget list). Copy changes update expect.txt in the same commit (public version uses expect-public.txt).

**Known pitfalls**:
- `LV_USE_STDLIB_MALLOC` was once `LV_STDLIB_BUILTIN` (LVGL carves out its own small pool fully isolated from system memory; it kept crashing from 128KB all the way up to 2MB). The root cause was the isolation itself — the litehtml version (`htmlmain.c`) used system `malloc()` with 690MB+ free and never had this problem. Now `LV_STDLIB_CLIB` (real system malloc/string/sprintf), same path as litehtml, and all `LV_MEM_SIZE` config removed (only meaningful for BUILTIN). **This is the real fix; the earlier "fewer unnecessary `lv_label_set_text` calls" only mitigated it** — but that optimization stays; the two aren't mutually exclusive.
- `lv_label_set_text()`/`_fmt()` free and reallocate the text buffer even when the new text is identical — compare against a cache before each `refresh_cb` and skip unchanged values; don't reset dozens of labels every second unconditionally.
- Real-device stability can only be verified functionally, e.g. "actually read `/tmp/luyaz/dumpfb` → `/tmp/fb.dump` once and confirm the byte count". `pidof` finding the process doesn't mean the main loop is running (an LVGL assertion failure is an infinite loop, not an exit; `pidof` keeps finding it).
- `lv_slider`/`lv_button`/`lv_switch` constructors enable `LV_OBJ_FLAG_SCROLL_ON_FOCUS` by default (with `LV_EVENT_FOCUSED` handling in `lv_obj.c`); if a control gets focus while a page is built, it scrolls itself into view. This project is touch-only, no group/keypad, so focus scrolling is meaningless, yet it's on by default. Symptom: a page's first build stops halfway scrolled (first visit to the 「系统」 (System) tab shows 「电池与负载」 ("Battery & load") instead of the top card 「屏幕」 ("Screen")), later visits are fine — easy to dismiss as flaky. Every page using `uk_scroll` ends its build function with `lv_obj_scroll_to_y(t, 0, LV_ANIM_OFF)` to pin it to the top — runs only at build time, doesn't affect later user scrolling.
- **Fonts and glyphs must match**: `LV_SYMBOL_*` (LVGL private-use codepoints) only go in montserrat labels (after the redesign only function icons and ✓); Chinese, `·`(U+00B7), `↓↑`(U+2193/2191) go in the CJK font or in Nunito with CJK fallback (`UF.*`). Swap them and both sides render tofu — a 2026-09-21 screenshot round had both at once: Tailscale's IP row built `·` with montserrat, the chart speed title built `LV_SYMBOL_DOWN` with FCN_S. Digits/ASCII are fine in the CJK font, so when unsure use FCN_S. Also `LV_SYMBOL_DOWN`/`LV_SYMBOL_UP` **draw as ∨ ∧ chevrons, not ↓ ↑ arrows**, reading as "expand/collapse" rather than down/up rates — top bar and chart rates always use FCN_S + real `↓`(U+2193) `↑`(U+2191), consistent with the then-live litehtml status bar.
- **Never put Chinese in a montserrat label**; it renders as tofu boxes. Hit four times on 2026-09-18/09-21 (network page PLMN row, Tailscale state 「已连接」 "connected", the 「未激活」 "inactive" suffix on carrier sub-cards, the WiFi password row 「密码未公开」 "password hidden"). The test is not "is the current value Chinese" but "**could** this label ever hold Chinese" — including fallback copy and state words. Digits/ASCII are fine in the Chinese font, so when unsure use `UF.cj*` or `UF.n*` (Nunito with CJK fallback).
- `lv_tileview_add_tile()` **does not** set the tileview's "current tile" pointer; `tile_act` stays NULL until the first `lv_tileview_set_tile*()` or the first scroll end (`lv_tileview.c:92,181`). So at startup, with home clearly on screen, `lv_tileview_get_tile_active(tv) == s_tiles[0]` is false, and every poll gated on it (Tailscale, eSIM) never runs — symptom: the home Tailscale card stays empty until you flip away and back. `ui_create()` must call `lv_tileview_set_tile_by_index(tv, 0, 0, LV_ANIM_OFF)` after building tiles.
- **Any "reset the UI to its initial state" call inside ui_create() must return safely when widgets aren't built yet.** In a 2026-09-21 refactor `sub_close()` (collapse the sub-page layer) also called `update_tabs()`, but the tab bar was built after it — `lv_obj_set_style_text_color(NULL, …)` segfaulted and the process died at startup. Because the official UI had been killed for the sideload test, the vendor watchdog saw no UI drawing and **rebooted the whole device** (this project's second such incident). Two responses: affected functions start with a built check like `if (!s_tabs[0]) return;`; the test start script **auto-rolls back if it doesn't come up** — log to `/data/lvgltest.log` (not `/tmp`; the RAM disk is wiped on reboot, losing exactly the log that explains the crash), `grep "first refresh done"` 8 seconds after start, and if missing kill it immediately and `sh start.sh legacy` to bring back the official UI. The `fprintf(stderr, "ui: …")` breadcrumbs in `ui_create()` exist for this check.
- Before a sideload test, don't blindly press the power key to light the screen — it's a toggle. Read `/sys/class/leds/led:lcd/brightness` first and press only if 0; otherwise you turn the screen off, `backlight_is_on()` becomes 0, every poll gated on it (Tailscale/CHILL/eSIM/WiFi switch state) reads nothing, and screenshots look like layout bugs.
- `backlight_is_on()` in the poll gates is 0 while the screen is off, so headless screenshot checks (SSH + fb-dump, no touching) leave those cards empty, looking like layout bugs. Light the screen first with `touchsim /dev/input/event0 key 116` (with auto-dim set to "always on", the UI never turns the backlight on by itself; only the power key does).

---

## 5. Admin web (device `:9090`)

This section covers **behavior and visuals** only, independent of framework (Phase 8, T19): the current luyaz-web and any future rewrite follow it. The terms "route family" (页族) and "three write tiers" (三档写操作) are in `docs/ui-glossary.md` §15. The current implementation's files and component names are collected at the end under "Current implementation".

Redesigned on 2026-09-24 in a **Cohere + Figma** visual language (reference files and design/eng review notes live outside the repo as maintainer working notes). The touch UI keeps its own palette from section 4; the web **no longer mirrors** `ui_theme.c`. The old Cool Tech (`admin.css`, Outfit/DM Sans) is deleted.

**Color only through tokens**: always the `--nd-*` CSS variables (names don't change with the framework); a hex value in a page is a defect. Tokens are defined in one place; after changing a token, re-run the contrast check and everything must pass (text ≥4.5:1, non-text ≥3:1).

**Surface**

- **Paper + ink**: canvas warm white `--nd-bg` `#fafaf8`, white cards with a 1px hairline `--nd-hair`, no shadows (only overlays — dialogs, toasts, ⌘K — have shadows). Body ink `--nd-t1` `#17171c`, secondary `--nd-t2` / `--nd-t3`.
- **One primary surface**: `--nd-primary` near-black (near-white in dark mode). Used for the primary button, selected segment, selected multi-select tag, switch on, and any "selected" outside the sidebar. One primary button per screen; the rest are secondary (white hairline pill) or text buttons (underlined).
- **Color blocks (Figma)**: each kind of page has its own block color, used in exactly three places — the top status block (when normal / neutral), the current sidebar item, and the background of the current icon in the mobile bottom bar. Charts `blkCharts` pale blue, network & bands `blkNetwork` lime, Wi-Fi & LAN `blkWifi` mint, services `blkServices` lavender, SMS `blkMessages` pink, device & maintenance `blkSystem` beige. Every page belongs to a **route family**; the shell sets `data-family` on the root element by family and that decides the block; pages don't pick colors.
- **Problems override category color**: attention `--nd-washW` amber, error `--nd-washB` coral. Text in blocks is always ink; only state symbols carry semantic color.
- **Deep green band (Cohere)**: home when state is normal, and the whole login page, use `--nd-band` `#003c33` + white text. The three home module cards are solid lavender (CHILL), beige (Scenarios), deep navy `blkNavy` (Tailscale), no hairline.
- **Alert bar**: not a color block but a single ink strip (Cohere announcement bar), one line on mobile, with detail on wide screens; avoids stacking two colored blocks with the status block.
- **Dark mode**: same roles; blocks become saturated deep tones (not greyish darks); the primary surface inverts; the alert bar is a raised dark strip in dark mode, never a white block; dark terminal bands (AT, logs, processes) are dark in both modes.
- **Chart lines** use separate vivid colors (`--nd-green/orange/red/blue`), not monochromed.

**Type**

- Latin and digits in **Geist**, data (IP, IMEI, PCI, logs) in **Geist Mono**, Chinese via system fonts (PingFang etc.). Chinese has only discrete weights, so only 400 / 500 / 600, never 700/800.
- Mobile first, one step up at ≥1024: page title 22 → 28, body 15 → 16, secondary 13 → 14, status block title 17 → 20, readout 28 → 40 (home main readout 44 → 64).
- **Inputs no smaller than 16px** (iOS zooms the page on focus below 16).
- Group headings are **small uppercase monospace eyebrows** (11px, tracking 0.06em, `--nd-t3`), the Cohere/Figma section mark; the Chinese UI uses the body font at 12px without uppercasing (monospace spacing is uneven with Chinese, and uppercase turns MHz into MHZ). Readout labels are not uppercased.
- Titles, states and module titles use 500; hierarchy comes from size and tracking, not bold; 600 only for small `<h3>` headings and selected items. State words in blocks are ink, only the symbol carries semantic color; the lightest grey inside a block is lifted to `--nd-t2` automatically.

**Shape**

- Radius: cards 12, panels 16, inputs and options 8, buttons and segments pill.
- Tappable areas always ≥44px: shrink text and padding, not hit areas. Switch visual 40×24, hit area 44×44; segment visual 30 tall, hit area 44.

**Structure (set in the redesign, don't regress)**

- Navigation: <640 four bottom tabs (Home / Charts / Functions / System), 640–1023 left icon rail, ≥1024 240px sidebar with groups; ⌘K searches all pages (Chinese, pinyin, aliases). The route table (path, family, title, search aliases) is defined in one place; navigation, ⌘K and families all derive from it.
- Page order: title → status block (state · reason · next step · freshness) → things you can change → detail. Settings pages max width 720; data pages may use two columns.
- **Three write tiers**: tier 1 sends directly; tier 2 confirms inline on the page (stating the consequence); tier 3 is a dialog (what happens / how long it's down / how to recover; Tailscale warning when accessed remotely; factory reset requires typing 「恢复出厂」 "factory reset"). Every write is read back afterwards; if readback fails say 「设备已接受」 ("device accepted"), not "success"; multi-step writes show each step, and on failure resend only unfinished steps. There's an inventory of which tier each control belongs to; change the inventory before changing a control.
- **Transactions** (E4, design `docs/designs/write-op-layer.md` DD4, DD5, DD8, DD15): writes through datad (currently network mode) no longer read back themselves; the device confirms; text follows datad's `op` block, vocabulary in `docs/ui-glossary.md` §13.
  - Shell strip (alert bar style, `role=status`, revert failure `role=alert`): on every page, in progress 「正在确认 · 1:42」 ("checking · 1:42"), ≥640 adds 「制式 · 旧 → 新 · 来源」 ("RAT · old → new · source"); 「详情 ›」 ("details ›") goes to the matching page, and it isn't shown on that page. The countdown is `aria-hidden`; only phase changes are announced.
  - The matching page's status block becomes the transaction state: same five layers as touch, buttons 「退回 X」「保留 Y」 are tier-2 inline confirms; stacked vertically on mobile, full width, ≥44px.
  - System › Device › Change log (`/changes`): same two lines as touch; undo in the detail, tier 3 for anything that drops cellular (network mode), tier 2 otherwise; every attempt gets a new `op_id`, never resent. Settings pages end with 「上次改动 14:32 · 触屏 ›」 ("last change 14:32 · touch ›") linking to that entry (currently network mode only).
  - Data service stuck: the strip says 「数据服务没响应」 ("data service not responding"), writes via datad grey out; web disconnects mid-change: the strip says 「和设备断开了 · 操作结果未知」 ("disconnected from device · result unknown"), no auto-resend, the result shows normally after reconnect. Old datad (`supported:false` from `/api/ops`): no strip and no change log; writes read back as before.
- When data goes stale keep old values, grey them, write "N min ago"; never clear to 0.
- Icons are Phosphor: selected and tiles filled, the rest bold line.

**Naming**: the product name is always **CHILL**; RELAX never appears in any UI or clash/ShellCrash config.

**Two CHILL page constraints** (carried over from 2026-09-16 decisions 7A, 7B): zashboard is **not embedded in an iframe**; it opens in a new tab through the `:9090/chill-ui/` reverse proxy; device and subscription list rows don't stretch with the viewport.

**Control semantics**: region `radiogroup` + `radio`; switches `switch`; segments are a single-select toggle group; alert bar `role="status" aria-live="polite"`; dialogs have a title and description, and focus returns to the trigger on close.

**Live data** (same once it becomes push): when a number changes only the number changes — no reflow, no flash; inputs being typed in, expanded details and pending confirms are not overwritten by new data (DR12); screen readers announce only verdict and alert changes, not every refreshed reading (DR13).

**Current implementation (luyaz-web; the old Next.js web was deleted 10-15, Phase 10 X13)**: tokens `crates/luyaz-web/static/nd/newdesign.css`; component styles in the same directory `nd.css`, `shell.css` (concatenated by `src/assets.rs`); contrast `node crates/luyaz-web/tools/contrast.ts > docs/web/contrast.md`; route table `crates/luyaz-web/src/routes.rs`; tier inventory `docs/web/controls-inventory.md` (written for the old web, not updated). When the implementation changes, edit this paragraph; the rules above stay.

---

## 6. Anti-patterns (neither platform)

- One card per metric, stacked into a mosaic. Cards are **only** allowed two ways: grouped lists (one card holding a set of similar rows) and whole tappable modules (home Scenarios / CHILL / Tailscale cards, function tiles).
- Gradient backgrounds, neon glow, gradient text, real blur (except the slight backdrop blur of the web top and bottom bars). The touch UI's "tinted glass" is only for the floating tab bar, back button, bottom sheet and title bar right-side buttons, and it's an opaque solid color.
- Icons in colored circles as decoration; three-column equal feature grids.
- Colored left-border cards.
- Everything center-aligned; one large radius on everything. Radius tiers are deliberate: touch sheet 22, cards and tiles 20, options 14; web panels 16, cards 12, inputs and options 8; pills are half-height.
- Emoji as design elements (except flags in node names, which are data; the touch UI shows them as country codes).
- Big colored pills for state; use a dot plus text. Filled pills for selected items (segments, bands, exit options) don't count as state pills.
