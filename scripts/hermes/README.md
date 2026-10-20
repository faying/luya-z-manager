# Hermes: Luya-Z alerts to chat

`luyaz_alerts.py` asks each Luya-Z device for new alerts through `luyaz-mcp` and prints one
line per alert. Hermes runs it as a cron job with `--no-agent`, and whatever it prints goes to
the chat (no output = no message). Design: `docs/designs/u60-mcp.md`, section 「多台 U60」 (D3–D7).

This is the master copy. The copy Hermes runs (`~/.hermes/scripts/luyaz_alerts.py`) is a plain
copy of this file. The script holds no addresses, tokens or channels.

## Device list (recommended, needed for more than one device)

`~/.config/luyaz/devices` is the list `luyaz-mcp` also reads. It has one line per device, and
`#` starts a comment:

```
# name    address                 token file                      device id (written by enroll)
home      192.168.0.1             ~/.config/luyaz/home.token
travel    http://100.64.0.9:9090  travel.token
```

- **address**: `host[:port]` (port 9090 when none is given) or `http://host[:port]`.
- **token file**: the first non-comment line holds the token, or the device's `name token` line.
  A relative path is relative to the list's directory.
- **device id**: after adding a line, run `luyaz-mcp enroll <name>`. It reads the device's
  `device_id` and writes it into the fourth column. From then on, an answer from another device at
  that address is refused. It needs an admin web (lweb) that sends `device_id`.
- If one line is wrong, or a name is used twice, the whole list is refused. Every tool then says
  which line is wrong, and the script forwards nothing and says so once.

### One token per line: Hermes and this script may need two lists

A list line has one token file. When Hermes's `luyaz` MCP entry uses a token marked `act`
(changes allowed) and this script should keep a read-only token, give the script its own list
with the read tokens, and enroll it there too:

```
LUYAZ_MCP_DEVICES=~/.config/luyaz/devices.alerts luyaz-mcp enroll home
python3 luyaz_alerts.py --devices ~/.config/luyaz/devices.alerts
```

Once a list exists, luyaz-mcp ignores `LUYAZ_MCP_URL` and `LUYAZ_MCP_TOKEN(_FILE)` from Hermes's
MCP config: the token comes from the list.

## Running it

```
python3 luyaz_alerts.py [--bin PATH] [--devices PATH] [--state-dir DIR] [--url URL --token-file PATH]
```

| option | environment | default |
|---|---|---|
| `--bin` | `LUYAZ_MCP_BIN` | `~/.config/luyaz/bin/luyaz-mcp` |
| `--devices` | `LUYAZ_MCP_DEVICES` | `~/.config/luyaz/devices` |
| `--state-dir` | `LUYAZ_ALERTS_DIR` | `~/.config/luyaz` |
| `--url`, `--token-file` | `LUYAZ_MCP_URL`, `LUYAZ_MCP_TOKEN_FILE` | without a list only |

Files in the state directory:
- `luyaz-alerts.json`: the cursors per `device_id` (the device's own id, so renaming a line does
  not lose the place), the last id seen per name, and the notices already given.
- `hermes-alerts.json`: the old single-device cursor. It is used when there is no list, and when
  the only listed device's admin web is too old to send a `device_id`. It is carried into
  `luyaz-alerts.json` when the list has exactly one device and that device is enrolled; the file
  is then renamed `hermes-alerts.json.migrated`.
- `hermes-alerts.last`: the night throttle. From 23:00 to 08:00 the script asks at most every
  30 minutes, because each ask wakes the device's cellular link.

Behaviour:
- A device that cannot be reached is skipped quietly (stderr only), and the other devices go on.
  It catches up from its cursor on the next run.
- An enrolled device that now answers as another device is paused, and its cursor is kept. The
  script sends one message saying so and asks you to run `luyaz-mcp enroll <name>`. After
  enrolling, the new device starts from its current position (nothing is replayed).
- The first time the script sees a device, it only records the position. Old alerts are not sent.
- Each device gets up to 6 pages of 50 per run (the device's whole queue). If the device already
  dropped some alerts, the script says so (`gap`).

## Order when switching from the single-device script

1. Ship an lweb that sends `device_id`.
2. Write the list with this device's line and run `luyaz-mcp enroll <name>`.
3. Copy this script over `~/.hermes/scripts/luyaz_alerts.py`.

The old cursor then moves over without repeating or missing an alert.

Do not copy the script before the list exists. Without a list, the script runs in single-device
mode, and nothing in it names the device. Every run would then fail quietly (stderr only), unless
the cron command passes `--url` and `--token-file`. Also, the old script only works while the
list has one line. Swap it before adding a second device.

## Tests

```
python3 -I -m unittest discover -s scripts/hermes -p 'test_*.py'
```

The tests run against `fake_luyaz_mcp.py` (a stand-in that answers from a JSON file) in a
temporary directory.
