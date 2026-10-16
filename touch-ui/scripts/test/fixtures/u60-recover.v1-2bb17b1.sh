#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# u60-recover.sh — boot-time clean-up of an interrupted `u60 ship` transaction
# on the U60 Pro (MU5250). Formats and the phase table: docs/SHIP.md.
#
# Installed once as /data/u60-ship/u60-recover.sh and called from rc.local
# before the zte-agent / zwrt-datad / u60-guard / u60-uid start lines:
#     sh /data/u60-ship/u60-recover.sh
# It is NOT updated by ship: replacing it is a step of its own (sh -n, then
# `sh u60-recover.sh selftest` on the device, and the user says yes), like
# editing rc.local.
#
# What it does, only when the transaction log was written in an earlier boot
# (another boot_id) and stopped in a non-terminal phase:
#   staged / trial              → programs untouched; state files listed go
#                                 back to <path>.prev-<txn> (R14; same md5
#                                 rules as below), phase=aborted
#   promote / check / rollback  → every program and state file listed goes
#                                 back to <path>.prev-<txn> (by md5: already
#                                 old → skipped; a .prev with the wrong md5
#                                 never overwrites anything → phase=failed),
#                                 else phase=rolledback
#   manifest                    → files untouched, phase=manifest_pending
# Anything else (no log, terminal phase, same boot) returns at once without
# writing a byte. A log it does not understand (version, no final "end=1"
# line, fields, paths outside /data) is left alone, with one line in
# recover.log.
# No arithmetic, no `.`, no network: a bad file must not stop the boot.
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────

ROOT=${U60R_ROOT:-}
DIR=${U60R_DIR:-$ROOT/data/u60-ship}
TXN_FILE=$DIR/txn
LOG=$DIR/recover.log
BOOT_ID_FILE=${U60R_BOOT_ID:-/proc/sys/kernel/random/boot_id}
SYNC=${U60R_SYNC:-sync}
NL='
'

log() { echo "$(date '+%Y-%m-%d %H:%M:%S' 2>/dev/null) $*" >>"$LOG" 2>/dev/null; }

md5_of() {
    [ -f "$1" ] || return 0
    md5sum "$1" 2>/dev/null | cut -d' ' -f1
}

is_md5() {
    case "$1" in
        '' | *[!0-9a-f]*) return 1 ;;
    esac
    [ "${#1}" = 32 ]
}

# same rule as u60-ship.sh's path_ok
path_ok() {
    case "$1" in
        '' | *[!A-Za-z0-9._/-]*) return 1 ;;
        *//* | */./* | */../* | */. | */.. | */) return 1 ;;
        "$ROOT"/data/?*) return 0 ;;
        "$ROOT"/etc/init.d/zte-agent | "$ROOT"/etc/init.d/zwrt-datad | "$ROOT"/etc/init.d/u60-guard | "$ROOT"/etc/init.d/u60-uid) return 0 ;;
    esac
    return 1
}

txn_ok() {
    case "$1" in
        [0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]-[0-9][0-9][0-9][0-9][0-9][0-9]-*) ;;
        *) return 1 ;;
    esac
    _s=${1#*-*-}
    case "$_s" in
        '' | *[!a-z0-9]*) return 1 ;;
    esac
    [ "${#_s}" -le 12 ]
}

# set_phase <phase> <reason>: rewrite the log (temp → sync → mv → sync),
# every other line kept as it is.
set_phase() {
    _t=$TXN_FILE.rec-tmp
    {
        grep -v -e '^phase=' -e '^reason=' -e '^t_phase=' -e '^end=' "$TXN_FILE"
        echo "phase=$1"
        echo "reason=$2"
        echo "t_phase=$(cut -d. -f1 /proc/uptime 2>/dev/null)"
        echo end=1
    } >"$_t" 2>/dev/null || {
        rm -f "$_t"
        log "！！事务日志写不上"
        return 1
    }
    $SYNC
    mv -f "$_t" "$TXN_FILE" || return 1
    $SYNC
    log "事务 $TXN：$1（$2）"
}

