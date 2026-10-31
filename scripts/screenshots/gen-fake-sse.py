#!/usr/bin/env python3
"""Synthetic /v2/events capture for luyaz-web mock. All values made up (RFC 5737/3849 IPs, test MCC 001/01)."""
import json, math, random, sys, time

random.seed(7)
OUT = sys.argv[1] if len(sys.argv) > 1 else "fake-events.sse"
T0 = int(time.time()) - 600
EPOCH = "fa4e000000000001"
IMEI, ICCID, IMSI, MSISDN = "350000000000001", "8900000000000000000", "001010000000001", "+15550100"

def ca(idx0, lst):
    return "".join(f"{idx0+i},{p},1,{b},{a},{bw},0,{r:.1f},{q:.1f},{s:.1f},{r+27:.1f};" for i, (p, b, a, bw, r, q, s) in enumerate(lst))

def signal(t):
    w = math.sin(t / 40.0)
    lr, nr = -88 + 3 * w + random.uniform(-1, 1), -84 + 4 * w + random.uniform(-1, 1)
    ls, ns = 14 + 2 * w, 19 + 3 * w
    return {"HSR": False, "band": "B3", "bars": 4, "home_mcc": 1, "lte_bands": "1,3,7,20,28", "lte_cell_id": 1234567,
        "lte_channel": 1650, "lte_pci": 101, "lte_rsrp": int(lr), "lte_rsrq": -10, "lte_rssi": int(lr) + 27,
        "lte_snr": f"{ls:.1f}", "lte_supported_bands": "1,3,5,7,8,20,28,38,40,41",
        "lteca": ca(1, [(202, 7, 3100, 20, lr - 4, -11, ls - 3)]), "ltecasig": f"{lr-4:.1f},-11.0,{ls-3:.1f},{lr+23:.1f};",
        "mcc": 1, "mnc": 1, "net_select": "WL_AND_5G", "net_select_mode": "auto_select", "nr_band": "78", "nr_bw": "100",
        "nr_cell_id": 9876543, "nr_channel": 636666, "nr_nsa_supported_bands": "1,3,28,41,78",
        "nr_pci": 303, "nr_rsrp": int(nr), "nr_rsrq": -11, "nr_rssi": int(nr) + 27, "nr_sa_supported_bands": "1,3,28,41,78",
        "nr_snr": f"{ns:.1f}", "nrca": "", "nsa_bands": "1,3,28,41,78", "operator": "Example Mobile", "roaming": "Home",
        "rssi": int(lr) + 27, "sa_bands": "1,3,28,41,78", "serving_mcc": 1, "true_roaming": False, "type": "ENDC",
        "wan_status": "ipv4_ipv6_connected"}

