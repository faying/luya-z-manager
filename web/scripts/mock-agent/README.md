# mock-agent — a stand-in for zte-agent

A zero-dependency Node HTTP server that answers the admin web's `/api/*`
calls with fixed data, so pages can be built and tested without touching the
U60 Pro (MU5250). Nothing in `src/` imports it, so it never ends up in
`npm run build` output.

Data describes a user travelling in Taiwan: NR SA n78 with NR CA plus LTE CA,
RSRP −95 / SINR 18, battery 76 % charging,
Tailscale running with 5 peers (3 online), 2 unread SMS.

## Run

Node 26 runs the `.ts` files directly (type stripping), no install step:

```sh
cd web
node scripts/mock-agent/server.ts                  # port 9199 on 127.0.0.1
MOCK_SCENARIO=weak,cmdfail node scripts/mock-agent/server.ts
```

In the browser (dev server on `localhost:3000`), open the console once:

```js
localStorage.setItem("u60.agent_url", "http://localhost:9199")
```

(Use `http://127.0.0.1:9199` if your browser resolves `localhost` to `::1`
first; the server binds IPv4 loopback only.) Log in with any non-empty
password. Remove the key to go back to the device:
`localStorage.removeItem("u60.agent_url")`.

## Auth

- `POST /api/auth/login` with any non-empty password → `{ok:true,data:{token:"mock-token-<n>"}}`;
  each login issues a new token and **invalidates the previous one**.
  Empty password → 401 `invalid password`; missing field → 400.
- Every other `/api/*` call except `GET /api/public/status` needs
  `Authorization: Bearer <current token>`, else 401 `{ok:false,error:"unauthorized"}`.
- Tokens expire after `MOCK_TOKEN_TTL` seconds (default 3600, same as the agent).

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
| `old-agent-names` | The agent before 2026-09-25 called `router_get_upnp_switch` / `router_get_qos_switch`, which B27 doesn't have: those GETs answer 503 `… (Method not found)`. Combine with `firmware-b27`. |
| `nbrscan` | `POST /api/cell/neighbors/scan` runs the old simulated neighbour scan instead of the real device's 410 (the stock scan drops mobile data and returns no cells). Used by the cell-lock page tests. |

Scenarios combine, e.g. `weak,missing` or `carriers8,stale`.

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
| `fixtures/<area>.ts` | Routes + data per area, typed with `satisfies` against `src/lib/api/schemas/<area>.ts`. |
| `record.ts` | Read-only recorder for the real device (see below). |
| `tsconfig.json` | Type-checks this folder + the schemas with `.ts` import extensions allowed. |

Type-check: `npx tsc --noEmit -p scripts/mock-agent` (the root config
excludes `scripts/`; this one allows `.ts` import extensions and also checks
`src/lib/api/schemas`). The schemas are also covered by the root
`npx tsc --noEmit -p .`.

## CORS

Requests from `http://localhost:*` / `http://127.0.0.1:*` get
`Access-Control-Allow-Origin: <origin>` on every response (including 401/404/500),
allowed headers `Authorization, Content-Type, X-Mock-Scenario`, methods
`GET, POST, PUT, DELETE`. `OPTIONS` → 204.

## record.ts (for later, on the real device — needs the user's OK)

Saves real agent responses for an audited allowlist of side-effect-free GET
endpoints and prints a key diff against the mock fixtures. Every allowlist
entry names the handler `file:line` that was checked; everything that scans,
writes uci, reloads, spawns state-changing processes, sends AT commands or
talks to the eUICC is excluded, with the reason in the file.

```sh
RECORD_CONFIRM=yes AGENT_URL=http://192.168.0.1:9090 TOKEN=<bearer> node scripts/mock-agent/record.ts
```

Output goes to `scripts/mock-agent/recorded/<path>.json` (not meant to be
committed — it contains real device data). It refuses to run without
`RECORD_CONFIRM=yes` and an explicit `AGENT_URL`.
