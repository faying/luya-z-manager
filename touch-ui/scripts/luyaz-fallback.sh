#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# luyaz-fallback.sh — the one emergency direct write on the U60 Pro (MU5250),
# for when luyaz-core (the only writer, E4) is really gone. Callers use it
# instead of running ubus themselves
# (write-op-layer.md D18, D23, D29).
#
#     luyaz-fallback.sh [--by SOURCE] ACTION [key=value ...]
#
#   cellular.set             enabled=0|1 roaming=0|1 connect_mode=N  (one or more)
#   network.set_mode         mode=WORD
#   band.set_lte             bands=1,3,28
#   band.set_nr_sa           bands=78
#   band.set_nr_nsa          bands=78
#   band.reset
#   cell.lock_lte            pci=N earfcn=N
#   cell.lock_nr             pci=N arfcn=N band=N
#   apn.set_mode             mode=0|1                (0 auto, 1 manual)
#   apn.enable               profile_id=WORD
#   netselect.auto           (AT+COPS=0: back to automatic network selection)
#   modem.online             (AT+CFUN=1: radio on)
#   wifi.radio               ap_2g=0|1 ap_5g=0|1     (both AP interfaces, guests off; always written and
#                                                    reloaded; ap_2g=0 ap_5g=1 refused)
#   cellular.redial          [type=1|2]              (dial the data call again; both without type)
#   nfc.set                  enabled=0|1 [flag=N]
#   power.direct_supply.set  enabled=0|1|true|false
#
# SOURCE is who asked (screen, web, …; default screen). It goes into the
# takeover marker only.
#
# Order, all under the cross-process write lock datad also takes:
#   1. arguments checked; the ubus JSON is built here from checked values only
#      (callers pass user input through system(), so this is the boundary).
#      Anything off → exit 2, nothing touched.
#   2. lock: flock on $LUYAZ_WRITE_LOCK (/var/run/luyaz-write.lock), tried
#      once a second for $LUYAZ_FALLBACK_LOCK_WAIT (10) s → else exit 4.
#   3. datad gone? Twice, $LUYAZ_FALLBACK_GAP (1) s apart. Alive when the pid in
#      $LUYAZ_PID_FILE (/var/run/luyaz-core.pid) has a comm starting with "luyaz-core" (side-by-side
#      names like luyaz-core.test count; so does the old "zwrt-datad"), or when its /healthz answers at all,
#      503 included (a datad that writes no pid file). Alive either time → exit
#      3, nothing written.
#   4. reads the write needs (cellular.set: get_wwaniface; direct supply: the
#      current mode, already there → exit 0 with nothing written).
#   5. one JSON line appended to $LUYAZ_OPS_DIR/takeover (/data/u60-ops),
#      then sync. datad, on its next start, sees it under the same lock and
#      drops the change it had pending (it would otherwise roll this one back).
#      No marker, no write: cannot append → exit 1.
#   6. the write, the same ubus calls datad's /control makes for the action
#      (control.rs). cellular.set reads get_wwaniface and changes only the
#      named fields, like datad: the old 4-field fallback dropped the rest.
#      Runs in the foreground; callers background the whole script.
#
# Exit: 0 written · 1 ubus/marker failed · 2 bad arguments · 3 datad is
# running, not written · 4 write lock busy (being taken over), not written.
# One line on stdout says which.
#
# AT commands take the AT port's own lock first ($LUYAZ_AT_LOCK,
# /var/run/luyaz-at.lock, shared with luyaz-core: two readers on one
# tty take each other's replies), up to 15 s.
#
# Test hooks: LUYAZ_FALLBACK_UCI (uci), LUYAZ_FALLBACK_AT (a command given the AT text, prints the
# modem's answer; default: the port, as the core does it), LUYAZ_FALLBACK_UBUS (ubus), LUYAZ_FALLBACK_WGET (wget),
# LUYAZ_FALLBACK_HEALTH_URL (empty = no /healthz check), LUYAZ_FALLBACK_PROC
# (/proc), LUYAZ_FALLBACK_LOCK_WAIT, LUYAZ_FALLBACK_GAP, LUYAZ_FALLBACK_VERIFY_GAP.
# Tests: scripts/test/luyaz-fallback/run.sh (busybox image).
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────

