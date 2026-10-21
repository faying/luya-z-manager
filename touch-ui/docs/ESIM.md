# eSIM switching

**English** · [中文](ESIM.zh-CN.md)

Where: 「蜂窝 → SIM 与 eSIM」 (Cellular → SIM & eSIM). The page lists the profiles on a removable eUICC card in the SIM slot (5ber, eSTK.me and the like). Tapping a profile twice switches to it. That is all the screen does: downloading, deleting and renaming profiles belong to the admin web.

## Who does the work

The screen never runs lpac. It talks to the core, and the core (`crates/luyaz-esim`) drives lpac:

```
UI (rs/devui/src/data/esim.rs) ──HTTP 127.0.0.1:9460/v2/api──▶ luyaz-core ──▶ lpac (qmi_qrtr) ──▶ eUICC card
```

| Request (under `/v2`, no token) | Use |
|---|---|
| `GET /api/esim/profiles` | The list on the card |
| `POST /api/esim/switch` | Start a switch |
| `GET /api/esim/job` | Progress of the running switch |

Enabling a profile is one lpac call. Getting the modem to use it is the slow part: ZTE's stack reads the SIM once, when its daemon starts. So after enabling, the core power-cycles the UIM, restarts `zte_topsw_mdm` and waits until the IMSI changes, which normally takes under 10 seconds. The screen only shows the progress and the outcome.

The page needs the eSIM component on the device (lpac under `/data/esim`, installed with the kit's `./install.sh esim`). Without it the page says it is missing.

## Things worth knowing

- **catBusy.** Some cards (eSTK.me, for one) occasionally refuse a switch with `catBusy`, and retrying later does not help; a device restart or pulling and reinserting the card does. The core makes one attempt, reads the card back to confirm nothing changed, and the page marks that profile "Busy: restart device or reinsert card".
- **Success is read from the card.** The core trusts the card's own profile list, not lpac's exit status. After a good switch it also drops older enable/disable notifications for the same profile that the new one supersedes, and sends the new one once data works; anything left over waits for "send notifications" in the admin web.
- **Remote use.** If your connection to the device runs over this U60's own mobile data, do not switch to a profile without data. You would lose the connection and have to switch back on the screen in person.
