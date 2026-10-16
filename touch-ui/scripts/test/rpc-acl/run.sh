#!/bin/sh
# rpc-acl.sh with ubus, rpcd, mount and jsonfilter stubbed. The stub rpcd
# answers from whatever file sits at the target when it is reloaded, like the
# real one. Needs python3 (the jsonfilter stub): run it in tools/luyaz-dev rust.
# web.json.b31 is the stock ACL as on the device (CN B31, 2026-10-16).
# SPDX-License-Identifier: MIT
command -v python3 >/dev/null || { echo "  skipped: no python3 (run it in tools/luyaz-dev rust)"; exit 0; }
HERE=$(cd "$(dirname "$0")" && pwd)
S=$HERE/../../rpc-acl.sh
PASS=0; FAIL=0
ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; }
check() { _d=$1; shift; if eval "$@"; then ok "$_d"; else bad "$_d  [$*]"; fi; }

setup() {
    T=$(mktemp -d); mkdir -p "$T/bin" "$T/ro"
    cp "$HERE/web.json.b31" "$T/ro/web.json"; cp "$T/ro/web.json" "$T/stock.orig"
    : >"$T/mounts"
    # the "mount": copy over the target, remember the original
    cat >"$T/bin/mount" <<X
#!/bin/sh
[ "\$1" = -o ] && [ "\$2" = bind ] || exit 1
cp "\$3" "\$4" && echo "x \$4 none bind 0 0" >>$T/mounts
X
    cat >"$T/bin/umount" <<X
#!/bin/sh
grep -q " \$1 " $T/mounts || exit 1
cp $T/stock.orig "\$1"; : >$T/mounts
X
    # rpcd: on reload, load the anonymous list from the target; `fail` = it ignores reloads
    cat >"$T/bin/rpcd" <<X
#!/bin/sh
echo "\$1" >>$T/rpcd.log
[ -f $T/rpcd-stuck ] && exit 0
python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); u=d["unauthenticated"]; s=set(u["read"]["ubus"].get("zwrt_mc.device.manager",[]))|set(u["write"]["ubus"].get("zwrt_mc.device.manager",[])); print(" ".join(sorted(s)))' $T/ro/web.json >$T/loaded
X
    cat >"$T/bin/ubus" <<X
#!/bin/sh
[ -f $T/ubus-down ] && exit 4
f=\$(echo "\$4" | sed 's/.*"function":"\([^"]*\)".*/\1/')
case " \$(cat $T/loaded) " in *" \$f "*) printf '{\n\t"access": true\n}\n' ;; *) printf '{\n\t"access": false\n}\n' ;; esac
X
    cat >"$T/bin/jsonfilter" <<'X'
