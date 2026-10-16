#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# u60-recover.sh — boot-time clean-up of an interrupted `u60 ship` transaction
# on the U60 Pro (MU5250). Formats and the phase table: docs/SHIP.md.
#
# Installed as /data/luyaz/ship/luyaz-recover.sh and called from rc.local
# before the service start lines:
#     sh /data/luyaz/ship/luyaz-recover.sh; [ $? = 10 ] && exec /bin/sh /etc/rc.local
# It is NOT updated by ship: replacing it is a step of its own (`u60-ship.sh
# install-recover`: sh -n, then this file's `selftest` on the device, and
# the user says yes), like editing rc.local.
#
# What it does, only when the transaction log was written in an earlier boot
# (another boot_id) and stopped in a non-terminal phase:
#   staged / trial              → programs untouched; state files listed go
#                                 back to their .prev (R14; same md5 rules as
#                                 below), phase=aborted
#   promote / check / rollback  → every program, directory and state file
#                                 listed goes back to its .prev (by md5 or
#                                 directory fingerprint: already old →
#                                 skipped; a .prev that is not the old one
#                                 never overwrites anything → phase=failed;
#                                 a program that did not exist before is
#                                 deleted), else phase=rolledback
#   manifest                    → files untouched, phase=manifest_pending
# The .prev of <path> is <path>.prev-<txn>, except /etc/init.d/<name>, whose
# is /data/luyaz/ship/prev/etc.init.d.<name>.prev-<txn> (never read from the
# log), and /etc/rc.local's is /data/luyaz/ship/prev/etc.rc.local.prev-<txn>.
# Log formats: v=1 (phase one), v=2 (dir= lines, programs marked "-" = did not
# exist, /etc/init.d programs) and v=3 (v=2 plus the sentinel's files:
# /etc/init.d/luyaz-sentinel and /etc/rc.local, manager docs/designs/
# u60-sentinel.md 5d-0) and v=4 (v=3 plus /etc/init.d/luyaz-web, manager
# docs/designs/u60-web.md step 6) and v=5 (v=4 plus /etc/init.d/luyaz-core,
# manager docs/designs/u60-phase10.md 10e); `formats` prints the ones known.
# In the core's new layout (/data/luyaz/core/.migrated, 10e) it never puts back
# an rc.local that starts /etc/init.d/zwrt-datad or a core program at the
# old path: there is no way back (the old layout is retired).
# Anything else (no log, terminal phase, same boot) returns at once without
# writing a byte. A log it does not understand (version, no final "end=1"
# line, fields, paths outside /data, a v=2 feature in a v=1 log) is left
# alone, with one line in recover.log.
# Exit status: 0, or 10 when it put /etc/rc.local back (the rc.local the
# sentinel component writes re-runs itself then: `exec /bin/sh /etc/rc.local`).
# No arithmetic, no `.`, no network: a bad file must not stop the boot.
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────

ROOT=${U60R_ROOT:-}
# Luya-Z rename D4: the ship family's directory (u60-ship.sh move-d4 moved it there; the old
# /data/u60-ship/u60-recover.sh, if any, is a shim that hands over)
DIR=${U60R_DIR:-$ROOT/data/luyaz/ship}
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

# tree_fp <dir>: the directory fingerprint (docs/SHIP.md): "<md5> <path>"
# per regular file (path without "./", LC_ALL=C order of the paths, one
# newline each), md5 of the whole text. Fails (1, nothing printed) for a
# missing directory or a link to one, anything but files and directories
# inside, names with a newline or "|", a file md5sum cannot read. The same
# function is in u60-ship.sh and doctor.sh (scripts/test/tree-fp).
tree_fp() {
    [ -d "$1" ] && [ ! -L "$1" ] || return 1
    (
        cd "$1" 2>/dev/null || exit 1
        [ -z "$(find . ! -type f ! -type d 2>/dev/null | head -n 1)" ] || exit 1
        [ -z "$(find . -name '*|*' 2>/dev/null | head -n 1)" ] || exit 1
        [ -z "$(find . -name "*$NL*" 2>/dev/null | head -n 1)" ] || exit 1
        _tn=$(find . -type f 2>/dev/null | wc -l | tr -dc 0-9)
        _tl=$(find . -type f -exec md5sum {} + 2>/dev/null |
            sed -n 's/^\([0-9a-f]\{32\}\)  \.\/\(.*\)$/\2|\1/p' | LC_ALL=C sort -t '|' -k 1,1)
        [ "$(printf '%s' "$_tl" | grep -c '|' | tr -dc 0-9)" = "${_tn:-x}" ] || exit 1
        printf '%s' "$_tl" | awk -F '|' 'NF { print $2 " " $1 }' | md5sum | cut -d' ' -f1
    )
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
        "$ROOT"/etc/init.d/luyaz-sentinel | "$ROOT"/etc/rc.local) return 0 ;;
        "$ROOT"/etc/init.d/luyaz-web) return 0 ;;
        "$ROOT"/etc/init.d/luyaz-core) return 0 ;;
    esac
    return 1
}