# restore_one <path> <old md5 or ->: put the old version back from
# <path>.prev-<txn> (the name is derived here, never read from the log).
restore_one() {
    if [ "$2" = - ]; then
        [ -e "$1" ] || return 0
        rm -f "$1" || return 1
        $SYNC
        log "还原：$1 原来不存在，删掉"
        return 0
    fi
    [ "$(md5_of "$1")" = "$2" ] && return 0
    _prev=$1.prev-$TXN
    if [ "$(md5_of "$_prev")" != "$2" ]; then
        log "！！还原不了 $1：$_prev 不在或 md5 不对，正式文件没动"
        return 1
    fi
    cp -p "$_prev" "$1.rec-tmp" 2>/dev/null || {
        rm -f "$1.rec-tmp"
        return 1
    }
    $SYNC
    if [ "$(md5_of "$1.rec-tmp")" != "$2" ]; then
        rm -f "$1.rec-tmp"
        return 1
    fi
    mv -f "$1.rec-tmp" "$1" || return 1
    $SYNC
    log "还原：$1 ← ${_prev##*/}"
}

recover() {
    [ -f "$TXN_FILE" ] || return 0
    V= TXN= PHASE= BOOT= FILES= STATES= END=
    while IFS= read -r _l; do
        case "$_l" in
            end=1) END=1 ;;
            v=*) V=${_l#v=} ;;
            txn=*) TXN=${_l#txn=} ;;
            phase=*) PHASE=${_l#phase=} ;;
            boot_id=*) BOOT=${_l#boot_id=} ;;
            file=*) FILES="$FILES${_l#file=}$NL" ;;
            state=*) STATES="$STATES${_l#state=}$NL" ;;
        esac
    done <<EOF
$(head -n 400 "$TXN_FILE" 2>/dev/null)
EOF
    # the normal boot: nothing to do, nothing written
    case "$PHASE" in
        done | rolledback | aborted | manifest_pending | failed) return 0 ;;
    esac
    _now=$(tr -dc '0-9a-f-' <"$BOOT_ID_FILE" 2>/dev/null)
    [ -n "$_now" ] && [ "$BOOT" = "$_now" ] && return 0

    if [ "$V" != 1 ]; then
        log "事务日志版本 “$V” 看不懂，不动"
        return 0
    fi
    if [ "$END" != 1 ]; then
        log "事务日志不完整（没有最后一行 end=1），不动"
        return 0
    fi
    if ! txn_ok "$TXN"; then
        log "事务号 “$TXN” 格式不对，不动"
        return 0
    fi
    # every entry is checked before anything is touched
    _ifs=$IFS
    IFS=$NL
    for _e in $FILES $STATES; do
        IFS=$_ifs
        _p=${_e%%|*}
        _r=${_e#*|}
        _o=${_r%%|*}
        if ! path_ok "$_p" || { ! is_md5 "$_o" && [ "$_o" != - ]; }; then
            log "事务 $TXN 的一行看不懂或路径越界（$(printf '%s' "$_e" | cut -c1-120)），不动"
            return 0
        fi
    done
    IFS=$_ifs
    for _e in $FILES; do
        case "${_e#*|}" in -*) log "事务 $TXN：程序文件不能标成原来不存在，不动"; return 0 ;; esac
    done

    case "$PHASE" in
        staged | trial)
            # programs never left the live slot; the test build may have
            # written the state files: those go back (R14)
            _bad=
            IFS=$NL
            for _e in $STATES; do
                IFS=$_ifs
                _p=${_e%%|*}
                _r=${_e#*|}
                restore_one "$_p" "${_r%%|*}" || _bad="$_bad ${_p##*/}"
            done
            IFS=$_ifs
            if [ -n "$_bad" ]; then
                set_phase failed "开机时中止不完整（断电时在 $PHASE）：状态文件$_bad 没换回，要人处理"
            else
                set_phase aborted "开机时已中止（断电时在 $PHASE，程序没动过，状态文件已换回试跑前）"
            fi
            ;;
        manifest)
            set_phase manifest_pending "开机时发现清单没写完：新版已通过检查，清单待补"
            ;;
        promote | check | rollback)
            _bad=
            IFS=$NL
            for _e in $FILES $STATES; do
                IFS=$_ifs
                _p=${_e%%|*}
                _r=${_e#*|}
                restore_one "$_p" "${_r%%|*}" || _bad="$_bad ${_p##*/}"
            done
            IFS=$_ifs
            if [ -n "$_bad" ]; then
                set_phase failed "开机时退回不完整（断电时在 $PHASE）：$_bad 没换回，要人处理"
            else
                set_phase rolledback "开机时已退回（断电时在 $PHASE）"
            fi
            ;;
        *)
            log "事务 $TXN 的阶段 “$PHASE” 看不懂，不动"
            ;;
    esac
    return 0
}

