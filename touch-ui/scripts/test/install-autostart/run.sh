#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# install-autostart.sh's rc.local edit (update_rc_hook), on a temp rc.local.
# Only that function and the settings above it are loaded: the rest of the
# script kills processes and runs init scripts. Busybox container:
#
#   scripts/test/docker.sh        (runs every suite under scripts/test/)
#
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────

SCRIPTS=${SCRIPTS:-/scripts}
SRC=$SCRIPTS/install-autostart.sh
PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; }
check() { _d=$1; shift; if eval "$@"; then ok "$_d"; else bad "$_d  [$*]"; fi; }

T=$(mktemp -d)
{
    sed -n '/^DEVUI_DIR=/,/^HOOK=/p' "$SRC"
    sed -n '/^update_rc_hook() {/,/^}/p' "$SRC"
} >"$T/fns.sh"
export AUTOSTART_RC=$T/etc/rc.local
. "$T/fns.sh"
RCF=$T/etc/rc.local
mkdir -p "$T/etc"
run() { update_rc_hook >"$T/out" 2>&1; R=$?; } # not $RC: the script's rc.local path

echo "## install-autostart.sh: rc.local"
cat >"$RCF" <<'RCEOF'
#!/bin/sh
/data/local/tmp/start_dropbear.sh &
[ -x /data/u60pro/start.sh ] && sh /data/u60pro/start.sh & # u60pro_devui
/etc/init.d/zte-agent start
exit 0
RCEOF
chmod 750 "$RCF"
cp "$RCF" "$T/rc.0"
run
check "old hooks out, ours in once, before exit 0" '[ $R = 0 ] && ! grep -q /data/u60pro/ "$RCF" && [ "$(grep -c u60pro_devui "$RCF")" = 1 ] && [ "$(tail -n 2 "$RCF" | head -n 1)" = "$HOOK" ] && [ "$(tail -n 1 "$RCF")" = "exit 0" ]'
check "the other lines kept in order" '[ "$(grep -v u60pro_devui "$RCF")" = "$(grep -v u60pro_devui "$T/rc.0")" ]'
check "mode kept (cp -p), no temp file left" '[ "$(stat -c %a "$RCF")" = 750 ] && [ ! -e "$RCF.autostart.tmp" ]'
check "the file as it was kept as rc.local.pre-autostart" 'cmp -s "$RCF.pre-autostart" "$T/rc.0"'
check "the new rc.local passes sh -n" 'sh -n "$RCF"'
cp "$RCF" "$T/rc.1"
ln "$RCF" "$T/rc.link" # same inode while nothing is written
run
check "run again: nothing to change, nothing written" '[ $R = 0 ] && cmp -s "$RCF" "$T/rc.1" && [ "$(stat -c %i "$RCF")" = "$(stat -c %i "$T/rc.link")" ] && cmp -s "$RCF.pre-autostart" "$T/rc.0"'
rm -f "$T/rc.link"

# a changed file is replaced by rename (a new inode), not rewritten in place
ln "$RCF" "$T/rc.link"
grep -v u60pro_devui "$RCF" >"$T/rc.x" && cat "$T/rc.x" >"$RCF" # in place: the link stays on it
run
check "a change goes in by rename: the old inode keeps the old text" '[ $R = 0 ] && [ "$(stat -c %i "$RCF")" != "$(stat -c %i "$T/rc.link")" ] && ! grep -q u60pro_devui "$T/rc.link" && grep -q u60pro_devui "$RCF"'
rm -f "$T/rc.link"

# a candidate that fails sh -n: rc.local untouched
printf '#!/bin/sh\nif true; then\nexit 0\n' >"$RCF"
cp "$RCF" "$T/rc.2"
rm -f "$RCF.pre-autostart"
run
check "fails sh -n: refused, rc.local and backup untouched, no temp file" '[ $R = 1 ] && cmp -s "$RCF" "$T/rc.2" && [ ! -e "$RCF.pre-autostart" ] && [ ! -e "$RCF.autostart.tmp" ] && grep -q "sh -n" "$T/out"'

rm -f "$RCF"
run
check "no rc.local: 1, nothing created" '[ $R = 1 ] && [ ! -e "$RCF" ] && [ ! -e "$RCF.autostart.tmp" ]'

# the script runs update_rc_hook, not the two old in-place writers
check "the script no longer writes rc.local in place (cat > \$RC)" '! grep -q "> *\"\$RC\"" "$SRC" && grep -q "^update_rc_hook\$" "$SRC"'
check "never enables or disables a vendor zte_topsw_* daemon (boot barrier)" '! grep -E "^[^#]*zte_topsw_[a-z_]* +(enable|disable)( |;|$)" "$SRC"'

# the kit's own zwrt-datad init is not a leftover to disable: the supervise.sh form and X9's
eval "$(sed -n '/^datad_is_kit_service() {/p' "$SRC")"
export AUTOSTART_DATAD_INITD=$T/zwrt-datad.init
DATAD_INITD=$AUTOSTART_DATAD_INITD
# (the X9 form as the kit's zwrt-datad.init had it; the file itself left the repo in Luya-Z rename E3)
printf '%s\n' "procd_set_param command /bin/sh -c 'exec >>/tmp/zwrt-datad.log 2>&1 </dev/null; exec \"\$0\" \"\$@\"' \"\$BIN\" -i 1000" >"$DATAD_INITD"
check "kit init (X9 form) recognised" 'datad_is_kit_service'
printf 'procd_set_param command /bin/sh "$SUPERVISE" zwrt-datad datad-crash "$BIN"\nSUPERVISE=/data/u60-guard/supervise.sh\n' >"$DATAD_INITD"
check "kit init (supervise.sh form) recognised" 'datad_is_kit_service'
printf '#!/bin/sh /etc/rc.common\nSTART=99\nstart() { /data/plugins/zwrt-datad/zwrt-datad & }\n' >"$DATAD_INITD"
check "a legacy zwrt-datad init is not the kit's" '! datad_is_kit_service'

rm -rf "$T"
echo "passed $PASS, failed $FAIL"
[ "$FAIL" = 0 ]
