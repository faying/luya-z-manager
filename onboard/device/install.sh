#!/bin/sh
# ═════════════════════════════════════════════════════════════════════════════
# Luya-Z 装机包：设备端（busybox ash）。

# 由电脑端 install.sh 调用，不用手动跑：它先把包解到 /data/local/tmp/u60-kit，
# 再经 ssh 执行
#   sh device/install.sh ssh admin ui esim     按给的顺序装这些组件（chill、recover 要单独点名）
#   sh device/install.sh status                   只读，打印各组件现状

# 守则（和项目 CLAUDE.md 一致，不要放宽）：
#   • 原厂服务一律不 /etc/init.d/<x> disable；开机自启只靠 /etc/rc.local
#   • luyaz-web / luyaz-core / luyaz-sentinel 由 procd 监督（崩溃自动拉起 + 告警）：
#     /etc/init.d/ 放各自的 init 脚本，但**不 enable**（/etc 是 overlay，enable/disable
#     会留 whiteout），由 rc.local 里一行 `/etc/init.d/<名字> start` 拉起
#   • 程序、数据都在 /data（升级固件不丢）；/etc 只放 init 脚本和 rc.local 里的启动行
#   • rc.local：第一次改前留一份原厂的，每次写之前 sh -n
#   • 不碰分区；固件自动升级只往「关」的方向写
# ═════════════════════════════════════════════════════════════════════════════
set -u   # 用到没设的变量就报错
KIT=$(cd "$(dirname "$0")/.." && pwd)   # 解包目录（device/ 的上一级）
P=$KIT/payload   # 组件文件都在这里

# 测试（onboard/test/device-recover.sh）用 KIT_* 把路径挪进临时目录
RC=${KIT_RC:-/etc/rc.local}
STATE=${KIT_STATE:-/data/luyaz/kit}       # 原厂 rc.local 备份 + 已装版本记录
SHIPD=${KIT_SHIP_DIR:-/data/luyaz/ship}    # u60 ship：事务、清单 manifest.jsonl、开机收尾脚本 luyaz-recover.sh（touch-ui docs/SHIP.md）

# 提示语言：电脑端用 LUYAZ_LANG=en|zh 传过来；没传（手动跑、测试）按中文
case "${LUYAZ_LANG:-zh}" in en) KLANG=en ;; *) KLANG=zh ;; esac
tx() { if [ "$KLANG" = zh ]; then printf '%s' "$1"; else printf '%s' "$2"; fi; }

# 输出都带 [device] 前缀，电脑端原样转给用户
log()  { echo "[device] $*"; }   # 进度

warn() { echo "[device] $(tx '注意' 'warning'): $*"; }   # 提醒，不中止

die()  { echo "[device] $(tx '失败' 'failed'): $*"; [ -z "${PROG_C:-}" ] || prog_set "$PROG_C" failed; exit 1; }   # 中止整个安装（进度里记下停在哪个组件）

# 换文件不原地覆盖：写同目录的 .new 再改名。正在跑的程序照样能换（不会 Text file busy），
# 中途出错原文件也还在。
put() { # <来源> <目标> <权限>
    cp "$1" "$2.new" && chmod "$3" "$2.new" && mv -f "$2.new" "$2" && return 0
    die "$(tx "写入 $2 失败" "could not write $2")"
}  # put

# 整个目录换新：解到 <目标>.new，解完整了才删旧的、改名
put_tree() { # <tgz> <目标目录>
    _nd="$2.new"
    rm -rf "$_nd"
    { mkdir -p "$_nd" && tar xzf "$1" -C "$_nd"; } || die "$(tx "解压到 $2 失败" "could not extract to $2")"
    chown -R 0:0 "$_nd" 2>/dev/null      # 包是在电脑上打的，属主改回 root
    { rm -rf "$2" && mv "$_nd" "$2"; } || die "$(tx "替换 $2 失败" "could not replace $2")"
}  # put_tree

# 原厂 rc.local 只留一次：之后再改都不覆盖这份
backup_rc() {   # 原厂备份只做一次
    if [ -f "$STATE/rc.local.orig" ]; then return 0; fi
    mkdir -p "$STATE" && cp "$RC" "$STATE/rc.local.orig" && log "$(tx "原厂 rc.local 已备份到 $STATE/rc.local.orig" "stock rc.local backed up to $STATE/rc.local.orig")"   # 第一次改之前
}  # backup_rc

# 把改好的候选文件换成 rc.local：sh -n → 同目录临时文件 → sync → mv → sync（照 tools/luyaz after-upgrade）。
# 写到一半掉电，rc.local 要么是旧的、要么是新的，不会半截（半截 = 开机什么都不起）。
# 临时文件必须和 rc.local 在同一个目录：/tmp 是内存盘，从那里 mv 到 /etc 是复制，不是改名。
# cp -p 先复制原文件，权限和属主跟原来一样。sh -n 不过或写不成：原文件不动，退出。
rc_write() { # <candidate>
    _rt="$RC.u60kit.tmp"
    if ! sh -n "$1"; then
        rm -f "$1"
        die "$(tx 'rc.local 语法检查没过，已放弃修改' 'rc.local failed the syntax check; change dropped')"
    fi
    rm -f "$_rt"
    if [ -e "$RC" ]; then cp -p "$RC" "$_rt"; else : > "$_rt" && chmod 755 "$_rt"; fi &&
        cat "$1" > "$_rt" && sync && mv -f "$_rt" "$RC" && sync || {
        rm -f "$_rt" "$1"
        die "$(tx '写 rc.local 失败，原文件没动' 'could not write rc.local; the original is untouched')"
    }
    rm -f "$1"
}

# rc.local 里加一行：放在第一行 exit 0 前面（没有 exit 0 就接在末尾）。
# 文件里已经有 <标记> 就什么都不做，所以重跑不会加出第二行。
rc_add() { # <要加的行> <标记>
    if grep -qF "$2" "$RC" 2>/dev/null; then return 0; fi
    backup_rc   # 改之前先留原厂的
    cat "$RC" 2>/dev/null | awk -v line="$1" '
        !put && /^exit 0/ { print line; put = 1 }
        { print }
        END { if (!put) print line }' > /tmp/rc.local.new
    rc_write /tmp/rc.local.new
    log "$(tx "rc.local 加入: $1" "rc.local added: $1")"   # 告诉用户加了哪一行
}  # rc_add

