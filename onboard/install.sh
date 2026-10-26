#!/usr/bin/env bash
# ═════════════════════════════════════════════════════════════════════════════
# Luya-Z 装机包：电脑端入口。macOS、Linux、Windows（Git Bash）都能跑。

# 用法（包目录里）：
#   ./install.sh                         装全套：ssh admin ui esim
#   ./install.sh <组件>…                 只装点名的组件：ssh admin ui esim chill recover
#        chill    CHILL 代理（mihomo + 面板），全套不含它。第一次装完不启动：先写订阅地址，再 safe-start
#        recover  u60 ship 的开机收尾：把 /data/luyaz/ship/luyaz-recover.sh 换成包里的（设备上自检通过才换），
#                 并在 rc.local 的服务启动行前面加一行调用它。全套不含它
#   ./install.sh status                  各组件现状
#   ./install.sh doctor                  只读体检（开机同步、自动升级、服务、心跳、Wi-Fi、告警…）
#   ./install.sh reboot                  重启设备，等它回来再看各组件是不是自己起来了（要 SSH 已通）
#   ./install.sh backup [--with-tailscale]   配置备份到这台电脑（目录 BACKUP_DIR，默认 ./backups）
#   ./install.sh restore <备份.tgz>          先比对旁存的 <备份.tgz>.sha256，再列出会动哪些文件，输入 yes 才写（没终端时设 RESTORE_YES=1）
#                                            没有 .sha256 的旧备份要 LUYAZ_RESTORE_NO_SHA256=1；摘要不符一律不恢复

# 前提：这台电脑已经能用 SSH 登录设备（root@GATEWAY，端口 2222，密钥 SSH_KEY）。
# 本包只走这条已有的 SSH，不会自己去开任何进入方式；SSH 怎么来的不在本项目范围内。
# 输入：有终端就当场问；没有终端（比如 Claude Code 代跑）就只看环境变量：
#   AGENT_PASSWORD    高级后台密码；没设 = 有终端就问（直接回车 = 保留设备上现有的），没终端就保留现有的；
#                     设成空串 = 保留设备上现有的。设备上还没装过后台时必须给
#   REBOOT=1          装完自动重启验证；REBOOT=0 或 NO_REBOOT=1 不重启（都没设时有终端才问）
#   LUYAZ_ALLOW_UNVERIFIED=1  firmware-capabilities.matrix.json 里标 code-only 的组件也装（unsupported 的照样跳过）
#   KIT_FRESH=1       不续装：同一个包上次没装完也从头装（默认从第一个没完成的组件续）
#   LUYAZ_LANG=en|zh  提示语言；没设时 LC_ALL/LC_MESSAGES/LANG 以 zh 开头用中文，否则英文（设备端跟着用同一种）
# 这几项和 GATEWAY、SSH_KEY 也能写进包目录的 u60.env（样例 u60.env.example）；环境变量里给了的优先。
# GATEWAY 默认 192.168.0.1，SSH_KEY 默认 ~/.ssh/id_ed25519。
# ═════════════════════════════════════════════════════════════════════════════
set -euo pipefail   # 出错即停、未定义变量算错、管道任一段失败都算失败

# Git Bash 默认会把 /data/... 这样的参数当成 Windows 路径改写。整体关掉
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'   # 只对 Git Bash（MSYS）有效，其他系统无影响

# ── 输出 ─────────────────────────────────────────────────────────────────────
C_RED= C_GRN= C_YLW= C_CYN= C_B= C_0=
if [ -t 1 ]; then   # 输出是终端才上色
  C_RED=$'\033[0;31m' C_GRN=$'\033[0;32m' C_YLW=$'\033[1;33m' C_CYN=$'\033[0;36m' C_B=$'\033[1m' C_0=$'\033[0m'
fi

info() { printf '%s[*]%s %s\n' "$C_CYN" "$C_0" "$1"; }

ok()   { printf '%s[+]%s %s\n' "$C_GRN" "$C_0" "$1"; }

warn() { printf '%s[!]%s %s\n' "$C_YLW" "$C_0" "$1"; }

fail() { printf '%s[-]%s %s\n' "$C_RED" "$C_0" "$1" >&2; exit 1; }

ask()  { printf '%s%s%s' "$C_CYN" "$1" "$C_0"; }   # 提示语，不换行

# ── 设置：u60.env 只补没给的项 ───────────────────────────────────────────────
KIT="$(cd "$(dirname "$0")" && pwd)"   # 包目录（install.sh 所在处）
ENV_KEYS="AGENT_PASSWORD GATEWAY REBOOT SSH_KEY"
if [ -f "$KIT/u60.env" ]; then   # 有设置文件才读
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
fi  # ask

# 提示语言：LUYAZ_LANG 优先，否则看 locale；tx '中文' 'English' 按它选一句
_loc=${LC_ALL:-${LC_MESSAGES:-${LANG:-}}}
case "${LUYAZ_LANG:-}" in
  zh|en) KLANG=$LUYAZ_LANG ;;
  *) case "$_loc" in zh*) KLANG=zh ;; *) KLANG=en ;; esac ;;
esac
unset _loc
tx() { if [ "${KLANG:-}" = zh ]; then printf '%s' "$1"; else printf '%s' "$2"; fi; }

