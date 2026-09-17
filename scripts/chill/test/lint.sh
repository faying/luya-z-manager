#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# chill 脚本的提交前检查
#
#   ./lint.sh          # 检查 scripts/chill 下所有 .sh 与 .init
#
# 为什么存在：下面这几类错误在 2026-09-16 一天之内被重复犯了——全角字符那条
# **犯了五次**。每次都靠「记得去扫一遍」，而事实证明记不住：install-chill.sh
# 写完就直接跑了，结果第 90 行的雷炸在运行时。
# 再写一条注释提醒自己没有用，把检查变成可执行的一步才有用。
#
# 有任何一项不通过就退出非 0，方便串进提交流程。
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"

FAIL=0
ok()  { printf '  ✓ %s\n' "$*"; }
ng()  { printf '  ✗ %s\n' "$*"; FAIL=$((FAIL + 1)); }
sec() { printf '\n=== %s\n' "$*"; }

# 跳过自己：规则字符串会匹配到自身（例如坑 3 的模式里就写着 "|| would"），
# 自匹配的误报没有意义，只会稀释真实告警。
FILES=$(find "$ROOT" -maxdepth 2 \( -name '*.sh' -o -name '*.init' \) | grep -v '/lint\.sh$' | sort)
[ -n "$FILES" ] || { echo "没找到待检查的脚本"; exit 1; }
echo "检查 $(printf '%s\n' "$FILES" | wc -l | tr -d ' ') 个文件"

# ── 1. 变量后紧跟全角字符 ────────────────────────────────────────────────────
# 中文 locale 下 bash/ash 会把紧跟其后的全角字符并进变量名，配 set -u 直接报
# unbound。典型：echo "已完成（$COUNT）" —— COUNT） 被当成变量名。
# 必须写成 ${COUNT}。排除注释行：注释里举反例是正当的。
sec "坑 1：变量后紧跟全角字符（应写 \${VAR}）"
hit=0
for f in $FILES; do
  out=$(grep -nP '^\s*[^#].*\$[A-Za-z_][A-Za-z0-9_]*[^\x00-\x7F]' "$f" 2>/dev/null || true)
  [ -n "$out" ] && { ng "$(basename "$f")"; printf '%s\n' "$out" | sed 's/^/      /'; hit=1; }
done
[ "$hit" -eq 0 ] && ok "无"

# ── 2. ssh -n 与 heredoc 同时出现 ───────────────────────────────────────────
# -n 把 stdin 接到 /dev/null，heredoc 就送不到远端，远端读到空输入什么都不做，
# 脚本却"成功"退出——假通过。（别处用 -n 是为了防 ssh 吞掉外层脚本的 stdin。）
sec "坑 2：ssh -n 搭配 heredoc"
hit=0
for f in $FILES; do
  out=$(grep -nE "ssh[^|]*-n[^|]*<<" "$f" 2>/dev/null || true)
  [ -n "$out" ] && { ng "$(basename "$f")"; printf '%s\n' "$out" | sed 's/^/      /'; hit=1; }
done
[ "$hit" -eq 0 ] && ok "无"

# ── 3. set -e 下被 || 掩盖的失败 ────────────────────────────────────────────
# `cmd_a && cmd_b || cmd_c`：cmd_b 真的失败时会滑到 cmd_c，把失败伪装成正常分支。
# 只报那些右侧是"提示类"函数的，避免误伤惯用的 `|| true`。
sec "坑 3：&& … || 提示函数（会掩盖真实失败）"
hit=0
for f in $FILES; do
  # 只报右侧是**自定义提示函数**的（would/warn/say）。
  # 不报 `… && echo A || echo B`：那是三元惯用法，echo 不会失败，报了就是噪音。
  # 同时排除注释行——注释里举反例是正当的。
  out=$(grep -nE '^[[:space:]]*[^#].*\&\&[^&|]+\|\|[[:space:]]*(would|warn|say)\b' "$f" 2>/dev/null || true)
  [ -n "$out" ] && { ng "$(basename "$f")"; printf '%s\n' "$out" | sed 's/^/      /'; hit=1; }
done
[ "$hit" -eq 0 ] && ok "无"

# ── 4. 语法 ─────────────────────────────────────────────────────────────────
# 设备端脚本按 POSIX sh 检查（目标是 busybox ash），Mac 端工具按 bash 检查。
sec "语法"
for f in $FILES; do
  b=$(basename "$f")
  case "$b" in
    chill.sh|chill.init|chill-unit.sh)
      sh -n "$f" 2>/dev/null && ok "$b (sh -n)" || ng "$b (sh -n 不过)" ;;
    *)
      bash -n "$f" 2>/dev/null && ok "$b (bash -n)" || ng "$b (bash -n 不过)" ;;
  esac
done

# ── 5. 不该出现的东西 ───────────────────────────────────────────────────────
sec "凭据与设备地址"
hit=0
for f in $FILES; do
  # 排除 sha256/校验和：它们本来就是 64 位十六进制，必然命中 [0-9a-f]{32}，
  # 属于可预见的误报。真要找的是 token / api_key / 明文密码。
  out=$(grep -nEi 'token=|api_key|password=|[0-9a-f]{32}' "$f" 2>/dev/null \
        | grep -viE 'sha256|checksum|校验' || true)
  [ -n "$out" ] && { ng "$(basename "$f") 疑似含凭据"; printf '%s\n' "$out" | sed 's/^/      /'; hit=1; }
done
[ "$hit" -eq 0 ] && ok "无凭据特征"

# busybox 没有 pgrep -c，用了会直接报用法错误而拿不到结果
sec "busybox 不支持的用法"
hit=0
for f in $FILES; do
  # timeout 只在**作为命令调用**时才算（行首、或 ; | & 之后）。
  # procd 的 term_timeout 是参数名，不是命令，排除掉。
  out=$(grep -nE 'pgrep[[:space:]]+-[a-z]*c|(^|[;|&(][[:space:]]*)setsid|(^|[;|&(][[:space:]]*)timeout[[:space:]]+[0-9]' "$f" 2>/dev/null \
        | grep -vE '^[[:space:]]*[0-9]+:[[:space:]]*#' | grep -v 'term_timeout' || true)
  [ -n "$out" ] && { ng "$(basename "$f") 用了设备上没有/不支持的命令"; printf '%s\n' "$out" | sed 's/^/      /'; hit=1; }
done
[ "$hit" -eq 0 ] && ok "无"

printf '\n=== 结果：%s\n' "$([ "$FAIL" -eq 0 ] && echo '全部通过' || echo "${FAIL} 项不通过")"
[ "$FAIL" -eq 0 ]
