#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# 装机包回归测试：在一台正在使用的 U60 Pro（MU5250）上开沙盒，完整跑一遍 install.sh 的 ADB 流程
#
#   onboard/test/sandbox.sh dry       # 只在本机：解包 → 改写成沙盒 → 防呆检查（不连设备，不用下面三个变量）
#   HOST=… GATEWAY=… SSH_KEY=… onboard/test/sandbox.sh run    # 上面那步 + 假 adb 在设备上跑 ssh/esim → status
#   HOST=… GATEWAY=… SSH_KEY=… onboard/test/sandbox.sh clean  # 杀掉沙盒进程、删目录
#
# 怎么做到不碰设备原有配置：
#   • 假 adb：adb shell/push 转成 ssh $HOST，拒绝任何含 reboot 的命令
#   • 设备端脚本路径全改写到 /data/local/tmp/kit-sb；dropbear 用 2223，agent 只听 127.0.0.1:19090
#   • FOTA 设置打桩；admin 的 luyaz-web 改到 $SB/luyaz、只听 127.0.0.1:19090（不装 /etc/init.d、/data/luyaz/scripts，
#     不记设备清单）；devui、recover、chill 直接禁用（会换掉设备上正在用的哨兵、屏幕、开机收尾、代理）
#   • 改写后还残留真实路径就中止
#   • HOST、GATEWAY、SSH_KEY 必须显式给（不再有默认值：以前默认指向原作者的设备），原作者的地址直接拒绝
# 设备上的 dropbear 仍然认 /etc/dropbear/authorized_keys，所以 SSH_KEY 要用那台设备已授权的 key。
#
# 环境变量（run、clean 必填）：HOST（ssh 别名）、GATEWAY（设备地址）、SSH_KEY（私钥路径）
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HOST="${HOST:-}"
GATEWAY="${GATEWAY:-}"
SSH_KEY="${SSH_KEY:-}"
T=/tmp/u60-kit-sandbox
SB=/data/local/tmp/kit-sb
CM=(-o ControlMaster=auto -o ControlPath=/tmp/u60-kit-sandbox-cm -o ControlPersist=300 -o LogLevel=ERROR)

dev() { ssh "${CM[@]}" "$HOST" "$@"; }

# 连设备之前：三个变量都要显式给，且不是原作者的环境
need_target() {
  local miss=
  [ -n "$HOST" ] || miss="$miss HOST"
  [ -n "$GATEWAY" ] || miss="$miss GATEWAY"
  [ -n "$SSH_KEY" ] || miss="$miss SSH_KEY"
  [ -z "$miss" ] || { echo "✗ 要显式给:$miss（例：HOST=u60 GATEWAY=192.168.0.1 SSH_KEY=~/.ssh/id_ed25519）" >&2; exit 2; }
  case "$GATEWAY" in
    192.168.0.1|192.168.0.1|192.168.0.1|192.168.0.1)
      echo "✗ GATEWAY=$GATEWAY 是原作者的设备地址，不连" >&2; exit 2 ;;
  esac
  case "$HOST" in *@192.168.0.1|*@192.168.0.1|*@192.168.0.1|*@192.168.0.1)
      echo "✗ HOST=$HOST 是原作者的环境，不连" >&2; exit 2 ;;
  esac
  [ -f "$SSH_KEY" ] && [ -f "$SSH_KEY.pub" ] || { echo "✗ 没有 $SSH_KEY 或 $SSH_KEY.pub" >&2; exit 2; }
  # ssh 别名展开后的真实地址也要和 GATEWAY 对得上，免得别名指着别的设备
  local hn; hn=$(ssh -G "$HOST" 2>/dev/null | awk '$1 == "hostname" { print $2; exit }')
  [ "$hn" = "$GATEWAY" ] || { echo "✗ ssh $HOST 连的是 ${hn:-?}，不是 GATEWAY=$GATEWAY" >&2; exit 2; }
}

clean() {
  dev "kill \$(cat /tmp/kitsb-dropbear.pid /tmp/kitsb-agent.pid /tmp/kitsb-lweb.pid 2>/dev/null) 2>/dev/null; sleep 1
       rm -rf $SB /data/local/tmp/u60-kit /tmp/kitsb-* /tmp/u60-kit-status.sh"
  ssh -O exit -o ControlPath=/tmp/u60-kit-sandbox-cm "$HOST" 2>/dev/null || true
  echo "sandbox cleaned on $HOST"
}

