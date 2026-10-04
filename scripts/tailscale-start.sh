#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# tailscale-start.sh — DEPRECATED. Use scripts/tailscale/start.sh instead.
#
# This was the old boot-time Tailscale bring-up. It is no longer maintained
# because it was unsafe to run from /etc/rc.local on the U60 Pro (MU5250):
#   - it waited for the network in the foreground (up to about 4 minutes).
#     rc.local runs inside the boot sequence, and the step that marks a new
#     firmware slot as good (abctl --set_success, S97ab-updater) only runs
#     after rc.local returns;
#   - it set the system clock and restarted the vendor NTP service;
#   - it added its own iptables rules, killed processes with -9 and started
#     other software (ShellCrash).
#
# The replacement, scripts/tailscale/start.sh, returns to rc.local at once,
# never touches the clock or the vendor services, can be switched off without
# editing rc.local, and falls back to Tailscale's userspace mode when kernel
# networking looks unsafe. How to install it and how to move over from this
# script: scripts/tailscale/README.md.
#
# Running this file now only prints this notice; it starts nothing.
# SPDX-License-Identifier: MIT
# ─────────────────────────────────────────────────────────────────────────────
cat >&2 <<'EOF'
tailscale-start.sh is deprecated and does nothing.
Use scripts/tailscale/start.sh (installed as /data/tailscale/start.sh);
see scripts/tailscale/README.md for install and migration steps.
EOF
exit 1
