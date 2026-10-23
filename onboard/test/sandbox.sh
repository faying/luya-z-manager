#!/usr/bin/env bash
# 装机包回归测试：把最新的装机包改写成「沙盒版」，在一台正在使用的 U60 上把 install.sh 走一遍，
# 最后核对设备原有的文件和进程没有被动过。
# ── 用法
#   onboard/test/sandbox.sh dry                                      只在本机解包、改写、做防呆检查；不连设备
#   HOST=<root@设备地址 或 ssh 别名> GATEWAY=<设备地址> SSH_KEY=<私钥> onboard/test/sandbox.sh run     dry + 在设备上跑各场景
#   HOST=… GATEWAY=… SSH_KEY=… onboard/test/sandbox.sh clean                          停掉沙盒进程、删沙盒目录
# ── 前提
# run/clean 的三个变量没有默认值，必须显式给；HOST 经 `ssh -G` 解析出来的地址必须等于 GATEWAY。
# SSH_KEY 要是这台设备 authorized_keys 里已有的那把（沙盒里的 dropbear 仍然认 /etc/dropbear/authorized_keys）。
# ── 隔离手段
#   - 假 adb：shell/push 走 ssh 到 HOST；命令里带 reboot 的一律拒绝
#   - 设备端脚本的所有落盘路径改到 /data/local/tmp/kit-sb；沙盒 dropbear 用 2223 端口
#   - 关自动升级的两条命令换成空操作（设备上本来就是关的，测试前后会核对）
#   - admin（luyaz-web）装进沙盒目录、只听 127.0.0.1:19090，不装 init 脚本和设备脚本，不写设备清单
#   - ui、recover、chill 在沙盒里直接报错退出（它们会换掉设备上正在用的哨兵、屏幕、开机收尾、代理）
#   - 改写后只要还能找到写真实位置的语句，就中止，不连设备
set -euo pipefail   # 出错即停
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HOST="${HOST:-}"
GATEWAY="${GATEWAY:-}"
SSH_KEY="${SSH_KEY:-}"

WORK=/tmp/u60-kit-sandbox          # 本机工作目录
KIT="$WORK/luyaz-kit"              # 解开后的装机包
SB=/data/local/tmp/kit-sb          # 设备上的沙盒根
MUX=/tmp/u60-kit-sandbox-cm        # ssh 连接复用的控制文件
SSH_OPTS=(-o ControlMaster=auto -o ControlPath=$MUX -o ControlPersist=300 -o LogLevel=ERROR)

# 测试前后各拍一次：真实位置的关键文件、两个自动升级开关、我们的进程
SNAP='md5sum /etc/rc.local /etc/dropbear/authorized_keys /data/local/tmp/start_*.sh /data/luyaz/web.env /data/luyaz/luyaz-web /etc/init.d/*; uci get zwrt_zte_dm.dm_update.dm_update_mode; uci get zwrt_zte_dm.dm_update.TURNOFFPOLLING; pidof luyaz-web luyaz-sentinel luyaz-ui luyaz-core'

say()   { echo "=== $*"; }

abort() { echo "✗ $*" >&2; exit 1; }

on_device() { ssh "${SSH_OPTS[@]}" "$HOST" "$@"; }

close_mux() { ssh -O exit -o ControlPath=$MUX "$HOST" 2>/dev/null || true; }

# ── 目标检查（只在要连设备时做）────────────────────────────────────────────
check_target() {
  local v missing="" resolved
  for v in HOST GATEWAY SSH_KEY; do [ -n "${!v}" ] || missing="$missing $v"; done
  if [ -n "$missing" ]; then
    echo "✗ 缺少:$missing（例：HOST=root@192.168.0.1 GATEWAY=192.168.0.1 SSH_KEY=~/.ssh/id_ed25519）" >&2; exit 2
  fi
  if [ ! -f "$SSH_KEY" ] || [ ! -f "$SSH_KEY.pub" ]; then
    echo "✗ 找不到 $SSH_KEY 或 $SSH_KEY.pub" >&2; exit 2
  fi
  resolved=$(ssh -G "$HOST" 2>/dev/null | awk '$1 == "hostname" { print $2; exit }')
  if [ "$resolved" != "$GATEWAY" ]; then
    echo "✗ ssh $HOST 实际连到 ${resolved:-?}，和 GATEWAY=$GATEWAY 对不上" >&2
    echo "  HOST 写 root@$GATEWAY，或 ~/.ssh/config 里 HostName 是 $GATEWAY 的别名（不认域名）" >&2; exit 2
  fi
}

