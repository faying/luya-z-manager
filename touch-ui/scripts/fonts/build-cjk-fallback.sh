#!/bin/sh
# 生成触屏的中文兜底字体：设备自带的 /usr/ui/fonts/ZTEZhengYuan.ttf 找不到时用它，
# 否则中文只能退到 LVGL 自带的 Montserrat（只有拉丁字符，中文全是空白）。
#
#   scripts/fonts/build-cjk-fallback.sh <输出目录> [缓存目录]
#
# 产出（放进装机包的字体目录，和 Nunito 一起装到 /data/plugins/u60pro-devui/fonts/）：
#   u60-cjk-fallback.ttf        Resource Han Rounded CN Regular 的子集，改名 "U60 CJK Fallback"
#   OFL-ResourceHanRounded.txt  原字体的许可证（OFL 1.1）
#   u60-cjk-fallback.txt        收了哪些字、多少个
#
# 为什么是它：圆体，和设备自带的正圆、数字用的 Nunito 一个风格；OFL 可以随包分发。
# 原字体保留了字体名 "Source"（Adobe），改过的版本不能用含它的名字，所以子集改名。
# 字符集：GB2312 全部（6763 个汉字和符号）+ Big5 常用字（5401 个，节点名、短信里的繁体）
# + 全角标点/符号 + 程序里写死的所有文字。短信、运营商名、Wi-Fi 名、节点名是任意文字，
# 只裁界面用到的字是不够的。
#
# 全在 Docker（python:3.12-slim）里做，本机不用装 Python 包；字体包按 sha256 固定。
# 字体文件是二进制，不进任何 git 仓库。
# SPDX-License-Identifier: MIT
set -eu

OUT=${1:?用法: build-cjk-fallback.sh <输出目录> [缓存目录]}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
CACHE=${2:-$ROOT/.cache/fonts}
VER=0.990
PKG=RHR-CN-$VER.7z
URL=https://github.com/CyanoHao/Resource-Han-Rounded/releases/download/v$VER/$PKG
SHA=e7005f7b4a7a0b8352d32c4a1358ff47564eb73be7fdb2db00d9f792755e9dc7
OFL_URL=https://raw.githubusercontent.com/CyanoHao/Resource-Han-Rounded/be90fee7a031c1297da5a260ccfb91756088d51c/OFL-License.txt
OFL_SHA=586a072c53a12f6e4389f0eb6034f9780afcb9f0a49558e9ed299664ab1107d8

mkdir -p "$OUT" "$CACHE"
OUT=$(cd "$OUT" && pwd); CACHE=$(cd "$CACHE" && pwd)
if [ ! -f "$CACHE/$PKG" ] || [ "$(shasum -a 256 "$CACHE/$PKG" | cut -d' ' -f1)" != "$SHA" ]; then
    echo "下载 $PKG"
    curl -fsSL -o "$CACHE/$PKG.tmp" "$URL"
    got=$(shasum -a 256 "$CACHE/$PKG.tmp" | cut -d' ' -f1)
    [ "$got" = "$SHA" ] || { rm -f "$CACHE/$PKG.tmp"; echo "sha256 不符：$got" >&2; exit 1; }
    mv "$CACHE/$PKG.tmp" "$CACHE/$PKG"
fi
if [ ! -s "$CACHE/OFL-License.txt" ] || [ "$(shasum -a 256 "$CACHE/OFL-License.txt" | cut -d' ' -f1)" != "$OFL_SHA" ]; then
    curl -fsSL -o "$CACHE/OFL-License.txt" "$OFL_URL"
fi
[ "$(shasum -a 256 "$CACHE/OFL-License.txt" | cut -d' ' -f1)" = "$OFL_SHA" ] || { echo "OFL 许可证 sha256 不符" >&2; exit 1; }
cp "$CACHE/OFL-License.txt" "$OUT/OFL-ResourceHanRounded.txt"

docker run --rm -v "$CACHE":/cache -v "$OUT":/out -v "$ROOT":/src:ro -w /cache python:3.12-slim sh -c '
set -e
pip -q install --disable-pip-version-check py7zr==0.22.0 fonttools==4.54.1 >/dev/null 2>&1
python - <<"PY"
import glob, py7zr, re
from fontTools import subset
from fontTools.ttLib import TTFont

