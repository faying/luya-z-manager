#!/usr/bin/env python3
"""Luya-Z device alerts -> chat (Hermes cron, --no-agent). docs/designs/u60-mcp.md 多台 (D3-D7).

Asks each device through luyaz-mcp (get_recent_events) for the alerts after its saved cursor and
prints one line per new alert; nothing new = no output = nothing sent. The master copy lives in
the manager repo (scripts/hermes/); the one Hermes runs is a copy. No addresses, tokens or
channels in here: devices come from the luyaz-mcp device list.

Two ways to run:
- With a device list (`~/.config/luyaz/devices`, or --devices / LUYAZ_MCP_DEVICES): every listed
  device, each on its own (one that cannot be reached does not stop the others); messages start
  with `[name]`. Cursors are kept per device_id in `<state dir>/luyaz-alerts.json`, so renaming a
  line neither repeats nor drops alerts. An enrolled device that now answers as another device is
  paused (its old cursor kept) and said once; after `luyaz-mcp enroll <name>` the new device
  starts from its current position. A bad device list: nothing is forwarded, said once.
- Without a list: one device as luyaz-mcp's LUYAZ_MCP_URL / LUYAZ_MCP_TOKEN(_FILE) say (--url,
  --token-file set them), the old cursor file `<state dir>/hermes-alerts.json`, messages as before.

The old cursor file is carried over (D7) only when the list has exactly one device and it is
enrolled; it is then renamed `.migrated`. If that one device's admin web is too old to send a
device_id, the old file keeps being used as before.

First sight of a device only sets its cursor (old alerts are not replayed). Device unreachable:
silent, stderr only (the next run catches up from the cursor).
"""
import argparse
import json
import os
import subprocess
import sys
import time

HOME = os.path.expanduser("~")
CONF = os.path.join(HOME, ".config", "luyaz")
PAGES = 6  # 50 per page = the device's whole queue (300)


class DeviceError(Exception):
    """luyaz-mcp said isError; code = structuredContent.error ("" when none)."""

    def __init__(self, code, text):
        super().__init__(text)
        self.code = code


class Cfg:
    def __init__(self, bin_cmd, devices, state_dir, env):
        self.bin_cmd = bin_cmd
        self.devices = devices
        self.state = os.path.join(state_dir, "luyaz-alerts.json")
        self.legacy = os.path.join(state_dir, "hermes-alerts.json")
        self.last = os.path.join(state_dir, "hermes-alerts.last")
        self.env = env


def call(cfg, tool, args):
    """One tools/call through a fresh luyaz-mcp; the result's JSON text, parsed."""
    msgs = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize",
         "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": {"name": "hermes-luyaz-alerts", "version": "2"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/call",
         "params": {"name": tool, "arguments": args}},
    ]
    p = subprocess.run(cfg.bin_cmd, input="".join(json.dumps(m) + "\n" for m in msgs),
                       capture_output=True, text=True, env=cfg.env, timeout=30)
    for line in p.stdout.splitlines():
        r = json.loads(line)
        if r.get("id") == 2:
            if "error" in r:
                raise RuntimeError(r["error"].get("message", "protocol error"))
            res = r.get("result") or {}
            text = (res.get("content") or [{}])[0].get("text", "")
            if res.get("isError"):
                raise DeviceError((res.get("structuredContent") or {}).get("error", ""), text)
            try:
                return json.loads(text)
            except ValueError:
                raise RuntimeError(text)  # "device cannot be reached …" and the like
    raise RuntimeError(p.stderr.strip() or "no reply from luyaz-mcp")


def read_pages(cfg, since, device=None):
    """Events after `since` (None = the newest only): up to PAGES pages; (events, last page, first page)."""
    def one(s):
        a = {"limit": 50}
        if s is not None:
            a["since"] = s
        if device is not None:
            a["device"] = device
        return call(cfg, "get_recent_events", a)

    first = page = one(since)
    events = list(page.get("events", []))
    for _ in range(PAGES - 1):
        if not page.get("more"):
            break
        page = one(page.get("next"))
        events += page.get("events", [])
    return events, page, first


