#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# doctor.sh — read-only health check of the U60 Pro (MU5250) setup (thin shim).
#
# The checks live in the core (Phase 10 X11): `<core> doctor [--tsv|--tsv2|
# --manifest|--tree-fp <dir>|--ledger-selftest|--calibrate-standby|--report …]`.
# This shim only runs it. The old shell body (doctor-legacy.sh) is gone
# (Luya-Z rename F4): no core with doctor = exit 2.
# DOC_CORE overrides the core (tests); DOC_ROOT prefixes its path. Without options the
# report ends with `net-tune.sh show` (DOC_NET_TUNE overrides it).
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
# ─────────────────────────────────────────────────────────────────────────────
CORE=${DOC_CORE:-${DOC_ROOT:-}/data/luyaz/luyaz-core}
NET_TUNE=${DOC_NET_TUNE:-${DOC_ROOT:-}/data/luyaz/scripts/net-tune.sh}
if [ -x "$CORE" ] && grep -q luyaz-doctor "$CORE" 2>/dev/null; then
    # with options (--tsv, --manifest, …) the core's output only, unchanged
    [ $# -gt 0 ] || [ ! -f "$NET_TUNE" ] && exec "$CORE" doctor "$@"
    # the plain report: then the network tuning's status (net-tune.sh show, net-performance 1b)
    "$CORE" doctor
    rc=$?
    echo
    echo "网络调参（net-tune.sh show）:"
    sh "$NET_TUNE" show 2>&1 | sed 's/^/  /'
    exit $rc
fi
echo "doctor.sh: no core with doctor at $CORE (install or ship core first)" >&2
exit 2