GATEWAY="${GATEWAY:-192.168.0.1}"   # 设备地址
SSH_PORT=2222   # dropbear 端口
SSH_KEY="${SSH_KEY:-$HOME/.ssh/id_ed25519}"   # 登录用的私钥
STAGE=/data/local/tmp/u60-kit          # 设备上解包的位置（装完就删，里面有密码）
FULL_SET="ssh admin ui esim"
HAVE_TTY=false
[ -t 0 ] && HAVE_TTY=true
TTY=$HAVE_TTY

# ── 命令行 ───────────────────────────────────────────────────────────────────
WITH_TS=""          # backup 的 --with-tailscale
RESTORE_FILE=""
if [ "${1:-}" = backup ]; then
  [ "${2:-}" != --with-tailscale ] || WITH_TS=--with-tailscale
  set -- backup
elif [ "${1:-}" = restore ]; then
  RESTORE_FILE="${2:-}"
  if [ ! -f "$RESTORE_FILE" ]; then echo "$(tx '用法: ./install.sh restore <备份.tgz>' 'Usage: ./install.sh restore <backup.tgz>')" >&2; exit 1; fi
  set -- restore
fi
COMPONENTS="${*:-$FULL_SET}"

MODE=install        # install | status | doctor | reboot | backup | restore
case "$COMPONENTS" in   # 先分出模式，再逐个校验组件名
  status|doctor|reboot|backup|restore) MODE=$COMPONENTS ;;
  *)
    for c in $COMPONENTS; do   # 每个名字都要认识
      case "$c" in   # 组件名
        status|doctor|reboot) fail "$(tx 'status / doctor / reboot 要单独跑，不能和组件混在一起' 'Run status / doctor / reboot on their own, not together with components')" ;;
        ssh|admin|ui|esim|chill|recover) ;;
        *) fail "$(tx "不认识的组件: ${c}（可选 ssh admin ui esim chill recover，或 status / doctor / reboot）" "Unknown component: ${c} (choose from ssh admin ui esim chill recover, or status / doctor / reboot)")" ;;
      esac
    done ;;  # ask
esac  # ask
INSTALLING=false
[ "$MODE" != install ] || INSTALLING=true

picked() { case " $COMPONENTS " in *" $1 "*) return 0 ;; esac; return 1; }

# ── 电脑上的工具 ─────────────────────────────────────────────────────────────
require() { # <命令> <去哪弄>
  command -v "$1" >/dev/null 2>&1 || fail "$(tx "缺少 $1 —— $2" "Missing $1 -- $2")"
}

require ssh "$(tx 'Windows 用 Git Bash 自带的；Linux: apt install openssh-client' 'Windows: use the one that comes with Git Bash; Linux: apt install openssh-client')"
require tar "$(tx '系统一般自带' 'usually comes with the system')"

sha256() { # 标准输入 → 十六进制摘要
  local line
  if command -v sha256sum >/dev/null 2>&1; then line=$(sha256sum); else line=$(shasum -a 256); fi
  printf '%s\n' "${line%% *}"
}  # sha256

# ── 包完整性：MANIFEST.txt 里每一行「摘要  路径」都要对得上 ──────────────────
check_kit() {
  info "$(tx '校验装机包文件…' 'Checking the kit files...')"   # MANIFEST.txt 逐行比 sha256
  local want path got
  while read -r want path; do
    [ -f "$KIT/$path" ] || fail "$(tx "包里缺文件: ${path}（包不完整，重新解压一次）" "Missing file in the kit: ${path} (the kit is incomplete; extract it again)")"
    got=$(sha256 < "$KIT/$path")
    [ "$got" = "$want" ] || fail "$(tx "文件损坏: ${path}（重新下载装机包）" "Damaged file: ${path} (download the kit again)")"
  done < <(grep -E '^[0-9a-f]{64}  ' "$KIT/MANIFEST.txt")
  ok "$(tx '装机包完整。' 'Kit is complete.')"   # 全部对上
}

# ── SSH ──────────────────────────────────────────────────────────────────────
# 192.168.0.1 这个地址很多路由器都用，host key 单独存一个文件，不和 ~/.ssh/known_hosts 打架
KNOWN="$HOME/.ssh/known_hosts_u60"   # U60 专用 known_hosts
SSH_OPTS=(-p "$SSH_PORT" -i "$SSH_KEY"
          -o IdentitiesOnly=yes -o ConnectTimeout=6 -o LogLevel=ERROR
          -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$KNOWN"   # 第一次见到就记下
          -o ServerAliveInterval=5 -o ServerAliveCountMax=3)

ssh_ok() {
  [ -f "$SSH_KEY" ] || return 1
  ssh "${SSH_OPTS[@]}" -o BatchMode=yes "root@$GATEWAY" true </dev/null >/dev/null 2>&1
}

ensure_key() { # ssh 组件要把这把公钥装进设备的 authorized_keys
  [ -f "$SSH_KEY.pub" ] || fail "$(tx "找不到公钥 $SSH_KEY.pub" "Public key not found: $SSH_KEY.pub")"
}

