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
6. **English wording** (set 2026-10-01 in L2; one set for touch UI, web, core, agent and alert SMS; words follow `docs/ui-glossary.md`):
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
Since 10-29 the core's heat care (`docs/designs/thermal-care.md`) also asks chill.sh for eco (`hot`) or direct (`critical`) through `/tmp/luyaz/heat`; chill.sh stays the one controller of the proxy core and logs `warm core=hot …` for those.

---

## 4. Touch UI (LVGL, new design)

Rust (`touch-ui/rs/`, LVGL 9.5 via `lvgl-sys`), 320×480, FreeType for CJK. Inherits section 2's state semantics (values changed, meanings map one-to-one: normal → okT / green, warning → warnT / orange, stopped → badT / red, neutral → t3) and section 1's principles. The full spec (palette and contrast, type scale, every component's size, every page layout, state table) is in the new design doc; the only implementation in code is touch-ui's `rs/ui/src/kit.rs` (components) and `rs/ui/src/theme.rs` (palette, font roles). Pages may only use these; they don't set their own colors, radii or font sizes.

**Theme**: light / dark / auto (`appearance` in `ui.conf`; auto = dark 19:00–07:00 device local time, switched only while the screen is off). One process uses one theme; switching = exec itself (same pid, same name). All colors are quantized to 565 and contrast-checked one by one; dim things with t3, never with lower opacity; vivid colors (green / orange / red / blue) only where there's no text, white text sits on fill colors. **Switch exception**: an on switch's vivid green track is only 2.2:1 on white; on/off is carried by the knob position (common practice), color is only a hint.

**Fonts**: digits Nunito (600/700/800, with CJK fallback attached; falls back to device Roboto if missing); Chinese uses the device's ZTEZhengYuan (regular 11–15, synthetic bold 15/17/20/22/24, needs an LVGL patch). Largest size 36 (speed test result); the home status card's verdict uses cj24b. Smallest: battery digits 10, value labels 11, secondary info 12.

**Components** (sizes per `rs/ui/src/kit.rs`): top bar 26 (time · 5 signal dots · RAT · rate · ▲ · battery; when the rate doesn't fit, step through shorter forms); floating tab bar 44 (only on the five top-level pages); sub-page title bar 62 (includes top bar, solid, round ‹ back); card radius 20, outer margin 10; list row 40 / two-line 50, divider starting at x=14; segmented button 30; switch 44×26; tile 145×78, always white, "on" (CHILL running, WiFi on) = a green dot before the subtitle + green text, never a full fill (a full blue tile looks permanently selected); status block 72 (top of a card, radius 20 plus a square patch underneath); bottom sheet radius 22. Press: one small RGB565 shade darker (lighter on neutral dark surfaces; filled actions darken in both themes to preserve white-text contrast), restore on release; transparent list rows lay a `track` background while pressed (darkening a transparent background shows nothing, 2026-09-25). Only two animations: bottom sheet rises in 180 ms, sub-page pushes in 150 ms; `UI_ANIM=0` turns both off. Hit areas no smaller than 40×40.

**Motion lifecycle**: `UI_ANIM` is read once per process; unset or `1` keeps the two entrances, `0` shows their final state immediately. Pressed colors, selection and result text remain immediate in either mode. Opening the same subpage or an already-visible sheet does not restart its entrance. Back to a parent reveals it immediately; closing a sheet or subpage cancels its motion and clears translation, including an interrupted entrance. No hidden entrance keeps ticking. `LV_USE_COLOR_FILTER=1` is required for the shared pressed feedback; its shade steps retain at least 4.5:1 text contrast after RGB565 conversion.

**Read performance**: periodic API, Tailscale, CHILL, screen-view and Wi-Fi auxiliary reads run on three persistent readers with a bounded queue, with one pending job per cache. LVGL keeps the last completed values; local actions invalidate older read results. Completed changes wake the UI through a nonblocking `eventfd`, without resuming panel timers while dark. Core snapshot/SSE handshakes and reconnects also run in the background; the UI alone drains the established stream, owns input/backlight and paints. HTTP bodies have a total read deadline, so a dripping peer cannot keep a reader indefinitely. Cache checks that find no change do not wake or repaint the UI. Completion-driven refreshes do not advance chart history or the one-second FPS/touch counters. This governs reads; existing command acknowledgement and confirmation semantics remain in their action paths.

**While the screen is dark, a finger hits nothing**: if the power key turned it off, touch does nothing and only the power key wakes it; if it auto-dimmed, a double tap wakes it (single taps, swipes, or two taps too far apart don't count — pocket protection). The waking gesture (the double tap, or a finger already down when the power key is pressed) is not a tap, and taps are ignored for 300 ms after wake. Rule lives in `touch-ui/rs/ui/src/logic.rs` `ui_tgate_step`, with unit and render tests.

**Long text**: values shown in full; names take the remaining width and end with `…`; IP, ICCID, IMEI are never truncated. LVGL 9's `…` mode needs a fixed height (`uk_label_w` handles it), otherwise it wraps.

**State**: every colored state also has a text cue (signal good / weak / none, running / off, direct / relay / offline, ▲). Charging only turns the battery bright green (user's choice, matches the mock, no bolt). Empty and error states use specific copy and a next step (state table in the design doc). Data service down ≠ no signal: the status block turns grey, says 「数字停在 hh:mm」 ("numbers frozen at hh:mm"), values dimmed but kept.

**Incidents (▲ in the top bar, 2026-10-24 「变差才通知」)**: the ▲ has two sources — unread alerts, and an incident the core says is open on fresh data (the `regress` block; stale or a silent core never lights it for an incident); the alert channel lost stays red. Its hit box is 44×44, its left edge on the rate's hit box's right edge, growing right over the ▲ and the battery (never over the rate's); `luyaz-ui --hitcheck` (rs/hitcheck.sh) opens every page and fails when a control sits a fifth or more under it (so 网络诊断's 再查一次 lives on the page, not the title bar). Tapping it opens 健康与告警, whose top section 事故 lists the open ones (≤5, then 「还有 N 个开着」) and those ended in the last 24 h (≤3; across a reboot only by a trusted wall clock): two lines (▲/● name · 还开着 / 已恢复 / 已结束 · N 次, then 14:32 起 or 03:12–03:29 and CHILL/Tailscale for an exit) plus an optional 12 px t2 line 「同时观察到」; rows are not tappable (details are on the web). Stale: the last rows in t3 under 「事故数据停在 hh:mm」; never read: 「读不到事故数据」; an older core: 「core 版本太旧，不支持事故」 — never hidden.

**CHILL list states**: empty group/node lists and unavailable traffic use explicit neutral status text rather than blank cards. Group chips stay within the card; traffic values remain complete and their names use the remaining width. The latency action owns a 40 px header row, separate from node-row targets, has shared pressed feedback and becomes noninteractive while testing or unavailable. With no nodes it is hidden; cached nodes keep their last delay values in neutral text with an unavailable status when CHILL cannot be reached. Opening CHILL immediately paints any home-exit state already in the model.

**Home hierarchy** (2026-10-24 review; replaces the 2026-09-24 verdict-first + two-tile layout): the first screen answers three questions at a glance: what state the network is in, what situation I'm in, and whether I need to do anything. Four fixed slots; content changes inside a slot and never moves the slots below it:

| Slot | y | Height | Content |
|---|---|---|---|
| Status card | 30 | 147 | SIM line 29 · body 92 · footer 26, one card tinted by tone (`wash` / `washW` / `washB`) |
| Scenario card | 182 | 174 | header (scenario, mode, why) · CHILL \| Tailscale |
| Bottom row | 362 | 54 | three cells: Wi-Fi · today · battery |
| Tab bar | 428 | 44 | unchanged |

- **Status card, SIM line** (12): operator logo (16 px tall, `-w` file in dark), then RAT long form and home/roaming. Roaming onto another network shows that network's logo too (「5G NSA · 漫游到 [logo]」). Dual standby takes two lines (2026-10-24 review): the data card's line, then the standby card's (logo dimmed · operator/RAT/place), each with its role word on the right (「上网中」 / 「没连上」 red; 「待机」「待机 · 信号弱」「待机 · 信号好」 when the data card has no service). The card keeps its 147 px: the two lines take 39, the body 82 (no speed wave, a 66 px ring). No SIM: bold 「没有 SIM」 plus slot/eSIM state as text.
- **Status card, body**: verdict cj24b in the tone colour (left); beside it two 12 px lines (owner, 2026-10-24): the signal word in its tier colour (「信号强」, 「正在搜网」), then 「干扰低 · 负载正常」 in t2, or 「已 12 分钟」 without service; short of room the load goes, then the noise. Below: ↓ speed Nunito 20 + ↑ in 15, and a 24 px speed wave (log scale, same as System's chart) along the bottom, stopping 92 px from the right. With a hint: the hint (13/17, up to **3 lines**, width 194, broken by `kit::wrap_cjk`) replaces speed and wave. Right: 74 px SINR ring centred in the body (arc coloured by quality, empty track when there's no signal, value 「—」).
- **Status card, footer** (a hairline on top, same tint, not a separate card): left = active carriers (band label + bar whose width is proportional to MHz and fills whatever the label and right text leave), right = total (「2 条聚合 · 120 MHz」「锚点 + 5G · 120 MHz」). When there's a hint, the right end is the **action** instead (acc colour, ≥40 hit height): 「查原因 ›」 (opens diagnosis), 「换这张上网 ›」 (dual SIM, data card without service, two-step confirm), 「去启用 ›」 (no SIM). No carriers: the left becomes one t2 status line (「待机卡 CMLink 信号好」「eSIM：CTM、CMLink」).
- **Scenario card, header** (`indW` band): 28 px icon disc, scenario name 20b in `ind` (`…` if long; six CJK characters fit), mode pill on the right: 「● 自动」, 「判断中 ●●○」 (filled dots = hits so far), or 「恢复自动 ›」 (fblue, when pinned; two-step confirm). Second line = why, 12 t2, key word in `ind`: 「因为看到家里 Wi-Fi Home-5G · 08:41 起」, 「因为在日本（卡是澳门的）· 到了 2 小时」, 「可能切到外出：家里 Wi-Fi 2 次没看到」, 「已固定 · 你 10:20 手动选的」. Long SSIDs keep both ends (first n−6 characters + `…` + last 5).
- **Scenario card, left column = CHILL** (the whole mihomo service): head row = icon · 「CHILL」 · mode chip (规则 / 全局 / 直连留 AI / 全直连) · run state on the right (「● 运行中」, 「▲ 没网」, 「▲ 过热暂停」, 「■ 已关」). Below, one line per traffic class: colour tag (国内 green, 其余 blue, AI violet, 版权 / IP teal, 全部 grey) · exit bold (直连 / 节点 / 家里 / —) · place (12 t2, `…`) · latency on the right: a 7 px dot + number + ms (green < 150, orange < 400, red ≥ 400); 「■ 已暂停」 red when the AI gate has shut; 「—」 with no data; nothing when the exit itself is 「—」. **Up to four lines** share the space under the head evenly (four lines = 20 px pitch). Five or more: keep the first three and fold the rest into one grey 「+N」 line (exit named if they share one, tags joined with 「/」, worst latency shown). One or two lines get a t3 sentence explaining the mode (「全局：所有流量都走这个节点」).
- **Scenario card, right column = Tailscale** (100 px; a separate VPN that the home exit rides on): state 15b (已连接; 「▲ 连不上」 warn; 「■ 已关」 t3), peers online 「3/5 在线」, and the path to home: 「家里 直连」, 「▲ 家里 中继」 or 「▲ 家里 不通」 (warn), 「无家里出口」 when off. Spread evenly in the column.
- **Bottom row**: three cells (radius 18, label 11 t3 / value Nunito 800 16 / sub 11 t2): Wi-Fi (devices 「6 台」 + SSID, or 「关」 + 「外出时关」), 今日 (GB + 「月 59.5 GB」), 电池 (hours left + %; low = red 「▲18%」 + 「约 1h 40m」; charging = % + green 「充电 50m 满」; hot = 「▲ 偏热 46°C」 warn). A **notice** takes the first two cells (blue dot, up to two lines, 「1/2」 when several, 「知道了」 44×44 on the right); the battery cell always stays.
- **Below the fold** (scroll): landing, Tailscale details, then **今天** (touch 1.16.0): one card, title 「今天」 with 「每小时平均信号」 on the right; 24 hour cells (9 px, 2 px apart, radius 3) coloured by the hour's average SINR in the core's tiers (green ≥ 13 dB, orange ≥ 0, red below; track = no reading; hours still to come not drawn), the RAT word (n11 t2) above the hour it changes, a 2 px t1 bar under the hour now, 0 / 6 / 12 / 18 / 24 under the strip; then the events newest first, one 24 px line each: HH:MM (n12 t2) · 7 px tone dot · text (cj12, `…`), at most eight, then 「还有 N 条」 (t3). No events: 「今天还没有要记的事」. Clock not trusted: only 「设备时间还没校准，时间线暂停」 in t3. No timeline from the core (older core, long-term layer off): no card. No rings, bars or frames: the cells are the chart. The 5-minute speed chart (download and upload) moved to System (owner, 2026-10-24): the status card's wave covers Home. Detail pages are unchanged: carriers in Cellular; CHILL rules/nodes and Tailscale peers in Exit.
- Taps: status card → Cellular (or diagnosis via the footer action); scenario header → Scenarios; CHILL column → Exit; Tailscale column → Exit › Tailscale; cells → Wi-Fi / traffic / System › battery.
- Core data (core 1.50.0): the matched Wi-Fi in `GET /api/scenario` (`matched {id, ssid, since}`, `enter_hits`/`exit_misses` beside `candidate`/`hits`/`misses`; never in the public status); the footer action target `story.fix` (`sim_enable` / `cell` / `placement` / `diag:<layer>` / `op`; the swap-SIM offer stays the touch's, it needs the dual-SIM readings); direct-exit latency `GET /v2/latency/direct` (on demand only, one probe per 60 s; node and AI latency come from CHILL's last delay test, home latency from a Tailscale ping); today's timeline `GET /v2/timeline/today` (events since local midnight + hourly `sinr_avg`/`rat`). Touch 1.16.0 reads them: `/api/scenario` once a minute at most and only on a lit Home (sooner when the public summary changes), the latency once a minute on a lit Home (shown 3 minutes at most; 「—」 on failure), the timeline every 5 minutes on a lit Home. The footer action follows `story.fix` after the touch's own op / no-SIM / swap checks: 「摆放模式 ›」 (placement), 「去打开 ›」 (cell → Cellular), 「查原因 ›」 (diag:*), 「去启用 ›」, 「详情 ›」; without `fix` the old rules. diag:<layer> opens the diagnosis scrolled to that layer's row (data → bearer, internet → e2e), tinted `washW` for 1.6 s once the run is done; no such row: the top. While the scans miss the Wi-Fi that brought the scenario: 「可能切到外出：家里 Wi-Fi 1 次没看到」 (the candidate's name from the config). Without the new data each part draws as touch 1.15 did.

**Placement page** (摆放模式, Cellular › 排查; owner review 2026-10-24, mocks v1–v10 in iCloud `u60-design-review/2026-10-24-摆放模式/`, the formula and its sources in `docs/designs/travel-assist.md` §8): one score answers "is here good", and the owner walks around with it. The device has no compass or motion sensor, so it never claims a direction or a place name: places are 「第 N 处」 with the time they were recorded.
- **Score** 0–100 = the share of this link's fastest speed (touch 1.13.4): 3GPP TR 36.942 Annex A.2's SINR-to-throughput curve (0.6·log2(1 + SINR), from −10 dB, capped at 4.4 b/s/Hz ≈ 22 dB; continuous) × a width discount (the primary carrier's own MHz — SA the NR cell, 4G the LTE cell, NSA anchor + NR leg; secondary carriers are left out because the base station adds them only for a download — ÷ the widest seen this visit; throughput is linear in bandwidth, TS 38.306 §4.1.2). RSRP and RSRQ are not multiplied in (SINR already carries the noise and interference, and no published model turns them into a factor); they name the reason. The SINR is smoothed over about 3 s, so the score glides when a reading steps. Tiers: ≥ 80 很好 (≈ 18 dB), ≥ 60 良好 (≈ 13 dB), ≥ 40 一般 (≈ 8 dB), else 差. Two places within 8 points are the same.
- **Status card** (tone-tinted as on Home, 198 px): verdict cj22b left, the score Nunito 36 right (the countdown 「5 s」 in acc while recording), two 13 px lines under it (what to do, with 「比 5 秒前 +8，继续往这边」 while walking; then why: the biggest deduction with its reading, 「差在干扰：SINR 9.6 dB，转个方向或靠窗」, or 「干扰低、信号强、基站不忙」 at 80+), and the score's last 3 minutes as Home's wave (filled area + 3 px line in the tone colour) along the bottom 126 px; each recorded place is a 20 px circle on the line at the moment it was taken (number inside, the best filled), the best also a 1.5 px line across.
- **Score card** (66 px): five columns, each a number (Nunito 20), its name and what it means (11 px): 总分 (= the big number, with 很好 / 良好 / 一般 / 差) | a rule | SINR (dB, 「77 分」) · 频宽 (MHz, 「扣 62」 or 「不扣」) · 信号强度 (RSRP, 正常 / 弱, orange at −105 or under) · 基站负载 (RSRQ, 不忙 / 忙, orange at −15 or under). The total = SINR's points − the width's; strength and load are in SINR already and take nothing of their own (owner 2026-10-25: the total first, each part in its own place; two numbers beside two words looked odd). No bars. NSA reads quality from the LTE anchor only; the NR leg adds its width.
- **Records**: three cells like Home's bottom row (第 N 处 · 14:03 / score / 最好 · 也行 · 低 N · 差不多); the best is wash-green; an empty cell is outlined, the next one says 「下一处」. A fourth record drops the oldest one that isn't the best.
- **Cell ping-pong**: at the current spot (taking a place starts the count again), three or more returns within 2 minutes to a cell just left, with the score under 60, = 「卡在两个基站之间」, naming the two cells (3GPP's ping-pong; a cell is its PCI without the band, since with carrier aggregation the PCI is filed under another carrier's band; PCI 65535 readings don't count). It then comes ahead of every other verdict; a place recorded across a cell change is marked 「切过基站」 and not crowned unless all were. Places tied on score go to the later one.
- **Verdicts**: no records yet = by score (「这里可以放」 / 「这里一般」 / 「这里信号差」); recording = 「正在记第 N 处」 (8 s, hands off, 「取消这次」); two or more = 「放回第 N 处」 (or 「就放这里」 when now is within 5 of the best), 「放哪都差不多」 when all are within 5, 「放哪都不行」 when the best is under 30 (button 「换个房间再记」), 「不是摆放的问题」 when they are the same and the load discount is what holds the score down (button 「去测速」 opens the speed test).
- **Below the fold**: 这次最好 · 对比 · 在用 (band · PCI), 重新开始, and the note that a cell change is marked and the page keeps the screen on.

**Navigation** (2026-09-25): five tabs 首页 · 蜂窝 · Wi-Fi · 出口 · 系统 (Home · Cellular · Wi-Fi · Exit · System), split by "what do I want to do"; every function has exactly one home. Scenarios are a separate module that only drives Wi-Fi / CHILL / Tailscale; no cellular setting ever appears on its pages.
- A row with **›** always opens a page (or jumps to a tab); rows without › aren't tappable and have no pressed state.
- **Switches** take effect immediately; **buttons** are only for one-off actions (speed test, network search, mark all read, restore defaults); any choice that can drop the connection uses the two-step confirm below.
- **Back**: tap ‹, or swipe right from the left edge (x < 24). Once the swipe registers, that press is void and the row under the finger isn't tapped.
- Sub-pages always open at the top; jumping to a tab from a home summary also lands at the top; tapping the current tab again scrolls to top, tapping another tab keeps its previous position.
- Sub-page title = the text on its entry.

With no signal or no data, the exit IP and CHILL rates are dimmed so nobody takes them as live.

**Interpret first, parameters second** (2026-09-24, user: "don't expose raw parameters; put an understandable reading on top"):
- The status card's verdict, hint, RAT wording, carrier wording and signal rating are all decided by core `rust/src/screen.rs` `story()`/`say()` (moved from touch `ui_net_story()` on 2026-09-26), every branch unit-tested. The touch UI only displays them.
- Parameters (SINR, RSRP, RSSI, band, bandwidth) are small supporting text only.

**Home verdict table** (set 2026-10-01 in L2, core `say()` is authoritative; full Chinese/English hint lines in `docs/ui-glossary.md` §5):

| Priority | story.state | Large text (Chinese) | Large text (English, ≤150px) | Tone |
|---|---|---|---|---|
| 1 | (touch: no data yet) | 读取中… | Loading… | neutral |
| 2 | (touch: core down) | 读不到数据 | No update | neutral, values greyed but kept |
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

changing and revert_fail are overlaid by write operations (E4, core `screen.rs` `with_op()`, only in `net.home` of `/v2/screen`; `net.story` is always just the network verdict): switching RAT often passes through no-service, so it ranks above no-service to avoid a run of red; not overlaid on no SIM or mobile network off. The hint line carries the countdown sentence (「1:42 后没通就退回到自动」, "if not connected in 1:42, revert to auto") or, on revert failure, the current and last confirmed values. Other persistent results don't take the large text; they add one sentence at the end of the hint line.

English "slow" is just Slow; the reason goes to the right column (signal, Noise, Load) and the hint line. hot = the firmware itself flagged thermal throttling (uci `zwrt_zte_mc_tmp.cpe.hightemp_datalimit_status` non-zero, core `/state` `thermal.hightemp_limit`) — a fact, not an inference, so it ranks above limit and signal checks; unreadable counts as absent. stall = dialed, and in 30 seconds `rmnet_data0` sent ≥ 20 packets but received none (window kept by the core sampling loop, `docs/designs/slow-diagnosis.md` §4.1). Roaming is never a verdict by itself; the hint line gets 「；漫游中 / ; roaming」 appended.

**English length budget** (real Nunito sizes; core and agent tests approximate by characters, the touch render checks text stays inside its parent and the screen and siblings don't overlap):

| Place | Size | Limit | If over |
|---|---|---|---|
| Home large verdict | 24b | 150px (~10 chars) | change the word only |
| Right column signal word | 17b | 120px | change the word |
| Right column noise · load | 12 | 150px | change the word |
| Hint line | 13 | 3 lines (width 194, beside the SINR ring; broken by `kit::wrap_cjk`, no closing punctuation at a line start) | change the word, no 4th line |
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

Render test scenes: good, weak, nosignal, core-down, loading, nosim, abroad, lowbat, full-charging, long-names, empty, nsa, lte, 3g, nodata, 5ga, edge, each in dark and light theme.

**Home carriers** (carrier detail card): serving cell first (`nr_band/nr_bw/nr_rsrp/nr_pci/nr_channel`), then every `nrca`/`lteca` group appended as-is (the serving cell appears again inside them with a -140 floor value — that's the modem's configured carrier, not deduplicated); inactive carriers collapse to one t3 line.

**Every control that changes device state uses a two-step confirm**: the first tap turns it fillOrange 「再按一次确认」 ("tap again to confirm"); only a second tap within 5 seconds sends it (network selection, apply band lock, restore defaults, eSIM switch, switch to stock UI, delete SMS, turn off CHILL). The WiFi switch is the exception.

**Every tap must be visible on the spot** (2026-09-25, user tapped on 「情景 · 网络」 and nothing happened):
- **The first tap** redraws right in the callback, without waiting for the 1-second refresh. Buttons turn fillOrange; list rows get a full-row `washW` pale yellow background, 「再点一次确认」 ("tap again to confirm") on the right, and one sentence under the card saying what will happen (e.g. 「切到「外出」：Wi-Fi 开着 / 之后一直固定，点「自动」才恢复」, "switch to Away: Wi-Fi on / stays fixed until you tap Auto"); break the line deliberately when it spans two lines so no single character drops to line two.
- **Single-choice lists** (scenarios, network selection, etc.) use radio circles: filled = the option actually in effect, not "the one just tapped".
- **Tapping the option already in effect** says something (「现在已经是自动 · 判断为「在家」」, "already Auto · detected Home"), never silently ignored; tapping while busy also says something (「正在切换，稍等」, "switching, hold on").
- **After sending**, draw 「切换中… / 已发送」 ("switching… / sent") first, then send the request (it's synchronous; the UI pauses briefly), and keep it until the device has really changed; on timeout (scenarios: 45 seconds) state the reason. Success shows one green result line that fades after a few seconds.
- **Rejection** reasons are written in that card, not only at the top of the page.

**Features without a data source say what's missing**: neighbor cells / MIMO / BLER aren't available on this device (no `/modem/latest-signals`); the page states why instead of drawing an empty shell.

**Transactions (write-op layer, E4, design `docs/designs/write-op-layer.md` DD3, DD5, DD9, DD12, DD13, DD16)**: for every write that goes through core (currently network mode), all text is sent by core in the `op` block (`say/next/note/what/source/old/target`, each with `_zh/_en`); the touch UI only lays it out; vocabulary in `docs/ui-glossary.md` §13.
- Home and the matching settings page (Cellular › Network mode) show it in the existing status block, no extra progress card; other tabs and sub-pages get a **transaction row** under the title bar 「正在换制式 · 1:42 ›」 ("switching RAT · 1:42 ›", whole row ≥40 tall), which opens the **transaction page**.
- Transaction page, five layers: verdict → 「旧 → 新 · 来源 时间」 ("old → new · source time") → three progress lines (setting applied / registered / data, ✓ or …) → countdown (「1:42 后没通就退回到 X」 "revert to X if not connected in 1:42"; with auto-revert off 「还剩 0:48 · 自动退回没开」 "0:48 left · auto-revert off") → buttons. If core pushes nothing for 20 seconds: countdown greys out as 「停在 1:42 · 等设备响应」 ("stopped at 1:42 · waiting for device") and stops ticking.
- Buttons name concrete values: 「退回 X」 ("revert to X") primary on the left, 「保留 Y」 ("keep Y") secondary on the right; both two-step confirm, only one pending at a time, pending = fillOrange plus one consequence sentence. At expiry the device decides and the pending confirm is void; while reverting, only progress shows, no buttons.
- When done, the transaction row becomes a result row (「▲ 没通 · 已退回自动 ›」, "▲ not connected · reverted to auto ›"); 3-second kinds (switched, reverted, no confirm needed) only show if their end was actually witnessed; persistent kinds stay until 「知道了」 ("got it") is tapped (only recorded via core; touch and web dismiss together). Revert also failed (DD9): red takes the status block, shows current and last confirmed values, buttons 「再试一次退回到 X」 ("try reverting to X again") and 「重启设备」 ("reboot device").
- System › Change log: one entry per two lines (「制式 · 自动 → 只用 4G」 / 「10-03 14:32 · 网页 · 已切到只用 4G」, "RAT · auto → 4G only" / "… · web · switched to 4G only"), tap for detail; undo/redo live in the detail; un-undoable entries are greyed with a reason.
- Data service stuck (20 s without movement): controls that write via core grey out early with 「数据服务没响应 · 暂时不能改设置」 ("data service not responding · can't change settings right now"). Busy (another write running) doesn't grey out early; on tap, show the sentence core returns.

**Diagnose page rows** (网络诊断, deep diagnosis v2, `docs/designs/deep-diag-v2.md`; words in `docs/ui-glossary.md` §10): one row per layer in run order: Wi-Fi · SIM 卡 · 信号 · 限速 · 数据连接 · 蜂窝链路 · 域名解析 · 外网 · 设备时间 · 基站负载 · 代理 (only with CHILL on) (Wi-Fi · SIM · Signal · Speed cap · Data connection · Cellular link · DNS · Internet · Device clock · Cell load · Proxy), then the speed row. A layer whose prerequisite is bad is `blocked`: grey name and grey ● 「没测 · 前面的<层名>不通」 (Not checked · <Layer> is down), not counted in 「正在检查… N/M」, not in 「另有 N 处疑点」, never the main sentence. Blocked rows show on the diagnose page only, never on Home. Up to 12 rows (`DG_LAYERS_MAX`); the card scrolls.

**Speed test page** (测速, speed test v2, `docs/designs/net-performance.md` DR1–DR13; words in `docs/ui-glossary.md` §17): numbers first, conclusion right after. Result card: n36 multi-connection stable rate; one cj17 conclusion line from core (「▲ 慢在单连接、下载时卡」, at most two layer names then 「等 N 处」; 「● 没发现瓶颈」); ↑ and latency (「延迟 23 ms，下载时 210 ms」); server · connections · data used; the 「也测经 CHILL」 switch row (off by default, back off after each run, grey with its reason when CHILL is not running or roaming); the button with the data cost under it. Below the card, a 「分层」 section (title + rule, not a card): five layer rows in fixed order — 单连接, 代理, 下载时卡, 无线, 基站、Wi-Fi — written like the diagnose page rows (● ▲ in state colour, untested rows grey with 「没测 · 原因」, 40 px). Only the proxy row (when ▲, to Exit) and the radio row (SINR < 3, to Placement) are tappable, whole row, with 「›」. While running: stage text with step number at the top right (「3/5 · 多连接下载」; 6 steps with CHILL; finding a node is not a step), live rate as the big number, the method in the conclusion slot; layers appear only when done. Too few samples: the number in t3, 「▲ 样本不足，数字只供参考」, no layers. Rough test (diagnosis or roaming): normal colour, 「● 粗测 · 漫游中只测 5 秒」, no layers. Roaming: a t2 hint line, and the first tap only turns the button into 「走漫游流量，再按一次」. An older core (no `method_version`) shows the page exactly as before; no history list, no old-method marker.

**Regression tests**: `touch-ui/rs/build.sh test` (unit tests, including render checks on text/glyphs/geometry); after changing pages, check the page with `touch-ui/rs/still.sh` / `scripts/screenshots/touch.sh` on view models, and on the device's screen. The pixel comparison against the C render test (`parity.sh`) and its view models were retired on 2026-10-21; new view models are to be generated from the Rust UI.

**Known pitfalls**:
- `LV_USE_STDLIB_MALLOC` was once `LV_STDLIB_BUILTIN` (LVGL carves out its own small pool fully isolated from system memory; it kept crashing from 128KB all the way up to 2MB). The root cause was the isolation itself — the litehtml version (the old C touch UI) used system `malloc()` with 690MB+ free and never had this problem. Now `LV_STDLIB_CLIB` (real system malloc/string/sprintf), same path as litehtml, and all `LV_MEM_SIZE` config removed (only meaningful for BUILTIN). **This is the real fix; the earlier "fewer unnecessary `lv_label_set_text` calls" only mitigated it** — but that optimization stays; the two aren't mutually exclusive.
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

Redesigned on 2026-09-24 in a **Cohere + Figma** visual language (reference files and design/eng review notes live outside the repo as maintainer working notes). The touch UI keeps its own palette from section 4; the web **no longer mirrors** `touch-ui/rs/ui/src/theme.rs`. The old Cool Tech (`admin.css`, Outfit/DM Sans) is deleted.

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
- **Transactions** (E4, design `docs/designs/write-op-layer.md` DD4, DD5, DD8, DD15): writes through core (currently network mode) no longer read back themselves; the device confirms; text follows the core's `op` block, vocabulary in `docs/ui-glossary.md` §13.
  - Shell strip (alert bar style, `role=status`, revert failure `role=alert`): on every page, in progress 「正在确认 · 1:42」 ("checking · 1:42"), ≥640 adds 「制式 · 旧 → 新 · 来源」 ("RAT · old → new · source"); 「详情 ›」 ("details ›") goes to the matching page, and it isn't shown on that page. The countdown is `aria-hidden`; only phase changes are announced.
  - The matching page's status block becomes the transaction state: same five layers as touch, buttons 「退回 X」「保留 Y」 are tier-2 inline confirms; stacked vertically on mobile, full width, ≥44px.
  - System › Device › Change log (`/changes`): same two lines as touch; undo in the detail, tier 3 for anything that drops cellular (network mode), tier 2 otherwise; every attempt gets a new `op_id`, never resent. Settings pages end with 「上次改动 14:32 · 触屏 ›」 ("last change 14:32 · touch ›") linking to that entry (currently network mode only).
  - Data service stuck: the strip says 「数据服务没响应」 ("data service not responding"), writes via core grey out; web disconnects mid-change: the strip says 「和设备断开了 · 操作结果未知」 ("disconnected from device · result unknown"), no auto-resend, the result shows normally after reconnect. Old core (`supported:false` from `/api/ops`): no strip and no change log; writes read back as before.
- When data goes stale keep old values, grey them, write "N min ago"; never clear to 0.
- Icons are Phosphor: selected and tiles filled, the rest bold line.

**Naming**: the product name is always **CHILL**; RELAX never appears in any UI or clash/ShellCrash config.

**Two CHILL page constraints** (carried over from 2026-09-16 decisions 7A, 7B): zashboard is **not embedded in an iframe**; it opens in a new tab through the `:9090/chill-ui/` reverse proxy; device and subscription list rows don't stretch with the viewport.

**Control semantics**: region `radiogroup` + `radio`; switches `switch`; segments are a single-select toggle group; alert bar `role="status" aria-live="polite"`; dialogs have a title and description, and focus returns to the trigger on close.

**Live data** (same once it becomes push): when a number changes only the number changes — no reflow, no flash; inputs being typed in, expanded details and pending confirms are not overwritten by new data (DR12); screen readers announce only verdict and alert changes, not every refreshed reading (DR13).

**Current implementation (luyaz-web)**: tokens `crates/luyaz-web/static/nd/newdesign.css`; component styles in the same directory `nd.css`, `shell.css` (concatenated by `src/assets.rs`); contrast `node crates/luyaz-web/tools/contrast.ts > docs/web/contrast.md`; route table `crates/luyaz-web/src/routes.rs`; transaction bar & write confirmation `crates/luyaz-web/src/opbar.rs`, `crates/luyaz-web/templates/components/write.html`, and Core transaction specs `crates/luyaz-core/src/ops/spec.rs`; e2e tests `crates/luyaz-web/e2e/`; tier inventory `docs/web/controls-inventory.md` (its 3-tier classification rules are normative). When the implementation changes, edit this paragraph; the rules above stay.

---

**Speed test page (/tools/speedtest, speed test v2)**: the same order as the touch page (number → conclusion → layers). The status block's reason line carries the conclusion (「完成 · ▲ 慢在单连接」) and, while running, 「3/5 · 多连接下载」; only the step and the conclusion are announced, never the percentage or live rate (DR13). The result wall keeps 4 tiles: download (multi-connection stable, large), upload, idle latency, ping while downloading (replaces jitter). A 「分层」 `<ul>` sits between the wall and 「测速来源与节点」, rows as on the touch page with text equivalents for ● ▲; the proxy row links to the CHILL page. Details add whole-run average, jitter, connections and a read-only 「内核调参：开 / 关」 from the result's `net_tune`. The 「也测经 CHILL」 switch lives in the source section, under the same rules as on the touch page. The inline confirm states the cost of the run it will start (30 s / 750 MB, 40 s / 1.15 GB with CHILL, the roaming sentence). Against an older core the page is unchanged.

**Incidents on /alerts (2026-10-24)**: order 标题 → 状态区块 (open incidents first: 「▲ N 个事故还开着」 · newest 「内存超标 · 14:32 起」 (+N)) → 「事故 · 7 天」 → 「记录」 → 短信告警. Open ones always on top (not counted, not limited to 7 days), then those ended in 7 days newest first, at most 50, then plain 「还有 N 条，可用 MCP 或 tools/luyaz 查」. A row is a `<button aria-expanded aria-controls>` (≥56 px): 「▲ 内存超标 · 还开着」 / 「10-24 14:32 起 · 已 2h 10m · 可能相关：core 上机」 (and 「 · 可能相关：演练」 when a real-device test switch was on in the hour before, same boot); 「● … · 已恢复」 / 「● … · 已结束 · 3 次」 (a crash row whose window passed: not recovered). Opening reads `/v2/incidents/<id>`: 状态 → 同时观察到 → 开始 / 结束 / 持续 → 可能相关的上机 → 可能相关的演练 → 内存快照 → build · id → 归入的告警; a refresh keeps it open and drawn (DR12). An alert whose incident is drawn leaves 「记录」 (its unread mark moves to the incident row); one whose incident is not drawn, or every alert when incidents cannot be read, stays. States: skeleton, 「● 7 天内没有事故」, stale (`.nd-stale` + when + 重试), 「■ 读不到事故：…」 + 重试 + 健康, 「core 版本太旧，不支持事故」, details 「正在读取…」 / 「读不到详情 · 重试」 / 「这条详情已超过保存期限」 (only past the evidence's 30 days: an incident the 8-day list let go is read back from it). Only the status block's verdict is announced, never the ticking duration.

## 6. Anti-patterns (neither platform)

- One card per metric, stacked into a mosaic. Cards are **only** allowed two ways: grouped lists (one card holding a set of similar rows) and whole tappable modules (home Scenarios / CHILL / Tailscale cards, function tiles).
- Gradient backgrounds, neon glow, gradient text, real blur (except the slight backdrop blur of the web top and bottom bars). The touch UI's "tinted glass" is only for the floating tab bar, back button, bottom sheet and title bar right-side buttons, and it's an opaque solid color.
- Icons in colored circles as decoration; three-column equal feature grids.
- Colored left-border cards.
- Everything center-aligned; one large radius on everything. Radius tiers are deliberate: touch sheet 22, cards and tiles 20, options 14; web panels 16, cards 12, inputs and options 8; pills are half-height.
- Emoji as design elements (except flags in node names, which are data; the touch UI shows them as country codes).
- Big colored pills for state; use a dot plus text. Filled pills for selected items (segments, bands, exit options) don't count as state pills.
