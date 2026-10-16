#!/bin/sh
# shadow mode must not change anything: FIFO, state files, no new files
B=/s/luyaz-sentinel
ok=0; bad=0
check() { if eval "$2"; then ok=$((ok+1)); echo "  ok   $1"; else bad=$((bad+1)); echo "  FAIL $1"; fi; }
mkdir -p /data/u60-uid /data/luyaz/ui
printf '#!/bin/sh\nwhile :; do sleep 1; done\n' >/data/luyaz/ui/luyaz-ui; chmod 755 /data/luyaz/ui/luyaz-ui
/data/luyaz/ui/luyaz-ui & UI=$!
mkfifo /tmp/luyaz-screen.ctl; echo 1 >/data/u60-uid/attempts
I0=$(stat -c %i /tmp/luyaz-screen.ctl)
find / -xdev \( -path /proc -o -path /sys \) -prune -o -print 2>/dev/null | sort >/tmp/before
$B --shadow 2>/dev/null &
SP=$!
sleep 3
kill -TERM $SP; wait $SP
find / -xdev \( -path /proc -o -path /sys \) -prune -o -print 2>/dev/null | sort >/tmp/after
check "FIFO untouched (same inode)" '[ -p /tmp/luyaz-screen.ctl ] && [ "$(stat -c %i /tmp/luyaz-screen.ctl)" = "$I0" ]'
check "no new state dir (R4)" '[ ! -e /data/u60-uid.ctl ] && [ ! -e /data/luyaz/screen ]'
check "attempts unchanged" '[ "$(cat /data/u60-uid/attempts)" = 1 ]'
check "UI still running" 'kill -0 $UI'
new=$(comm -13 /tmp/before /tmp/after | grep -v -e '^/tmp/before$' -e '^/tmp/after$')
check "only the shadow file is new" '[ "$new" = /tmp/luyaz-sentinel.shadow ]'
echo "new: $new"
grep -q "adopted running u60pro-devui pid $UI" /tmp/luyaz-sentinel.shadow && echo "  (shadow adopted the running UI)"
echo "passed $ok, failed $bad"
