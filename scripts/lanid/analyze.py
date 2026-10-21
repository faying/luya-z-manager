#!/usr/bin/env python3
"""Step 0 of docs/designs/lan-client-id.md: what each source tells about each LAN client.

Reads the pcap files the core's step-0 capture wrote (crates/luyaz-core/src/lanid_capture.rs,
pulled from /tmp/luyaz/lanid/) and prints, per source MAC:

  dhcp:    option 12 host name, option 60 vendor class, option 55 order
  mdns:    instance / host names, _device-info model (md= / model=)
  ssdp:    SERVER header, NT / ST
  netbios: name in registrations and queries
  new:     sources that gave something the DHCP host name didn't

then the per-source hit counts (how many MACs got anything new from it). The phone / PC / IoT
category is not guessed: pass --kinds FILE with lines `MAC kind` to get the counts split by it.

Plain python3, no packages. Run with `python3 -I` on pulled captures (untrusted input).
    analyze.py [--json] [--kinds FILE] cap.old.pcap cap.pcap
"""

import json
import struct
import sys

MDNS, SSDP, NBNS, DHCP = 5353, 1900, 137, 67


def pcap_frames(path):
    with open(path, "rb") as f:
        data = f.read()
    if len(data) < 24:
        return
    magic = struct.unpack("<I", data[:4])[0]
    if magic in (0xA1B2C3D4, 0xA1B23C4D):
        e = "<"
    elif magic in (0xD4C3B2A1, 0x4D3CB2A1):
        e = ">"
    else:
        raise ValueError(f"{path}: not a pcap file")
    off = 24
    while off + 16 <= len(data):
        sec, usec, caplen, _orig = struct.unpack(e + "IIII", data[off:off + 16])
        off += 16
        if caplen > 65535 or off + caplen > len(data):
            return
        yield sec, usec, data[off:off + caplen]
        off += caplen


def mac_str(b):
    return ":".join(f"{x:02x}" for x in b)


def udp_of(frame):
    """(src mac, src port, dst port, payload) of an untagged IPv4/IPv6 UDP frame, else None."""
    if len(frame) < 14:
        return None
    src = mac_str(frame[6:12])
    et = struct.unpack("!H", frame[12:14])[0]
    p = frame[14:]
    if et == 0x0800:
        if len(p) < 20 or p[9] != 17:
            return None
        ihl = (p[0] & 0x0F) * 4
        if ihl < 20 or struct.unpack("!H", p[6:8])[0] & 0x1FFF or (p[6] & 0x20):
            return None
        u = p[ihl:]
    elif et == 0x86DD:
        if len(p) < 40 or p[6] != 17:
            return None
        u = p[40:]
    else:
        return None
    if len(u) < 8:
        return None
    sp, dp, ln = struct.unpack("!HHH", u[:6])
    return src, sp, dp, u[8:max(8, min(ln, len(u)))]


def dhcp(payload):
    """Options 12, 55, 60 of a BOOTREQUEST."""
    if len(payload) < 240 or payload[0] != 1 or payload[236:240] != b"\x63\x82\x53\x63":
        return None
    out = {}
    i = 240
    while i < len(payload):
        code = payload[i]
        if code == 255:
            break
        if code == 0:
            i += 1
            continue
        if i + 1 >= len(payload):
            break
        n = payload[i + 1]
        v = payload[i + 2:i + 2 + n]
        i += 2 + n
        if code == 12:
            out["hostname"] = v.decode("utf-8", "replace")
        elif code == 60:
            out["vendor"] = v.decode("utf-8", "replace")
        elif code == 55:
            out["opt55"] = ",".join(str(x) for x in v)
        elif code == 53 and v:
            out["type"] = v[0]
    return out


def dns_name(msg, off, depth=0):
    """(name, next offset); compression pointers followed at most 16 deep, no loops."""
    labels = []
    jumped = None
    hops = 0
    while True:
        if off >= len(msg):
            raise ValueError("name past end")
        n = msg[off]
        if n & 0xC0 == 0xC0:
            if off + 1 >= len(msg):
                raise ValueError("cut pointer")
            hops += 1
            if hops > 16:
                raise ValueError("pointer loop")
            if jumped is None:
                jumped = off + 2
            off = ((n & 0x3F) << 8) | msg[off + 1]
            continue
        if n == 0:
            off += 1
            break
        if n > 63 or off + 1 + n > len(msg):
            raise ValueError("bad label")
        labels.append(msg[off + 1:off + 1 + n].decode("utf-8", "replace"))
        off += 1 + n
        if len(labels) > 128:
            raise ValueError("too many labels")
    return ".".join(labels), (jumped if jumped is not None else off)