UBUS=${LUYAZ_FALLBACK_UBUS:-ubus}
UCI=${LUYAZ_FALLBACK_UCI:-uci}
WGET=${LUYAZ_FALLBACK_WGET:-wget}
HEALTH=${LUYAZ_FALLBACK_HEALTH_URL-http://127.0.0.1:9460/healthz}
PROC=${LUYAZ_FALLBACK_PROC:-/proc}
LOCK=${LUYAZ_WRITE_LOCK-/var/run/luyaz-write.lock}
ATLOCK=${LUYAZ_AT_LOCK-/var/run/luyaz-at.lock} # both u60-*.lock before Luya-Z rename E2
ATHOOK=${LUYAZ_FALLBACK_AT:-}
# Luya-Z rename D2: the core's pid file (before: /var/run/zwrt-datad.pid; a list is taken, a
# stale entry names no live core). The ops dir: the new one once it was moved (a kit install
# keeps the old one).
PIDF=${LUYAZ_PID_FILE-/var/run/luyaz-core.pid}
if [ -d /data/luyaz/core/ops ]; then OPS=${LUYAZ_OPS_DIR-/data/luyaz/core/ops}
else OPS=${LUYAZ_OPS_DIR-/data/u60-ops}; fi

# Numbers only from here on: busybox ash dies on a bad number in $(( )).
num() { case $1 in '' | *[!0-9]*) echo "$2" ;; *) echo "$1" ;; esac; }
WAIT=$(num "${LUYAZ_FALLBACK_LOCK_WAIT:-}" 10)
GAP=$(num "${LUYAZ_FALLBACK_GAP:-}" 1)
VGAP=$(num "${LUYAZ_FALLBACK_VERIFY_GAP:-}" 1)

done_() { echo "$2"; exit "$1"; }
invalid() { done_ 2 "invalid: $*"; }
failed() { done_ 1 "failed: $*"; }

