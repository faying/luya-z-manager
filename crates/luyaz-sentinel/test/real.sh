#!/bin/sh
# luyaz-sentinel real mode against stub UIs (arm64 container, see run.sh)
B=/s/luyaz-sentinel
ok=0; bad=0
check() { if eval "$2"; then ok=$((ok+1)); echo "  ok   $1"; else bad=$((bad+1)); echo "  FAIL $1"; fi; }
mkdir -p /data/plugins/u60pro-devui /data/luyaz/ui /etc/init.d /usr/local/bin /data/alerts
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
# a state dir from before the move (E3: no longer copied or read) must be left alone
mkdir -p /data/u60-uid && chmod 700 /data/u60-uid && echo 41 >/data/u60-uid/events.seq
ui() { pgrep -x luyaz-ui | head -1; }
vendor() { pgrep -f /usr/local/bin/zte_topsw_devui | head -1; }
$B >/tmp/sentinel.stderr 2>&1 &
SP=$!
sleep 3
U=$(ui)
check "launched our UI" '[ -n "$U" ]'
check "FIFO made" '[ -p /tmp/luyaz-screen.ctl ]'
check "heartbeat has 5 fields, says ours" '[ "$(wc -w </tmp/luyaz-sentinel.hb)" = 5 ] && [ "$(cut -d" " -f3 /tmp/luyaz-sentinel.hb)" = ours ]'
check "round ③: only the new FIFO" '[ -p /tmp/luyaz-screen.ctl ] && [ ! -e /tmp/u60-uid.ctl ]'
check "attempts counted before the launch (new dir; old no longer mirrored)" '[ "$(cat /data/luyaz/screen/attempts)" = 1 ] && [ ! -e /data/u60-uid/attempts ]'
check "state dir 0700" '[ "$(stat -c %a /data/luyaz/screen)" = 700 ]'
check "UI has no blocked signals" '[ "$(awk "/^SigBlk/ {print \$2}" /proc/$U/status)" = 0000000000000000 ]'
check "UI ignores SIGPIPE, as under u60-uid" '[ $(( 0x$(awk "/^SigIgn/ {print \$2}" /proc/$U/status) & 0x1000 )) != 0 ]'
check "UI in its own session" '[ "$(awk "{print \$6}" /proc/$U/stat)" = "$U" ]'
echo vendor >/tmp/luyaz-screen.ctl
sleep 3
check "vendor request: ours stopped, vendor on" '[ -z "$(ui)" ] && [ -n "$(vendor)" ]'
check "/tmp keeps 1777" '[ "$(stat -c %a /tmp)" = 1777 ]'
check "want file says vendor" 'grep -q vendor /tmp/luyaz/screen.want'
check "stop logged once, requested, no crash" '[ "$(grep -c "stopping luyaz-ui pid $U (requested)" /tmp/luyaz-screen.log)" = 1 ] && grep -q "pid $U ended: exit 0 (requested)" /tmp/luyaz-screen.log && ! grep -q devui-crash /data/alerts/queue 2>/dev/null'
echo devui >/tmp/luyaz-screen.ctl
sleep 4
U=$(ui)
check "devui request on the new FIFO: ours back, vendor stopped" '[ -n "$U" ] && [ -z "$(vendor)" ]'
touch /tmp/ui-crash
kill -SEGV "$U"
sleep 8
check "crash then crash on start: gave up" '[ -f /data/luyaz/screen/gave-up ] && [ -n "$(vendor)" ] && [ -z "$(ui)" ]'
check "two devui-crash, one devui-gave-up in the queue" '[ "$(grep -c devui-crash /data/alerts/queue)" = 2 ] && [ "$(grep -c devui-gave-up /data/alerts/queue)" = 1 ]'
check "events in the new dir, the old dir left as it was" '[ -s /data/luyaz/screen/events ] && [ "$(cat /data/u60-uid/events.seq)" = 41 ] && [ ! -e /data/u60-uid/events ]'
check "a crash record left" '[ "$(ls /data/crashlog/u60pro-devui/*.log | wc -l)" -ge 1 ]'
rm -f /tmp/ui-crash
echo devui >/tmp/luyaz-screen.ctl
sleep 4
U=$(ui)
check "after give-up, devui request brings ours back" '[ -n "$U" ] && [ ! -f /data/luyaz/screen/gave-up ] && [ ! -f /data/u60-uid/gave-up ]'
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
# R5 round ③: an old u60pro-devui is not ours any more, and its path is never started
kill "$U"; sleep 1; rm -f /data/luyaz/screen/attempts
cp /data/luyaz/ui/luyaz-ui /data/plugins/u60pro-devui/u60pro-devui; chmod 755 /data/plugins/u60pro-devui/u60pro-devui
rm /data/luyaz/ui/luyaz-ui
$B >>/tmp/sentinel.stderr 2>&1 &
SP=$!
sleep 3
check "luyaz-ui missing: the old program is not started instead" '[ -z "$(pgrep -x u60pro-devui)" ] && [ -z "$(ui)" ]'
kill -TERM $SP; wait $SP
echo "--- luyaz-screen.log"; cat /tmp/luyaz-screen.log
echo "passed $ok, failed $bad"