# ── 设备脚本：/data/luyaz/scripts + /etc/init.d/{luyaz-core,luyaz-sentinel,luyaz-web}
# 和 u60 ship scripts 同一套（luyaz-ship.sh 的 SCRIPTS_FILES/SCRIPTS_INITS）。admin 和 ui 都要用到
# （核心的 init 也在这里）。执行器 luyaz-ship.sh 和 core-trial.sh 也放在 S。
# 只装 Luya-Z 布局（核心 /data/luyaz/luyaz-core、状态 /data/luyaz/core、屏幕状态 /data/luyaz/screen）；
# 更早布局的设备（install_scripts 开头列出的旧目录还在）这个装机包不认，要先用旧版装机包升级。
S=${KIT_SCRIPTS:-/data/luyaz/scripts}
CORE_DIR=${KIT_CORE_DIR:-/data/luyaz/core}
SCREEN_DIR=${KIT_SCREEN_DIR:-/data/luyaz/screen}

install_scripts() {
    [ -d "$P/scripts" ] || die "$(tx '包里没有 scripts/（装机包太旧？）' 'the kit has no scripts/ (kit too old?)')"
    for d in /data/u60d /data/u60-uid /data/u60-ship /data/plugins/u60pro-devui /data/plugins/zwrt-datad; do  # RESIDUE-OK: 旧布局守卫
        [ -d "$d" ] && die "$(tx "这台还是更早的布局（有 $d）：这个装机包只装 Luya-Z 布局，先用旧版装机包升级" "this device still has an older layout ($d exists): this kit only installs the Luya-Z layout; upgrade with an older kit first")"
    done
    mkdir -p "$S" "$CORE_DIR" "$SCREEN_DIR"
    for f in alert-lib.sh web-auth.sh chaos.sh doctor.sh config-backup.sh wan-sources.sh rpc-acl.sh \
             luyaz-fallback.sh net-tune.sh; do
        put "$P/scripts/$f" "$S/$f" 755
    done
    # the device scripts' version (docs/designs/component-versions.md); older kits have none
    [ ! -f "$P/scripts/VERSION" ] || put "$P/scripts/VERSION" "$S/VERSION" 644
    put "$P/scripts/luyaz-ship.sh" "$S/luyaz-ship.sh" 755
    put "$P/scripts/core-trial.sh" "$S/core-trial.sh" 755
    place_recover
    for s in luyaz-core luyaz-sentinel luyaz-web; do
        put "$P/scripts/$s.init" "/etc/init.d/$s" 755
        cp "$P/scripts/$s.init" "$S/$s.init"
    done
}

# ── u60 ship 的开机收尾脚本 /data/luyaz/ship/luyaz-recover.sh（touch-ui docs/SHIP.md）──
# 它不随 u60 ship 更新，换它要单独一步（sh -n + 设备上自检 + 用户同意）。所以 admin/ui
# 只在设备上还没有时放一份（先 sh -n、selftest）；已有且不同就不换，提示用 recover 组件。
# rc.local 里调用它的那一行只由 recover 组件加（不在全套里，要单独点名，算用户同意）。
recover_check() { # <file>：sh -n + selftest 都过 = 0
    sh -n "$1" 2>/dev/null && sh "$1" selftest 2>/dev/null | grep -q 'selftest: PASS'
}

recover_put() { # 自检过了才放：临时名 → sync → mv → sync
    recover_check "$P/scripts/luyaz-recover.sh" || die "$(tx '包里的 luyaz-recover.sh 在这台设备上 sh -n 或 selftest 不过，没装' "the kit's luyaz-recover.sh fails sh -n or selftest on this device; not installed")"
    mkdir -p "$SHIPD" && cp "$P/scripts/luyaz-recover.sh" "$SHIPD/luyaz-recover.sh.new" && chmod 755 "$SHIPD/luyaz-recover.sh.new" \
        && sync && mv -f "$SHIPD/luyaz-recover.sh.new" "$SHIPD/luyaz-recover.sh" && sync || die "$(tx "写入 $SHIPD/luyaz-recover.sh 失败" "could not write $SHIPD/luyaz-recover.sh")"
}

place_recover() {
    [ -f "$P/scripts/luyaz-recover.sh" ] || return 0
    if [ ! -f "$SHIPD/luyaz-recover.sh" ]; then
        recover_put
        log "$(tx "u60 ship 开机收尾脚本: 已放到 $SHIPD/luyaz-recover.sh（自检通过）；rc.local 那一行要 ./install.sh recover 才加" "u60 ship boot cleanup script: placed at $SHIPD/luyaz-recover.sh (selftest passed); its rc.local line is added only by ./install.sh recover")"
    elif ! cmp -s "$P/scripts/luyaz-recover.sh" "$SHIPD/luyaz-recover.sh"; then
        warn "$(tx "设备上的 $SHIPD/luyaz-recover.sh 和包里的不一样，没换（换它要单独一步：./install.sh recover，先问用户）" "$SHIPD/luyaz-recover.sh on the device differs from the kit's; not replaced (replacing it is a separate step: ./install.sh recover, ask the owner first)")"
    fi
}

# recover 组件：换上包里的 luyaz-recover.sh（装成 luyaz-recover.sh，自检过才换），并在 rc.local 里各服务启动行之前加一行调用它
do_recover() {
    [ -f "$P/scripts/luyaz-recover.sh" ] || die "$(tx '包里没有 scripts/luyaz-recover.sh（装机包太旧？）' 'the kit has no scripts/luyaz-recover.sh (kit too old?)')"
    if [ -f "$SHIPD/luyaz-recover.sh" ] && cmp -s "$P/scripts/luyaz-recover.sh" "$SHIPD/luyaz-recover.sh"; then
        log "$(tx 'luyaz-recover.sh: 已是包里的版本' "luyaz-recover.sh: already the kit's version")"
    else
        [ -f "$SHIPD/luyaz-recover.sh" ] && cp "$SHIPD/luyaz-recover.sh" "$SHIPD/luyaz-recover.sh.prev-kit"
        recover_put
        log "$(tx 'luyaz-recover.sh: 已换上（自检通过）' 'luyaz-recover.sh: replaced (selftest passed)')"
    fi
    # 「; [ $? = 10 ] && exec」：luyaz-recover.sh 换回了旧 rc.local（退出 10）时重读它，不接着起已经不在的哨兵
    # （同 tools/luyaz after-upgrade）
    line="sh $SHIPD/luyaz-recover.sh; [ \$? = 10 ] && exec /bin/sh $RC   # luyaz recover：开机先收尾上次没做完的 u60 ship"
    # 已有一行就不加
    grep -v '^[[:space:]]*#' "$RC" 2>/dev/null | grep -q 'luyaz-recover\.sh' && { log "$(tx 'rc.local: 已有调用 luyaz-recover.sh 的一行' 'rc.local: already has the line that calls luyaz-recover.sh')"; return 0; }
    backup_rc
    # 插在第一条服务启动行（luyaz-core / luyaz-web / luyaz-sentinel）之前；都没有就放 exit 0 之前
    awk -v ins="$line" '
        !d && !/^[[:space:]]*#/ && (/\/etc\/init\.d\/(luyaz-core|luyaz-web|luyaz-sentinel)[[:space:]]+start/ || /^exit 0/) { print ins; d = 1 }
        { print }
        END { if (!d) print ins }' "$RC" > /tmp/rc.local.new
    rc_write /tmp/rc.local.new
    log "$(tx "rc.local 加入（在服务启动行之前）: $line" "rc.local added (before the service start lines): $line")"
}

