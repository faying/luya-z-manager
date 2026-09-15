#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# shellcrash-fix-df-wrap.sh — patch ShellCrash ≥1.9.5 core_tools.sh for
# devices whose rootfs name is too long for busybox `df` (SDX75 OpenWrt:
# "overlayfs:/overlay/rootfs-upper").
#
# Symptom: after "正在在线获取meta核心文件 ... 100%" the menu dies with
#   /etc/ShellCrash/menu.sh: line 16: arithmetic syntax error
# Cause: store_raw_worth_it() does `df -k $BINDIR | awk 'END{print $4}'`;
# busybox df wraps the long fs name onto its own line, so the last line is
# "<blocks> <used> <avail> <use%> <mount>" and $4 becomes "0%" → $(( 0% - est )).
#
# Fix: index fields from the end (NF-2 / NF-5), correct whether df wraps or not.
# Idempotent. Re-run after every ShellCrash script update (9 → 更新) — the
# update overwrites libs/core_tools.sh.
#
#   sh scripts/shellcrash-fix-df-wrap.sh [/path/to/ShellCrash]   (default /etc/ShellCrash)
# ─────────────────────────────────────────────────────────────────────────────
CRASHDIR=${1:-${CRASHDIR:-/etc/ShellCrash}}
F="$CRASHDIR/libs/core_tools.sh"

[ -f "$F" ] || { echo "not found: $F"; exit 1; }
grep -q 'store_raw_worth_it' "$F" || { echo "no store_raw_worth_it() in $F — ShellCrash too old, nothing to patch"; exit 0; }
grep -q 'print \$(NF-2)' "$F" && { echo "already patched: $F"; exit 0; }

cp "$F" "$F.orig"
sed -i \
  -e '/store_raw_worth_it/,/^}/ s/df -T "\$TMPDIR" 2>\/dev\/null | awk '"'"'END{print \$2}'"'"'/df -T "$TMPDIR" 2>\/dev\/null | awk '"'"'END{print $(NF-5)}'"'"'/' \
  -e '/store_raw_worth_it/,/^}/ s/df -k "\$BINDIR" 2>\/dev\/null | awk '"'"'END{print \$4}'"'"'/df -k "$BINDIR" 2>\/dev\/null | awk '"'"'END{print $(NF-2)}'"'"'/' \
  -e '/store_raw_worth_it/,/^}/ s/df -T "\$BINDIR" 2>\/dev\/null | awk '"'"'END{print \$2}'"'"'/df -T "$BINDIR" 2>\/dev\/null | awk '"'"'END{print $(NF-5)}'"'"'/' \
  "$F"

if ash -n "$F" 2>/dev/null && grep -q 'print \$(NF-2)' "$F"; then
    echo "patched: $F (backup: $F.orig)"
    grep -n 'NF-' "$F"
else
    echo "patch failed, restoring"; mv "$F.orig" "$F"; exit 1
fi
