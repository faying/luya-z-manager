#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# doctor-legacy.sh — the old body of doctor.sh (Phase 10 X11): doctor.sh now runs
# the core's `doctor` and comes here only for an old core or --report /
# --calibrate-standby. Read-only health check of the U60 Pro (MU5250) setup.
#
#   sh /data/luyaz/scripts/doctor.sh          human-readable (install.sh doctor)
#   sh /data/luyaz/scripts/doctor.sh --tsv    one check per line for u60d's health API:
#                                          <ok|warn|bad>\t<id>\t<label>\t<detail>
#                                          (last row: the device manifest, docs/SHIP.md)
#   sh /data/luyaz/scripts/doctor.sh --tsv2   the same rows with English beside them, for the
#                                          English UI (docs/designs/ui-english.md R2):
#                                          first line "#tsv2", then
#                                          <ok|warn|bad>\t<id>\t<label>\t<detail>\t<label_en>\t<detail_en>
#                                          (columns 1-4 always equal the --tsv row; --tsv
#                                          itself never changes, older readers split it in 4)
#   sh /data/luyaz/scripts/doctor.sh --manifest  the manifest, entry by entry:
#                                          <same|differs|unrecorded>\t<kind:name>\t<path>\t<recorded>\t<actual>
#                                          then state\t<ok|warn>\t<verdict>
#   sh /data/luyaz/scripts/doctor.sh --report 24h|7d   the scorecard from the ledger
#   sh /data/luyaz/scripts/doctor.sh --ledger-selftest can the ledger work here (PASS/FAIL)
#   sh /data/luyaz/scripts/doctor.sh --tree-fp <dir>  a directory's fingerprint (docs/SHIP.md)
#   sh /data/luyaz/scripts/doctor.sh --calibrate-standby
#                                          set the standby sentinel's baseline from
#                                          u60-guard's screen-off records (≥30 lines;
#                                          run after 30-60 min with the screen off)
#
# One implementation of "is this device healthy", used by the install kit over
# SSH (works with u60d dead) and by u60d's /v2/api/health (health page,
# touch-screen summary). Changes nothing, ever — safe to run any time.
# Every command and path can be overridden from the environment for tests.
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────

UBUS=${DOC_UBUS:-ubus}
UCI=${DOC_UCI:-uci}
PS=${DOC_PS:-ps w}
RC=${DOC_RC:-/etc/rc.local}
INITD=${DOC_INITD:-/etc/init.d}
UPTIME_FILE=${DOC_UPTIME:-/proc/uptime}
HEARTBEAT=${DOC_HEARTBEAT:-/tmp/scenario.heartbeat}
MARKER=${DOC_MARKER:-/tmp/luyaz/wifiguard.took-over}
# zte-agent is gone (manager docs/designs/u60-web.md 8d-2c): u60d serves its
# API on 127.0.0.1:9460 /v2/api/*, luyaz-web the pages on :9090
ALERTS=${DOC_ALERTS:-/data/alerts}
CRASH=${DOC_CRASH:-/data/crashlog}
OVERLAY_RCD=${DOC_OVERLAY_RCD:-/zteoverlay/etc-upper_a/rc.d}
DAEMON_CONF=${DOC_DAEMON_CONF:-/etc/config/zte_topsw_daemon.conf}
DATA_DIR=${DOC_DATA:-/data}
WGET=${DOC_WGET:-wget}
PROC=${DOC_PROC:-/proc}
STANDBY_STAT=${DOC_STANDBY_STAT:-/tmp/standby.stat}
# scripts_file NAME: the device scripts' directory (Luya-Z rename D1, the same rule as u60d paths.rs)
scripts_file() { echo "/data/luyaz/scripts/$1"; }
STANDBY_BASE=${DOC_STANDBY_BASE:-$(scripts_file standby.baseline)}
DATE=${DOC_DATE:-date}
DF=${DOC_DF:-df}
UID_WANT=${DOC_UID_WANT:-/tmp/luyaz/screen.want}
# luyaz_new ROOT: the core's layout switch (Phase 10 10e; same line in u60-ship.sh,
# scripts/test/luyaz-dir): /data/luyaz/core/.migrated = luyaz-core, else zwrt-datad
luyaz_new() { [ -f "$1/data/luyaz/core/.migrated" ]; }
LROOT=${DOC_ROOT:-}
LUYAZ_TMP=${DOC_LUYAZ_TMP:-/tmp/luyaz}
if luyaz_new "$LROOT"; then CORE_NAME=luyaz-core; else CORE_NAME=zwrt-datad; fi
# the state trees are the new ones on every device (Luya-Z rename E3: no old-or-new picker; = doctor.rs)
GUARD_TMP=/tmp/luyaz
UID_STATE=${DOC_UID_STATE:-/data/luyaz/screen}
CLOCK_OK=${DOC_CLOCK_OK:-$GUARD_TMP/clock-ok}
WALL_LAST=${DOC_WALL_LAST:-/data/ledger/state/wall.last}
LEDGER_DIR=${DOC_LEDGER_DIR:-/data/ledger}
CRASHCAP_DIR=${DOC_CRASHCAP_DIR:-/data/crashcap}
# Where the scorecard's windows can start at the earliest (docs/LEDGER.md §11),
# on the device clock (local time under UTC): modem crash recovery first on,
# and u60-uid taking the screen over.
KEYLOG=${DOC_KEYLOG:-/data/logfs/key.log}
MSS_RECOVERY=${DOC_MSS_RECOVERY:-/sys/class/remoteproc/remoteproc0/recovery}
KMSG=${DOC_KMSG:-/dev/kmsg}
BOOT_ID_FILE=${DOC_BOOT_ID_FILE:-/proc/sys/kernel/random/boot_id}
LEDGER_TMP=${DOC_LEDGER_TMP:-$GUARD_TMP/ledger}
JSONFILTER=${DOC_JSONFILTER:-jsonfilter}
DATAD_URL=${DOC_DATAD_URL:-http://127.0.0.1:9460/v2/state}
SCORECARD_URL=${DOC_SCORECARD_URL:-http://127.0.0.1:9460/v2/scorecard}
SCORECARD_WAIT=${DOC_SCORECARD_WAIT:-15}
CALIB_URL=${DOC_CALIB_URL:-http://127.0.0.1:9460/v2/longterm/calib}
SELFTEST_WAIT=${DOC_SELFTEST_WAIT:-6}
WAN_IF=${DOC_WAN_IF:-rmnet_data0}
ROUTE=${DOC_ROUTE:-/proc/net/route}
IP=${DOC_IP:-ip}
RECOV_LIVE=1790458980 # 2026-09-26 21:43
UID_LIVE=1790121600   # 2026-09-23
CLOCK_YEAR_MIN=1767225600 # 2026-01-01 00:00

# Standby sentinel. u60-guard writes one line per screen-off minute:
#   <uptime> <cellular pkts/min> <tunnel pkts/min> <wakeups/s per program | ->
# Columns 2-8 are judged against a baseline of median and MAD (median
# absolute deviation) taken from those same records.
# Standby baseline, version 2 (docs/LEDGER.md §12): besides "<col> <median> <MAD>"
# it records, per column, the fingerprint of what that column measures — the
# program's binary md5 (u60-guard caches them in $FP, computed once per boot or
# pid change) plus its config files (small, hashed here). A column whose
# fingerprint no longer matches was measured on other software: "baseline
# stale", not judged, until someone recalibrates at home while naturally idle.
FP=${DOC_FP:-$GUARD_TMP/ledger/fp}
FP_CHANGED=${DOC_FP_CHANGED:-$GUARD_TMP/ledger/fp-changed}
TS_TUNING=${DOC_TS_TUNING:-/data/tailscale/tuning.env}
CHILL_TEMPLATE=${DOC_CHILL_TEMPLATE:-/data/chill/template.yaml}
CHILL_ENV=${DOC_CHILL_ENV:-/data/chill/chill.env}
CHILL_STATE=${DOC_CHILL_STATE:-/tmp/chill.state}
standby_progs='tailscaled mihomo u60pro-devui zwrt-datad zte-agent'
standby_labels='- 蜂窝包/分 Tailscale隧道包/分 tailscaled唤醒/秒 mihomo唤醒/秒 触屏唤醒/秒 数据服务唤醒/秒 后台唤醒/秒'
standby_labels_en='-|Cell pkts/min|Tailscale tunnel pkts/min|tailscaled wakeups/s|mihomo wakeups/s|Screen UI wakeups/s|Data service wakeups/s|Admin wakeups/s'

standby_last() { # the last column of a sentinel record: uptime, cellular, tunnel, then one per program
    set -- $standby_progs
    echo $((3 + $#))
}
cfg_md5() { # md5 prefix of a config file, "none" when it does not exist
    _m=$(md5sum "$1" 2>/dev/null | cut -c1-8)
    echo "${_m:-none}"
}
prog_md5() { # md5 prefix of a running program from u60-guard's cache, "?" when unknown
    _m=
    [ -f "$FP" ] && _m=$(awk -v n="$1" '$1 == n { print $3; exit }' "$FP" 2>/dev/null)
    echo "${_m:-?}"
}
# one line "<column> <fingerprint>" for columns 3.. (column 2, cellular packets, has none)
standby_fp() {
    _tsm=$(prog_md5 tailscaled)
    _tun=$(cfg_md5 "$TS_TUNING")
    echo "3 $_tsm+$_tun"
    _k=4
    for _p in $standby_progs; do
        case "$_p" in
            tailscaled) _f="$_tsm+$_tun" ;;
            mihomo) _f="$(prog_md5 mihomo)+$(cfg_md5 "$CHILL_TEMPLATE")+$(cfg_md5 "$CHILL_ENV")" ;;
            # retired (8d-2c): no process, the column is not judged (awk below)
            zte-agent) _f=retired ;;
            # the column keeps its name; after the move (10e) the process is luyaz-core
            zwrt-datad) _f=$(prog_md5 "$CORE_NAME"); [ "$_f" = "?" ] && _f=$(prog_md5 zwrt-datad) ;;
            *) _f=$(prog_md5 "$_p") ;;
        esac
        echo "$_k $_f"
        _k=$((_k + 1))
    done
}

calibrate_standby() {
    n=$(wc -l 2>/dev/null <"$STANDBY_STAT" || echo 0)
    if [ "${n:-0}" -lt 30 ]; then
        echo "只有 ${n:-0} 行息屏记录，至少要 30 行（屏幕熄灭约 30 分钟后再试）"; return 1
    fi
    {
        echo v2
        awk -v last="$(standby_last)" '
        function sortv(a, c,   i, j, v) { for (i = 2; i <= c; i++) { v = a[i]; j = i - 1; while (j > 0 && a[j] > v) { a[j + 1] = a[j]; j-- } a[j + 1] = v } }
        function med(a, c) { sortv(a, c); return (c % 2) ? a[(c + 1) / 2] : (a[c / 2] + a[c / 2 + 1]) / 2 }
        { for (k = 2; k <= last; k++) if ($k != "-" && $k != "") v[k, ++cnt[k]] = $k }
        END {
            for (k = 2; k <= last; k++) {
                c = cnt[k] + 0; if (c == 0) { print k, "-", "-"; continue }
                for (i = 1; i <= c; i++) x[i] = v[k, i]
                m = med(x, c)
                for (i = 1; i <= c; i++) { d = v[k, i] - m; x[i] = (d < 0 ? -d : d) }   # busybox awk: ?: must be parenthesised
                printf "%d %.1f %.1f\n", k, m, med(x, c)
            }
        }' "$STANDBY_STAT" || exit 1
        standby_fp | sed 's/^/fp /'
    } >"$STANDBY_BASE.tmp" && mv -f "$STANDBY_BASE.tmp" "$STANDBY_BASE" || return 1
    echo "基线已写入 $STANDBY_BASE（$n 行记录）"
    # tell the ledger (docs/LEDGER.md §4 calib): the core's long-term layer (u60-sentinel.md
    # §20); when it does not answer, the guard's spool as before (a file that survives a reboot)
    _cb=$(cat "$BOOT_ID_FILE" 2>/dev/null)
    case "$_cb" in '' | *[!0-9a-f-]*) return 0 ;; esac
    _cu=$(uptime_s)
    _ci=calib-$(echo "$_cb" | cut -c1-8)-$_cu
    $WGET -q -T 5 --post-data x -O - "$CALIB_URL?id=$_ci&rows=$n" 2>/dev/null | grep -q '"ok":true' && return 0
    mkdir -p "$LEDGER_DIR/spool" 2>/dev/null &&
        printf '%s\t%s\t%s\tcalib\n,"rows":%d\n' "$_ci" "$_cb" "$_cu" "$n" >"$LEDGER_DIR/spool/$_ci.tmp" &&
        dd if=/dev/null of="$LEDGER_DIR/spool/$_ci.tmp" conv=notrunc,fsync 2>/dev/null &&
        mv -f "$LEDGER_DIR/spool/$_ci.tmp" "$LEDGER_DIR/spool/$_ci.ev" && sync
    return 0
}

