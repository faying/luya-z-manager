# Feature tour

**English** · [中文](SCREENSHOTS.zh-CN.md)

The screenshots show the English UI with made-up data (example operator, documentation IP ranges); both UIs also come in Chinese and in light and dark themes.

## Admin web (`luyaz-web`, port 9090)

### Overview

The home page answers "is the connection OK?" first: a one-line verdict (signal, network type, operator, carriers, bars), then aggregate bandwidth, RSRP/SINR, live download and upload speed, temperature and battery with time left. Below are connected devices, today's and this month's traffic, uptime, public IP, SIM and registered operator, roaming, both Wi-Fi bands and the active carriers. On a desktop it uses a sidebar; on a phone it switches to bottom tabs (Home / Charts / Functions / System).

<img src="images/web-home-desktop.png" width="720" alt="Home (desktop)">

| | |
|---|---|
| <img src="images/web-home-phone.png" width="260" alt="Home (phone)"> | <img src="images/web-home-phone-dark.png" width="260" alt="Home (phone, dark)"> |
| Phone, light | Phone, dark |

### Signal and trends

The Signal page shows RSRP, SINR, RSRQ and bars with a plain-word rating each, a trend chart over the last couple of minutes, and the serving cell (cell ID, PCI, ARFCN, band, bandwidth). The data roaming switch and links to band lock and signal detect sit beside it. "Last 5 Minutes" charts mobile speed, CPU, memory and battery; points are only collected while one of these pages is open, so nothing runs in the background.

<img src="images/web-signal.png" width="720" alt="Signal">

<img src="images/web-trends.png" width="720" alt="Last 5 minutes">

### Clients

Every device on the router with its IP, MAC and how long it has been connected. Wi-Fi clients also show band, Wi-Fi generation, link rate, signal quality, current speed and data used since joining, so you can see who is using the bandwidth. Expired DHCP leases are hidden behind "Show expired".

<img src="images/web-clients.png" width="720" alt="Clients">

### Band lock

Shows the bands in use now and the current locks, then lets you pick LTE bands and NR (5G) bands; one NR choice applies to both SA and NSA. Applying asks for confirmation in a dialog, and the device then checks within about two minutes that it reads the lock back, is registered and has data, reporting the result on the page. "Clear all locks" under Automatic bands drops every band and cell lock and hands the choice back to the modem.

<img src="images/web-bandlock.png" width="720" alt="Band lock">

### SIM / eSIM

The SIM page shows SIM state, IMSI, ICCID, PIN lock and remaining PIN/PUK attempts, with actions to enable the PIN lock, change the PIN or network-unlock. The U60 has no built-in eSIM, but with a removable eUICC card (eSTK.me, 5ber and similar) in the SIM slot the eSIM page lists its profiles, lets you rename, switch or delete them, and downloads new ones from an activation code. A switch usually takes about 40 s; if the new profile doesn't attach, the device reboots. Pending carrier notifications and card details (EID, free space) are shown too.

<img src="images/web-sim.png" width="720" alt="SIM / PIN">

<img src="images/web-esim.png" width="720" alt="eSIM profiles">

### SMS

An inbox for messages stored on the SIM and on the device, with storage counts, unread markers, filters (All / SIM / Device), bulk select and a Compose button. Useful for carrier messages such as roaming pack activations, without moving the SIM to a phone.

<img src="images/web-sms.png" width="720" alt="SMS">

### Wi-Fi

A summary of both networks (channel, width, connected devices), then the settings of each band: SSID, security, channel, bandwidth, transmit power, hidden or not. Guest Wi-Fi has its own page. Wi-Fi power save and NFC tap-to-join are switches at the bottom, each with a line on the trade-off.

<img src="images/web-wifi.png" width="720" alt="Wi-Fi">

### USB mode

Shows the USB-C port (cable, CC, mode, USB-to-Ethernet adapter) and the charger state. You can switch the USB mode (DEBUG / MTP / RNDIS), which reconnects the USB interface, and turn on fast charging for a phone plugged into the USB-C port.

<img src="images/web-usb.png" width="720" alt="USB mode">

### Health and alerts

A read-only check of the device, rerun every minute: boot sync, ZTE auto-update off, services supervised and reachable, clock, Wi-Fi, screen UI, unread alerts, alert SMS set up, crash logs, `/data` space and standby traffic. The same check runs over SSH as `./install.sh doctor`, so it still works when the admin web is down. Recent crash logs can be opened at the bottom.

<img src="images/web-health.png" width="720" alt="Health">

### Change log

Every setting changed through the device, newest first: what changed, when, from where (web, screen or a scenario) and whether it took. Open a row for details and, where possible, undo it. Only the latest 50 are kept.

<img src="images/web-changes.png" width="720" alt="Change log">

### Device info

Identity (IMEI, IMSI, ICCID, phone number, operator, network type), WAN and LAN addresses, DNS, and system facts (hostname, kernel, uptime, memory) on one page, for when you need a number for a support ticket or a carrier.

<img src="images/web-device-info.png" width="720" alt="Device info">

### Sign-in

The admin web is reachable only from the LAN and asks for one admin password.

<img src="images/web-login-phone.png" width="260" alt="Sign-in (phone)">

## Touch UI (front panel)

### Home

The front screen opens with a verdict ("All good"), signal strength, noise and load, then carriers, Wi-Fi with device count, and today's and this month's traffic; the status bar shows live speeds and battery. A one-time notice explains that a network-mode change which can't connect is reverted automatically, and a banner warns when the battery is low.

| | | | |
|---|---|---|---|
| <img src="images/touch-home.png" width="180" alt="Home"> | <img src="images/touch-home-dark.png" width="180" alt="Home (dark)"> | <img src="images/touch-notice.png" width="180" alt="Home with a notice"> | <img src="images/touch-lowbat.png" width="180" alt="Low battery"> |
| Home | Dark | Auto-revert notice | Low battery |

### Cellular and Wi-Fi

| | |
|---|---|
| <img src="images/touch-cellular.png" width="220" alt="Cellular"> | <img src="images/touch-wifi.png" width="220" alt="Wi-Fi"> |
| **Cellular:** APN, mobile data and data roaming switches, and network mode (Auto, 5G NSA, 5G SA, 4G+3G, 4G). Changes that can cut data or cost roaming fees need a second tap to confirm. | **Wi-Fi:** the hotspot name, security and password at a glance (handy for guests), switches for Wi-Fi, each band, power saving and NFC tap, then connected devices. |

### Band lock and health

| | |
|---|---|
| <img src="images/touch-bandlock.png" width="220" alt="Band lock"> | <img src="images/touch-alerts.png" width="220" alt="Health and alerts"> |
| **Band lock:** pick 5G SA, 5G NSA and LTE bands right on the device, then apply. | **Health and alerts:** health-check items that need a look and the alert log (e.g. the Wi-Fi watchdog restoring Wi-Fi); tap a row for the full text. |

### System

| | |
|---|---|
| <img src="images/touch-system.png" width="220" alt="System"> | <img src="images/touch-system-charts.png" width="220" alt="System: charts"> |
| Language (also used for alert SMS), brightness, screen-off timer and light/dark/auto theme, plus battery details with a time-left estimate, charger and CPU. | Memory and uptime, 5-minute charts for CPU, RAM and battery, and the firmware version. |

To regenerate: [scripts/screenshots/](../scripts/screenshots/README.md) (`web.sh`, `touch.sh`).
