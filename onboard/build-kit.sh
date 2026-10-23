#!/usr/bin/env bash
# ═════════════════════════════════════════════════════════════════════════════
# build-kit.sh：生成 Luya-Z 装机包（目标设备 U60 Pro / MU5250）。用法：
#   ./onboard/build-kit.sh            # → onboard/dist/luyaz-kit-YYYYMMDD.tar.gz
# 只要 manager 这一个仓库（touch-ui/ 在里面，Phase 10 10f）+ Docker + 网络就能从零打出完整的包，
# 不需要从任何已装好的设备上拉东西。包里的东西和来源：
#   dropbear          SSH 服务端：OpenWrt 23.05.4 发布的 aarch64 ipk，下载后按写死的 sha256 核对
#   luyaz-web         管理网页（:9090），本仓库 HEAD 的 crates/luyaz-web 现编（cargo zigbuild，profile release；
#                     没装就走 Docker，镜像按 digest 固定）
#   operator-logos/   网页首页的运营商 logo（assets/operator-logos 原样；开源版没有，就不带）
#   luyaz-sentinel    哨兵（屏幕主人 + 看门狗），本仓库 HEAD 的 crates/luyaz-sentinel
#                     现编（profile release-device，同 tools/luyaz ship sentinel）；SENTINEL_BIN 给了就用那个
#   ui/               触屏界面：TOUCH_BIN 没给、且 touch-ui/out/ 里没有时，
#                     自动跑 touch-ui/rs/build.sh device 现编（Rust 版）；启动/自启脚本
#                     （界面文字编进二进制，不再带 ui/ 目录）
#   ui/fonts/         Nunito 600/700/800 + 中文兜底字体（都是 OFL，按 sha256 钉死的公开下载，Docker 里生成，
#                     缓存 onboard/cache/fonts）。设备自带的 ZTE 字体（/usr/ui/fonts）不打包，触屏直接读设备上的
#   ui/luyaz-core     核心 = 本仓库 HEAD 的 crates/luyaz-core（cargo zigbuild --profile release-device，同
#                     tools/luyaz ship core），装成 /data/luyaz/luyaz-core；CORE_BIN 给了就用那个
#   esim.tgz          scripts/esim/build-esim-bundle.sh --out 现打：Alpine 的 lpac + 依赖库（alpine.lock 钉版本和
#                     sha256）+ statx 兼容垫片 + qmi_uim_probe
#   scripts/          touch-ui/scripts 里还在用的设备脚本（= u60 ship scripts 那一套：
#                     alert-lib.sh、web-auth.sh、doctor.sh…）+ luyaz-ship.sh + luyaz-core/luyaz-sentinel/luyaz-web 的
#                     procd init 脚本（装到 /data/luyaz/scripts 和 /etc/init.d）
#   chill/            CHILL：scripts/chill/stage.sh 摆好的 mihomo（sha256 固定）、规则集（下载时的最新版）、脚本、
#                     zashboard（sha256 固定），下载缓存在 onboard/cache/chill。CHILL=0 不打
# 另外：install.sh、device/install.sh、README、u60.env.example、CLAUDE.md（kit-CLAUDE.md）、
#       MANIFEST.txt（来源 + 每个文件的 sha256，install.sh 先核对）、versions.json（各组件版本）。

# 环境变量：TOUCH_REPO（默认本仓库的 touch-ui/）、CHILL=0（不打 CHILL）、
#           TOUCH_BIN=路径（触屏二进制，默认 $TOUCH_REPO/out/luyaz-ui，没有就现编）、
#           SENTINEL_BIN=路径（用这个 luyaz-sentinel，不现编）、
#           CORE_BIN=路径（用这个核心，不现编 luyaz-core）、
#           SHIP_RECORDS=目录（u60 ship 存在本机的上机记录：每次事务一个子目录，放着上机的文件和清单副本。
#           给了它，触屏、哨兵、数据服务就先用设备上正在跑的那份（按最新清单的 md5 在记录里找），
#           找不到才走默认；上面三个 *_BIN 显式给了的优先。FRESH=1 不用记录、照常现编）、
#           ESIM_TGZ=路径（用现成的 eSIM 包，不现打）、
#           UI_FONTS_DIR=目录（现成的字体目录，要有 Nunito-600/700/800.ttf、OFL-Nunito.txt、
#           u60-cjk-fallback.ttf、OFL-ResourceHanRounded.txt；不给就自动生成到 onboard/cache/fonts）、
#           FONTS=0（不打字体：触屏数字退回设备自带 Roboto，中文用设备自带字体）、
#           KIT_ZIGBUILD=命令（Rust 程序换个地方编，见下面 zigbuild）、ZIGBUILD_IMAGE=镜像（本机 Docker 编时用）
#           这些也可以写进 onboard/kit.local.env（不进 git），每次打包自动读取。
#
# 打包前会检查两样最容易装错、装错了又看不出来的东西，不对就停：
#   触屏二进制必须是 touch-ui/rs/build.sh device 编的 luyaz-ui。
#   核心必须是 Rust 版（luyaz-core），且没有写死的外部更新源（releases/latest/download）。
#     用 CORE_BIN 指定的文件可能是带上游更新源的版本。
# ═════════════════════════════════════════════════════════════════════════════
set -euo pipefail   # 任何一步出错就停
ONB="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$ONB/.." && pwd)"

