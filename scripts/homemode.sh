#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# homemode.sh — "Home Mode" Wi-Fi auto-off for the ZTE U60 Pro
#
# Goal: when the device sees a known *home* Wi-Fi nearby, switch OFF the U60's
# own Wi-Fi so phones/laptops fall back to the (faster) home router instead of
# clinging to the U60. When the home Wi-Fi is gone (you left), turn it back on.
#
# Mechanism (validated on this firmware — qcacld32, OpenWrt 23.05.4):
#   OFF  = disable both radios via  uci wireless.wifiN.disabled=1 + zwrt_wlan reload
#          (daemon-safe: zte_topsw_wlan does NOT revert it; netdevs vanish so the
#           U60 network truly disappears and clients roam to home).
#   SCAN = while OFF we can't scan (radio down), so every CHECK_EVERY ticks we
#          briefly wake ONLY the 2.4G radio (wifi0) to scan, then put it back.
#          2.4G is enough — home APs always beacon on 2.4G — and it avoids the
#          5G DFS/CAC bring-up delay entirely.
#
# Driven by cron once per minute. State + debounce persisted under STATE_DIR.
# Safety: `touch DISABLE_FLAG` to suspend the whole thing; rc.local force-resets
# Wi-Fi to ON at every boot so a reboot can never leave you locked out.
# ─────────────────────────────────────────────────────────────────────────────

# ── Config ───────────────────────────────────────────────────────────────────
# Home SSIDs are read from $SSID_FILE (one per line; blank lines and #comments
# ignored), maintained via the admin UI / zte-agent. If the file is missing or
# empty we fall back to DEFAULT_SSIDS so the device is never left unguarded.
DEFAULT_SSIDS="MINWEI.CO SH.MINWEI.CO IOT.MINWEI.CO"  # any one present ⇒ "at home"
SCAN_IFACE="wlan0"          # 2.4G AP iface used for scanning
# 2.4G channels 1-11 ONLY. wlan0 and wlan2 share one phy (wcn7851), so a bare
# `iw scan` sweeps 2.4G+5G (~35 ch incl. DFS, ~5s) and takes BOTH radios
# off-channel: measured 100-130ms RTT spikes + loss on 5G clients every minute.
# Limiting to 2.4G takes ~1.4s and leaves 5G clients untouched (<=22ms).
SCAN_FREQS="2412 2417 2422 2427 2432 2437 2442 2447 2452 2457 2462"
RADIO_2G="wireless.wifi0.disabled"
RADIO_5G="wireless.wifi1.disabled"
CHECK_EVERY_DEFAULT=2       # while in home mode, scan every N cron ticks (~N min)
EXIT_MISSES_DEFAULT=2       # consecutive missed scans before declaring "left home"
WAKE_SETTLE=8               # seconds to let 2.4G come up before scanning

STATE_DIR=/data/homemode
STATE_FILE="$STATE_DIR/state"
SSID_FILE="$STATE_DIR/ssids"         # one SSID per line; managed by admin UI
CONFIG_FILE="$STATE_DIR/config"      # key=value tunables; managed by admin UI
DISABLE_FLAG="$STATE_DIR/disabled"   # touch to suspend home mode entirely
LOG=/data/log/homemode.log            # switch events: actual Wi-Fi on/off transitions
SCANLOG=/data/log/homemode-scan.log   # scan activity: the frequent periodic rechecks
LOG_MAX=1048576             # rotate (truncate) each log past ~1 MB

# Surviving sleep in home mode: the radios are OFF and (usually) no clients are
# attached, so after ~120s idle the ZTE sleep daemon (zte_topsw_sleep_faw)
# suspends the device — crond stops firing and this worker never runs, leaving
# Wi-Fi stuck OFF until the device spontaneously wakes hours later. (Verified:
# device deep-slept ~03:00–07:00; suspend_stats climbed; it only suspends in
# home mode, not normal mode where the AP is up.)
#
# Two independent defenses, either of which is sufficient — belt and suspenders
# because this has regressed before:
#  1. Ask the sleep daemon not to suspend, via its OWN ubus API. Writing
#     /sys/power/wake_lock does NOT work — that only gates kernel *autosleep*,
#     while the daemon forces suspend with an explicit `echo mem >
#     /sys/power/state` that ignores the wakelock set (this is why the previous
#     wakelock fix failed). enableAutoSleep is the daemon's master switch; the
#     stock ZTE daemons (zte_dm/data/wlan…) use this same API to inhibit sleep.
#  2. Arm an RTC wake alarm a little over one cron period out. If the device
#     suspends anyway, the alarm wakes it so the next cron tick runs and can
#     still notice home is gone. Re-armed every tick (while awake it keeps
#     getting pushed out and never fires); cleared in normal mode. Verified the
#     RTC accepts/holds an alarm and the daemon doesn't clobber it.
SLEEP_UBUS_OBJ=zwrt_zte_sleep_faw.wakelock
RTC_WAKEALARM=/sys/class/rtc/rtc0/wakealarm
RTC_SINCE_EPOCH=/sys/class/rtc/rtc0/since_epoch
WAKE_ALARM_SECS=75          # > 60s cron period, so a re-arm pre-empts it while awake

