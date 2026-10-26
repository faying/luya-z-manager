#!/usr/bin/env bash
# 备份旁存 sha256 的离线测试，不碰设备：
#   电脑端 restore_sum_check：相符放行；不符一律失败（LUYAZ_RESTORE_NO_SHA256=1 也不行）；
#     缺旁存只在 LUYAZ_RESTORE_NO_SHA256=1 时放行
#   设备端 config-backup.sh：CB_EXPECT_SHA256 不符时 verify / plan / restore 都拒绝，一个文件都不写
# 用法：bash onboard/test/backup-sha.sh
set -u
ONB=$(cd "$(dirname "$0")/.." && pwd)
CB=$ONB/../touch-ui/scripts/config-backup.sh
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
fail=0
bad() { echo "FAIL $1"; fail=1; }

# ── 电脑端 ──
eval "$(sed -n '/^sha256() {/,/^}  # sha256/p' "$ONB/install.sh")"
eval "$(sed -n '/^restore_sum_check() {/,/^}  # restore_sum_check/p' "$ONB/install.sh")"
ok() { :; }; warn() { :; }; tx() { printf '%s' "$1"; }
fail() { echo "$1" >&2; exit 1; }
B=$T/config-1.tgz
echo backup > "$B"
printf '%s  %s\n' "$(sha256 < "$B")" config-1.tgz > "$B.sha256"
( restore_sum_check "$B" ) 2>/dev/null || bad "相符却拒绝"
( RESTORE_SHA=; restore_sum_check "$B"; [ "$RESTORE_SHA" = "$(sha256 < "$B")" ] ) 2>/dev/null || bad "RESTORE_SHA 没带出来"
echo changed >> "$B"
( restore_sum_check "$B" ) 2>/dev/null && bad "不符却放行"
( LUYAZ_RESTORE_NO_SHA256=1 restore_sum_check "$B" ) 2>/dev/null && bad "不符时 override 也放行了"
rm -f "$B.sha256"
( restore_sum_check "$B" ) 2>/dev/null && bad "缺旁存却放行"
( LUYAZ_RESTORE_NO_SHA256=1 restore_sum_check "$B" ) 2>/dev/null || bad "缺旁存 + override 却拒绝"
echo 'not a sum' > "$B.sha256"
( LUYAZ_RESTORE_NO_SHA256=1 restore_sum_check "$B" ) 2>/dev/null && bad "旁存里没摘要却放行"

# ── 设备端 ──
R=$T/dev; mkdir -p "$R/data/region" "$R/data/luyaz"
echo CN > "$R/data/region/active"
echo "LUYAZ_WEB_PASSWORD=x" > "$R/data/luyaz/web.env"
CB_ROOT=$R CB_UCI=false sh "$CB" export > "$T/a.tgz" 2>/dev/null || bad "export"
good=$(sha256 < "$T/a.tgz")
CB_UCI=false CB_EXPECT_SHA256=$good sh "$CB" verify "$T/a.tgz" >/dev/null 2>&1 || bad "设备端相符却 verify 失败"
wrong=$(printf '%064d' 0)
CB_UCI=false CB_EXPECT_SHA256=$wrong sh "$CB" verify "$T/a.tgz" >/dev/null 2>&1 && bad "设备端不符却 verify 通过"
CB_UCI=false CB_EXPECT_SHA256=$wrong sh "$CB" plan "$T/a.tgz" "$T/out" >/dev/null 2>&1 && bad "设备端不符却 plan 通过"
CB_UCI=false CB_EXPECT_SHA256=$wrong sh "$CB" restore "$T/a.tgz" "$T/out" >/dev/null 2>&1 && bad "设备端不符却 restore 通过"
[ -e "$T/out/data/region/active" ] && bad "不符时写了文件"
CB_UCI=false CB_EXPECT_SHA256=$(printf '%s' "$good" | tr a-f A-F) sh "$CB" restore "$T/a.tgz" "$T/out" >/dev/null 2>&1 || bad "相符（大写）却 restore 失败"
cmp -s "$R/data/region/active" "$T/out/data/region/active" || bad "restore 没写回"
sh -n "$CB" || bad "sh -n config-backup.sh"
[ $fail = 0 ] && echo "backup-sha: 全部通过"
exit $fail
