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
#   • zte-agent / zwrt-datad / u60-guard 由 procd 监督（崩溃自动拉起 + 告警）：
#     /etc/init.d/ 放各自的 init 脚本，但**不 enable**（/etc 是 overlay，enable/disable
#     会留 whiteout），由 rc.local 里一行 `/etc/init.d/<名字> start` 拉起
#   • 程序和数据全放 /data（固件升级也保留）；/etc 下只放 rc.local 的钩子
#   • 每次改 rc.local 前先 sh -n 做语法检查，第一次改之前备份原厂版本
# ─────────────────────────────────────────────────────────────────────────────
set -u

KIT=$(cd "$(dirname "$0")/.." && pwd)
P=$KIT/payload
RC=/etc/rc.local
STATE=/data/u60-kit          # 原厂 rc.local 备份 + 已装版本记录

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

# 幂等地在 exit 0 之前插一行；<marker> 已存在就跳过
rc_add() { # <line> <marker>
    grep -qF "$2" "$RC" 2>/dev/null && return 0
    backup_rc
    if grep -q '^exit 0' "$RC"; then
        awk -v ins="$1" '/^exit 0/ && !d { print ins; d=1 } { print }' "$RC" > /tmp/rc.local.new
    else
        { cat "$RC"; echo "$1"; } > /tmp/rc.local.new
    fi
    sh -n /tmp/rc.local.new || { rm -f /tmp/rc.local.new; die "rc.local 语法检查没过，已放弃修改"; }
    cat /tmp/rc.local.new > "$RC" && rm -f /tmp/rc.local.new
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
        sh -n /tmp/rc.local.new || { rm -f /tmp/rc.local.new; die "rc.local 语法检查没过，已放弃修改"; }
        cat /tmp/rc.local.new > "$RC" && rm -f /tmp/rc.local.new
        log "rc.local 去掉旧行: $2"
        return 0
    fi
    if grep -qF "$2" "$RC" 2>/dev/null; then
        backup_rc
        awk -v old="$2" -v ins="$1" 'index($0, old) && !d { print ins; d=1; next } { print }' "$RC" > /tmp/rc.local.new
        sh -n /tmp/rc.local.new || { rm -f /tmp/rc.local.new; die "rc.local 语法检查没过，已放弃修改"; }
        cat /tmp/rc.local.new > "$RC" && rm -f /tmp/rc.local.new
        log "rc.local 替换: $2 → $1"
    else
        rc_add "$1" "$3"
    fi
}

# ── 进程监督与 Wi-Fi 兜底：/data/u60-guard + /etc/init.d/{zte-agent,zwrt-datad,u60-guard}
# 契约见 docs/RELIABILITY.md。admin 和 devui 都要用到（datad 也由 supervise.sh 包着）。
G=/data/u60-guard
install_guard() {
    [ -d "$P/guard" ] || die "包里没有 guard/（装机包太旧？）"
    mkdir -p "$G"
    for f in alert-lib.sh u60-guard.sh supervise.sh agent-auth.sh chaos.sh doctor.sh config-backup.sh power-sample.sh wan-sources.sh; do put "$P/guard/$f" "$G/$f" 755; done
    for s in zte-agent zwrt-datad u60-guard; do
        put "$P/guard/$s.init" "/etc/init.d/$s" 755
        cp "$P/guard/$s.init" "$G/$s.init"
    done
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
    rc_add "sh /data/local/tmp/start_dropbear.sh" "start_dropbear.sh"

    if netstat -ltn 2>/dev/null | grep -q ':2222 '; then
        # 已经在跑（比如这次就是 ssh 进来重装的）：只同步公钥，不动进程
        cp /data/ssh/authorized_keys /etc/dropbear/authorized_keys 2>/dev/null
        log "SSH: 2222 端口已在监听，公钥已同步"
    else
        sh /data/local/tmp/start_dropbear.sh
        sleep 1
        netstat -ltn 2>/dev/null | grep -q ':2222 ' || die "dropbear 没起来"
        log "SSH: dropbear 已在 2222 端口启动"
    fi

    fota_off
}

# 固件升级会覆盖 /etc/rc.local（自启全丢），而且 B28 起中兴封了开 ADB 的接口，
# 升上去就再也回不来，所以装 SSH 时一并关掉自动升级
fota_off() {
    roam=$(uci -q get zwrt_zte_dm.dm_update.dm_update_roam_permission)
    itime=$(uci -q get zwrt_zte_dm.dm_update.dm_install_time)
    ubus call zwrt_zte_dm set_update_mode \
        "{\"dm_update_mode\":\"0\",\"dm_update_roam_permission\":\"${roam:-1}\",\"dm_install_time\":\"$itime\"}" \
        >/dev/null 2>&1
    if [ "$(uci -q get zwrt_zte_dm.dm_update.dm_update_mode)" = 0 ]; then
        log "固件自动升级: 已关闭"
    else
        warn "没能确认关掉自动升级，请到网页后台「系统设置 → 软件升级」手动关"
    fi
}

