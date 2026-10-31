#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# Offline test of the kit's first-install snapshot and `undo`
# (onboard/device/install.sh space_check / snap_take / do_undo;
# docs/research/ext-1030-installer-rollback.md §4-B). Nothing touches a device:
# the whole root moves into a temp dir (KIT_ROOT, KIT_RC, KIT_STATE, KIT_SHIP_DIR,
# KIT_SCRIPTS, KIT_CORE_DIR, KIT_SCREEN_DIR); the components are stubs that write
# the same paths the real do_* write, through the real put / rc_add; the real main
# section (from the usage line to the end) runs. In the device's busybox:
#
#   docker run --rm -v "$PWD/onboard":/onboard:ro u60-device-busybox:20260928 \
#       sh /onboard/test/kit-undo.sh
#
# Checks: die at component k (k = 1..5) → undo → the fake root's fingerprint equals
# the one before the install; undo killed after n renames → undo again → same;
# not enough space on /data or /etc → nothing written; a bad snapshot rc.local →
# undo changes nothing; the default undo keeps SSH; a later fresh run keeps the
# first snapshot.
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
# ─────────────────────────────────────────────────────────────────────────────
set -u
ONB=${ONB:-$(cd "$(dirname "$0")/.." && pwd)}
PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; [ -f "$T/out" ] && sed 's/^/       | /' "$T/out" | tail -n 8; }
check() { _d=$1; shift; if eval "$@"; then ok "$_d"; else bad "$_d  [$*]"; fi; }
MAIN=$(sed -n '/^\[ \$# -gt 0 \] || die/,$p' "$ONB/device/install.sh")
[ -n "$MAIN" ] || { echo "FAIL main section not found"; exit 1; }

setup() {
    T=$(mktemp -d)
    R=$T/root
    K=$T/kit
    mkdir -p "$K/device" "$K/payload" "$R/etc/init.d" "$R/etc/config" "$R/data/local/tmp" "$R/data/esim" "$R/data/luyaz"
    cp "$ONB/device/install.sh" "$K/device/"
    echo payload >"$K/payload/blob"
    printf '#!/bin/sh\n/etc/init.d/zte_topsw_get_brand stop\nexit 0\n' >"$R/etc/rc.local"
    chmod 755 "$R/etc/rc.local"
    echo 'stock init' >"$R/etc/init.d/zte_topsw_devui"
    printf "config zone\n\toption name 'lan'\n" >"$R/etc/config/firewall"
    echo 'config dnsmasq' >"$R/etc/config/dhcp"
    echo 'other' >"$R/data/local/tmp/keep.txt"
    echo 'old lpac' >"$R/data/esim/lpac.sh"
    export KIT_ROOT=$R KIT_RC=$R/etc/rc.local KIT_STATE=$R/data/luyaz/kit KIT_SHIP_DIR=$R/data/luyaz/ship \
        KIT_SCRIPTS=$R/data/luyaz/scripts KIT_CORE_DIR=$R/data/luyaz/core KIT_SCREEN_DIR=$R/data/luyaz/screen
    unset KIT_FRESH KIT_DF_DATA KIT_DF_ETC KIT_UNDO_SSH UNDO_CRASH_AFTER
}

# path type md5/target for every entry under the root; the kit's own state dir left out
fp() { # [extra grep -v pattern]
    (cd "$R" && find . | sort | while IFS= read -r p; do
        if [ -L "$p" ]; then echo "$p L $(readlink "$p")"
        elif [ -d "$p" ]; then echo "$p D"
        else echo "$p F $(md5sum <"$p" | cut -d' ' -f1) $(ls -l "$p" | cut -c1-10)"; fi
    done) | grep -v "^\./data/luyaz/kit" | { if [ -n "${1:-}" ]; then grep -v "$1"; else cat; fi; }
}