src = "/cache/ResourceHanRoundedCN-Regular.ttf"
import os
if not os.path.exists(src):
    with py7zr.SevenZipFile("/cache/RHR-CN-0.990.7z") as z:
        z.extract(path="/cache", targets=["ResourceHanRoundedCN-Regular.ttf"])

chars = set()
# GB2312：两字节区 0xA1A1–0xF7FE
for hi in range(0xA1, 0xF8):
    for lo in range(0xA1, 0xFF):
        try: chars.add(bytes([hi, lo]).decode("gb2312"))
        except UnicodeDecodeError: pass
# Big5 常用字（一级）：0xA440–0xC67E
for hi in range(0xA4, 0xC7):
    for lo in list(range(0x40, 0x7F)) + list(range(0xA1, 0xFF)):
        code = (hi << 8) | lo
        if 0xA440 <= code <= 0xC67E:
            try: chars.add(bytes([hi, lo]).decode("big5"))
            except UnicodeDecodeError: pass
# ASCII、Latin-1、通用标点、箭头、数学符号、方框、CJK 符号与标点、全角
for a, b in [(0x20, 0x7E), (0xA0, 0xFF), (0x2000, 0x206F), (0x2190, 0x21FF),
             (0x2200, 0x22FF), (0x2500, 0x257F), (0x25A0, 0x25FF), (0x3000, 0x303F),
             (0xFF00, 0xFFEF)]:
    chars.update(chr(c) for c in range(a, b + 1))
# 程序里写死的文字（C 源码、界面模板）
lit = 0
for f in glob.glob("/src/src/*.c") + glob.glob("/src/include/*.h") + glob.glob("/src/ui/**/*.html", recursive=True):
    t = open(f, encoding="utf-8", errors="ignore").read()
    for ch in t:
        if ord(ch) > 0x7F:
            if ch not in chars: lit += 1
            chars.add(ch)

font = TTFont(src)
cmap = font.getBestCmap()
have = sorted(c for c in chars if ord(c) in cmap)
miss = sorted(c for c in chars if ord(c) not in cmap and ord(c) > 0x7F)

opts = subset.Options()
opts.layout_features = ["*"]
opts.name_IDs = ["*"]
opts.notdef_outline = True
opts.hinting = False           # 位图模式按像素渲染，TrueType 指令对体积影响大、对效果影响小
sub = subset.Subsetter(opts)
sub.populate(unicodes=[ord(c) for c in have])
sub.subset(font)

# OFL：改过的版本不能用保留字体名（Source），整体改名
name = font["name"]
fam = "U60 CJK Fallback"
for rec in list(name.names):
    if rec.nameID in (1, 4, 16, 21):
        rec.string = fam if rec.nameID != 4 else fam + " Regular"
    elif rec.nameID == 6:
        rec.string = "U60CJKFallback-Regular"
    elif rec.nameID == 3:
        rec.string = "U60CJKFallback-Regular;derived from Resource Han Rounded CN 0.990"
    elif rec.nameID in (2, 17, 22):
        rec.string = "Regular"
    elif rec.nameID == 10:
        rec.string = ("Subset of Resource Han Rounded CN Regular 0.990 (Cyano Hao; portions Adobe, "
                      "Reserved Font Name Source) for the U60 Pro (MU5250) touch UI. SIL OFL 1.1.")
font.save("/out/u60-cjk-fallback.ttf")

han = sum(1 for c in have if 0x4E00 <= ord(c) <= 0x9FFF)
with open("/out/u60-cjk-fallback.txt", "w", encoding="utf-8") as f:
    f.write(f"U60 CJK Fallback = Resource Han Rounded CN Regular 0.990 的子集\n")
    f.write(f"字符 {len(have)} 个，其中汉字 {han} 个；程序里用到而 GB2312/Big5 没有的 {lit} 个\n")
    missing = "".join(miss)
    f.write(f"原字体里没有、没收进来的 {len(miss)} 个：{missing}\n")
print(f"u60-cjk-fallback.ttf: {len(have)} 个字符（汉字 {han}），原字体缺 {len(miss)} 个")
PY
'
ls -l "$OUT/u60-cjk-fallback.ttf"
