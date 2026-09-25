#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# build-kit.sh — 打出给别人的 U60 Pro（MU5250）装机包
#
#   ./onboard/build-kit.sh            # → onboard/dist/u60-kit-YYYYMMDD.tar.gz
#
# 只要三个公开仓库（manager、touch-ui、data-service 并排放）+ Docker + 网络就能从零打出完整的包，
# 不需要从任何已装好的设备上拉东西。包里的东西和来源：
#   dropbear          OpenWrt 23.05.4 官方 ipk，按 sha256 钉死（和维护者设备上 /data/dropbear 是同一个二进制）
#   zte-agent         本仓库 HEAD 现编（cargo zigbuild；没装就走 Docker，镜像按 digest 固定）
#   admin.tgz         本仓库 web/ 现编（静态导出）
#   devui/            触屏界面 LVGL 版 + u60-uid：DEVUI_BIN/UID_BIN 没给、且 ../zte-u60-pro-mu5250-touch-ui/out/ 里没有时，
#                     自动跑 ../zte-u60-pro-mu5250-touch-ui/scripts/build-docker.sh 现编；ui/ + 启动/自启脚本
#   devui/fonts/      Nunito 600/700/800 + 中文兜底字体（都是 OFL，按 sha256 钉死的公开下载，Docker 里生成，
#                     缓存 onboard/cache/fonts）。设备自带的 ZTE 字体（/usr/ui/fonts）不打包，触屏直接读设备上的
#   devui/zwrt-datad  Rust 版数据服务：DATAD_BIN 没给时，自动跑 ../zte-u60-pro-mu5250-data-service/scripts/build-docker.sh 现编
#   esim.tgz          scripts/esim/build-esim-bundle.sh --out 现打：Alpine 的 lpac + 依赖库（alpine.lock 钉版本和
#                     sha256）+ statx 兼容垫片 + qmi_uim_probe
#   guard/            ../zte-u60-pro-mu5250-touch-ui/scripts 的进程监督与 Wi-Fi 兜底：supervise.sh、u60-guard.sh、
#                     alert-lib.sh、agent-auth.sh、chaos.sh、doctor.sh… + zte-agent/zwrt-datad/u60-guard 的
#                     procd init 脚本（装到 /data/u60-guard 和 /etc/init.d，见 docs/RELIABILITY.md）
#
# 环境变量：DEVUI_REPO（默认 ../zte-u60-pro-mu5250-touch-ui）、DATAD_REPO（默认 ../zte-u60-pro-mu5250-data-service）、
#           DEVUI_BIN=路径（触屏二进制，默认 $DEVUI_REPO/out/u60pro-devui-lvgl.stripped，没有就现编）、
#           UID_BIN=路径（u60-uid，默认 $DEVUI_REPO/out/u60-uid，没有就现编）、
#           DATAD_BIN=路径（用这个 zwrt-datad，不现编）、
#           ESIM_TGZ=路径（用现成的 eSIM 包，不现打）、
#           DEVUI_FONTS_DIR=目录（现成的字体目录，要有 Nunito-600/700/800.ttf、OFL-Nunito.txt、
#           u60-cjk-fallback.ttf、OFL-ResourceHanRounded.txt；不给就自动生成到 onboard/cache/fonts）、
#           FONTS=0（不打字体：触屏数字退回设备自带 Roboto，中文用设备自带字体）
#           这些也可以写进 onboard/kit.local.env（不进 git），每次打包自动读取。
#
# 打包前会检查两样最容易装错、装错了又看不出来的东西，不对就停：
#   触屏二进制必须是 LVGL 版（make 编的）。touch-ui 的 scripts/build.sh 编的是旧 litehtml 版，
#     而且会写到同一个 u60pro-devui.stripped，默认路径下一不留神就打进旧界面。
#   zwrt-datad 必须是 Rust 版，且没有写死的外部更新源（releases/latest/download）。
#     用 DATAD_BIN 指定的文件可能是更早的 C 版或带上游更新源的版本。
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ONB="$ROOT/onboard"
# 本机的默认值（不进 git）：KEY=值 一行一个，命令行上给的环境变量优先
if [ -f "$ONB/kit.local.env" ]; then
  while IFS='=' read -r k v; do
    case "$k" in ''|\#*) continue ;; esac
    [ -n "${!k:-}" ] || export "$k=$v"
  done < "$ONB/kit.local.env"
