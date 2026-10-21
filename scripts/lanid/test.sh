#!/bin/sh
# Synthetic capture through analyze.py; checks what each made-up client yields.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
python3 -I "$here/make_fixture.py" "$tmp/cap.pcap"
python3 -I "$here/analyze.py" --json "$tmp/cap.pcap" > "$tmp/out.json"
python3 -I - "$tmp/out.json" <<'P'
import json, sys
d = json.load(open(sys.argv[1]))["clients"]
ok = lambda c, m: c or sys.exit(f"FAIL: {m}\n{json.dumps(d, indent=1)}")
p = d["02:11:22:33:44:01"]
ok(p["dhcp"]["hostname"] == "Pixel-9" and p["new"] == ["dhcp-fp"], "phone: known answers and queries are not its names")
ok(p["dhcp"]["types"] == {"1": 1}, "dhcp types")
m = d["3c:22:fb:00:00:02"]
ok(m["mdns"]["names"] == ["Test MacBook Pro"] and m["mdns"]["model"] == ["Mac14,2"], "mac mdns")
ok(m["netbios"]["names"] == ["TEST-MBP"] and "netbios" in m["new"] and "mdns" in m["new"], "mac netbios")
t = d["00:09:b0:00:00:03"]
ok(t["ssdp"]["server"].endswith("Roku/12.5") and t["new"] == ["ssdp"], "tv")
ok(d["02:de:ad:00:00:04"]["mdns"].get("malformed") is True, "loop guard")
s = json.load(open(sys.argv[1]))["stats"]
ok(s["packets"]["mdns"] == 3 and s["packets"]["netbios"] == 3 and s["duplicates"] == 0, "stats")
print("lanid analyze: ok")
P
# the same file twice: every frame is a duplicate the second time
python3 -I "$here/analyze.py" --json "$tmp/cap.pcap" "$tmp/cap.pcap" | python3 -I -c 'import json,sys; d=json.load(sys.stdin)["stats"]; assert d["duplicates"] == 10, d'
python3 -I "$here/analyze.py" "$tmp/cap.pcap" | tail -n 3
