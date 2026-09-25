#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# tailscale-start.sh — boot-time Tailscale bring-up for ZTE 5G routers.
#
# Consolidated from what actually runs on real devices (U60 Pro (MU5250) and
# TopFlow/MU5252). One script for all of them: everything that differs per
# device is either read from $TSCONF or auto-detected, so the same file can be
# dropped on any of them.
#
# All site-specific settings live in $TSCONF, written by install.sh and kept
# chmod 600 ON THE DEVICE ONLY — never commit it. This template carries NO
# secrets.
#
#   TSCONF keys:
#     TS_AUTHKEY        tskey-auth-…  (use a *reusable* key; leave empty after
#                                      first login to rely on saved state)
#     TS_ROUTES         advertise-routes, e.g. 192.168.0.0/24
#     TS_EXIT_NODE      exit-node (IP or name); empty = none
#     TS_ACCEPT_ROUTES  true|false (default true)
#     TS_ACCEPT_DNS     true|false (default false)
#     TS_HOSTNAME       node name; MUST be set once the node's name differs from
#                       the default, because `tailscale up` refuses to run at all
#                       unless every non-default setting is named on the command line
#     TS_STATE          state file; empty = auto-detect an existing one
#     TS_LAN_IF         LAN bridge for the forwarding rules (default br-lan)
#     TS_SNAT           true|false — masquerade LAN clients behind our tailnet IP
#                       (default true)
#     TS_MWAN3_BYPASS   auto|true|false — steer tailnet traffic around mwan3's
#                       policy routing (default auto: on when mwan3 marks are live)
#     TS_TAILNET_SSH    auto|true|false — extra dropbear bound to the tailnet IP
#                       (default auto: on when /data/dropbear/bin/dropbear exists)
# ─────────────────────────────────────────────────────────────────────────────
TSDIR=/data/tailscale
TSCONF=$TSDIR/tsconfig
SOCK=/tmp/tailscaled.sock
LOG=/data/tailscale-autostart.log
DAEMON_LOG=/data/tailscaled.log
echo "=== $(date) begin ===" > $LOG

# config (no secrets in this script; tsconfig is device-only)
[ -f "$TSCONF" ] && . "$TSCONF"
: "${TS_ROUTES:=}"
: "${TS_EXIT_NODE:=}"
: "${TS_ACCEPT_ROUTES:=true}"
: "${TS_ACCEPT_DNS:=false}"
: "${TS_HOSTNAME:=}"
: "${TS_STATE:=}"
: "${TS_LAN_IF:=br-lan}"
: "${TS_SNAT:=true}"
: "${TS_MWAN3_BYPASS:=auto}"
: "${TS_TAILNET_SSH:=auto}"

[ -z "$TS_ROUTES" ] && \
    echo "$(date) WARNING: TS_ROUTES empty in $TSCONF — no subnet route advertised" >> $LOG

# Never guess the state file: starting tailscaled against a *missing* state file
# silently creates an empty one, which drops the node's identity and forces a
# re-login. Older installs use $TSDIR/state, newer ones use
# $TSDIR/tailscaled.state — adopt whichever is already there.
if [ -z "$TS_STATE" ]; then
    if   [ -f "$TSDIR/tailscaled.state" ]; then TS_STATE="$TSDIR/tailscaled.state"
    elif [ -f "$TSDIR/state" ];            then TS_STATE="$TSDIR/state"
    else                                        TS_STATE="$TSDIR/tailscaled.state"
    fi
fi
echo "$(date) state file: $TS_STATE" >> $LOG

# ---------- 1. wait for the WAN ----------
# Interface-agnostic on purpose: the U60 Pro exits via rmnet_data0, the TopFlow
# via V3E1net0/V3E2net0 under mwan3. Any working default route will do.
for i in $(seq 1 60); do
    if ping -c 1 -W 2 223.5.5.5 >/dev/null 2>&1; then
        echo "$(date) network ready after ${i}" >> $LOG
        break
    fi
    sleep 2
done
sleep 3

