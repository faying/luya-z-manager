#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# U60 Pro 装机包 — 在电脑上运行（macOS / Linux / Windows 的 Git Bash）
#
#   ./install.sh                     # 全套：开 ADB → SSH → 高级后台 → devui → eSIM
#   ./install.sh ssh                 # 只开 ADB + 持久化 SSH
#   ./install.sh admin devui         # 只装指定组件（SSH 或 ADB 通着就行）
#   ./install.sh status              # 看设备上各组件状态
#   ./install.sh doctor              # 只读体检（开机同步、自动升级、各服务、心跳、Wi-Fi、告警…）
#   ./install.sh reboot              # 重启设备并确认各组件开机自己起来（要 SSH 已通）
#   ./install.sh backup [--with-tailscale]   # 把配置备份到这台电脑（BACKUP_DIR，默认 ./backups）
#   ./install.sh restore <备份.tgz>          # 先列出会改哪些文件，输入 yes 才写（没终端时要 RESTORE_YES=1）
#
# 在终端里跑会逐项提示输入；没有终端（比如由 Claude Code 代跑）时从环境变量读：
#   ROUTER_PASSWORD   路由器管理密码（开 ADB 时才需要）
#   AGENT_PASSWORD    高级后台密码（不设 = 同路由器密码；设成空 = 沿用设备上已有的）
#   REBOOT=1          装完直接重启验证（不设时终端里会问，没终端就跳过）
# 这些也可以写进本目录的 u60.env（模板见 u60.env.example），脚本启动时自动读取。
# 其他：GATEWAY（默认 192.168.0.1）、SSH_KEY（默认 ~/.ssh/id_ed25519）
#
# 只适用于 CN 固件 B27 及以下；B28 起中兴删掉了开 ADB 的接口。
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

# Git Bash 会把 /data/... 这类参数改写成 Windows 路径，adb 就推错地方了。所以全局关掉改写，
# 交给 adb.exe 的本地路径再用 cygpath 手动转（macOS/Linux 没有 cygpath，原样返回）
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'
localpath() { if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s' "$1"; fi; }

if [ -t 1 ]; then
  RED=$'\033[0;31m'; GREEN=$'\033[0;32m'; YELLOW=$'\033[1;33m'; CYAN=$'\033[0;36m'; BOLD=$'\033[1m'; NC=$'\033[0m'
else
  RED=; GREEN=; YELLOW=; CYAN=; BOLD=; NC=
fi
info() { printf "${CYAN}[*]${NC} %s\n" "$1"; }
ok()   { printf "${GREEN}[+]${NC} %s\n" "$1"; }
warn() { printf "${YELLOW}[!]${NC} %s\n" "$1"; }
fail() { printf "${RED}[-]${NC} %s\n" "$1" >&2; exit 1; }

KIT="$(cd "$(dirname "$0")" && pwd)"
# u60.env 只补没设的变量：命令行/环境变量里给了的优先
if [ -f "$KIT/u60.env" ]; then
  __given=$(for __v in ROUTER_PASSWORD AGENT_PASSWORD GATEWAY REBOOT SSH_KEY; do
              if [ -n "${!__v+x}" ]; then printf '%s=%q\n' "$__v" "${!__v}"; fi; done)
  set -a; . "$KIT/u60.env"; set +a
  eval "$__given"
fi
GATEWAY="${GATEWAY:-192.168.0.1}"
SSH_PORT=2222
SSH_KEY="${SSH_KEY:-$HOME/.ssh/id_ed25519}"
STAGE=/data/local/tmp/u60-kit
ALL="ssh admin devui esim"
TTY=false; [ -t 0 ] && TTY=true

# backup / restore 带自己的参数，先拿出来，剩下的按组件解析
WITH_TS=
RESTORE_FILE=
case "${1:-}" in
  backup)
    [ "${2:-}" = --with-tailscale ] && WITH_TS=--with-tailscale
    set -- backup ;;
  restore)
    RESTORE_FILE="${2:-}"
    [ -f "$RESTORE_FILE" ] || { echo "用法: ./install.sh restore <备份.tgz>" >&2; exit 1; }
    set -- restore ;;
esac

COMPONENTS="${*:-$ALL}"
case " $COMPONENTS " in
  " status "|" reboot "|" doctor "|" backup "|" restore ") ;;
  *" status "*|*" reboot "*|*" doctor "*) fail "status / doctor / reboot 要单独跑，不能和组件混在一起" ;;
  *) for c in $COMPONENTS; do
       case "$c" in ssh|admin|devui|esim) ;; *) fail "不认识的组件: ${c}（可选 ssh admin devui esim，或 status / doctor / reboot）" ;; esac
     done ;;
