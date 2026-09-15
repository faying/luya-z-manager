#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# 装机包回归测试：在一台正在使用的 U60 Pro（MU5250）上开沙盒，完整跑一遍 install.sh 的 ADB 流程
#
#   onboard/test/sandbox.sh run       # 解包 → 改写成沙盒 → 假 adb 跑 ssh/admin/esim → status
#   onboard/test/sandbox.sh clean     # 杀掉沙盒进程、删目录
#
# 怎么做到不碰设备原有配置：
#   • 假 adb：adb shell/push 转成 ssh $HOST，拒绝任何含 reboot 的命令
#   • 设备端脚本路径全改写到 /data/local/tmp/kit-sb；dropbear 用 2223，agent 只听 127.0.0.1:19090
#   • FOTA 设置、killall zte-agent 打桩；devui 组件直接禁用（会抢屏幕）
#   • 改写后还残留真实路径就中止
# 设备上的 dropbear 仍然认 /etc/dropbear/authorized_keys，所以 SSH_KEY 要用那台设备已授权的 key。
#
# 环境变量：HOST（ssh 别名，默认 u60）、GATEWAY（默认 192.168.0.1）、SSH_KEY（默认 ~/.ssh/u60）
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HOST="${HOST:-u60}"
GATEWAY="${GATEWAY:-192.168.0.1}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/u60}"
T=/tmp/u60-kit-sandbox
SB=/data/local/tmp/kit-sb
CM=(-o ControlMaster=auto -o ControlPath=/tmp/u60-kit-sandbox-cm -o ControlPersist=300 -o LogLevel=ERROR)

dev() { ssh "${CM[@]}" "$HOST" "$@"; }

clean() {
  dev "kill \$(cat /tmp/kitsb-dropbear.pid /tmp/kitsb-agent.pid 2>/dev/null) 2>/dev/null; sleep 1
       rm -rf $SB /data/local/tmp/u60-kit /tmp/kitsb-* /tmp/u60-kit-status.sh"
  ssh -O exit -o ControlPath=/tmp/u60-kit-sandbox-cm "$HOST" 2>/dev/null || true
  echo "sandbox cleaned on $HOST"
}