# ── 通道：只用用户自己已经配好的 SSH ────────────────────────────────────────
pick_channel() {
  [ -f "$SSH_KEY" ] || fail "$(tx "找不到私钥 $SSH_KEY。本包只经已经能用的 SSH 登录工作（root@$GATEWAY，端口 $SSH_PORT）；用 SSH_KEY=… 指定你的密钥。SSH 怎么开通不在本项目范围内" "Private key not found: $SSH_KEY. This kit only works over an SSH login that already works (root@$GATEWAY, port $SSH_PORT); point SSH_KEY=... at your key. How you get SSH is outside this project")"
  ssh_ok || fail "$(tx "SSH 登录不了 root@$GATEWAY:$SSH_PORT（密钥 $SSH_KEY）。本包需要 SSH 已经能用；电脑连着设备吗？代理/VPN 的 TUN 模式要让 $GATEWAY 直连。SSH 怎么开通不在本项目范围内" "Cannot log in over SSH to root@$GATEWAY:$SSH_PORT (key $SSH_KEY). This kit needs SSH to work already; is this computer connected to the device? A proxy/VPN in TUN mode must send $GATEWAY direct. How you get SSH is outside this project")"
  CH=ssh; ok "$(tx 'SSH 已经能登录，走 SSH。' 'SSH login works; using SSH.')"
}

# ── 和设备打交道────────────────────────────────────────────
dev_run() { # <命令>：在设备上跑，输出原样
  ssh "${SSH_OPTS[@]}" "root@$GATEWAY" "$1" </dev/null
}  # dev_run

push_file() { # <本机文件> <设备路径>
  ssh "${SSH_OPTS[@]}" "root@$GATEWAY" "cat > '$2'" < "$1"
}

# ── 装前自检：型号、固件 → firmware-capabilities.matrix.json，按组件各自放行 ───────────
# 矩阵每行一条（model/region/build，* = 任意），第一条命中的算数；每个组件 verified / code-only / unsupported。
# 没命中 = 全部 unsupported。code-only 要 LUYAZ_ALLOW_UNVERIFIED=1；unsupported 跳过并说明，不让整次安装失败。
# 一个都装不了才停。离线测试 onboard/test/compat.sh。
CAP_MATRIX="$KIT/firmware-capabilities.matrix.json"

cap_of() { # <model> <region> <build> <组件> → verified | code-only | unsupported
  local line m r b st
  while IFS= read -r line; do
    m=$(printf '%s' "$line" | sed -n 's/.*"model": *"\([^"]*\)".*/\1/p')
    r=$(printf '%s' "$line" | sed -n 's/.*"region": *"\([^"]*\)".*/\1/p')
    b=$(printf '%s' "$line" | sed -n 's/.*"build": *"\([^"]*\)".*/\1/p')
    { [ "$m" = "$1" ] || [ "$m" = '*' ]; } || continue
    { [ "$r" = "$2" ] || [ "$r" = '*' ]; } || continue
    { [ "$b" = "$3" ] || [ "$b" = '*' ]; } || continue
    st=$(printf '%s' "$line" | sed -n "s/.*\"$4\": *\"\([a-z-]*\)\".*/\1/p")
    case "$st" in verified|code-only) echo "$st" ;; *) echo unsupported ;; esac
    return 0
  done < <(grep '"model"' "$CAP_MATRIX")
  echo unsupported
}  # cap_of

check_device() {
  local raw arch iv model region build c st keep="" skipped=0 tried=0
  [ -f "$CAP_MATRIX" ] || fail "$(tx '包里没有 firmware-capabilities.matrix.json（装机包太旧或不完整）' 'The kit has no firmware-capabilities.matrix.json (kit too old or incomplete)')"
  raw=$(dev_run 'uname -m; ubus call zwrt_web device_info "{}" 2>/dev/null | sed -n "s/.*\"integrate_version\": *\"\([^\"]*\)\".*/\1/p"' | tr -d '\r') \
    || fail "$(tx '读不到设备信息（uname / ubus），没法判断这是不是 U60 Pro' 'Cannot read device info (uname / ubus), so cannot tell whether this is a U60 Pro')"
  arch=$(printf '%s\n' "$raw" | sed -n 1p)
  iv=$(printf '%s\n' "$raw" | sed -n 2p)   # 例：CN_ZTE_MU5250V1.0.0B31
  [ "$arch" = aarch64 ] || fail "$(tx "设备 CPU 是 ${arch:-未知}，不是 aarch64：这不是 U60 Pro，不装" "Device CPU is ${arch:-unknown}, not aarch64: this is not a U60 Pro. Not installing")"
  model=$(printf '%s' "$iv" | sed -n 's/.*_\(MU[0-9][0-9]*\)V.*/\1/p')
  region=${iv%%_*}
  build=$(printf '%s' "$iv" | sed -n 's/.*V[0-9.]*\(B[0-9][0-9]*\)$/\1/p')
  [ -n "$model" ] && [ -n "$build" ] || fail "$(tx "认不出固件版本「${iv:-空}」，不装（不是 U60 Pro，或固件太新/太旧）" "Unknown firmware version \"${iv:-empty}\". Not installing (not a U60 Pro, or firmware too new/old)")"
  info "$(tx "设备：$model，固件 $region $build" "Device: $model, firmware $region $build")"
  for c in $COMPONENTS; do
    tried=$((tried + 1))
    st=$(cap_of "$model" "$region" "$build" "$c")
    case "$st" in
      verified) keep="$keep $c"; ok "$(tx "  $c：实测支持" "  $c: tested")" ;;
      code-only)
        if [ "${LUYAZ_ALLOW_UNVERIFIED:-0}" = 1 ]; then
          keep="$keep $c"
          warn "$(tx "  $c：未经实测（代码里有支持），按 LUYAZ_ALLOW_UNVERIFIED=1 继续" "  $c: not tested (the code supports it); going on because LUYAZ_ALLOW_UNVERIFIED=1")"
        else
          skipped=$((skipped + 1))
          warn "$(tx "  $c：这个型号/固件没在真机上装过，跳过。确认要试就加 LUYAZ_ALLOW_UNVERIFIED=1 再跑；先 ./install.sh backup" "  $c: not tested on this model/firmware; skipped. To try anyway, run again with LUYAZ_ALLOW_UNVERIFIED=1; run ./install.sh backup first")"
        fi ;;
      *) skipped=$((skipped + 1))
         warn "$(tx "  $c：这个型号/固件不支持（firmware-capabilities.matrix.json），跳过。别为了装它去升级或降级固件" "  $c: not supported on this model/firmware (firmware-capabilities.matrix.json); skipped. Do not upgrade or downgrade the firmware just for this kit")" ;;
    esac
  done
  keep=${keep# }
  [ -n "$keep" ] || fail "$(tx "设备 $model / 固件 $region $build 上点名的组件一个都装不了，不装" "None of the picked components can be installed on $model / firmware $region $build. Not installing")"
  COMPONENTS=$keep
  if [ "$skipped" -gt 0 ]; then
    warn "$(tx "只装：$COMPONENTS（跳过 $skipped 个，共 $tried 个）" "Installing only: $COMPONENTS (skipped $skipped of $tried)")"
  elif [ "${LUYAZ_ALLOW_UNVERIFIED:-0}" = 1 ] && [ "$(for c in $COMPONENTS; do cap_of "$model" "$region" "$build" "$c"; done | sort -u)" != verified ]; then
    warn "$(tx "装：$COMPONENTS（含未经实测的）" "Installing: $COMPONENTS (some not tested)")"
  else
    ok "$(tx "装：$COMPONENTS（都实测支持）" "Installing: $COMPONENTS (all tested)")"
  fi
}  # check_device

