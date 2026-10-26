# mock-agent — a stand-in for the device API (served by luyaz-core on the device)

A zero-dependency Node HTTP server that answers the admin web's `/api/*`
calls with fixed data, so pages can be built and tested without touching the
U60 Pro (MU5250). It backs luyaz-web's browser checks (`../fake-agent.mjs`, `../core-front.mjs`).
Reply shapes follow what luyaz-core sends today. Some fixtures still carry
`import type … from "../../../src/lib/api/schemas/…"` lines whose target is gone; Node drops
type-only imports, so nothing breaks at run time.

Data describes a user travelling in Taiwan: NR SA n78 with NR CA plus LTE CA,
RSRP −95 / SINR 18, battery 76 % charging,
Tailscale running with 5 peers (3 online), 2 unread SMS.

## Run

Node 26 runs the `.ts` files directly (type stripping), no install step:

```sh
# from source/manager
node crates/luyaz-web/e2e/mock-agent/server.ts                  # 127.0.0.1:9199 (MOCK_PORT to change)
MOCK_SCENARIO=weak,cmdfail node crates/luyaz-web/e2e/mock-agent/server.ts
```

The browser checks normally start it for you through `../fake-agent.mjs`; point a
local luyaz-web at that port instead of the core's `127.0.0.1:9460` to click
around by hand. It listens on IPv4 loopback only.

## Auth

- `POST /api/auth/login` (a mock-only shortcut; the core itself signs in on `/auth/login`
  with Basic auth) takes any non-empty password and answers `{ok:true,data:{token:"mock-token-<n>"}}`.
  A new login **voids the token before it**. Empty password → 401 `invalid password`;
  no password string → 400.
- Every other `/api/*` call except `GET /api/public/status` needs
  `Authorization: Bearer <current token>`, else 401 `{ok:false,error:"unauthorized"}`.
- Tokens expire after `MOCK_TOKEN_TTL` seconds (default 3600, one hour).

## Scenarios

Set with `MOCK_SCENARIO` (comma-separated), per request with the
`X-Mock-Scenario` header (overrides the runtime setting for that request), or
at runtime:

```sh
curl 'http://127.0.0.1:9199/__mock/scenario?set=weak,missing'
curl 'http://127.0.0.1:9199/__mock/scenario?set=normal'   # back to normal
curl  http://127.0.0.1:9199/__mock/scenario               # show current
curl  http://127.0.0.1:9199/__mock/state                  # shared state + route list
```

