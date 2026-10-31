#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# chill.sh 离线单测（T8 的单测部分）
#
#   ./chill-unit.sh
#
# 不碰设备、不需要订阅、不需要 root。做法是用同名 shell 函数覆盖 ip / uci / nft
# 等外部命令（POSIX sh 里函数优先于 PATH 查找），把它们的调用记录下来再断言。
#
# 不在这里测的东西，以及原因：
#   - 正则的**语义**（大小写、误匹配）：(?i) 是 PCRE/Go 语法，grep -E 根本不支持，
#     硬塞进来只会得到虚假通过。那部分归 test/region-filter.sh 用 python 跑。
#   - 真实的 TUN / nft / dnsmasq 行为：属 E2E（chill-e2e.sh），要真机。
# ─────────────────────────────────────────────────────────────────────────────
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
SUT="$HERE/../chill.sh"

PASS=0; FAIL=0
ok() { PASS=$((PASS+1)); printf '  ✓ %s\n' "$*"; }
ng() { FAIL=$((FAIL+1)); printf '  ✗ %s\n' "$*"; }
eq() { # eq <名称> <实际> <期望>
  if [ "$2" = "$3" ]; then ok "$1"; else ng "$1"; printf '      期望 [%s]\n      实际 [%s]\n' "$3" "$2"; fi
}
has() { # has <名称> <字符串> <子串>
  case "$2" in *"$3"*) ok "$1" ;; *) ng "$1（未包含 [$3]）" ;; esac
}
sec() { printf '\n=== %s\n' "$*"; }

[ -f "$SUT" ] || { echo "找不到 $SUT"; exit 1; }

# 隔离：别让被测脚本写到真实路径
export CHILL_DIR=/tmp/chill-unit-fixture
rm -rf "$CHILL_DIR"; mkdir -p "$CHILL_DIR/ruleset" "$CHILL_DIR/run"
LOG=/tmp/chill-unit.log; : > "$LOG"

# 只加载函数，不执行子命令
CHILL_LIB_ONLY=1 . "$SUT"
# 被测脚本里的路径常量是在 source 时就展开的，这里必须**逐个**重新指向 fixture。
# 只改 CHILL_DIR 是不够的：EVENTS/STATE/CORE_PID 早已展开成 /data/chill/... 的绝对
# 路径，漏掉它们的话相关函数会往不存在的路径写，测试看似通过、其实一行都没覆盖到。
CHILL_DIR=/tmp/chill-unit-fixture
LOG=/tmp/chill-unit.log
EVENTS="$CHILL_DIR/events.log"
STATE="$CHILL_DIR/chill.state"
CORE_PID="$CHILL_DIR/core.pid"
PROFILE_FILE="$CHILL_DIR/profile"
MODE_FILE="$CHILL_DIR/mode"
RESTART_FLAG="$CHILL_DIR/restart"
THERMAL_ECO="$CHILL_DIR/thermal-eco"
IDLE_LEASE="$CHILL_DIR/idle"
CHILL_PROC="$CHILL_DIR/proc"
mkdir -p "$CHILL_PROC/sys/kernel/random"
printf 'boot-a\n' > "$CHILL_PROC/sys/kernel/random/boot_id"
printf '10000.00 0.00\n' > "$CHILL_PROC/uptime"

# ─────────────────────────────────────────────────────────────────────────────
sec "build_filters：结构与裁剪"

CHILL_REGIONS=''
build_filters
has "TW 含中文名"            "$FILTER_TW" '台湾'
has "TW 含字母代码与边界"     "$FILTER_TW" '(^|[^A-Za-z])TWN?([^A-Za-z]|$)'
has "TW 前置忽略大小写 flag"  "$FILTER_TW" '(?i)'
has "JP 正确"                "$FILTER_JP" '日本'
has "SG 正确"                "$FILTER_SG" '新加坡'
has "US 正确"                "$FILTER_US" '美国'

# (?i) 必须只出现一次且在最前面——否则 Go 能编译但 python 测试脚本会失败，
# 归属测试就成了盲区。
n=$(printf '%s' "$FILTER_UNION" | grep -o '(?i)' | wc -l | tr -d ' ')
eq "并集里 (?i) 只出现一次" "$n" "1"
case "$FILTER_UNION" in '(?i)('*) ok "并集以 (?i)( 开头" ;; *) ng "并集未以 (?i)( 开头" ;; esac

has "并集含四个地区" "$FILTER_UNION" '台湾'
has "并集含日本"     "$FILTER_UNION" '日本'
has "并集含新加坡"   "$FILTER_UNION" '新加坡'
has "并集含美国"     "$FILTER_UNION" '美国'

full_len=${#FILTER_UNION}
CHILL_REGIONS='TW JP'
build_filters
cut_len=${#FILTER_UNION}
# 花括号不能省：中文 locale 下 $cut_len 紧跟全角「）」会被当成变量名的一部分，
# set -u 直接报 unbound。今天已在 spike.sh 里踩过三次，这里是第四次。
if [ "$cut_len" -lt "$full_len" ]; then ok "CHILL_REGIONS 裁剪生效（${full_len} -> ${cut_len}）"
else ng "CHILL_REGIONS 未生效（${full_len} -> ${cut_len}）"; fi
case "$FILTER_UNION" in *新加坡*) ng "裁剪后仍含新加坡" ;; *) ok "裁剪后不含新加坡" ;; esac

CHILL_REGIONS='XX'
if build_filters 2>/dev/null; then ng "全是未知地区时应拒绝渲染"; else ok "全是未知地区时返回失败"; fi
CHILL_REGIONS=''

# ─────────────────────────────────────────────────────────────────────────────
sec "strip_owned：自管区间边界"

SAMPLE='0:	from all lookup local
5210:	from all fwmark 0x80000/0xff0000 lookup main
5270:	from all lookup 52
8989:	from 192.168.0.7 lookup main
8990:	from 192.168.0.8 lookup main
8999:	from 192.168.0.9 lookup main
9000:	from all iif br-lan goto 9002
9099:	from all nop
9100:	from all lookup main
32766:	from all lookup main'

out=$(printf '%s\n' "$SAMPLE" | strip_owned)
case "$out" in *5210:*) ok "保留 Tailscale 5210" ;; *) ng "误删了 Tailscale 5210" ;; esac
case "$out" in *5270:*) ok "保留 Tailscale 5270" ;; *) ng "误删了 Tailscale 5270" ;; esac
case "$out" in *8989:*) ok "保留区间下界外 8989" ;; *) ng "误删了 8989（区间外）" ;; esac
case "$out" in *9100:*) ok "保留区间上界外 9100" ;; *) ng "误删了 9100（区间外）" ;; esac
case "$out" in *8990:*) ng "未剔除下界 8990" ;; *) ok "剔除下界 8990" ;; esac
case "$out" in *8999:*) ng "未剔除绕行规则 8999" ;; *) ok "剔除绕行规则 8999" ;; esac
case "$out" in *9099:*) ng "未剔除上界 9099" ;; *) ok "剔除上界 9099" ;; esac

sec "check_rules_drift：只认删除"

RULES_BEFORE="$CHILL_DIR/rules.before"
printf '%s\n' "$SAMPLE" > "$RULES_BEFORE"
RULES_NOW="$SAMPLE"
ip() { [ "${1:-}" = "-6" ] || printf '%s\n' "$RULES_NOW"; }
rules_drift=; check_rules_drift; eq "没变：不漂移" "$rules_drift" "false"
RULES_NOW="$SAMPLE
4200000059:	from all iif wlan0 failed_policy"
rules_drift=; check_rules_drift; eq "原厂新增规则：不漂移" "$rules_drift" "false"
RULES_NOW=$(printf '%s\n' "$SAMPLE" | grep -v '^8990:\|^9000:')
rules_drift=; check_rules_drift; eq "自管区间内消失：不漂移" "$rules_drift" "false"
: > "$LOG"
RULES_NOW=$(printf '%s\n' "$SAMPLE" | grep -v '^5210:')
rules_drift=; check_rules_drift; eq "Tailscale 5210 消失：漂移" "$rules_drift" "true"
has "日志写出消失的规则" "$(cat "$LOG")" '5210:'
case "$(cat "$LOG")" in *5270:*) ng "日志混进了没消失的规则" ;; *) ok "日志只写消失的规则" ;; esac