# 设备上有没结束的 u60 ship 事务时，不装会停/换我们组件的东西（admin、ui、recover）：
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
        tx "设备上的上机事务日志 $SHIPD/txn 读不懂" "cannot read the ship transaction log $SHIPD/txn on the device"; echo
        return 0
    fi
    case "$_ph" in
        done | rolledback | aborted | manifest_pending) ;;
        staged | trial | promote | check | manifest | rollback) tx "设备上有没结束的上机事务 $_tx（$_cp，$_ph）" "an unfinished ship transaction is on the device: $_tx ($_cp, $_ph)"; echo ;;
        failed) tx "设备上的上机事务 $_tx（$_cp）停在 failed，要人处理" "ship transaction $_tx ($_cp) stopped at failed; someone needs to look at it"; echo ;;
        *) tx "设备上的上机事务日志阶段看不懂（${_ph:-空}）" "unknown phase in the ship transaction log (${_ph:-empty})"; echo ;;
    esac
}

# 装完一个组件，在设备清单里记一条 kind=kit（luyaz-ship.sh record-kit）。来源在 scripts/kit-source
# （build-kit.sh 写的：装机包日期、提交号、格式版本、有没有未提交改动）。记不上只提醒。
# kit-source 里没有这个组件的提交号：记 0000000 并标成有改动，
# 不冒充某个干净提交编的。
kit_record() { # <组件>
    [ -f "$S/luyaz-ship.sh" ] && [ -f "$P/scripts/kit-source" ] || { warn "$(tx '没记进设备清单（包里缺 luyaz-ship.sh 或 kit-source，装机包太旧？）' 'not recorded in the device manifest (the kit lacks luyaz-ship.sh or kit-source; kit too old?)')"; return 0; }
    stamp=$(sed -n 's/^stamp=//p' "$P/scripts/kit-source")
    commit=$(sed -n "s/^$1_commit=//p" "$P/scripts/kit-source")
    format=$(sed -n "s/^$1_format=//p" "$P/scripts/kit-source")
    dirty=$(sed -n "s/^$1_dirty=//p" "$P/scripts/kit-source")
    [ -n "$commit" ] || dirty=1
    [ "$dirty" = 1 ] && dirty=dirty || dirty=
    case "${KIT_MAC_TIME:-}" in '' | *[!0-9]*) KIT_MAC_TIME=$(date +%s) ;; esac
    if sh "$S/luyaz-ship.sh" record-kit "$1" "$stamp" "${commit:-0000000}" "${format:-1}" "$KIT_MAC_TIME" $dirty >/dev/null 2>&1; then
        log "$(tx "设备清单: 记下装机包 $stamp 装的 $1（${commit:-0000000}${dirty:+，有未提交改动}）" "device manifest: recorded $1 from kit $stamp (${commit:-0000000}${dirty:+, with uncommitted changes})")"
    else
        warn "$(tx "设备清单没记上 $1（有 u60 ship 事务在进行？看 $SHIPD/ship.log）" "could not record $1 in the device manifest (a u60 ship transaction running? see $SHIPD/ship.log)")"
    fi
}

# ── 哨兵 luyaz-sentinel：屏幕主人 + 看门狗（manager docs/designs/u60-sentinel.md）
SENT_LINE='/etc/init.d/luyaz-sentinel start   # u60 ship sentinel: the screen owner and watchdog'