# ---------- 2. sync time, but only if the clock is unusable ----------
sync_time_http() {
    for url in http://www.taobao.com http://www.baidu.com http://www.aliyun.com; do
        if command -v curl >/dev/null 2>&1; then
            HTTP_DATE=$(curl -sI --max-time 5 "$url" 2>/dev/null | awk -F': ' 'tolower($1)=="date"{print $2; exit}' | tr -d '\r\n')
        else
            HTTP_DATE=$(wget -S -O /dev/null "$url" 2>&1 | awk -F'Date: ' '/Date: /{print $2; exit}' | tr -d '\r\n')
        fi
        [ -n "$HTTP_DATE" ] || continue
        # busybox `date -s` cannot parse RFC 1123 ("Mon, 24 Aug 2026 19:13:16 GMT")
        # and fails on every boot without this. Convert to "YYYY-MM-DD hh:mm:ss".
        CONV=$(printf '%s\n' "$HTTP_DATE" | awk '
            BEGIN{split("Jan Feb Mar Apr May Jun Jul Aug Sep Oct Nov Dec",m," ");
                  for(i=1;i<=12;i++) mon[m[i]]=sprintf("%02d",i)}
            NF>=5 && mon[$3]!="" {print $4"-"mon[$3]"-"$2" "$5}')
        [ -n "$CONV" ] || continue
        if date -u -s "$CONV" >> $LOG 2>&1; then
            echo "$(date) time synced via $url -> $CONV UTC" >> $LOG
            return 0
        fi
    done
    echo "$(date) WARNING: HTTP time sync failed, trying ZTE NTP" >> $LOG
    /etc/init.d/zte_topsw_ntp restart >> $LOG 2>&1
    /etc/init.d/sysfixtime restart >> $LOG 2>&1
    sleep 5
}
# Only step in when the clock is actually unusable — TLS just needs a sane year.
# Do not "correct" a working clock: these devices run Beijing time labelled UTC,
# and ZTE's own time daemon drags it back within minutes, so syncing every boot
# only churns the clock by 8h for no gain.
if [ "$(date +%Y)" -lt 2026 ]; then
    echo "$(date) clock unusable ($(date +%Y)) — syncing" >> $LOG
    sync_time_http
    for i in $(seq 1 30); do
        [ "$(date +%Y)" -ge 2026 ] && { echo "$(date) time confirmed OK" >> $LOG; break; }
        sleep 2
    done
else
    echo "$(date) clock already sane — left alone" >> $LOG
fi
sleep 2

# ---------- 3. TUN ----------
[ -e /dev/net/tun ] || modprobe tun

# ---------- 4. clean old processes ----------
killall -9 tailscale 2>/dev/null
killall tailscaled 2>/dev/null
sleep 3
rm -f $SOCK

# ---------- 5. start tailscaled ----------
nohup $TSDIR/tailscaled \
    --state="$TS_STATE" \
    --socket=$SOCK \
    > $DAEMON_LOG 2>&1 &
echo "$(date) tailscaled launched, pid=$!" >> $LOG

# ---------- 6. wait for socket ----------
for i in $(seq 1 30); do
    [ -S $SOCK ] && { echo "$(date) socket ready after ${i}s" >> $LOG; break; }
    sleep 1
done
[ -S $SOCK ] || { echo "$(date) ERROR: socket not created" >> $LOG; tail -30 $DAEMON_LOG >> $LOG; }
sleep 2

# ---------- 7. tailscale up (args built from tsconfig) ----------
UP_ARGS="--accept-routes=$TS_ACCEPT_ROUTES --accept-dns=$TS_ACCEPT_DNS --timeout=30s"
[ -n "$TS_AUTHKEY" ]   && UP_ARGS="$UP_ARGS --authkey=$TS_AUTHKEY"
[ -n "$TS_ROUTES" ]    && UP_ARGS="$UP_ARGS --advertise-routes=$TS_ROUTES"
[ -n "$TS_EXIT_NODE" ] && UP_ARGS="$UP_ARGS --exit-node=$TS_EXIT_NODE --exit-node-allow-lan-access"
[ -n "$TS_HOSTNAME" ]  && UP_ARGS="$UP_ARGS --hostname=$TS_HOSTNAME"
# shellcheck disable=SC2086
$TSDIR/tailscale --socket=$SOCK up $UP_ARGS >> $LOG 2>&1
echo "$(date) tailscale up done (rc=$?)" >> $LOG

# ---------- 8. wait for interface ----------
for i in $(seq 1 20); do
    ip link show tailscale0 >/dev/null 2>&1 && break
    sleep 1
done
sleep 3

# ---------- 9. MSS clamp (avoid MTU blackhole) ----------
# -C before -A so re-running this script (not just rebooting) cannot stack
# duplicates. Note that on a device where fw3 files tailscale0 under the wan
# zone you will also see fw3's own --clamp-mss-to-pmtu pair here; those are not
# ours and must not be counted as duplicates.
iptables -t mangle -C OUTPUT  -o tailscale0 -p tcp --tcp-flags SYN,RST SYN -j TCPMSS --set-mss 1180 2>/dev/null || \
iptables -t mangle -A OUTPUT  -o tailscale0 -p tcp --tcp-flags SYN,RST SYN -j TCPMSS --set-mss 1180 2>/dev/null
iptables -t mangle -C FORWARD -o tailscale0 -p tcp --tcp-flags SYN,RST SYN -j TCPMSS --set-mss 1180 2>/dev/null || \
iptables -t mangle -A FORWARD -o tailscale0 -p tcp --tcp-flags SYN,RST SYN -j TCPMSS --set-mss 1180 2>/dev/null
iptables -t mangle -C FORWARD -i tailscale0 -p tcp --tcp-flags SYN,RST SYN -j TCPMSS --set-mss 1180 2>/dev/null || \
iptables -t mangle -A FORWARD -i tailscale0 -p tcp --tcp-flags SYN,RST SYN -j TCPMSS --set-mss 1180 2>/dev/null

# ---------- 9a. LAN <-> tailnet forwarding (subnet-router mode) ----------
iptables -C FORWARD -i "$TS_LAN_IF" -o tailscale0 -j ACCEPT 2>/dev/null || \
    iptables -I FORWARD 1 -i "$TS_LAN_IF" -o tailscale0 -j ACCEPT
iptables -C FORWARD -i tailscale0 -o "$TS_LAN_IF" -j ACCEPT 2>/dev/null || \
    iptables -I FORWARD 1 -i tailscale0 -o "$TS_LAN_IF" -j ACCEPT

# ---------- 9b. SNAT LAN -> tailnet ----------
# Peers that run WITHOUT --accept-routes (e.g. the Aliyun subnet routers) have
# no route back to our LAN prefix, so replies to 192.168.x.y leave via their
# default gateway and die. Masquerade LAN clients behind our tailnet IP instead
# (the device's own tailnet IP is always reachable). Costs source visibility only.
if [ "$TS_SNAT" = "true" ]; then
    iptables -t nat -C POSTROUTING -o tailscale0 -j MASQUERADE 2>/dev/null || \
        iptables -t nat -A POSTROUTING -o tailscale0 -j MASQUERADE
fi

# ---------- 9b'. SNAT tailnet -> LAN ----------
# tailscaled would masquerade subnet-route traffic itself (ts-forward marks it,
# ts-postrouting masquerades on the mark), but our 9a ACCEPT sits at FORWARD 1
# and lets tailnet->LAN packets through before ts-forward ever sees them, so
# the mark is never set and LAN clients receive the raw 100.x source. Devices
# that only answer their own subnet (e.g. a KVM on the LAN was the
# first) then drop every packet, while the router's own address still works.
# Masquerade behind the LAN address so every client sees $TS_LAN_IF's IP.
# Inserted at 1 so it runs before fw3's zone chains. The nat table cannot
# match -i, hence the source-prefix match.
if [ "$TS_SNAT" = "true" ]; then
    iptables -t nat -C POSTROUTING -s 100.64.0.0/10 -o "$TS_LAN_IF" -j MASQUERADE 2>/dev/null || \
        iptables -t nat -I POSTROUTING 1 -s 100.64.0.0/10 -o "$TS_LAN_IF" -j MASQUERADE
fi

# ---------- 9c. steer the tailnet around mwan3's policy routing ----------
# Only the TopFlow runs mwan3. Its mwan3_hook marks connections 0xd00/0xe00 and
# stores the mark in conntrack, so packets to/from the tailnet get routed out a
# cellular interface with a 100.x source address and are dropped upstream;
# outbound they hit `fwmark … unreachable` and surface as EPERM. mwan3's own
# rules sit at priority 2013+, so ours must come first.
#
# Do NOT use `lookup 52` (tailscale's own table) as the primary: it is emptied
# from outside, and once empty the rule falls through, traffic drops back to the
# cellular path, and the symptom is `tailscale ping` working while every TCP
# connection times out. Table 152 is ours and holds one aggregate route.
ts_mwan3_wanted() {
    case "$TS_MWAN3_BYPASS" in
        true)  return 0 ;;
        false) return 1 ;;
        *)     ip rule show 2>/dev/null | grep -qE 'fwmark 0x(d|e)00' ;;
    esac
}
if ts_mwan3_wanted && ip link show tailscale0 >/dev/null 2>&1; then
    ip route replace 100.64.0.0/10 dev tailscale0 table 152 2>/dev/null
    ip rule del to 100.64.0.0/10 lookup 152 2>/dev/null
    ip rule add to 100.64.0.0/10 lookup 152 priority 999 2>/dev/null
    # Keep 52 as the fallback: it carries subnet routes other nodes advertise.
    ip rule del to 100.64.0.0/10 lookup 52 2>/dev/null
    ip rule add to 100.64.0.0/10 lookup 52 priority 1000 2>/dev/null
    echo "$(date) tailnet bypass: table 152 via tailscale0 (prio 999), 52 at 1000" >> $LOG
