#!/bin/sh
# config-backup.sh round trip on a fake device tree (busybox container).
# SPDX-License-Identifier: MIT
SCRIPTS=${SCRIPTS:-/scripts}
CB=$SCRIPTS/config-backup.sh
PASS=0; FAIL=0
ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; }
check() { _d=$1; shift; if eval "$@"; then ok "$_d"; else bad "$_d  [$*]"; fi; }

T=$(mktemp -d); D=$T/dev
mkdir -p $D/data/scenario $D/data/alerts $D/data/chill $D/data/tailscale/state $D/data/local/tmp $T/bin
echo 'ZTE_AGENT_PASSWORD=pw123' >$D/data/zte-agent.env
echo '{"version":2,"scenarios":[]}' >$D/data/scenario/scenarios.json
echo '{"k":1}' >$D/data/local/tmp/scheduler.json
echo '+8612300000000' >$D/data/alerts/sms-to
echo 'secret-sub-url' >$D/data/chill/chill.env
echo 'nodekey' >$D/data/tailscale/state/tailscaled.state
echo '{"runtime":1}' >$D/data/scenario/state.json      # runtime: must NOT be backed up
cat >$T/bin/uci <<X
#!/bin/sh
case "\$*" in
  "export wireless") printf "package wireless\n\nconfig wifi-iface 'main_2g'\n\toption ssid 'Test'\n" ;;
  *import*) cat > /dev/null ;;
  *show*) exit 0 ;;
esac
X
chmod +x $T/bin/uci
export CB_ROOT=$D CB_UCI=$T/bin/uci

sh $CB export >$T/b.tgz
list=$(tar tzf $T/b.tgz)
check "export includes config" 'echo "$list" | grep -q "files/data/scenario/scenarios.json" && echo "$list" | grep -q "files/data/zte-agent.env"'
check "runtime state left out" '! echo "$list" | grep -q "state.json"'
check "tailscale identity left out by default" '! echo "$list" | grep -q tailscale'
check "Wi-Fi exported as uci text" 'echo "$list" | grep -q "u60-backup/wireless.uci"'
sh $CB export --with-tailscale >$T/bts.tgz
check "--with-tailscale adds it" 'tar tzf $T/bts.tgz | grep -q "files/data/tailscale/state/tailscaled.state"'

check "verify passes on a good backup" 'sh $CB verify $T/b.tgz >/dev/null'

R=$T/root; mkdir -p $R/data/scenario; echo '{"old":1}' >$R/data/scenario/scenarios.json
sh $CB plan $T/b.tgz $R >$T/plan
check "plan: existing different file = overwrite" 'grep -q "overwrite  /data/scenario/scenarios.json" $T/plan'
check "plan: missing file = new" 'grep -q "new        /data/zte-agent.env" $T/plan'
check "plan writes nothing" '[ "$(cat $R/data/scenario/scenarios.json)" = "{\"old\":1}" ] && [ ! -e $R/data/zte-agent.env ]'
sh $CB restore $T/b.tgz $R >/dev/null
check "restore writes the files" 'cmp -s $R/data/zte-agent.env $D/data/zte-agent.env && cmp -s $R/data/scenario/scenarios.json $D/data/scenario/scenarios.json'
check "secrets restored 600" '[ "$(stat -c %a $R/data/zte-agent.env)" = 600 ] && [ "$(stat -c %a $R/data/chill/chill.env)" = 600 ]'
check "plain config restored 644" '[ "$(stat -c %a $R/data/scenario/scenarios.json)" = 644 ]'
check "previous copy kept" '[ "$(cat $R/data/scenario/scenarios.json.pre-restore)" = "{\"old\":1}" ]'
check "wireless written to etc/config under the root" 'grep -q "main_2g" $R/etc/config/wireless'

# Tampering
mkdir -p $T/evil/u60-backup; echo x >$T/evil/u60-backup/MANIFEST; echo pwn >$T/evil/escape
(cd $T/evil && tar czf $T/evil.tgz u60-backup escape)
check "entry outside u60-backup/: refused" '! sh $CB verify $T/evil.tgz 2>/dev/null && ! sh $CB restore $T/evil.tgz $R 2>/dev/null'
# dotdot.tgz.b64: a tar.gz made with Python's tarfile (busybox tar will not
# write such a name), kept as base64 text so no binary lives in the repo:
#   u60-backup/MANIFEST, u60-backup/files/../../escape
base64 -d $SCRIPTS/test/config-backup/dotdot.tgz.b64 >$T/dotdot.tgz
check "'..' in a path: refused, nothing written" '! sh $CB restore $T/dotdot.tgz $R 2>/dev/null && [ ! -e $R/escape ] && [ ! -e /tmp/escape ]'
mkdir -p $T/bad; tar xzf $T/b.tgz -C $T/bad; echo 'not json' >$T/bad/u60-backup/files/data/scenario/scenarios.json
(cd $T/bad && tar czf $T/bad.tgz u60-backup)
check "corrupted file: verify fails" '! sh $CB verify $T/bad.tgz >/dev/null 2>&1'
check "not a backup at all: refused" 'echo x >$T/x.tgz; ! sh $CB verify $T/x.tgz 2>/dev/null'

rm -rf $T
echo; echo "passed $PASS, failed $FAIL"; [ "$FAIL" = 0 ]
