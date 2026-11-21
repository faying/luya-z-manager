#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# CHILL 主控脚本（T2）
#
#   chill.sh run                 procd 实例命令。前台常驻，监督 mihomo 子进程。
#   chill.sh start|stop|restart  总开关（写/删 /data/chill/disabled 并调 init.d）
#   chill.sh reload              重新渲染并热重载（不重启进程）
#   chill.sh status              打印 chill.state
#   chill.sh flush               幂等清理（可单独执行）
#   chill.sh validate [ID]      校验指定或正在使用的配置档案
#   chill.sh validate --template <文件>  校验候选模板和正在使用的档案（ship 转正前用）
#   chill.sh pause <分钟>        临时直连，到点自动恢复
#   chill.sh safe-start          首装用：挂 5 分钟死人开关，没 confirm 就自动 stop
#   chill.sh confirm             撤销 safe-start 的死人开关
#   chill.sh profile [eco|standard|perf]   看/改档位（见下方「档位」）
#
# 目标环境是设备上的 busybox ash，**不是 bash**。以下写法一律不要用：
#   pgrep -c（busybox 不支持 -c）、setsid、timeout、数组、[[ ]]、local -n、进程替换。
# 另外：变量后面紧跟全角字符必须写 ${VAR}，否则中文 locale 下 bash/ash 会把全角字符
# 并进变量名（2026-09-16 因此白跑过一次实验）。
# ─────────────────────────────────────────────────────────────────────────────
set -u

CHILL_DIR=/data/chill
BIN="$CHILL_DIR/bin/mihomo"
TEMPLATE="$CHILL_DIR/template.yaml"
RUN_DIR="$CHILL_DIR/run"
CONFIG="$RUN_DIR/config.yaml"
ENV_FILE="$CHILL_DIR/chill.env"
CN_LIST="$CHILL_DIR/ruleset/cn.list"
DHCP_BACKUP="$CHILL_DIR/dhcp.backup"
# 提成变量而不是硬编码，单测才能注入 fixture 去真正测 restore_dns——
# 它是 flush 的第一步，决定降级时是「直连」还是「全屋断网」，不能只靠替身测。
DHCP_CONF="${DHCP_CONF:-/etc/config/dhcp}"
DNSMASQ_INIT="${DNSMASQ_INIT:-/etc/init.d/dnsmasq}"
# 运营商下发的 DNS（netifd 写的，v4、v6 都在）。/etc/resolv.conf 是原厂另写的，可能只剩 v6。
RESOLV_AUTO="${RESOLV_AUTO:-/tmp/resolv.conf.d/resolv.conf.auto}"
RESOLV_SYS="${RESOLV_SYS:-/etc/resolv.conf}"
EVENTS="$CHILL_DIR/events.log"
DISABLED="$CHILL_DIR/disabled"
GAVEUP="$CHILL_DIR/gaveup"
LEGACY_PAUSE="$CHILL_DIR/pause_until"
PAUSE_UNTIL="$CHILL_DIR/pause.v2"
PROFILE_FILE="$CHILL_DIR/profile"     # 档位：eco / standard / perf，没有 = standard
MODE_FILE="$CHILL_DIR/mode"           # 最近一次的出口模式（rule/global/direct），重启核心后恢复

STATE=/tmp/chill.state
CORE_PID=/tmp/chill.core.pid
LOG=/tmp/chill.log
RULES_BEFORE=/tmp/chill.rules.before
SYS_NET=/sys/class/net                   # 看接口在不在跑（check_rules_drift）；单测指向 fixture
RESTART_FLAG=/tmp/chill.restart       # 有它 = 监督循环下一拍平稳重启核心（不算崩溃）
THERMAL_ECO=/tmp/chill.thermal-eco    # 有它 = 因温度暂时按 eco 跑（重启设备自然清掉）
HEAT_REQ="${HEAT_REQ:-/tmp/luyaz/heat}"  # core 温控的请求（docs/designs/thermal-care.md）：hot = 降到 eco，critical = 停核直连；没有 = 不管
IDLE_LEASE=/tmp/chill.idle.v2            # 核心写的「息屏暂停」租约：到这个秒数前停着（u60-radio.md 6e）

FAKE_IP_NET=198.18.0.0/16
TABLE=2022
# CHILL 自管的 ip rule 区间。8990–9099：mihomo 自己用 9000+，按源 IP 绕行用 8999。
# 区间之外的规则和路由表一律不删。
RULE_LO=8990
RULE_HI=9099
BYPASS_PRIO=8999
# 国内直连（CHILL_CN_SET=1）：nft 集合给发往 CN 段的局域网流量打这一位，8998 这条规则把它送回
# main 表，不进 TUN。位要避开 0xff0000（含原厂的 connmark 0x40000）和 0xC0。
CN_MARK=0x1000000
CN_PRIO=8998
CN_NFT=/tmp/chill.cn.nft
CN_SET_MARK=/tmp/chill.cnset          # 有它 = CN 段由 nft 集合绕开，render 不再把 CN 段写进 route-exclude-address

TEMP_HOT_DEFAULT=75000      # 毫摄氏度。占位值，待第 3 组基线峰值加余量回填
TEMP_RECOVER_DELTA=10000    # 恢复阈值比触发阈值低 10°C
CORE_RSS_MAX_DEFAULT=153600 # kB，约 150MB
MEM_AVAIL_MIN=500           # MB
DWELL_SECONDS=120           # 进入直连后至少驻留多久才评估恢复
CRASH_WINDOW=600            # 10 分钟
CRASH_MAX=3
HEAL_MAX=3                  # 10 分钟内自愈重启超过这么多次就回直连退避（有东西一直在删规则）
TUN_SNAP=/tmp/chill.tun.rules         # 核心起来后自管区间里的 ip rule（自愈对照用）
CORE_READY_MAX=10           # 起核心后最多等这么久（秒）让 API 和 dns.listen 就绪
# /tmp 是内存盘，日志不截断的话长期运行会把内存吃掉（§2 要求每 60 秒截断）。
LOG_MAX_BYTES=524288        # 512 KB
SUP_PID=/tmp/chill.supervisor.pid
BYPASS_PID=/tmp/chill.bypass.pid
CHILL_INIT=/etc/init.d/chill
DNSMASQ=/usr/sbin/dnsmasq
active_id='' active_kind='' active_name='' ipv6_mode=unavailable
runtime_home=$CHILL_DIR

[ -f "$ENV_FILE" ] && . "$ENV_FILE"
legacy_url=${SUB_URL:-}
CHILL_API_LAN="${CHILL_API_LAN:-0}"
CHILL_CN_SET="${CHILL_CN_SET:-0}"
CHILL_BYPASS_IP="${CHILL_BYPASS_IP:-}"
CHILL_API_ALLOW_IP="${CHILL_API_ALLOW_IP:-}"
CHILL_TEMP_HOT="${CHILL_TEMP_HOT:-$TEMP_HOT_DEFAULT}"
# 温度到这里先降到 eco，再高到 CHILL_TEMP_HOT 才停核直连。**默认不设 = 不自动降档**：
# 降档要重启核心（断一下连接），而且阈值要等实测（TODOS.md「稳定性与耗电专场」）。
CHILL_TEMP_WARM="${CHILL_TEMP_WARM:-}"
COOL_HOLD=600               # 降到 WARM-10°C 以下持续这么久才回到原档位
CHILL_CORE_RSS_MAX="${CHILL_CORE_RSS_MAX:-$CORE_RSS_MAX_DEFAULT}"

log() { echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) $*" >> "$LOG" 2>/dev/null || true; }

# /proc/uptime includes suspend. Built-in read/arithmetic: no date/cat/awk per tick.
CHILL_PROC="${CHILL_PROC:-/proc}"
uint() {
  case "$1" in ''|*[!0-9]*|0?*) return 1 ;; esac
  [ "${#1}" -le 12 ]
}
boot_now() {
  IFS=' ' read -r _up _rest 2>/dev/null < "$CHILL_PROC/uptime" || return 1
  case "$_up" in
    *.*) _frac=${_up#*.}; case "$_frac" in ''|*[!0-9]*) return 1 ;; esac ;;
  esac
  _up=${_up%%.*}
  uint "$_up" || return 1
  printf '%s\n' "$_up"
}
boot_id() {
  IFS= read -r _bid 2>/dev/null < "$CHILL_PROC/sys/kernel/random/boot_id" || return 1
  case "$_bid" in ''|*[!A-Za-z0-9-]*) return 1 ;; esac
  [ "${#_bid}" -le 64 ] || return 1
  printf '%s\n' "$_bid"
}
# Return 0 active, 1 inactive (including reboot), 2 invalid/unknown.
# Read a whole file and reject trailing data, exactly like Rust's parser.
deadline_state() {
  _ds=$(cat "$1" 2>/dev/null) || return 2
  set -f
  set -- $_ds
  set +f
  [ "$#" -eq 4 ] || return 2
  [ "$1" = v2 ] || return 2
  case "$2" in ''|*[!A-Za-z0-9-]*) return 2 ;; esac
  [ "${#2}" -le 64 ] || return 2
  uint "$3" && uint "$4" && [ "$4" -ge "$3" ] || return 2
  _db=$(boot_id) && _dn=$(boot_now) || return 2
  [ "$2" = "$_db" ] || return 1
  [ "$_dn" -ge "$3" ] || return 2
  [ "$_dn" -lt "$4" ]
}
idle_paused() { deadline_state "$IDLE_LEASE"; [ "$?" -eq 0 ]; }
# An unreadable manual pause/legacy epoch must not silently resume traffic. Explicit
# pause 0 clears it. A valid v2 pause expires on reboot, not on service restart.
manual_paused() {
  [ -e "$LEGACY_PAUSE" ] && return 0
  [ -e "$PAUSE_UNTIL" ] || return 1
  deadline_state "$PAUSE_UNTIL"; _ps=$?
  [ "$_ps" -eq 1 ] && return 1
  return 0
}
pause_for() {
  _minutes=$1
  uint "$_minutes" && [ "$_minutes" -le 10080 ] || return 2
  if [ "$_minutes" -eq 0 ]; then
    rm -f "$PAUSE_UNTIL" "$LEGACY_PAUSE"
    return
  fi
  _pb=$(boot_id) && _pn=$(boot_now) || return 1
  _pu=$((_pn + _minutes * 60))
  uint "$_pu" || return 1
  (umask 077; printf 'v2 %s %s %s\n' "$_pb" "$_pn" "$_pu" > "$PAUSE_UNTIL.$$" ) &&
    mv "$PAUSE_UNTIL.$$" "$PAUSE_UNTIL" || return 1
  rm -f "$LEGACY_PAUSE"
}

# 状态迁移留痕（DT3）。只在状态真的变化时写，截断到最后 20 行。
last_event=''
event() {
  [ "$1" = "$last_event" ] && return 0
  last_event="$1"
  # 目录按 $EVENTS 自己的路径建，不要按 $CHILL_DIR——两者不同步时会「建了 A、写向 B」。
  # 整块用 { ...; } 2>/dev/null 包住：**重定向失败是 shell 层的错误，
  # 命令级的 2>/dev/null 抑制不掉**，否则路径不存在时会持续往 procd 日志灌噪音。
  {
    mkdir -p "$(dirname "$EVENTS")"
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) $*" >> "$EVENTS"
    tail -20 "$EVENTS" > "$EVENTS.tmp" && mv "$EVENTS.tmp" "$EVENTS"
  } 2>/dev/null || true
}

# ── 温度与内存 ───────────────────────────────────────────────────────────────
# 取 cpuss-0..3 / battery / xo-therm / mdmss-* 的最高值。逐个 zone 读 type 再比对，
# 不写死 zone 编号——固件升级后编号会变。
max_temp() {
  m=0
  for z in /sys/class/thermal/thermal_zone*; do
    t=$(cat "$z/type" 2>/dev/null) || continue
    case "$t" in
      cpuss*|battery|xo-therm|mdmss*) ;;
      *) continue ;;
    esac
    v=$(cat "$z/temp" 2>/dev/null) || continue
    [ "$v" -gt "$m" ] 2>/dev/null && m=$v
  done
  echo "$m"
}
mem_avail_mb() { awk '/^MemAvailable/{printf "%d", $2/1024}' /proc/meminfo 2>/dev/null || echo 0; }
core_rss_kb() {
  p=$(cat "$CORE_PID" 2>/dev/null) || { echo 0; return; }
  awk '/^VmRSS/{print $2}' "/proc/$p/status" 2>/dev/null || echo 0
}

# GET /version 是否 2xx，供监督循环判活。带 Authorization 头是安全的：
# CHILL_SECRET 为空时 mihomo 根本不检查这个头，不会因为发了个空 token 出错。
# 2026-09-17 真机事故：这里原来完全不带头，CHILL_SECRET 一旦真的被设置，
# 这条健康检查自己就被 mihomo 拒成 401，连续两次 401 被当成"核心崩溃"，
# 陷入反复重启直到撞上 CRASH_MAX 才 gaveup——不是核心真的崩了，是判活探针
# 自己没跟上鉴权。用的是 curl 不是 wget：这台设备的 busybox wget 不支持
# --header/-H。
# 参数是 curl 的 -m（秒），默认 3；起核心时的就绪轮询用 1。
api_alive() {
  curl -s -m "${1:-3}" -o /dev/null -w '%{http_code}' \
    -H "Authorization: Bearer ${CHILL_SECRET:-}" \
    http://127.0.0.1:9999/version 2>/dev/null | grep -q '^2'
}

