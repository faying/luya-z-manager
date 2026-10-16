#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# Phase 10 10d had luyaz_dir, the rule for which state tree was live; Luya-Z rename E3 removed it
# (the new trees are the only ones): no script has it again.
# luyaz_new (10e, the core's layout switch): the same line everywhere it is.
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

# Luya-Z rename E3: the old trees are gone, so no script picks between them any more
for f in doctor-legacy.sh u60-ship.sh chaos.sh luyaz-core.init; do
    check "$f has no luyaz_dir" '! grep -q "luyaz_dir" "$SCRIPTS/$f"'
done

# 10e: the core's layout switch, luyaz_new ROOT, one line in every script that picks names by it
for f in u60-ship.sh doctor-legacy.sh u60-recover.sh chaos.sh agent-auth.sh luyaz-core.init; do
    grep '^luyaz_new()' "$SCRIPTS/$f" >"$T/$f.ln"
    check "$f has one luyaz_new" '[ "$(wc -l <"$T/$f.ln")" -eq 1 ]'
    check "$f: luyaz_new is u60-ship.sh's" 'cmp -s "$T/$f.ln" "$T/u60-ship.sh.ln"'
done
. "$T/u60-ship.sh.ln"
check "luyaz_new: no marker = old" '! luyaz_new "$T/r"'
mkdir -p "$T/r/data/luyaz/core"
check "luyaz_new: core dir without .migrated = old" '! luyaz_new "$T/r"'
: >"$T/r/data/luyaz/core/.migrated"
check "luyaz_new: .migrated = new" 'luyaz_new "$T/r"'
rm -rf "$T/r"

# Luya-Z rename D4: every copy of the ship family's switch names the same mark, executor and
# manifest (the Rust copies are checked when the tree is there: cross-repo layouts lack it)
# (u60-recover.sh and doctor-legacy.sh read the new paths only since the de-old: no mark)
check "u60-ship.sh: the D4 mark" 'grep -q "/data/luyaz/ship/.migrated" "$SCRIPTS/u60-ship.sh"'
check "u60-recover.sh: the new directory" 'grep -q "/data/luyaz/ship" "$SCRIPTS/u60-recover.sh"'
check "u60-ship.sh: executor and manifest" 'grep -q "/data/luyaz/scripts/luyaz-ship.sh" "$SCRIPTS/u60-ship.sh" && grep -q "/data/luyaz/ship/manifest.jsonl" "$SCRIPTS/u60-ship.sh"'
check "doctor-legacy.sh: manifest and heartbeat" 'grep -q "/data/luyaz/ship/manifest.jsonl" "$SCRIPTS/doctor-legacy.sh" && grep -q "/tmp/luyaz/ship/heartbeat" "$SCRIPTS/doctor-legacy.sh"'
_rs=$SCRIPTS/../../crates
if [ -f "$_rs/luyaz-core/src/paths.rs" ]; then
    check "paths.rs: the same names" 'grep -q "\"/data/luyaz/ship\"" "$_rs/luyaz-core/src/paths.rs" && grep -q "\"/data/luyaz/ship/manifest.jsonl\"" "$_rs/luyaz-core/src/paths.rs" && grep -q "\"/data/luyaz/scripts/luyaz-ship.sh\"" "$_rs/luyaz-core/src/paths.rs" && grep -q "\"/tmp/luyaz/ship\"" "$_rs/luyaz-core/src/paths.rs"'
    check "worker.rs: the same names" 'grep -q "\"/data/luyaz/ship/.migrated\"" "$_rs/luyaz-sentinel/src/worker.rs" && grep -q "\"/data/luyaz/scripts/luyaz-ship.sh\"" "$_rs/luyaz-sentinel/src/worker.rs"'
fi

echo "luyaz-dir: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