SYS_NET="$CHILL_DIR/sysnet"; mkdir -p "$SYS_NET/wlan0" "$SYS_NET/wlan2"
WL='4200000025:	from all iif wlan0 failed_policy
4200000026:	from all iif wlan2 failed_policy'
printf '%s\n%s\n' "$SAMPLE" "$WL" > "$RULES_BEFORE"
echo down > "$SYS_NET/wlan0/operstate"; rm -rf "$SYS_NET/wlan2"
RULES_NOW="$SAMPLE"
rules_drift=; check_rules_drift; eq "情景关 Wi-Fi，接口规则跟着没了：不漂移" "$rules_drift" "false"
echo up > "$SYS_NET/wlan0/operstate"; mkdir -p "$SYS_NET/wlan2"; echo up > "$SYS_NET/wlan2/operstate"
RULES_NOW="$SAMPLE
4200000061:	from all iif wlan0 failed_policy
4200000062:	from all iif wlan2 failed_policy"
rules_drift=; check_rules_drift; eq "Wi-Fi 回来、优先级变了：不漂移" "$rules_drift" "false"
: > "$LOG"
RULES_NOW="$SAMPLE
4200000061:	from all iif wlan0 failed_policy"
rules_drift=; check_rules_drift; eq "接口在跑、规则真没了：漂移" "$rules_drift" "true"
has "日志写出 wlan2 那条" "$(cat "$LOG")" 'iif wlan2'
case "$(cat "$LOG")" in *"iif wlan0"*) ng "日志混进了还在的 wlan0" ;; *) ok "日志不写还在的 wlan0" ;; esac
unset -f ip; rm -f "$RULES_BEFORE"

# ─────────────────────────────────────────────────────────────────────────────
sec "tun_rules_ok / tun_heal / wan_now：运行中自愈"

TUN_SNAP="$CHILL_DIR/tun.rules"
TUN_RULES='8998:	from all fwmark 0x1000000/0x1000000 lookup main
9000:	from all to 172.19.0.0/30 lookup 2022
9001:	from all iif br-lan lookup 2022
9002:	from all lookup main suppress_prefixlength 0'
T2022='default dev chill0 scope link'
DEF='default via 10.1.1.1 dev rmnet_data0 proto static src 10.1.1.2'
ip() {
  case "$*" in
    "rule show") printf '0:	from all lookup local\n5210:	from all fwmark 0x80000/0xff0000 lookup main\n%s\n32766:	from all lookup main\n' "$RULES_NOW" ;;
    "route show table 2022") [ -n "$T2022" ] && printf '%s\n' "$T2022" ;;
    "route show default") [ -n "$DEF" ] && printf '%s\n' "$DEF" ;;
  esac
  return 0
}
rm -f "$TUN_SNAP"
RULES_NOW='8998:	from all fwmark 0x1000000/0x1000000 lookup main'
tun_rules_ok; eq "TUN 规则还没起来：算好" "$?" 0
eq "没有像样快照：不留 9000 段" "$(grep -c 'lookup 2022' "$TUN_SNAP")" 0
RULES_NOW=$TUN_RULES
tun_rules_ok; eq "规则起来：记快照" "$?" 0
eq "快照只记自管区间" "$(grep -c '^5210:\|^32766:' "$TUN_SNAP")" 0
tun_rules_ok; eq "没变：好" "$?" 0
RULES_NOW="$TUN_RULES
9003:	from all iif wlan9 lookup 2022"
tun_rules_ok; eq "多了一条：好" "$?" 0
: > "$LOG"
RULES_NOW=$(printf '%s\n' "$TUN_RULES" | grep -v '^9001:')
tun_rules_ok; eq "9001 被删：坏" "$?" 1
has "日志写出不见的规则" "$(cat "$LOG")" '9001:'
RULES_NOW=$(printf '%s\n' "$TUN_RULES" | grep -v '^8998:')
tun_rules_ok; eq "国内直连 8998 被删：坏" "$?" 1
RULES_NOW=$TUN_RULES; T2022=
tun_rules_ok; eq "table 2022 清空：坏" "$?" 1
T2022='default dev chill0 scope link'

# 下面换掉几个真函数：放子 shell 里跑，后面的测试照常用真的（失败数用退出码带出来）
( PASS=0; FAIL=0
CALLS=
restart_core() { CALLS="$CALLS restart"; }
enter_direct() { CALLS="$CALLS direct:$1"; }
next_backoff() { CALLS="$CALLS backoff"; }
profile_effective() { echo standard; }
boot_now() { echo "$FAKE_NOW"; }
heal_n=0; heal_t0=-1; FAKE_NOW=1000
tun_heal; tun_heal; tun_heal
eq "三次以内：重启重建" "$CALLS" " restart restart restart"
CALLS=; tun_heal
eq "10 分钟内第 4 次：回直连退避" "$CALLS" " direct:rules_lost backoff"
CALLS=; FAKE_NOW=$((1000 + 601)); tun_heal
eq "过了 10 分钟：重新计数" "$CALLS" " restart"
has "自愈记一笔事件" "$(cat "$EVENTS")" 'rules_lost -> restart'
exit "$FAIL" ) || FAIL=$((FAIL + $?))
PASS=$((PASS + 4))

eq "默认路由走蜂窝" "$(wan_now)" ok
DEF=; eq "没有默认路由（重拨）" "$(wan_now)" none
DEF='default via 192.168.8.1 dev wlan1'; eq "走了别的口" "$(wan_now)" other
unset -f ip; rm -f "$TUN_SNAP"

# ─────────────────────────────────────────────────────────────────────────────
sec "render：占位符与拒绝渲染"

cat > "$CHILL_DIR/template.yaml" <<'YAML'
external-controller: '${CONTROLLER}'
a: '${SUB_URL}'
b: '${FILTER_TW}'
c: [${CN_CIDR}
   10.0.0.0/8]
YAML
printf '1.0.1.0/24\n1.0.2.0/23\n' > "$CHILL_DIR/ruleset/cn.list"
TEMPLATE="$CHILL_DIR/template.yaml"; CN_LIST="$CHILL_DIR/ruleset/cn.list"
CONFIG="$CHILL_DIR/run/config.yaml"; RUN_DIR="$CHILL_DIR/run"

# 打桩：让 mihomo -t 永远通过，把渲染与校验解耦
BIN=/tmp/chill-unit-mihomo
printf '#!/bin/sh\nexit 0\n' > "$BIN"; chmod +x "$BIN"

# 用 SUB_URL（不是 SUB_SNAP1）：2026-09-16 核对实际数据后 snap1/snap2 改为
# type:file 静态快照，chill.sh 的 render() 现在只校验+导出 SUB_URL 这一个。
SUB_URL='https://ok.example/a'
if render; then
  ok "正常值渲染成功"
  body=$(cat "$CONFIG")
  case "$body" in *'${'*) ng "仍有未替换占位符" ;; *) ok "占位符全部替换" ;; esac
  # 光「没有残留 ${...}」不够：awk 对未导出进子进程环境的变量会替换成空字符串，
  # 那样这条断言照样会通过、但值是错的（空的）。之前 render() 少导出 SUB_URL
  # 到 awk 环境这个真实 bug 就是被这个盲区放过的，所以这里必须直接查替换后的值。
  has "SUB_URL 已注入正确的值（不是空字符串替换）" "$body" 'https://ok.example/a'
  has "CN_CIDR 已注入（末尾连逗号）" "$body" '1.0.2.0/23,'
  has "filter 已注入"  "$body" '(?i)'
else
  ng "正常值渲染失败"
fi

SUB_URL="https://bad.example/'quote"
if render 2>/dev/null; then ng "含单引号的订阅值应被拒绝渲染"; else ok "含单引号时拒绝渲染"; fi
SUB_URL='https://ok.example/a'

# 校验不过时必须保留旧配置，不能把正在用的配置换坏
printf '#!/bin/sh\nexit 1\n' > "$BIN"; chmod +x "$BIN"
cp "$CONFIG" "$CHILL_DIR/run/prev.yaml"
render 2>/dev/null
if cmp -s "$CONFIG" "$CHILL_DIR/run/prev.yaml"; then ok "校验失败时保留旧配置"; else ng "校验失败却覆盖了旧配置"; fi
printf '#!/bin/sh\nexit 0\n' > "$BIN"; chmod +x "$BIN"

# ─────────────────────────────────────────────────────────────────────────────
sec "direct_dns4：直连 DNS 补上运营商的 IPv4"

