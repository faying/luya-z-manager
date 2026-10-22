#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# Tests for luyaz-fallback.sh (the emergency direct write when the core is
# gone; write-op-layer.md D18/D23/D29). ubus and wget are stand-ins that log
# every call; /proc is a fake tree except in the one real-process case.
# Everything lives under $T.
#
#   scripts/test/docker.sh        (runs every suite under scripts/test/)
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
# ─────────────────────────────────────────────────────────────────────────────

SCRIPTS=${SCRIPTS:-/scripts}
FB=$SCRIPTS/luyaz-fallback.sh
PASS=0
FAIL=0

ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; }
check() {
    _d=$1
    shift
    if eval "$@"; then ok "$_d"; else bad "$_d  [$*]"; fi
}

setup() {
    T=$(mktemp -d)
    mkdir -p "$T/proc" "$T/ops" "$T/r"
    # ubus: logs "obj.method lock fd9 json"; reply from r/<obj.method>, exit from r/<obj.method>.rc.
    # zwrt_bsp.charger keeps its mode in r/mode; set changes it unless r/sticky exists.
    cat >"$T/ubus" <<EOF
#!/bin/sh
[ "\$1" = call ] || exit 2
k="\$2.\$3"
if flock -n "$T/lock" true 2>/dev/null; then l=unlocked; else l=locked; fi
f=closed; [ -e /proc/\$\$/fd/9 ] && f=open
printf '%s %s %s %s\n' "\$k" "\$l" "\$f" "\$4" >>"$T/calls"
case \$k in
    zwrt_bsp.charger.list) [ -f "$T/r/mode" ] && printf '{\n\t"direct_power_supply_mode": "%s"\n}\n' "\$(cat "$T/r/mode")" ;;
    zwrt_bsp.charger.set) [ -f "$T/r/sticky" ] || echo "\$4" | sed 's/.*:"\(.*\)"}/\1/' >"$T/r/mode" ;;
esac
[ -f "$T/r/\$k" ] && cat "$T/r/\$k"
exit "\$(cat "$T/r/\$k.rc" 2>/dev/null || echo 0)"
EOF
    # wget: answers per line of r/wget (1 = up, 503 = answers 503 like busybox
    # wget does, anything else = down), one line per call; default down
    cat >"$T/wget" <<EOF
#!/bin/sh
n=\$(( \$(cat "$T/wget.n" 2>/dev/null || echo 0) + 1 ))
echo \$n >"$T/wget.n"
a=\$(sed -n "\${n}p" "$T/r/wget" 2>/dev/null)
[ "\$a" = 503 ] && { echo "wget: server returned error: HTTP/1.1 503 Service Unavailable" >&2; exit 1; }
[ "\$a" = 1 ] || { echo "wget: can't connect to remote host (127.0.0.1): Connection refused" >&2; exit 1; }
EOF
    chmod +x "$T/ubus" "$T/wget"
    export LUYAZ_FALLBACK_UBUS=$T/ubus LUYAZ_FALLBACK_WGET=$T/wget LUYAZ_FALLBACK_HEALTH_URL=http://127.0.0.1:1/healthz
    export LUYAZ_FALLBACK_PROC=$T/proc LUYAZ_FALLBACK_GAP=0 LUYAZ_FALLBACK_VERIFY_GAP=0 LUYAZ_FALLBACK_LOCK_WAIT=10
    export LUYAZ_WRITE_LOCK=$T/lock LUYAZ_PID_FILE=$T/pid LUYAZ_OPS_DIR=$T/ops
}
teardown() { rm -rf "$T"; }

# run ARGS...: $OUT = stdout, $RC = exit code
run() {
    OUT=$(sh "$FB" "$@" 2>"$T/err")
    RC=$?
}
calls() { cat "$T/calls" 2>/dev/null; }
ncalls() { calls | wc -l | tr -d ' '; }
marks() { cat "$T/ops/takeover" 2>/dev/null | wc -l | tr -d ' '; }
# the JSON sent to the one write call named $1
sent() { calls | grep "^$1 " | head -n 1 | cut -d' ' -f4-; }

