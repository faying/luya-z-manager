#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# u60-ship.sh — the device side of `u60 ship` on the U60 Pro (MU5250):
# side-by-side trial, promotion into the live slot, post-promotion check and
# rollback, as one transaction that survives a lost SSH session and a sudden
# reboot. Formats (stage dir, transaction log, manifest): docs/SHIP.md.
#
# Two entry points share this file:
#   sh u60-ship.sh <command> …     ship commands (see ship_usage below)
#   sh datad-trial.sh <command> …  the old datad trial: datad-trial.sh sets
#                                  U60S_ENTRY=datad-trial and sources this
#                                  file, so $0 (and every log line, command
#                                  and path of the old tool) stays the same.
#
# ── datad trial (the part datad-trial.sh uses) ──────────────────────────────
# Every 10 s it checks; the first failure aborts the trial:
#   1. screen data stopped: /v2/state's live block (observed_at) has not moved for 30 s
#      (a failed or timed-out fetch counts as "not changed")
#   2. test build crashed: `pidof luyaz-core.test` is empty
#   3. screen trouble: u60-uid logged a give-up / vendor-UI hand-over / an
#      unrequested u60pro-devui exit (/tmp/luyaz-screen.log; the device has no
#      logd, so logread is empty), or u60pro-devui is gone
#   (zte-agent's netwatch counter was a fourth check; gone with the agent,
#   8d-2c: u60d writes no such counter.)
# Abort: kill the test build, `/etc/init.d/luyaz-core start`, reason into the
# log. Production counts as back only when its /v2/state has a live block. If
# the test build survives kill -9, production is NOT started (both would want
# 9460): loud log line, exit non-0. An unexpected watcher exit (script error,
# signal other than KILL) restores too (EXIT trap); launch fails and restores
# if the watcher is not confirmed running within 5 s. After the window (1 h)
# datad-trial restores production the same way and logs PASS; it never
# promotes (that is `u60-ship.sh`'s job).
#
# Detaching from the SSH session (the device's BusyBox has no setsid and no
# timeout): setsid if present, else `start-stop-daemon -S -b` (BusyBox: fork +
# new session, stdio to /dev/null, so it runs /bin/sh -c 'exec … >>log' to keep
# the output), else plain `nohup … &` (logged: it may die with the SSH
# session then). See detach().
#
# Time comes from /proc/uptime (monotonic): the device clock is local time
# labelled UTC and SNTP can step it.
# Every command and path can be overridden from the environment for tests.
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────

ENTRY=${U60S_ENTRY:-u60-ship}

INITD_DATAD=${DT_INITD:-/etc/init.d/luyaz-core}
RC=${DT_RC:-/etc/rc.local}
LOG=${DT_LOG:-/data/luyaz/scripts/datad-trial.log}
PIDFILE=${DT_PIDFILE:-/tmp/datad-trial.pid}
UPTIME_FILE=${DT_UPTIME:-/proc/uptime}
STATE_URL=${DT_STATE_URL:-http://127.0.0.1:9460/v2/state}
CURL=${DT_CURL:-/usr/bin/curl}
PIDOF=${DT_PIDOF:-pidof}
PS=${DT_PS:-ps w}
# The screen log, the sentinel's (/tmp/u60-uid.log before Luya-Z rename E2c)
UID_LOG=${DT_UID_LOG:-/tmp/luyaz-screen.log}
# the sentinel moves the log to <log>.old at its cap (mv): read both, .old
# first (cat goes on past a missing file); uid_bad_count follows the rotation
LOGREAD=${DT_LOGREAD:-cat $UID_LOG.old $UID_LOG}
KILL=${DT_KILL:-kill}
SLEEP=${DT_SLEEP:-sleep}
DATE=${DT_DATE:-date}
SETSID=${DT_SETSID:-setsid}
SSD=${DT_SSD:-start-stop-daemon}
SH=${DT_SH:-/bin/sh}
TEST_BIN=${DT_TEST_BIN:-/data/luyaz/luyaz-core.test}
TEST_LOG=${DT_TEST_LOG:-/data/luyaz/scripts/datad-trial.test.log}

TEST_NAME=${DT_TEST_NAME:-luyaz-core.test}
PROD_NAME=${DT_PROD_NAME:-luyaz-core}
UI_NAME=${DT_UI_NAME:-luyaz-ui}
# Old manual flow (start.sh copied to start.test.sh): still killed on
# restore so a leftover wrapper cannot relaunch the test build. launch does
# not use a wrapper.
WRAPPER=${DT_WRAPPER:-start.test.sh}

# What /etc/init.d/luyaz-core's start_service runs.
# scripts/test/datad-trial checks these against scripts/luyaz-core.init.
# (the token: luyaz-core.init's, Luya-Z rename D2; the old /data/plugins/zwrt-datad one is gone)
_tok=/data/luyaz/core/auth.token
LAUNCH_ARGS="-i 1000 --lan-bind 0.0.0.0 --lan-port 9461 --auth-token-file $_tok"
# (LUYAZ_ names only since the Luya-Z rename D3 clean-up, see luyaz-core.init)
LAUNCH_ENV="LUYAZ_UBUS=auto"
# Auto revert (E4 D30): the init script turns it on while this file exists;
# looked at when launching (the file may come later), not when sourced.
ROLLBACK_FLAG=${DT_ROLLBACK_FLAG:-/data/luyaz/core/ops/rollback-on}
launch_env() {
    if [ -f "$ROLLBACK_FLAG" ]; then echo "$LAUNCH_ENV LUYAZ_ROLLBACK=1"
    else echo "$LAUNCH_ENV LUYAZ_ROLLBACK=0"; fi
}

INTERVAL=${DT_INTERVAL:-10}
STALE=${DT_STALE:-30}
WINDOW=${DT_WINDOW:-3600}

# ── ship paths (docs/SHIP.md) ───────────────────────────────────────────────
# U60S_ROOT prefixes every device path (tests, selftest); U60S_TMP is the
# RAM-disk directory. The datad-trial defaults above stay as they were.
ROOT=${U60S_ROOT:-}
# Luya-Z rename D4 (manager docs/designs/luyaz-rename.md): the ship family lives under
# /data/luyaz/ship, the tmpfs half under /tmp/luyaz/ship, the manifest beside the txn. The
# executor is /data/luyaz/scripts/luyaz-ship.sh, the boot-time clean-up luyaz-recover.sh beside
# the txn. SHIP_MARK is what `move-d4` writes last: a device without it was never moved and
# stage / prepare-rollback refuse until it is (the old paths are only move-d4's).
# Same constants as luyaz-core paths.rs and luyaz-sentinel worker.rs (scripts/test/luyaz-dir).
SHIP_MARK=$ROOT/data/luyaz/ship/.migrated
SHIP_DIR_OLD=$ROOT/data/u60-ship
SHIP_DIR_NEW=$ROOT/data/luyaz/ship
SHIP_EXEC_OLD=$ROOT/data/u60-guard/u60-ship.sh
SHIP_EXEC_NEW=$ROOT/data/luyaz/scripts/luyaz-ship.sh
SHIP_TMP=${U60S_TMP:-/tmp/luyaz/ship}
SHIP_DIR=${U60S_DIR:-$SHIP_DIR_NEW}
MANIFEST=${U60S_MANIFEST:-$SHIP_DIR_NEW/manifest.jsonl}
REC_NAME=luyaz-recover.sh
TXN_FILE=$SHIP_DIR/txn
STAGE_DIR=$SHIP_DIR/stage
LOCK_FILE=$SHIP_DIR/lock
HB_FILE=$SHIP_TMP/heartbeat
BOOT_ID_FILE=${U60S_BOOT_ID:-/proc/sys/kernel/random/boot_id}
SYNC=${U60S_SYNC:-sync}
DF=${U60S_DF:-df}
MIN_FREE_KB=${U60S_MIN_FREE_KB:-204800}   # 200 MB (R11)
HB_STALE=30                               # recover-live: heartbeat older = dead
KEEP_PREV=3                               # ship-made .prev per file (R11)
OBSERVE=600                               # observation after done (R7; 10 min since 10-14, user)
CRASH_AT=${U60S_CRASH_AT:-}               # tests: kill -9 ourselves here
TO_INITD=${U60S_TO_INITD:-25}             # s: one init.d stop/start (T4)
POLL=${U60S_POLL:-0.2}                    # s: how often step() looks (real time)
TXN_RE='^[0-9]{8}-[0-9]{6}-[a-z0-9]{1,12}$'
HB_STEP=
if [ "$ENTRY" != datad-trial ]; then
    TRIAL_PIDFILE=$PIDFILE                # a datad-trial watcher = busy
    PIDFILE=$SHIP_TMP/executor.pid
    LOG=${U60S_LOG:-$SHIP_DIR/ship.log}
    TEST_LOG=${U60S_TEST_LOG:-$SHIP_DIR/test.log}
    INITD_DATAD=${DT_INITD:-$ROOT/etc/init.d/luyaz-core}
    REAL_SLEEP=$SLEEP
    SLEEP=hb_sleep                        # every sleep of the executor beats
fi

# phase-two components (docs/SHIP.md「第二期」)
DEVUI_DIR=$ROOT/data/plugins/u60pro-devui
# Phase 10 R5 (round ②): the touch UI is luyaz-ui in /data/luyaz/ui (program, fonts, touchsim);
# start.sh and the *.orig-backup stay in DEVUI_DIR; devui.conf, operator-logos are in
# UI_DIR too since Luya-Z D5 (esim.conf, read by nothing since M6b, was deleted)
UI_DIR=${U60S_UI_DIR:-$ROOT/data/luyaz/ui}
UI_BIN=$UI_DIR/luyaz-ui
# ui_bin: the program the sentinel starts: the new one when it is there (luyaz-sentinel run.rs pick_ui)
ui_bin() { echo "$UI_BIN"; } # round ③: the old program is deleted
UID_INITD=${U60S_UID_INITD:-$ROOT/etc/init.d/u60-uid}
UID_STATE=${U60S_UID_STATE:-$ROOT/data/u60-uid}
# the screen's state moves to /data/luyaz/screen (Phase 10 R4; the sentinel copies it, writes
# .migrated last and mirrors attempts/gave-up into the old one until round ③): clear both
UID_STATE_NEW=${U60S_UID_STATE_NEW:-$ROOT/data/luyaz/screen}
uid_clear() { rm -f "$UID_STATE/attempts" "$UID_STATE/gave-up" "$UID_STATE_NEW/attempts" "$UID_STATE_NEW/gave-up"; }
PROCDIR=${U60S_PROC:-/proc}
# Phase 10 X11: a one-shot `<core> doctor` is a check, not a core. It renames its comm to
# luyaz-doctor, but busybox pidof also matches argv0 and the exe's name, so every pidof here
# drops a pid whose argv[1] is "doctor" (tests: test/u60-ship "pidof skips doctor").
PIDOF_RAW=$PIDOF
pidof_nodoctor() {
    _pn=
    for _pp in $($PIDOF_RAW "$@"); do
        [ "$(tr '\0' '\n' <"$PROCDIR/$_pp/cmdline" 2>/dev/null | sed -n 2p)" = doctor ] && continue
        _pn="$_pn${_pn:+ }$_pp"
    done
    [ -n "$_pn" ] && echo "$_pn"
}
PIDOF=pidof_nodoctor
# The retired guard's directory: only legacy_here, guard_pids (a legacy install) and move-d4 look
# there. Everything else is in SCRIPTS_DIR (Luya-Z rename D1, manager docs/designs/luyaz-rename.md):
# the scripts, this file (as luyaz-ship.sh) and datad-trial.sh.
GUARD_DIR=$ROOT/data/u60-guard
SCRIPTS_DIR=$ROOT/data/luyaz/scripts
GUARD_INITD=${U60S_GUARD_INITD:-$ROOT/etc/init.d/u60-guard}
# luyaz_new ROOT: the core's layout switch (Phase 10 10e, manager docs/designs/u60-phase10.md
# R1/R2): /data/luyaz/core/.migrated (the retired luyaz-migrate.sh wrote it last) = the new names
# (/data/luyaz/luyaz-core, /etc/init.d/luyaz-core, procd luyaz-core, trial luyaz-core.test,
# /data/luyaz/web.env). Same line in doctor.sh, u60-recover.sh, chaos.sh, agent-auth.sh,
# luyaz-fallback.sh (scripts/test/luyaz-dir). Only e3_old_layout asks it now: since E3 an
# old-layout device (the zwrt-datad names) is refused, so the names below are the new ones.
luyaz_new() { [ -f "$1/data/luyaz/core/.migrated" ]; }
CORE_NAME=luyaz-core
CORE_BIN=$ROOT/data/luyaz/luyaz-core
# the init, the process and the trial follow the switch (DT_* still win: tests, datad-trial)
INITD_DATAD=${DT_INITD:-$ROOT/etc/init.d/$CORE_NAME}
TEST_BIN=${DT_TEST_BIN:-$CORE_BIN.test}
TEST_NAME=${DT_TEST_NAME:-$CORE_NAME.test}
PROD_NAME=${DT_PROD_NAME:-$CORE_NAME}
GUARD_STOPREQ=${U60S_GUARD_STOPREQ:-/tmp/luyaz/stop-requested}
KMSG_DEV=${U60S_KMSG:-/dev/kmsg}
UBUS=${U60S_UBUS:-ubus}
TO_DOCTOR=${U60S_TO_DOCTOR:-90}           # s: one doctor.sh run (guard)
BLANK_MAX=20                              # s: the screen without a UI (touch trial)
UID_UP=10                                 # s: uid-restart waits this long per start
# (The retired guard component's GUARD_FILES / GUARD_INITS tables left with its
# scripts; docs/SHIP.md keeps them as history.)
# The sentinel (manager docs/designs/u60-sentinel.md 5f): its program. Its init
# script goes up with the scripts component (an upload is named by the base
# name, and /data/luyaz/luyaz-sentinel and /etc/init.d/luyaz-sentinel share it);
# not with guard, whose log would then need v=3. The first sentinel ship
# converts rc.local (note=convert): the u60-uid and u60-guard start lines become
# one sentinel line, derived on the device from the live file (rc_derive). After
# that, sentinel ships carry only the program. 5f order: install-recover (formats
# 3) → ship scripts (the init) → ship sentinel --convert.
SENT_BIN=$ROOT/data/luyaz/luyaz-sentinel
SENT_INITD=${U60S_SENT_INITD:-$ROOT/etc/init.d/luyaz-sentinel}
SENT_HB=${U60S_SENT_HB:-/tmp/luyaz-sentinel.hb}
# 10e: the /tmp tree's switch; every sentinel start (ship, rollback) removes it
# first and the sentinel that knows /tmp/luyaz writes it again (an older one
# does not: the readers then go back to /tmp/u60-guard)
SENT_TMP_MIGRATED=${U60S_SENT_TMP_MIGRATED:-/tmp/luyaz/.migrated}
SENT_HB_STALE=150                         # s: the core's "sentinel silent" line (§4)
UID_CTL=${U60S_UID_CTL:-/tmp/luyaz-screen.ctl} # the sentinel's FIFO (Phase 10 R4 round ③: the only one)
# R4 floor: a sentinel that does not know this FIFO (from before round ①) starts the old program,
# which round ③ deleted: never staged, never rolled back to, never restored by u60-recover.sh
SENT_MARK=/tmp/luyaz-screen.ctl
# Luya-Z rename D1 floor: the old copies in /data/u60-guard are gone, so a core or touch UI from
# before D1 would miss the scripts and the flags (a core that does not see lan-ipv6-off lets LAN
# IPv6 back on): never staged, never rolled back to
SCRIPTS_MARK=/data/luyaz/scripts
# D2 floor: once /data/u60-ops was moved to OPS_MARK, a core that does not know it would start
# with an empty journal and no APN vault: never staged, never rolled back to
OPS_MARK=/data/luyaz/core/ops
d2_moved() { [ -d "$ROOT$OPS_MARK" ] && [ ! -e "$ROOT/data/u60-ops" ]; }
# …and the Wi-Fi lock's new name (the core takes only it since D2's clean-up): a sentinel that
# takes only the old one would write Wi-Fi alongside the core (a pre-D2 core is refused above)
LOCK_MARK=/tmp/luyaz-wifi.lock
# D3 floor: once datad.env is on the LUYAZ_ names, the init and LAUNCH_ENV give nothing else, so a
# core from before D3 (it reads the old names only) would quietly lose auto revert, socket ubus and
# its switches: a core without the D3 build marker is never staged, never rolled back to
CORE_MARK=LUYAZ_CORE_BUILD_SELF_CONTAINED
d3_named() { grep -q '^LUYAZ_' "$ROOT/data/luyaz/core/datad.env" 2>/dev/null; }
# D4 floor: the ship family moved, so a core or sentinel from before D4b-1 reads the old txn,
# heartbeat and manifest and runs the deleted /data/u60-guard/u60-ship.sh for recover-live:
# never staged, never rolled back to. Their builds carry these paths.
SHIP_CORE_MARK=/data/luyaz/ship/manifest.jsonl
SHIP_SENT_MARK=/data/luyaz/scripts/luyaz-ship.sh
# D5 floor: the touch settings left /data/plugins/u60pro-devui, so a core, sentinel or touch UI
# from before D5 would read no devui.conf (Chinese alert SMS, default screen settings) and the UI
# would write a stray one: once the old file is gone, never staged, never rolled back to
UI_CONF_MARK=/data/luyaz/ui/devui.conf
d5_moved() { [ -f "$ROOT$UI_CONF_MARK" ] && [ ! -e "$DEVUI_DIR/devui.conf" ]; }
# E2 floor: the write and AT locks are luyaz-write.lock / luyaz-at.lock only since E2's de-old,
# so a core or sentinel from before E2 (it takes only u60-write.lock, reads /tmp/u60-uid.log)
# would write alongside the others: its build lacks this name, never staged, never rolled back
# to. The same for a luyaz-fallback.sh that lacks FALLBACK_E2_MARK (the scripts' rollback too).
E2_MARK=/tmp/luyaz-screen.log
FALLBACK_E2_MARK=luyaz-write.lock
# E3 (no floor on builds: everything from before E3 also works in the new layout): the core, the
# sentinel, the touch UI and the scripts from E3 on know only the new trees (/data/luyaz/core,
# /tmp/luyaz, /data/luyaz/screen), so a device still in the old layout (the install kit's own rule
# since E1: an old directory there without the new one's .migrated) gets none of them
e3_old_layout() {
    { [ -d "$ROOT/data/u60d" ] && ! luyaz_new "$ROOT"; } ||
        { [ -d "$ROOT/data/u60-uid" ] && [ ! -f "$ROOT/data/luyaz/screen/.migrated" ]; }
}
# D4c: the core component is written "core" (txn ids, the txn file, the manifest);
# before it, "datad". Both name it, in every lookup. tools/luyaz sends "core" only
# to an executor that has this line.
CORE_COMP=core
is_core() { [ "$1" = core ] || [ "$1" = datad ]; }
comp_pat() { if is_core "$1"; then echo '"comp":"(core|datad)"'; else echo "\"comp\":\"$1\""; fi; } # grep -E
HOLD_MAX=3600                             # s: the sentinel's longest hold (luyaz-sentinel HOLD_MAX_S)
RC_LOCAL=${U60S_RC_LOCAL:-$ROOT/etc/rc.local}
VERDICT=$SHIP_DIR/verdict                 # the trial judge's line (core, §7)
# The scripts component (D2): the guard component without the guard itself,
# once the sentinel took its place. zte-agent's init (both copies) left in
# 8d-2c: nothing starts the retired agent; the files stay on the device as
# they are. agent-auth.sh stays: it writes /data/zte-agent.env, which
# luyaz-web logs in with.
# zwrt-datad.init (and its /etc/init.d copy) left in Luya-Z rename E3: no device has the old
# layout it starts in; the copies on the device stay as they are (it refuses the new layout).
SCRIPTS_FILES="alert-lib.sh agent-auth.sh chaos.sh doctor.sh doctor-legacy.sh config-backup.sh wan-sources.sh rpc-acl.sh luyaz-fallback.sh"
# luyaz-web's init rides here too, as the sentinel's does (needs u60-recover.sh
# formats 4); it refuses to start while /data/luyaz/luyaz-web is not there.
# luyaz-core's (10e, formats 5) too; zwrt-datad's went with E3 (see SCRIPTS_FILES).
# (Not in SCRIPTS_FILES: that list must stay within the install kit's.)
SCRIPTS_INITS="luyaz-core luyaz-sentinel luyaz-web"

# u60-uid's own wording (src/uid.c logf_). Its file lines have no "u60-uid:"
# prefix, so the pattern must not require one. "giving up:" with the colon:
# every normal launch line ends "(attempt N of M before giving up)".
UID_BAD='(giving up:|starting the vendor UI|vendor UI on screen|u60pro-devui pid [0-9]+ ended:)'

now() { cut -d. -f1 "$UPTIME_FILE"; }

log() {
    mkdir -p "$(dirname "$LOG")" 2>/dev/null
    echo "$($DATE '+%Y-%m-%d %H:%M:%S') $*" >>"$LOG"
}

say() { echo "$ENTRY: $*"; }

# step <timeout s, 0 = none> <command…>: the ship executor runs every
# command that can block (init.d, sync) in the background and waits in short
# real-time polls, writing its heartbeat every second, so no normal step goes
# 30 s without one (the guard would take over: docs/SHIP.md). Over the
# timeout the command gets kill -9 and 124 comes back. No timeout (0) for
# sync: a sync that was given up on would break the write-then-rename order.
# datad-trial (and anything outside a transaction) runs the command directly.
step() {
    _stpto=$1
    shift
    if [ "$ENTRY" = datad-trial ] || [ -z "$X_TXN" ]; then
        "$@"
        return
    fi
    "$@" &
    _stppid=$!
    _stpn=0
    _stpd=0.02
    while kill -0 "$_stppid" 2>/dev/null; do
        sleep "$_stpd"
        _stpd=$POLL
        _stpn=$((_stpn + 1))
        [ $((_stpn % 5)) = 0 ] && heartbeat
        if [ "$_stpto" -gt 0 ] && [ "$_stpn" -ge $((_stpto * 5)) ]; then
            kill -9 "$_stppid" 2>/dev/null
            log "！！超时（${_stpto}s），已放弃：$*"
            return 124
        fi
    done
    wait "$_stppid"
}

sync_hb() { step 0 $SYNC; }

# ── checks ──────────────────────────────────────────────────────────────────

# Fetch /v2/state once; sets STATE (empty on failure).
fetch_state() {
    STATE=$($CURL -s -m 3 --noproxy '*' "$STATE_URL" 2>/dev/null)
}

# The live block's observed_at (STATE_V2.md §4): the last round datad read
# system info and traffic. It moves every round; a stale live block keeps it
# still, so a datad that answers but no longer reads counts as stopped.
state_ts() {
    printf '%s' "$STATE" | grep -oE '"live"[[:space:]]*:[[:space:]]*[{][^{}]*"observed_at"[[:space:]]*:[[:space:]]*[0-9]+' |
        head -n 1 | sed 's/.*://; s/[^0-9]//g'
}

# num <string>: decimal digits only, leading zeros dropped ("08" → 8; ash
# arithmetic would read 08 as bad octal and kill the script), empty → 0.
num() {
    _v=$(printf '%s' "$1" | tr -dc '0-9' | sed 's/^0*//')
    echo "${_v:-0}"
}

# watcher_pid_ok <pid>: that pid is alive AND is this script (pid reuse).
watcher_pid_ok() {
    [ -n "$1" ] && [ -d "/proc/$1" ] && tr '\0' ' ' <"/proc/$1/cmdline" 2>/dev/null | grep -q 'datad-trial\.sh'
}

# uid_bad_in <file…>: u60-uid's bad lines in those files (0 if none there)
uid_bad_in() { num "$(cat "$@" 2>/dev/null | grep -E "$UID_BAD" | grep -vc '(requested)')"; }
inode_of() { ls -i "$1" 2>/dev/null | awk 'NR == 1 { print $1 }'; }

# uid_bad_reset, then uid_bad_count → UB_N: u60-uid's bad lines so far, as a
# count that only grows across a rotation (not in a $( ), it keeps state).
# Each call notes the log's and .old's inode and bad lines. A file seen last
# time that is gone now takes its bad lines into UB_GONE: the last log is
# still there as the log or (rotated) as the .old; the last .old only as the
# .old. A bad line written into a file that came and went between two polls
# (two rotations in one poll) is not seen.
# With DT_LOGREAD (tests, a logread) it is the plain count of that output.
uid_bad_reset() { UB_GONE=0 UB_CI= UB_OI= UB_CC=0 UB_OC=0; }
uid_bad_count() {
    if [ -n "${DT_LOGREAD:-}" ]; then
        UB_N=$(num "$($LOGREAD 2>/dev/null | grep -E "$UID_BAD" | grep -vc '(requested)')")
        return 0
    fi
    _ci=$(inode_of "$UID_LOG")
    _oi=$(inode_of "$UID_LOG.old")
    _cc=$(uid_bad_in "$UID_LOG")
    _oc=$(uid_bad_in "$UID_LOG.old")
    if [ -n "$UB_CI" ] && [ "$_ci" != "$UB_CI" ] && [ "$_oi" != "$UB_CI" ]; then
        UB_GONE=$((UB_GONE + UB_CC))
    fi
    if [ -n "$UB_OI" ] && [ "$_oi" != "$UB_OI" ]; then
        UB_GONE=$((UB_GONE + UB_OC))
    fi
    UB_CI=$_ci UB_OI=$_oi UB_CC=$_cc UB_OC=$_oc
    UB_N=$((UB_GONE + _oc + _cc))
}

wrapper_pids() {
    [ -n "$WRAPPER" ] || return 0
    $PS 2>/dev/null | grep -F "$WRAPPER" | grep -v -e grep -e datad-trial | awk '{ print $1 }'
}

# ── restore production ──────────────────────────────────────────────────────

kill_test() {
    for _p in $(wrapper_pids); do $KILL "$_p" 2>/dev/null; done
    _pids=$($PIDOF "$TEST_NAME" 2>/dev/null)
    [ -n "$_pids" ] && $KILL $_pids 2>/dev/null
    _i=0
    while [ $_i -lt 5 ]; do
        _pids=$($PIDOF "$TEST_NAME" 2>/dev/null)
        [ -z "$_pids" ] && return 0
        $SLEEP 1
        _i=$((_i + 1))
    done
    for _p in $(wrapper_pids); do $KILL -9 "$_p" 2>/dev/null; done
    $KILL -9 $_pids 2>/dev/null
    # -9 can take a moment (D state): wait, try -9 again, wait again.
    _i=0
    while [ $_i -lt 6 ]; do
        $SLEEP 1
        _pids=$($PIDOF "$TEST_NAME" 2>/dev/null)
        [ -z "$_pids" ] && return 0
        [ $_i = 2 ] && $KILL -9 $_pids 2>/dev/null
        _i=$((_i + 1))
    done
    return 1
}

start_prod() {
    _try=1
    while [ $_try -le 2 ]; do
        step "$TO_INITD" "$INITD_DATAD" start >/dev/null 2>&1
        # Back = the process exists AND /v2/state has a live block, within 20 s.
        _i=0
        while [ $_i -lt 20 ]; do
            _p=$($PIDOF "$PROD_NAME" 2>/dev/null)
            if [ -n "$_p" ]; then
                fetch_state
                if [ -n "$(state_ts)" ]; then
                    log "正式版已恢复（pid $_p，/v2/state 有 live 块）"
                    return 0
                fi
            fi
            $SLEEP 1
            _i=$((_i + 1))
        done
        _try=$((_try + 1))
    done
    log "！！正式版没起来：请手动 $INITD_DATAD start 并检查"
    return 1
}

# restore <outcome line for the log>
restore() {
    [ -n "$RESTORED" ] && return 0
    RESTORED=1
    log "$1"
    if kill_test; then
        log "测试版已停止"
    else
        # Starting production now would fight it for 9460.
        log "！！测试版杀不掉（pid $($PIDOF "$TEST_NAME" 2>/dev/null)），没有启动正式版：请手动 kill -9 后 $INITD_DATAD start"
        rm -f "$PIDFILE"
        return 1
    fi
    start_prod
    _rc=$?
    rm -f "$PIDFILE"
    return $_rc
}

# ── preflight ───────────────────────────────────────────────────────────────

# Checks shared by start and launch; one reason per line, empty = OK.
preflight_common() {
    if [ ! -f "$INITD_DATAD" ]; then
        echo "$INITD_DATAD 不存在，恢复正式版没有办法"
    elif grep -q '\.test' "$INITD_DATAD"; then
        echo "$INITD_DATAD 里有 .test：它必须只指向正式程序"
    fi
    if grep -v '^[[:space:]]*#' "$RC" 2>/dev/null | grep -e 'zwrt-datad' -e 'luyaz-core' | grep -q '\.test'; then
        echo "$RC 里 zwrt-datad（或 luyaz-core）那行指向 .test：它必须只指向正式程序"
    fi
    if [ -f "$PIDFILE" ] && [ -d "/proc/$(cat "$PIDFILE" 2>/dev/null)" ]; then
        echo "已经有一个守护在跑（pid $(cat "$PIDFILE")）"
    fi
    # a ship transaction owns 9460 as well (docs/SHIP.md)
    _busy=$(txn_busy)
    [ -n "$_busy" ] && echo "$_busy"
}

# start: production already stopped, test build already running.
preflight() {
    preflight_common
    if [ -n "$($PIDOF "$PROD_NAME" 2>/dev/null)" ]; then
        echo "正式版 $PROD_NAME 还在跑：先 $INITD_DATAD stop（两个 datad 会抢 9460），或者用 launch"
    fi
    if [ -z "$($PIDOF "$TEST_NAME" 2>/dev/null)" ]; then
        echo "测试版 $TEST_NAME 没在跑：用 launch 起它"
    fi
}

# launch: production running is the normal case; the test build must exist
# and not be running yet. Extra arguments must be VAR=value.
preflight_launch() {
    preflight_common
    if [ ! -f "$TEST_BIN" ] || [ ! -x "$TEST_BIN" ]; then
        echo "$TEST_BIN 不存在或不可执行：先推送并 chmod +x"
    fi
    if [ -n "$($PIDOF "$TEST_NAME" 2>/dev/null)" ]; then
        echo "测试版 $TEST_NAME 已经在跑（pid $($PIDOF "$TEST_NAME")）：用 start 守护它，或先 abort"
    fi
    for _kv in "$@"; do
        printf '%s' "$_kv" | grep -qE '^[A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*$' ||
            echo "额外参数 “$_kv” 不是 VAR=value（不带空格）"
    done
}

refuse() { # refuse <verb> <problems>
    say "拒绝$1："
    echo "$2" | sed 's/^/  - /'
    log "拒绝$1：$(echo "$2" | tr '\n' ';')"
    exit 1
}

# The command launch runs, one line (the extra VAR=value go after LAUNCH_ENV).
launch_cmd() {
    echo "env $(launch_env)${*:+ $*} nohup $TEST_BIN $LAUNCH_ARGS"
}

# detach <outfile> <command…>: run the command in the background, out of this
# session (survives the SSH logout), stdin /dev/null, stdout+stderr appended to
# <outfile>. The command and its arguments are passed as separate words, never
# pasted into a shell string.
detach() {
    _out=$1
    shift
    if command -v "$SETSID" >/dev/null 2>&1; then
        "$SETSID" "$@" >>"$_out" 2>&1 </dev/null &
    elif command -v "$SSD" >/dev/null 2>&1 || command -v "/sbin/$SSD" >/dev/null 2>&1; then
        command -v "$SSD" >/dev/null 2>&1 || SSD=/sbin/$SSD
        # -p on a file that never exists: BusyBox then matches no running
        # process (with only -x it would match every busybox /bin/sh and
        # refuse "already running"). $0 of the inner sh = the output file.
        # shellcheck disable=SC2016
        "$SSD" -S -b -p "$PIDFILE.none" -x "$SH" -- \
            -c 'exec "$@" >>"$0" 2>&1 </dev/null' "$_out" "$@"
    else
        log "没有 setsid 也没有 start-stop-daemon：用 nohup … & 后台启动，SSH 断开时可能被一起带走"
        "$@" >>"$_out" 2>&1 </dev/null &   # both callers already start with nohup
    fi
}

# Absolute path of this script: the detached watcher must not depend on the cwd.
SELF=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")

# Starts the watcher and confirms (≤ 5 s) it runs: pidfile written, pid alive,
# and it is this script. If not: restore production, return 1.
do_start() {
    problems=$(preflight)
    [ -n "$problems" ] && refuse 开始 "$problems"
    detach /dev/null nohup sh "$SELF" run
    _i=0
    while [ $_i -lt 5 ]; do
        $SLEEP 1
        _i=$((_i + 1))
        if watcher_pid_ok "$(cat "$PIDFILE" 2>/dev/null)"; then
            say "已开始守护（pid $(cat "$PIDFILE")），日志：$LOG"
            return 0
        fi
    done
    RESTORED=
    restore "启动失败：守护 5s 内没起来（$PIDFILE 没有或进程不在），已恢复正式版"
    say "失败：守护没起来，已停测试版并恢复正式版；看 $LOG"
    return 1
}

# stop_prod: init.d stop, wait ≤ 15 s for the process to go. 1 = still there.
stop_prod() {
    step "$TO_INITD" "$INITD_DATAD" stop >/dev/null 2>&1
    _i=0
    while [ -n "$($PIDOF "$PROD_NAME" 2>/dev/null)" ]; do
        [ $_i -ge 15 ] && return 1
        $SLEEP 1
        _i=$((_i + 1))
    done
    return 0
}

# launch_test_build [VAR=value …]: start the test build detached (no
# supervise.sh: a crash must stay a crash) and wait ≤ 20 s until it runs and
# /v2/state has a live block. 1 = not up, why in _why.
launch_test_build() {
    mkdir -p "$(dirname "$TEST_LOG")" 2>/dev/null
    echo "=== $($DATE '+%Y-%m-%d %H:%M:%S') $(launch_cmd "$@")" >>"$TEST_LOG"
    log "launch：$(launch_cmd "$@")"
    # shellcheck disable=SC2086
    detach "$TEST_LOG" env $(launch_env) "$@" nohup "$TEST_BIN" $LAUNCH_ARGS

    _i=0
    _why="进程没出现"
    while [ $_i -lt 20 ]; do
        $SLEEP 1
        _i=$((_i + 1))
        [ -n "$($PIDOF "$TEST_NAME" 2>/dev/null)" ] || continue
        _why="/v2/state 没有 live 块"
        fetch_state
        [ -n "$(state_ts)" ] || continue
        log "launch：测试版已起来（pid $($PIDOF "$TEST_NAME" 2>/dev/null)，${_i}s）"
        return 0
    done
    return 1
}

# launch [VAR=value …]
do_launch() {
    problems=$(preflight_launch "$@")
    [ -n "$problems" ] && refuse 启动 "$problems"

    log "launch：停正式版（$INITD_DATAD stop）"
    if ! stop_prod; then
        RESTORED=
        restore "启动失败：正式版 15s 内没有退出（pid $($PIDOF "$PROD_NAME" 2>/dev/null)），没有起测试版"
        say "失败：正式版停不下来，已恢复，看日志 $LOG"
        exit 1
    fi

    if launch_test_build "$@"; then
        say "测试版已起来，输出：$TEST_LOG"
        do_start
        exit $?
    fi
    RESTORED=
    restore "启动失败：测试版 20s 内没起来（$_why），看 $TEST_LOG"
    say "失败：测试版没起来（$_why），已恢复正式版；看 $LOG 和 $TEST_LOG"
    exit 1
}

# ── the watch loop ──────────────────────────────────────────────────────────

# wait_step: one check interval. Plain datad-trial sleeps INTERVAL at once;
# the ship executor (HB_STEP set) sleeps in HB_STEP pieces and writes its
# heartbeat after each one.
wait_step() {
    if [ -z "$HB_STEP" ]; then
        $SLEEP "$INTERVAL"
        return
    fi
    _ws=0
    while [ "$_ws" -lt "$INTERVAL" ]; do
        $SLEEP "$HB_STEP"
        heartbeat
        _ws=$((_ws + HB_STEP))
    done
}

# watch_datad <process name> <window s> <label>: the datad checks (header)
# every INTERVAL s on <process name>, for <window> s. 0 = the window passed;
# 1 = a check failed, the reason (without "中止：") in REASON.
watch_datad() {
    _wname=$1
    _wwin=$2
    T0=$(now)
    fetch_state
    LAST_TS=$(state_ts)
    LAST_CHANGE=$T0
    uid_bad_reset
    uid_bad_count
    UID_BASE=$UB_N
    log "开始：$3 pid $($PIDOF "$_wname" 2>/dev/null)，窗口 ${_wwin}s，每 ${INTERVAL}s 查一次；observed_at=${LAST_TS:-无}"

    while :; do
        wait_step
        T=$(now)

        # 1. screen data (a failed fetch counts as "not changed")
        fetch_state
        TS=$(state_ts)
        if [ -n "$TS" ] && [ "$TS" != "$LAST_TS" ]; then
            LAST_TS=$TS
            LAST_CHANGE=$T
        elif [ $((T - LAST_CHANGE)) -gt "$STALE" ]; then
            REASON="屏幕数据停了（/v2/state 的 live.observed_at ${LAST_TS:-无} 已 $((T - LAST_CHANGE))s 没变）"
            return 1
        fi

        # 2. the build under watch
        if [ -z "$($PIDOF "$_wname" 2>/dev/null)" ]; then
            REASON="$4（pidof $_wname 为空）"
            return 1
        fi

        # 3. screen owner / UI
        uid_bad_count
        N=$UB_N
        if [ "$N" -lt "$UID_BASE" ]; then
            UID_BASE=$N # log buffer wrapped
        elif [ "$N" -gt "$UID_BASE" ]; then
            LINE=$($LOGREAD 2>/dev/null | grep -E "$UID_BAD" | grep -v '(requested)' | tail -n 1 | cut -c1-200)
            REASON="界面异常（u60-uid：$LINE）"
            return 1
        fi
        if [ -z "$($PIDOF "$UI_NAME" 2>/dev/null)" ]; then
            REASON="界面异常（$UI_NAME 进程消失）"
            return 1
        fi

        [ $((T - T0)) -ge "$_wwin" ] && return 0
    done
}

# datad-trial's watcher: watch the test build, then always restore production.
trial_run() {
    RESTORED=
    # EXIT: a script error or an unexpected exit still restores production
    # (restore runs once: RESTORED). SIGKILL (OOM) cannot be caught.
    trap 'restore "中止：守护意外退出"' EXIT
    trap 'restore "中止：守护被停止（abort 或信号）"; exit 1' INT TERM  # not HUP: nohup's ignore must stay
    echo $$ >"$PIDFILE"

    if watch_datad "$TEST_NAME" "$WINDOW" 测试版 测试版崩溃; then
        restore "通过：${WINDOW}s 窗口内没有异常，恢复正式版（升级到正式位置另做）" && exit 0
        exit 1
    fi
    restore "中止：$REASON"
    exit 1
}

trial_main() {
    cmd=$1
    [ $# -gt 0 ] && shift
    case "$cmd" in
        launch)
            do_launch "$@"
            ;;
        start)
            do_start || exit 1
            ;;
        print-launch)
            launch_cmd "$@"
            ;;
        run)
            trial_run
            ;;
        status)
            if [ -f "$PIDFILE" ] && [ -d "/proc/$(cat "$PIDFILE" 2>/dev/null)" ]; then
                say "运行中（pid $(cat "$PIDFILE")）"
            else
                say "没在运行"
            fi
            tail -n 5 "$LOG" 2>/dev/null
            ;;
        abort)
            p=$(cat "$PIDFILE" 2>/dev/null)
            if watcher_pid_ok "$p"; then
                $KILL "$p" # the watcher restores production itself
                say "已通知守护（pid $p）中止，看日志：$LOG"
            else
                [ -n "$p" ] && [ -d "/proc/$p" ] && log "abort：pidfile 里的 pid $p 不是守护（pid 复用），不动它"
                RESTORED=
                restore "中止：手动 abort（守护没在运行）"
            fi
            ;;
        *)
            echo "usage: $0 launch [VAR=value ...]|start|status|abort|print-launch" >&2
            exit 2
            ;;
    esac
}