def mdns(payload):
    """Instance names, host names and device-info model from questions and records."""
    if len(payload) < 12:
        return None
    _id, flags, qd, an, ns, ar = struct.unpack("!HHHHHH", payload[:12])
    response = bool(flags & 0x8000)
    out = {"names": set(), "hosts": set(), "model": set(), "services": set()}
    off = 12
    try:
        for _ in range(min(qd, 64)):
            _name, off = dns_name(payload, off)
            off += 4
        for idx in range(min(an + ns + ar, 256)):
            name, off = dns_name(payload, off)
            if off + 10 > len(payload):
                break
            rtype, _cls, _ttl, rdlen = struct.unpack("!HHIH", payload[off:off + 10])
            off += 10
            rd = payload[off:off + rdlen]
            # a response's answers and additionals are the sender's own; a query's answers are
            # known answers about other devices, only its authority records (probe) are its own
            own = (idx < an or idx >= an + ns) if response else (an <= idx < an + ns)
            if not own:
                pass
            elif rtype == 12:  # PTR: service -> instance
                target, _ = dns_name(payload, off)
                if name.startswith("_") and not name.endswith(".arpa"):
                    out["services"].add(name.removesuffix(".local"))
                    inst = target.split("._", 1)[0]
                    if inst and not target.startswith("_"):
                        out["names"].add(inst)
            elif rtype in (1, 28):  # A / AAAA: host name
                out["hosts"].add(name.removesuffix(".local"))
            elif rtype == 33 and len(rd) >= 6:  # SRV
                tgt, _ = dns_name(payload, off + 6)
                out["hosts"].add(tgt.removesuffix(".local"))
                inst = name.split("._", 1)[0]
                if inst and not name.startswith("_"):
                    out["names"].add(inst)
            elif rtype == 16:  # TXT
                i = 0
                while i < len(rd):
                    n = rd[i]
                    s = rd[i + 1:i + 1 + n].decode("utf-8", "replace")
                    i += 1 + n
                    k, _, v = s.partition("=")
                    if k.lower() in ("md", "model", "ty", "usb_mdl") and v:
                        out["model"].add(v)
            off += rdlen
    except ValueError:
        out["malformed"] = True
    return out


def ssdp(payload):
    text = payload[:2048].decode("utf-8", "replace")
    out = {}
    for line in text.split("\r\n")[1:]:
        k, _, v = line.partition(":")
        k = k.strip().upper()
        if k == "SERVER" or k == "USER-AGENT":
            out["server"] = v.strip()
        elif k in ("NT", "ST"):
            out.setdefault("nt", set()).add(v.strip())
    return out


def nbns(payload):
    """The sender's own unique name, from registrations and refreshes only (a query names
    someone else); group names are skipped."""
    if len(payload) < 12 + 34 or payload[12] != 32:
        return None
    flags = struct.unpack("!H", payload[2:4])[0]
    if (flags >> 11) & 0xF not in (5, 8, 9):
        return None
    # the additional record after the question (name pointer, then 10 bytes, then NB flags)
    if len(payload) >= 50 + 2 + 10 + 2 and payload[50] == 0xC0 and payload[62] & 0x80:
        return None
    enc = payload[13:45]
    try:
        raw = bytes(((enc[i] - 0x41) << 4) | (enc[i + 1] - 0x41) for i in range(0, 32, 2))
    except ValueError:
        return None
    name = raw[:15].decode("ascii", "replace").rstrip()
    return {"name": name, "suffix": raw[15]} if name and name != "*" else None


SOURCES = ("dhcp", "mdns", "ssdp", "netbios", "other")


def source_of(sp, dp):
    if dp == DHCP:
        return "dhcp"
    if MDNS in (sp, dp):
        return "mdns"
    if dp == SSDP:
        return "ssdp"
    if dp == NBNS:
        return "netbios"
    return "other"


STATS = {}