# 10-03 真机的样子：/etc/resolv.conf 只剩 v6，v4 只在 resolv.conf.auto 里
RESOLV_AUTO="$CHILL_DIR/resolv.auto"; RESOLV_SYS="$CHILL_DIR/resolv.sys"
printf '# Interface zte_wan\nnameserver 222.66.251.8\nnameserver 116.236.159.8\n# Interface zte_wan6\nnameserver 240e:58:c000:1700:222:66:251:8\n' > "$RESOLV_AUTO"
printf 'nameserver 240e:58:c000:1700:222:66:251:8\nnameserver 116.236.159.8\n' > "$RESOLV_SYS"
eq "只取 v4、去重、保持顺序" "$(direct_dns4)" ', "222.66.251.8", "116.236.159.8"'
printf 'nameserver 240e::1\n' > "$RESOLV_AUTO"; printf 'nameserver fe80::1\n' > "$RESOLV_SYS"
eq "没有 v4 时输出空" "$(direct_dns4)" ""
rm -f "$RESOLV_AUTO" "$RESOLV_SYS"
eq "文件都不在时输出空" "$(direct_dns4)" ""
printf 'nameserver 1.1.1.1\nnameserver 1.1.1.2\nnameserver 1.1.1.3\nnameserver 1.1.1.4\nnameserver 1.1.1.5\n' > "$RESOLV_AUTO"
eq "最多 4 个" "$(direct_dns4)" ', "1.1.1.1", "1.1.1.2", "1.1.1.3", "1.1.1.4"'

printf 'direct-nameserver: ["system://"${DIRECT_DNS_EXTRA}]\n' > "$CHILL_DIR/template.yaml"
printf 'nameserver 10.1.2.3\n' > "$RESOLV_AUTO"
render; eq "渲染进 direct-nameserver" "$(cat "$CONFIG")" 'direct-nameserver: ["system://", "10.1.2.3"]'
eq "记下渲染时用的值" "$RENDERED_DNS_EXTRA" ', "10.1.2.3"'
: > "$RESOLV_AUTO"
render; eq "拿不到 v4 时退回只有 system://" "$(cat "$CONFIG")" 'direct-nameserver: ["system://"]'
rm -f "$RESOLV_AUTO"

# ─────────────────────────────────────────────────────────────────────────────
sec "restore_dns：幂等与触发条件"

# 这里测的是 chill.sh 里**真实的** restore_dns。
# 早先的写法是在单测里重新定义整个 restore_dns 再去断言，那等于测自己写的替身，
# 永远会通过、毫无价值。改为只注入它依赖的两个外部路径（fixture 配置 + stub
# init 脚本），函数体本身完全不动。
DHCP_BACKUP="$CHILL_DIR/dhcp.backup"
DHCP_CONF="$CHILL_DIR/etc-config-dhcp"
DNSMASQ_INIT="$CHILL_DIR/stub-dnsmasq-init"
RELOAD_LOG="$CHILL_DIR/reloads"

: > "$RELOAD_LOG"
cat > "$DNSMASQ_INIT" <<STUB
#!/bin/sh
echo "\$1" >> "$RELOAD_LOG"
STUB
chmod +x "$DNSMASQ_INIT"
reloads() { wc -l < "$RELOAD_LOG" | tr -d ' '; }

printf 'config dnsmasq\n' > "$DHCP_BACKUP"
printf 'config dnsmasq\n' > "$DHCP_CONF"

restore_dns; eq "内容一致时不 reload" "$(reloads)" "0"

printf 'config dnsmasq\noption noresolv 1\n' > "$DHCP_CONF"
restore_dns; eq "内容不同时回滚并 reload 一次" "$(reloads)" "1"
if cmp -s "$DHCP_BACKUP" "$DHCP_CONF"; then ok "回滚后内容与备份一致"; else ng "回滚后内容仍不一致"; fi

restore_dns; eq "回滚后再调不重复 reload（幂等）" "$(reloads)" "1"

# 没有备份时必须安全返回，而不是把配置清掉
rm -f "$DHCP_BACKUP"
printf 'config dnsmasq\noption noresolv 1\n' > "$DHCP_CONF"
if restore_dns; then ok "无备份时安全返回 0"; else ng "无备份时返回了失败"; fi
eq "无备份时不 reload" "$(reloads)" "1"
has "无备份时不改动现有配置" "$(cat "$DHCP_CONF")" 'noresolv'

# ─────────────────────────────────────────────────────────────────────────────
sec "AI 闸门：CHILL 停下时同一次重启里封上，确认落地后才放"

AI_LIST="$CHILL_DIR/ruleset/ai_domain.list"
AI_BLOCK="$CHILL_DIR/dnsmasq.d/luyaz-ai-block.conf"
AI_OK="$CHILL_DIR/ai-ok"
AI_LOCK="$CHILL_DIR/ai-gate.lock"
mkdir -p "$CHILL_PROC/net"
printf 'Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\nrmnet_data0\t00000000\t00000000\t0001\t0\t0\t0\t00000000\n' > "$CHILL_PROC/net/route"
ip() { [ "$*" = "-4 -o addr show dev rmnet_data0" ] && echo "9: rmnet_data0    inet 10.20.30.40/30 scope global rmnet_data0"; }
uci() { :; }
printf '+.openai.com\nai.google.dev\n# 注释\n+.Bad_Name.com\nnodot\r\n+.claude.ai\n' > "$AI_LIST"
printf '100.00 0.00\n' > "$CHILL_PROC/uptime"
restarts() { grep -c '^restart$' "$RELOAD_LOG" | tr -d ' '; }
plain_reloads() { grep -c '^reload$' "$RELOAD_LOG" | tr -d ' '; }
: > "$RELOAD_LOG"
printf 'config dnsmasq\n' > "$DHCP_BACKUP"; cp "$DHCP_BACKUP" "$DHCP_CONF"

CHILL_AI_STRICT=0
restore_dns
eq "没开：不写封锁文件" "$([ -e "$AI_BLOCK" ] && echo y || echo n)" n
eq "没开：dnsmasq 不动" "$(restarts)/$(plain_reloads)" "0/0"

CHILL_AI_STRICT=1
restore_dns
eq "开了、没确认：封上" "$([ -e "$AI_BLOCK" ] && echo y || echo n)" y
eq "封锁文件变了就 restart（reload 不重读 conf-dir）" "$(restarts)/$(plain_reloads)" "1/0"
has "规则集的 +. 去掉前缀" "$(cat "$AI_BLOCK")" 'address=/openai.com/'
has "规则集的完整网域照写" "$(cat "$AI_BLOCK")" 'address=/ai.google.dev/'
has "内置底单也在" "$(cat "$AI_BLOCK")" 'address=/chatgpt.com/'
eq "坏行（注释、大写下划线、无点）丢掉" "$(grep -c 'Bad_Name\|nodot\|#' "$AI_BLOCK")" 0
eq "重复的只写一次" "$(grep -c '^address=/claude.ai/$' "$AI_BLOCK")" 1
restore_dns
eq "再停一次：不重复 restart（幂等）" "$(restarts)" 1

printf 'config dnsmasq\noption noresolv 1\n' > "$DHCP_CONF"
restore_dns
eq "DNS 回滚、封锁不变：照旧 reload" "$(restarts)/$(plain_reloads)" "1/1"

rm -f "$AI_LIST"
CHILL_AI_STRICT=0; restore_dns; CHILL_AI_STRICT=1
restore_dns
has "规则集不见了：内置底单照样封" "$(cat "$AI_BLOCK")" 'address=/openai.com/'
printf '+.openai.com\n+.claude.ai\n' > "$AI_LIST"

: > "$RELOAD_LOG"
printf 'v2 boot-a 50 500 direct rmnet_data0 10.20.30.40 TW\n' > "$AI_OK"
restore_dns
eq "核心确认过这条连接：放" "$([ -e "$AI_BLOCK" ] && echo y || echo n)" n
eq "放也要 restart" "$(restarts)" 1
for bad in 'v2 boot-a 50 500 direct rmnet_data0 10.20.30.41 TW' \
           'v2 boot-b 50 500 direct rmnet_data0 10.20.30.40 TW' \
           'v2 boot-a 50 90 direct rmnet_data0 10.20.30.40 TW' \
           'v2 boot-a 150 500 direct rmnet_data0 10.20.30.40 TW' \
           'v2 boot-a 50 500 global rmnet_data0 10.20.30.40 TW' \
           'v2 boot-a 50 500 direct rmnet_data0 10.20.30.40' \
           'garbage'; do
  printf '%s\n' "$bad" > "$AI_OK"
  eq "确认不算数（换了 IP/开机/过期/未来/别的路径/缺字段/坏档）：$bad" "$(ai_want_down)" on
done
rm -f "$AI_OK"
eq "没有默认路由：封" "$(CHILL_PROC=/nonexistent ai_want_down)" on