# 解包 + 改写成沙盒 + 防呆检查，只动本机的 $T
prepare() {
  local kit; kit=$(ls -t "$ROOT"/onboard/dist/luyaz-kit-*.tar.gz 2>/dev/null | head -1)
  [ -n "$kit" ] || { echo "先跑 onboard/build-kit.sh" >&2; exit 1; }
  rm -rf "$T" && mkdir -p "$T/bin" "$T/home/.ssh"
  tar xzf "$kit" -C "$T"
  local K=$T/luyaz-kit

  cat > "$T/bin/adb" <<EOF
#!/usr/bin/env bash
SSH=(ssh ${CM[*]} $HOST)
case "\$1" in
  devices) printf 'List of devices attached\nSANDBOX\tdevice\n' ;;
  shell) shift; case "\$*" in *reboot*) echo "fake-adb: refusing reboot" >&2; exit 1;; esac
         "\${SSH[@]}" "\$*" ;;
  push) if [ -d "\$2" ]; then tar czf - -C "\$(dirname "\$2")" "\$(basename "\$2")" | "\${SSH[@]}" "mkdir -p '\$3' && tar xzf - -C '\$3'"
        else "\${SSH[@]}" "cat > '\$3'" < "\$2"; fi ;;
  *) echo "fake-adb: unsupported \$*" >&2; exit 1 ;;
esac
EOF
  chmod +x "$T/bin/adb"

  # admin = luyaz-web：沙盒里换成 $SB/luyaz/luyaz-web，只听 127.0.0.1:19090，密码、会话、logo 都在 $SB；
  # 不装 /data/luyaz/scripts、/etc/init.d（install_guard 换成只放 agent-auth.sh），不记设备清单
  cat > "$T/admin.sed" <<SED
