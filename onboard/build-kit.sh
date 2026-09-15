#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# build-kit.sh — 打出给别人的 U60 Pro（MU5250）装机包
#
#   ./onboard/build-kit.sh            # → onboard/dist/u60-kit-YYYYMMDD.tar.gz
#
# 包里的东西和来源：
#   dropbear          OpenWrt 23.05.4 官方 ipk，按 sha256 钉死（和维护者设备上 /data/dropbear 是同一个二进制）
#   zte-agent         本仓库 HEAD 现编（cargo zigbuild）
#   admin.tgz         本仓库 web/ 现编（静态导出）
#   devui/            ../zte-u60-pro-mu5250-touch-ui 里已构建的 u60pro-devui.stripped + ui/ + 启动/自启脚本
#                     （不带 CHILL 页：朋友没有 ShellCrash）
#   devui/zwrt-datad  } 从正在使用的设备上拉（FLEET_HOST，默认 ssh 别名 u60），
#   esim.tgz          } 缓存在 onboard/cache/。上游 datad 新版体积和接口都变了、lpac 依赖
#                       Alpine edge 会漂，所以用设备上验证过的那份
#
# 环境变量：DEVUI_REPO（默认 ../zte-u60-pro-mu5250-touch-ui）、FLEET_HOST（默认 u60）、
#           REFRESH_FLEET=1（重新从设备拉 datad/eSIM，不用缓存）
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ONB="$ROOT/onboard"
DEVUI_REPO="${DEVUI_REPO:-$ROOT/../zte-u60-pro-mu5250-touch-ui}"
FLEET_HOST="${FLEET_HOST:-u60}"
CACHE="$ONB/cache"
DIST="$ONB/dist"

DROPBEAR_URL="https://downloads.openwrt.org/releases/23.05.4/targets/armsr/armv8/packages/dropbear_2022.82-6_aarch64_generic.ipk"
DROPBEAR_SHA256="4fadd1b8529f22fb5d64ee27159d11f4feb68224657953d298a1acf85a83a5c0"

# macOS 上 /usr/bin/git 可能被 Xcode 许可证拦住，默认走 CommandLineTools 的
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Library/Developer/CommandLineTools}"
export PATH="$HOME/.cargo/bin:$PATH"
export COPYFILE_DISABLE=1   # 别把 macOS 的 ._* 文件打进包

step() { printf '\033[0;36m▶\033[0m %s\n' "$1"; }
die()  { printf '\033[0;31m✗\033[0m %s\n' "$1" >&2; exit 1; }
sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
rev() { git -C "$1" log -1 --format='%h %ad %s' --date=short 2>/dev/null || echo unknown; }
dirty() { [ -z "$(git -C "$1" status --porcelain -- "${@:2}" 2>/dev/null)" ] || echo " (有未提交改动)"; }

[ -d "$DEVUI_REPO" ] || die "找不到 devui 仓库: ${DEVUI_REPO}（用 DEVUI_REPO=… 指定）"
mkdir -p "$CACHE" "$DIST"
STAMP=$(date +%Y%m%d)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
KIT="$WORK/u60-kit"
PL="$KIT/payload"
mkdir -p "$PL/devui"

# ── dropbear ────────────────────────────────────────────────────────────────
step "dropbear（OpenWrt 23.05.4，sha256 校验）"
IPK="$CACHE/$(basename "$DROPBEAR_URL")"
[ -f "$IPK" ] || curl -fsSL "$DROPBEAR_URL" -o "$IPK"
[ "$(sha256 "$IPK")" = "$DROPBEAR_SHA256" ] || { rm -f "$IPK"; die "dropbear ipk 校验失败"; }
tar xzf "$IPK" -C "$WORK" ./data.tar.gz
tar xzf "$WORK/data.tar.gz" -C "$WORK" ./usr/sbin/dropbear
cp "$WORK/usr/sbin/dropbear" "$PL/dropbear"

# ── 高级后台 ────────────────────────────────────────────────────────────────
step "zte-agent（cargo zigbuild）"
( cd "$ROOT" && cargo zigbuild --release --target aarch64-unknown-linux-musl -p zte-agent 2>&1 | tail -1 )
cp "$ROOT/target/aarch64-unknown-linux-musl/release/zte-agent" "$PL/zte-agent"