# 本机的默认值（不进 git）：KEY=值 一行一个，命令行上给的环境变量优先
if [ -f "$ONB/kit.local.env" ]; then
  while IFS='=' read -r k v; do
    case "$k" in ''|\#*) continue ;; esac
    [ -n "${!k:-}" ] || export "$k=$v"
  done < "$ONB/kit.local.env"
fi
TOUCH_REPO="${TOUCH_REPO:-$ROOT/touch-ui}"

# Rust 构建镜像：没指定就按 onboard/build.Dockerfile 现建（和 data-service scripts/build.Dockerfile 同一份，
# 标签取文件哈希，所以两个仓库共用同一个镜像）。给了 KIT_ZIGBUILD 就不在本机建。
if [ -z "${ZIGBUILD_IMAGE:-}" ] && [ -z "${KIT_ZIGBUILD:-}" ]; then
  ZIGBUILD_IMAGE=luyaz-rust-build:$( (shasum -a 256 "$ONB/build.Dockerfile" 2>/dev/null || sha256sum "$ONB/build.Dockerfile") | cut -c1-12)
  docker image inspect "$ZIGBUILD_IMAGE" >/dev/null 2>&1 ||
    docker build -q -t "$ZIGBUILD_IMAGE" -f "$ONB/build.Dockerfile" "$ONB" >/dev/null
fi
CACHE="$ONB/cache"          # 下载和生成物的缓存，跨次复用
DIST="$ONB/dist"            # 打好的包

# SSH 服务端用 OpenWrt 官方发布的二进制，不自己编；换版本时两行一起改
DROPBEAR_SHA256=4fadd1b8529f22fb5d64ee27159d11f4feb68224657953d298a1acf85a83a5c0
DROPBEAR_URL=https://downloads.openwrt.org/releases/23.05.4/targets/armsr/armv8/packages/dropbear_2022.82-6_aarch64_generic.ipk

# macOS：没装完整 Xcode 时 /usr/bin/git 会弹许可证提示，默认改用 CommandLineTools 里的 git
export DEVELOPER_DIR=${DEVELOPER_DIR:-/Library/Developer/CommandLineTools} PATH=$HOME/.cargo/bin:$PATH
export COPYFILE_DISABLE=1   # macOS tar 不要夹带 ._* 元数据文件

# 进度（青色 ▶）和致命错误（红色 ✗，退出 1）
step() {
  printf '\033[0;36m▶\033[0m %s\n' "$1"
}

die() {
  printf '\033[0;31m✗\033[0m %s\n' "$1" >&2
  exit 1
}

sha256() { # <文件> → 十六进制摘要
  local s; s=$(shasum -a 256 "$1"); printf '%s\n' "${s%% *}"
}

rev() { # <仓库> → 「短提交号 日期 标题」
  git -C "$1" log -1 --date=short --format='%h %ad %s' 2>/dev/null || echo unknown
}

dirty() { # <仓库> [路径…] → 这些路径有未提交改动时打印标注
  if [ -n "$(git -C "$1" status --porcelain -- "${@:2}" 2>/dev/null)" ]; then echo " (有未提交改动)"; fi
}

md5q() { if command -v md5sum >/dev/null 2>&1; then md5sum "$1" | awk '{print $1}'; else /sbin/md5 -q "$1"; fi; }