echo "luyaz-fallback: one payload per action (core gone)"
for c in \
    'network.set_mode mode=Only_LTE|zte_nwinfo_api.nwinfo_set_netselect|{"net_select":"Only_LTE"}' \
    'band.set_lte bands=1,3,28|zte_nwinfo_api.nwinfo_set_lte_ext_band|{"lte_band":"1,3,28"}' \
    'band.set_nr_sa bands=78|zte_nwinfo_api.nwinfo_set_nrbandlock|{"nr5g_type":"0","nr5g_band":"78"}' \
    'band.set_nr_nsa bands=41,78|zte_nwinfo_api.nwinfo_set_nrbandlock|{"nr5g_type":"1","nr5g_band":"41,78"}' \
    'band.reset|zte_nwinfo_api.nwinfo_reset_band_cell_setting|{}' \
    'cell.lock_lte pci=12 earfcn=1850|zte_nwinfo_api.nwinfo_lock_lte_cell|{"lock_lte_pci":"12","lock_lte_earfcn":"1850"}' \
    'cell.lock_nr pci=5 arfcn=627264 band=78|zte_nwinfo_api.nwinfo_lock_nr_cell|{"lock_nr_pci":"5","lock_nr_earfcn":"627264","lock_nr_cell_band":"78"}' \
    'apn.set_mode mode=0|zwrt_apn_object.set_apn_mode|{"apn_mode":0}' \
    'apn.enable profile_id=2|zwrt_apn_object.enable_manu_apn_id|{"profileId":"2"}' \
    'cellular.redial type=2|zwrt_qcmap_cli.set_qcliiface|{"source_module":"zte_topsw_data","type":2,"enable":1,"sub_id":1}' \
    'nfc.set enabled=1 flag=2|zwrt_nfc.zwrt_nfc_wifi_set|{"switch":1,"flag":2}' \
    'nfc.set enabled=0|zwrt_nfc.zwrt_nfc_wifi_set|{"switch":0}'; do
    setup
    args=${c%%|*} rest=${c#*|}
    meth=${rest%%|*} want=${rest#*|}
    # shellcheck disable=SC2086
    run $args
    check "$args: exit 0, says ok" '[ "$RC" = 0 ] && [ "$OUT" = ok ]'
    check "$args: sends $want" '[ "$(sent "$meth")" = "$want" ]'
    check "$args: lock held during the call, fd 9 not inherited" 'calls | grep -q "^$meth locked closed "'
    check "$args: one takeover line" '[ "$(marks)" = 1 ]'
    teardown
done
setup
printf '#!/bin/sh\necho "$*" >>"%s/uci"\n' "$T" >"$T/bin-uci"
mkdir -p "$T/bin" && mv "$T/bin-uci" "$T/bin/uci" && chmod +x "$T/bin/uci"
export LUYAZ_FALLBACK_UCI=$T/bin/uci
run wifi.radio ap_2g=1 ap_5g=0
check "wifi.radio: uci set both, guests off, commit, then reload" '[ "$RC" = 0 ] && [ "$(cat "$T/uci")" = "set wireless.main_2g.disabled=0
set wireless.main_5g.disabled=1
-q get wireless.guest_2g
set wireless.guest_2g.disabled=1
-q get wireless.guest_5g
set wireless.guest_5g.disabled=1
commit wireless" ] && [ "$(sent zwrt_wlan.reload)" = "{}" ]'
: >"$T/uci"
run wifi.radio ap_2g=0 ap_5g=1
check "wifi.radio: 2.4 off with 5 on → exit 2, nothing written" '[ "$RC" = 2 ] && [ ! -s "$T/uci" ]'
run wifi.radio ap_2g=1
check "wifi.radio needs both → exit 2" '[ "$RC" = 2 ]'
teardown
setup
run cellular.redial
check "cellular.redial without type: both legs" '[ "$(calls | grep -c set_qcliiface)" = 2 ] && sent zwrt_qcmap_cli.set_qcliiface | grep -q "\"type\":1"'
run cellular.redial type=3
check "cellular.redial type=3 → exit 2" '[ "$RC" = 2 ]'
teardown
setup
run nfc.set enabled=1
check "nfc.set: then zwrt_nfc_wifi_change, like the core" '[ "$(calls | cut -d" " -f1 | tr "\n" " ")" = "zwrt_nfc.zwrt_nfc_wifi_set zwrt_nfc.zwrt_nfc_wifi_change " ]'
teardown

echo "luyaz-fallback: AT commands (core gone)"
for c in 'netselect.auto|AT+COPS=0' 'modem.online|AT+CFUN=1'; do
    setup
    # stand-in modem: logs the text and whether the AT lock is held, answers OK
    cat >"$T/at" <<EOF
#!/bin/sh
if flock -n "$T/atlock" true 2>/dev/null; then l=unlocked; else l=locked; fi
echo "\$1 \$l" >>"$T/atcalls"
printf '\r\nOK\r\n'
EOF
    chmod +x "$T/at"
    export LUYAZ_FALLBACK_AT=$T/at LUYAZ_AT_LOCK=$T/atlock
    run "${c%|*}"
    check "${c%|*}: exit 0, sent ${c#*|} holding the AT lock" '[ "$RC" = 0 ] && [ "$(cat "$T/atcalls")" = "${c#*|} locked" ]'
    check "${c%|*}: one takeover line, no ubus" '[ "$(marks)" = 1 ] && [ "$(ncalls)" = 0 ]'
    teardown
done
setup
printf '#!/bin/sh\nprintf "\\r\\n+CME ERROR: 3\\r\\n"\n' >"$T/at"
chmod +x "$T/at"
export LUYAZ_FALLBACK_AT=$T/at LUYAZ_AT_LOCK=$T/atlock
run netselect.auto
check "netselect.auto: modem says ERROR → exit 1" '[ "$RC" = 1 ] && case $OUT in *ERROR*) true ;; *) false ;; esac'
run netselect.auto x=1
check "netselect.auto takes no arguments → exit 2" '[ "$RC" = 2 ]'
teardown

echo "luyaz-fallback: takeover marker"
setup
run --by agent network.set_mode mode=WL_AND_5G
run band.set_lte bands=3
check "two writes → two lines" '[ "$(marks)" = 2 ]'
check "line says who, what, params" 'head -n 1 "$T/ops/takeover" | grep -q "\"by\":\"agent\",\"action\":\"network.set_mode\",\"params\":{\"mode\":\"WL_AND_5G\"}}$"'
check "second line defaults to screen" 'sed -n 2p "$T/ops/takeover" | grep -q "\"by\":\"screen\",\"action\":\"band.set_lte\",\"params\":{\"bands\":\"3\"}"'
check "line has ts and t" 'head -n 1 "$T/ops/takeover" | grep -Eq "^\{\"ts\":[0-9]+,\"t\":\"[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9:]{8}\","'
teardown
setup
export LUYAZ_OPS_DIR=$T/notadir/ops
: >"$T/notadir"
run band.reset
check "marker cannot be written → exit 1, nothing written" '[ "$RC" = 1 ] && [ "$(ncalls)" = 0 ]'
teardown
setup
export LUYAZ_OPS_DIR=
run band.reset
check "no ops dir (core keeps no state) → writes without a marker" '[ "$RC" = 0 ] && [ "$(ncalls)" = 1 ]'
teardown

echo "luyaz-fallback: is the core gone? (D18)"
for c in 'luyaz-core|3' 'luyaz-core.test|3' 'sh|0' 'luyaz-cor|0' 'zwrt-datad|0'; do
    setup
    echo 4242 >"$T/pid"
    mkdir -p "$T/proc/4242"
    echo "${c%|*}" >"$T/proc/4242/comm"
    run band.reset
    check "pid file → comm \"${c%|*}\": exit ${c#*|}" '[ "$RC" = "${c#*|}" ]'
    [ "${c#*|}" = 3 ] && check "comm \"${c%|*}\": nothing written, no marker" '[ "$(ncalls)" = 0 ] && [ "$(marks)" = 0 ] && [ "$OUT" = "core running: not written" ]'
    teardown
done
setup
# core writes "<pid>\n"; also take it without the newline
printf '4242' >"$T/pid"
mkdir -p "$T/proc/4242"
printf 'luyaz-core\n' >"$T/proc/4242/comm"
run band.reset
check "pid file without a newline → still read, exit 3" '[ "$RC" = 3 ]'
teardown
# two pid files; a stale one does not hide the live core
for c in 'stale-new|3' 'stale-old|3' 'both-stale|0'; do
    setup
    export LUYAZ_PID_FILE="$T/pid.new $T/pid"
    mkdir -p "$T/proc/4242"; printf 'luyaz-core\n' >"$T/proc/4242/comm"
    case ${c%|*} in
        stale-new) echo 999 >"$T/pid.new"; echo 4242 >"$T/pid" ;;
        stale-old) echo 4242 >"$T/pid.new"; echo 999 >"$T/pid" ;;
        both-stale) echo 999 >"$T/pid.new"; echo 998 >"$T/pid" ;;
    esac
    run band.reset
    check "two pid files, ${c%|*}: exit ${c#*|}" '[ "$RC" = "${c#*|}" ]'
    teardown