show_status() { # 只把设备端脚本放到 /tmp 跑 status，不推整个包
  local tmp=/tmp/u60-kit-status.sh
  push_file "$KIT/device/install.sh" "$tmp" \
    || fail "$(tx '读状态失败：SSH 推送状态脚本没成功（刚连过的话等 20 秒再试）' 'Status failed: could not send the status script over SSH (if you just connected, wait 20 seconds and try again)')"   # dropbear 限速时常见
  dev_run "LUYAZ_LANG=$KLANG sh $tmp status; rm -f $tmp" | tr -d '\r' \
    || fail "$(tx '读状态失败：设备上执行状态脚本没成功' 'Status failed: the status script did not run on the device')"   # 运行这一步失败
}  # show_status

show_doctor() { # 体检脚本是独立的：推包里那份到 /tmp 直接跑，没装过 guard 的设备也能查
  # doctor.sh 是薄壳：转给设备上 /data/luyaz/luyaz-core 的 doctor（没装核心就报错退出）
  local dir="$KIT/payload/scripts" d=/tmp/u60-kit-doctor files=doctor.sh
  [ -f "$dir/doctor.sh" ] || fail "$(tx '包里没有 payload/scripts/doctor.sh' 'The kit has no payload/scripts/doctor.sh')"
  # shellcheck disable=SC2086
  COPYFILE_DISABLE=1 tar -C "$dir" -cf - $files \
    | ssh "${SSH_OPTS[@]}" "root@$GATEWAY" "rm -rf $d && mkdir -p $d && tar -xf - -C $d" || fail "$(tx '体检失败：推送脚本没成功' 'Doctor failed: could not send the script')"
  dev_run "sh $d/doctor.sh; r=\$?; rm -rf $d; exit \$r" | tr -d '\r'
}