# shipped <设备上的路径>：在 SHIP_RECORDS 里找设备上正在跑的那份。最新清单里这个路径的最后一行就是现状
# （u60 rollback 也是一次 ship，写回旧的 md5）；记录里同名、同 md5 的文件就是它。
# 找到了设 SHIP_FILE 和 SHIP_COMMIT（编它的提交；只记录过的文件没有提交，留空），找不到返回 1。
shipped() {
  SHIP_FILE= SHIP_COMMIT=
  [ "${FRESH:-0}" != 1 ] && [ -n "${SHIP_RECORDS:-}" ] && [ -d "$SHIP_RECORDS" ] || return 1
  local m line md5 f
  m=$(ls -1 "$SHIP_RECORDS"/*/u60-manifest.jsonl 2>/dev/null | LC_ALL=C sort | tail -n 1) || return 1
  [ -n "$m" ] || return 1
  line=$(grep -F "\"path\":\"$1\",\"md5\":" "$m" | tail -n 1) || return 1
  md5=$(printf '%s\n' "$line" | sed -n "s|.*\"path\":\"$1\",\"md5\":\"\([0-9a-f]\{32\}\)\".*|\1|p")
  [ -n "$md5" ] || return 1
  for f in "$SHIP_RECORDS"/*/"$(basename "$1")"; do
    [ -f "$f" ] && [ "$(md5q "$f")" = "$md5" ] || continue
    SHIP_FILE=$f
    SHIP_COMMIT=$(printf '%s\n' "$line" | sed -n 's/^{"v":1,"kind":"ship",.*"commit":"\([0-9a-f]\{40\}\)".*/\1/p')
    return 0
  done
  return 1
}

# 判断核心二进制能不能进装机包：0 = 能；1 = 不是 Luya-Z 改名 D3 起的核心（没有构建标记
# LUYAZ_CORE_BUILD_SELF_CONTAINED；包里的 init 只给 LUYAZ_ 环境变量，D3 前的核心只认旧名）；
# 2 = 带着上游写死的外部更新源。
core_bin_check() {
  grep -a -q 'LUYAZ_CORE_BUILD_SELF_CONTAINED' "$1" || return 1
  ! grep -a -q 'releases/latest/download' "$1" || return 2
  return 0
}

[ -d "$TOUCH_REPO" ] || die "找不到触屏仓库: ${TOUCH_REPO}（用 TOUCH_REPO=… 指定）"
mkdir -p "$CACHE" "$DIST"   # 缓存和产物目录
STAMP=$(date +%Y%m%d) WORK=$(mktemp -d)   # 包名里的日期；临时工作目录
trap 'rm -rf "$WORK"' EXIT   # 退出时清掉工作目录
KIT="$WORK/luyaz-kit"       # 包的根目录
PL="$KIT/payload"           # 推到设备上的东西
mkdir -p "$PL/ui"

# ── dropbear：从 ipk 里取出 usr/sbin/dropbear ─────────────────────────────────
# ipk 是 tar.gz 套 tar.gz：外层的 data.tar.gz 里才是文件
step "dropbear（OpenWrt 23.05.4 官方 ipk，核对 sha256）"
DB_IPK="$CACHE/${DROPBEAR_URL##*/}"
if [ ! -f "$DB_IPK" ]; then curl -fsSL -o "$DB_IPK" "$DROPBEAR_URL"; fi
if [ "$(sha256 "$DB_IPK")" != "$DROPBEAR_SHA256" ]; then
  rm -f "$DB_IPK"
  die "dropbear ipk 校验失败"
fi
mkdir -p "$WORK/dropbear"
tar xzf "$DB_IPK" -C "$WORK/dropbear" ./data.tar.gz
tar xzf "$WORK/dropbear/data.tar.gz" -C "$WORK/dropbear" ./usr/sbin/dropbear
cp "$WORK/dropbear/usr/sbin/dropbear" "$PL/dropbear"

# zigbuild <profile> <包>：本仓库现编一个 aarch64-musl 程序，打印产物路径。有 cargo-zigbuild 就本机编，
# 没有走 Docker（同 tools/luyaz ship）。LUYAZ_COMMIT：核心把它当版本号写进长期记录（u60-sentinel.md §17）
# KIT_ZIGBUILD=<命令>：换个地方编（比如另一台构建机），调用 <命令> <profile> <包> <commit> <产物路径>，
# 它要把本仓库工作区编出的 aarch64-musl 程序放到 <产物路径>（包 ui = 触屏界面，见下面 ui 一节）
zigbuild() {
  local head; head=$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo 0000000)
  local out="$ROOT/target/aarch64-unknown-linux-musl/$1/$2" log="$WORK/build-$2.log"
  rm -f "$out"   # 编失败时别拿上次的旧产物充数
  if [ -n "${KIT_ZIGBUILD:-}" ]; then
    $KIT_ZIGBUILD "$1" "$2" "$head" "$out" >"$log" 2>&1
  elif command -v cargo-zigbuild >/dev/null 2>&1; then
    ( cd "$ROOT" && LUYAZ_COMMIT=$head cargo zigbuild --profile "$1" --target aarch64-unknown-linux-musl -p "$2" ) >"$log" 2>&1
  else
    docker run --rm -v "$ROOT":/repo -v u60-cargo-registry:/usr/local/cargo/registry -v u60-zig-cache:/root/.cache \
      -w /repo -e CARGO_TARGET_DIR=/repo/target -e LUYAZ_COMMIT="$head" "$ZIGBUILD_IMAGE" \
      cargo zigbuild --profile "$1" --target aarch64-unknown-linux-musl -p "$2" >"$log" 2>&1
  fi || { tail -n 20 "$log" >&2; die "$2 构建失败"; }
  [ -f "$out" ] || die "$2 没编出来（$out 不在）"
  echo "$out"
}

