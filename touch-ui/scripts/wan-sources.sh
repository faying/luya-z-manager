#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# wan-sources.sh — who keeps the cellular link busy?
#
#   sh /data/luyaz/scripts/wan-sources.sh [minutes]        default 5
#
# Run it detached, with no SSH session open (SSH rides Tailscale, which rides
# the cellular link, and would show up as the top talker):
#   nohup sh /data/luyaz/scripts/wan-sources.sh 10 >/dev/null 2>&1 &
#
# Captures packet HEADERS only (addresses, ports, protocol, length; never
# payload) on the cellular interface for the given time, then reports:
#   - per flow (protocol, local port, remote address:port): packets, bytes,
#     how many distinct seconds it was active, and the owning program where
#     the local port maps to one (netstat -tunp snapshots at start and end);
#   - per program: packets and active seconds.
# "Active seconds" is what matters for the modem: a flow that sends one tiny
# packet every few seconds keeps the radio awake as surely as a download.
# Read-only. The report goes to /data/power/wan-sources-<time>.txt; the raw
# header capture is deleted afterwards.
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────

WAN=${WS_WAN:-rmnet_data0}
OUT_DIR=${WS_OUT:-/data/power}
TCPDUMP=${WS_TCPDUMP:-tcpdump}
NETSTAT=${WS_NETSTAT:-netstat}

# stdin: tcpdump -tt -nn -q lines; $1: file of "proto port program" owners;
# $2: space-separated local addresses. Prints the report body.
summarize() {
    awk -v owners="$1" -v locals=" $2 " '
        BEGIN {
            while ((getline l < owners) > 0) { split(l, f, " "); own[f[1] "/" f[2]] = f[3] }
        }
        # 1790190000.123456 IP 10.46.15.61.41641 > 1.2.3.4.3478: UDP, length 36
        # 1790190000.123456 IP6 240e::1.58834 > 2607::44c.443: tcp 0
        $2 == "IP" || $2 == "IP6" {
            sec = int($1); src = $3; dst = $5; sub(/:$/, "", dst)
            # the whole ?: in parentheses: busybox awk misparses "x = (c) ? a : b" as x = (c)
            proto = ((tolower($0) ~ /udp/) ? "udp" : ((tolower($0) ~ /icmp/) ? "icmp" : "tcp"))
            len = 0; if (match($0, /length [0-9]+/)) len = substr($0, RSTART + 7, RLENGTH - 7) + 0
            else if (match($0, /tcp [0-9]+/)) len = substr($0, RSTART + 4, RLENGTH - 4) + 0
            # split host.port (the port is after the last dot)
            sp = src; sub(/\.[0-9]+$/, "", sp); spo = substr(src, length(sp) + 2)
            dp = dst; sub(/\.[0-9]+$/, "", dp); dpo = substr(dst, length(dp) + 2)
            if (index(locals, " " sp " ")) { lport = spo; remote = dp ":" dpo }
            else if (index(locals, " " dp " ")) { lport = dpo; remote = sp ":" spo }
            else { lport = spo; remote = dp ":" dpo }
            prog = own[proto "/" lport]; if (prog == "") prog = "?"
            key = proto " " lport " " remote
            pk[key]++; by[key] += len; pg[key] = prog
            if (!(key SUBSEP sec in seen)) { seen[key, sec] = 1; act[key]++ }
            ppk[prog]++; if (!(prog SUBSEP sec in pseen)) { pseen[prog, sec] = 1; pact[prog]++ }
            if (!(sec in allsec)) { allsec[sec] = 1; nsec++ }
            if (first == "" || sec < first) first = sec; if (sec > last) last = sec
            total++
        }
        END {
            printf "packets: %d, seconds with any packet: %d of the %d-second span\n\n", total, nsec, (last >= first && first != "") ? last - first + 1 : 0
            print "By program (packets, active seconds)"
            for (p in ppk) printf "%8d %6d  %s\n", ppk[p], pact[p], p | "sort -k2 -nr"
            close("sort -k2 -nr")
            print "\nBy flow (packets, bytes, active seconds, program, proto local-port remote)"
            for (k in pk) printf "%8d %9d %6d  %-14s %s\n", pk[k], by[k], act[k], pg[k], k | "sort -k3 -nr | head -40"
            close("sort -k3 -nr | head -40")
        }'
}

# "proto port program" for every socket netstat can attribute
owners() {
    $NETSTAT -tunp 2>/dev/null | awk '
        # tcp and udp lines both end in PID/program (tcp has a State column before it)
        $1 ~ /^(tcp|udp)/ && $NF ~ /\// {
            p = (($1 ~ /^udp/) ? "udp" : "tcp")
            a = $4; n = split(a, x, ":"); port = x[n]
            split($NF, q, "/"); prog = q[2]
            if (port != "" && prog != "") print p, port, prog
        }
 ' | sort -u
}

case "$1" in
    --summarize) summarize "$2" "$3"; exit ;;   # test hook: stdin = tcpdump lines
esac

MIN=${1:-5}
W=$(mktemp -d) || exit 1
trap 'rm -rf "$W"' EXIT
owners > "$W/own"
LOCALS=$(ip -o addr show dev "$WAN" 2>/dev/null | awk '{sub(/\/.*/, "", $4); printf "%s ", $4}')
$TCPDUMP -i "$WAN" -tt -nn -q -l > "$W/cap" 2>/dev/null &
TD=$!
sleep $((MIN * 60))
kill "$TD" 2>/dev/null; wait "$TD" 2>/dev/null
owners >> "$W/own"; sort -u -o "$W/own" "$W/own"

mkdir -p "$OUT_DIR"
report=$OUT_DIR/wan-sources-$(date +%Y%m%d-%H%M%S)-$WAN.txt
{
    echo "Cellular talkers on $WAN — $(date '+%Y-%m-%d %H:%M') device local, $MIN min (headers only)"
    echo "local addresses: $LOCALS"
    echo
    summarize "$W/own" "$LOCALS" < "$W/cap"
} > "$report"
cat "$report"; echo; echo "saved: $report"
