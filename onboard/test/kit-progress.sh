#!/bin/sh
# 装机进度（device/install.sh 的 prog_*，/data/luyaz/kit/progress）的离线测试，不碰设备：
# 组件换成桩，跑真正的主循环。同一个包断在半路 → 再跑从没完成的组件续；换包 / 已装完 / KIT_FRESH=1 → 从头来。
#   sh onboard/test/kit-progress.sh
set -u
ONB=$(cd "$(dirname "$0")/.." && pwd)
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
LOOP=$(sed -n '/^\[ "\$1" = status \] || prog_begin/,/^\[ "\$1" = status \] || prog_finish/p' "$ONB/device/install.sh")
[ -n "$LOOP" ] || { echo "FAIL 找不到主循环"; exit 1; }
fail=0

run() { # <KIT_ID> <在哪个组件失败，空 = 都成功> <组件…>：输出跑过的组件到 $T/ran
    _id=$1 _bad=$2; shift 2
    (
        KIT_STATE=$T/state KIT_LIB_ONLY=1 KIT_ID=$_id KIT_MAC_TIME=1700000000
        export KIT_STATE KIT_LIB_ONLY KIT_ID KIT_MAC_TIME
        . "$ONB/device/install.sh" >/dev/null
        stub() { echo "$1" >> "$T/ran"; [ "$1" != "$_bad" ] || die "stub $1"; }
        do_ssh() { stub ssh; }; do_admin() { stub admin; }; do_ui() { stub ui; }
        do_esim() { stub esim; }; do_chill() { stub chill; }; do_recover() { stub recover; }
        COMPONENTS="$*"
        set -- "$@"
        eval "$LOOP"
    ) > "$T/out" 2>&1
}
ran() { tr '\n' ' ' < "$T/ran" 2>/dev/null | sed 's/ $//'; rm -f "$T/ran"; }
key() { sed -n "s/^$1=//p" "$T/state/progress"; }
expect() { # <说明> <实际> <期望>
    if [ "$2" = "$3" ]; then :; else echo "FAIL $1: 得到 [$2]，期望 [$3]"; cat "$T/out"; fail=1; fi
}

run A ui ssh admin ui esim
expect "第一次跑过" "$(ran)" "ssh admin ui"
expect "停在" "$(key current) $(key step)" "ui failed"
expect "已完成" "$(key done)" "ssh admin"
run A "" ssh admin ui esim
expect "同一个包续装" "$(ran)" "ui esim"
expect "装完" "$(key step) $(key done)" "complete ssh admin ui esim"
run A "" ssh admin ui esim
expect "已装完再跑 = 从头" "$(ran)" "ssh admin ui esim"
run A esim ssh admin ui esim
ran >/dev/null
run B "" ssh admin ui esim
expect "换包 = 从头" "$(ran)" "ssh admin ui esim"
expect "换包记的 id" "$(key kit)" "B"
run B admin ssh admin ui
ran >/dev/null
KIT_FRESH=1; export KIT_FRESH
run B "" ssh admin ui
expect "KIT_FRESH=1 = 从头" "$(ran)" "ssh admin ui"
unset KIT_FRESH
# 没有临时文件剩下；没有 KIT_ID 不记
expect "临时文件" "$(ls "$T/state")" "progress"
rm -rf "$T/state"
run "" "" ssh
expect "没 KIT_ID 不记" "$([ -e "$T/state/progress" ] && echo yes || echo no)" "no"
ran >/dev/null
# 设备上是 busybox ash：有就用它语法检查一次
if command -v busybox >/dev/null 2>&1; then busybox sh -n "$ONB/device/install.sh" || { echo "FAIL busybox sh -n"; fail=1; }; fi
[ $fail = 0 ] && echo "kit-progress: 全部通过"
exit $fail