# ── 高级后台：luyaz-web（:9090）────────────────────────────────────────────────
# profile release（unwind，同 tools/luyaz ship lweb：一个请求 panic 只结束它自己）。
# 密码文件是 /data/luyaz/web.env，设备端 web-auth.sh migrate 写。
step "luyaz-web（cargo zigbuild）"
LW_OUT=$(zigbuild release luyaz-web) || exit 1
cp "$LW_OUT" "$PL/luyaz-web"

# 首页运营商 logo：luyaz-web 从磁盘目录读（--logo-dir 默认 /data/luyaz/operator-logos），不编进程序。
# logo 是商标，开源版没有（sync-public 删掉），就不带，网页首页不显示 logo。
if ls "$ROOT"/assets/operator-logos/*.svg >/dev/null 2>&1; then
  mkdir -p "$PL/operator-logos"
  for f in "$ROOT"/assets/operator-logos/*.svg "$ROOT"/assets/operator-logos/*.png; do
    [ -f "$f" ] && cp "$f" "$PL/operator-logos/"
  done
fi

# ── 触屏界面（ui）：程序、字体、logo ─────────────────────────────────────
step "ui（${TOUCH_REPO}）"

# 设备现行版（SHIP_RECORDS）：只替代没显式给的；SHIP_<组件> 记编它的提交，写进 kit-source
SHIP_touch= SHIP_sentinel= SHIP_core=
if [ -z "${TOUCH_BIN:-}" ] && shipped /data/luyaz/ui/luyaz-ui; then
  TOUCH_BIN=$SHIP_FILE SHIP_touch=${SHIP_COMMIT:-0000000}
  step "  触屏用设备现行版：$TOUCH_BIN（${SHIP_touch:0:7}）"
fi
if [ -z "${SENTINEL_BIN:-}" ] && shipped /data/luyaz/luyaz-sentinel; then
  SENTINEL_BIN=$SHIP_FILE SHIP_sentinel=${SHIP_COMMIT:-0000000}
  step "  哨兵用设备现行版：$SENTINEL_BIN（${SHIP_sentinel:0:7}）"
fi
if [ -z "${CORE_BIN:-}" ] && shipped /data/luyaz/luyaz-core; then
  CORE_BIN=$SHIP_FILE SHIP_core=${SHIP_COMMIT:-0000000}
fi
TOUCH_BIN="${TOUCH_BIN:-$TOUCH_REPO/out/luyaz-ui}"
UI_PREBUILT=1
if [ ! -f "$TOUCH_BIN" ]; then
  UI_PREBUILT=0
  case "$TOUCH_BIN" in "$TOUCH_REPO"/out/*) ;; *) die "没有 $TOUCH_BIN" ;; esac
  step "触屏界面还没编：跑 ${TOUCH_REPO}/rs/build.sh device（Docker；源码卷没有就先 rs/setup-src.sh）"
  if [ -n "${KIT_ZIGBUILD:-}" ]; then
    mkdir -p "$(dirname "$TOUCH_BIN")"
    $KIT_ZIGBUILD release-device luyaz-ui "$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo 0000000)" "$TOUCH_BIN" >"$WORK/build-ui.log" 2>&1
  else
    { export LUYAZ_RS_IMAGE="${LUYAZ_RS_IMAGE:-$ZIGBUILD_IMAGE}"; "$TOUCH_REPO/rs/setup-src.sh" && "$TOUCH_REPO/rs/build.sh" device "$TOUCH_BIN"; } >/dev/null
  fi || die "触屏界面构建失败（单独跑 ${TOUCH_REPO}/rs/build.sh device 看原因）"
fi

# 触屏二进制要同时有 pages+chrome built 和 ui/src/ 两个标记
grep -a -q 'pages+chrome built' "$TOUCH_BIN" || die "$(basename "$TOUCH_BIN") 不是触屏界面"
grep -a -q 'ui/src/' "$TOUCH_BIN" || die "$(basename "$TOUCH_BIN") 不是 Rust 版触屏（用 touch-ui/rs/build.sh device 编）"
cp "$TOUCH_BIN" "$PL/ui/luyaz-ui"  # Phase 10 R5: installed as /data/luyaz/ui/luyaz-ui

# 触屏字体：Nunito（数字）+ 中文兜底（设备自带 ZTE 字体缺失时用），都是 OFL。
# 字体文件是二进制，不进任何 git，只随装机包走；设备自带的 /usr/ui/fonts 不打包。
FONT_FILES="Nunito-600.ttf Nunito-700.ttf Nunito-800.ttf OFL-Nunito.txt u60-cjk-fallback.ttf OFL-ResourceHanRounded.txt"
if [ "${FONTS:-1}" != 0 ]; then
  if [ -z "${UI_FONTS_DIR:-}" ]; then
    UI_FONTS_DIR="$CACHE/fonts"
    have=1; for f in $FONT_FILES; do [ -f "$UI_FONTS_DIR/$f" ] || have=0; done
    if [ "$have" = 0 ]; then
      step "生成字体（Nunito + 中文兜底，Docker 里做，缓存 onboard/cache/fonts）"
      "$TOUCH_REPO/scripts/fonts/build-nunito.sh" "$UI_FONTS_DIR" "$CACHE/fonts-src" >/dev/null || die "Nunito 生成失败"
      "$TOUCH_REPO/scripts/fonts/build-cjk-fallback.sh" "$UI_FONTS_DIR" "$CACHE/fonts-src" >/dev/null || die "中文兜底字体生成失败"
    fi
  fi
  mkdir -p "$PL/ui/fonts"
  for f in $FONT_FILES; do
    [ -f "$UI_FONTS_DIR/$f" ] || die "UI_FONTS_DIR=${UI_FONTS_DIR} 里缺 $f"
    cp "$UI_FONTS_DIR/$f" "$PL/ui/fonts/"
  done
else
  printf '\033[0;33m!\033[0m %s\n' "FONTS=0：不打字体，触屏数字用设备自带的 Roboto，设备字体缺失时中文不显示"
fi

# 首页运营商 logo（透明底 PNG，由 web 首页同一批 SVG 在 Docker 里生成）。logo 是商标，
# 开源版没有 SVG（sync-public 删掉），这里就跳过，触屏首页不显示 logo。
LOGO_SVG_DIR="$ROOT/assets/operator-logos"
if ls "$LOGO_SVG_DIR"/*.svg >/dev/null 2>&1; then
  step "运营商 logo（Docker 里生成）"
  "$TOUCH_REPO/scripts/logos/build-operator-logos.sh" "$LOGO_SVG_DIR" "$CACHE/operator-logos" >/dev/null || die "运营商 logo 生成失败"
  mkdir -p "$PL/ui/operator-logos"
  cp "$CACHE"/operator-logos/*.png "$PL/ui/operator-logos/"
fi

# ── 进程监督与 Wi-Fi 兜底 ─────────────────────────────────────────────────────
step "设备脚本（${TOUCH_REPO}/scripts）"
mkdir -p "$PL/scripts"

# luyaz-ship.sh、core-trial.sh（它的壳，要和它放一起）也进 /data/luyaz/scripts；
# luyaz-recover.sh 由设备端放成 /data/luyaz/ship/luyaz-recover.sh（自检过才放，已有就不换；rc.local 那一行只由 recover 组件加）
# 去掉 luyaz-ship.sh、core-trial.sh、luyaz-recover.sh 以后，这张表要和 luyaz-ship.sh 的 SCRIPTS_FILES + SCRIPTS_INITS
# 一致（u60 ship scripts，touch-ui docs/SHIP.md）。
for f in alert-lib.sh web-auth.sh chaos.sh doctor.sh config-backup.sh wan-sources.sh rpc-acl.sh luyaz-fallback.sh VERSION \
         luyaz-ship.sh core-trial.sh luyaz-recover.sh \
         luyaz-core.init luyaz-sentinel.init luyaz-web.init; do
  [ -f "$TOUCH_REPO/scripts/$f" ] || die "缺 $TOUCH_REPO/scripts/$f"
  cp "$TOUCH_REPO/scripts/$f" "$PL/scripts/"
done

# 设备清单（kind=kit）的来源：装机包日期、各组件编的提交、数据格式版本、有没有未提交改动。
# touch 记 touch-ui 当前的提交；用 TOUCH_BIN 指定现成程序时，程序未必是这个提交编的，记成有改动。
# 用了设备现行版（SHIP_<组件>）时记上机时的提交、没有改动；只记录过、不知道提交的（0000000）记成有改动。
kit_format() { sed 's/#.*//' "$1" 2>/dev/null | awk 'NF { print $1; exit }'; }
{
  echo "stamp=$STAMP"
  echo "lweb_commit=$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo 0000000)"
  echo "lweb_format=$(kit_format "$ROOT/ship/lweb/format")"
  echo "lweb_dirty=$([ -n "$(dirty "$ROOT" crates Cargo.toml Cargo.lock)" ] && echo 1 || echo 0)"
  for c in touch; do
    v=SHIP_$c sc=${!v}
    if [ -n "$sc" ]; then
      echo "${c}_commit=$sc"
      echo "${c}_format=$(kit_format "$TOUCH_REPO/ship/$c/format")"
      echo "${c}_dirty=$([ "$sc" = 0000000 ] && echo 1 || echo 0)"
      continue
    fi
    echo "${c}_commit=$(git -C "$TOUCH_REPO" rev-parse HEAD 2>/dev/null || echo 0000000)"
    echo "${c}_format=$(kit_format "$TOUCH_REPO/ship/$c/format")"
    echo "${c}_dirty=$([ -n "$(dirty "$TOUCH_REPO" src scripts)" ] || [ "$UI_PREBUILT" = 1 ] && echo 1 || echo 0)"
  done
  # core：本仓库当前的提交编的 luyaz-core（u60 ship core 一样记 manager 的提交，格式在
  # crates/luyaz-core/ship/core）；用 CORE_BIN 指定现成程序（下面不现编）时记成有改动
  echo "core_format=$(kit_format "$ROOT/crates/luyaz-core/ship/core/format")"
  if [ -n "$SHIP_core" ]; then
    echo "core_commit=$SHIP_core"
    echo "core_dirty=$([ "$SHIP_core" = 0000000 ] && echo 1 || echo 0)"
  else
    echo "core_commit=$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo 0000000)"
    echo "core_dirty=$([ -n "${CORE_BIN:-}" ] || [ -n "$(dirty "$ROOT" crates Cargo.toml Cargo.lock)" ] && echo 1 || echo 0)"
  fi
  # sentinel：本仓库当前的提交编的（同 u60 ship sentinel；没有 ship/sentinel/format，记 1）
  echo "sentinel_format=1"
  if [ -n "$SHIP_sentinel" ]; then
    echo "sentinel_commit=$SHIP_sentinel"
    echo "sentinel_dirty=$([ "$SHIP_sentinel" = 0000000 ] && echo 1 || echo 0)"
  else
    echo "sentinel_commit=$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo 0000000)"
    echo "sentinel_dirty=$([ -n "${SENTINEL_BIN:-}" ] || [ -n "$(dirty "$ROOT" crates Cargo.toml Cargo.lock)" ] && echo 1 || echo 0)"
  fi
} > "$PL/scripts/kit-source"

