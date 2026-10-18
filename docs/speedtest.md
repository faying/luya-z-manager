# On-device speed test

The admin page (`/tools/speedtest`) and touch screen use the same core engine.
Measurements originate on the device, not in the browser. The source and server
used are included in each result. Results from different servers or providers
are not directly comparable.

## Providers

- **Auto** tries Ookla, then configured LibreSpeed endpoints if Ookla discovery,
  latency or a transfer fails. It tries at most three nodes per provider.
- **Ookla** or **LibreSpeed** selects that provider without switching to another.
- Selecting a specific node also fixes its provider; node IDs are scoped to a
  provider. Existing requests with only `server_id` continue to mean Ookla.

There are no bundled LibreSpeed nodes. A working internet connection does not
ensure that a particular measurement service is reachable. To use the fallback,
add a LibreSpeed server reachable from the device's network, for example your
own server or an operator's server that permits these tests. Reachability on
Russian networks must be verified on the affected network.

## Configure LibreSpeed

On the admin speed-test page, expand **Configure endpoints**, edit the JSON,
then save and confirm. The page verifies the saved value. Saving does not run a
measurement. The touch screen uses the same saved nodes.

```json
{
  "librespeed_servers": [
    {
      "id": 1,
      "name": "My server",
      "server": "https://example.com/speedtest/",
      "dlURL": "garbage.php",
      "ulURL": "empty.php",
      "pingURL": "empty.php"
    }
  ]
}
```

`example.com` is a placeholder, not a working test endpoint. Use the backend's
actual paths; a Go backend commonly uses `garbage` and `empty`. Up to eight
servers are supported. IDs must be unique positive integers, names at most 64
UTF-8 bytes, and base URLs must end in `/`. HTTP and HTTPS are accepted; embedded
credentials, fragments and endpoints on a different origin are rejected.
Setting `librespeed_servers` to `[]` removes the configured fallback.

The core saves this in `speedtest.json` under its state directory (normally
`/data/luyaz/core/`). No remote LibreSpeed directory is needed. An unreadable or
invalid configuration is reported on the page and is not silently overwritten.

## API and errors

All paths below are relative to `/v2` on the core:

- `GET /api/speedtest/config`: `{ok,data:{config,load_error}}`.
- `PUT /api/speedtest/config`: the configuration object above; validates and
  saves locally. It is subject to the existing compatibility-write gate.
- `GET /api/speedtest/servers`: cache-only `{ok,data:[...],loading}`. An initial
  empty list with `loading:true` is not an outage; poll for the updated cache.
  One background refresh is shared by list reads and starting tests, including
  a 30-second negative-cache backoff. With no usable nodes a completed failed
  discovery returns a structured service error. An explicit start permits a
  new attempt after a previous discovery failure. The explicit server-list retry
  uses `?refresh=1` to bypass the backoff without duplicating an active refresh.
- `POST /api/speedtest/start`: `{provider:"auto"|"ookla"|"librespeed",server_id?}`.
  Returns `200 {ok:true,data:{status:"started"}}` as soon as the worker is
  admitted. Node discovery and unknown-node errors are reported by progress.
  A concurrent test or diagnosis is still refused synchronously with 409.
- `GET /api/speedtest/progress`: includes the `discovering` phase and `provider`,
  `error_code`, `error`, `error_en`, `fallback_reason` in addition to existing
  speed and byte fields. Provider errors do not change core reachability.
- `POST /api/speedtest/stop`: also cancels discovery and prevents further
  attempts. A request already in flight can take up to its remaining timeout
  to finish (3 seconds for discovery/download/ping, 10 seconds for upload).

Codes include `provider_unreachable`, `provider_timeout`, `no_servers`,
`invalid_server_list`, `provider_not_configured`, `transfer_failed`, and
`budget_exhausted`. A timeout cannot prove censorship; the UI describes a
possible network restriction without asserting one. Failed transfers never
become a successful zero-Mbps result.

One full run shares a 60-second deadline and 512 MB test-payload budget across
all fallback attempts. Byte counters include failed attempts; speed metrics
belong to the selected attempt. Protocol overhead is additional. Diagnosis
continues to use its smaller shared budget: up to five seconds / 30 MB,
download only. Stop, mutual exclusion and limits are shared by both UIs.

See [LibreSpeed's custom server-list format](https://github.com/librespeed/speedtest-cli#use-a-custom-backend-server-list).

## Verification

Run the `luyaz-speedtest`, `luyaz-core` and `luyaz-web` tests and
`touch-ui/rs/build.sh test`. Tests use loopback servers, including delayed and
unreachable discovery, fallback success, manual-provider isolation, cancellation,
configuration persistence, transfer failures, and shared budgets. They do not
contact public speed-test servers or reproduce an actual Russian network block.
