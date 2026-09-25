#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# /data/tailscale/start.sh — start tailscaled and bring the node up.
# Called from /etc/rc.local at boot, and by apply.sh when trying a tuning
# variant. Installed from source/manager/scripts/tailscale/start.sh.
#
#   binaries  /data/tailscale/{tailscaled,tailscale}
#   state     /data/tailscale/state
#   socket    /tmp/tailscaled.sock
#   log       /data/tailscaled.log  (appended; u60-guard keeps it ≤ 1 MB + one .old)
#
# Nothing here is specific to one owner's network: the advertised subnet is
# read from br-lan at start, the hostname defaults to u60pro. Per-device
# choices go in /data/tailscale/tuning.env (not in any repository):
#   TS_HOSTNAME=...            node name
#   TS_ROUTES=a.b.c.0/24       override the subnet read from br-lan
#   TS_TAILSCALED_FLAGS=...    extra tailscaled flags, e.g. --no-logs-no-support
#   TS_TAILSCALED_ENV=...      extra environment for tailscaled, e.g.
#                              TS_DISABLE_PORTMAPPER=1
#   TS_TAILSCALED_BIN=...      another tailscaled build to run (keep the file
#                              name "tailscaled": pidof finds it by name), e.g.
#                              /data/tailscale/nofight/tailscaled
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────

D=${TS_DIR:-/data/tailscale}
LOG=${TS_LOG:-/data/tailscaled.log}
SOCK=/tmp/tailscaled.sock

mkdir -p "$D/state"
# the tun device node is not always there this early in boot
[ -c /dev/net/tun ] || { mkdir -p /dev/net; mknod /dev/net/tun c 10 200; }
pidof tailscaled >/dev/null && exit 0

TS_HOSTNAME=u60pro
TS_ROUTES=
TS_TAILSCALED_FLAGS=
TS_TAILSCALED_ENV=
TS_TAILSCALED_BIN=
[ -f "$D/tuning.env" ] && . "$D/tuning.env"

# The LAN subnet, e.g. 192.168.0.1/24 on br-lan → 192.168.0.0/24
if [ -z "$TS_ROUTES" ]; then
    TS_ROUTES=$(ip -o -4 addr show br-lan 2>/dev/null | awk '{print $4; exit}' |
        awk -F'[./]' 'NF == 5 && $5 == 24 {print $1 "." $2 "." $3 ".0/24"}')
fi

# Append (>>), never truncate-on-open: u60-guard caps the file by copying it
# to .old and truncating, which is only safe for an O_APPEND writer.
# shellcheck disable=SC2086 # the extra flags/env are deliberately word-split
env $TS_TAILSCALED_ENV nohup "${TS_TAILSCALED_BIN:-$D/tailscaled}" --state="$D/state/tailscaled.state" --statedir="$D/state" \
    --socket="$SOCK" --port=41641 --tun=tailscale0 $TS_TAILSCALED_FLAGS >>"$LOG" 2>&1 &
sleep 4   # let tailscaled open its socket before "up"

"$D/tailscale" --socket="$SOCK" up ${TS_ROUTES:+--advertise-routes=$TS_ROUTES} --accept-routes \
    --accept-dns=false --hostname="$TS_HOSTNAME" --timeout=30s >/tmp/tailscale-up.log 2>&1 &