# prints "<ok|warn>\t<detail>"; "standby_check en": the detail in English.
# Only idle screen-off minutes are judged: a minute
# whose cellular packets exceed the baseline median + 3 MAD is someone using
# the network, and judging those raised false alarms (docs/LEDGER.md §12).
standby_check() {
    if [ ! -f "$STANDBY_BASE" ]; then
        if [ "$1" = en ]; then printf 'ok\tNot calibrated (run doctor.sh --calibrate-standby after 30-60 min with the screen off)\n'
        else printf 'ok\t未校准（屏幕熄灭 30~60 分钟后执行 doctor.sh --calibrate-standby）\n'; fi
        return
    fi
    _bv=
    { read -r _bv <"$STANDBY_BASE"; } 2>/dev/null
    if [ "$_bv" != v2 ]; then
        if [ "$1" = en ]; then printf 'ok\tOld baseline format, not judged; recalibrate at home while idle (doctor.sh --calibrate-standby)\n'
        else printf 'ok\t基线格式旧，不判；请在家自然空闲时重新校准（doctor.sh --calibrate-standby）\n'; fi
        return
    fi
    _since=$(cat "$FP_CHANGED" 2>/dev/null)
    case "$_since" in '' | *[!0-9]*) _since=0 ;; esac
    awk -v now="$(uptime_s)" -v since="$_since" -v labels="$standby_labels" -v base="$STANDBY_BASE" -v last="$(standby_last)" \
        -v fpnow="$(standby_fp | tr '\n' ';')" -v en="$1" -v labels_en="$standby_labels_en" '
        function sortv(a, c,   i, j, v) { for (i = 2; i <= c; i++) { v = a[i]; j = i - 1; while (j > 0 && a[j] > v) { a[j + 1] = a[j]; j-- } a[j + 1] = v } }
        function med(a, c) { sortv(a, c); return (c % 2) ? a[(c + 1) / 2] : (a[c / 2] + a[c / 2 + 1]) / 2 }
        BEGIN {
            if (en == "en") split(labels_en, lab, "|")
            else split(labels, lab, " ")
            while ((getline l < base) > 0) {
                split(l, f, " ")
                if (f[1] == "fp") bf[f[2]] = f[3]
                else if (f[1] != "v2") { bm[f[1]] = f[2]; bd[f[1]] = f[3] }
            }
            np = split(fpnow, pairs, ";")
            for (i = 1; i <= np; i++) if (pairs[i] != "") { split(pairs[i], q, " "); cf[q[1]] = q[2] }
            thr = ((bm[2] == "-" || bm[2] == "") ? -1 : bm[2] + 3 * bd[2])
        }
        $1 >= now - 900 && $1 > since {
            rows++
            if (thr < 0 || $2 == "-" || $2 == "" || $2 + 0 > thr) { busy++; next }
            idle++
            for (k = 2; k <= last; k++) if ($k != "-" && $k != "") v[k, ++cnt[k]] = $k
        }
        END {
            if (idle < 8) {
                if (en == "en") {
                    if (rows >= 8 && busy * 2 > rows) printf "ok\tNetwork in use, standby not judged (%d of %d rows in the last 15 min had traffic)\n", busy, rows
                    else printf "ok\tToo few idle screen-off rows in the last 15 min (%d), not judged\n", idle
                    exit
                }
                if (rows >= 8 && busy * 2 > rows) printf "ok\t有流量在用，不判待机（最近 15 分钟 %d 行里 %d 行有流量）\n", rows, busy
                else printf "ok\t最近 15 分钟空闲的息屏记录只有 %d 行，不判定\n", idle
                exit
            }
            out = ""; stale = ""
            for (k = 3; k <= last; k++) {
                if (bm[k] == "-" || bm[k] == "" || cf[k] == "retired") continue
                if (!(k in bf) || !(k in cf) || bf[k] != cf[k] || index(cf[k], "?") > 0) {
                    if (en == "en") stale = stale (stale == "" ? "" : ", ") lab[k]
                    else stale = stale (stale == "" ? "" : "、") lab[k]
                    continue
                }
                c = cnt[k] + 0; if (c == 0) continue
                for (i = 1; i <= c; i++) x[i] = v[k, i]
                m = med(x, c)
                if (m > bm[k] + 3 * bd[k] && m > 1.5 * bm[k] && m - bm[k] >= 1) {
                    if (en == "en") out = out (out == "" ? "" : "; ") sprintf("%s %.0f (baseline %.0f)", lab[k], m, bm[k])
                    else out = out (out == "" ? "" : "；") sprintf("%s %.0f（基线 %.0f）", lab[k], m, bm[k])
                }
            }
            c = cnt[2] + 0
            for (i = 1; i <= c; i++) x[i] = v[2, i]
            cell = med(x, c)
            if (en == "en") {
                tail = ((stale == "") ? "" : sprintf("; baseline out of date for %s, not judged", stale))
                if (out != "") printf "warn\t%s%s\n", out, tail
                else printf "ok\tNormal (cell %.0f pkts/min)%s\n", cell, tail
                exit
            }
            tail = ((stale == "") ? "" : sprintf("；%s 基线过期，不判", stale))
            if (out != "") printf "warn\t%s%s\n", out, tail
            else printf "ok\t正常（蜂窝每分钟 %.0f 个包）%s\n", cell, tail
        }' "$STANDBY_STAT" 2>/dev/null || {
        if [ "$1" = en ]; then printf 'ok\tNo screen-off records\n'; else printf 'ok\t没有息屏记录\n'; fi
    }
}