fi

# ---------- 9d. tailnet SSH (separate instance bound to the tailnet IP) ----------
# dropbear starts before this script in rc.local, when tailscale0 does not exist
# yet, so the LAN instance cannot bind the tailnet address. Add one that can,
# reusing the same host keys (no host-key mismatch). The existing LAN instance is
# left alone so live sessions are not killed. Idempotent.
ts_ssh_wanted() {
    case "$TS_TAILNET_SSH" in
        true)  return 0 ;;
        false) return 1 ;;
        *)     [ -x /data/dropbear/bin/dropbear ] ;;
    esac
}
if ts_ssh_wanted; then
    TS_IP="$($TSDIR/tailscale --socket=$SOCK ip -4 2>/dev/null | head -1)"
    if [ -n "$TS_IP" ] && [ -x /data/dropbear/bin/dropbear ]; then
        if netstat -tln 2>/dev/null | grep -q "$TS_IP:22 "; then
            echo "$(date) tailnet dropbear already listening on $TS_IP:22" >> $LOG
        else
            /data/dropbear/bin/dropbear -p "$TS_IP:22" \
                -r /data/dropbear/dropbear_ed25519_host_key \
                -r /data/dropbear/dropbear_rsa_host_key \
                -P /var/run/dropbear-tailnet.pid >> $LOG 2>&1
            echo "$(date) tailnet dropbear started on $TS_IP:22" >> $LOG
        fi
    fi