# rc.local 里一行哨兵，放在核心那一行（luyaz-core）之后（已有的哨兵行去掉；核心行之后第一个被去掉的行的位置，
# 没有就 exit 0 之前）。结果要过 luyaz-ship.sh 的 rc_mode：核心、luyaz-recover.sh 都在哨兵之前。
# 写法和 tools/luyaz after-upgrade 的一样。已经是这样就不写。
rc_owner() {
    awk -v ins="$SENT_LINE" '
        { l = $0; sub(/[ \t]+$/, "", l); c = l; sub(/[ \t]+#.*$/, "", c) }
        c == "/etc/init.d/luyaz-sentinel start" { if (dn && !d) { print ins; d = 1 }; next }
        !/^[ \t]*#/ && c == "/etc/init.d/luyaz-core start" { dn = 1 }
        /^exit 0/ && !d { print ins; d = 1 }
        { print }
        END { if (!d) print ins }' "$RC" > /tmp/rc.local.new
    if cmp -s /tmp/rc.local.new "$RC"; then rm -f /tmp/rc.local.new; return 0; fi
    backup_rc
    rc_write /tmp/rc.local.new
    log "$(tx 'rc.local: 屏幕和看门狗归哨兵（一行 /etc/init.d/luyaz-sentinel start）' 'rc.local: the sentinel owns the screen and the watchdog (one line /etc/init.d/luyaz-sentinel start)')"
}

# 停掉哨兵（只 stop，绝不 disable）。哨兵停下时把界面留在屏上。
owners_stop() {
    [ -x /etc/init.d/luyaz-sentinel ] && /etc/init.d/luyaz-sentinel stop >/dev/null 2>&1
    return 0
}

# 起哨兵，等它把刚装的界面放上屏：哨兵在 procd 下、心跳说 ours、界面进程的 /proc/<pid>/exe 就是刚装的。
# 最多 60 秒；没确认只提醒，之后 do_ui 照旧检查界面进程在不在。
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
    warn "$(tx '60 秒内没确认哨兵把新界面放上屏（看 /tmp/luyaz-sentinel.log、/tmp/luyaz-sentinel.hb）' 'could not confirm within 60 seconds that the sentinel put the new UI on screen (see /tmp/luyaz-sentinel.log, /tmp/luyaz-sentinel.hb)')"
}

svc_running() { # <name>
    ubus call service list "{\"name\":\"$1\"}" 2>/dev/null | jsonfilter -e "@[\"$1\"].instances.*.running" 2>/dev/null | grep -q true
}

# ── SSH：dropbear 放 /data/ssh，只认密钥，端口 2222 ─────────────────────────
# 自启动脚本 /data/local/tmp/start_dropbear.sh 由 rc.local 调用；这里写出它的内容。
write_dropbear_starter() {
    cat > /data/local/tmp/start_dropbear.sh <<'STARTER'
#!/bin/sh
# Luya-Z：开机时由 /etc/rc.local 调用，起 SSH（dropbear，2222，只认密钥）。
# 公钥以 /data/ssh/authorized_keys 为准，这里每次开机抄到 dropbear 读的 /etc/dropbear；
# host key 也在 /data/ssh，升级固件后指纹不变。
mkdir -p /etc/dropbear && chmod 700 /etc/dropbear   # 运行时目录，内存里（RESIDUE-OK 通用写法）
cp /data/ssh/authorized_keys /etc/dropbear/authorized_keys && chmod 600 /etc/dropbear/authorized_keys   # 公钥正本 → dropbear
start-stop-daemon -S -b -m -p /tmp/dropbear.pid -x /data/ssh/dropbear -- -F -s -p 2222 -r /data/ssh/dropbear_ed25519_host_key   # 后台起、记 pid
STARTER
    chmod 755 /data/local/tmp/start_dropbear.sh   # rc.local 用 sh 调它，可执行位只是方便手动跑
}

ssh_listening() { netstat -ltn 2>/dev/null | grep -q ':2222 '; }

do_ssh() {   # dropbear + 公钥 + 开机自启 + 关自动升级
    [ -s "$P/authorized_keys" ] || die "$(tx '包里没有公钥（authorized_keys）' 'the kit has no public key (authorized_keys)')"   # 电脑端没放公钥
    mkdir -p /data/ssh && chmod 700 /data/ssh   # 只有 root 能进
    put "$P/dropbear" /data/ssh/dropbear 755   # 程序本体
    # 这个 dropbear 是多合一程序，看自己被叫成什么名字决定干什么；dropbearkey 指向它就行
    ln -sf dropbear /data/ssh/dropbearkey   # 同一个程序，换个名字就是 dropbearkey
    # 公钥只追加、不重复，设备上原有的保留
    touch /data/ssh/authorized_keys   # 没有就建个空的
    while IFS= read -r key; do
        case "$key" in '') continue ;; esac
        grep -qxF "$key" /data/ssh/authorized_keys || printf '%s\n' "$key" >> /data/ssh/authorized_keys
    done < "$P/authorized_keys"   # 包里带来的公钥
    chmod 600 /data/ssh/authorized_keys   # dropbear 要求权限够严
    if [ ! -s /data/ssh/dropbear_ed25519_host_key ]; then   # 已有 host key 就沿用（指纹不变）
        /data/ssh/dropbearkey -t ed25519 -f /data/ssh/dropbear_ed25519_host_key >/dev/null 2>&1 || die "$(tx '生成 host key 失败' 'could not create the host key')"
    fi  # do_ssh
    write_dropbear_starter
    # 写进开机自启的，必须是在这台设备上刚刚确认能起来、能监听的东西
    if ssh_listening; then
        # 这次多半就是从 SSH 进来的：进程不动，只把新公钥同步过去
        pidof dropbear >/dev/null || die "$(tx '2222 端口被别的程序占着、不是 dropbear，rc.local 没改' 'port 2222 is taken by another program, not dropbear; rc.local not changed')"
        cp /data/ssh/authorized_keys /etc/dropbear/authorized_keys 2>/dev/null   # 运行中的 dropbear 读这份
        log "$(tx 'SSH: 2222 端口已在监听，公钥已同步' 'SSH: already listening on port 2222; public keys synced')"   # 没动进程
    else  # do_ssh
        sh /data/local/tmp/start_dropbear.sh   # 和开机时同一个启动脚本
        n=0
        until ssh_listening || [ $n -ge 10 ]; do sleep 1; n=$((n + 1)); done
        pidof dropbear >/dev/null || die "$(tx 'dropbear 进程没起来（/data/ssh/dropbear 在这台设备上跑不了？），rc.local 没改' 'dropbear did not start (/data/ssh/dropbear cannot run on this device?); rc.local not changed')"
        ssh_listening || die "$(tx 'dropbear 进程在、但 10 秒内没在 2222 端口监听，rc.local 没改' 'dropbear runs but did not listen on port 2222 within 10 seconds; rc.local not changed')"
        log "$(tx 'SSH: dropbear 已在 2222 端口启动' 'SSH: dropbear started on port 2222')"   # 进程在、端口在听
    fi  # do_ssh
    rc_add "sh /data/local/tmp/start_dropbear.sh" "start_dropbear.sh"   # 确认能起来之后才写自启
    fota_off   # 装 SSH 时顺手关自动升级
}  # do_ssh

# ── 固件自动升级：装 SSH 时一并关掉 ───────────────────────────────────────────
# 为什么：升级会换掉 /etc/rc.local（我们的自启全没了）。
# 「关」= dm_update_mode=0 且 TURNOFFPOLLING=1（项目硬规则的基准，doctor 用的是同一个）。这里只会往「关」写，绝不往「开」写；写完读回两项核对。
fota_vals() { echo "dm_update_mode=$(uci -q get zwrt_zte_dm.dm_update.dm_update_mode) TURNOFFPOLLING=$(uci -q get zwrt_zte_dm.dm_update.TURNOFFPOLLING)"; }

fota_is_off() { [ "$(fota_vals)" = "dm_update_mode=0 TURNOFFPOLLING=1" ]; }

fota_off() {   # 只往「关」写，写完读回
    if [ "$(uci -q get zwrt_zte_dm.dm_update.dm_update_mode)" != 0 ]; then
        # set_update_mode 要三项一起给；另外两项照抄现值（漫游许可没有值时按 1）
        _roam=$(uci -q get zwrt_zte_dm.dm_update.dm_update_roam_permission)
        _when=$(uci -q get zwrt_zte_dm.dm_update.dm_install_time)
        ubus call zwrt_zte_dm set_update_mode "{\"dm_update_mode\":\"0\",\"dm_update_roam_permission\":\"${_roam:-1}\",\"dm_install_time\":\"$_when\"}" >/dev/null 2>&1
    fi
    if [ "$(uci -q get zwrt_zte_dm.dm_update.TURNOFFPOLLING)" != 1 ]; then
        uci -q set zwrt_zte_dm.dm_update.TURNOFFPOLLING=1 && uci -q commit zwrt_zte_dm
    fi
    if fota_is_off; then
        log "$(tx "固件自动升级: 已关闭（$(fota_vals)）" "firmware auto-update: off ($(fota_vals))")"
    else  # fota_off
        warn "$(tx "没能确认关掉自动升级（读回 $(fota_vals)），请到网页后台「系统设置 → 软件升级」手动关" "could not confirm auto-update is off (read back $(fota_vals)); turn it off by hand in the router's web page under System Settings -> Software Upgrade")"
    fi  # fota_off
}  # fota_off