done
for p in '12ab' '' '-1' '99999999999999999999'; do
    setup
    printf '%s\n' "$p" >"$T/pid"
    run band.reset
    check "pid file \"$p\": not alive, writes" '[ "$RC" = 0 ]'
    teardown
done
setup
printf '1\n' >"$T/r/wget"
run band.reset
check "no pid file but /healthz answers → exit 3" '[ "$RC" = 3 ] && [ "$(ncalls)" = 0 ]'
teardown
setup
printf '503\n' >"$T/r/wget"
run band.reset
check "no pid file, /healthz 503 (starting or stuck) → still alive, exit 3" '[ "$RC" = 3 ] && [ "$(ncalls)" = 0 ]'
teardown
setup
printf '0\n1\n' >"$T/r/wget"
run band.reset
check "gone on the first check, back on the second → exit 3, no marker" '[ "$RC" = 3 ] && [ "$(ncalls)" = 0 ] && [ "$(marks)" = 0 ]'
check "asked /healthz twice" '[ "$(cat "$T/wget.n")" = 2 ]'
teardown
setup
printf '0\n0\n' >"$T/r/wget"
run band.reset
check "gone both times → writes" '[ "$RC" = 0 ] && [ "$(cat "$T/wget.n")" = 2 ]'
teardown
setup
export LUYAZ_FALLBACK_HEALTH_URL=
run band.reset
check "no /healthz URL → pid check only" '[ "$RC" = 0 ] && [ ! -e "$T/wget.n" ]'
teardown
setup
# a real process under a side-by-side name: comm is the script's file name
printf '#!/bin/sh\nwhile :; do sleep 1; done\n' >"$T/luyaz-core.test"
chmod +x "$T/luyaz-core.test"
"$T/luyaz-core.test" &
DP=$!
echo $DP >"$T/pid"
export LUYAZ_FALLBACK_PROC=/proc
sleep 0.2
run band.reset
check "real process luyaz-core.test → exit 3" '[ "$RC" = 3 ] && [ "$(ncalls)" = 0 ]'
kill $DP 2>/dev/null
wait $DP 2>/dev/null
run band.reset
check "same pid file after it died → writes" '[ "$RC" = 0 ]'
teardown