# luyaz_new ROOT: the core's layout switch (same line in u60-ship.sh; scripts/test/luyaz-dir)
luyaz_new() { [ -f "$1/data/luyaz/core/.migrated" ]; }

# v5_path <path>: a file only a v=5 log may name
v5_path() {
    case "$1" in
        "$ROOT"/etc/init.d/luyaz-core) return 0 ;;
    esac
    return 1
}

# v4_path <path>: a file only a v=4 log may name
v4_path() {
    case "$1" in
        "$ROOT"/etc/init.d/luyaz-web) return 0 ;;
    esac
    return 1
}

# v3_path <path>: a file only a v=3 log may name
v3_path() {
    case "$1" in
        "$ROOT"/etc/init.d/luyaz-sentinel | "$ROOT"/etc/rc.local) return 0 ;;
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

# prev_of <path>: where ship kept the version before $TXN (docs/SHIP.md).
prev_of() {
    case "$1" in
        "$ROOT"/etc/init.d/*) echo "$DIR/prev/etc.init.d.${1##*/}.prev-$TXN" ;;
        "$ROOT"/etc/rc.local) echo "$DIR/prev/etc.rc.local.prev-$TXN" ;;
        *) echo "$1.prev-$TXN" ;;
    esac
}

# dir_ok <path>: the directories ship may replace (the admin web pages)
dir_ok() {
    path_ok "$1" || return 1
    case "$1" in
        "$ROOT"/data/admin) return 0 ;;
    esac
    return 1
}

# restore_one <path> <old md5 or ->: put the old version back from its .prev
# (the name is derived here, never read from the log).
restore_one() {
    if [ "$2" = - ]; then
        [ -e "$1" ] || [ -L "$1" ] || return 0
        # round ③: luyaz-ui is the only touch UI; deleting it would leave the screen empty
        if [ "$1" = "$ROOT/data/luyaz/ui/luyaz-ui" ] && [ ! -f "$ROOT/data/plugins/u60pro-devui/u60pro-devui" ]; then
            log "！！不删 $1：旧界面已经删了（Phase 10 R5），删了屏幕上就没有界面，正式文件没动"
            return 1
        fi
        rm -f "$1" || return 1
        $SYNC
        log "还原：$1 原来不存在，删掉"
        return 0
    fi
    [ "$(md5_of "$1")" = "$2" ] && return 0
    _prev=$(prev_of "$1")
    # Phase 10 R4/R5 round ③: the old touch UI is deleted, so never a sentinel that would start it
    # (one that does not know /tmp/luyaz-screen.ctl, from before round ①)
    if [ "$1" = "$ROOT/data/luyaz/luyaz-sentinel" ] && [ -f "$_prev" ] && ! grep -q /tmp/luyaz-screen.ctl "$_prev" 2>/dev/null; then
        log "！！不还原 $1：那一版哨兵是 Phase 10 R4 之前的，会去起已删的旧界面，正式文件没动"
        return 1
    fi
    # 10e (D4/D7): moved = the old core's path and init and an rc.local that starts it stay as they are
    if luyaz_new "$ROOT"; then
        case "$1" in
            # (/etc/init.d/zwrt-datad may come back: a copy from 10e on refuses to start once moved)
            "$ROOT"/data/plugins/zwrt-datad/zwrt-datad)
                log "！！不还原 $1：核心已经搬到 /data/luyaz（.migrated），旧路径不放回（不能退回旧布局，见 docs/designs/u60-phase10.md），正式文件没动"
                return 1 ;;
        esac
        if [ "$1" = "$ROOT/etc/rc.local" ] && grep -q '^[[:space:]]*/etc/init.d/zwrt-datad start' "$_prev" 2>/dev/null; then
            log "！！不还原 $1：那一份起 /etc/init.d/zwrt-datad，核心已经搬到 luyaz-core（不能退回旧布局，见 docs/designs/u60-phase10.md），正式文件没动"
            return 1
        fi
    fi
    if [ "$(md5_of "$_prev")" != "$2" ]; then
        log "！！还原不了 $1：$_prev 不在或 md5 不对，正式文件没动"
        return 1
    fi
    # Phase 10 10c (review D7): never an rc.local that starts u60-uid/u60-guard
    # once they are deleted: it would start no screen owner, every boot
    if [ "$1" = "$ROOT/etc/rc.local" ] && grep -q '^/etc/init.d/u60-\(uid\|guard\) start' "$_prev" &&
        ! { [ -x "$ROOT/data/plugins/u60pro-devui/u60-uid" ] && [ -f "$ROOT/data/u60-guard/u60-guard.sh" ]; }; then
        log "！！不还原 $1：那一份起 u60-uid + u60-guard，它们已经删了，正式文件没动"
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