# ── 高级后台：luyaz-web（:9090 网页，数据和写操作都经核心 127.0.0.1:9460）───────────
# 只连本机 /login，不碰核心：全套里 admin 在 ui（装核心）之前。
WEB_ENV=/data/luyaz/web.env
LW=/data/luyaz

web_code() { # <curl 参数…> → HTTP 状态码（连不上 = 000）
    /usr/bin/curl -s -m 10 -o /dev/null -w '%{http_code}' "$@" 2>/dev/null
}

# 鉴权三项（取代 web-auth.sh verify，它查的是 agent 的 /api/auth/login 和 Bearer）：
#   1. 没登录打开首页：不给页面（转去 /login）
#   2. 密码文件里的密码能登录：200 + lw_sid 会话 cookie
#   3. 空密码登录不了：没有 cookie
# 密码只从 600 的临时文件读（--data-urlencode name@file），不进命令行。
web_verify() {
    _pw=$(grep -m1 '^LUYAZ_WEB_PASSWORD=' "$WEB_ENV" 2>/dev/null) ||
        { warn "$(tx "$WEB_ENV 里没有密码" "no password in $WEB_ENV")"; return 1; }
    _w=$(mktemp -d) || return 1
    chmod 700 "$_w"
    (umask 077; printf '%s' "${_pw#LUYAZ_WEB_PASSWORD=}" > "$_w/pw"; : > "$_w/empty")
    _u=http://127.0.0.1:9090
    _f=0
    c=$(web_code "$_u/")
    if [ "$c" != 200 ] && [ "$c" != 000 ]; then log "$(tx "  ok   没登录打开首页 → $c" "  ok   home page without login -> $c")"; else warn "$(tx "没登录打开首页 → $c（应当不给页面）" "home page without login -> $c (should not show the page)")"; _f=1; fi
    c=$(web_code -D "$_w/h" -H "Origin: $_u" --data-urlencode "password@$_w/pw" "$_u/login")
    if [ "$c" = 200 ] && grep -qi '^set-cookie: lw_sid=[^;]' "$_w/h"; then log "$(tx '  ok   后台密码能登录' '  ok   admin web password logs in')"; else warn "$(tx "后台密码登录 → $c（没拿到会话）" "admin web password login -> $c (no session)")"; _f=1; fi
    rm -f "$_w/h"
    c=$(web_code -D "$_w/h" -H "Origin: $_u" --data-urlencode "password@$_w/empty" "$_u/login")
    if grep -qi '^set-cookie: lw_sid=[^;]' "$_w/h" 2>/dev/null; then warn "$(tx "空密码登录成功了（→ $c）" "empty password logged in (-> $c)")"; _f=1; else log "$(tx "  ok   空密码登录不了（→ $c）" "  ok   empty password cannot log in (-> $c)")"; fi
    rm -rf "$_w"
    return $_f
}

