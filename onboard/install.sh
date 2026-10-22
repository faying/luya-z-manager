#!/usr/bin/env bash
# ═════════════════════════════════════════════════════════════════════════════
# Luya-Z 装机包：电脑端入口。macOS、Linux、Windows（Git Bash）都能跑。
#
# 用法（包目录里）：
#   ./install.sh                         装全套：ssh admin ui esim（没通道时先经网页后台开 ADB）
#   ./install.sh <组件>…                 只装点名的组件：ssh admin ui esim chill recover
#        chill    CHILL 代理（mihomo + 面板），全套不含它。第一次装完不启动：先写订阅地址，再 safe-start
#        recover  u60 ship 的开机收尾：把 /data/luyaz/ship/luyaz-recover.sh 换成包里的（设备上自检通过才换），
#                 并在 rc.local 的服务启动行前面加一行调用它。全套不含它
#   ./install.sh status                  各组件现状
#   ./install.sh doctor                  只读体检（开机同步、自动升级、服务、心跳、Wi-Fi、告警…）
#   ./install.sh reboot                  重启设备，等它回来再看各组件是不是自己起来了（要 SSH 已通）
#   ./install.sh backup [--with-tailscale]   配置备份到这台电脑（目录 BACKUP_DIR，默认 ./backups）
#   ./install.sh restore <备份.tgz>          先列出会动哪些文件，输入 yes 才写（没终端时设 RESTORE_YES=1）
#
# 输入：有终端就当场问；没有终端（比如 Claude Code 代跑）就只看环境变量：
#   ROUTER_PASSWORD   路由器管理密码，只有要经网页后台开 ADB 时才用
#   AGENT_PASSWORD    高级后台密码；没设 = 跟路由器密码相同，设成空串 = 保留设备上现有的
#   REBOOT=1          装完自动重启验证；REBOOT=0 或 NO_REBOOT=1 不重启（都没设时有终端才问）
# 这几项和 GATEWAY、SSH_KEY 也能写进包目录的 u60.env（样例 u60.env.example）；环境变量里给了的优先。
# GATEWAY 默认 192.168.0.1，SSH_KEY 默认 ~/.ssh/id_ed25519。
#
# 经网页后台开 ADB 靠固件里的 zwrt_bsp.usb set：CN B27 及更早有；国行 B31 实测没有（据说 B28 就拿掉了）。
# ═════════════════════════════════════════════════════════════════════════════
set -euo pipefail

# Git Bash 默认会把 /data/... 这样的参数当成 Windows 路径改写，传给 adb 就错了。整体关掉，
# 真要给 adb.exe 本地路径时用 win_path 转（没有 cygpath 的系统原样返回）。
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'
win_path() {
  if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s' "$1"; fi
}

# ── 输出 ─────────────────────────────────────────────────────────────────────
C_RED= C_GRN= C_YLW= C_CYN= C_B= C_0=
if [ -t 1 ]; then
  C_RED=$'\033[0;31m' C_GRN=$'\033[0;32m' C_YLW=$'\033[1;33m' C_CYN=$'\033[0;36m' C_B=$'\033[1m' C_0=$'\033[0m'
fi
info() { printf '%s[*]%s %s\n' "$C_CYN" "$C_0" "$1"; }
ok()   { printf '%s[+]%s %s\n' "$C_GRN" "$C_0" "$1"; }
warn() { printf '%s[!]%s %s\n' "$C_YLW" "$C_0" "$1"; }
fail() { printf '%s[-]%s %s\n' "$C_RED" "$C_0" "$1" >&2; exit 1; }
ask()  { printf '%s%s%s' "$C_CYN" "$1" "$C_0"; }   # 提示语，不换行

# ── 设置：u60.env 只补没给的项 ───────────────────────────────────────────────
KIT="$(cd "$(dirname "$0")" && pwd)"
ENV_KEYS="ROUTER_PASSWORD AGENT_PASSWORD GATEWAY REBOOT SSH_KEY"
if [ -f "$KIT/u60.env" ]; then
  # 先记下调用方已经给了的（空串也算给了），读完文件再放回去
  keep=""
  for k in $ENV_KEYS; do
    if [ "${!k+set}" = set ]; then keep+=$(printf '%s=%q' "$k" "${!k}")$'\n'; fi
  done
  set -a
  # shellcheck disable=SC1091
  . "$KIT/u60.env"
  set +a
  eval "$keep"
  unset keep k