# ── 清理设备上的沙盒 ──────────────────────────────────────────────────────
clean() { # 停沙盒进程、删沙盒文件
  on_device "kill \$(cat /tmp/kitsb-dropbear.pid /tmp/kitsb-agent.pid /tmp/kitsb-lweb.pid 2>/dev/null) 2>/dev/null; sleep 1
       rm -rf $SB /data/local/tmp/u60-kit /tmp/kitsb-* /tmp/u60-kit-status.sh"   # 沙盒目录、推送目录、临时文件
  close_mux
  echo "sandbox cleaned on $HOST"   # 完成
}   # clean

# ── 本机准备：解包 → 假 adb → 改写 → 防呆 → 重算 MANIFEST ─────────────────
write_fake_adb() {
  # 生成的假 adb：devices 报一台 SANDBOX；shell 经 ssh 执行（含 reboot 的拒绝）；push 用 cat 或 tar 管道
  cat >"$WORK/bin/adb" <<FAKE_ADB
#!/usr/bin/env bash
SSH=(ssh ${SSH_OPTS[*]} $HOST)
cmd=\$1; shift
if [ "\$cmd" = devices ]; then
    printf 'List of devices attached\nSANDBOX\tdevice\n'
elif [ "\$cmd" = shell ]; then
    case "\$*" in *reboot*) echo "fake-adb: refusing reboot" >&2; exit 1 ;; esac
    "\${SSH[@]}" "\$*"
elif [ "\$cmd" = push ] && [ -d "\$1" ]; then
    tar czf - -C "\$(dirname "\$1")" "\$(basename "\$1")" | "\${SSH[@]}" "mkdir -p '\$2' && tar xzf - -C '\$2'"
elif [ "\$cmd" = push ]; then
    "\${SSH[@]}" "cat > '\$2'" <"\$1"
else
    echo "fake-adb: unsupported \$cmd \$*" >&2; exit 1
fi
FAKE_ADB
  chmod +x "$WORK/bin/adb"
}

# 第一轮：do_admin 换成沙盒里的 luyaz-web（$SB/luyaz，127.0.0.1:19090，密码/会话/logo 都在 $SB），
# 设备脚本只放 web-auth.sh，不碰 /etc/init.d，不写清单。规则按 device/install.sh 的原文逐行匹配。
rewrite_admin() {
  local f="$KIT/device/install.sh" marker
  cat > "$WORK/admin.sed" <<SED
s|^LW=/data/luyaz\$|LW=$SB/luyaz|
s|^    install_scripts   #.*|    mkdir -p $SB/scripts \&\& put "\$P/scripts/web-auth.sh" $SB/scripts/web-auth.sh 755; S=$SB/scripts|
s|^    \[ -x /etc/init.d/luyaz-web \] .*|    kill "\$(cat /tmp/kitsb-lweb.pid 2>/dev/null)" 2>/dev/null|
s|pidof luyaz-web >/dev/null \|\| netstat -ltn 2>/dev/null \| grep -q ':9090 '|kill -0 "\$(cat /tmp/kitsb-lweb.pid 2>/dev/null)" 2>/dev/null|
s|^    rc_add "/etc/init.d/luyaz-web start" "/etc/init.d/luyaz-web start"|    rc_add "sh $SB/lweb-start.sh" "$SB/lweb-start.sh"|
s|^    /etc/init.d/luyaz-web start\$|    sh $SB/lweb-start.sh|
s|^    svc_running luyaz-web \&\& pidof luyaz-web >/dev/null|    kill -0 "\$(cat /tmp/kitsb-lweb.pid 2>/dev/null)" 2>/dev/null|
s|^    kit_record lweb\$|    true|
SED
  sed -f "$WORK/admin.sed" "$f" > "$f.tmp" && mv "$f.tmp" "$f"
  for marker in "LW=$SB/luyaz" "S=$SB/scripts" "sh $SB/lweb-start.sh" "kitsb-lweb.pid"; do
    grep -qF "$marker" "$f" || abort "admin 的沙盒改写没生效：$marker（device/install.sh 改过了？）"
  done
}

