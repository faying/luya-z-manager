#!/bin/sh
# Run every script test under scripts/test/ in a busybox container (same
# applets as the device). Nothing touches the device.
# SPDX-License-Identifier: MIT
SCRIPTS=$(cd "$(dirname "$0")/.." && pwd)
exec docker run --rm -v "$SCRIPTS":/scripts:ro busybox:latest sh -c '
    rc=0
    for t in /scripts/test/*/run.sh; do
        echo "### $t"
        sh "$t" || rc=1
    done
    exit $rc'