# ── 状态文件（对外契约，见 §9）────────────────────────────────────────────────
# 原子写入：先写临时文件再 mv。字段名不能随便改，触屏和后台都按这个读。
write_state() {
  _state="$1"; _reason="$2"
  _pid=$(cat "$CORE_PID" 2>/dev/null || echo 0)
  _bypass="${bypass_stale_json:-[]}"
  cat > "$STATE.$$" <<EOF
{"state":"$_state","reason":$( [ -n "$_reason" ] && echo "\"$_reason\"" || echo null ),
"cpuss_c":$(( $(max_temp) / 1000 )),"mem_avail_mb":$(mem_avail_mb),
"core_pid":$_pid,"started_at":"${started_at:-}","updated_at":"$(date -u +%Y-%m-%dT%H:%M:%SZ)",
"bypass_stale":$_bypass,"rules_drift":${rules_drift:-false},"mem_pressure":${mem_pressure:-false},
"profile":"$(profile_read)","profile_effective":"$(profile_effective)","thermal_eco":$([ -f "$THERMAL_ECO" ] && echo true || echo false),
"cn_set":"${cn_set:-routes}","profile_id":"$active_id","profile_name":$(json_string "$active_name"),
"mode":"$(saved_mode)","ipv6_mode":"$ipv6_mode"}
EOF
  mv "$STATE.$$" "$STATE" 2>/dev/null
}

json_string() {
  printf '%s\n' "$1" | awk 'BEGIN { printf "\"" } NR > 1 { printf "\\n" } { for (i=1;i<=length($0);i++) { c=substr($0,i,1); if (c=="\"" || c=="\\") printf "\\"; if (c !~ /[[:cntrl:]]/) printf "%s", c } } END { printf "\"" }'
}

# ── AI 闸门（CHILL_AI_STRICT=1）──────────────────────────────────────────────
# AI 的流量只走 🤖 AI 选的地区。CHILL 一停（崩溃、卡死、过热、暂停、息屏暂停、总开关关）
# 或出口模式不是 rule（direct / global 不看规则），AI 网域就由 dnsmasq 答「不存在」，
# 直到核心确认蜂窝落地就是 🤖 AI 的地区（核心写 $AI_OK，见 crates/luyaz-core 的
# engines/chill_ai_gate.rs）。其余网站照常直连，管理网页不受影响。
# - 封锁文件放 dnsmasq 的 conf-dir。dnsmasq 的 reload 只发 SIGHUP，不重读 conf-dir（10-24 真机
#   验证），所以封锁文件一有变化就 restart。
# - 先封后放：CHILL 停下的同一次 dnsmasq 重启里就封上，不等核心。
# - 网域取自 🤖 AI 的规则集（mihomo 每 3 天更新的文本版），再加一份内置的底，规则集坏了也不空。
AI_LIST="$CHILL_DIR/ruleset/ai_domain.list"
AI_BLOCK="${AI_BLOCK:-/tmp/dnsmasq.d/luyaz-ai-block.conf}"
AI_OK="${AI_OK:-/tmp/chill.ai-ok}"
AI_LOCK="${AI_LOCK:-/tmp/chill.ai-gate.lock}"
AI_FALLBACK="openai.com chatgpt.com oaistatic.com oaiusercontent.com sora.com anthropic.com claude.ai claude.com gemini.google.com aistudio.google.com generativelanguage.googleapis.com perplexity.ai x.ai grok.com"

ai_strict() { [ "${CHILL_AI_STRICT:-0}" = 1 ]; }

# dnsmasq 的封锁文件内容：每个网域一行 address=/网域/（连子网域一起答 NXDOMAIN，A 和 AAAA 都是）。
ai_block_text() {
  { cat "$AI_LIST" 2>/dev/null; for _d in $AI_FALLBACK; do echo "$_d"; done; } \
    | tr -d '\r' | sed -n 's/^+\.//; /^[a-z0-9][a-z0-9.-]*\.[a-z0-9-]*[a-z0-9]$/p' \
    | sort -u | sed 's|.*|address=/&/|'
}

# 主路由表默认路由的「接口 IPv4」（和核心 api/exits.rs 的 route_key 一样）：重拨、换卡都会变。
ai_route_key() {
  _if=$(awk 'NR>1 && $2=="00000000" && $8=="00000000" {print $1; exit}' "$CHILL_PROC/net/route" 2>/dev/null)
  [ -n "$_if" ] || return 1
  _ip=$(ip -4 -o addr show dev "$_if" 2>/dev/null | awk '{split($4, a, "/"); print a[1]; exit}')
  echo "$_if $_ip"
}

# 核心确认过：这次开机、这条蜂窝连接、还没到期，直连的落地就是 🤖 AI 的地区。
# 文件：v2 <boot_id> <起> <止> direct <接口> <IPv4> <地区>（起止是 /proc/uptime 的秒）。
ai_ok_direct() {
  _ok=$(cat "$AI_OK" 2>/dev/null) || return 1
  set -f; set -- $_ok; set +f
  [ "$#" -eq 8 ] && [ "$1" = v2 ] && [ "$5" = direct ] || return 1
  uint "$3" && uint "$4" || return 1
  _ob=$(boot_id) && _on=$(boot_now) || return 1
  [ "$2" = "$_ob" ] && [ "$_on" -ge "$3" ] && [ "$_on" -lt "$4" ] || return 1
  [ "$(ai_route_key)" = "$6 $7" ]
}

# CHILL 不在跑（或模式 direct）时要不要封：on / off。
ai_want_down() {
  ai_strict || { echo off; return; }
  ai_ok_direct && echo off || echo on
}
# CHILL 刚接管 DNS、或运行中复查：rule 模式放开；direct 同上；global 先封，由核心经 7894 确认后放
# （运行中复查时 global 交给核心，返回 keep）。
ai_want_up() {
  ai_strict || { echo off; return; }
  case "$(cat "$MODE_FILE" 2>/dev/null)" in
    direct) ai_want_down ;;
    global) echo "${1:-on}" ;;
    *) echo off ;;
  esac
}

# 把封锁文件改成 on / off（keep 不动）。返回 0 = 变了（调用方要 restart dnsmasq），1 = 没变。
# 核心经 `chill.sh ai-gate` 也会写，用 flock 串起来（busybox 的 flock 没有 -w；锁里只有文件操作，
# dnsmasq 重启在锁外，不会久等）。
ai_gate_write() {
  [ "$1" = keep ] && return 1
  if command -v flock >/dev/null 2>&1; then
    ( flock -x 9 || true; ai_gate_write_locked "$1" ) 9>"$AI_LOCK"
  else
    ai_gate_write_locked "$1"
  fi
}
ai_gate_write_locked() {
  if [ "$1" = on ]; then
    mkdir -p "$(dirname "$AI_BLOCK")" 2>/dev/null
    { ai_block_text > "$AI_BLOCK.tmp"; } 2>/dev/null || return 1
    if cmp -s "$AI_BLOCK.tmp" "$AI_BLOCK"; then rm -f "$AI_BLOCK.tmp"; return 1; fi
    mv "$AI_BLOCK.tmp" "$AI_BLOCK" || return 1
    log "AI 闸门：封（$(wc -l < "$AI_BLOCK" | tr -d ' ') 个网域）"
    event "ai_gate -> blocked"
  else
    [ -e "$AI_BLOCK" ] || return 1
    rm -f "$AI_BLOCK" || return 1
    log "AI 闸门：放"
    event "ai_gate -> open"
  fi
  return 0
}

dnsmasq_restart() {
  "$DNSMASQ_INIT" restart >/dev/null 2>&1
}

# 运行中每分钟一次：封锁文件和现在的模式对不上就改（核心刚好在 CHILL 起来时写了封锁的那种竞态）。
ai_gate_tick() {
  ai_gate_write "$(ai_want_up keep)" && dnsmasq_restart
  return 0
}

# ── dnsmasq 回滚（决定 3）────────────────────────────────────────────────────
# ⚠ 这是整个脚本最不能出错的函数。dnsmasq 上游指向 mihomo 期间，只停核不回滚 DNS
# 会让全屋**断网**而不是降级成直连——过热、崩溃、stop、重启全都会踩到。
# 幂等：用 cmp -s 判断，一致就不动，避免无谓 reload。
# 换档位后马上把 chill.state 里的 profile / profile_effective 改成新值，其余字段原样。以前要等
# 核心重启（进出 eco，约 10 秒）或下一次写状态，网页换档位的读回总是「没有生效」（10-09 真机发现）。
# run 下一次写状态时会按同样的规则重算，不会冲突。
state_profile_now() {
  [ -f "$STATE" ] || return 0
  _p=$(profile_read); _e=$(profile_effective)
  sed -e "s/\"profile\":\"[a-z]*\"/\"profile\":\"$_p\"/" \
      -e "s/\"profile_effective\":\"[a-z]*\"/\"profile_effective\":\"$_e\"/" \
      "$STATE" > "$STATE.tmp" 2>/dev/null && mv "$STATE.tmp" "$STATE" 2>/dev/null
}

# 总开关关掉以后：把 chill.state 写成 stopped。run 只会写 running / direct，以前停机后文件一直
# 停在关之前的 running，网页读回「没有生效」、首页也照旧显示运行中（2026-10-09 真机发现）。
mark_stopped() {
  started_at=""
  select_profile || true
  write_state stopped ""
}

# AI 闸门在同一次 dnsmasq 重启里封上（见「AI 闸门」）：封锁文件变了就 restart，不 reload。
restore_dns() {
  _ai_changed=1
  ai_gate_write "$(ai_want_down)" && _ai_changed=0
  if [ -f "$DHCP_BACKUP" ] && ! cmp -s "$DHCP_BACKUP" "$DHCP_CONF"; then
    if ! cp "$DHCP_BACKUP" "$DHCP_CONF" 2>/dev/null; then
      [ "$_ai_changed" = 0 ] && dnsmasq_restart
      return 1
    fi
    if [ "$_ai_changed" = 0 ]; then
      dnsmasq_restart
    else
      "$DNSMASQ_INIT" reload >/dev/null 2>&1 || dnsmasq_restart
    fi
    log "restore_dns: dnsmasq 已回滚到备份"
    event "dns_restored -> direct"
  elif [ "$_ai_changed" = 0 ]; then
    dnsmasq_restart
  fi
  return 0
}

# 让 dnsmasq 把查询转发给 mihomo。**必须在确认 mihomo 的 dns.listen 能应答之后再调**，
# 反序会造成启动期间全屋 DNS 中断。
apply_dns() {
  [ -f "$DHCP_BACKUP" ] || cp "$DHCP_CONF" "$DHCP_BACKUP" 2>/dev/null || return 1
  # 段名是 lan_dns 不是 @dnsmasq[0]（本机实例名），写错会静默设到不生效的段上
  uci -q set dhcp.lan_dns.noresolv=1 || return 1
  uci -q delete dhcp.lan_dns.server 2>/dev/null
  uci -q add_list dhcp.lan_dns.server=127.0.0.1 && uci -q commit dhcp || return 1
  if ai_gate_write "$(ai_want_up)"; then
    dnsmasq_restart
  else
    "$DNSMASQ_INIT" reload >/dev/null 2>&1
  fi || return 1
  log "apply_dns: dnsmasq 上游已指向 mihomo"
}

dns_answers() {
  # 确认 mihomo 的 dns.listen 真的能应答，再动 dnsmasq
  nslookup www.gstatic.com 127.0.0.1 >/dev/null 2>&1
}

# 核心建好 chill0 后补它的 RPS（manager docs/designs/net-performance.md 1b）。chill0 每次启动
# 都是新建的，所以每次都要调。子进程跑、结果不管；脚本不在（旧 scripts 组件）就什么都不做，
# 调优开关没开（没有 net-tune.want）时它自己什么都不改。
NET_TUNE="${CHILL_NET_TUNE:-/data/luyaz/scripts/net-tune.sh}"
net_tune_chill0() {
  [ -f "$NET_TUNE" ] || return 0
  sh "$NET_TUNE" apply chill0 >> "$LOG" 2>&1 < /dev/null || true
}

# 起核心后等它就绪：API 回 2xx（与判活同一个探针，带鉴权头）且 dns.listen 能应答。
# 代替原来固定的 sleep 5：快的时候不白等；慢的时候 apply_dns 只在启动时试一次，
# 5 秒没起来就整轮都不接管 DNS。每秒问一次，最多 CORE_READY_MAX 秒（不短于原来的
# 5 秒）。设备上没有 timeout 命令，单次探测靠 curl 自己的 -m 1 限时。
# 返回 0 = 就绪，1 = 到时间还没就绪，2 = 核心已经退出（交给调用方的 kill -0 处理）。
wait_core_ready() {
  _wpid="$1"
  _wn=$(boot_now) || return 1
  _wend=$((_wn + CORE_READY_MAX))
  _wtries=0
  while :; do
    kill -0 "$_wpid" 2>/dev/null || return 2
    api_alive 1 && dns_answers && return 0
    _wn=$(boot_now) || return 1
    [ "$_wn" -lt "$_wend" ] && [ "$_wtries" -lt "$CORE_READY_MAX" ] || return 1
    _wtries=$((_wtries + 1))
    sleep 1
  done
}