# ── 高级后台：zte-agent（:9090 API）+ 管理网页（/data/admin）───────────────────
do_admin() {
    install_guard
    # 停掉旧的：procd 装法和旧的 start-stop-daemon 装法都可能在跑
    [ -x /etc/init.d/zte-agent ] && /etc/init.d/zte-agent stop >/dev/null 2>&1
    [ -f /tmp/zte-agent.pid ] && kill "$(cat /tmp/zte-agent.pid)" 2>/dev/null
    killall zte-agent 2>/dev/null
    i=0
    while { pidof zte-agent >/dev/null || netstat -ltn 2>/dev/null | grep -q ':9090 '; } && [ $i -lt 10 ]; do
        sleep 1; i=$((i + 1))
    done
    put "$P/zte-agent" /data/zte-agent 755
    put_tree "$P/admin.tgz" /data/admin

    # 密码放 /data/zte-agent.env（600）。agent 自己读这个文件，procd 的 env 里只放路径
    # （ubus call service list 会列出 env）。没有密码的 agent 对所有请求放行，所以没有就停。
    #  • 电脑端带了新密码（agent.env）→ 用新的，覆盖旧的
    #  • 没带 → 沿用 /data/zte-agent.env；再没有就从旧装法的启动脚本迁过来
    if [ -s "$P/agent.env" ]; then
        rm -f /data/zte-agent.env
        AGENT_AUTH_OLD="$P/agent.env" sh "$G/agent-auth.sh" migrate || die "写入后台密码失败"
    elif [ ! -s /data/zte-agent.env ]; then
        [ -f /data/local/tmp/start_zte_agent.sh ] || die "没有后台密码：设备上既没有 /data/zte-agent.env 也没有旧的启动脚本"
        sh "$G/agent-auth.sh" migrate || die "从旧启动脚本迁移后台密码失败"
    fi
    # 旧装法的启动脚本留着不动（回滚用），只是不再从 rc.local 调它
    rc_replace "/etc/init.d/zte-agent start" "start_zte_agent.sh" "/etc/init.d/zte-agent start"

    /etc/init.d/zte-agent start
    sleep 4
    svc_running zte-agent && pidof zte-agent >/dev/null || die "zte-agent 没起来，看 /tmp/zte-agent.log 和 logread"
    wget -q -O /dev/null http://127.0.0.1:9090/ || die "管理网页打不开（http://127.0.0.1:9090/）"
    # 三项鉴权：未登录 401、密码能登录且 token 可用、空密码登录失败
    sh "$G/agent-auth.sh" verify || die "后台鉴权检查没过（见上）——不要让设备带着开放的后台运行"
    log "高级后台: 已启动（:9090，procd 监督，鉴权三项已验证）"

    # Wi-Fi 兜底看门狗 + 告警短信：agent 的心跳它才看得懂，所以跟着 admin 装
    rc_add "/etc/init.d/u60-guard start" "/etc/init.d/u60-guard start"
    /etc/init.d/u60-guard restart >/dev/null 2>&1 || /etc/init.d/u60-guard start
    sleep 2
    svc_running u60-guard && log "Wi-Fi 兜底看门狗: 已启动" || warn "u60-guard 没起来，看 logread"
}

