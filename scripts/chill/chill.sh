#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# CHILL 主控脚本（T2）
#
#   chill.sh run                 procd 实例命令。前台常驻，监督 mihomo 子进程。
#   chill.sh start|stop|restart  总开关（写/删 /data/chill/disabled 并调 init.d）
#   chill.sh reload              重新渲染并热重载（不重启进程）
#   chill.sh status              打印 chill.state
#   chill.sh flush               幂等清理（可单独执行）
#   chill.sh pause <分钟>        临时直连，到点自动恢复
#   chill.sh safe-start          首装用：挂 5 分钟死人开关，没 confirm 就自动 stop
#   chill.sh confirm             撤销 safe-start 的死人开关
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
EVENTS="$CHILL_DIR/events.log"
DISABLED="$CHILL_DIR/disabled"
GAVEUP="$CHILL_DIR/gaveup"
PAUSE_UNTIL="$CHILL_DIR/pause_until"

STATE=/tmp/chill.state
CORE_PID=/tmp/chill.core.pid
LOG=/tmp/chill.log
RULES_BEFORE=/tmp/chill.rules.before

FAKE_IP_NET=198.18.0.0/16
TABLE=2022
# CHILL 自管的 ip rule 区间。8990–9099：mihomo 自己用 9000+，按源 IP 绕行用 8999。
# 区间之外一律不删——Tailscale 的 5210–5270 与 table 52 永不触碰。
RULE_LO=8990
RULE_HI=9099
BYPASS_PRIO=8999

TEMP_HOT_DEFAULT=75000      # 毫摄氏度。占位值，待第 3 组基线峰值加余量回填
TEMP_RECOVER_DELTA=10000    # 恢复阈值比触发阈值低 10°C
CORE_RSS_MAX_DEFAULT=153600 # kB，约 150MB
MEM_AVAIL_MIN=500           # MB
DWELL_SECONDS=120           # 进入直连后至少驻留多久才评估恢复
CRASH_WINDOW=600            # 10 分钟
CRASH_MAX=3
# /tmp 是内存盘，日志不截断的话长期运行会把内存吃掉（§2 要求每 60 秒截断）。
LOG_MAX_BYTES=524288        # 512 KB

[ -f "$ENV_FILE" ] && . "$ENV_FILE"
CHILL_API_LAN="${CHILL_API_LAN:-0}"
CHILL_BYPASS_IP="${CHILL_BYPASS_IP:-}"
CHILL_API_ALLOW_IP="${CHILL_API_ALLOW_IP:-}"
CHILL_TEMP_HOT="${CHILL_TEMP_HOT:-$TEMP_HOT_DEFAULT}"
CHILL_CORE_RSS_MAX="${CHILL_CORE_RSS_MAX:-$CORE_RSS_MAX_DEFAULT}"

log() { echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) $*" >> "$LOG" 2>/dev/null || true; }

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
api_alive() {
  curl -s -m 3 -o /dev/null -w '%{http_code}' \
    -H "Authorization: Bearer ${CHILL_SECRET:-}" \
    http://127.0.0.1:9999/version 2>/dev/null | grep -q '^2'
}

# ── 状态文件（对外契约，见 §9）────────────────────────────────────────────────
# 原子写入：先写临时文件再 mv。字段名不能随便改，触屏和后台都按这个读。
write_state() {
  _state="$1"; _reason="$2"
  _pid=$(cat "$CORE_PID" 2>/dev/null || echo 0)
  _bypass="${bypass_stale_json:-[]}"
  cat > "$STATE.tmp" <<EOF
{"state":"$_state","reason":$( [ -n "$_reason" ] && echo "\"$_reason\"" || echo null ),
"cpuss_c":$(( $(max_temp) / 1000 )),"mem_avail_mb":$(mem_avail_mb),
"core_pid":$_pid,"started_at":"${started_at:-}","updated_at":"$(date -u +%Y-%m-%dT%H:%M:%SZ)",
"bypass_stale":$_bypass,"rules_drift":${rules_drift:-false},"mem_pressure":${mem_pressure:-false}}
EOF
  mv "$STATE.tmp" "$STATE" 2>/dev/null
}