# IPv4-only profiles must not leave a public IPv6 forwarding path around TUN.
# The router's own IPv6 and LAN-scoped destinations are unaffected.
block_ipv6() {
  nft -f - <<'EOF'
table inet chillv6 {
  chain forward {
    type filter hook forward priority -5; policy accept;
    iifname "br-lan" ip6 daddr != { fc00::/7, fe80::/10, ff00::/8 } counter reject
  }
}
EOF
}

bypass_lock() ( umask 077; flock -x 8 && "$@" ) 8>"$CHILL_DIR/bypass.lock"

bypass_configured() (
  [ ! -f "$ENV_FILE" ] || . "$ENV_FILE"
  printf '%s\n' "${CHILL_BYPASS_IP:-}"
)

read_lan_addrs() {
  bypass_lan4=$(ip -4 addr show dev br-lan 2>/dev/null | awk '/inet / {split($2,a,"/"); print a[1]; exit}')
  bypass_lan6=$(ip -6 addr show dev br-lan scope link 2>/dev/null | awk '/inet6 / {split($2,a,"/"); print a[1]; exit}')
  [ -n "$bypass_lan4" ]
}

bypass_macs() {
  # Static leases work even if the client has not appeared in the ARP table
  # since this boot. Fall back to live ARP for clients with manually fixed IPs.
  client_macs=''
  for addr in $1; do
    mac=$(uci -q show dhcp | awk -F= -v ip="'$addr'" '
      $2 == ip && $1 ~ /\.ip$/ {section=$1; sub(/\.ip$/, "",section); want[section]=1}
      $1 ~ /\.mac$/ {section=$1; sub(/\.mac$/, "",section); value[section]=$2}
      END {for (s in want) if (s in value) print value[s]}' | tr -d "'")
    [ -n "$mac" ] || mac=$(ip -4 neigh show to "$addr" dev br-lan 2>/dev/null | awk '{for(i=1;i<NF;i++) if ($i=="lladdr") {print $(i+1); exit}}')
    [ -n "$mac" ] || return 1
    client_macs="$client_macs $mac"
  done
  printf '%s\n' "$client_macs" | awk '
    { for (i=1;i<=NF;i++) {
      mac=tolower($i); if (split(mac,bytes,":") != 6) { bad=1; continue }
      for (j=1;j<=6;j++) if (bytes[j] !~ /^[0-9a-f][0-9a-f]$/) bad=1
      if (!seen[mac]++) { printf "%s%s", count++ ? ", " : "", mac }
    } }
    END {print ""; if (bad || !count) exit 1}'
}

bypass_dns_pid() {
  IFS= read -r _dns_pid 2>/dev/null < "$BYPASS_PID" && uint "$_dns_pid" &&
    [ "$_dns_pid" -gt 1 ] &&
    [ "$(readlink "/proc/$_dns_pid/exe" 2>/dev/null)" = "$DNSMASQ" ] &&
    tr '\0' '\n' < "/proc/$_dns_pid/cmdline" 2>/dev/null | grep -Fxq -- "--pid-file=$BYPASS_PID"
}

bypass_dns_stop() {
  if bypass_dns_pid; then kill "$_dns_pid" 2>/dev/null || true; fi
  rm -f "$BYPASS_PID"
}

bypass_dns_start() {
  bypass_dns_pid && return 0
  [ -s "$RESOLV_AUTO" ] && grep -q '^nameserver[[:space:]]' "$RESOLV_AUTO" || return 1
  dns_addresses=$bypass_lan4
  [ -z "$bypass_lan6" ] || dns_addresses="$dns_addresses,$bypass_lan6"
  # Stock dnsmasq keeps stdout open after forking. Close the Core job's pipes.
  "$DNSMASQ" --conf-file=/dev/null --bind-interfaces --listen-address="$dns_addresses" \
    --port=1054 --resolv-file="$RESOLV_AUTO" --no-hosts --cache-size=0 \
    --pid-file="$BYPASS_PID" 8>&- </dev/null >/dev/null 2>&1 && bypass_dns_pid
}

bypass_dns_rules() {
  if nft list table inet chilldns >/dev/null 2>&1; then echo 'delete table inet chilldns'; fi
  [ -n "$1" ] || return 0
  printf 'table inet chilldns {\n set clients { type ipv4_addr; elements = { %s }; }\n' "$(printf '%s' "$1" | sed 's/ /, /g')"
  cat <<'EOF'
 chain pre {
   type nat hook prerouting priority -105; policy accept;
   iifname "br-lan" ip saddr @clients meta l4proto { tcp, udp } th dport 53 redirect to :1054
EOF
  if [ -n "$2" ]; then
    printf '   iifname "br-lan" ether saddr { %s } meta nfproto ipv6 meta l4proto { tcp, udp } th dport 53 redirect to :1054\n' "$2"
  fi
  cat <<'EOF'
 }
 chain input {
   type filter hook input priority -5; policy accept;
   iifname "br-lan" ip saddr @clients meta l4proto { tcp, udp } th dport 1054 accept
EOF
  if [ -n "$2" ]; then
    printf '   iifname "br-lan" ether saddr { %s } meta nfproto ipv6 meta l4proto { tcp, udp } th dport 1054 accept\n' "$2"
  fi
  printf '   meta l4proto { tcp, udp } th dport 1054 drop\n }\n}\n'
}

bypass_route_clear() {
  ip rule show 2>/dev/null | awk -F: -v p="$BYPASS_PRIO" '$1+0==p {print $1}' |
    while IFS= read -r pref; do ip rule del pref "$pref" || exit 1; done
}

bypass_route_add() {
  for addr in $1; do ip rule add from "$addr" priority "$BYPASS_PRIO" lookup main || return 1; done
}

bypass_apply() { # requested IPv4 list; caller holds bypass lock
  bypass_old=$(ip rule show 2>/dev/null | awk -v p="$BYPASS_PRIO:" '$1==p && $2=="from" {sub(/\/32$/,"",$3); printf "%s%s", (n++ ? " " : ""), $3}')
  if [ -n "$1" ]; then
    read_lan_addrs || return 1
    if [ -n "$bypass_lan6" ]; then
      bypass_v6_macs=$(bypass_macs "$1") || return 1
    else
      bypass_v6_macs=''
    fi
    bypass_dns_rules "$1" "$bypass_v6_macs" | nft -c -f - >/dev/null 2>&1 || return 1
    bypass_dns_start || return 1
  fi
  if bypass_route_clear && bypass_route_add "$1" &&
     bypass_dns_rules "$1" "${bypass_v6_macs:-}" | nft -f - >/dev/null 2>&1; then
    [ -n "$1" ] || bypass_dns_stop
    return 0
  fi
  bypass_route_clear && bypass_route_add "$bypass_old" || log 'bypass: route rollback failed'
  [ -n "$bypass_old" ] || bypass_dns_stop
  return 1
}

bypass_apply_saved() { bypass_apply "$(bypass_configured)"; }

bypass_set() { # Core passes only a canonical, sorted list of IPv4 addresses
  bypass_want=$*
  for addr in "$@"; do
    printf '%s\n' "$addr" | awk -F. 'NF!=4 {exit 1} {for(i=1;i<=4;i++) if ($i!~/^[0-9]+$/ || $i>255) exit 1}' || return 1
  done
  {
    if [ -f "$ENV_FILE" ]; then awk '!/^[[:space:]]*(export[[:space:]]+)?CHILL_BYPASS_IP=/' "$ENV_FILE"; fi
    printf "CHILL_BYPASS_IP='%s'\n" "$bypass_want"
  } > "$ENV_FILE.$$" || return 1
  bypass_previous=$(bypass_configured)
  bypass_live=0
  if api_alive 1; then
    bypass_live=1
    if ! bypass_apply "$bypass_want"; then
      rm -f "$ENV_FILE.$$"
      echo 'bypass_dns_unavailable' >&2
      return 1
    fi
  fi
  if mv -f "$ENV_FILE.$$" "$ENV_FILE"; then return 0; fi
  [ "$bypass_live" = 0 ] || bypass_apply "$bypass_previous"
  return 1
}

bypass_clear() {
  nft delete table inet chilldns 2>/dev/null || true
  bypass_dns_stop
}

# ── flush（幂等；任何退出路径都要能跑）──────────────────────────────────────
flush() {
  # 第一步必须是 restore_dns，理由见该函数注释
  restore_dns

  kill_core

  nft delete table inet mihomo 2>/dev/null || true
  nft delete table inet chill 2>/dev/null || true
  nft delete table inet chillcn 2>/dev/null || true
  nft delete table inet chillv6 2>/dev/null || true
  bypass_lock bypass_clear
  rm -f "$CN_SET_MARK"
  ipv6_mode=unavailable

  # 只删自管区间，逐条删到没有。区间外一律不碰。
  for fam in 4 6; do
    [ "$fam" = 4 ] && C="ip" || C="ip -6"
    n=0
    while :; do
      p=$($C rule show 2>/dev/null | awk -F: -v lo="$RULE_LO" -v hi="$RULE_HI" \
            '$1+0>=lo && $1+0<=hi {print $1; exit}')
      [ -n "$p" ] || break
      $C rule del pref "$p" 2>/dev/null || break
      n=$((n+1))
      [ "$n" -gt 64 ] && break   # 防御性上限，避免异常情况下死循环
    done
    $C route flush table "$TABLE" 2>/dev/null || true
  done

  ip route del unreachable "$FAKE_IP_NET" 2>/dev/null || true
  rm -f "$CORE_PID" "$TUN_SNAP"
  log "flush 完成"
  check_rules_drift
}

# 段外漂移检测（T3）。
# 只检测自管区间之外的规则有没有被删除。比对启动前和当前的快照时，
# 两边都剔除 8990–9099。正常降级会清空段内规则，所以段内变化不记作漂移。
strip_owned() {
  awk -F: -v lo="$RULE_LO" -v hi="$RULE_HI" '
    { p = $1 + 0 }
    p >= lo && p <= hi { next }
    { print }' 2>/dev/null
}
# 只看「快照里有、现在没有」（删除）：新增不算漂移。原厂会按接口动态加规则
# （如 IPv6 的 `42000000xx: from all iif wlanN failed_policy`，优先级随 ifindex 变），
# 按新增判会常驻误报、每分钟刷日志；也不按优先级或关键字加白名单。
missing_rules() { # missing_rules <快照> <现状>：输出快照里有、现状里没有的行
  awk 'NR==FNR { have[$0] = 1; next } !($0 in have)' "$2" "$1" 2>/dev/null
}
# 绑在接口上的规则（`iif X` / `oif X`）跟着接口走：接口关了（情景关 Wi-Fi）原厂就删，
# 接口回来再加，优先级随 ifindex 变。所以这类规则：接口没在跑 → 不算消失；接口在跑 →
# 只比优先级后面的内容。真被删掉的（接口在跑、内容也找不到）照样算。
iface_rules_settled() { # iface_rules_settled <现状>：stdin 是消失的行，输出仍算消失的
  while IFS= read -r _l; do
    _if=$(printf '%s\n' "$_l" | sed -n 's/.*[[:space:]][io]if[[:space:]]\{1,\}\([^[:space:]]\{1,\}\).*/\1/p')
    if [ -n "$_if" ]; then
      _st=$(cat "$SYS_NET/$_if/operstate" 2>/dev/null)
      case "$_st" in up|unknown) ;; *) continue ;; esac
      awk -v b="${_l#*:}" '{ x = $0; sub(/^[^:]*:/, "", x); if (x == b) f = 1 } END { exit !f }' "$1" && continue
    fi
    printf '%s\n' "$_l"
  done
}
check_rules_drift() {
  [ -f "$RULES_BEFORE" ] || return 0
  { ip rule show; ip -6 rule show; } 2>/dev/null | strip_owned > /tmp/chill.rules.after
  strip_owned < "$RULES_BEFORE" > /tmp/chill.rules.snap
  _gone=$(missing_rules /tmp/chill.rules.snap /tmp/chill.rules.after | iface_rules_settled /tmp/chill.rules.after)
  if [ -z "$_gone" ]; then
    rules_drift=false
  else
    rules_drift=true
    log "rules_drift: 自管区间之外有启动前快照里的 ip rule 不见了"
    log "--- 消失的规则 ---"
    printf '%s\n' "$_gone" | head -10 >> "$LOG" 2>/dev/null
  fi
  rm -f /tmp/chill.rules.after /tmp/chill.rules.snap
}