is01() { case $1 in 0 | 1) return 0 ;; esac; return 1; }
isint() { case $1 in '' | *[!0-9]*) return 1 ;; esac; [ ${#1} -le 9 ]; }
isword() { case $1 in '' | *[!A-Za-z0-9_]*) return 1 ;; esac; [ ${#1} -le 32 ]; }
isbands() {
    case $1 in '' | *[!0-9,]*) return 1 ;; esac
    case $1 in *[0-9]*) ;; *) return 1 ;; esac
    [ ${#1} -le 200 ]
}

# Top-level fields of one JSON object (ubus replies are pretty-printed).
#   mode=get   key=K        print the value of K (strings without quotes); exit 1 if absent
#   mode=merge ov="k=v;…"   print the object with each k set to the raw JSON v
#                           (replaced where present, appended otherwise);
#                           an empty v ("k=") drops k
# Nested objects/arrays and escaped strings pass through untouched; not an
# object → exit 1.
# shellcheck disable=SC2016 # awk program, not shell
JSON_AWK='
function trim(x) { sub(/^[ \t\r\n]+/, "", x); sub(/[ \t\r\n]+$/, "", x); return x }
function piece(p,    i, c, L, esc) {
    p = trim(p)
    if (p == "") return
    if (substr(p, 1, 1) != "\"") { bad = 1; return }
    L = length(p); esc = 0
    for (i = 2; i <= L; i++) {
        c = substr(p, i, 1)
        if (esc) esc = 0
        else if (c == "\\") esc = 1
        else if (c == "\"") break
    }
    n++
    K[n] = substr(p, 1, i)
    v = trim(substr(p, i + 1))
    if (substr(v, 1, 1) != ":") { bad = 1; return }
    V[n] = trim(substr(v, 2))
}
{ s = s $0 "\n" }
END {
    s = trim(s); L = length(s); n = 0; bad = 0
    if (substr(s, 1, 1) != "{" || substr(s, L, 1) != "}") exit 1
    depth = 0; instr = 0; esc = 0; start = 2
    for (i = 1; i <= L; i++) {
        c = substr(s, i, 1)
        if (instr) {
            if (esc) esc = 0
            else if (c == "\\") esc = 1
            else if (c == "\"") instr = 0
            continue
        }
        if (c == "\"") { instr = 1; continue }
        if (c == "{" || c == "[") { depth++; continue }
        if (c == "}" || c == "]") {
            if (depth == 1) piece(substr(s, start, i - start))
            depth--
            continue
        }
        if (depth == 1 && c == ",") { piece(substr(s, start, i - start)); start = i + 1 }
    }
    if (depth != 0 || instr || bad) exit 1
    if (mode == "get") {
        for (i = 1; i <= n; i++)
            if (K[i] == "\"" key "\"") {
                v = V[i]
                if (substr(v, 1, 1) == "\"") v = substr(v, 2, length(v) - 2)
                print v
                exit 0
            }
        exit 1
    }
    m = split(ov, o, ";")
    for (j = 1; j <= m; j++) {
        eq = index(o[j], "=")
        k = "\"" substr(o[j], 1, eq - 1) "\""
        v = substr(o[j], eq + 1)
        hit = 0
        for (i = 1; i <= n; i++) if (K[i] == k) { V[i] = v; hit = 1; if (v == "") K[i] = "" }
        if (!hit && v != "") { n++; K[n] = k; V[n] = v }
    }
    out = "{"; sep = ""
    for (i = 1; i <= n; i++) if (K[i] != "") { out = out sep K[i] ":" V[i]; sep = "," }
    print out "}"
}'
json() { awk -v mode="$1" -v key="$2" -v ov="$3" "$JSON_AWK"; }

# ── 1. arguments ──────────────────────────────────────────────────────────────
BY=screen
case ${1:-} in
    --by)
        [ $# -ge 2 ] || invalid "--by needs a value"
        BY=$2
        shift 2
        ;;
    --by=*)
        BY=${1#--by=}
        shift
        ;;
esac
isword "$BY" || invalid "bad source"
[ $# -ge 1 ] || invalid "no action"
ACTION=$1
shift
case $ACTION in
    cellular.set) KEYS="enabled roaming connect_mode" ;;
    network.set_mode) KEYS="mode" ;;
    band.set_lte | band.set_nr_sa | band.set_nr_nsa) KEYS="bands" ;;
    band.reset) KEYS="" ;;
    cell.lock_lte) KEYS="pci earfcn" ;;
    cell.lock_nr) KEYS="pci arfcn band" ;;
    apn.set_mode) KEYS="mode" ;;
    apn.enable) KEYS="profile_id" ;;
    netselect.auto | modem.online) KEYS="" ;;
    cellular.redial) KEYS="type" ;;
    wifi.radio) KEYS="ap_2g ap_5g" ;;
    nfc.set) KEYS="enabled flag" ;;
    power.direct_supply.set) KEYS="enabled" ;;
    *) invalid "unknown action" ;;
esac
SEEN=" "
for a in "$@"; do
    case $a in *=*) ;; *) invalid "expected key=value" ;; esac
    k=${a%%=*}
    case " $KEYS " in *" $k "*) ;; *) invalid "unknown key for $ACTION" ;; esac
    case $SEEN in *" $k "*) invalid "$k given twice" ;; esac
    SEEN="$SEEN$k "
    eval "P_$k=\${a#*=}"
done

