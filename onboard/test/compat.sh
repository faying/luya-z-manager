#!/usr/bin/env bash
# install.sh 的装前自检（cap_of + check_device）：拿各种 uname / integrate_version / 组件喂进去，
# 看按 firmware-capabilities.matrix.json 各组件放行、跳过还是整次拒装。
# 用法：bash onboard/test/compat.sh（不连设备）
set -u
HERE=$(cd "$(dirname "$0")/.." && pwd)
KIT=$HERE
fn=$(sed -n '/^CAP_MATRIX=/,/^}  # check_device/p' "$HERE/install.sh")
ok()   { echo "OK $1"; }
info() { echo "INFO $1"; }
warn() { echo "WARN $1"; }
fail() { echo "FAIL $1"; exit 1; }
tx()   { printf "%s" "$1"; }
eval "$fn"
pass=0 bad=0
# <期望最后一行 OK|WARN|FAIL> <期望装的组件> <uname> <integrate_version> <组件> [ALLOW]
t() {
  local got
  U=$3 IV=$4
  got=$(dev_run() { printf '%s\n%s\n' "$U" "$IV"; }; LUYAZ_ALLOW_UNVERIFIED=${6:-0}; COMPONENTS=$5
        (check_device; echo "SET $COMPONENTS") > /tmp/compat.$$ 2>&1
        grep -v '^SET ' /tmp/compat.$$ | tail -n1; grep '^SET ' /tmp/compat.$$)
  local last set
  last=$(printf '%s\n' "$got" | sed -n 1p); set=$(printf '%s\n' "$got" | sed -n 's/^SET //p')
  if [ "${last%% *}" = "$1" ] && [ "$set" = "$2" ]; then pass=$((pass + 1))
  else bad=$((bad + 1)); echo "不对：期望 $1 [$2]，$3 $4 ($5) → $last [$set]"; fi
}
ALL="ssh admin ui esim"
t OK   "$ALL" aarch64 CN_ZTE_MU5250V1.0.0B31 "$ALL"
t OK   "ssh admin ui esim chill recover" aarch64 CN_ZTE_MU5250V1.0.0B27 "ssh admin ui esim chill recover"
t FAIL ""     aarch64 CN_ZTE_MU5250V1.0.0B29 "$ALL"
t WARN "$ALL" aarch64 CN_ZTE_MU5250V1.0.0B29 "$ALL" 1
t FAIL ""     aarch64 CN_ZTE_MU5250V1.0.0B32 "$ALL"
t FAIL ""     aarch64 CN_ZTE_MU5250V1.0.0B32 "$ALL" 1
t FAIL ""     aarch64 EU_ZTE_MU5250V1.0.0B31 "$ALL"
t WARN "$ALL" aarch64 EU_ZTE_MU5252V1.0.0B05 "$ALL" 1
t FAIL ""     aarch64 "" "$ALL"
t FAIL ""     armv7l  CN_ZTE_MU5250V1.0.0B31 "$ALL"
t FAIL ""     aarch64 garbage "$ALL"
t OK   "$ALL" aarch64 BD_CNMU5250V1.0.0B31 "$ALL"           # 无底线写法也认得
t FAIL ""     aarch64 BD_XCBZHKMU5250V1.0.0B03 "$ALL"       # 港版 B03：矩阵里没有 → 不装
t FAIL ""     aarch64 BD_XCBZHKMU5250V1.0.0B03 "$ALL" 1     # 放开未实测也不装（unsupported）
t FAIL ""     aarch64 CN_ZTE_MU5120V1.0.0B31 "$ALL"         # 别的型号

# 按功能：临时矩阵里 B99 的 esim 不支持、chill 只有代码 → 其余照装，esim 跳过
M=$(mktemp)
cp "$CAP_MATRIX" "$M"
sed -i.bak 's|^\(    \)\({"model": "MU5252"\)|\1{"model": "MU5250", "region": "CN", "build": "B99", "features": {"ssh": "verified", "admin": "verified", "ui": "verified", "esim": "unsupported", "chill": "code-only"}},\n\1\2|' "$M"
rm -f "$M.bak"
if command -v jq >/dev/null 2>&1 && ! jq -e . "$M" >/dev/null; then echo "临时矩阵不是 JSON"; bad=$((bad + 1)); fi
CAP_MATRIX=$M
t WARN "ssh admin ui"       aarch64 CN_ZTE_MU5250V1.0.0B99 "$ALL"
t WARN "ssh admin ui"       aarch64 CN_ZTE_MU5250V1.0.0B99 "$ALL chill"
t WARN "ssh admin ui chill" aarch64 CN_ZTE_MU5250V1.0.0B99 "$ALL chill" 1
t FAIL ""                   aarch64 CN_ZTE_MU5250V1.0.0B99 "esim"
t WARN "ui"                 aarch64 CN_ZTE_MU5250V1.0.0B99 "ui recover"     # 条目里没写的功能 = unsupported
rm -f "$M" /tmp/compat.$$

# 矩阵本身：是 JSON，每条一行，所有状态都认识
if command -v jq >/dev/null 2>&1; then
  jq -e '.entries | length > 0' "$HERE/firmware-capabilities.matrix.json" >/dev/null || { echo "矩阵 entries 为空或不是 JSON"; bad=$((bad + 1)); }
  [ "$(jq -r '[.entries[].features[]] | map(select(. != "verified" and . != "code-only" and . != "unsupported")) | length' "$HERE/firmware-capabilities.matrix.json")" = 0 ] \
    || { echo "矩阵里有不认识的状态"; bad=$((bad + 1)); }
  [ "$(jq '.entries | length' "$HERE/firmware-capabilities.matrix.json")" = "$(grep -c '"model"' "$HERE/firmware-capabilities.matrix.json")" ] \
    || { echo "矩阵每条要单独一行"; bad=$((bad + 1)); }
fi
echo "通过 $pass，失败 $bad"
[ "$bad" = 0 ]
