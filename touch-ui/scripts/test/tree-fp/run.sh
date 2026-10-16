#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# The directory fingerprint (docs/SHIP.md) is one algorithm in three device
# scripts: u60-ship.sh tree-fp, u60-recover.sh tree-fp, doctor.sh --tree-fp.
# All three must give scripts/test/fixtures/tree.expected for fixtures/tree/
# (the Mac side, tools/u60, checks the same fixture with md5 -q), agree on
# runtime trees, and refuse the same broken ones. Runs in a busybox container
# (scripts/test/docker.sh); writes only under mktemp -d.
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────

SCRIPTS=${SCRIPTS:-/scripts}
FIX=$SCRIPTS/test/fixtures
PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "  FAIL $1"; }

fp() { # fp <ship|recover|doctor> <dir> → prints the fingerprint, exit status kept
    case "$1" in
        ship) sh "$SCRIPTS/u60-ship.sh" tree-fp "$2" ;;
        recover) sh "$SCRIPTS/u60-recover.sh" tree-fp "$2" ;;
        doctor) sh "$SCRIPTS/doctor.sh" --tree-fp "$2" ;;
    esac
}

echo "== tree-fp =="
WANT=$(cat "$FIX/tree.expected")
LWANT=$(md5sum <"$FIX/tree.listing" | cut -d' ' -f1)
[ "$LWANT" = "$WANT" ] && ok "tree.expected = md5 of tree.listing" || bad "tree.expected ($WANT) ≠ md5 of tree.listing ($LWANT)"

# an independent per-file loop (the spec, written out the slow way)
SLOW=$(cd "$FIX/tree" && find . -type f | sed 's|^\./||' | LC_ALL=C sort |
    while IFS= read -r f; do printf '%s %s\n' "$(md5sum <"$f" | cut -d' ' -f1)" "$f"; done)
[ "$(printf '%s\n' "$SLOW" | md5sum | cut -d' ' -f1)" = "$WANT" ] && ok "slow per-file loop gives tree.expected" || bad "slow per-file loop differs"

for s in ship recover doctor; do
    got=$(fp $s "$FIX/tree")
    rc=$?
    [ $rc = 0 ] && [ "$got" = "$WANT" ] && ok "$s: fixture → $WANT" || bad "$s: fixture → “$got” (rc $rc), want $WANT"
done

T=$(mktemp -d)
cp -a "$FIX/tree" "$T/t"

agree() { # agree <case> <dir> <expected fp or FAIL>
    for s in ship recover doctor; do
        got=$(fp $s "$2")
        rc=$?
        if [ "$3" = FAIL ]; then
            [ $rc != 0 ] && [ -z "$got" ] && ok "$s: $1 → refused" || bad "$s: $1 → “$got” (rc $rc), want a refusal"
        else
            [ $rc = 0 ] && [ "$got" = "$3" ] && ok "$s: $1 → $3" || bad "$s: $1 → “$got” (rc $rc), want $3"
        fi
    done
}

mkdir "$T/empty"
agree "empty directory" "$T/empty" d41d8cd98f00b204e9800998ecf8427e
mkdir -p "$T/onlydirs/a/b"
agree "only (empty) directories" "$T/onlydirs" d41d8cd98f00b204e9800998ecf8427e
mkdir "$T/t/new empty dir"
agree "an empty subdirectory does not count" "$T/t" "$WANT"
printf 'changed\n' >"$T/t/a"
CH=$(fp ship "$T/t")
[ -n "$CH" ] && [ "$CH" != "$WANT" ] && ok "one byte changed → another fingerprint" || bad "content change not seen"
agree "changed tree, same answer everywhere" "$T/t" "$CH"
cp -a "$FIX/tree" "$T/r"
mv "$T/r/a b" "$T/r/a c"
RN=$(fp ship "$T/r")
[ "$RN" != "$WANT" ] && ok "renamed file → another fingerprint" || bad "rename not seen"
agree "nonexistent directory" "$T/nope" FAIL
printf 'x\n' >"$T/plainfile"
agree "a file, not a directory" "$T/plainfile" FAIL
ln -s "$FIX/tree" "$T/linkdir"
agree "a link to a directory" "$T/linkdir" FAIL
cp -a "$FIX/tree" "$T/l"
ln -s a "$T/l/link"
agree "a symlink inside" "$T/l" FAIL
cp -a "$FIX/tree" "$T/p"
printf 'x\n' >"$T/p/pi|pe"
agree "a name with |" "$T/p" FAIL
cp -a "$FIX/tree" "$T/n"
printf 'x\n' >"$T/n/new
line"
agree "a name with a newline" "$T/n" FAIL
if command -v mkfifo >/dev/null 2>&1; then
    cp -a "$FIX/tree" "$T/f"
    mkfifo "$T/f/fifo"
    agree "a fifo inside" "$T/f" FAIL
fi
if [ "$(id -u)" != 0 ]; then
    cp -a "$FIX/tree" "$T/u"
    chmod 000 "$T/u/a"
    agree "an unreadable file" "$T/u" FAIL
    chmod 644 "$T/u/a"
fi
rm -rf "$T"

echo "tree-fp: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
