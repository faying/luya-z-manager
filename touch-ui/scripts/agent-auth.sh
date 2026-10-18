#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# agent-auth.sh — move the admin password into the password file luyaz-web
# reads (/data/luyaz/web.env; /data/zte-agent.env in the old layout), and prove
# the admin pages are not running open afterwards.
#
#   agent-auth.sh migrate   /data/local/tmp/start_zte_agent.sh → the password file
#   agent-auth.sh verify    the three checks, against http://127.0.0.1:9090
#
# Why this matters (first seen with the retired zte-agent): an admin
# backend with no password set answers EVERY request
# unauthenticated. The old launcher carried the password as an `export`; the
# procd service does not run that launcher, so moving to procd without this
# step would silently open the admin API to the whole LAN.
#
# The password never appears in output, in argv (ps shows argv to everyone),
# or in a file other than the 600 env file and a 600 temp file for curl.
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────

OLD=${AGENT_AUTH_OLD:-/data/local/tmp/start_zte_agent.sh}
# after the core's move (Phase 10 10e R6) the file is /data/luyaz/web.env
# (luyaz-web reads it first); same switch line as u60-ship.sh (scripts/test/luyaz-dir)
luyaz_new() { [ -f "$1/data/luyaz/core/.migrated" ]; }
if luyaz_new "${AGENT_AUTH_ROOT:-}"; then _env_def=/data/luyaz/web.env; else _env_def=/data/zte-agent.env; fi
ENV_FILE=${AGENT_AUTH_ENV:-$_env_def}
URL=${AGENT_AUTH_URL:-http://127.0.0.1:9090}
CURL=${AGENT_AUTH_CURL:-/usr/bin/curl}

say() { echo "[agent-auth] $*"; }
die() { echo "[agent-auth] FAILED: $*" >&2; exit 1; }

# Only what can go into both a shell-free env line and a JSON string without
# escaping: no quotes, backslashes, whitespace or control characters.
plain() {
    [ -n "$1" ] || return 1
    case "$1" in *[\"\'\\\ \	]*) return 1 ;; esac
    [ "$(printf '%s' "$1" | tr -d '\041-\176' | wc -c)" -eq 0 ]
}

read_old() {
    [ -f "$OLD" ] || die "$OLD not found"
    _line=$(grep -m1 -E '^[[:space:]]*(export[[:space:]]+)?ZTE_AGENT_PASSWORD=' "$OLD") ||
        die "no ZTE_AGENT_PASSWORD line in $OLD"
    _v=${_line#*ZTE_AGENT_PASSWORD=}
    # install.sh writes it single-quoted; older launchers had it bare.
    case "$_v" in
        \'*\') _v=${_v#\'}; _v=${_v%\'} ;;
    esac
    plain "$_v" || die "the password in $OLD is empty or has quotes/spaces/backslashes; set it by hand in $ENV_FILE"
    PASSWORD=$_v
}

read_env() {
    [ -f "$ENV_FILE" ] || die "$ENV_FILE not found (run: $0 migrate)"
    _line=$(grep -m1 '^ZTE_AGENT_PASSWORD=' "$ENV_FILE") || die "no ZTE_AGENT_PASSWORD in $ENV_FILE"
    PASSWORD=${_line#ZTE_AGENT_PASSWORD=}
    plain "$PASSWORD" || die "password in $ENV_FILE is empty or not plain"
}

do_migrate() {
    read_old
    if [ -f "$ENV_FILE" ]; then
        _cur=$(grep -m1 '^ZTE_AGENT_PASSWORD=' "$ENV_FILE")
        [ "${_cur#ZTE_AGENT_PASSWORD=}" = "$PASSWORD" ] && { say "$ENV_FILE already holds the same password"; exit 0; }
        die "$ENV_FILE exists with a different password; not overwriting"
    fi
    _tmp=$ENV_FILE.tmp.$$
    (umask 077; printf 'ZTE_AGENT_PASSWORD=%s\n' "$PASSWORD" >"$_tmp") || die "write $_tmp"
    chmod 600 "$_tmp" && mv -f "$_tmp" "$ENV_FILE" || { rm -f "$_tmp"; die "install $ENV_FILE"; }
    say "password moved to $ENV_FILE (600); $OLD left as it was"
}

# status <method> <path> [token] [body-file] → HTTP status code
status() {
    set -- "$1" "$2" "${3:-}" "${4:-}"
    if [ -n "$4" ]; then
        $CURL -s -m 10 -o "$RESP" -w '%{http_code}' -X "$1" -H 'Content-Type: application/json' \
            --data @"$4" "$URL$2"
    elif [ -n "$3" ]; then
        # The token is short-lived and only good on this device; argv is fine.
        $CURL -s -m 10 -o "$RESP" -w '%{http_code}' -H "Authorization: Bearer $3" "$URL$2"
    else
        $CURL -s -m 10 -o "$RESP" -w '%{http_code}' "$URL$2"
    fi
}

do_verify() {
    read_env
    WORK=$(mktemp -d) || die "mktemp"
    chmod 700 "$WORK"
    trap 'rm -rf "$WORK"' EXIT
    RESP=$WORK/resp
    BODY=$WORK/body
    fails=0

    # 1. No token: must be refused. A 200 here means the agent has no password.
    c=$(status GET /api/scenario)
    if [ "$c" = 401 ]; then say "ok   unauthenticated /api/scenario → 401"; else say "FAIL unauthenticated /api/scenario → $c (want 401: the API is OPEN)"; fails=$((fails + 1)); fi

    # 2. The migrated password logs in, and its token works.
    (umask 077; printf '{"password":"%s"}' "$PASSWORD" >"$BODY")
    c=$(status POST /api/auth/login "" "$BODY")
    token=$(sed -n 's/.*"token"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$RESP")
    if [ "$c" = 200 ] && [ -n "$token" ] && [ "$(status GET /api/scenario "$token")" = 200 ]; then
        say "ok   the password logs in and its token reaches /api/scenario"
    else
        say "FAIL login with the migrated password → $c${token:+ (token did not work)}"
        fails=$((fails + 1))
    fi

    # 3. An empty password must not: "" is a valid password to set, so a 401
    #    in check 1 alone cannot tell "migrated" from "set to empty".
    printf '{"password":""}' >"$BODY"
    c=$(status POST /api/auth/login "" "$BODY")
    if grep -q '"token"' "$RESP" 2>/dev/null; then
        say "FAIL an empty password logged in (→ $c)"
        fails=$((fails + 1))
    else
        say "ok   an empty password is refused (→ $c)"
    fi

    [ "$fails" = 0 ] || die "$fails of 3 checks failed — roll back to the old launcher"
    say "all 3 checks passed"
}

case "$1" in
    migrate) do_migrate ;;
    verify) do_verify ;;
    *) echo "usage: $0 migrate|verify" >&2; exit 2 ;;
esac
