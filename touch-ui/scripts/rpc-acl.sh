#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# rpc-acl.sh — take the factory/ADB methods out of the stock web ACL's
# not-logged-in list.
#
#   sh /data/luyaz/scripts/rpc-acl.sh status    what is in force (read only)
#   sh /data/luyaz/scripts/rpc-acl.sh check     build the tightened copy and test it, mount nothing
#   sh /data/luyaz/scripts/rpc-acl.sh on        tighten, and keep it tightened at boot
#   sh /data/luyaz/scripts/rpc-acl.sh off       back to the stock list, now and at boot
#   sh /data/luyaz/scripts/rpc-acl.sh boot      rc.local: tighten again if `on` was chosen
#
# Why: the stock rpcd ACL (/usr/share/rpcd/acl.d/web.json → the read-only
# /usr/zte_web/web/copy/web.json) lets the anonymous session reach
# zwrt_mc.device.manager fac_open/fac_close/fac_reset/fac_reboot/fac_query_key
# and adb_switch (CN B31, 2026-10-16): anyone on the LAN could ask for a
# factory reset without the web password. The logged-in "web" list is not
# touched, nor anything else: the stock web UI, after login, does what it did,
# and on-device callers (core, sentinel, touch UI) use ubus directly, not rpcd.
#
# How: the copy is rebuilt from the stock file every time (a firmware upgrade
# brings its own), changing exactly two lines; it is checked before use (it
# parses, "web" is identical, nothing of fac_*/adb_switch is left anonymous,
# get_device_info stays for the login page), bind-mounted over the stock file,
# rpcd reloaded, and the result asked of rpcd with `session access` — never by
# calling the methods. If rpcd does not agree, it is unmounted again.
# doctor's rpc-acl row reads the same `session access` answers.
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
# ─────────────────────────────────────────────────────────────────────────────

DIR=${RPCACL_DIR:-/data/luyaz/rpc-acl}
TARGET=${RPCACL_TARGET:-/usr/zte_web/web/copy/web.json}
MOUNTS=${RPCACL_MOUNTS:-/proc/mounts}
UBUS=${RPCACL_UBUS:-ubus}
JF=${RPCACL_JSONFILTER:-jsonfilter}
RPCD=${RPCACL_RPCD:-/etc/init.d/rpcd}
MOUNT=${RPCACL_MOUNT:-mount}
UMOUNT=${RPCACL_UMOUNT:-umount}
BOOT_WAIT=${RPCACL_BOOT_WAIT:-60}
SID=00000000000000000000000000000000
METHODS="fac_open fac_close fac_reset fac_reboot fac_query_key adb_switch"

say() { echo "rpc-acl: $*"; }

mounted() { grep -q " $TARGET " "$MOUNTS" 2>/dev/null; }

# access <function>: true | false | ? (rpcd's answer for the anonymous session)
access() {
    _a=$($UBUS call session access "{\"ubus_rpc_session\":\"$SID\",\"scope\":\"ubus\",\"object\":\"zwrt_mc.device.manager\",\"function\":\"$1\"}" 2>/dev/null)
    case "$_a" in
        *'"access": true'* | *'"access":true'*) echo true ;;
        *'"access"'*) echo false ;;
        *) echo '?' ;;
    esac
}

# open: the methods the anonymous session can reach ("" = none); 1 if rpcd did not answer
open() {
    _o=
    for _m in $METHODS; do
        case $(access "$_m") in true) _o="$_o $_m" ;; '?') return 1 ;; esac
    done
    echo "${_o# }"
}

# build <stock> <out>: the tightened copy, checked; 0 = good
build() {
    sed -e '/"zwrt_mc\.device\.manager"[[:space:]]*:[[:space:]]*\[[^]]*"fac_query_key"/s/\[[^]]*\]/["get_device_info"]/' \
        -e '/"zwrt_mc\.device\.manager"[[:space:]]*:[[:space:]]*\[[^]]*"fac_reset"/d' "$1" >"$2.tmp" || return 1
    if ! $JF -i "$2.tmp" -e '@' >/dev/null 2>&1; then say "the copy does not parse"; rm -f "$2.tmp"; return 1; fi
    if [ "$($JF -i "$1" -e '@.web')" != "$($JF -i "$2.tmp" -e '@.web')" ]; then
        say "the logged-in list would change"; rm -f "$2.tmp"; return 1
    fi
    _u=$($JF -i "$2.tmp" -e '@.unauthenticated')
    case "$_u" in *fac_* | *adb_switch*) say "fac_*/adb_switch still anonymous (stock file has another shape)"; rm -f "$2.tmp"; return 1 ;; esac
    case "$_u" in *get_device_info*) ;; *) say "get_device_info gone (the login page needs it)"; rm -f "$2.tmp"; return 1 ;; esac
    mv "$2.tmp" "$2"
}

# reload rpcd until it gives the wanted answer: tight | stock
settle() {
    for _how in reload restart; do
        $RPCD "$_how" >/dev/null 2>&1
        _i=0
        while [ $_i -lt 10 ]; do
            sleep 1
            if _o=$(open); then
                case "$1" in
                    tight) [ -z "$_o" ] && return 0 ;;
                    stock) [ -n "$_o" ] && return 0 ;;
                esac
            fi
            _i=$((_i + 1))
        done
    done
    return 1
}

tighten() {
    mkdir -p "$DIR" || return 1
    if ! mounted; then
        cp "$TARGET" "$DIR/web.json.stock" || return 1
        build "$DIR/web.json.stock" "$DIR/web.json.tight" || return 1
        $MOUNT -o bind "$DIR/web.json.tight" "$TARGET" || { say "mount failed"; return 1; }
    fi
    if settle tight; then
        say "tightened: the anonymous session reaches none of $METHODS"
        return 0
    fi
    say "rpcd still answers with the stock list; unmounting"
    $UMOUNT "$TARGET" 2>/dev/null
    $RPCD reload >/dev/null 2>&1
    return 1
}

case "$1" in
    status)
        [ -f "$DIR/enabled" ] && echo "chosen: on" || echo "chosen: off"
        mounted && echo "mounted: yes" || echo "mounted: no"
        if o=$(open); then echo "anonymous: ${o:-none}"; else echo "anonymous: unreadable"; fi
        ;;
    check)
        mkdir -p "$DIR" || exit 1
        src=$TARGET; mounted && src=$DIR/web.json.stock
        if build "$src" "$DIR/web.json.check"; then
            say "copy OK ($DIR/web.json.check); differences:"
            diff "$src" "$DIR/web.json.check"
            exit 0
        fi
        exit 1
        ;;
    on)
        tighten || exit 1
        touch "$DIR/enabled"
        ;;
    off)
        rm -f "$DIR/enabled"
        if mounted; then
            $UMOUNT "$TARGET" || { say "umount failed"; exit 1; }
        fi
        if settle stock; then say "back to the stock list"; else say "rpcd does not answer as expected; check: $0 status"; exit 1; fi
        ;;
    boot)
        [ -f "$DIR/enabled" ] || exit 0
        i=0
        while ! open >/dev/null; do
            i=$((i + 1)); [ $i -ge "$BOOT_WAIT" ] && { say "rpcd not up after ${BOOT_WAIT}s"; exit 1; }
            sleep 1
        done
        tighten
        ;;
    *)
        sed -n '5,9p' "$0"
        exit 2
        ;;
esac
