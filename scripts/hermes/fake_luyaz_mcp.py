#!/usr/bin/env python3
"""A stand-in luyaz-mcp for test_luyaz_alerts.py: answers list_devices and get_recent_events from
the JSON file FAKE_MCP_WORLD and appends each call to FAKE_MCP_LOG.

World: {"list": [{"name", "enrolled", "device_id"?}] | {"error": code, "text": ...},
        "devices": {name: {"device_id"?, "events": [seq, ...], "sms": [...], "gap"?,
                           "unreachable"?, "refuse"?: code}}}
A device answers as luyaz-mcp would: an enrolled id that differs from the device's own is refused
(wrong_device); no `device` with one device = that one ("-" when there is no list at all).
"""
import json
import os
import sys


def result(obj=None, error=None, text=None):
    if error:
        return {"content": [{"type": "text", "text": text or error}], "isError": True,
                "structuredContent": {"error": error}}
    if text is not None:
        return {"content": [{"type": "text", "text": text}], "isError": False}
    return {"content": [{"type": "text", "text": json.dumps(obj)}], "isError": False}


def events(dev, args):
    seqs = dev.get("events", [])
    limit = args.get("limit", 20)
    since = args.get("since")
    if since is None:
        page, more = seqs[-limit:], False
    else:
        after = [s for s in seqs if s > since]
        page, more = after[:limit], len(after) > limit
    nxt = page[-1] if page else (since if since is not None else (seqs[-1] if seqs else 0))
    v = {"events": [{"seq": s, "time": "2026-10-20T08:00:00+08:00", "kind": "k", "label": "L",
                     "text": {"untrusted": f"e{s}"}} for s in page],
         "next": nxt, "more": more, "gap": bool(dev.get("gap")) and since is not None,
         "sms": dev.get("sms", [])}
    if dev.get("device_id"):
        v["device_id"] = dev["device_id"]
    return v


def answer(world, name, args):
    if name == "list_devices":
        lst = world["list"]
        if isinstance(lst, dict):
            return result(error=lst["error"], text=lst["text"])
        return result({"list": True, "devices": lst})
    listed = {d["name"]: d for d in world["list"]} if isinstance(world.get("list"), list) else {}
    want = args.get("device")
    if want is None:
        want = next(iter(listed)) if len(listed) == 1 else "-"
    dev = world["devices"][want]
    if dev.get("unreachable"):
        return result(text="The device cannot be reached (connect: refused).")
    if dev.get("refuse"):
        return result(error=dev["refuse"], text=f"refused: {dev['refuse']}")
    pin = listed.get(want, {}).get("device_id")
    if pin and pin != dev.get("device_id"):
        return result(error="wrong_device",
                      text=f"Refused: {want} now answers as another device (device_id {dev.get('device_id')}).")
    return result(events(dev, args))


def main():
    world = json.load(open(os.environ["FAKE_MCP_WORLD"]))
    for line in sys.stdin:
        m = json.loads(line)
        if "id" not in m:
            continue
        if m["method"] == "initialize":
            r = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}}
        else:
            p = m["params"]
            with open(os.environ["FAKE_MCP_LOG"], "a") as f:
                f.write(json.dumps([p["name"], p.get("arguments", {})]) + "\n")
            r = answer(world, p["name"], p.get("arguments", {}))
        print(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": r}), flush=True)


if __name__ == "__main__":
    main()
