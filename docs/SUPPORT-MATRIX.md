# Support matrix: what has actually been verified

**English** · [中文](SUPPORT-MATRIX.zh-CN.md)

Snapshot: 2026-11-23. This table only records what was seen working on a real device. A successful build, a passing offline test, or a result on one unit is never carried over to another unit, firmware or model.

- **Verified**: seen working on a real device of this row (date given).
- **Design only**: code exists, but it was only tested with mocks or offline.
- **Unknown**: no evidence, or the evidence contradicts itself.

## Rows

| Row | Hardware | Firmware |
|---|---|---|
| A | MU5250, single SIM slot | CN `B31`, upgraded from `B27` (B27 still in the other A/B slot) |
| B | MU5250, dual SIM (`support_dual_sim=1`) | CN `B31`, factory, never upgraded |
| C | MU5250, a contributor's unit, modified by a third party before they got it | CN `B11` (`FLY_CN_MU5250V1.0.0B11`); baseband reflashed to `B27` by that third party |
| Other firmware | MU5250 | `B27` (historical), `B28`–`B30`, other regional builds |
| MU5252 | MU5252 | any |

Units A and B report the same firmware version string and the same set of writable settings (read-only comparison, 2026-10-22). Row C was installed and checked by its owner, not on the project's own units. Results on B31 do not apply to B28–B30; they have to be checked on their own.

## Features

| Feature | A (CN B31, upgraded) | B (CN B31, factory, dual SIM) | C (CN B11, third-party modded) | Other firmware | MU5252 |
|---|---|---|---|---|---|
| Install kit (`onboard/`) | Verified (installed on B27; SSH and startup came back after the B27→B31 upgrade, 2026-10-03) | Verified (fresh install, 2026-10-20) | Verified (fresh install 2026-11-17; no key SSH existed, so the `ssh` component was bootstrapped over ADB; cold-boot verified) | B27: verified historically; B28–B30: unknown | Unknown |
| Core service: status, signal, alerts | Verified (daily use since 2026-10) | Verified (2026-10-20, health check clean) | Verified (2026-11-17, doctor clean after cold boot) | B27: verified historically; others unknown | Design only (mock model) |
| Touch UI | Verified | Verified (2026-10-20); dual-SIM switching on screen only tested offscreen | Verified (2026-11-17; heartbeat `ours` after cold boot) | B27: verified historically; others unknown | Unknown |
| Admin web | Verified | Verified (2026-10-20) | Verified (2026-11-17, 3 login checks; reachable after cold boot) | Unknown | Unknown |
| Sentinel (screen owner, watchdog) | Verified | Verified (2026-10-20) | Verified (2026-11-17, screen takeover on cold boot) | Unknown | Unknown |
| eSIM: read chip, list, switch profiles | Verified (2026-10-20) | Verified (switch 2026-10-22, identity path 2026-10-24) | Unknown (lpac installed 2026-11-17; no eUICC card available to test) | Unknown | Unknown |
| eSIM: download, delete | Unknown | Unknown | Unknown | Unknown | Unknown |
| SMS read/send | Verified (2026-10-20) | Verified (long UCS-2 SMS 2026-10-20, receipt resend 2026-10-23) | Unknown | Unknown | Design only (MU5252 encrypts SMS; never tested) |
| APN management | Verified | Unknown (no write test on this unit) | Unknown | Unknown | Unknown |
| Band / cell lock with rollback | Verified on one CN B31 unit (2026-10-19; the record does not say which) | Unknown | Unknown | Unknown | Unknown |
| CHILL proxy (mihomo) | Verified | Verified (2026-10-20) | Unknown (not installed) | Unknown | Unknown |
| Tailscale | Verified | Verified (survives reboot, 2026-10-20) | Unknown | Unknown | Unknown |
| MCP agent (read and act) | Verified (2026-10-17) | Verified (multi-device read, 2026-10-20) | Unknown | Unknown | Unknown |
| USB ECM to a computer | Verified (2026-10-24) | Unknown | Unknown (USB RNDIS to this computer was used throughout the install) | Unknown | Unknown |
| Dual SIM switching | Not applicable (single slot) | Verified (2026-10-20; timeout rollback and smart mode not tested live) | Unknown | Unknown | Unknown |
| Firmware capability report | Verified, read-only (2026-10-19) | Verified, read-only (2026-10-22) | Unknown | Unknown by design | Unknown |
| Wi-Fi settings | Verified | Verified | Unknown | B27: turning on "5 GHz only" switched all Wi-Fi off | Unknown |
| Battery estimate | Verified (calibrated per unit) | Partly (one on-the-road reading, 2026-10-21) | Unknown | Unknown | Unknown |

## Known gaps

- B28–B30 and other regional firmware have never been tested. The install kit does not refuse them, but nothing above applies to them.
- Row C (CN B11) behaves like B27–B31 where it was probed: boot gate, ubus surface, touch hardware (`1-0055`), layout and FOTA uci names all matched. `zwrt_bsp.usb set` still exists there. B11 is `code-only` in the install kit's matrix; this unit was installed with its owner's approval on 2026-11-17; only the features marked Verified above were seen working.
- Claims that B28 and later "removed ADB" are not confirmed: on B31 `adbd` is present; what changed is one USB setting method.
- Recovery from airplane mode (low-power mode) on B31 is unknown and deliberately not tested: the only way out seen so far is a reboot.

If you have a device in one of the unknown rows and want to help, open an issue with your firmware version string (`Settings → About` on the stock web UI) and the output of the install kit's read-only `status` and `doctor` commands. Please remove your IMEI, ICCID and phone number before posting.