# ═════════════════════════════════════════════════════════════════════════════
# ship: one transaction = stage → (trial) → promote → check → manifest → done.
# Formats and the phase table: docs/SHIP.md. Every phase change is written to
# the transaction log (temp file → sync → mv → sync) BEFORE files are touched,
# so u60-recover.sh (next boot) or `recover-live` (same boot) can finish it.
# ═════════════════════════════════════════════════════════════════════════════

ACTIVE=' staged trial promote check manifest rollback '

# crashpoint <name>: tests simulate a power cut here (no traps run).
crashpoint() {
    [ -n "$CRASH_AT" ] && [ "$CRASH_AT" = "$1" ] && kill -9 $$
    :
}

md5_of() {
    [ -f "$1" ] || return 0
    md5sum "$1" 2>/dev/null | cut -d' ' -f1
}

# tree_fp <dir>: the directory fingerprint (docs/SHIP.md): "<md5> <path>"
# per regular file (path without "./", LC_ALL=C order of the paths, one
# newline each), md5 of the whole text. Fails (1, nothing printed) for a
# missing directory or a link to one, anything but files and directories
# inside, names with a newline or "|", a file md5sum cannot read. The same
# function is in u60-recover.sh and doctor.sh (scripts/test/tree-fp).
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

boot_id() { tr -dc '0-9a-f-' <"$BOOT_ID_FILE" 2>/dev/null; }

# only_chars <string> <tr set>: the string is non-empty and has nothing else.
only_chars() {
    [ -n "$1" ] && [ -z "$(printf '%s' "$1" | tr -d "$2")" ]
}

matches() { printf '%s\n' "$1" | grep -qE "$2" && [ "$(printf '%s' "$1" | wc -l)" = 0 ]; }

# path_ok <path>: under $ROOT/data/, one of our init scripts or rc.local; plain
# characters only, no "..", "//", "./" games.
path_ok() {
    only_chars "$1" 'A-Za-z0-9._/-' || return 1
    case "$1" in
        *//* | */./* | */../* | */. | */.. | */) return 1 ;;
        "$ROOT"/data/?*) return 0 ;;
        "$ROOT"/etc/init.d/zte-agent | "$ROOT"/etc/init.d/zwrt-datad | "$ROOT"/etc/init.d/u60-guard | "$ROOT"/etc/init.d/u60-uid) return 0 ;;
        # the sentinel component (u60-sentinel.md 5d-0): only in a v=3 log, which needs a
        # u60-recover.sh whose formats say 3
        "$ROOT"/etc/init.d/luyaz-sentinel | "$ROOT"/etc/rc.local) return 0 ;;
        # luyaz-web's (manager docs/designs/u60-web.md step 6): only in a v=4 log
        "$ROOT"/etc/init.d/luyaz-web) return 0 ;;
        # luyaz-core's (10e): only in a v=5 log
        "$ROOT"/etc/init.d/luyaz-core) return 0 ;;
    esac
    return 1
}

in_list() { # in_list <word> <space-separated list>
    case " $2 " in *" $1 "*) return 0 ;; esac
    return 1
}

# ── the sentinel and the screen owner (u60-sentinel.md 5f, T7) ──────────────