# 清理存量核心：只认 /proc/<pid>/exe 指向我们的 bin，**绝不按进程名**匹配，
# 否则会误杀同名进程。
kill_core() {
  p=$(cat "$CORE_PID" 2>/dev/null || echo '')
  if [ -n "$p" ] && [ -e "/proc/$p/exe" ]; then
    if readlink "/proc/$p/exe" 2>/dev/null | grep -q "$BIN"; then
      kill "$p" 2>/dev/null
      i=0; while [ $i -lt 3 ] && [ -e "/proc/$p" ]; do sleep 1; i=$((i+1)); done
      [ -e "/proc/$p" ] && kill -9 "$p" 2>/dev/null
    fi
  fi
  # 兜底：按可执行文件路径扫描，仍然不按进程名
  for d in /proc/[0-9]*; do
    q=${d#/proc/}
    [ "$(readlink "$d/exe" 2>/dev/null)" = "$BIN" ] || continue
    # Validation and inactive-provider probes use the same executable.
    tr '\0' '\n' < "$d/cmdline" 2>/dev/null | grep -Eq '^(-t|-ext-ctl-unix)$' && continue
    kill -9 "$q" 2>/dev/null
  done
  rm -f "$CORE_PID"
}

# ── 地区表 → filter 正则（T5 的生成侧）───────────────────────────────────────
# 这张表是地区的唯一事实来源，模板里只有占位符。改地区只改这里。
#
# ⚠ 字母代码必须套边界 (^|[^A-Za-z])…([^A-Za-z]|$)：
#   不套的话 US 会命中 "Russia"、SG 会命中 "Lansing" 这类子串，把节点错分到别的地区，
#   而且这种错分不会报错，只会让某个地区组里混进不相干的节点。中文名不需要边界。
REGION_TW='台湾|台灣|Taiwan'
REGION_JP='日本|Japan'
REGION_SG='新加坡|Singapore'
REGION_US='美国|美國|United States'
CODE_TW='TWN?'
CODE_JP='JPN?'
CODE_SG='SGP?'
CODE_US='USA?'

build_filters() {
  _b='(^|[^A-Za-z])'; _e='([^A-Za-z]|$)'

  # 先拼**不带 flag**的 body，(?i) 统一加在最外层最前面。
  # 2026-09-16 实测：不加忽略大小写时，us-01 / tw01 / jp-tokyo 这类小写命名
  # 全部匹配不到，节点被静默漏掉——地区组少一批节点却不报任何错。
  # ⚠ 不要让每个分支各带 (?i) 再拼成并集：Go 允许 (?i) 出现在任意位置，
  #   但 Python 3.11+ 要求全局 flag 必须在开头，那样写会让离线测试脚本直接
  #   编译失败，等于把归属测试变成盲区。统一前置只留一个 flag，两种引擎都能编。
  _tw="${REGION_TW}|${_b}${CODE_TW}${_e}"
  _jp="${REGION_JP}|${_b}${CODE_JP}${_e}"
  _sg="${REGION_SG}|${_b}${CODE_SG}${_e}"
  _us="${REGION_US}|${_b}${CODE_US}${_e}"
  FILTER_TW="(?i)(${_tw})"
  FILTER_JP="(?i)(${_jp})"
  FILTER_SG="(?i)(${_sg})"
  FILTER_US="(?i)(${_us})"

  # 并集用于 provider 的 filter：只有被并集收录的节点才会进入本地缓存。
  # CHILL_REGIONS 可裁剪地区（留空=四个全要）。被裁掉的地区其策略组会没有成员，
  # 命中 empty-fallback: REJECT，这是预期行为而不是故障。
  # 并集同样只在最前面加一个 (?i)，所以这里拼的是不带 flag 的 body
  _u=''
  for r in ${CHILL_REGIONS:-TW JP SG US}; do
    eval "_f=\${_$(echo "$r" | tr 'A-Z' 'a-z'):-}"
    [ -n "$_f" ] || { log "build_filters: 未知地区 ${r}，跳过"; continue; }
    [ -n "$_u" ] && _u="$_u|$_f" || _u="$_f"
  done
  [ -n "$_u" ] && FILTER_UNION="(?i)($_u)" || FILTER_UNION=""

  # 只排除信息类假节点（订阅商塞进节点列表的广告/状态条目），不再连坐高倍率
  # 真实节点。2026-09-21 真机核对：tag provider 里 47/321 个节点被原来那条
  # "(5|10|20|50|100)[x×倍]" 子句误杀，其中日本星链/蜂窝5G、新加坡家宽等
  # 真实家宽/优选节点全在里面——这些节点本来就用倍率后缀命名（"10x"标的是
  # 消耗配额倍数，不是广告文案），排除规则不该把它们当垃圾丢掉。
  EXCLUDE_FILTER='(?i)(流量|到期|过期|重置|Traffic|Expire|官网|剩余|套餐)'

  [ -n "$FILTER_UNION" ] || { log "build_filters: 并集为空，拒绝渲染"; return 1; }
  return 0
}

# ── 渲染 ────────────────────────────────────────────────────────────────────
# 用 awk 按 ENVIRON 做字面量替换（index/substr），不用 gsub——值里含 / 和 & 无碍。
# 值含单引号或换行则拒绝渲染：模板把占位符写在 YAML 单引号内，含引号会破坏结构。
# ── 档位 ─────────────────────────────────────────────────────────────────────
# 只动三类开关，都是省电/发热和切换速度之间的取舍，不碰分流规则和节点：
#                       eco       standard（= 以前唯一的一套）  perf
#   节点探测间隔 (s)     3600      1800                          600
#   TCP keep-alive (s)   900       600                           120
#   tcp-concurrent       false     false                         true（多个 IP 并发握手，连得快、多耗一点）
#   GOMAXPROCS           2         不设（4 核）                  不设
# GOMAXPROCS 只在核心启动时生效，所以进出 eco 要重启核心（断几秒）；standard↔perf 热重载。
# 数值是按取舍方向定的初值，没有实测；实测放在「稳定性与耗电专场」。
profile_read() {
  _p=$(cat "$PROFILE_FILE" 2>/dev/null)
  case "$_p" in eco|standard|perf) echo "$_p" ;; *) echo standard ;; esac
}
profile_effective() {
  if [ -f "$THERMAL_ECO" ]; then echo eco; else profile_read; fi
}
profile_vars() { # <profile> → HC_INTERVAL KEEPALIVE TCP_CONCURRENT GMP
  case "$1" in
    eco)  HC_INTERVAL=3600; KEEPALIVE=900; TCP_CONCURRENT=false; GMP=2 ;;
    perf) HC_INTERVAL=600;  KEEPALIVE=120; TCP_CONCURRENT=true;  GMP= ;;
    *)    HC_INTERVAL=1800; KEEPALIVE=600; TCP_CONCURRENT=false; GMP= ;;
  esac
}

# ── 出口模式的保存与恢复 ───────────────────────────────────────────────────────
# 分组的选择有 store-selected 记着，模式（rule/global/direct）没有：热重载和重启核心
# 都会回到模板里的 mode: rule，「全部直连」「全局」就被悄悄换掉了。每分钟和每次
# 重载/重启之前记一次，核心起来后按记下的恢复。
api_mode() {
  curl -s -m 3 -H "Authorization: Bearer ${CHILL_SECRET:-}" http://127.0.0.1:9999/configs 2>/dev/null \
    | sed -n 's/.*"mode":"\([a-z]*\)".*/\1/p'
}
saved_mode() {
  _saved=$(cat "$MODE_FILE" 2>/dev/null)
  case "$_saved" in rule|global|direct) printf '%s\n' "$_saved" ;; *) echo rule ;; esac
}

write_marker() {
  (umask 077; printf '%s\n' "$2" > "$1.$$") && mv -f "$1.$$" "$1"
}

save_mode() {
  _m=$(api_mode)
  case "$_m" in
    rule|global|direct) [ "$(cat "$MODE_FILE" 2>/dev/null)" = "$_m" ] || write_marker "$MODE_FILE" "$_m" ;;
  esac
}
restore_mode() {
  _m=$(cat "$MODE_FILE" 2>/dev/null)
  case "$_m" in rule|global|direct) ;; *) return 0 ;; esac
  [ "$(api_mode)" = "$_m" ] && return 0
  curl -fsS -m 3 -o /dev/null -X PATCH -H "Authorization: Bearer ${CHILL_SECRET:-}" \
    -H 'Content-Type: application/json' --data "{\"mode\":\"$_m\"}" \
    http://127.0.0.1:9999/configs 2>/dev/null && [ "$(api_mode)" = "$_m" ] || return 1
  log "出口模式恢复为 $_m"
}

# 热重载：渲染 → PUT /configs。渲染不过就保留旧配置。模式先记后恢复。
do_reload() {
  select_profile || return 1
  save_mode
  render || return 1
  curl -s -m 5 -o /dev/null -w '%{http_code}' -X PUT \
    -H "Authorization: Bearer ${CHILL_SECRET:-}" \
    --data "{\"path\":\"$CONFIG\"}" "http://127.0.0.1:9999/configs?force=true" \
    | grep -q '^2' || return 1
  restore_mode
  return 0
}

# A profile ID is also a directory name. Validate it before using metadata or a
# marker as a path. The stock template is the only implicit (legacy) profile.
valid_id() {
  case "$1" in [a-z0-9]*) ;; *) return 1 ;; esac
  case "$1" in *[!a-z0-9_-]*) return 1 ;; esac
  [ "${#1}" -le 32 ]
}

profile_field() {
  jsonfilter -i "$CHILL_DIR/profiles/$1/profile.json" -e "@.$2" 2>/dev/null
}

marker_id() {
  cat "$CHILL_DIR/active-profile" 2>/dev/null || true
}

select_profile() {
  active_id='' active_name='' active_kind='' runtime_home=$CHILL_DIR
  SUB_URL=${legacy_url:-}
  _selected=${1-$(marker_id)}
  if [ -z "$_selected" ]; then
    if [ -n "$SUB_URL" ] && [ ! -d "$CHILL_DIR/profiles" ]; then
      active_kind=legacy active_name=CHILL
    fi
    RUN_DIR="$CHILL_DIR/run" CONFIG="$CHILL_DIR/run/config.yaml"
    return 0
  fi
  valid_id "$_selected" || return 1
  active_kind=$(profile_field "$_selected" kind) || return 1
  case "$active_kind" in
    node_url) SUB_URL=$(profile_field "$_selected" url) || return 1 ;;
    full_url|local_yaml) ;;
    *) return 1 ;;
  esac
  active_name=$(profile_field "$_selected" name) || return 1
  active_id=$_selected
  runtime_home="$CHILL_DIR/profiles/$active_id"
  RUN_DIR="$runtime_home/run" CONFIG="$runtime_home/run/config.yaml"
}

prepare_profile_home() {
  [ "$active_id" ] || return 0
  (umask 077; mkdir -p "$RUN_DIR" "$runtime_home/providers" "$runtime_home/ruleset") || return 1
  if [ -d "$CHILL_DIR/ui" ] && [ ! -e "$runtime_home/ui" ]; then
    ln -s "$CHILL_DIR/ui" "$runtime_home/ui" || return 1
  fi
  if [ "$active_kind" = node_url ]; then
    for rule_file in "$CHILL_DIR"/ruleset/*; do
      [ -f "$rule_file" ] || continue
      rule_target="$runtime_home/ruleset/${rule_file##*/}"
      [ -e "$rule_target" ] || (umask 077; cp "$rule_file" "$rule_target") || return 1
    done
  fi
}

# Full-profile source and runtime are prepared by Core. Run the exact pinned
# executable against the candidate, keeping Mihomo's potentially sensitive
# diagnostics out of logs and API responses.
config_test() {
  if ! "$BIN" -t -d "$runtime_home" -f "$1" >/dev/null 2>&1; then
    echo 'mihomo_config_invalid' >&2
    return 1
  fi
}

validate_profile() {
  select_profile "$1" && [ "$active_kind" ] && prepare_profile_home || return 1
  case "$active_kind" in
    full_url|local_yaml) config_test "${2:-$CONFIG}" ;;
    *) render ;;
  esac
}

profile_options() {
  case "$active_kind" in
    full_url|local_yaml) grep -Eq '^ipv6:[[:space:]]*true[[:space:]]*$' "${1:-$CONFIG}" ;;
    *) return 1 ;;
  esac
}

public_ipv6() {
  _wan6=$(ip -6 route show default 2>/dev/null | awk '{for (i=1;i<NF;i++) if ($i=="dev") {print $(i+1); exit}}')
  [ -n "$_wan6" ] && ip -6 addr show dev "$_wan6" scope global 2>/dev/null | grep -Eq 'inet6 [23][0-9a-fA-F]*:'
}

profile_ready() {
  [ "$(jsonfilter -i "$STATE" -e '@.state' 2>/dev/null)" = running ] &&
    [ "$(jsonfilter -i "$STATE" -e '@.profile_id' 2>/dev/null)" = "$1" ] &&
    _ready_pid=$(jsonfilter -i "$STATE" -e '@.core_pid' 2>/dev/null) &&
    [ "$_ready_pid" != "$2" ] && uint "$_ready_pid" && [ "$_ready_pid" -gt 0 ] &&
    kill -0 "$_ready_pid" 2>/dev/null && api_alive 1 && dns_answers && tun_present &&
    [ "$(api_mode)" = "$(saved_mode)" ]
}

wait_profile() {
  local now deadline
  now=$(boot_now) || return 1
  deadline=$((now + 55))
  while [ "$now" -lt "$deadline" ]; do
    profile_ready "$1" "$2" && return 0
    case "$(jsonfilter -i "$STATE" -e '@.reason' 2>/dev/null)" in
      ipv6_unavailable|profile_not_ready|render_failed|profile_switch_failed)
        [ "$(jsonfilter -i "$STATE" -e '@.profile_id' 2>/dev/null)" != "$1" ] || return 1 ;;
    esac
    sleep 1
    now=$(boot_now) || return 1
  done
  return 1
}

