#!/usr/bin/env python3
"""A fake zwrt-datad for rs/smoke.sh: the device-mode binary talks to it on 127.0.0.1:9460.

  GET  /v2/state   one snapshot (signal, battery 77 %, live, sim, sms, wlan, clients …)
  GET  /v2/events  that snapshot, then heartbeats every 0.5 s, and at 1.5 s a battery block
                   event (66 %): the screen must follow seq/epoch and show 66
  GET  /v2/screen  the home card's view (headline 顺畅, operator 测试运营商)
  POST /control    the whole body read (Content-Length), checked to be JSON from the screen,
                   answered only after 1 s (the screen must not hang up first)

Every request is logged to the file given as the first argument, one JSON line each.
SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
"""
import json
import urllib.error
import urllib.request
import socket
import sys
import threading
import time

LOG = open(sys.argv[1], "a", buffering=1)
LOCK = threading.Lock()


def log(**kw):
    with LOCK:
        LOG.write(json.dumps(kw, ensure_ascii=False) + "\n")


def blk(data, stale=False):
    return {"revision": 1, "observed_at": 1, "stale": stale, "data": data}


BLOCKS = {
    "signal": blk({"type": "NR5G_SA", "operator": "CMCC", "mcc": 460, "mnc": 0, "bars": 4, "nr_band": "78",
                   "nr_rsrp": -88, "nr_snr": "17.5", "nr_pci": 603, "nr_channel": 627264, "net_select": "WL_AND_5G",
                   "wan_status": "ipv4_ipv6_connected", "operate_mode": "ONLINE", "roaming": "Home"}),
    "battery": blk({"percent": 77, "temp": 31, "charging": 0, "charger_connect": 0, "bat_uv": 4012000}),
    "live": blk({"system": {"uptime": 1234, "cpu_temp": 45, "cpu_usage": 7, "mem_used_pct": 40,
                            "model": "MU5250", "imei": "860000000000000"},
                 "runtime": {"x": 1}, "traffic": {"rx_speed": 1532000, "tx_speed": 188000}}),
    "sim": blk({"state": "sim ready", "iccid": "89860000000000000001", "imsi": "460001234567890", "spn": ""}),
    "sms": blk({"unread": 1, "max_id": 9, "count": 1}),
    "sms_list": blk({"list": [{"id": 9, "num": "10086", "date": "10-11 09:00", "unread": 1, "text": "测试短信"}]}),
    "wlan": blk({"ssid": "U60-SMOKE", "key": "k", "enc": "psk2", "enabled": 1}),
    "clients": blk({"total": 1, "wifi": 1, "lan": 0, "list": [{"name": "mac", "ip": "192.168.0.2", "mac": "aa"}]}),
    "interfaces": blk({"cellular": {"enable": 1, "roam_enable": 0, "connect_status": "ipv4_connected"}}),
    "nfc": blk(None, stale=True),
}
EPOCH = "smoke-1"
SCREEN = {
    "v": 1, "ts": 1, "exec_age_ms": 0,
    "op": {"rollback_enabled": True, "active": None, "last": None},
    "net": {
        "carriers": [{"kind": "nr", "band": 78, "pci": 603, "bw": 100, "active": True, "arfcn": 627264,
                      "rsrp": "-88", "rsrq": "-11", "sinr": "17.5", "label": "n78", "label_short": "n78",
                      "sinr_tone": "ok"}],
        "act_n": 1, "roam_known": True, "roam": False, "nosvc": False, "sim_usable": True, "other": False,
        "logo": "china-mobile", "fine": "5G SA", "name": "测试运营商", "where": "本地",
        "ca_val": "单载波", "ca_sub": "↓ n78   ↑ n78", "bars_tier": 2, "mode_word": "自动", "mode_auto": True,
        "story": {"tone": "ok", "cause": "none", "state": "ok", "sig_tone": "ok", "noise_tone": "ok",
                  "headline": "顺畅", "hint": "", "rat": "5G", "link": "单载波 · 带宽充足", "sig": "强",
                  "noise": "小", "load": "正常", "limit": "无"},
    },
}