def live(t):
    rx = int(abs(math.sin(t / 25.0)) * 38_000_000 + random.uniform(0, 3e6))
    tx = int(rx / 9)
    total_rx = 18_400_000_000 + t * 2_000_000
    return {"runtime": {"connections": {"tcp4": 42, "tcp6": 6, "tcp_active": 31, "tcp_other": 17, "udp4": 14, "udp6": 3, "unix": 88},
        "cpu_cores": {f"cpu{i}": 80 + 30 * i + random.randint(0, 60) for i in range(4)},
        "cpu_freq_mhz": {f"cpu{i}": {"cur": 1209, "max": 2208} for i in range(4)},
        "cpu_usage_tenths": 140 + random.randint(0, 60),
        "link_rates": [{"interface": "rmnet_data0", "rx_bps": rx, "rx_bytes": total_rx, "source": "netdev", "tx_bps": tx, "tx_bytes": total_rx // 9, "window_ms": 2000}],
        "memory_kb": {"available": 1_820_000, "buffers": 41_000, "cached": 910_000, "free": 760_000, "swap_free": 0, "swap_total": 0, "total": 3_906_000},
        "storage": {"available": 9_100_000, "total": 12_000_000, "used": 2_900_000},
        "thermal_zones": [{"temp_milli": 41000 + random.randint(0, 2000), "type": n} for n in ("cpuss-0", "cpuss-1", "battery", "modem-0")],
        "throughput": {"rx_bps": rx, "tx_bps": tx, "window_ms": 2000}},
        "system": {"cpu_temp": 42, "cpu_usage": 16, "fw": "OpenWrt", "hostname": "U60", "imei": IMEI, "mem_avail": 1_820_000,
            "mem_total": 3_906_000, "mem_used_pct": 53, "model": "MU5250", "sw_version": "EXAMPLE_FW_B01", "uptime": 86_400 * 3 + t},
        "traffic": {"clear_day": {"cid": 1, "clearday": 1, "enable": 1}, "day_rx_bytes": 3_200_000_000 + t * 2_000_000,
            "day_tx_bytes": 410_000_000 + t * 200_000, "limit": {"cid": 1, "enable": 1, "overflow": 0, "ratio": 90, "type": 2, "value": "107374182400"},
            "max_rx_speed": 61_000_000, "max_tx_speed": 9_800_000, "month_rx_bytes": 41_000_000_000 + t * 2_000_000,
            "month_tx_bytes": 5_300_000_000, "rx_bytes": total_rx, "rx_speed": rx, "session_time": 7200 + t,
            "total_rx_bytes": 380_000_000_000, "total_tx_bytes": 46_000_000_000, "tx_bytes": total_rx // 9, "tx_speed": tx}}

def battery(t):
    return {"bat_ua": -620000, "bat_uv": 3_980_000, "charger_connect": 0, "charger_type": 0, "charging": 0, "chg_ua": 0, "chg_uv": 0,
        "health": 1, "online": 1, "percent": 78 - t // 300, "temp": 31000, "time_to_full": -1}

static = {
    "advice": {"backfilled": False, "bands": {}, "net": "001-01", "pref": "auto",
        "prefs": {"auto": {"crashes": 0, "drops": 1, "secs": 640_000}, "Only_LTE": {"crashes": 0, "drops": 0, "secs": 3600}},
        "text": "自动模式运行良好", "text_en": "Auto mode has been stable: 1 drop in 177.8 h", "unclear": 0, "verdict": "ok"},
    "charger": {"direct_supply": {"enabled": False, "mode": "disable", "supported": True}},
    "clients": {"lan": 1, "total": 4, "wifi": 3, "list": [
        {"ip": "192.168.0.101", "mac": "02:00:00:00:00:01", "name": "MacBook-Air"},
        {"ip": "192.168.0.102", "mac": "02:00:00:00:00:02", "name": "Pixel-9"},
        {"ip": "192.168.0.103", "mac": "02:00:00:00:00:03", "name": "iPad"},
        {"ip": "192.168.0.104", "mac": "02:00:00:00:00:04", "name": "Desktop-PC"}]},
    "dhcp": {"ip": "192.168.0.1", "leasetime": "12h", "limit": "150", "start": "100"},
    "interfaces": {
        "cellular": {"cid": 1, "connect_fail_count": 0, "connect_mode": 1, "connect_status": "ipv4_ipv6_connected", "enable": 1,
            "ipv4_address": "198.51.100.23", "ipv4_dev_name": "rmnet_data0", "ipv4_dns_prefer": "192.0.2.53", "ipv4_dns_standby": "192.0.2.54",
            "ipv4_gateway": "198.51.100.22", "ipv4_netmask": "255.255.255.252", "ipv6_address": "2001:db8:4f2a:1c07::1", "ipv6_dev_name": "rmnet_data0",
            "ipv6_dns_prefer": "2001:db8:100::53", "ipv6_dns_standby": "2001:db8:100::54", "ipv6_gateway": "fe80::1", "roam_enable": 0,
            "roll_connect_status": "success"},
        "lan": {"device": "br-lan", "dns": [], "ipv4": [{"address": "192.168.0.1", "mask": 24}], "ipv6": [], "proto": "static", "up": True},
        "lan_config": {"lan_info": [{"bridge_name": "br-lan", "device": "br-lan", "ipv6_default_gateway": "", "ipv6_dhcp_end": "", "ipv6_dhcp_start": "",
            "ipv6_dns": "", "ipv6_global_addr": "", "ipv6_link_local_addr": "fe80::200:ff:fe00:1", "ipv6_ra_enable": 0, "ipv6_unique_local_addr": "",
            "mac": "02:00:00:00:00:a0", "rx_bytes": 9_800_000_000, "rx_dropped": 0, "rx_error": 0, "rx_packets": 8_100_000,
            "tx_bytes": 71_000_000_000, "tx_dropped": 0, "tx_error": 0, "tx_packets": 52_000_000}]},
        "wan4": {"device": "rmnet_data0", "dns": ["192.0.2.53", "192.0.2.54"], "ipv4": [{"address": "198.51.100.23", "mask": 30}], "ipv6": [], "proto": "static", "up": True},
        "wan6": {"device": "rmnet_data0", "dns": ["2001:db8:100::53"], "ipv4": [], "ipv6": [{"address": "2001:db8:4f2a:1c07::1", "class": "global", "mask": 64}], "proto": "static", "up": True}},
    "nfc": {"switch": 0},
    "op": {"active": None, "last": None, "rollback_enabled": True},
    "powerbank": {"state": 0},
    "qos": {"ambr_dl": "1000.000", "ambr_ul": "200.000", "bearer": "nr_pdu", "qci": 9, "stale": False, "usb_mode": "user"},
    "sim": {"current_slot": 1, "dual_sim": 1, "iccid": ICCID, "imsi": IMSI, "modem_state": "modem_init_complete", "msisdn": MSISDN,
        "pin_status": "0", "sim1_provision": 0, "sim2_provision": 0, "spn": "Example Mobile", "state": "ready"},
    "typec": {"cc_attch_state": 0, "data_role": "device", "power_role": "sink"},
    "wlan": {"enabled": 1, "enc": "psk3+ccmp", "ssid": "Example-5G"},
}
u = {k: "" for k in "common_model_name day_time device_alias_name month_time total_time timezone mss mtu login_fail_lock_timeout login_fail_num login_timeout".split()}
u.update({"common_model_name": "MU5250", "device_manufacturer": "ZTE", "device_market_name": "U60 Pro", "device_model": "MU5250", "hostname": "U60",
    "hardware_version": "HW1.0", "hardware_version_ci": "HW1.0", "iccid": ICCID, "imei": IMEI, "imsi": IMSI, "msisdn": MSISDN,
    "integrate_version": "EXAMPLE_FW_B01", "software_version": "EXAMPLE_FW_B01", "wa_inner_version": "EXAMPLE_INNER_01",
    "serial_number": "SN0000000001", "mac_address": "02:00:00:00:00:a0", "lan_ipaddr": "192.168.0.1", "lan_netmask": "255.255.255.0",
    "dhcpEnabled": "1", "dhcpStart": "192.168.0.100", "dhcpEnd": "192.168.0.249", "dhcpLease_hour": "12h", "mcc": "001", "mnc": "01",
    "modem_main_state": "modem_init_complete", "modem_msn": "MSN0000000001", "pin_status": "0", "sim_states": "ready",
    "radio_operator": "Example Mobile", "radio_network_type": "ENDC", "radio_net_select": "WL_AND_5G", "radio_signalbar": "4",
    "radio_lte_band": "3", "radio_lte_bands": "1,3,7,20,28", "radio_lte_cell_id": "1234567", "radio_lte_channel": "1650", "radio_lte_pci": "101",
    "radio_lte_rsrp": "-88", "radio_lte_rsrq": "-10", "radio_lte_snr": "14.0", "radio_nr_band": "78", "radio_nr_bandwidth": "100",
    "radio_nr_channel": "636666", "radio_nr_pci": "303", "radio_nr_rsrp": "-84", "radio_nr_rsrq": "-11", "radio_nr_snr": "19.0",
    "radio_nr_nsa_bands": "1,3,28,41,78", "radio_nr_sa_bands": "1,3,28,41,78", "wan_dns": "192.0.2.53", "web_language": "en",
    "day_rx_bytes": "3200000000", "day_tx_bytes": "410000000", "month_rx_bytes": "41000000000", "month_tx_bytes": "5300000000",
    "total_rx_bytes": "380000000000", "total_tx_bytes": "46000000000", "timezone": "UTC0", "mtu": "1500", "mss": "1460"})
static["uci_device_info"] = u

seq = 100
def blk(name, data, at):
    global seq; seq += 1
    return {"epoch": EPOCH, "seq": seq, "name": name, "revision": seq, "observed_at": at, "stale": False, "data": data}

blocks = dict(static, signal=signal(0), live=live(0), battery=battery(0))
snap = {"epoch": EPOCH, "seq": seq, "blocks": {k: {"revision": 1, "observed_at": T0, "stale": False, "data": v} for k, v in blocks.items()}}
out = [f"event: snapshot\ndata: {json.dumps(snap)}\n\n"]
for t in range(2, 602, 2):
    at = T0 + t
    for n, fn in (("signal", signal), ("live", live)):
        out.append(f"event: block\ndata: {json.dumps(blk(n, fn(t), at))}\n\n")
    if t % 30 == 0:
        out.append(f"event: block\ndata: {json.dumps(blk('battery', battery(t), at))}\n\n")
    if t % 10 == 0:
        out.append(f"event: heartbeat\ndata: {json.dumps({'epoch': EPOCH, 'seq': seq, 'blocks': {k: at for k in blocks}, 'exec_age_ms': 0})}\n\n")
open(OUT, "w").write("".join(out))
print(OUT, len(out), "frames")
