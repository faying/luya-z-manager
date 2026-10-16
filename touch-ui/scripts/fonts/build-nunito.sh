#!/bin/sh
# 生成触屏数字字体 Nunito-600/700/800.ttf（OFL）：
#
#   scripts/fonts/build-nunito.sh <输出目录> [缓存目录]
#
# 来源：google/fonts 仓库里的可变字体 ofl/nunito/Nunito[wght].ttf（提交号 + sha256 固定），
# 在 Docker（python:3.12-slim + fonttools 4.54.1）里用 varLib.instancer 切出三个字重。
# 不改字体名表（和 2026-09 装机包里的字体逐表一致，只有 head.modified 不同；
# 这里用固定的 SOURCE_DATE_EPOCH，重跑结果逐字节相同）。
# 产出：Nunito-600.ttf Nunito-700.ttf Nunito-800.ttf OFL-Nunito.txt
# 字体文件是二进制，不进任何 git 仓库。
# SPDX-License-Identifier: MIT
set -eu

OUT=${1:?用法: build-nunito.sh <输出目录> [缓存目录]}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
CACHE=${2:-$ROOT/.cache/fonts}
REV=23e54b51ddffbc7713c583748e3bd86f62b1fa4a
BASE=https://raw.githubusercontent.com/google/fonts/$REV/ofl/nunito
VF=Nunito-wght-$REV.ttf
VF_SHA=bb55a5ca5c2042335b3991af27c4d0705d0ef41cac6164ac737fd8f2a1e85207
OFL_SHA=580df76c95a1ec5ab878ceb25bb3d85c6a076804e9c970c8c6972aea775fdf65

sha() { shasum -a 256 "$1" | cut -d' ' -f1; }
get() { # url file sha
    [ -f "$CACHE/$2" ] && [ "$(sha "$CACHE/$2")" = "$3" ] && return 0
    echo "下载 $2"
    curl -fsSL -o "$CACHE/$2.tmp" "$1"
    [ "$(sha "$CACHE/$2.tmp")" = "$3" ] || { rm -f "$CACHE/$2.tmp"; echo "sha256 不符：$2" >&2; exit 1; }
    mv "$CACHE/$2.tmp" "$CACHE/$2"
}

mkdir -p "$OUT" "$CACHE"
OUT=$(cd "$OUT" && pwd); CACHE=$(cd "$CACHE" && pwd)
get "$BASE/Nunito%5Bwght%5D.ttf" "$VF" "$VF_SHA"
get "$BASE/OFL.txt" OFL-Nunito-$REV.txt "$OFL_SHA"
cp "$CACHE/OFL-Nunito-$REV.txt" "$OUT/OFL-Nunito.txt"

docker run --rm -e SOURCE_DATE_EPOCH=1758672000 -v "$CACHE":/cache:ro -v "$OUT":/out python:3.12-slim sh -c "
set -e
pip -q install --disable-pip-version-check fonttools==4.54.1 >/dev/null 2>&1
for w in 600 700 800; do
  fonttools varLib.instancer -q -o /out/Nunito-\$w.ttf /cache/$VF wght=\$w
done
"
ls -l "$OUT"/Nunito-*.ttf
