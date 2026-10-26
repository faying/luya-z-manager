#!/bin/sh
# 可重现构建检查：同一个提交在两个全新目录里各编一次（各自空的 target，第二次晚几分钟、换个时区），
# 比两份产物的 sha256。一样 = 别人拿同一提交 + 同一镜像（onboard/build.Dockerfile）能编出同一个文件。
#
#   onboard/repro-check.sh [提交，默认 HEAD] [包…，默认 luyaz-core luyaz-web luyaz-sentinel]
#
# 要 Docker。编的是 git archive 出来的已提交内容，工作区没提交的改动不算。
# 没有 .git 的地方（比如同步过去的构建机）：REPRO_ARCHIVE=<git archive 出的 tar> REPRO_REV=<提交号>。触屏 luyaz-ui 要另外的源码卷
# （touch-ui/rs/setup-src.sh），这里不含。结果写到 target/repro/<提交前 12 位>.txt，
# 第一轮的产物留在 target/repro/bin-a/（release.yml 拿去发）。REPRO_OUT=<目录> 换输出位置。
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
if [ -n "${REPRO_ARCHIVE:-}" ]; then
  REV=${REPRO_REV:?REPRO_ARCHIVE 要配 REPRO_REV}
else
  REV=$(git -C "$ROOT" rev-parse --verify "${1:-HEAD}^{commit}")
fi
[ $# -gt 0 ] && shift
PKGS=${*:-luyaz-core luyaz-web luyaz-sentinel}
PROFILE=release-device
IMG=luyaz-rust-build:$( (shasum -a 256 "$ROOT/onboard/build.Dockerfile" 2>/dev/null || sha256sum "$ROOT/onboard/build.Dockerfile") | cut -c1-12)
docker image inspect "$IMG" >/dev/null 2>&1 || docker build -q -t "$IMG" -f "$ROOT/onboard/build.Dockerfile" "$ROOT/onboard" >/dev/null
WORK=$(mktemp -d "${TMPDIR:-/tmp}/luyaz-repro.XXXXXX")
# 容器里编出的文件归 root：用同一个镜像删
trap 'docker run --rm -v "$WORK":/w "$IMG" sh -c "rm -rf /w/*" >/dev/null 2>&1; rm -rf "$WORK"' EXIT
OUT=${REPRO_OUT:-$ROOT/target/repro}; mkdir -p "$OUT"

build() { # <轮次> <时区>
  d="$WORK/$1"; mkdir -p "$d/src" "$d/bin"
  if [ -n "${REPRO_ARCHIVE:-}" ]; then tar -xf "$REPRO_ARCHIVE" -C "$d/src"; else git -C "$ROOT" archive "$REV" | tar -xf - -C "$d/src"; fi
  for p in $PKGS; do
    prof=$PROFILE; [ "$p" = luyaz-web ] && prof=release   # 同 build-kit.sh
    # 源码一律挂在 /repo：路径会进调试信息和 panic 文字，两轮要一样
    docker run --rm -v "$d/src":/repo -v u60-cargo-registry:/usr/local/cargo/registry -v u60-zig-cache:/root/.cache \
      -w /repo -e CARGO_TARGET_DIR=/repo/target -e LUYAZ_COMMIT="$REV" -e TZ="$2" "$IMG" \
      cargo zigbuild --locked --profile "$prof" --target aarch64-unknown-linux-musl -p "$p" >"$d/$p.log" 2>&1 \
      || { tail -n 20 "$d/$p.log" >&2; echo "repro-check: $p 第 $1 轮编译失败" >&2; exit 1; }
    cp "$d/src/target/aarch64-unknown-linux-musl/$prof/$p" "$d/bin/$p"
  done
}

build a UTC
sleep 61   # 跨一分钟，时间戳要是混进产物就会露出来
build b Asia/Tokyo
{
  echo "commit $REV  image $IMG"
  for p in $PKGS; do
    a=$( (shasum -a 256 "$WORK/a/bin/$p" 2>/dev/null || sha256sum "$WORK/a/bin/$p") | cut -d' ' -f1)
    b=$( (shasum -a 256 "$WORK/b/bin/$p" 2>/dev/null || sha256sum "$WORK/b/bin/$p") | cut -d' ' -f1)
    if [ "$a" = "$b" ]; then echo "same  $p  $a"; else echo "DIFF  $p  $a  $b"; fi
  done
} | tee "$OUT/$(echo "$REV" | cut -c1-12).txt"
rm -rf "$OUT/bin-a" && cp -R "$WORK/a/bin" "$OUT/bin-a"
if grep -q '^DIFF' "$OUT/$(echo "$REV" | cut -c1-12).txt"; then exit 1; fi