echo "luyaz-fallback: write lock (D29)"
setup
export LUYAZ_FALLBACK_LOCK_WAIT=2
flock "$T/lock" sleep 6 &
HP=$!
sleep 0.3
s0=$(date +%s)
run band.reset
s1=$(date +%s)
check "held longer than the wait → exit 4" '[ "$RC" = 4 ] && [ "$OUT" = "busy: being taken over" ]'
check "nothing written, no marker" '[ "$(ncalls)" = 0 ] && [ "$(marks)" = 0 ]'
check "gave up after about the wait" '[ $((s1 - s0)) -ge 2 ] && [ $((s1 - s0)) -le 4 ]'
check "did not check core before it had the lock" '[ ! -e "$T/wget.n" ]'
kill $HP 2>/dev/null
wait $HP 2>/dev/null
teardown
setup
flock "$T/lock" sleep 2 &
HP=$!
sleep 0.3
run band.reset
check "released while waiting → writes" '[ "$RC" = 0 ] && [ "$(ncalls)" = 1 ]'
wait $HP 2>/dev/null
teardown
echo "luyaz-fallback: the default locks are the Luya-Z names"
setup
# the defaults point at /var/run: check the names the script holds, not the files
check "write lock default luyaz-write.lock" 'grep -q "^LOCK=\${LUYAZ_WRITE_LOCK-/var/run/luyaz-write.lock}" "$FB"'
check "AT lock default luyaz-at.lock" 'grep -q "^ATLOCK=\${LUYAZ_AT_LOCK-/var/run/luyaz-at.lock}" "$FB"'
check "no old lock name left" '! grep -q "u60-write.lock\|u60-at.lock" "$FB"'
teardown
setup
export LUYAZ_WRITE_LOCK=$T/no/such/dir/lock
run band.reset
check "lock file cannot be opened → exit 5, says so" '[ "$RC" = 5 ] && [ "$OUT" = "lock: cannot open the write lock" ] && grep -q "not written" "$T/err"'
check "lock file cannot be opened → nothing written, no marker, core not checked" '[ "$(ncalls)" = 0 ] && [ "$(marks)" = 0 ] && [ ! -e "$T/wget.n" ]'
teardown

