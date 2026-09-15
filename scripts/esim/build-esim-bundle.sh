#!/usr/bin/env bash
#
# Build the /data/esim lpac bundle for the U60 Pro (aarch64 musl) and deploy it.
#
# The stock OpenWrt lpac package only ships the `at` / `uqmi` APDU backends,
# which need a /dev/cdc-wdm the U60's integrated SDX75 modem doesn't expose.
# Alpine's lpac is built WITH the `qmi_qrtr` driver, which talks straight to
# the modem's QMI UIM service over the QRTR socket family — the same path the
# ZTE daemons use, and the one verified working on this device.
#
# So we take Alpine edge's lpac binary + its shared-library closure, add a tiny
# `statx` compat shim (the device's musl 1.2.4 predates the statx symbol newer
# glib needs), and stage it all under /data/esim with a wrapper that sets the
# driver env. TLS uses the device's own /etc/ssl/certs/ca-certificates.crt,
# which is exactly the path Alpine's libcurl is compiled to look for.
#
# Usage:
#   scripts/esim/build-esim-bundle.sh [GATEWAY]
#
# GATEWAY defaults to 192.168.0.1; honours the same env var as deploy.sh.
# Requires: zig (cross-compiler for the shim), python3, curl, ssh.
set -euo pipefail

GATEWAY="${1:-${GATEWAY:-192.168.0.1}}"
SSH_PORT="${SSH_PORT:-2222}"
SSH="ssh -p ${SSH_PORT} root@${GATEWAY}"
HERE="$(cd "$(dirname "$0")" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "==> Resolving Alpine package closure for lpac"
python3 "$HERE/alpine_closure.py" lpac
PKGS_DIR="$HERE/alpine-pkgs"   # alpine_closure.py writes here

echo "==> Extracting binary + shared libraries"
mkdir -p "$WORK/stage/lib"
for apk in "$PKGS_DIR"/*.apk; do
  tar xzf "$apk" -C "$WORK/x" 2>/dev/null || { mkdir -p "$WORK/x"; tar xzf "$apk" -C "$WORK/x" 2>/dev/null; }
done
cp -a "$WORK"/x/usr/lib/*.so* "$WORK/stage/lib/" 2>/dev/null || true
cp -a "$WORK"/x/lib/*.so* "$WORK/stage/lib/" 2>/dev/null || true
cp "$WORK/x/usr/bin/lpac" "$WORK/stage/lpac"
chmod +x "$WORK/stage/lpac"

echo "==> Building statx compat shim (zig cc, aarch64-linux-musl)"
zig cc -target aarch64-linux-musl -shared -fPIC -O2 \
  "$HERE/musl_compat_shim.c" -o "$WORK/stage/lib/libmusl-compat.so"

echo "==> Building qmi_uim_probe (UIM power-cycle helper for the no-reboot switch)"
zig cc -target aarch64-linux-musl -static -O2 \
  "$HERE/../qmi_uim_probe.c" -o "$WORK/stage/qmi_uim_probe"

cat > "$WORK/stage/lpac.sh" <<'WRAP'
#!/bin/sh
export LD_LIBRARY_PATH=/data/esim/lib
export LD_PRELOAD=/data/esim/lib/libmusl-compat.so
export LPAC_APDU=qmi_qrtr
export LPAC_HTTP=curl
exec /data/esim/lpac "$@"
WRAP
chmod +x "$WORK/stage/lpac.sh"

echo "==> Deploying to ${GATEWAY}:/data/esim ($(du -sh "$WORK/stage" | cut -f1))"
tar czf - -C "$WORK/stage" . | $SSH 'rm -rf /data/esim && mkdir -p /data/esim && tar xzf - -C /data/esim'

echo "==> Verifying (chip info)"
$SSH '/data/esim/lpac.sh chip info' | head -c 400
echo
echo "==> Done. The agent's eSIM page (Mobile Network → eSIM) is now live."