s|^LW=/data/luyaz\$|LW=$SB/luyaz|
s|^    install_guard   #.*|    mkdir -p $SB/scripts \&\& put "\$P/guard/agent-auth.sh" $SB/scripts/agent-auth.sh 755; S=$SB/scripts|
s|^    \[ -x /etc/init.d/luyaz-web \] .*|    kill "\$(cat /tmp/kitsb-lweb.pid 2>/dev/null)" 2>/dev/null|
s|pidof zte-agent luyaz-web >/dev/null \|\| netstat -ltn 2>/dev/null \| grep -q ':9090 '|kill -0 "\$(cat /tmp/kitsb-lweb.pid 2>/dev/null)" 2>/dev/null|
s|AGENT_AUTH_OLD="\$P/agent.env" sh "\$S/agent-auth.sh" migrate|AGENT_AUTH_ENV=$SB/zte-agent.env AGENT_AUTH_OLD="\$P/agent.env" sh "\$S/agent-auth.sh" migrate|
s|^        sh "\$S/agent-auth.sh" migrate|        AGENT_AUTH_ENV=$SB/zte-agent.env AGENT_AUTH_OLD=$SB/start_zte_agent.sh sh "\$S/agent-auth.sh" migrate|
s|^    rc_add "/etc/init.d/luyaz-web start" "/etc/init.d/luyaz-web start"|    rc_add "sh $SB/lweb-start.sh" "$SB/lweb-start.sh"|
s|^    /etc/init.d/luyaz-web start\$|    sh $SB/lweb-start.sh|
s|^    svc_running luyaz-web \&\& pidof luyaz-web >/dev/null|    kill -0 "\$(cat /tmp/kitsb-lweb.pid 2>/dev/null)" 2>/dev/null|
s|^    kit_record lweb\$|    true|
SED
  sed -f "$T/admin.sed" "$K/device/install.sh" > "$K/device/install.a" && mv "$K/device/install.a" "$K/device/install.sh"
  for want in "LW=$SB/luyaz" "S=$SB/scripts" "sh $SB/lweb-start.sh" "kitsb-lweb.pid" "AGENT_AUTH_ENV=$SB/zte-agent.env AGENT_AUTH_OLD=$SB/start_zte_agent.sh"; do
    grep -qF "$want" "$K/device/install.sh" || { echo "✗ admin 的沙盒改写没套上：$want（device/install.sh 改过了？）" >&2; exit 1; }
  done

  # 路径变量都是 X=${KIT_X:-真实路径}：整行换成沙盒路径（2026-10-14 曾因只认旧写法 RC=/etc/rc.local 漏改，
  # 往真 rc.local 里加了一行；防呆检查也不再放过任何 KIT_ 默认值）
  sed -e "s#^RC=\${KIT_RC:-/etc/rc.local}#RC=$SB/etc/rc.local#" \
      -e "s#^STATE=\${KIT_STATE:-/data/luyaz/kit}#STATE=$SB/state#" \
      -e "s#^SHIPD=\${KIT_SHIP_DIR:-/data/luyaz/ship}#SHIPD=$SB/luyaz-ship#" \
      -e "s#^S=\${KIT_SCRIPTS:-/data/luyaz/scripts}#S=$SB/scripts#" \
      -e "s#^CORE_DIR=\${KIT_CORE_DIR:-/data/luyaz/core}#CORE_DIR=$SB/luyaz-core#" \
      -e "s#^SCREEN_DIR=\${KIT_SCREEN_DIR:-/data/luyaz/screen}#SCREEN_DIR=$SB/luyaz-screen#" \
      -e "s#\${KIT_OLD_STATE:-/data/u60-kit}#$SB/old-state#g" \
      -e "s#^WEB_ENV=/data/luyaz/web.env#WEB_ENV=$SB/zte-agent.env#" \
      -e "s#/data/local/tmp/start_dropbear.sh#$SB/start_dropbear.sh#g" \
      -e "s#/data/local/tmp/start_zte_agent.sh#$SB/start_zte_agent.sh#g" \
      -e "s#/data/ssh#$SB/ssh#g" \
      -e "s#/etc/dropbear#$SB/etc/dropbear#g" \
      -e "s#/tmp/dropbear.pid#/tmp/kitsb-dropbear.pid#g" \
      -e "s#-p 2222#-p 2223#g; s#:2222 #:2223 #g; s#(:2222)#(:2223)#g" \
      -e "s#ubus call zwrt_zte_dm set_update_mode#true#" \
      -e "s#uci -q set zwrt_zte_dm.dm_update.TURNOFFPOLLING=1 \&\& uci -q commit zwrt_zte_dm#true#" \
      -e "s#killall zte-agent#true#" \
      -e "s#/tmp/zte-agent.pid#/tmp/kitsb-agent.pid#g; s#/tmp/zte-agent.log#/tmp/kitsb-agent.log#g" \
      -e "s#/data/zte-agent#$SB/zte-agent#g; s#/data/admin#$SB/admin#g" \
      -e "s#pidof zte-agent >/dev/null || die#kill -0 \"\$(cat /tmp/kitsb-agent.pid)\" 2>/dev/null || die#" \
      -e "s#127.0.0.1:9090#127.0.0.1:19090#g" \
      -e "s#          cat \"\$P/agent.env\"#          cat \"\$P/agent.env\"; echo \"export ZTE_AGENT_BIND=127.0.0.1:19090 ZTE_AGENT_UI_DIR=$SB/admin\"#" \
      -e "s#put_tree \"\$P/esim.tgz\" /data/esim#put_tree \"\$P/esim.tgz\" $SB/esim; sed -i \"s,/data/esim,$SB/esim,g\" $SB/esim/lpac.sh#" \
      -e "s#/data/esim/lpac.sh chip info#$SB/esim/lpac.sh chip info#" \
      -e 's#^do_devui() {#do_devui() { die "sandbox: devui disabled";#' \
      -e 's#^do_recover() {#do_recover() { die "sandbox: recover disabled";#' \
      -e 's#^do_chill() {#do_chill() { die "sandbox: chill disabled";#' \
      "$K/device/install.sh" > "$K/device/install.sb"
  mv "$K/device/install.sb" "$K/device/install.sh"

  if grep -n '\${KIT_[A-Z0-9_]*:-/' "$K/device/install.sh"; then
    echo "✗ 上面这些路径变量没换成沙盒路径（device/install.sh 加了新的 KIT_ 变量？），先更新本脚本" >&2; exit 1
  fi
  # 防呆：会写设备真实位置的东西一样都不能剩（注释行、只读的 status、已禁用的 admin/devui/recover 和只有它们
  # 调用的函数除外）。被禁用的函数体整段跳过：从「名字() {」到下一个行首的「}」
  if awk '/^(do_admin|do_devui|do_recover|install_guard|layout_new|retire_agent|place_recover|recover_put|rc_owner|owners_stop|sentinel_start|kit_record|do_chill|do_status)\(\) \{/ { skip = 1 }
          !skip { print } skip && /^}/ { skip = 0 }' "$K/device/install.sh" \
     | grep -vE '^[[:space:]]*#|^[[:space:]]*p "' \
     | grep -nE '(^|[^b])/etc/rc\.local|/data/ssh|/data/zte-agent|/data/admin|/data/luyaz|/data/u60d|/etc/init\.d/|put_tree "\$P/esim.tgz" /data/esim|set_update_mode|uci -q (set|commit)|killall zte-agent| -p 2222|127\.0\.0\.1:9090' \
     | grep -vE '^[0-9]+:(SENT_LINE|LW)=|^[0-9]+:[[:space:]]*\[ -x "/etc/init.d/\$s" \]|svc_running|/etc/init.d/zte_topsw'; then
    echo "✗ 设备端脚本改过了，沙盒改写没覆盖到上面这些行，先更新本脚本" >&2; exit 1
  fi

  sed -i '' -e 's#^SSH_PORT=2222#SSH_PORT=2223#' -e 's#dev_run "reboot"#dev_run "echo sandbox-no-reboot"#' \
            -e "s#/data/local/tmp/start_zte_agent.sh#$SB/start_zte_agent.sh#g" \
            -e "s#/data/zte-agent.env#$SB/zte-agent.env#g" -e "s#/data/luyaz/web.env#$SB/zte-agent.env#g" "$K/install.sh"
  if grep -n '"reboot"\|/data/local/tmp/start_\|/data/zte-agent\|/data/luyaz/web.env' "$K/install.sh" | grep -v '"reboot")'; then
    echo "✗ 电脑端脚本里还有真的 reboot 调用或设备真实路径，先更新本脚本" >&2; exit 1
  fi
  ( cd "$K" && { sed -n '1,/^sha256/p' MANIFEST.txt
      find install.sh device payload -type f | LC_ALL=C sort | while read -r f; do
        printf '%s  %s\n' "$(shasum -a 256 "$f" | awk '{print $1}')" "$f"; done; } > M && mv M MANIFEST.txt )
  echo "✓ 沙盒改写和防呆检查通过：$K"
}

