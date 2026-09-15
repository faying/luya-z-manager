#!/bin/sh
# Cross-reboot device monitor — samples every 10s, appends to /data/log/monitor.log
LOG=/data/log/monitor.log
FORENSIC_DIR=/data/log/forensic
mkdir -p /data/log "$FORENSIC_DIR"

# Re-assert full ramdump after boot (zte_topsw_mc resets dload_mode to "mini" via UCI=2)
echo full > /sys/kernel/dload/dload_mode 2>/dev/null
echo 1    > /sys/kernel/dload/emmc_dload 2>/dev/null

RCAUSE=$(cat /proc/zte_bsp_rebootcause 2>/dev/null)
UP0=$(awk '{print int($1)}' /proc/uptime)
echo "$(date -Iseconds) === BOOT === reboot_cause=$RCAUSE uptime_at_start=$UP0" >> "$LOG"

# Seed PREV_* from current state so first iteration doesn't false-positive
PREV_SSR=$(awk '/Modem out of reset/' /data/ssr_kpi/ssr_kpi.txt 2>/dev/null | wc -l)
PREV_STA5G=$(iw dev wlan2 station dump 2>/dev/null | grep -c '^Station')
PREV_STA2G=$(iw dev wlan0 station dump 2>/dev/null | grep -c '^Station')
PREV_TEMP_INT=0
LAST_FORENSIC=0

snapshot() {
  REASON="$1"
  NOW=$(date +%s)
  # rate-limit: at most once per 60s
  [ $((NOW - LAST_FORENSIC)) -lt 60 ] && return
  LAST_FORENSIC=$NOW
  F="$FORENSIC_DIR/forensic-$(date +%Y%m%d-%H%M%S)-${REASON}.txt"
  {
    echo "=== forensic snapshot: $REASON ==="
    echo "=== date $(date -Iseconds) uptime $(awk '{print $1}' /proc/uptime)s ==="
    echo "=== /proc/loadavg ==="; cat /proc/loadavg
    echo "=== /proc/meminfo (top) ==="; head -10 /proc/meminfo
    echo "=== thermal zones ==="
    for z in /sys/class/thermal/thermal_zone*/; do
      n=$(cat "$z/type" 2>/dev/null); t=$(cat "$z/temp" 2>/dev/null)
      [ -n "$n" ] && echo "$n=$((t/1000))C"
    done | sort -t= -k2 -n -r | head -15
    echo "=== ps top ==="; ps w | head -30
    echo "=== wlan stations ==="
    for w in wlan0 wlan2; do
      echo "--- $w ---"
      iw dev $w station dump 2>/dev/null | head -40
    done
    echo "=== wlan debugfs ==="
    ls /sys/kernel/debug/wlan/ 2>/dev/null
    echo "=== ssr_kpi tail ==="; tail -10 /data/ssr_kpi/ssr_kpi.txt 2>/dev/null
    echo "=== dmesg tail ==="; dmesg 2>/dev/null | tail -150
    echo "=== modem status ==="
    ubus -t 2 call zwrt_data get_wwaniface '{"source_module":"zte_topsw_data","cid":1}' 2>/dev/null
    echo "=== ip route ==="; ip route show
    echo "--- table 52 ---"; ip route show table 52
  } > "$F" 2>&1
  echo "$(date -Iseconds) ★ FORENSIC saved $F ($REASON)" >> "$LOG"
}