# ── the scorecard: doctor.sh --report 24h|7d [--summary] (docs/LEDGER.md §11) ──
# Reads the whole ledger and changes nothing. --summary prints the three lines
# u60-guard keeps in $LEDGER_DIR/summary for the default output.
REPORT_AWK='
# The scorecard (docs/LEDGER.md §11). Input: every ledger segment, in file
# name order. -v: W (window, s), LABEL, RECOV (recovery go-live, wall s),
# UIDTK (u60-uid takeover, wall s), CAPF (capture files: "<boot8> <ends in a
# crash line 0|1>"), WARNF (current health rows not ok), STBF (the standby
# check verdict), ALERTQ, GAVEUP (1 when the u60-uid give-up flag
# is set now), THR (standby idle threshold, pkts/min, or empty), SUMMARY (1:
# print the three summary lines only).
function fld(l, name,   p, v) { # a field of a flat JSON line; strings unquoted; "" when absent
    if (!match(l, "\"" name "\":(\"[^\"]*\"|[^,}]*)")) return ""
    p = length(name) + 3
    v = substr(l, RSTART + p, RLENGTH - p)
    if (v ~ /^"/) v = substr(v, 2, length(v) - 2)
    return v
}
function isn(x) { return (x ~ /^-?[0-9]+(\.[0-9]+)?$/) }
function idfrom(id,   n, a) { n = split(id, a, "-"); return a[n] + 0 } # h-/hp-/hq-<boot8>-<from>: where its hour began
function trim(x) { sub(/；$/, "", x); return x }
function wall(s, up) { return ((s in OFF) ? int(up) + OFF[s] : "") }
function dur(x,   d, h, m) { # seconds as 天/小时/分
    x = int(x); d = int(x / 86400); h = int((x % 86400) / 3600); m = int((x % 3600) / 60)
    if (d > 0) return d " 天" ((h > 0) ? " " h " 小时" : "")
    if (h > 0) return h " 小时" ((m > 0) ? " " m " 分" : "")
    return m " 分"
}
function when(x) { return ((x == "") ? "时间不明" : strftime("%m-%d %H:%M", x)) }
function place(net, home) { # MCC-MNC text; home when the countries agree
    if (net == "" || net == "null" || home == "" || home == "null") return "不明"
    return ((substr(net, 1, 3) == substr(home, 1, 3)) ? "在家" : "国外")
}
function planned(s) { return (B_PLAN[s] != "" && B_PLAN[s] != "null") }
function klass(code) {
    if (code == 1150 || code == 1152) return "manual"
    if (code == 1112) return "scheduled"
    if (code == 1134 || code == 1132) return "modem"
    if (code == 1185) return "noui"
    if (code == 1155) return "cutoff"
    if (code == 1182) return "1182"
    return "other"
}
function wstart(req, change,   s) { # the window start: requested length, the last relevant change, the start of the ledger
    s = WEND - ((req < W) ? req : W)
    if (change != "" && change > s) s = change
    if (LSTART != "" && LSTART > s) s = LSTART
    return s
}
# Coverage of [ws, WEND] for the sources in srcs: the awake seconds, the
# seconds they were not working (the g_* of hour lines, plus holes between boots
# that no power-off explains), and whether one gap passed 10 minutes.
function cov(ws, srcs,   n, sv, i, j, a, g, hs, he, f, gi, long, x) {
    n = split(srcs, sv, " "); a = 0; g = 0; long = 0
    for (i = 1; i <= NH; i++) {
        hs = HWS[i]; he = HWE[i]
        if (hs == "" || he <= ws || hs >= WEND) continue
        f = 1
        if (he > hs) { x = ((he < WEND) ? he : WEND) - ((hs > ws) ? hs : ws); f = x / (he - hs) }
        a += H_awake[i] * f
        gi = 0
        for (j = 1; j <= n; j++) { gi += H_g[i, sv[j]] * f; if (H_g[i, sv[j]] > 600) long = 1 }
        g += ((gi > H_awake[i] * f) ? H_awake[i] * f : gi)
    }
    for (i = 1; i <= NHOLE; i++) { # only the part inside the window counts
        x = ((HOLE_E[i] < WEND) ? HOLE_E[i] : WEND) - ((HOLE_S[i] > ws) ? HOLE_S[i] : ws)
        if (x <= 0) continue
        g += x; a += x
        if (x > 600) long = 1
    }
    for (i = 1; i <= NGAP; i++) {
        if (GAP_S[i] == "" || GAP_E[i] == "") continue
        x = ((GAP_E[i] < WEND) ? GAP_E[i] : WEND) - ((GAP_S[i] > ws) ? GAP_S[i] : ws)
        for (j = 1; j <= n; j++) if (GAP_SRC[i] == sv[j] && x > 600) long = 1
    }
    COV_A = a; COV_G = g
    if (a <= 0) return "没有记录"
    if (g > 0.05 * a || long) return sprintf("不完整（缺 %s / 醒着 %s%s）", dur(g), dur(a), (long ? "，有一段超过 10 分钟" : ""))
    return "完整"
}
# One line per metric: id, name, value, tier, window, coverage, how judged.
function row(id, name, val, tier, ws, req, cv, how) {
    if (SUMMARY) return
    printf "  %s %s：%s · %s", id, name, val, tier
    if (req > 0) printf " · %s / %s", dur(WEND - ws), dur(req)
    if (cv != "") printf " · 覆盖%s", cv
    printf " · %s\n", how
}
function sub_(text) { if (!SUMMARY) printf "      %s\n", text }
function tier(viol, cv, ws, req) {
    if (cv != "完整") return "没测"
    if (viol) return "不达标"
    if (WEND - ws < req - 60) return "注意"
    return "达标"
}
function p95(arr, n,   i, j, v, k) {
    for (i = 2; i <= n; i++) { v = arr[i]; j = i - 1; while (j > 0 && arr[j] > v) { arr[j + 1] = arr[j]; j-- } arr[j + 1] = v }
    k = int(n * 0.95); if (k < n * 0.95) k++; if (k < 1) k = 1
    return arr[k]
}
BEGIN {
    while ((getline l < CAPF) > 0) { split(l, x, " "); CAPEND[x[1]] = x[2] }
    split("guard job watcher capture uidlog crashlog datad", gs, " ")
    split("on_home on_abroad off_home off_abroad", cs, " ")
    split("agent datad devui mihomo ts", rs, " ")
}
{
    # each NUL ends a record: a run of them left by a power cut counts once a segment, not as lines
    if ($0 == "") { if (!(FILENAME in NULF)) { NULF[FILENAME] = 1; NULS++ } next }
    if ($0 !~ /^[{]"v":[0-9]+,"seq":[0-9]+,"n":[0-9]+,"up":[0-9.]+,"t":(null|[0-9]+),"k":"[a-z_]+"/) { BAD++; next }
    if (fld($0, "v") + 0 > 1) { NEWER++; next }
    id = fld($0, "id")
    if (id != "") { if (id in SEEN) next; SEEN[id] = 1 }
    k = fld($0, "k"); s = fld($0, "seq") + 0; up = fld($0, "up") + 0; t = fld($0, "t")
    if (!(s in SEQS)) { SEQS[s] = 1; NS++; SQ[NS] = s }
    if (t != "null") OFF[s] = t - int(up)
    if (k == "boot") { B_CODE[s] = fld($0, "code"); B_ID[s] = fld($0, "boot"); B_MODE[s] = fld($0, "mode"); B_PLAN[s] = fld($0, "planned") }
    else if (k == "boot_backfill") { NBF++; BF_SEQ[NBF] = s; BF_CODE[NBF] = fld($0, "code"); BF_AT[NBF] = fld($0, "at"); if (fld($0, "gap") == "1") BF_GAP[s] = 1 }
    else if (k == "guard_start") { GS_N[s]++; if (!(s in GS_UP)) GS_UP[s] = up; if (fld($0, "requested") != "1" && GS_N[s] > 1) { NGX++; GX_S[NGX] = s; GX_UP[NGX] = up } }
    else if (k == "ver") { p = fld($0, "prog"); m = fld($0, "md5"); if (m != "null" && (p in VER_M) && VER_M[p] != m) { VER_CS[p] = s; VER_CU[p] = up } if (m != "null") VER_M[p] = m; if (p == "zwrt-datad") DATAD_X = fld($0, "extra") }
    else if (k == "ssr") { NSSR++; SSR_ID[NSSR] = id; SSR_S[NSSR] = s; SSR_UP[NSSR] = up; SSR_NET[NSSR] = fld($0, "net"); SSR_HOME[NSSR] = fld($0, "home"); SSR_RAT[NSSR] = fld($0, "rat"); SSR_BAND[NSSR] = fld($0, "band"); SSR_NR[NSSR] = fld($0, "nrband"); SSR_PPM[NSSR] = fld($0, "wan_ppm"); LAST_SSR[s] = NSSR }
    else if (k == "ssr_result") { r = fld($0, "ssr"); RES[r] = fld($0, "result"); RES_REC[r] = fld($0, "recovered_s"); RES_RX[r] = fld($0, "rx_seen_s"); RES_RESUMED[r] = fld($0, "resumed"); RES_SLEPT[r] = fld($0, "slept") }
    else if (k == "svc_exit") { NSX++; SX_S[NSX] = s; SX_UP[NSX] = up; SX_P[NSX] = fld($0, "prog"); SX_ST[NSX] = fld($0, "status") }
    else if (k == "proc_restart") { NPR++; PR_S[NPR] = s; PR_UP[NPR] = up; PR_P[NPR] = fld($0, "prog"); PR_CL[NPR] = fld($0, "crashlog"); PR_SH[NPR] = fld($0, "ship") }
    else if (k == "uid") { if (fld($0, "what") == "gave_up") { NUG++; UG_S[NUG] = s; UG_UP[NUG] = up } }
    else if (k == "datad_degraded") { st = fld($0, "state"); sc = fld($0, "since")
        if (st == "start") { NDD++; DD_S[NDD] = s; DD_UP[NDD] = up; DD_SINCE[NDD] = sc; DDI[sc] = NDD }
        else if (sc in DDI) { DD_DUR[DDI[sc]] = fld($0, "dur_s"); DD_HOW[DDI[sc]] = fld($0, "how") } }
    else if (k == "gap") { NGAPR++; GR_S[NGAPR] = s; GR_SRC[NGAPR] = fld($0, "src"); GR_F[NGAPR] = fld($0, "from"); GR_T[NGAPR] = fld($0, "to") }
    else if (k == "calib") { CAL_S = s; CAL_UP = up }
    else if (k == "net_change") { NNC++; NC_S[NNC] = s; NC_UP[NNC] = up }
    else if (k == "oom") { NOOM++; OOM_S[NOOM] = s; OOM_UP[NOOM] = up }
    else if (k == "thermal") { NTH++; TH_S[NTH] = s; TH_UP[NTH] = up; TH_LV[NTH] = fld($0, "level") }
    else if (k == "hour") { NH++; H_S[NH] = s; H_F[NH] = fld($0, "from") + 0; H_T[NH] = fld($0, "to") + 0; H_FT[NH] = fld($0, "ft"); H_TT[NH] = fld($0, "tt")
        H_awake[NH] = fld($0, "awake") + 0; H_ab[NH] = fld($0, "abroad") + 0; H_NET[NH] = fld($0, "net"); H_HOME[NH] = fld($0, "home")
        for (j = 1; j <= 7; j++) H_g[NH, gs[j]] = fld($0, "g_" gs[j]) + 0
        HK[s, H_F[NH]] = NH
        if (isn(H_TT[NH])) OFF[s] = H_TT[NH] - H_T[NH] }
    else if (k == "hour_power") { i = HK[s, idfrom(id)]; if (i != "") { HP[i] = 1; HP_E[i] = fld($0, "e"); HP_PART[i] = fld($0, "e_partial"); HP_THR[i] = fld($0, "throttle_s")
        for (j = 1; j <= 4; j++) { HP_CS[i, cs[j]] = fld($0, cs[j] "_s") + 0; HP_CE[i, cs[j]] = fld($0, cs[j] "_e") } } }
    else if (k == "hour_proc") { i = HK[s, idfrom(id)]; if (i != "") { HQ[i] = 1; HQ_CPU[i] = fld($0, "cpu_datad")
        HQ_CON[i] = fld($0, "chill_on_s"); HQ_COK[i] = fld($0, "chill_ok_s")
        HQ_TON[i] = fld($0, "ts_on_s"); HQ_TCK[i] = fld($0, "ts_chk_s"); HQ_TOK[i] = fld($0, "ts_ok_s")
        for (j = 1; j <= 5; j++) HQ_RSS[i, rs[j]] = fld($0, "rss_" rs[j]) } }
}
END {
    # ── time: each hour wall span, the window end, the start of the ledger ──
    for (i = 1; i <= NH; i++) {
        s = H_S[i]
        HWS[i] = (isn(H_FT[i]) ? H_FT[i] + 0 : wall(s, H_F[i])); HWE[i] = (isn(H_TT[i]) ? H_TT[i] + 0 : wall(s, H_T[i]))
        if (HWE[i] != "" && (WEND == "" || HWE[i] > WEND)) WEND = HWE[i]
        if (HWS[i] != "" && (LSTART == "" || HWS[i] < LSTART)) LSTART = HWS[i]
        if (!(s in BLAST) || H_T[i] > H_T[BLAST[s]]) BLAST[s] = i
        if (!(s in BFIRST) || H_F[i] < H_F[BFIRST[s]]) BFIRST[s] = i
    }
    if (WEND == "") {
        if (NH) printf "账本有 %d 小时的记录，但设备时钟一直没对上，排不进时间窗口（对时以后再看）\n", NH
        else print "账本里还没有一小时的记录（上线一小时后再看）"
        exit 1
    }
    # holes between boots that no power-off explains: coverage gaps (§8)
    for (x = 1; x <= NS; x++) {
        s = SQ[x]; p = SQ[x - 1]
        if (x == 1 || !(s in BFIRST) || !(p in BLAST)) continue
        hs = HWE[BLAST[p]]; he = HWS[BFIRST[s]]
        if (hs == "" || he == "" || he - hs <= 300) continue
        # (s in B_CODE) first: a lookup creates the element, and a boot with no boot line
        # would then count as a reboot of unknown cause
        if (((s in B_CODE) && (klass(B_CODE[s]) == "manual" || planned(s))) || B_MODE[s] ~ /charg/) continue
        NHOLE++; HOLE_S[NHOLE] = hs; HOLE_E[NHOLE] = he
    }
    for (i = 1; i <= NGAPR; i++) { s = GR_S[i]; NGAP++; GAP_SRC[NGAP] = GR_SRC[i]; GAP_S[NGAP] = wall(s, GR_F[i]); GAP_E[NGAP] = wall(s, GR_T[i]) }
    # ── reboots, in order: each boot line and the key.log backfills written before it ──
    for (x = 1; x <= NS; x++) {
        s = SQ[x]
        for (i = 1; i <= NBF; i++) if (BF_SEQ[i] == s) {
            NR_++; R_CODE[NR_] = BF_CODE[i]; R_LEDGER[NR_] = 0
            at = BF_AT[i]; gsub(/[-:]/, " ", at)
            R_W[NR_] = ((substr(BF_AT[i], 1, 4) >= 2026) ? mktime(at) : ((s in OFF) ? OFF[s] - 1 : ""))
        }
        if (s in B_CODE) { NR_++; R_CODE[NR_] = B_CODE[s]; R_LEDGER[NR_] = s; R_W[NR_] = ((s in OFF) ? OFF[s] : "") }
    }
    for (i = 1; i <= NR_; i++) {
        c = klass(R_CODE[i]); pl = "不明"; ps = ((i > 1) ? R_LEDGER[i - 1] : 0)
        # asked for (web, touch screen, plan-reboot): CN B31 logs a deliberate reboot as 1134 too
        if (R_LEDGER[i] != 0 && planned(R_LEDGER[i])) c = "manual"
        if (ps && (ps in BLAST)) pl = ((H_ab[BLAST[ps]] * 2 > H_awake[BLAST[ps]]) ? "国外" : "在家")
        if (c == "1182") {
            c = "unknown"
            if (ps) {
                j = LAST_SSR[ps]
                if (j != "" && !(SSR_ID[j] in RES)) c = "modem"
                else if (substr(B_ID[ps], 1, 8) in CAPEND && CAPEND[substr(B_ID[ps], 1, 8)] == 1) c = "modem"
            }
        }
        R_CLASS[i] = c; R_PLACE[i] = pl
        R_GAPCOL[i] = 0
        if (c == "modem") { # before guard was up in the boot that ended: the S2a gap column
            if (!ps || !(ps in GS_UP)) R_GAPCOL[i] = 1
            else { j = LAST_SSR[ps]; if (j != "" && SSR_UP[j] < GS_UP[ps]) R_GAPCOL[i] = 1 }
        }
    }
    if (!SUMMARY) {
        printf "设备成绩单（%s，截至 %s）\n", LABEL, strftime("%Y-%m-%d %H:%M", WEND)
        printf "  账本：%d 次开机，%d 小时的记录，从 %s 起%s%s\n\n", NS, NH, strftime("%m-%d %H:%M", LSTART),
            (NULS ? sprintf("；%d 段有断电留下的空字节，已跳过", NULS) : "") (BAD ? sprintf("；%d 行读不懂，已跳过", BAD) : ""), (NEWER ? sprintf("；%d 行是更新的格式，已跳过", NEWER) : "")
        print "稳"
    }
    D7 = 604800; D1 = 86400
    # S1 unexpected reboots (key.log reason codes)
    ws = wstart(D7, RECOV); n = 0; sch = 0; man = 0; det = ""
    for (i = 1; i <= NR_; i++) if (R_W[i] != "" && R_W[i] > ws && R_W[i] <= WEND) {
        if (R_CLASS[i] == "scheduled") sch++
        else if (R_CLASS[i] == "manual") man++
        else { n++; det = det sprintf("%s %s（%s，%s）；", when(R_W[i]), R_CODE[i], R_CLASS[i], R_PLACE[i]) }
    }
    cv = "完整"; for (x = 1; x <= NS; x++) if (BF_GAP[SQ[x]]) cv = "不完整（key.log 补记的锚点丢过一次）"
    S1N = n; S1T = tier(n > 0, cv, ws, D7)
    row("S1", "意外整机重启", n " 次" ((sch || man) ? "（另有" (sch ? sprintf("原厂定时 %d 次", sch) : "") ((sch && man) ? "、" : "") (man ? sprintf("人为 %d 次", man) : "") "）" : ""), S1T, ws, D7, cv, "自动")
    if (det != "") sub_(det)
    # S2a modem crash -> whole reboot, from guard up; the boot-gap column apart
    ws = wstart(D7, RECOV); n = 0; ng = 0
    for (i = 1; i <= NR_; i++) if (R_W[i] != "" && R_W[i] > ws && R_W[i] <= WEND && R_CLASS[i] == "modem") { if (R_GAPCOL[i]) ng++; else n++ }
    cv = cov(ws, "watcher capture")
    row("S2a", "基带崩溃升级成整机重启", n " 次（开机空档 " ng " 次）", tier(n > 0, cv, ws, D7), ws, D7, cv, "自动")
    # S2b recovery time P95, from the ssr_result lines; S3 counts
    lastnc = ""; for (i = 1; i <= NNC; i++) { x = wall(NC_S[i], NC_UP[i]); if (x != "" && (lastnc == "" || x > lastnc)) lastnc = x }
    ws = wstart(D7, lastnc)
    # crashes in the order they happened (spool files are drained by name, not by time)
    for (i = 1; i <= NSSR; i++) ORD[i] = i
    for (i = 2; i <= NSSR; i++) { v = ORD[i]; j = i - 1
        while (j > 0 && (SSR_S[ORD[j]] > SSR_S[v] || (SSR_S[ORD[j]] == SSR_S[v] && SSR_UP[ORD[j]] > SSR_UP[v]))) { ORD[j + 1] = ORD[j]; j-- }
        ORD[j + 1] = v }
    nh = 0; na = 0; nsl = 0; nnd = 0
    for (z = 1; z <= NSSR; z++) {
        i = ORD[z]
        x = wall(SSR_S[i], SSR_UP[i]); if (x == "" || x <= ws || x > WEND) continue
        r = SSR_ID[i]; pl = place(SSR_NET[i], SSR_HOME[i])
        # before any RES[r]: a lookup creates the element, and the check would never fire
        if (!(r in RES) && SSR_S[i] == SQ[NS]) continue # still being followed
        if (RES[r] == "no_drop") { nnd++; continue }
        if (RES[r] == "merged") continue # its chain is counted at the crash that ends it
        v = ""
        if (RES[r] == "ok") v = RES_REC[r] + 0
        else v = 600 # timeout, or the device rebooted first: at least 600 s
        # merged crashes just before this one: one long outage from the first of them
        sl_ = (RES_SLEPT[r] == "1")
        y = z - 1; while (y >= 1 && SSR_S[ORD[y]] == SSR_S[i] && RES[SSR_ID[ORD[y]]] == "merged") {
            v += SSR_UP[ORD[y + 1]] - SSR_UP[ORD[y]]; if (RES_SLEPT[SSR_ID[ORD[y]]] == "1") sl_ = 1; y-- }
        if (sl_) { nsl++; continue } # asleep anywhere in the chain: kept apart, never for 达标
        if (pl == "国外") { na++; PA[na] = v } else { nh++; PH[nh] = v }
        b_ = ((THR == "" || !isn(SSR_PPM[i])) ? "不明" : ((SSR_PPM[i] + 0 <= THR + 0) ? "空闲" : "有流量"))
        NB[pl "·" b_]++; BV[pl "·" b_, NB[pl "·" b_]] = v
    }
    cv = cov(ws, "watcher capture")
    ph = (nh ? p95(PH, nh) : ""); pa = (na ? p95(PA, na) : "")
    val = "在家 " (nh ? sprintf("P95 %d 秒（%d 次）", ph, nh) : "没有崩溃") "；国外 " (na ? sprintf("P95 %d 秒（%d 次）", pa, na) : "没有崩溃")
    if (nnd) val = val sprintf("；没断网 %d 次另列", nnd)
    if (nsl) val = val sprintf("；跟随中睡过 %d 次另列", nsl)
    S2BT = tier((nh && ph > 45) || (na && pa > 45), cv, ws, D7)
    if (S2BT == "达标" && nsl && !nh && !na) S2BT = "注意"
    row("S2b", "基带崩溃后恢复用时", val, S2BT, ws, D7, cv, "自动")
    det = ""; for (x in NB) { delete TMP; for (j = 1; j <= NB[x]; j++) TMP[j] = BV[x, j]; det = det sprintf("%s P95 %d 秒（%d 次）；", x, p95(TMP, NB[x]), NB[x]) }
    if (det != "") sub_(det)
    nh = 0; det = ""; delete CNT
    for (i = 1; i <= NSSR; i++) {
        x = wall(SSR_S[i], SSR_UP[i]); if (x == "" || x <= ws || x > WEND) continue
        if (place(SSR_NET[i], SSR_HOME[i]) == "国外") CNT[SSR_NET[i] " " SSR_RAT[i] " " SSR_BAND[i] ((SSR_NR[i] != "null" && SSR_NR[i] != "") ? "/n" SSR_NR[i] : "")]++
        else nh++
    }
    na = 0; for (x in CNT) { na += CNT[x]; det = det x " " CNT[x] " 次；" }
    S3N = nh + na
    row("S3", "基带崩溃（按国家、制式、频段）", "在家 " nh " 次；国外 " na " 次", tier(nh > 0, cv, ws, D7), ws, D7, cv, (na ? "在家：自动；国外有没有对策：人工" : "自动"))
    if (det != "") sub_("国外：" det)
    # S4 our programs, each from its last new build
    split("zte-agent zwrt-datad u60pro-devui u60-uid u60-guard.sh", PG, " ")
    split("agent datad 触屏 u60-uid guard", PL, " ")
    val = ""; viol = 0; worst = "达标"; cvall = "完整"
    for (q = 1; q <= 5; q++) {
        p = PG[q]; ch = ((p in VER_CS) ? wall(VER_CS[p], VER_CU[p]) : "")
        ws = wstart(D7, ch); n = 0; pre = 0
        for (i = 1; i <= NSX; i++) if (SX_P[i] == p && SX_ST[i] != "0" && SX_ST[i] != "exit 0") { x = wall(SX_S[i], SX_UP[i]); if (x != "" && x > ws && x <= WEND) n++; else if (x != "") pre++ }
        for (i = 1; i <= NPR; i++) if (PR_P[i] == p && PR_CL[i] != "1" && PR_SH[i] != "1") { x = wall(PR_S[i], PR_UP[i]); if (x != "" && x > ws && x <= WEND) n++ }
        if (p == "u60-guard.sh") for (i = 1; i <= NGX; i++) { x = wall(GX_S[i], GX_UP[i]); if (x != "" && x > ws && x <= WEND) n++ }
        cv = cov(ws, "crashlog guard"); t_ = tier(n > 0, cv, ws, D7)
        val = val PL[q] " " n ((t_ != "达标") ? "（" t_ ((t_ == "注意") ? " " dur(WEND - ws) : "") "）" : "") "；"
        if (t_ == "不达标") worst = t_; else if (t_ == "没测" && worst != "不达标") worst = t_; else if (t_ == "注意" && worst == "达标") worst = t_
    }
    row("S4", "我们的程序意外退出", trim(val), worst, 0, 0, "", "自动；上机事务里的重启不算（10-04 起记）；手工重启仍算；退出码 0 的意外退出第二步才看得到")
    # S5a / S5b the screen
    ws = wstart(D7, UIDTK); n = 0
    for (i = 1; i <= NR_; i++) if (R_W[i] != "" && R_W[i] > ws && R_W[i] <= WEND && R_CODE[i] == 1185) n++
    row("S5a", "屏幕没界面导致重启", n " 次", tier(n > 0, "完整", ws, D7), ws, D7, "完整", "自动")
    n = 0; for (i = 1; i <= NUG; i++) { x = wall(UG_S[i], UG_UP[i]); if (x != "" && x > ws && x <= WEND) n++ }
    cv = cov(ws, "uidlog")
    row("S5b", "屏幕交还原厂界面", n " 次" (GAVEUP ? "（现在仍停在原厂界面）" : ""), tier(n > 0 || GAVEUP, cv, ws, D7), ws, D7, cv, "自动")
    # S6 datad degraded (one day)
    ws = wstart(D1, ""); n = 0; tot = 0; unk = 0
    for (i = 1; i <= NDD; i++) { x = wall(DD_S[i], DD_UP[i]); if (x != "" && x > ws && x <= WEND) { n++; if (isn(DD_DUR[i])) tot += DD_DUR[i]; else unk++ } }
    cv = cov(ws, "guard datad")
    S6N = n
    row("S6", "datad 降级", n " 次" (n ? "，共 " dur(tot) (unk ? sprintf("（%d 次时长不明）", unk) : "") : ""), tier(n > 0, cv, ws, D1), ws, D1, cv, "自动")
    row("S7", "上机安全和一致", "设备上的程序还没有全部对得上提交", "不达标", 0, 0, "", "E1 前靠已知事实")
    # S8 exits. Tailscale: healthy seconds over the seconds guard could check it;
    # an hour line without ts_chk_s is from before that check, all of it unchecked.
    ws = wstart(D7, ""); con = 0; cok = 0; ton = 0; tck = 0; tok = 0; tlong = 0
    for (i = 1; i <= NH; i++) if (HWE[i] > ws && HWS[i] < WEND && (i in HQ)) {
        con += HQ_CON[i]; cok += HQ_COK[i]; ton += HQ_TON[i]
        x = (isn(HQ_TCK[i]) ? HQ_TCK[i] + 0 : 0); tck += x
        if (isn(HQ_TCK[i]) && isn(HQ_TOK[i])) tok += HQ_TOK[i]
        if (HQ_TON[i] - x > 600) tlong = 1
    }
    cv = cov(ws, "guard")
    # the coverage of Tailscale itself (docs/LEDGER.md §8): unchecked on-time <= 5%, and no hour with over 10 minutes of it
    if (cv == "完整" && ton && (ton - tck > 0.05 * ton || tlong))
        cv = sprintf("不完整（Tailscale 开着 %s，查到 %s%s）", dur(ton), dur(tck), (tlong ? "，有一小时里超过 10 分钟没查到" : ""))
    tv = (ton ? (tck ? sprintf("%.1f%%", tok * 100 / tck) : "没查到过") : "没开过")
    tbad = (ton && tok + ton - tck < 0.99 * ton) # under 99% even if every unchecked second was healthy
    cbad = (con && cok < 0.99 * con)
    val = "CHILL " (con ? sprintf("%.1f%%", cok * 100 / con) : "没开过") "；Tailscale " tv
    t_ = tier(cbad || (tck && tok < 0.99 * tck), ((con || ton) ? cv : "没有记录"), ws, D7)
    if (cbad || tbad) t_ = "不达标"
    row("S8", "出口可用", val, t_, ws, D7, cv, "自动")
    # ── 好用 ──
    if (!SUMMARY) print "\n好用"
    row("U1", "点了当场有反应", "首页每秒同步请求会卡几十到上百 ms，超过 50 ms", "不达标", 0, 0, "", "第二步前靠已知事实")
    row("U2", "屏幕说的是真的", "崩溃后照常显示旧值；缺 SINR 当「中」；WAN 为空当「已连上」", "不达标", 0, 0, "", "第二步前靠已知事实")
    row("U3", "出事说得清", "基带崩溃时屏幕没有一句解释", "不达标", 0, 0, "", "第二步前靠已知事实")
    row("U4", "操作闭环", "原厂界面切制式没生效也不报错", "不达标", 0, 0, "", "E4 前靠已知事实")
    row("U5", "不用 ssh", "出行中不得不开会话修", "不达标", 0, 0, "", "设备锁做好前靠已知事实")
    # U6 alerts in the window, and the ▲ of the health check now
    ws = wstart(D7, ""); n = 0; det = ""; oldest = ""; firstseq = ""
    while ((getline l < ALERTQ) > 0) { split(l, a, "\t"); if (firstseq == "") { firstseq = a[1]; oldest = a[2] }
        if (a[4] == "sms-test") continue
        if (a[2] + 0 > ws && a[2] + 0 <= WEND) { n++; det = det sprintf("%s %s；", when(a[2] + 0), a[4]) } }
    nw = 0; wdet = ""; while ((getline l < WARNF) > 0) { nw++; wdet = wdet l "；" }
    cv = ((firstseq != "" && firstseq > 1 && oldest + 0 > ws) ? "不完整（告警队列剪过，最老一条晚于窗口起点）" : "完整")
    row("U6", "体检和告警可信", sprintf("窗口内告警 %d 条，现在 ▲/■ %d 项", n, nw), ((cv == "完整") ? "人工" : "没测"), ws, D7, cv, "人工：逐条看是不是真问题")
    if (det != "") sub_("告警：" det)
    if (wdet != "") sub_("体检：" wdet)
    # ── 省 ──
    if (!SUMMARY) print "\n省"
    ws = wstart(D7, ""); e = 0; es = 0; part = 0; delete CE; delete CSS
    for (i = 1; i <= NH; i++) if (HWE[i] > ws && HWS[i] < WEND && (i in HP) && isn(HP_E[i])) {
        e += HP_E[i]; if (HP_PART[i] == "1") part++
        for (j = 1; j <= 4; j++) { c = cs[j]; CSS[c] += HP_CS[i, c]; if (isn(HP_CE[i, c])) CE[c] += HP_CE[i, c] } }
    for (j = 1; j <= 4; j++) es += CSS[cs[j]]
    cv = cov(ws, "guard")
    P1V = (es ? sprintf("%.0f mW", e * 3600 / es) : "")
    row("P1", "真实使用的续航", (es ? "平均 " P1V "（" dur(es) " 的样本" (part ? sprintf("，%d 小时只有部分能量", part) : "") "）" : "没有能量记录"), "没测（目标待定）", ws, D7, cv, "自动")
    val = ""; split("亮屏在家 亮屏国外 息屏在家 息屏国外", CL, " ")
    for (j = 1; j <= 4; j++) { c = cs[j]; val = val CL[j] " " ((CSS[c] >= 21600) ? sprintf("%.0f mW", CE[c] * 3600 / CSS[c]) : "样本不足") "；" }
    row("P2", "同类场景的耗电", trim(val), "没测（目标待定）", ws, D7, cv, "自动；每格少于 6 小时写样本不足；小时记录不分空闲和有流量")
    getline stb < STBF; split(stb, sb, "\t")
    t_ = ((sb[2] ~ /未校准|没有息屏记录/) ? "没测" : ((sb[2] ~ /格式旧|基线过期/) ? "注意" : "达标"))
    row("P3", "待机判断正确", sb[2], t_, 0, 0, "", "自动（只判空闲的息屏分钟，基线过期的列停判）")
    ch = (("zwrt-datad" in VER_CS) ? wall(VER_CS["zwrt-datad"], VER_CU["zwrt-datad"]) : "")
    ws = wstart(D1, ch); cpu = 0; nc = 0
    for (i = 1; i <= NH; i++) if (HWE[i] > ws && HWS[i] < WEND && (i in HQ) && isn(HQ_CPU[i])) { cpu += HQ_CPU[i]; nc++ }
    cv = cov(ws, "guard")
    row("P4a", "datad 的 CPU", (nc ? sprintf("平均 %.1f%%（%d 小时）", cpu / nc / 10, nc) : "没有记录"), tier(nc && cpu / nc > 20, (nc ? cv : "没有记录"), ws, D1), ws, D1, cv, "自动")
    p4b = (DATAD_X == "ubus=socket" || DATAD_X == "ubus=auto")
    row("P4b", "读数不派生进程", (p4b ? ((DATAD_X == "ubus=auto") ? "socket 后端（auto：连不上 ubusd 才临时退回 CLI）" : "socket 后端") : "cli 后端，每读一次起一个 ubus 进程"), (p4b ? "达标" : "不达标"), 0, 0, "", "设计事实")
    # P5 memory over boots that ran 24 h; oom kills
    ws = wstart(D7, ""); val = ""; viol = 0; long_ = 0; cmp_ = 0
    for (x = 1; x <= NS; x++) { s = SQ[x]; if (!(s in BLAST) || H_T[BLAST[s]] < 86400) continue
        long_ = 1; b = ""
        for (i = 1; i <= NH; i++) if (H_S[i] == s && H_T[i] >= 3600 && (i in HQ) && (b == "" || H_F[i] < H_F[b])) b = i
        l_ = BLAST[s]; if (b == "" || !(l_ in HQ)) continue
        for (j = 1; j <= 5; j++) { r_ = rs[j]; if (isn(HQ_RSS[b, r_]) && isn(HQ_RSS[l_, r_]) && HQ_RSS[b, r_] + 0 > 0) { cmp_++
            g_ = (HQ_RSS[l_, r_] - HQ_RSS[b, r_]) * 100 / HQ_RSS[b, r_]; if (g_ > 10) { viol = 1; val = val sprintf("%s +%.0f%%；", r_, g_) } } } }
    n = 0; for (i = 1; i <= NOOM; i++) { x = wall(OOM_S[i], OOM_UP[i]); if (x != "" && x > ws && x <= WEND) n++ }
    cv = cov(ws, "guard watcher")
    m_ = ((!long_) ? "没有连续运行满 24 小时的开机" : ((!cmp_) ? "没有内存记录" : ((val != "") ? trim(val) : "RSS 增长都在 10% 以内")))
    row("P5", "内存不涨", m_ "；OOM " n " 次", tier(viol || n > 0, ((long_ && cmp_) ? cv : "没有记录"), ws, D7), ws, D7, cv, "自动")
    # P6 throttling and thermal level rises
    thr = 0; nul = 0
    for (i = 1; i <= NH; i++) if (HWE[i] > ws && HWS[i] < WEND && (i in HP)) { if (isn(HP_THR[i])) thr += HP_THR[i]; else nul++ }
    n = 0; delete FL
    for (i = 1; i <= NTH; i++) { s = TH_S[i]; lv = TH_LV[i]; v = ((lv ~ /^0x/) ? strtonum_(lv) : ""); if (v == "") continue
        if (!(s in FL)) { FL[s] = v; continue }
        x = wall(s, TH_UP[i]); if (v > FL[s] && x != "" && x > ws && x <= WEND) n++ }
    cv = cov(ws, "guard watcher"); if (nul) cv = sprintf("不完整（%d 小时没有降频数据）", nul)
    row("P6", "温度和降频", "降频 " dur(thr) "；过热等级上升 " n " 次", tier(thr > 0 || n > 0, cv, ws, D7), ws, D7, cv, "自动")
    if (SUMMARY) {
        printf "  账本（%s，截至 %s）：意外重启 %d 次（%s）、基带崩溃 %d 次、datad 降级今天 %d 次\n", LABEL, strftime("%m-%d %H:%M", WEND), S1N, S1T, S3N, S6N
        printf "  耗电：%s\n", ((P1V != "") ? "平均 " P1V : "还没有能量记录")
        printf "  详细成绩单：doctor.sh --report 7d\n"
    }
}
function strtonum_(h,   i, c, v) { v = 0; h = tolower(substr(h, 3)); for (i = 1; i <= length(h); i++) { c = index("0123456789abcdef", substr(h, i, 1)); if (!c) return ""; v = v * 16 + c - 1 } return v }

'
# The core's scorecard (u60d /v2/scorecard, u60-sentinel.md §19 T9b): the same card over the
# same ledger, with this run's health check as the request body (the first line standby_check,
# then the rows that are not ok). The exit code comes as the last line (busybox wget sees no
# status). Prints the card and sets _rc; false when there is no card to be had.
report_core() { # <24h|7d>
    { printf '%s\n' "$(head -n 1 "$_rt/standby")"; cat "$_rt/warn"; } >"$_rt/body"
    $WGET -q -T "$SCORECARD_WAIT" --post-file "$_rt/body" -O - "$SCORECARD_URL?w=$1&summary=$_sum" >"$_rt/resp" 2>/dev/null || return 1
    _rc=$(tail -n 1 "$_rt/resp" | sed -n 's/^rc=\([0-9][0-9]*\)$/\1/p')
    [ -n "$_rc" ] || return 1
    sed '$d' "$_rt/resp"
}
report_ledger() { # <24h|7d> [--summary]
    case "$1" in
        24h) _rw=86400 _rl="24 小时" ;;
        7d) _rw=604800 _rl="7 天" ;;
        *) echo "用法：doctor.sh --report 24h|7d"; return 2 ;;
    esac
    _sum=0
    [ "$2" = --summary ] && _sum=1
    # DOC_REPORT_KEEP=<dir>: also leave there everything the scorecard read and what it said,
    # as one fixture for the Rust port (u60-sentinel.md §19 T9a, tools/scorecard-parity.sh)
    _keep=${DOC_REPORT_KEEP:-}
    # DOC_REPORT_SRC: auto = the core's card, else worked out here; local; core = no fallback
    _src=${DOC_REPORT_SRC:-auto}
    [ -z "$_keep" ] || _src=local
    _rt=$(mktemp -d 2>/dev/null) || _rt=/tmp/doctor-report.$$
    mkdir -p "$_rt"
    : >"$_rt/warn"
    [ "$_sum" = 1 ] || sh "$0" --tsv 2>/dev/null | awk -F'\t' '$1 != "ok" { print $3 "：" $4 }' >"$_rt/warn"
    standby_check >"$_rt/standby"
    if [ "$_src" != local ]; then
        if report_core "$1"; then
            rm -rf "$_rt"
            return "$_rc"
        fi
        if [ "$_src" = core ]; then
            echo "核心的成绩单读不到（$SCORECARD_URL）"
            rm -rf "$_rt"
            return 2
        fi
    fi
    _segs=$(ls "$LEDGER_DIR"/boot-*.jsonl 2>/dev/null | sort)
    if [ -n "$_keep" ]; then
        mkdir -p "$_keep/ledger"
        printf 'W=%s\nLABEL=%s\nRECOV=%s\nUIDTK=%s\nSUMMARY=%s\nLEDGER_DIR=%s\n' "$_rw" "$_rl" "$RECOV_LIVE" "$UID_LIVE" "$_sum" "$LEDGER_DIR" >"$_keep/vars"
        [ -z "$_segs" ] || cp $_segs "$_keep/ledger/"
    fi
    if [ -z "$_segs" ]; then
        echo "账本还没有记录（$LEDGER_DIR）" | { [ -n "$_keep" ] && tee "$_keep/out" || cat; }
        [ -z "$_keep" ] || echo 1 >"$_keep/rc"
        rm -rf "$_rt"
        return 1
    fi
    # 1182 needs this: does each kept kernel log end in a modem crash line?
    for _cf in "$CRASHCAP_DIR"/kmsg-*.log; do
        [ -f "$_cf" ] || continue
        _cb=${_cf##*/kmsg-}
        printf '%s %s\n' "${_cb%.log}" "$(tail -n 40 "$_cf" | grep '^[0-9][0-9]*,[0-9][0-9]*,' | tail -n 1 |
            grep -c -i -E 'fatal error received|watchdog received|crash detected|rproc recovery|subsystem restart|err_fatal')"
    done >"$_rt/cap"
    _thr=$(awk '$1 == "2" && $2 != "-" { print $2 + 3 * $3; exit }' "$STANDBY_BASE" 2>/dev/null)
    _gave=0
    [ -f "$UID_STATE/gave-up" ] && _gave=1
    awk -v W="$_rw" -v LABEL="$_rl" -v RECOV="$RECOV_LIVE" -v UIDTK="$UID_LIVE" -v CAPF="$_rt/cap" -v WARNF="$_rt/warn" \
        -v STBF="$_rt/standby" -v ALERTQ="$ALERTS/queue" -v GAVEUP="$_gave" -v THR="$_thr" -v SUMMARY="$_sum" \
        "$REPORT_AWK" $_segs >"$_rt/out"
    _rc=$?
    cat "$_rt/out"
    if [ -n "$_keep" ]; then
        printf 'GAVEUP=%s\nTHR=%s\n' "$_gave" "$_thr" >>"$_keep/vars"
        cp "$_rt/cap" "$_rt/warn" "$_rt/standby" "$_rt/out" "$_keep/"
        [ ! -f "$ALERTS/queue" ] || cp "$ALERTS/queue" "$_keep/alerts"
        echo "$_rc" >"$_keep/rc"
    fi
    rm -rf "$_rt"
    return $_rc
}

