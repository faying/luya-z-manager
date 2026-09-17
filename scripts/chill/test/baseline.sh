#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# baseline.sh — 采集一组性能基线，输出 parse-baseline.sh 能直接解析的日志
#
#   ./baseline.sh idle            熄屏空闲（第 1 组，已采过）
#   ./baseline.sh direct-1080p    直连播放国内 1080p（第 2 组）
#   ./baseline.sh proxy-1080p     走 CHILL 播放 YouTube 1080p（第 3 组）
#
#   ./baseline.sh <组名> [分钟数] [熄屏|亮屏]      默认 30 分钟、熄屏
#
# 为什么要有这个脚本：第 1 组是临时拼的内联命令采的。三组之间要做差值
# （过热阈值和「电流增量 ≤ ? mA」都是靠组间相减得出的），只要有一组采的字段
# 或口径不一样，差值就是错的，而且事后从日志里看不出来。所以三组必须同一个脚本。
#
# 口径（与第 1 组逐字段对齐，不要改）：
#   cpuss   取 cpuss-0..3 四个 thermal zone 的最大值（不是平均）
#   xo      thermal_zone35 (xo-therm)
#   batt    thermal_zone39 (battery)，不是 power_supply/battery/temp（那个是分度值 350）
#   cur     power_supply/battery/current_now，微安，负数表示放电
#   idle    /proc/stat 聚合 cpu 行的第 5 列
#   频率    policy0（这台设备 4 核共用一条曲线），首尾各取一次，驻留率用差值算
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

HOST="${HOST:-u60}"
GROUP="${1:-}"
MINUTES="${2:-30}"
SCREEN="${3:-熄屏}"
INTERVAL=60
REMOTE=/data/local/tmp/chill-baseline.sh
CACHE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.cache"

if [ -z "$GROUP" ]; then
  echo "用法: $0 {idle|direct-1080p|proxy-1080p} [分钟数] [熄屏|亮屏]" >&2
  exit 1
fi

COUNT=$(( MINUTES * 60 / INTERVAL ))
mkdir -p "$CACHE"
OUT="$CACHE/baseline-${GROUP}-$(date +%Y%m%d-%H%M%S).log"

# 设备端采样器。引号包住的 heredoc：内容原样落到设备，不在 Mac 上展开。
ssh -o BatchMode=yes -o ConnectTimeout=8 "$HOST" "cat > $REMOTE" <<'EOF'
#!/bin/sh
GROUP="$1"; COUNT="$2"; INTERVAL="$3"; SCREEN="$4"
HAP=/data/vendor/wifi/hostapd

cpuss_max() {
  m=0
  for z in 16 17 18 19; do
    v=$(cat /sys/class/thermal/thermal_zone$z/temp 2>/dev/null || echo 0)
    [ "$v" -gt "$m" ] && m=$v
  done
  echo "$m"
}
clients() {
  # 不能写 "|| echo 0"：grep -c 没匹配时会同时打印 0 并返回 1，
  # 再补一个 echo 0 就变成两行，头部会写成 clients=0 0+0 0。grep -c 本来就必定打印数字。
  a=$(hostapd_cli -i wlan0 -p $HAP all_sta 2>/dev/null | grep -cE '^[0-9a-f]{2}:')
  b=$(hostapd_cli -i wlan2 -p $HAP all_sta 2>/dev/null | grep -cE '^[0-9a-f]{2}:')
  echo "$a+$b"
}

echo "# group=$GROUP start=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "# clients=$(clients)"
echo "# screen=$SCREEN usb=$(cat /sys/class/power_supply/usb/online 2>/dev/null || echo ?)"
echo "# note=screen 由操作者声明；这台设备没有 /sys/class/backlight，读不到真实背光状态"
echo "## freq_start"
cat /sys/devices/system/cpu/cpufreq/policy0/stats/time_in_state

i=0
while [ "$i" -lt "$COUNT" ]; do
  printf '%s cpuss=%s xo=%s batt=%s cur=%s cap=%s memavail=%s idle=%s\n' \
    "$(date +%H:%M:%S)" \
    "$(cpuss_max)" \
    "$(cat /sys/class/thermal/thermal_zone35/temp 2>/dev/null || echo 0)" \
    "$(cat /sys/class/thermal/thermal_zone39/temp 2>/dev/null || echo 0)" \
    "$(cat /sys/class/power_supply/battery/current_now 2>/dev/null || echo 0)" \
    "$(cat /sys/class/power_supply/battery/capacity 2>/dev/null || echo 0)" \
    "$(awk '/^MemAvailable/{print $2}' /proc/meminfo)" \
    "$(awk '/^cpu /{print $5}' /proc/stat)"
  i=$((i + 1))
  [ "$i" -lt "$COUNT" ] && sleep "$INTERVAL"
done

echo "## freq_end"
cat /sys/devices/system/cpu/cpufreq/policy0/stats/time_in_state
echo "# end=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
EOF

echo "[*] 组别 ${GROUP}，${MINUTES} 分钟 / ${COUNT} 个采样点，间隔 ${INTERVAL}s，屏幕状态：${SCREEN}"
echo "[*] 输出 ${OUT}"
echo "[!] 采集期间不要 ssh 进设备、不要碰屏幕：第 1 组就是因为中途跑了几条探测，"
echo "    电流峰值被抬到 425mA。中位数扛得住，峰值不能用。"
echo

ssh -o BatchMode=yes -o ConnectTimeout=8 -o ServerAliveInterval=30 -n "$HOST" \
  "sh $REMOTE '$GROUP' '$COUNT' '$INTERVAL' '$SCREEN'" > "$OUT"

echo "[+] 采集完成"
"$(dirname "${BASH_SOURCE[0]}")/parse-baseline.sh" "$OUT"