# rc_mode <rc.local>: legacy (exactly one u60-uid and one u60-guard start line,
# the guard's right after, no sentinel line), sentinel (exactly one sentinel
# start line, neither of the two), else bad. Both need the u60-recover.sh line
# once and before the owner, the datad start before the owner, and no other
# (conditional, compound, commented-in) mention of the three. Plain string
# matches on the owner lines (a trailing "   # comment" allowed), no regex.
rc_mode() {
    if [ ! -f "$1" ] || [ -L "$1" ]; then
        echo bad
        return
    fi
    awk '
        { l = $0; sub(/[ \t]+$/, "", l); c = l; sub(/[ \t]+#.*$/, "", c) }
        l ~ /^sh \/data\/(u60-ship\/u60|luyaz\/ship\/luyaz)-recover\.sh([ \t;#]|$)/ { rec++; if (recn == 0) recn = NR; next }
        c == "/etc/init.d/zwrt-datad start" || c == "/etc/init.d/luyaz-core start" { if (dn == 0) dn = NR; next }
        c == "/etc/init.d/u60-uid start" { uid++; uidn = NR; next }
        c == "/etc/init.d/u60-guard start" { gd++; gdn = NR; next }
        c == "/etc/init.d/luyaz-sentinel start" { st++; stn = NR; next }
        l ~ /^[ \t]*#/ { next }
        l ~ /u60-uid|u60-guard|luyaz-sentinel/ { other++ }
        l ~ /^[ \t]*exit 0/ { if (ex == 0) ex = NR }
        END {
            if (other > 0 || rec != 1 || dn == 0) { print "bad"; exit }
            if (uid == 1 && gd == 1 && st == 0 && gdn == uidn + 1 && recn < uidn && dn < uidn && (ex == 0 || ex > gdn)) { print "legacy"; exit }
            if (st == 1 && uid == 0 && gd == 0 && recn < stn && dn < stn && (ex == 0 || ex > stn)) { print "sentinel"; exit }
            print "bad"
        }' "$1"
}

# legacy_here: the two programs a legacy rc.local starts are still on disk
# (10c deletes each with its init).
# Phase 10 10c deletes them (manager docs/designs/u60-phase10.md X6, X7; review
# D7): from then on a legacy rc.local must never come back, by a rollback, a
# restore or a start. It would start nothing, the firmware reboots a screen
# with no UI (1185), and the next boot runs the same rc.local again.
legacy_here() {
    [ -x "$DEVUI_DIR/u60-uid" ] && [ -f "$GUARD_DIR/u60-guard.sh" ]
}

# rc_derive <legacy rc.local> <out>: the converted file. The u60-uid line
# becomes the sentinel's, the u60-guard line goes, and the recovery line re-runs
# rc.local when u60-recover.sh put an older rc.local back (exit 10): the shell
# reading this file must not go on to start a sentinel that is no longer there.
# Every other byte stays.
rc_derive() {
    awk '
        { l = $0; sub(/[ \t]+$/, "", l); c = l; sub(/[ \t]+#.*$/, "", c) }
        c == "/etc/init.d/u60-uid start" { print "/etc/init.d/luyaz-sentinel start   # u60 ship sentinel: the screen owner and watchdog (was u60-uid + u60-guard)"; next }
        c == "/etc/init.d/u60-guard start" { next }
        l ~ /^sh \/data\/(u60-ship\/u60|luyaz\/ship\/luyaz)-recover\.sh([ \t#]|$)/ && done == 0 {
            p = "sh /data/luyaz/ship/luyaz-recover.sh"
            q = ((l ~ /^sh \/data\/u60-ship\//) ? "sh /data/u60-ship/u60-recover.sh" : p)
            print p "; [ $? = 10 ] && exec /bin/sh /etc/rc.local" substr($0, length(q) + 1)
            done = 1
            next
        }
        { print }' "$1" >"$2"
}

# the old md5 of <path> in this transaction's file list
x_old_md5() {
    printf '%s' "$X_FILES" | awk -F'|' -v p="$1" '$1 == p { print $2; exit }'
}

# luyaz-sentinel processes: sent_pids the real ones, shadow_pids the --shadow ones
sent_ps() { # <shadow: 1|0>
    for _sp in $($PIDOF luyaz-sentinel 2>/dev/null); do
        if tr '\0' ' ' <"$PROCDIR/$_sp/cmdline" 2>/dev/null | grep -q -e '--shadow'; then
            [ "$1" = 1 ] && echo "$_sp"
        else
            [ "$1" = 0 ] && echo "$_sp"
        fi
    done
}
sent_pids() { sent_ps 0; }
shadow_pids() { sent_ps 1; }

# screen_owner: sentinel | uid | (empty)
screen_owner() {
    if [ -n "$(sent_pids)" ]; then
        echo sentinel
    elif [ -n "$(first_pid u60-uid)" ]; then
        echo uid
    fi
}

# the sentinel heartbeat: "<mono s> <pid> <ours|vendor|none|held> <launches> <worker mono>"
sent_hb() { # <field 1-5>
    awk -v f="$1" 'NR == 1 { print $f }' "$SENT_HB" 2>/dev/null
}

# ctl_send <command>: one line into the owner's FIFO, at most 5 s (a FIFO with
# no reader would block the open for ever).
ctl_send() {
    if [ ! -p "$UID_CTL" ]; then
        REASON="$UID_CTL 不是 FIFO（屏幕主人没在听）"
        return 1
    fi
    (echo "$1" >"$UID_CTL") 2>/dev/null &
    _cw=$!
    _i=0
    while kill -0 "$_cw" 2>/dev/null; do
        if [ $_i -ge 5 ]; then
            kill "$_cw" 2>/dev/null
            REASON="写 $UID_CTL 5s 没写进去"
            return 1
        fi
        $SLEEP 1
        _i=$((_i + 1))
    done
    wait "$_cw"
}

# sent_hold: the sentinel puts the screen down (no adopting, no launching)
# for the whole transaction plus a margin; ≤ 10 s until its heartbeat says held.
sent_hold() {
    _hs=$(($(num "${X_TRIAL:-60}") + $(num "${X_CHECK:-120}") + 300))
    # the sentinel takes at most HOLD_MAX; longer trials renew it (c_touch_trial_watch)
    [ "$_hs" -gt "$HOLD_MAX" ] && _hs=$HOLD_MAX
    log "让哨兵放下屏幕（hold ${_hs}s）"
    ctl_send "hold $_hs" || return 1
    _i=0
    while [ "$(sent_hb 3)" != held ]; do
        if [ $_i -ge 10 ]; then
            REASON="哨兵 10s 内没有放下屏幕（心跳说 $(sent_hb 3)）"
            return 1
        fi
        $SLEEP 1
        _i=$((_i + 1))
    done
}

# sent_renew: hold again for what is left of the trial plus the check (quiet; a failed write
# is caught when the hold runs out: the trial watch sees the sentinel's UI, not the test).
sent_renew() {
    _hs=$(( $(num "${X_TRIAL:-60}") - ($(num "$(now)") - T0) + $(num "${X_CHECK:-120}") + 300 ))
    [ "$_hs" -gt "$HOLD_MAX" ] && _hs=$HOLD_MAX
    ctl_send "hold $_hs" || log "续 hold 没写进去：$REASON"
}

# sent_release [<UI md5>]: the sentinel takes the screen back and launches the
# live UI; ≤ 2×UID_UP s until it runs at that version. UI_PID as uid_restart.
sent_release() {
    _wu=${1:-$(md5_of "$(ui_bin)")}
    uid_clear
    log "让哨兵收回屏幕（release；要界面 ${_wu%"${_wu#????????}"}）"
    ctl_send release || return 1
    _i=0
    while :; do
        _pu=$(ui_pid)
        if [ -n "$_pu" ] && [ "$(exe_md5 "$_pu")" = "$_wu" ] && [ "$(sent_hb 3)" = ours ]; then
            UI_PID=$_pu
            log "哨兵已收回屏幕，界面 pid $_pu"
            return 0
        fi
        if [ $_i -ge $((UID_UP * 2)) ]; then
            REASON="哨兵收回屏幕后界面没起来（心跳说 $(sent_hb 3)）"
            return 1
        fi
        $SLEEP 1
        _i=$((_i + 1))
    done
}

# verdict_of_txn: what the core's trial judge says about this transaction
# (§7): "<ok|fail|pending> <why>", empty when it said nothing about it.
verdict_of_txn() {
    _vl=$(head -n 1 "$VERDICT" 2>/dev/null)
    case "$_vl" in "$X_TXN "*) echo "${_vl#"$X_TXN "}" ;; esac
}

# observe_clash <comp>: D4, one of core and sentinel at a time, observation
# hour included — from the manifest's history, not only the last transaction.
observe_clash() {
    case "$1" in
        datad | core) _oc=sentinel ;;
        sentinel) _oc=core ;;
        *) return 1 ;;
    esac
    _ol=$(grep '"kind":"ship"' "$MANIFEST" 2>/dev/null | grep -E "$(comp_pat "$_oc")" | tail -n 1)
    [ -n "$_ol" ] || return 1
    _ob=$(printf '%s' "$_ol" | sed -n 's/.*"boot_id":"\([^"]*\)".*/\1/p')
    _ou=$(printf '%s' "$_ol" | sed -n 's/.*"uptime":\([0-9]*\).*/\1/p')
    [ "$_ob" = "$(boot_id)" ] || return 1
    _left=$((OBSERVE - ($(num "$(now)") - $(num "$_ou"))))
    [ "$_left" -gt 0 ] || return 1
    echo "$_oc 上次上机还在观察期（还剩 $((_left / 60 + 1)) 分钟）：核心和哨兵一次只换一个（D4）"
}

c_sentinel_pre() {
    _rm=$(rc_mode "$RC_LOCAL")
    if [ "$_rm" = bad ]; then
        REASON="rc.local 不是认得的样子（只认 u60-uid + u60-guard 两行，或哨兵一行）"
        return 1
    fi
    _ro=$(x_old_md5 "$RC_LOCAL")
    if [ -n "$_ro" ] && [ "$(md5_of "$RC_LOCAL")" != "$_ro" ]; then
        REASON="rc.local 在暂存之后被改过"
        return 1
    fi
    if [ ! -f "$SENT_INITD" ]; then
        REASON="$SENT_INITD 不在：先 u60 ship scripts（带上哨兵的启动脚本）"
        return 1
    fi
    log "转正前：rc.local 是 $_rm 的样子"
}

# stop: every screen owner and the guard, the UI left on screen. A shadow
# sentinel first (it only watches, but its process name is the real one's).
c_sentinel_stop() {
    for _p in $(shadow_pids); do
        log "停影子哨兵（pid $_p）"
        $KILL "$_p" 2>/dev/null
    done
    if [ -n "$(sent_pids)" ]; then
        log "停哨兵（界面留在屏上）"
        [ -f "$SENT_INITD" ] && step "$TO_INITD" "$SENT_INITD" stop >/dev/null 2>&1
        _i=0
        while [ -n "$(sent_pids)" ]; do
            [ $_i = 5 ] && $KILL $(sent_pids) 2>/dev/null # not under procd (or the init is gone)
            if [ $_i -ge 15 ]; then
                REASON="哨兵 15s 内没有退出（pid $(sent_pids | tr '\n' ' ')）"
                return 1
            fi
            $SLEEP 1
            _i=$((_i + 1))
        done
    fi
    uid_stop || return 1
    if [ -n "$(guard_pids)" ]; then
        c_guard_stop || return 1
    fi
    if [ -n "$(shadow_pids)" ]; then
        REASON="影子哨兵停不下来"
        return 1
    fi
}

# start: whichever set the files on disk say (after a rollback: the old one).
# Never a second owner: one still running (a stop that failed) = refuse.
c_sentinel_start() {
    if [ -n "$(screen_owner)" ]; then
        REASON="还有屏幕主人在跑（$(screen_owner)），不起第二个"
        return 1
    fi
    if [ "$(rc_mode "$RC_LOCAL")" = sentinel ] && [ -f "$SENT_INITD" ] && [ -x "$SENT_BIN" ]; then
        rm -f "$SENT_HB" "$SENT_TMP_MIGRATED"
        _try=1
        while [ $_try -le 2 ]; do
            log "起哨兵（$SENT_INITD start，第 $_try 次）"
            step "$TO_INITD" "$SENT_INITD" start >/dev/null 2>&1
            _i=0
            while [ $_i -lt 20 ]; do
                _sp=$(sent_pids | head -n 1)
                if [ -n "$_sp" ] && [ -s "$SENT_HB" ] && [ -n "$(ui_pid)" ]; then
                    if [ "$(exe_md5 "$_sp")" != "$(md5_of "$SENT_BIN")" ]; then
                        REASON="哨兵跑的不是这一版"
                        return 1
                    fi
                    SENT_PID=$_sp
                    UI_PID=$(ui_pid)
                    log "哨兵（pid $_sp）起来了，界面 pid $UI_PID"
                    return 0
                fi
                $SLEEP 1
                _i=$((_i + 1))
            done
            _try=$((_try + 1))
        done
        REASON="哨兵没起来，或没接管界面"
        return 1
    fi
    if ! legacy_here; then
        REASON="rc.local 不是哨兵的，旧的 u60-uid + u60-guard 已经删了（Phase 10 10c）：要手动修 rc.local"
        return 1
    fi
    log "起旧的一套：u60-uid，再 u60-guard"
    uid_restart || return 1
    c_guard_start
}

# check (300 s): the same sentinel process at this version, its heartbeat
# moving (twice at least, never stalled past SENT_HB_STALE), no u60-uid, the
# screen never empty past BLANK_MAX; and the core's verdict (D5, fail closed:
# not "ok" by the end = not promoted).
c_sentinel_check() {
    T0=$(num "$(now)")
    _want=$(md5_of "$SENT_BIN")
    _sp=${SENT_PID:-$(sent_pids | head -n 1)}
    _h0=$(sent_hb 1)
    _ht=$T0
    _adv=0
    _blank0=
    log "开始：哨兵检查 pid $_sp，窗口 ${X_CHECK}s，每 ${INTERVAL}s 查一次"
    while :; do
        wait_step
        T=$(num "$(now)")
        _cur=$(sent_pids | head -n 1)
        if [ -z "$_cur" ]; then
            REASON="哨兵不在了"
            return 1
        fi
        if [ "$_cur" != "$_sp" ]; then
            REASON="哨兵被重启过（pid $_sp → $_cur）"
            return 1
        fi
        if [ "$(exe_md5 "$_cur")" != "$_want" ]; then
            REASON="哨兵跑的不是这一版"
            return 1
        fi
        if [ -n "$(first_pid u60-uid)" ]; then
            REASON="u60-uid 也在跑：两个屏幕主人"
            return 1
        fi
        _h=$(sent_hb 1)
        if [ -n "$_h" ] && [ "$_h" != "$_h0" ]; then
            _adv=$((_adv + 1))
            _h0=$_h
            _ht=$T
        fi
        if [ $((T - _ht)) -gt "$SENT_HB_STALE" ]; then
            REASON="哨兵心跳 $((T - _ht))s 没动"
            return 1
        fi
        if [ -z "$(ui_pid)" ] && [ -z "$(first_pid zte_topsw_devui)" ]; then
            [ -n "$_blank0" ] || _blank0=$T
            if [ $((T - _blank0)) -gt "$BLANK_MAX" ]; then
                REASON="屏幕空了 $((T - _blank0))s"
                return 1
            fi
        else
            _blank0=
        fi
        _vd=$(verdict_of_txn)
        if [ "${_vd%% *}" = fail ]; then
            REASON="核心判定不过：${_vd#* }"
            return 1
        fi
        if [ $((T - T0)) -ge "$X_CHECK" ]; then
            if [ "$_adv" -lt 2 ]; then
                REASON="哨兵心跳在 ${X_CHECK}s 里只动了 $_adv 次"
                return 1
            fi
            [ "${_vd%% *}" = ok ] && return 0
            REASON="核心的判定没到 ok（${_vd:-没有判定}）：按 D5 不转正"
            return 1
        fi
    done
}

# scripts: nothing runs them as a service; the files are checked at stage
# (md5, sh -n) and here once more after the move.
c_scripts_stop() { :; }
c_scripts_start() { :; }
c_scripts_check() {
    _ifs=$IFS
    IFS=$NL
    for _e in $X_FILES; do
        IFS=$_ifs
        _p=${_e%%|*}
        if [ "$(md5_of "$_p")" != "${_e##*|}" ]; then
            REASON="$_p 的 md5 不对"
            return 1
        fi
        if is_script "$_p" && ! sh -n "$_p" 2>/dev/null; then
            REASON="$_p 有语法错误"
            return 1
        fi
    done
    IFS=$_ifs
}

# ── components ──────────────────────────────────────────────────────────────
# comp_load <name>: C_KIND (trial = side-by-side stand-in, direct = promote
# at once), C_FILES (live files), C_DIRS (live directories, replaced whole:
# no component since web went, 8d-2c; old v=2 logs still read), C_STATE_ALLOW (exact state files, never a directory prefix:
# append-only logs must not be rolled back), C_TRIAL / C_CHECK (s),
# C_ALLOW_NEW (a live file may not exist yet: guard), C_PRE (a "pre" hook
# runs before a direct promotion), C_NOTREADY (stage refuses, with this
# reason). Hooks: c_<name>_{pre,stop,start,launch_test,trial_watch,kill_test,
# check}; each returns 0 = fine, 1 = failed with the reason in REASON (or
# _why for launch_test).
comp_load() {
    C_NAME=$1
    C_KIND=
    C_FILES=
    C_DIRS=
    C_STATE_ALLOW=
    C_TRIAL=180
    C_CHECK=120
    C_ALLOW_NEW=
    C_PRE=
    C_NOTREADY=
    case "$1" in
        datad | core)
            # core = datad (10e R10); written as given (D4c: tools/luyaz sends core)
            C_NAME=$1
            C_KIND=trial
            C_FILES="$CORE_BIN"
            # no state files since 10-06 (cooling.conf, neighbor.json went with the fan control and
            # the neighbour scan; crates/luyaz-core/ship/datad/state-files is empty). Only stage
            # checks this list: a rollback restores what its own txn log names (state= lines)
            TEST_BIN=$CORE_BIN.test
            ;;
        touch)
            C_KIND=trial
            # R5: the program at its new path (new on the first ship after the rename:
            # stage makes /data/luyaz/ui), start.sh where the firmware expects it
            C_FILES="$UI_BIN $DEVUI_DIR/start.sh"
            C_ALLOW_NEW=1
            C_TRIAL=60
            C_STATE_ALLOW="$UI_DIR/devui.conf"
            TEST_BIN=$UI_BIN.test
            TEST_NAME=luyaz-ui.test
            WRAPPER=
            # its direct-write fallback is looked for under this name only (the scripts ship
            # that first put it there; ship never deletes it, nor does a scripts rollback)
            [ -f "$SCRIPTS_DIR/luyaz-fallback.sh" ] || C_NOTREADY="${SCRIPTS_DIR#"$ROOT"}/luyaz-fallback.sh 不在：先 u60 ship scripts"
            ;;
        lweb)
            C_KIND=trial
            C_FILES="$LW_BIN"
            C_ALLOW_NEW=1
            TEST_BIN=$LW_BIN.test
            TEST_NAME=luyaz-web.test
            WRAPPER=
            [ -f "$LW_INITF" ] || C_NOTREADY="${LW_INITF#"$ROOT"} 不在：先 u60 ship scripts（带上 luyaz-web 的启动脚本）"
            ;;
        sentinel)
            C_KIND=direct
            C_FILES="$SENT_BIN"
            # the conversion (first sentinel ship): rc.local too
            [ "$(rc_mode "$RC_LOCAL")" = legacy ] && C_FILES="$C_FILES $RC_LOCAL"
            C_ALLOW_NEW=1
            C_PRE=1
            C_CHECK=300
            ;;
        scripts)
            C_KIND=direct
            for _gf in $SCRIPTS_FILES; do C_FILES="$C_FILES$SCRIPTS_DIR/$_gf "; done
            for _gf in $SCRIPTS_INITS; do C_FILES="$C_FILES$ROOT/etc/init.d/$_gf "; done
            C_FILES=${C_FILES% }
            C_ALLOW_NEW=1
            C_CHECK=60
            ;;
        selftest)
            # sandbox only: never exists on the real paths
            [ -n "$ROOT" ] || return 1
            C_KIND=direct
            C_FILES="$ROOT/data/selftest/prog $ROOT/data/selftest/conf"
            C_STATE_ALLOW="$ROOT/data/selftest-state/a $ROOT/data/selftest-state/b"
            C_CHECK=$(num "${U60S_SELFTEST_CHECK:-2}")
            ;;
        uid | guard)
            # removed in Phase 10 10c (manager docs/designs/u60-phase10.md X6, X7):
            # the sentinel owns the screen and the watchdog since 5f (10-10)
            C_NOTREADY="组件 “$1” 已删除（Phase 10 10c）：屏幕和看门狗归 luyaz-sentinel（u60 ship sentinel），脚本用 u60 ship scripts"
            return 1
            ;;
        agent | web)
            # removed in 8d-2c (u60-web.md): zte-agent is retired, u60d +
            # luyaz-web took over, and :9090 is luyaz-web's
            C_NOTREADY="组件 “$1” 已删除（8d-2c）：zte-agent 和它的网页 /data/admin 退役了，接口归 u60d、:9090 归 luyaz-web；网页用 u60 ship lweb"
            return 1
            ;;
        *) return 1 ;;
    esac
    return 0
}

hook() {
    _hk=c_${C_NAME}_$1
    [ "$C_NAME" = core ] && _hk=c_datad_$1 # D4c: one set of hooks
    shift
    "$_hk" "$@"
}

c_datad_stop() {
    log "停正式版（$INITD_DATAD stop）"
    stop_prod && return 0
    REASON="正式版 15s 内没有退出（pid $($PIDOF "$PROD_NAME" 2>/dev/null)）"
    return 1
}
c_datad_start() {
    start_prod && return 0
    REASON="正式版没起来（/v2/state 没有 live 块）"
    return 1
}
c_datad_launch_test() { launch_test_build; }
c_datad_trial_watch() { watch_datad "$TEST_NAME" "$X_TRIAL" 测试版 测试版崩溃; }
c_datad_kill_test() { kill_test; }
c_datad_check() { watch_datad "$PROD_NAME" "$X_CHECK" 正式版 正式版消失; }

# ── lweb (luyaz-web, :9090; manager docs/designs/u60-web.md step 6) ─────────
# Stand-in trial like datad's (9090 takes one listener; the engine stops
# the live one before the trial anyway). Its init comes with the scripts
# component (v=4) and must be there first: stage refuses otherwise. The first
# lweb ship has no old program: stop and start then have nothing to do, so an
# aborted trial or a rollback ends with no luyaz-web, as before.
# Checked every INTERVAL s: the process is there (in check: the same pid,
# running this version); /health answers 200 with its "link" (idle when no
# tab is open: not required to be live) and /login 200 with a page (rendered
# by luyaz-web itself, no session); something listens on 0.0.0.0:9090 (an
# init that lost --bind would still answer a loopback curl).
# What /etc/init.d/luyaz-web's start_service runs, minus supervise.sh;
# scripts/test/u60-ship-p2 checks this against scripts/luyaz-web.init.
LW_BIN=$ROOT/data/luyaz/luyaz-web
LW_INITF=$ROOT/etc/init.d/luyaz-web           # the file (scripts component, v=4)
LW_INITD=${U60S_LWEB_INITD:-$LW_INITF}        # what is run
LW_ARGS="serve --bind 0.0.0.0:9090 --upstream http://127.0.0.1:9460 --password-file /data/luyaz/web.env"
LW_PROD=luyaz-web
LW_URL=${U60S_LWEB_URL:-http://127.0.0.1:9090}
LW_TCP=$PROCDIR/net/tcp
LW_UP=20                                      # s for a started luyaz-web to answer

# 8d-2c: the agent is retired, :9090 is luyaz-web's alone (the engine stops
# the live one before the trial)
lw_trial_args() { echo "$LW_ARGS"; }
lw_launch_cmd() { echo "nohup $TEST_BIN $(lw_trial_args)"; }

# lw_answers: 0 = /health says 200 with a link, /login 200 with a page, and
# 0.0.0.0:9090 listens
lw_answers() {
    mkdir -p "$SHIP_TMP" 2>/dev/null
    _lb=$SHIP_TMP/lweb-health.$$
    _c=$($CURL -s -m 5 --noproxy '*' -o "$_lb" -w '%{http_code}' "$LW_URL/health" 2>/dev/null)
    _lk=$(sed -n 's/.*"link":"\([a-z]*\)".*/\1/p' "$_lb" 2>/dev/null)
    rm -f "$_lb"
    case "$_c" in
        200) ;;
        '' | 000) REASON=":9090 没有回答"; return 1 ;;
        *) REASON=":9090 /health 返回 $_c（应为 200）"; return 1 ;;
    esac
    if [ -z "$_lk" ]; then
        REASON=":9090 /health 的回答里没有 link"
        return 1
    fi
    _c=$($CURL -s -m 5 --noproxy '*' -o "$_lb" -w '%{http_code}' "$LW_URL/login" 2>/dev/null)
    _ls=$(wc -c <"$_lb" 2>/dev/null)
    rm -f "$_lb"
    if [ "$_c" != 200 ] || [ "$(num "$_ls")" = 0 ]; then
        REASON=":9090 /login 返回 ${_c:-000}、${_ls:-0} 字节（应为 200 和一页）"
        return 1
    fi
    # 0A = LISTEN; 00000000:2382 = 0.0.0.0:9090
    if ! awk '$2 == "00000000:2382" && $4 == "0A" { f = 1 } END { exit !f }' "$LW_TCP" 2>/dev/null; then
        REASON="没有在 0.0.0.0:9090 上听（只听本机？启动参数不对）"
        return 1
    fi
}

# lw_up <process name>: ≤ LW_UP s until it runs and answers
lw_up() {
    _i=0
    _why="进程没出现"
    while [ $_i -lt "$LW_UP" ]; do
        $SLEEP 1
        _i=$((_i + 1))
        [ -n "$($PIDOF "$1" 2>/dev/null)" ] || continue
        lw_answers || { _why=$REASON; continue; }
        return 0
    done
    return 1
}

# watch_lweb <process name> <window s> <label> [<md5>]: with an md5 (check)
# the pid must stay the same and run that file
watch_lweb() {
    T0=$(num "$(now)")
    _lp=$($PIDOF "$1" 2>/dev/null | awk '{ print $1 }')
    log "开始：$3 pid $_lp，窗口 ${2}s，每 ${INTERVAL}s 查一次"
    while :; do
        wait_step
        T=$(num "$(now)")
        _cur=$($PIDOF "$1" 2>/dev/null | awk '{ print $1 }')
        if [ -z "$_cur" ]; then
            REASON="$3不在了（pidof $1 为空）"
            return 1
        fi
        if [ -n "$4" ]; then
            if [ "$_cur" != "$_lp" ]; then
                REASON="$3被重启过（pid $_lp → $_cur）"
                return 1
            fi
            if [ "$(exe_md5 "$_cur")" != "$4" ]; then
                REASON="$3跑的不是这一版"
                return 1
            fi
        fi
        lw_answers || return 1
        [ $((T - T0)) -ge "$2" ] && return 0
    done
}

# stop: always through the init, even with nothing running: a build that crash-loops
# between respawns is still a procd instance (and after a first-ship rollback
# procd would go on respawning a program that is gone). A never-started service's
# stop does nothing.
c_lweb_stop() {
    log "停正式版（$LW_INITD stop）"
    step "$TO_INITD" "$LW_INITD" stop >/dev/null 2>&1
    _i=0
    while [ -n "$($PIDOF "$LW_PROD" 2>/dev/null)" ]; do
        if [ $_i -ge 15 ]; then
            REASON="正式版 15s 内没有退出（pid $($PIDOF "$LW_PROD" 2>/dev/null)）"
            return 1
        fi
        $SLEEP 1
        _i=$((_i + 1))
    done
}
c_lweb_start() {
    if [ ! -e "$LW_BIN" ]; then
        log "没有 ${LW_BIN#"$ROOT"}（第一次上机退回了）：不起"
        return 0
    fi
    _try=1
    while [ $_try -le 2 ]; do
        step "$TO_INITD" "$LW_INITD" start >/dev/null 2>&1
        if lw_up "$LW_PROD"; then
            log "正式版已起来（pid $($PIDOF "$LW_PROD" 2>/dev/null)，:9090 回答）"
            return 0
        fi
        _try=$((_try + 1))
    done
    REASON="正式版没起来（$_why）"
    log "！！正式版没起来：请手动 $LW_INITD start 并检查"
    return 1
}
c_lweb_launch_test() {
    mkdir -p "$(dirname "$TEST_LOG")" 2>/dev/null
    echo "=== $($DATE '+%Y-%m-%d %H:%M:%S') $(lw_launch_cmd)" >>"$TEST_LOG"
    log "launch：$(lw_launch_cmd)"
    # shellcheck disable=SC2086
    detach "$TEST_LOG" nohup "$TEST_BIN" $(lw_trial_args)
    lw_up "$TEST_NAME" || return 1
    log "launch：测试版已起来（pid $($PIDOF "$TEST_NAME" 2>/dev/null)，${_i}s）"
}
c_lweb_trial_watch() { watch_lweb "$TEST_NAME" "$X_TRIAL" 测试版; }
c_lweb_kill_test() { kill_test; }
c_lweb_check() { watch_lweb "$LW_PROD" "$X_CHECK" 正式版 "$(md5_of "$LW_BIN")"; }