fi

GATEWAY="${GATEWAY:-192.168.0.1}"
SSH_PORT=2222
SSH_KEY="${SSH_KEY:-$HOME/.ssh/id_ed25519}"
STAGE=/data/local/tmp/u60-kit          # 设备上解包的位置（装完就删，里面有密码）
FULL_SET="ssh admin ui esim"
HAVE_TTY=false
[ -t 0 ] && HAVE_TTY=true
TTY=$HAVE_TTY
ROUTER_PW="${ROUTER_PASSWORD:-}"

# ── 命令行 ───────────────────────────────────────────────────────────────────
WITH_TS=""          # backup 的 --with-tailscale
RESTORE_FILE=""
if [ "${1:-}" = backup ]; then
  [ "${2:-}" != --with-tailscale ] || WITH_TS=--with-tailscale
  set -- backup
elif [ "${1:-}" = restore ]; then
  RESTORE_FILE="${2:-}"
  if [ ! -f "$RESTORE_FILE" ]; then echo "用法: ./install.sh restore <备份.tgz>" >&2; exit 1; fi
  set -- restore
fi
COMPONENTS="${*:-$FULL_SET}"

MODE=install        # install | status | doctor | reboot | backup | restore
case "$COMPONENTS" in
  status|doctor|reboot|backup|restore) MODE=$COMPONENTS ;;
  *)
    for c in $COMPONENTS; do
      case "$c" in
        status|doctor|reboot) fail "status / doctor / reboot 要单独跑，不能和组件混在一起" ;;
        ssh|admin|ui|esim|chill|recover) ;;
        *) fail "不认识的组件: ${c}（可选 ssh admin ui esim chill recover，或 status / doctor / reboot）" ;;
      esac
    done ;;
esac
INSTALLING=false
[ "$MODE" != install ] || INSTALLING=true
picked() { case " $COMPONENTS " in *" $1 "*) return 0 ;; esac; return 1; }

# ── 电脑上的工具 ─────────────────────────────────────────────────────────────
require() { # <命令> <去哪弄>
  command -v "$1" >/dev/null 2>&1 || fail "缺少 $1 —— $2"
}
require ssh "Windows 用 Git Bash 自带的；Linux: apt install openssh-client"
require ssh-keygen "同 ssh"
require curl "系统一般自带"
require tar "系统一般自带"
require_adb() {
  require adb "macOS: brew install android-platform-tools；Windows: 下载 Google platform-tools 并加进 PATH；Linux: apt install adb"
}

sha256() { # 标准输入 → 十六进制摘要
  local line
  if command -v sha256sum >/dev/null 2>&1; then line=$(sha256sum); else line=$(shasum -a 256); fi
  printf '%s\n' "${line%% *}"
}

# ── 包完整性：MANIFEST.txt 里每一行「摘要  路径」都要对得上 ──────────────────
check_kit() {
  info "校验装机包文件…"
  local want path got
  while read -r want path; do
    [ -f "$KIT/$path" ] || fail "包里缺文件: ${path}（包不完整，重新解压一次）"
    got=$(sha256 < "$KIT/$path")
    [ "$got" = "$want" ] || fail "文件损坏: ${path}（重新下载装机包）"
  done < <(grep -E '^[0-9a-f]{64}  ' "$KIT/MANIFEST.txt")
  ok "装机包完整。"
}

# ── SSH ──────────────────────────────────────────────────────────────────────
# 192.168.0.1 这个地址很多路由器都用，host key 单独存一个文件，不和 ~/.ssh/known_hosts 打架
KNOWN="$HOME/.ssh/known_hosts_u60"
SSH_OPTS=(-p "$SSH_PORT" -i "$SSH_KEY"
          -o IdentitiesOnly=yes -o ConnectTimeout=6 -o LogLevel=ERROR
          -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$KNOWN"
          -o ServerAliveInterval=5 -o ServerAliveCountMax=3)