esac
INSTALLING=true
case "$COMPONENTS" in status|reboot|doctor|backup|restore) INSTALLING=false ;; esac

# ── 0. 检查工具和包完整性 ────────────────────────────────────────────────────
need() { command -v "$1" >/dev/null 2>&1 || fail "缺少 $1 —— $2"; }
need_adb() { need adb "macOS: brew install android-platform-tools；Windows: 下载 Google platform-tools 并加进 PATH；Linux: apt install adb"; }
need ssh "Windows 用 Git Bash 自带的；Linux: apt install openssh-client"
need ssh-keygen "同 ssh"
need curl "系统一般自带"
need tar "系统一般自带"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum | awk '{print $1}'
  else shasum -a 256 | awk '{print $1}'; fi
}

if $INSTALLING; then
  info "校验装机包文件…"
  ( cd "$KIT" && while read -r sum file; do
      [ -f "$file" ] || fail "包里缺文件: ${file}（包不完整，重新解压一次）"
      [ "$(sha256 < "$file")" = "$sum" ] || fail "文件损坏: ${file}（重新下载装机包）"
    done < <(grep -E '^[0-9a-f]{64}  ' MANIFEST.txt) )
  ok "装机包完整。"
fi

# ── 1. SSH 密钥（安装时没有就生成一把）───────────────────────────────────────────
if $INSTALLING && [ ! -f "$SSH_KEY" ]; then
  info "生成 SSH 密钥 $SSH_KEY …"
  mkdir -p "$(dirname "$SSH_KEY")"
  ssh-keygen -t ed25519 -f "$SSH_KEY" -N "" -C "u60-$(whoami)" >/dev/null
fi
$INSTALLING && { [ -f "$SSH_KEY.pub" ] || fail "找不到公钥 $SSH_KEY.pub"; }

# 同一个 192.168.0.1 可能是别的路由器，host key 单独记一个文件，避免冲突报错
KNOWN="$HOME/.ssh/known_hosts_u60"
SSH_OPTS=(-p "$SSH_PORT" -i "$SSH_KEY" -o IdentitiesOnly=yes -o ConnectTimeout=6
          -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$KNOWN"
          -o ServerAliveInterval=5 -o ServerAliveCountMax=3 -o LogLevel=ERROR)
ssh_ok() { [ -f "$SSH_KEY" ] && ssh "${SSH_OPTS[@]}" -o BatchMode=yes "root@$GATEWAY" true </dev/null >/dev/null 2>&1; }

adb_count() { adb devices 2>/dev/null | grep -c 'device$' || true; }
adb_ok() { [ "$(adb_count)" -ge 1 ]; }
# 未授权/离线的设备也会让 adb shell 报 more than one device，所以数全部
adb_all() { adb devices 2>/dev/null | awk 'NR > 1 && NF >= 2' | wc -l | tr -d ' '; }
# 确认 ADB 连的真是 U60（root + 有中兴的 ubus 对象），免得把 root 脚本推到别的安卓设备上
adb_is_u60() { adb shell '[ "$(id -u)" = 0 ] && ubus list zwrt_bsp.usb >/dev/null 2>&1 && echo U60_OK' 2>/dev/null | grep -q U60_OK; }

# ── 2. 选通道：SSH 通就用 SSH，否则 ADB，都不通就去网页后台开 ADB ─────────────
ubus_call() { # <session> <object> <method> <json-params>
  # 不走代理：国内电脑常开着代理，不能让用户关（Claude Code 自己也靠它联网）
  curl -s -m 10 --noproxy '*' "http://$GATEWAY/ubus/?t=$(date +%s)" \
    -H 'Content-Type: application/json' -H "Referer: http://$GATEWAY/" -H "Origin: http://$GATEWAY" \
    -d "[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"call\",\"params\":[\"$1\",\"$2\",\"$3\",$4]}]"
}
field() { sed -n "s/.*\"$1\":\"\\{0,1\\}\\([^\",}]*\\).*/\\1/p"; }
upper() { tr '[:lower:]' '[:upper:]'; }