while true; do
  TS=$(date -Iseconds)
  UPS=$(awk '{print int($1)}' /proc/uptime)
  SSR=$(awk '/Modem out of reset/' /data/ssr_kpi/ssr_kpi.txt 2>/dev/null | wc -l)
  STA2G=$(iw dev wlan0 station dump 2>/dev/null | grep -c '^Station')
  STA5G=$(iw dev wlan2 station dump 2>/dev/null | grep -c '^Station')
  VBATT=$(cat /sys/class/power_supply/battery/voltage_now 2>/dev/null)
  PCT=$(cat /sys/class/power_supply/battery/capacity 2>/dev/null)
  BSTAT=$(cat /sys/class/power_supply/battery/status 2>/dev/null | tr -d '\n')
  TMAX=$(cat /sys/class/thermal/thermal_zone*/temp 2>/dev/null | awk 'BEGIN{m=0} {if($1>m)m=$1} END{printf "%.1f", m/1000}')
  TMAX_INT=$(echo "$TMAX" | cut -d. -f1)
  MDM=$(ubus -t 2 call zwrt_data get_wwaniface '{"source_module":"zte_topsw_data","cid":1}' 2>/dev/null | jsonfilter -e '@.connect_status' 2>/dev/null)
  NETINFO=$(ubus -t 2 call zte_nwinfo_api nwinfo_get_netinfo 2>/dev/null)
  NETTYPE=$(echo "$NETINFO" | jsonfilter -e '@.network_type' 2>/dev/null)
  BAR=$(echo "$NETINFO" | jsonfilter -e '@.signalbar' 2>/dev/null)
  if [ "$NETTYPE" = "LTE" ]; then
    RSRP=$(echo "$NETINFO" | jsonfilter -e '@.lte_rsrp' 2>/dev/null)
    SNR=$(echo "$NETINFO" | jsonfilter -e '@.lte_snr' 2>/dev/null)
  else
    RSRP=$(echo "$NETINFO" | jsonfilter -e '@.nr5g_rsrp' 2>/dev/null)
    SNR=$(echo "$NETINFO" | jsonfilter -e '@.nr5g_snr' 2>/dev/null)
  fi

  DSSR=$((SSR - PREV_SSR))
  D5G=$((STA5G - PREV_STA5G))
  D2G=$((STA2G - PREV_STA2G))
  EV=""
  [ "$DSSR" -gt 0 ] && EV="$EV modem_crashed+$DSSR"
  [ "$D5G" -gt 0 ] && EV="$EV sta5g_joined+$D5G"
  [ "$D5G" -lt 0 ] && EV="$EV sta5g_left$D5G"
  [ "$D2G" -gt 0 ] && EV="$EV sta2g_joined+$D2G"
  [ "$D2G" -lt 0 ] && EV="$EV sta2g_left$D2G"

  echo "$TS,up=${UPS}s,ssr=$SSR,sta2g=$STA2G,sta5g=$STA5G,vbatt=$VBATT,pct=$PCT,bstat=$BSTAT,temp=${TMAX}C,mdm=$MDM,net=$NETTYPE,bar=$BAR,rsrp=$RSRP,snr=$SNR$([ -n "$EV" ] && echo " ★$EV")" >> "$LOG"

  # Danger triggers — capture forensic snapshot before the device dies
  if [ "$TMAX_INT" -ge 58 ]; then
    snapshot "hot${TMAX_INT}C"
  fi
  # Sudden mass disconnect: >=2 clients dropped this sample
  ABS_D5G=$D5G; [ "$D5G" -lt 0 ] && ABS_D5G=$((-D5G))
  ABS_D2G=$D2G; [ "$D2G" -lt 0 ] && ABS_D2G=$((-D2G))
  if [ "$D5G" -lt 0 ] || [ "$D2G" -lt 0 ]; then
    TOTAL_DROP=$((ABS_D5G + ABS_D2G))
    [ "$TOTAL_DROP" -ge 2 ] && snapshot "mass_drop${TOTAL_DROP}"
  fi
  # Modem SSR (only after early boot settles)
  if [ "$DSSR" -gt 0 ] && [ "$UPS" -gt 120 ]; then
    snapshot "ssr+${DSSR}"
  fi

  PREV_SSR=$SSR
  PREV_STA5G=$STA5G
  PREV_STA2G=$STA2G
  PREV_TEMP_INT=$TMAX_INT
  sleep 10
done