# The journal is written before replacing either file or the marker. Recovery
# uses copies until the journal is removed, so a second interruption is safe.
profile_rollback() {
  _journal="$CHILL_DIR/active-profile.prev"
  [ -f "$_journal" ] || return 0
  {
    IFS= read -r rollback_id && IFS= read -r _rollback_target &&
      IFS= read -r _rollback_mode && IFS= read -r _rollback_refresh
  } < "$_journal" || return 1
  valid_id "$_rollback_target" || return 1
  if [ "$_rollback_refresh" = 1 ]; then
    _rdir="$CHILL_DIR/profiles/$_rollback_target"
    for _file in source.yaml run/config.yaml; do
      cp -p "$_rdir/$_file.prev" "$_rdir/$_file.restore" &&
        mv -f "$_rdir/$_file.restore" "$_rdir/$_file" || return 1
    done
  fi
  if [ "$rollback_id" = '-' ]; then
    rm -f "$CHILL_DIR/active-profile"
    rollback_id=''
  else
    valid_id "$rollback_id" && write_marker "$CHILL_DIR/active-profile" "$rollback_id" || return 1
  fi
  case "$_rollback_mode" in
    rule|global|direct) write_marker "$MODE_FILE" "$_rollback_mode" || return 1 ;;
    *) rm -f "$MODE_FILE" ;;
  esac
  sync
  rm -f "$_journal"
  sync
  if [ "$_rollback_refresh" = 1 ]; then
    rm -f "$_rdir/source.yaml.prev" "$_rdir/run/config.yaml.prev"
  fi
}

supervisor_alive() {
  IFS= read -r _supervisor 2>/dev/null < "$SUP_PID" && uint "$_supervisor" &&
    [ "$_supervisor" -gt 1 ] && kill -0 "$_supervisor" 2>/dev/null
}

profile_switch() ( # <id> [refresh], invoked under the profile lock
  target=$1 refresh=${2:-0}
  profile_rollback || { echo 'profile_recovery_failed' >&2; return 1; }
  select_profile "$target" && prepare_profile_home || { echo 'profile_missing' >&2; return 1; }
  if [ "$refresh" = 1 ]; then
    candidate="$RUN_DIR/config.next.yaml"
    [ -f "$runtime_home/source.next.yaml" ] || return 1
    config_test "$candidate" || return 1
  else
    candidate=$CONFIG
    validate_profile "$target" || return 1
  fi

  old_id=$(marker_id)
  # Refreshing an inactive profile does not change the active marker or service.
  activate=1
  if [ "$refresh" = 1 ] && [ "$old_id" != "$target" ]; then activate=0; fi
  if [ "$activate" = 1 ] && profile_options "$candidate" && ! public_ipv6; then
    echo 'ipv6_unavailable' >&2
    return 1
  fi
  old_mode=$(api_mode)
  case "$old_mode" in rule|global|direct) ;; *) old_mode=$(cat "$MODE_FILE" 2>/dev/null) ;; esac
  live=0
  if [ "$activate" = 1 ] && [ ! -f "$DISABLED" ] && supervisor_alive; then live=1; fi
  old_pid=$(cat "$CORE_PID" 2>/dev/null)
  if [ "$refresh" = 1 ]; then
    cp -p "$CONFIG" "$CONFIG.prev" && cp -p "$runtime_home/source.yaml" "$runtime_home/source.yaml.prev" || return 1
  fi
  journal=$(printf '%s\n%s\n%s\n%s' "${old_id:--}" "$target" "${old_mode:--}" "$refresh")
  write_marker "$CHILL_DIR/active-profile.prev" "$journal" || return 1
  sync

  if { [ "$refresh" = 0 ] || { mv -f "$candidate" "$CONFIG" && mv -f "$runtime_home/source.next.yaml" "$runtime_home/source.yaml"; }; } &&
     { [ "$activate" = 0 ] || write_marker "$CHILL_DIR/active-profile" "$target"; } &&
     { [ -z "$old_mode" ] || write_marker "$MODE_FILE" "$old_mode"; }; then
    sync
    if [ "$live" = 0 ] || { rm -f "$CHILL_DIR/profile-switch-failed" "$GAVEUP" "$STATE"; : > "$RESTART_FLAG"; wait_profile "$target" "$old_pid"; }; then
      rm -f "$CHILL_DIR/active-profile.prev"
      sync
      [ "$refresh" = 0 ] || rm -f "$CONFIG.prev" "$runtime_home/source.yaml.prev"
      if [ "$activate" = 1 ] && [ "$live" = 0 ]; then mark_stopped; fi
      return 0
    fi
  fi

  failed_pid=$(cat "$CORE_PID" 2>/dev/null)
  if ! profile_rollback; then
    : > "$CHILL_DIR/profile-switch-failed"
    : > "$RESTART_FLAG"
    echo 'profile_recovery_failed' >&2
    return 1
  fi
  if [ "$live" = 1 ]; then
    rm -f "$GAVEUP" "$STATE"
    : > "$RESTART_FLAG"
    if [ -n "$old_pid" ] && ! wait_profile "$rollback_id" "$failed_pid"; then
      : > "$CHILL_DIR/profile-switch-failed"
      : > "$RESTART_FLAG"
      for _attempt in $(seq 1 12); do
        [ "$(jsonfilter -i "$STATE" -e '@.reason' 2>/dev/null)" = profile_switch_failed ] && break
        sleep 1
      done
      echo 'profile_switch_failed' >&2
      return 1
    fi
  fi
  echo 'profile_activation_failed_rolled_back' >&2
  return 1
)

profile_lock() (
  umask 077
  flock -n -x 9 || { echo 'profile_busy' >&2; return 1; }
  "$@"
) 9>"$CHILL_DIR/profile.lock"

recover_interrupted_profile() {
  [ -f "$CHILL_DIR/active-profile.prev" ] || return 1
  profile_lock profile_rollback >/dev/null 2>&1 || return 1
  log 'profile: restored interrupted activation'
  : > "$RESTART_FLAG"
}

validate_template() (
  select_profile '' || return 1
  TEMPLATE=$1
  [ -f "$TEMPLATE" ] || return 1
  SUB_URL=${legacy_url:-}
  SUB_URL=${SUB_URL:-https://example.invalid/provider}
  runtime_home=$(mktemp -d /tmp/chill-validate.XXXXXX) || return 1
  trap 'rm -rf "$runtime_home"' EXIT
  RUN_DIR="$runtime_home/run" CONFIG="$RUN_DIR/config.yaml"
  render
)

validate_active() {
  select_profile || return 1
  case "$active_kind" in
    full_url|local_yaml|node_url) config_test "$CONFIG" ;;
    legacy) validate_template "$TEMPLATE" ;;
    '') return 0 ;;
  esac
}