# 配置备份只存在这台电脑上。设备端脚本是包里的 payload/scripts/config-backup.sh。
do_backup() {
  local dir="${BACKUP_DIR:-$KIT/backups}" out
  mkdir -p "$dir" && chmod 700 "$dir"
  out="$dir/config-$(date +%Y%m%d-%H%M%S).tgz"
  push_file "$KIT/payload/scripts/config-backup.sh" /tmp/u60-cb.sh || fail "$(tx '推送备份脚本失败' 'Could not send the backup script')"
  ssh "${SSH_OPTS[@]}" "root@$GATEWAY" "sh /tmp/u60-cb.sh export $WITH_TS" < /dev/null > "$out"
  chmod 600 "$out"
  [ -s "$out" ] || { rm -f "$out"; fail "$(tx '备份是空的' 'The backup is empty')"; }
  # 取回之后再送回去校验：证明电脑上这份能完整解开、每个文件都能解析
  push_file "$out" /tmp/u60-cb-check.tgz || fail "$(tx '送回校验失败' 'Could not send the backup back for checking')"
  dev_run "sh /tmp/u60-cb.sh verify /tmp/u60-cb-check.tgz; r=\$?; rm -f /tmp/u60-cb-check.tgz /tmp/u60-cb.sh; exit \$r" | tr -d '\r' \
    || fail "$(tx "备份校验没过：$out" "Backup check failed: $out")"
  # 旁存 sha256（sha256sum -c 认的格式）：restore 先比对它，不符就不恢复
  printf '%s  %s\n' "$(sha256 < "$out")" "$(basename "$out")" > "$out.sha256"
  chmod 600 "$out.sha256"
  ok "$(tx "配置已备份到 $out（摘要 $(basename "$out").sha256）" "Settings backed up to $out (checksum $(basename "$out").sha256)")"
  # 系统文件另存一份（只给人看、升级前留底，restore 不会恢复它们）：rc.local、我们的 init 脚本、
  # dropbear 的 host key 和启动脚本。固件升级会换掉 rc.local 和 /etc/init.d（B31 社区调研 K4）。
  local sys="${out%.tgz}.system.tgz"
  local tarcmd="cd / && tar czf - \$(for f in etc/rc.local etc/init.d/luyaz-core etc/init.d/luyaz-sentinel etc/init.d/luyaz-web \
      etc/init.d/chill data/ssh/dropbear_ed25519_host_key data/ssh/authorized_keys \
      data/local/tmp/start_dropbear.sh; do [ -e \$f ] && echo \$f; done)"
  ssh "${SSH_OPTS[@]}" "root@$GATEWAY" "$tarcmd" < /dev/null > "$sys" 2>/dev/null
  chmod 600 "$sys"
  if tar tzf "$sys" 2>/dev/null | grep -qx 'etc/rc.local'; then
    printf '%s  %s\n' "$(sha256 < "$sys")" "$(basename "$sys")" > "$sys.sha256"; chmod 600 "$sys.sha256"
    ok "$(tx "系统文件（rc.local、init 脚本、SSH host key）另存到 $sys（只留底，restore 不恢复）" "System files (rc.local, init scripts, SSH host key) saved separately to $sys (for reference only; restore does not bring them back)")"
  else
    rm -f "$sys"; warn "$(tx 'rc.local、init 脚本、SSH host key 没存下来' 'Could not save rc.local, init scripts, SSH host key')"
  fi
  [ -n "$WITH_TS" ] && warn "$(tx '这份备份含 Tailscale 身份：恢复到另一台设备前，先让这台下线，否则两台会抢同一个节点。' 'This backup holds the Tailscale identity: before restoring it to another device, take this one offline, or the two will fight over the same node.')"
  info "$(tx '含后台密码、短信号码、订阅地址等：只放在这台电脑上，别发给别人。' 'It contains the admin web password, SMS numbers, subscription URLs and more: keep it on this computer and do not share it.')"
}