fi

# ---------- 10. drop stray priority-5270 rule ----------
# ZTE's stack leaves this behind on the U60 Pro; harmless where it is absent.
ip rule del priority 5270 2>/dev/null

# ---------- 11. log status ----------
echo "--- ip rule ---" >> $LOG
ip rule show >> $LOG 2>&1
echo "--- tailscale status ---" >> $LOG
$TSDIR/tailscale --socket=$SOCK status >> $LOG 2>&1

# ---------- 12. start ShellCrash if installed ----------
if [ -f /etc/init.d/shellcrash ]; then
    echo "" >> $LOG
    echo "$(date) === checking ShellCrash ===" >> $LOG
    chmod +x /etc/ShellCrash/starts/*.sh /etc/ShellCrash/start.sh /etc/ShellCrash/init.sh 2>/dev/null
    if ps w | grep -E "CrashCore|sing-box" | grep -v grep > /dev/null; then
        echo "$(date) ShellCrash already running" >> $LOG
    else
        echo "$(date) ShellCrash not running, starting…" >> $LOG
        rm -f /etc/ShellCrash/.start_error
        rm -rf /tmp/ShellCrash/start_shellcrash.lock
        /etc/init.d/shellcrash start >> $LOG 2>&1
        for i in $(seq 1 30); do
            if ps w | grep -E "CrashCore|sing-box" | grep -v grep > /dev/null; then
                echo "$(date) ShellCrash started after ${i}s" >> $LOG
                break
            fi
            sleep 1
        done
        if ! ps w | grep -E "CrashCore|sing-box" | grep -v grep > /dev/null; then
            echo "$(date) ERROR: ShellCrash failed to start" >> $LOG
            echo "--- ShellCrash log ---" >> $LOG
            cat /tmp/ShellCrash/ShellCrash.log 2>/dev/null >> $LOG
        fi
    fi
fi

echo "=== $(date) all done ===" >> $LOG