# restore_dir <path> <old fingerprint>: the directory back from
# <path>.prev-<txn> (idempotent: a power cut anywhere in here is finished by
# the next run). Already old → nothing; the .prev not the old one → 1,
# nothing touched.
restore_dir() {
    if [ "$(tree_fp "$1")" = "$2" ]; then
        # already old; leftovers of a restore cut short after its last mv
        rm -rf "$1.rec-tmp" "$1.bad-$TXN"
        return 0
    fi
    _prev=$1.prev-$TXN
    if [ "$(tree_fp "$_prev")" != "$2" ]; then
        log "！！还原不了 $1：$_prev 不在或指纹不对，正式目录没动"
        return 1
    fi
    rm -rf "$1.rec-tmp"
    cp -a "$_prev" "$1.rec-tmp" 2>/dev/null || {
        rm -rf "$1.rec-tmp"
        return 1
    }
    $SYNC
    if [ "$(tree_fp "$1.rec-tmp")" != "$2" ]; then
        rm -rf "$1.rec-tmp"
        return 1
    fi
    rm -rf "$1.bad-$TXN"
    if [ -e "$1" ] || [ -L "$1" ]; then
        mv -f "$1" "$1.bad-$TXN" || return 1
    fi
    mv -f "$1.rec-tmp" "$1" || return 1
    $SYNC
    rm -rf "$1.bad-$TXN"
    log "还原：$1 ← ${_prev##*/}"
}