: > "$RELOAD_LOG"
printf 'garbage\n' > "$AI_OK"; restore_dns; rm -f "$AI_OK"
printf 'rule\n' > "$MODE_FILE"
apply_dns
eq "CHILL 接管 DNS（rule）：放，同一次 restart" "$([ -e "$AI_BLOCK" ] && echo y || echo n)/$(restarts)/$(plain_reloads)" "n/2/0"
printf 'global\n' > "$MODE_FILE"
apply_dns
eq "接管时是全局模式：先封，等核心经 7894 确认" "$([ -e "$AI_BLOCK" ] && echo y || echo n)" y
rm -f "$AI_BLOCK"
ai_gate_tick
eq "运行中复查：全局交给核心，不动" "$([ -e "$AI_BLOCK" ] && echo y || echo n)" n
printf 'direct\n' > "$MODE_FILE"
ai_gate_tick
eq "运行中复查：全部直连且没确认，封" "$([ -e "$AI_BLOCK" ] && echo y || echo n)" y
printf 'rule\n' > "$MODE_FILE"
_r0=$(restarts)
ai_gate_tick
eq "运行中复查：回到 rule，放并 restart" "$([ -e "$AI_BLOCK" ] && echo y || echo n)/$(( $(restarts) - _r0 ))" "n/1"
ai_gate_tick
eq "运行中复查：没变化不 restart" "$(( $(restarts) - _r0 ))" 1
# 设备上的 busybox flock 只认 -sxun（没有 -w，10-24 真机）
flock() { case "$1" in -[sxun]) return 0 ;; *) echo "flock: unrecognized option" >&2; return 1 ;; esac; }
_fl=$( { ai_gate_write on; ai_gate_write off; } 2>&1 >/dev/null )
eq "flock 只用 busybox 认得的选项" "$_fl" ""
unset -f flock
CHILL_AI_STRICT=0
printf 'direct\n' > "$MODE_FILE"
ai_gate_tick
eq "关掉开关：不封" "$([ -e "$AI_BLOCK" ] && echo y || echo n)" n
rm -f "$MODE_FILE" "$AI_LIST"
unset -f ip uci
printf '10000.00 0.00\n' > "$CHILL_PROC/uptime"

# ─────────────────────────────────────────────────────────────────────────────
sec "event：状态迁移留痕（DT3）"

rm -f "$EVENTS"; last_event=''
event "overheat cpuss=86 -> direct"
if [ -f "$EVENTS" ]; then ok "写出 events.log"; else ng "未写出 events.log"; fi
has "记录含状态与原因" "$(cat "$EVENTS" 2>/dev/null)" 'overheat'
has "记录含 UTC 时间戳" "$(cat "$EVENTS" 2>/dev/null)" 'T'

# 同一状态重复上报不应反复记账，否则 20 行很快被同一条刷满
event "overheat cpuss=86 -> direct"
eq "同状态去重" "$(wc -l < "$EVENTS" | tr -d ' ')" "1"

event "recover cpuss=63 -> running"
eq "状态变化后追加" "$(wc -l < "$EVENTS" | tr -d ' ')" "2"

# 截断到 20 行：触屏底部只渲染最近 1 条，后台展开 20 条
i=0
while [ $i -lt 40 ]; do i=$((i+1)); event "probe-${i} -> direct"; done
eq "截断到 20 行" "$(wc -l < "$EVENTS" | tr -d ' ')" "20"
has "保留的是最新一条" "$(tail -1 "$EVENTS")" 'probe-40'

# 路径不可写时必须静默失败，不能把错误灌进 procd 日志
EVENTS_SAVE="$EVENTS"
EVENTS=/nonexistent-dir-xyz/events.log; last_event=''
err=$(event "x -> direct" 2>&1 >/dev/null)
eq "不可写时无 stderr 噪音" "$err" ""
EVENTS="$EVENTS_SAVE"

# ─────────────────────────────────────────────────────────────────────────────
sec "note_crash：崩溃计数与窗口重置"

# 这些只在故障时才执行，平时看不出对错，正是最需要回归保护的部分。
crash_n=0; crash_t0=-1
note_crash; eq "第 1 次崩溃后计数为 1" "$crash_n" "1"
note_crash; eq "第 2 次崩溃后计数为 2" "$crash_n" "2"
if note_crash; then ng "第 3 次应返回非 0（达上限）"; else ok "第 3 次达上限返回非 0"; fi
eq "达上限时计数为 3" "$crash_n" "3"

# 距首次崩溃超过窗口应重置。此前「启动即退出」那条路径没有重置逻辑，
# 累计 3 次就永久 gaveup，哪怕间隔几小时——这条断言就是防它回归。
crash_t0=$(( $(boot_now) - CRASH_WINDOW - 60 ))
if note_crash; then ok "超窗后重置并可继续重试"; else ng "超窗后仍判定为达上限"; fi
eq "超窗重置后计数归 1" "$crash_n" "1"

# ─────────────────────────────────────────────────────────────────────────────
sec "next_backoff：2 / 5 / 15 分钟递增"

degrade_n=0; backoff=0
# 用范围断言而不是精确相等：backoff 是拿函数内外两次 date +%s 相减算出来的，
# 只要恰好跨秒就会得到 119 或 121。精确比对会让这几条随机变红，然后被
# 「重跑一下就好了」糊弄过去——那种用例比没有用例更糟。
near() { # near <名称> <实际> <期望> [容差秒]
  _tol=${4:-2}
  _d=$(( $2 - $3 )); [ "$_d" -lt 0 ] && _d=$(( 0 - _d ))
  if [ "$_d" -le "$_tol" ]; then ok "$1"; else ng "$1（期望 ~$3 实际 $2）"; fi
}

now=$(boot_now); next_backoff
w1=$((backoff - now)); near "第 1 次退避约 120 秒" "$w1" 120
now=$(boot_now); next_backoff
w2=$((backoff - now)); near "第 2 次退避约 300 秒" "$w2" 300
now=$(boot_now); next_backoff
w3=$((backoff - now)); near "第 3 次退避约 900 秒" "$w3" 900
now=$(boot_now); next_backoff
w4=$((backoff - now)); near "第 4 次及以后保持约 900 秒" "$w4" 900
if [ "$w1" -ge "$DWELL_SECONDS" ]; then ok "退避不低于驻留下限（${DWELL_SECONDS}s）"; else ng "退避低于驻留下限"; fi

# ─────────────────────────────────────────────────────────────────────────────
sec "truncate_log：超限才截断"

LOG="$CHILL_DIR/trunc.log"
: > "$LOG"; i=0
while [ $i -lt 50 ]; do i=$((i+1)); echo "line-${i}" >> "$LOG"; done
small=$(wc -c < "$LOG" | tr -d ' ')
truncate_log
eq "未超限时不动" "$(wc -c < "$LOG" | tr -d ' ')" "$small"

# 撑到超过 512KB
i=0
while [ $i -lt 700 ]; do i=$((i+1)); head -c 1024 /dev/zero | tr '\0' 'x' >> "$LOG"; echo >> "$LOG"; done
before=$(wc -c < "$LOG" | tr -d ' ')
if [ "$before" -gt "$LOG_MAX_BYTES" ]; then ok "构造出超限日志（${before} 字节）"; else ng "构造失败，未超限"; fi
truncate_log
after=$(wc -c < "$LOG" | tr -d ' ')
if [ "$after" -lt "$before" ]; then ok "超限后被截断（${before} -> ${after}）"; else ng "超限却未截断"; fi
if [ "$after" -le "$LOG_MAX_BYTES" ]; then ok "截断后不超过上限"; else ng "截断后仍超上限"; fi
if [ -f "$LOG.tmp" ]; then ng "遗留了 .tmp 临时文件"; else ok "未遗留 .tmp"; fi

# 核心进程一直开着日志（>>）：截断后它的写入要落进同一个文件，不能写进删掉的旧 inode
i=0
while [ $i -lt 700 ]; do i=$((i+1)); head -c 1024 /dev/zero | tr '\0' 'y' >> "$LOG"; echo >> "$LOG"; done
ino_before=$(ls -i "$LOG" | awk '{print $1}')
exec 3>>"$LOG"
truncate_log
echo "core-still-writing" >&3
exec 3>&-
eq "截断后还是同一个文件（inode 不变）" "$(ls -i "$LOG" | awk '{print $1}')" "$ino_before"
has "开着的 fd 写进了当前日志" "$(tail -n 1 "$LOG")" "core-still-writing"
if [ "$(wc -c < "$LOG" | tr -d ' ')" -le "$LOG_MAX_BYTES" ]; then ok "fd 续写后仍不超上限"; else ng "fd 续写后超上限（写到了偏移处？）"; fi
LOG=/tmp/chill-unit.log

# ─────────────────────────────────────────────────────────────────────────────
sec "档位：读取、生效值、渲染"

rm -f "$PROFILE_FILE" "$THERMAL_ECO"
eq "没有档位文件 = standard" "$(profile_read)" "standard"
echo turbo > "$PROFILE_FILE"; eq "不认识的值当 standard" "$(profile_read)" "standard"
echo eco > "$PROFILE_FILE";   eq "eco 读出来是 eco" "$(profile_read)" "eco"
echo perf > "$PROFILE_FILE";  : > "$THERMAL_ECO"
eq "温度降档时生效值是 eco" "$(profile_effective)" "eco"
eq "温度降档不改用户选的档位" "$(profile_read)" "perf"
rm -f "$THERMAL_ECO" "$PROFILE_FILE"