# 恢复前比对旁存的 sha256（<备份>.sha256）：不符一律失败；缺旁存（更早的备份没有）只在
# LUYAZ_RESTORE_NO_SHA256=1 时放行。比对过的摘要放进 RESTORE_SHA，推到设备后再比一次。
restore_sum_check() { # <备份>
  local side="$1.sha256" want got
  got=$(sha256 < "$1")
  if [ ! -f "$side" ]; then
    [ "${LUYAZ_RESTORE_NO_SHA256:-0}" = 1 ] \
      || fail "$(tx "找不到 $(basename "$side")（备份旁存的摘要），不恢复。这是更早的、没有摘要的备份就加 LUYAZ_RESTORE_NO_SHA256=1 再跑" "$(basename "$side") (the backup's checksum file) is missing; not restoring. If this is an older backup made without one, run again with LUYAZ_RESTORE_NO_SHA256=1")"
    warn "$(tx "没有 $(basename "$side")，按 LUYAZ_RESTORE_NO_SHA256=1 继续（只靠备份里自带的校验）" "No $(basename "$side"); going on because LUYAZ_RESTORE_NO_SHA256=1 (only the backup's own checks apply)")"
    RESTORE_SHA=$got; return 0
  fi
  want=$(sed -n '1s/^\([0-9a-fA-F]\{64\}\).*/\1/p' "$side" | tr 'A-F' 'a-f')
  [ -n "$want" ] || fail "$(tx "$(basename "$side") 里没有 sha256 摘要，不恢复" "$(basename "$side") holds no sha256 checksum; not restoring")"
  [ "$got" = "$want" ] || fail "$(tx "备份和它的摘要对不上（文件损坏或被改过），不恢复：$1" "The backup does not match its checksum (damaged or changed); not restoring: $1")"
  ok "$(tx '备份摘要相符。' 'Backup checksum matches.')"
  RESTORE_SHA=$got
}  # restore_sum_check

do_restore() {
  local ans
  RESTORE_SHA=""
  restore_sum_check "$RESTORE_FILE"
  push_file "$KIT/payload/scripts/config-backup.sh" /tmp/u60-cb.sh || fail "$(tx '推送脚本失败' 'Could not send the script')"
  push_file "$RESTORE_FILE" /tmp/u60-cb-restore.tgz || fail "$(tx '推送备份失败' 'Could not send the backup')"
  dev_run "CB_EXPECT_SHA256=$RESTORE_SHA sh /tmp/u60-cb.sh verify /tmp/u60-cb-restore.tgz" | tr -d '\r' || fail "$(tx '这份备份校验没过，不恢复' 'This backup failed its check; not restoring')"
  echo; info "$(tx '恢复会做这些（same = 一样不动；overwrite 会先把原文件留成 .pre-restore）：' 'Restore will do this (same = left alone; overwrite keeps the old file as .pre-restore first):')"
  dev_run "CB_EXPECT_SHA256=$RESTORE_SHA sh /tmp/u60-cb.sh plan /tmp/u60-cb-restore.tgz /" | tr -d '\r'
  if $TTY; then
    printf '%s%s%s ' "$C_CYN" "$(tx '确认恢复请输入 yes:' 'Type yes to restore:')" "$C_0"; read -r ans
  else
    ans=$([ "${RESTORE_YES:-}" = 1 ] && echo yes)
  fi
  if [ "$ans" != yes ]; then
    dev_run "rm -f /tmp/u60-cb.sh /tmp/u60-cb-restore.tgz" >/dev/null
    info "$(tx '没有恢复。' 'Nothing restored.')"; return 0
  fi
  dev_run "CB_EXPECT_SHA256=$RESTORE_SHA sh /tmp/u60-cb.sh restore /tmp/u60-cb-restore.tgz /; r=\$?; rm -f /tmp/u60-cb.sh /tmp/u60-cb-restore.tgz; exit \$r" | tr -d '\r' \
    || fail "$(tx '设备上恢复没成功（推过去的备份和摘要对不上，或写文件出错），看上面的输出' 'Restore on the device failed (the copy sent over does not match the checksum, or a write failed); see the output above')"
  ok "$(tx '已写回。Wi-Fi 设置不会自动生效；各服务要重启才读新配置——最简单是 ./install.sh reboot。' 'Written back. Wi-Fi settings do not take effect by themselves, and services read the new settings only after a restart -- easiest is ./install.sh reboot.')"
}

# 重启并等 SSH 回来。等待按经过的秒数算（每次 ssh_ok 自己最多还要 6 秒），总共约 4 分钟。
reboot_verify() {   # 重启 → 等 SSH → 看状态和体检
  local give_up
  dev_run "reboot" >/dev/null 2>&1 || true   # 连接会被断开，退出码不算数
  info "$(tx '已发出重启，等它回来（最多 4 分钟）…' 'Reboot sent; waiting for it to come back (up to 4 minutes)...')"   # 先睡 30 秒再开始探
  sleep 30   # 关机本身要一会儿，太早探会误以为还活着
  give_up=$((SECONDS + 210))
  while ! ssh_ok; do
    [ "$SECONDS" -lt "$give_up" ] || fail "$(tx '4 分钟没连回来。等屏幕亮起、电脑重新连上 U60 的 Wi-Fi 后跑 ./install.sh status 看看' "Not back after 4 minutes. When the screen is on and this computer is back on the U60's Wi-Fi, run ./install.sh status")"
    sleep 5   # 探测间隔
  done  # reboot_verify
  ok "$(tx '重启后 SSH 自动恢复。' 'SSH came back by itself after the reboot.')"   # 开机自启的 dropbear 正常
  sleep 15            # rc.local 里界面和后台排在最后，给它们一点时间
  show_status   # 各组件现状
  echo
  info "$(tx '体检：' 'Doctor:')"
  show_doctor || true
}  # reboot_verify

# ── 装机：密码 → 组装 → 推送 → 设备端安装 ───────────────────────────────────
# 高级后台密码。拿不到、设备上又没装过后台：推送之前就停（不然到设备端才失败，ui/esim 也跟着没装）。
admin_password() { # <暂存目录>
  local pw
  if [ "${AGENT_PASSWORD+set}" = set ]; then
    pw=$AGENT_PASSWORD
  elif $HAVE_TTY; then
    echo ""   # 和上面的输出隔开
    ask "$(tx "高级后台（http://$GATEWAY:9090）登录密码（直接回车 = 保留设备上现有的）: " "Admin web (http://$GATEWAY:9090) login password (press Enter = keep the one on the device): ")"
    read -rs pw; echo
  else  # admin_password
    pw=""   # 没终端又没给：保留设备上现有的
  fi  # admin_password
  if [ -z "$pw" ]; then
    dev_run '[ -s /data/luyaz/web.env ] && echo AGENT_INSTALLED' 2>/dev/null | grep -q AGENT_INSTALLED \
      || fail "$(tx '需要高级后台密码：设备上还没装过后台。设置 AGENT_PASSWORD（或在终端里跑并输入）再跑' 'An admin web password is needed: the admin web is not on the device yet. Set AGENT_PASSWORD (or run in a terminal and type it) and run again')"   # 推送前就停
    warn "$(tx '没有后台密码：沿用设备上已有的。' 'No admin web password given: keeping the one already on the device.')"   # web.env 已经有了
    return 0
  fi
  case "$pw" in
    *[\'\"\\\ ]*) fail "$(tx '后台密码不能含引号、反斜杠或空格（设备上的 /data/luyaz/web.env 按原样存它，管理网页 luyaz-web 按原样读）' 'The admin web password cannot contain quotes, backslashes or spaces (it is stored as-is in /data/luyaz/web.env on the device and read as-is by luyaz-web)')" ;;
  esac  # admin_password
  # 设备端 web-auth.sh migrate 把它写成 /data/luyaz/web.env（600）
  printf "export LUYAZ_WEB_PASSWORD='%s'\n" "$pw" > "$1/web-password.env"
}

# 只放选中组件要的东西；scripts/ 几个组件共用，放一次
stage_payload() { # <暂存目录>
  local out=$1 from="$KIT/payload" c
  with_scripts() { [ -d "$out/scripts" ] || cp -R "$from/scripts" "$out/"; }
  for c in $COMPONENTS; do   # 按选中的组件逐个放
    case "$c" in   # 每个组件要的文件
      ssh)
        cp "$from/dropbear" "$out/"
        cp "$SSH_KEY.pub" "$out/authorized_keys" ;;
      admin)
        cp "$from/luyaz-web" "$out/"
        with_scripts
        [ ! -d "$from/operator-logos" ] || cp -R "$from/operator-logos" "$out/" ;;
      ui)
        [ -f "$from/luyaz-sentinel" ] || fail "$(tx '这个装机包没带 luyaz-sentinel（装机包太旧？）' 'This kit has no luyaz-sentinel (kit too old?)')"
        cp -R "$from/ui" "$out/"
        cp "$from/luyaz-sentinel" "$out/"
        with_scripts ;;
      esim)
        cp "$from/esim.tgz" "$out/" ;;
      recover)
        [ -f "$from/scripts/luyaz-recover.sh" ] || fail "$(tx '这个装机包没带 luyaz-recover.sh（装机包太旧？）' 'This kit has no luyaz-recover.sh (kit too old?)')"
        with_scripts ;;
      chill)
        [ -d "$from/chill" ] || fail "$(tx '这个装机包没带 CHILL（打包时 CHILL=0？）' 'This kit has no CHILL (built with CHILL=0?)')"
        cp -R "$from/chill" "$out/" ;;
    esac  # stage_payload
  done  # stage_payload
}

