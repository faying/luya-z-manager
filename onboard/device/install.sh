#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# U60 装机包 — 设备端安装脚本（busybox ash）
#
# 不要手动跑：由电脑端 install.sh 把整个包推到 /data/local/tmp/u60-kit，
# 再通过 adb shell 或 ssh 调用本脚本。
#
#   sh device/install.sh ssh admin devui esim     # 按顺序装指定组件
#   sh device/install.sh status                   # 只打印状态
#
# 约定（和 CLAUDE.md 一致）：
#   • 开机自启只走 /etc/rc.local，绝不 /etc/init.d/<x> disable 原厂守护进程
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
    [ -f /tmp/zte-agent.pid ] && kill "$(cat /tmp/zte-agent.pid)" 2>/dev/null
    killall zte-agent 2>/dev/null
    sleep 1
    put "$P/zte-agent" /data/zte-agent 755
    put_tree "$P/admin.tgz" /data/admin

    # 密码由电脑端写进 agent.env（一行 export ZTE_AGENT_PASSWORD='...'）；
    # 没带就沿用设备上现有的启动脚本
    if [ -s "$P/agent.env" ]; then
        { echo '#!/bin/sh'
          cat "$P/agent.env"
          echo "start-stop-daemon -S -b -m -p /tmp/zte-agent.pid -x /bin/sh -- -c 'exec /data/zte-agent >/tmp/zte-agent.log 2>&1'"
        } > /data/local/tmp/start_zte_agent.sh
        chmod 700 /data/local/tmp/start_zte_agent.sh
    fi
    [ -f /data/local/tmp/start_zte_agent.sh ] || die "没有后台密码，也没有现成的启动脚本"
    rc_add "sh /data/local/tmp/start_zte_agent.sh" "start_zte_agent.sh"

    sh /data/local/tmp/start_zte_agent.sh
    sleep 2
    pidof zte-agent >/dev/null || die "zte-agent 没起来，看 /tmp/zte-agent.log"
    wget -q -O /dev/null http://127.0.0.1:9090/ || die "管理网页打不开（http://127.0.0.1:9090/）"
    if [ -s "$P/login.json" ]; then
        curl -s -m 5 -H 'Content-Type: application/json' --data @"$P/login.json" \
            http://127.0.0.1:9090/api/auth/login | grep -q '"token"' \
            || die "后台起来了，但用新密码登录失败"
        log "高级后台: 已启动（:9090，新密码登录已验证）"
    else
        log "高级后台: 已启动（:9090，沿用原来的密码）"
    fi
}

# ── devui 触屏界面 + 数据后端 zwrt-datad ─────────────────────────────────────
do_devui() {
    D=/data/plugins/u60pro-devui
    DD=/data/plugins/zwrt-datad
    mkdir -p "$D/ui" "$DD"
    killall -9 u60pro-devui zwrt-datad 2>/dev/null
    sleep 1
    put "$P/devui/u60pro-devui" "$D/u60pro-devui" 755
    put "$P/devui/start.sh" "$D/start.sh" 755
    put "$P/devui/zwrt-datad" "$DD/zwrt-datad" 755
    tar xzf "$P/devui/ui.tgz" -C "$D/ui" || die "解压界面文件失败"
    # eSIM 页要靠 eSIM 组件：这次不装、设备上也没装过，就不放入口
    case " $COMPONENTS " in *" esim "*) ;; *) [ -x /data/esim/lpac ] || rm -f "$D/ui/functions/esim.html" ;; esac

    backup_rc
    # 用 devui 仓库自带的安装脚本：保留原厂 zte_topsw_devui 做开机早期的屏幕/触摸
    # 初始化，再由 rc.local 调 start.sh 接管（这是上游验证过的稳定链路）
    sh "$P/devui/install-autostart.sh" >/tmp/u60-kit-devui.log 2>&1
    grep -q "u60pro_devui" "$RC" || rc_add \
        "[ -x $D/start.sh ] && sh $D/start.sh >/tmp/u60pro-boot.log 2>&1 & # u60pro_devui" "u60pro_devui"
    sh -n "$RC" || die "rc.local 语法检查没过（devui 安装脚本改坏了？原厂备份在 $STATE/rc.local.orig）"

    i=0
    while [ $i -lt 10 ]; do
        pidof u60pro-devui >/dev/null && pidof zwrt-datad >/dev/null && break
        sleep 1; i=$((i + 1))
    done
    pidof u60pro-devui >/dev/null || die "devui 没起来，看 /tmp/u60pro-devui.log 和 /tmp/u60-kit-devui.log"
    pidof zwrt-datad >/dev/null || warn "zwrt-datad 没起来，屏幕会没有数据，看 /tmp/zwrt-datad.log"
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

do_status() {
    p() { printf '  %s：%s\n' "$1" "$2"; }   # 中文按字节算宽度，对不齐，就不对齐了
    fw=$(ubus call zwrt_web device_info '{}' 2>/dev/null | sed -n 's/.*"wa_inner_version": *"\([^"]*\)".*/\1/p')
    p "固件" "${fw:-未知}"
    p "SSH" "$(netstat -ltn 2>/dev/null | grep -q ':2222 ' && echo '运行中 (:2222)' || echo 未运行)"
    p "高级后台" "$(pidof zte-agent >/dev/null && echo '运行中 (:9090)' || echo 未运行)"
    p "devui" "$(pidof u60pro-devui >/dev/null && echo 运行中 || echo 未运行)"
    p "zwrt-datad" "$(pidof zwrt-datad >/dev/null && echo 运行中 || echo 未运行)"
    p "eSIM 组件" "$([ -x /data/esim/lpac ] && echo 已装 || echo 未装)"
    p "自动升级" "$([ "$(uci -q get zwrt_zte_dm.dm_update.dm_update_mode)" = 0 ] && echo 已关闭 || echo 开着)"
    p "USB 模式" "$(ubus call zwrt_bsp.usb list '{}' 2>/dev/null | sed -n 's/.*"mode": *"\([^"]*\)".*/\1/p')"
    echo "  rc.local 自启:"
    grep -E "start_dropbear|start_zte_agent|u60pro_devui" "$RC" 2>/dev/null | sed 's/^/    /'
}

[ $# -gt 0 ] || die "用法: sh device/install.sh {ssh|admin|devui|esim|status}..."
COMPONENTS="$*"
for c in "$@"; do
    case "$c" in
        ssh)    do_ssh ;;
        admin)  do_admin ;;
        devui)  do_devui ;;
        esim)   do_esim ;;
        status) do_status ;;
        *) die "不认识的组件: $c" ;;
    esac
done
