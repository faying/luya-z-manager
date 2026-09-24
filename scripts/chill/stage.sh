#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# 把 CHILL 要装到设备上的全部文件摆成设备上 /data/chill 的样子：
#
#   scripts/chill/stage.sh <缓存目录> <输出目录>
#
# 输出目录（先清空）：
#   bin/mihomo            解压好的 mihomo（fetch-assets.sh 校验过 sha256）
#   ruleset/*.mrs cn.list 规则集
#   ui/                   zashboard（index.html 注入了 :9999 自动配置，见下）
#   chill.sh chill.init template.yaml chill.env.example
#
# install-chill.sh（手工装）和 onboard/build-kit.sh（装机包）都用它，
# 两条路装出来的 /data/chill 一模一样。
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

CACHE="${1:?用法: stage.sh <缓存目录> <输出目录>}"
OUT="${2:?用法: stage.sh <缓存目录> <输出目录>}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

die() { printf '[-] %s\n' "$*" >&2; exit 1; }
command -v unzip >/dev/null 2>&1 || die "缺 unzip（官方包是 .zip，设备上没有 unzip，只能在电脑上解开）"
for f in chill.sh chill.init template.yaml chill.env.example; do
    [ -f "$HERE/$f" ] || die "缺 $HERE/$f"
done
sh -n "$HERE/chill.sh"   || die "chill.sh 语法不过"
sh -n "$HERE/chill.init" || die "chill.init 语法不过"

"$HERE/fetch-assets.sh" "$CACHE"
MIHOMO_VER="${MIHOMO_VER:-v1.19.31}"
ZASHBOARD_VER="${ZASHBOARD_VER:-v3.27.0}"

rm -rf "$OUT" && mkdir -p "$OUT/bin" "$OUT/ruleset"
gunzip -c "$CACHE/mihomo-$MIHOMO_VER.gz" > "$OUT/bin/mihomo"
chmod 755 "$OUT/bin/mihomo"
cp "$CACHE"/ruleset/*.mrs "$CACHE/ruleset/cn.list" "$OUT/ruleset/"
cp "$HERE/chill.sh" "$HERE/chill.init" "$HERE/template.yaml" "$HERE/chill.env.example" "$OUT/"
chmod 755 "$OUT/chill.sh" "$OUT/chill.init"

ZTMP="$(mktemp -d)"
trap 'rm -rf "$ZTMP"' EXIT
unzip -q "$CACHE/zashboard-$ZASHBOARD_VER.zip" -d "$ZTMP" || die "解压 zashboard 失败"
[ -f "$ZTMP/dist/index.html" ] || die "zashboard 包里没有 dist/index.html，官方包结构可能变了"
# 2026-09-22 真机踩过的坑：zashboard 第一次打开会跳到"面板配置"设置页，
# 默认预填 127.0.0.1:9090（zashboard 自己的猜测，碰巧和 zte-agent 端口一样），
# mihomo 实际在 9999，连不上，看起来就像"白屏/打不开"。
# 注入一段引导脚本：首次没有 setup/api-list 时，用当前页面的 hostname + 9999 填好。
# 这只对 mihomo 自己的 :9999/ui 有意义；zte-agent 的 /chill-ui 会把这段删掉
# （chill_proxy.rs 的 strip_bootstrap，两边的开头字符串要一致），它走管理网页带过去的参数。
# key 名（setup/api-list、setup/active-uuid）是真机读 localStorage 核对的，zashboard
# 升级后可能改名，那时这段静默失效，退回"要手填一次"。
ZB_BOOTSTRAP='<script>;(function(){try{if(localStorage.getItem("setup/api-list"))return;var u="xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g,function(c){var r=Math.random()*16|0,v=c=="x"?r:r&3|8;return v.toString(16)});var p={type:"clash",protocol:location.protocol=="https:"?"https":"http",host:location.hostname,port:"9999",secondaryPath:"",password:"",label:"CHILL",uuid:u};localStorage.setItem("setup/api-list",JSON.stringify([p]));localStorage.setItem("setup/active-uuid",u)}catch(e){}})()</script>'
awk -v bs="$ZB_BOOTSTRAP" '!done && /<script type="module"/ { print bs; done=1 } { print }' \
    "$ZTMP/dist/index.html" > "$ZTMP/dist/index.html.tmp" && mv "$ZTMP/dist/index.html.tmp" "$ZTMP/dist/index.html"
grep -q 'setup/api-list' "$ZTMP/dist/index.html" || die "zashboard index.html 注入失败（没找到 <script type=\"module\">）"
mv "$ZTMP/dist" "$OUT/ui"

printf '[+] CHILL 文件已摆好：%s（mihomo %s、%s 个规则集、zashboard %s）\n' \
    "$OUT" "$MIHOMO_VER" "$(ls "$OUT/ruleset" | grep -c '\.mrs$')" "$ZASHBOARD_VER"