cat > "$CHILL_DIR/template.yaml" <<'YAML'
tcp-concurrent: ${TCP_CONCURRENT}
keep-alive-interval: ${KEEPALIVE}
hc: {interval: ${HC_INTERVAL}, timeout: 3000}
YAML
render; eq "standard 渲染出以前那套值" "$(cat "$CONFIG")" "$(printf 'tcp-concurrent: false\nkeep-alive-interval: 600\nhc: {interval: 1800, timeout: 3000}')"
eq "standard 不限核" "$GMP" ""
echo eco > "$PROFILE_FILE"; render
eq "eco 渲染" "$(cat "$CONFIG")" "$(printf 'tcp-concurrent: false\nkeep-alive-interval: 900\nhc: {interval: 3600, timeout: 3000}')"
eq "eco 限 2 核" "$GMP" "2"
echo perf > "$PROFILE_FILE"; render
eq "perf 渲染" "$(cat "$CONFIG")" "$(printf 'tcp-concurrent: true\nkeep-alive-interval: 120\nhc: {interval: 600, timeout: 3000}')"
rm -f "$PROFILE_FILE"

# ─────────────────────────────────────────────────────────────────────────────
sec "家里出口：真实模板的标签行（E3 T2）"

# 用仓库里真实的 template.yaml：小样本模板测不出标签行漏删、规则顺序这类问题。
TEMPLATE="$HERE/../template.yaml"
HOME_IP_LIST="$CHILL_DIR/home_ip.list"; rm -f "$HOME_IP_LIST"
unset CHILL_HOME_EXIT CHILL_HOME_MEDIA CHILL_HOME_IP CHILL_HOME_REST CHILL_HOME_PROBE_URL
# 行号（找不到给 0），用来比规则先后
ln_of() { n=$(grep -nF -- "$2" "$1" | head -1 | cut -d: -f1); echo "${n:-0}"; }
# 去掉整行注释（模板头和说明里本来就写着标签名、占位符名）
body() { grep -v '^[[:space:]]*#' "$CONFIG"; }
no_home() { # no_home <名称>：渲染结果里没有任何家里出口的东西
  b=$(body)
  case "$b" in *'家里'*|*media_*|*home_ip*|*'#@'*) ng "$1" ;; *) ok "$1" ;; esac
}

render; eq "没配家里出口：渲染成功" "$?" "0"
no_home "没配家里出口：一行家里的都没有"
has "fake-ip-filter 是 rule 模式" "$(cat "$CONFIG")" 'fake-ip-filter-mode: rule'
has "fake-ip-filter 末尾 MATCH,fake-ip" "$(cat "$CONFIG")" '- MATCH,fake-ip'
if body | grep -q '\${[A-Z_]*}'; then ng "仍有未替换占位符"; else ok "占位符全部替换"; fi

CHILL_HOME_MEDIA=1; CHILL_HOME_IP=1; CHILL_HOME_REST=1
render; no_home "只开开关、没有 CHILL_HOME_EXIT：照样一行都没有"

for bad in '10.0.0.1' '10.0.0.1:' ':1080' '1.2.3.4:70000' '1.2.3.4:0' '1.2.3.256:80' '1.2.3:80' 'a.b.c.d:1' '1.2.3.4:80:1' "1.2.3.4:1080'"; do
  CHILL_HOME_EXIT="$bad"
  if render; then no_home "坏地址 [$bad]：不渲染、render 不失败"; else ng "坏地址 [$bad] 让 render 失败了"; fi
done
unset CHILL_HOME_MEDIA CHILL_HOME_IP CHILL_HOME_REST

CHILL_HOME_EXIT='192.0.2.10:1080'
render; b=$(body)
has "配了地址：有 家里" "$b" '{name: 家里, type: socks5, server: 192.0.2.10, port: 1080, udp: false, interface-name: tailscale0}'
has "配了地址：版权组" "$b" '{name: 家里出口, type: fallback, proxies: [家里, DIRECT], url: '"'"'http://connect.rom.miui.com/generate_204'"'"''
has "配了地址：家里 IP 组退回 🐟" "$b" 'proxies: [家里, 🐟 漏网之鱼]'
has "配了地址：版权规则集先下好" "$b" 'media_iqiyi:'
case "$b" in *'#@'*) ng "标签没去掉" ;; *) ok "标签都去掉了" ;; esac
case "$b" in *'RULE-SET,media_cn,'*|*'MATCH,家里IP'*|*home_ip*) ng "开关都关着却有家里的规则" ;; *) ok "开关都关着：没有家里的规则" ;; esac

CHILL_HOME_PROBE_URL='http://example.cn/204'
render; has "探测地址来自 env" "$(cat "$CONFIG")" "url: 'http://example.cn/204'"
CHILL_HOME_PROBE_URL="http://x/'a"
render; has "探测地址含单引号：用默认" "$(cat "$CONFIG")" "url: 'http://connect.rom.miui.com/generate_204'"
unset CHILL_HOME_PROBE_URL

CHILL_HOME_MEDIA=1
render
fm=$(ln_of "$CONFIG" '- RULE-SET,media_iqiyi,fake-ip'); fc=$(ln_of "$CONFIG" '- RULE-SET,cn_domain,real-ip')
[ "$fm" -gt 0 ] && [ "$fm" -lt "$fc" ] && ok "版权 fake-ip 在 cn_domain real-ip 之前" || ng "版权 fake-ip 位置不对（${fm} / ${fc}）"
rq=$(ln_of "$CONFIG" '- AND,((NETWORK,UDP),(DST-PORT,443),(RULE-SET,media_cn)),REJECT')
rm_=$(ln_of "$CONFIG" '- RULE-SET,media_cn,家里出口')
rc=$(ln_of "$CONFIG" '- RULE-SET,cn_domain,DIRECT')
rp=$(ln_of "$CONFIG" '- DOMAIN,detectportal.firefox.com,DIRECT')
[ "$rp" -gt 0 ] && [ "$rp" -lt "$rq" ] && [ "$rq" -lt "$rm_" ] && [ "$rm_" -lt "$rc" ] \
  && ok "门户检测 < 拒绝 QUIC < 版权规则 < cn_domain" || ng "版权规则位置不对（${rp} ${rq} ${rm_} ${rc}）"
eq "VoWiFi 仍是第一条规则" "$(grep -A1 '^rules:' "$CONFIG" | tail -1; grep -m1 '^  - DOMAIN-SUFFIX,3gppnetwork.org' "$CONFIG")" "$(printf '\n  - DOMAIN-SUFFIX,3gppnetwork.org,📞 VoWiFi')"
case "$(cat "$CONFIG")" in *home_ip*|*'MATCH,家里IP'*) ng "只开版权却带出了别的" ;; *) ok "只开版权：没有家里 IP 和其余" ;; esac
unset CHILL_HOME_MEDIA

CHILL_HOME_IP=1
render; case "$(cat "$CONFIG")" in *home_ip*) ng "表不存在却渲染了 home_ip" ;; *) ok "表不存在：家里 IP 不渲染" ;; esac
printf '\n  \n' > "$HOME_IP_LIST"
render; case "$(cat "$CONFIG")" in *home_ip*) ng "表只有空白却渲染了 home_ip" ;; *) ok "表只有空白：不渲染" ;; esac
printf '+.example-bank.cn\n' > "$HOME_IP_LIST"
render; b=$(cat "$CONFIG")
has "表有内容：home_ip provider" "$b" 'home_ip:        {type: file, behavior: domain, format: text, path: ./home_ip.list}'
fi_=$(ln_of "$CONFIG" '- RULE-SET,home_ip,fake-ip')
ri=$(ln_of "$CONFIG" '- RULE-SET,home_ip,家里IP')
[ "$fi_" -gt 0 ] && [ "$fi_" -lt "$fc" ] && [ "$ri" -gt 0 ] && [ "$ri" -lt "$rc" ] \
  && ok "家里 IP 的 fake-ip 和规则都在 cn_domain 之前" || ng "家里 IP 位置不对（${fi_} ${ri}）"
unset CHILL_HOME_IP; rm -f "$HOME_IP_LIST"