ssh_ok() {
  [ -f "$SSH_KEY" ] || return 1
  ssh "${SSH_OPTS[@]}" -o BatchMode=yes "root@$GATEWAY" true </dev/null >/dev/null 2>&1
}
ensure_key() { # 装机时才需要公钥；没有就新建一把无密码短语的
  if [ ! -f "$SSH_KEY" ]; then
    info "生成 SSH 密钥 $SSH_KEY …"
    mkdir -p "$(dirname "$SSH_KEY")"
    ssh-keygen -q -t ed25519 -N "" -C "u60-$(whoami)" -f "$SSH_KEY" >/dev/null
  fi
  [ -f "$SSH_KEY.pub" ] || fail "找不到公钥 $SSH_KEY.pub"
}

# ── ADB ──────────────────────────────────────────────────────────────────────
adb_ready() { # 至少一台处于 device 状态
  local n
  n=$(adb devices 2>/dev/null | grep -c 'device$' || true)
  [ "${n:-0}" -ge 1 ]
}
adb_listed() { # 列表里的所有设备，未授权、离线的也算（它们一样会让 adb shell 不知道选哪台）
  adb devices 2>/dev/null | awk 'NR > 1 && NF >= 2 { n++ } END { print n + 0 }'
}
adb_is_u60() { # root，而且有中兴的 ubus 对象：别把 root 脚本推到手机上
  adb shell '[ "$(id -u)" = 0 ] && ubus list zwrt_bsp.usb >/dev/null 2>&1 && echo U60_OK' 2>/dev/null | grep -q U60_OK
}

# ── 网页后台（ubus JSON-RPC）：只用来开 ADB ──────────────────────────────────
# 一律不走代理：用户电脑常开着代理，又不能让他关
rpc() { # <session> <object> <method> <参数 JSON>
  local body
  body=$(printf '[{"jsonrpc":"2.0","id":1,"method":"call","params":["%s","%s","%s",%s]}]' "$1" "$2" "$3" "$4")
  curl -s -m 10 --noproxy '*' -H 'Content-Type: application/json' \
       -H "Origin: http://$GATEWAY" -H "Referer: http://$GATEWAY/" \
       -d "$body" "http://$GATEWAY/ubus/?t=$(date +%s)"
}
json_get() { # <键>：从标准输入的 JSON 里取第一个同名标量（字符串或数字）
  sed -n "s/.*\"$1\":\"\\{0,1\\}\\([^\",}]*\\).*/\\1/p"
}
login_hash() { # <密码> <salt> → 网页后台要的 SHA256(SHA256(密码) 大写十六进制 + salt)，大写
  local h
  h=$(printf '%s' "$1" | sha256 | tr 'a-f' 'A-F')
  printf '%s%s' "$h" "$2" | sha256 | tr 'a-f' 'A-F'
}