# ── the screen: u60-uid and the touch UI (touch, uid; docs/SHIP.md) ─────────
# The panel takes one UI at a time (two fighting over /dev/dri/card0 = whole-
# device reboot), and a panel with no UI for minutes makes the firmware
# reboot the device (reason 1185). Processes are found by name with pidof
# (never pgrep -f): `pidof u60pro-devui` and `pidof u60pro-devui.test` do not
# see each other (the test build's comm is u60pro-devui.te, argv0
# …/u60pro-devui.test). u60-uid adopts any comm starting with u60pro-devui,
# so it is stopped for the whole touch trial.

# exe_md5 <pid>: md5 of the program the process runs (as loaded)
exe_md5() { md5sum <"$PROCDIR/$1/exe" 2>/dev/null | cut -d' ' -f1; }

first_pid() { $PIDOF "$1" 2>/dev/null | awk '{ print $1 }'; }

# live_pid NAME: the first pid of NAME that is not a zombie. An exited UI whose parent (the
# sentinel, during a hold) has not reaped it yet still has /proc/<pid> and a comm, so pidof
# finds it (10-11: two touch ships aborted on a UI that had exited at once).
live_pid() {
    for _lp in $($PIDOF "$1" 2>/dev/null); do
        _ls=$(awk '{ print $3 }' "$PROCDIR/$_lp/stat" 2>/dev/null)
        [ "$_ls" = Z ] || { echo "$_lp"; return 0; }
    done
}

# ui_pid: our touch UI, luyaz-ui (Phase 10 R5; u60pro-devui is gone since round ③), the first
# live one. pidof is exact, so the side-by-side build is looked for by its own TEST_NAME.
ui_pid() { live_pid luyaz-ui; }

# uid_stop: u60-uid stop (the UI stays on screen), ≤ 5 s until it is gone
uid_stop() {
    # the sentinel owns the screen (5f on): it puts the screen down and keeps
    # the rest of its duties (Wi-Fi, recovery, dropbear), §5 new 4
    if [ "$(screen_owner)" = sentinel ]; then
        sent_hold
        return
    fi
    [ -n "$(first_pid u60-uid)" ] || return 0
    log "停 u60-uid（$UID_INITD stop，界面留在屏上）"
    step "$TO_INITD" "$UID_INITD" stop >/dev/null 2>&1
    _i=0
    while [ -n "$(first_pid u60-uid)" ]; do
        if [ $_i -ge 5 ]; then
            REASON="u60-uid 5s 内没有退出（pid $(first_pid u60-uid)）"
            return 1
        fi
        $SLEEP 1
        _i=$((_i + 1))
    done
}

# uid_restart [<UI md5> [<u60-uid md5>]] (R8; the install kit calls it too):
# u60-uid stop → clear its launch count and give-up → start → ≤ UID_UP s
# until u60-uid and u60pro-devui both run, each program (/proc/<pid>/exe)
# with the md5 given (default: the live file on disk) → else start once
# more → else 1 with REASON. UI_PID = the UI's pid when it worked.
uid_restart() {
    if [ -n "$(sent_pids)" ]; then
        sent_release "$1"
        return
    fi
    _wu=${1:-$(md5_of "$(ui_bin)")}
    _wi=${2:-$(md5_of "$DEVUI_DIR/u60-uid")}
    uid_stop || return 1
    uid_clear
    _try=1
    while [ $_try -le 2 ]; do
        log "起 u60-uid（$UID_INITD start，第 $_try 次；要界面 ${_wu%"${_wu#????????}"}、u60-uid ${_wi%"${_wi#????????}"}）"
        step "$TO_INITD" "$UID_INITD" start >/dev/null 2>&1
        _i=0
        _why="u60-uid 或界面进程没出现"
        while :; do
            _pu=$(ui_pid)
            _pi=$(first_pid u60-uid)
            if [ -n "$_pu" ] && [ -n "$_pi" ]; then
                _mu=$(exe_md5 "$_pu")
                _mi=$(exe_md5 "$_pi")
                if [ "$_mu" = "$_wu" ] && [ "$_mi" = "$_wi" ]; then
                    UI_PID=$_pu
                    log "u60-uid（pid $_pi）和界面（pid $_pu）都在，版本对"
                    return 0
                fi
                _why="版本不对（界面 ${_mu:-?}，u60-uid ${_mi:-?}）"
            fi
            [ $_i -ge "$UID_UP" ] && break
            $SLEEP 1
            _i=$((_i + 1))
        done
        log "u60-uid 第 $_try 次没起好：$_why"
        _try=$((_try + 1))
    done
    REASON="u60-uid 两次 start 都没起好：$_why"
    log "！！$REASON：请手动 $UID_INITD start 并检查"
    return 1
}

# watch_screen <window s> <want UI md5>: every INTERVAL s: u60-uid and the
# UI run, the UI is the same process at the version wanted (not restarted),
# u60-uid logged no give-up / vendor hand-over / unrequested UI exit.
watch_screen() {
    _ww=$1
    _wpid=${UI_PID:-$(ui_pid)}
    uid_bad_reset
    uid_bad_count
    _wbase=$UB_N
    T0=$(num "$(now)")
    log "开始：界面检查 pid $_wpid，窗口 ${_ww}s，每 ${INTERVAL}s 查一次"
    while :; do
        wait_step
        T=$(num "$(now)")
        if [ -z "$(screen_owner)" ]; then
            REASON="屏幕主人（u60-uid 或哨兵）不在了"
            return 1
        fi
        _cur=$(ui_pid)
        if [ -z "$_cur" ]; then
            REASON="界面进程不在了"
            return 1
        fi
        if [ "$_cur" != "$_wpid" ]; then
            REASON="界面被重启过（pid $_wpid → $_cur）"
            return 1
        fi
        if [ "$(exe_md5 "$_cur")" != "$2" ]; then
            REASON="界面进程跑的不是这一版"
            return 1
        fi
        uid_bad_count
        N=$UB_N
        if [ "$N" -lt "$_wbase" ]; then
            _wbase=$N
        elif [ "$N" -gt "$_wbase" ]; then
            REASON="界面异常（u60-uid：$($LOGREAD 2>/dev/null | grep -E "$UID_BAD" | grep -v '(requested)' | tail -n 1 | cut -c1-200)）"
            return 1
        fi
        if [ $((T - T0)) -ge "$_ww" ]; then
            # under the sentinel the core judges touch too (§7, D5): fail closed
            if [ "$(screen_owner)" = sentinel ] && [ "$C_NAME" = touch ]; then
                _vd=$(verdict_of_txn)
                if [ "${_vd%% *}" != ok ]; then
                    REASON="核心的判定没到 ok（${_vd:-没有判定}）：按 D5 不转正"
                    return 1
                fi
            fi
            return 0
        fi
        if [ "$(screen_owner)" = sentinel ] && [ "$C_NAME" = touch ]; then
            _vd=$(verdict_of_txn)
            if [ "${_vd%% *}" = fail ]; then
                REASON="核心判定不过：${_vd#* }"
                return 1
            fi
        fi
    done
}

touch_launch_cmd() { echo "nohup sh -c cd \"\$1\" && exec \"\$2\" sh $UI_DIR $TEST_BIN"; }

c_touch_stop() {
    TOUCH_RESTARTED=
    uid_stop || return 1
    uid_clear
    _p=$(ui_pid)
    if [ -n "$_p" ]; then
        log "停正式界面（pid $_p）"
        $KILL $($PIDOF luyaz-ui 2>/dev/null) 2>/dev/null
    fi
    # really gone before anything else opens the panel
    _i=0
    while [ -n "$(ui_pid)" ]; do
        if [ $_i -ge 10 ]; then
            REASON="正式界面 10s 内没有退出（pid $(ui_pid)）"
            uid_restart >/dev/null
            return 1
        fi
        [ $_i = 5 ] && $KILL -9 $($PIDOF luyaz-ui 2>/dev/null) 2>/dev/null
        $SLEEP 1
        _i=$((_i + 1))
    done
    BLANK_T0=$(num "$(now)")
}
c_touch_start() {
    if [ -n "$TOUCH_RESTARTED" ] && [ -n "$(first_pid u60-uid)" ] && [ -n "$(ui_pid)" ]; then
        return 0 # trial_watch put production back already
    fi
    uid_restart
}
# The test build: what u60-uid runs (chdir into the plugin directory, exec
# the binary; src/uid.c launch()), minus u60-uid: a crash must stay a crash.
c_touch_launch_test() {
    mkdir -p "$(dirname "$TEST_LOG")" 2>/dev/null
    echo "=== $($DATE '+%Y-%m-%d %H:%M:%S') $(touch_launch_cmd)" >>"$TEST_LOG"
    log "launch：$(touch_launch_cmd)"
    # shellcheck disable=SC2016
    detach "$TEST_LOG" nohup sh -c 'cd "$1" && exec "$2"' sh "$UI_DIR" "$TEST_BIN"
    _why="测试版没出现，屏幕空了 ${BLANK_MAX}s"
    while [ $(($(num "$(now)") - ${BLANK_T0:-0})) -lt "$BLANK_MAX" ]; do
        if [ -n "$(first_pid "$TEST_NAME")" ]; then
            TEST_PID=$(first_pid "$TEST_NAME")
            log "launch：测试版已起来（pid $TEST_PID，屏幕空了 $(($(num "$(now)") - ${BLANK_T0:-0}))s）"
            return 0
        fi
        $SLEEP 1
    done
    return 1
}
# Every second for the first 30 s, then every 2 s. The test build gone →
# production back at once (the panel must not stay dark), then fail.
c_touch_trial_watch() {
    T0=$(num "$(now)")
    _tn=0
    log "开始：测试版 pid $TEST_PID，窗口 ${X_TRIAL}s，前 30s 每秒查、之后每 2s"
    while :; do
        $SLEEP 1
        _tn=$((_tn + 1))
        T=$(num "$(now)")
        if [ $((T - T0)) -le 30 ] || [ $((_tn % 2)) = 0 ] || [ $((T - T0)) -ge "$X_TRIAL" ]; then
            if [ -z "$(first_pid "$TEST_NAME")" ]; then
                REASON="测试版崩溃（第 $((T - T0))s，pidof $TEST_NAME 为空）"
                log "试跑：$REASON，马上拉起正式版"
                uid_restart && TOUCH_RESTARTED=1
                return 1
            fi
        fi
        [ $((T - T0)) -ge "$X_TRIAL" ] && return 0
        # the sentinel caps a hold at 3600 s (HOLD_MAX_S): renew it every 10 min, or a trial of
        # an hour or more outlives it and the sentinel relaunches the old UI before promote
        # (10-11: a 60m trial rolled back that way)
        if [ $((_tn % 600)) = 0 ] && [ "$(screen_owner)" = sentinel ]; then
            sent_renew
        fi
    done
}
c_touch_kill_test() {
    kill_test || return 1
    BLANK_T0=$(num "$(now)")
}
c_touch_check() { watch_screen "$X_CHECK" "$(md5_of "$(ui_bin)")"; }

c_uid_stop() { uid_stop; }
c_uid_start() { uid_restart; }
c_uid_check() { watch_screen "$X_CHECK" "$(md5_of "$(ui_bin)")"; }

# ── guard (/data/u60-guard + our four /etc/init.d scripts) ──────────────────
# Before: the OLD doctor's --tsv (cannot run → no promotion). Stop: the
# stop-requested marker (the ledger then calls the next start a requested
# one), init.d stop. Check 300 s: the guard runs, one kmsg capture pipeline,
# every new file is in place, sh -n of the scripts, doctor --ledger-selftest
# passes, and the new doctor's --tsv is no worse than the old one row by row
# (standby, clock and manifest left out; old ok → new not ok = worse).
# guard_pids: the main loop's pid. procd's own record of the instance
# (ubus service list) first: u60-guard.sh's background jobs are ( … ) &
# subshells with the very same cmdline, and one still finishing after a stop
# must neither hold up the stop nor pass for a running guard. Without ubus
# (tests, odd builds): every process whose cmdline is exactly
# /bin/sh …/u60-guard.sh.
guard_pids() {
    if command -v "$UBUS" >/dev/null 2>&1; then
        _gp=$($UBUS call service list '{"name":"u60-guard"}' 2>/dev/null | tr -d ' \t\n' |
            sed -n 's/.*"running":true,"pid":\([0-9][0-9]*\).*/\1/p' | head -n 1)
        [ -n "$_gp" ] && { tr '\0' ' ' <"$PROCDIR/$_gp/cmdline"; } 2>/dev/null | grep -q "u60-guard\.sh" && echo "$_gp"
        return 0
    fi
    for _gd in "$PROCDIR"/[0-9]*; do
        { tr '\0' ' ' <"$_gd/cmdline"; } 2>/dev/null | grep -qx "\(/bin/\)\{0,1\}sh $GUARD_DIR/u60-guard\.sh " && echo "${_gd##*/}"
    done
}
# kmsg_readers: processes that are exactly "cat <kmsg>" (the capture
# pipeline's reader; its sh -c wrapper has more words and does not count)
kmsg_readers() {
    _kn=0
    for _gd in "$PROCDIR"/[0-9]*; do
        { tr '\0' ' ' <"$_gd/cmdline"; } 2>/dev/null | grep -qx "cat $KMSG_DEV " && _kn=$((_kn + 1))
    done
    echo "$_kn"
}
# guard_doctor <output file> <args…>: run the live doctor.sh with a timeout
guard_doctor() {
    _go=$1
    shift
    rm -f "$_go"
    step "$TO_DOCTOR" sh -c 'f=$1; shift; sh "$f" "$@" >"$0" 2>&1' "$_go" "$SCRIPTS_DIR/doctor.sh" "$@"
}
c_guard_pre() {
    mkdir -p "$SHIP_TMP" 2>/dev/null
    GD_OLD=$SHIP_TMP/doctor-before.tsv
    guard_doctor "$GD_OLD" --tsv
    if [ $? = 124 ] || ! grep -q '^[a-z]*	[a-z0-9_-]*	' "$GD_OLD" 2>/dev/null; then
        REASON="旧 doctor.sh --tsv 跑不完，没有转正"
        return 1
    fi
    log "旧 doctor --tsv：$(grep -c . "$GD_OLD") 行"
}
c_guard_stop() {
    mkdir -p "${GUARD_STOPREQ%/*}" 2>/dev/null
    echo "u60-ship $X_TXN" >"$GUARD_STOPREQ" 2>/dev/null
    log "停 guard（$GUARD_INITD stop）"
    step "$TO_INITD" "$GUARD_INITD" stop >/dev/null 2>&1
    _i=0
    while [ -n "$(guard_pids)" ]; do
        if [ $_i -ge 15 ]; then
            REASON="guard 15s 内没有退出（pid $(guard_pids | tr '\n' ' ')）"
            return 1
        fi
        $SLEEP 1
        _i=$((_i + 1))
    done
}
c_guard_start() {
    _try=1
    while [ $_try -le 2 ]; do
        step "$TO_INITD" "$GUARD_INITD" start >/dev/null 2>&1
        _i=0
        while [ $_i -lt 15 ]; do
            if [ -n "$(guard_pids)" ]; then
                log "guard 已起来（pid $(guard_pids | tr '\n' ' ')）"
                return 0
            fi
            $SLEEP 1
            _i=$((_i + 1))
        done
        _try=$((_try + 1))
    done
    REASON="guard 没起来（$GUARD_INITD start 两次）"
    log "！！$REASON"
    return 1
}
c_guard_check() {
    T0=$(num "$(now)")
    _gonce=
    log "开始：guard 检查，窗口 ${X_CHECK}s，每 ${INTERVAL}s 查一次"
    while :; do
        wait_step
        T=$(num "$(now)")
        if [ -z "$(guard_pids)" ]; then
            REASON="guard 进程不在了"
            return 1
        fi
        if [ -e "$KMSG_DEV" ] && [ "$(kmsg_readers)" != 1 ]; then
            REASON="kmsg 落盘管道有 $(kmsg_readers) 条（应为 1）"
            return 1
        fi
        _ifs=$IFS
        IFS=$NL
        for _e in $X_FILES; do
            IFS=$_ifs
            if [ "$(md5_of "${_e%%|*}")" != "${_e##*|}" ]; then
                IFS=$_ifs
                REASON="${_e%%|*} 不是这一版了"
                return 1
            fi
        done
        IFS=$_ifs
        if [ -z "$_gonce" ]; then
            # once, after the first interval (the new guard has started its capture)
            _gonce=1
            for _e in $X_FILES; do
                _p=${_e%%|*}
                if is_script "$_p" && ! sh -n "$_p" 2>/dev/null; then
                    REASON="$_p 有语法错误（sh -n 不过）"
                    return 1
                fi
            done
            guard_doctor "$SHIP_TMP/ledger-selftest.out" --ledger-selftest
            if [ $? != 0 ]; then
                REASON="doctor.sh --ledger-selftest 没通过：$(grep -m1 '^FAIL' "$SHIP_TMP/ledger-selftest.out" 2>/dev/null | cut -c1-120)"
                return 1
            fi
            log "sh -n、doctor --ledger-selftest 通过"
        fi
        [ $((T - T0)) -ge "$X_CHECK" ] && break
    done
    # the new doctor against the old one, row by row
    _gn=$SHIP_TMP/doctor-after.tsv
    guard_doctor "$_gn" --tsv
    if [ $? = 124 ] || ! grep -q '^[a-z]*	[a-z0-9_-]*	' "$_gn" 2>/dev/null; then
        REASON="新 doctor.sh --tsv 跑不完"
        return 1
    fi
    _worse=$(awk -F '\t' '
        $2 == "standby" || $2 == "clock" || $2 == "manifest" { next }
        NR == FNR { old[$2] = $1; next }
        ($2 in old) && old[$2] == "ok" && $1 != "ok" { printf "%s%s（%s→%s）", (n++ ? "、" : ""), $2, old[$2], $1 }
    ' "$SHIP_TMP/doctor-before.tsv" "$_gn")
    if [ -n "$_worse" ]; then
        REASON="doctor 变坏：$_worse"
        return 1
    fi
    log "doctor --tsv 和换之前比没有变坏"
}

# selftest: flags in the sandbox decide.
c_selftest_stop() { echo stop >>"$ROOT/selftest.ops"; }
c_selftest_start() {
    echo start >>"$ROOT/selftest.ops"
    [ ! -f "$ROOT/selftest-start-fail" ] && return 0
    REASON="selftest：起不来"
    return 1
}
c_selftest_check() {
    # the new version writes its state: a rollback must undo that
    echo "written by the new version" >"$ROOT/data/selftest-state/a"
    _w=0
    while [ "$_w" -lt "$X_CHECK" ]; do
        $SLEEP 1
        _w=$((_w + 1))
    done
    [ ! -f "$ROOT/selftest-check-fail" ] && return 0
    REASON="selftest：检查不过"
    return 1
}

# ── transaction log ─────────────────────────────────────────────────────────
# X_* hold the log in memory; X_FILES / X_STATES / X_STATELIST are
# newline-separated entries (paths are checked: no spaces, no glob chars).
NL='
'

txn_clear() {
    X_V= X_TXN= X_COMP= X_PHASE= X_REASON= X_BOOT= X_COMMIT= X_MACTIME=
    X_FORMAT= X_TRIAL= X_CHECK= X_EXEC= X_TSTAGE= X_TPHASE= X_NOTE=
    X_FILES= X_DIRS= X_STATES= X_STATELIST=
}

txn_load() {
    txn_clear
    [ -f "$TXN_FILE" ] || return 1
    _end=
    while IFS= read -r _l || [ -n "$_l" ]; do
        case "$_l" in
            end=1) _end=1 ;;
            v=*) X_V=${_l#v=} ;;
            txn=*) X_TXN=${_l#txn=} ;;
            comp=*) X_COMP=${_l#comp=} ;;
            phase=*) X_PHASE=${_l#phase=} ;;
            reason=*) X_REASON=${_l#reason=} ;;
            boot_id=*) X_BOOT=${_l#boot_id=} ;;
            commit=*) X_COMMIT=${_l#commit=} ;;
            mac_time=*) X_MACTIME=${_l#mac_time=} ;;
            format=*) X_FORMAT=${_l#format=} ;;
            trial=*) X_TRIAL=${_l#trial=} ;;
            note=*) X_NOTE=${_l#note=} ;;
            check=*) X_CHECK=${_l#check=} ;;
            exec_pid=*) X_EXEC=${_l#exec_pid=} ;;
            t_stage=*) X_TSTAGE=${_l#t_stage=} ;;
            t_phase=*) X_TPHASE=${_l#t_phase=} ;;
            file=*) X_FILES="$X_FILES${_l#file=}$NL" ;;
            dir=*) X_DIRS="$X_DIRS${_l#dir=}$NL" ;;
            state=*) X_STATES="$X_STATES${_l#state=}$NL" ;;
            statelist=*) X_STATELIST="$X_STATELIST${_l#statelist=}$NL" ;;
        esac
    done <"$TXN_FILE"
    case "$X_V" in 1 | 2 | 3 | 4 | 5) ;; *) return 1 ;; esac
    [ "$_end" = 1 ]
}

# txn_v: the log version this transaction needs. 2 only for what v=1 cannot
# say (docs/SHIP.md): a directory, a program that did not exist, a program
# under /etc; 3 for rc.local or the sentinel's init (u60-recover.sh formats 3);
# 4 for luyaz-web's init (formats 4); otherwise 1, which the phase-one
# u60-recover.sh reads. The highest any entry needs (one entry never lowers it).
txn_v() {
    _tv=1
    [ -n "$X_DIRS" ] && _tv=2
    _ifs=$IFS
    IFS=$NL
    for _e in $X_FILES; do
        _r=${_e#*|}
        _ev=1
        [ "${_r%%|*}" = - ] && _ev=2
        case "${_e%%|*}" in "$ROOT"/etc/*) _ev=2 ;; esac
        case "${_e%%|*}" in "$ROOT"/etc/rc.local | "$ROOT"/etc/init.d/luyaz-sentinel) _ev=3 ;; esac
        case "${_e%%|*}" in "$LW_INITF") _ev=4 ;; esac
        case "${_e%%|*}" in "$ROOT"/etc/init.d/luyaz-core) _ev=5 ;; esac
        [ "$_ev" -gt "$_tv" ] && _tv=$_ev
    done
    IFS=$_ifs
    echo "$_tv"
}

# prev_of <path>: where the version before $X_TXN is kept: <path>.prev-<txn>,
# except /etc/init.d/<name> (no extra file that looks like a service there):
# $SHIP_DIR/prev/etc.init.d.<name>.prev-<txn>. u60-recover.sh has the same rule.
prev_of() {
    case "$1" in
        "$ROOT"/etc/init.d/*) echo "$SHIP_DIR/prev/etc.init.d.${1##*/}.prev-$X_TXN" ;;
        "$ROOT"/etc/rc.local) echo "$SHIP_DIR/prev/etc.rc.local.prev-$X_TXN" ;;
        *) echo "$1.prev-$X_TXN" ;;
    esac
}

