#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# CHILL 的外部文件，在 Mac 上下载并校验，放进一个缓存目录：
#
#   scripts/chill/fetch-assets.sh <缓存目录>
#
# 产出（已存在且校验通过的不重下）：
#   <缓存>/mihomo-<版本>.gz        mihomo 官方 arm64 包（sha256 固定）
#   <缓存>/ruleset/<名字>.mrs       template.yaml 里声明的每一个规则集
#   <缓存>/ruleset/cn.list          CN CIDR 列表（route-exclude-address 用）
#   <缓存>/zashboard-<版本>.zip     zashboard 官方 dist.zip（sha256 固定）
#
# install-chill.sh（手工装）和 onboard/build-kit.sh（装机包）都用它，
# 规则集清单只从 template.yaml 解析这一处来，两边不会不同步。
# 规则集是每天更新的数据，没法固定 sha256；程序和面板固定。
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

CACHE="${1:?用法: fetch-assets.sh <缓存目录>}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEMPLATE="$HERE/template.yaml"

MIHOMO_VER="${MIHOMO_VER:-v1.19.31}"
# 校验的是下载下来的 .gz（不是解压后的二进制）。2026-09-16 实测值。
MIHOMO_SHA256="${MIHOMO_SHA256:-9e0f11afbf38426b8bd88fdc594678f8161c57eccb4e1b77acb12b493904f1d4}"
ZASHBOARD_VER="${ZASHBOARD_VER:-v3.27.0}"
# 2026-09-24 第一次固定（Zephyruso/zashboard 官方 releases 的 dist.zip）。
ZASHBOARD_SHA256="${ZASHBOARD_SHA256:-d78cbf20763a9ec78a065b0fb3e3a5176b53c9bfad59ec631762ef2a2e96a135}"
MRS_BASE="https://github.com/MetaCubeX/meta-rules-dat/raw/meta/geo"

die() { printf '[-] %s\n' "$*" >&2; exit 1; }
say() { printf '[*] %s\n' "$*"; }
sha() { shasum -a 256 "$1" | awk '{print $1}'; }

for c in curl shasum awk; do command -v "$c" >/dev/null 2>&1 || die "缺少命令：$c"; done
[ -f "$TEMPLATE" ] || die "缺 $TEMPLATE"
mkdir -p "$CACHE/ruleset"

# mihomo
GZ="$CACHE/mihomo-$MIHOMO_VER.gz"
if [ ! -f "$GZ" ] || [ "$(sha "$GZ")" != "$MIHOMO_SHA256" ]; then
    say "下载 mihomo ${MIHOMO_VER}"
    curl -fsSL -o "$GZ.tmp" "https://github.com/MetaCubeX/mihomo/releases/download/$MIHOMO_VER/mihomo-linux-arm64-$MIHOMO_VER.gz"
    got="$(sha "$GZ.tmp")"
    [ "$got" = "$MIHOMO_SHA256" ] || { rm -f "$GZ.tmp"; die "mihomo sha256 不符：期望 $MIHOMO_SHA256 实际 $got"; }
    mv "$GZ.tmp" "$GZ"
fi

# 规则集：从 template.yaml 的 rule-providers 解析 名字=路径
MRS_LIST="$(awk '
  /^ *[a-z_]+: *\{type: http/ {
    name = $1; sub(/:$/, "", name)
    if (match($0, /geo\/[a-z]+\/[^.]+\.mrs/)) print name "=" substr($0, RSTART + 4, RLENGTH - 4)
  }' "$TEMPLATE")"
[ -n "$MRS_LIST" ] || die "没能从 template.yaml 解析出规则集"
n=0
for item in $MRS_LIST; do
    name="${item%%=*}"; path="${item#*=}"
    out="$CACHE/ruleset/$name.mrs"
    [ -s "$out" ] || { curl -fsSL -o "$out.tmp" "$MRS_BASE/$path" && mv "$out.tmp" "$out"; } || die "下载 $path 失败"
    n=$((n + 1))
done
[ -s "$CACHE/ruleset/cn.list" ] || { curl -fsSL -o "$CACHE/ruleset/cn.list.tmp" "$MRS_BASE/geoip/cn.list" && mv "$CACHE/ruleset/cn.list.tmp" "$CACHE/ruleset/cn.list"; } || die "下载 cn.list 失败"

# zashboard
Z="$CACHE/zashboard-$ZASHBOARD_VER.zip"
if [ ! -s "$Z" ]; then
    say "下载 zashboard ${ZASHBOARD_VER}"
    curl -fsSL -o "$Z.tmp" "https://github.com/Zephyruso/zashboard/releases/download/$ZASHBOARD_VER/dist.zip" || die "下载 zashboard 失败"
    mv "$Z.tmp" "$Z"
fi
if [ -n "$ZASHBOARD_SHA256" ] && [ "$(sha "$Z")" != "$ZASHBOARD_SHA256" ]; then
    die "zashboard sha256 不符：期望 $ZASHBOARD_SHA256 实际 $(sha "$Z")（删掉 $Z 重下，或确认新版本后改这里）"
fi

say "就绪：mihomo ${MIHOMO_VER}、${n} 个规则集 + cn.list、zashboard ${ZASHBOARD_VER}（${CACHE}）"