PARAMS=
add() { PARAMS="$PARAMS${PARAMS:+,}\"$1\":$2"; }
case $ACTION in
    cellular.set)
        # The enable read back is not sent back: it is the last value written,
        # 0 after boot while the auto dial is connected, and writing that 0 cuts
        # the data (E4 T12, B31). Like datad's cellular_args.
        OV=
        if [ -n "${P_enabled+x}" ]; then
            is01 "$P_enabled" || invalid "enabled must be 0 or 1"
            OV="${OV}enable=$P_enabled;"
            add enabled "$P_enabled"
        fi
        if [ -n "${P_roaming+x}" ]; then
            is01 "$P_roaming" || invalid "roaming must be 0 or 1"
            OV="${OV}roam_enable=$P_roaming;"
            add roaming "$P_roaming"
        fi
        if [ -n "${P_connect_mode+x}" ]; then
            isint "$P_connect_mode" || invalid "connect_mode must be a number"
            # a string, like datad (control.rs maps it as one)
            OV="${OV}connect_mode=\"$P_connect_mode\";"
            add connect_mode "\"$P_connect_mode\""
        fi
        [ -n "$OV" ] || invalid "no cellular fields supplied"
        [ -n "${P_enabled+x}" ] || OV="enable=;$OV"
        OV="${OV}source_module=\"WEBUI\";cid=1"
        ;;
    network.set_mode)
        isword "${P_mode:-}" || invalid "mode must be letters, digits and _"
        add mode "\"$P_mode\""
        ;;
    band.*)
        if [ "$ACTION" != band.reset ]; then
            isbands "${P_bands:-}" || invalid "bands must be numbers and commas"
            add bands "\"$P_bands\""
        fi
        ;;
    cell.lock_lte | cell.lock_nr)
        isint "${P_pci:-}" || invalid "pci must be a number"
        add pci "\"$P_pci\""
        if [ "$ACTION" = cell.lock_lte ]; then
            isint "${P_earfcn:-}" || invalid "earfcn must be a number"
            add earfcn "\"$P_earfcn\""
        else
            isint "${P_arfcn:-}" || invalid "arfcn must be a number"
            isint "${P_band:-}" || invalid "band must be a number"
            add arfcn "\"$P_arfcn\""
            add band "\"$P_band\""
        fi
        ;;
    apn.set_mode)
        is01 "${P_mode:-}" || invalid "mode must be 0 or 1"
        add mode "$P_mode"
        ;;
    wifi.radio)
        is01 "${P_ap_2g:-}" || invalid "ap_2g must be 0 or 1"
        is01 "${P_ap_5g:-}" || invalid "ap_5g must be 0 or 1"
        # QCMAP's single-AP mode only starts wlan0 (2.4 GHz): 5 GHz alone = no Wi-Fi at all
        [ "$P_ap_2g$P_ap_5g" = 01 ] && invalid "2.4 GHz off with 5 GHz on stops all Wi-Fi on this device"
        add ap_2g "$P_ap_2g"
        add ap_5g "$P_ap_5g"
        ;;
    cellular.redial)
        if [ -n "${P_type+x}" ]; then
            case $P_type in 1 | 2) ;; *) invalid "type must be 1 or 2" ;; esac
            add type "$P_type"
        fi
        ;;
    apn.enable)
        isword "${P_profile_id:-}" || invalid "profile_id must be letters, digits and _"
        add profile_id "\"$P_profile_id\""
        ;;
    nfc.set)
        is01 "${P_enabled:-}" || invalid "enabled must be 0 or 1"
        add enabled "$P_enabled"
        if [ -n "${P_flag+x}" ]; then
            isint "$P_flag" || invalid "flag must be a number"
            add flag "$P_flag"
        fi
        ;;
    power.direct_supply.set)
        case ${P_enabled:-} in
            1 | true) P_enabled=1 ;;
            0 | false) P_enabled=0 ;;
            *) invalid "enabled must be 0, 1, true or false" ;;
        esac
        add enabled "$P_enabled"
        ;;
esac

# ── 2. lock (fd 9; every child gets 9>&- so none keeps it) ────────────────────
if [ -n "$LOCK" ]; then
    # a failed redirection on exec would end the script (ash): try it in a subshell first
    if (: >>"$LOCK") 2>/dev/null; then
        exec 9>>"$LOCK"
        i=0
        until flock -n 9; do
            i=$((i + 1))
            [ "$i" -le "$WAIT" ] || done_ 4 "busy: being taken over"
            sleep 1
        done
    else
        # same as datad: a lock that cannot be opened does not stop the write
        echo "luyaz-fallback: cannot open $LOCK, writing without it" >&2
    fi
fi