refresh_node_cache() (
  local scratch probe pid nodes
  scratch=$(mktemp -d "$RUN_DIR/provider.XXXXXX") || return 1
  pid=''
  trap '[ -z "$pid" ] || { kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null; }; rm -rf "$scratch"' EXIT
  # Test the provider with the same pinned parser used during a real start.
  # This instance has no listeners besides its private Unix control socket.
  curl -fsSL --proto '=http,https' --proto-redir '=http,https' --max-redirs 5 \
    --max-time 20 --max-filesize 4194304 -o "$scratch/sub.yaml" -- "$SUB_URL" 2>/dev/null || return 1
  cat > "$scratch/probe.yaml" <<'EOF'
ipv6: false
proxy-providers:
  sub:
    type: file
    path: ./sub.yaml
    override: {interface-name: "", routing-mark: 0}
EOF
  probe="$scratch/api.sock"
  "$BIN" -d "$scratch" -f "$scratch/probe.yaml" -ext-ctl-unix "$probe" 9>&- >/dev/null 2>&1 &
  pid=$!
  for _attempt in $(seq 1 8); do
    kill -0 "$pid" 2>/dev/null || return 1
    nodes=$(curl -fsS -m 1 --unix-socket "$probe" http://localhost/providers/proxies/sub 2>/dev/null) || nodes=''
    if printf '%s' "$nodes" | jsonfilter -e '@.proxies[0].name' >/dev/null; then
      mv -f "$scratch/sub.yaml" "$runtime_home/providers/sub.yaml"
      return $?
    fi
    sleep 1
  done
  return 1
)

profile_refresh() {
  select_profile "$1" && prepare_profile_home || return 1
  [ "$active_kind" = node_url ] || { profile_switch "$1" 1; return $?; }
  if [ "$(marker_id)" = "$1" ] && api_alive 1; then
    # A successful PUT can also be an unchanged subscription; verify it still
    # has nodes in that case instead of requiring the timestamp to move.
    if curl -fsS -m 25 -X PUT -o /dev/null -H "Authorization: Bearer ${CHILL_SECRET:-}" \
      http://127.0.0.1:9999/providers/proxies/sub 2>/dev/null && providers_ready; then
      return 0
    fi
  elif refresh_node_cache; then
    return 0
  fi
  echo 'provider_refresh_failed' >&2
  return 1
}

# 直连用的 DNS 里补上运营商的 IPv4 DNS，输出 `, "a.b.c.d", ...`（没有就输出空）。
# 原因（10-03 真机）：原厂把 /etc/resolv.conf 改写成只有两个 IPv6 DNS，而 CHILL
# 是 ipv6: false，mihomo 连不了 v6 的 DNS，direct-nameserver 只有 system:// 时
# 所有走 DIRECT、又不在 fake-ip-filter 里的域名（Apple 一整组：App Store 搜索/
# 更新、推送）都报「ip version error」连不上。运营商的 v4 DNS 其实还在
# resolv.conf.auto 里，只是没进 /etc/resolv.conf。
direct_dns4() {
  cat "$RESOLV_AUTO" "$RESOLV_SYS" 2>/dev/null | awk '
    $1 == "nameserver" && $2 ~ /^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$/ && !seen[$2]++ {
      printf ", \"%s\"", $2; n++; if (n >= 4) exit
    }'
}

# ── 国内直连：nft 集合代替 route-exclude-address 里的 CN 段（CHILL_CN_SET=1）─────────
# 原来把 cn.list 全部写进 route-exclude-address，table 2022 有 1.2 万多条路由。sing-tun 的
# 默认网口监视器每次检查都把所有路由表整个读一遍，而开机那一波路由会撑爆它的 netlink
# 订阅、之后每秒检查一次——空载时 mihomo 77% 的 CPU 花在这里（10-07 剖析）。
# 改成一张 nft 区间集合 + 一条 fwmark 规则，语义不变：局域网发往 CN 段的包走 main 表直连。
#   - 单独一张表 inet chillcn：rebuild_api_guard 会删 inet chill；fw3/原厂只动 legacy
#     iptables，碰不到 nft 表，所以不用定期自检。
#   - 优先级 -140，排在 legacy mangle（-150）后面、用 or 打标：/etc 里原厂有不带掩码的
#     set-mark 0xC0，排在它前面的话我们这一位会被覆盖。
#   - 只做 IPv4：局域网 IPv6 由 luyaz-core 关着（/data/luyaz/scripts/lan-ipv6-off），v6 的 table 2022 本来就是空的。
#   - auto-merge：cn.list 有相邻/重叠段，不合并 nft 会拒绝（实测 6204 条 → 3300 个区间）。
# 先删后建、不在一个批次里「建-删-建」：nft 1.1 上那样整批报 ENOENT。中间一瞬间 CN 流量
# 进 TUN，mihomo 的 GEOIP,CN 规则照样让它直连。
cn_nft_file() { # 输出 nft 脚本；元素按文件读，不拼 shell 变量（163 KB 会撞 argv 上限）
  awk -v mark="$CN_MARK" '
    BEGIN { n = 0
      print "table inet chillcn {"
      print "  set cn4 {"
      print "    type ipv4_addr; flags interval; auto-merge;"
      printf "    elements = { " }
    /:/ || /^[ \t]*$/ { next }
    { if (n > 0) printf ", "; printf "%s", $1; n++ }
    END {
      print " }"
      print "  }"
      print "  chain pre {"
      print "    type filter hook prerouting priority -140; policy accept;"
      print "    iifname \"br-lan\" ip daddr @cn4 counter meta mark set meta mark or " mark
      print "  }"
      print "}"
      if (n == 0) exit 1
    }' "$CN_LIST"
}

# 核心启动前调用（flush 之后）。成功：留下 CN_SET_MARK，cn_set=nft；
# 没开：cn_set=routes；失败：清掉半成品，cn_set=failed，render 照旧写 CN 段。
cn_set_up() {
  rm -f "$CN_SET_MARK"
  cn_set=routes
  [ "$CHILL_CN_SET" = "1" ] || return 0
  nft delete table inet chillcn 2>/dev/null || true
  ip rule del priority "$CN_PRIO" 2>/dev/null || true
  if cn_nft_file > "$CN_NFT" 2>/dev/null \
    && nft -f "$CN_NFT" 2>/dev/null \
    && ip rule add fwmark "$CN_MARK/$CN_MARK" priority "$CN_PRIO" lookup main 2>/dev/null; then
    : > "$CN_SET_MARK"
    cn_set=nft
    log "cn_set: 国内直连改由 nft 集合 + 规则 ${CN_PRIO}"
  else
    nft delete table inet chillcn 2>/dev/null || true
    ip rule del priority "$CN_PRIO" 2>/dev/null || true
    cn_set=failed
    log "cn_set: nft 集合没建成，退回把 CN 段写进 route-exclude-address"
  fi
  rm -f "$CN_NFT"
}

# 订阅不许指定出口网卡（ext-1030 第 14 条）：proxy-providers 里的节点一律不带
# interface-name / routing-mark。出站走哪张网卡由 tun 的 auto-detect-interface 决定；
# 订阅里写了 interface-name: rmnet_data0 会把连接固定到旧网卡，换网时无法随默认路由切换。
# 两层做法：
#   1) 渲染结果里 proxy-providers 段（含 inline 的 payload）出现的这两个键直接删掉；
#   2) 每个 provider 的 override 补上 interface-name: "" 和 routing-mark: 0，
#      mihomo 下载回来的订阅正文（不经过这里）也会被清空。
# 顶层 proxies: 为模板作者明确指定，不动。
# 只认块写法 `    override:` 和单行 `{…}` 两种；列表项第一个键（`- interface-name:`）不处理。
strip_egress() {
  awk '
    function strip(s) {
      gsub(/,[ \t]*(interface-name|routing-mark):[ \t]*[^,}]*/, "", s)
      gsub(/(interface-name|routing-mark):[ \t]*[^,}]*,[ \t]*/, "", s)
      gsub(/(interface-name|routing-mark):[ \t]*[^,}]*/, "", s)
      return s
    }
    function close_prov() {
      if (prov && !ovr) print "    override: {interface-name: \"\", routing-mark: 0}"
      prov = 0; ovr = 0
    }
    /^[^ \t#]/ { if (insec) close_prov(); insec = ($0 ~ /^proxy-providers:/); print; next }
    !insec { print; next }
    /^[ \t]*(#|$)/ { print; next }
    /^  [^ \t#-][^:]*:[ \t]*$/ { close_prov(); prov = 1; print; next }
    prov && /^    override:[ \t]*$/ {
      print; print "      interface-name: \"\""; print "      routing-mark: 0"; ovr = 1; next
    }
    prov && /^    override:[ \t]*\{/ {
      line = strip($0); sub(/\{[ \t]*/, "{interface-name: \"\", routing-mark: 0, ", line)
      sub(/, *\}/, "}", line); print line; ovr = 1; next
    }
    /^[ \t]+(interface-name|routing-mark):/ { next }
    { print strip($0) }
    END { if (insec) close_prov() }
  ' "$1" > "$1.eg" && mv "$1.eg" "$1"
}

# 起核心前看它要用的端口和 TUN 名是不是已经被别的程序占了（ext-1030 第 16 条）。
# 自己上一份核心在这之前已经被 flush / enter_direct 停掉，所以这里看到的都算别人的。
# 被占时核心起来也只会立刻退出，那不是崩溃：不进崩溃计数、不 gaveup，停在直连等它让出来。
# 端口：external-controller 9999、dns.listen 53、两个 netinfo 入口 7894/7895；只看监听
# （TCP 0A、UDP 07），地址只认 127.0.0.1 / 0.0.0.0 / :: / ::ffff:127.0.0.1——dnsmasq 绑在
# 192.168.0.1:53 上不算冲突。CHILL_API_LAN=1 时 9999 绑 0.0.0.0，任何地址上的 9999 都算。
# 输出第一个被占的（tcp:9999 / udp:53 / tun:chill0），没有被占返回 1。
TUN_DEV=chill0
core_slots_busy() {
  if [ -e "$SYS_NET/$TUN_DEV" ]; then echo "tun:$TUN_DEV"; return 0; fi
  for _f in tcp tcp6 udp udp6; do
    [ -r "$CHILL_PROC/net/$_f" ] || continue
    awk -v proto="${_f%6}" -v lan="$CHILL_API_LAN" '
      NR == 1 { next }
      {
        split($2, a, ":"); addr = toupper(a[1]); port = toupper(a[2])
        if (proto == "tcp") { if ($4 != "0A") next } else { if ($4 != "07") next }
        if (proto == "tcp") { if (port != "270F" && port != "1ED6" && port != "1ED7") next }
        else { if (port != "0035") next }
        # 0.0.0.0 和 :: 都是全 0；::ffff:127.0.0.1 是 0…0FFFF0000 + 0100007F
        mine = 0
        if (addr == "0100007F" || addr ~ /^0+$/) mine = 1
        if (addr ~ /^0+FFFF00000100007F$/) mine = 1
        if (lan == "1" && port == "270F") mine = 1
        if (!mine) next
        n["270F"] = 9999; n["1ED6"] = 7894; n["1ED7"] = 7895; n["0035"] = 53
        print proto ":" n[port]; found = 1; exit
      }
      END { exit (found ? 0 : 1) }' "$CHILL_PROC/net/$_f" && return 0
  done
  return 1
}

# 被占：进直连并写清原因（port_busy / tun_busy），返回 0 让 run 跳过这一轮。
# 先等 CHILL_BUSY_RECHECK 秒再看一次：刚停掉的核心偶尔还没把 TUN 收回。
CHILL_BUSY_RECHECK="${CHILL_BUSY_RECHECK:-2}"
start_blocked() {
  _busy=$(core_slots_busy) || return 1
  sleep "$CHILL_BUSY_RECHECK"
  _busy=$(core_slots_busy) || return 1
  log "起核心前发现被占：${_busy}（别的程序在用，不算崩溃）"
  case "$_busy" in tun:*) enter_direct tun_busy ;; *) enter_direct port_busy ;; esac
  return 0
}

render() {
  case "$active_kind" in
    full_url|local_yaml)
      profile_vars "$(profile_effective)"
      [ -s "$CONFIG" ] && config_test "$CONFIG"
      return $? ;;
  esac
  umask 077
  mkdir -p "$RUN_DIR"
  [ -f "$TEMPLATE" ] || { log "render: 缺 template.yaml"; return 1; }
  [ -f "$CN_LIST" ] || { log "render: 缺 cn.list"; return 1; }

  # 地区 filter 必须在这里生成。忘了生成的话它们会是空串，四个地区组匹配不到任何
  # 节点，命中 empty-fallback: REJECT，表现为「代理全挂但配置校验通过」。
  build_filters || return 1

  if [ "$CHILL_API_LAN" = "1" ]; then CONTROLLER="0.0.0.0:9999"; else CONTROLLER="127.0.0.1:9999"; fi
  # 不在这里把 cn.list（9646 行，约 163KB）拼成一个 shell 变量：那样等下要把它
  # 整个塞进 awk 的 VAR=value 前缀，会撞上 exec 的 argv/envp 大小限制。
  # 2026-09-16 真机实测：ash 直接报 "Argument list too long"，render() 提前
  # 失败，chill.sh 于是一直卡在 render_failed 降级循环——这台设备第一次真正
  # 装订阅后才暴露，本地/干跑测试用的都是小样本 CN 列表，没有踩到这个门槛。
  # 改为让 awk 自己按文件路径读取并拼接，全程走文件 I/O，不受该限制。

  # snap1/snap2 已改为 type:file 静态快照（2026-09-16 核对实际数据后的修正，见
  # template.yaml 里的注释），不再需要 URL；SUB_URL 仍是唯一的 type:http。
  # CHILL_SECRET 同样会落进单引号 YAML 标量（template.yaml 的 secret: 字段），
  # 校验规则跟 SUB_URL 一样。
  for v in SUB_URL CHILL_SECRET; do
    eval "val=\${$v:-}"
    case "$val" in
      *"'"*|*"
"*) log "render: ${v} 含单引号或换行，拒绝渲染"; return 1 ;;
    esac
  done

  # ⚠ 这里必须显式把 SUB_URL 传进 awk 子进程的环境，不能只做上面的校验就
  # 以为完了。chill.env 是普通变量赋值（不带 export），source 进来的变量不会
  # 自动出现在子进程的 ENVIRON 里；先前这里漏了这一项，导致 template.yaml 里
  # 的 ${SUB_URL} 永远不会被替换，会原样留在渲染结果里传给 mihomo。
  profile_vars "$(profile_effective)"
  DIRECT_DNS_EXTRA=$(direct_dns4)
  # Managed profiles keep CN routing in Mihomo, so GLOBAL really is global.
  if [ "$active_kind" = node_url ] || [ -f "$CN_SET_MARK" ]; then _cn_routes=0; else _cn_routes=1; fi
  HC_INTERVAL="$HC_INTERVAL" KEEPALIVE="$KEEPALIVE" TCP_CONCURRENT="$TCP_CONCURRENT" \
  CONTROLLER="$CONTROLLER" SUB_URL="${SUB_URL:-}" \
  CHILL_SECRET="${CHILL_SECRET:-}" DIRECT_DNS_EXTRA="$DIRECT_DNS_EXTRA" \
  FILTER_UNION="${FILTER_UNION:-}" EXCLUDE_FILTER="${EXCLUDE_FILTER:-}" \
  FILTER_TW="${FILTER_TW:-}" FILTER_JP="${FILTER_JP:-}" \
  FILTER_SG="${FILTER_SG:-}" FILTER_US="${FILTER_US:-}" \
  awk -v cnlist="$CN_LIST" -v cnroutes="$_cn_routes" -v managed="$active_kind" '
    # CN_CIDR 单独处理：值有 163KB 量级，不能走 ENVIRON（会撞 argv/envp 大小
    # 限制，见上面的中文注释）。-v 传的只是文件路径（几十字节），BEGIN 里
    # 自己按行读文件拼接，全程文件 I/O，没有大小限制。
    BEGIN {
      # 2026-09-16 真机实测：三元运算符跟字符串拼接混用在这台设备的 awk 实现
      # 里解析有问题——cn_cidr = (cond) ? a : b c 这种写法最终只会得到 "0"
      # （9647 行全部读到了，拼接结果却只有 1 个字符），懒得深究是不是运算符
      # 优先级的坑，改成显式 if/else 更稳妥，不依赖三元表达式在这个 awk 实现
      # 里的具体行为。
      # cnroutes=0：CN 段已由 nft 集合绕开（cn_set_up），这里留空。模板里 ${CN_CIDR} 后面
      # 不带逗号，有值时由这里补上，所以留空也不会渲染出「[,」。
      cn_cidr = ""; first = 1
      if (cnroutes == 1) {
        while ((getline cline < cnlist) > 0) {
          if (cline == "") continue
          if (first) { cn_cidr = cline; first = 0 }
          else { cn_cidr = cn_cidr ", " cline }
        }
        close(cnlist)
        if (!first) cn_cidr = cn_cidr ","
      }
    }
    /^    filter:.*FILTER_UNION/ && managed == "node_url" { next }
    /^  - MATCH,/ && managed == "node_url" { print "  - RULE-SET,cn_ip,DIRECT,no-resolve" }
    managed == "node_url" && /name: 🎯 手动节点,/ { sub(/, filter: '\''\$\{FILTER_UNION\}'\''/, "") }
    managed == "node_url" && /name: 🚀 节点选择,/ {
      sub(/proxies: \[/, "proxies: [🎯 手动节点, "); sub(/, 🎯 手动节点, DIRECT/, ", DIRECT")
    }
    {
      line = $0
      result = ""
      while (match(line, /\$\{[A-Z_]+\}/)) {
        key = substr(line, RSTART + 2, RLENGTH - 3)
        # 同上：这台设备的 awk 三元运算符不可靠，这里也改用显式 if/else。
        if (key == "CN_CIDR") { val = cn_cidr } else { val = ENVIRON[key] }
        result = result substr(line, 1, RSTART - 1) val
        line = substr(line, RSTART + RLENGTH)
      }
      print result line
    }' "$TEMPLATE" > "$CONFIG.tmp" || return 1
  strip_egress "$CONFIG.tmp" || { rm -f "$CONFIG.tmp"; return 1; }

  # 绝对路径 -f：验证从任何工作目录都能通过
  if config_test "$CONFIG.tmp"; then
    mv "$CONFIG.tmp" "$CONFIG"; chmod 600 "$CONFIG"
    RENDERED_DNS_EXTRA="$DIRECT_DNS_EXTRA"
    return 0
  fi
  log 'render: mihomo -t 未通过，保留旧配置'
  rm -f "$CONFIG.tmp"
  return 1
}

# ── 前置检查 ────────────────────────────────────────────────────────────────
rulesets_ok() {
  for f in cn_domain.mrs private_domain.mrs ai_domain.list; do
    [ -s "$CHILL_DIR/ruleset/$f" ] || return 1
  done
  return 0
}
wan_is_cellular() {
  ip route show default 2>/dev/null | grep -q 'dev rmnet_data0'
}

# ── 运行中的规则自愈 ────────────────────────────────────────────────────────
# mihomo 只在 TUN 启动时建 9000 段 ip rule 和 table 2022，被别人删了（重拨、原厂
# 重刷路由）不会自己补；check_rules_drift 故意不看自管区间，这里单独看。
# 不写死 sing-tun 规则的样子：核心起来后把自管区间（含 8998）照原样记下来，
# 之后每分钟比一次「快照里有、现在没有」。只看 v4：局域网 IPv6 关着。
# 绕行 8999 不算：网页随时增删（luyaz-core bypass_set），删一条不该重启核心；被外部删了下次启动补。
owned_rules() {
  ip rule show 2>/dev/null | awk -F: -v lo="$RULE_LO" -v hi="$RULE_HI" -v by="$BYPASS_PRIO" '$1+0>=lo && $1+0<=hi && $1+0!=by'
}

tun_present() {
  owned_rules | grep -q "lookup $TABLE" && ip route show table "$TABLE" 2>/dev/null | grep -q 'dev chill0'
}

tun_v6_ready() {
  ip -6 rule show 2>/dev/null | grep -q "lookup $TABLE" &&
    ip -6 route show table "$TABLE" 2>/dev/null | grep -q 'dev chill0' &&
    curl -6 -fsS -m 5 https://one.one.one.one/cdn-cgi/trace >/dev/null 2>&1
}

controller_get() {
  curl -fsS -m 3 -H "Authorization: Bearer ${CHILL_SECRET:-}" "http://127.0.0.1:9999/$1" 2>/dev/null
}

providers_ready() (
  proxies=$(controller_get providers/proxies) && rule_sets=$(controller_get providers/rules) || return 1
  proxy_entries=$(printf '%s' "$proxies" | jsonfilter -e '@.providers.*')
  if [ -n "$proxy_entries" ]; then
    printf '%s\n' "$proxy_entries" | while IFS= read -r entry; do
      kind=$(printf '%s' "$entry" | jsonfilter -e '@.vehicleType')
      [ "$kind" = Compatible ] || printf '%s' "$entry" | jsonfilter -e '@.proxies[0].name' >/dev/null || exit 1
    done || return 1
  fi
  counts=$(printf '%s' "$rule_sets" | jsonfilter -e '@.providers.*.ruleCount')
  for count in $counts; do uint "$count" && [ "$count" -gt 0 ] || return 1; done
  [ "$active_kind" != node_url ] || printf '%s' "$proxies" | jsonfilter -e '@.providers.sub.proxies[0].name' >/dev/null
)

wait_gateway_ready() {
  for _gate_attempt in $(seq 1 15); do
    kill -0 "$1" 2>/dev/null || return 1
    tun_present && { [ -z "$active_id" ] || providers_ready; } && return 0
    sleep 1
  done
  return 1
}
# 返回 1 = 缺了（快照里的规则不见了，或 table 2022 里没有 chill0 的路由）。
# 还没有像样的快照（TUN 规则刚起来）就现在记，这一轮算好。
tun_rules_ok() {
  if ! grep -q "lookup $TABLE" "$TUN_SNAP" 2>/dev/null; then
    owned_rules > "$TUN_SNAP"
    return 0
  fi
  owned_rules > /tmp/chill.tun.now
  _lost=$(missing_rules "$TUN_SNAP" /tmp/chill.tun.now)
  rm -f /tmp/chill.tun.now
  if [ -n "$_lost" ]; then
    log "TUN 规则不见了："
    printf '%s\n' "$_lost" | head -5 >> "$LOG" 2>/dev/null
    return 1
  fi
  ip route show table "$TABLE" 2>/dev/null | grep -q 'dev chill0' && return 0
  log "table $TABLE 里没有 chill0 的路由了"
  return 1
}
# 规则丢了：平稳重启核心重建（结构性修复，不算崩溃）。10 分钟内超过 HEAL_MAX 次
# 就回直连、走退避，免得每分钟重启一次。返回后调用方要 break。
tun_heal() {
  _hn=$(boot_now) || _hn=0
  if [ "$heal_t0" -lt 0 ] || [ "$_hn" -lt "$heal_t0" ] || [ $((_hn - heal_t0)) -gt "$CRASH_WINDOW" ]; then
    heal_n=0; heal_t0=$_hn
  fi
  heal_n=$((heal_n + 1))
  if [ "$heal_n" -gt "$HEAL_MAX" ]; then
    log "10 分钟内第 ${heal_n} 次规则丢失，回直连退避"
    enter_direct rules_lost
    direct_since=$_hn
    next_backoff
    return 0
  fi
  event "rules_lost -> restart"
  restart_core "$(profile_effective)"
}
# 运行中看 WAN（启动前那次 wan_is_cellular 之外）。输出 ok / none / other：
# none = 没有默认路由（蜂窝断了或正在重拨）：核心留着，回来接着用；
# other = 默认路由走了别的口（连上了别的 WAN）：调用方连续两次才回直连。
wan_now() {
  _w=$(ip route show default 2>/dev/null)
  if [ -z "$_w" ]; then echo none
  elif printf '%s\n' "$_w" | grep -q 'dev rmnet_data0'; then echo ok
  else echo other
  fi
}

# 直连状态：装 unreachable 路由，让持有 fake-ip 的客户端立刻失败重解析，
# 而不是静默超时等到 TTL 过期。
enter_direct() {
  reason="$1"
  flush
  [ "$reason" != no_profile ] || { write_state direct "$reason"; return; }
  if [ "$reason" = ipv6_unavailable ]; then ipv6_mode=unavailable; fi
  ip route add unreachable "$FAKE_IP_NET" 2>/dev/null || true
  event "$reason -> direct"
  write_state direct "$reason"
  log "进入直连：$reason"
}

# ── 崩溃计数 / 退避 / 日志截断（提取出来是为了能被单测直接调用）───────────────

# 统一两条崩溃路径的计数。此前「启动即退出」那条没有窗口重置，累计 3 次就永久
# gaveup，哪怕两次间隔几小时——那不是设计要的「10 分钟内 3 次」。
# 返回 0 表示还可以再试，返回 1 表示已达上限、应进入 gaveup。
note_crash() {
  now=$(boot_now) || return 1
  if [ "$crash_t0" -lt 0 ] || [ "$now" -lt "$crash_t0" ] || [ $((now - crash_t0)) -gt "$CRASH_WINDOW" ]; then
    crash_n=0; crash_t0=$now
  fi
  crash_n=$((crash_n + 1))
  log "崩溃计数：窗口内第 ${crash_n} 次（上限 ${CRASH_MAX}）"
  [ "$crash_n" -lt "$CRASH_MAX" ]
}

# 连续降级时退避 2 / 5 / 15 分钟，避免反复重建 TUN 打断连接。
# 每档都 >= DWELL_SECONDS，所以「进入直连后至少驻留 120 秒再评估恢复」这条
# 由退避本身保证，不需要另设计时器。
next_backoff() {
  degrade_n=$((degrade_n + 1))
  case "$degrade_n" in
    1) _w=120 ;;
    2) _w=300 ;;
    *) _w=900 ;;
  esac
  [ "$_w" -lt "$DWELL_SECONDS" ] && _w="$DWELL_SECONDS"
  _bn=$(boot_now) || { backoff=unknown; return 1; }
  backoff=$((_bn + _w))
  log "降级第 ${degrade_n} 次，退避 ${_w} 秒"
}

