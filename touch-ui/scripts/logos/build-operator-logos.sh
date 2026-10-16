#!/bin/sh
# 生成首页结论块顶行左边的运营商 logo（卡本来的运营商，透明底 PNG）：
#
#   scripts/logos/build-operator-logos.sh <SVG 目录> <输出目录>
#
# SVG 目录一般是 manager 的 assets/operator-logos（网页首页用的同一批）；
# 找不到 SVG 的可放透明底大 PNG，同样处理；同名 PNG 和 SVG 都有时用 PNG（后处理，覆盖 SVG 的结果）。
# 每个 <slug>.svg 产出两张，装到 /data/plugins/u60pro-devui/operator-logos/：
#   <slug>.png     原色，浅色主题用
#   <slug>-w.png   单色白（界面里再压到约 85% 不透明），深色主题用：深蓝、深红的 logo 在深绿底上看不见
# 先裁掉透明边和白底边，再按视觉面积（H=16 时约 16×46）缩放：方形图标高 16，长字标矮一些；
# 画布统一高 H（默认 16）、竖直居中。界面里不缩放。
#
# logo 是商标：PNG 和 SVG 都不进开源仓库（tools/sync-public.sh 删），PNG 是二进制，不进任何 git，
# 只随装机包走。全在 Docker（alpine + rsvg-convert + ImageMagick）里做，本机不用装东西。
# SPDX-License-Identifier: MIT
set -eu

SRC=${1:?用法: build-operator-logos.sh <SVG 目录> <输出目录>}
OUT=${2:?用法: build-operator-logos.sh <SVG 目录> <输出目录>}
mkdir -p "$OUT"
SRC=$(cd "$SRC" && pwd); OUT=$(cd "$OUT" && pwd)

docker run --rm -e H="${LOGO_H:-16}" -v "$SRC:/src:ro" -v "$OUT:/out" alpine:3.20 sh -euc '
apk add -q --no-cache rsvg-convert imagemagick >/dev/null
MAXW=96   # 画布高 H 默认 16（环境变量 LOGO_H 可改）；面积、最矮高度跟着 H 等比
for f in /src/*.svg /src/*.png; do
    [ -f "$f" ] || continue
    s=$(basename "$f"); s=${s%.*}
    # 先大图渲染（高 280），再裁：透明边、白底边都裁掉（有的 SVG 带白色底框或留白，
    # 不裁的话 logo 实际笔画偏小、中心偏，跟后面的字对不齐）。
    case $f in
        *.svg) rsvg-convert -h 280 "$f" -o /tmp/big.png ;;
        *)     magick "$f" -resize x280 PNG32:/tmp/big.png ;;   # 没有 SVG 的（CTM）：透明底大图
    esac
    magick /tmp/big.png -alpha on -fuzz 6% -fill none -draw "color 0,0 floodfill" \
        -fuzz 8% -trim +repage /tmp/t.png
    # 按「视觉面积」定大小，不按高度：面积约 H×40·H/14（H=14 时 14×40），越宽越矮（SoftBank 这种长字标高度
    # 等高会显得比方形图标大一大截）；高 H·9/14～H、宽 ≤96。画布统一高 H、logo 竖直居中，
    # 宽的上下自然留白，中心仍和后面的字对齐。
    wh=$(magick identify -format "%w %h" /tmp/t.png)
    hh=$(echo "$wh" | awk -v A=$((H * H * 40 / 14)) -v H=$H -v W=$MAXW "{ r = \$1 / \$2; h = sqrt(A / r); if (h > H) h = H; if (h < H * 9 / 14) h = H * 9 / 14; if (h * r > W) h = W / r; printf \"%d\", h + 0.5 }")
    # 缩到十几像素边缘会发虚：先按线性光缩（颜色不发灰），再轻微锐化
    magick /tmp/t.png -colorspace RGB -filter Lanczos -resize "x$hh" -colorspace sRGB -unsharp 0x0.6+0.8+0 /tmp/a.png
    w2=$(magick identify -format %w /tmp/a.png)
    magick /tmp/a.png -background none -gravity center -extent "${w2}x$H" /tmp/a.png
    magick /tmp/a.png -strip "PNG32:/out/$s.png"
    # 原图里本来是白的部分（LG 的脸、M1 的字）先抠掉，否则整块刷白就成了实心圆
    magick "/out/$s.png" -fuzz 12% -transparent white -fill white -colorize 100 -strip "PNG32:/out/$s-w.png"
    echo "$s $(magick identify -format %wx%h "/out/$s.png")"
done
'