adb_via_web() {
  local nobody=00000000000000000000000000000000 reply salt tries locked sid ver waited
  info "通过网页后台开启 ADB（电脑要连着 U60 的 Wi-Fi）…"
  reply=$(rpc "$nobody" zwrt_web web_login_info '{}') \
    || fail "连不上 http://$GATEWAY —— 电脑连的是这台 U60 的 Wi-Fi 吗？代理/VPN 开着 TUN/增强模式的话，把 192.168.0.0/16 设成直连；地址不是 $GATEWAY 的话用 GATEWAY=… ./install.sh"
  salt=$(json_get zte_web_sault <<<"$reply")
  tries=$(json_get login_fail_num <<<"$reply")
  locked=$(json_get login_fail_lock_lefttime <<<"$reply")
  [ -n "$salt" ] || fail "网页后台返回不认识的内容，这可能不是 U60 Pro：$reply"
  if [ "${locked:-0}" -gt 0 ] 2>/dev/null; then
    fail "网页后台密码输错太多次被锁了，${locked} 秒后再试"
  fi

  # 密码要在真正登录之前拿到；没有就停，不浪费一次登录机会
  if [ -z "$ROUTER_PW" ]; then
    $HAVE_TTY || fail "需要路由器管理密码：设置 ROUTER_PASSWORD 环境变量或写进 u60.env（剩余可试 ${tries:-?} 次）"
    ask "路由器管理密码（机身背面/网页登录用的那个，剩余可试 ${tries:-?} 次）: "
    read -rs ROUTER_PW; echo
    [ -n "$ROUTER_PW" ] || fail "密码不能为空"
  fi

  sid=$(rpc "$nobody" zwrt_web web_login "{\"password\":\"$(login_hash "$ROUTER_PW" "$salt")\"}" | json_get ubus_rpc_session)
  if [ -z "$sid" ]; then
    tries=$(rpc "$nobody" zwrt_web web_login_info '{}' | json_get login_fail_num)
    fail "登录失败，路由器管理密码不对（还能试 ${tries:-?} 次，用完会锁一段时间；别盲目重试）"
  fi
  ok "已登录网页后台。"

  ver=$(rpc "$sid" zwrt_web device_info '{}' | json_get wa_inner_version)
  [ -z "$ver" ] || info "固件版本: $ver"

  reply=$(rpc "$sid" zwrt_bsp.usb set '{"mode":"debug"}')
  case "$reply" in
    *'"result":[0'*) ok "已切到 USB 调试模式。" ;;
    *) fail "开 ADB 被拒绝：这版固件的 zwrt_bsp.usb 可能没有 set 方法（国行 B31 实测没有；B28 起据称就删了），本包不适用：$reply" ;;
  esac

  info "等 ADB 设备出现（用 USB-C 数据线把 U60 接到这台电脑；最多等 90 秒）…"
  waited=0
  until adb_ready; do
    [ "$waited" -lt 90 ] || fail "90 秒内没看到 ADB 设备。换根能传数据的线/换个 USB 口；Windows 可能要装 ADB 驱动（见 README）"
    sleep 3; waited=$((waited + 3))
  done
  ok "ADB 已连接。"
}

# ── 选通道：先 SSH，再 ADB；装机时两样都没有就经网页后台开 ADB ──────────────
pick_channel() {
  if ssh_ok; then
    CH=ssh; ok "SSH 已经能登录，走 SSH。"
    return
  fi
  [ "$MODE" != reboot ] || fail "SSH 不通，没法做重启验证（先跑 ./install.sh，或确认电脑连着 U60 的 Wi-Fi）"
  if ! $INSTALLING && ! command -v adb >/dev/null 2>&1; then
    fail "SSH 不通（电脑连着 U60 的 Wi-Fi 吗？），这台电脑也没装 adb"
  fi
  require_adb
  if adb_ready; then
    ok "检测到 ADB 设备，走 ADB。"
  elif $INSTALLING; then
    adb_via_web
  else
    fail "SSH 和 ADB 都不通，先跑一次 ./install.sh"
  fi
  CH=adb
  if [ "$(adb_listed)" -gt 1 ] && [ -z "${ANDROID_SERIAL:-}" ]; then
    fail "电脑上接了不止一个 ADB 设备（含未授权/离线的），拔掉别的（或设置 ANDROID_SERIAL）再试"
  fi
  adb_is_u60 || fail "ADB 连着的设备不是 U60（或者没有 root）。拔掉手机等其他安卓设备再试"
}

# ── 和设备打交道（两种通道一个样）────────────────────────────────────────────
dev_run() { # <命令>：在设备上跑，输出原样
  case "$CH" in
    ssh) ssh "${SSH_OPTS[@]}" "root@$GATEWAY" "$1" </dev/null ;;
    *)   adb shell "$1" ;;
  esac
}

push_file() { # <本机文件> <设备路径>
  case "$CH" in
    ssh) ssh "${SSH_OPTS[@]}" "root@$GATEWAY" "cat > '$2'" < "$1" ;;
    *)   adb push "$(win_path "$1")" "$2" >/dev/null ;;
  esac
}

show_status() { # 只把设备端脚本放到 /tmp 跑 status，不推整个包
  local tmp=/tmp/u60-kit-status.sh
  if [ "$CH" = ssh ]; then
    push_file "$KIT/device/install.sh" "$tmp" \
      || fail "读状态失败：SSH 推送状态脚本没成功（刚连过的话等 20 秒再试）"
  else
    push_file "$KIT/device/install.sh" "$tmp" || fail "读状态失败：adb push 没成功"
  fi
  dev_run "sh $tmp status; rm -f $tmp" | tr -d '\r' \
    || fail "读状态失败：设备上执行状态脚本没成功"
}