# 平稳重启核心：记下模式、停核、清理，回到外层循环按新档位重新起（不计崩溃、不退避）。
restart_core() {
  save_mode
  event "restart -> $1"
  log "按档位 $1 重启核心"
  kill_core
  flush
}

# 温度降档（CHILL_TEMP_WARM 设了才有）。返回 0 = 已经重启核心，调用方要 break。
# core 温控的请求读进 h（hot/critical/空）。只用内建 read，每个 tick 不多开进程。
heat_req() {
  h=
  [ -f "$HEAT_REQ" ] && read -r h < "$HEAT_REQ"
  return 0
}

# $1 = 本机最高温（毫摄氏度），$2 = core 温控的请求（hot/critical/空）。
# 降档的理由有两个：自己的 CHILL_TEMP_WARM（默认不设）或 core 说热；两个都没了、再凉够 COOL_HOLD 才回档。
thermal_step() {
  case "${2:-}" in hot|critical) req=1 ;; *) req=0 ;; esac
  [ -n "$CHILL_TEMP_WARM" ] || [ "$req" = 1 ] || [ -f "$THERMAL_ECO" ] || return 1
  if [ ! -f "$THERMAL_ECO" ]; then
    cool_since=-1
    [ "$(profile_read)" = eco ] && return 1
    if [ "$req" = 0 ]; then
      [ "$1" -ge "$CHILL_TEMP_WARM" ] || return 1
    fi
    : > "$THERMAL_ECO"
    if [ "$req" = 1 ]; then event "warm core=$2 cpuss=$(( $1 / 1000 )) -> eco"
    else event "warm cpuss=$(( $1 / 1000 )) -> eco"; fi
    restart_core eco
    return 0
  fi
  if [ "$req" = 1 ]; then cool_since=-1; return 1; fi
  if [ -n "$CHILL_TEMP_WARM" ] && [ "$1" -ge $((CHILL_TEMP_WARM - TEMP_RECOVER_DELTA)) ]; then
    cool_since=-1; return 1
  fi
  now=$(boot_now) || return 1
  [ "${cool_since:--1}" -ge 0 ] && [ "$now" -ge "$cool_since" ] || { cool_since=$now; return 1; }
  [ $((now - cool_since)) -ge "$COOL_HOLD" ] || return 1
  rm -f "$THERMAL_ECO"; cool_since=-1
  event "cool cpuss=$(( $1 / 1000 )) -> $(profile_read)"
  restart_core "$(profile_read)"
  return 0
}

# §2 要求每 60 秒截断日志。不截断的话 /tmp 是内存盘，长期运行会把内存吃掉。
truncate_log() {
  [ -f "$LOG" ] || return 0
  sz=$(wc -c < "$LOG" 2>/dev/null || echo 0)
  [ "$sz" -le "$LOG_MAX_BYTES" ] && return 0
  # 原地覆盖，不 mv：mihomo 的输出是 >> 打开的，fd 一直指着这个 inode；换成新文件的话
  # 它接着往已删掉的旧文件里写，内存盘上的空间再也收不回来。O_APPEND 的写入总落在
  # 当前末尾，原地截短后它接着往新末尾写。tail 和 cat 之间写进来的几行会丢，和 mv 一样。
  tail -c "$((LOG_MAX_BYTES / 2))" "$LOG" > "$LOG.tmp" 2>/dev/null && cat "$LOG.tmp" > "$LOG" 2>/dev/null
  rm -f "$LOG.tmp"
}