txn_save() {
    mkdir -p "$SHIP_DIR" 2>/dev/null
    _tt=$TXN_FILE.tmp.$$
    {
        echo "v=$(txn_v)"
        echo "txn=$X_TXN"
        echo "comp=$X_COMP"
        echo "phase=$X_PHASE"
        echo "reason=$X_REASON"
        echo "boot_id=$X_BOOT"
        echo "commit=$X_COMMIT"
        echo "mac_time=$X_MACTIME"
        echo "format=$X_FORMAT"
        echo "trial=$X_TRIAL"
        echo "note=$X_NOTE"
        echo "check=$X_CHECK"
        echo "exec_pid=$X_EXEC"
        echo "t_stage=$X_TSTAGE"
        echo "t_phase=$X_TPHASE"
        printf '%s' "$X_FILES" | sed 's/^/file=/'
        printf '%s' "$X_DIRS" | sed 's/^/dir=/'
        printf '%s' "$X_STATELIST" | sed 's/^/statelist=/'
        printf '%s' "$X_STATES" | sed 's/^/state=/'
        echo end=1
    } >"$_tt" 2>/dev/null || {
        rm -f "$_tt"
        return 1
    }
    sync_hb
    mv -f "$_tt" "$TXN_FILE" || {
        rm -f "$_tt"
        return 1
    }
    sync_hb
}

heartbeat() {
    [ -n "$X_TXN" ] || return 0
    mkdir -p "$SHIP_TMP" 2>/dev/null
    echo "$(now) $$ $X_TXN $X_PHASE" >"$HB_FILE.tmp.$$" 2>/dev/null &&
        mv -f "$HB_FILE.tmp.$$" "$HB_FILE" 2>/dev/null
}

hb_sleep() {
    $REAL_SLEEP "$1"
    heartbeat
}

# set_phase <phase> [reason]: log first (docs/SHIP.md). 1 = log not written.
set_phase() {
    X_PHASE=$1
    X_REASON=$(printf '%s' "$2" | tr -d '|' | tr '\n' ' ')
    X_TPHASE=$(now)
    heartbeat
    log "事务 $X_TXN（$X_COMP）：$1${2:+：$2}"
    txn_save && return 0
    log "！！事务日志写不上（$TXN_FILE）"
    return 1
}

# txn_busy: one line if a transaction blocks new work (active or failed).
txn_busy() {
    [ -f "$TXN_FILE" ] || return 0
    (
        if ! txn_load; then
            echo "事务日志 $TXN_FILE 看不懂：先看 ${LOG##*/} 手工处理"
        elif in_list "$X_PHASE" "$ACTIVE"; then
            echo "u60-ship 事务 $X_TXN（$X_COMP）还在进行：$X_PHASE"
        elif [ "$X_PHASE" = failed ]; then
            echo "u60-ship 事务 $X_TXN（$X_COMP）停在 failed（$X_REASON）：先手工处理"
        fi
    )
}

exec_alive() { # exec_alive <pid>: alive and it is the executor, either name (pid reuse)
    [ -n "$1" ] && [ -d "/proc/$1" ] && tr '\0' ' ' <"/proc/$1/cmdline" 2>/dev/null | grep -q -e 'u60-ship\.sh' -e 'luyaz-ship\.sh'
}

# hb_read: HB_UP, HB_PID, HB_AGE (s; empty = no heartbeat)
hb_read() {
    HB_UP= HB_PID= HB_AGE=
    [ -f "$HB_FILE" ] || return 1
    read -r HB_UP HB_PID _r <"$HB_FILE" 2>/dev/null
    HB_UP=$(num "$HB_UP")
    HB_PID=$(num "$HB_PID")
    HB_AGE=$(($(num "$(now)") - HB_UP))
}

# ── files ───────────────────────────────────────────────────────────────────

# cp_sync <from> <to>: cp -p into a temp name next to <to>, sync, mv, sync.
cp_sync() {
    cp -p "$1" "$2.cp-$$" 2>/dev/null || {
        rm -f "$2.cp-$$"
        return 1
    }
    sync_hb
    mv -f "$2.cp-$$" "$2" || {
        rm -f "$2.cp-$$"
        return 1
    }
    sync_hb
}

# restore_one <path> <old md5 or -> <what>: put the old version back.
# Already old → nothing. The .prev must have the old md5, else the live file
# is not touched and 1 is returned (a torn .prev never overwrites anything).
restore_one() {
    _rp=$1
    _ro=$2
    _rprev=$(prev_of "$_rp")
    if [ "$_ro" = - ]; then
        [ -e "$_rp" ] || [ -L "$_rp" ] || return 0
        rm -f "$_rp" && sync_hb && log "还原：$3 $_rp 原来不存在，删掉" && return 0
        return 1
    fi
    [ "$(md5_of "$_rp")" = "$_ro" ] && return 0
    if [ "$(md5_of "$_rprev")" != "$_ro" ]; then
        log "！！还原不了 $_rp：$_rprev 不在或 md5 不对"
        return 1
    fi
    if [ "$_rp" = "$RC_LOCAL" ] && [ "$(rc_mode "$_rprev")" = legacy ] && ! legacy_here; then
        log "！！不还原 $_rp：那一份起 u60-uid + u60-guard，它们已经删了（Phase 10 10c）"
        return 1
    fi
    cp_sync "$_rprev" "$_rp" || return 1
    [ "$(md5_of "$_rp")" = "$_ro" ] || return 1
    log "还原：$3 $_rp ← ${_rprev##*/}"
}

# restore_dir <path> <old fingerprint>: the directory back from its .prev,
# idempotent (docs/SHIP.md; u60-recover.sh does the same). Already old →
# nothing; the .prev is not the old one → 1 and nothing is touched.
restore_dir() {
    _rp=$1
    _ro=$2
    _rprev=$_rp.prev-$X_TXN
    if [ "$(tree_fp "$_rp")" = "$_ro" ]; then
        # already old; leftovers of a restore cut short after its last mv
        rm -rf "$_rp.rec-tmp" "$_rp.bad-$X_TXN"
        return 0
    fi
    if [ "$(tree_fp "$_rprev")" != "$_ro" ]; then
        log "！！还原不了 $_rp：$_rprev 不在或指纹不对"
        return 1
    fi
    rm -rf "$_rp.rec-tmp"
    cp -a "$_rprev" "$_rp.rec-tmp" 2>/dev/null || {
        rm -rf "$_rp.rec-tmp"
        return 1
    }
    sync_hb
    crashpoint "rollback:dir-copied:${_rp##*/}"
    if [ "$(tree_fp "$_rp.rec-tmp")" != "$_ro" ]; then
        rm -rf "$_rp.rec-tmp"
        return 1
    fi
    rm -rf "$_rp.bad-$X_TXN"
    if [ -e "$_rp" ] || [ -L "$_rp" ]; then
        mv -f "$_rp" "$_rp.bad-$X_TXN" || return 1
    fi
    crashpoint "rollback:dir-bad:${_rp##*/}"
    mv -f "$_rp.rec-tmp" "$_rp" || return 1
    crashpoint "rollback:dir-moved:${_rp##*/}"
    sync_hb
    rm -rf "$_rp.bad-$X_TXN"
    log "还原：目录 $_rp ← ${_rprev##*/}"
}

restore_all() { # restore_all files|dirs|states → 0 all back, 1 something not
    _bad=0
    _ifs=$IFS
    IFS=$NL
    case "$1" in
        files) _list=$X_FILES ;;
        dirs) _list=$X_DIRS ;;
        *) _list=$X_STATES ;;
    esac
    for _e in $_list; do
        IFS=$_ifs
        _p=${_e%%|*}
        _rest=${_e#*|}
        _o=${_rest%%|*}
        if [ "$1" = dirs ]; then
            restore_dir "$_p" "$_o" || _bad=1
        else
            restore_one "$_p" "$_o" "$1" || _bad=1
        fi
        crashpoint "rollback:$1:${_p##*/}"
    done
    IFS=$_ifs
    return $_bad
}

# state_snapshot: every state file → .prev-<txn> (sync), X_STATES filled.
state_snapshot() {
    X_STATES=
    _ifs=$IFS
    IFS=$NL
    for _p in $X_STATELIST; do
        IFS=$_ifs
        if [ -f "$_p" ]; then
            _o=$(md5_of "$_p")
            cp_sync "$_p" "$(prev_of "$_p")" || {
                REASON="状态文件 $_p 拷不了"
                return 1
            }
            [ "$(md5_of "$(prev_of "$_p")")" = "$_o" ] || {
                REASON="状态文件 $_p 的副本 md5 不对"
                return 1
            }
        else
            _o=-
        fi
        X_STATES="$X_STATES$_p|$_o$NL"
    done
    IFS=$_ifs
}