# ── doctor.sh --ledger-selftest (docs/LEDGER.md §13) ──
# Whether the ledger can work on this device, item by item, PASS or FAIL.
# Touches neither the ledger nor the recovery switch, starts no reader or
# watcher; writes only $LEDGER_DIR/.selftest (removed again) and one test line
# to the kernel log. Also tries each device tool u60-guard relies on, the way
# u60-guard calls it, so a device build that lacks one shows up here first.
selftest() {
    _sf=0
    st() { # st <PASS|FAIL> <item> [<why>]
        [ "$1" = FAIL ] && _sf=$((_sf + 1))
        printf '%s %s%s\n' "$1" "$2" "${3:+：$3}"
    }
    _sx=$(awk 'BEGIN { srand(); printf "%d", rand() * 1000000000 }')
    # /data/ledger takes a 512-byte line, fsynced, and gives it back
    _sl=$(printf '%0512d' 0 | tr 0 x)
    if mkdir -p "$LEDGER_DIR" 2>/dev/null && printf '%s\n' "$_sl" >"$LEDGER_DIR/.selftest" 2>/dev/null &&
        dd if=/dev/null of="$LEDGER_DIR/.selftest" conv=notrunc,fsync 2>/dev/null && [ "$(cat "$LEDGER_DIR/.selftest")" = "$_sl" ]; then
        st PASS "账本目录可写（$LEDGER_DIR）"
    else
        st FAIL "账本目录可写（$LEDGER_DIR）" "写不进、fsync 失败或读回不一致"
    fi
    rm -f "$LEDGER_DIR/.selftest"
    # key.log.0 too: the firmware rotates key.log, and right after a rotation
    # this boot's code is only in the older file
    _sc=$(cat "$KEYLOG.0" "$KEYLOG" 2>/dev/null | grep 'reboot_reason_code=[0-9]' | tail -n 1 | sed -n 's/.*reboot_reason_code=\([0-9][0-9]*\).*/\1/p')
    if [ -n "$_sc" ]; then st PASS "key.log 的原因码（这次开机 $_sc）"; else st FAIL "key.log 的原因码" "$KEYLOG 和 $KEYLOG.0 里都找不到 reboot_reason_code="; fi
    _sr=
    { read -r _sr <"$MSS_RECOVERY"; } 2>/dev/null
    if [ -n "$_sr" ]; then st PASS "基带崩溃自恢复开关（$_sr）"; else st FAIL "基带崩溃自恢复开关" "读不到 $MSS_RECOVERY"; fi
    # device tools, called the way u60-guard calls them
    _st=$(mktemp -d 2>/dev/null) || _st=/tmp/doctor-selftest.$$
    mkdir -p "$_st"
    printf 'abcdef\n' >"$_st/f"
    [ "$(dd if="$_st/f" iflag=skip_bytes skip=2 bs=65536 2>/dev/null)" = cdef ] && st PASS "dd iflag=skip_bytes" || st FAIL "dd iflag=skip_bytes" "观察循环读不了落盘文件"
    set -- $(ls -ln "$_st/f" 2>/dev/null)
    [ "$5" = 7 ] && st PASS "ls -ln 第 5 列是大小" || st FAIL "ls -ln 第 5 列是大小" "读到 ${5:-空}"
    case "$(awk 'BEGIN { print mktime("2026 01 01 00 00 00"), strftime("%Y", 1790500000) }' 2>/dev/null)" in
        [0-9]*" 2026") st PASS "awk 的 mktime / strftime" ;;
        *) st FAIL "awk 的 mktime / strftime" "成绩单没法换算时间" ;;
    esac
    [ "$(printf '1\n' | awk '{ v = (($1 == 1) ? "yes" : "no"); print v }')" = yes ] && st PASS "awk 的 ?:（括起来的写法）" || st FAIL "awk 的 ?:（括起来的写法）"
    (exec 7>>"$_st/lock" && flock -n 7) && st PASS "flock -n" || st FAIL "flock -n" "账本写锁和观察循环的锁都用它"
    case "$(echo '{"a":{"b":1}}' | $JSONFILTER -e 'X=@.a.b' -e 'Y=@.a.c' -e 'Z=@.a.b' 2>/dev/null)" in
        *X=*Z=*) st PASS "jsonfilter 多个 -e（中间一个路径不存在）" ;;
        *) st FAIL "jsonfilter 多个 -e（中间一个路径不存在）" "网络缓存读不全" ;;
    esac
    case "$($WGET -q -T 2 -O - "$DATAD_URL" 2>/dev/null | head -c 1)" in
        "{") st PASS "wget -T 2 读 datad /v2/state" ;;
        *) st FAIL "wget -T 2 读 datad /v2/state" "网络缓存没有来源（datad 没在跑也会这样）" ;;
    esac
    rm -rf "$_st"
    # what the crash watcher calls the link up (S2b): an IPv4 address and the main table's default route
    case "$($IP -4 -o addr show dev "$WAN_IF" 2>/dev/null)" in
        *"inet "*) st PASS "ip -4 -o addr 看得到 $WAN_IF 的地址" ;;
        *) st FAIL "ip -4 -o addr 看得到 $WAN_IF 的地址" "崩溃跟随判不出恢复（此刻没联网也会这样）" ;;
    esac
    if awk -v i="$WAN_IF" '$1 == i && $2 == "00000000" { f = 1 } END { exit !f }' "$ROUTE" 2>/dev/null; then
        st PASS "默认路由走 $WAN_IF（$ROUTE）"
    else
        st FAIL "默认路由走 $WAN_IF（$ROUTE）" "崩溃跟随只能看地址（此刻没联网也会这样）"
    fi
    # a test line through /dev/kmsg shows up in this boot's capture file within the wait
    _sb=$(cut -c1-8 "$BOOT_ID_FILE" 2>/dev/null)
    _scf=$CRASHCAP_DIR/kmsg-${_sb:-boot}.log
    if [ ! -f "$_scf" ]; then
        st FAIL "kmsg 落盘" "没有这次开机的落盘文件 $_scf"
    elif ! echo "u60-ledger-selftest $_sx" >>"$KMSG" 2>/dev/null; then
        st FAIL "kmsg 落盘" "写不进 $KMSG"
    else
        _sw=0
        while [ "$_sw" -le "$SELFTEST_WAIT" ] && ! grep -q "u60-ledger-selftest $_sx" "$_scf" 2>/dev/null; do
            sleep 1
            _sw=$((_sw + 1))
        done
        if ! grep -q "u60-ledger-selftest $_sx" "$_scf" 2>/dev/null; then
            st FAIL "kmsg 落盘" "测试行 $SELFTEST_WAIT 秒内没出现在 $_scf"
        else
            st PASS "kmsg 落盘（${_sw} 秒内）"
            # the guard's watcher, when it runs, has read past it (its pid file goes when it is
            # stopped or handed to the core, its position file stays: u60-sentinel.md §20)
            _swp=
            [ -f "$LEDGER_TMP/watcher.pid" ] && { read -r _swp _rest <"$LEDGER_TMP/w.pos"; } 2>/dev/null
            if [ -n "$_swp" ]; then
                _sat=$(awk -v m="u60-ledger-selftest $_sx" 'index($0, m) { print b + 0; exit } { b += length($0) + 1 }' "$_scf")
                _sw=0
                while [ "$_sw" -le "$SELFTEST_WAIT" ] && [ "${_swp:-0}" -le "${_sat:-0}" ]; do
                    sleep 1
                    _sw=$((_sw + 1))
                    { read -r _swp _rest <"$LEDGER_TMP/w.pos"; } 2>/dev/null
                done
                if [ "${_swp:-0}" -gt "${_sat:-0}" ]; then st PASS "观察循环读过了测试行"; else st FAIL "观察循环读过了测试行" "它的位置 $_swp 没过 $_sat"; fi
            fi
        fi
    fi
    echo
    if [ "$_sf" = 0 ]; then echo "全部通过"; else echo "$_sf 项没通过"; fi
    [ "$_sf" = 0 ]
}