show_doctor() { # 体检脚本是独立的：推包里那份到 /tmp 直接跑，没装过 guard 的设备也能查
  # doctor.sh 是薄壳：转给设备上 /data/luyaz/luyaz-core 的 doctor（没装核心就报错退出）
  local dir="$KIT/payload/scripts" d=/tmp/u60-kit-doctor f files=doctor.sh
  [ -f "$dir/doctor.sh" ] || fail "包里没有 payload/scripts/doctor.sh"
  if [ "$CH" = ssh ]; then
    # shellcheck disable=SC2086
    COPYFILE_DISABLE=1 tar -C "$dir" -cf - $files \
      | ssh "${SSH_OPTS[@]}" "root@$GATEWAY" "rm -rf $d && mkdir -p $d && tar -xf - -C $d" || fail "体检失败：推送脚本没成功"
  else
    dev_run "rm -rf $d && mkdir -p $d" >/dev/null || fail "体检失败：建不了 $d"
    for f in $files; do
      adb push "$(win_path "$dir/$f")" "$d/$f" >/dev/null || fail "体检失败：adb push 没成功"
    done
  fi
  dev_run "sh $d/doctor.sh; r=\$?; rm -rf $d; exit \$r" | tr -d '\r'
}

# 配置备份只存在这台电脑上。设备端脚本是包里的 payload/scripts/config-backup.sh。
do_backup() {
  local dir="${BACKUP_DIR:-$KIT/backups}" out
  mkdir -p "$dir" && chmod 700 "$dir"
  out="$dir/config-$(date +%Y%m%d-%H%M%S).tgz"
  push_file "$KIT/payload/scripts/config-backup.sh" /tmp/u60-cb.sh || fail "推送备份脚本失败"
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
  # 系统文件另存一份（只给人看、升级前留底，restore 不会恢复它们）：rc.local、我们的 init 脚本、
  # dropbear 的 host key 和启动脚本。固件升级会换掉 rc.local 和 /etc/init.d（B31 社区调研 K4）。
  local sys="${out%.tgz}.system.tgz"
  local tarcmd="cd / && tar czf - \$(for f in etc/rc.local etc/init.d/luyaz-core etc/init.d/luyaz-sentinel etc/init.d/luyaz-web \
      etc/init.d/chill data/ssh/dropbear_ed25519_host_key data/ssh/authorized_keys \
      data/local/tmp/start_dropbear.sh; do [ -e \$f ] && echo \$f; done)"
  if [ "$CH" = ssh ]; then
    ssh "${SSH_OPTS[@]}" "root@$GATEWAY" "$tarcmd" < /dev/null > "$sys" 2>/dev/null
  else
    adb exec-out "$tarcmd" > "$sys" 2>/dev/null
  fi
  chmod 600 "$sys"
  if tar tzf "$sys" 2>/dev/null | grep -qx 'etc/rc.local'; then
    ok "系统文件（rc.local、init 脚本、SSH host key）另存到 $sys（只留底，restore 不恢复）"
  else
    rm -f "$sys"; warn "rc.local、init 脚本、SSH host key 没存下来"
  fi
  [ -n "$WITH_TS" ] && warn "这份备份含 Tailscale 身份：恢复到另一台设备前，先让这台下线，否则两台会抢同一个节点。"
  info "含后台密码、短信号码、订阅地址等：只放在这台电脑上，别发给别人。"
}

