#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# doctor.sh — read-only health check of the U60 Pro (MU5250) setup (thin shim).
#
# The checks live in the core (Phase 10 X11): `<core> doctor [--tsv|--tsv2|
# --manifest|--tree-fp <dir>|--ledger-selftest|--calibrate-standby|--report …]`.
# This shim only runs it. The old shell body (doctor-legacy.sh) is gone
# (Luya-Z rename F4): no core with doctor = exit 2.
# DOC_CORE overrides the core (tests); DOC_ROOT prefixes its path.
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
# ─────────────────────────────────────────────────────────────────────────────
CORE=${DOC_CORE:-${DOC_ROOT:-}/data/luyaz/luyaz-core}
if [ -x "$CORE" ] && grep -q luyaz-doctor "$CORE" 2>/dev/null; then
    exec "$CORE" doctor "$@"
fi
echo "doctor.sh: no core with doctor at $CORE (install or ship core first)" >&2
exit 2