#!/usr/bin/env python3
import json,sys
a=sys.argv; f=a[a.index("-i")+1]; e=a[a.index("-e")+1]
try: d=json.load(open(f))
except Exception: sys.exit(1)
for k in e[2:].split(".") if e!="@" else []: d=d[k]
print(json.dumps(d))
X
    chmod +x "$T"/bin/*
    "$T/bin/rpcd" start
    export RPCACL_DIR=$T/data RPCACL_TARGET=$T/ro/web.json RPCACL_MOUNTS=$T/mounts RPCACL_UBUS=$T/bin/ubus \
        RPCACL_JSONFILTER=$T/bin/jsonfilter RPCACL_RPCD=$T/bin/rpcd RPCACL_MOUNT=$T/bin/mount \
        RPCACL_UMOUNT=$T/bin/umount RPCACL_BOOT_WAIT=2
}

echo "stock B31"
setup
check "status: off, all six open" '[ "$(sh "$S" status)" = "chosen: off
mounted: no
anonymous: fac_open fac_close fac_reset fac_reboot fac_query_key adb_switch" ]'
sh "$S" check >"$T/out" 2>&1
check "check: copy OK, two lines differ, nothing mounted" 'grep -q "copy OK" "$T/out" && [ "$(grep -c "^[<>]" "$T/out")" = 3 ] && [ ! -s "$T/mounts" ]'
check "check: the logged-in list is unchanged" '[ "$(python3 -c "import json,sys;print(json.load(open(sys.argv[1]))[\"web\"]==json.load(open(sys.argv[2]))[\"web\"])" "$T/stock.orig" "$T/data/web.json.check")" = True ]'
check "check: anonymous keeps get_device_info only" '[ "$(python3 -c "import json,sys;u=json.load(open(sys.argv[1]))[\"unauthenticated\"];print(*u[\"read\"][\"ubus\"][\"zwrt_mc.device.manager\"], \"zwrt_mc.device.manager\" in u[\"write\"][\"ubus\"], len(u[\"write\"][\"ubus\"]))" "$T/data/web.json.check")" = "get_device_info False 5" ]'
sh "$S" on >"$T/out" 2>&1; rc=$?
check "on: tightened, chosen, mounted" '[ $rc = 0 ] && grep -q tightened "$T/out" && [ -f "$T/data/enabled" ] && [ -s "$T/mounts" ] && [ "$(sh "$S" status | tail -n 1)" = "anonymous: none" ]'
check "on: reload was enough" '[ "$(tail -n 1 "$T/rpcd.log")" = reload ]'
sh "$S" on >/dev/null 2>&1
check "on twice: still one mount" '[ "$(wc -l <"$T/mounts")" = 1 ]'
sh "$S" off >"$T/out" 2>&1; rc=$?
check "off: stock back, not chosen" '[ $rc = 0 ] && [ ! -s "$T/mounts" ] && [ ! -f "$T/data/enabled" ] && cmp -s "$T/ro/web.json" "$T/stock.orig" && sh "$S" status | grep -q "anonymous: fac_open"'
rm -rf "$T"

echo "boot"
setup
sh "$S" boot; rc=$?
check "boot when not chosen: nothing" '[ $rc = 0 ] && [ ! -s "$T/mounts" ] && [ ! -d "$T/data" ]'
mkdir -p "$T/data"; touch "$T/data/enabled"
sh "$S" boot >/dev/null 2>&1; rc=$?
check "boot when chosen: tightened" '[ $rc = 0 ] && [ -s "$T/mounts" ] && sh "$S" status | grep -q "anonymous: none"'
rm -rf "$T"
setup; mkdir -p "$T/data"; touch "$T/data/enabled" "$T/ubus-down"
sh "$S" boot >"$T/out" 2>&1; rc=$?
check "boot, rpcd never up: gives up, mounts nothing" '[ $rc = 1 ] && grep -q "not up" "$T/out" && [ ! -s "$T/mounts" ]'
rm -rf "$T"

echo "refusals"
setup
touch "$T/rpcd-stuck"
sh "$S" on >"$T/out" 2>&1; rc=$?
check "rpcd keeps the stock list: unmounted again, not chosen" '[ $rc = 1 ] && grep -q unmounting "$T/out" && [ ! -s "$T/mounts" ] && [ ! -f "$T/data/enabled" ] && grep -q restart "$T/rpcd.log"'
rm -rf "$T"
setup
sed 's/"adb_switch" , "get_device_info"/"get_device_info"],\n"x": ["adb_switch"/' "$HERE/web.json.b31" >"$T/ro/web.json"
"$T/bin/rpcd" reload
sh "$S" on >"$T/out" 2>&1; rc=$?
check "another file shape: refused before mounting" '[ $rc = 1 ] && grep -q "still anonymous" "$T/out" && [ ! -s "$T/mounts" ]'
printf '{ "unauthenticated": ' >"$T/ro/web.json"
sh "$S" on >"$T/out" 2>&1; rc=$?
check "broken stock file: refused" '[ $rc = 1 ] && grep -q "does not parse" "$T/out" && [ ! -s "$T/mounts" ]'
rm -rf "$T"

echo "passed $PASS, failed $FAIL"
[ "$FAIL" = 0 ]