# ── Helpers ──────────────────────────────────────────────────────────────────
mkdir -p "$STATE_DIR" /data/log

# cfg <key> <default> — read an integer tunable from CONFIG_FILE (key=value).
# Extracts digits only (no sourcing), so the file can never inject commands.
cfg() {
    v=$(sed -n "s/^$1=\([0-9]\{1,\}\).*/\1/p" "$CONFIG_FILE" 2>/dev/null | head -1)
    echo "${v:-$2}"
}
CHECK_EVERY=$(cfg check_every "$CHECK_EVERY_DEFAULT")
EXIT_MISSES=$(cfg exit_misses "$EXIT_MISSES_DEFAULT")
# Clamp to sane ranges so a bad config can't wedge the worker.
[ "$CHECK_EVERY" -ge 1 ] 2>/dev/null || CHECK_EVERY=1
[ "$CHECK_EVERY" -le 60 ] 2>/dev/null || CHECK_EVERY=60
[ "$EXIT_MISSES" -ge 1 ] 2>/dev/null || EXIT_MISSES=1
[ "$EXIT_MISSES" -le 30 ] 2>/dev/null || EXIT_MISSES=30

# _logto <file> <message…> — timestamped append with size-based rotation.
_logto() {
    f="$1"; shift
    if [ -f "$f" ] && [ "$(wc -c < "$f" 2>/dev/null || echo 0)" -gt "$LOG_MAX" ]; then
        tail -c 262144 "$f" > "$f.tmp" 2>/dev/null && mv "$f.tmp" "$f"
        echo "$(date '+%Y-%m-%dT%H:%M:%S%z') [log rotated]" >> "$f"
    fi
    echo "$(date '+%Y-%m-%dT%H:%M:%S%z') $*" >> "$f"
}
# Wi-Fi state changes (rare, kept long) vs scan rechecks (frequent, separate file
# so they never push switch events out of view).
log_switch() { _logto "$LOG" "$@"; }
log_scan()   { _logto "$SCANLOG" "$@"; }

uci_get()   { uci -q get "$1"; }
radios_off() { [ "$(uci_get "$RADIO_2G")" = "1" ]; }
# both_off — true only when BOTH radios are disabled. Used to self-heal a
# half-on state (e.g. a scan or a racing cron left 2.4G awake) without an
# unnecessary reload when already fully off.
both_off() { [ "$(uci_get "$RADIO_2G")" = "1" ] && [ "$(uci_get "$RADIO_5G")" = "1" ]; }

apply_wifi() {
    # $1/$2 = desired disabled value for 2.4G / 5G ; commit + daemon reload
    uci set "$RADIO_2G=$1"
    uci set "$RADIO_5G=$2"
    uci commit wireless
    ubus call zwrt_wlan reload >/dev/null 2>&1
}

home_ssids() {
    # emit configured home SSIDs, one per line. A present file wins (even if
    # empty ⇒ "never trigger"); fall back to defaults only when it is ABSENT
    # (fresh install). '#' is only a comment at line start, so SSIDs may contain it.
    if [ -f "$SSID_FILE" ]; then
        grep -v '^[[:space:]]*#' "$SSID_FILE" | grep -v '^[[:space:]]*$' \
            | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//'
    else
        printf '%s\n' $DEFAULT_SSIDS
    fi
}

scan_home() {
    # echo the first home SSID found nearby, or nothing. Needs $SCAN_IFACE up.
    out=$(iw dev "$SCAN_IFACE" scan freq $SCAN_FREQS 2>/dev/null)
    [ -z "$out" ] && out=$(iw dev "$SCAN_IFACE" scan freq $SCAN_FREQS 2>/dev/null)  # one retry (radio busy)
    home_ssids | while IFS= read -r s; do
        [ -z "$s" ] && continue
        if echo "$out" | grep -qF "SSID: $s"; then
            echo "$s"
            break
        fi
    done
}