# ── the device manifest (docs/SHIP.md): doctor's first line, --tsv's last ───
# Two kinds of entries in /data/luyaz/ship/manifest.jsonl: what `u60 ship` put on
# the device (the last "ship"/"kit" line of each component, one md5 per file)
# and the files we only record (the last "record" line of each name; settings
# rewritten in normal use are deliberately not among them). Each is compared with
# the file's md5 now. md5s are cached by (path, size, inode, m/ctime) in /tmp,
# so a 30 MB binary is hashed once per change, not on every minute's run.
# Luya-Z rename D4: the ship family lives under /data/luyaz/ship (u60-ship.sh)
MANIFEST=${DOC_MANIFEST:-/data/luyaz/ship/manifest.jsonl}
SHIP_TXN=${DOC_SHIP_TXN:-/data/luyaz/ship/txn}
SHIP_HB=${DOC_SHIP_HB:-/tmp/luyaz/ship/heartbeat}
MD5_CACHE=${DOC_MD5_CACHE:-/tmp/luyaz/doctor/md5}
MD5SUM=${DOC_MD5SUM:-md5sum}
TS_DIR=${DOC_TS_DIR:-/data/tailscale}
DEVUI_DIR=${DOC_DEVUI_DIR:-/data/plugins/u60pro-devui}
# the touch UI's fonts moved with it (Phase 10 R5): the new directory once it is there
UI_LOGOS=${DOC_UI_LOGOS:-/data/luyaz/ui/operator-logos}
if [ -d /data/luyaz/ui/fonts ]; then UI_FONTS=${DOC_UI_FONTS:-/data/luyaz/ui/fonts}; else UI_FONTS=${DOC_UI_FONTS:-$DEVUI_DIR/fonts}; fi
U60D_PLUGINS=${DOC_U60D_PLUGINS:-/data/luyaz/core/plugins}
OBSERVE=600     # = u60-ship.sh (10 min since 10-14)
TREE_FRESH=     # --manifest: directory fingerprints always computed afresh

m_uptime() { _mu=$(cut -d. -f1 "$UPTIME_FILE" 2>/dev/null); case "$_mu" in '' | *[!0-9]*) _mu=0 ;; esac; echo "$_mu"; }

# The files we only record, "<name> <path>" (u60-ship.sh record knows the same list).
record_list() {
    echo "chill.sh ${DOC_CHILL_DIR:-/data/chill}/chill.sh"
    echo "template.yaml $CHILL_TEMPLATE"
    echo "tailscale-start.sh $TS_DIR/start.sh"
    echo "tuning.env $TS_TUNING"
    echo "tailscaled $TS_DIR/tailscaled"
    echo "tailscaled-nofight $TS_DIR/nofight/tailscaled"
    for _s in $CORE_NAME tailscale; do echo "init.d/$_s $INITD/$_s"; done
    echo "rc.local $RC"
    echo "luyaz-recover.sh ${SHIP_TXN%/*}/luyaz-recover.sh"
    # directories (third word "tree"): compared by their fingerprint
    echo "ui-fonts $UI_FONTS tree"
    echo "ui-logos $UI_LOGOS tree"
    echo "core-plugins $U60D_PLUGINS tree"
}

