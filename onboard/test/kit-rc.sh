#!/bin/sh
# 装机包改 rc.local 的离线测试（不碰设备）：两种起点（原厂、已是哨兵）
# 走一遍 do_ui 改 rc.local 的那几步（核心那一行 → rc_owner）；
# 结果必须过 luyaz-ship.sh 的 rc_mode = sentinel，已是哨兵的那份要原样不变，再跑一遍也不变。
#   sh onboard/test/kit-rc.sh      # 用本仓库的 touch-ui/（Phase 10 10f）
set -u
ONB=$(cd "$(dirname "$0")/.." && pwd)
SHIP=${SHIP_SH:-$ONB/../touch-ui/scripts/luyaz-ship.sh}
[ -f "$SHIP" ] || { echo "没有 $SHIP" >&2; exit 2; }
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
sed -n '/^rc_mode() {/,/^}/p' "$SHIP" > "$T/rc_mode.sh"
. "$T/rc_mode.sh"
REC='sh /data/luyaz/ship/luyaz-recover.sh; [ $? = 10 ] && exec /bin/sh /etc/rc.local   # luyaz recover：开机先收尾上次没做完的 u60 ship'
SENT='/etc/init.d/luyaz-sentinel start   # u60 ship sentinel: the screen owner and watchdog'
fail=0

printf '%s\n' '#!/bin/sh' '# vendor' "$REC" 'exit 0' > "$T/vendor"
printf '%s\n' '#!/bin/sh' 'sh /data/local/tmp/start_dropbear.sh' "$REC" '/etc/init.d/luyaz-core start' \
    "$SENT" 'sh /data/tailscale/start.sh' '[ -f /data/chill/disabled ] || /etc/init.d/chill start' 'exit 0' > "$T/sentinel"

steps() { # 同 do_ui：核心行 → rc_owner
    rc_add "/etc/init.d/luyaz-core start" "/etc/init.d/luyaz-core start" >/dev/null
    rc_owner >/dev/null
}
for start in vendor sentinel; do
    mkdir -p "$T/$start.d"
    cp "$T/$start" "$T/$start.d/rc.local"
    (
        KIT_RC="$T/$start.d/rc.local" KIT_STATE="$T/$start.d/state" KIT_LIB_ONLY=1
        export KIT_RC KIT_STATE KIT_LIB_ONLY
        . "$ONB/device/install.sh"
        steps; cp "$RC" "$RC.once"
        steps; cmp -s "$RC" "$RC.once" || { echo "FAIL $start: 第二遍又改了"; exit 1; }
    ) || { fail=1; continue; }
    m=$(rc_mode "$T/$start.d/rc.local")
    [ "$m" = sentinel ] || { echo "FAIL $start: rc_mode = $m"; cat "$T/$start.d/rc.local"; fail=1; continue; }
    if [ $start = sentinel ] && ! cmp -s "$T/sentinel" "$T/$start.d/rc.local"; then
        echo "FAIL sentinel: 已是哨兵的 rc.local 被改了"; diff "$T/sentinel" "$T/$start.d/rc.local"; fail=1; continue
    fi
    sh -n "$T/$start.d/rc.local" || { echo "FAIL $start: sh -n"; fail=1; continue; }
    [ "$(grep -c '^/etc/init.d/luyaz-core start$' "$T/$start.d/rc.local")" = 1 ] || { echo "FAIL $start: luyaz-core 行不是正好一行"; fail=1; continue; }
    echo "ok   $start"
done

# 故障注入（docs/designs/fault-injection.md K1/K2）：rc_write 的 cat / sync / mv 各自失败时
# rc.local 逐字节不变、临时文件清掉、退出非 0；上次在 mv 前被杀留下的临时文件，下一遍照常。
mkdir -p "$T/shim"
for step in cat sync mv; do
    d="$T/fail-$step"; mkdir -p "$d"; cp "$T/vendor" "$d/rc.local"
    case $step in
    cat) printf '%s\n' '#!/bin/sh' '[ "$1" = /tmp/rc.local.new ] && { head -c 7 "$1"; exit 1; }' 'exec /bin/cat "$@"' ;;
    sync) printf '%s\n' '#!/bin/sh' 'exit 1' ;;
    mv) printf '%s\n' '#!/bin/sh' 'case "$*" in *u60kit.tmp*) exit 1 ;; esac' 'exec /bin/mv "$@"' ;;
    esac > "$T/shim/$step"
    chmod 755 "$T/shim/$step"
    (
        KIT_RC="$d/rc.local" KIT_STATE="$d/state" KIT_LIB_ONLY=1 PATH="$T/shim:$PATH"
        export KIT_RC KIT_STATE KIT_LIB_ONLY PATH
        . "$ONB/device/install.sh"
        steps
    ) >/dev/null 2>&1 && { echo "FAIL $step 失败却退出 0"; fail=1; }
    rm -f "$T/shim/$step"
    cmp -s "$T/vendor" "$d/rc.local" || { echo "FAIL $step 失败后 rc.local 变了"; fail=1; continue; }
    [ ! -e "$d/rc.local.u60kit.tmp" ] || { echo "FAIL $step 失败后临时文件还在"; fail=1; continue; }
    echo "ok   $step 失败：rc.local 不变"
done
d="$T/stale"; mkdir -p "$d"; cp "$T/vendor" "$d/rc.local"
printf 'exit 0\n# half' > "$d/rc.local.u60kit.tmp"
(
    KIT_RC="$d/rc.local" KIT_STATE="$d/state" KIT_LIB_ONLY=1
    export KIT_RC KIT_STATE KIT_LIB_ONLY
    . "$ONB/device/install.sh"
    steps
) >/dev/null 2>&1 || { echo "FAIL 残留临时文件：重跑失败"; fail=1; }
if [ "$(rc_mode "$d/rc.local")" = sentinel ] && [ ! -e "$d/rc.local.u60kit.tmp" ] && ! grep -q '# half' "$d/rc.local"; then
    echo "ok   残留临时文件：重跑正常"
else
    echo "FAIL 残留临时文件：结果不对"; fail=1
fi

[ -n "${SHOW:-}" ] && for s in vendor sentinel; do echo "== $s"; cat "$T/$s.d/rc.local"; done
exit $fail