# state file format:  MODE TICK MISS    (MODE = normal|home)
read_state() {
    MODE=normal TICK=0 MISS=0
    if [ -f "$STATE_FILE" ]; then
        read -r MODE TICK MISS < "$STATE_FILE" 2>/dev/null
        [ -z "$MODE" ] && MODE=normal
        [ -z "$TICK" ] && TICK=0
        [ -z "$MISS" ] && MISS=0
    fi
}
# arm_wakealarm SECS — set the RTC to fire SECS from now (clears any pending
# alarm first, which the kernel requires before re-arming). Best effort.
arm_wakealarm() {
    [ -w "$RTC_WAKEALARM" ] || return 0
    now=$(cat "$RTC_SINCE_EPOCH" 2>/dev/null) || return 0
    [ -n "$now" ] || return 0
    echo 0 > "$RTC_WAKEALARM" 2>/dev/null
    echo $(( now + $1 )) > "$RTC_WAKEALARM" 2>/dev/null
}
clear_wakealarm() { [ -w "$RTC_WAKEALARM" ] && echo 0 > "$RTC_WAKEALARM" 2>/dev/null; return 0; }

# write_state — persist state AND reconcile both sleep defenses to the current
# MODE. Every exit path calls this, so they can never drift from the mode: in
# home mode inhibit sleep AND arm the RTC safety-net wake; in normal mode
# re-enable sleep and clear our alarm. Idempotent; all failures non-fatal.
write_state() {
    echo "$MODE $TICK $MISS" > "$STATE_FILE"
    if [ "$MODE" = "home" ]; then
        command -v ubus >/dev/null 2>&1 && \
            ubus call "$SLEEP_UBUS_OBJ" enableAutoSleep '{"switch":false}' >/dev/null 2>&1
        arm_wakealarm "$WAKE_ALARM_SECS"
    else
        command -v ubus >/dev/null 2>&1 && \
            ubus call "$SLEEP_UBUS_OBJ" enableAutoSleep '{"switch":true}' >/dev/null 2>&1
        clear_wakealarm
    fi
}

# ── Main ─────────────────────────────────────────────────────────────────────
read_state

# Suspend switch — restore Wi-Fi and do nothing.
if [ -f "$DISABLE_FLAG" ]; then
    if radios_off; then
        apply_wifi 0 0
        log_switch "SUSPEND flag present → Wi-Fi restored ON; home mode idle"
    fi
    MODE=normal TICK=0 MISS=0
    write_state
    exit 0
fi

if [ "$MODE" != "home" ]; then
    # ── NORMAL: radios on, U60 serving. Watch for arriving home. ──────────────
    found=$(scan_home)
    if [ -n "$found" ]; then
        apply_wifi 1 1
        MODE=home TICK=0 MISS=0
        log_switch "HOME detected ($found) → Wi-Fi OFF (both radios disabled)"
    fi
    write_state
    exit 0
fi

# ── HOME MODE: radios off. Periodically wake 2.4G to check if we left. ────────
TICK=$((TICK + 1))
if [ "$TICK" -lt "$CHECK_EVERY" ]; then
    write_state
    exit 0
fi
TICK=0

# Safety: if something external re-enabled the radios, just re-assert OFF state
# is not desirable here — instead wake 2.4G cleanly for the scan.
woke=0
if radios_off; then
    apply_wifi 0 1          # wake 2.4G only (5G stays off → no DFS)
    woke=1
    sleep "$WAKE_SETTLE"
fi

found=$(scan_home)
if [ -n "$found" ]; then
    # still home → ensure BOTH radios are off. Unconditional (not gated on
    # `woke`) so it self-heals if the scan wake — or a racing second cron —
    # left 2.4G enabled; both_off skips the reload when already fully off.
    MISS=0
    both_off || apply_wifi 1 1
    log_scan "still HOME ($found) → stay OFF"
else
    MISS=$((MISS + 1))
    if [ "$MISS" -ge "$EXIT_MISSES" ]; then
        apply_wifi 0 0
        MODE=normal TICK=0 MISS=0
        log_switch "HOME gone (${EXIT_MISSES} misses) → Wi-Fi ON, back to normal"
    else
        both_off || apply_wifi 1 1
        log_scan "HOME not seen (miss $MISS/$EXIT_MISSES) → stay OFF, recheck soon"
    fi
fi
write_state