# ── 3. is datad really gone? ──────────────────────────────────────────────────
alive() {
    for _pf in $PIDF; do
        pid=
        [ -r "$_pf" ] && read -r pid <"$_pf"
        case $pid in
            '' | *[!0-9]*) ;;
            *)
                comm=
                [ -r "$PROC/$pid/comm" ] && read -r comm <"$PROC/$pid/comm"
                # the core under its old or new name (10e; prefix: side-by-side trials)
                case $comm in zwrt-datad* | luyaz-core*) return 0 ;; esac
                ;;
        esac
    done
    [ -n "$HEALTH" ] || return 1
    # Any HTTP answer is a datad: /healthz says 503 while it is starting or
    # its executor is stuck, and busybox wget then fails with "server returned
    # error" (not answering at all is "can't connect" or a timeout).
    _h=$("$WGET" -q -T 2 -O /dev/null "$HEALTH" 2>&1 9>&-) && return 0
    case $_h in *"server returned error"*) return 0 ;; esac
    return 1
}
alive && done_ 3 "datad running: not written"
sleep "$GAP"
alive && done_ 3 "datad running: not written"

# ── 4. reads the write needs (no marker yet: a failed read changes nothing) ──
call() { "$UBUS" call "$1" "$2" "$3" 2>&1 9>&-; }
write() { call "$@" >/dev/null || failed "$1 $2"; }
case $ACTION in
    cellular.set)
        cur=$(call zwrt_data get_wwaniface '{"source_module":"web","cid":1,"connect_status":""}') ||
            failed "zwrt_data get_wwaniface"
        new=$(printf '%s\n' "$cur" | json merge "" "$OV") || failed "unreadable get_wwaniface reply"
        ;;
    power.direct_supply.set)
        want=disable
        [ "$P_enabled" = 1 ] && want=enable
        mode=$(call zwrt_bsp.charger list '{}' | json get direct_power_supply_mode)
        case $mode in
            enable | disable) ;;
            *) failed "direct supply is not supported or state is unknown" ;;
        esac
        [ "$mode" = "$want" ] && done_ 0 "ok"
        ;;
esac

# ── 5. takeover marker ────────────────────────────────────────────────────────
if [ -n "$OPS" ]; then
    mkdir -p "$OPS" 2>/dev/null
    now=$(date +%s)
    t=$(date '+%Y-%m-%d %H:%M:%S')
    printf '{"ts":%s,"t":"%s","by":"%s","action":"%s","params":{%s}}\n' \
        "$(num "$now" 0)" "$t" "$BY" "$ACTION" "$PARAMS" >>"$OPS/takeover" 2>/dev/null ||
        failed "cannot write $OPS/takeover"
    sync
fi

# One AT command, the way the core sends it (cat the port in the background,
# write, wait, stop the reader); prints what came back.
at_raw() {
    if [ -n "$ATHOOK" ]; then
        "$ATHOOK" "$1" 9>&- 8>&-
        return
    fi
    for p in ${LUYAZ_AT_PORT:-/dev/at_mdm0 /dev/at_mdm1 /dev/at_usb0 /dev/smd7 /dev/smd11}; do
        [ -e "$p" ] || continue
        out=$( (cat "$p" & r=$!; sleep 1; printf 'AT\r' >"$p"; sleep 1; kill "$r") 2>/dev/null 9>&- 8>&-)
        case $out in *OK*) ;; *) continue ;; esac
        (cat "$p" & r=$!; sleep 1; printf '%s\r' "$1" >"$p"; sleep 6; kill "$r") 2>/dev/null 9>&- 8>&-
        return
    done
    echo "no AT port"
}
at_write() {
    if [ -n "$ATLOCK" ] && (: >>"$ATLOCK") 2>/dev/null; then
        exec 8>>"$ATLOCK"
        i=0
        until flock -n 8; do
            i=$((i + 1))
            [ "$i" -le 15 ] || break # the agent's longest command: go ahead, like it does
            sleep 1
        done
    fi
    ans=$(at_raw "$1")
    case $ans in *OK*) ;; *) failed "$1: $(echo "$ans" | tr -d '\r' | tr '\n' ' ')" ;; esac
}