# ── 哨兵 luyaz-sentinel（本仓库现编，profile release-device，同 tools/luyaz ship sentinel）──────────
if [ -z "${SENTINEL_BIN:-}" ]; then
  step "luyaz-sentinel（cargo zigbuild --profile release-device）"
  SENTINEL_BIN=$(zigbuild release-device luyaz-sentinel) || exit 1
  SENTINEL_SRC="$(rev "$ROOT")$(dirty "$ROOT" crates Cargo.toml Cargo.lock)"
else
  step "luyaz-sentinel（${SENTINEL_BIN}）"
  [ -f "$SENTINEL_BIN" ] || die "SENTINEL_BIN 不存在: $SENTINEL_BIN"
  SENTINEL_SRC="${SHIP_sentinel:+设备现行版 ${SHIP_sentinel:0:7}，}$(basename "$SENTINEL_BIN")"
fi
cp "$SENTINEL_BIN" "$PL/luyaz-sentinel"

# ── 核心 luyaz-core（本仓库现编，profile release-device，同 tools/luyaz ship core）──────
if [ -z "${CORE_BIN:-}" ]; then
  step "核心 luyaz-core（cargo zigbuild --profile release-device）"
  CORE_BIN=$(zigbuild release-device luyaz-core) || exit 1
  CORE_SRC="luyaz-core $(rev "$ROOT")$(dirty "$ROOT" crates Cargo.toml Cargo.lock)"