def snapshot(seq):
    return {"epoch": EPOCH, "seq": seq, "blocks": BLOCKS}


def read_request(c):
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = c.recv(65536)
        if not chunk:
            return None, None, None
        data += chunk
    head, body = data.split(b"\r\n\r\n", 1)
    lines = head.decode("latin1").split("\r\n")
    want = 0
    for l in lines[1:]:
        k, _, v = l.partition(":")
        if k.strip().lower() == "content-length":
            want = int(v.strip())
    while len(body) < want:
        chunk = c.recv(65536)
        if not chunk:
            break
        body += chunk
    method, path = lines[0].split(" ")[:2]
    return method, path, body


def reply(c, status, body, ctype="application/json"):
    b = body.encode()
    c.sendall(f"HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {len(b)}\r\nConnection: close\r\n\r\n".encode() + b)


def events(c):
    c.sendall(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\n")
    seq = 40
    c.sendall(f"event: snapshot\ndata: {json.dumps(snapshot(seq), ensure_ascii=False)}\n\n".encode())
    t0 = time.time()
    sent_battery = False
    while True:
        time.sleep(0.5)
        seq += 1
        if not sent_battery and time.time() - t0 >= 1.5:
            ev = {"epoch": EPOCH, "seq": seq, "name": "battery", "revision": 2, "observed_at": 2, "stale": False,
                  "data": {"percent": 66, "temp": 31, "charging": 0, "charger_connect": 0}}
            c.sendall(f"event: block\ndata: {json.dumps(ev)}\n\n".encode())
            sent_battery = True
            log(sse="battery 66", seq=seq)
        else:
            c.sendall(f"event: heartbeat\ndata: {json.dumps({'epoch': EPOCH, 'seq': seq, 'blocks': {}})}\n\n".encode())


def handle(c):
    try:
        method, path, body = read_request(c)
        if method is None:
            return
        if path == "/v2/events":
            log(req="GET /v2/events")
            events(c)
            return
        if path == "/v2/state":
            log(req="GET /v2/state")
            reply(c, 200, json.dumps(snapshot(0), ensure_ascii=False))
        elif path == "/v2/screen":
            log(req="GET /v2/screen")
            reply(c, 200, json.dumps(SCREEN, ensure_ascii=False))
        elif path.startswith("/v2/api/"):
            # u60d's screen API (M6b): the agent-shaped routes, served by fake_agent.py on :9090
            log(req=f"{method} {path}")
            try:
                r = urllib.request.urlopen(urllib.request.Request(
                    "http://127.0.0.1:9090" + path[3:], data=body if method != "GET" else None,
                    method=method), timeout=2)
                code, text = r.status, r.read().decode()
            except urllib.error.HTTPError as e:
                code, text = e.code, e.read().decode()
            except OSError:
                code, text = 502, "{}"
            reply(c, code, text)
        elif path == "/control" and method == "POST":
            try:
                j = json.loads(body)
                ok = j.get("source") == "screen" and "action" in j and "params" in j
            except ValueError:
                j, ok = None, False
            log(req="POST /control", action=(j or {}).get("action"), params=(j or {}).get("params"),
                complete=ok, body=body.decode("utf8", "replace"))
            time.sleep(1.0)
            try:
                reply(c, 200, '{"ok":true}')
                log(answered=(j or {}).get("action"))
            except OSError as e:
                log(answered=None, error=str(e))
        else:
            log(req=f"{method} {path}", unknown=True)
            reply(c, 404, "{}")
    except OSError:
        pass
    finally:
        c.close()


def main():
    s = socket.socket()
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("127.0.0.1", 9460))
    s.listen(16)
    log(up=True)
    while True:
        c, _ = s.accept()
        threading.Thread(target=handle, args=(c,), daemon=True).start()


main()