# 第二轮：路径变量（X=\${KIT_X:-真实路径} 整行换掉）、ssh 相关路径和端口、自动升级、esim、禁用的组件。
# 只认新写法会漏（2026-10-14 曾因此往真 rc.local 加过一行），所以之后再查一遍有没有剩下的 KIT_ 默认值。
rewrite_paths() {
  local f="$KIT/device/install.sh"
  local rules=(
    -e "s#^RC=\${KIT_RC:-/etc/rc.local}#RC=$SB/etc/rc.local#"
    -e "s#^STATE=\${KIT_STATE:-/data/luyaz/kit}#STATE=$SB/state#"
    -e "s#^SHIPD=\${KIT_SHIP_DIR:-/data/luyaz/ship}#SHIPD=$SB/luyaz-ship#"
    -e "s#^S=\${KIT_SCRIPTS:-/data/luyaz/scripts}#S=$SB/scripts#"
    -e "s#^CORE_DIR=\${KIT_CORE_DIR:-/data/luyaz/core}#CORE_DIR=$SB/luyaz-core#"
    -e "s#^SCREEN_DIR=\${KIT_SCREEN_DIR:-/data/luyaz/screen}#SCREEN_DIR=$SB/luyaz-screen#"
    -e "s#^WEB_ENV=/data/luyaz/web.env#WEB_ENV=$SB/web.env#"
    -e "s#/data/local/tmp/start_dropbear.sh#$SB/start_dropbear.sh#g"
    -e "s#/data/ssh#$SB/ssh#g"
    -e "s#/etc/dropbear#$SB/etc/dropbear#g"
    -e "s#/tmp/dropbear.pid#/tmp/kitsb-dropbear.pid#g"
    -e "s#-p 2222#-p 2223#g; s#:2222 #:2223 #g; s#(:2222)#(:2223)#g"
    -e "s#ubus call zwrt_zte_dm set_update_mode#true#"
    -e "s#uci -q set zwrt_zte_dm.dm_update.TURNOFFPOLLING=1 \&\& uci -q commit zwrt_zte_dm#true#"
    -e "s#127.0.0.1:9090#127.0.0.1:19090#g"
    -e "s#put_tree \"\$P/esim.tgz\" /data/esim#put_tree \"\$P/esim.tgz\" $SB/esim; sed -i \"s,/data/esim,$SB/esim,g\" $SB/esim/lpac.sh#"
    -e "s#/data/esim/lpac.sh chip info#$SB/esim/lpac.sh chip info#"
    -e 's#^do_ui() {#do_ui() { die "sandbox: ui disabled";#'
    -e 's#^do_recover() {#do_recover() { die "sandbox: recover disabled";#'
    -e 's#^do_chill() {#do_chill() { die "sandbox: chill disabled";#'
  )
  sed "${rules[@]}" "$f" > "$f.tmp" && mv "$f.tmp" "$f"
}

# 防呆：改写后的设备端脚本里不能再有写真实位置的语句。
# 不看：注释、p "…" 输出行，以及沙盒里禁用/不会调用的函数（从「名字() {」到下一个行首「}」整段跳过）。
guard_device_script() {
  local f="$KIT/device/install.sh"
  local skipped='^(do_admin|do_ui|do_recover|install_scripts|place_recover|recover_put|rc_owner|owners_stop|sentinel_start|kit_record|do_chill|do_status)\(\) \{'
  local real='(^|[^b])/etc/rc\.local|/data/ssh|/data/luyaz|/data/plugins|/etc/init\.d/|put_tree "\$P/esim.tgz" /data/esim|set_update_mode|uci -q (set|commit)| -p 2222|127\.0\.0\.1:9090'
  local allowed='^[0-9]+:(SENT_LINE|LW)=|^[0-9]+:[[:space:]]*\[ -x "/etc/init.d/\$s" \]|svc_running|/etc/init.d/zte_topsw'
  if grep -n '\${KIT_[A-Z0-9_]*:-/' "$f"; then
    abort "上面这些路径变量没换成沙盒路径（device/install.sh 加了新的 KIT_ 变量？），先更新本脚本"
  fi
  if SKIP_RE="$skipped" awk '$0 ~ ENVIRON["SKIP_RE"] { off = 1 } !off { print } off && /^}/ { off = 0 }' "$f" \
       | grep -vE '^[[:space:]]*#|^[[:space:]]*p "' \
       | grep -nE "$real" \
       | grep -vE "$allowed"; then
    abort "设备端脚本有上面这些新语句，沙盒改写没覆盖到，先更新本脚本"
  fi   # 没有漏网的真实写入
}