_TNL='
'
# tree_fp <dir>: the directory fingerprint (docs/SHIP.md): "<md5> <path>"
# per regular file (path without "./", LC_ALL=C order of the paths, one
# newline each), md5 of the whole text. Fails (1, nothing printed) for a
# missing directory or a link to one, anything but files and directories
# inside, names with a newline or "|", a file md5sum cannot read. The same
# function is in u60-ship.sh and u60-recover.sh (scripts/test/tree-fp).
tree_fp() {
    [ -d "$1" ] && [ ! -L "$1" ] || return 1
    (
        cd "$1" 2>/dev/null || exit 1
        [ -z "$(find . ! -type f ! -type d 2>/dev/null | head -n 1)" ] || exit 1
        [ -z "$(find . -name '*|*' 2>/dev/null | head -n 1)" ] || exit 1
        [ -z "$(find . -name "*$_TNL*" 2>/dev/null | head -n 1)" ] || exit 1
        _tn=$(find . -type f 2>/dev/null | wc -l | tr -dc 0-9)
        _tl=$(find . -type f -exec md5sum {} + 2>/dev/null |
            sed -n 's/^\([0-9a-f]\{32\}\)  \.\/\(.*\)$/\2|\1/p' | LC_ALL=C sort -t '|' -k 1,1)
        [ "$(printf '%s' "$_tl" | grep -c '|' | tr -dc 0-9)" = "${_tn:-x}" ] || exit 1
        printf '%s' "$_tl" | awk -F '|' 'NF { print $2 " " $1 }' | md5sum | cut -d' ' -f1
    )
}

# md5c <path>: its md5, "-" when it does not exist; cached.
md5c() {
    [ -f "$1" ] || { echo -; return; }
    # size, inode, mtime and ctime to the nanosecond: a same-size rewrite in
    # the same second still changes the key
    _mk=$(stat -c '%s:%i:%y:%z' "$1" 2>/dev/null | tr ' ' '_')
    _mh=$(awk -v p="$1" -v k="$_mk" '$1 == p && $2 == k { print $3; exit }' "$MD5_CACHE" 2>/dev/null)
    if [ -n "$_mh" ]; then echo "$_mh"; return; fi
    _mh=$($MD5SUM "$1" 2>/dev/null | cut -d' ' -f1)
    if [ -n "$_mh" ] && [ -n "$_mk" ] && mkdir -p "${MD5_CACHE%/*}" 2>/dev/null; then
        { grep -v "^$1 " "$MD5_CACHE" 2>/dev/null; echo "$1 $_mk $_mh"; } >"$MD5_CACHE.$$" 2>/dev/null &&
            mv -f "$MD5_CACHE.$$" "$MD5_CACHE" 2>/dev/null
        rm -f "$MD5_CACHE.$$" 2>/dev/null
    fi
    echo "${_mh:--}"
}

# treec <dir>: its fingerprint, "-" when it does not exist, "unreadable" when
# it cannot be taken. --manifest computes it every time (docs/SHIP.md); the
# minute-by-minute --tsv keeps it in the md5 cache under a key made of every
# file's name, size, inode and m/ctime, so unchanged trees are not re-hashed.
treec() {
    [ -e "$1" ] || [ -L "$1" ] || { echo -; return; }
    if [ -z "$TREE_FRESH" ]; then
        # xargs, not "-exec … {} +": the device's busybox find runs one stat per
        # file for "+" (741 processes a minute for /data/admin, u60-ledger.md §13)
        _tk=$( (cd "$1" 2>/dev/null && find . -print0 | xargs -0 stat -c '%n|%s|%i|%y|%z' 2>/dev/null) | LC_ALL=C sort | md5sum | cut -d' ' -f1)
        _th=$(awk -v p="tree:$1" -v k="$_tk" '$1 == p && $2 == k { print $3; exit }' "$MD5_CACHE" 2>/dev/null)
        if [ -n "$_th" ]; then echo "$_th"; return; fi
    fi
    _th=$(tree_fp "$1")
    if [ -n "$_th" ] && [ -z "$TREE_FRESH" ] && mkdir -p "${MD5_CACHE%/*}" 2>/dev/null; then
        { grep -v "^tree:$1 " "$MD5_CACHE" 2>/dev/null; echo "tree:$1 $_tk $_th"; } >"$MD5_CACHE.$$" 2>/dev/null &&
            mv -f "$MD5_CACHE.$$" "$MD5_CACHE" 2>/dev/null
        rm -f "$MD5_CACHE.$$" 2>/dev/null
    fi
    echo "${_th:-unreadable}"
}

# manifest_entries: "<comp|record> <name> <path> <md5> [tree]" for what the
# manifest says is on the device now (last line per component / name wins);
# "tree" = a directory, compared by its fingerprint. A path two components list
# (scripts took over the retired guard's files, u60-sentinel.md 5f) belongs to
# the newer of their lines.
manifest_entries() {
    awk '
        function fld(k,   m) {
            if (match($0, "\"" k "\":\"[^\"]*\"")) { m = substr($0, RSTART, RLENGTH); sub("^\"" k "\":\"", "", m); sub("\"$", "", m); return m }
            return ""
        }
        # record names before the Luya-Z rename D4c read as their new ones
        function canon(n) {
            if (n == "devui-fonts") return "ui-fonts"
            if (n == "devui-logos") return "ui-logos"
            if (n == "u60d-plugins") return "core-plugins"
            if (n == "u60-recover.sh") return "luyaz-recover.sh"
            return n
        }
        /^\{"v":1,/ {
            k = fld("kind")
            if (k == "ship" || k == "kit") {
                c = fld("comp"); if (c == "") next
                s = $0; f = ""
                while (match(s, /"path":"[^"]*","(md5|tree)":"[^"]*"/)) {
                    e = substr(s, RSTART, RLENGTH); s = substr(s, RSTART + RLENGTH)
                    t = (index(e, "\"tree\":") ? " tree" : "")
                    gsub(/"path":"|"md5":"|"tree":"|"/, "", e); sub(",", " ", e)
                    f = f "comp " c " " e t "\n"
                    p = e; sub(/ .*/, "", p); owner[p] = c
                }
                files[c] = f
            } else if (k == "retire") {
                nc = split(fld("comps"), RC, " "); for (i = 1; i <= nc; i++) delete files[RC[i]]
                nr = split(fld("records"), RR, " "); for (i = 1; i <= nr; i++) delete rec[canon(RR[i])]
            } else if (k == "record") {
                n = canon(fld("name")); if (n == "") next
                if (index($0, "\"tree\":\"")) rec[n] = "record " n " " fld("path") " " fld("tree") " tree\n"
                else rec[n] = "record " n " " fld("path") " " fld("md5") "\n"
            }
        }
        END {
            for (c in files) {
                n = split(files[c], L, "\n")
                for (i = 1; i <= n; i++) {
                    if (L[i] == "") continue
                    split(L[i], F, " ")
                    if (owner[F[3]] == c) print L[i]
                }
            }
            for (n in rec) printf "%s", rec[n]
        }
    ' "$MANIFEST" 2>/dev/null | sort
}

phase_word() {
    case "$1" in
        staged) echo 已暂存 ;; trial) echo 试跑中 ;; promote) echo 转正中 ;; check) echo 检查中 ;;
        manifest) echo 写清单中 ;; rollback) echo 退回中 ;; *) echo "$1" ;;
    esac
}
phase_word_en() {
    case "$1" in
        staged) echo staged ;; trial) echo "on trial" ;; promote) echo promoting ;; check) echo checking ;;
        manifest) echo "writing the manifest" ;; rollback) echo "rolling back" ;; *) echo "$1" ;;
    esac
}
# en_why <reason>: " (<reason>)" when it is printable ASCII, else nothing;
# u60-ship writes its reasons in Chinese, and the English column stays English.
en_why() {
    [ -n "$1" ] && [ -z "$(printf '%s' "$1" | tr -d ' -~')" ] && printf ' (%s)' "$1"
    return 0
}

# manifest_state: M_LEVEL (ok|warn) and M_TEXT, the first line's verdict
# (M_TEXT_EN: the same in English, for --tsv2).
# M_ROWS gets "<same|differs> <kind> <name> <path> <recorded> <actual>" lines.
manifest_state() {
    M_ROWS=
    _ph= _cp= _rs= _tp= _bt=
    if [ -f "$SHIP_TXN" ]; then
        while IFS= read -r _l; do
            case "$_l" in
                phase=*) _ph=${_l#phase=} ;; comp=*) _cp=${_l#comp=} ;; reason=*) _rs=${_l#reason=} ;;
                t_phase=*) _tp=${_l#t_phase=} ;; boot_id=*) _bt=${_l#boot_id=} ;;
            esac
        done <"$SHIP_TXN"
    fi
    case "$_ph" in
        staged | trial | promote | check | manifest | rollback)
            _hu= _hp=
            { read -r _hu _hp _x <"$SHIP_HB"; } 2>/dev/null
            case "$_hu" in '' | *[!0-9]*) _hu= ;; esac
            case "$_hp" in '' | *[!0-9]*) _hp= ;; esac
            _u=$(m_uptime)
            if [ -n "$_hu" ] && [ -n "$_hp" ] && [ $((_u - _hu)) -le 30 ] &&
                { tr '\0' ' ' <"$PROC/$_hp/cmdline"; } 2>/dev/null | grep -q -e 'u60-ship\.sh' -e 'luyaz-ship\.sh'; then
                M_LEVEL=ok M_TEXT="正在上机：$_cp（$(phase_word "$_ph")）"
                M_TEXT_EN="Deploying $_cp ($(phase_word_en "$_ph"))"
            else
                M_LEVEL=warn M_TEXT="上次上机停在半路：$_cp（$(phase_word "$_ph")），guard 或 u60 status 会收尾"
                M_TEXT_EN="Last deploy stopped midway: $_cp ($(phase_word_en "$_ph")); guard or u60 status will finish it"
            fi
            return
            ;;
        failed)
            M_LEVEL=warn M_TEXT="上次上机停在半路，要人处理：$_cp（$_rs）"
            M_TEXT_EN="Last deploy stopped midway, needs attention: $_cp$(en_why "$_rs")"
            return
            ;;
        manifest_pending)
            M_LEVEL=warn M_TEXT="清单待补：$_cp 已转正并通过检查，清单还没写上（下次 ship 或 u60 status 补）"
            M_TEXT_EN="Manifest pending: $_cp is live and checked, not yet in the manifest (next ship or u60 status adds it)"
            return
            ;;
    esac
    if [ ! -f "$MANIFEST" ]; then
        M_LEVEL=ok M_TEXT="还没有清单"
        M_TEXT_EN="No manifest yet"
        return
    fi
    _ents=$(manifest_entries)
    if [ -z "$_ents" ]; then
        M_LEVEL=warn M_TEXT="清单读不懂（$MANIFEST）"
        M_TEXT_EN="Manifest unreadable ($MANIFEST)"
        return
    fi
    _n=0 _bad=0 _first= _first_en=
    _ifs=$IFS
    IFS='
'
    for _e in $_ents; do
        IFS=$_ifs
        set -- $_e
        if [ "$5" = tree ]; then _act=$(treec "$3"); else _act=$(md5c "$3"); fi
        _n=$((_n + 1))
        if [ "$_act" = "$4" ]; then
            M_ROWS="${M_ROWS}same $1 $2 $3 $4 $_act
"
        else
            M_ROWS="${M_ROWS}differs $1 $2 $3 $4 $_act
"
            _bad=$((_bad + 1))
            [ -n "$_first" ] || _first="${3##*/}（清单 $(printf '%s' "$4" | cut -c1-8)，实际 $(printf '%s' "$_act" | cut -c1-8)）"
            [ -n "$_first_en" ] || _first_en="${3##*/} (manifest $(printf '%s' "$4" | cut -c1-8), actual $(printf '%s' "$_act" | cut -c1-8))"
        fi
    done
    IFS=$_ifs
    if [ "$_bad" -gt 0 ]; then
        M_LEVEL=warn M_TEXT="不一致的是 $_first"
        M_TEXT_EN="Mismatch: $_first_en"
        [ "$_bad" -gt 1 ] && M_TEXT="$M_TEXT 等 $_bad 项" && M_TEXT_EN="$M_TEXT_EN and $((_bad - 1)) more"
        return
    fi
    M_LEVEL=ok M_TEXT="一致（$_n 项）"
    M_TEXT_EN="All match ($_n)"
    case "$_tp" in '' | *[!0-9]*) _tp= ;; esac
    _now_boot=$(tr -dc '0-9a-f-' <"$BOOT_ID_FILE" 2>/dev/null)
    if [ "$_ph" = done ] && [ -n "$_tp" ] && [ "$_bt" = "$_now_boot" ]; then
        _left=$((OBSERVE - ($(m_uptime) - _tp)))
        [ "$_left" -gt 0 ] && M_TEXT="观察中（还剩 $(((_left + 59) / 60)) 分钟）：$_cp 刚上机；$M_TEXT" &&
            M_TEXT_EN="Watching ($(((_left + 59) / 60)) min left): $_cp just deployed; $M_TEXT_EN"
    fi
    case "$_ph" in
        rolledback) M_TEXT="$M_TEXT；上次上机已退回：$_cp（$_rs）" M_TEXT_EN="$M_TEXT_EN; last deploy rolled back: $_cp$(en_why "$_rs")" ;;
        aborted) M_TEXT="$M_TEXT；上次上机已中止：$_cp（$_rs）" M_TEXT_EN="$M_TEXT_EN; last deploy aborted: $_cp$(en_why "$_rs")" ;;
    esac
}

# doctor.sh --manifest: one row per entry, then the verdict (for u60 status).
manifest_report() {
    TREE_FRESH=1
    manifest_state
    printf '%s' "$M_ROWS" | awk 'NF == 6 { printf "%s\t%s:%s\t%s\t%s\t%s\n", $1, $2, $3, $4, $5, $6 }'
    # the files we only record but never did
    record_list | while read -r _n _p _k; do
        printf '%s' "$M_ROWS" | awk -v n="$_n" '$2 == "record" && $3 == n { f = 1 } END { exit !f }' && continue
        if [ "$_k" = tree ]; then _a=$(treec "$_p"); else _a=$(md5c "$_p"); fi
        printf 'unrecorded\trecord:%s\t%s\t-\t%s\n' "$_n" "$_p" "$_a"
    done
    printf 'state\t%s\t%s\n' "$M_LEVEL" "$M_TEXT"
}

if [ "$1" = "--manifest" ]; then
    manifest_report
    exit 0
fi

if [ "$1" = "--tree-fp" ]; then
    tree_fp "$2"
    exit $?
fi

if [ "$1" = "--ledger-selftest" ]; then
    selftest
    exit $?
fi

if [ "$1" = "--report" ]; then
    uptime_s() { cut -d. -f1 "$UPTIME_FILE" 2>/dev/null || echo 0; }
    report_ledger "$2" "$3"
    exit $?
fi