ROUTER_PW="${ROUTER_PASSWORD:-}"
enable_adb() {
  local anon=00000000000000000000000000000000 resp salt left lock hash session fw
  info "通过网页后台开启 ADB（电脑要连着 U60 的 Wi-Fi）…"
  resp=$(ubus_call "$anon" zwrt_web web_login_info '{}') \
    || fail "连不上 http://$GATEWAY —— 电脑连的是这台 U60 的 Wi-Fi 吗？代理/VPN 开着 TUN/增强模式的话，把 192.168.0.0/16 设成直连；地址不是 $GATEWAY 的话用 GATEWAY=… ./install.sh"
  salt=$(printf '%s' "$resp" | field zte_web_sault)
  left=$(printf '%s' "$resp" | field login_fail_num)
  lock=$(printf '%s' "$resp" | field login_fail_lock_lefttime)
  [ -n "$salt" ] || fail "网页后台返回不认识的内容，这可能不是 U60 Pro：$resp"
  [ "${lock:-0}" -gt 0 ] 2>/dev/null && fail "网页后台密码输错太多次被锁了，${lock} 秒后再试"
  if [ -z "$ROUTER_PW" ]; then
    $TTY || fail "需要路由器管理密码：设置 ROUTER_PASSWORD 环境变量或写进 u60.env（剩余可试 ${left:-?} 次）"
    printf "${CYAN}路由器管理密码（机身背面/网页登录用的那个，剩余可试 %s 次）:${NC} " "${left:-?}"
    read -rs ROUTER_PW; echo
    [ -n "$ROUTER_PW" ] || fail "密码不能为空"
  fi

  hash=$(printf '%s' "$ROUTER_PW" | sha256 | upper)
  hash=$(printf '%s' "$hash$salt" | sha256 | upper)
  session=$(ubus_call "$anon" zwrt_web web_login "{\"password\":\"$hash\"}" | field ubus_rpc_session)
  if [ -z "$session" ]; then
    left=$(ubus_call "$anon" zwrt_web web_login_info '{}' | field login_fail_num)
    fail "登录失败，路由器管理密码不对（还能试 ${left:-?} 次，用完会锁一段时间；别盲目重试）"
  fi
  ok "已登录网页后台。"

  fw=$(ubus_call "$session" zwrt_web device_info '{}' | field wa_inner_version)
  [ -n "$fw" ] && info "固件版本: $fw"

  resp=$(ubus_call "$session" zwrt_bsp.usb set '{"mode":"debug"}')
  case "$resp" in
    *'"result":[0'*) ok "已切到 USB 调试模式。" ;;
    *) fail "开 ADB 被拒绝（固件是 B28 或更新？那版把这个接口删了，本包不适用）：$resp" ;;
  esac

  info "等 ADB 设备出现（用 USB-C 数据线把 U60 接到这台电脑；最多等 90 秒）…"
  local i=0
  while ! adb_ok; do
    i=$((i + 3)); [ $i -le 90 ] || fail "90 秒内没看到 ADB 设备。换根能传数据的线/换个 USB 口；Windows 可能要装 ADB 驱动（见 README）"
    sleep 3
  done
  ok "ADB 已连接。"
}

if ssh_ok; then
  CH=ssh; ok "SSH 已经能登录，走 SSH。"
elif [ "$COMPONENTS" = reboot ]; then
  fail "SSH 不通，没法做重启验证（先跑 ./install.sh，或确认电脑连着 U60 的 Wi-Fi）"
else
  if [ "$INSTALLING" = false ] && [ "$COMPONENTS" != reboot ] && ! command -v adb >/dev/null 2>&1; then
    fail "SSH 不通（电脑连着 U60 的 Wi-Fi 吗？），这台电脑也没装 adb"
  fi
  need_adb
  if adb_ok; then
    CH=adb; ok "检测到 ADB 设备，走 ADB。"
  else
    [ "$INSTALLING" = false ] && fail "SSH 和 ADB 都不通，先跑一次 ./install.sh"
    enable_adb; CH=adb
  fi
  if [ "$(adb_all)" -gt 1 ] && [ -z "${ANDROID_SERIAL:-}" ]; then
    fail "电脑上接了不止一个 ADB 设备（含未授权/离线的），拔掉别的（或设置 ANDROID_SERIAL）再试"
  fi
  adb_is_u60 || fail "ADB 连着的设备不是 U60（或者没有 root）。拔掉手机等其他安卓设备再试"
fi