def analyze(paths):
    seen = {}
    dup = set()
    first = last = None
    per = {s: 0 for s in SOURCES}
    dups = 0
    for path in paths:
        for sec, usec, frame in pcap_frames(path):
            # the ring's old file overlaps between pulls
            key = (sec, usec, frame)
            if key in dup:
                dups += 1
                continue
            dup.add(key)
            first = sec if first is None else min(first, sec)
            last = sec if last is None else max(last, sec)
            u = udp_of(frame)
            if not u:
                per["other"] += 1
                continue
            src, sp, dp, pl = u
            per[source_of(sp, dp)] += 1
            c = seen.setdefault(src, {"dhcp": {}, "mdns": {}, "ssdp": {}, "netbios": {}, "packets": 0})
            c["packets"] += 1
            if dp == DHCP:
                r = dhcp(pl)
                if r:
                    if "type" in r:
                        t = c["dhcp"].setdefault("types", {})
                        t[str(r["type"])] = t.get(str(r["type"]), 0) + 1
                    c["dhcp"].update({k: v for k, v in r.items() if k != "type"})
            elif MDNS in (sp, dp):
                r = mdns(pl)
                if r:
                    for k, v in r.items():
                        if isinstance(v, set):
                            c["mdns"].setdefault(k, set()).update(v)
                        else:
                            c["mdns"][k] = v
            elif dp == SSDP:
                r = ssdp(pl)
                if r.get("server"):
                    c["ssdp"]["server"] = r["server"]
                if r.get("nt"):
                    c["ssdp"].setdefault("nt", set()).update(r["nt"])
            elif dp == NBNS:
                r = nbns(pl)
                if r:
                    c["netbios"].setdefault("names", set()).add(r["name"])
    for c in seen.values():
        host = (c["dhcp"].get("hostname") or "").lower()
        new = []
        if c["dhcp"].get("vendor") or c["dhcp"].get("opt55"):
            new.append("dhcp-fp")
        m = c["mdns"]
        if any(n.lower() != host for n in m.get("names", set()) | m.get("hosts", set())) or m.get("model"):
            new.append("mdns")
        if c["ssdp"].get("server"):
            new.append("ssdp")
        if any(n.lower() != host for n in c["netbios"].get("names", set())):
            new.append("netbios")
        c["new"] = new
    span = (last - first) if first is not None else 0
    STATS.update(span_s=span, duplicates=dups, packets=per,
                 per_hour={k: round(v * 3600 / span, 1) for k, v in per.items()} if span else {})
    return seen


def plain(v):
    if isinstance(v, set):
        return sorted(v)
    if isinstance(v, dict):
        return {k: plain(x) for k, x in v.items()}
    return v


def main(argv):
    as_json = "--json" in argv
    kinds = {}
    args = [a for a in argv if a != "--json"]
    if "--kinds" in args:
        i = args.index("--kinds")
        with open(args[i + 1]) as f:
            for line in f:
                p = line.split()
                if len(p) >= 2 and not line.startswith("#"):
                    kinds[p[0].lower()] = p[1]
        del args[i:i + 2]
    if not args:
        print(__doc__, file=sys.stderr)
        return 2
    seen = analyze(args)
    if as_json:
        print(json.dumps({"clients": plain(seen), "stats": STATS}, ensure_ascii=False, indent=1, sort_keys=True))
        return 0
    for mac in sorted(seen):
        c = seen[mac]
        local = int(mac[:2], 16) & 2
        print(f"{mac} {'(random)' if local else '(oui)':9} {kinds.get(mac, '?'):6} pkts={c['packets']} new={','.join(c['new']) or '-'}")
        d = c["dhcp"]
        if d:
            print(f"  dhcp    host={d.get('hostname', '-')!r} vendor={d.get('vendor', '-')!r} opt55={d.get('opt55', '-')} types={d.get('types', {})}")
        m = c["mdns"]
        if m:
            print(f"  mdns    names={sorted(m.get('names', []))} hosts={sorted(m.get('hosts', []))} model={sorted(m.get('model', []))}"
                  + (" MALFORMED" if m.get("malformed") else ""))
        if c["ssdp"]:
            print(f"  ssdp    server={c['ssdp'].get('server', '-')!r} nt={len(c['ssdp'].get('nt', []))}")
        if c["netbios"]:
            print(f"  netbios names={sorted(c['netbios']['names'])}")
    print()
    print(f"span {STATS['span_s']} s, duplicates skipped {STATS['duplicates']}, packets {STATS['packets']}")
    if STATS["per_hour"]:
        print(f"per hour {STATS['per_hour']}")
    total = {}
    for mac, c in seen.items():
        k = kinds.get(mac, "all")
        t = total.setdefault(k, {"macs": 0})
        t["macs"] += 1
        for s in c["new"]:
            t[s] = t.get(s, 0) + 1
    for k, t in sorted(total.items()):
        n = t.pop("macs")
        line = " ".join(f"{s}={v}/{n} ({100 * v // n}%)" for s, v in sorted(t.items()))
        print(f"{k}: {n} MACs  {line or 'nothing new'}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
