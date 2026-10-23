#!/bin/sh
# luyaz-sentinel real mode against stub UIs (arm64 container, see run.sh)
B=/s/luyaz-sentinel
ok=0; bad=0
check() { if eval "$2"; then ok=$((ok+1)); echo "  ok   $1"; else bad=$((bad+1)); echo "  FAIL $1"; fi; }
mkdir -p /data/other /data/luyaz/ui /etc/init.d /usr/local/bin /data/alerts
cat >/data/luyaz/ui/luyaz-ui <<'S'
#!/bin/sh
[ -f /tmp/ui-crash ] && { sleep 1; exit 1; }
trap 'exit 0' TERM
while :; do sleep 1; done
S
cat >/usr/local/bin/zte_topsw_devui <<'S'
#!/bin/sh
while :; do sleep 1; done
S
cat >/etc/init.d/zte_topsw_devui <<'S'
#!/bin/sh
case "$1" in
start) pgrep -f /usr/local/bin/zte_topsw_devui >/dev/null || (setsid /usr/local/bin/zte_topsw_devui </dev/null >/dev/null 2>&1 &) ;;
stop) pkill -f /usr/local/bin/zte_topsw_devui ;;
esac
S
chmod 755 /data/luyaz/ui/luyaz-ui /usr/local/bin/zte_topsw_devui /etc/init.d/zte_topsw_devui
chmod 1777 /tmp
ui() { pgrep -x luyaz-ui | head -1; }
vendor() { pgrep -f /usr/local/bin/zte_topsw_devui | head -1; }
$B >/tmp/sentinel.stderr 2>&1 &
SP=$!
sleep 3
U=$(ui)
check "launched our UI" '[ -n "$U" ]'
check "FIFO made" '[ -p /tmp/luyaz-screen.ctl ]'
check "heartbeat has 5 fields, says ours" '[ "$(wc -w </tmp/luyaz-sentinel.hb)" = 5 ] && [ "$(cut -d" " -f3 /tmp/luyaz-sentinel.hb)" = ours ]'
check "attempts counted before the launch" '[ "$(cat /data/luyaz/screen/attempts)" = 1 ]'
check "state dir 0700" '[ "$(stat -c %a /data/luyaz/screen)" = 700 ]'
check "UI has no blocked signals" '[ "$(awk "/^SigBlk/ {print \$2}" /proc/$U/status)" = 0000000000000000 ]'
check "UI ignores SIGPIPE" '[ $(( 0x$(awk "/^SigIgn/ {print \$2}" /proc/$U/status) & 0x1000 )) != 0 ]'
check "UI in its own session" '[ "$(awk "{print \$6}" /proc/$U/stat)" = "$U" ]'
echo vendor >/tmp/luyaz-screen.ctl
sleep 3
check "vendor request: ours stopped, vendor on" '[ -z "$(ui)" ] && [ -n "$(vendor)" ]'
check "/tmp keeps 1777" '[ "$(stat -c %a /tmp)" = 1777 ]'
check "want file says vendor" 'grep -q vendor /tmp/luyaz/screen.want'
check "stop logged once, requested, no crash" '[ "$(grep -c "stopping luyaz-ui pid $U (requested)" /tmp/luyaz-screen.log)" = 1 ] && grep -q "pid $U ended: exit 0 (requested)" /tmp/luyaz-screen.log && ! grep -q ui-crash /data/alerts/queue 2>/dev/null'
echo ui >/tmp/luyaz-screen.ctl
sleep 4
U=$(ui)
check "ui request: ours back, vendor stopped" '[ -n "$U" ] && [ -z "$(vendor)" ]'
touch /tmp/ui-crash
kill -SEGV "$U"
sleep 8
check "crash then crash on start: gave up" '[ -f /data/luyaz/screen/gave-up ] && [ -n "$(vendor)" ] && [ -z "$(ui)" ]'
check "two ui-crash, one ui-gave-up in the queue" '[ "$(grep -c ui-crash /data/alerts/queue)" = 2 ] && [ "$(grep -c ui-gave-up /data/alerts/queue)" = 1 ]'
check "events written" '[ -s /data/luyaz/screen/events ]'
check "a crash record left" '[ "$(ls /data/crashlog/luyaz-ui/*.log | wc -l)" -ge 1 ]'
rm -f /tmp/ui-crash
echo ui >/tmp/luyaz-screen.ctl
sleep 4
U=$(ui)
check "after give-up, ui request brings ours back" '[ -n "$U" ] && [ ! -f /data/luyaz/screen/gave-up ]'
kill -TERM $SP; wait $SP
check "TERM: sentinel exits 0" '[ $? = 0 ]'
check "TERM: FIFO removed, UI left running" '[ ! -e /tmp/luyaz-screen.ctl ] && [ "$(ui)" = "$U" ]'
check "TERM: logged" 'tail -1 /tmp/luyaz-screen.log | grep -q "stopping (luyaz-ui left running)"'
$B >>/tmp/sentinel.stderr 2>&1 &
SP=$!
sleep 2
check "restart adopts, launches nothing" 'grep -q "adopted running luyaz-ui pid $U" /tmp/luyaz-screen.log && [ "$(pgrep -x luyaz-ui | wc -l)" = 1 ]'
check "restart: the old dir still not copied" '! grep -q "screen state copied" /tmp/luyaz-screen.log'
kill -TERM $SP; wait $SP
# another UI program is not ours, and its path is never started
kill "$U"; sleep 1; rm -f /data/luyaz/screen/attempts
cp /data/luyaz/ui/luyaz-ui /data/other/otherui; chmod 755 /data/other/otherui
rm /data/luyaz/ui/luyaz-ui
$B >>/tmp/sentinel.stderr 2>&1 &
SP=$!
sleep 3
check "luyaz-ui missing: another program is not started instead" '[ -z "$(pgrep -x otherui)" ] && [ -z "$(ui)" ]'
kill -TERM $SP; wait $SP
echo "--- luyaz-screen.log"; cat /tmp/luyaz-screen.log
echo "passed $ok, failed $bad"