else   # 用指定的核心
  step "核心（${CORE_BIN}）"
  [ -f "$CORE_BIN" ] || die "CORE_BIN 不存在: $CORE_BIN"
  CORE_SRC="$(basename "$CORE_BIN")"
  [ -z "$SHIP_core" ] || CORE_SRC="设备现行版 ${SHIP_core:0:7}（$CORE_BIN）"
fi
cp "$CORE_BIN" "$PL/ui/luyaz-core"

# D3 起的核心（标记 LUYAZ_CORE_BUILD_SELF_CONTAINED）；
# 装机包要完全自主：不能带上游写死的更新源。规则见上面 core_bin_check。
core_rc=0; core_bin_check "$PL/ui/luyaz-core" || core_rc=$?
case $core_rc in
  0) ;;
  1) die "包里的核心不是 Luya-Z 改名 D3 起的核心（${CORE_BIN}；二进制里没有 LUYAZ_CORE_BUILD_SELF_CONTAINED）。
  用 CORE_BIN=… 指定 D3 起的核心，或去掉 CORE_BIN 让脚本现编 luyaz-core" ;;
  *) die "包里的核心还带着写死的外部更新源（releases/latest/download）。用去掉了更新源的 fork 版（CORE_BIN=…）" ;;
esac

# ── 组件 lanid-parse（已连设备识别的解析器，docs/designs/lan-client-id.md）──────────────
# 只带这一个组件：其他策略在设备上照旧用内置实现。没有它核心照常跑，只是识别关着。
# LANID_CWASM=路径 给了就用那个；否则本机 Docker 现编（crates/luyaz-plugins/build.sh，
# plugc 只能在本机编）。编不出来只警告，不停。
if [ -n "${LANID_CWASM:-}" ]; then
  step "组件 lanid-parse（${LANID_CWASM}）"
  [ -f "$LANID_CWASM" ] || die "LANID_CWASM 不存在: $LANID_CWASM"
  LANID_SRC="$(basename "$LANID_CWASM")"