# promote_files: per file cp live → .prev (sync), check both md5s, mv the
# side file over the live one, sync.
promote_files() {
    _ifs=$IFS
    IFS=$NL
    for _e in $X_FILES; do
        IFS=$_ifs
        _p=${_e%%|*}
        _rest=${_e#*|}
        _o=${_rest%%|*}
        _n=${_rest#*|}
        _b=${_p##*/}
        _pv=$(prev_of "$_p")
        if [ "$_o" = - ]; then
            # a new file: nothing to keep; it must still not be there
            if [ -e "$_p" ] || [ -L "$_p" ]; then
                REASON="$_p 暂存时不存在，现在却有了"
                return 1
            fi
        else
            mkdir -p "${_pv%/*}" 2>/dev/null
            cp -p "$_p" "$_pv" 2>/dev/null || {
                REASON="$_p 拷不成上一版"
                return 1
            }
            crashpoint "promote:prev-copied:$_b"
            sync_hb
            crashpoint "promote:prev-synced:$_b"
            [ "$(md5_of "$_pv")" = "$_o" ] || {
                REASON="上一版副本 ${_pv##*/} 的 md5 不对"
                return 1
            }
        fi
        [ "$(md5_of "$_p.test")" = "$_n" ] || {
            REASON="旁路文件 $_p.test 的 md5 不对"
            return 1
        }
        mv -f "$_p.test" "$_p" || {
            REASON="$_p.test 换不上"
            return 1
        }
        crashpoint "promote:moved:$_b"
        sync_hb
        log "转正：$_p（${_o%"${_o#????????}"} → ${_n%"${_n#????????}"}）"
    done
    IFS=$_ifs
}

# promote_dirs: per directory (docs/SHIP.md) check both fingerprints, then
# mv live → .prev-<txn>, sync, mv side → live, sync. A power cut between the
# two renames leaves no live directory: the log says how to put it back.
promote_dirs() {
    _ifs=$IFS
    IFS=$NL
    for _e in $X_DIRS; do
        IFS=$_ifs
        _p=${_e%%|*}
        _rest=${_e#*|}
        _o=${_rest%%|*}
        _n=${_rest#*|}
        _b=${_p##*/}
        [ "$(tree_fp "$_p")" = "$_o" ] || {
            REASON="正式目录 $_p 在暂存以后变了（指纹不是暂存时的）"
            return 1
        }
        [ "$(tree_fp "$_p.test")" = "$_n" ] || {
            REASON="旁路目录 $_p.test 的指纹不对"
            return 1
        }
        rm -rf "$_p.prev-$X_TXN"
        mv -f "$_p" "$_p.prev-$X_TXN" || {
            REASON="$_p 移不成上一版"
            return 1
        }
        crashpoint "promote:dir-away:$_b"
        sync_hb
        crashpoint "promote:dir-away-synced:$_b"
        mv -f "$_p.test" "$_p" || {
            REASON="$_p.test 换不上"
            return 1
        }
        crashpoint "promote:dir-moved:$_b"
        sync_hb
        crashpoint "promote:dir-moved-synced:$_b"
        log "转正：目录 $_p（${_o%"${_o#????????}"} → ${_n%"${_n#????????}"}）"
    done
    IFS=$_ifs
}

# prune_prev <path>: keep the newest KEEP_PREV ship-made .prev-<txn> of it.
# Hand-made backups (.prev-esim-…, .orig-backup, …) do not match TXN_RE.
prune_prev() {
    _pb=$(prev_of "$1")
    _pb=${_pb%.prev-"$X_TXN"}
    _d=${_pb%/*}
    _b=$(printf '%s' "${_pb##*/}" | sed 's/\./\\./g')
    _all=$(ls -1 "$_d" 2>/dev/null | grep -E "^$_b\.prev-[0-9]{8}-[0-9]{6}-[a-z0-9]{1,12}\$" | sort)
    _cnt=$(num "$(printf '%s' "$_all" | grep -c .)")
    _del=$((_cnt - KEEP_PREV))
    [ "$_del" -gt 0 ] || return 0
    for _f in $(printf '%s\n' "$_all" | sed -n "1,${_del}p"); do
        rm -rf "${_d:?}/$_f" && log "清理：删掉较早的上一版 $_d/$_f（每个文件留 ${KEEP_PREV} 份）"
    done
}

prune_all() {
    _ifs=$IFS
    IFS=$NL
    for _e in $X_FILES $X_DIRS $X_STATES; do
        IFS=$_ifs
        prune_prev "${_e%%|*}"
    done
    IFS=$_ifs
}

# ── manifest (/data/luyaz/ship/manifest.jsonl) ─────────────────────────────────────

manifest_line() {
    _files=
    _ifs=$IFS
    IFS=$NL
    for _e in $X_FILES; do
        _p=${_e%%|*}
        _n=${_e##*|}
        _r=${_e#*|}
        if [ "${_r%%|*}" = - ]; then
            # new file: no version before it
            _files="$_files${_files:+,}{\"path\":\"$_p\",\"md5\":\"$_n\"}"
        else
            _files="$_files${_files:+,}{\"path\":\"$_p\",\"md5\":\"$_n\",\"prev\":\"$(prev_of "$_p")\"}"
        fi
    done
    for _e in $X_DIRS; do
        _p=${_e%%|*}
        _n=${_e##*|}
        _files="$_files${_files:+,}{\"path\":\"$_p\",\"tree\":\"$_n\",\"prev\":\"$_p.prev-$X_TXN\"}"
    done
    _st=
    for _e in $X_STATES; do
        _st="$_st${_st:+,}\"${_e%%|*}\""
    done
    IFS=$_ifs
    manifest_records
    _nt=
    [ -n "$X_NOTE" ] && _nt=",\"note\":\"$X_NOTE\""
    printf '{"v":1,"kind":"ship","comp":"%s","txn":"%s","commit":"%s","format":%s,"mac_time":%s,"boot_id":"%s","uptime":%s,"files":[%s],"state":[%s]%s}\n' \
        "$X_COMP" "$X_TXN" "$X_COMMIT" "$(num "$X_FORMAT")" "$(num "$X_MACTIME")" "$X_BOOT" "$(num "$(now)")" "$_files" "$_st" "$_nt"
}

# manifest_records: a kind=record line for every file of this transaction
# that is also on the record list (the /etc/init.d scripts guard ships), with
# its new md5 — else doctor would call it changed until a `u60 record`. Printed
# before the ship line, in the same write (manifest_has_txn stays the test).
manifest_records() {
    _ifs=$IFS
    IFS=$NL
    for _e in $X_FILES; do
        IFS=$_ifs
        _p=${_e%%|*}
        _n=${_e##*|}
        _rn=$(record_list | awk -v p="$_p" '$2 == p && $3 != "tree" { print $1; exit }')
        [ -n "$_rn" ] || continue
        printf '{"v":1,"kind":"record","name":"%s","path":"%s","md5":"%s","mac_time":%s,"boot_id":"%s","uptime":%s,"why":"ship %s"}\n' \
            "$_rn" "$_p" "$_n" "$(num "$X_MACTIME")" "$X_BOOT" "$(num "$(now)")" "$X_TXN"
    done
    IFS=$_ifs
}

manifest_has_txn() { grep -q "\"txn\":\"$X_TXN\"" "$MANIFEST" 2>/dev/null; }

# manifest_write: old content + one line → temp → sync → mv → sync. Idempotent.
manifest_write() {
    manifest_has_txn && return 0
    _mt=$MANIFEST.tmp.$$
    {
        [ -f "$MANIFEST" ] && cat "$MANIFEST"
        manifest_line
    } >"$_mt" 2>/dev/null || {
        rm -f "$_mt" 2>/dev/null
        REASON="清单写不上（$MANIFEST）"
        return 1
    }
    crashpoint manifest:tmp
    sync_hb
    mv -f "$_mt" "$MANIFEST" 2>/dev/null || {
        rm -f "$_mt"
        REASON="清单换不上（$MANIFEST）"
        return 1
    }
    crashpoint manifest:moved
    sync_hb
}

# The files the manifest only records (R1/D10): settings that change in
# normal use are not among them. doctor.sh has the same list.
record_list() {
    echo "tailscale-start.sh $ROOT/data/tailscale/start.sh"
    echo "tuning.env $ROOT/data/tailscale/tuning.env"
    echo "tailscaled $ROOT/data/tailscale/tailscaled"
    echo "tailscaled-nofight $ROOT/data/tailscale/nofight/tailscaled"
    for _s in $CORE_NAME tailscale; do echo "init.d/$_s $ROOT/etc/init.d/$_s"; done
    echo "rc.local $ROOT/etc/rc.local"
    echo "luyaz-recover.sh $SHIP_DIR/$REC_NAME"
    # directories (third word "tree"): recorded by their fingerprint
    if [ -d "$UI_DIR/fonts" ]; then echo "ui-fonts $UI_DIR/fonts tree"; else echo "ui-fonts $DEVUI_DIR/fonts tree"; fi
    echo "ui-logos $UI_DIR/operator-logos tree"
    # u60d's policy components (manager crates/luyaz-plugins); u60d reads them at start
    echo "core-plugins $ROOT/data/luyaz/core/plugins tree"
}

# manifest_append <line>: old content + the line → temp → sync → mv → sync.
manifest_append() {
    _mt=$MANIFEST.tmp.$$
    {
        [ -f "$MANIFEST" ] && cat "$MANIFEST"
        printf '%s\n' "$1"
    } >"$_mt" 2>/dev/null || {
        rm -f "$_mt" 2>/dev/null
        return 1
    }
    sync_hb
    mv -f "$_mt" "$MANIFEST" 2>/dev/null || {
        rm -f "$_mt"
        return 1
    }
    sync_hb
}

# record <name|all> <mac time> [why…]: write the current md5 of a file we only
# record ("u60 record"), so the manifest stops calling it changed.
ship_record() {
    _rn=$1
    case $_rn in # the names before D4c
        devui-fonts) _rn=ui-fonts ;;
        devui-logos) _rn=ui-logos ;;
        u60d-plugins) _rn=core-plugins ;;
        u60-recover.sh) _rn=luyaz-recover.sh ;;
    esac
    _rt=$2
    shift 2 2>/dev/null
    _why=$(printf '%s' "$*" | tr -d '"\\' | tr -d '\000-\037' | cut -c1-120)
    matches "$_rt" '^[0-9]{9,11}$' || ship_refuse 记录 "时间 “$_rt” 不是 Unix 秒"
    P=$(txn_busy)
    [ -n "$P" ] && ship_refuse 记录 "$P"
    _hit=
    _lines=
    while read -r _n _p _k; do
        [ "$_rn" = all ] || [ "$_rn" = "$_n" ] || continue
        _hit=1
        if [ "$_k" = tree ]; then
            if [ -e "$_p" ] || [ -L "$_p" ]; then
                _m=$(tree_fp "$_p")
                [ -n "$_m" ] || ship_refuse 记录 "$_p 里有链接、特殊文件或带换行、| 的名字，算不了指纹"
            else
                _m=-
            fi
            _key=tree
        else
            _m=$(md5_of "$_p")
            _key=md5
        fi
        _lines="$_lines$(printf '{"v":1,"kind":"record","name":"%s","path":"%s","%s":"%s","mac_time":%s,"boot_id":"%s","uptime":%s,"why":"%s"}' \
            "$_n" "$_p" "$_key" "${_m:--}" "$(num "$_rt")" "$(boot_id)" "$(num "$(now)")" "$_why")$NL"
    done <<EOF
$(record_list)
EOF
    [ -n "$_hit" ] || ship_refuse 记录 "不认识 “$_rn”（能记录的：$(record_list | cut -d' ' -f1 | tr '\n' ' ')all）"
    mkdir -p "$SHIP_DIR" 2>/dev/null
    manifest_append "$(printf '%s' "$_lines" | sed '/^$/d')" || ship_refuse 记录 "清单写不上（$MANIFEST）"
    log "记录：$_rn（${_why:-没写原因}）"
    printf '%s' "$_lines" | sed -n 's/.*"name":"\([^"]*\)","path":"\([^"]*\)","\(md5\|tree\)":"\([^"]*\)".*/u60-ship: 已记录 \1 \4/p'
}

# retire <mac time>: one "retire" line, after which doctor --manifest no longer
# checks the removed components' files nor their init records (Phase 10 10c:
# zte-agent and /data/admin went in 8d-2c, u60-uid and u60-guard in 10c, and
# 10c deleted their files). A later ship or kit line of the name counts again.
RETIRED_COMPS="agent web uid guard"
# 10e: the core's init record is init.d/luyaz-core; the old one (u60 record no
# longer knows it) goes too.
RETIRED_RECORDS="init.d/zte-agent init.d/u60-guard init.d/u60-uid init.d/zwrt-datad"
ship_retire() {
    matches "$1" '^[0-9]{9,11}$' || ship_refuse 退役 "时间 “$1” 不是 Unix 秒"
    P=$(txn_busy)
    [ -n "$P" ] && ship_refuse 退役 "$P"
    mkdir -p "$SHIP_DIR" 2>/dev/null
    manifest_append "$(printf '{"v":1,"kind":"retire","comps":"%s","records":"%s","mac_time":%s,"boot_id":"%s","uptime":%s,"why":"Phase 10 10c"}' \
        "$RETIRED_COMPS" "$RETIRED_RECORDS" "$(num "$1")" "$(boot_id)" "$(num "$(now)")")" || ship_refuse 退役 "清单写不上（$MANIFEST）"
    log "退役：$RETIRED_COMPS；记录 $RETIRED_RECORDS"
    say "清单里退役了：$RETIRED_COMPS（和 $RETIRED_RECORDS）"
}

# device_format <comp>: "format" of the comp's last ship or kit line; none → 1.
device_format() {
    _f=$(grep -e '"kind":"ship"' -e '"kind":"kit"' "$MANIFEST" 2>/dev/null | grep -E "$(comp_pat "$1")" | tail -n 1 |
        sed -n 's/.*"format":\([0-9][0-9]*\).*/\1/p')
    num "${_f:-1}"
}

# ── outcomes ────────────────────────────────────────────────────────────────

# finish <terminal phase> <reason>: write it, tidy up, exit.
finish() {
    FINISHED=1
    set_phase "$1" "$2"
    case "$1" in
        done)
            prune_all
            rm -rf "${STAGE_DIR:?}/$X_TXN"
            say "完成：$X_TXN"
            exit 0
            ;;
        manifest_pending)
            say "已转正，但清单没写上：$2（下次 stage 或 manifest-finish 补）"
            exit 2
            ;;
    esac
    # aborted / rolledback / failed: the side files have no use any more
    if [ "$1" != failed ]; then
        _ifs=$IFS
        IFS=$NL
        for _e in $X_FILES; do rm -f "${_e%%|*}.test"; done
        for _e in $X_DIRS; do rm -rf "${_e%%|*}.test"; done
        IFS=$_ifs
        rm -rf "${STAGE_DIR:?}/$X_TXN"
    fi
    say "$1：$2"
    exit 1
}

# abort_trial <reason>: the live slot was never touched.
abort_trial() {
    if ! hook kill_test; then
        finish failed "$1；测试版杀不掉，没有启动正式版（手动 kill -9 后 init.d start）"
    fi
    restore_all states || finish failed "$1；状态文件还原不了"
    hook start || finish failed "$1；正式版没起来：$REASON"
    finish aborted "$1"
}

# do_rollback <reason>: back to the old version (files by md5, idempotent).
do_rollback() {
    set_phase rollback "$1"
    crashpoint rollback:begin
    hook stop || log "停不下来（$REASON），照样换回"
    _rb=0
    restore_all files || _rb=1
    restore_all dirs || _rb=1
    restore_all states || _rb=1
    [ $_rb = 0 ] || finish failed "$1；退回不完整（看 ${LOG##*/}）"
    hook start || finish failed "$1；已换回旧版，但起不来：$REASON"
    finish rolledback "$1"
}

# live_recover <why>: finish the transaction from whatever phase it is in
# (EXIT trap of the executor, recover-live).
live_recover() {
    case "$X_PHASE" in
        staged) finish aborted "$1（还没开始）" ;;
        trial) abort_trial "$1（试跑中）" ;;
        promote | check | rollback) do_rollback "$1（$X_PHASE）" ;;
        manifest)
            manifest_write || finish manifest_pending "$1；$REASON"
            finish done "$1；清单已补完"
            ;;
    esac
    return 0
}

on_exec_exit() {
    [ -n "$FINISHED" ] && return 0
    trap - EXIT INT TERM
    FINISHED=
    live_recover "${EXIT_WHY:-执行器意外退出}"
}

# ── commands ────────────────────────────────────────────────────────────────

ship_refuse() { # ship_refuse <verb> <problems>
    say "拒绝$1："
    printf '%s\n' "$2" | sed '/^$/d; s/^/  - /'
    log "拒绝$1：$(printf '%s' "$2" | tr '\n' ';')"
    exit 1
}

# meta_md5 <meta lines> <name>: what follows "<name> " in file=/dir= lines
meta_md5() {
    printf '%s' "$1" | sed -n "s/^$(printf '%s' "$2" | sed 's/\./\\./g') //p" | head -n 1
}

# is_script <live path>: a shell file (sh -n at stage and in the guard check)
is_script() {
    case "$1" in *.sh | *.init | "$ROOT"/etc/init.d/* | "$ROOT"/etc/rc.local) return 0 ;; esac
    return 1
}

# tgz_problem <tgz>: one line when the archive could write outside its
# directory or holds anything but files and directories; empty = fine.
tgz_problem() {
    _tz=$(tar -tzf "$1" 2>/dev/null) || {
        echo "不是能读的 gzip tar"
        return
    }
    # busybox tar itself strips a leading / or ../ and only says so on stderr
    if printf '%s\n' "$_tz" | grep -qE '^/|(^|/)\.\.(/|$)' ||
        tar -tzf "$1" 2>&1 >/dev/null | grep -qi 'removing leading'; then
        echo "里面有绝对路径或 .."
        return
    fi
    tar -tvzf "$1" 2>/dev/null | grep -qv '^[-d]' && echo "里面有链接或特殊文件"
}

# stage_undo <why>: placing the side files failed half-way: remove them all
stage_undo() {
    for _q in $C_FILES; do rm -f "$_q.test"; done
    for _q in $C_DIRS; do rm -rf "$_q.test"; done
    ship_refuse 暂存 "$1"
}

# stage <txn>: check the upload (docs/SHIP.md for every refusal), place the
# side files, write phase=staged. The live slot is never touched here.
ship_stage() {
    _txn=$1
    matches "$_txn" "$TXN_RE" || ship_refuse 暂存 "事务号 “$_txn” 格式不对"
    [ -f "$SHIP_MARK" ] || ship_refuse 暂存 "ship 目录还没搬到 $SHIP_DIR_NEW（没有 ${SHIP_MARK##*/}）：先 move-d4"
    mkdir -p "$SHIP_DIR" 2>/dev/null
    if command -v flock >/dev/null 2>&1; then
        exec 9>"$LOCK_FILE"
        flock -n 9 || ship_refuse 暂存 "另一个 stage 正在进行"
    fi

    # an unfinished manifest from last time is finished first
    if txn_load && [ "$X_PHASE" = manifest_pending ]; then
        if comp_load "$X_COMP" && manifest_write; then
            set_phase done "清单已补完（下一次 stage 时）"
        else
            ship_refuse 暂存 "上一次事务 $X_TXN 的清单还是写不上：${REASON:-组件看不懂}"
        fi
    fi
    P=$(txn_busy)
    _tp=$(cat "$TRIAL_PIDFILE" 2>/dev/null)
    if [ -n "$_tp" ] && [ -d "/proc/$_tp" ] && tr '\0' ' ' <"/proc/$_tp/cmdline" 2>/dev/null | grep -q 'datad-trial\.sh'; then
        P="$P${NL}datad-trial 的守护在跑（pid $_tp）"
    fi
    [ -n "$P" ] && ship_refuse 暂存 "$P"

    _sd=$STAGE_DIR/$_txn
    [ -f "$_sd/meta" ] || ship_refuse 暂存 "$_sd/meta 不存在"
    txn_clear
    _mv= _mfiles= _mstates= _mdirs=
    P=
    while IFS= read -r _l || [ -n "$_l" ]; do
        case "$_l" in
            '') ;;
            v=*) _mv=${_l#v=} ;;
            txn=*) X_TXN=${_l#txn=} ;;
            comp=*) X_COMP=${_l#comp=} ;;
            commit=*) X_COMMIT=${_l#commit=} ;;
            mac_time=*) X_MACTIME=${_l#mac_time=} ;;
            format=*) X_FORMAT=${_l#format=} ;;
            trial=*) X_TRIAL=${_l#trial=} ;;
            note=*) X_NOTE=${_l#note=} ;;
            file=*) _mfiles="$_mfiles${_l#file=}$NL" ;;
            dir=*) _mdirs="$_mdirs${_l#dir=}$NL" ;;
            state=*) _mstates="$_mstates${_l#state=}$NL" ;;
            *) P="$P${NL}meta 里有看不懂的一行：$(printf '%s' "$_l" | cut -c1-60)" ;;
        esac
    done <"$_sd/meta"
    [ "$_mv" = 1 ] || P="$P${NL}meta 的版本 “$_mv” 看不懂（要 v=1）"
    [ "$X_TXN" = "$_txn" ] || P="$P${NL}meta 的 txn “$X_TXN” 和目录名 $_txn 不一样"
    C_NOTREADY=
    if ! matches "$X_COMP" '^[a-z0-9]{1,12}$' || ! comp_load "$X_COMP"; then
        ship_refuse 暂存 "$P${NL}${C_NOTREADY:-组件 “$X_COMP” 不认识}"
    fi
    [ -n "$C_NOTREADY" ] && ship_refuse 暂存 "$P${NL}$C_NOTREADY"
    case "$_txn" in *-"$X_COMP") ;; *) P="$P${NL}事务号 $_txn 不是以组件名 $X_COMP 结尾" ;; esac
    X_COMP=$C_NAME
    # 10e: the Mac names the core's artifact zwrt-datad; in the new layout the live
    # file is luyaz-core: rename it here, before anything reads the names
    if is_core "$X_COMP" && [ "${CORE_BIN##*/}" = luyaz-core ] && [ -f "$_sd/zwrt-datad" ] && [ ! -e "$_sd/luyaz-core" ]; then
        mv "$_sd/zwrt-datad" "$_sd/luyaz-core" &&
            sed -i 's/^file=zwrt-datad /file=luyaz-core /' "$_sd/meta" &&
            _mfiles=$(printf '%s' "$_mfiles" | sed 's/^zwrt-datad /luyaz-core /')$NL
    fi
    matches "$X_COMMIT" '^[0-9a-f]{7,40}$' || P="$P${NL}提交号 “$X_COMMIT” 不是 7–40 位十六进制"
    matches "$X_MACTIME" '^[0-9]{9,11}$' || P="$P${NL}mac_time “$X_MACTIME” 不是 Unix 秒"
    matches "$X_FORMAT" '^[0-9]{1,4}$' || P="$P${NL}format “$X_FORMAT” 不是数字"
    case "$X_NOTE" in '' | rollback | requires-ignored | rollback+requires-ignored | convert) ;; *) P="$P${NL}note “$X_NOTE” 看不懂" ;; esac
    # the sentinel: the conversion is asked for, and rc.local is derived here
    if [ "$X_COMP" = sentinel ]; then
        _rm=$(rc_mode "$RC_LOCAL")
        if [ "$_rm" = bad ]; then
            P="$P${NL}rc.local 不是认得的样子（只认 u60-uid + u60-guard 两行，或哨兵一行；看 docs/SHIP.md）"
        elif [ "$_rm" = legacy ] && [ "$X_NOTE" != convert ]; then
            P="$P${NL}这次会改 rc.local（u60-uid、u60-guard 两行换成哨兵一行）：要明确的 --convert"
        elif [ "$_rm" = sentinel ] && [ "$X_NOTE" = convert ]; then
            P="$P${NL}rc.local 已经是哨兵的样子了，不用 --convert"
        elif [ "$_rm" = legacy ]; then
            rc_derive "$RC_LOCAL" "$_sd/rc.local"
            if [ "$(rc_mode "$_sd/rc.local")" != sentinel ] || ! sh -n "$_sd/rc.local" 2>/dev/null || ! sh -n "$RC_LOCAL" 2>/dev/null; then
                P="$P${NL}改出来的 rc.local 不对（或现在的 rc.local sh -n 不过）"
            else
                _mfiles="${_mfiles}rc.local $(md5_of "$_sd/rc.local")$NL"
                log "暂存：rc.local 改法（- 旧 / + 新）：$(diff "$RC_LOCAL" "$_sd/rc.local" 2>/dev/null | grep '^[-+][^-+]' | tr '\n' ';' | cut -c1-400)"
            fi
        fi
        [ -f "$SENT_INITD" ] || P="$P${NL}$SENT_INITD 不在：先 u60 ship scripts（带上哨兵的启动脚本）"
    fi
    _clash=$(observe_clash "$X_COMP") && P="$P${NL}$_clash"
    if [ -n "$X_TRIAL" ]; then
        if ! matches "$X_TRIAL" '^[0-9]{1,5}$' || [ "$(num "$X_TRIAL")" -lt 60 ] || [ "$(num "$X_TRIAL")" -gt 86400 ]; then
            P="$P${NL}trial “$X_TRIAL” 不在 60–86400 秒"
        fi
    fi
    X_TRIAL=$(num "${X_TRIAL:-$C_TRIAL}")
    X_CHECK=$C_CHECK

    # files: exactly the component's, md5 as uploaded, live present (or,
    # for a component that may add files, its directory present)
    _need2=
    # R5: the touch UI's new directory, so its first ship there can add the program
    [ "$X_COMP" = touch ] && mkdir -p "$UI_DIR" 2>/dev/null
    # D1: the scripts' new directory, so their first ship there can add them
    [ "$X_COMP" = scripts ] && mkdir -p "$SCRIPTS_DIR" 2>/dev/null
    case $X_COMP in core | datad | sentinel | touch | scripts)
        e3_old_layout && P="$P${NL}这台还是 Phase 10 之前的旧布局（/data/u60d 或 /data/u60-uid 在、新目录没有 .migrated）：Luya-Z 改名 E3 起的版本只认新布局，不上机（先搬家）" ;;
    esac
    for _p in $C_FILES; do
        path_ok "$_p" || P="$P${NL}组件的正式文件 $_p 路径越界"
        case "$_p" in "$ROOT"/etc/*) _need2=1 ;; esac
        _b=${_p##*/}
        _m=$(meta_md5 "$_mfiles" "$_b")
        if ! matches "$_m" '^[0-9a-f]{32}$'; then
            P="$P${NL}meta 里没有 file=$_b <md5>（或 md5 格式不对）"
            continue
        fi
        if [ ! -f "$_sd/$_b" ]; then
            P="$P${NL}暂存目录里没有 $_b"
        elif [ "$(md5_of "$_sd/$_b")" != "$_m" ]; then
            P="$P${NL}$_b 的 md5 不对（上传没完成？）"
        elif is_script "$_p" && ! sh -n "$_sd/$_b" 2>/dev/null; then
            P="$P${NL}$_b 有语法错误（sh -n 不过）"
        elif [ "$_p" = "$SENT_BIN" ] && ! grep -q "$SENT_MARK" "$_sd/$_b" 2>/dev/null; then
            P="$P${NL}$_b 是 Phase 10 R4 之前的哨兵（不认 $SENT_MARK，会去起已删的旧界面）：不上机"
        elif { [ "$_p" = "$CORE_BIN" ] || [ "$_p" = "$UI_BIN" ]; } && ! grep -q "$SCRIPTS_MARK" "$_sd/$_b" 2>/dev/null; then
            P="$P${NL}$_b 是 Luya-Z 改名 D1 之前的（不认 $SCRIPTS_MARK，/data/u60-guard 的旧副本已删）：不上机"
        elif [ "$_p" = "$CORE_BIN" ] && d2_moved && ! grep -q "$OPS_MARK" "$_sd/$_b" 2>/dev/null; then
            P="$P${NL}$_b 是 Luya-Z 改名 D2 之前的（不认 $OPS_MARK，/data/u60-ops 已搬走）：不上机"
        elif [ "$_p" = "$CORE_BIN" ] && d3_named && ! grep -a -q "$CORE_MARK" "$_sd/$_b" 2>/dev/null; then
            P="$P${NL}$_b 是 Luya-Z 改名 D3 之前的核心（只认旧环境变量名，datad.env 已是 LUYAZ_ 名）：不上机"
        elif [ "$_p" = "$SENT_BIN" ] && d2_moved && ! grep -q "$LOCK_MARK" "$_sd/$_b" 2>/dev/null; then
            P="$P${NL}$_b 是 Luya-Z 改名 D2 之前的哨兵（不拿 $LOCK_MARK，会和核心同时写 Wi-Fi）：不上机"
        elif [ "$_p" = "$CORE_BIN" ] && ! grep -a -q "$SHIP_CORE_MARK" "$_sd/$_b" 2>/dev/null; then
            P="$P${NL}$_b 是 Luya-Z 改名 D4 之前的核心（不认 /data/luyaz/ship，ship 目录已搬走）：不上机"
        elif [ "$_p" = "$SENT_BIN" ] && ! grep -a -q "$SHIP_SENT_MARK" "$_sd/$_b" 2>/dev/null; then
            P="$P${NL}$_b 是 Luya-Z 改名 D4 之前的哨兵（不认 $SHIP_SENT_MARK，ship 目录已搬走）：不上机"
        elif { [ "$_p" = "$CORE_BIN" ] || [ "$_p" = "$SENT_BIN" ] || [ "$_p" = "$UI_BIN" ]; } && d5_moved && ! grep -a -q "$UI_CONF_MARK" "$_sd/$_b" 2>/dev/null; then
            P="$P${NL}$_b 是 Luya-Z 改名 D5 之前的（不认 $UI_CONF_MARK，旧 devui.conf 已删）：不上机"
        elif { [ "$_p" = "$CORE_BIN" ] || [ "$_p" = "$SENT_BIN" ]; } && ! grep -a -q "$E2_MARK" "$_sd/$_b" 2>/dev/null; then
            P="$P${NL}$_b 是 Luya-Z 改名 E2 之前的（不拿 luyaz-write.lock、不认 $E2_MARK），会和别的写方同时写：不上机"
        elif [ "$_p" = "$SCRIPTS_DIR/luyaz-fallback.sh" ] && ! grep -q "$FALLBACK_E2_MARK" "$_sd/$_b" 2>/dev/null; then
            P="$P${NL}$_b 是 Luya-Z 改名 E2 之前的（不拿 $FALLBACK_E2_MARK），会和核心同时写：不上机"
        fi
        if [ -L "$_p" ]; then
            P="$P${NL}正式文件 $_p 是软链"
        elif [ -f "$_p" ]; then
            :
        elif [ -n "$C_ALLOW_NEW" ] && [ ! -e "$_p" ] && [ -d "${_p%/*}" ]; then
            _need2=1
        else
            P="$P${NL}正式文件 $_p 不存在（第一次安装走装机包）"
        fi
    done
    _ifs=$IFS
    IFS=$NL
    for _e in $_mfiles; do
        IFS=$_ifs
        _b=${_e%% *}
        _ok=
        for _p in $C_FILES; do [ "${_p##*/}" = "$_b" ] && _ok=1; done
        [ -n "$_ok" ] || P="$P${NL}file=$_b 不是组件 $X_COMP 的文件"
    done
    IFS=$_ifs

    # directories: dir=<name> <fingerprint> <md5 of <name>.tgz>
    for _p in $C_DIRS; do
        _need2=1
        _b=${_p##*/}
        _dl=$(meta_md5 "$_mdirs" "$_b")
        _df=${_dl%% *}
        _dm=${_dl#* }
        if ! matches "$_dl" '^[0-9a-f]{32} [0-9a-f]{32}$'; then
            P="$P${NL}meta 里没有 dir=$_b <指纹> <tgz 的 md5>（或格式不对）"
            continue
        fi
        if [ ! -f "$_sd/$_b.tgz" ]; then
            P="$P${NL}暂存目录里没有 $_b.tgz"
        elif [ "$(md5_of "$_sd/$_b.tgz")" != "$_dm" ]; then
            P="$P${NL}$_b.tgz 的 md5 不对（上传没完成？）"
        else
            _tw=$(tgz_problem "$_sd/$_b.tgz")
            [ -n "$_tw" ] && P="$P${NL}$_b.tgz：$_tw"
        fi
        if [ -L "$_p" ] || [ ! -d "$_p" ]; then
            P="$P${NL}正式目录 $_p 不存在或是软链（第一次安装走装机包）"
        elif [ -z "$(tree_fp "$_p")" ]; then
            P="$P${NL}正式目录 $_p 里有链接、特殊文件或带换行、| 的名字，算不了指纹"
        fi
    done
    _ifs=$IFS
    IFS=$NL
    for _e in $_mdirs; do
        IFS=$_ifs
        _b=${_e%% *}
        _ok=
        for _p in $C_DIRS; do [ "${_p##*/}" = "$_b" ] && _ok=1; done
        [ -n "$_ok" ] || P="$P${NL}dir=$_b 不是组件 $X_COMP 的目录"
    done
    IFS=$_ifs

    # a v=2 log is only safe with a u60-recover.sh that reads it
    if [ -n "$_need2" ] && ! sh "$SHIP_DIR/$REC_NAME" formats 2>/dev/null | grep -qw 2; then
        P="$P${NL}这次要写 v=2 的事务日志，设备上的 u60-recover.sh 读不了：先装新版 u60-recover.sh（u60 install-recover，要用户同意）"
    fi
    # v=3 (rc.local, the sentinel's init) likewise
    _need3=
    for _p in $C_FILES; do
        case "$_p" in "$RC_LOCAL" | "$SENT_INITD") _need3=1 ;; esac
    done
    if [ -n "$_need3" ] && ! sh "$SHIP_DIR/$REC_NAME" formats 2>/dev/null | grep -qw 3; then
        P="$P${NL}这次要写 v=3 的事务日志（rc.local 或哨兵的启动脚本），设备上的 u60-recover.sh 读不了：先装新版（u60 install-recover，要用户同意）"
    fi
    # v=4 (luyaz-web's init) likewise
    if in_list "$LW_INITF" "$C_FILES" && ! sh "$SHIP_DIR/$REC_NAME" formats 2>/dev/null | grep -qw 4; then
        P="$P${NL}这次要写 v=4 的事务日志（luyaz-web 的启动脚本），设备上的 u60-recover.sh 读不了：先装新版（u60 install-recover，要用户同意）"
    fi
    # v=5 (luyaz-core's init, 10e) likewise
    if in_list "$ROOT/etc/init.d/luyaz-core" "$C_FILES" && ! sh "$SHIP_DIR/$REC_NAME" formats 2>/dev/null | grep -qw 5; then
        P="$P${NL}这次要写 v=5 的事务日志（luyaz-core 的启动脚本），设备上的 u60-recover.sh 读不了：先装新版（u60 install-recover，要用户同意）"
    fi

    # state files: listed ones must be in the component's exact list
    [ -n "$_mstates" ] || _mstates=$(printf '%s\n' $C_STATE_ALLOW)$NL
    _ifs=$IFS
    IFS=$NL
    for _s in $_mstates; do
        IFS=$_ifs
        [ -n "$_s" ] || continue
        if ! path_ok "$_s" || ! in_list "$_s" "$C_STATE_ALLOW"; then
            P="$P${NL}状态文件 $(printf '%s' "$_s" | cut -c1-80) 不在组件 $X_COMP 的清单里（或路径越界）"
        elif [ -L "$_s" ] || { [ -e "$_s" ] && [ ! -f "$_s" ]; }; then
            P="$P${NL}状态文件 $_s 不是普通文件"
        else
            X_STATELIST="$X_STATELIST$_s$NL"
        fi
    done
    IFS=$_ifs

    # a test build still running would fight for the side file / 9460 / the screen
    if [ "$C_KIND" = trial ] && [ -n "$($PIDOF "${TEST_BIN##*/}" 2>/dev/null)" ]; then
        P="$P${NL}测试版 ${TEST_BIN##*/} 还在跑"
    fi

    _free=$(num "$($DF -Pk "$ROOT/data" 2>/dev/null | awk 'NR == 2 { print $4 }')")
    [ "$_free" -lt "$(num "$MIN_FREE_KB")" ] && P="$P${NL}/data 可用只有 $((_free / 1024)) MB（< $((MIN_FREE_KB / 1024)) MB）"

    if matches "$X_FORMAT" '^[0-9]{1,4}$'; then
        _dfmt=$(device_format "$X_COMP")
        if [ "$(num "$X_FORMAT")" -gt "$_dfmt" ]; then
            P="$P${NL}数据格式版本升高（设备 $_dfmt → 新版 $(num "$X_FORMAT")）：要单独写迁移和退回方案，不能自动上机"
        elif [ "$(num "$X_FORMAT")" -lt "$_dfmt" ]; then
            P="$P${NL}数据格式版本降低（设备 $_dfmt → 新版 $(num "$X_FORMAT")）：旧程序读新格式不可预期，不能自动上机"
        fi
    fi
    P=$(printf '%s' "$P" | sed '/^$/d')
    [ -n "$P" ] && ship_refuse 暂存 "$P"

    # place the side files next to the live ones
    for _p in $C_FILES; do
        _b=${_p##*/}
        _m=$(meta_md5 "$_mfiles" "$_b")
        rm -f "$_p.test"
        cp "$_sd/$_b" "$_p.test" 2>/dev/null || stage_undo "拷不了 $_p.test（/data 满了？）"
        # every component file is a program or a script: 755, whatever mode
        # the upload had (the /etc/init.d one included)
        chmod 755 "$_p.test"
        if [ -f "$_p" ]; then _o=$(md5_of "$_p"); else _o=-; fi
        sync_hb
        [ "$(md5_of "$_p.test")" = "$_m" ] || stage_undo "$_p.test 拷过去以后 md5 不对"
        X_FILES="$X_FILES$_p|$_o|$_m$NL"
    done
    # directories: unpack next to the live one, check the fingerprint
    for _p in $C_DIRS; do
        _b=${_p##*/}
        _dl=$(meta_md5 "$_mdirs" "$_b")
        rm -rf "$_p.test"
        mkdir -p "$_p.test" && tar -xzof "$_sd/$_b.tgz" -C "$_p.test" 2>/dev/null ||
            stage_undo "$_b.tgz 解不到 $_p.test（/data 满了？）"
        sync_hb
        [ "$(tree_fp "$_p.test")" = "${_dl%% *}" ] || stage_undo "$_p.test 解开以后指纹不对（$(tree_fp "$_p.test" | cut -c1-8)，应为 $(printf '%s' "$_dl" | cut -c1-8)）"
        X_DIRS="$X_DIRS$_p|$(tree_fp "$_p")|${_dl%% *}$NL"
    done
    for _p in $C_FILES; do rm -f "$_sd/${_p##*/}"; done
    for _p in $C_DIRS; do rm -f "$_sd/${_p##*/}.tgz"; done
    # other transactions' leftovers
    for _o in "$STAGE_DIR"/*; do
        [ -d "$_o" ] && [ "${_o##*/}" != "$_txn" ] && rm -rf "$_o"
    done

    X_BOOT=$(boot_id)
    X_TSTAGE=$(now)
    X_EXEC=
    set_phase staged || ship_refuse 暂存 "事务日志写不上"
    say "已暂存 $_txn（$X_COMP $X_COMMIT）"
}

# run <txn>: the executor (detached by `start`; tests call it directly).
ship_run() {
    _txn=$1
    matches "$_txn" "$TXN_RE" || { say "事务号格式不对"; exit 1; }
    if ! txn_load || [ "$X_TXN" != "$_txn" ] || [ "$X_PHASE" != staged ]; then
        say "事务 $_txn 不在「已暂存」（现在：${X_TXN:-无} ${X_PHASE:-}）"
        exit 1
    fi
    FINISHED=
    EXIT_WHY=
    # EXIT: a script error still finishes the transaction by its phase.
    # SIGKILL (OOM) cannot be caught: the guard (recover-live) and the next
    # boot (u60-recover.sh) cover that.
    trap on_exec_exit EXIT
    trap 'EXIT_WHY="执行器被停止（abort 或信号）"; exit 1' INT TERM
    if ! comp_load "$X_COMP" || [ -n "$C_NOTREADY" ]; then
        finish aborted "组件 $X_COMP 不能用 ${C_NOTREADY:-}"
    fi
    X_EXEC=$$
    [ "$(boot_id)" = "$X_BOOT" ] || finish aborted "暂存以后设备重启过（开机号变了），没有开始"

    # claim the transaction: `start` may be giving up on us at this moment
    txn_lock
    if ! txn_load || [ "$X_TXN" != "$_txn" ] || [ "$X_PHASE" != staged ]; then
        FINISHED=1
        say "事务 $_txn 已经不在「已暂存」（${X_PHASE:-无}），不做"
        exit 1
    fi
    X_EXEC=$$
    comp_load "$X_COMP"
    if [ "$C_KIND" = trial ]; then _first=trial; else _first=promote; fi
    set_phase "$_first" || {
        txn_unlock
        finish aborted "事务日志写不上"
    }
    txn_unlock

    if [ "$C_KIND" = trial ]; then
        HB_STEP=1
        crashpoint trial:begin
        hook stop || {
            hook start
            finish aborted "试跑没开始：$REASON"
        }
        if ! state_snapshot; then
            hook start
            finish aborted "试跑没开始：$REASON"
        fi
        txn_save
        crashpoint trial:state
        hook launch_test || abort_trial "测试版没起来（$_why）"
        crashpoint trial:running
        hook trial_watch || abort_trial "试跑不过：$REASON"
        hook kill_test || finish failed "试跑通过，但测试版杀不掉，没有启动正式版（手动 kill -9 后 init.d start）"
        log "试跑通过（${X_TRIAL}s）"
    fi

    HB_STEP=5
    if [ "$X_PHASE" != promote ]; then
        set_phase promote || do_rollback "事务日志写不上"
    fi
    crashpoint promote:begin
    if [ "$C_KIND" = direct ]; then
        # pre: nothing touched yet, a failure is an abort (guard: old doctor)
        [ -n "$C_PRE" ] && { hook pre || finish aborted "转正前：$REASON"; }
        hook stop || do_rollback "转正前停不下来：$REASON"
        state_snapshot || do_rollback "转正前：$REASON"
        txn_save
        crashpoint promote:state
    fi
    promote_files || do_rollback "转正失败：$REASON"
    promote_dirs || do_rollback "转正失败：$REASON"
    hook start || do_rollback "新版起不来：$REASON"
    set_phase check || do_rollback "事务日志写不上"
    crashpoint check:begin
    hook check || do_rollback "转正后检查不过：$REASON"
    set_phase manifest || do_rollback "事务日志写不上"
    crashpoint manifest:begin
    manifest_write || finish manifest_pending "$REASON"
    crashpoint manifest:written
    finish done "已转正、检查通过、清单已写"
}

ship_start() {
    _txn=$1
    matches "$_txn" "$TXN_RE" || { say "事务号格式不对"; exit 1; }
    if ! txn_load || [ "$X_TXN" != "$_txn" ] || [ "$X_PHASE" != staged ]; then
        say "事务 $_txn 不在「已暂存」"
        exit 1
    fi
    rm -f "$HB_FILE"
    detach /dev/null nohup sh "$SELF" run "$_txn"
    # real seconds (not the executor's clock): 5 s for the first heartbeat
    _i=0
    while [ $_i -lt 5 ]; do
        sleep 1
        _i=$((_i + 1))
        if hb_read && exec_alive "$HB_PID"; then
            say "执行器已起来（pid $HB_PID），日志：$LOG"
            return 0
        fi
        txn_load && [ "$X_PHASE" != staged ] && break
    done
    txn_lock
    if txn_load && [ "$X_PHASE" = staged ]; then
        FINISHED=1
        set_phase aborted "执行器 5s 内没起来"
        txn_unlock
        say "失败：执行器没起来，事务已中止（正式位置没动）"
        return 1
    fi
    txn_unlock
    say "执行器已接手：$X_PHASE ${X_REASON}"
    return 0
}

# txn_lock / txn_unlock: the stage flock, around the staged → running claim
# (fd 8; stage itself uses fd 9 on the same file).
txn_lock() {
    command -v flock >/dev/null 2>&1 || return 0
    mkdir -p "$SHIP_DIR" 2>/dev/null
    exec 8>"$LOCK_FILE"
    flock -x 8
}
txn_unlock() {
    command -v flock >/dev/null 2>&1 || return 0
    flock -u 8
    exec 8>&-
}

ship_status() {
    if ! txn_load; then
        [ -f "$TXN_FILE" ] && echo "phase=unreadable" || echo "phase=none"
        return 0
    fi
    echo "txn=$X_TXN"
    echo "comp=$X_COMP"
    echo "phase=$X_PHASE"
    echo "reason=$X_REASON"
    echo "commit=$X_COMMIT"
    hb_read
    echo "hb_age=$HB_AGE"
    if exec_alive "$HB_PID"; then echo "exec_alive=1"; else echo "exec_alive=0"; fi
    if [ "$X_PHASE" = done ] && [ "$(boot_id)" = "$X_BOOT" ]; then
        _left=$((OBSERVE - ($(num "$(now)") - $(num "$X_TPHASE"))))
        [ "$_left" -gt 0 ] && echo "observe_left=$_left"
    fi
    return 0
}

ship_wait() {
    _max=$(num "$1")
    _t0=$(num "$(now)")
    while :; do
        if txn_load && ! in_list "$X_PHASE" "$ACTIVE"; then
            ship_status
            return 0
        fi
        if [ $(($(num "$(now)") - _t0)) -ge "$_max" ]; then
            ship_status
            return 4
        fi
        $REAL_SLEEP 2
    done
}

ship_abort() {
    hb_read
    if exec_alive "$HB_PID"; then
        kill "$HB_PID"
        say "已通知执行器（pid $HB_PID）中止，它按阶段自己收尾；看 $LOG"
        return 0
    fi
    say "没有执行器在跑（事务停在半路时用 recover-live）"
    return 1
}

# recover-live [--force]: the executor died (OOM kill -9) or hangs; finish
# the transaction in this boot. Refuses while the executor looks alive.
ship_recover_live() {
    txn_load || {
        say "没有事务"
        return 0
    }
    in_list "$X_PHASE" "$ACTIVE" || return 0
    hb_read
    if exec_alive "$HB_PID"; then
        if [ "$1" != --force ] && [ -n "$HB_AGE" ] && [ "$HB_AGE" -le "$HB_STALE" ]; then
            say "执行器还活着（pid $HB_PID，心跳 ${HB_AGE}s 前），不接手"
            return 3
        fi
        log "recover-live：杀掉卡住的执行器（pid $HB_PID，心跳 ${HB_AGE:-?}s 前）"
        kill -9 "$HB_PID" 2>/dev/null
        _i=0
        while exec_alive "$HB_PID" && [ $_i -lt 5 ]; do
            $REAL_SLEEP 1
            _i=$((_i + 1))
        done
    fi
    if ! comp_load "$X_COMP"; then
        FINISHED=1
        set_phase failed "recover-live：组件 $X_COMP 看不懂"
        return 1
    fi
    FINISHED=
    trap on_exec_exit EXIT
    trap 'EXIT_WHY="recover-live 被停止"; exit 1' INT TERM
    HB_STEP=5
    live_recover "执行器没了（心跳 ${HB_AGE:-?}s 前），recover-live 接手"
}

ship_manifest_finish() {
    txn_load || {
        say "没有事务"
        return 1
    }
    case "$X_PHASE" in
        done) return 0 ;;
        manifest_pending) ;;
        manifest)
            hb_read
            if exec_alive "$HB_PID"; then
                say "执行器还在写清单"
                return 1
            fi
            ;;
        *)
            say "事务不在清单待补（$X_PHASE）"
            return 1
            ;;
    esac
    comp_load "$X_COMP"
    FINISHED=1
    manifest_write || {
        say "清单还是写不上：$REASON"
        return 1
    }
    set_phase done "清单已补完"
    prune_all
    say "清单已补完：$X_TXN"
}