run() { # <component to fail in, empty = none> <args…>
    _bad=$1; shift
    (
        KIT_LIB_ONLY=1 KIT_ID=K1 KIT_MAC_TIME=1700000000
        export KIT_LIB_ONLY KIT_ID KIT_MAC_TIME
        . "$K/device/install.sh"
        KIT=$K P=$K/payload   # sourced: $0 is not the kit's
        stub() { [ "$1" != "$_bad" ] || die "stub $1"; }
        do_ssh() {
            mkdir -p "$KR/data/ssh"; put "$P/blob" "$KR/data/ssh/dropbear" 755
            ln -sf dropbear "$KR/data/ssh/dropbearkey"; echo key >"$KR/data/ssh/authorized_keys"
            echo '#!/bin/sh' >"$KR/data/local/tmp/start_dropbear.sh"
            stub ssh
            rc_add "sh /data/local/tmp/start_dropbear.sh" "start_dropbear.sh"
        }
        do_admin() {
            mkdir -p "$S" "$CORE_DIR" "$SCREEN_DIR"; put "$P/blob" "$S/doctor.sh" 755
            for s in luyaz-core luyaz-sentinel luyaz-web; do put "$P/blob" "$KR/etc/init.d/$s" 755; done
            put "$P/blob" "$KR/data/luyaz/luyaz-web" 755; echo pw >"$KR/data/luyaz/web.env"
            stub admin
            rc_add "/etc/init.d/luyaz-web start" "/etc/init.d/luyaz-web start"
            mkdir -p "$SHIPD"; echo '{}' >>"$SHIPD/manifest.jsonl"
        }
        do_ui() {
            mkdir -p "$KR/data/luyaz/ui/fonts" "$CORE_DIR/plugins"
            put "$P/blob" "$KR/data/luyaz/ui/luyaz-ui" 755; put "$P/blob" "$KR/data/luyaz/ui/fonts/a.ttf" 644
            put "$P/blob" "$KR/data/luyaz/luyaz-core" 755; put "$P/blob" "$KR/data/luyaz/luyaz-sentinel" 755
            stub ui
            rc_add "/etc/init.d/luyaz-core start" "/etc/init.d/luyaz-core start"
        }
        do_esim() { rm -rf "$KR/data/esim"; mkdir -p "$KR/data/esim"; echo 'new lpac' >"$KR/data/esim/lpac.sh"; stub esim; }
        do_chill() {
            mkdir -p "$KR/data/chill/bin"; put "$P/blob" "$KR/data/chill/bin/mihomo" 755
            put "$P/blob" "$KR/etc/init.d/chill" 755
            printf "config zone\n\toption name 'chill'\n" >>"$KR/etc/config/firewall"
            stub chill
            rc_add "[ -f /data/chill/disabled ] || /etc/init.d/chill start" "/etc/init.d/chill start"
        }
        if [ -n "${UNDO_CRASH_AFTER:-}" ]; then   # a crash after n renames
            _mvn=0
            mv() { _mvn=$((_mvn + 1)); [ "$_mvn" -le "$UNDO_CRASH_AFTER" ] || exit 137; command mv "$@"; }
        fi
        set -- "$@"
        eval "$MAIN"
    ) >"$T/out" 2>&1
    RC=$?
}

ALL="ssh admin ui esim chill"
echo "== kit: snapshot + undo =="

k=0
for c in $ALL; do
    k=$((k + 1))
    setup
    fp >"$T/fp0"; fp "data/ssh\|start_dropbear\|etc/rc.local" >"$T/fp0s"
    run "$c" $ALL
    check "die at component $k ($c): install failed, snapshot pending" '[ $RC != 0 ] && grep -qx state=pending "$R/data/luyaz/kit/snap/state"'
    KIT_UNDO_SSH=1 run "" undo
    check "die at $k ($c) → undo --ssh: fingerprint = before" '[ $RC = 0 ] && fp >"$T/fp1" && cmp -s "$T/fp0" "$T/fp1"'
    check "die at $k ($c): no progress left, state undone" '[ ! -e "$R/data/luyaz/kit/progress" ] && grep -qx state=undone "$R/data/luyaz/kit/snap/state"'
    rm -rf "$T"
done

