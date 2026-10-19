#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# Phase 10 10d had luyaz_dir, the rule for which state tree was live; Luya-Z rename E3 removed it
# (the new trees are the only ones): no script has it again.
# luyaz_new (10e, the core's layout switch) and the .migrated marks went in rename F5/6.
#
#   docker run --rm -v "$PWD/scripts":/scripts:ro u60-device-busybox:20260928 \
#       sh /scripts/test/luyaz-dir/run.sh
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────

SCRIPTS=${SCRIPTS:-/scripts}
PASS=0
FAIL=0
check() {
    if eval "$2"; then PASS=$((PASS + 1)); else FAIL=$((FAIL + 1)); echo "FAIL: $1"; fi
}

T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT

# Luya-Z rename E3 removed luyaz_dir, F5/6 the last layout switches (luyaz_new, the .migrated
# marks): every script knows the new layout only (luyaz-recover.sh aside: it is replaced only by
# install-recover, and its refusals to put an old-layout file back stay)
for f in luyaz-ship.sh chaos.sh web-auth.sh luyaz-core.init luyaz-fallback.sh doctor.sh config-backup.sh; do
    check "$f has no layout switch" '! grep -q -e "luyaz_dir" -e "luyaz_new" -e "\.migrated" "$SCRIPTS/$f"'
done

# Luya-Z rename D4: every copy of the ship family names the same executor and manifest (the
# Rust copies are checked when the tree is there: cross-repo layouts lack it)
check "luyaz-recover.sh: the new directory" 'grep -q "/data/luyaz/ship" "$SCRIPTS/luyaz-recover.sh"'
check "luyaz-ship.sh: executor and manifest" 'grep -q "/data/luyaz/scripts/luyaz-ship.sh" "$SCRIPTS/luyaz-ship.sh" && grep -q "/data/luyaz/ship/manifest.jsonl" "$SCRIPTS/luyaz-ship.sh"'
_rs=$SCRIPTS/../../crates
if [ -f "$_rs/luyaz-core/src/paths.rs" ]; then
    # (E3 left the Rust side on the new names only: paths.rs keeps the manifest, worker.rs the rest)
    check "paths.rs: the same manifest" 'grep -q "\"/data/luyaz/ship/manifest.jsonl\"" "$_rs/luyaz-core/src/paths.rs"'
    check "worker.rs: the same names" 'grep -q "\"/data/luyaz/scripts/luyaz-ship.sh\"" "$_rs/luyaz-sentinel/src/worker.rs" && grep -q "\"/data/luyaz/ship/txn\"" "$_rs/luyaz-sentinel/src/worker.rs" && grep -q "\"/tmp/luyaz/ship/heartbeat\"" "$_rs/luyaz-sentinel/src/worker.rs"'
fi

echo "luyaz-dir: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
