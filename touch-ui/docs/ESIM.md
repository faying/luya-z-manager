# eSIM switching page

**English** · [中文](ESIM.zh-CN.md)

On the touch screen, 「蜂窝 → SIM 与 eSIM」 (Cellular → SIM & eSIM) lists the profiles on the removable eUICC card in the SIM slot (5ber, eSTK.me and similar); tap twice to switch.
**It only switches**; downloading, deleting and renaming are done in the admin web.

## Data flow: call the core, don't run lpac directly

```
lpac (qmi_qrtr) ◀── luyaz-core (crates/luyaz-esim) ──HTTP 127.0.0.1:9460/v2/api──▶ rs/devui/src/data/esim.rs ──▶ UI
```

Endpoints used (under `/v2` on the core's loopback port): `GET /api/esim/profiles`, `POST /api/esim/switch`, `GET /api/esim/job` (switch progress).

The switch itself is a single `lpac profile enable`; the hard part is getting ZTE's stack to recognize the new card: it reads the SIM only once when its daemon starts,
so the core power-cycles the UIM, restarts `zte_topsw_mdm` and waits for the IMSI to change, usually within 10 seconds. All of this happens in the core; the UI only shows progress and the result.

Prerequisite: the eSIM component is installed on the device (lpac in `/data/esim`, via the install kit's `./install.sh esim`); if it's missing, the page says so.

## Login

None: the core's loopback listener needs no token, so the UI does not log in.

## Notes

- Some cards (e.g. eSTK.me) sometimes answer a switch with `catBusy`. Waiting and retrying doesn't clear it; restarting the device (or taking the card out and putting it back) does. The core tries once, reads the card to confirm the profile didn't change, and the screen marks that profile "Busy: restart device or reinsert card".
- Whether a switch worked is judged by the card's own profile list, not by lpac's exit. After a successful switch the core removes older enable/disable notifications that a newer one for the same profile has superseded, and sends the switch's own once data is back (otherwise they stay pending for the web page's "send notifications").
- If you are operating remotely over this U60's own network, don't switch to a profile with no data; once switched, you can't connect back and have to switch back on the screen in front of the device.