do_admin() {   # 管理网页 luyaz-web
    install_scripts   # 也装 /etc/init.d/luyaz-web
    [ -x /etc/init.d/luyaz-web ] && /etc/init.d/luyaz-web stop >/dev/null 2>&1
    i=0
    while { pidof luyaz-web >/dev/null || netstat -ltn 2>/dev/null | grep -q ':9090 '; } && [ $i -lt 10 ]; do
        sleep 1; i=$((i + 1))
    done
    mkdir -p "$LW"
    put "$P/luyaz-web" "$LW/luyaz-web" 755
    # 运营商 logo：包里有才装（开源版没有，首页就不显示 logo）
    if [ -d "$P/operator-logos" ]; then
        mkdir -p "$LW/operator-logos"
        for f in "$P"/operator-logos/*; do put "$f" "$LW/operator-logos/$(basename "$f")" 644; done
    fi
    # 密码放 $WEB_ENV（600）。luyaz-web 每次登录都读它，
    # 命令行里只放路径（ubus call service list 会列出命令行和 env）。没有密码它拒绝一切登录。
    #  • 电脑端带了新密码（web-password.env）→ 用新的，覆盖旧的
    #  • 没带 → 沿用 $WEB_ENV；没有就不装
    if [ -s "$P/web-password.env" ]; then
        rm -f "$WEB_ENV"
        WEB_AUTH_OLD="$P/web-password.env" WEB_AUTH_ENV="$WEB_ENV" sh "$S/web-auth.sh" migrate || die "$(tx '写入后台密码失败' 'could not write the admin web password')"
    elif [ ! -s "$WEB_ENV" ]; then
        die "$(tx "没有后台密码：包里没带新密码，设备上也没有 $WEB_ENV" "no admin web password: the kit has none and $WEB_ENV is not on the device")"
    fi
    rc_add "/etc/init.d/luyaz-web start" "/etc/init.d/luyaz-web start"

    /etc/init.d/luyaz-web start
    # :9090 被占着时 luyaz-web 每 10 秒重试一次绑定
    i=0
    while [ "$(web_code http://127.0.0.1:9090/login)" != 200 ] && [ $i -lt 20 ]; do
        sleep 1; i=$((i + 1))
    done
    svc_running luyaz-web && pidof luyaz-web >/dev/null || die "$(tx 'luyaz-web 没起来，看 /tmp/luyaz-web.log 和 logread' 'luyaz-web did not start; see /tmp/luyaz-web.log and logread')"
    [ "$(web_code http://127.0.0.1:9090/login)" = 200 ] || die "$(tx '管理网页打不开（http://127.0.0.1:9090/login），看 /tmp/luyaz-web.log' 'the admin web does not open (http://127.0.0.1:9090/login); see /tmp/luyaz-web.log')"
    web_verify || die "$(tx '后台鉴权检查没过（见上）——不要让设备带着开放的后台运行' 'the admin web login check failed (see above) -- do not leave the device running with an open admin web')"
    log "$(tx '高级后台: luyaz-web 已启动（:9090，procd 监督，鉴权三项已验证）' 'admin web: luyaz-web started (:9090, under procd, 3 login checks passed)')"
    # Wi-Fi 兜底看门狗和告警归哨兵，随 ui 装
    kit_record lweb
}  # do_admin

# ── ui 触屏界面 + 核心 luyaz-core + 哨兵 ──────────────────────────────
do_ui() {
    # the program, its fonts, ui.conf, the logos, the boot trace and the vendor UI's backup
    UI=/data/luyaz/ui
    mkdir -p "$UI"
    [ -f "$P/luyaz-sentinel" ] || die "$(tx '包里没有 luyaz-sentinel（装机包太旧？）' 'the kit has no luyaz-sentinel (kit too old?)')"
    install_scripts
    # The sentinel would read the
    # kill below as a crash (alert, relaunch race). Stopping it leaves the UI running; the
    # sentinel is started at the end.
    owners_stop
    [ -x /etc/init.d/luyaz-core ] && /etc/init.d/luyaz-core stop >/dev/null 2>&1
    killall -9 luyaz-ui luyaz-core 2>/dev/null
    sleep 1   # 等被杀的进程真正退出
    put "$P/ui/luyaz-ui" "$UI/luyaz-ui" 755
    mkdir -p /data/luyaz
    put "$P/luyaz-sentinel" /data/luyaz/luyaz-sentinel 755
    put "$P/ui/luyaz-core" /data/luyaz/luyaz-core 755
    # 已连设备识别的解析组件：包里有才装，整个目录换新（只有这一个组件，其他策略用内置实现）；
    # 没有时核心照常跑，识别关着
    if [ -f "$P/ui/plugins/plugins.json" ]; then
        _pd=/data/luyaz/core/plugins
        mkdir -p /data/luyaz/core
        rm -rf "$_pd.new"
        { mkdir -p "$_pd.new" && cp "$P"/ui/plugins/* "$_pd.new/"; } || die "$(tx "写入 $_pd 失败" "could not write $_pd")"
        chown -R 0:0 "$_pd.new" 2>/dev/null
        chmod 700 "$_pd.new"; chmod 600 "$_pd.new"/*
        { rm -rf "$_pd" && mv "$_pd.new" "$_pd"; } || die "$(tx "替换 $_pd 失败" "could not replace $_pd")"
    fi
    # OUI 厂商表：包里有才装（公开构建没有）
    if [ -f "$P/ui/lanid/oui.tsv" ]; then
        mkdir -p /data/luyaz/lanid
        put "$P/ui/lanid/oui.tsv" /data/luyaz/lanid/oui.tsv 644
    fi
    # 数字字体（Nunito，OFL）：装机包里有才装；没有时触屏退回 /usr/ui/fonts/Roboto.ttf
    if [ -d "$P/ui/fonts" ]; then
        mkdir -p "$UI/fonts"
        for f in "$P"/ui/fonts/*; do put "$f" "$UI/fonts/$(basename "$f")" 644; done
    fi
    # 首页运营商 logo：装机包里有才装；没有时首页不显示 logo
    if [ -d "$P/ui/operator-logos" ]; then
        mkdir -p "$UI/operator-logos"
        for f in "$P"/ui/operator-logos/*.png; do put "$f" "$UI/operator-logos/$(basename "$f")" 644; done
    fi
    # 原厂 zte_topsw_devui 做开机早期的屏幕/触摸初始化：只 start，绝不 enable/disable
    # （它在固件的开机同步名单里；disable 会留 whiteout = 开机卡 logo）
    /etc/init.d/zte_topsw_devui start >/dev/null 2>&1
    # 核心归 procd（崩了拉起，崩溃记录和告警由它自己的 svcwatch 做）。
    # 先加它：哨兵那一行要排在它后面（rc_owner）
    rc_add "/etc/init.d/luyaz-core start" "/etc/init.d/luyaz-core start"
    # 屏幕和看门狗归哨兵（procd 监督）：拉起/接管/放弃后交还原厂界面/长按右下角回来，Wi-Fi 兜底和告警
    rc_owner
    /etc/init.d/luyaz-core start
    sentinel_start "$UI/luyaz-ui"

    i=0   # 最多等 10 秒
    while [ $i -lt 10 ]; do   # 界面和核心都起来就停
        pidof luyaz-ui >/dev/null && pidof luyaz-core >/dev/null && break
        sleep 1; i=$((i + 1))   # 下一秒再看
    done  # do_ui
    pidof luyaz-ui >/dev/null || die "$(tx 'ui 没起来，看 /tmp/luyaz-ui.log 和 /tmp/luyaz-sentinel.log' 'ui did not start; see /tmp/luyaz-ui.log and /tmp/luyaz-sentinel.log')"
    [ "$(pidof luyaz-ui | wc -w)" -le 1 ] || warn "$(tx '有不止一个触屏界面在跑' 'more than one touch UI is running')"
    svc_running luyaz-sentinel || warn "$(tx '哨兵没在 procd 下运行，看 /tmp/luyaz-sentinel.log 和 logread' 'the sentinel is not running under procd; see /tmp/luyaz-sentinel.log and logread')"
    pidof luyaz-core >/dev/null || warn "$(tx '核心 luyaz-core 没起来，屏幕会没有数据，看 /tmp/luyaz-core.log 和 logread' 'the core luyaz-core did not start, so the screen will have no data; see /tmp/luyaz-core.log and logread')"
    [ "$(pidof luyaz-core | wc -w)" -le 1 ] || warn "$(tx '有不止一个核心在跑' 'more than one core is running')"
    log "$(tx 'ui: 屏幕界面已接管' 'ui: the screen UI has taken over')"
    kit_record touch
    kit_record sentinel
    kit_record core
    # 组件目录是「只记录」的条目：记下现状，u60 status 才不报不一致
    [ -f "$S/luyaz-ship.sh" ] && sh "$S/luyaz-ship.sh" record core-plugins "${KIT_MAC_TIME:-$(date +%s)}" 装机包 >/dev/null 2>&1
}  # do_ui

# ── eSIM：lpac（走 qmi_qrtr）放 /data/esim；后台和触屏读写 eUICC 卡都用它 ─────
do_esim() {   # lpac 组件 + 读一次卡
    put_tree "$P/esim.tgz" /data/esim   # 整个目录换新
    # 装完顺手读一次卡：插的是普通 SIM 时读不到，不算失败
    # 和核心共用一把卡锁（luyaz-esim CARD_FLOCK），免得两边同时动卡；没有 flock 就直接读
    _flock=; command -v flock >/dev/null 2>&1 && _flock="flock -w 60 /var/run/luyaz-esim.lock"
    _eid=$($_flock /data/esim/lpac.sh chip info 2>&1 | sed -n 's/.*"eidValue":"\([0-9A-Fa-f]*\)".*/\1/p')
    if [ -z "$_eid" ]; then
        log "$(tx 'eSIM: 组件已装，但没读到 eUICC 卡（现在插的是普通 SIM 的话这是正常的）' 'eSIM: installed, but no eUICC card was read (normal if a plain SIM is in)')"   # 不算失败
    else
        log "$(tx "eSIM: 读到 eUICC 卡，EID $_eid" "eSIM: eUICC card found, EID $_eid")"
    fi  # do_esim
}  # do_esim

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
    [ -d "$P/chill" ] || die "$(tx '包里没有 chill/（装机包太旧？）' 'the kit has no chill/ (kit too old?)')"
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
            log "$(tx 'CHILL: 已建防火墙区 chill' 'CHILL: firewall zone chill created')"
        fi
    fi
    # 自启只走 rc.local（不 enable init.d，见 chill.init 开头）。关掉 CHILL = touch $C/disabled
    rc_add "[ -f $C/disabled ] || /etc/init.d/chill start" "/etc/init.d/chill start"

    if [ $fresh = 1 ]; then
        log "$(tx 'CHILL: 已装好，没有启动。接下来：' 'CHILL: installed, not started. Next:')"
        log "$(tx "  1. cp $C/chill.env.example $C/chill.env && chmod 600 $C/chill.env，填订阅地址；节点快照放 $C/providers/" "  1. cp $C/chill.env.example $C/chill.env && chmod 600 $C/chill.env, then fill in the subscription URL; put node snapshots in $C/providers/")"
        log "$(tx "  2. sh $C/chill.sh safe-start，网络正常就在 5 分钟内 sh $C/chill.sh confirm" "  2. sh $C/chill.sh safe-start; if the network works, run sh $C/chill.sh confirm within 5 minutes")"
        return 0
    fi
    if [ $was_running = 0 ]; then
        log "$(tx "CHILL: 已更新（${changed:- 程序没变}）；原来没在跑，不启动" "CHILL: updated (${changed:- programs unchanged}); it was not running, so not started")"
        return 0
    fi
    if [ -z "$changed" ]; then
        log "$(tx 'CHILL: 程序、脚本、模板都没变，只更新了规则集和面板，核心不重启' 'CHILL: programs, scripts and template unchanged; only rule sets and panel updated, core not restarted')"
        return 0
    fi
    # 换了 chill.sh 必须 stop+start，reload 只让 mihomo 重读配置，监督进程还是旧脚本
    t0=$(chill_field started_at)
    /etc/init.d/chill stop >/dev/null 2>&1
    /etc/init.d/chill start
    if chill_wait_running 60 "$t0"; then
        log "$(tx "CHILL: 已更新并重启（换了:$changed），新核心运行中，70 秒后复查也正常" "CHILL: updated and restarted (replaced:$changed); the new core runs and was still fine 70 seconds later")"
        return 0
    fi
    warn "$(tx "CHILL 更新后 60 秒没回到运行状态（$(chill_state)），换回原来的版本" "CHILL not running 60 seconds after the update ($(chill_state)); going back to the previous version")"
    for f in $changed; do [ -f "$C/$f.prev" ] && mv -f "$C/$f.prev" "$C/$f"; done
    put "$C/chill.init" /etc/init.d/chill 755
    t0=$(chill_field started_at)
    /etc/init.d/chill stop >/dev/null 2>&1
    /etc/init.d/chill start
    chill_wait_running 60 "$t0" && die "$(tx 'CHILL 新版起不来，已换回原来的版本并恢复运行，看 /tmp/chill.log' 'the new CHILL does not start; went back to the previous version, which runs again; see /tmp/chill.log')" \
        || die "$(tx "CHILL 换回原来的版本后也没起来（$(chill_state)）。chill.sh 停机时会回滚 DNS，网络是直连；看 /tmp/chill.state 和 /tmp/chill.log" "CHILL did not start even after going back to the previous version ($(chill_state)). chill.sh restores DNS when stopped, so traffic goes direct; see /tmp/chill.state and /tmp/chill.log")"
}