# ── dnsmasq 回滚（决定 3）────────────────────────────────────────────────────
# ⚠ 这是整个脚本最不能出错的函数。dnsmasq 上游指向 mihomo 期间，只停核不回滚 DNS
# 会让全屋**断网**而不是降级成直连——过热、崩溃、stop、重启全都会踩到。
# 幂等：用 cmp -s 判断，一致就不动，避免无谓 reload。
restore_dns() {
  [ -f "$DHCP_BACKUP" ] || return 0
  cmp -s "$DHCP_BACKUP" "$DHCP_CONF" && return 0
  cp "$DHCP_BACKUP" "$DHCP_CONF" 2>/dev/null || return 1
  "$DNSMASQ_INIT" reload >/dev/null 2>&1 || "$DNSMASQ_INIT" restart >/dev/null 2>&1
  log "restore_dns: dnsmasq 已回滚到备份"
  event "dns_restored -> direct"
}

# 让 dnsmasq 把查询转发给 mihomo。**必须在确认 mihomo 的 dns.listen 能应答之后再调**，
# 反序会造成启动期间全屋 DNS 中断。
apply_dns() {
  [ -f "$DHCP_BACKUP" ] || cp "$DHCP_CONF" "$DHCP_BACKUP" 2>/dev/null
  # 段名是 lan_dns 不是 @dnsmasq[0]（本机实例名），写错会静默设到不生效的段上
  uci -q set dhcp.lan_dns.noresolv=1
  uci -q delete dhcp.lan_dns.server 2>/dev/null
  uci -q add_list dhcp.lan_dns.server=127.0.0.1
  uci -q commit dhcp
  "$DNSMASQ_INIT" reload >/dev/null 2>&1
  log "apply_dns: dnsmasq 上游已指向 mihomo"
}

dns_answers() {
  # 确认 mihomo 的 dns.listen 真的能应答，再动 dnsmasq
  nslookup www.gstatic.com 127.0.0.1 >/dev/null 2>&1
}

# ── 绕行（决定 1：按源 IP，不再用 exclude-mac-address）──────────────────────
# 增删绕行设备只是加删一条 ip rule，不碰配置文件、不 reload、不重建 TUN、零中断。
apply_bypass() {
  [ -n "$CHILL_BYPASS_IP" ] || return 0
  lan=$(ip -4 addr show br-lan 2>/dev/null | awk '/inet /{print $2; exit}')
  for one in $CHILL_BYPASS_IP; do
    case "$one" in
      *[!0-9.]*) log "bypass: 跳过非法 IP ${one}"; continue ;;
    esac
    ip rule add from "$one" priority "$BYPASS_PRIO" lookup main 2>/dev/null \
      && log "bypass: ${one} 已绕行（lan=${lan}）" \
      || log "bypass: ${one} 添加失败"
  done
}

# ── flush（幂等；任何退出路径都要能跑）──────────────────────────────────────
flush() {
  # 第一步必须是 restore_dns，理由见该函数注释
  restore_dns

  kill_core

  nft delete table inet mihomo 2>/dev/null || true
  nft delete table inet chill 2>/dev/null || true

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
  rm -f "$CORE_PID"
  log "flush 完成"
  check_rules_drift
}

