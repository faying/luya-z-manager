#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# U60 装机包 — 设备端安装脚本（busybox ash）
#
# 不要手动跑：由电脑端 install.sh 把整个包推到 /data/local/tmp/u60-kit，
# 再通过 adb shell 或 ssh 调用本脚本。
#
#   sh device/install.sh ssh admin devui esim     # 按顺序装指定组件（chill 要单独点名）
#   sh device/install.sh status                   # 只打印状态
#
# 约定（和 CLAUDE.md 一致）：
#   • 开机自启只走 /etc/rc.local，绝不 /etc/init.d/<x> disable 原厂守护进程
#   • luyaz-web / luyaz-core / luyaz-sentinel 由 procd 监督（崩溃自动拉起 + 告警）：
#     /etc/init.d/ 放各自的 init 脚本，但**不 enable**（/etc 是 overlay，enable/disable
#     会留 whiteout），由 rc.local 里一行 `/etc/init.d/<名字> start` 拉起
#   • 程序和数据全放 /data（固件升级也保留）；/etc 下只放 rc.local 的钩子
#   • 每次改 rc.local 前先 sh -n 做语法检查，第一次改之前备份原厂版本
# ─────────────────────────────────────────────────────────────────────────────
set -u

KIT=$(cd "$(dirname "$0")/.." && pwd)
P=$KIT/payload
# 测试（onboard/test/device-recover.sh）用 KIT_* 把路径挪进临时目录
RC=${KIT_RC:-/etc/rc.local}
STATE=${KIT_STATE:-/data/luyaz/kit}       # 原厂 rc.local 备份 + 已装版本记录（Luya-Z 改名 E 前是 /data/u60-kit，见下面 COMPONENTS 处）
SHIPD=${KIT_SHIP_DIR:-/data/luyaz/ship}    # u60 ship：事务、清单 manifest.jsonl、开机收尾脚本 luyaz-recover.sh（touch-ui docs/SHIP.md）

log()  { echo "[device] $*"; }
warn() { echo "[device] 注意: $*"; }
die()  { echo "[device] 失败: $*"; exit 1; }

# 装一个文件：先写 .new 再 mv，覆盖正在运行的二进制也不会 "Text file busy"
put() { # <src> <dst> <mode>
    cp "$1" "$2.new" && chmod "$3" "$2.new" && mv -f "$2.new" "$2" || die "写入 $2 失败"
}

# 解压一个目录：先解到 .new，成功了再整体替换
put_tree() { # <tgz> <dst-dir>
    rm -rf "$2.new" && mkdir -p "$2.new" && tar xzf "$1" -C "$2.new" || die "解压到 $2 失败"
    chown -R 0:0 "$2.new" 2>/dev/null
    rm -rf "$2" && mv "$2.new" "$2" || die "替换 $2 失败"
}

backup_rc() {
    [ -f "$STATE/rc.local.orig" ] && return 0
    mkdir -p "$STATE" && cp "$RC" "$STATE/rc.local.orig" && log "原厂 rc.local 已备份到 $STATE/rc.local.orig"
}

# 把改好的候选文件换成 rc.local：sh -n → 同目录临时文件 → sync → mv → sync（照 tools/luyaz after-upgrade）。
# 写到一半掉电，rc.local 要么是旧的、要么是新的，不会半截（半截 = 开机什么都不起）。
# 临时文件必须和 rc.local 在同一个目录：/tmp 是内存盘，从那里 mv 到 /etc 是复制，不是改名。
# cp -p 先复制原文件，权限和属主跟原来一样。sh -n 不过或写不成：原文件不动，退出。
rc_write() { # <candidate>
    _rt="$RC.u60kit.tmp"
    if ! sh -n "$1"; then
        rm -f "$1"
        die "rc.local 语法检查没过，已放弃修改"
    fi
    rm -f "$_rt"
    if [ -e "$RC" ]; then cp -p "$RC" "$_rt"; else : > "$_rt" && chmod 755 "$_rt"; fi &&
        cat "$1" > "$_rt" && sync && mv -f "$_rt" "$RC" && sync || {
        rm -f "$_rt" "$1"
        die "写 rc.local 失败，原文件没动"
    }
    rm -f "$1"
}

# 幂等地在 exit 0 之前插一行；<marker> 已存在就跳过
rc_add() { # <line> <marker>
    grep -qF "$2" "$RC" 2>/dev/null && return 0
    backup_rc
    if grep -q '^exit 0' "$RC"; then
        awk -v ins="$1" '/^exit 0/ && !d { print ins; d=1 } { print }' "$RC" > /tmp/rc.local.new
    else
        { cat "$RC"; echo "$1"; } > /tmp/rc.local.new
    fi
    rc_write /tmp/rc.local.new
    log "rc.local 加入: $1"
}

# 把 rc.local 里含 <old-marker> 的那一行原地换成 <line>；没有就在 exit 0 前加。
# 用于把旧装法（start-stop-daemon / nohup）迁到 procd，不改其它行的顺序。
rc_replace() { # <line> <old-marker> <new-marker>
    if grep -qF "$3" "$RC" 2>/dev/null; then
        # Already migrated. A reinstall can put the old line back (devui's own
        # install script re-adds its start.sh hook): drop it, keep the new one.
        grep -qF "$2" "$RC" 2>/dev/null || return 0
        backup_rc
        awk -v old="$2" '!index($0, old)' "$RC" > /tmp/rc.local.new
        rc_write /tmp/rc.local.new
        log "rc.local 去掉旧行: $2"
        return 0
    fi
    if grep -qF "$2" "$RC" 2>/dev/null; then
        backup_rc
        awk -v old="$2" -v ins="$1" 'index($0, old) && !d { print ins; d=1; next } { print }' "$RC" > /tmp/rc.local.new
        rc_write /tmp/rc.local.new
        log "rc.local 替换: $2 → $1"
    else
        rc_add "$1" "$3"
    fi
}

