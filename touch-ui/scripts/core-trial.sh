#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# core-trial.sh — run a side-by-side luyaz-core test build on the U60 Pro
# (MU5250), watch it, and put the production core back by itself.
#
# The old touch UI only reads 127.0.0.1:9460, so the test build cannot run
# next to the production one: it runs INSTEAD of it for an hour.
#
# This file is a thin shell: the code lives in luyaz-ship.sh (same directory),
# which this script sources, so $0, the commands, the log lines and the
# paths below are exactly those of the old stand-alone core-trial.sh.
# Promotion into the live slot is `luyaz-ship.sh`'s job, never this one's.
#
# On the device:
#   1. push the new build as /data/luyaz/luyaz-core.test
#      (chmod +x); this script and luyaz-ship.sh (as luyaz-ship.sh) live in /data/luyaz/scripts/
#   2. sh /data/luyaz/scripts/core-trial.sh launch [VAR=value …]
#        preflight; /etc/init.d/luyaz-core stop; start luyaz-core.test with
#        the arguments and environment of the init script's start_service
#        (extra VAR=value, e.g. LUYAZ_UBUS=cli|socket, are added);
#        once it serves /v2/state, `start` the watcher. If it does not come up,
#        production is started again at once and launch fails.
#        The test build runs WITHOUT procd: a crash is not respawned,
#        so the watcher sees it.
#   3. sh /data/luyaz/scripts/core-trial.sh status   running? + last log lines
#      sh /data/luyaz/scripts/core-trial.sh abort    stop now, restore production
#   Logs: /data/luyaz/scripts/core-trial.log       (watcher: reasons, outcome)
#         /data/luyaz/scripts/core-trial.test.log  (test build stdout/stderr)
#   `start` alone watches an already running test build (production stopped
#   by hand); `print-launch` shows the launch command without running it.
# What is checked, how it aborts and restores: the header of luyaz-ship.sh.
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
# ─────────────────────────────────────────────────────────────────────────────

# `.` of a missing file ends the whole shell on BusyBox: check first.
# (luyaz-ship.sh on the device, Luya-Z rename D4; luyaz-ship.sh is its name in the repo)
_dt_engine=${DT_ENGINE:-$(dirname "$0")/luyaz-ship.sh}
[ -n "$DT_ENGINE" ] || [ -f "$_dt_engine" ] || _dt_engine=$(dirname "$0")/luyaz-ship.sh
if [ ! -f "$_dt_engine" ]; then
    echo "core-trial: 找不到 $_dt_engine（luyaz-ship.sh 要和 core-trial.sh 放在同一个目录）" >&2
    exit 1
fi
U60S_ENTRY=core-trial
# shellcheck source=luyaz-ship.sh
. "$_dt_engine"