CHILL_HOME_REST=1
render
ra=$(ln_of "$CONFIG" '- RULE-SET,ai_domain,🤖 AI'); rr=$(ln_of "$CONFIG" '- MATCH,家里IP')
eq "其余：ai_domain 后面第一条规则就是 MATCH" "$(sed -n "$((ra + 1)),\$p" "$CONFIG" | grep -v '^[[:space:]]*#' | head -1)" '  - MATCH,家里IP'
[ "$rr" -gt "$ra" ] && ok "其余：MATCH 在 ai_domain 之后" || ng "其余：MATCH 位置不对（${ra} / ${rr}）"
has "其余：原来的 MATCH 还在最后" "$(tail -1 "$CONFIG")" '- MATCH,🐟 漏网之鱼'
unset CHILL_HOME_REST CHILL_HOME_EXIT

# 常驻的 chill.sh run 只在启动时 source 过 env；agent 改了 env 以后，它自己再渲染也要用新值。
ENV_FILE="$CHILL_DIR/chill.env"
printf "SUB_URL='https://ok.example/a'\nCHILL_HOME_EXIT='192.0.2.10:1080'\nCHILL_HOME_MEDIA=1\n" > "$ENV_FILE"
render; has "env 文件里开了版权：渲染出来" "$(body)" '- RULE-SET,media_cn,家里出口'
printf "SUB_URL='https://ok.example/a'\nCHILL_HOME_EXIT='192.0.2.10:1080'\n" > "$ENV_FILE"
render; case "$(body)" in *'RULE-SET,media_cn,家里出口'*) ng "env 里关了，渲染却还开着（用的是旧变量）" ;; *) ok "env 里关了：下次渲染就关" ;; esac
eq "env 现读不改别的变量" "$SUB_URL" 'https://ok.example/a'
: > "$ENV_FILE"
render; no_home "env 里删掉地址：整个不出现"
CHILL_HOME_EXIT='192.0.2.10:1080'; CHILL_HOME_MEDIA=1
printf 'if then\n' > "$ENV_FILE"
render; has "env 文件坏了：沿用现在的设置" "$(body)" '- RULE-SET,media_cn,家里出口'
rm -f "$ENV_FILE"; ENV_FILE=/nonexistent/chill.env
unset CHILL_HOME_EXIT CHILL_HOME_MEDIA

# ─────────────────────────────────────────────────────────────────────────────
sec "出口模式：记下与恢复"

API_MODE_OUT='{"mode":"direct","port":0}'; PATCHED=""
curl() {
  case "$*" in
    *PATCH*) PATCHED="$*" ;;
    *) printf '%s' "$API_MODE_OUT" ;;
  esac
}
rm -f "$MODE_FILE"
save_mode; eq "记下 direct" "$(cat "$MODE_FILE")" "direct"
API_MODE_OUT='{"mode":"rule"}'
restore_mode; has "核心回到 rule 时恢复 direct" "$PATCHED" '{"mode":"direct"}'
PATCHED=""; echo rule > "$MODE_FILE"
restore_mode; eq "记下的是 rule 时不用恢复" "$PATCHED" ""
API_MODE_OUT=''; save_mode; eq "读不到模式时不覆盖记录" "$(cat "$MODE_FILE")" "rule"
unset -f curl

# ─────────────────────────────────────────────────────────────────────────────
sec "thermal_step：默认关闭、降档、回温"

# 打桩：只记录调用，不真的停核/清规则
CALLS=""
kill_core() { CALLS="$CALLS kill"; }
flush() { CALLS="$CALLS flush"; }
save_mode() { CALLS="$CALLS save"; }
crash_n=0

CHILL_TEMP_WARM=""
if thermal_step 90000; then ng "没设 CHILL_TEMP_WARM 时不该降档"; else ok "没设 CHILL_TEMP_WARM 时不降档"; fi
[ -f "$THERMAL_ECO" ] && ng "不该留下降档标记" || ok "没有降档标记"

CHILL_TEMP_WARM=67000
if thermal_step 60000; then ng "低于 WARM 不该降档"; else ok "低于 WARM 不降档"; fi
if thermal_step 68000; then ok "到 WARM 降档并要求 break"; else ng "到 WARM 没降档"; fi
[ -f "$THERMAL_ECO" ] && ok "留下降档标记" || ng "没有降档标记"
has "降档时平稳重启核心（先记模式再停核清理）" "$CALLS" "save kill flush"
eq "平稳重启不算崩溃" "$crash_n" "0"
eq "降档后生效值 eco" "$(profile_effective)" "eco"

CALLS=""
if thermal_step 62000; then ng "只降到 WARM-5°C 不该回档"; else ok "没降够 10°C 不回档"; fi
if thermal_step 50000; then ng "刚凉下来不该马上回档"; else ok "凉下来先开始计时"; fi
cool_since=$(( $(boot_now) - COOL_HOLD - 1 ))
if thermal_step 50000; then ok "凉够 ${COOL_HOLD} 秒回到原档位"; else ng "凉够了没回档"; fi
[ -f "$THERMAL_ECO" ] && ng "回档后降档标记还在" || ok "回档后降档标记清掉"
has "回档也平稳重启" "$CALLS" "kill flush"

echo eco > "$PROFILE_FILE"; CALLS=""
if thermal_step 90000; then ng "本来就是 eco 不该再重启"; else ok "本来就是 eco 不动"; fi
rm -f "$PROFILE_FILE"; CHILL_TEMP_WARM=""

# heat_req：读 core 的请求，没有文件或空文件 = 空
HEAT_REQ="$CHILL_DIR/heat-req"
rm -f "$HEAT_REQ"; heat_req; eq "没有请求文件 = 空" "$h" ""
: > "$HEAT_REQ"; heat_req; eq "空文件 = 空" "$h" ""
echo critical > "$HEAT_REQ"; heat_req; eq "读到 critical" "$h" "critical"
rm -f "$HEAT_REQ"

# core 温控的请求（thermal-care.md）：没设 CHILL_TEMP_WARM 也照降；请求还在就不回档
CALLS=""
if thermal_step 40000 ""; then ng "没有请求不该降档"; else ok "没有请求不降档"; fi
if thermal_step 40000 hot; then ok "core 说 hot 就降档"; else ng "core 说 hot 没降档"; fi
[ -f "$THERMAL_ECO" ] && ok "core 请求也留下降档标记" || ng "core 请求没留降档标记"
has "core 请求降档也平稳重启" "$CALLS" "save kill flush"
CALLS=""
cool_since=$(( $(boot_now) - COOL_HOLD - 1 ))
if thermal_step 40000 critical; then ng "请求还在不该回档"; else ok "请求还在不回档"; fi
if thermal_step 40000 ""; then ng "请求刚撤不该马上回档"; else ok "请求撤了先开始计时"; fi
cool_since=$(( $(boot_now) - COOL_HOLD - 1 ))
if thermal_step 40000 ""; then ok "请求撤了凉够 ${COOL_HOLD} 秒回档"; else ng "请求撤了凉够了没回档"; fi
[ -f "$THERMAL_ECO" ] && ng "回档后降档标记还在" || ok "回档后降档标记清掉"

# ─────────────────────────────────────────────────────────────────────────────
sec "wait_core_ready：起核心后等 API 和 dns.listen，代替固定 sleep 5"

# curl 在管道里跑（子 shell），计数和参数只能记到文件里。sleep/date 打桩成假时钟，
# 测试不真的等。kill 打桩：KILL_ALIVE_UNTIL 次之后核心「退出」。
CURL_LOG="$CHILL_DIR/curl.log"
curl() {
  echo "$*" >> "$CURL_LOG"
  _n=$(wc -l < "$CURL_LOG")
  if [ "$_n" -ge "$CURL_OK_FROM" ]; then printf 200; else printf 000; fi
}
dns_answers() { [ "$DNS_OK" = 1 ]; }
FAKE_NOW=1000000; SLEPT=0; KILLS=0
sleep() { SLEPT=$((SLEPT+1)); FAKE_NOW=$((FAKE_NOW+$1)); }
boot_now() { echo "$FAKE_NOW"; }
kill() { KILLS=$((KILLS+1)); [ "$KILLS" -le "$KILL_ALIVE_UNTIL" ]; }
ready_case() { # ready_case <curl 第几次起成功> <dns 能否应答> <核心活几次检查>
  : > "$CURL_LOG"; CURL_OK_FROM=$1; DNS_OK=$2; KILL_ALIVE_UNTIL=$3; SLEPT=0; KILLS=0
  wait_core_ready 4242; RC=$?
}

CHILL_SECRET=s3cret
ready_case 1 1 99
eq "第一次就就绪：返回 0" "$RC" "0"
eq "第一次就就绪：不睡" "$SLEPT" "0"
has "探针带鉴权头（9-17 事故）" "$(cat "$CURL_LOG")" "Authorization: Bearer s3cret"
has "单次探测 -m 1 限时" "$(cat "$CURL_LOG")" "-m 1 "
has "探的是 /version" "$(cat "$CURL_LOG")" "127.0.0.1:9999/version"