# ── 设备脚本：/data/luyaz/scripts + /etc/init.d/{luyaz-core,luyaz-sentinel,luyaz-web}
# 和 u60 ship scripts 同一套（u60-ship.sh 的 SCRIPTS_FILES/SCRIPTS_INITS）。admin 和 devui 都要用到
# （核心的 init 也在这里）。Phase 10 10b 起不装 u60-guard.sh、u60-guard/u60-uid 的 init：
# 看门狗和屏幕归哨兵；设备上已有的旧文件留着不删（u60 rollback sentinel 的退回目标）。
# Luya-Z 改名 D1 起脚本进 /data/luyaz/scripts（S）；D4 起执行器也在这里，叫 luyaz-ship.sh（同目录还有 datad-trial.sh）。
S=${KIT_SCRIPTS:-/data/luyaz/scripts}
# Luya-Z 改名 E：新装直接是 Phase 10 布局（核心 /data/luyaz/luyaz-core + /etc/init.d/luyaz-core、状态 /data/luyaz/core、
# 屏幕状态 /data/luyaz/screen，两处都放 .migrated）。读方 E 以后不再认旧布局，所以旧布局的设备（/data/u60d 还在、
# 没有新标记）这里不装：那台要先按 Phase 10 10e 搬家。
CORE_DIR=${KIT_CORE_DIR:-/data/luyaz/core}
SCREEN_DIR=${KIT_SCREEN_DIR:-/data/luyaz/screen}
layout_new() {
    [ -d /data/u60d ] && [ ! -f "$CORE_DIR/.migrated" ] &&
        die "这台的核心还是 Phase 10 之前的旧布局（/data/u60d，没有 $CORE_DIR/.migrated）：先搬家（manager docs/designs/u60-phase10.md 10e），再装"
    [ -d /data/u60-uid ] && [ ! -f "$SCREEN_DIR/.migrated" ] &&
        die "这台的屏幕状态还在旧的 /data/u60-uid（没有 $SCREEN_DIR/.migrated）：先让 10h 起的哨兵跑一次把它搬过去，再装"
    mkdir -p "$CORE_DIR" "$SCREEN_DIR" && : >>"$CORE_DIR/.migrated" && : >>"$SCREEN_DIR/.migrated" ||
        die "写不了 $CORE_DIR/.migrated 或 $SCREEN_DIR/.migrated"
}
install_guard() {
    [ -d "$P/guard" ] || die "包里没有 guard/（装机包太旧？）"
    [ -f "$P/guard/luyaz-sentinel.init" ] || die "包里没有 guard/luyaz-sentinel.init（装机包太旧，还是 u60-uid/u60-guard 那套）"
    # D4 之前装的设备（事务、清单还在 /data/u60-ship）：直接写新布局会把它们丢在旧目录没人管
    [ -d /data/u60-ship ] && [ ! -f "$SHIPD/.migrated" ] &&
        die "这台的 ship 目录还是 D4 之前的 /data/u60-ship：先用旧执行器搬家（sh /data/u60-guard/u60-ship.sh move-d4 <电脑上的 unix 时间>），再装"
    layout_new
    mkdir -p "$S"
    for f in alert-lib.sh supervise.sh agent-auth.sh chaos.sh doctor.sh doctor-legacy.sh config-backup.sh wan-sources.sh rpc-acl.sh \
             luyaz-fallback.sh; do
        put "$P/guard/$f" "$S/$f" 755
    done
    put "$P/guard/u60-ship.sh" "$S/luyaz-ship.sh" 755
    put "$P/guard/datad-trial.sh" "$S/datad-trial.sh" 755
    place_recover
    for s in luyaz-core luyaz-sentinel luyaz-web; do
        put "$P/guard/$s.init" "/etc/init.d/$s" 755
        cp "$P/guard/$s.init" "$S/$s.init"
    done
    retire_agent
}

# zte-agent 2026-10-14 起退役：它的网页归 luyaz-web，API 和后台任务归核心。
# 标记 /data/luyaz/core/agent-retired 在、且 zte-agent 进程不在 = 交接清单全归核心（没有它核心对 eSIM、
# 情景、CHILL、定时任务这些一律回 409）；旧 agent 看到它也拒绝启动。设备上已有的 agent 停掉，
# rc.local 里它那一行去掉；/data/zte-agent、/data/admin 留着不删（只是不再用）。
retire_agent() {
    mkdir -p "$CORE_DIR" && touch "$CORE_DIR/agent-retired" || die "写不了 $CORE_DIR/agent-retired"
    [ -x /etc/init.d/zte-agent ] && /etc/init.d/zte-agent stop >/dev/null 2>&1
    [ -f /tmp/zte-agent.pid ] && kill "$(cat /tmp/zte-agent.pid)" 2>/dev/null
    killall zte-agent 2>/dev/null
    for old in "/etc/init.d/zte-agent start" "start_zte_agent.sh"; do
        grep -qF "$old" "$RC" 2>/dev/null || continue
        backup_rc
        awk -v old="$old" '!index($0, old)' "$RC" > /tmp/rc.local.new
        rc_write /tmp/rc.local.new
        log "rc.local 去掉旧行: $old"
    done
}

# ── u60 ship 的开机收尾脚本 /data/luyaz/ship/luyaz-recover.sh（包里叫 u60-recover.sh；touch-ui docs/SHIP.md）──
# 目录里放 .migrated：新装的设备一开始就是 D4 布局（执行器、清单都在 /data/luyaz/ship）。
# 它不随 u60 ship 更新，换它要单独一步（sh -n + 设备上自检 + 用户同意）。所以 admin/devui
# 只在设备上还没有时放一份（先 sh -n、selftest）；已有且不同就不换，提示用 recover 组件。
# rc.local 里调用它的那一行只由 recover 组件加（不在全套里，要单独点名，算用户同意）。
recover_check() { # <file>：sh -n + selftest 都过 = 0
    sh -n "$1" 2>/dev/null && sh "$1" selftest 2>/dev/null | grep -q 'selftest: PASS'
}
recover_put() { # 自检过了才放：临时名 → sync → mv → sync
    recover_check "$P/guard/u60-recover.sh" || die "包里的 u60-recover.sh 在这台设备上 sh -n 或 selftest 不过，没装"
    mkdir -p "$SHIPD" && cp "$P/guard/u60-recover.sh" "$SHIPD/luyaz-recover.sh.new" && chmod 755 "$SHIPD/luyaz-recover.sh.new" \
        && sync && mv -f "$SHIPD/luyaz-recover.sh.new" "$SHIPD/luyaz-recover.sh" && sync || die "写入 $SHIPD/luyaz-recover.sh 失败"
}
place_recover() {
    mkdir -p "$SHIPD" && : >>"$SHIPD/.migrated" || die "写不了 $SHIPD/.migrated"
    [ -f "$P/guard/u60-recover.sh" ] || return 0
    if [ ! -f "$SHIPD/luyaz-recover.sh" ]; then
        recover_put
        log "u60 ship 开机收尾脚本: 已放到 $SHIPD/luyaz-recover.sh（自检通过）；rc.local 那一行要 ./install.sh recover 才加"
    elif ! cmp -s "$P/guard/u60-recover.sh" "$SHIPD/luyaz-recover.sh"; then
        warn "设备上的 $SHIPD/luyaz-recover.sh 和包里的不一样，没换（换它要单独一步：./install.sh recover，先问用户）"
    fi
}

