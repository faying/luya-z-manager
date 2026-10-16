#!/bin/sh
# wan-sources.sh summarizing against a fixture capture (busybox container).
# SPDX-License-Identifier: MIT
SCRIPTS=${SCRIPTS:-/scripts}
PASS=0; FAIL=0
ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; }
check() { _d=$1; shift; if eval "$@"; then ok "$_d"; else bad "$_d  [$*]"; fi; }
T=$(mktemp -d)
printf 'udp 41641 tailscaled\ntcp 38000 zte-agent\n' > "$T/own"
cat > "$T/cap" <<X
1000.1 IP 10.0.0.5.41641 > 1.2.3.4.3478: UDP, length 36
1000.2 IP 1.2.3.4.3478 > 10.0.0.5.41641: UDP, length 40
1001.5 IP 10.0.0.5.41641 > 1.2.3.4.3478: UDP, length 36
1004.0 IP 10.0.0.5.41641 > 5.6.7.8.3478: UDP, length 36
1004.5 IP 10.0.0.5.38000 > 9.9.9.9.443: tcp 120
1009.9 IP 10.0.0.5.51000 > 8.8.8.8.53: UDP, length 30
X
out=$(sh "$SCRIPTS/wan-sources.sh" --summarize "$T/own" "10.0.0.5" < "$T/cap")
echo "$out" > "$T/out"
check "totals: 6 packets, 4 distinct seconds, 10 s span" 'grep -q "packets: 6, seconds with any packet: 4 of the 10-second span" "$T/out"'
check "tailscaled owns 4 packets over 3 seconds" 'grep -Eq "^ +4 +3  tailscaled$" "$T/out"'
check "zte-agent attributed by local tcp port" 'grep -Eq "^ +1 +1  zte-agent$" "$T/out"'
check "unknown local port shows ?" 'grep -Eq "^ +1 +1  \?$" "$T/out"'
check "flow to 1.2.3.4:3478 has 3 packets, 112 bytes, 2 seconds" 'grep -Eq "^ +3 +112 +2  tailscaled +udp 41641 1.2.3.4:3478$" "$T/out"'
rm -rf "$T"
echo "wan-sources: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