# selftest: every phase, the torn cases and the no-op boots, in a sandbox.
selftest() {
    _p=0
    _f=0
    SELF=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")
    _sb=$(mktemp -d "${TMPDIR:-/tmp}/u60-recover-selftest.XXXXXX") || {
        echo "selftest: FAIL（mktemp）"
        return 1
    }
    t() { # t <case> <phase before> <prog content> <prev content> <expect phase> <expect prog>
        rm -rf "$_sb/root"
        mkdir -p "$_sb/root/data/u60-ship" "$_sb/root/data/x"
        printf 'old\n' >"$_sb/o"
        printf 'new\n' >"$_sb/n"
        _om=$(md5_of "$_sb/o")
        _nm=$(md5_of "$_sb/n")
        printf '%s' "$3" >"$_sb/root/data/x/prog"
        [ "$4" = none ] || printf '%s' "$4" >"$_sb/root/data/x/prog.prev-20000101-000000-st"
        printf 'v=1\ntxn=20000101-000000-st\ncomp=st\nphase=%s\nboot_id=old-boot\nfile=%s|%s|%s\nend=1\n' \
            "$2" "$_sb/root/data/x/prog" "$_om" "$_nm" >"$_sb/root/data/u60-ship/txn"
        echo new-boot >"$_sb/boot"
        (
            export U60R_ROOT="$_sb/root" U60R_BOOT_ID="$_sb/boot"
            unset U60R_DIR
            sh "$SELF"
        ) >/dev/null 2>&1
        _ph=$(sed -n 's/^phase=//p' "$_sb/root/data/u60-ship/txn")
        _pc=$(cat "$_sb/root/data/x/prog")
        if [ "$_ph" = "$5" ] && [ "$_pc" = "$6" ]; then
            _p=$((_p + 1))
            echo "  ok   $1"
        else
            _f=$((_f + 1))
            echo "  FAIL $1（phase $_ph，文件 “$_pc”）"
        fi
    }
    t "试跑中断电 → 已中止，程序不动" trial "old$NL" none aborted old
    t "转正中断电 → 换回旧版" promote "new$NL" "old$NL" rolledback old
    t "检查中断电 → 换回旧版" check "new$NL" "old$NL" rolledback old
    t "正式文件 0 字节 → 换回旧版" promote "" "old$NL" rolledback old
    t "已是旧版 → 跳过" promote "old$NL" none rolledback old
    t ".prev 截断 → 不碰正式文件，failed" check "new$NL" "ol" failed new
    t "写清单中断电 → 清单待补" manifest "new$NL" "old$NL" manifest_pending new
    t "完成 → 不动" done "new$NL" "old$NL" done new
    rm -rf "$_sb"
    if [ $_f = 0 ]; then
        echo "selftest: PASS（$_p 项）"
        return 0
    fi
    echo "selftest: FAIL（$_f 项不过，$_p 项通过）"
    return 1
}

case "$1" in
    selftest) selftest ;;
    '') recover ;;
    *)
        echo "usage: $0 [selftest]" >&2
        exit 2
        ;;
esac
