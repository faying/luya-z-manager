# Battery time estimate

**English** · [中文](battery-estimate.zh-CN.md)

There is one implementation: `crates/luyaz-policy/src/battery_eta.rs`. The core, luyaz-core (`crates/luyaz-core/src/plugin/battery_eta.rs`), feeds it a sample every 5 seconds when it reads the battery (whether or not anyone is watching).
The result is in `estimate` of `GET /api/battery` and `battery` of `GET /api/screen`; the admin web UI and the touch screen only turn it into text.

`battery-estimate/fixtures.json` holds examples of these rules and is run by the `battery_eta.rs` tests. When changing the rules, change this doc (both languages), the fixtures and `battery_eta.rs` together.

## `state` in the result

| state | Meaning | UI |
|---|---|---|
| `ok` | Normal | Shown by kind |
| `estimating` | Just started, no samples yet | "—" |
| `stale` | Sampling thread stopped for over 20 s (values are the last ones) | "—" |
| `unavailable` | Battery can't be read | "—" |

## Inputs

- Samples, ascending by time: `[t seconds, battery current µA, charger connected]`. Positive current = into the battery (charging), negative = discharging (sign of sysfs `battery/current_now`). Charger state comes from the core's `charger_connect`; when unknown, the previous value is kept.
- `soc`: `battery/capacity`, 0–100.
- `charge_full_uah`: `battery/charge_full`; optional.
- `charge_counter_uah`: `battery/charge_counter` (remaining charge); optional.
- `target_pct`: the charge-protection limit when protection is on and the limit is within 50–100, otherwise 100.
- `paused_at_limit` = charge protection on AND `charging_stopped` AND not a manual stop (`manual_override` false) AND charger not definitely unplugged (unknown `charger_connect` counts as plugged in). A manual stop does not count as reaching the limit.
  When charging is stopped the firmware cuts USB input (`usb/online`, `usb/current_now` go to 0 and the battery discharges at about 0.35 A), so the sample's charger state is ignored here.

The kernel's `power_now` and `time_to_full_now` are not used (they contradict each other in practice).

## Average current

1. Walk back from the newest sample and stop at any of: time earlier than "newest t − 180 s"; current sign differs from the newest (0 counts as positive); charger state differs from the newest.
2. Time-weighted: weight of sample i = next sample's t − its t; the last sample has weight 0. If the total weight is 0 (only one sample left), use the newest current directly.

## Result (checked in order, first match wins)

| Condition | kind | minutes |
|---|---|---|
| No samples | `unknown` | null |
| `paused_at_limit` | `paused_at_limit` | null |
| Charger connected and `soc ≥ target_pct`, and average current ≥ 0 or \|average\| < 50 000 µA (a full battery shows a few mA of discharge) | `reached_target` | null |
| \|average\| < 50 000 µA | `unknown` | null |
| Average > 0: `unknown` if `charge_full_uah` is missing; otherwise needed = charge_full × (target − soc) / 100, minutes = needed / average × 60 | `charging_eta` | rounded |
| Average < 0: remaining = charge_counter, else charge_full × soc / 100, `unknown` if both missing; minutes = remaining / \|average\| × 60 | `discharging_eta` | rounded |

## Display

The UI is Chinese; the strings below are translations.

- `charging_eta`: target 100 → "full in about X", otherwise "N% in about X".
- `discharging_eta`: "about X left".
- `reached_target`: target 100 → "Fully charged", otherwise "At limit N%".
- `paused_at_limit`: "At limit, charging paused".
- `unknown`: "—".
- X: under 60 minutes "N min", otherwise "H h M min" (just "H h" when M is 0). The touch screen must not use `%f`.
- Any `state` other than `ok` shows "—".
