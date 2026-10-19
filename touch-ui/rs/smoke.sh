#!/bin/sh
# Offscreen end-to-end smoke test of the device program (no device). The binary the trial ships
# (the static aarch64-musl build; SMOKE_BIN=native: the glibc one) runs in its device mode with
# --offscreen (memory panel, no touch, no key) in a container, against a fake core
# (rs/smoke/fake_core.py on 127.0.0.1:9460) and a fake u60d screen API (fake_agent.py on :9090,
# reached through fake_core's /v2/api/..., as u60d serves it since M6b):
#
#   lit       12 s lit, then SIGTERM: Home shows /v2/screen's verdict and operator and the
#             agent's 情景, the status bar the battery from an SSE block event; /control bodies
#             whole and answered after 1 s without a hang-up; the quiet loop engaged; exit 0
#   defaults  no font or backlight variables (the device's own paths, missing here): starts
#             dark, core paced to 5 s, exit 0 on SIGTERM
#   autooff   lit with --autooff=3000: the backlight goes off by itself, core paced to 5 s
#             once, the dark loop sleeps (at most a push a second: LVGL resumes its refresh
#             timer on any invalidation and the 1 s refresh re-sets some styles, as in C),
#             exit 0 from the dark loop
#   wakeguard started dark with --screen-off --autooff=3000 (a theme exec on a dark screen):
#             stays dark
#   restart   the UI first, core 4 s later, then core killed and started again: Home recovers
#             (verdict, battery), the stream and the pace come back, no banner at the end
#   sentinel  launched as the sentinel does (src/uid.c: cwd /data/plugins/u60pro-devui, no
#             variables but the backlight's, stdin /dev/null, output appended to the log, its
#             own session; one argument, --offscreen: there is no panel here), as
#             u60pro-devui.test, with devui.conf and the fonts at the device's own paths: the
#             conf is read, the fonts found, the comm, it ends on SIGTERM
#   exec      tests/ui_exec_test.c's checks on the binary (same pid and comm, no fd kept, the
#             launch arguments; a binary replaced on disk refused); the static one also runs
#             in a bare busybox container
#
#   LUYAZ_DEVICE_FONTS=… LUYAZ_NUNITO_DIR=… sh rs/smoke.sh [OUT_DIR]
#
# Needs `rs/build.sh device` (or native) first. Writes OUT_DIR/<case>.{texts,png,core,agent,ui}.
# Exit 0 when every check passed.
# SPDX-License-Identifier: MIT
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out=${1:-/tmp/u60-smoke}
mkdir -p "$out"; out=$(cd "$out" && pwd)
rm -f "$out"/*.texts "$out"/*.png "$out"/*.core "$out"/*.agent "$out"/*.ui "$out"/*.out "$out"/*.bl "$out"/exec.log
bin=/t/aarch64-unknown-linux-musl/release-device/luyaz-ui
[ "${SMOKE_BIN:-}" = native ] && bin=/t/release-device/luyaz-ui
docker run --rm -v u60-devui-rs-target:/t:ro -v "$here/smoke":/smoke:ro -v "$out":/o \
    -v "$LUYAZ_DEVICE_FONTS":/fonts/dev:ro -v "$LUYAZ_NUNITO_DIR":/fonts/nunito:ro u60-rust-ci:1.99.0 sh -c '
bin=$1
fonts="LUYAZ_UI_CJK_FONT=/fonts/dev/ZTEZhengYuan.ttf LUYAZ_UI_ROBOTO=/fonts/dev/Roboto.ttf LUYAZ_UI_FONT_DIR=/fonts/nunito"
# one case: fakes up, the UI for SECS s with ENV and ARGS, SIGTERM, the backlight file kept
run() {
    name=$1 secs=$2 bright=$3 envs=$4 args=$5
    mkdir -p /tmp/bl && echo $bright >/tmp/bl/brightness && echo 255 >/tmp/bl/max_brightness
    python3 -I /smoke/fake_core.py /o/$name.core & d=$!
    python3 -I /smoke/fake_agent.py /o/$name.agent & a=$!
    sleep 0.5
    env LUYAZ_UI_LOOPSTATS=1 $envs $bin --offscreen --exit-after=60000 --texts=/o/$name.texts --png=/o/$name.png $args 2>/o/$name.ui >/o/$name.out &
    ui=$!
    sleep $secs
    cat /tmp/bl/brightness >/o/$name.bl
    kill -TERM $ui; wait $ui; echo "exit $?" >>/o/$name.ui
    sleep 1.2   # the last /control answers
    kill $d $a; wait $d $a 2>/dev/null
}
run lit 12 200 "$fonts LUYAZ_UI_BACKLIGHT_DIR=/tmp/bl" ""
run defaults 6 200 "" ""
run autooff 25 200 "$fonts LUYAZ_UI_BACKLIGHT_DIR=/tmp/bl" "--autooff=3000"
run wakeguard 8 0 "$fonts LUYAZ_UI_BACKLIGHT_DIR=/tmp/bl" "--screen-off --autooff=3000"
# restart: no core at first, then one, then a new one
mkdir -p /tmp/bl && echo 200 >/tmp/bl/brightness && echo 255 >/tmp/bl/max_brightness
env LUYAZ_UI_LOOPSTATS=1 $fonts LUYAZ_UI_BACKLIGHT_DIR=/tmp/bl $bin --offscreen --exit-after=60000 --texts=/o/restart.texts 2>/o/restart.ui >/o/restart.out &
ui=$!
sleep 4
python3 -I /smoke/fake_core.py /o/restart.core & d=$!
sleep 5
kill $d; wait $d 2>/dev/null
sleep 2
python3 -I /smoke/fake_core.py /o/restart.core & d=$!
sleep 5
kill -TERM $ui; wait $ui; echo "exit $?" >>/o/restart.ui
sleep 1.2
kill $d; wait $d 2>/dev/null
# sentinel: as src/uid.c starts it, with the device paths populated
mkdir -p /data/plugins/u60pro-devui /data/luyaz/ui/fonts /usr/ui/fonts
cp /fonts/nunito/Nunito-*.ttf /data/luyaz/ui/fonts/ && cp /fonts/dev/ZTEZhengYuan.ttf /fonts/dev/Roboto.ttf /usr/ui/fonts/
printf "speed_bits=1\nautooff=0\nbright=200\nappearance=dark\nappearance_dark_from=19:00\nappearance_dark_to=07:00\nlang=zh\nusb_attach=ask\n" >/data/luyaz/ui/devui.conf
cp $bin /data/luyaz/ui/luyaz-ui.test
python3 -I /smoke/fake_core.py /o/sentinel.core & d=$!
sleep 0.5
(cd /data/luyaz/ui && LUYAZ_UI_BACKLIGHT_DIR=/tmp/bl setsid /data/luyaz/ui/luyaz-ui.test --offscreen </dev/null >>/o/sentinel.ui 2>&1) &
sleep 6
ui=$(pidof luyaz-ui.test || pidof luyaz-ui.te || true)
echo "comm=$(cat /proc/$ui/comm 2>/dev/null) cwd=$(readlink /proc/$ui/cwd 2>/dev/null)" >>/o/sentinel.ui
[ -n "$ui" ] && kill -TERM $ui; sleep 1.5; [ -n "$ui" ] && [ -d /proc/$ui ] && echo "still running" >>/o/sentinel.ui
kill $d; wait $d 2>/dev/null
mkdir -p /tmp/x && cp $bin /tmp/x/luyaz-ui.test
LUYAZ_UI_EXEC_SELFTEST=1 /tmp/x/luyaz-ui.test >/o/exec.log 2>&1; echo "exit $?" >>/o/exec.log
LUYAZ_UI_EXEC_SELFTEST=delete /tmp/x/luyaz-ui.test >>/o/exec.log 2>&1; echo "exit $?" >>/o/exec.log
' sh "$bin"
fail=0
check() { if eval "$2"; then echo "ok   $1"; else echo "FAIL $1"; fail=1; fi; }
o=$out
pushes() { sed -n 's/^loop: flushes [0-9]* pushes \([0-9]*\).*/\1/p' "$1"; }
echo "## lit"
check "SIGTERM: a clean exit" "grep -q '^exit 0$' '$o/lit.ui'"
check "the quiet loop engaged" "grep -Eq 'loop: .* quiet [1-9]' '$o/lit.ui'"
check "GET /v2/state, /v2/events, /v2/screen" "grep -q 'GET /v2/state' '$o/lit.core' && grep -q 'GET /v2/events' '$o/lit.core' && grep -q 'GET /v2/screen' '$o/lit.core'"
check "Home: the verdict from /v2/screen (顺畅)" "grep -qx '顺畅' '$o/lit.texts'"
check "Home: the operator from /v2/screen (测试运营商)" "grep -q '测试运营商' '$o/lit.texts'"
check "Home: the agent's 情景 (在家)" "grep -q '在家' '$o/lit.texts'"
check "status bar: the battery from the SSE block event (66)" "grep -qx '66' '$o/lit.texts'"
check "no banner (core down, agent lost)" "! grep -Eq '后台数据服务不可用|管理后台失联' '$o/lit.texts'"
check "agent: /api/public/status read" "grep -q '/api/public/status' '$o/lit.agent'"
check "/control: the pace (state.set_interval 1000)" "grep -q '\"action\": \"state.set_interval\", \"params\": {\"milliseconds\": 1000}' '$o/lit.core'"
check "/control: every request whole and from the screen" "! grep '/control' '$o/lit.core' | grep -q '\"complete\": false'"
check "/control: answered while the screen still listened" "grep -q '\"answered\": \"state.set_interval\"' '$o/lit.core'"
check "/control: no 'closed without an answer', no resync" "! grep -Eq 'closed without an answer|out of step' '$o/lit.ui'"
echo "## defaults"
check "starts with the device's own paths (missing here) and exits 0" "grep -q 'ui: first refresh done' '$o/defaults.ui' && grep -q '^exit 0$' '$o/defaults.ui'"
check "no backlight: dark, core paced to 5 s" "grep -q '\"milliseconds\": 5000' '$o/defaults.core'"
echo "## autooff"
check "the backlight went off by itself" "[ \"\$(cat '$o/autooff.bl')\" = 0 ]"
check "core paced to 5 s exactly once" "[ \$(grep -c '\"milliseconds\": 5000' '$o/autooff.core') = 1 ]"
check "the dark loop sleeps (≤ 12 pushes between LOOPSTATS at 10 s and 20 s)" "(set -- \$(pushes '$o/autooff.ui'); [ \$# -ge 2 ] && [ \$((\$2 - \$1)) -le 12 ])"
check "SIGTERM from the dark loop: exit 0" "grep -q '^exit 0$' '$o/autooff.ui'"
echo "## wakeguard"
check "started dark stays dark (its only 1 s pace is the one at exit)" "[ \"\$(cat '$o/wakeguard.bl')\" = 0 ] && [ \$(grep -c '\"milliseconds\": 1000' '$o/wakeguard.core') -le 1 ] && grep '/control' '$o/wakeguard.core' | tail -1 | grep -q '\"milliseconds\": 1000'"
check "exit 0" "grep -q '^exit 0$' '$o/wakeguard.ui'"
echo "## restart"
check "core came twice: two snapshots streamed" "[ \$(grep -c '\"up\": true' '$o/restart.core') = 2 ] && [ \$(grep -c 'GET /v2/events' '$o/restart.core') -ge 2 ]"
check "the pace again after each reconnect" "[ \$(grep -c 'state.set_interval' '$o/restart.core') -ge 2 ]"
check "Home current at the end (顺畅, 66), no banner" "grep -qx '顺畅' '$o/restart.texts' && grep -qx '66' '$o/restart.texts' && ! grep -q '后台数据服务不可用' '$o/restart.texts'"
check "exit 0" "grep -q '^exit 0$' '$o/restart.ui'"
echo "## sentinel"
check "devui.conf read from /data/luyaz/ui (appearance=dark)" "grep -q 'appearance=dark dark=1' '$o/sentinel.ui'"
check "fonts at the device's own paths" "grep -q 'fonts cjk=device numerals=nunito' '$o/sentinel.ui'"
check "comm luyaz-ui.test, cwd the UI dir (R5)" "grep -q 'comm=luyaz-ui.test cwd=/data/luyaz/ui' '$o/sentinel.ui'"
check "core read, the pace sent" "grep -q 'GET /v2/events' '$o/sentinel.core' && grep -q 'state.set_interval' '$o/sentinel.core'"
check "ended on SIGTERM" "grep -q 'first refresh done' '$o/sentinel.ui' && grep -q '^comm=u60pro' '$o/sentinel.ui' && ! grep -q 'still running' '$o/sentinel.ui'"
echo "## exec"
check "exec self-test (pid, comm, fds, arguments; a replaced binary refused)" "! grep -q FAIL '$o/exec.log' && [ \$(grep -c '^exit 0$' '$o/exec.log') = 2 ]"
if [ "${SMOKE_BIN:-}" != native ]; then
    check "static: runs in a bare busybox container" "docker run --rm -v u60-devui-rs-target:/t:ro -e LUYAZ_UI_EXEC_SELFTEST=1 busybox:latest $bin 2>&1 | grep -q 'exec selftest: failed 0'"
fi
echo "lit texts: $(tr '\n' '|' <"$o/lit.texts" | cut -c1-300)"
exit $fail
