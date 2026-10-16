#!/bin/sh
# Boot chores for the U60 Pro (MU5250) screen UI. Installed at
# /data/plugins/u60pro-devui/start.sh.
#
# The screen belongs to u60-uid (u60-uid.init; both retired, now luyaz-sentinel): it starts, restarts and gives
# up the UI. u60-uid runs `start.sh prep` once when it starts; all this script
# does is append the boot conditions to boot-trace.log.
#
# Called with no argument (the old rc.local hook, and install-autostart.sh at
# the end of an install) it does the same chores and starts nothing: the UI
# and zwrt-datad are started by their procd services. The old `legacy` and
# `procd` launch modes (nohup UI, .fallback startup guard, corner-wake, a
# nohup zwrt-datad) are gone with u60pro-devui.init and corner-wake.
#
# SPDX-License-Identifier: MIT
DEVUI_DIR=/data/plugins/u60pro-devui
MODE="${1:-legacy}"

mkdir -p "$DEVUI_DIR"

read_mode_main_state() {
    awk -F"'" '/option mode_main_state/ { print $2; exit }' /etc/config/zwrt_zte_mc_tmp 2>/dev/null
}

read_reboot_reason_code() {
    awk -F"'" '/option reboot_reason_code/ { print $2; exit }' /etc/config/zwrt_zte_mc_tmp 2>/dev/null
}

boot_trace() {
    LOG="$DEVUI_DIR/boot-trace.log"
    {
        echo "=== $(date '+%Y-%m-%d %H:%M:%S') ==="
        echo "mode_main_state=$mode_main_state"
        echo "reboot_reason_code=$reboot_reason_code"
        echo "bootmode=$BOOTMODE"
        echo -n "cmdline="
        cat /proc/cmdline 2>/dev/null
        for f in \
            /sys/class/power_supply/usb/online \
            /sys/class/power_supply/usb/voltage_now \
            /sys/class/power_supply/battery/status \
            /sys/class/power_supply/battery/capacity \
            /sys/class/power_supply/charger_zte/present_mbb \
            /sys/class/power_supply/charger_zte/status_mbb \
            /sys/class/power_supply/type-c_zte/present_mbb \
            /sys/class/power_supply/type-c_zte/real_type_mbb \
            /sys/class/power_supply/statistics_zte/batt_status \
            /sys/class/power_supply/statistics_zte/batt_online \
            /sys/class/power_supply/battery_zte/status_mbb \
            /sys/class/power_supply/battery_zte/online_mbb
        do
            [ -e "$f" ] && echo "$f=$(cat "$f" 2>/dev/null)"
        done
        for e in /dev/input/event*; do
            [ -e "$e" ] || continue
            echo "$e=$(cat /sys/class/input/${e##*/}/device/name 2>/dev/null)"
        done
        echo
    } >> "$LOG"
    tail -n 160 "$LOG" > "$LOG.tmp" 2>/dev/null && mv "$LOG.tmp" "$LOG"
}

# Power-off charging boots do not expose silent_boot.mode=nonsilent.
BOOTMODE=charge
mode_main_state="$(read_mode_main_state)"
reboot_reason_code="$(read_reboot_reason_code)"
case "$mode_main_state" in
    mode_power_off_*) BOOTMODE=charge ;;
    mode_power_on|mode_power_on_charger) BOOTMODE=normal ;;
    *)
        if grep -q 'silent_boot.mode=nonsilent' /proc/cmdline 2>/dev/null; then
            BOOTMODE=normal
        fi
        ;;
esac
boot_trace

case "$MODE" in
    prep) ;;
    legacy|procd)
        echo "start.sh: the screen belongs to u60-uid (/etc/init.d/u60-uid); boot chores only" >&2
        ;;
    *)
        echo "usage: $0 [prep]" >&2
        exit 2
        ;;
esac
exit 0