dev_run() { # <shell command>，输出原样打出来
  if [ "$CH" = ssh ]; then ssh "${SSH_OPTS[@]}" "root@$GATEWAY" "$1" </dev/null
  else adb shell "$1"; fi
}

show_status() { # 状态不需要推整个包，只推设备端脚本
  if [ "$CH" = ssh ]; then
    ssh "${SSH_OPTS[@]}" "root@$GATEWAY" "cat > /tmp/u60-kit-status.sh" < "$KIT/device/install.sh" \
      || fail "读状态失败：SSH 推送状态脚本没成功（刚连过的话等 20 秒再试）"
  else
    adb push "$(localpath "$KIT/device/install.sh")" /tmp/u60-kit-status.sh >/dev/null || fail "读状态失败：adb push 没成功"
  fi
  dev_run "sh /tmp/u60-kit-status.sh status; rm -f /tmp/u60-kit-status.sh" | tr -d '\r' \
    || fail "读状态失败：设备上执行状态脚本没成功"
}

show_doctor() { # 体检脚本是独立的：推包里那份到 /tmp 直接跑，没装过 guard 的设备也能查
  local src="$KIT/payload/guard/doctor.sh"
  [ -f "$src" ] || fail "包里没有 payload/guard/doctor.sh"
  if [ "$CH" = ssh ]; then
    ssh "${SSH_OPTS[@]}" "root@$GATEWAY" "cat > /tmp/u60-kit-doctor.sh" < "$src" || fail "体检失败：推送脚本没成功"
  else
    adb push "$(localpath "$src")" /tmp/u60-kit-doctor.sh >/dev/null || fail "体检失败：adb push 没成功"
  fi
  dev_run "sh /tmp/u60-kit-doctor.sh; r=\$?; rm -f /tmp/u60-kit-doctor.sh; exit \$r" | tr -d '\r'
}

push_file() { # <local> <device path>
  if [ "$CH" = ssh ]; then
    ssh "${SSH_OPTS[@]}" "root@$GATEWAY" "cat > '$2'" < "$1"
  else
    adb push "$(localpath "$1")" "$2" >/dev/null
  fi
}

# 配置备份只存在这台电脑上。设备端脚本是包里的 payload/guard/config-backup.sh。
do_backup() {
  local dir="${BACKUP_DIR:-$KIT/backups}" out
  mkdir -p "$dir" && chmod 700 "$dir"
  out="$dir/config-$(date +%Y%m%d-%H%M%S).tgz"
  push_file "$KIT/payload/guard/config-backup.sh" /tmp/u60-cb.sh || fail "推送备份脚本失败"
  if [ "$CH" = ssh ]; then
    ssh "${SSH_OPTS[@]}" "root@$GATEWAY" "sh /tmp/u60-cb.sh export $WITH_TS" < /dev/null > "$out"
  else
    adb exec-out "sh /tmp/u60-cb.sh export $WITH_TS" > "$out"
  fi
  chmod 600 "$out"
  [ -s "$out" ] || { rm -f "$out"; fail "备份是空的"; }
  # 取回之后再送回去校验：证明电脑上这份能完整解开、每个文件都能解析
  push_file "$out" /tmp/u60-cb-check.tgz || fail "送回校验失败"
  dev_run "sh /tmp/u60-cb.sh verify /tmp/u60-cb-check.tgz; r=\$?; rm -f /tmp/u60-cb-check.tgz /tmp/u60-cb.sh; exit \$r" | tr -d '\r' \
    || fail "备份校验没过：$out"
  ok "配置已备份到 $out"
  [ -n "$WITH_TS" ] && warn "这份备份含 Tailscale 身份：恢复到另一台设备前，先让这台下线，否则两台会抢同一个节点。"
  info "含后台密码、短信号码、订阅地址等：只放在这台电脑上，别发给别人。"
}

