#!/bin/sh
# 装机包改 rc.local 的离线测试（不碰设备）：三种起点（原厂、旧装法 u60-uid + u60-guard、已是哨兵）
# 走一遍 do_devui 改 rc.local 的那几步（install-autostart 放回 start.sh 钩子 → 核心那一行 → rc_owner），
# 旧装法的 zwrt-datad 行原地换成 luyaz-core（Luya-Z 改名 E）；结果必须过 u60-ship.sh 的 rc_mode = sentinel，已是哨兵的那份要原样不变，再跑一遍也不变。
#   sh onboard/test/kit-rc.sh      # 用本仓库的 touch-ui/（Phase 10 10f）
set -u
ONB=$(cd "$(dirname "$0")/.." && pwd)
SHIP=${SHIP_SH:-$ONB/../touch-ui/scripts/u60-ship.sh}
[ -f "$SHIP" ] || { echo "没有 $SHIP" >&2; exit 2; }
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
sed -n '/^rc_mode() {/,/^}/p' "$SHIP" > "$T/rc_mode.sh"
. "$T/rc_mode.sh"
HOOK='[ -x /data/plugins/u60pro-devui/start.sh ] && sh /data/plugins/u60pro-devui/start.sh >/tmp/u60pro-boot.log 2>&1 & # u60pro_devui'
REC='sh /data/luyaz/ship/luyaz-recover.sh; [ $? = 10 ] && exec /bin/sh /etc/rc.local   # luyaz recover：开机先收尾上次没做完的 u60 ship'
SENT='/etc/init.d/luyaz-sentinel start   # u60 ship sentinel: the screen owner and watchdog (was u60-uid + u60-guard)'
fail=0

printf '%s\n' '#!/bin/sh' '# vendor' "$REC" 'exit 0' > "$T/vendor"
printf '%s\n' '#!/bin/sh' 'sh /data/local/tmp/start_dropbear.sh' "$REC" '/etc/init.d/zwrt-datad start' \
    '/etc/init.d/u60-uid start' '/etc/init.d/u60-guard start' 'sh /data/tailscale/start.sh' \
    '[ -f /data/example-svc/disabled ] || /etc/init.d/example-svc start' 'exit 0' > "$T/legacy"
printf '%s\n' '#!/bin/sh' 'sh /data/local/tmp/start_dropbear.sh' "$REC" '/etc/init.d/luyaz-core start' \
    "$SENT" 'sh /data/tailscale/start.sh' '[ -f /data/example-svc/disabled ] || /etc/init.d/example-svc start' 'exit 0' > "$T/sentinel"

steps() { # 同 do_devui：install-autostart 的钩子（去旧钩子、exit 0 前加）→ 核心行 → rc_owner
    grep -v 'u60pro_devui' "$RC" | awk -v h="$HOOK" '/^exit 0/ && !d { print h; d = 1 } { print }' > "$RC.h" && mv "$RC.h" "$RC"
    rc_replace "/etc/init.d/luyaz-core start" "/etc/init.d/zwrt-datad start" "/etc/init.d/luyaz-core start" >/dev/null
    rc_owner >/dev/null
}
for start in vendor legacy sentinel; do
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
    grep -q zwrt-datad "$T/$start.d/rc.local" && { echo "FAIL $start: 还有 zwrt-datad 那一行"; fail=1; continue; }
    [ "$(grep -c '^/etc/init.d/luyaz-core start$' "$T/$start.d/rc.local")" = 1 ] || { echo "FAIL $start: luyaz-core 行不是正好一行"; fail=1; continue; }
    echo "ok   $start"
done
[ -n "${SHOW:-}" ] && for s in vendor legacy; do echo "== $s"; cat "$T/$s.d/rc.local"; done
exit $fail