# ── run：procd 实例命令 ─────────────────────────────────────────────────────
run() {
  trap 'log "收到 TERM/INT"; flush; rm -f "$SUP_PID"; exit 0' TERM INT
  flush
  # Only recover a journal whose writer no longer holds the lock. A live
  # profile job intentionally leaves it in place while the supervisor starts.
  profile_lock profile_rollback >/dev/null 2>&1 || true
  write_marker "$SUP_PID" "$$" || return 1
  ip rule show > "$RULES_BEFORE" 2>/dev/null
  ip -6 rule show >> "$RULES_BEFORE" 2>/dev/null

  started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  rules_drift=false; mem_pressure=false; bypass_stale_json='[]'
  crash_n=0; crash_t0=-1; backoff=0; direct_since=0; degrade_n=0; state_idle=0
  heal_n=0; heal_t0=-1

  while :; do
    recover_interrupted_profile || true
    if [ -f "$RESTART_FLAG" ]; then
      rm -f "$RESTART_FLAG"
      backoff=0; crash_n=0; crash_t0=-1
    fi
    # ---- 不该启动的情形，保持进程存活、停在直连 ----
    if ! select_profile; then enter_direct render_failed; sleep 5; continue; fi
    if [ -f "$DISABLED" ]; then enter_direct disabled; sleep 5; continue; fi
    if [ -f "$CHILL_DIR/profile-switch-failed" ]; then enter_direct profile_switch_failed; sleep 5; continue; fi
    if [ -z "$active_kind" ]; then
      if [ "$state_idle" != no_profile ]; then write_state direct no_profile; state_idle=no_profile; fi
      sleep 5; continue
    fi
    if [ -f "$GAVEUP" ];   then enter_direct gaveup;   sleep 5; continue; fi
    if manual_paused; then enter_direct paused; sleep 5; continue; fi
    [ -e "$PAUSE_UNTIL" ] && rm -f "$PAUSE_UNTIL"
    # 息屏暂停：只在进入时记一次（每 5 秒一行的日志没有用）
    if idle_paused; then
      [ "$state_idle" = 1 ] || { enter_direct idle; state_idle=1; }
      sleep 5; continue
    fi
    state_idle=0
    case "$active_kind" in
      node_url|legacy) if ! rulesets_ok; then enter_direct ruleset_missing; sleep 5; continue; fi ;;
    esac
    if ! wan_is_cellular; then enter_direct captive_wan; sleep 15; continue; fi
    if [ "$backoff" != 0 ]; then
      _rn=$(boot_now) || { sleep 5; continue; }
      [ "$backoff" = unknown ] && { next_backoff; sleep 5; continue; }
      [ "$_rn" -lt "$backoff" ] && { sleep 5; continue; }
      backoff=0
    fi
    # core 温控还要直连（critical）就不起核心：否则退避一到就起、一起又过热直连，反复断连接、反复发热
    heat_req
    if [ "$h" = critical ]; then sleep 15; continue; fi

    # ---- 启动核心 ----
    if start_blocked; then sleep 5; continue; fi
    cn_set=routes
    [ "$active_kind" != legacy ] || cn_set_up
    prepare_profile_home && render || { enter_direct render_failed; sleep 5; continue; }
    if profile_options; then
      if ! public_ipv6; then enter_direct ipv6_unavailable; sleep 5; continue; fi
      ipv6_mode=tun
    else
      if ! block_ipv6; then enter_direct ipv6_unavailable; sleep 5; continue; fi
      ipv6_mode=blocked
    fi
    if [ "$CHILL_API_LAN" = 1 ] && ! rebuild_api_guard; then enter_direct api_guard_failed; sleep 5; continue; fi
    ip route del unreachable "$FAKE_IP_NET" 2>/dev/null || true

    # render() 刚按当前档位设过 GMP（eco = 2 核）
    # Imported names, URLs and keys can appear in raw errors. Publish only
    # phase diagnostics for managed profiles, never Mihomo's raw log output.
    mihomo_log=/dev/null
    [ "$active_kind" != legacy ] || mihomo_log=$LOG
    (umask 077; if [ -n "$GMP" ]; then export GOMAXPROCS="$GMP"; fi; exec "$BIN" -d "$runtime_home" -f "$CONFIG") >> "$mihomo_log" 2>&1 &
    mihomo_pid=$!
    printf '%s\n' "$mihomo_pid" > "$CORE_PID"
    if ! wait_core_ready "$mihomo_pid" || ! wait_gateway_ready "$mihomo_pid"; then
      enter_direct profile_not_ready
      note_crash || : > "$GAVEUP"
      sleep 5; continue
    fi
    if [ "$ipv6_mode" = tun ] && ! tun_v6_ready; then enter_direct ipv6_unavailable; sleep 5; continue; fi

    net_tune_chill0

    # Do not publish running or hand dnsmasq to a partially started gateway.
    if ! restore_mode || ! bypass_lock bypass_apply_saved || ! apply_dns; then
      enter_direct profile_not_ready
      note_crash || : > "$GAVEUP"
      sleep 5; continue
    fi
    save_mode

    event "started -> running"
    write_state running ""
    direct_since=0; tick=0; wan_other=0; wan_was=$(ip route show default 2>/dev/null)
    owned_rules > "$TUN_SNAP"

    # ---- 监督循环 ----
    while :; do
      # One process per tick instead of five: a background sleep plus wait,
      # which a signal interrupts at once, so the TERM trap still fires
      # immediately (a foreground sleep would hold it for up to 5 s).
      sleep 5 & wait $!
      tick=$((tick+5))
      recover_interrupted_profile || true

      # 要换档位（进出 eco）：平稳重启核心。必须在下面「子进程消失」之前处理，
      # 否则自己杀掉的核心会被记成一次崩溃，攒三次就 gaveup。
      if [ -f "$RESTART_FLAG" ]; then
        rm -f "$RESTART_FLAG"
        backoff=0; crash_n=0; crash_t0=-1
        restart_core "$(profile_effective)"
        break
      fi

      if manual_paused; then enter_direct paused; break; fi

      # 息屏暂停：停核心回直连（不算崩溃：在「子进程消失」之前处理），外层循环等租约到期
      if idle_paused; then
        enter_direct idle; state_idle=1
        break
      fi

      if ! kill -0 "$(cat "$CORE_PID" 2>/dev/null)" 2>/dev/null; then
        log "子进程消失"
        flush
        note_crash || { : > "$GAVEUP"; enter_direct gaveup; }
        break
      fi

      [ "$tick" -lt 60 ] && continue
      tick=0

      truncate_log
      ai_gate_tick

      # API 连续 2 次失败按崩溃处理。**必须计数**：此前这条路径只 flush 后
      # break，crash_n 不增，于是核心卡死时永远不会 gaveup，会无限重启。
      if ! api_alive; then
        sleep 3
        if ! api_alive; then
          log "API 连续 2 次无响应，按崩溃处理"
          flush
          note_crash || { : > "$GAVEUP"; enter_direct gaveup; }
          break
        fi
      fi

      t=$(max_temp)
      heat_req
      if [ "$t" -ge "$CHILL_TEMP_HOT" ] || [ "$h" = critical ]; then
        enter_direct overheat
        direct_since=$(boot_now) || direct_since=0
        next_backoff
        break
      fi
      thermal_step "$t" "$h" && break
      save_mode

      # 低内存先归因：只有核心自己 RSS 超标才停，否则只标 mem_pressure。
      # 停核这条同样要走 next_backoff，否则外层立刻重启核心、陷入快速起停循环。
      rss=$(core_rss_kb); avail=$(mem_avail_mb)
      if [ "$avail" -lt "$MEM_AVAIL_MIN" ]; then
        if [ "$rss" -gt "$CHILL_CORE_RSS_MAX" ]; then
          enter_direct lowmem
          direct_since=$(boot_now) || direct_since=0
          next_backoff
          break
        else
          mem_pressure=true; log "内存紧张但核心 RSS ${rss}kB 未超标，仅标记"
        fi
      else
        mem_pressure=false
      fi

      # 默认路由换了别的口：连续两次（约 2 分钟）才回直连；没有默认路由是蜂窝断了
      # 或在重拨，核心留着。
      case $(wan_now) in
        other)
          wan_other=$((wan_other + 1))
          if [ "$wan_other" -ge 2 ]; then enter_direct captive_wan; break; fi ;;
        *) wan_other=0 ;;
      esac
      # 重拨了（默认路由的口或源地址变了）：记一笔；规则下面照常查。
      _wn=$(ip route show default 2>/dev/null)
      if [ -n "$_wn" ] && [ "$_wn" != "$wan_was" ]; then
        [ -n "$wan_was" ] && event "wan_changed -> check"
        wan_was=$_wn
      fi
      if ! tun_rules_ok; then tun_heal; break; fi

      # 段外漂移只记录、不自动处理（T3）。注意比的是自管区间**之外**，
      # 用来发现我们是否误删了别人的规则；段内被清空属正常降级，不是漂移。
      check_rules_drift

      # 运营商 DNS 变了（换卡、重新拨号）就热重载，让直连用上新的。拿不到 v4 时
      # （断网中）不动，留着上次的。
      case "$active_kind" in
        node_url|legacy)
          d4=$(direct_dns4)
          if [ -n "$d4" ] && [ "$d4" != "${RENDERED_DNS_EXTRA:-}" ]; then
            if do_reload; then event "direct_dns -> reload"; else log "运营商 DNS 变了，但热重载失败"; fi
          fi ;;
      esac

      write_state running ""
    done
  done
}

# CHILL_API_LAN=1 时只允许本机和 br-lan 上白名单里的 IPv4 来源。fw3 不管理这张 nft 表。
rebuild_api_guard() {
  nft delete table inet chill 2>/dev/null || true
  allow=""
  for one in $CHILL_API_ALLOW_IP; do allow="$allow $one,"; done
  nft -f - <<EOF 2>/dev/null
table inet chill {
  chain input {
    type filter hook input priority -5;
    tcp dport 9999 iifname "lo" accept
    $( [ -n "$allow" ] && echo "tcp dport 9999 iifname \"br-lan\" ip saddr {${allow%,} } accept" )
    tcp dport 9999 drop
  }
}
EOF
}

# ─────────────────────────────────────────────────────────────────────────────
# 可测性开关：单测用 `CHILL_LIB_ONLY=1 . ./chill.sh` 只加载函数、不执行子命令。
# 没有它就没法对内部函数做离线单测——source 一下就会掉进下面的 case 里。
# return 在非 source 上下文不合法，所以用 `|| exit 0` 兜底。
if [ "${CHILL_LIB_ONLY:-0}" = "1" ]; then
  return 0 2>/dev/null || exit 0
fi

case "${1:-}" in
  run)     run ;;
  flush)   flush ;;
  status)  cat "$STATE" 2>/dev/null || echo '{"state":"unknown"}' ;;
  start)   rm -f "$DISABLED" "$GAVEUP" "$CHILL_DIR/profile-switch-failed"; "$CHILL_INIT" start ;;
  stop)
    : > "$DISABLED"
    "$CHILL_INIT" stop
    mark_stopped
    # 总开关停机后 unreachable 路由保留 5 分钟再删，让客户端尽快重解析而不是静默超时
    (sleep 300; ip route del unreachable "$FAKE_IP_NET" 2>/dev/null) >/dev/null 2>&1 &
    ;;
  restart) "$CHILL_INIT" restart ;;
  profile-validate|profile-switch|profile-refresh)
    [ "$#" -eq 2 ] && valid_id "$2" || { echo 'invalid_profile_id' >&2; exit 2; }
    case "$1" in
      profile-validate) profile_lock validate_profile "$2" ;;
      profile-switch) profile_lock profile_switch "$2" ;;
      profile-refresh) profile_lock profile_refresh "$2" ;;
    esac ;;
  bypass-set) shift; bypass_lock bypass_set "$@" ;;
  validate)
    case "${2:-}" in
      '') profile_lock validate_active ;;
      --template) [ "$#" -eq 3 ] && validate_template "$3" && TEMPLATE=$3 profile_lock validate_active ;;
      *) if valid_id "$2"; then profile_lock validate_profile "$2";
         else validate_template "$2" && TEMPLATE=$2 profile_lock validate_active; fi ;;
    esac
    ;;
  reload)
    # 要返回正确的退出码：调用方（脚本、u60d）凭它判断 reload 成没成。
    # mihomo 的 /configs 只认 PUT 且必须带 {"path": ...}（真机验证：空 body 400，带 path 204）。
    if do_reload; then
      echo "reloaded"
    else
      echo "reload 失败（配置未通过校验时会保留旧配置）" >&2
      exit 1
    fi
    ;;
  profile)
    if [ -z "${2:-}" ]; then
      echo "$(profile_read) $(profile_effective)"
      exit 0
    fi
    case "$2" in eco|standard|perf) ;; *) echo "档位只能是 eco / standard / perf" >&2; exit 2 ;; esac
    old=$(profile_effective)
    if [ "$2" = standard ]; then rm -f "$PROFILE_FILE"; else echo "$2" > "$PROFILE_FILE"; fi
    new=$(profile_effective)
    state_profile_now
    if [ "$old" = "$new" ]; then echo "档位 $2（实际 ${new}，不用动核心）"; exit 0; fi
    if ! grep -q '"state":"running"' "$STATE" 2>/dev/null; then
      echo "档位 $2（核心没在跑，下次启动生效）"; exit 0
    fi
    profile_vars "$old"; g_old=$GMP
    profile_vars "$new"; g_new=$GMP
    if [ "$g_old" != "$g_new" ]; then
      : > "$RESTART_FLAG"
      echo "档位 ${2}：重启核心（约 10 秒）"
    elif do_reload; then
      event "reload -> $new"
      echo "档位 ${2}：已热重载"
    else
      echo "档位 ${2}：热重载失败，核心仍按原配置运行" >&2
      exit 1
    fi
    ;;
  pause)
    m="${2:-30}"
    pause_for "$m" || { echo "暂停失败：分钟需为 0–10080，且开机时钟必须可读" >&2; exit 1; }
    echo "已暂停 ${m} 分钟"
    ;;
  safe-start)
    # 2026-09-16 真机实测踩到的缺口：start 子命令会先清 disabled/gaveup，
    # safe-start 原来没有——上一轮 safe-start 的 5 分钟死人开关到期后正确地
    # 设置了 disabled 并停止，但重新 safe-start 时这个文件还在，新实例一起来
    # 就立刻卡在「已关闭」，连 render 都不会尝试。safe-start 本该是「重新开始
    # 一轮全新验证」的入口，语义上就该跟 start 一样先清场。
    rm -f "$DISABLED" "$GAVEUP" "$CHILL_DIR/confirmed" "$CHILL_DIR/profile-switch-failed"
    # 2026-09-16 真机实测踩到的第二个缺口，比上面那个更严重：每次 safe-start
    # 都会独立起一个 300 秒计时器，旧的从来不会被取消。当天连续调了几次
    # safe-start 调试，旧计时器在我 confirm 之前就先到期检查了 confirmed、
    # 发现还不存在，直接把刚激活、工作正常的核心关掉了——confirm 晚到没用，
    # 因为检查只做一次。记录死人开关自己的 pid，下次 safe-start 前先杀掉上一个。
    [ -f "$CHILL_DIR/deadman.pid" ] && kill "$(cat "$CHILL_DIR/deadman.pid")" 2>/dev/null
    "$CHILL_INIT" start || exit 1
    # 首装保险：5 分钟内没 confirm 就自动关掉，避免把自己锁在外面
    ( sleep 300; [ -f "$CHILL_DIR/confirmed" ] || { : > "$DISABLED"; "$CHILL_INIT" stop; mark_stopped; } ) \
      >/dev/null 2>&1 &
    echo $! > "$CHILL_DIR/deadman.pid"
    echo "safe-start：5 分钟内请执行 chill.sh confirm"
    ;;
  confirm)
    : > "$CHILL_DIR/confirmed"
    # 主动杀掉挂起的死人开关，不被动等它自己去查文件——更快、更确定，
    # 不依赖它的 sleep 恰好在 confirmed 写入之后才醒来检查。
    [ -f "$CHILL_DIR/deadman.pid" ] && kill "$(cat "$CHILL_DIR/deadman.pid")" 2>/dev/null
    echo "已确认"
    ;;
  ai-gate)
    # 核心（engines/chill_ai_gate.rs）用：on = 封、off = 放；status 打印 blocked / open。
    case "${2:-}" in
      on|off) if ai_gate_write "$2"; then dnsmasq_restart; echo changed; else echo same; fi ;;
      status) [ -e "$AI_BLOCK" ] && echo blocked || echo open ;;
      *) echo "usage: $0 ai-gate {on|off|status}" >&2; exit 2 ;;
    esac
    ;;
  *) echo "usage: $0 {run|start|stop|restart|reload|status|flush|pause|safe-start|confirm|profile|profile-validate|profile-switch|profile-refresh|validate|bypass-set|ai-gate}"; exit 1 ;;
esac