# ── devui 触屏界面 + 数据后端 zwrt-datad ─────────────────────────────────────
do_devui() {
    D=/data/plugins/u60pro-devui
    DD=/data/plugins/zwrt-datad
    mkdir -p "$D/ui" "$DD"
    install_guard
    # u60-uid would read the kill below as a crash (alert, relaunch race).
    # Stopping it leaves the UI running; it is started again at the end.
    [ -x /etc/init.d/u60-uid ] && /etc/init.d/u60-uid stop >/dev/null 2>&1
    [ -x /etc/init.d/zwrt-datad ] && /etc/init.d/zwrt-datad stop >/dev/null 2>&1
    killall -9 u60pro-devui zwrt-datad 2>/dev/null
    sleep 1
    put "$P/devui/u60pro-devui" "$D/u60pro-devui" 755
    put "$P/devui/start.sh" "$D/start.sh" 755
    put "$P/devui/u60-uid" "$D/u60-uid" 755
    put "$P/devui/u60-uid.init" /etc/init.d/u60-uid 755
    put "$P/devui/zwrt-datad" "$DD/zwrt-datad" 755
    tar xzf "$P/devui/ui.tgz" -C "$D/ui" || die "解压界面文件失败"
    # 数字字体（Nunito，OFL）：装机包里有才装；没有时触屏退回 /usr/ui/fonts/Roboto.ttf
    if [ -d "$P/devui/fonts" ]; then
        mkdir -p "$D/fonts"
        for f in "$P"/devui/fonts/*; do put "$f" "$D/fonts/$(basename "$f")" 644; done
    fi
    # eSIM 页要靠 eSIM 组件：这次不装、设备上也没装过，就不放入口
    case " $COMPONENTS " in *" esim "*) ;; *) [ -x /data/esim/lpac ] || rm -f "$D/ui/functions/esim.html" ;; esac

    backup_rc
    # devui 仓库自带的安装脚本：清理旧版残留、保留原厂 zte_topsw_devui 做开机早期的
    # 屏幕/触摸初始化，并在 rc.local 里放一行 start.sh（带 # u60pro_devui 标记）。
    # 它认得本包的 /etc/init.d/zwrt-datad（supervise.sh 版），不会去停它。
    sh "$P/devui/install-autostart.sh" >/tmp/u60-kit-devui.log 2>&1
    sh -n "$RC" || die "rc.local 语法检查没过（devui 安装脚本改坏了？原厂备份在 $STATE/rc.local.orig）"

    # 屏幕归 u60-uid（procd 监督）：它是拉起/停止触屏界面、放弃后交还原厂界面、长按右下角
    # 回来的唯一主人（取代 corner-wake 和 start.sh 的拉起）。start.sh 那一行原地换掉；
    # start.sh 看到这一行就只做开机杂务，不再自己起界面。
    rc_replace "/etc/init.d/u60-uid start" "u60pro_devui" "/etc/init.d/u60-uid start"
    # zwrt-datad 归 procd（supervise.sh 包着，崩了拉起并告警）；start.sh 看到这一行就不再起第二份
    rc_add "/etc/init.d/zwrt-datad start" "/etc/init.d/zwrt-datad start"
    /etc/init.d/zwrt-datad start
    # 这是一次有人主动做的安装，不是崩溃：清掉 u60-uid 的启动计数。否则 10 分钟内装两三次，
    # 每次重启 u60-uid 都算一次失败的启动，第三次它就放弃、交还原厂界面（2026-09-23 实际遇到）。
    # 新程序真崩的话，从 0 开始照样两次就放弃。
    rm -f /data/u60-uid/attempts /data/u60-uid/gave-up
    /etc/init.d/u60-uid restart >/dev/null 2>&1 || /etc/init.d/u60-uid start

    i=0
    while [ $i -lt 10 ]; do
        pidof u60pro-devui >/dev/null && pidof zwrt-datad >/dev/null && break
        sleep 1; i=$((i + 1))
    done
    pidof u60pro-devui >/dev/null || die "devui 没起来，看 /tmp/u60pro-devui.log、/tmp/u60-uid.log 和 /tmp/u60-kit-devui.log"
    [ "$(pidof u60pro-devui | wc -w)" -le 1 ] || warn "有不止一个 u60pro-devui 在跑"
    svc_running u60-uid || warn "u60-uid 没在 procd 下运行，看 logread"
    pidof zwrt-datad >/dev/null || warn "zwrt-datad 没起来，屏幕会没有数据，看 /tmp/zwrt-datad.log 和 logread"
    [ "$(pidof zwrt-datad | wc -w)" -le 1 ] || warn "有不止一个 zwrt-datad 在跑（start.sh 没认出 procd 装法？）"
    log "devui: 屏幕界面已接管"
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
    p "高级后台" "$(pidof zte-agent >/dev/null && echo '运行中 (:9090)' || echo 未运行)$(svc zte-agent)"
    p "devui" "$(pidof u60pro-devui >/dev/null && echo 运行中 || echo 未运行)$(svc u60-uid)$([ -f /data/u60-uid/gave-up ] && echo '，已放弃重试（长按右下角 3 秒恢复）')"
    p "zwrt-datad" "$(pidof zwrt-datad >/dev/null && echo 运行中 || echo 未运行)$(svc zwrt-datad)"
    p "Wi-Fi 兜底" "$(svc_running u60-guard && echo 运行中 || echo 未运行)"
    p "eSIM 组件" "$([ -x /data/esim/lpac ] && echo 已装 || echo 未装)"
    p "CHILL" "$([ -x /data/chill/chill.sh ] && { s=$(chill_state); echo "已装，${s:-未运行}$(svc chill)$([ -f /data/chill/disabled ] && echo '，已关闭')"; } || echo 未装)"
    p "自动升级" "$([ "$(uci -q get zwrt_zte_dm.dm_update.dm_update_mode)" = 0 ] && echo 已关闭 || echo 开着)"
    p "USB 模式" "$(ubus call zwrt_bsp.usb list '{}' 2>/dev/null | sed -n 's/.*"mode": *"\([^"]*\)".*/\1/p')"
    echo "  rc.local 自启:"
    grep -E "start_dropbear|start_zte_agent|u60pro_devui|/etc/init.d/(zte-agent|zwrt-datad|u60-guard|u60-uid|chill) start" "$RC" 2>/dev/null | sed 's/^/    /'
}

[ $# -gt 0 ] || die "用法: sh device/install.sh {ssh|admin|devui|esim|chill|status}..."
COMPONENTS="$*"
for c in "$@"; do
    case "$c" in
        ssh)    do_ssh ;;
        admin)  do_admin ;;
        devui)  do_devui ;;
        esim)   do_esim ;;
        chill)  do_chill ;;
        status) do_status ;;
        *) die "不认识的组件: $c" ;;
    esac
done
