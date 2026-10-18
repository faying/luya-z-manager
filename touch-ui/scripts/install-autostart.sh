#!/bin/sh
# Device-side installer, run by the install kit (manager onboard/device/install.sh)
# before it hands the screen to luyaz-sentinel: cleans up older installs, starts
# the vendor zte_topsw_devui for early panel/touch bring-up, and puts the
# start.sh hook (marker # u60pro_devui) in rc.local, which the kit then
# replaces in place with /etc/init.d/luyaz-sentinel start. start.sh itself only does
# boot chores now; it starts no UI.
#
# Expects the binaries + start.sh already copied into the plugin dirs.
#
# SPDX-License-Identifier: MIT
DEVUI_DIR=/data/plugins/u60pro-devui
DATAD_DIR=/data/plugins/zwrt-datad
UI_DIR=$DEVUI_DIR/ui
LEGACY_DIR=/data/u60pro
LEGACY_UI_DIR=/data/ui
RC=${AUTOSTART_RC:-/etc/rc.local}
HOOK="[ -x $DEVUI_DIR/start.sh ] && sh $DEVUI_DIR/start.sh >/tmp/u60pro-boot.log 2>&1 & # u60pro_devui"

# rc.local: drop the old start.sh hooks and put ours before the first exit 0,
# in one candidate written next to rc.local (/tmp is RAM: a mv from there is a
# copy, not a rename) with rc.local's mode (cp -p). The candidate must pass
# sh -n; then sync, mv over rc.local, sync: a power cut leaves the old file or
# the new one, never half of one (half = nothing starts at boot). Unchanged:
# nothing written. Changed: the file as it was is kept as $RC.pre-autostart.
# Fails (1) with rc.local untouched when it is missing or anything goes wrong.
update_rc_hook() {
    [ -f "$RC" ] || return 1
    _rt="$RC.autostart.tmp"
    rm -f "$_rt"
    if ! { cp -p "$RC" "$_rt" &&
        grep -v -e "$DEVUI_DIR/start.sh" -e "$LEGACY_DIR/start.sh" -e "u60pro_devui" "$RC" |
        awk -v hook="$HOOK" '/^exit 0/ && !d { print hook; d = 1 } { print }' >"$_rt"; }; then
        rm -f "$_rt"
        echo "rc.local: could not write $_rt; left as it was" >&2
        return 1
    fi
    if cmp -s "$_rt" "$RC"; then
        rm -f "$_rt"
        return 0
    fi
    if ! sh -n "$_rt"; then
        rm -f "$_rt"
        echo "rc.local: the new version fails sh -n; left as it was" >&2
        return 1
    fi
    if ! { cp -p "$RC" "$RC.pre-autostart" && sync && mv -f "$_rt" "$RC" && sync; }; then
        rm -f "$_rt"
        echo "rc.local: could not replace it; left as it was" >&2
        return 1
    fi
}

mkdir -p "$DEVUI_DIR" "$DATAD_DIR" "$UI_DIR"
[ -f "$LEGACY_DIR/devui.conf" ] && [ ! -f "$DEVUI_DIR/devui.conf" ] && cp -f "$LEGACY_DIR/devui.conf" "$DEVUI_DIR/devui.conf"
chmod 755 "$DEVUI_DIR/start.sh" "$DEVUI_DIR/u60pro-devui" "$DATAD_DIR/zwrt-datad" 2>/dev/null
rm -f "$DATAD_DIR/u60-datad" "$DEVUI_DIR"/*.new "$DATAD_DIR"/*.new \
      "$DEVUI_DIR/ui.tar.gz" "$DEVUI_DIR/u60pro_ui.tar.gz" "$DEVUI_DIR/boot-trace.log.tmp" 2>/dev/null
rm -rf "$DEVUI_DIR/ui_extract" 2>/dev/null

update_rc_hook

/etc/init.d/u60pro-devui disable 2>/dev/null
# /etc/init.d/zwrt-datad may now be the procd service from the install kit
# (zwrt-datad.init: it ran datad through supervise.sh, since Phase 10 X9 straight into
# /tmp/zwrt-datad.log). That one is not a leftover — leave it alone, or this script
# stops the data service it relies on.
DATAD_INITD=${AUTOSTART_DATAD_INITD:-/etc/init.d/zwrt-datad}
datad_is_kit_service() { grep -qE 'supervise\.sh|>>/tmp/zwrt-datad\.log' "$DATAD_INITD" 2>/dev/null; }
datad_is_kit_service || /etc/init.d/zwrt-datad disable 2>/dev/null
/etc/init.d/u60-datad disable 2>/dev/null
rm -f /etc/rc.d/S*u60pro-devui /etc/rc.d/K*u60pro-devui \
      $(datad_is_kit_service || echo /etc/rc.d/S*zwrt-datad /etc/rc.d/K*zwrt-datad) \
      /etc/rc.d/S*u60-datad /etc/rc.d/K*u60-datad

/etc/init.d/u60pro-devui stop 2>/dev/null
datad_is_kit_service || /etc/init.d/zwrt-datad stop 2>/dev/null
/etc/init.d/u60-datad stop 2>/dev/null

killall -9 u60pro-devui 2>/dev/null
datad_is_kit_service || killall -9 zwrt-datad 2>/dev/null
killall -9 u60-datad 2>/dev/null

rm -f "$LEGACY_DIR/u60pro-devui" "$LEGACY_DIR/u60-datad" "$LEGACY_DIR/zwrt-datad" \
      "$LEGACY_DIR/start.sh" "$LEGACY_DIR/u60pro_ui.tar.gz" "$LEGACY_DIR/ui.tar.gz" \
      "$LEGACY_DIR/boot-trace.log" "$LEGACY_DIR/boot-trace.log.tmp" "$LEGACY_DIR"/*.new 2>/dev/null
rm -rf "$LEGACY_DIR/ui_extract" 2>/dev/null
rm -rf "$LEGACY_DIR" "$LEGACY_UI_DIR"

# zte_topsw_devui is one of the firmware's boot-barrier daemons: start it, never
# enable/disable it (its ROM rc.d links are already there; enable only adds an
# overlay copy, and disable would whiteout them = stuck on the logo at boot).
/etc/init.d/zte_topsw_devui start 2>/dev/null

sh "$DEVUI_DIR/start.sh" >/tmp/u60pro-boot.log 2>&1 &

echo "--- service status ---"
if [ -x /etc/init.d/zte_topsw_devui ]; then
    /etc/init.d/zte_topsw_devui enabled >/dev/null 2>&1
echo "zte_topsw_devui_enabled=$?"
fi
echo "u60pro_devui_enabled=legacy_rc_local"
ps | grep -E 'zte_topsw_devui|u60pro-devui|zwrt-datad|u60-datad' | grep -v grep || true

echo "--- rc.local tail ---"
grep -n "u60pro\|plugins\|exit 0" "$RC" 2>/dev/null || true
