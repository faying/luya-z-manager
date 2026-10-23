#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# alert-lib.sh — the one way to record an alert event. Sourced, not run.
#
#   . /data/luyaz/scripts/alert-lib.sh
#   alert_add agent-crash "exit 139 (SIGSEGV)"
#
# Shared by the device shell scripts that raise alerts (e.g. scripts/tailscale/
# start.sh); luyaz-core's svcwatch writes the same queue. File formats: source/manager docs/RELIABILITY.md §4 — the
# agent reads what this writes, so change both together.
#
# Why events carry a sequence number and not a line number or a timestamp:
# the file is trimmed, which moves line numbers, and before the network sets
# the clock the device thinks it is 2025-01-04. The number is allocated under the
# directory's flock, so the web's "read" cursor and the core's "SMS done"
# cursor stay valid across trims.
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
# ─────────────────────────────────────────────────────────────────────────────

ALERT_DIR=${ALERT_DIR:-/data/alerts}
ALERT_UPTIME_FILE=${ALERT_UPTIME_FILE:-/proc/uptime}
ALERT_KEEP=200    # lines kept after a trim
ALERT_TRIM_AT=300 # trim once the queue grows past this

# The text goes verbatim into an SMS and a web page: printable ASCII only, no
# tabs or newlines (they are the field and record separators), at most 120
# characters. Never pass configuration (numbers, passwords, SSIDs) in it.
alert_clean() {
    printf '%s' "$*" | tr '\t\r\n' '   ' | tr -cd '\040-\176' | cut -c1-120
}

# alert_add <kind> <text…>
# kind: lowercase letters, digits and '-'. Returns non-zero if the event could
# not be recorded; callers should carry on regardless — a full /data must not
# stop a crashed program from being restarted.
alert_add() {
    _kind=$1
    shift
    case "$_kind" in
        '' | *[!a-z0-9-]*) return 2 ;;
    esac
    _text=$(alert_clean "$*")
    mkdir -p "$ALERT_DIR" 2>/dev/null && chmod 700 "$ALERT_DIR" 2>/dev/null
    (
        flock 8 || exit 1
        _seq=$(cat "$ALERT_DIR/seq" 2>/dev/null)
        case "$_seq" in '' | *[!0-9]*) _seq=0 ;; esac
        # seq lost or behind (power cut on a fresh file): never reuse a number the queue has,
        # the read and SMS cursors would skip the new event.
        _qmax=$(awk -F'\t' 'NF == 5 && $1 ~ /^[0-9]+$/ && $1 + 0 > m { m = $1 + 0 } END { print m + 0 }' \
            "$ALERT_DIR/queue" 2>/dev/null)
        case "$_qmax" in '' | *[!0-9]*) _qmax=0 ;; esac
        [ "$_qmax" -gt "$_seq" ] && _seq=$_qmax
        _seq=$((_seq + 1))
        # a writer killed mid-line left no newline: end that line, or ours would join it
        [ -n "$(tail -c 1 "$ALERT_DIR/queue" 2>/dev/null)" ] && echo >>"$ALERT_DIR/queue"
        _up=$(cut -d. -f1 "$ALERT_UPTIME_FILE" 2>/dev/null)
        # seq is written before the line: a crash in between skips a number,
        # which is harmless; the reverse could hand out the same number twice.
        echo "$_seq" >"$ALERT_DIR/.seq.tmp" && mv "$ALERT_DIR/.seq.tmp" "$ALERT_DIR/seq" || exit 1
        printf '%s\t%s\t%s\t%s\t%s\n' "$_seq" "$(date +%s)" "${_up:-0}" "$_kind" "$_text" \
            >>"$ALERT_DIR/queue" || exit 1
        # Readers never take the lock, so trim by replacing the file whole.
        if [ "$(wc -l <"$ALERT_DIR/queue")" -gt "$ALERT_TRIM_AT" ]; then
            tail -n "$ALERT_KEEP" "$ALERT_DIR/queue" >"$ALERT_DIR/.queue.tmp" &&
                mv "$ALERT_DIR/.queue.tmp" "$ALERT_DIR/queue"
        fi
    ) 8>>"$ALERT_DIR/lock"
}