run() {
  local kit; kit=$(ls -t "$ROOT"/onboard/dist/u60-kit-*.tar.gz 2>/dev/null | head -1)
  [ -n "$kit" ] || { echo "先跑 onboard/build-kit.sh" >&2; exit 1; }
  rm -rf "$T" && mkdir -p "$T/bin" "$T/home/.ssh"
  tar xzf "$kit" -C "$T"
  local K=$T/u60-kit

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

  sed -e "s#^RC=/etc/rc.local#RC=$SB/etc/rc.local#" \
      -e "s#^STATE=/data/u60-kit#STATE=$SB/state#" \
      -e "s#/data/local/tmp/start_dropbear.sh#$SB/start_dropbear.sh#g" \
      -e "s#/data/local/tmp/start_zte_agent.sh#$SB/start_zte_agent.sh#g" \
      -e "s#/data/ssh#$SB/ssh#g" \
      -e "s#/etc/dropbear#$SB/etc/dropbear#g" \
      -e "s#/tmp/dropbear.pid#/tmp/kitsb-dropbear.pid#g" \
      -e "s#-p 2222#-p 2223#g; s#:2222 #:2223 #g; s#(:2222)#(:2223)#g" \
      -e "s#ubus call zwrt_zte_dm set_update_mode#true#" \
      -e "s#killall zte-agent#true#" \
      -e "s#/tmp/zte-agent.pid#/tmp/kitsb-agent.pid#g; s#/tmp/zte-agent.log#/tmp/kitsb-agent.log#g" \
      -e "s#/data/zte-agent#$SB/zte-agent#g; s#/data/admin#$SB/admin#g" \
      -e "s#pidof zte-agent >/dev/null || die#kill -0 \"\$(cat /tmp/kitsb-agent.pid)\" 2>/dev/null || die#" \
      -e "s#127.0.0.1:9090#127.0.0.1:19090#g" \
      -e "s#          cat \"\$P/agent.env\"#          cat \"\$P/agent.env\"; echo \"export ZTE_AGENT_BIND=127.0.0.1:19090 ZTE_AGENT_UI_DIR=$SB/admin\"#" \
      -e "s#put_tree \"\$P/esim.tgz\" /data/esim#put_tree \"\$P/esim.tgz\" $SB/esim; sed -i \"s,/data/esim,$SB/esim,g\" $SB/esim/lpac.sh#" \
      -e "s#/data/esim/lpac.sh chip info#$SB/esim/lpac.sh chip info#" \
      -e 's#^do_devui() {#do_devui() { die "sandbox: devui disabled";#' \
      "$K/device/install.sh" > "$K/device/install.sb"
  mv "$K/device/install.sb" "$K/device/install.sh"

  # 防呆：会写设备真实位置的东西一样都不能剩（注释行和只读的 status 除外）
  if grep -vE '^[[:space:]]*#|^[[:space:]]*p "' "$K/device/install.sh" \
     | grep -nE '(^|[^b])/etc/rc\.local|/data/ssh|/data/zte-agent|/data/admin|put_tree "\$P/esim.tgz" /data/esim|set_update_mode|killall zte-agent| -p 2222|127\.0\.0\.1:9090'; then
    echo "✗ 设备端脚本改过了，沙盒改写没覆盖到上面这些行，先更新本脚本" >&2; exit 1
  fi

  sed -i '' -e 's#^SSH_PORT=2222#SSH_PORT=2223#' -e 's#dev_run "reboot"#dev_run "echo sandbox-no-reboot"#' \
            -e "s#/data/local/tmp/start_zte_agent.sh#$SB/start_zte_agent.sh#g" "$K/install.sh"
  if grep -n '"reboot"\|/data/local/tmp/start_' "$K/install.sh" | grep -v '"reboot")'; then
    echo "✗ 电脑端脚本里还有真的 reboot 调用或设备真实路径，先更新本脚本" >&2; exit 1
  fi
  ( cd "$K" && { sed -n '1,/^sha256/p' MANIFEST.txt
      find install.sh device payload -type f | LC_ALL=C sort | while read -r f; do
        printf '%s  %s\n' "$(shasum -a 256 "$f" | awk '{print $1}')" "$f"; done; } > M && mv M MANIFEST.txt )
  cp "$SSH_KEY" "$SSH_KEY.pub" "$T/home/.ssh/"

  clean >/dev/null
  dev "mkdir -p $SB/etc && sed -e '/^sh \/data/d' -e '/^# Tailscale/d' -e '/^\[ -x \/data\/plugins/d' /etc/rc.local > $SB/etc/rc.local && sh -n $SB/etc/rc.local"
  local before; before=$(dev 'md5sum /etc/rc.local /etc/dropbear/authorized_keys /data/local/tmp/start_*.sh; uci get zwrt_zte_dm.dm_update.dm_update_mode; pidof zte-agent u60pro-devui')

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
  local after; after=$(dev 'md5sum /etc/rc.local /etc/dropbear/authorized_keys /data/local/tmp/start_*.sh; uci get zwrt_zte_dm.dm_update.dm_update_mode; pidof zte-agent u60pro-devui')
  ssh -O exit -o ControlPath=/tmp/u60-kit-sandbox-cm "$HOST" 2>/dev/null || true
  [ "$before" = "$after" ] && echo "✓ $HOST 上的设备文件和进程与测试前一致" || { echo "✗ $HOST 状态有变化:"; diff <(echo "$before") <(echo "$after"); exit 1; }
}

case "${1:-}" in
  run) run ;;
  clean) clean ;;
  *) echo "usage: $0 {run|clean}"; exit 1 ;;
esac
