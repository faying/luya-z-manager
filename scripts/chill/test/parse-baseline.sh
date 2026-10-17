#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# parse-baseline.sh — 解析 baseline-*.log，算出设计文档要填的那几个数
#
#   用法：./parse-baseline.sh [日志文件]        # 不给就取最新一个
#
# 报中位数而不是平均值：偶尔几个被污染的采样点会把 30 点的平均数拉偏，
# 而这些数字要发给另一个会话当功耗预算，偏了没人看得出来。
# 同时报范围，让对方看得见离散程度。
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

CACHE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.cache"
LOG="${1:-}"
if [ -z "$LOG" ]; then
  LOG="$(ls -t "$CACHE"/baseline-*.log 2>/dev/null | head -1 || true)"
fi
[ -n "$LOG" ] && [ -f "$LOG" ] || { echo "找不到日志文件" >&2; exit 1; }

echo "日志：$LOG"
grep '^#[^#]' "$LOG" || true
echo

awk '
# ── 频率驻留块 ──────────────────────────────────────────────────────────────
/^## freq_start/ { mode="s"; next }
/^## freq_end/   { mode="e"; next }
/^#/             { next }
/^[0-9]+[ \t]+[0-9]+$/ {
  if (mode=="s") { s[$1]=$2; seen_s=1 }
  else if (mode=="e") { e[$1]=$2; seen_e=1 }
  next
}
# ── 采样行 ──────────────────────────────────────────────────────────────────
/^[0-9][0-9]:[0-9][0-9]:[0-9][0-9] / {
  mode=""
  n++
  v=$0; gsub(/[a-zA-Z_]+=/, "", v); split(v, f, " ")
  # f[1]=时间 f[2]=cpuss f[3]=xo f[4]=batt f[5]=cur f[6]=cap f[7]=memavail f[8]=idle
  cpuss[n]=f[2]/1000; xo[n]=f[3]/1000; batt[n]=f[4]/1000
  cur[n]=-f[5]/1000; mem[n]=f[7]/1024; idle[n]=f[8]
  next
}
{ mode="" }

function med(arr, cnt,   i, j, t, a) {
  for (i=1; i<=cnt; i++) a[i]=arr[i]
  for (i=1; i<cnt; i++) for (j=i+1; j<=cnt; j++) if (a[i]>a[j]) { t=a[i]; a[i]=a[j]; a[j]=t }
  return (cnt%2) ? a[(cnt+1)/2] : (a[cnt/2]+a[cnt/2+1])/2
}
function lo(arr, cnt,   i, m) { m=arr[1]; for(i=2;i<=cnt;i++) if(arr[i]<m) m=arr[i]; return m }
function hi(arr, cnt,   i, m) { m=arr[1]; for(i=2;i<=cnt;i++) if(arr[i]>m) m=arr[i]; return m }

END {
  if (n<2) { print "采样点不足，无法统计"; exit 1 }
  secs=(n-1)*60

  printf "样本数 %d，跨度 %d 分钟\n\n", n, secs/60
  printf "%-14s %8s %8s %8s\n", "指标", "中位数", "最小", "最大"
  printf "%-14s %8.1f %8.1f %8.1f   ℃\n", "cpuss 温度", med(cpuss,n), lo(cpuss,n), hi(cpuss,n)
  printf "%-14s %8.1f %8.1f %8.1f   ℃\n", "xo 温度",    med(xo,n),    lo(xo,n),    hi(xo,n)
  printf "%-14s %8.1f %8.1f %8.1f   ℃\n", "电池温度",   med(batt,n),  lo(batt,n),  hi(batt,n)
  printf "%-14s %8.0f %8.0f %8.0f   mA\n", "放电电流",  med(cur,n),   lo(cur,n),   hi(cur,n)
  printf "%-14s %8.0f %8.0f %8.0f   MB\n", "MemAvailable", med(mem,n), lo(mem,n),  hi(mem,n)

  d=idle[n]-idle[1]
  printf "\n4 核空闲率     %.1f%%  （idle 增量 %d ticks / %d s）\n", d/(secs*100*4)*100, d, secs

  # ── 691MHz 驻留：必须用首尾差值，不能用绝对计数 ──────────────────────────
  if (seen_s && seen_e) {
    # 循环变量不能再叫 f：上面 split 已经把 f 变成数组名，awk 会拒绝再赋值给它。
    # 注意这段 awk 整体包在 shell 单引号里，注释里不能出现单引号，否则会提前闭合。
    tot=0; low=0
    for (q in e) { dd=e[q]-s[q]; if (dd<0) dd=0; delta[q]=dd; tot+=dd }
    if (tot>0) {
      minf=0
      for (q in delta) if (minf==0 || q+0 < minf+0) minf=q
      low=delta[minf]
      printf "\n最低频 %d kHz 驻留  %.1f%%   （窗口内 %d / %d ticks）\n", minf, low/tot*100, low, tot
      printf "\n各档驻留（窗口内增量）\n"
      k=0; for (q in delta) { fr[++k]=q }
      for (i=1; i<k; i++) for (j=i+1; j<=k; j++) if (fr[i]+0 > fr[j]+0) { t=fr[i]; fr[i]=fr[j]; fr[j]=t }
      for (i=1; i<=k; i++) if (delta[fr[i]]>0)
        printf "  %8d kHz  %6.2f%%  (%d ticks)\n", fr[i], delta[fr[i]]/tot*100, delta[fr[i]]
    } else {
      print "\n频率计数器在窗口内没有变化，无法算驻留"
    }
  } else if (seen_s) {
    print "\n只有 freq_start，日志还没跑完（缺 freq_end），驻留待计算"
  }
}
' "$LOG"