else
  step "组件 lanid-parse（crates/luyaz-plugins/build.sh device）"
  LANID_CWASM="$ROOT/crates/luyaz-plugins/out/lanid-parse.cwasm"
  rm -f "$LANID_CWASM"
  "$ROOT/crates/luyaz-plugins/build.sh" device >/dev/null 2>&1 || true
  LANID_SRC="lanid-parse $(rev "$ROOT")$(dirty "$ROOT" crates/luyaz-plugins crates/luyaz-policy/wit)"
fi
if [ -f "$LANID_CWASM" ]; then
  LANID_ABI=$(sed -n 's/^package \(.*\);$/\1/p' "$ROOT/crates/luyaz-policy/wit/lanid-parse.wit")
  mkdir -p "$PL/ui/plugins"
  cp "$LANID_CWASM" "$PL/ui/plugins/lanid-parse.cwasm"
  printf '{"plugins":[{"name":"lanid-parse","abi":"%s","file":"lanid-parse.cwasm","sha256":"%s"}]}\n' \
    "$LANID_ABI" "$(sha256 "$LANID_CWASM")" >"$PL/ui/plugins/plugins.json"
else
  printf '\033[0;33m!\033[0m 没编出 lanid-parse.cwasm：装上后已连设备识别关着（只有 DHCP 名字）\n' >&2
  LANID_SRC="没带（识别关着）"
fi

# OUI 厂商表（scripts/lanid/make-oui.sh 从 IEEE oui.txt 生成）：OUI_TSV=路径 给了才带；
# 公开仓库不带这张表，没有时厂商只来自 mDNS/SSDP
if [ -n "${OUI_TSV:-}" ]; then
  [ -f "$OUI_TSV" ] || die "OUI_TSV 不存在: $OUI_TSV"
  mkdir -p "$PL/ui/lanid"
  cp "$OUI_TSV" "$PL/ui/lanid/oui.tsv"
fi

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

# ── CHILL ───────────────────────────────────────────────────────────────────
if [ "${CHILL:-1}" != 0 ]; then
  step "CHILL（mihomo + 规则集 + zashboard，缓存 onboard/cache/chill）"
  "$ROOT/scripts/chill/stage.sh" "$CACHE/chill" "$WORK/chill" >/dev/null
  mkdir -p "$PL/chill"
  cp -R "$WORK/chill/bin" "$WORK/chill/ruleset" "$WORK/chill/chill.sh" "$WORK/chill/chill.init" "$WORK/chill/template.yaml" "$WORK/chill/chill.env.example" "$PL/chill/"
  tar czf "$PL/chill/ui.tgz" -C "$WORK/chill/ui" .
fi