ready_case 3 1 99
eq "第 3 次就绪：返回 0" "$RC" "0"
eq "第 3 次就绪：睡 2 次" "$SLEPT" "2"

ready_case 1 0 99
eq "API 通但 DNS 不应答：到时返回 1" "$RC" "1"
eq "最多等 CORE_READY_MAX 秒" "$SLEPT" "$CORE_READY_MAX"
[ "$CORE_READY_MAX" -ge 5 ] && ok "上限不短于原来的 5 秒" || ng "上限短于 5 秒"

ready_case 99 1 3
eq "核心中途退出：返回 2" "$RC" "2"
eq "核心退出后不再探测（活着的 3 次各探一次）" "$(wc -l < "$CURL_LOG" | tr -d ' ')" "3"

ready_case 1 1 0
eq "核心起来就退出：返回 2、不探测" "$RC:$(wc -l < "$CURL_LOG" | tr -d ' ')" "2:0"

CHILL_SECRET=''
unset -f curl dns_answers sleep boot_now kill

# ─────────────────────────────────────────────────────────────────────────────
sec "国内直连：nft 集合代替 route-exclude-address 的 CN 段（CHILL_CN_SET）"

# 前面几节把 flush/kill_core 等换成了替身，这里重新加载真函数，再把路径和外部命令接到 fixture。
CHILL_LIB_ONLY=1 . "$SUT"
CHILL_DIR=/tmp/chill-unit-fixture; LOG=/tmp/chill-unit.log
STATE="$CHILL_DIR/chill.state"; CORE_PID="$CHILL_DIR/core.pid"
PROFILE_FILE="$CHILL_DIR/profile"; MODE_FILE="$CHILL_DIR/mode"; THERMAL_ECO="$CHILL_DIR/thermal-eco"
TEMPLATE="$HERE/../template.yaml"; CN_LIST="$CHILL_DIR/ruleset/cn.list"   # 真实模板
CONFIG="$CHILL_DIR/run/config.yaml"; RUN_DIR="$CHILL_DIR/run"
CN_SET_MARK="$CHILL_DIR/cnset"; CN_NFT="$CHILL_DIR/cn.nft"
BIN=/tmp/chill-unit-mihomo; printf '#!/bin/sh\nexit 0\n' > "$BIN"; chmod +x "$BIN"
SUB_URL='https://ok.example/a'
printf '1.0.1.0/24\n\n1.0.2.0/23\n2400:3200::/32\n' > "$CN_LIST"

NFT_LOG="$CHILL_DIR/nft.log"; IP_LOG="$CHILL_DIR/ip.log"; NFT_FAIL=0; NFT_SEEN=""
nft() { echo "$*" >> "$NFT_LOG"
  if [ "${1:-}" = "-f" ]; then NFT_SEEN=$(cat "$2"); [ "$NFT_FAIL" = 0 ]; else return 0; fi; }
ip() { echo "$*" >> "$IP_LOG"; return 0; }
restore_dns() { :; }; kill_core() { :; }; check_rules_drift() { :; }

out=$(cn_nft_file); rc=$?
eq "nft 脚本：生成成功" "$rc" "0"
has "nft 脚本：区间集合 + auto-merge" "$out" 'flags interval; auto-merge;'
has "nft 脚本：只收 IPv4、跳过空行" "$out" 'elements = { 1.0.1.0/24, 1.0.2.0/23 }'
case "$out" in *2400:*) ng "nft 脚本混进了 IPv6" ;; *) ok "nft 脚本：IPv6 段不进集合" ;; esac
has "nft 脚本：只匹配 br-lan、用 or 打标" "$out" 'iifname "br-lan" ip daddr @cn4 counter meta mark set meta mark or 0x1000000'
has "nft 脚本：排在 legacy mangle（-150）之后" "$out" 'priority -140;'
eq "标记位不碰原厂的 0xff0000 / 0xC0" "$(( CN_MARK & 0xff00c0 ))" "0"
eq "规则优先级在自管区间内、排在 mihomo 的 9000 前" "$(( CN_PRIO >= RULE_LO && CN_PRIO < 9000 && CN_PRIO != BYPASS_PRIO ))" "1"
printf '' > "$CHILL_DIR/empty.list"
( CN_LIST="$CHILL_DIR/empty.list"; cn_nft_file >/dev/null ); eq "cn.list 没有 IPv4 段：生成失败" "$?" "1"

: > "$NFT_LOG"; : > "$IP_LOG"
CHILL_CN_SET=0; cn_set_up
eq "开关关：不碰 nft" "$(wc -l < "$NFT_LOG" | tr -d ' ')" "0"
eq "开关关：cn_set=routes、没有标记文件" "$cn_set:$([ -f "$CN_SET_MARK" ] && echo y || echo n)" "routes:n"
render >/dev/null 2>&1; has "开关关：CN 段照旧写进配置" "$(cat "$CONFIG")" '[1.0.1.0/24, 1.0.2.0/23, 2400:3200::/32,'

CHILL_CN_SET=1; cn_set_up
eq "开关开：cn_set=nft、留下标记文件" "$cn_set:$([ -f "$CN_SET_MARK" ] && echo y || echo n)" "nft:y"
has "开关开：先删旧表再载入" "$(cat "$NFT_LOG")" 'delete table inet chillcn'
has "开关开：载入的就是生成的脚本" "$NFT_SEEN" 'table inet chillcn {'
has "开关开：加 fwmark 规则回 main" "$(cat "$IP_LOG")" 'rule add fwmark 0x1000000/0x1000000 priority 8998 lookup main'
eq "开关开：临时 nft 文件已删" "$([ -f "$CN_NFT" ] && echo y || echo n)" "n"
render >/dev/null 2>&1; body=$(cat "$CONFIG")
case "$body" in *1.0.1.0/24*) ng "开关开：配置里还有 CN 段" ;; *) ok "开关开：CN 段不再写进配置" ;; esac
case "$body" in *'[,'*) ng "开关开：渲染出了「[,」" ;; *) ok "开关开：没有「[,」" ;; esac
has "开关开：私网段照旧" "$body" '10.0.0.0/8'
write_state running ""; has "state 带 cn_set" "$(cat "$STATE")" '"cn_set":"nft"'

: > "$IP_LOG"; NFT_FAIL=1; cn_set_up
eq "nft 载入失败：cn_set=failed、没有标记文件" "$cn_set:$([ -f "$CN_SET_MARK" ] && echo y || echo n)" "failed:n"
case "$(cat "$IP_LOG")" in *'rule add'*) ng "nft 失败后仍加了规则" ;; *) ok "nft 失败：不加规则" ;; esac
render >/dev/null 2>&1; has "nft 失败：退回把 CN 段写进配置" "$(cat "$CONFIG")" '1.0.1.0/24'
NFT_FAIL=0

cn_set_up; : > "$NFT_LOG"; flush
has "flush：删掉 inet chillcn" "$(cat "$NFT_LOG")" 'delete table inet chillcn'
eq "flush：删掉标记文件" "$([ -f "$CN_SET_MARK" ] && echo y || echo n)" "n"
unset -f nft ip restore_dns kill_core check_rules_drift
CHILL_CN_SET=0

sec "停机后写 stopped"
rm -f "$CORE_PID"; mark_stopped
has "mark_stopped：state=stopped" "$(cat "$STATE")" '"state":"stopped"'
has "mark_stopped：没有核心 pid" "$(cat "$STATE")" '"core_pid":0'
has "mark_stopped：没有启动时间" "$(cat "$STATE")" '"started_at":""'
if command -v python3 >/dev/null 2>&1; then
  python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$STATE" && ok "mark_stopped：是合法 JSON" || ng "mark_stopped：不是合法 JSON"
fi

sec "换档位马上写进 chill.state"
write_state running ""
echo eco > "$PROFILE_FILE"; state_profile_now
has "state_profile_now：profile=eco" "$(cat "$STATE")" '"profile":"eco"'
has "state_profile_now：profile_effective=eco" "$(cat "$STATE")" '"profile_effective":"eco"'
has "state_profile_now：别的字段不动" "$(cat "$STATE")" '"state":"running"'
rm -f "$PROFILE_FILE"; state_profile_now
has "state_profile_now：回到 standard" "$(cat "$STATE")" '"profile":"standard","profile_effective":"standard"'
rm -f "$STATE"; state_profile_now; eq "state_profile_now：没有 state 文件就不建" "$([ -f "$STATE" ] && echo y || echo n)" "n"