echo "luyaz-fallback: cellular.set reads, then changes only the named fields"
WWAN='{
	"cid": 1,
	"enable": 0,
	"roam_enable": 1,
	"apn": "inter\"net,{x}",
	"pdp_type": "IPV4V6",
	"ipv4": { "addr": "10.0.0.2", "dns": [ "1.1.1.1", "8.8.8.8" ] },
	"source_module": "web"
}'
setup
printf '%s\n' "$WWAN" >"$T/r/zwrt_data.get_wwaniface"
run cellular.set enabled=1
check "exit 0" '[ "$RC" = 0 ]'
check "read with the core's params" '[ "$(sent zwrt_data.get_wwaniface)" = "{\"source_module\":\"web\",\"cid\":1,\"connect_status\":\"\"}" ]'
want='{"cid":1,"enable":1,"roam_enable":1,"apn":"inter\"net,{x}","pdp_type":"IPV4V6","ipv4":{ "addr": "10.0.0.2", "dns": [ "1.1.1.1", "8.8.8.8" ] },"source_module":"WEBUI"}'
check "everything else kept, escaped and nested values untouched" '[ "$(sent zwrt_data.set_wwaniface)" = "$want" ]'
teardown
setup
printf '{ "pdp_type": "IP", "nested": { "enable": 0 } }\n' >"$T/r/zwrt_data.get_wwaniface"
run cellular.set roaming=0 connect_mode=1
want='{"pdp_type":"IP","nested":{ "enable": 0 },"roam_enable":0,"connect_mode":"1","source_module":"WEBUI","cid":1}'
check "missing fields appended (connect_mode a string, like the core), a nested \"enable\" is not top-level" '[ "$(sent zwrt_data.set_wwaniface)" = "$want" ]'
check "marker params" 'grep -q "\"params\":{\"roaming\":0,\"connect_mode\":\"1\"}" "$T/ops/takeover"'
teardown
setup
printf '%s\n' "$WWAN" >"$T/r/zwrt_data.get_wwaniface"
run cellular.set roaming=0
want='{"cid":1,"roam_enable":0,"apn":"inter\"net,{x}","pdp_type":"IPV4V6","ipv4":{ "addr": "10.0.0.2", "dns": [ "1.1.1.1", "8.8.8.8" ] },"source_module":"WEBUI"}'
check "the enable read back (0 after boot, data up) is not sent back: it would cut the data" '[ "$(sent zwrt_data.set_wwaniface)" = "$want" ]'
teardown
setup
echo 'Command failed: Not found' >"$T/r/zwrt_data.get_wwaniface"
echo 4 >"$T/r/zwrt_data.get_wwaniface.rc"
run cellular.set enabled=0
check "read fails → exit 1, no set_wwaniface, no marker" '[ "$RC" = 1 ] && ! calls | grep -q set_wwaniface && [ "$(marks)" = 0 ]'
teardown
for reply in 'garbage' '[1,2]' '{"a":1' '{"a" 1}' '{"a":"x}'; do
    setup
    printf '%s\n' "$reply" >"$T/r/zwrt_data.get_wwaniface"
    run cellular.set enabled=0
    check "reply $reply → exit 1, never the 4-field write" '[ "$RC" = 1 ] && ! calls | grep -q set_wwaniface && [ "$(marks)" = 0 ]'
    teardown
done
setup
printf '%s\n' "$WWAN" >"$T/r/zwrt_data.get_wwaniface"
echo 1 >"$T/r/zwrt_data.set_wwaniface.rc"
run cellular.set enabled=1
check "set fails → exit 1, says which call" '[ "$RC" = 1 ] && [ "$OUT" = "failed: zwrt_data set_wwaniface" ]'
teardown