step "管理网页（next build）"
( cd "$ROOT/web" && { [ -d node_modules ] || npm ci --no-audit --no-fund >/dev/null; } && npm run build >/dev/null 2>&1 ) \
  || die "web 构建失败（cd web && npm run build 看原因）"
[ -f "$ROOT/web/out/index.html" ] || die "web/out/index.html 不存在"
tar czf "$PL/admin.tgz" -C "$ROOT/web/out" .

# ── devui ───────────────────────────────────────────────────────────────────
step "devui（${DEVUI_REPO}）"
[ -f "$DEVUI_REPO/u60pro-devui.stripped" ] || die "没有 u60pro-devui.stripped，先在触屏界面仓库构建（见其 README）"
cp "$DEVUI_REPO/u60pro-devui.stripped" "$PL/devui/u60pro-devui"
cp "$DEVUI_REPO/scripts/start.sh" "$DEVUI_REPO/scripts/install-autostart.sh" "$PL/devui/"
( cd "$DEVUI_REPO/ui" && tar czf "$PL/devui/ui.tgz" --exclude 'functions/chill.html' -- * )

# ── 设备上拉的 zwrt-datad + eSIM ───────────────────────────────────────────────
FLEET="$CACHE/fleet.tgz"
if [ ! -s "$FLEET" ] || [ "${REFRESH_FLEET:-0}" = 1 ]; then
  step "从 $FLEET_HOST 拉 zwrt-datad + /data/esim（约 9 MB，蜂窝链路上要一两分钟）"
  ssh -o ServerAliveInterval=15 "$FLEET_HOST" 'cd /data && tar czf - esim plugins/zwrt-datad/zwrt-datad' > "$FLEET.part"
  mv "$FLEET.part" "$FLEET"
else
  step "zwrt-datad + eSIM：用缓存 ${FLEET}（REFRESH_FLEET=1 重新拉）"
fi
mkdir -p "$WORK/fleet"
tar xzf "$FLEET" -C "$WORK/fleet"
[ -x "$WORK/fleet/esim/lpac" ] && [ -f "$WORK/fleet/esim/lpac.sh" ] || die "缓存里没有完整的 esim/"
cp "$WORK/fleet/plugins/zwrt-datad/zwrt-datad" "$PL/devui/zwrt-datad"
tar czf "$PL/esim.tgz" -C "$WORK/fleet/esim" .

# ── 脚本 + 文档 + 清单 ────────────────────────────────────────────────────────
step "装机脚本、说明、清单"
cp "$ONB/install.sh" "$ONB/README.md" "$ONB/u60.env.example" "$KIT/"
cp "$ONB/kit-CLAUDE.md" "$KIT/CLAUDE.md"   # 对方在包目录里开 Claude Code 会自动读到
mkdir -p "$KIT/device" && cp "$ONB/device/install.sh" "$KIT/device/"
chmod 755 "$KIT/install.sh"

{
  echo "U60 装机包 $STAMP"
  echo
  echo "来源："
  echo "  u60p          $(rev "$ROOT")$(dirty "$ROOT" zte-agent web onboard)"
  echo "  u60pro-devui  $(rev "$DEVUI_REPO")$(dirty "$DEVUI_REPO" src ui scripts)"
  echo "  datad/eSIM    $FLEET_HOST 上 /data（缓存于 $(date -r "$FLEET" '+%Y-%m-%d %H:%M')）"
  echo "  dropbear      $(basename "$DROPBEAR_URL")"
  echo
  echo "sha256（install.sh 开头会逐个校验）："
  ( cd "$KIT" && find install.sh device payload -type f | LC_ALL=C sort | while read -r f; do
      printf '%s  %s\n' "$(sha256 "$f")" "$f"
    done )
} > "$KIT/MANIFEST.txt"

OUT="$DIST/u60-kit-$STAMP.tar.gz"
tar czf "$OUT" -C "$WORK" u60-kit
printf '\033[0;32m✓\033[0m %s（%s）\n' "$OUT" "$(du -h "$OUT" | cut -f1)"
sed -n '3,7p' "$KIT/MANIFEST.txt"