sec "boot-bound deadlines：租约 / 手动暂停 / 重启 / 坏档"
CHILL_PROC="$CHILL_DIR/proc"
mkdir -p "$CHILL_PROC/sys/kernel/random"
printf 'boot-a\n' > "$CHILL_PROC/sys/kernel/random/boot_id"
printf '100.25 0.00\n' > "$CHILL_PROC/uptime"
LEGACY_PAUSE="$CHILL_DIR/pause_until"; PAUSE_UNTIL="$CHILL_DIR/pause.v2"
IDLE_LEASE="$CHILL_DIR/idle.v2"
rm -f "$IDLE_LEASE"
eq "没有租约：不暂停" "$(idle_paused && echo y || echo n)" "n"
printf 'v2 boot-a 0 180\n' > "$IDLE_LEASE"
eq "租约未到期（0 有效）：暂停" "$(idle_paused && echo y || echo n)" "y"
# date 的 ±8h 不参与任何 deadline，直接让调用 date +%s 的回归失败。
date() { [ "${1:-}" = '+%s' ] && return 99; command date "$@"; }
eq "wall time 不可读不影响租约" "$(idle_paused && echo y || echo n)" "y"
printf '180.00 0.00\n' > "$CHILL_PROC/uptime"
eq "边界到期：恢复" "$(idle_paused && echo y || echo n)" "n"
printf '7200.00 0.00\n' > "$CHILL_PROC/uptime"
eq "休眠计入到期" "$(idle_paused && echo y || echo n)" "n"
printf '100.00 0.00\n' > "$CHILL_PROC/uptime"
for bad in '1790000000' 'v2 a 00 180' 'v2 boot-a 101 180' 'v2 boot-b 0 180' 'v2 boot-a -1 180' 'v2 boot-a 0 180 extra'; do
  printf '%s\n' "$bad" > "$IDLE_LEASE"
  eq "拒绝旧/坏/未来/跨 boot：$bad" "$(idle_paused && echo y || echo n)" "n"
done
pause_for 2
eq "手动暂停 v2" "$(cat "$PAUSE_UNTIL")" 'v2 boot-a 100 220'
eq "同 boot 服务重启仍暂停" "$(manual_paused && echo y || echo n)" y
printf 'boot-b\n' > "$CHILL_PROC/sys/kernel/random/boot_id"
eq "设备重启失效" "$(manual_paused && echo y || echo n)" n
printf 'boot-a\n' > "$CHILL_PROC/sys/kernel/random/boot_id"
printf 'garbage\n' > "$PAUSE_UNTIL"
eq "手动坏档保守保留直连" "$(manual_paused && echo y || echo n)" y
printf '9999999999\n' > "$LEGACY_PAUSE"
rm -f "$PAUSE_UNTIL"
eq "旧 epoch 暂停不猜测转换" "$(manual_paused && echo y || echo n)" y
pause_for 0
eq "显式 pause 0 清两代手动记录" "$(manual_paused && echo y || echo n)" n
for bad in -1 01 abc '1+2' 10081; do
  if pause_for "$bad"; then ng "拒绝非法分钟 $bad"; else ok "拒绝非法分钟 $bad"; fi
done
rm -f "$CHILL_PROC/uptime"
if pause_for 2; then ng "缺时钟不得写暂停"; else ok "缺时钟拒绝写暂停"; fi
unset -f date

# ─────────────────────────────────────────────────────────────────────────────
sec "strip_egress：订阅不许指定出口网卡（ext-1030 第 14 条）"

cat > "$CHILL_DIR/template.yaml" <<'YAML'
proxies:
  - {name: 家里, type: socks5, server: 192.0.2.10, port: 1080, interface-name: tailscale0}
proxy-providers:
  # 注释
  sub:
    type: inline
    payload:
      - {name: a, type: ss, server: 1.2.3.4, port: 1, interface-name: rmnet_data0, cipher: aes-128-gcm}
      - name: b
        type: ss
        interface-name: rmnet_data0
        routing-mark: 6666
        server: 1.2.3.5
  snap2:
    type: file
    path: ./providers/snap2.yaml
    override:
      interface-name: rmnet_data0
      proxy-name:
        - {pattern: x, target: y}

proxy-groups:
  - {name: g, type: select, use: [sub, snap2]}
YAML
TEMPLATE="$CHILL_DIR/template.yaml"; CHILL_HOME_EXIT=''
render >/dev/null 2>&1; eq "带 interface-name 的订阅照样渲染成功" "$?" "0"
body=$(cat "$CONFIG")
case "$body" in *rmnet_data0*) ng "渲染结果里还有 interface-name: rmnet_data0" ;; *) ok "订阅里的 interface-name: rmnet_data0 去掉了" ;; esac
case "$body" in *6666*) ng "routing-mark 还在" ;; *) ok "订阅里的 routing-mark 去掉了" ;; esac
has "模板自己的家里出口不动" "$body" 'interface-name: tailscale0}'
has "流式节点其余字段还在" "$body" '{name: a, type: ss, server: 1.2.3.4, port: 1, cipher: aes-128-gcm}'
has "块写法节点其余字段还在" "$body" "$(printf '        type: ss\n        server: 1.2.3.5')"
has "没有 override 的 provider 补上清空" "$body" "$(printf '        server: 1.2.3.5\n    override: {interface-name: %s, routing-mark: 0}' '""')"
has "已有 override 的 provider 补进去" "$body" "$(printf '    override:\n      interface-name: %s\n      routing-mark: 0\n      proxy-name:' '""')"
eq "每个 provider 恰好一份清空" "$(grep -c 'interface-name: ""' "$CONFIG")" "2"
has "后面的段原样" "$body" '  - {name: g, type: select, use: [sub, snap2]}'

# ─────────────────────────────────────────────────────────────────────────────
sec "start_blocked：端口 / TUN 被别人占着（ext-1030 第 16 条）"

SYS_NET="$CHILL_DIR/sysnet"; mkdir -p "$CHILL_PROC/net"; rm -rf "$SYS_NET/chill0"
HDR='  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode'
printf '%s\n' "$HDR" > "$CHILL_PROC/net/tcp6"; printf '%s\n' "$HDR" > "$CHILL_PROC/net/udp6"
# dnsmasq 绑在 192.168.0.1:53 上（UDP）和一条到 9999 的已建立连接，都不算占用
printf '%s\n   0: 0142000A:0035 00000000:0000 07 00000000:00000000 00:00000000 00000000 0 0 1\n' "$HDR" > "$CHILL_PROC/net/udp"
printf '%s\n   0: 0100007F:C350 0100007F:270F 01 00000000:00000000 00:00000000 00000000 0 0 2\n' "$HDR" > "$CHILL_PROC/net/tcp"
CHILL_BUSY_RECHECK=0; CHILL_API_LAN=0
kill_core() { :; }; restore_dns() { :; }; ip() { :; }
if core_slots_busy >/dev/null; then ng "空闲时误报被占"; else ok "dnsmasq 的 192.168.0.1:53、已建立连接都不算被占"; fi

printf '   1: 0100007F:270F 00000000:0000 0A 00000000:00000000 00:00000000 00000000 0 0 3\n' >> "$CHILL_PROC/net/tcp"
eq "认出 9999 被占" "$(core_slots_busy)" "tcp:9999"
GAVEUP="$CHILL_DIR/gaveup"; crash_n=1; crash_t0=5; rm -f "$GAVEUP" "$STATE"
if start_blocked; then ok "9999 被占：不起核心"; else ng "9999 被占却照样起核心"; fi
has "进直连，理由是 port_busy" "$(cat "$STATE")" '"state":"direct","reason":"port_busy"'
eq "不算崩溃：计数不变" "$crash_n" "1"
[ -f "$GAVEUP" ] && ng "不该 gaveup" || ok "没有 gaveup"

printf '%s\n' "$HDR" > "$CHILL_PROC/net/tcp"
printf '   0: 00000000:0035 00000000:0000 07 00000000:00000000 00:00000000 00000000 0 0 4\n' >> "$CHILL_PROC/net/udp"
eq "0.0.0.0:53 的 UDP 算被占" "$(core_slots_busy)" "udp:53"
printf '%s\n' "$HDR" > "$CHILL_PROC/net/udp"
mkdir -p "$SYS_NET/chill0"
rm -f "$STATE"; start_blocked; has "TUN 名被占：理由是 tun_busy" "$(cat "$STATE")" '"reason":"tun_busy"'
rm -rf "$SYS_NET/chill0"
if start_blocked; then ng "都空着却不起核心"; else ok "都空着：照常起核心"; fi
unset -f kill_core restore_dns ip

# ─────────────────────────────────────────────────────────────────────────────
printf '\n=== 结果：%d 通过 / %d 失败\n' "$PASS" "$FAIL"
rm -rf "$CHILL_DIR" "$BIN"
[ "$FAIL" -eq 0 ]