fi
DEVUI_REPO="${DEVUI_REPO:-$ROOT/../zte-u60-pro-mu5250-touch-ui}"
if [ -z "${DATAD_REPO:-}" ]; then
  for d in "$ROOT/../zte-u60-pro-mu5250-data-service" "$ROOT/../zte-u60-pro-mu5250-data-service"; do
    [ -d "$d" ] && { DATAD_REPO="$d"; break; }
  done
fi
ZIGBUILD_IMAGE="${ZIGBUILD_IMAGE:-messense/cargo-zigbuild@sha256:d8313491ec5798de0633fdc1c5753761bff79967bea69076020dc78121b2cca8}"
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
# 判断 zwrt-datad 二进制能不能进装机包：0 = 能；1 = 不是 Rust 版（C 版没有下面两个标记）；
# 2 = 带着上游写死的外部更新源。Rust 版的标记二选一：
#   ZWRT_DATAD_FORK_RUST_SELF_CONTAINED —— 新版 fork（OTA 整个删了）嵌的
#   ZWRT_DATAD_OTA_DISABLE_AUTO        —— 旧版（如 device-backups/zwrt-datad.rust-b5e8786），start.sh 用它关自动 OTA
datad_bin_check() {
  grep -a -q -e 'ZWRT_DATAD_FORK_RUST_SELF_CONTAINED' -e 'ZWRT_DATAD_OTA_DISABLE_AUTO' "$1" || return 1
  ! grep -a -q 'releases/latest/download' "$1" || return 2
  return 0
}

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
if command -v cargo-zigbuild >/dev/null 2>&1; then
  ( cd "$ROOT" && cargo zigbuild --release --target aarch64-unknown-linux-musl -p zte-agent 2>&1 | tail -1 )
else
  docker run --rm -v "$ROOT":/src -w /src -e CARGO_TARGET_DIR=/src/target "$ZIGBUILD_IMAGE" \
    cargo zigbuild --release --target aarch64-unknown-linux-musl -p zte-agent 2>&1 | tail -1
fi
cp "$ROOT/target/aarch64-unknown-linux-musl/release/zte-agent" "$PL/zte-agent"

step "管理网页（next build）"
( cd "$ROOT/web" && { [ -d node_modules ] || npm ci --no-audit --no-fund >/dev/null; } && npm run build >/dev/null 2>&1 ) \
  || die "web 构建失败（cd web && npm run build 看原因）"
[ -f "$ROOT/web/out/index.html" ] || die "web/out/index.html 不存在"
tar czf "$PL/admin.tgz" -C "$ROOT/web/out" .

