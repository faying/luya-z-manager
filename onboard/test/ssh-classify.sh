#!/bin/bash
# 装机包电脑端 SSH 验证失败分类的离线测试（不碰设备）：从 onboard/install.sh 抽出
# ssh_err_class / ssh_dev_class，喂典型的 ssh 报错和设备探针输出，看分到哪一类。
#   bash onboard/test/ssh-classify.sh
set -u
ONB=$(cd "$(dirname "$0")/.." && pwd)
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
sed -n '/^ssh_err_class() {/,/^}/p; /^ssh_dev_class() {/,/^}/p' "$ONB/install.sh" > "$T/f.sh"
. "$T/f.sh"
fail=0
chk() { # <期望> <实际> <说明>
  if [ "$1" = "$2" ]; then echo "ok   $3 → $2"; else echo "FAIL $3: 要 $1，得到 $2"; fail=1; fi
}
chk refused "$(ssh_err_class 'ssh: connect to host 192.168.0.1 port 2222: Connection refused')" refused
chk auth    "$(ssh_err_class 'root@192.168.0.1: Permission denied (publickey).')" auth
chk auth    "$(ssh_err_class 'Received disconnect from 192.168.0.1 port 2222:2: Too many authentication failures')" too-many
chk timeout "$(ssh_err_class 'ssh: connect to host 192.168.0.1 port 2222: Operation timed out')" mac-timeout
chk timeout "$(ssh_err_class 'ssh: connect to host 192.168.0.1 port 2222: Connection timed out')" linux-timeout
chk timeout "$(ssh_err_class 'Connection timed out during banner exchange')" banner-timeout
chk timeout "$(ssh_err_class 'ssh: connect to host 192.168.0.1 port 2222: No route to host')" no-route
chk 'other:kex_exchange_identification: read: Connection reset by peer' \
    "$(ssh_err_class "$(printf 'x\nkex_exchange_identification: read: Connection reset by peer')")" other-last-line
chk noproc   "$(ssh_dev_class "$(printf '__NOPROC__\n__NOLISTEN__')")" dev-noproc
chk nolisten "$(ssh_dev_class '__NOLISTEN__')" dev-nolisten
chk ''       "$(ssh_dev_class '')" dev-fine
[ $fail = 0 ] && echo "全部通过" || exit 1
