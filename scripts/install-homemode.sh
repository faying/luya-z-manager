#!/bin/bash
# ─────────────────────────────────────────────────────────────────────────────
# install-homemode.sh — deploy the Home Mode Wi-Fi auto-off worker to the U60.
#
#   DEVICE=192.168.0.1 ./scripts/install-homemode.sh
#
# Pushes homemode.sh + homemode-bootsafe.sh, installs a 1-minute cron entry,
# and wires the boot-safety reset into /etc/rc.local. Idempotent — safe to
# re-run after editing the scripts.
#
# Uninstall:  ./scripts/install-homemode.sh uninstall
#
# SUPERSEDED by the scenario engine (zte-agent/src/scenario.rs), which does the
# same job with real verification instead of assuming `ubus call zwrt_wlan
# reload` worked — it does not, reliably. The two MUST NOT run together: this
# worker writes wireless.wifiN.disabled from cron every minute while the engine
# writes wireless.main_Ng.disabled from its own tick, and each would see the
# other's edits as the user taking over. The guard below refuses to install
# while a configured engine is present.
# ─────────────────────────────────────────────────────────────────────────────
set -e

DEVICE="${DEVICE:-192.168.0.1}"
SSH_PORT="${SSH_PORT:-2222}"
SSH="ssh -p $SSH_PORT -o StrictHostKeyChecking=no root@$DEVICE"
DIR="$(cd "$(dirname "$0")" && pwd)"
CRON_LINE="* * * * * /data/homemode.sh"
RC_LINE="sh /data/homemode-bootsafe.sh &"

# Refuse to add a second writer of the wireless config. Checked on the device
# rather than locally, because that is where the conflict would actually happen.
if [ "$1" != "uninstall" ]; then
    if $SSH 'test -s /data/scenario/scenarios.json && grep -q "\"scenarios\"[[:space:]]*:[[:space:]]*\[[[:space:]]*{" /data/scenario/scenarios.json' 2>/dev/null; then
        echo "REFUSING: the scenario engine is configured on $DEVICE." >&2
        echo "  It already manages Wi-Fi per scenario, and running this cron worker" >&2
        echo "  alongside it means two processes fighting over the same uci keys." >&2
        echo "  Turn the engine off first (PUT /api/scenario/enabled {\"enabled\":false})" >&2
        echo "  or clear its config, then re-run this." >&2
        exit 1
    fi
fi

if [ "$1" = "uninstall" ]; then
    echo "Uninstalling home mode from $DEVICE ..."
    $SSH '
        sed -i "\#/data/homemode.sh#d" /etc/crontabs/root 2>/dev/null
        sed -i "\#homemode-bootsafe#d" /etc/rc.local 2>/dev/null
        /etc/init.d/cron restart 2>/dev/null
        # restore Wi-Fi to a known-good ON state, under the shared Wi-Fi lock
        flock /tmp/u60-wifi.lock -c "
        uci set wireless.wifi0.disabled=0; uci set wireless.wifi1.disabled=0
        uci set wireless.main_2g.macfilter=deny; uci set wireless.main_5g.macfilter=deny
        uci -q delete wireless.main_2g.maclist; uci -q delete wireless.main_5g.maclist
        uci commit wireless; ubus call zwrt_wlan reload >/dev/null 2>&1"
        rm -f /data/homemode.sh /data/homemode-bootsafe.sh
        rm -rf /data/homemode
        echo "  removed scripts, cron entry, rc.local hook; Wi-Fi restored ON"
    '
    echo "Done."
    exit 0
fi

echo "Deploying home-mode scripts to $DEVICE ..."
cat "$DIR/homemode.sh"          | $SSH "cat > /data/homemode.sh && chmod +x /data/homemode.sh"
cat "$DIR/homemode-bootsafe.sh" | $SSH "cat > /data/homemode-bootsafe.sh && chmod +x /data/homemode-bootsafe.sh"

echo "Installing cron entry + boot hook ..."
$SSH "
    touch /etc/crontabs/root
    grep -qF '/data/homemode.sh' /etc/crontabs/root || echo '$CRON_LINE' >> /etc/crontabs/root
    /etc/init.d/cron restart >/dev/null 2>&1 || killall -HUP crond 2>/dev/null

    if ! grep -qF 'homemode-bootsafe' /etc/rc.local; then
        if grep -q '^exit 0' /etc/rc.local; then
            sed -i '/^exit 0/i $RC_LINE' /etc/rc.local
        else
            echo '$RC_LINE' >> /etc/rc.local
        fi
    fi
    mkdir -p /data/homemode /data/log
"

echo "Done. Worker runs every minute; logs to /data/log/homemode.log on the device."
echo "Suspend any time with:  ssh -p $SSH_PORT root@$DEVICE 'touch /data/homemode/disabled'"
echo "Resume with:            ssh -p $SSH_PORT root@$DEVICE 'rm /data/homemode/disabled'"