# 电脑端 install.sh：ssh 端口换成 2223，重启换成空操作，web.env 指到沙盒
rewrite_host_script() {
  # 端口 2222→2223；dev_run "reboot" 变成 echo；设备上的 web.env 指到沙盒
  sed -i '' \
      -e 's#^SSH_PORT=2222#SSH_PORT=2223#' \
      -e 's#dev_run "reboot"#dev_run "echo sandbox-no-reboot"#' \
      -e "s#/data/luyaz/web.env#$SB/web.env#g" \
      "$KIT/install.sh"
  if grep -n '"reboot"\|/data/luyaz/web.env' "$KIT/install.sh" | grep -v '"reboot")'; then
    abort "电脑端脚本里还有真实的 reboot 调用或设备真实路径，先更新本脚本"
  fi   # 改写完整
}

# 改过的文件要重算 sha256，不然 install.sh 的完整性检查会拒绝
refresh_manifest() {
  ( cd "$KIT"
    { sed -n '1,/^sha256/p' MANIFEST.txt
      find install.sh device payload -type f | LC_ALL=C sort | while read -r f; do   # 和 build-kit 同样的顺序和格式
        printf '%s  %s\n' "$(shasum -a 256 "$f" | awk '{print $1}')" "$f"
      done; } > MANIFEST.new && mv MANIFEST.new MANIFEST.txt )
}

prepare() {
  local tarball
  tarball=$(ls -t "$REPO"/onboard/dist/luyaz-kit-*.tar.gz 2>/dev/null | head -1)
  [ -n "$tarball" ] || { echo "先跑 onboard/build-kit.sh" >&2; exit 1; }
  rm -rf "$WORK" && mkdir -p "$WORK/bin" "$WORK/home/.ssh"
  tar xzf "$tarball" -C "$WORK"
  write_fake_adb
  rewrite_admin
  rewrite_paths
  guard_device_script
  rewrite_host_script
  refresh_manifest
  echo "✓ 沙盒改写和防呆检查通过：$KIT"
}

# ── 设备上的场景 ─────────────────────────────────────────────────────────
KEY_IN_HOME() { echo "$WORK/home/.ssh/$(basename "$SSH_KEY")"; }

# 在包目录里跑 install.sh：假 adb 在 PATH 前面，HOME 指到工作目录，默认不重启
kit() {
  ( cd "$KIT" && env HOME="$WORK/home" PATH="$WORK/bin:$PATH" GATEWAY="$GATEWAY" SSH_KEY="$(KEY_IN_HOME)" \
      NO_REBOOT=1 bash ./install.sh "$@" )
}

expect_fail() { "$@" </dev/null && abort "应该失败：$*" || true; }

stage_device() {
  clean >/dev/null   # 从干净的沙盒开始
  # 沙盒版 luyaz-web 的启动脚本，代替 /etc/init.d/luyaz-web start
  on_device "mkdir -p $SB && cat > $SB/lweb-start.sh" <<LWS
nohup $SB/luyaz/luyaz-web serve --bind 127.0.0.1:19090 --upstream http://127.0.0.1:9460 --password-file $SB/web.env \\
    --session-file $SB/web-sessions --logo-dir $SB/luyaz/operator-logos >/tmp/kitsb-lweb.log 2>&1 &
echo \$! > /tmp/kitsb-lweb.pid
LWS
  # 沙盒 rc.local = 设备 rc.local 去掉我们自己的启动行
  on_device "mkdir -p $SB/etc && sed -e '/^sh \/data/d' -e '/^# Tailscale/d' -e '/^\[ -x \/data\/plugins/d' /etc/rc.local > $SB/etc/rc.local && sh -n $SB/etc/rc.local"
}

