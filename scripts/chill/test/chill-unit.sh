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
8989:	from 10.0.66.7 lookup main
8990:	from 10.0.66.8 lookup main
8999:	from 10.0.66.9 lookup main
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

# ─────────────────────────────────────────────────────────────────────────────
sec "render：占位符与拒绝渲染"

cat > "$CHILL_DIR/template.yaml" <<'YAML'
external-controller: '${CONTROLLER}'
a: '${SUB_SHOUHOU}'
b: '${FILTER_TW}'
c: [${CN_CIDR}]
YAML
printf '1.0.1.0/24\n1.0.2.0/23\n' > "$CHILL_DIR/ruleset/cn.list"
TEMPLATE="$CHILL_DIR/template.yaml"; CN_LIST="$CHILL_DIR/ruleset/cn.list"
CONFIG="$CHILL_DIR/run/config.yaml"; RUN_DIR="$CHILL_DIR/run"

# 打桩：让 mihomo -t 永远通过，把渲染与校验解耦
BIN=/tmp/chill-unit-mihomo
printf '#!/bin/sh\nexit 0\n' > "$BIN"; chmod +x "$BIN"

# 用 SUB_SHOUHOU（不是 SUB_OIX）：2026-09-16 核对实际数据后 oix/nexi 改为
# type:file 静态快照，chill.sh 的 render() 现在只校验+导出 SUB_SHOUHOU 这一个。
SUB_SHOUHOU='https://ok.example/a'
if render; then
  ok "正常值渲染成功"
  body=$(cat "$CONFIG")
  case "$body" in *'${'*) ng "仍有未替换占位符" ;; *) ok "占位符全部替换" ;; esac
  # 光「没有残留 ${...}」不够：awk 对未导出进子进程环境的变量会替换成空字符串，
  # 那样这条断言照样会通过、但值是错的（空的）。之前 render() 少导出 SUB_SHOUHOU
  # 到 awk 环境这个真实 bug 就是被这个盲区放过的，所以这里必须直接查替换后的值。
  has "SUB_SHOUHOU 已注入正确的值（不是空字符串替换）" "$body" 'https://ok.example/a'
  has "CN_CIDR 已注入" "$body" '1.0.1.0/24'
  has "filter 已注入"  "$body" '(?i)'
else
  ng "正常值渲染失败"
fi

SUB_SHOUHOU="https://bad.example/'quote"
if render 2>/dev/null; then ng "含单引号的订阅值应被拒绝渲染"; else ok "含单引号时拒绝渲染"; fi
SUB_SHOUHOU='https://ok.example/a'

# 校验不过时必须保留旧配置，不能把正在用的配置换坏
printf '#!/bin/sh\nexit 1\n' > "$BIN"; chmod +x "$BIN"
cp "$CONFIG" "$CHILL_DIR/run/prev.yaml"
render 2>/dev/null
if cmp -s "$CONFIG" "$CHILL_DIR/run/prev.yaml"; then ok "校验失败时保留旧配置"; else ng "校验失败却覆盖了旧配置"; fi
printf '#!/bin/sh\nexit 0\n' > "$BIN"; chmod +x "$BIN"

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
crash_n=0; crash_t0=0
note_crash; eq "第 1 次崩溃后计数为 1" "$crash_n" "1"
note_crash; eq "第 2 次崩溃后计数为 2" "$crash_n" "2"
if note_crash; then ng "第 3 次应返回非 0（达上限）"; else ok "第 3 次达上限返回非 0"; fi
eq "达上限时计数为 3" "$crash_n" "3"

# 距首次崩溃超过窗口应重置。此前「启动即退出」那条路径没有重置逻辑，
# 累计 3 次就永久 gaveup，哪怕间隔几小时——这条断言就是防它回归。
crash_t0=$(( $(date +%s) - CRASH_WINDOW - 60 ))
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

now=$(date +%s); next_backoff
w1=$((backoff - now)); near "第 1 次退避约 120 秒" "$w1" 120
now=$(date +%s); next_backoff
w2=$((backoff - now)); near "第 2 次退避约 300 秒" "$w2" 300
now=$(date +%s); next_backoff
w3=$((backoff - now)); near "第 3 次退避约 900 秒" "$w3" 900
now=$(date +%s); next_backoff
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
LOG=/tmp/chill-unit.log

# ─────────────────────────────────────────────────────────────────────────────
printf '\n=== 结果：%d 通过 / %d 失败\n' "$PASS" "$FAIL"
rm -rf "$CHILL_DIR" "$BIN"
[ "$FAIL" -eq 0 ]