# record-kit <comp> <kit stamp> <commit> <format> <mac time> [dirty]: the
# install kit (onboard/device/install.sh) put this component on the device
# itself: one "kit" line with the live files' md5s (docs/SHIP.md). "dirty" =
# the kit was built from a work tree with uncommitted changes.
ship_record_kit() {
    { matches "$1" '^[a-z0-9]{1,12}$' && comp_load "$1"; } || ship_refuse 记录装机包 "${C_NOTREADY:-组件 “$1” 不认识}"
    matches "$2" '^[0-9]{8}(-[0-9]{4,6})?$' || ship_refuse 记录装机包 "装机包日期 “$2” 格式不对"
    matches "$3" '^[0-9a-f]{7,40}$' || ship_refuse 记录装机包 "提交号 “$3” 不是 7–40 位十六进制"
    matches "$4" '^[0-9]{1,4}$' || ship_refuse 记录装机包 "format “$4” 不是数字"
    matches "$5" '^[0-9]{9,11}$' || ship_refuse 记录装机包 "时间 “$5” 不是 Unix 秒"
    case "${6:-}" in '' | dirty) ;; *) ship_refuse 记录装机包 "第 6 个参数只能是 dirty" ;; esac
    P=$(txn_busy)
    [ -n "$P" ] && ship_refuse 记录装机包 "$P"
    [ -n "$C_FILES$C_DIRS" ] || ship_refuse 记录装机包 "$1 没有文件清单"
    _files=
    for _p in $C_FILES; do
        if [ -f "$_p" ]; then
            _m=$(md5_of "$_p")
        elif [ -n "$C_ALLOW_NEW" ] && [ ! -e "$_p" ]; then
            _m=-    # the kit does not install every file of the component
        else
            ship_refuse 记录装机包 "$_p 不在"
        fi
        _files="$_files${_files:+,}{\"path\":\"$_p\",\"md5\":\"$_m\"}"
    done
    for _p in $C_DIRS; do
        _m=$(tree_fp "$_p")
        [ -n "$_m" ] || ship_refuse 记录装机包 "$_p 不在或算不了指纹"
        _files="$_files${_files:+,}{\"path\":\"$_p\",\"tree\":\"$_m\"}"
    done
    _nt=
    [ "${6:-}" = dirty ] && _nt=',"note":"kit-dirty"'
    mkdir -p "$SHIP_DIR" 2>/dev/null
    manifest_append "$(printf '{"v":1,"kind":"kit","comp":"%s","kit":"%s","commit":"%s","format":%s,"mac_time":%s,"boot_id":"%s","uptime":%s,"files":[%s],"state":[]%s}' \
        "$1" "$2" "$3" "$(num "$4")" "$(num "$5")" "$(boot_id)" "$(num "$(now)")" "$_files" "$_nt")" ||
        ship_refuse 记录装机包 "清单写不上（$MANIFEST）"
    log "记录：装机包 $2 装了 $1（$3${6:+，有未提交改动}）"
    say "已记录装机包装的 $1（$3）"
}

# prepare-rollback <comp> <txn> <mac time>: fill stage/<txn>/ with the
# version before the component's last ship (its .prev-<that txn>), so `u60
# rollback` goes through stage / trial / promote / check like any ship. The
# commit and format are those of the ship before it (0000000 and the current
# format when that version came from somewhere else).
ship_prepare_rollback() {
    _c=$1
    _txn=$2
    matches "$_txn" "$TXN_RE" || ship_refuse 准备退回 "事务号 “$_txn” 格式不对"
    [ -f "$SHIP_MARK" ] || ship_refuse 准备退回 "ship 目录还没搬到 $SHIP_DIR_NEW（没有 ${SHIP_MARK##*/}）：先 move-d4"
    case "$_txn" in *-"$_c") ;; *) ship_refuse 准备退回 "事务号不是以组件名 $_c 结尾" ;; esac
    matches "$3" '^[0-9]{9,11}$' || ship_refuse 准备退回 "时间 “$3” 不是 Unix 秒"
    { matches "$_c" '^[a-z0-9]{1,12}$' && comp_load "$_c"; } || ship_refuse 准备退回 "${C_NOTREADY:-组件 “$_c” 不认识}"
    [ -n "$C_NOTREADY" ] && ship_refuse 准备退回 "$C_NOTREADY"
    P=$(txn_busy)
    [ -n "$P" ] && ship_refuse 准备退回 "$P"
    _lk=$(grep -e '"kind":"ship"' -e '"kind":"kit"' "$MANIFEST" 2>/dev/null | grep -E "$(comp_pat "$_c")" | tail -n 1)
    case "$_lk" in *'"kind":"kit"'*) ship_refuse 准备退回 "$_c 最后一次是装机包装的，没有 ship 留下的上一版：用装机包或 u60 ship 指定提交" ;; esac
    _ls=$(grep '"kind":"ship"' "$MANIFEST" 2>/dev/null | grep -E "$(comp_pat "$_c")")
    _l1=$(printf '%s\n' "$_ls" | tail -n 1)
    _l0=
    [ "$(printf '%s\n' "$_ls" | grep -c .)" -ge 2 ] && _l0=$(printf '%s\n' "$_ls" | tail -n 2 | head -n 1)
    [ -n "$_l1" ] || ship_refuse 准备退回 "清单里没有 $_c 上机的记录，不知道上一版在哪"
    # 10e rollback floor (D4/D7): after the move only a core shipped into the new
    # layout can come back; one from before it would go to the old path
    if is_core "$_c"; then
        case "$_l1" in *"\"path\":\"$ROOT/data/luyaz/"*) ;; *)
            ship_refuse 准备退回 "核心已经搬到新布局（/data/luyaz/core/.migrated），最后一次 datad 上机是搬家之前的，退回会放回旧路径：不能退回旧布局（见 docs/designs/u60-phase10.md）" ;;
        esac
    fi
    # R5 rollback floor (review D4, Finding 4): a touch ship from before the rename put the
    # program at the old path, and the one that first put luyaz-ui had nothing before it there;
    # undoing either would bring back a pre-rename UI the sentinel no longer prefers (and after
    # round ③ no longer knows)
    if [ "$_c" = touch ]; then
        case "$_l1" in *"\"path\":\"$UI_BIN\""*) ;; *)
            ship_refuse 准备退回 "最后一次 touch 上机是改名（luyaz-ui，Phase 10 R5）之前的：退回会放回旧名字的触屏，不做；要换版本用 u60 ship touch --commit" ;;
        esac
        # (luyaz-ui's own entry only: a glob over the line would see start.sh's prev)
        _ue=$(printf '%s' "$_l1" | grep -o "\"path\":\"$UI_BIN\",\"md5\":\"[0-9a-f-]*\"[^}]*}")
        case "$_ue" in *'"prev":"'*) ;; *)
            ship_refuse 准备退回 "最后一次 touch 上机是第一次放 luyaz-ui（改名那次），它之前没有新名字的触屏：退回会回到改名前，不做；要换版本用 u60 ship touch --commit" ;;
        esac
    fi
    # R4 floor: the sentinel before the last ship must know the new FIFO and luyaz-ui
    if [ "$_c" = sentinel ]; then
        _sp=$(printf '%s' "$_l1" | grep -o "\"path\":\"$SENT_BIN\",\"md5\":\"[0-9a-f-]*\"[^}]*}" | sed -n 's/.*"prev":"\([^"]*\)".*/\1/p')
        if [ -n "$_sp" ] && [ -f "$_sp" ] && ! grep -q "$SENT_MARK" "$_sp" 2>/dev/null; then
            ship_refuse 准备退回 "上一版哨兵是 Phase 10 R4 之前的（不认 $SENT_MARK），退回后它会去起已删的旧界面 u60pro-devui：不做"
        fi
        if [ -n "$_sp" ] && [ -f "$_sp" ] && d2_moved && ! grep -q "$LOCK_MARK" "$_sp" 2>/dev/null; then
            ship_refuse 准备退回 "上一版哨兵是 Luya-Z 改名 D2 之前的（不拿 $LOCK_MARK），退回后它和核心会同时写 Wi-Fi：不做"
        fi
        if [ -n "$_sp" ] && [ -f "$_sp" ] && ! grep -a -q "$SHIP_SENT_MARK" "$_sp" 2>/dev/null; then
            ship_refuse 准备退回 "上一版哨兵是 Luya-Z 改名 D4 之前的（不认 $SHIP_SENT_MARK），ship 目录已搬走，退回后它看不到事务、收不了尾：不做；要换版本用 u60 ship sentinel --commit"
        fi
    fi
    # D1 floor: the old copies are gone, so the core's and the UI's previous version must know
    # SCRIPTS_DIR (the scripts' own undo only keeps the files it added: no floor needed)
    if is_core "$_c" || [ "$_c" = touch ]; then
        if is_core "$_c"; then _db=$CORE_BIN; else _db=$UI_BIN; fi
        _dp=$(printf '%s' "$_l1" | grep -o "\"path\":\"$_db\",\"md5\":\"[0-9a-f-]*\"[^}]*}" | sed -n 's/.*"prev":"\([^"]*\)".*/\1/p')
        if [ -n "$_dp" ] && [ -f "$_dp" ] && ! grep -q "$SCRIPTS_MARK" "$_dp" 2>/dev/null; then
            ship_refuse 准备退回 "上一版 ${_db##*/} 是 Luya-Z 改名 D1 之前的（不认 $SCRIPTS_MARK），/data/u60-guard 的旧副本已删，退回后它找不到脚本和 lan-ipv6-off（局域网 IPv6 会打开）：不做；要换版本用 u60 ship $_c --commit"
        fi
    fi
    if is_core "$_c" && d2_moved; then
        _dp=$(printf '%s' "$_l1" | grep -o "\"path\":\"$CORE_BIN\",\"md5\":\"[0-9a-f-]*\"[^}]*}" | sed -n 's/.*"prev":"\([^"]*\)".*/\1/p')
        if [ -n "$_dp" ] && [ -f "$_dp" ] && ! grep -q "$OPS_MARK" "$_dp" 2>/dev/null; then
            ship_refuse 准备退回 "上一版核心是 Luya-Z 改名 D2 之前的（不认 $OPS_MARK），/data/u60-ops 已搬走，退回后它的操作流水账和 APN 保管库是空的：不做；要换版本用 u60 ship datad --commit"
        fi
        if [ -n "$_dp" ] && [ -f "$_dp" ] && d3_named && ! grep -a -q "$CORE_MARK" "$_dp" 2>/dev/null; then
            ship_refuse 准备退回 "上一版核心是 Luya-Z 改名 D3 之前的（只认旧环境变量名），退回后它没有自动退回、socket ubus 和 datad.env 的开关：不做；要换版本用 u60 ship datad --commit"
        fi
        if [ -n "$_dp" ] && [ -f "$_dp" ] && ! grep -a -q "$SHIP_CORE_MARK" "$_dp" 2>/dev/null; then
            ship_refuse 准备退回 "上一版核心是 Luya-Z 改名 D4 之前的（不认 /data/luyaz/ship），ship 目录已搬走，退回后它看不到事务和清单：不做；要换版本用 u60 ship datad --commit"
        fi
    fi
    # D5 floor: the previous core, sentinel or UI must read devui.conf at its new place
    if d5_moved && { is_core "$_c" || [ "$_c" = sentinel ] || [ "$_c" = touch ]; }; then
        case $_c in sentinel) _db=$SENT_BIN ;; touch) _db=$UI_BIN ;; *) _db=$CORE_BIN ;; esac
        _dp=$(printf '%s' "$_l1" | grep -o "\"path\":\"$_db\",\"md5\":\"[0-9a-f-]*\"[^}]*}" | sed -n 's/.*"prev":"\([^"]*\)".*/\1/p')
        if [ -n "$_dp" ] && [ -f "$_dp" ] && ! grep -a -q "$UI_CONF_MARK" "$_dp" 2>/dev/null; then
            ship_refuse 准备退回 "上一版 ${_db##*/} 是 Luya-Z 改名 D5 之前的（不认 $UI_CONF_MARK），旧 devui.conf 已删，退回后告警短信语言和屏幕设置会丢：不做；要换版本用 u60 ship $_c --commit"
        fi
    fi
    # E2 floor: the previous core or sentinel must take luyaz-write.lock (and read the new log)
    if is_core "$_c" || [ "$_c" = sentinel ]; then
        if [ "$_c" = sentinel ]; then _db=$SENT_BIN; else _db=$CORE_BIN; fi
        _dp=$(printf '%s' "$_l1" | grep -o "\"path\":\"$_db\",\"md5\":\"[0-9a-f-]*\"[^}]*}" | sed -n 's/.*"prev":"\([^"]*\)".*/\1/p')
        if [ -n "$_dp" ] && [ -f "$_dp" ] && ! grep -a -q "$E2_MARK" "$_dp" 2>/dev/null; then
            ship_refuse 准备退回 "上一版 ${_db##*/} 是 Luya-Z 改名 E2 之前的（不拿 luyaz-write.lock、不认 $E2_MARK），退回后它会和别的写方同时写：不做；要换版本用 u60 ship $_c --commit"
        fi
    fi
    # …and the scripts' previous luyaz-fallback.sh
    if [ "$_c" = scripts ]; then
        _dp=$(printf '%s' "$_l1" | grep -o "\"path\":\"$SCRIPTS_DIR/luyaz-fallback.sh\",\"md5\":\"[0-9a-f-]*\"[^}]*}" | sed -n 's/.*"prev":"\([^"]*\)".*/\1/p')
        if [ -n "$_dp" ] && [ -f "$_dp" ] && ! grep -q "$FALLBACK_E2_MARK" "$_dp" 2>/dev/null; then
            ship_refuse 准备退回 "上一版 luyaz-fallback.sh 是 Luya-Z 改名 E2 之前的（不拿 $FALLBACK_E2_MARK），退回后它会和核心同时写：不做；要换版本用 u60 ship scripts --commit"
        fi
    fi
    case "$_l1" in *"\"path\":\"$RC_LOCAL\""*)
        legacy_here || ship_refuse 准备退回 "最后一次 $_c 上机是哨兵转正（换了 rc.local），退回会回到 u60-uid + u60-guard，它们已经删了（Phase 10 10c）" ;;
    esac
    _cm=$(printf '%s' "$_l0" | sed -n 's/.*"commit":"\([0-9a-f]*\)".*/\1/p')
    _fm=$(printf '%s' "$_l0" | sed -n 's/.*"format":\([0-9]*\).*/\1/p')
    [ -n "$_cm" ] || _cm=0000000
    [ -n "$_fm" ] || _fm=$(device_format "$_c")
    _sd=$STAGE_DIR/$_txn
    rm -rf "$_sd"
    mkdir -p "$_sd" || ship_refuse 准备退回 "建不了 $_sd"
    _meta="v=1${NL}txn=$_txn${NL}comp=$_c${NL}commit=$_cm${NL}mac_time=$3${NL}format=$_fm${NL}note=rollback"
    for _p in $C_FILES; do
        _pe=$(printf '%s' "$_l1" | grep -o "\"path\":\"$_p\",\"md5\":\"[0-9a-f-]*\"[^}]*}")
        _pv=$(printf '%s' "$_pe" | sed -n 's/.*"prev":"\([^"]*\)".*/\1/p')
        if [ -n "$C_ALLOW_NEW" ] && [ -z "$_pv" ] && [ -f "$_p" ]; then
            # new in that ship (or not in it at all): the version before it
            # had no such file; ship cannot delete one, so it stays as it is
            log "准备退回：$_p 在那次上机以前没有，保持现在的版本"
            _pv=$_p
        fi
        if [ -z "$_pv" ] || ! path_ok "$_pv" || [ ! -f "$_pv" ]; then
            rm -rf "$_sd"
            ship_refuse 准备退回 "$_p 的上一版 ${_pv:-（清单里没写）} 不在了"
        fi
        cp "$_pv" "$_sd/${_p##*/}" || {
            rm -rf "$_sd"
            ship_refuse 准备退回 "拷不了 $_pv"
        }
        _meta="$_meta${NL}file=${_p##*/} $(md5_of "$_sd/${_p##*/}")"
    done
    for _p in $C_DIRS; do
        _pv=$(printf '%s' "$_l1" | grep -o "\"path\":\"$_p\",\"tree\":\"[0-9a-f]*\",\"prev\":\"[^\"]*\"" | sed 's/.*"prev":"//; s/"$//')
        _pf=
        [ -n "$_pv" ] && path_ok "$_pv" && _pf=$(tree_fp "$_pv")
        if [ -z "$_pf" ]; then
            rm -rf "$_sd"
            ship_refuse 准备退回 "$_p 的上一版 ${_pv:-（清单里没写）} 不在了或算不了指纹"
        fi
        tar -czf "$_sd/${_p##*/}.tgz" -C "$_pv" . 2>/dev/null || {
            rm -rf "$_sd"
            ship_refuse 准备退回 "打不了 $_pv 的包"
        }
        _meta="$_meta${NL}dir=${_p##*/} $_pf $(md5_of "$_sd/${_p##*/}.tgz")"
    done
    printf '%s\n' "$_meta" >"$_sd/meta"
    sync_hb
    log "准备退回：$_c 退到 $_cm（上一版文件来自最后一次上机的 .prev）"
    say "已准备退回 $_txn：$_c → $_cm"
}

# install-recover <txn>: replace /data/luyaz/ship/luyaz-recover.sh (the boot-time
# clean-up, not touched by ship) with the one in stage/<txn>/ (docs/SHIP.md):
# md5 as uploaded, sh -n, the new one's own selftest and formats, then temp
# name → sync → mv → sync, the old one kept as .prev-<txn>, one record line.
# Only between transactions. Asking the user first is the Mac side's job.
ship_install_recover() {
    _txn=$1
    matches "$_txn" "$TXN_RE" || ship_refuse 换恢复脚本 "事务号 “$_txn” 格式不对"
    case "$_txn" in *-recover) ;; *) ship_refuse 换恢复脚本 "事务号不是以 -recover 结尾" ;; esac
    P=$(txn_busy)
    [ -n "$P" ] && ship_refuse 换恢复脚本 "$P"
    _sd=$STAGE_DIR/$_txn
    [ -f "$_sd/meta" ] || ship_refuse 换恢复脚本 "$_sd/meta 不存在"
    _mv= _mt= _mc= _mcm= _mm= _mf=
    P=
    while IFS= read -r _l || [ -n "$_l" ]; do
        case "$_l" in
            '') ;;
            v=*) _mv=${_l#v=} ;;
            txn=*) _mt=${_l#txn=} ;;
            comp=*) _mc=${_l#comp=} ;;
            commit=*) _mcm=${_l#commit=} ;;
            mac_time=*) _mm=${_l#mac_time=} ;;
            file=*) _mf=${_l#file=} ;;
            *) P="$P${NL}meta 里有看不懂的一行：$(printf '%s' "$_l" | cut -c1-60)" ;;
        esac
    done <"$_sd/meta"
    [ "$_mv" = 1 ] || P="$P${NL}meta 的版本 “$_mv” 看不懂（要 v=1）"
    [ "$_mt" = "$_txn" ] || P="$P${NL}meta 的 txn 和目录名不一样"
    [ "$_mc" = recover ] || P="$P${NL}meta 的 comp 应为 recover"
    matches "$_mcm" '^[0-9a-f]{7,40}$' || P="$P${NL}提交号 “$_mcm” 不是 7–40 位十六进制"
    matches "$_mm" '^[0-9]{9,11}$' || P="$P${NL}mac_time “$_mm” 不是 Unix 秒"
    _new=$_sd/u60-recover.sh
    if ! matches "$_mf" '^u60-recover\.sh [0-9a-f]{32}$'; then
        P="$P${NL}meta 里没有 file=u60-recover.sh <md5>"
    elif [ ! -f "$_new" ]; then
        P="$P${NL}暂存目录里没有 u60-recover.sh"
    elif [ "$(md5_of "$_new")" != "${_mf#* }" ]; then
        P="$P${NL}u60-recover.sh 的 md5 不对（上传没完成？）"
    elif ! sh -n "$_new" 2>/dev/null; then
        P="$P${NL}新的 u60-recover.sh 有语法错误（sh -n 不过）"
    else
        _so=$(sh "$_new" selftest 2>&1)
        if [ $? != 0 ] || ! printf '%s\n' "$_so" | grep -q 'selftest: PASS'; then
            P="$P${NL}新的 u60-recover.sh 自检没过：$(printf '%s\n' "$_so" | grep -e FAIL -e 'selftest:' | head -n 3 | tr '\n' ' ' | cut -c1-200)"
        elif ! sh "$_new" formats 2>/dev/null | grep -qw 1; then
            P="$P${NL}新的 u60-recover.sh 读不了 v=1 的事务日志（formats）"
        fi
    fi
    P=$(printf '%s' "$P" | sed '/^$/d')
    [ -n "$P" ] && ship_refuse 换恢复脚本 "$P"

    _live=$SHIP_DIR/$REC_NAME
    if [ -f "$_live" ]; then
        cp -p "$_live" "$_live.prev-$_txn" 2>/dev/null || ship_refuse 换恢复脚本 "旧的拷不成 .prev（/data 满了？）"
    fi
    cp "$_new" "$_live.tmp.$$" 2>/dev/null && chmod 755 "$_live.tmp.$$" || {
        rm -f "$_live.tmp.$$"
        ship_refuse 换恢复脚本 "拷不过去（/data 满了？）"
    }
    sync_hb
    [ "$(md5_of "$_live.tmp.$$")" = "${_mf#* }" ] || {
        rm -f "$_live.tmp.$$"
        ship_refuse 换恢复脚本 "拷过去以后 md5 不对"
    }
    mv -f "$_live.tmp.$$" "$_live" || {
        rm -f "$_live.tmp.$$"
        ship_refuse 换恢复脚本 "换不上"
    }
    sync_hb
    manifest_append "$(printf '{"v":1,"kind":"record","name":"luyaz-recover.sh","path":"%s","md5":"%s","mac_time":%s,"boot_id":"%s","uptime":%s,"why":"install-recover %s"}' \
        "$_live" "$(md5_of "$_live")" "$(num "$_mm")" "$(boot_id)" "$(num "$(now)")" "$_mcm")" ||
        log "！！u60-recover.sh 已换上，但清单写不上（$MANIFEST）：用 record luyaz-recover.sh 补"
    rm -rf "${STAGE_DIR:?}/$_txn"
    log "换恢复脚本：u60-recover.sh → $_mcm（$(md5_of "$_live" | cut -c1-8)；上一版 ${_live##*/}.prev-$_txn）"
    say "已换上新的 u60-recover.sh（$_mcm，formats：$(sh "$_live" formats 2>/dev/null)）"
}