# ── devui ───────────────────────────────────────────────────────────────────
step "devui（${DEVUI_REPO}）"
DEVUI_BIN="${DEVUI_BIN:-$DEVUI_REPO/out/u60pro-devui-lvgl.stripped}"
UID_BIN="${UID_BIN:-$DEVUI_REPO/out/u60-uid}"
# 旧做法在仓库根目录 make u60-uid：out/ 里没有时也认那一份
[ -f "$UID_BIN" ] || [ "$UID_BIN" != "$DEVUI_REPO/out/u60-uid" ] || [ ! -f "$DEVUI_REPO/u60-uid" ] || UID_BIN="$DEVUI_REPO/u60-uid"
if [ ! -f "$DEVUI_BIN" ] || [ ! -f "$UID_BIN" ]; then
  for f in "$DEVUI_BIN" "$UID_BIN"; do
    [ -f "$f" ] || case "$f" in "$DEVUI_REPO"/out/*) ;; *) die "没有 $f" ;; esac
  done
  step "触屏界面 + u60-uid 还没编：跑 ${DEVUI_REPO}/scripts/build-docker.sh（Docker，第一次十几分钟）"
  "$DEVUI_REPO/scripts/build-docker.sh" "$DEVUI_REPO/out" >/dev/null || die "触屏界面构建失败（单独跑 ${DEVUI_REPO}/scripts/build-docker.sh 看原因）"
fi
# LVGL 版启动时打印这一句，litehtml 版没有
grep -a -q 'pages+chrome built' "$DEVUI_BIN" || die "$(basename "$DEVUI_BIN") 不是 LVGL 版触屏（多半是 scripts/build.sh 编的旧 litehtml 版）。
  LVGL 版：在 touch-ui 里 make CROSS_COMPILE=… 编出来再 strip，然后用 DEVUI_BIN=… 指定（或写进 onboard/kit.local.env）"
cp "$DEVUI_BIN" "$PL/devui/u60pro-devui"
cp "$DEVUI_REPO/scripts/start.sh" "$DEVUI_REPO/scripts/install-autostart.sh" "$PL/devui/"
# u60-uid：屏幕主人守护进程，和它的 procd init
cp "$UID_BIN" "$PL/devui/u60-uid"
cp "$DEVUI_REPO/scripts/u60-uid.init" "$PL/devui/u60-uid.init"
( cd "$DEVUI_REPO/ui" && tar czf "$PL/devui/ui.tgz" -- * )
# 触屏字体：Nunito（数字）+ 中文兜底（设备自带 ZTE 字体缺失时用），都是 OFL。
# 字体文件是二进制，不进任何 git，只随装机包走；设备自带的 /usr/ui/fonts 不打包。
FONT_FILES="Nunito-600.ttf Nunito-700.ttf Nunito-800.ttf OFL-Nunito.txt u60-cjk-fallback.ttf OFL-ResourceHanRounded.txt"
if [ "${FONTS:-1}" != 0 ]; then
  if [ -z "${DEVUI_FONTS_DIR:-}" ]; then
    DEVUI_FONTS_DIR="$CACHE/fonts"
    have=1; for f in $FONT_FILES; do [ -f "$DEVUI_FONTS_DIR/$f" ] || have=0; done
    if [ "$have" = 0 ]; then
      step "生成字体（Nunito + 中文兜底，Docker 里做，缓存 onboard/cache/fonts）"
      "$DEVUI_REPO/scripts/fonts/build-nunito.sh" "$DEVUI_FONTS_DIR" "$CACHE/fonts-src" >/dev/null || die "Nunito 生成失败"
      "$DEVUI_REPO/scripts/fonts/build-cjk-fallback.sh" "$DEVUI_FONTS_DIR" "$CACHE/fonts-src" >/dev/null || die "中文兜底字体生成失败"
    fi
  fi
  mkdir -p "$PL/devui/fonts"
  for f in $FONT_FILES; do
    [ -f "$DEVUI_FONTS_DIR/$f" ] || die "DEVUI_FONTS_DIR=${DEVUI_FONTS_DIR} 里缺 $f"
    cp "$DEVUI_FONTS_DIR/$f" "$PL/devui/fonts/"
  done
else
  printf '\033[0;33m!\033[0m %s\n' "FONTS=0：不打字体，触屏数字用设备自带的 Roboto，设备字体缺失时中文不显示"
fi

# ── 进程监督与 Wi-Fi 兜底 ─────────────────────────────────────────────────────
step "guard（${DEVUI_REPO}/scripts）"
mkdir -p "$PL/guard"
for f in alert-lib.sh u60-guard.sh supervise.sh agent-auth.sh chaos.sh doctor.sh config-backup.sh power-sample.sh wan-sources.sh \
         zte-agent.init zwrt-datad.init u60-guard.init; do
  [ -f "$DEVUI_REPO/scripts/$f" ] || die "缺 $DEVUI_REPO/scripts/$f"
  cp "$DEVUI_REPO/scripts/$f" "$PL/guard/"
done

# ── zwrt-datad（Rust 版，data-service 仓库现编）──────────────────────────────
if [ -z "${DATAD_BIN:-}" ]; then
  [ -n "${DATAD_REPO:-}" ] && [ -x "$DATAD_REPO/scripts/build-docker.sh" ] \
    || die "找不到 data-service 仓库（放在 manager 旁边，或用 DATAD_REPO=… / DATAD_BIN=… 指定）"
  step "zwrt-datad（${DATAD_REPO}，Docker 里 cargo zigbuild）"
  DATAD_BIN="$CACHE/zwrt-datad-aarch64"
  ZIGBUILD_IMAGE="$ZIGBUILD_IMAGE" "$DATAD_REPO/scripts/build-docker.sh" "$DATAD_BIN" >/dev/null || die "zwrt-datad 构建失败"
  DATAD_SRC="$(rev "$DATAD_REPO")$(dirty "$DATAD_REPO" rust)"
else
  step "zwrt-datad（${DATAD_BIN}）"
  [ -f "$DATAD_BIN" ] || die "DATAD_BIN 不存在: $DATAD_BIN"
  DATAD_SRC="$(basename "$DATAD_BIN")"
fi
cp "$DATAD_BIN" "$PL/devui/zwrt-datad"
# Rust 版（新标记 ZWRT_DATAD_FORK_RUST_SELF_CONTAINED 或旧标记 ZWRT_DATAD_OTA_DISABLE_AUTO，有其一即可）；
# 装机包要完全自主：不能带上游写死的更新源。规则见上面 datad_bin_check。
datad_rc=0; datad_bin_check "$PL/devui/zwrt-datad" || datad_rc=$?
case $datad_rc in
  0) ;;
  1) die "包里的 zwrt-datad 不是 Rust 版（${DATAD_BIN}；二进制里既没有 ZWRT_DATAD_FORK_RUST_SELF_CONTAINED 也没有 ZWRT_DATAD_OTA_DISABLE_AUTO）。
  用 DATAD_BIN=… 指定 Rust 版，或去掉 DATAD_BIN 让脚本从 data-service 仓库现编" ;;
  *) die "包里的 zwrt-datad 还带着写死的外部更新源（releases/latest/download）。用去掉了更新源的 fork 版（DATAD_BIN=…）" ;;
esac

# ── eSIM（lpac 包，本机现打）────────────────────────────────────────────────
if [ -n "${ESIM_TGZ:-}" ]; then
  step "eSIM：用现成的 ${ESIM_TGZ}"
  cp "$ESIM_TGZ" "$PL/esim.tgz"
  ESIM_SRC="$(basename "$ESIM_TGZ")"
else
  step "eSIM（Alpine lpac + 依赖库，按 scripts/esim/alpine.lock 下载校验）"
  ALPINE_PKGS_DIR="$CACHE/alpine-pkgs" "$ROOT/scripts/esim/build-esim-bundle.sh" --out "$PL/esim.tgz" >/dev/null \
    || die "eSIM 包打不出来。多半是 Alpine 替换了钉住的包（安全更新）：跑 python3 scripts/esim/alpine_closure.py --relock，
  重打后先在一台设备上验证 lpac.sh chip info，再提交新的 alpine.lock"
  ESIM_SRC="lpac $(awk '!/^#/ && $2=="lpac"{print $3" ("$1")"}' "$ROOT/scripts/esim/alpine.lock")"
fi
tar tzf "$PL/esim.tgz" | grep -q '^\./lpac\.sh$' || die "eSIM 包里没有 lpac.sh"


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
  echo "  u60pro-devui  $(rev "$DEVUI_REPO")$(dirty "$DEVUI_REPO" src ui scripts)（guard/ 脚本同源）"
  echo "  u60pro-devui  二进制 $(basename "$DEVUI_BIN") $(sha256 "$DEVUI_BIN" | cut -c1-12)"
  echo "  u60-uid       二进制 $(basename "$UID_BIN") $(sha256 "$UID_BIN" | cut -c1-12)"
  echo "  zwrt-datad    ${DATAD_SRC} $(sha256 "$DATAD_BIN" | cut -c1-12)"
  echo "  eSIM          ${ESIM_SRC}"
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
