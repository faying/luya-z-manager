# OpenU60 — Mobile Apps for ZTE U60 Pro (MU5250)

Native companion apps for the ZTE U60 Pro 5G mobile router. Connect to the `zte-agent` REST API running on the router over WiFi -- no ADB required.

## Features

| Feature | Details |
|---|---|
| Signal Monitoring | Live NR 5G / LTE / WCDMA metrics with color-coded thresholds |
| RSRP History Chart | Scrollable chart tracking signal strength over time |
| Battery & Thermal | Battery %, temperature, CPU thermal, charge policy |
| Traffic Stats | Real-time DL/UL speed (Mbps), total bytes transferred |
| Connected Devices | MAC, hostname, IPv4/IPv6 via host hints + DHCP enrichment |
| Device Info | SIM (ICCID, IMSI, MSISDN), IMEI, WAN/LAN IPs |
| Band Lock/Unlock | Lock NR5G NSA/SA and LTE bands, unlock all |
| Cell Lock | Lock to specific NR/LTE cells by PCI/EARFCN |
| STC Cell Lock | Smart traffic control cell locking |
| Signal Detection | Signal quality measurement with progress tracking |
| WiFi Settings | SSID, password, channel, bandwidth, tx power, WiFi 6 |
| Guest WiFi | Guest network enable/disable, SSID, timer |
| SMS | Read, send, delete SMS; mark as read |
| SIM Management | PIN verify/change/enable/disable, PUK unlock, SIM unlock |
| Mobile Network | Data toggle, airplane mode, network scan, manual register |
| Network Mode | Auto/manual network selection |
| APN Settings | Auto/manual APN mode, add/edit/delete/activate profiles |
| DNS Settings | Custom DNS configuration |
| LAN Settings | LAN IP/DHCP configuration |
| Firewall | Switch, level, NAT, DMZ, UPnP, port forwarding, filter rules |
| VPN Passthrough | VPN passthrough toggle |
| QoS | Quality of service toggle |
| Telemetry Blocker | Domain filter rules for blocking telemetry |
| Schedule Reboot | Automatic reboot scheduling |
| Device Control | Reboot, factory reset, power supply mode, power save, fast boot |
| USB Mode | USB mode switching, powerbank mode |
| Voice Calls | Dial, answer, hangup, DTMF, mute via AT commands |
| USSD | Send/respond/cancel USSD sessions |
| STK Menu | SIM Toolkit menu browsing |
| Enable ADB | One-tap USB debug mode via WiFi |
| Config Decrypt/Encrypt | Import .bin, auto-detect key, browse XML, re-encrypt, export |

## Architecture

- **Pattern**: MVVM
- **Transport**: HTTP to `zte-agent` REST API (`http://<router>:9090/api/...`)
- **Auth**: Token-based (password → bearer token)

The app talks to `zte-agent`, a lightweight Rust HTTP server on the router that exposes typed REST endpoints, internally calling the router's ubus subsystem and returning JSON. The mobile app is a pure front-end with no direct ubus/AT/sysfs access.

## iOS App

**Path**: `ios/OpenU60/`

### Requirements

- iOS 16.0+
- Xcode 15+
- No external dependencies (uses only Apple frameworks)

### Tech Stack

| Component | Implementation |
|---|---|
| UI | SwiftUI |
| HTTP | URLSession |
| Charts | Swift Charts |
| Secure storage | Keychain Services |
| Config crypto | CommonCrypto (AES-128-ECB, AES-256-CBC) |
| Compression | Compression framework (zlib) |
| Key derivation | CryptoKit (Insecure.MD5) |

### Project Structure

