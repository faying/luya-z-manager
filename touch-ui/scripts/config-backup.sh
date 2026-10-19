#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# config-backup.sh — back up and restore the U60 Pro (MU5250) configuration.
#
#   config-backup.sh list    [--with-tailscale]   what a backup would contain
#   config-backup.sh export  [--with-tailscale]   tar.gz to stdout
#   config-backup.sh verify  <archive>            unpack into a sandbox, check every file parses
#   config-backup.sh plan    <archive> [root]     dry run: what a restore would change
#   config-backup.sh restore <archive> [root]     write the files (root defaults to /)
#
# Driven from the computer by the install kit (./install.sh backup / restore);
# backups are only ever stored on that computer.
#
# Configuration only, never runtime state (review X10): the scenario engine's
# state/pin/log, alert history, crash logs, downloaded providers and
# rule sets are all left out — restoring them would replay another moment's
# state onto a device. Tailscale's state is the node's identity: restoring it
# onto a second device while the first is online makes two machines claim to
# be one, so it is only included with --with-tailscale.
#
# Wi-Fi settings go in as `uci export wireless` text and are restored by
# writing /etc/config/wireless; nothing here reloads Wi-Fi — that, like every
# Wi-Fi change on this device, is for the owner to decide (and to do under the
# Wi-Fi lock).
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────

UCI=${CB_UCI:-uci}
JSONFILTER=${CB_JSONFILTER:-jsonfilter}
ROOT_SRC=${CB_ROOT:-}          # tests: read the "device" from here instead of /

# Configuration files, absolute paths. Secrets are marked: restored as 600.
FILES="
/data/luyaz/web.env                          secret
/data/alerts/sms-to                          secret
/data/alerts/sms-abroad                      plain
/data/scenario/scenarios.json                json
/data/local/tmp/charge_limit.json            json
/data/local/tmp/doh_config.json              json
/data/local/tmp/scheduler.json               json
/data/local/tmp/sms_forward.json             json-secret
/data/region/active                          plain
/data/luyaz/ui/devui.conf                    plain
/data/ssh/authorized_keys                    secret
/data/homemode/ssids                         plain
/data/homemode/config                        plain
"
FILES="$FILES
/data/chill/chill.env                        secret
/data/chill/template.yaml                    secret
/data/chill/confirmed                        plain
/data/luyaz/ui/chill.conf                    plain
"
TAILSCALE_DIR=/data/tailscale/state

die() { echo "config-backup: $*" >&2; exit 1; }

with_ts=0
for a in "$@"; do [ "$a" = --with-tailscale ] && with_ts=1; done

present() { # the configured files that exist, one path per line
    echo "$FILES" | while read -r p kind; do
        [ -n "$p" ] || continue
        [ -f "$ROOT_SRC$p" ] && echo "$p"
    done
    if [ "$with_ts" = 1 ] && [ -d "$ROOT_SRC$TAILSCALE_DIR" ]; then
        (cd "$ROOT_SRC/" && find "${TAILSCALE_DIR#/}" -type f) | sed 's|^|/|'
    fi
}

kind_of() { # secret | json | json-secret | plain | tailscale
    case "$1" in "$TAILSCALE_DIR"/*) echo tailscale; return ;; esac
    echo "$FILES" | awk -v p="$1" '$1 == p {print $2}'
}

do_list() {
    present
    echo "(Wi-Fi: uci export wireless)"
}

do_export() {
    W=$(mktemp -d) || die "mktemp"
    trap 'rm -rf "$W"' EXIT
    mkdir -p "$W/u60-backup/files"
    present > "$W/u60-backup/list"
    while read -r p; do
        mkdir -p "$W/u60-backup/files$(dirname "$p")"
        cp -p "$ROOT_SRC$p" "$W/u60-backup/files$p"
    done < "$W/u60-backup/list"
    $UCI export wireless > "$W/u60-backup/wireless.uci" 2>/dev/null || rm -f "$W/u60-backup/wireless.uci"
    {
        echo "u60 config backup"
        echo "time: $(date '+%Y-%m-%d %H:%M:%S') device local"
        echo "firmware: $(ubus call zwrt_web device_info '{}' 2>/dev/null | sed -n 's/.*"wa_inner_version": *"\([^"]*\)".*/\1/p')"
        echo "tailscale: $([ "$with_ts" = 1 ] && echo included || echo not included)"
        echo "files:"
        (cd "$W/u60-backup" && find files wireless.uci -type f 2>/dev/null | sort | while read -r f; do
            echo "  $(md5sum "$f" | cut -c1-32)  $f"
        done)
    } > "$W/u60-backup/MANIFEST"
    tar czf - -C "$W" u60-backup
}

unpack() { # <archive> → $X/u60-backup
    [ -f "$1" ] || die "no such archive: $1"
    # Look before unpacking: every entry must sit under u60-backup/, with no
    # absolute paths and no '..' anywhere.
    tar tzf "$1" > /tmp/u60-backup-list.$$ 2>/dev/null || { rm -f /tmp/u60-backup-list.$$; die "not a readable tar.gz"; }
    if grep -qv '^u60-backup/\|^u60-backup$' /tmp/u60-backup-list.$$ || grep -q '\.\.' /tmp/u60-backup-list.$$; then
        rm -f /tmp/u60-backup-list.$$
        die "archive has entries outside u60-backup/ or '..' paths; refusing"
    fi
    rm -f /tmp/u60-backup-list.$$
    X=$(mktemp -d) || die "mktemp"
    tar xzf "$1" -C "$X" 2>/dev/null || die "not a readable tar.gz"
    [ -f "$X/u60-backup/MANIFEST" ] || die "not a u60 config backup (no MANIFEST)"
}