# 没有真 ADB、没终端、没路由器密码：只应调匿名接口然后停下，不提交登录（不消耗密码次数）
scenario_no_router_password() {
  [ "$(adb devices 2>/dev/null | grep -c 'device$' || true)" = 0 ] || return 0
  say "缺路由器密码时在登录前停下"
  local log
  log=$( cd "$KIT" && env HOME="$WORK/home" GATEWAY="$GATEWAY" SSH_KEY="$(KEY_IN_HOME)" \
           bash ./install.sh ssh </dev/null 2>&1 ) && abort "应该失败"
  echo "$log" | tail -1
  echo "$log" | grep -q 'ROUTER_PASSWORD' || abort "没有提示 ROUTER_PASSWORD"
}

# 输入全走环境变量 / u60.env，stdin 接 /dev/null（和代装的 Claude Code 一样没有终端）
scenario_admin_needs_password() {
  say "沙盒里没装过后台、两个密码都没给：应在推送前停下"
  local log
  log=$(kit ssh admin </dev/null 2>&1) && abort "应该失败"
  echo "$log" | tail -1
  echo "$log" | grep -q '需要高级后台密码' || abort "没有在推送前要后台密码"
  echo "$log" | grep -q '已推送' && abort "失败前已经推送了"
  return 0
}

scenario_fresh_install() {
  say "ADB 模式全新安装（ssh admin esim，AGENT_PASSWORD 走环境变量）"
  AGENT_PASSWORD='kit-Test_123' kit ssh admin esim </dev/null
}

scenario_ssh_rerun() {
  say "SSH 模式重跑（u60.env 里 AGENT_PASSWORD 为空 = 沿用；不设 NO_REBOOT，没终端应跳过重启）"
  local log
  printf "AGENT_PASSWORD=''\n" > "$KIT/u60.env"
  log=$( cd "$KIT" && env HOME="$WORK/home" PATH="$WORK/bin:$PATH" GATEWAY="$GATEWAY" \
           SSH_KEY="$(KEY_IN_HOME)" bash ./install.sh ssh admin </dev/null 2>&1 )
  rm -f "$KIT/u60.env"
  echo "$log" | grep -E '^\[(device|!|\*)\]|没有重启验证'
  echo "$log" | grep -q '没有重启验证' || abort "没终端时应跳过重启并提示 ./install.sh reboot"
}

scenario_reboot_noop() {
  say "./install.sh reboot（reboot 已换成空操作，只走等待和状态）"
  kit reboot </dev/null
}

scenario_bad_input() {
  say "错误路径：非法后台密码、未知组件、status 和组件混用"
  AGENT_PASSWORD='a b' expect_fail kit admin
  # 命令行优先于 u60.env：文件里为空（沿用），环境变量给了非法值，应按环境变量被拒
  local log
  printf "AGENT_PASSWORD=''\n" > "$KIT/u60.env"
  log=$(AGENT_PASSWORD='a b' kit admin </dev/null 2>&1) && abort "环境变量没有优先"
  rm -f "$KIT/u60.env"
  echo "$log" | grep -q '不能含引号' || abort "环境变量没有优先于 u60.env"
  echo "[env>u60.env] AGENT_PASSWORD：命令行优先 ✓"
  expect_fail kit foo
  expect_fail kit ssh status
}

run() {
  check_target
  prepare
  cp "$SSH_KEY" "$SSH_KEY.pub" "$WORK/home/.ssh/"
  stage_device
  local before after
  before=$(on_device "$SNAP")

  scenario_no_router_password
  scenario_admin_needs_password
  scenario_fresh_install
  scenario_ssh_rerun
  scenario_reboot_noop
  scenario_bad_input
  say "status"; kit status </dev/null
  clean   # 收尾，再拍一次快照比较
  after=$(on_device "$SNAP")
  close_mux
  if [ "$before" = "$after" ]; then
    echo "✓ $HOST 上的设备文件和进程与测试前一致"
  else
    echo "✗ $HOST 状态有变化:"; diff <(echo "$before") <(echo "$after"); exit 1
  fi
}   # run

# ── 入口
mode=${1:-}
if [ "$mode" = dry ]; then prepare
elif [ "$mode" = run ]; then run
elif [ "$mode" = clean ]; then check_target; clean
else echo "usage: $0 {dry|run|clean}"; exit 1
fi
