#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# chill.sh 设备端测试（离线单测覆盖不到的部分）
#
#   HOST=u60 ./chill-device.sh
#
# 为什么要单独一个：有些函数只能在真设备上验——/proc 的 exe 链接、busybox 的
# 行为、nft。Mac 上没有 /proc，chill-unit.sh 测不了这些。
#
# 本脚本只做**不碰网络**的项：不起 TUN、不改路由、不动 dnsmasq、不建 nft 表。
# 会接管路由的那些（rebuild_api_guard、run 主循环）归 chill-e2e.sh。
#
# ── 两个必须遵守的写法（都是踩出来的）──────────────────────────────────────
# 1. busybox 按 **argv[0] 的 basename** 选 applet。把 busybox 复制后改名再调用，
#    会立刻 "applet not found" 退出。要造一个「长得像我们的二进制、又真的会跑」
#    的进程，必须**保留文件名 busybox、只换目录**。
# 2. 这类测试必须**先断言前提**（进程确实存活、exe 确实指向预期路径），
#    前提不成立立即中止。否则被测对象根本没运行，后面每条断言都会「通过」——
#    2026-09-16 就是这样拿到过三个假阳性的 ✓。
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

HOST="${HOST:-u60}"
SB=/data/local/tmp/chill-spike
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SUT="$HERE/../chill.sh"

[ -f "$SUT" ] || { echo "找不到 $SUT" >&2; exit 1; }

echo "[*] 推送当前 chill.sh 到设备（否则测的是旧版本）"
ssh -o BatchMode=yes -o ConnectTimeout=8 "$HOST" "mkdir -p $SB && cat > $SB/chill.sh.test" < "$SUT"

# ⚠ 这里**不能加 `-n`**：`-n` 会把 stdin 接到 /dev/null，下面的 heredoc 就送不到
# 远端的 `sh -s`，远端读到空输入、什么都不执行，脚本却"成功"退出——假通过。
# （别处用 `ssh -n` 是为了防止 ssh 吞掉外层脚本的 stdin，两种场景正好相反。）
ssh -o BatchMode=yes -o ConnectTimeout=8 "$HOST" "SB=$SB sh -s" <<'REMOTE'
set -u
PASS=0; FAIL=0
ok() { PASS=$((PASS+1)); printf '  ✓ %s\n' "$*"; }
ng() { FAIL=$((FAIL+1)); printf '  ✗ %s\n' "$*"; }
sec() { printf '\n=== %s\n' "$*"; }

sec "kill_core：只按 /proc/<pid>/exe 精确匹配，绝不按进程名"

D=/tmp/chill-device-kctest
rm -rf "$D"; mkdir -p "$D"
# 保留文件名 busybox，只换目录——改名会让 applet 选择失败、进程立即退出
cp /bin/busybox "$D/busybox"
FAKE="$D/busybox"

"$FAKE" sleep 300 & P1=$!
"$FAKE" sleep 300 & P2=$!
sleep 300 & P3=$!
sleep 1

# ---- 前提断言：不成立就中止，绝不往下跑出假阳性 ----
fail=0
for p in "$P1" "$P2" "$P3"; do
  kill -0 "$p" 2>/dev/null || { echo "  ✗ 前提失败：pid $p 未存活"; fail=1; }
done
e1=$(readlink "/proc/$P1/exe" 2>/dev/null || echo '')
e3=$(readlink "/proc/$P3/exe" 2>/dev/null || echo '')
[ "$e1" = "$FAKE" ] || { echo "  ✗ 前提失败：目标 exe=[$e1] 期望 [$FAKE]"; fail=1; }
{ [ -n "$e3" ] && [ "$e3" != "$FAKE" ]; } || { echo "  ✗ 前提失败：对照组 exe=[$e3] 无区分度"; fail=1; }
if [ "$fail" -ne 0 ]; then
  echo "[-] 前提不成立，中止（此时任何“通过”都是假的）"
  kill -9 "$P1" "$P2" "$P3" 2>/dev/null || true; rm -rf "$D"; exit 1
fi
ok "前提成立：三进程存活，目标 exe=$FAKE，对照组 exe=$e3"

echo "$P1" > /tmp/chill-device-core.pid
CHILL_LIB_ONLY=1 . "$SB/chill.sh.test"
BIN="$FAKE"; CORE_PID=/tmp/chill-device-core.pid; LOG=/tmp/chill-device.log

kill_core
sleep 1

kill -0 "$P1" 2>/dev/null && ng "目标进程仍存活" || ok "目标进程已终止（core.pid 路径）"
kill -0 "$P2" 2>/dev/null && ng "同源进程漏网（兜底扫描失效）" || ok "同源进程也被清理（兜底扫描有效）"
# 这条最关键：设备上大量系统进程的 exe 都是 /bin/busybox，
# 匹配写松一点就是灾难性误杀。
kill -0 "$P3" 2>/dev/null && ok "对照组未被误杀（exe 精确匹配生效）" || ng "对照组被误杀！匹配过宽"
[ -f /tmp/chill-device-core.pid ] && ng "core.pid 未清除" || ok "core.pid 已清除"

sec "kill_core：边界输入"
: > /tmp/chill-device-core.pid; CORE_PID=/tmp/chill-device-core.pid
kill_core 2>/tmp/chill-device-err.txt && ok "空 pid 文件返回 0" || ng "空 pid 文件返回非 0"
echo 999999 > /tmp/chill-device-core.pid
kill_core 2>>/tmp/chill-device-err.txt && ok "不存在的 pid 返回 0" || ng "不存在的 pid 返回非 0"
rm -f /tmp/chill-device-core.pid
kill_core 2>>/tmp/chill-device-err.txt && ok "pid 文件缺失时返回 0" || ng "pid 文件缺失时返回非 0"
[ -s /tmp/chill-device-err.txt ] && { ng "有 stderr 噪音"; head -3 /tmp/chill-device-err.txt; } || ok "全程无 stderr 噪音"

sec "清理"
kill -9 "$P3" 2>/dev/null || true
# 用 /proc 查而不是 ps|grep：ps 会把本脚本自己的命令行也匹配进去
n=0
for d in /proc/[0-9]*; do
  [ "$(readlink "$d/exe" 2>/dev/null || echo '')" = "$FAKE" ] && n=$((n+1))
done
[ "$n" -eq 0 ] && ok "无残留假进程" || ng "残留 $n 个假进程"
rm -rf "$D" /tmp/chill-device-err.txt /tmp/chill-device.log

printf '\n=== 结果：%d 通过 / %d 失败\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
REMOTE