ship_to_device() { # <含 u60-kit/ 的目录>
  info "$(tx "推送到设备（${CH}）…" "Sending to the device (${CH})...")"   # 整个 u60-kit/ 目录
  tar czf - -C "$1" u60-kit | ssh "${SSH_OPTS[@]}" "root@$GATEWAY" \
    "rm -rf $STAGE && mkdir -p /data/local/tmp && tar xzf - -C /data/local/tmp" \
    || fail "$(tx '推送失败' 'Sending failed')"   # 设备上先清掉旧的暂存
  ok "$(tx '已推送。' 'Sent.')"   # 文件都在 $STAGE 了
}

# 远端退出码不全靠得住：设备端成功时最后打印一行 __U60_KIT_OK__，看有没有这一行。
# 不管成败都删掉设备上的暂存包（里面有密码）。KIT_MAC_TIME：设备清单用电脑的时间（刚开机的设备时钟可能不对）。
run_on_device() { # <日志文件>
  echo ""   # 空一行
  printf '%s── %s%s ──%s\n' "$C_B" "$(tx '设备端安装：' 'Installing on the device: ')" "$COMPONENTS" "$C_0"
  dev_run "LUYAZ_LANG=$KLANG KIT_MAC_TIME=$(date +%s) KIT_ID=$KIT_ID KIT_FRESH=$KIT_FRESH sh $STAGE/device/install.sh $COMPONENTS; rc=\$?; rm -rf $STAGE; [ \$rc = 0 ] && echo __U60_KIT_OK__" \
    2>&1 | tr -d '\r' | tee "$1" | grep -v '^__U60_KIT_OK__$' || true
  grep -q '^__U60_KIT_OK__$' "$1" || fail "$(tx '设备端安装没有完成，看上面的报错' 'The install on the device did not finish; see the errors above')"
  echo ""   # 空一行
  ok "$(tx '设备端安装完成。' 'Install on the device finished.')"   # 看到了成功标记
}

# ── 从电脑验证 SSH ────────────────────────────────────────────────────────
# 设备端已经确认 dropbear 进程在、端口在监听（device/install.sh do_ssh，不然走不到这里）。
# 失败时分 6 类说清楚：进程没起 / 没监听（再问一次设备，dropbear 可能装完后又退了）、
# 拒绝连接 / 认证失败 / 超时（看 ssh 的报错）、成功。分类函数 ssh_err_class 有离线测试 onboard/test/ssh-classify.sh。
# 失败只警告、不中止：设备那边已经装好，原来那条 SSH 登录也还在。
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

verify_ssh() {   # 成功返回 0；失败把原因写进 SSH_WHY
  local i err
  [ -f "$SSH_KEY" ] || { SSH_WHY=nokey; return 1; }
  for i in 1 2 3 4 5; do
    err=$(ssh "${SSH_OPTS[@]}" -o BatchMode=yes "root@$GATEWAY" true </dev/null 2>&1 >/dev/null) && { SSH_WHY=ok; return 0; }
    SSH_WHY=$(ssh_err_class "$err")
    [ "$SSH_WHY" = auth ] && break   # 认证失败重试也不会好
    sleep 3
  done
  return 1   # 走到这里就是没登上
}  # verify_ssh