# ── 6. the write ──────────────────────────────────────────────────────────────
case $ACTION in
    netselect.auto)
        at_write "AT+COPS=0"
        ;;
    modem.online)
        at_write "AT+CFUN=1"
        ;;
    wifi.radio)
        # disabled = not on; the caller verifies by watching hostapd
        "$UCI" set "wireless.main_2g.disabled=$((1 - P_ap_2g))" 9>&- 8>&- || failed "uci set"
        "$UCI" set "wireless.main_5g.disabled=$((1 - P_ap_5g))" 9>&- 8>&- || failed "uci set"
        # guests off: one band's guest beside the mains makes exactly three APs, or a guest without
        # its main network — both leave Wi-Fi down (luyaz_wifi::layout). Missing sections are skipped.
        for g in guest_2g guest_5g; do
            "$UCI" -q get "wireless.$g" >/dev/null 9>&- 8>&- &&
                { "$UCI" set "wireless.$g.disabled=1" 9>&- 8>&- || failed "uci set"; }
        done
        "$UCI" commit wireless 9>&- 8>&- || failed "uci commit"
        write zwrt_wlan reload '{}'
        ;;
    cellular.redial)
        for ty in ${P_type:-1 2}; do
            write zwrt_qcmap_cli set_qcliiface "{\"source_module\":\"zte_topsw_data\",\"type\":$ty,\"enable\":1,\"sub_id\":1}"
        done
        ;;
    cellular.set)
        write zwrt_data set_wwaniface "$new"
        ;;
    network.set_mode)
        write zte_nwinfo_api nwinfo_set_netselect "{\"net_select\":\"$P_mode\"}"
        ;;
    band.set_lte)
        write zte_nwinfo_api nwinfo_set_lte_ext_band "{\"lte_band\":\"$P_bands\"}"
        ;;
    band.set_nr_sa | band.set_nr_nsa)
        typ=0
        [ "$ACTION" = band.set_nr_nsa ] && typ=1 # vendor web: SA "0", NSA "1"
        write zte_nwinfo_api nwinfo_set_nrbandlock "{\"nr5g_type\":\"$typ\",\"nr5g_band\":\"$P_bands\"}"
        ;;
    band.reset)
        write zte_nwinfo_api nwinfo_reset_band_cell_setting '{}'
        ;;
    cell.lock_lte)
        write zte_nwinfo_api nwinfo_lock_lte_cell "{\"lock_lte_pci\":\"$P_pci\",\"lock_lte_earfcn\":\"$P_earfcn\"}"
        ;;
    cell.lock_nr)
        write zte_nwinfo_api nwinfo_lock_nr_cell "{\"lock_nr_pci\":\"$P_pci\",\"lock_nr_earfcn\":\"$P_arfcn\",\"lock_nr_cell_band\":\"$P_band\"}"
        ;;
    apn.set_mode)
        write zwrt_apn_object set_apn_mode "{\"apn_mode\":$P_mode}"
        ;;
    apn.enable)
        write zwrt_apn_object enable_manu_apn_id "{\"profileId\":\"$P_profile_id\"}"
        ;;
    nfc.set)
        args="{\"switch\":$P_enabled"
        [ -n "${P_flag+x}" ] && args="$args,\"flag\":$P_flag"
        write zwrt_nfc zwrt_nfc_wifi_set "$args}"
        call zwrt_nfc zwrt_nfc_wifi_change '{}' >/dev/null
        ;;
    power.direct_supply.set)
        reply=$(call zwrt_bsp.charger set "{\"direct_power_supply_mode\":\"$want\"}") ||
            failed "zwrt_bsp.charger set"
        # an empty reply is success (B20); otherwise no "error" and result 0
        if [ -n "$(printf '%s' "$reply" | tr -d ' \t\r\n')" ]; then
            printf '%s\n' "$reply" | json get error >/dev/null && failed "zwrt_bsp.charger set"
            # like datad: only a result that reads as a non-zero number fails
            res=$(printf '%s\n' "$reply" | json get result | tr -d ' ')
            case $res in '' | *[!0-9-]* | -) ;; *[!0]*) failed "zwrt_bsp.charger set" ;; esac
        fi
        i=0
        while :; do
            [ "$(call zwrt_bsp.charger list '{}' | json get direct_power_supply_mode)" = "$want" ] && break
            i=$((i + 1))
            [ "$i" -lt 5 ] || failed "direct supply readback did not confirm the requested mode"
            sleep "$VGAP"
        done
        ;;
esac
done_ 0 "ok"