# Night (23:00–08:00 Mac local time): ask at most every 30 min. Each ask wakes the device's
# cellular link through the tailnet (Phase 9a, 10-18: one ask per 5 min drove a large share of tailscaled's
# night traffic). Daytime stays every tick (5 min).
NIGHT = (23, 8)
NIGHT_EVERY_S = 30 * 60


def skip_tick(last, hour=None, now=None):
    h = time.localtime().tm_hour if hour is None else hour
    if not (h >= NIGHT[0] or h < NIGHT[1]):
        return False
    try:
        return (time.time() if now is None else now) - os.path.getmtime(last) < NIGHT_EVERY_S
    except OSError:
        return False


def plain(v):
    return v.get("untrusted", "") if isinstance(v, dict) else (v or "")


def lines_for(who, events, page, sms_seen):
    """Messages for a page read from a known cursor; `who` = "U60" (no list) or "[name]"."""
    out = []
    if page.get("gap"):
        out.append(("⚠️ U60：" if who == "U60" else f"⚠️ {who} ")
                   + "中间有告警已被设备清掉，没能全部转发；完整列表看管理网页「告警」。")
    for e in events:
        t = (e.get("time") or "")[:16].replace("T", " ")
        out.append(f"🔔 {who} 告警 #{e.get('seq')} {t}｜{e.get('label') or e.get('kind')}：{plain(e.get('text'))}")
    for s in page.get("sms", []):
        key = (s.get("seq"), s.get("result"))
        if key not in sms_seen and s.get("result") not in ("sent", "suppressed-abroad"):
            out.append(f"📵 {who} 告警短信没发出去（#{s.get('seq')} {s.get('kind')}：{s.get('result')}）")
    return out


def cursor_after(page, old_next):
    return {"next": page.get("next", old_next),
            "sms": [[s.get("seq"), s.get("result")] for s in page.get("sms", [])]}


def load(path):
    try:
        with open(path) as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def save(path, obj):
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(obj, f)
    os.replace(tmp, path)


# ---- no device list: the single device, the old cursor file, the old messages ----

def run_single(cfg):
    state = load(cfg.legacy)
    first = state is None
    state = state or {"next": None, "sms": []}
    try:
        events, page, _ = read_pages(cfg, state["next"])
    except Exception as e:  # noqa: BLE001 — any failure: quiet, retry next tick
        print(f"luyaz alerts: {e}", file=sys.stderr)
        return []
    out = [] if first else lines_for("U60", events, page, {tuple(x) for x in state.get("sms", [])})
    save(cfg.legacy, cursor_after(page, state["next"]))
    return out


# ---- the device list ----

def notify_once(state, topic, detail, text, out):
    """Say `text` once while `topic` stays at `detail` (a new detail is said again)."""
    notes = state.setdefault("notified", {})
    if notes.get(topic) != detail:
        out.append(text)
    notes[topic] = detail


def clear_note(state, topic):
    state.get("notified", {}).pop(topic, None)