# recover 组件：换上包里的 u60-recover.sh（装成 luyaz-recover.sh，自检过才换），并在 rc.local 里各服务启动行之前加一行调用它
do_recover() {
    [ -f "$P/guard/u60-recover.sh" ] || die "包里没有 guard/u60-recover.sh（装机包太旧？）"
    mkdir -p "$SHIPD" && : >>"$SHIPD/.migrated" || die "写不了 $SHIPD/.migrated"
    if [ -f "$SHIPD/luyaz-recover.sh" ] && cmp -s "$P/guard/u60-recover.sh" "$SHIPD/luyaz-recover.sh"; then
        log "luyaz-recover.sh: 已是包里的版本"
    else
        [ -f "$SHIPD/luyaz-recover.sh" ] && cp "$SHIPD/luyaz-recover.sh" "$SHIPD/luyaz-recover.sh.prev-kit"
        recover_put
        log "luyaz-recover.sh: 已换上（自检通过）"
    fi
    # 「; [ $? = 10 ] && exec」：luyaz-recover.sh 换回了旧 rc.local（退出 10）时重读它，不接着起已经不在的哨兵
    # （同 u60-ship.sh rc_derive、tools/luyaz after-upgrade）
    line="sh $SHIPD/luyaz-recover.sh; [ \$? = 10 ] && exec /bin/sh $RC   # luyaz recover：开机先收尾上次没做完的 u60 ship"
    # 已有一行就不加：新的 luyaz-recover.sh，或 D4 前的 u60-recover.sh（执行器 move-d4 会把它调成新的）
    grep -v '^[[:space:]]*#' "$RC" 2>/dev/null | grep -qE '(luyaz|u60)-recover\.sh' && { log "rc.local: 已有调用 luyaz-recover.sh 的一行"; return 0; }
    backup_rc
    # 插在第一条服务启动行（luyaz-core / zwrt-datad / u60-guard / u60-uid / luyaz-web，旧设备上还有 zte-agent）之前；
    # 都没有就放 exit 0 之前
    awk -v ins="$line" '
        !d && !/^[[:space:]]*#/ && (/\/etc\/init\.d\/(zte-agent|zwrt-datad|luyaz-core|u60-guard|u60-uid|luyaz-web|luyaz-sentinel)[[:space:]]+start/ || /^exit 0/) { print ins; d = 1 }
        { print }
        END { if (!d) print ins }' "$RC" > /tmp/rc.local.new
    rc_write /tmp/rc.local.new
    log "rc.local 加入（在服务启动行之前）: $line"
}

# 设备上有没结束的 u60 ship 事务时，不装会停/换我们组件的东西（admin、devui、recover）：
# 执行器、guard 的兜底和开机收尾都按事务日志里的旧版 md5 干活，装机包插进来会把它们搞乱。
# 读事务日志判断；读不懂（v 不是 1 或 2、没有 end=1、阶段不认识）也算有事务。
# v=2 是 web、guard 的事务日志（目录、/etc 下的文件；touch-ui docs/SHIP.md），阶段的规则和 v=1 一样。
ship_busy() { # 有事务就打印一句原因
    [ -f "$SHIPD/txn" ] || return 0
    _v= _e= _ph= _cp= _tx=
    while IFS= read -r _l; do
        case "$_l" in
            v=*) _v=${_l#v=} ;; end=1) _e=1 ;; phase=*) _ph=${_l#phase=} ;;
            comp=*) _cp=${_l#comp=} ;; txn=*) _tx=${_l#txn=} ;;
        esac
    done < "$SHIPD/txn"
    case "$_v" in 1 | 2) ;; *) _e= ;; esac
    if [ "$_e" != 1 ]; then
        echo "设备上的上机事务日志 $SHIPD/txn 读不懂"
        return 0
    fi
    case "$_ph" in
        done | rolledback | aborted | manifest_pending) ;;
        staged | trial | promote | check | manifest | rollback) echo "设备上有没结束的上机事务 $_tx（$_cp，$_ph）" ;;
        failed) echo "设备上的上机事务 $_tx（$_cp）停在 failed，要人处理" ;;
        *) echo "设备上的上机事务日志阶段看不懂（${_ph:-空}）" ;;
    esac
}

# 装完一个组件，在设备清单里记一条 kind=kit（u60-ship.sh record-kit）。来源在 guard/kit-source
# （build-kit.sh 写的：装机包日期、提交号、格式版本、有没有未提交改动）。记不上只提醒。
# kit-source 里没有这个组件的提交号（datad：build-kit.sh 还不写 datad_*）：记 0000000 并标成有改动，
# 不冒充某个干净提交编的。
kit_record() { # <组件>
    [ -f "$S/luyaz-ship.sh" ] && [ -f "$P/guard/kit-source" ] || { warn "没记进设备清单（包里缺 u60-ship.sh 或 kit-source，装机包太旧？）"; return 0; }
    stamp=$(sed -n 's/^stamp=//p' "$P/guard/kit-source")
    commit=$(sed -n "s/^$1_commit=//p" "$P/guard/kit-source")
    format=$(sed -n "s/^$1_format=//p" "$P/guard/kit-source")
    dirty=$(sed -n "s/^$1_dirty=//p" "$P/guard/kit-source")
    [ -n "$commit" ] || dirty=1
    [ "$dirty" = 1 ] && dirty=dirty || dirty=
    case "${KIT_MAC_TIME:-}" in '' | *[!0-9]*) KIT_MAC_TIME=$(date +%s) ;; esac
    if sh "$S/luyaz-ship.sh" record-kit "$1" "$stamp" "${commit:-0000000}" "${format:-1}" "$KIT_MAC_TIME" $dirty >/dev/null 2>&1; then
        log "设备清单: 记下装机包 $stamp 装的 $1（${commit:-0000000}${dirty:+，有未提交改动}）"
    else
        warn "设备清单没记上 $1（有 u60 ship 事务在进行？看 $SHIPD/ship.log）"
    fi
}