echo "luyaz-fallback: power.direct_supply.set reads back, like the core"
setup
echo disable >"$T/r/mode"
run power.direct_supply.set enabled=true
check "disable → enable: exit 0" '[ "$RC" = 0 ]'
check "sent enable, then read back" '[ "$(sent zwrt_bsp.charger.set)" = "{\"direct_power_supply_mode\":\"enable\"}" ] && [ "$(calls | grep -c charger.list)" = 2 ]'
teardown
setup
echo enable >"$T/r/mode"
run power.direct_supply.set enabled=1
check "already enabled → exit 0, no set, no marker" '[ "$RC" = 0 ] && ! calls | grep -q charger.set && [ "$(marks)" = 0 ]'
teardown
setup
run power.direct_supply.set enabled=0
check "no mode in the reply (unsupported) → exit 1, no set, no marker" '[ "$RC" = 1 ] && ! calls | grep -q charger.set && [ "$(marks)" = 0 ]'
teardown
setup
echo disable >"$T/r/mode"
: >"$T/r/sticky"
run power.direct_supply.set enabled=1
check "readback never confirms → exit 1 after 5 reads" '[ "$RC" = 1 ] && [ "$(calls | grep -c charger.list)" = 6 ]'
teardown
for reply in '{"result":1}' '{"result":"2"}' '{"error":"x"}'; do
    setup
    echo disable >"$T/r/mode"
    printf '%s\n' "$reply" >"$T/r/zwrt_bsp.charger.set"
    run power.direct_supply.set enabled=1
    check "set replies $reply → exit 1" '[ "$RC" = 1 ]'
    teardown
done
for reply in '{"result":0}' '{"result":"0"}' '{}'; do
    setup
    echo disable >"$T/r/mode"
    printf '%s\n' "$reply" >"$T/r/zwrt_bsp.charger.set"
    run power.direct_supply.set enabled=1
    check "set replies $reply → exit 0" '[ "$RC" = 0 ]'
    teardown
done

echo "luyaz-fallback: bad arguments touch nothing"
for a in \
    '' 'device.reboot' 'network.set_mode' 'network.set_mode mode=' "network.set_mode mode=a;reboot" \
    'network.set_mode mode=$(reboot)' "network.set_mode mode=a'b" 'network.set_mode mode=a"b' \
    'network.set_mode mode=Only_LTE mode=WL_AND_5G' 'network.set_mode mode=Only_LTE x=1' 'network.set_mode Only_LTE' \
    'band.set_lte bands=' 'band.set_lte bands=,,,' 'band.set_lte bands=1;3' 'band.set_lte bands=1 3' \
    'band.reset bands=1' 'cellular.set' 'cellular.set enabled=2' 'cellular.set roaming=true' \
    'cellular.set connect_mode=-1' 'cell.lock_lte pci=1' 'cell.lock_nr pci=1 arfcn=2' 'cell.lock_nr pci=1 arfcn=2 band=n78' \
    'apn.set_mode mode=2' 'apn.enable' 'apn.enable profile_id=a;b' 'nfc.set' 'nfc.set enabled=1 flag=x' 'power.direct_supply.set enabled=yes' \
    '--by' '--by bad-name band.reset' '--by=a;b band.reset'; do
    setup
    # shellcheck disable=SC2086
    run $a
    check "\"$a\" → exit 2, no lock file, no call, no marker" '[ "$RC" = 2 ] && [ ! -e "$T/lock" ] && [ "$(ncalls)" = 0 ] && [ ! -e "$T/ops/takeover" ]'
    teardown
done
setup
run network.set_mode 'mode=a b'
check "a value with a space → exit 2" '[ "$RC" = 2 ] && [ "$(ncalls)" = 0 ]'
run network.set_mode "mode=$(printf 'a\nb')"
check "a value with a newline → exit 2" '[ "$RC" = 2 ] && [ "$(ncalls)" = 0 ]'
teardown
setup
export LUYAZ_FALLBACK_GAP=x LUYAZ_FALLBACK_LOCK_WAIT=1y
run band.reset
check "bad numbers in the test hooks fall back to defaults, no shell error" '[ "$RC" = 0 ] && [ ! -s "$T/err" ]'
teardown

echo
echo "luyaz-fallback: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