# ── 装机进度：$STATE/progress（电脑端传来 KIT_ID = 装机包 MANIFEST.txt 的 sha256 前 16 位）──
# 每行 键=值：kit components done current step updated。每次都写同目录临时文件再 mv，掉电不会半截。
# 同一个包上次没装完（step 不是 complete）：已装完的组件跳过，从第一个没完成的续；
# 换了包、上次已装完、或 KIT_FRESH=1：从头来。没有 KIT_ID（手动跑、旧电脑端）不记。
PROG=$STATE/progress
PROG_C=
prog_get() { sed -n "s/^$1=//p" "$PROG" 2>/dev/null | head -n 1; }   # <键>

prog_write() { # <kit> <components> <done> <current> <step>
    mkdir -p "$STATE" 2>/dev/null || return 0
    printf 'kit=%s\ncomponents=%s\ndone=%s\ncurrent=%s\nstep=%s\nupdated=%s\n' "$1" "$2" "$3" "$4" "$5" "${KIT_MAC_TIME:-$(date +%s)}" > "$PROG.tmp" \
        && mv -f "$PROG.tmp" "$PROG" || rm -f "$PROG.tmp"
}

prog_begin() { # <本次的组件…>：定 PROG_DONE（要跳过的）
    PROG_DONE=
    [ -n "${KIT_ID:-}" ] || return 0
    if [ "${KIT_FRESH:-0}" != 1 ] && [ "$(prog_get kit)" = "$KIT_ID" ] && [ "$(prog_get step)" != complete ]; then
        PROG_DONE=$(prog_get done)
        [ -z "$PROG_DONE" ] || log "$(tx "同一个装机包上次停在 $(prog_get current)（$(prog_get step)）：已完成的 $PROG_DONE 跳过，从没完成的续装（KIT_FRESH=1 从头来）" "the same kit stopped last time at $(prog_get current) ($(prog_get step)): skipping the finished $PROG_DONE and resuming (KIT_FRESH=1 starts over)")"
    fi
    prog_write "$KIT_ID" "$*" "$PROG_DONE" "" begin
}

prog_is_done() { case " $PROG_DONE " in *" $1 "*) return 0 ;; esac; return 1; }   # <组件>

prog_set() { # <组件> <步骤>
    [ -n "${KIT_ID:-}" ] || return 0
    prog_write "$KIT_ID" "$COMPONENTS" "$PROG_DONE" "$1" "$2"
}

prog_done() { # <组件>
    PROG_C=
    [ -n "${KIT_ID:-}" ] || return 0
    PROG_DONE=${PROG_DONE:+$PROG_DONE }$1
    prog_write "$KIT_ID" "$COMPONENTS" "$PROG_DONE" "" "done:$1"
}

