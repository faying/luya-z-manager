#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# homemode-bootsafe.sh — boot-time safety reset for Home Mode.
#
# Invoked from /etc/rc.local at every boot. Home Mode persists the radio-off
# state to uci (so the daemon reload picks it up), which means a reboot taken
# while "at home" would otherwise come up with Wi-Fi still disabled. This forces
# Wi-Fi back to a known-good ON state on boot, then clears Home Mode's state so
# the cron worker re-evaluates from scratch. A reboot can never lock you out.
# ─────────────────────────────────────────────────────────────────────────────
sleep 8   # let ubus / zte_topsw_wlan settle after boot

# Same lock as zte-agent and u60-guard (wifi_radio.rs LOCK_FILE).
(
    flock 9
    echo $$ > /tmp/u60-wifi.lock
    uci set wireless.wifi0.disabled=0
    uci set wireless.wifi1.disabled=0
    uci set wireless.main_2g.macfilter=deny
    uci set wireless.main_5g.macfilter=deny
    uci -q delete wireless.main_2g.maclist
    uci -q delete wireless.main_5g.maclist
    uci commit wireless
    ubus call zwrt_wlan reload >/dev/null 2>&1
) 9>>/tmp/u60-wifi.lock

rm -f /data/homemode/state
echo "$(date '+%Y-%m-%dT%H:%M:%S%z') [bootsafe] Wi-Fi forced ON, home-mode state cleared" >> /data/log/homemode.log