# full install, then the default undo: SSH stays, the rest goes back
setup
fp "data/ssh\|start_dropbear\|etc/rc.local" >"$T/fp0s"
cp "$R/etc/rc.local" "$T/rc0"
run "" $ALL
check "full install ok" '[ $RC = 0 ] && [ -x "$R/data/luyaz/luyaz-core" ]'
run "" undo
check "undo (default): all but SSH as before" '[ $RC = 0 ] && fp "data/ssh\|start_dropbear\|etc/rc.local" >"$T/fp1" && cmp -s "$T/fp0s" "$T/fp1"'
check "undo (default): SSH kept, rc.local = stock + the dropbear line, parses" '[ -x "$R/data/ssh/dropbear" ] && [ -f "$R/data/local/tmp/start_dropbear.sh" ] && sh -n "$R/etc/rc.local" && [ "$(grep -v start_dropbear "$R/etc/rc.local" | md5sum)" = "$(md5sum <"$T/rc0")" ] && [ "$(grep -c start_dropbear "$R/etc/rc.local")" = 1 ]'
rm -rf "$T"

# undo killed after n renames, run again → same result
for n in 1 2 3 4 5 6 7; do
    setup
    fp >"$T/fp0"
    run "" $ALL
    KIT_UNDO_SSH=1 UNDO_CRASH_AFTER=$n run "" undo
    _crc=$RC
    KIT_UNDO_SSH=1 run "" undo
    [ $_crc != 0 ] || { echo "  (undo did fewer than $n renames)"; rm -rf "$T"; break; }
    check "undo killed after $n renames, undo again: fingerprint = before" '[ $RC = 0 ] && fp >"$T/fp1" && cmp -s "$T/fp0" "$T/fp1"'
    rm -rf "$T"
done

# not enough space: nothing written (the kit's state dir included)
for v in "KIT_DF_DATA=100" "KIT_DF_ETC=100"; do
    setup
    fp >"$T/fp0"; (cd "$R" && find . | sort) >"$T/all0"
    eval "export $v"
    run "" $ALL
    unset KIT_DF_DATA KIT_DF_ETC
    check "$v: refused, nothing written" '[ $RC != 0 ] && grep -q "什么都没装" "$T/out" && (cd "$R" && find . | sort) >"$T/all1" && cmp -s "$T/all0" "$T/all1" && fp >"$T/fp1" && cmp -s "$T/fp0" "$T/fp1"'
    rm -rf "$T"
done

# a snapshot rc.local that does not parse: undo changes nothing
setup
run "" $ALL
fp >"$T/fpi"
_i=$(awk -v p="$KIT_RC" '$2 == p {print $1}' "$R/data/luyaz/kit/snap/targets")
echo 'if then' >"$R/data/luyaz/kit/snap/before/$_i"
run "" undo
check "bad snapshot rc.local: undo refused, nothing changed" '[ $RC != 0 ] && fp >"$T/fp1" && cmp -s "$T/fpi" "$T/fp1" && grep -qx state=pending "$R/data/luyaz/kit/snap/state"'
rm -rf "$T"

# a second fresh run (KIT_FRESH=1) keeps the first snapshot; no snapshot → undo refuses
setup
fp >"$T/fp0"
run "" ssh admin
KIT_FRESH=1 run "" $ALL
check "KIT_FRESH=1 later: first snapshot kept" '[ $RC = 0 ] && [ ! -e "$R/data/luyaz/kit/snap/before/$(awk -v p="$R/data/luyaz/luyaz-web" "\$2 == p {print \$1}" "$R/data/luyaz/kit/snap/targets")" ]'
KIT_UNDO_SSH=1 run "" undo
check "… and undo goes back to before the first install" '[ $RC = 0 ] && fp >"$T/fp1" && cmp -s "$T/fp0" "$T/fp1"'
rm -rf "$T"
setup
run "" undo
check "no snapshot: undo refused" '[ $RC != 0 ] && grep -q "没有完整的装机前快照" "$T/out"'
rm -rf "$T"

if command -v busybox >/dev/null 2>&1; then busybox sh -n "$ONB/device/install.sh" && ok "busybox sh -n install.sh" || bad "busybox sh -n install.sh"; fi
echo "kit-undo: $PASS ok, $FAIL failed"
[ $FAIL = 0 ]