if [ "$1" = "--calibrate-standby" ]; then
    uptime_s() { cut -d. -f1 "$UPTIME_FILE" 2>/dev/null || echo 0; }
    calibrate_standby; exit $?
fi

TSV=0
[ "$1" = "--tsv" ] && TSV=1
[ "$1" = "--tsv2" ] && TSV=2
[ "$TSV" = 2 ] && echo "#tsv2"
NBAD=0
NWARN=0

# report <ok|warn|bad> <id> <label> <detail> <label_en> <detail_en>
# English wording: docs/DESIGN.md §1 item 6 and docs/ui-glossary.md §3 (no
# closing full stop, no "Please", ASCII only; scripts/test/doctor checks).
report() {
    case "$1" in bad) NBAD=$((NBAD + 1)) ;; warn) NWARN=$((NWARN + 1)) ;; esac
    if [ "$TSV" = 1 ]; then
        printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$(printf '%s' "$4" | tr '\t\n' '  ')"
    elif [ "$TSV" = 2 ]; then
        printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$(printf '%s' "$4" | tr '\t\n' '  ')" \
            "$(printf '%s' "$5" | tr '\t\n' '  ')" "$(printf '%s' "$6" | tr '\t\n' '  ')"
    else
        case "$1" in ok) s='●' ;; warn) s='▲' ;; *) s='■' ;; esac
        printf '  %s %s：%s\n' "$s" "$3" "$4"
    fi
}

uptime_s() { cut -d. -f1 "$UPTIME_FILE" 2>/dev/null || echo 0; }
count_comm() { # processes whose /proc comm starts with $1
    _n=0
    for _d in "$PROC"/[0-9]*; do
        { read -r _c <"$_d/comm"; } 2>/dev/null || continue   # the process may be gone by now
        case "$_c" in "$1"*) _n=$((_n + 1)) ;; esac
    done
    echo "$_n"
}
svc_running() {
    $UBUS call service list "{\"name\":\"$1\"}" 2>/dev/null | grep -q '"running": *true'
}

# ── the manifest: first line for people, last row for --tsv (R9) ───────────
manifest_state
[ "$TSV" != 0 ] || report "$M_LEVEL" manifest "清单" "$M_TEXT" Manifest "$M_TEXT_EN"

# ── boot and firmware ───────────────────────────────────────────────────────
sync=$($UBUS call zwrt_topsw_daemon.sync get_sync_info '{}' 2>/dev/null | sed -n 's/.*"noSyncModuleName": *"\([^"]*\)".*/\1/p')
if [ "$sync" = "sync success" ]; then
    report ok boot-sync "开机同步" "sync success" "Boot sync" "All stock services registered (sync success)"
else
    report bad boot-sync "开机同步" "${sync:-读不到}（有原厂服务没注册：屏幕卡 logo / 不拨号的根源）" "Boot sync" "${sync:-Unreadable} (a stock service did not register: why the screen sticks on the logo or nothing dials)"
fi

mode=$($UCI -q get zwrt_zte_dm.dm_update.dm_update_mode)
poll=$($UCI -q get zwrt_zte_dm.dm_update.TURNOFFPOLLING)
if [ "$mode" = 0 ] && [ "$poll" = 1 ]; then
    report ok fota "ZTE 自动升级" "已关闭" "ZTE auto-update" "Off"
else
    report bad fota "ZTE 自动升级" "没有完全关闭（dm_update_mode=${mode:-?} TURNOFFPOLLING=${poll:-?}）；升级会覆盖 rc.local" "ZTE auto-update" "Not fully off (dm_update_mode=${mode:-?} TURNOFFPOLLING=${poll:-?}); an update would overwrite rc.local"
fi

# Whiteouts in the /etc overlay's rc.d silently delete boot links. On a daemon
# the boot barrier waits for, that means a device stuck on the logo.
if [ -d "$OVERLAY_RCD" ]; then
    wo=$(ls -la "$OVERLAY_RCD" 2>/dev/null | awk '/^c/ {print $NF}')
    hit=
    for w in $wo; do
        n=$(echo "$w" | sed 's/^[SK][0-9]*//')
        grep -qE "^[^#]*[[:space:]]$n\$" "$DAEMON_CONF" 2>/dev/null && hit="$hit $w"
    done
    if [ -n "$hit" ]; then
        report bad whiteout "开机链接" "被屏蔽的原厂服务:$hit（删掉 $OVERLAY_RCD 里对应的字符设备）" "Boot links" "Stock services masked:$hit (delete their character devices in $OVERLAY_RCD)"
    else
        report ok whiteout "开机链接" "开机同步名单里的服务没有被屏蔽${wo:+（另有 $(echo $wo | wc -w) 个无关的：$(echo $wo | tr '\n' ' ')）}" \
            "Boot links" "No boot-sync service is masked${wo:+ ($(echo $wo | wc -w) unrelated: $(echo $wo))}"
    fi
fi

# Stock phone-home (read only): what the firmware reports outward. Whether to
# turn any of it off is the user's call (with uci, never by stopping stock
# services); FOTA has its own row above. One /proc pass, three uci forks.
pmq=0; ptr=0; psm=0
for _d in "$PROC"/[0-9]*; do
    { read -r _c <"$_d/comm"; } 2>/dev/null || continue
    case "$_c" in zte_mqtt_sdk_st) pmq=1 ;; zte_topsw_tr069) ptr=1 ;; zte_smart_manag*) psm=1 ;; esac
done
_run() { [ "$1" = 1 ] && printf '在跑' || printf '没在跑'; }
_run_en() { [ "$1" = 1 ] && printf 'running' || printf 'not running'; }
mq=$($UCI -q show zwrt_mqtt.config 2>/dev/null | sed -n "s/^zwrt_mqtt\.config\.mqttOnreportEnable='*\([^']*\)'*\$/\1/p")
case "$mq" in
    1) mqz="MQTT 上报开着（$(_run $pmq)）"; mqe="MQTT reporting on ($(_run_en $pmq))" ;;
    0) mqz="MQTT 上报关着"; mqe="MQTT reporting off" ;;
    *) mqz="MQTT 读不到"; mqe="MQTT unreadable" ;;
esac
trs=$($UCI -q show zwrt_tr069.ManagementServer 2>/dev/null)
tre=$(printf '%s\n' "$trs" | sed -n "s/^zwrt_tr069\.ManagementServer\.EnableCWMP='*\([^']*\)'*\$/\1/p")
tru=$(printf '%s\n' "$trs" | sed -n "s/^zwrt_tr069\.ManagementServer\.URL='*\([^']*\)'*\$/\1/p")
case "$tre" in
    1) if [ -n "$tru" ]; then trz="TR-069 开着（$(_run $ptr)），配了服务器"; tren="TR-069 on ($(_run_en $ptr)), server set"
       else trz="TR-069 开着（$(_run $ptr)），没配服务器"; tren="TR-069 on ($(_run_en $ptr)), no server set"; fi ;;
    0) trz="TR-069 关着"; tren="TR-069 off" ;;
    *) trz="TR-069 读不到"; tren="TR-069 unreadable" ;;
esac
ddn=$($UCI -q show zwrt_zte_dadian_debug 2>/dev/null | grep -c "\.switch='*1'*\$")
case "$ddn" in '' | *[!0-9]*) ddn=0 ;; esac
report ok phonehome "原厂外联上报" "$mqz；$trz；打点开关 $ddn 个开着；应用库更新（smart_manage）$(_run $psm)" \
    "Stock phone-home" "$mqe; $tren; $ddn analytics switches on; app catalog updates (smart_manage) $(_run_en $psm)"

# Anonymous RPC (read only): the stock rpcd ACL lets the not-logged-in session
# reach the factory/ADB methods (B31, 2026-10-16). Asked with `session access`,
# never by calling them. A warning, not an error: tightening it is the user's call.
acl=; acn=0
for _f in fac_open fac_close fac_reset fac_reboot fac_query_key adb_switch; do
    _a=$($UBUS call session access "{\"ubus_rpc_session\":\"00000000000000000000000000000000\",\"scope\":\"ubus\",\"object\":\"zwrt_mc.device.manager\",\"function\":\"$_f\"}" 2>/dev/null)
    case "$_a" in *'"access": true'* | *'"access":true'*) acl="$acl $_f" ;; *'"access"'*) acn=$((acn + 1)) ;; esac
done
if [ -n "$acl" ]; then
    report warn rpc-acl "匿名权限" "不登录也能调原厂出厂/ADB 方法:$acl（原厂 ACL，没收紧）" "Anonymous RPC" "Stock factory/ADB methods callable without login:$acl (stock ACL, not tightened)"
elif [ "$acn" = 6 ]; then
    report ok rpc-acl "匿名权限" "出厂/ADB 方法要登录才能调" "Anonymous RPC" "Factory/ADB methods need a login"
else
    report ok rpc-acl "匿名权限" "读不到" "Anonymous RPC" "Unreadable"
fi

# Clock trust: u60-guard's verdict when it is fresh, else the same rule here
# (docs/LEDGER.md §7). Before NTP the device clock reads 2025-01-04.
up=$(uptime_s)
case "$up" in '' | *[!0-9]*) up=0 ;; esac
t=$($DATE +%s)
case "$t" in '' | *[!0-9]*) t=0 ;; esac
ck=
cu=
{ read -r ck _cw cu _ch <"$CLOCK_OK"; } 2>/dev/null
case "$cu" in '' | *[!0-9]*) cu= ;; esac
if [ -n "$cu" ] && [ "$cu" -le "$up" ] && [ $((up - cu)) -le 300 ]; then
    clk=$ck
else
    last=$(cat "$WALL_LAST" 2>/dev/null)
    case "$last" in '' | *[!0-9]*) last=0 ;; esac
    clk=0
    if [ "$t" -ge "$CLOCK_YEAR_MIN" ] && { [ "$last" -eq 0 ] || [ "$t" -ge $((last - 86400)) ]; }; then
        clk=1
    fi
fi
if [ "$clk" = 1 ]; then
    _dt=$($DATE '+%Y-%m-%d %H:%M')
    report ok clock "时钟" "已对时（$_dt 设备当地时间）" Clock "Synced ($_dt device local time)"
elif [ "$up" -lt 300 ]; then
    report ok clock "时钟" "刚开机，还在对时" Clock "Just booted; still syncing"
else
    report warn clock "时钟" "还没对时，告警短信在对时前不发" Clock "Not synced yet; no alert SMS until it is"
fi

# ── our services ────────────────────────────────────────────────────────────
# After the sentinel took over (rc.local starts luyaz-sentinel instead of
# u60-uid and u60-guard; manager docs/designs/u60-sentinel.md 5f) the two old
# services are not expected any more.
if grep -q "^[^#]*$INITD/luyaz-sentinel start" "$RC" 2>/dev/null; then
    SVCS="$CORE_NAME luyaz-sentinel"
else
    SVCS="$CORE_NAME u60-guard u60-uid"
fi
# the core's move (10e, review D2): half-way = abnormal. The new directories
# without .migrated (the move or a sentinel stopped part-way), or the switch
# there but the new init or program missing (nothing would start the core).
if luyaz_new "$LROOT"; then
    _lm=
    [ -x "$INITD/luyaz-core" ] || _lm="$_lm $INITD/luyaz-core"
    [ -x "$LROOT/data/luyaz/luyaz-core" ] || _lm="$_lm $LROOT/data/luyaz/luyaz-core"
    if [ -n "$_lm" ]; then
        report bad core-move "核心搬家" "已切到新布局（.migrated），但缺:$_lm（重启后核心起不来；见 docs/designs/u60-phase10.md）" \
            "Core move" "Switched to the new layout (.migrated) but missing:$_lm (the core won't start after a reboot; see docs/designs/u60-phase10.md)"
    fi
elif [ -d "$LROOT/data/luyaz/core" ]; then
    report bad core-move "核心搬家" "/data/luyaz/core 在但没有 .migrated：搬了一半，还在用旧路径（旧布局，见 docs/designs/u60-phase10.md）" \
        "Core move" "/data/luyaz/core exists without .migrated: moved half-way, still on the old paths (old layout, see docs/designs/u60-phase10.md)"
fi
if [ -d "$LUYAZ_TMP" ] && [ ! -f "$LUYAZ_TMP/.migrated" ]; then
    report bad tmp-move "哨兵暂存目录" "$LUYAZ_TMP 在但没有 .migrated：哨兵搬了一半，还在用 /tmp/u60-guard" \
        "Sentinel scratch dir" "$LUYAZ_TMP exists without .migrated: moved half-way, still on /tmp/u60-guard"
fi
# zte-agent is retired (8d-2c): none expected; one still running is odd
if [ "$(count_comm zte-agent)" -ge 1 ]; then
    report warn svc-zte-agent "高级后台" "已退役（8d-2c），却还在跑：核心不会接手它的活" \
        "Admin backend" "Retired (8d-2c) but still running: the core does not take over its work"
else
    report ok svc-zte-agent "高级后台" "已退役（核心和新管理网页接替）" "Admin backend" "Retired (the core and the new admin pages took over)"
fi
# luyaz-web (u60 ship lweb, manager docs/designs/u60-web.md): where its program is there or
# rc.local starts it (program without the line = "won't come back after a reboot")
if [ -e "${DOC_LWEB_BIN:-/data/luyaz/luyaz-web}" ] || grep -q "^[^#]*$INITD/luyaz-web start" "$RC" 2>/dev/null; then
    SVCS="$SVCS luyaz-web"
fi
for s in $SVCS; do
    case "$s" in
        zwrt-datad | luyaz-core) label="数据服务" label_en="Data service" ;;
        u60-guard) label="Wi-Fi 兜底看门狗" label_en="Wi-Fi watchdog" ;;
        u60-uid) label="屏幕守护进程" label_en="Screen supervisor" ;;
        luyaz-sentinel) label="哨兵（屏幕和看门狗）" label_en="Sentinel (screen and watchdog)" ;;
        luyaz-web) label="管理网页（:9090）" label_en="Admin pages (:9090)" ;;
    esac
    if [ ! -x "$INITD/$s" ]; then
        report warn "svc-$s" "$label" "没装 procd 服务（旧装法：崩了没人拉起）" "$label_en" "No procd service (old install: nothing restarts it after a crash)"
    elif svc_running "$s"; then
        if grep -q "^[^#]*$INITD/$s start" "$RC" 2>/dev/null; then
            report ok "svc-$s" "$label" "procd 监督中" "$label_en" "Supervised by procd"
        else
            report warn "svc-$s" "$label" "在跑，但 rc.local 里没有启动它：重启后不会起来" "$label_en" "Running, but rc.local doesn't start it: it won't come back after a reboot"
        fi
    else
        report bad "svc-$s" "$label" "procd 服务没在运行（logread 看原因；/etc/init.d/$s start）" "$label_en" "procd service not running (see logread; /etc/init.d/$s start)"
    fi