do_restore() {
  local ans
  push_file "$KIT/payload/scripts/config-backup.sh" /tmp/u60-cb.sh || fail "推送脚本失败"
  push_file "$RESTORE_FILE" /tmp/u60-cb-restore.tgz || fail "推送备份失败"
  dev_run "sh /tmp/u60-cb.sh verify /tmp/u60-cb-restore.tgz" | tr -d '\r' || fail "这份备份校验没过，不恢复"
  echo; info "恢复会做这些（same = 一样不动；overwrite 会先把原文件留成 .pre-restore）："
  dev_run "sh /tmp/u60-cb.sh plan /tmp/u60-cb-restore.tgz /" | tr -d '\r'
  if $TTY; then
    printf "${C_CYN}确认恢复请输入 yes:${C_0} "; read -r ans
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

# 重启并等 SSH 回来。等待按经过的秒数算（每次 ssh_ok 自己最多还要 6 秒），总共约 4 分钟。
reboot_verify() {
  local give_up
  dev_run "reboot" >/dev/null 2>&1 || true
  info "已发出重启，等它回来（最多 4 分钟）…"
  sleep 30
  give_up=$((SECONDS + 210))
  while ! ssh_ok; do
    [ "$SECONDS" -lt "$give_up" ] || fail "4 分钟没连回来。等屏幕亮起、电脑重新连上 U60 的 Wi-Fi 后跑 ./install.sh status 看看"
    sleep 5
  done
  ok "重启后 SSH 自动恢复。"
  sleep 15            # rc.local 里界面和后台排在最后，给它们一点时间
  CH=ssh
  show_status
  echo
  info "体检："
  show_doctor || true
}

# ── 装机：密码 → 组装 → 推送 → 设备端安装 ───────────────────────────────────
# 高级后台密码。拿不到、设备上又没装过后台：推送之前就停（不然到设备端才失败，ui/esim 也跟着没装）。
admin_password() { # <暂存目录>
  local pw
  if [ "${AGENT_PASSWORD+set}" = set ]; then
    pw=$AGENT_PASSWORD
  elif $HAVE_TTY; then
    echo
    ask "高级后台（http://$GATEWAY:9090）登录密码"
    [ -z "$ROUTER_PW" ] || ask "（直接回车 = 和路由器管理密码一样）"
    ask ": "
    read -rs pw; echo
    pw=${pw:-$ROUTER_PW}
  else
    pw=$ROUTER_PW
  fi
  if [ -z "$pw" ]; then
    dev_run '[ -s /data/luyaz/web.env ] && echo AGENT_INSTALLED' 2>/dev/null | grep -q AGENT_INSTALLED \
      || fail "需要高级后台密码：设备上还没装过后台。设置 AGENT_PASSWORD（或 ROUTER_PASSWORD，两者相同时）再跑"
    warn "没有后台密码：沿用设备上已有的。"
    return 0
  fi
  case "$pw" in
    *[\'\"\\\ ]*) fail "后台密码不能含引号、反斜杠或空格（设备上的 /data/luyaz/web.env 按原样存它，管理网页 luyaz-web 按原样读）" ;;
  esac
  # 设备端 web-auth.sh migrate 把它写成 /data/luyaz/web.env（600）
  printf "export LUYAZ_WEB_PASSWORD='%s'\n" "$pw" > "$1/web-password.env"
}

# 只放选中组件要的东西；scripts/ 几个组件共用，放一次
stage_payload() { # <暂存目录>
  local out=$1 from="$KIT/payload" c
  with_scripts() { [ -d "$out/scripts" ] || cp -R "$from/scripts" "$out/"; }
  for c in $COMPONENTS; do
    case "$c" in
      ssh)
        cp "$from/dropbear" "$out/"
        cp "$SSH_KEY.pub" "$out/authorized_keys" ;;
      admin)
        cp "$from/luyaz-web" "$out/"
        with_scripts
        [ ! -d "$from/operator-logos" ] || cp -R "$from/operator-logos" "$out/" ;;
      ui)
        [ -f "$from/luyaz-sentinel" ] || fail "这个装机包没带 luyaz-sentinel（装机包太旧？）"
        cp -R "$from/ui" "$out/"
        cp "$from/luyaz-sentinel" "$out/"
        with_scripts ;;
      esim)
        cp "$from/esim.tgz" "$out/" ;;
      recover)
        [ -f "$from/scripts/luyaz-recover.sh" ] || fail "这个装机包没带 luyaz-recover.sh（装机包太旧？）"
        with_scripts ;;
      chill)
        [ -d "$from/chill" ] || fail "这个装机包没带 CHILL（打包时 CHILL=0？）"
        cp -R "$from/chill" "$out/" ;;
    esac
  done
}