# ── 装机脚本、说明、清单 ──────────────────────────────────────────────────────
step "装机脚本、说明文档和清单"
mkdir -p "$KIT/device"
cp "$ONB/install.sh" "$ONB/README.md" "$ONB/README.zh-CN.md" "$ONB/u60.env.example" "$KIT/"
cp "$ONB/device/install.sh" "$KIT/device/"
cp "$ONB/kit-CLAUDE.md" "$KIT/CLAUDE.md"   # 在包目录里开 Claude Code，它会先读这份
chmod 755 "$KIT/install.sh"   # 对方直接 ./install.sh

# MANIFEST.txt：前面是给人看的来源（第 3 行起，打完包会印出几行），
# 「sha256」那一行之后是 install.sh 要逐个核对的「摘要  路径」（两个空格）
kit_hashes() {
  ( cd "$KIT" && find install.sh device payload -type f | LC_ALL=C sort ) | while read -r rel; do
    printf '%s  %s\n' "$(sha256 "$KIT/$rel")" "$rel"
  done
}

{ # MANIFEST.txt 正文
  printf 'U60 装机包 %s\n\n来源：\n' "$STAMP"
  echo "  manager       $(rev "$ROOT")$(dirty "$ROOT" crates Cargo.toml Cargo.lock assets/operator-logos onboard)"
  echo "  luyaz-web     二进制 $(sha256 "$PL/luyaz-web" | cut -c1-12)$([ -d "$PL/operator-logos" ] && echo "，运营商 logo $(ls "$PL/operator-logos" | wc -l | tr -d ' ') 个")"
  echo "  luyaz-ui      $(rev "$TOUCH_REPO")$(dirty "$TOUCH_REPO" rs scripts)（scripts/ 同源）"
  echo "  luyaz-ui      二进制 $(basename "$TOUCH_BIN") $(sha256 "$TOUCH_BIN" | cut -c1-12)"
  echo "  luyaz-sentinel ${SENTINEL_SRC} $(sha256 "$SENTINEL_BIN" | cut -c1-12)"
  echo "  luyaz-core    ${CORE_SRC} $(sha256 "$CORE_BIN" | cut -c1-12)"
  echo "  lanid-parse   ${LANID_SRC}$([ -f "$PL/ui/plugins/lanid-parse.cwasm" ] && echo " $(sha256 "$PL/ui/plugins/lanid-parse.cwasm" | cut -c1-12)")"
  [ -f "$PL/ui/lanid/oui.tsv" ] && echo "  OUI 表        $(wc -l <"$PL/ui/lanid/oui.tsv" | tr -d ' ') 条 $(sha256 "$PL/ui/lanid/oui.tsv" | cut -c1-12)"
  echo "  eSIM          ${ESIM_SRC}"
  echo "  dropbear      ${DROPBEAR_URL##*/}"
  [ -d "$PL/chill" ] && echo "  CHILL         mihomo ${MIHOMO_VER:-v1.19.31}、zashboard ${ZASHBOARD_VER:-v3.27.0}（规则集下载于 $(date -r "$CACHE/chill/ruleset/cn.list" '+%Y-%m-%d')）"
  printf '\nsha256（install.sh 开头会逐个校验）：\n'
  kit_hashes
} >"$KIT/MANIFEST.txt"   # 写出清单

# each component's version (docs/designs/component-versions.md): from the LUYAZ_VERSION=
# marker in the binary (not run), the scripts' from scripts/VERSION; null: a build before 1.0.0
kver() { v=$(grep -a -o 'LUYAZ_VERSION=[0-9]*\.[0-9]*\.[0-9]*;' "$1" 2>/dev/null | head -n 1); v=${v#LUYAZ_VERSION=}; v=${v%;}; if [ -n "$v" ]; then echo "\"$v\""; else echo null; fi; }
_sv=$(cat "$PL/scripts/VERSION" 2>/dev/null)
printf '{\n  "v": 1,\n  "kit": "%s",\n  "components": {\n    "core": %s,\n    "lweb": %s,\n    "sentinel": %s,\n    "touch": %s,\n    "scripts": %s\n  }\n}\n' \
  "$STAMP" "$(kver "$CORE_BIN")" "$(kver "$PL/luyaz-web")" "$(kver "$SENTINEL_BIN")" "$(kver "$TOUCH_BIN")" \
  "$(if [ -n "$_sv" ]; then echo "\"$_sv\""; else echo null; fi)" >"$KIT/versions.json"

OUT="$DIST/luyaz-kit-$STAMP.tar.gz"
tar czf "$OUT" -C "$WORK" luyaz-kit
printf '\033[0;32m✓\033[0m %s（%s）\n' "$OUT" "$(du -h "$OUT" | cut -f1)"   # 包路径和大小
sed -n '3,7p' "$KIT/MANIFEST.txt"   # 来源的头几行