do_restore() {
  local ans
  push_file "$KIT/payload/guard/config-backup.sh" /tmp/u60-cb.sh || fail "推送脚本失败"
  push_file "$RESTORE_FILE" /tmp/u60-cb-restore.tgz || fail "推送备份失败"
  dev_run "sh /tmp/u60-cb.sh verify /tmp/u60-cb-restore.tgz" | tr -d '\r' || fail "这份备份校验没过，不恢复"
  echo; info "恢复会做这些（same = 一样不动；overwrite 会先把原文件留成 .pre-restore）："
  dev_run "sh /tmp/u60-cb.sh plan /tmp/u60-cb-restore.tgz /" | tr -d '\r'
  if $TTY; then
    printf "${CYAN}确认恢复请输入 yes:${NC} "; read -r ans
  else
    ans=$([ "${RESTORE_YES:-}" = 1 ] && echo yes)
  fi
  if [ "$ans" != yes ]; then
    dev_run "rm -f /tmp/u60-cb.sh /tmp/u60-cb-restore.tgz" >/dev/null
    info "没有恢复。"; return 0
  fi
  dev_run "sh /tmp/u60-cb.sh restore /tmp/u60-cb-restore.tgz /; rm -f /tmp/u60-cb.sh /tmp/u60-cb-restore.tgz" | tr -d '\r'
  ok "已写回。Wi-Fi 设置不会自动生效；各服务要重启才读新配置——最简单是 ./install.sh reboot。"
}

reboot_verify() {
  local deadline
  dev_run "reboot" >/dev/null 2>&1 || true
  info "已发出重启，等它回来（最多 4 分钟）…"
  sleep 30
  deadline=$((SECONDS + 210))   # 按实际经过的时间算，每次 ssh_ok 自己还要最多 6 秒
  until ssh_ok; do
    [ $SECONDS -lt $deadline ] || fail "4 分钟没连回来。等屏幕亮起、电脑重新连上 U60 的 Wi-Fi 后跑 ./install.sh status 看看"
    sleep 5
  done
  ok "重启后 SSH 自动恢复。"
  sleep 15   # devui/后台在 rc.local 里是最后起的
  CH=ssh
  show_status
  echo
  info "体检："
  show_doctor || true
}

case "$COMPONENTS" in
  status) show_status; exit 0 ;;
  doctor) show_doctor; exit $? ;;
  backup) do_backup; exit 0 ;;
  restore) do_restore; exit 0 ;;
  reboot) reboot_verify; exit 0 ;;
esac

# 已装过 SSH、这次只装别的组件时，确保通道还能用
case " $COMPONENTS " in
  *" ssh "*) ;;
  *) [ "$CH" = ssh ] || warn "这次没选 ssh，装完后重启 ADB 就没了；需要 SSH 的话加上 ssh 组件。" ;;
esac

# ── 3. 高级后台密码 ─────────────────────────────────────────────────────────
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/u60-kit/payload"

case " $COMPONENTS " in *" admin "*)
  if [ -n "${AGENT_PASSWORD+set}" ]; then
    AGENT_PW="$AGENT_PASSWORD"
  elif $TTY; then
    echo
    printf "${CYAN}高级后台（http://%s:9090）登录密码${NC}" "$GATEWAY"
    [ -n "$ROUTER_PW" ] && printf "${CYAN}（直接回车 = 和路由器管理密码一样）${NC}"
    printf "${CYAN}:${NC} "
    read -rs AGENT_PW; echo
    AGENT_PW="${AGENT_PW:-$ROUTER_PW}"
  else
    AGENT_PW="$ROUTER_PW"
  fi
  if [ -z "$AGENT_PW" ]; then
    # 设备上没装过后台时，现在就停，别等推送完在设备端才失败（那样 devui/esim 也跟着没装）
    dev_run '{ [ -s /data/zte-agent.env ] || [ -f /data/local/tmp/start_zte_agent.sh ]; } && echo AGENT_INSTALLED' 2>/dev/null | grep -q AGENT_INSTALLED \
      || fail "需要高级后台密码：设备上还没装过后台。设置 AGENT_PASSWORD（或 ROUTER_PASSWORD，两者相同时）再跑"
    warn "没有后台密码：沿用设备上已有的。"
  else
    case "$AGENT_PW" in
      *[\'\"\\\ ]*) fail "后台密码不能含引号、反斜杠或空格（设备上的 /data/zte-agent.env 和屏幕 eSIM 页都按原样读它）" ;;
    esac
    # 设备端 agent-auth.sh migrate 把它写成 /data/zte-agent.env（600）
    printf "export ZTE_AGENT_PASSWORD='%s'\n" "$AGENT_PW" > "$TMP/u60-kit/payload/agent.env"
  fi
;; esac

# ── 4. 组装要推的文件：只带选中的组件 ─────────────────────────────────────────
info "准备要推送的文件…"
cp -R "$KIT/device" "$TMP/u60-kit/"
S="$TMP/u60-kit/payload"
for c in $COMPONENTS; do
  case "$c" in
    ssh)   cp "$KIT/payload/dropbear" "$S/"; cp "$SSH_KEY.pub" "$S/authorized_keys" ;;
    admin) cp "$KIT/payload/zte-agent" "$KIT/payload/admin.tgz" "$S/"; cp -R "$KIT/payload/guard" "$S/" ;;
    devui) cp -R "$KIT/payload/devui" "$S/"; cp -R "$KIT/payload/guard" "$S/" ;;
    esim)  cp "$KIT/payload/esim.tgz" "$S/" ;;
  esac