| Scenario | What happens |
|---|---|
| `normal` | Default data. |
| `weak` | Poor signal: RSRP about −115, SINR about −2, 1 bar. |
| `nosignal` | No service: disconnected, radio values null, no WAN IP. |
| `stale` | `GET /api/network/signal` and `/api/network/speed` hang `MOCK_STALE_MS` (default 12 s, past the planned 9 s client timeout) before answering. |
| `missing` | GET payloads come back with fields absent or `null` (generic transform; a few endpoints do their own). |
| `cmdfail` | Every write returns 500 `{ok:false,error:"mock failure"}` (login and read-over-POST endpoints excluded). |
| `fakesuccess` | 200 `ok:true` but the downstream failed: Tailscale returns `data.error`. |
| `reboot-token` | After `POST /api/device/reboot` is acknowledged, every connection is dropped for `MOCK_REBOOT_MS` (default 20 s; `/api/public/status` too), and all tokens issued before are invalid when it comes back. Without this scenario reboot just returns ok. |
| `step2-timeout` | The second write of a multi-step operation (a write arriving within `MOCK_STEP_WINDOW_MS`, default 3 s, of the previous write finishing — e.g. the second `POST /api/cell/band/nr`, or `POST /api/doh/disable` after `PUT /api/router/dns`) hangs `MOCK_STEP2_MS` (default 20 s), then completes. |
| `down` | Every socket is destroyed (connection reset). Only `/__mock/*` still answers, so you can switch back. |
| `carriers8` | The signal payload carries 8 carriers for the carrier table. |
| `firmware-b27` | Shapes recorded from a real U60 Pro (MU5250) on firmware B27 (2026-09-25; values are fake): UPnP as `router_get_upnp` answers it (`enable_upnp`…), QoS `{}` (unconfigured; the QoS PUT still 503s — its setter is `router_set_qos` with rate limits), `/api/cell/stc/params`, `/api/cell/stc/status` → 503 `… (Method not found)`; port-forward / filter-rules / domain-filter → `{}` when empty (rules or domains added while the scenario is on come back as `{rule_list}` / `{blocked_domains}` — the real non-empty shape is unconfirmed); `/api/router/vpn` → `{alg_sip_enable}` only; APN profiles with integer `pdpType` / `pppAuthMode` / `roamingPdpType` + `extraInt1`; SMS capacity with per-box `sms_{sim,nv}_{rev,send,draftbox}_total`, no `sms_simused_total` and `sms_nvused_total: 0`; signal-detect progress as strings (`""` idle) and results `{}` when none; SA signal without `lte_pci` / `cell_id` / `wan_active_channel`, numeric `rmcc` / `rmnc`; public status `scenario.last_switch: null`. The default persona keeps the richer shapes. |
| `old-agent-names` | Builds before 2026-09-25 asked for `router_get_upnp_switch` / `router_get_qos_switch`, which B27 doesn't have: those GETs answer 503 `… (Method not found)`. Combine with `firmware-b27`. |
| `nbrscan` | `POST /api/cell/neighbors/scan` runs the old simulated neighbour scan instead of the real device's passive read (`not_reported`, no cells, as in SA). Used by the cell-lock page tests. |
| `diag-waiting` | `POST /api/diagnose` first waits 3 s (`state: "waiting"`, `waiting_for: "speedtest"`), then runs. |
| `diag-slow` | `POST /api/diagnose` takes 4 s per layer instead of 0.8 s, so a run stays going long enough to see the busy 409 elsewhere. |
| `diag-sim-warn` | `POST /api/diagnose`: the sim layer is warn (eSIM channels all taken), so it is the main cause. |
| `diag-bearer-bad` | `POST /api/diagnose`: bearer bad (not connected); link, dns, e2e, clock, crowd (and proxy) are `blocked`. |
| `finding` | The core's open finding (T6): registered, data connection down, confidence sure — `GET /api/diagnose` `data.finding` (evidence per layer, `iccid_tail`) and `/api/public/status` `network.finding` (symptom only). `POST /api/diagnose/dismiss` keeps it away for 5 s (the core's 24 h, shortened). |
| `verdict-stall` / `verdict-crowd` | `/api/public/status` `network.verdict` is `stall` / `crowd` (the core's home verdict) while the signal itself stays good. Otherwise the verdict follows the persona: `weak`, `nosvc` (nosignal), `airplane`, else `ok`. |
| `home-network` | `/api/netinfo` says not roaming (`roaming: false`); the v2 speed test then runs in full. Default is roaming, where the mock core runs the coarse 5 s test. |
| `speed-v1` | `/api/speedtest/*` answer as an old core (before core 1.48.0): no `method_version` or other v2 fields. |
| `speed-short` | the v2 speed test ends `sample_short: true`, without a verdict. |
| `mf-alldown` | CHILL's manual-first notice says the hand-picked node and its fallback are both down (`notice_kind: "alldown"`), in `/api/public/status` and `/api/services/chill`. |

Scenarios combine, e.g. `weak,missing` or `carriers8,stale`.

## Diagnosis

`fixtures/diagnose.ts` stands in for the core's deep diagnosis (`/api/diagnose`): a run fills one layer every
0.8 s (Wi-Fi, signal, speed cap, cellular link, cell load, plus proxy while
the proxy service runs). With `weak` active when the run starts, signal comes out bad and
the link suspect (main cause "Weak signal", +1 more); otherwise nothing is
found. `/api/diagnose/speed` measures for 3 s, then gives a "Direct ↓ … Mbps"
row; feedback is taken once per run. While a run is going, the speed test,
network search and manual register get a busy 409
(`busy: "diagnose"`, 正在诊断，约 N 秒后再试).

## Writes

Writes update in-memory state so the matching GET reads the change back (Wi-Fi
settings, band lock, APN profiles, SMS read /
delete, scheduler jobs, …). Values that several endpoints report (Wi-Fi on,
Tailscale, unread SMS, home mode, band lock) live in `shared.ts`.
Restart the server to reset everything.

## Layout

| File | |
|---|---|
| `server.ts` | HTTP server, CORS, auth, scenarios, `/__mock/*` control. |
| `routes.ts` | All fixture routes in one list (used by server and recorder). |
| `lib.ts` | `Route` / `Ctx` / `Reply` types, scenario names, helpers. |
| `shared.ts` | Cross-area state. |
| `fixtures/<area>.ts` | Routes + data per area. |
| `record.ts` | Read-only recorder for the real device (see below). |

There is no type-check step: the schema files the fixtures were once checked
against are gone (X13).

## CORS

Requests from `http://localhost:*` / `http://127.0.0.1:*` get
`Access-Control-Allow-Origin: <origin>` on every response (including 401/404/500),
allowed headers `Authorization, Content-Type, X-Mock-Scenario`, methods
`GET, POST, PUT, DELETE`. `OPTIONS` → 204.

## record.ts (for later, on the real device — needs the user's OK)

Fetches a short list of GET endpoints from the real core, saves the replies and
prints which keys differ from the mock. Each entry on the list names the core
handler that was read to make sure it changes nothing; anything that scans,
writes uci, reloads, starts processes that change state, sends AT commands or
talks to the eUICC stays off the list, and the file says why.

```sh
RECORD_CONFIRM=yes AGENT_URL=http://127.0.0.1:9460 TOKEN=<bearer> node crates/luyaz-web/e2e/mock-agent/record.ts
```

Output goes to `crates/luyaz-web/e2e/mock-agent/recorded/<path>.json` (not meant to be
committed — it contains real device data). It refuses to run without
`RECORD_CONFIRM=yes` and an explicit `AGENT_URL`.