```
ios/OpenU60/
├── OpenU60App.swift                  App entry point
├── Core/
│   ├── Networking/
│   │   ├── AgentClient.swift              REST client (getJSON/postJSON/putJSON)
│   │   ├── AgentError.swift               Error types
│   │   └── AuthManager.swift              Token auth, Keychain helper
│   ├── Components/
│   │   └── WiFiQRGenerator.swift          WiFi QR code generation
│   ├── Crypto/
│   │   ├── ***REMOVED***.swift          AES-ECB/CBC, header parsing, key derivation
│   │   └── ZTECompression.swift           ZLIB plain/chunked/raw decompress + compress
│   ├── Models/
│   │   ├── SignalModels.swift             NRSignal, LTESignal, WCDMASignal, OperatorInfo
│   │   ├── DeviceModels.swift             Battery, Thermal, Traffic, ConnectedDevice
│   │   ├── RouterSettingsModels.swift     Firewall, DNS, LAN, APN, etc.
│   │   ├── BandModels.swift               BandConfig
│   │   └── ConfigModels.swift             ConfigHeader, PayloadType, known keys table
│   └── Extensions/
│       └── ColorExtensions.swift          RSRP/SINR color thresholds
├── Features/
│   ├── Dashboard/                         Summary cards (signal, battery, speed, WiFi, devices)
│   ├── Signal/                            Live NR/LTE/WCDMA panels + RSRP chart
│   ├── BandLock/                          NR/LTE band selection grid, lock/unlock
│   ├── SMS/                               Conversations, send/delete, mark read
│   ├── Call/                              Voice calls, DTMF, mute
│   ├── DeviceInfo/                        SIM, IMEI, WAN/LAN IPs
│   ├── Clients/                           Connected devices list
│   ├── Config/                            Import, decrypt, XML browser, re-encrypt, export
│   ├── USBMode/                           USB mode switching, powerbank
│   ├── RouterSettings/                    All router configuration screens
│   │   ├── APN/                           APN profiles management
│   │   ├── CellLock/                      Cell lock by PCI/EARFCN
│   │   ├── DNS/                           DNS settings
│   │   ├── Device/                        Reboot, reset, power modes
│   │   ├── Firewall/                      Firewall, NAT, DMZ, port forward
│   │   ├── LAN/                           LAN settings
│   │   ├── MobileNetwork/                 Data, airplane, network scan
│   │   ├── NetworkMode/                   Network selection mode
│   │   ├── QoS/                           Quality of service
│   │   ├── SIM/                           SIM/PIN management, STK
│   │   ├── STC/                           Smart traffic control
│   │   ├── Schedule/                      Scheduled reboot
│   │   ├── SignalDetect/                  Signal quality detection
│   │   ├── Telemetry/                     Telemetry domain filter
│   │   ├── VPN/                           VPN passthrough
│   │   └── WiFi/                          WiFi + guest WiFi settings
│   ├── Tools/                             Tools list, Enable ADB
│   └── Settings/                          Agent URL, password, poll interval, theme
└── Navigation/
    └── TabBarView.swift                   Dashboard | Signal | SMS | Tools | Settings
```

### Setup

1. Build `zte-agent`: `cargo build --release --target aarch64-unknown-linux-musl -p zte-agent`
2. Deploy to router via ADB or SCP (see root README for details)
3. Open the project in Xcode
4. Add `NSAppTransportSecurity` → `NSAllowsArbitraryLoads = YES` to `Info.plist`
5. Build and run on device or simulator
6. Connect to the router's WiFi and set the agent URL (default: `http://192.168.0.1:9090`)

## Agent REST API

The iOS app communicates exclusively through typed REST endpoints on `zte-agent`. Key endpoint groups:

| Group | Endpoints | Description |
|---|---|---|
| `/api/network/*` | signal, wan, wan6, lan-status, clients, speeds, rmnet, traffic | Network status |
| `/api/device/*` | thermal, charger, system, battery-info, reboot, factory-reset, power-supply, power-save | Device info & control |
| `/api/wifi/*` | status, settings, guest | WiFi configuration |
| `/api/modem/*` | status, online, data, airplane, network-mode, scan/*, register/*, schedule-reboot | Modem control |
| `/api/sms/*` | list, capacity, send, delete, read | SMS management |
| `/api/sim/*` | info, imei, pin/*, unlock, lock-trials | SIM management |
| `/api/cell/*` | lock/*, neighbors/*, band/*, stc/*, signal-detect/* | Cell & band locking |
| `/api/router/*` | dns, lan, firewall/*, vpn, qos, domain-filter, apn/* | Router configuration |
| `/api/usb/*` | status, mode, powerbank | USB management |
| `/api/call/*` | dial, hangup, answer, status, dtmf, mute | Voice calls (AT) |
| `/api/ussd/*` | send, respond, cancel | USSD sessions (AT) |
| `/api/stk/*` | menu, select | SIM Toolkit (AT) |
| `/api/battery` | — | Battery (sysfs) |
| `/api/cpu` | — | CPU usage |
| `/api/memory` | — | Memory info |

### Authentication

1. Set `ZTE_AGENT_PASSWORD` env var when starting the agent
2. `POST /api/auth/login` with `{"password": "..."}` → receive bearer token
3. Include `Authorization: Bearer <token>` on all subsequent requests

## Signal Color Thresholds

| Metric | Green | Yellow | Orange | Red |
|---|---|---|---|---|
| RSRP (dBm) | >= -80 | >= -100 | >= -110 | < -110 |
| SINR (dB) | >= 20 | >= 10 | >= 0 | < 0 |

## Config Decrypt/Encrypt

The app can decrypt and re-encrypt ZTE router configuration backup files (`.bin`):

- **Header**: 128 bytes starting with `ZXHN` magic
  - Payload type at offset 4: ECB (0), CBC (1), Plain (2), CBC New (3)
  - Signature at offset 8 (max 64 bytes, null-terminated)
  - Payload offset at offset 72 (4-byte big-endian)
- **Encryption**: AES-128-ECB (16-byte key) or AES-256-CBC (32-byte key, first 16 bytes of payload = IV)
- **Compression**: ZLIB — plain, chunked (4-byte BE length prefix per chunk), or raw deflate
- **Key resolution**: Tries 14 known static keys + MD5(serial)[:16] + MD5(signature)[:16]

## License

Same as parent project.
