# Retired Tailscale integration

Luya-Z no longer installs, starts, ships or restores Tailscale. This note is
for owners migrating an older installation; it is not an automatic uninstall
of a separately managed VPN.

Before changing a device, establish a working local management path and inspect
the active ship transaction and `/data/luyaz/ship/watch/`: an older observation
can still treat a stopped Tailscale connection as a reason to roll back. Wait
for it to finish or follow the temporary `auto-rollback-off` procedure in
`touch-ui/docs/SHIP.md`. Check for an active `apply.sh` or older watcher too.

Inspect `/etc/rc.local` for both `/data/tailscale/start.sh` and the older
`/data/tailscale-start.sh`; check `/etc/init.d/tailscale`, `/data/tailscale`,
the daemon and its socket. Only remove entries and files known to belong to
the retired Luya-Z integration. An older `tsconfig` may contain an auth key;
old state files and archives may contain node identity. Decide whether to log
out the node while it is still running and how to handle retained backups.

The old `start.sh stop` stops its watcher or procd instance and attempts daemon
cleanup. Check the result, including the interface, routing rules, table 52 and
firewall chains; old custom scripts may have made additional changes. Do not
delete unrelated routes, firewall rules or stock services. Changes to
`/etc/rc.local` and a reboot require the device owner's permission; preserve
the other boot lines and check the edited file's shell syntax. Check the
management path and network again after any approved reboot.

Configuration restore continues to verify old archives, but reports and skips
their `/data/tailscale` files. Old system backups, kit snapshots, old builds and
ship rollback copies can still contain historical scripts or boot lines; do not
restore them blindly after retirement.