done

info "推送到设备（${CH}）…"
if [ "$CH" = ssh ]; then
  tar czf - -C "$TMP" u60-kit | ssh "${SSH_OPTS[@]}" "root@$GATEWAY" \
    "rm -rf $STAGE && mkdir -p /data/local/tmp && tar xzf - -C /data/local/tmp"
else
  adb shell "rm -rf $STAGE && mkdir -p /data/local/tmp" >/dev/null
  adb push "$(localpath "$TMP/u60-kit")" /data/local/tmp/ >/dev/null || fail "adb push 失败"
fi
ok "已推送。"

# ── 5. 设备上安装 ───────────────────────────────────────────────────────────
echo
printf "${BOLD}── 设备端安装：%s ──${NC}\n" "$COMPONENTS"
# adb shell 不可靠地传退出码，所以用最后一行的哨兵判断成功；装完删掉临时包（含密码）
LOG="$TMP/device.log"
dev_run "sh $STAGE/device/install.sh $COMPONENTS; rc=\$?; rm -rf $STAGE; [ \$rc = 0 ] && echo __U60_KIT_OK__" \
  2>&1 | tr -d '\r' | tee "$LOG" | grep -v '^__U60_KIT_OK__$' || true
grep -q '^__U60_KIT_OK__$' "$LOG" || fail "设备端安装没有完成，看上面的报错"
echo
ok "设备端安装完成。"

# ── 6. 从电脑验证 SSH ────────────────────────────────────────────────────────
verify_ssh() {
  local i
  for i in 1 2 3 4 5; do ssh_ok && return 0; sleep 3; done
  return 1
}
case " $COMPONENTS " in *" ssh "*)
  if verify_ssh; then
    ok "SSH 公钥登录验证通过。"
  else
    warn "设备上 SSH 已经起来了，但这台电脑连不上 $GATEWAY:${SSH_PORT}。"
    warn "确认电脑连着 U60 的 Wi-Fi、代理没有劫持 ${GATEWAY}，再试: ssh -p $SSH_PORT -i $SSH_KEY root@$GATEWAY"
  fi
;; esac

# ── 7. 重启一次，验证开机自启 ──────────────────────────────────────────────────
if [ "${NO_REBOOT:-0}" != 1 ] && [ "${REBOOT:-}" != 0 ] && [[ " $COMPONENTS " == *" ssh "* ]] && ssh_ok; then
  if [ "${REBOOT:-}" = 1 ]; then
    reboot_verify
  elif $TTY; then
    echo
    printf "${CYAN}现在重启 U60 验证开机自启吗？（约 2 分钟，会断网）[Y/n]:${NC} "
    read -r a
    case "$a" in [Nn]*) ;; *) reboot_verify ;; esac
  else
    info "没有重启验证。要确认开机自启，跑: ./install.sh reboot（会断网约 2 分钟）"
  fi
fi

# ── 8. 总结 ────────────────────────────────────────────────────────────────
CHOSEN=" $COMPONENTS "
echo
printf "${GREEN}${BOLD}装好了。${NC}\n"
if [[ "$CHOSEN" == *" ssh "* ]]; then cat <<EOF

  SSH 登录:   ssh -p $SSH_PORT -i $SSH_KEY -o UserKnownHostsFile=$KNOWN root@$GATEWAY
  想简写成 "ssh u60"，把下面几行加进 ~/.ssh/config：

    Host u60
        HostName $GATEWAY
        Port $SSH_PORT
        User root
        IdentityFile $SSH_KEY
        IdentitiesOnly yes
        UserKnownHostsFile $KNOWN
EOF
fi
[[ "$CHOSEN" == *" admin "* ]] && printf "\n  高级后台:   http://%s:9090/\n" "$GATEWAY"
[[ "$CHOSEN" == *" esim "* ]] && printf "  eSIM:       高级后台 → 移动网络 → eSIM；屏幕上「更多功能 → eSIM」可切换\n"
echo