ship_to_device() { # <含 u60-kit/ 的目录>
  info "推送到设备（${CH}）…"
  if [ "$CH" = ssh ]; then
    tar czf - -C "$1" u60-kit | ssh "${SSH_OPTS[@]}" "root@$GATEWAY" \
      "rm -rf $STAGE && mkdir -p /data/local/tmp && tar xzf - -C /data/local/tmp"
  else
    adb shell "rm -rf $STAGE && mkdir -p /data/local/tmp" >/dev/null
    adb push "$(win_path "$1/u60-kit")" /data/local/tmp/ >/dev/null || fail "adb push 失败"
  fi
  ok "已推送。"
}

# adb shell 的退出码靠不住：设备端成功时最后打印一行 __U60_KIT_OK__，看有没有这一行。
# 不管成败都删掉设备上的暂存包（里面有密码）。KIT_MAC_TIME：设备清单用电脑的时间（刚开机的设备时钟可能不对）。
run_on_device() { # <日志文件>
  echo
  printf '%s── 设备端安装：%s ──%s\n' "$C_B" "$COMPONENTS" "$C_0"
  dev_run "KIT_MAC_TIME=$(date +%s) sh $STAGE/device/install.sh $COMPONENTS; rc=\$?; rm -rf $STAGE; [ \$rc = 0 ] && echo __U60_KIT_OK__" \
    2>&1 | tr -d '\r' | tee "$1" | grep -v '^__U60_KIT_OK__$' || true
  grep -q '^__U60_KIT_OK__$' "$1" || fail "设备端安装没有完成，看上面的报错"
  echo
  ok "设备端安装完成。"
}

# ── 从电脑验证 SSH ────────────────────────────────────────────────────────
# 设备端已经确认 dropbear 进程在、端口在监听（device/install.sh do_ssh，不然走不到这里）。
# 失败时分 6 类说清楚：进程没起 / 没监听（再问一次设备，dropbear 可能装完后又退了）、
# 拒绝连接 / 认证失败 / 超时（看 ssh 的报错）、成功。分类函数 ssh_err_class 有离线测试 onboard/test/ssh-classify.sh。
# 失败只警告、不中止：设备那边已经装好，别的进入方式（adb）也还在。
SSH_WHY=
ssh_err_class() { # <ssh 的 stderr> → refused | auth | timeout | other:<最后一行>
  case "$1" in
    *"Connection refused"*) echo refused ;;
    *"Permission denied"*|*"Too many authentication failures"*) echo auth ;;
    *"timed out"*|*"No route to host"*|*"Network is unreachable"*) echo timeout ;;
    *) echo "other:$(printf '%s' "$1" | tail -n 1)" ;;
  esac
}
ssh_dev_class() { # 设备端探针的输出 → noproc | nolisten | 空（进程在、端口在听）
  case "$1" in
    *__NOPROC__*) echo noproc ;;
    *__NOLISTEN__*) echo nolisten ;;
  esac
}
verify_ssh() {
  local i err
  [ -f "$SSH_KEY" ] || { SSH_WHY=nokey; return 1; }
  for i in 1 2 3 4 5; do
    err=$(ssh "${SSH_OPTS[@]}" -o BatchMode=yes "root@$GATEWAY" true </dev/null 2>&1 >/dev/null) && { SSH_WHY=ok; return 0; }
    SSH_WHY=$(ssh_err_class "$err")
    [ "$SSH_WHY" = auth ] && break   # 认证失败重试也不会好
    sleep 3
  done
  # 通道是 adb 时再问设备一次：进程还在不在、端口还在不在听（ssh 通道说明它显然在）
  if [ "$CH" != ssh ]; then
    local d
    d=$(dev_run "pidof dropbear >/dev/null || echo __NOPROC__; netstat -ltn 2>/dev/null | grep -q ':${SSH_PORT} ' || echo __NOLISTEN__" 2>/dev/null | tr -d '\r')
    d=$(ssh_dev_class "$d")
    [ -n "$d" ] && SSH_WHY=$d
  fi
  return 1
}
report_ssh() {
  if verify_ssh; then
    ok "SSH 公钥登录验证通过。"
    return
  fi
  warn "这台电脑登不上 $GATEWAY:${SSH_PORT}："
  case "$SSH_WHY" in
    nokey)    warn "  这台电脑上没有私钥 $SSH_KEY。" ;;
    noproc)   warn "  设备上 dropbear 进程已经不在了（装机时起来过，之后退出了）：看设备 logread | grep dropbear。" ;;
    nolisten) warn "  设备上 dropbear 进程在，但没在 ${SSH_PORT} 端口监听：设备上 netstat -ltn 看看端口被谁占了。" ;;
    refused)  warn "  连接被拒绝：连到的不是 U60（GATEWAY 不对？），或代理把 $GATEWAY 接到了别处。" ;;
    auth)     warn "  连上了但公钥认证失败：$SSH_KEY 不是包里那把公钥对应的私钥。" ;;
    timeout)  warn "  连接超时/不可达：电脑和 U60 之间不通（没连 U60 的 Wi-Fi？代理/防火墙挡了？）。" ;;
    *)        warn "  ${SSH_WHY#other:}" ;;
  esac
  warn "手动再试: ssh -p $SSH_PORT -i $SSH_KEY root@$GATEWAY（adb 仍可用，rc.local 里的 dropbear 行只在设备上验证能起来后才写）"
}