# 段外漂移检测（T3）。
# 要回答的问题是「我们的 flush 有没有误删别人的规则」——尤其 Tailscale 的
# 5210–5270 与 table 52。所以比的是**自管区间之外**的部分：把启动前快照和当前
# 状态各自剔除 8990–9099 后对比，不一致才算漂移。
# ⚠ 不要反过来写成「段内规则消失就告警」：那既检测不到误删 Tailscale，
#   又会在正常降级（本就该清空段内规则）时误报。
strip_owned() {
  awk -F: -v lo="$RULE_LO" -v hi="$RULE_HI" '
    { p = $1 + 0 }
    p >= lo && p <= hi { next }
    { print }' 2>/dev/null
}
check_rules_drift() {
  [ -f "$RULES_BEFORE" ] || return 0
  { ip rule show; ip -6 rule show; } 2>/dev/null | strip_owned > /tmp/chill.rules.after
  if strip_owned < "$RULES_BEFORE" | cmp -s - /tmp/chill.rules.after; then
    rules_drift=false
  else
    rules_drift=true
    log "rules_drift: 自管区间之外的 ip rule 与启动前快照不一致"
    log "--- 差异 ---"
    strip_owned < "$RULES_BEFORE" | diff - /tmp/chill.rules.after 2>/dev/null | head -10 >> "$LOG" 2>/dev/null
  fi
  rm -f /tmp/chill.rules.after
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
    readlink "$d/exe" 2>/dev/null | grep -q "^$BIN$" || continue
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
render() {
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

  # oix/nexi 已改为 type:file 静态快照（2026-09-16 核对实际数据后的修正，见
  # template.yaml 里的注释），不再需要 URL；SUB_SHOUHOU 仍是唯一的 type:http。
  # CHILL_SECRET 同样会落进单引号 YAML 标量（template.yaml 的 secret: 字段），
  # 校验规则跟 SUB_SHOUHOU 一样。
  for v in SUB_SHOUHOU CHILL_SECRET; do
    eval "val=\${$v:-}"
    case "$val" in
      *"'"*|*"
"*) log "render: ${v} 含单引号或换行，拒绝渲染"; return 1 ;;
    esac
  done

  # ⚠ 这里必须显式把 SUB_SHOUHOU 传进 awk 子进程的环境，不能只做上面的校验就
  # 以为完了。chill.env 是普通变量赋值（不带 export），source 进来的变量不会
  # 自动出现在子进程的 ENVIRON 里；先前这里漏了这一项，导致 template.yaml 里
  # 的 ${SUB_SHOUHOU} 永远不会被替换，会原样留在渲染结果里传给 mihomo。
  CONTROLLER="$CONTROLLER" SUB_SHOUHOU="${SUB_SHOUHOU:-}" \
  CHILL_SECRET="${CHILL_SECRET:-}" \
  FILTER_UNION="${FILTER_UNION:-}" EXCLUDE_FILTER="${EXCLUDE_FILTER:-}" \
  FILTER_TW="${FILTER_TW:-}" FILTER_JP="${FILTER_JP:-}" \
  FILTER_SG="${FILTER_SG:-}" FILTER_US="${FILTER_US:-}" \
  awk -v cnlist="$CN_LIST" '
    # CN_CIDR 单独处理：值有 163KB 量级，不能走 ENVIRON（会撞 argv/envp 大小
    # 限制，见上面的中文注释）。-v 传的只是文件路径（几十字节），BEGIN 里
    # 自己按行读文件拼接，全程文件 I/O，没有大小限制。
    BEGIN {
      # 2026-09-16 真机实测：三元运算符跟字符串拼接混用在这台设备的 awk 实现
      # 里解析有问题——cn_cidr = (cond) ? a : b c 这种写法最终只会得到 "0"
      # （9647 行全部读到了，拼接结果却只有 1 个字符），懒得深究是不是运算符
      # 优先级的坑，改成显式 if/else 更稳妥，不依赖三元表达式在这个 awk 实现
      # 里的具体行为。
      cn_cidr = ""; first = 1
      while ((getline cline < cnlist) > 0) {
        if (cline == "") continue
        if (first) { cn_cidr = cline; first = 0 }
        else { cn_cidr = cn_cidr ", " cline }
      }
      close(cnlist)
    }
    {
      line = $0
      while (match(line, /\$\{[A-Z_]+\}/)) {
        key = substr(line, RSTART + 2, RLENGTH - 3)
        # 同上：这台设备的 awk 三元运算符不可靠，这里也改用显式 if/else。
        if (key == "CN_CIDR") { val = cn_cidr } else { val = ENVIRON[key] }
        line = substr(line, 1, RSTART - 1) val substr(line, RSTART + RLENGTH)
      }
      print line
    }' "$TEMPLATE" > "$CONFIG.tmp" || return 1

  # 绝对路径 -f：验证从任何工作目录都能通过
  if "$BIN" -t -d "$CHILL_DIR" -f "$CONFIG.tmp" >/dev/null 2>&1; then
    mv "$CONFIG.tmp" "$CONFIG"; chmod 600 "$CONFIG"
    return 0
  fi
  log "render: mihomo -t 未通过，保留旧配置"
  rm -f "$CONFIG.tmp"
  return 1
}

# ── 前置检查 ────────────────────────────────────────────────────────────────
rulesets_ok() {
  for f in cn_domain private_domain ai_domain; do
    [ -s "$CHILL_DIR/ruleset/$f.mrs" ] || return 1
  done
  return 0
}
wan_is_cellular() {
  ip route show default 2>/dev/null | grep -q 'dev rmnet_data0'
}

# 直连状态：装 unreachable 路由，让持有 fake-ip 的客户端立刻失败重解析，
# 而不是静默超时等到 TTL 过期。
enter_direct() {
  reason="$1"
  restore_dns
  kill_core
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
  now=$(date +%s)
  if [ "$crash_t0" -eq 0 ] || [ $((now - crash_t0)) -gt "$CRASH_WINDOW" ]; then
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
  backoff=$(( $(date +%s) + _w ))
  log "降级第 ${degrade_n} 次，退避 ${_w} 秒"
}

# §2 要求每 60 秒截断日志。不截断的话 /tmp 是内存盘，长期运行会把内存吃掉。
truncate_log() {
  [ -f "$LOG" ] || return 0
  sz=$(wc -c < "$LOG" 2>/dev/null || echo 0)
  [ "$sz" -le "$LOG_MAX_BYTES" ] && return 0
  tail -c "$((LOG_MAX_BYTES / 2))" "$LOG" > "$LOG.tmp" 2>/dev/null && mv "$LOG.tmp" "$LOG" 2>/dev/null
}

# ── run：procd 实例命令 ─────────────────────────────────────────────────────
run() {
  trap 'log "收到 TERM/INT"; flush; exit 0' TERM INT

  flush
  ip rule show > "$RULES_BEFORE" 2>/dev/null
  ip -6 rule show >> "$RULES_BEFORE" 2>/dev/null

  started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  rules_drift=false; mem_pressure=false; bypass_stale_json='[]'
  crash_n=0; crash_t0=0; backoff=0; direct_since=0; degrade_n=0

  while :; do
    # ---- 不该启动的情形，保持进程存活、停在直连 ----
    if [ -f "$DISABLED" ]; then enter_direct disabled; sleep 5; continue; fi
    if [ -f "$GAVEUP" ];   then enter_direct gaveup;   sleep 5; continue; fi
    if [ -f "$PAUSE_UNTIL" ]; then
      until_ts=$(cat "$PAUSE_UNTIL" 2>/dev/null || echo 0)
      if [ "$(date +%s)" -lt "$until_ts" ]; then enter_direct paused; sleep 5; continue; fi
      rm -f "$PAUSE_UNTIL"
    fi
    if ! rulesets_ok; then enter_direct ruleset_missing; sleep 30; continue; fi
    if ! wan_is_cellular; then enter_direct captive_wan; sleep 15; continue; fi
    if [ "$backoff" -gt 0 ]; then
      [ "$(date +%s)" -lt "$backoff" ] && { sleep 5; continue; }
      backoff=0
    fi

    # ---- 启动核心 ----
    render || { enter_direct render_failed; sleep 30; continue; }
    [ "$CHILL_API_LAN" = "1" ] && rebuild_api_guard
    ip route del unreachable "$FAKE_IP_NET" 2>/dev/null || true

    "$BIN" -d "$CHILL_DIR" -f "$CONFIG" >> "$LOG" 2>&1 &
    echo $! > "$CORE_PID"
    sleep 5

    if ! kill -0 "$(cat "$CORE_PID" 2>/dev/null)" 2>/dev/null; then
      log "核心启动即退出"
      note_crash || : > "$GAVEUP"
      flush; sleep 5; continue
    fi

    # DNS 与绕行都要在核心确认可用之后才施加
    if dns_answers; then apply_dns; else log "dns.listen 无应答，跳过 apply_dns"; fi
    apply_bypass

    event "started -> running"
    write_state running ""
    direct_since=0; tick=0

    # ---- 监督循环 ----
    while :; do
      i=0; while [ $i -lt 5 ]; do sleep 1; i=$((i+1)); done
      tick=$((tick+5))

      if ! kill -0 "$(cat "$CORE_PID" 2>/dev/null)" 2>/dev/null; then
        log "子进程消失"
        flush
        note_crash || { : > "$GAVEUP"; enter_direct gaveup; }
        break
      fi

      [ "$tick" -lt 60 ] && continue
      tick=0

      truncate_log

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
      if [ "$t" -ge "$CHILL_TEMP_HOT" ]; then
        enter_direct overheat
        direct_since=$(date +%s)
        next_backoff
        break
      fi

      # 低内存先归因：只有核心自己 RSS 超标才停，否则只标 mem_pressure。
      # 停核这条同样要走 next_backoff，否则外层立刻重启核心、陷入快速起停循环。
      rss=$(core_rss_kb); avail=$(mem_avail_mb)
      if [ "$avail" -lt "$MEM_AVAIL_MIN" ]; then
        if [ "$rss" -gt "$CHILL_CORE_RSS_MAX" ]; then
          enter_direct lowmem
          direct_since=$(date +%s)
          next_backoff
          break
        else
          mem_pressure=true; log "内存紧张但核心 RSS ${rss}kB 未超标，仅标记"
        fi
      else
        mem_pressure=false
      fi

      # 段外漂移只记录、不自动处理（T3）。注意比的是自管区间**之外**，
      # 用来发现我们是否误删了别人的规则；段内被清空属正常降级，不是漂移。
      check_rules_drift

      write_state running ""
    done
  done
}

# CHILL_API_LAN=1 时限制 :9999 的来源。fw3 不管 nft 表，所以这张表不会被 reload 冲掉。
rebuild_api_guard() {
  nft delete table inet chill 2>/dev/null || true
  allow=""
  for one in $CHILL_API_ALLOW_IP; do allow="$allow $one,"; done
  nft -f - <<EOF 2>/dev/null || log "rebuild_api_guard 失败"
table inet chill {
  chain input {
    type filter hook input priority -5;
    tcp dport 9999 iifname { "lo", "tailscale0" } accept
    $( [ -n "$allow" ] && echo "tcp dport 9999 ip saddr { ${allow%,} } accept" )
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
  start)   rm -f "$DISABLED" "$GAVEUP"; /etc/init.d/chill start ;;
  stop)
    : > "$DISABLED"
    /etc/init.d/chill stop
    # 总开关停机后 unreachable 路由保留 5 分钟再删，让客户端尽快重解析而不是静默超时
    (sleep 300; ip route del unreachable "$FAKE_IP_NET" 2>/dev/null) >/dev/null 2>&1 &
    ;;
  restart) /etc/init.d/chill restart ;;
  reload)
    # 要返回正确的退出码：先前写成 `… && echo 成功 || echo 失败`，失败时也返回 0，
    # 调用方（脚本、agent）无法凭退出码判断 reload 到底成没成。
    # mihomo 的 /configs 只认 PUT 且必须带 {"path": ...}——用 wget --post-data
    # 发的是 POST 且空 body，两条都不对，这条 reload 路径从写出来就没真正成功过，
    # 2026-09-17 靠 chill.rs 的订阅改 URL 功能第一次真正调用到才暴露（真机验证：
    # 空 body 的 PUT 返回 400，带 path 的 PUT 返回 204）。
    if render && curl -s -m 5 -o /dev/null -w '%{http_code}' -X PUT \
         -H "Authorization: Bearer ${CHILL_SECRET:-}" \
         --data "{\"path\":\"$CONFIG\"}" "http://127.0.0.1:9999/configs?force=true" \
         | grep -q '^2'; then
      echo "reloaded"
    else
      echo "reload 失败（配置未通过校验时会保留旧配置）" >&2
      exit 1
    fi
    ;;
  pause)
    m="${2:-30}"
    echo $(( $(date +%s) + m * 60 )) > "$PAUSE_UNTIL"
    echo "已暂停 ${m} 分钟"
    ;;
  safe-start)
    # 2026-09-16 真机实测踩到的缺口：start 子命令会先清 disabled/gaveup，
    # safe-start 原来没有——上一轮 safe-start 的 5 分钟死人开关到期后正确地
    # 设置了 disabled 并停止，但重新 safe-start 时这个文件还在，新实例一起来
    # 就立刻卡在「已关闭」，连 render 都不会尝试。safe-start 本该是「重新开始
    # 一轮全新验证」的入口，语义上就该跟 start 一样先清场。
    rm -f "$DISABLED" "$GAVEUP"
    # 2026-09-16 真机实测踩到的第二个缺口，比上面那个更严重：每次 safe-start
    # 都会独立起一个 300 秒计时器，旧的从来不会被取消。当天连续调了几次
    # safe-start 调试，旧计时器在我 confirm 之前就先到期检查了 confirmed、
    # 发现还不存在，直接把刚激活、工作正常的核心关掉了——confirm 晚到没用，
    # 因为检查只做一次。记录死人开关自己的 pid，下次 safe-start 前先杀掉上一个。
    [ -f "$CHILL_DIR/deadman.pid" ] && kill "$(cat "$CHILL_DIR/deadman.pid")" 2>/dev/null
    /etc/init.d/chill start
    # 首装保险：5 分钟内没 confirm 就自动关掉，避免把自己锁在外面
    ( sleep 300; [ -f "$CHILL_DIR/confirmed" ] || { : > "$DISABLED"; /etc/init.d/chill stop; } ) \
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
  *) echo "usage: $0 {run|start|stop|restart|reload|status|flush|pause <min>|safe-start|confirm}"; exit 1 ;;
esac