report_ssh() {
  if verify_ssh; then   # 一切正常就只说一句
    ok "$(tx 'SSH 公钥登录验证通过。' 'SSH key login checked: it works.')"   # 用的是包里那把公钥
    return
  fi
  warn "$(tx "这台电脑登不上 $GATEWAY:${SSH_PORT}：" "This computer cannot log in to $GATEWAY:${SSH_PORT}:")"
  case "$SSH_WHY" in
    nokey)    warn "$(tx "  这台电脑上没有私钥 $SSH_KEY。" "  No private key $SSH_KEY on this computer.")" ;;
    noproc)   warn "$(tx '  设备上 dropbear 进程已经不在了（装机时起来过，之后退出了）：看设备 logread | grep dropbear。' '  dropbear is no longer running on the device (it started during the install, then quit): on the device, see logread | grep dropbear.')" ;;
    nolisten) warn "$(tx "  设备上 dropbear 进程在，但没在 ${SSH_PORT} 端口监听：设备上 netstat -ltn 看看端口被谁占了。" "  dropbear runs on the device but does not listen on port ${SSH_PORT}: on the device, run netstat -ltn to see what holds the port.")" ;;
    refused)  warn "$(tx "  连接被拒绝：连到的不是 U60（GATEWAY 不对？），或代理把 $GATEWAY 接到了别处。" "  Connection refused: that is not the U60 (wrong GATEWAY?), or a proxy sends $GATEWAY somewhere else.")" ;;
    auth)     warn "$(tx "  连上了但公钥认证失败：$SSH_KEY 不是包里那把公钥对应的私钥。" "  Connected, but key login failed: $SSH_KEY does not match the public key the kit installed.")" ;;
    timeout)  warn "$(tx '  连接超时/不可达：电脑和 U60 之间不通（没连 U60 的 Wi-Fi？代理/防火墙挡了？）。' "  Timed out / unreachable: this computer cannot reach the U60 (not on the U60's Wi-Fi? blocked by a proxy/firewall?).")" ;;
    *)        warn "  ${SSH_WHY#other:}" ;;
  esac
  warn "$(tx "手动再试: ssh -p $SSH_PORT -i $SSH_KEY root@$GATEWAY（rc.local 里的 dropbear 行只在设备上验证能起来后才写）" "Try by hand: ssh -p $SSH_PORT -i $SSH_KEY root@$GATEWAY (the dropbear line goes into rc.local only after it was seen starting on the device)")"
}

# 装了 ssh 且现在能登录时，才谈得上重启验证
maybe_reboot() {
  [ "${NO_REBOOT:-0}" != 1 ] && [ "${REBOOT:-}" != 0 ] || return 0
  picked ssh && ssh_ok || return 0
  if [ "${REBOOT:-}" = 1 ]; then   # 明确要求重启
    reboot_verify   # 不再问
  elif $HAVE_TTY; then
    local yn
    echo ""   # 和上面的输出隔开
    ask "$(tx '现在重启 U60 验证开机自启吗？（约 2 分钟，会断网）[Y/n]: ' 'Reboot the U60 now to check that everything starts on boot? (about 2 minutes, internet drops) [Y/n]: ')"
    read -r yn
    case "$yn" in [Nn]*) ;; *) reboot_verify ;; esac
  else  # maybe_reboot
    info "$(tx '没有重启验证。要确认开机自启，跑: ./install.sh reboot（会断网约 2 分钟）' 'Reboot check skipped. To check that everything starts on boot, run: ./install.sh reboot (internet drops for about 2 minutes)')"   # 没终端又没设 REBOOT
  fi  # maybe_reboot
}

summary() {
  echo ""   # 摘要前空一行
  printf '%s%s%s%s\n' "$C_GRN" "$C_B" "$(tx '装好了。' 'Done.')" "$C_0"
  if picked ssh; then
    printf '\n  %s ssh -p %s -i %s -o UserKnownHostsFile=%s root@%s\n' "$(tx 'SSH 登录:  ' 'SSH login: ')" "$SSH_PORT" "$SSH_KEY" "$KNOWN" "$GATEWAY"
    printf '  %s\n\n' "$(tx '想简写成 "ssh u60"，把下面几行加进 ~/.ssh/config：' 'To shorten it to "ssh u60", add these lines to ~/.ssh/config:')"
    printf '    %s\n' "Host u60" \
      "    HostName $GATEWAY" "    Port $SSH_PORT" "    User root" \
      "    IdentityFile $SSH_KEY" "    IdentitiesOnly yes" "    UserKnownHostsFile $KNOWN"
  fi  # summary
  if picked admin; then printf '\n  %s http://%s:9090/\n' "$(tx '高级后台:  ' 'Admin web: ')" "$GATEWAY"; fi
  if picked chill; then printf '  CHILL:      %s\n' "$(tx "高级后台 → 服务 → CHILL（面板在 http://$GATEWAY:9090/chill-ui/，从后台的按钮进）" "Admin web -> Services -> CHILL (panel at http://$GATEWAY:9090/chill-ui/, open it from the button in the admin web)")"; fi
  if picked esim; then printf '  eSIM:       %s\n' "$(tx '高级后台 → 移动网络 → eSIM；屏幕上「更多功能 → eSIM」可切换' 'Admin web -> Mobile network -> eSIM; on the screen, More -> eSIM switches profiles')"; fi
  echo ""   # 结尾空一行
}

# 装机进度（设备上 /data/luyaz/kit/progress）认包用的 id：MANIFEST.txt 的 sha256 前 16 位，每次打包都不同。
# 同一个包上次断在半路，再跑就从第一个没完成的组件续；KIT_FRESH=1 从头来。
KIT_ID=""
[ "${KIT_FRESH:-0}" = 1 ] && KIT_FRESH=1 || KIT_FRESH=0

install_kit() {
  KIT_ID=$(sha256 < "$KIT/MANIFEST.txt" | cut -c1-16)
  WORKDIR=$(mktemp -d)
  trap 'rm -rf "$WORKDIR"' EXIT
  mkdir -p "$WORKDIR/u60-kit/payload"
  if picked admin; then admin_password "$WORKDIR/u60-kit/payload"; fi

  info "$(tx '准备要推送的文件…' 'Preparing the files to send...')"
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
if $INSTALLING; then check_device; fi
case "$MODE" in
  status)  show_status ;;
  doctor)  show_doctor; exit $? ;;
  backup)  do_backup ;;
  restore) do_restore ;;
  reboot)  reboot_verify ;;
  install) install_kit ;;
esac
exit 0