done

for p in zwrt-datad u60pro-devui; do
    n=$(count_comm "$p")
    # the core under either name (10e): a leftover old one next to the new is two
    [ "$p" = zwrt-datad ] && n=$((n + $(count_comm luyaz-core)))
    # the touch UI under either name (10h R5): one of each is two owners of card0
    [ "$p" = u60pro-devui ] && n=$((n + $(count_comm luyaz-ui)))
    if [ "$n" -gt 1 ]; then
        report bad "dup-$p" "$p 实例数" "$n 份在跑（应当只有 1 份）" "$p instances" "$n running (should be 1)"
    fi
done

# CHILL's controller (:9999) is for this device only (CHILL_API_LAN=0; the
# Bearer secret is off). chill.env is not in the manifest, so a hand edit that
# opens it to the LAN would go unnoticed: read the listening sockets instead.
# No row without CHILL installed.
if [ -f "$CHILL_ENV" ]; then
    api=$(cat "$PROC/net/tcp" "$PROC/net/tcp6" 2>/dev/null | awk '$4 == "0A" && $2 ~ /:270F$/ { sub(/:270F$/, "", $2); print $2 }' | sort -u)
    apiw=
    for a in $api; do
        case "$a" in
            0100007F | 00000000000000000000000001000000 | 0000000000000000FFFF00000100007F) ;;
            00000000) apiw="$apiw 0.0.0.0" ;;
            00000000000000000000000000000000) apiw="$apiw ::" ;;
            *) apiw="$apiw $a" ;;
        esac
    done
    if [ -n "$apiw" ]; then
        report warn chill-api "CHILL 管理接口" "对局域网开放（:9999 听在$apiw）；chill.env 里 CHILL_API_LAN 应为 0" \
            "CHILL API" "Open to the LAN (:9999 on$apiw); set CHILL_API_LAN=0 in chill.env"
    elif [ -n "$api" ]; then
        report ok chill-api "CHILL 管理接口" "只听本机" "CHILL API" "This device only"
    else
        report ok chill-api "CHILL 管理接口" "没在监听（CHILL 没开）" "CHILL API" "Not listening (CHILL off)"
    fi
    # Home exit (E3, manager docs/designs/home-exit.md §6): only when chill.env
    # names one. Walks the chain and reports the first broken link: the value,
    # CHILL running, the Tailscale route to home, home answering through
    # 家里 (one mihomo delay test, China-side 204 like the agent's probe).
    hx=$(sed -n "s/^CHILL_HOME_EXIT=['\"]\{0,1\}\([^'\"]*\)['\"]\{0,1\}[[:space:]]*\$/\1/p" "$CHILL_ENV" | tail -n 1)
    if [ -n "$hx" ]; then
        hxip=${hx%:*}
        hxport=${hx##*:}
        case "$hx" in *[!0-9.:]* | *:*:* | :* | *:) hxip= ;; esac
        [ "$hxip" = "$hx" ] && hxip=
        if [ -z "$hxip" ]; then
            report warn home-exit "家里出口" "chill.env 的 CHILL_HOME_EXIT 不是 IPv4:端口（$hx）" \
                "Home exit" "CHILL_HOME_EXIT in chill.env is not IPv4:port ($hx)"
        elif grep -q '"reason":"idle"' "$CHILL_STATE" 2>/dev/null; then
            report ok home-exit "家里出口" "CHILL 息屏暂停（没人用），用不上" "Home exit" "Not in use (CHILL paused: screen off, nobody connected)"
        elif ! grep -q '"state":"running"' "$CHILL_STATE" 2>/dev/null; then
            report ok home-exit "家里出口" "CHILL 没在跑，用不上" "Home exit" "Not in use (CHILL not running)"
        elif ! $IP route get "$hxip" 2>/dev/null | grep -q 'dev tailscale0'; then
            report warn home-exit "家里出口" "到 $hxip 不走 Tailscale（子网路由没发布，或 Tailscale 没连上）" \
                "Home exit" "No Tailscale route to $hxip (subnet route not advertised, or Tailscale down)"
        else
            # The core runs this every minute. Only the home exit engine probes 家里 (every
            # 5 min while someone can use it, none while nobody can; manager
            # docs/designs/u60-radio.md §5, D3/D14), and mihomo keeps each result in the
            # proxy's history: show the last entry, never probe here (a probe wakes the
            # tailnet and the cellular link). Older than 30 min: say how old.
            hxh=$($WGET -q -T 3 -O - "http://127.0.0.1:9999/proxies/%E5%AE%B6%E9%87%8C" 2>/dev/null |
                grep -o '"history":\[[^]]*\]' | tail -n 1 | tr '{' '\n' | tail -n 1)
            hxd=$(echo "$hxh" | sed -n 's/.*"delay":\([0-9][0-9]*\).*/\1/p')
            hxa=$(echo "$hxh" | sed -n 's/.*"time":"\([0-9-]*\)T\([0-9:]*\).*/\1 \2/p' |
                TZ=UTC awk -v now="$t" '{ gsub(/[-:]/, " "); a = now - mktime($0); print ((a >= 0) ? a : "") }')
            hxz= hxe=
            if [ -n "$hxa" ] && [ "$hxa" -ge 1800 ]; then
                hxz="，$((hxa / 60)) 分钟前" hxe=", $((hxa / 60)) min ago"
            fi
            if [ -z "$hxd" ]; then
                report ok home-exit "家里出口" "经 Tailscale 通；还没测过家里（没人用时不测）" \
                    "Home exit" "Routed via Tailscale; home not probed yet (no probe while nobody uses it)"
            elif [ "$hxd" -gt 0 ]; then
                report ok home-exit "家里出口" "经 Tailscale 能用（${hxd} ms$hxz）" "Home exit" "Working via Tailscale (${hxd} ms$hxe)"
            else
                report warn home-exit "家里出口" "家里没回应（$hxip:$hxport$hxz；Mac 睡了，或代理端口变了）" \
                    "Home exit" "Home not answering ($hxip:$hxport$hxe; Mac asleep, or proxy port changed)"
            fi
        fi
    fi
fi
# luyaz-web holds :9090 (its /health, no login)
if $WGET -q -T 5 -O /dev/null http://127.0.0.1:9090/health 2>/dev/null; then
    report ok agent-http "管理网页 :9090" "能访问" "Web admin :9090" "Reachable"
else
    report bad agent-http "管理网页 :9090" "打不开" "Web admin :9090" "Not reachable"
fi
if $WGET -q -T 5 -O /dev/null "$DATAD_URL" 2>/dev/null; then
    report ok datad-http "数据服务 :9460" "能访问" "Data service :9460" "Reachable"
else
    report bad datad-http "数据服务 :9460" "读不到 /v2/state（屏幕会没有数据）" "Data service :9460" "Can't read /v2/state (the screen will have no data)"
fi

# ── Wi-Fi safety net ────────────────────────────────────────────────────────
up=$(uptime_s)
hb=$(cat "$HEARTBEAT" 2>/dev/null)
case "$hb" in '' | *[!0-9]*) hb= ;; esac
if [ -z "$hb" ]; then
    report bad heartbeat "后台心跳" "没有心跳文件（后台没在运行？）" "Admin heartbeat" "No heartbeat file (admin backend not running?)"
elif [ $((up - hb)) -le 120 ]; then
    report ok heartbeat "后台心跳" "$((up - hb)) 秒前" "Admin heartbeat" "$((up - hb)) s ago"
else
    report bad heartbeat "后台心跳" "$((up - hb)) 秒没更新（情景引擎卡住了？看门狗 5 分钟后会接管 Wi-Fi）" "Admin heartbeat" "No update for $((up - hb)) s (scenario engine stuck? The watchdog takes over Wi-Fi after 5 min)"
fi
if [ -f "$MARKER" ]; then
    report warn takeover "Wi-Fi 看门狗" "接管中：情景固定暂停，后台稳定 10 分钟后自动交回" "Wi-Fi watchdog" "In control: scenarios paused; handed back after 10 min of stable admin backend"
fi
aps=$($PS 2>/dev/null | awk '/\/hostapd( |$)/ && !/awk/ {n++} END {print n+0}')
if [ "$aps" -gt 0 ]; then
    report ok wifi "Wi-Fi" "在广播" Wi-Fi "Broadcasting"
else
    # 情景有意关掉的（在家）不算问题：问后台当前情景是不是关 Wi-Fi 的那种
    pub=$($WGET -q -T 3 -O - http://127.0.0.1:9460/v2/api/public/status 2>/dev/null)
    case "$pub" in
        *'"wifi_off":true'*) report ok wifi "Wi-Fi" "当前情景关着 Wi-Fi（按设定）" Wi-Fi "Off by the current scenario (as set)" ;;
        *) report warn wifi "Wi-Fi" "没有在广播，当前情景也没要求关" Wi-Fi "Not broadcasting, and the current scenario doesn't turn it off" ;;
    esac
fi

# ── screen ──────────────────────────────────────────────────────────────────
if [ -f "$UID_STATE/gave-up" ]; then
    report bad screen "触屏界面" "反复启动失败，已换回原厂界面（长按屏幕右下角 3 秒，或 echo devui > /tmp/luyaz-screen.ctl）" \
        "Screen UI" "Kept failing to start; stock UI on (hold bottom-right 3 s, or echo devui > /tmp/luyaz-screen.ctl)"
elif [ "$(cat "$UID_WANT" 2>/dev/null)" = vendor ]; then
    report ok screen "触屏界面" "按要求显示原厂界面" "Screen UI" "Stock UI on, as asked"
elif [ $(($(count_comm u60pro-devui) + $(count_comm luyaz-ui))) -ge 1 ]; then
    a=$(cat "$UID_STATE/attempts" 2>/dev/null)
    case "$a" in '' | 0) report ok screen "触屏界面" "运行中" "Screen UI" "Running" ;;
        *) report ok screen "触屏界面" "运行中（刚启动，稳定 10 分钟后确认）" "Screen UI" "Running (just started; confirmed after 10 min stable)" ;; esac
else
    report bad screen "触屏界面" "没在运行" "Screen UI" "Not running"
fi

# ── alerts, crashes, storage ────────────────────────────────────────────────
unread=0
if [ -f "$ALERTS/queue" ]; then
    read_seq=$(cat "$ALERTS/read" 2>/dev/null)
    case "$read_seq" in '' | *[!0-9]*) read_seq=0 ;; esac
    unread=$(awk -F'\t' -v r="$read_seq" 'NF == 5 && $1 + 0 > r + 0' "$ALERTS/queue" | wc -l)
fi
if [ "$unread" -gt 0 ]; then
    report warn alerts "告警" "$unread 条未读（管理网页「系统 → 告警」）" Alerts "$unread unread (Alerts in web admin)"
else
    report ok alerts "告警" "没有未读" Alerts "None unread"
fi
if [ -s "$ALERTS/sms-to" ]; then
    report ok sms "短信告警" "已配置" "Alert SMS" "Set up"
else
    report warn sms "短信告警" "没配置号码：后台挂了你不会知道" "Alert SMS" "No number set: you won't hear if the admin backend dies"
fi

recent=$(find "$CRASH" -name '*.log' -mtime -1 2>/dev/null | wc -l)
if [ "$recent" -gt 0 ]; then
    # 每次崩溃都会记一条告警；告警都读过了，就只记一笔、不再算「注意」
    if [ "$unread" -gt 0 ]; then
        report warn crashes "崩溃记录" "最近 24 小时 $recent 份（$CRASH）" "Crash logs" "$recent in the last 24h ($CRASH)"
    else
        report ok crashes "崩溃记录" "最近 24 小时 $recent 份，对应告警已读" "Crash logs" "$recent in the last 24h; their alerts read"
    fi
else
    report ok crashes "崩溃记录" "最近 24 小时没有" "Crash logs" "None in the last 24h"
fi

# Last line, counted from the end: a long device name makes df wrap its row.
free_kb=$($DF -k "$DATA_DIR" 2>/dev/null | tail -n 1 | awk '{print $(NF - 2)}')
case "$free_kb" in '' | *[!0-9]*) free_kb= ;; esac
if [ -z "$free_kb" ]; then
    report warn disk "/data 空间" "读不到" "/data space" "Unreadable"
elif [ "$free_kb" -lt 20480 ]; then
    report bad disk "/data 空间" "只剩 $((free_kb / 1024)) MB" "/data space" "Only $((free_kb / 1024)) MB left"
elif [ "$free_kb" -lt 102400 ]; then
    report warn disk "/data 空间" "只剩 $((free_kb / 1024)) MB" "/data space" "Only $((free_kb / 1024)) MB left"
else
    report ok disk "/data 空间" "剩 $((free_kb / 1024)) MB" "/data space" "$((free_kb / 1024)) MB free"
fi

_sb=$(standby_check)
_sbe=
[ "$TSV" = 2 ] && _sbe=$(standby_check en)
report "${_sb%%	*}" standby "待机" "${_sb#*	}" Standby "${_sbe#*	}"
[ "$TSV" != 0 ] && report "$M_LEVEL" manifest "清单" "$M_TEXT" Manifest "$M_TEXT_EN"

if [ "$TSV" = 0 ]; then
    echo
    if [ "$NBAD" = 0 ] && [ "$NWARN" = 0 ]; then
        echo "  一切正常"
    else
        echo "  $NBAD 项异常，$NWARN 项需要注意"
    fi
    # the scorecard's three lines: the core's card now (u60-sentinel.md §20: once the guard is
    # gone nobody keeps the file), else as u60-guard last wrote them (docs/LEDGER.md §12)
    _rt=$(mktemp -d 2>/dev/null) || _rt=/tmp/doctor-summary.$$
    mkdir -p "$_rt"
    : >"$_rt/warn"
    _sum=1
    standby_check >"$_rt/standby"
    _sw=$SCORECARD_WAIT
    SCORECARD_WAIT=5 # a 7d card is 0.1 s on the device; this is only the tail of the output
    if report_core 7d >"$_rt/card" && [ -s "$_rt/card" ]; then
        echo
        cat "$_rt/card"
    elif [ -s "$LEDGER_DIR/summary" ]; then
        echo
        cat "$LEDGER_DIR/summary"
    fi
    SCORECARD_WAIT=$_sw
    rm -rf "$_rt"
fi
[ "$NBAD" = 0 ]
