#!/bin/sh
# agent-auth.sh migrate: parsing and refusal cases (verify needs a live agent;
# it is exercised against a sidecar agent, see the commit that added this).
# SPDX-License-Identifier: MIT
SCRIPTS=${SCRIPTS:-/scripts}
PASS=0; FAIL=0
ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; }
check() { _d=$1; shift; if eval "$@"; then ok "$_d"; else bad "$_d  [$*]"; fi; }
T=$(mktemp -d)
export AGENT_AUTH_OLD=$T/start.sh AGENT_AUTH_ENV=$T/env
mig() { rm -f "$T/env"; printf '#!/bin/sh\n%s\nstart-stop-daemon -S ...\n' "$1" >"$T/start.sh"; sh "$SCRIPTS/agent-auth.sh" migrate >"$T/out" 2>&1; }

mig "export ZTE_AGENT_PASSWORD=s3cr3t!#"
check "bare value migrated" '[ "$(cat $T/env)" = "ZTE_AGENT_PASSWORD=s3cr3t!#" ]'
check "env file is 600" '[ "$(stat -c %a $T/env)" = 600 ]'
check "password not printed" '! grep -q s3cr3t $T/out'
mig "export ZTE_AGENT_PASSWORD='abc123'"
check "single-quoted value unquoted" '[ "$(cat $T/env)" = "ZTE_AGENT_PASSWORD=abc123" ]'
mig "ZTE_AGENT_PASSWORD=plain"
check "without export" '[ "$(cat $T/env)" = "ZTE_AGENT_PASSWORD=plain" ]'
mig "export ZTE_AGENT_PASSWORD=''"
check "empty refused" '[ ! -f $T/env ]'
mig "export ZTE_AGENT_PASSWORD="
check "missing value refused" '[ ! -f $T/env ]'
mig "export ZTE_AGENT_PASSWORD='it'\''s'"
check "embedded quote refused" '[ ! -f $T/env ]'
mig 'export ZTE_AGENT_PASSWORD=a\b'
check "backslash refused" '[ ! -f $T/env ]'
mig "# nothing here"
check "no line: refused" '[ ! -f $T/env ]'
printf '#!/bin/sh\nexport ZTE_AGENT_PASSWORD=same\n' >"$T/start.sh"
echo "ZTE_AGENT_PASSWORD=same" >"$T/env"
sh "$SCRIPTS/agent-auth.sh" migrate >/dev/null 2>&1
check "same password already there: fine" '[ $? = 0 ]'
echo "ZTE_AGENT_PASSWORD=other" >"$T/env"
sh "$SCRIPTS/agent-auth.sh" migrate >/dev/null 2>&1; rc=$?
check "different password there: not overwritten" '[ $rc != 0 ] && [ "$(cat $T/env)" = "ZTE_AGENT_PASSWORD=other" ]'
rm -rf "$T"
echo; echo "passed $PASS, failed $FAIL"; [ "$FAIL" = 0 ]