run() {
  need_target
  prepare
  local K=$T/luyaz-kit
  cp "$SSH_KEY" "$SSH_KEY.pub" "$T/home/.ssh/"

  clean >/dev/null
  # 沙盒的 luyaz-web 启动脚本（do_admin 里 /etc/init.d/luyaz-web start 的替身）
  dev "mkdir -p $SB && cat > $SB/lweb-start.sh" <<LWS
nohup $SB/luyaz/luyaz-web serve --bind 127.0.0.1:19090 --upstream http://127.0.0.1:9460 --password-file $SB/zte-agent.env \\
    --session-file $SB/web-sessions --logo-dir $SB/luyaz/operator-logos >/tmp/kitsb-lweb.log 2>&1 &
echo \$! > /tmp/kitsb-lweb.pid
LWS
  dev "mkdir -p $SB/etc && sed -e '/^sh \/data/d' -e '/^# Tailscale/d' -e '/^\[ -x \/data\/plugins/d' /etc/rc.local > $SB/etc/rc.local && sh -n $SB/etc/rc.local"
  local before; before=$(dev "$SNAP")

  R() { ( cd "$K" && env HOME="$T/home" PATH="$T/bin:$PATH" GATEWAY="$GATEWAY" SSH_KEY="$T/home/.ssh/$(basename "$SSH_KEY")" \
          NO_REBOOT=1 bash ./install.sh "$@" ); }
  # 没终端、没 ROUTER_PASSWORD、SSH/ADB 都不通：应该只调匿名的 web_login_info 就停下，不提交登录（不消耗密码次数）
  if [ "$(adb devices 2>/dev/null | grep -c 'device$' || true)" = 0 ]; then
    echo "=== 缺路由器密码时在登录前停下"
    local out; out=$( cd "$K" && env HOME="$T/home" GATEWAY="$GATEWAY" SSH_KEY="$T/home/.ssh/$(basename "$SSH_KEY")" \
                      bash ./install.sh ssh </dev/null 2>&1 ) && { echo "✗ 应该失败" >&2; exit 1; }
    echo "$out" | tail -1
    echo "$out" | grep -q 'ROUTER_PASSWORD' || { echo "✗ 没有提示 ROUTER_PASSWORD" >&2; exit 1; }
  fi

  # 输入都从环境变量/u60.env 给（和对方的 Claude Code 一样没有终端），stdin 接 /dev/null
  # （下面这条在电脑端就停下，设备端的 admin 已禁用也测得到）
  echo "=== 沙盒还没装过后台、两个密码都没给：应在推送前停下"
  local out4; out4=$(R ssh admin </dev/null 2>&1) && { echo "✗ 应该失败" >&2; exit 1; }
  echo "$out4" | tail -1
  echo "$out4" | grep -q '需要高级后台密码' || { echo "✗ 没有在推送前要后台密码" >&2; exit 1; }
  echo "$out4" | grep -q '已推送' && { echo "✗ 失败前已经推送了" >&2; exit 1; }

  echo "=== ADB 模式全新安装（ssh admin esim，AGENT_PASSWORD 环境变量）"
  AGENT_PASSWORD='kit-Test_123' R ssh admin esim </dev/null
  echo "=== SSH 模式重跑（u60.env 里 AGENT_PASSWORD 设成空 = 沿用；不设 NO_REBOOT，没终端应跳过重启）"
  printf "AGENT_PASSWORD=''\n" > "$K/u60.env"
  local out2; out2=$( cd "$K" && env HOME="$T/home" PATH="$T/bin:$PATH" GATEWAY="$GATEWAY" \
                      SSH_KEY="$T/home/.ssh/$(basename "$SSH_KEY")" bash ./install.sh ssh admin </dev/null 2>&1 )
  rm -f "$K/u60.env"
  echo "$out2" | grep -E '^\[(device|!|\*)\]|没有重启验证'
  echo "$out2" | grep -q '没有重启验证' || { echo "✗ 没终端时应跳过重启并提示 ./install.sh reboot" >&2; exit 1; }
  echo "=== ./install.sh reboot（reboot 已替换成空操作，走一遍等待和状态）"
  R reboot </dev/null
  echo "=== 错误路径：非法后台密码、未知组件、status 和组件混用"
  AGENT_PASSWORD='a b' R admin </dev/null && { echo "✗ 应该拒绝含空格的密码" >&2; exit 1; } || true
  # 环境变量优先于 u60.env：文件里是空（=沿用），命令行给了非法值，应该按命令行的值被拒绝
  printf "AGENT_PASSWORD=''\n" > "$K/u60.env"
  local out3; out3=$(AGENT_PASSWORD='a b' R admin </dev/null 2>&1) && { echo "✗ 环境变量没有优先" >&2; exit 1; }
  rm -f "$K/u60.env"
  echo "$out3" | grep -q '不能含引号' || { echo "✗ 环境变量没有优先于 u60.env" >&2; exit 1; }
  echo "[env>u60.env] 命令行的 AGENT_PASSWORD 优先 ✓"
  R foo </dev/null && { echo "✗ 应该拒绝未知组件" >&2; exit 1; } || true
  R ssh status </dev/null && { echo "✗ 应该拒绝 status 混用" >&2; exit 1; } || true
  echo "=== status";                          R status </dev/null

  clean
  local after; after=$(dev "$SNAP")
  ssh -O exit -o ControlPath=/tmp/u60-kit-sandbox-cm "$HOST" 2>/dev/null || true
  [ "$before" = "$after" ] && echo "✓ $HOST 上的设备文件和进程与测试前一致" || { echo "✗ $HOST 状态有变化:"; diff <(echo "$before") <(echo "$after"); exit 1; }
}

# 测试前后对比：设备真实位置的文件、两个自动升级开关、我们的进程
SNAP='md5sum /etc/rc.local /etc/dropbear/authorized_keys /data/local/tmp/start_*.sh /data/zte-agent.env /data/luyaz/luyaz-web /etc/init.d/*; uci get zwrt_zte_dm.dm_update.dm_update_mode; uci get zwrt_zte_dm.dm_update.TURNOFFPOLLING; pidof luyaz-web luyaz-sentinel u60pro-devui luyaz-ui zwrt-datad'

case "${1:-}" in
  dry) prepare ;;
  run) run ;;
  clean) need_target; clean ;;
  *) echo "usage: $0 {dry|run|clean}"; exit 1 ;;
esac