check_json() { # a JSON file parses
    if command -v "$JSONFILTER" >/dev/null 2>&1; then
        $JSONFILTER -i "$1" -e '@' >/dev/null 2>&1
    else
        # No jsonfilter (tests on a bare busybox): at least an object or array.
        head -c 1 "$1" | grep -q '[{[]'
    fi
}

do_verify() {
    unpack "$1"
    trap 'rm -rf "$X"' EXIT
    B=$X/u60-backup
    bad=0
    # MANIFEST checksums
    sed -n 's/^  \([0-9a-f]\{32\}\)  \(.*\)$/\1 \2/p' "$B/MANIFEST" | while read -r sum f; do
        [ "$(md5sum "$B/$f" 2>/dev/null | cut -c1-32)" = "$sum" ] || echo "  FAIL checksum $f"
    done | tee "$X/cs"
    [ -s "$X/cs" ] && bad=1
    (cd "$B" && find files -type f) | while read -r f; do
        p=${f#files}
        k=$(kind_of "$p")
        case "$k" in
            json | json-secret)
                check_json "$B/$f" && echo "  ok   $p (json)" || { echo "  FAIL $p is not valid JSON"; echo x >> "$X/bad"; } ;;
            *)
                [ -r "$B/$f" ] && echo "  ok   $p" || { echo "  FAIL $p unreadable"; echo x >> "$X/bad"; } ;;
        esac
        case "$p" in
            /data/luyaz/web.env)
                grep -q -e '^LUYAZ_WEB_PASSWORD=.' -e '^ZTE_AGENT_PASSWORD=.' "$B/$f" || { echo "  FAIL $p has no password line"; echo x >> "$X/bad"; } ;;
        esac
    done
    if [ -f "$B/wireless.uci" ]; then
        mkdir -p "$X/uci"
        if $UCI -c "$X/uci" import wireless < "$B/wireless.uci" 2>/dev/null && $UCI -c "$X/uci" show wireless >/dev/null 2>&1; then
            echo "  ok   wireless (uci parses)"
        else
            echo "  FAIL wireless.uci does not parse"; echo x >> "$X/bad"
        fi
    fi
    [ -f "$X/bad" ] && bad=1
    [ "$bad" = 0 ] && echo "verify: all files check out" || { echo "verify: problems found"; exit 1; }
}

do_plan() { # <archive> <root>
    unpack "$1"
    trap 'rm -rf "$X"' EXIT
    R=${2:-/}
    case "$R" in */) ;; *) R="$R/" ;; esac
    (cd "$X/u60-backup" && find files -type f | sort) | while read -r f; do
        p=${f#files}
        t=$R${p#/}
        if [ ! -e "$t" ]; then echo "  new        $p"
        elif cmp -s "$X/u60-backup/$f" "$t"; then echo "  same       $p"
        else echo "  overwrite  $p"; fi
    done
    [ -f "$X/u60-backup/wireless.uci" ] && echo "  (Wi-Fi settings: written to /etc/config/wireless only with restore; Wi-Fi is not reloaded)"
}

do_restore() { # <archive> <root>
    unpack "$1"
    trap 'rm -rf "$X"' EXIT
    R=${2:-/}
    case "$R" in */) ;; *) R="$R/" ;; esac
    (cd "$X/u60-backup" && find files -type f) | while read -r f; do
        p=${f#files}
        t=$R${p#/}
        mkdir -p "$(dirname "$t")"
        [ -e "$t" ] && cp -p "$t" "$t.pre-restore" 2>/dev/null
        cp "$X/u60-backup/$f" "$t.restore-tmp" || { echo "  FAIL $p"; continue; }
        # an archive from before F7 has the password under the old key: written under the new one
        if [ "$p" = /data/luyaz/web.env ] && ! grep -q '^LUYAZ_WEB_PASSWORD=' "$t.restore-tmp"; then
            sed -i 's/^ZTE_AGENT_PASSWORD=/LUYAZ_WEB_PASSWORD=/' "$t.restore-tmp"
        fi
        mv -f "$t.restore-tmp" "$t" || { echo "  FAIL $p"; continue; }
        case "$(kind_of "$p")" in secret | json-secret | tailscale) chmod 600 "$t" ;; *) chmod 644 "$t" ;; esac
        echo "  restored $p"
    done
    if [ -f "$X/u60-backup/wireless.uci" ]; then
        mkdir -p "${R}etc/config"
        [ -e "${R}etc/config/wireless" ] && cp -p "${R}etc/config/wireless" "${R}etc/config/wireless.pre-restore"
        cp "$X/u60-backup/wireless.uci" "${R}etc/config/wireless.restore-tmp" &&
            mv -f "${R}etc/config/wireless.restore-tmp" "${R}etc/config/wireless" &&
            echo "  restored /etc/config/wireless (not reloaded; previous copy in wireless.pre-restore)"
    fi
    echo "restore: done. Restart the services to pick the files up (or reboot)."
}

case "$1" in
    list) do_list ;;
    export) do_export ;;
    verify) do_verify "$2" ;;
    plan) do_plan "$2" "${3:-/}" ;;
    restore) do_restore "$2" "${3:-/}" ;;
    *) sed -n '5,10p' "$0"; exit 2 ;;
esac