recover() {
    [ -f "$TXN_FILE" ] || return 0
    V= TXN= PHASE= BOOT= FILES= STATES= DIRS= END=
    while IFS= read -r _l; do
        case "$_l" in
            end=1) END=1 ;;
            v=*) V=${_l#v=} ;;
            txn=*) TXN=${_l#txn=} ;;
            phase=*) PHASE=${_l#phase=} ;;
            boot_id=*) BOOT=${_l#boot_id=} ;;
            file=*) FILES="$FILES${_l#file=}$NL" ;;
            dir=*) DIRS="$DIRS${_l#dir=}$NL" ;;
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

    case "$V" in
        1 | 2 | 3 | 4 | 5) ;;
        *)
            log "事务日志版本 “$V” 看不懂，不动"
            return 0
            ;;
    esac
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
    for _e in $DIRS; do
        IFS=$_ifs
        _p=${_e%%|*}
        _r=${_e#*|}
        if ! dir_ok "$_p" || ! is_md5 "${_r%%|*}"; then
            log "事务 $TXN 的目录一行看不懂或不是能换的目录（$(printf '%s' "$_e" | cut -c1-120)），不动"
            return 0
        fi
    done
    IFS=$_ifs
    if [ "$V" != 3 ] && [ "$V" != 4 ] && [ "$V" != 5 ]; then
        IFS=$NL
        for _e in $FILES $STATES; do
            IFS=$_ifs
            if v3_path "${_e%%|*}"; then
                log "事务 $TXN：v=$V 的日志里有只有 v=3 才能写的文件（${_e%%|*}），不动"
                return 0
            fi
        done
        IFS=$_ifs
    fi
    if [ "$V" != 4 ] && [ "$V" != 5 ]; then
        IFS=$NL
        for _e in $FILES $STATES; do
            IFS=$_ifs
            if v4_path "${_e%%|*}"; then
                log "事务 $TXN：v=$V 的日志里有只有 v=4 才能写的文件（${_e%%|*}），不动"
                return 0
            fi
        done
        IFS=$_ifs
    fi
    if [ "$V" != 5 ]; then
        IFS=$NL
        for _e in $FILES $STATES; do
            IFS=$_ifs
            if v5_path "${_e%%|*}"; then
                log "事务 $TXN：v=$V 的日志里有只有 v=5 才能写的文件（${_e%%|*}），不动"
                return 0
            fi
        done
        IFS=$_ifs
    fi
    if [ "$V" = 1 ]; then
        # v=2 features in a v=1 log: not something phase one wrote
        if [ -n "$DIRS" ]; then
            log "事务 $TXN：v=1 的日志里有 dir= 行，不动"
            return 0
        fi
        for _e in $FILES; do
            case "${_e#*|}" in -*) log "事务 $TXN：程序文件不能标成原来不存在（v=1），不动"; return 0 ;; esac
        done
    fi

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
            _rc0=$(md5_of "$ROOT/etc/rc.local")
            IFS=$NL
            for _e in $FILES; do
                IFS=$_ifs
                _p=${_e%%|*}
                _r=${_e#*|}
                restore_one "$_p" "${_r%%|*}" || _bad="$_bad ${_p##*/}"
            done
            IFS=$NL
            for _e in $DIRS; do
                IFS=$_ifs
                _p=${_e%%|*}
                _r=${_e#*|}
                restore_dir "$_p" "${_r%%|*}" || _bad="$_bad ${_p##*/}/"
            done
            IFS=$NL
            for _e in $STATES; do
                IFS=$_ifs
                _p=${_e%%|*}
                _r=${_e#*|}
                restore_one "$_p" "${_r%%|*}" || _bad="$_bad ${_p##*/}"
            done
            IFS=$_ifs
            # rc.local itself went back: the shell running it has the new one open and would go
            # on with its lines (start a sentinel whose init is gone). The sentinel's rc.local
            # runs us as `…u60-recover.sh; [ $? = 10 ] && exec /bin/sh /etc/rc.local`.
            [ "$(md5_of "$ROOT/etc/rc.local")" != "$_rc0" ] && RC_CHANGED=1
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
        mkdir -p "$_sb/root/data/luyaz/ship" "$_sb/root/data/x"
        printf 'old\n' >"$_sb/o"
        printf 'new\n' >"$_sb/n"
        _om=$(md5_of "$_sb/o")
        _nm=$(md5_of "$_sb/n")
        printf '%s' "$3" >"$_sb/root/data/x/prog"
        [ "$4" = none ] || printf '%s' "$4" >"$_sb/root/data/x/prog.prev-20000101-000000-st"
        printf 'v=1\ntxn=20000101-000000-st\ncomp=st\nphase=%s\nboot_id=old-boot\nfile=%s|%s|%s\nend=1\n' \
            "$2" "$_sb/root/data/x/prog" "$_om" "$_nm" >"$_sb/root/data/luyaz/ship/txn"
        echo new-boot >"$_sb/boot"
        (
            export U60R_ROOT="$_sb/root" U60R_BOOT_ID="$_sb/boot"
            unset U60R_DIR
            sh "$SELF"
        ) >/dev/null 2>&1
        _ph=$(sed -n 's/^phase=//p' "$_sb/root/data/luyaz/ship/txn")
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
    # v=2: a directory, a program that did not exist, /etc/init.d's .prev
    t2() { # t2 <case> <phase> <log version> <setup: live|gone|torn> <expect phase> <expect: old|new|dirnew>
        rm -rf "$_sb/root"
        _a=$_sb/root/data/admin
        _g=$_sb/root/etc/init.d/u60-guard
        mkdir -p "$_sb/root/data/luyaz/ship/prev" "$_a/sub" "$_a.prev-20000101-000000-st/sub" "${_g%/*}" "$_sb/root/data/x"
        printf 'old page\n' >"$_a.prev-20000101-000000-st/index.html"
        printf 'old js\n' >"$_a.prev-20000101-000000-st/sub/a b.js"
        _of=$(tree_fp "$_a.prev-20000101-000000-st")
        printf 'new page\n' >"$_a/index.html"
        _nf=$(tree_fp "$_a")
        printf 'old init\n' >"$_sb/root/data/luyaz/ship/prev/etc.init.d.u60-guard.prev-20000101-000000-st"
        printf 'new init\n' >"$_g"
        printf 'old init\n' >"$_sb/o"
        _om=$(md5_of "$_sb/o")
        printf 'brand new\n' >"$_sb/root/data/x/added.sh"
        case "$4" in
            gone) rm -rf "$_a" ;;                                  # between the two renames
            torn) printf 'x\n' >"$_a.prev-20000101-000000-st/extra" ;; # the .prev is not the old one
        esac
        printf 'v=%s\ntxn=20000101-000000-st\ncomp=st\nphase=%s\nboot_id=old-boot\nfile=%s|%s|0123456789abcdef0123456789abcdef\nfile=%s|-|0123456789abcdef0123456789abcdef\ndir=%s|%s|%s\nend=1\n' \
            "$3" "$2" "$_g" "$_om" "$_sb/root/data/x/added.sh" "$_a" "$_of" "${_nf:-0123456789abcdef0123456789abcdef}" >"$_sb/root/data/luyaz/ship/txn"
        echo new-boot >"$_sb/boot"
        (
            export U60R_ROOT="$_sb/root" U60R_BOOT_ID="$_sb/boot"
            unset U60R_DIR
            sh "$SELF"
        ) >/dev/null 2>&1
        _ph=$(sed -n 's/^phase=//p' "$_sb/root/data/luyaz/ship/txn")
        _ok=
        if [ "$6" = old ]; then
            [ "$(tree_fp "$_a")" = "$_of" ] && [ "$(cat "$_g")" = "old init" ] && [ ! -e "$_sb/root/data/x/added.sh" ] &&
                [ ! -e "$_a.rec-tmp" ] && [ ! -e "$_a.bad-20000101-000000-st" ] && _ok=1
        elif [ "$6" = dirnew ]; then
            [ "$(tree_fp "$_a")" = "$_nf" ] && [ ! -e "$_a.rec-tmp" ] && _ok=1
        else
            [ "$(cat "$_g")" = "new init" ] && [ -f "$_sb/root/data/x/added.sh" ] && [ "$(tree_fp "$_a")" = "$_nf" ] && _ok=1
        fi
        if [ "$_ph" = "$5" ] && [ -n "$_ok" ]; then
            _p=$((_p + 1))
            echo "  ok   $1"
        else
            _f=$((_f + 1))
            echo "  FAIL $1（phase $_ph）"
        fi
    }
    t2 "v=2 检查中断电 → 目录、/etc 启动脚本换回，新文件删掉" check 2 live rolledback old
    t2 "v=2 两次改名之间断电（没有正式目录）→ 换回" promote 2 gone rolledback old
    t2 "v=2 目录的 .prev 不对 → 目录不动，failed" check 2 torn failed dirnew
    t2 "v=1 的日志里有 dir= 和新文件 → 一律不动" check 1 live check new
    t2 "v=6 → 不动" check 6 live check new
    # v=3: the sentinel's init and rc.local
    t3() { # t3 <case> <log version> <expect phase> <expect: old|new>
        rm -rf "$_sb/root"
        _rc=$_sb/root/etc/rc.local
        _si=$_sb/root/etc/init.d/luyaz-sentinel
        mkdir -p "$_sb/root/data/luyaz/ship/prev" "${_si%/*}"
        printf 'old rc\n' >"$_sb/root/data/luyaz/ship/prev/etc.rc.local.prev-20000101-000000-st"
        printf 'new rc\n' >"$_rc"
        printf 'new init\n' >"$_si"
        printf 'old rc\n' >"$_sb/o"
        _om=$(md5_of "$_sb/o")
        printf 'v=%s\ntxn=20000101-000000-st\ncomp=sentinel\nphase=check\nboot_id=old-boot\nfile=%s|%s|0123456789abcdef0123456789abcdef\nfile=%s|-|0123456789abcdef0123456789abcdef\nend=1\n' \
            "$2" "$_rc" "$_om" "$_si" >"$_sb/root/data/luyaz/ship/txn"
        echo new-boot >"$_sb/boot"
        (
            export U60R_ROOT="$_sb/root" U60R_BOOT_ID="$_sb/boot"
            unset U60R_DIR
            sh "$SELF"
        ) >/dev/null 2>&1
        _rrc=$?
        _ph=$(sed -n 's/^phase=//p' "$_sb/root/data/luyaz/ship/txn")
        _ok=
        if [ "$4" = old ]; then
            [ "$(cat "$_rc")" = "old rc" ] && [ ! -e "$_si" ] && [ "$_rrc" = 10 ] && _ok=1
        else
            [ "$(cat "$_rc")" = "new rc" ] && [ -f "$_si" ] && [ "$_rrc" = 0 ] && _ok=1
        fi
        if [ "$_ph" = "$3" ] && [ -n "$_ok" ]; then
            _p=$((_p + 1))
            echo "  ok   $1"
        else
            _f=$((_f + 1))
            echo "  FAIL $1（phase $_ph）"
        fi
    }
    t3 "v=3 检查中断电 → rc.local 换回、新装的哨兵 init 删掉、返回 10" 3 rolledback old
    t3 "v=2 的日志里有 rc.local → 一律不动" 2 check new
    # v=4: luyaz-web's init (a new file, as the first scripts ship that carries it puts it)
    t4() { # t4 <case> <log version> <expect phase> <expect: gone|new> [init name]
        rm -rf "$_sb/root"
        _wi=$_sb/root/etc/init.d/${5:-luyaz-web}
        mkdir -p "$_sb/root/data/luyaz/ship/prev" "${_wi%/*}"
        printf 'new init\n' >"$_wi"
        printf 'v=%s\ntxn=20000101-000000-sc\ncomp=scripts\nphase=check\nboot_id=old-boot\nfile=%s|-|0123456789abcdef0123456789abcdef\nend=1\n' \
            "$2" "$_wi" >"$_sb/root/data/luyaz/ship/txn"
        echo new-boot >"$_sb/boot"
        (
            export U60R_ROOT="$_sb/root" U60R_BOOT_ID="$_sb/boot"
            unset U60R_DIR
            sh "$SELF"
        ) >/dev/null 2>&1
        _rrc=$?
        _ph=$(sed -n 's/^phase=//p' "$_sb/root/data/luyaz/ship/txn")
        _ok=
        if [ "$4" = gone ]; then
            [ ! -e "$_wi" ] && [ "$_rrc" = 0 ] && _ok=1
        else
            [ -f "$_wi" ] && [ "$_rrc" = 0 ] && _ok=1
        fi
        if [ "$_ph" = "$3" ] && [ -n "$_ok" ]; then
            _p=$((_p + 1))
            echo "  ok   $1"
        else
            _f=$((_f + 1))
            echo "  FAIL $1（phase $_ph）"
        fi
    }
    t4 "v=4 检查中断电 → 新装的 luyaz-web init 删掉" 4 rolledback gone
    t4 "v=3 的日志里有 luyaz-web 的 init → 一律不动" 3 check new
    t4 "v=5 检查中断电 → 新装的 luyaz-core init 删掉" 5 rolledback gone luyaz-core
    t4 "v=4 的日志里有 luyaz-core 的 init → 一律不动" 4 check new luyaz-core
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
    formats) echo "1 2 3 4 5" ;;
    tree-fp) tree_fp "$2" ;;
    '')
        RC_CHANGED=
        recover
        if [ -n "$RC_CHANGED" ]; then
            log "rc.local 已换回：返回 10，让正在跑的 rc.local 重新执行换回的那一份"
            exit 10
        fi
        ;;
    *)
        echo "usage: $0 [selftest|formats|tree-fp <dir>]" >&2
        exit 2
        ;;
esac