def run_device(cfg, state, d, only_one, out):
    name = d["name"]
    who = f"[{name}]"
    store = state.setdefault("devices", {})
    names = state.setdefault("names", {})
    legacy = load(cfg.legacy) if only_one and os.path.exists(cfg.legacy) else None

    def cur(key):
        if key == "legacy":
            return legacy
        return store.get(key) if key else None

    # where to read from: the enrolled id's cursor, the last id seen under this name, the old file
    pre = None
    for k in (d.get("device_id"), names.get(name), "legacy" if legacy is not None else None):
        if k and cur(k) is not None:
            pre = k
            break
    cursor = cur(pre)
    try:
        events, page, first = read_pages(cfg, cursor["next"] if cursor else None, name)
    except DeviceError as e:
        if e.code in ("wrong_device", "no_device_id"):
            notify_once(state, f"wrong:{name}", f"{e.code}:{e}",
                        f"⚠️ {who} 现在回答的是另一台设备（或它的管理网页太旧，说不出是哪台），这台的告警暂停转发；"
                        f"确认是换了设备后运行 luyaz-mcp enroll {name}。（{e}）", out)
            return
        raise
    clear_note(state, f"wrong:{name}")
    did = first.get("device_id")
    key = did or ("legacy" if only_one else f"name:{name}")
    migrating = pre == "legacy" and did and d.get("enrolled")
    if cursor is not None and (key == pre or migrating):
        out.extend(lines_for(who, events, page, {tuple(x) for x in cursor.get("sms", [])}))
        new = cursor_after(page, cursor["next"])
    elif cur(key) is not None:
        # this device under another name (a renamed line): go on from its own cursor
        old = cur(key)
        events, page, _ = read_pages(cfg, old["next"], name)
        out.extend(lines_for(who, events, page, {tuple(x) for x in old.get("sms", [])}))
        new = cursor_after(page, old["next"])
    else:
        # first sight of this device: only its current position, nothing replayed
        if cursor is not None:
            _, page, _ = read_pages(cfg, None, name)
            if pre != "legacy":
                out.append(f"ℹ️ {who} 现在是另一台设备（device_id {did or '未知'}），从现在起转发它的告警；之前那台没转发完的不补发。")
        new = cursor_after(page, None)
    if key == "legacy":
        save(cfg.legacy, new)
    else:
        store[key] = dict(new, name=name)
    names[name] = key
    if migrating:
        os.replace(cfg.legacy, cfg.legacy + ".migrated")


def run_list(cfg):
    out = []
    state = load(cfg.state) or {}
    try:
        listing = call(cfg, "list_devices", {})
    except DeviceError as e:
        if e.code == "devices_file":
            notify_once(state, "devices_file", str(e), f"⚠️ Luya-Z 设备清单有问题，这一轮所有设备都不转发告警：{e}", out)
            save(cfg.state, state)
        else:
            print(f"luyaz alerts: {e}", file=sys.stderr)
        return out
    except Exception as e:  # noqa: BLE001
        print(f"luyaz alerts: {e}", file=sys.stderr)
        return out
    clear_note(state, "devices_file")
    devices = listing.get("devices", [])
    for d in devices:
        try:
            run_device(cfg, state, d, len(devices) == 1, out)
        except Exception as e:  # noqa: BLE001 — this device only; the others go on
            tag = f"[{d.get('name')}]"
            print(f"luyaz alerts: {e if str(e).startswith(tag) else f'{tag} {e}'}", file=sys.stderr)
    save(cfg.state, state)
    return out


def run(cfg):
    if skip_tick(cfg.last):
        return []
    open(cfg.last, "w").close()
    return run_list(cfg) if os.path.exists(cfg.devices) else run_single(cfg)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--bin", default=os.environ.get("LUYAZ_MCP_BIN", os.path.join(CONF, "bin", "luyaz-mcp")),
                    help="luyaz-mcp (default ~/.config/luyaz/bin/luyaz-mcp)")
    ap.add_argument("--devices", default=os.environ.get("LUYAZ_MCP_DEVICES", os.path.join(CONF, "devices")),
                    help="device list (default ~/.config/luyaz/devices)")
    ap.add_argument("--state-dir", default=os.environ.get("LUYAZ_ALERTS_DIR", CONF),
                    help="where the cursors live (default ~/.config/luyaz)")
    ap.add_argument("--url", help="no list only: the device (sets LUYAZ_MCP_URL)")
    ap.add_argument("--token-file", help="no list only: its token (sets LUYAZ_MCP_TOKEN_FILE)")
    a = ap.parse_args(argv)
    env = dict(os.environ, LUYAZ_MCP_DEVICES=a.devices)
    if a.url:
        env["LUYAZ_MCP_URL"] = a.url
    if a.token_file:
        env["LUYAZ_MCP_TOKEN_FILE"] = a.token_file
    out = run(Cfg([a.bin], a.devices, a.state_dir, env))
    if out:
        print("\n".join(out))


if __name__ == "__main__":
    main()