# 装了 ssh 且现在能登录时，才谈得上重启验证
maybe_reboot() {
  [ "${NO_REBOOT:-0}" != 1 ] && [ "${REBOOT:-}" != 0 ] || return 0
  picked ssh && ssh_ok || return 0
  if [ "${REBOOT:-}" = 1 ]; then
    reboot_verify
  elif $HAVE_TTY; then
    local yn
    echo
    ask "现在重启 U60 验证开机自启吗？（约 2 分钟，会断网）[Y/n]: "
    read -r yn
    case "$yn" in [Nn]*) ;; *) reboot_verify ;; esac
  else
    info "没有重启验证。要确认开机自启，跑: ./install.sh reboot（会断网约 2 分钟）"
  fi
}

summary() {
  echo
  printf '%s%s装好了。%s\n' "$C_GRN" "$C_B" "$C_0"
  if picked ssh; then
    printf '\n  SSH 登录:   ssh -p %s -i %s -o UserKnownHostsFile=%s root@%s\n' "$SSH_PORT" "$SSH_KEY" "$KNOWN" "$GATEWAY"
    printf '  想简写成 "ssh u60"，把下面几行加进 ~/.ssh/config：\n\n'
    printf '    %s\n' "Host u60" \
      "    HostName $GATEWAY" "    Port $SSH_PORT" "    User root" \
      "    IdentityFile $SSH_KEY" "    IdentitiesOnly yes" "    UserKnownHostsFile $KNOWN"
  fi
  if picked admin; then printf '\n  高级后台:   http://%s:9090/\n' "$GATEWAY"; fi
  if picked chill; then printf '  CHILL:      高级后台 → 服务 → CHILL（面板在 http://%s:9090/chill-ui/，从后台的按钮进）\n' "$GATEWAY"; fi
  if picked esim; then printf '  eSIM:       高级后台 → 移动网络 → eSIM；屏幕上「更多功能 → eSIM」可切换\n'; fi
  echo
}

install_kit() {
  # 这次没装 ssh、又是经 adb 进来的：提醒一句，重启后 adb 就没了
  picked ssh || [ "$CH" = ssh ] || warn "这次没选 ssh，装完后重启 ADB 就没了；需要 SSH 的话加上 ssh 组件。"

  WORKDIR=$(mktemp -d)
  trap 'rm -rf "$WORKDIR"' EXIT
  mkdir -p "$WORKDIR/u60-kit/payload"
  if picked admin; then admin_password "$WORKDIR/u60-kit/payload"; fi

  info "准备要推送的文件…"
  cp -R "$KIT/device" "$WORKDIR/u60-kit/"
  stage_payload "$WORKDIR/u60-kit/payload"
  ship_to_device "$WORKDIR"
  run_on_device "$WORKDIR/device.log"

  if picked ssh; then report_ssh; fi
  maybe_reboot
  summary
}

# ── 入口 ─────────────────────────────────────────────────────────────────────
if $INSTALLING; then
  check_kit
  ensure_key
fi
pick_channel
case "$MODE" in
  status)  show_status ;;
  doctor)  show_doctor; exit $? ;;
  backup)  do_backup ;;
  restore) do_restore ;;
  reboot)  reboot_verify ;;
  install) install_kit ;;
esac
exit 0
