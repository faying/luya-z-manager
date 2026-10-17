#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# doctor.sh — read-only health check of the U60 Pro (MU5250) setup (thin shim).
#
# Phase 10 X11 (manager docs/designs/u60-phase10.md「X11 核心接管体检」): the
# checks now live in the core, `<core> doctor [--tsv|--tsv2|--manifest|
# --tree-fp <dir>|--ledger-selftest|--calibrate-standby]`, same rows, same output, same exit code.
# This shim runs it; it falls back to the old body (doctor-legacy.sh, kept for
# the observation period) when
#   - the core is not there or predates X11 (no `doctor` in it: such a core
#     would take the word for its own command line), or
#   - the core says 64: a mode it does not have (--report; --calibrate-standby
#     before it was ported); it prints nothing to stdout then.
# The core is picked by the layout switch, as in u60-ship.sh: /data/luyaz/core/
# .migrated = /data/luyaz/luyaz-core, else /data/plugins/zwrt-datad/zwrt-datad.
# DOC_CORE overrides it (tests); every DOC_* of doctor-legacy.sh works for both.
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────
case "$0" in */*) HERE=${0%/*} ;; *) HERE=. ;; esac
LROOT=${DOC_ROOT:-}
if [ -f "$LROOT/data/luyaz/core/.migrated" ]; then
    CORE=$LROOT/data/luyaz/luyaz-core
else
    CORE=$LROOT/data/plugins/zwrt-datad/zwrt-datad
fi
CORE=${DOC_CORE:-$CORE}
if [ -x "$CORE" ] && grep -q luyaz-doctor "$CORE" 2>/dev/null; then
    "$CORE" doctor "$@"
    rc=$?
    [ "$rc" = 64 ] || exit "$rc"
fi
# next to this file; a lone copy (the kit's install.sh doctor pushes only doctor.sh to /tmp)
# uses the installed one
for L in "$HERE/doctor-legacy.sh" /data/luyaz/scripts/doctor-legacy.sh; do
    [ -f "$L" ] && exec sh "$L" "$@"
done
echo "doctor.sh: no core with doctor ($CORE) and no doctor-legacy.sh" >&2
exit 2