# move-d4 <mac unix time>: Luya-Z rename D4b-2 (manager docs/designs/luyaz-rename.md), once.
# /data/u60-ship → /data/luyaz/ship (txn, prev/, stage/, logs, verdict; u60-recover.sh as
# luyaz-recover.sh), the manifest → /data/luyaz/ship/manifest.jsonl (only the old directory's
# prefix in it rewritten), this file and datad-trial.sh → /data/luyaz/scripts (luyaz-ship.sh).
# SHIP_MARK last. Then the old recover becomes a shim (rc.local and the rc.local prev still
# call it): the new one's exit code, 0 when it is not there (never 10: no rc.local loop).
# Refused during a transaction, an observation window, or while the running core, sentinel,
# recover or doctor-legacy.sh does not know the new place. Asking the user is the Mac side's job.
ship_move_d4() {
    _mm=$1
    matches "$_mm" '^[0-9]{9,11}$' || ship_refuse 搬家 "mac_time “$_mm” 不是 Unix 秒"
    if [ -f "$SHIP_MARK" ]; then
        echo "U60 move-d4 already"
        return 0
    fi
    # not moved yet: the txn, lock, log and manifest are still the old ones
    SHIP_DIR=$SHIP_DIR_OLD TXN_FILE=$SHIP_DIR_OLD/txn STAGE_DIR=$SHIP_DIR_OLD/stage LOCK_FILE=$SHIP_DIR_OLD/lock
    LOG=${U60S_LOG:-$SHIP_DIR_OLD/ship.log} MANIFEST=${U60S_MANIFEST_OLD:-$ROOT/data/u60-manifest.jsonl}
    P=$(txn_busy)
    _ol=$(grep '"kind":"ship"' "$MANIFEST" 2>/dev/null | tail -n 1)
    if [ -n "$_ol" ]; then
        _ob=$(printf '%s' "$_ol" | sed -n 's/.*"boot_id":"\([^"]*\)".*/\1/p')
        _ou=$(printf '%s' "$_ol" | sed -n 's/.*"uptime":\([0-9]*\).*/\1/p')
        if [ "$_ob" = "$(boot_id)" ] && [ $(($(num "$(now)") - $(num "$_ou"))) -lt "$OBSERVE" ]; then
            P="$P${NL}上次上机还在观察期：过了再搬"
        fi
    fi
    _orec=$SHIP_DIR_OLD/u60-recover.sh
    [ -f "$CORE_BIN" ] && ! grep -a -q "$SHIP_CORE_MARK" "$CORE_BIN" 2>/dev/null &&
        P="$P${NL}在跑的核心不认 /data/luyaz/ship（D4b-1 之前）：先 ship core"
    [ -f "$SENT_BIN" ] && ! grep -a -q "$SHIP_SENT_MARK" "$SENT_BIN" 2>/dev/null &&
        P="$P${NL}在跑的哨兵不认 $SHIP_SENT_MARK（D4b-1 之前）：先 ship sentinel"
    grep -q 'luyaz/ship' "$_orec" 2>/dev/null ||
        P="$P${NL}$_orec 不认 /data/luyaz/ship：先 u60 install-recover"
    [ -f "$SCRIPTS_DIR/doctor-legacy.sh" ] && ! grep -q 'luyaz/ship' "$SCRIPTS_DIR/doctor-legacy.sh" &&
        P="$P${NL}doctor-legacy.sh 不认 /data/luyaz/ship：先 ship scripts"
    [ -f "$SHIP_EXEC_OLD" ] || P="$P${NL}$SHIP_EXEC_OLD 不在"
    [ -d "$SCRIPTS_DIR" ] || P="$P${NL}$SCRIPTS_DIR 不在"
    P=$(printf '%s' "$P" | sed '/^$/d')
    [ -n "$P" ] && ship_refuse 搬家 "$P"
    if command -v flock >/dev/null 2>&1; then
        exec 9>"$SHIP_DIR_OLD/lock"
        flock -n 9 || ship_refuse 搬家 "另一个 stage 正在进行"
    fi
    _n=$SHIP_DIR_NEW
    # a move stopped half-way (no mark) is started over
    rm -rf "$_n"
    mkdir -p "$_n" && cp -a "$SHIP_DIR_OLD/." "$_n/" || ship_refuse 搬家 "拷不过去（/data 满了？）；旧的没动"
    rm -f "$_n/u60-recover.sh"
    for _f in "$_n"/u60-recover.sh.prev-*; do
        [ -f "$_f" ] && mv -f "$_f" "$_n/luyaz-recover.sh.prev-${_f##*.prev-}"
    done
    cp -p "$_orec" "$_n/luyaz-recover.sh" || ship_refuse 搬家 "恢复脚本拷不过去；旧的没动"
    if [ -f "$MANIFEST" ]; then
        sed "s|\"$SHIP_DIR_OLD/|\"$SHIP_DIR_NEW/|g" "$MANIFEST" >"$_n/manifest.jsonl" || ship_refuse 搬家 "清单写不过去；旧的没动"
    fi
    cp -p "$SHIP_EXEC_OLD" "$SHIP_EXEC_NEW.tmp.$$" && mv -f "$SHIP_EXEC_NEW.tmp.$$" "$SHIP_EXEC_NEW" ||
        ship_refuse 搬家 "执行器拷不过去；旧的没动"
    if [ -f "$GUARD_DIR/datad-trial.sh" ]; then
        cp -p "$GUARD_DIR/datad-trial.sh" "$SCRIPTS_DIR/datad-trial.sh" || ship_refuse 搬家 "datad-trial.sh 拷不过去；旧的没动"
    fi
    $SYNC
    [ "$(md5_of "$_n/luyaz-recover.sh")" = "$(md5_of "$_orec")" ] &&
        [ "$(md5_of "$SHIP_EXEC_NEW")" = "$(md5_of "$SHIP_EXEC_OLD")" ] &&
        { [ ! -f "$MANIFEST" ] || [ "$(wc -l <"$_n/manifest.jsonl")" = "$(wc -l <"$MANIFEST")" ]; } ||
        ship_refuse 搬家 "拷过去以后对不上；旧的没动（新目录没有 .migrated，不生效）"
    : >"$SHIP_MARK" || ship_refuse 搬家 "写不了 $SHIP_MARK；旧的没动"
    $SYNC
    # from here on every reader takes the new place (this run too); the old one only keeps the shim
    _om=$MANIFEST
    SHIP_DIR=$_n MANIFEST=$_n/manifest.jsonl LOG=$_n/ship.log
    cat >"$_orec.tmp.$$" <<'SHIM'
#!/bin/sh
# Luya-Z rename D4: moved to /data/luyaz/ship/luyaz-recover.sh. rc.local (and its saved copies)
# may still call this path: hand over, exit code included; nothing to do when it is not there.
n=${U60R_ROOT:-}/data/luyaz/ship/luyaz-recover.sh
[ -f "$n" ] || exit 0
exec sh "$n" "$@"
SHIM
    chmod 755 "$_orec.tmp.$$" && mv -f "$_orec.tmp.$$" "$_orec"
    [ -f "$_om" ] && mv -f "$_om" "$_n/u60-manifest.jsonl.pre-d4"
    for _f in "$SHIP_DIR_OLD"/* ; do
        [ "$_f" = "$_orec" ] || rm -rf "$_f"
    done
    rm -f "$SHIP_EXEC_OLD" "$GUARD_DIR/datad-trial.sh"
    $SYNC
    log "搬家（D4）：$SHIP_DIR_OLD → $_n，执行器 → $SHIP_EXEC_NEW；旧的恢复脚本换成薄壳"
    manifest_append "$(printf '{"v":1,"kind":"record","name":"luyaz-recover.sh","path":"%s","md5":"%s","mac_time":%s,"boot_id":"%s","uptime":%s,"why":"move-d4"}' \
        "$_n/luyaz-recover.sh" "$(md5_of "$_n/luyaz-recover.sh")" "$(num "$_mm")" "$(boot_id)" "$(num "$(now)")")" ||
        log "！！搬好了，但清单写不上（$MANIFEST）：用 record luyaz-recover.sh 补"
    echo "U60 move-d4 done"
}

# uid-restart [<UI md5> [<u60-uid md5>]]: the screen's restart order, for
# the install kit as well (R8). Refused while a transaction is in progress.
ship_uid_restart() {
    for _a in "$@"; do
        matches "$_a" '^[0-9a-f]{32}$' || ship_refuse 重启屏幕 "“$_a” 不是 md5"
    done
    if txn_load && in_list "$X_PHASE" "$ACTIVE"; then
        ship_refuse 重启屏幕 "u60-ship 事务 $X_TXN（$X_COMP）还在进行：$X_PHASE"
    fi
    txn_clear
    if uid_restart "$@"; then
        say "u60-uid 和界面都在，版本对"
        return 0
    fi
    say "失败：$REASON"
    return 1
}

# selftest: a whole transaction in a /tmp sandbox with the selftest
# component (pass; check fails → rolled back; bad md5 → refused).
ship_selftest() {
    _sp=0
    _sf=0
    st_ok() {
        _sp=$((_sp + 1))
        echo "  ok   $1"
    }
    st_bad() {
        _sf=$((_sf + 1))
        echo "  FAIL $1"
    }
    for _a in md5sum sync mv cp mktemp df tr sed grep awk; do
        command -v "$_a" >/dev/null 2>&1 && continue
        st_bad "缺少命令 $_a"
    done
    if command -v "$SETSID" >/dev/null 2>&1 || command -v "$SSD" >/dev/null 2>&1 || command -v "/sbin/$SSD" >/dev/null 2>&1; then
        st_ok "能脱离 ssh 起执行器（setsid 或 start-stop-daemon）"
    else
        st_bad "没有 setsid 也没有 start-stop-daemon"
    fi
    _sb=$(mktemp -d "${TMPDIR:-/tmp}/u60-ship-selftest.XXXXXX") || {
        echo "selftest: FAIL（mktemp）"
        return 1
    }
    st_run() { # st_run <case> <expected phase> <meta md5 override or ->
        rm -rf "$_sb/root" "$_sb/tmp"
        mkdir -p "$_sb/root/data/selftest" "$_sb/root/data/selftest-state" "$_sb/root/data/luyaz/ship/stage/20000101-000000-selftest" "$_sb/tmp"
        : >"$_sb/root/data/luyaz/ship/.migrated"
        echo old-prog >"$_sb/root/data/selftest/prog"
        chmod 755 "$_sb/root/data/selftest/prog"
        echo old-conf >"$_sb/root/data/selftest/conf"
        echo state-a >"$_sb/root/data/selftest-state/a"
        _st=$_sb/root/data/luyaz/ship/stage/20000101-000000-selftest
        echo new-prog >"$_st/prog"
        echo new-conf >"$_st/conf"
        _mp=$(md5_of "$_st/prog")
        [ "$3" = - ] || _mp=$3
        printf 'v=1\ntxn=20000101-000000-selftest\ncomp=selftest\ncommit=0000000\nmac_time=1000000000\nformat=1\nfile=prog %s\nfile=conf %s\n' \
            "$_mp" "$(md5_of "$_st/conf")" >"$_st/meta"
        echo sb-boot >"$_sb/boot_id"
        _old_p=$(md5_of "$_sb/root/data/selftest/prog")
        _old_a=$(md5_of "$_sb/root/data/selftest-state/a")
        _new_p=$(md5_of "$_st/prog")
        [ "$1" = rollback ] && : >"$_sb/root/selftest-check-fail"
        (
            export U60S_ROOT="$_sb/root" U60S_TMP="$_sb/tmp" U60S_BOOT_ID="$_sb/boot_id"
            export U60S_MIN_FREE_KB=0 U60S_SELFTEST_CHECK=1 U60S_MANIFEST="$_sb/root/data/luyaz/ship/manifest.jsonl"
            unset U60S_DIR U60S_LOG U60S_TEST_LOG U60S_CRASH_AT
            sh "$SELF" stage 20000101-000000-selftest && sh "$SELF" run 20000101-000000-selftest
        ) >"$_sb/out" 2>&1
        _ph=$(sed -n 's/^phase=//p' "$_sb/root/data/luyaz/ship/txn" 2>/dev/null)
        if [ "${_ph:-none}" = "$2" ]; then st_ok "$1：结果 $2"; else st_bad "$1：结果 ${_ph:-none}，应为 $2"; fi
        case "$2" in
            done)
                [ "$(md5_of "$_sb/root/data/selftest/prog")" = "$_new_p" ] && st_ok "$1：正式文件是新版" || st_bad "$1：正式文件不是新版"
                [ "$(md5_of "$_sb/root/data/selftest/prog.prev-20000101-000000-selftest")" = "$_old_p" ] && st_ok "$1：上一版留着" || st_bad "$1：上一版没留"
                grep -q '"txn":"20000101-000000-selftest"' "$_sb/root/data/luyaz/ship/manifest.jsonl" 2>/dev/null && st_ok "$1：清单写了" || st_bad "$1：清单没写"
                ;;
            *)
                [ "$(md5_of "$_sb/root/data/selftest/prog")" = "$_old_p" ] && st_ok "$1：正式文件是旧版" || st_bad "$1：正式文件不是旧版"
                [ "$(md5_of "$_sb/root/data/selftest-state/a")" = "$_old_a" ] && st_ok "$1：状态文件是旧的" || st_bad "$1：状态文件没还原"
                [ -s "$_sb/root/data/luyaz/ship/manifest.jsonl" ] && st_bad "$1：清单被写了" || st_ok "$1：清单没动"
                ;;
        esac
    }
    st_run pass done -
    st_run rollback rolledback -
    st_run "bad md5" none 00000000000000000000000000000000
    rm -rf "$_sb"
    if [ $_sf = 0 ]; then
        echo "selftest: PASS（$_sp 项）"
        return 0
    fi
    echo "selftest: FAIL（$_sf 项不过，$_sp 项通过）"
    return 1
}

ship_usage() {
    cat >&2 <<EOF
usage: $0 stage <txn>        check the upload in stage/<txn>/, place side files
       $0 start <txn>        run the transaction detached from the SSH session
       $0 run <txn>          the executor itself (foreground)
       $0 status             key=value: txn comp phase reason hb_age exec_alive …
       $0 wait <seconds>     until a terminal phase (exit 4 on timeout)
       $0 abort              TERM the executor (it finishes by phase)
       $0 recover-live [--force]   finish a transaction whose executor died
       $0 manifest-finish    write the manifest line a transaction still owes
       $0 selftest           whole transactions in a /tmp sandbox
       $0 print-launch datad|touch|lweb   the trial's start command
       $0 print-state <component>          the state files it may carry
       $0 uid-restart [<UI md5> [<u60-uid md5>]]   restart u60-uid, confirm both run at those versions
       $0 install-recover <txn>   replace u60-recover.sh with stage/<txn>/u60-recover.sh
       $0 move-d4 <mac unix time>   Luya-Z D4: the ship family to /data/luyaz/ship (once)
       $0 record <name|all> <mac unix time> [why…]   accept a recorded file as it is now
       $0 retire <mac unix time>                   the removed components out of the manifest (10c)
       $0 prepare-rollback <comp> <txn> <mac unix time>   stage the version before the last ship
       $0 record-kit <comp> <kit stamp> <commit> <format> <mac unix time> [dirty]   the install kit put it there
       $0 tree-fp <dir>      the directory fingerprint (exit 1: cannot be taken)
formats: docs/SHIP.md
EOF
    exit 2
}

ship_main() {
    _cmd=$1
    [ $# -gt 0 ] && shift
    case "$_cmd" in
        stage) [ $# = 1 ] || ship_usage; ship_stage "$1" ;;
        start) [ $# = 1 ] || ship_usage; ship_start "$1" ;;
        run) [ $# = 1 ] || ship_usage; ship_run "$1" ;;
        status) ship_status ;;
        wait) [ $# = 1 ] || ship_usage; ship_wait "$1" ;;
        abort) ship_abort ;;
        recover-live) ship_recover_live "$1" ;;
        manifest-finish) ship_manifest_finish ;;
        selftest) ship_selftest ;;
        record) [ $# -ge 2 ] || ship_usage; ship_record "$@" ;;
        retire) [ $# = 1 ] || ship_usage; ship_retire "$1" ;;
        prepare-rollback) [ $# = 3 ] || ship_usage; ship_prepare_rollback "$@" ;;
        record-kit) [ $# -ge 5 ] && [ $# -le 6 ] || ship_usage; ship_record_kit "$@" ;;
        tree-fp) [ $# = 1 ] || ship_usage; tree_fp "$1" ;;
        install-recover) [ $# = 1 ] || ship_usage; ship_install_recover "$1" ;;
        move-d4) [ $# = 1 ] || ship_usage; ship_move_d4 "$1" ;;
        uid-restart) [ $# -le 2 ] || ship_usage; ship_uid_restart "$@" ;;
        print-launch)
            # what the trial starts, to compare with the init script (tests)
            case "$1" in
                datad | core) comp_load "$1" && launch_cmd ;;
                lweb) comp_load lweb && lw_launch_cmd ;;
                touch) comp_load touch && touch_launch_cmd ;;
                *) ship_usage ;;
            esac
            ;;
        print-state)
            # the component's allowed state files, one per line (tests: ship/<comp>/state-files)
            [ $# = 1 ] || ship_usage
            case "$1" in
                datad | core | touch | uid | guard | lweb) comp_load "$1" && printf '%s\n' $C_STATE_ALLOW ;;
                *) ship_usage ;;
            esac
            ;;
        *) ship_usage ;;
    esac
}

# ── dispatch ────────────────────────────────────────────────────────────────

case "$ENTRY" in
    datad-trial)
        trial_main "$@"
        ;;
    *)
        ship_main "$@"
        ;;
esac