prog_finish() { [ -z "${KIT_ID:-}" ] || prog_write "$KIT_ID" "$COMPONENTS" "$PROG_DONE" "" complete; }

prog_status() { # status 里那一行
    [ -f "$PROG" ] || { tx '没有记录' 'no record'; return 0; }
    if [ "$(prog_get step)" = complete ]; then
        tx "装机包 $(prog_get kit) 已装完：$(prog_get done)" "kit $(prog_get kit) finished: $(prog_get done)"
    else
        tx "装机包 $(prog_get kit) 没装完：停在 $(prog_get current)（$(prog_get step)），已完成 $(prog_get done)；同一个包再跑会续装" "kit $(prog_get kit) unfinished: stopped at $(prog_get current) ($(prog_get step)), finished $(prog_get done); running the same kit again resumes"
    fi
}

# ── status：只读，一项一行（中文宽度对不齐，就不对齐）────────────────────────
do_status() {   # 只读
    p() { printf '  %s%s%s\n' "$1" "$(tx '：' ': ')" "$2"; }
    yes_no() { if eval "$1"; then echo "$2"; else echo "$3"; fi; }   # <条件> <真> <假>
    svc() { svc_running "$1" && tx '（procd 监督）' ' (under procd)' || { [ -x "/etc/init.d/$1" ] && tx '（procd：未运行）' ' (procd: not running)'; }; }
    _fw=$(ubus call zwrt_web device_info '{}' 2>/dev/null | sed -n 's/.*"wa_inner_version": *"\([^"]*\)".*/\1/p')
    p "$(tx 固件 Firmware)" "${_fw:-$(tx 未知 unknown)}"
    p "SSH" "$(netstat -ltn 2>/dev/null | grep -q ':2222 ' && tx '运行中 (:2222)' 'running (:2222)' || tx 未运行 'not running')"   # 看端口
    p "$(tx 高级后台 'Admin web')" "$(pidof luyaz-web >/dev/null && tx '运行中 (:9090)' 'running (:9090)' || tx 未运行 'not running')$(svc luyaz-web)"
    p "ui" "$(pidof luyaz-ui >/dev/null && tx 运行中 running || tx 未运行 'not running')$([ -f "$SCREEN_DIR/gave-up" ] && tx '，已放弃重试（长按右下角 3 秒恢复）' ', gave up retrying (hold the bottom-right corner for 3 seconds to bring it back)')"
    p "$(tx 哨兵 Sentinel)" "$(pidof luyaz-sentinel >/dev/null && tx 运行中 running || tx 未运行 'not running')$(svc luyaz-sentinel)$(h=$(cut -d' ' -f3 /tmp/luyaz-sentinel.hb 2>/dev/null) && [ -n "$h" ] && tx "，屏幕：$h" ", screen: $h")"
    p "$(tx 核心 Core)" "$(pidof luyaz-core >/dev/null && tx 运行中 running || tx 未运行 'not running')$(svc luyaz-core)"
    p "$(tx 'eSIM 组件' 'eSIM')" "$(yes_no '[ -x /data/esim/lpac ]' "$(tx 已装 installed)" "$(tx 未装 'not installed')")"
    p "$(tx 上机收尾 'Ship cleanup')" "$([ -f "$SHIPD/luyaz-recover.sh" ] && tx 'luyaz-recover.sh 已放' 'luyaz-recover.sh in place' || tx 'luyaz-recover.sh 没放' 'luyaz-recover.sh missing')$(grep -v '^[[:space:]]*#' "$RC" 2>/dev/null | grep -q 'luyaz-recover\.sh' && tx '，rc.local 有那一行' ', rc.local has its line' || tx '，rc.local 没有那一行（./install.sh recover）' ', rc.local lacks its line (./install.sh recover)')"
    p "CHILL" "$([ -x /data/chill/chill.sh ] && { s=$(chill_state); echo "$(tx 已装， 'installed, ')${s:-$(tx 未运行 'not running')}$(svc chill)$([ -f /data/chill/disabled ] && tx '，已关闭' ', turned off')"; } || tx 未装 'not installed')"
    p "$(tx 装机进度 'Install progress')" "$(prog_status)"
    p "$(tx 自动升级 'Auto-update')" "$(fota_is_off && tx 已关闭 off || tx "没有完全关闭（$(fota_vals)）" "not fully off ($(fota_vals))")"
    echo "  $(tx 'rc.local 自启:' 'rc.local autostart:')"   # 下面列出我们加的启动行
    grep -E "start_dropbear|/etc/init.d/(luyaz-core|luyaz-web|luyaz-sentinel|chill) start" "$RC" 2>/dev/null | sed 's/^/    /'
}  # do_status

# 测试（onboard/test/kit-rc.sh）只要上面的函数
[ -n "${KIT_LIB_ONLY:-}" ] && return 0

[ $# -gt 0 ] || die "$(tx '用法' 'usage'): sh device/install.sh {ssh|admin|ui|esim|chill|recover|status}..."
COMPONENTS="$*"   # 给 ship_busy 之类的检查用
case " $COMPONENTS " in
    *" admin "* | *" ui "* | *" recover "*)
        busy=$(ship_busy)
        [ -n "$busy" ] && die "$busy$(tx "：先等它结束（电脑上 tools/luyaz status 看；sh $S/luyaz-ship.sh status），这次什么都没装" ": wait for it to finish (on the computer: tools/luyaz status; on the device: sh $S/luyaz-ship.sh status). Nothing was installed this time")"
        ;;
esac
[ "$1" = status ] || prog_begin "$@"
for c in "$@"; do   # 按命令行顺序装
    if [ "$c" != status ] && prog_is_done "$c"; then log "$(tx "$c：同一个装机包上次已装完，跳过" "$c: already installed by this kit last time; skipped")"; continue; fi
    [ "$c" = status ] || { PROG_C=$c; prog_set "$c" started; }
    case "$c" in   # 组件名 → 函数
        ssh)     do_ssh ;;   # SSH
        admin)   do_admin ;;   # 管理网页
        ui)   do_ui ;;
        esim)    do_esim ;;   # eSIM
        recover) do_recover ;;
        chill)   do_chill ;;
        status)  do_status ;;   # 现状
        *)       die "$(tx "不认识的组件: $c" "unknown component: $c")" ;;   # 其他一律拒绝
    esac  # do_status
    [ "$c" = status ] || prog_done "$c"
done  # do_status
[ "$1" = status ] || prog_finish