# ── 哨兵 luyaz-sentinel：屏幕主人 + 看门狗（取代 u60-uid 和 u60-guard，manager docs/designs/u60-sentinel.md）
SENT_LINE='/etc/init.d/luyaz-sentinel start   # u60 ship sentinel: the screen owner and watchdog (was u60-uid + u60-guard)'

# rc.local 里的屏幕主人收成一行哨兵：旧的 start.sh 钩子（# u60pro_devui，devui 自己的安装脚本每次都会放回来）、
# u60-uid、u60-guard、已有的哨兵行都去掉，哨兵行放在核心那一行（luyaz-core，旧设备上 zwrt-datad）之后（第一个在它之后的旧行的位置，
# 没有就 exit 0 之前）。结果要过 u60-ship.sh 的 rc_mode（sentinel）：datad、luyaz-recover.sh 都在哨兵之前。
# 写法和 u60-ship.sh rc_derive、tools/luyaz after-upgrade 的一样。已经是这样就不写。
rc_owner() {
    awk -v ins="$SENT_LINE" '
        { l = $0; sub(/[ \t]+$/, "", l); c = l; sub(/[ \t]+#.*$/, "", c) }
        index($0, "u60pro_devui") || c == "/etc/init.d/u60-uid start" || c == "/etc/init.d/u60-guard start" \
            || c == "/etc/init.d/luyaz-sentinel start" { if (dn && !d) { print ins; d = 1 }; next }
        !/^[ \t]*#/ && (c == "/etc/init.d/luyaz-core start" || c == "/etc/init.d/zwrt-datad start") { dn = 1 }
        /^exit 0/ && !d { print ins; d = 1 }
        { print }
        END { if (!d) print ins }' "$RC" > /tmp/rc.local.new
    if cmp -s /tmp/rc.local.new "$RC"; then rm -f /tmp/rc.local.new; return 0; fi
    backup_rc
    rc_write /tmp/rc.local.new
    log "rc.local: 屏幕和看门狗归哨兵（一行 /etc/init.d/luyaz-sentinel start）"
}

# 停掉旧的屏幕主人和看门狗（只 stop，绝不 disable）。哨兵和 u60-uid 停下时都把界面留在屏上。
owners_stop() {
    for s in luyaz-sentinel u60-uid u60-guard; do
        [ -x "/etc/init.d/$s" ] && /etc/init.d/$s stop >/dev/null 2>&1
    done
    killall u60-uid 2>/dev/null
    return 0
}

# 起哨兵，等它把刚装的界面放上屏：哨兵在 procd 下、心跳说 ours、界面进程的 /proc/<pid>/exe 就是刚装的。
# 最多 60 秒；没确认只提醒，之后 do_devui 照旧检查界面进程在不在。
sentinel_start() { # <界面程序>
    _um=$(md5sum <"$1" | cut -d' ' -f1)
    # 这是一次有人主动做的安装，不是崩溃：清掉启动计数（哨兵的 /data/luyaz/screen）。否则 10 分钟内装两三次，
    # 每次都算一次失败的启动，第三次就放弃、交还原厂界面（2026-09-23 实际遇到）。
    rm -f "$SCREEN_DIR/attempts" "$SCREEN_DIR/gave-up"
    /etc/init.d/luyaz-sentinel start
    i=0
    while [ $i -lt 60 ]; do
        _pu=$(pidof luyaz-ui | awk '{ print $1 }')
        if svc_running luyaz-sentinel && [ "$(cut -d' ' -f3 /tmp/luyaz-sentinel.hb 2>/dev/null)" = ours ] \
           && [ -n "$_pu" ] && [ "$(md5sum < "/proc/$_pu/exe" 2>/dev/null | cut -d' ' -f1)" = "$_um" ]; then
            return 0
        fi
        sleep 1; i=$((i + 1))
    done
    warn "60 秒内没确认哨兵把新界面放上屏（看 /tmp/luyaz-sentinel.log、/tmp/luyaz-sentinel.hb）"
}

svc_running() { # <name>
    ubus call service list "{\"name\":\"$1\"}" 2>/dev/null | jsonfilter -e "@[\"$1\"].instances.*.running" 2>/dev/null | grep -q true
}

# ── SSH：dropbear 装到 /data/ssh，端口 2222，公钥登录 ─────────────────────────
do_ssh() {
    [ -s "$P/authorized_keys" ] || die "包里没有公钥（authorized_keys）"
    mkdir -p /data/ssh && chmod 700 /data/ssh
    put "$P/dropbear" /data/ssh/dropbear 755
    # OpenWrt 的 dropbear 是多合一程序，按调用名分派，dropbearkey 就是个软链
    ln -sf dropbear /data/ssh/dropbearkey

    touch /data/ssh/authorized_keys
    while IFS= read -r k; do
        [ -n "$k" ] || continue
        grep -qxF "$k" /data/ssh/authorized_keys || echo "$k" >> /data/ssh/authorized_keys
    done < "$P/authorized_keys"
    chmod 600 /data/ssh/authorized_keys

    # host key 也放 /data，固件升级后指纹不变，电脑那边不会报 host key 变了
    if [ ! -s /data/ssh/dropbear_ed25519_host_key ]; then
        /data/ssh/dropbearkey -t ed25519 -f /data/ssh/dropbear_ed25519_host_key >/dev/null 2>&1 \
            || die "生成 host key 失败"
    fi

    cat > /data/local/tmp/start_dropbear.sh <<'EOF'
#!/bin/sh
# 开机由 /etc/rc.local 调用。公钥和 host key 的正本都在 /data/ssh；
# dropbear 只认 /etc/dropbear/authorized_keys，所以每次开机同步过去。
# 加/删公钥请改 /data/ssh/authorized_keys。-s = 只允许密钥登录。
mkdir -p /etc/dropbear && chmod 700 /etc/dropbear
cp /data/ssh/authorized_keys /etc/dropbear/authorized_keys && chmod 600 /etc/dropbear/authorized_keys
start-stop-daemon -S -b -m -p /tmp/dropbear.pid -x /data/ssh/dropbear -- \
    -F -s -p 2222 -r /data/ssh/dropbear_ed25519_host_key
EOF
    chmod 755 /data/local/tmp/start_dropbear.sh

    # 先起、确认真在监听，再写 rc.local：写进开机自启的必须是这台设备上验证过能起来的东西
    if netstat -ltn 2>/dev/null | grep -q ':2222 '; then
        # 已经在跑（比如这次就是 ssh 进来重装的）：只同步公钥，不动进程
        cp /data/ssh/authorized_keys /etc/dropbear/authorized_keys 2>/dev/null
        log "SSH: 2222 端口已在监听，公钥已同步"
    else
        sh /data/local/tmp/start_dropbear.sh
        i=0
        while [ $i -lt 10 ] && ! netstat -ltn 2>/dev/null | grep -q ':2222 '; do sleep 1; i=$((i + 1)); done
        if ! pidof dropbear >/dev/null; then
            die "dropbear 进程没起来（/data/ssh/dropbear 在这台设备上跑不了？），rc.local 没改"
        fi
        netstat -ltn 2>/dev/null | grep -q ':2222 ' || die "dropbear 进程在、但 10 秒内没在 2222 端口监听，rc.local 没改"
        log "SSH: dropbear 已在 2222 端口启动"
    fi
    rc_add "sh /data/local/tmp/start_dropbear.sh" "start_dropbear.sh"

    fota_off
}

# 固件升级会覆盖 /etc/rc.local（自启全丢），而且 B28 起中兴封了开 ADB 的接口，
# 升上去就再也回不来，所以装 SSH 时一并关掉自动升级。关闭 = 两个开关都是关的（项目硬规则的基准，
# 同 tools/u60-cfgkit.py 的 FOTA_OFF、doctor.sh 的检查）：dm_update_mode=0 且 TURNOFFPOLLING=1。
# 这里只会把它们往「关」写，绝不往「开」写。设完读回两个，不对就提醒。
fota_vals() { echo "dm_update_mode=$(uci -q get zwrt_zte_dm.dm_update.dm_update_mode) TURNOFFPOLLING=$(uci -q get zwrt_zte_dm.dm_update.TURNOFFPOLLING)"; }
fota_is_off() { [ "$(fota_vals)" = "dm_update_mode=0 TURNOFFPOLLING=1" ]; }
fota_off() {
    if [ "$(uci -q get zwrt_zte_dm.dm_update.dm_update_mode)" != 0 ]; then
        roam=$(uci -q get zwrt_zte_dm.dm_update.dm_update_roam_permission)
        itime=$(uci -q get zwrt_zte_dm.dm_update.dm_install_time)
        ubus call zwrt_zte_dm set_update_mode \
            "{\"dm_update_mode\":\"0\",\"dm_update_roam_permission\":\"${roam:-1}\",\"dm_install_time\":\"$itime\"}" \
            >/dev/null 2>&1
    fi
    if [ "$(uci -q get zwrt_zte_dm.dm_update.TURNOFFPOLLING)" != 1 ]; then
        uci -q set zwrt_zte_dm.dm_update.TURNOFFPOLLING=1 && uci -q commit zwrt_zte_dm
    fi
    if fota_is_off; then
        log "固件自动升级: 已关闭（$(fota_vals)）"
    else
        warn "没能确认关掉自动升级（读回 $(fota_vals)），请到网页后台「系统设置 → 软件升级」手动关"
    fi
}

# ── 高级后台：luyaz-web（:9090 网页，数据和写操作都经核心 127.0.0.1:9460）───────────
# 只连本机 /login，不碰核心：全套里 admin 在 devui（装核心）之前。
WEB_ENV=/data/luyaz/web.env   # Phase 10 R6（原 /data/zte-agent.env；luyaz-web 照 --password-file 的旧名找新处）
LW=/data/luyaz
web_code() { # <curl 参数…> → HTTP 状态码（连不上 = 000）
    /usr/bin/curl -s -m 10 -o /dev/null -w '%{http_code}' "$@" 2>/dev/null
}
# 鉴权三项（取代 agent-auth.sh verify，它查的是 agent 的 /api/auth/login 和 Bearer）：
#   1. 没登录打开首页：不给页面（转去 /login）
#   2. 密码文件里的密码能登录：200 + lw_sid 会话 cookie
#   3. 空密码登录不了：没有 cookie
# 密码只从 600 的临时文件读（--data-urlencode name@file），不进命令行。
web_verify() {
    _pw=$(grep -m1 '^ZTE_AGENT_PASSWORD=' "$WEB_ENV" 2>/dev/null) || { warn "$WEB_ENV 里没有密码"; return 1; }
    _w=$(mktemp -d) || return 1
    chmod 700 "$_w"
    (umask 077; printf '%s' "${_pw#ZTE_AGENT_PASSWORD=}" > "$_w/pw"; : > "$_w/empty")
    _u=http://127.0.0.1:9090
    _f=0
    c=$(web_code "$_u/")
    if [ "$c" != 200 ] && [ "$c" != 000 ]; then log "  ok   没登录打开首页 → $c"; else warn "没登录打开首页 → $c（应当不给页面）"; _f=1; fi
    c=$(web_code -D "$_w/h" -H "Origin: $_u" --data-urlencode "password@$_w/pw" "$_u/login")
    if [ "$c" = 200 ] && grep -qi '^set-cookie: lw_sid=[^;]' "$_w/h"; then log "  ok   后台密码能登录"; else warn "后台密码登录 → $c（没拿到会话）"; _f=1; fi
    rm -f "$_w/h"
    c=$(web_code -D "$_w/h" -H "Origin: $_u" --data-urlencode "password@$_w/empty" "$_u/login")
    if grep -qi '^set-cookie: lw_sid=[^;]' "$_w/h" 2>/dev/null; then warn "空密码登录成功了（→ $c）"; _f=1; else log "  ok   空密码登录不了（→ $c）"; fi
    rm -rf "$_w"
    return $_f
}

do_admin() {
    install_guard   # 也装 /etc/init.d/luyaz-web，并让 zte-agent 退役（retire_agent）
    [ -x /etc/init.d/luyaz-web ] && /etc/init.d/luyaz-web stop >/dev/null 2>&1
    i=0
    while { pidof zte-agent luyaz-web >/dev/null || netstat -ltn 2>/dev/null | grep -q ':9090 '; } && [ $i -lt 10 ]; do
        sleep 1; i=$((i + 1))
    done
    mkdir -p "$LW"
    put "$P/luyaz-web" "$LW/luyaz-web" 755
    # 运营商 logo：包里有才装（开源版没有，首页就不显示 logo）
    if [ -d "$P/operator-logos" ]; then
        mkdir -p "$LW/operator-logos"
        for f in "$P"/operator-logos/*; do put "$f" "$LW/operator-logos/$(basename "$f")" 644; done
    fi

    # 密码放 $WEB_ENV（600；agent-auth.sh 看到 $CORE_DIR/.migrated 就写这里）。luyaz-web 每次登录都读它，
    # 命令行里只放路径（ubus call service list 会列出命令行和 env）。没有密码它拒绝一切登录。
    #  • 电脑端带了新密码（agent.env）→ 用新的，覆盖旧的
    #  • 没带 → 沿用 $WEB_ENV；再没有就从旧装法的启动脚本迁过来
    if [ -s "$P/agent.env" ]; then
        rm -f "$WEB_ENV"
        AGENT_AUTH_OLD="$P/agent.env" sh "$S/agent-auth.sh" migrate || die "写入后台密码失败"
    elif [ ! -s "$WEB_ENV" ]; then
        [ -f /data/local/tmp/start_zte_agent.sh ] || die "没有后台密码：设备上既没有 $WEB_ENV 也没有旧的启动脚本"
        sh "$S/agent-auth.sh" migrate || die "从旧启动脚本迁移后台密码失败"
    fi
    rc_add "/etc/init.d/luyaz-web start" "/etc/init.d/luyaz-web start"

    /etc/init.d/luyaz-web start
    # :9090 被占着时 luyaz-web 每 10 秒重试一次绑定
    i=0
    while [ "$(web_code http://127.0.0.1:9090/login)" != 200 ] && [ $i -lt 20 ]; do
        sleep 1; i=$((i + 1))
    done
    svc_running luyaz-web && pidof luyaz-web >/dev/null || die "luyaz-web 没起来，看 /tmp/luyaz-web.log 和 logread"
    [ "$(web_code http://127.0.0.1:9090/login)" = 200 ] || die "管理网页打不开（http://127.0.0.1:9090/login），看 /tmp/luyaz-web.log"
    web_verify || die "后台鉴权检查没过（见上）——不要让设备带着开放的后台运行"
    log "高级后台: luyaz-web 已启动（:9090，procd 监督，鉴权三项已验证）"
    # Wi-Fi 兜底看门狗和告警归哨兵，随 devui 装（Phase 10 10b 起不再装 u60-guard）
    kit_record lweb
}

# ── devui 触屏界面 + 核心 luyaz-core + 哨兵 ──────────────────────────────
do_devui() {
    D=/data/plugins/u60pro-devui
    # Phase 10 R5: the program and its fonts in /data/luyaz/ui; Luya-Z D5: devui.conf and the
    # logos too (only start.sh and the *.orig-backup stay in $D)
    UI=/data/luyaz/ui
    mkdir -p "$D" "$UI"
    [ -f "$P/luyaz-sentinel" ] || die "包里没有 luyaz-sentinel（装机包太旧？）"
    install_guard
    # The screen owner (sentinel, or u60-uid/u60-guard on an older install) would read the
    # kill below as a crash (alert, relaunch race). Stopping it leaves the UI running; the
    # sentinel is started at the end.
    owners_stop
    for s in luyaz-core zwrt-datad; do
        [ -x "/etc/init.d/$s" ] && /etc/init.d/$s stop >/dev/null 2>&1
    done
    killall -9 u60pro-devui luyaz-ui luyaz-core zwrt-datad 2>/dev/null
    sleep 1
    put "$P/devui/luyaz-ui" "$UI/luyaz-ui" 755
    put "$P/devui/start.sh" "$D/start.sh" 755
    mkdir -p /data/luyaz
    put "$P/luyaz-sentinel" /data/luyaz/luyaz-sentinel 755
    put "$P/devui/luyaz-core" /data/luyaz/luyaz-core 755
    # 数字字体（Nunito，OFL）：装机包里有才装；没有时触屏退回 /usr/ui/fonts/Roboto.ttf
    if [ -d "$P/devui/fonts" ]; then
        mkdir -p "$UI/fonts"
        for f in "$P"/devui/fonts/*; do put "$f" "$UI/fonts/$(basename "$f")" 644; done
    fi
    # 首页运营商 logo：装机包里有才装；没有时首页不显示 logo
    if [ -d "$P/devui/operator-logos" ]; then
        mkdir -p "$UI/operator-logos"
        for f in "$P"/devui/operator-logos/*.png; do put "$f" "$UI/operator-logos/$(basename "$f")" 644; done
    fi

    backup_rc
    # devui 仓库自带的安装脚本：清理旧版残留、保留原厂 zte_topsw_devui 做开机早期的
    # 屏幕/触摸初始化，并在 rc.local 里放一行 start.sh（带 # u60pro_devui 标记）。
    # 它认得 procd 装法的核心，不会去停它。
    AUTOSTART_RC="$RC" sh "$P/devui/install-autostart.sh" >/tmp/u60-kit-devui.log 2>&1
    sh -n "$RC" || die "rc.local 语法检查没过（devui 安装脚本改坏了？原厂备份在 $STATE/rc.local.orig）"

    # 核心归 procd（崩了拉起，崩溃记录和告警由它自己的 svcwatch 做）。旧装法的 zwrt-datad 那一行原地换掉。
    # 先加它：哨兵那一行要排在它后面（rc_owner）
    rc_replace "/etc/init.d/luyaz-core start" "/etc/init.d/zwrt-datad start" "/etc/init.d/luyaz-core start"
    # 屏幕和看门狗归哨兵（procd 监督）：拉起/接管/放弃后交还原厂界面/长按右下角回来，Wi-Fi 兜底和告警。
    # start.sh 那一行、旧的 u60-uid、u60-guard 两行都换成一行哨兵
    rc_owner
    /etc/init.d/luyaz-core start
    sentinel_start "$UI/luyaz-ui"

    i=0
    while [ $i -lt 10 ]; do
        pidof luyaz-ui >/dev/null && pidof luyaz-core >/dev/null && break
        sleep 1; i=$((i + 1))
    done
    pidof luyaz-ui >/dev/null || die "devui 没起来，看 /tmp/luyaz-ui.log、/tmp/luyaz-sentinel.log 和 /tmp/u60-kit-devui.log"
    [ "$(pidof luyaz-ui u60pro-devui | wc -w)" -le 1 ] || warn "有不止一个触屏界面在跑"
    svc_running luyaz-sentinel || warn "哨兵没在 procd 下运行，看 /tmp/luyaz-sentinel.log 和 logread"
    pidof luyaz-core >/dev/null || warn "核心 luyaz-core 没起来，屏幕会没有数据，看 /tmp/luyaz-core.log 和 logread"
    [ "$(pidof luyaz-core zwrt-datad | wc -w)" -le 1 ] || warn "有不止一个核心在跑"
    log "devui: 屏幕界面已接管"
    kit_record touch
    kit_record sentinel
    kit_record datad
}

# ── eSIM：lpac（qmi_qrtr）装到 /data/esim，后台和屏幕都靠它读写卡 ───────────────
do_esim() {
    put_tree "$P/esim.tgz" /data/esim
    out=$(/data/esim/lpac.sh chip info 2>&1)
    eid=$(echo "$out" | sed -n 's/.*"eidValue":"\([0-9A-Fa-f]*\)".*/\1/p')
    if [ -n "$eid" ]; then
        log "eSIM: 读到 eUICC 卡，EID $eid"
    else
        log "eSIM: 组件已装，但没读到 eUICC 卡（现在插的是普通 SIM 的话这是正常的）"
    fi
}

# ── CHILL：mihomo 透明代理 + zashboard，装到 /data/chill（设计见 docs/CHILL.md）──────
# 两条路：
#   • 设备上已经有 CHILL（更新）：只换程序、脚本、模板、规则集和面板；chill.env（订阅地址）、
#     providers/、cache.db、confirmed 不碰。改动的文件先留一份 .prev；原来在跑就重启核心，
#     60 秒内回不到 running 就换回 .prev 再起一次，然后报错。
#   • 第一次装：再建防火墙区、装 init.d、rc.local 加一行，但**不启动**——没有订阅地址起不来，
#     首次要 safe-start（5 分钟死人开关）由人来确认网络正常。
C=/data/chill
CHILL_FILES="bin/mihomo chill.sh chill.init template.yaml"
chill_state() { sed -n 's/.*"state":"\([a-z]*\)".*/\1/p' /tmp/chill.state 2>/dev/null; }
chill_field() { tr -d '\n' < /tmp/chill.state 2>/dev/null | sed -n "s/.*\"$1\":\"*\([^\",}]*\).*/\1/p"; }
# 新核心真的起来了：状态 running、started_at 和停之前不同（停机时 chill.sh 不改写状态文件，
# 旧的 "running" 会一直留着）、core_pid 就是现在唯一的 mihomo；过 70 秒（至少一次监督循环的
# API 检查）再看一次还是同一个核心。
chill_wait_running() { # <秒> <停之前的 started_at>
    i=0
    while [ $i -lt "$1" ]; do
        p=$(pidof mihomo)
        if [ "$(chill_state)" = running ] && [ "$(chill_field started_at)" != "$2" ] \
           && [ -n "$p" ] && [ "$p" = "$(chill_field core_pid)" ]; then
            sleep 70
            [ "$(chill_state)" = running ] && [ "$(pidof mihomo)" = "$p" ] && return 0
            return 1
        fi
        sleep 2; i=$((i + 2))
    done
    return 1
}
do_chill() {
    [ -d "$P/chill" ] || die "包里没有 chill/（装机包太旧？）"
    fresh=0; [ -x "$C/chill.sh" ] || fresh=1
    was_running=0; svc_running chill && was_running=1
    mkdir -p "$C/bin" "$C/ruleset" "$C/run" "$C/providers" "$STATE"

    changed=
    for f in $CHILL_FILES; do
        if [ -f "$C/$f" ] && cmp -s "$P/chill/$f" "$C/$f"; then continue; fi
        [ -f "$C/$f" ] && cp -p "$C/$f" "$C/$f.prev"
        changed="$changed $f"
    done
    for f in $changed; do put "$P/chill/$f" "$C/$f" 755; done
    chmod 644 "$C/template.yaml"
    for f in "$P"/chill/ruleset/*; do put "$f" "$C/ruleset/$(basename "$f")" 644; done
    put_tree "$P/chill/ui.tgz" "$C/ui"
    put "$C/chill.init" /etc/init.d/chill 755
    put "$P/chill/chill.env.example" "$C/chill.env.example" 644

    if [ $fresh = 1 ]; then
        # restore_dns 要用的原样 dhcp 配置。只在第一次拿：之后它可能已经指向 mihomo
        [ -f "$C/dhcp.backup" ] || cp /etc/config/dhcp "$C/dhcp.backup"
        [ -f "$STATE/firewall.orig" ] || cp /etc/config/firewall "$STATE/firewall.orig"
        # UDP 从 br-lan 进 chill0 要过 FORWARD，默认 DROP，不放行就是全屋断网
        if ! uci -q show firewall | grep -q "name='chill'"; then
            z=$(uci add firewall zone)
            uci set firewall.$z.name=chill
            uci add_list firewall.$z.device=chill0
            uci set firewall.$z.input=ACCEPT
            uci set firewall.$z.output=ACCEPT
            uci set firewall.$z.forward=ACCEPT
            uci set firewall.$z.masq=0
            f=$(uci add firewall forwarding); uci set firewall.$f.src=chill; uci set firewall.$f.dest=lan
            f=$(uci add firewall forwarding); uci set firewall.$f.src=lan; uci set firewall.$f.dest=chill
            uci commit firewall
            /etc/init.d/firewall reload >/dev/null 2>&1
            log "CHILL: 已建防火墙区 chill"
        fi
    fi
    # 自启只走 rc.local（不 enable init.d，见 chill.init 开头）。关掉 CHILL = touch $C/disabled
    rc_add "[ -f $C/disabled ] || /etc/init.d/chill start" "/etc/init.d/chill start"

    if [ $fresh = 1 ]; then
        log "CHILL: 已装好，没有启动。接下来："
        log "  1. cp $C/chill.env.example $C/chill.env && chmod 600 $C/chill.env，填订阅地址；节点快照放 $C/providers/"
        log "  2. sh $C/chill.sh safe-start，网络正常就在 5 分钟内 sh $C/chill.sh confirm"
        return 0
    fi
    if [ $was_running = 0 ]; then
        log "CHILL: 已更新（${changed:- 程序没变}）；原来没在跑，不启动"
        return 0
    fi
    if [ -z "$changed" ]; then
        log "CHILL: 程序、脚本、模板都没变，只更新了规则集和面板，核心不重启"
        return 0
    fi
    # 换了 chill.sh 必须 stop+start，reload 只让 mihomo 重读配置，监督进程还是旧脚本
    t0=$(chill_field started_at)
    /etc/init.d/chill stop >/dev/null 2>&1
    /etc/init.d/chill start
    if chill_wait_running 60 "$t0"; then
        log "CHILL: 已更新并重启（换了:$changed），新核心运行中，70 秒后复查也正常"
        return 0
    fi
    warn "CHILL 更新后 60 秒没回到运行状态（$(chill_state)），换回原来的版本"
    for f in $changed; do [ -f "$C/$f.prev" ] && mv -f "$C/$f.prev" "$C/$f"; done
    put "$C/chill.init" /etc/init.d/chill 755
    t0=$(chill_field started_at)
    /etc/init.d/chill stop >/dev/null 2>&1
    /etc/init.d/chill start
    chill_wait_running 60 "$t0" && die "CHILL 新版起不来，已换回原来的版本并恢复运行，看 /tmp/chill.log" \
        || die "CHILL 换回原来的版本后也没起来（$(chill_state)）。chill.sh 停机时会回滚 DNS，网络是直连；看 /tmp/chill.state 和 /tmp/chill.log"
}

do_status() {
    p() { printf '  %s：%s\n' "$1" "$2"; }   # 中文按字节算宽度，对不齐，就不对齐了
    fw=$(ubus call zwrt_web device_info '{}' 2>/dev/null | sed -n 's/.*"wa_inner_version": *"\([^"]*\)".*/\1/p')
    p "固件" "${fw:-未知}"
    p "SSH" "$(netstat -ltn 2>/dev/null | grep -q ':2222 ' && echo '运行中 (:2222)' || echo 未运行)"
    svc() { svc_running "$1" && echo "（procd 监督）" || { [ -x "/etc/init.d/$1" ] && echo "（procd：未运行）"; }; }
    p "高级后台" "$(pidof luyaz-web >/dev/null && echo '运行中 (:9090)' || echo 未运行)$(svc luyaz-web)$(pidof zte-agent >/dev/null && echo '，旧 zte-agent 还在跑')"
    p "devui" "$({ pidof u60pro-devui || pidof luyaz-ui; } >/dev/null && echo 运行中 || echo 未运行)$([ -f "$SCREEN_DIR/gave-up" ] && echo '，已放弃重试（长按右下角 3 秒恢复）')"
    p "哨兵" "$(pidof luyaz-sentinel >/dev/null && echo 运行中 || echo 未运行)$(svc luyaz-sentinel)$(h=$(cut -d' ' -f3 /tmp/luyaz-sentinel.hb 2>/dev/null) && [ -n "$h" ] && echo "，屏幕：$h")$(svc_running u60-uid && echo '，旧 u60-uid 还在跑')$(svc_running u60-guard && echo '，旧 u60-guard 还在跑')"
    p "核心" "$(pidof luyaz-core >/dev/null && echo 运行中 || echo 未运行)$(svc luyaz-core)$(pidof zwrt-datad >/dev/null && echo '，旧 zwrt-datad 还在跑')"
    p "eSIM 组件" "$([ -x /data/esim/lpac ] && echo 已装 || echo 未装)"
    p "上机收尾" "$([ -f "$SHIPD/luyaz-recover.sh" ] && echo 'luyaz-recover.sh 已放' || echo 'luyaz-recover.sh 没放')$(grep -v '^[[:space:]]*#' "$RC" 2>/dev/null | grep -qE '(luyaz|u60)-recover\.sh' && echo '，rc.local 有那一行' || echo '，rc.local 没有那一行（./install.sh recover）')"
    p "CHILL" "$([ -x /data/chill/chill.sh ] && { s=$(chill_state); echo "已装，${s:-未运行}$(svc chill)$([ -f /data/chill/disabled ] && echo '，已关闭')"; } || echo 未装)"
    p "自动升级" "$(fota_is_off && echo 已关闭 || echo "没有完全关闭（$(fota_vals)）")"
    p "USB 模式" "$(ubus call zwrt_bsp.usb list '{}' 2>/dev/null | sed -n 's/.*"mode": *"\([^"]*\)".*/\1/p')"
    echo "  rc.local 自启:"
    grep -E "start_dropbear|start_zte_agent|u60pro_devui|/etc/init.d/(zte-agent|zwrt-datad|luyaz-core|u60-guard|u60-uid|luyaz-web|luyaz-sentinel|chill) start" "$RC" 2>/dev/null | sed 's/^/    /'
}

# 测试（onboard/test/kit-rc.sh）只要上面的函数
[ -n "${KIT_LIB_ONLY:-}" ] && return 0

[ $# -gt 0 ] || die "用法: sh device/install.sh {ssh|admin|devui|esim|chill|recover|status}..."
COMPONENTS="$*"
# 旧的 /data/u60-kit 里有原厂 rc.local 备份（Luya-Z 改名 E 前的 STATE）：不搬就会在新处把已经改过的 rc.local
# 备份成「原厂」（ssh、recover、chill 都会先 backup_rc）。所以除了只读的 status，一律先要求搬过去
case " $COMPONENTS " in
    *" ssh "* | *" admin "* | *" devui "* | *" esim "* | *" chill "* | *" recover "*)
        [ -d "${KIT_OLD_STATE:-/data/u60-kit}" ] && [ ! -d "$STATE" ] &&
            die "装机记录还在旧的 ${KIT_OLD_STATE:-/data/u60-kit}（里面有原厂 rc.local 备份）：先 mv ${KIT_OLD_STATE:-/data/u60-kit} $STATE，再装"
        ;;
esac
case " $COMPONENTS " in
    *" admin "* | *" devui "* | *" recover "*)
        busy=$(ship_busy)
        [ -n "$busy" ] && die "$busy：先等它结束（电脑上 tools/luyaz status 看；sh $S/luyaz-ship.sh status），这次什么都没装"
        ;;
esac
for c in "$@"; do
    case "$c" in
        ssh)    do_ssh ;;
        admin)  do_admin ;;
        devui)  do_devui ;;
        esim)   do_esim ;;
        recover) do_recover ;;
        chill)  do_chill ;;
        status) do_status ;;
        *) die "不认识的组件: $c" ;;
    esac
done
