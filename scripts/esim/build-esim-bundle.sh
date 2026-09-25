#!/usr/bin/env bash
#
# Build the /data/esim lpac bundle for the U60 Pro / MU5250 (aarch64 musl).
#
# The stock OpenWrt lpac package only ships the `at` / `uqmi` APDU backends,
# which need a /dev/cdc-wdm the U60's integrated SDX75 modem doesn't expose.
# Alpine's lpac is built WITH the `qmi_qrtr` driver, which talks straight to
# the modem's QMI UIM service over the QRTR socket family — the same path the
# ZTE daemons use, and the one verified working on this device.
#
# So we take Alpine's lpac binary + its shared-library closure (pinned by
# version + sha256 in alpine.lock), add a tiny `statx` compat shim (the
# device's musl 1.2.4 predates the statx symbol newer glib needs), and stage it
# all under /data/esim with a wrapper that sets the driver env. TLS uses the
# device's own /etc/ssl/certs/ca-certificates.crt, which is exactly the path
# Alpine's libcurl is compiled to look for.
#
# Usage:
#   scripts/esim/build-esim-bundle.sh --out esim.tgz     # build the tarball only (no device)
#   GATEWAY=192.168.0.1 scripts/esim/build-esim-bundle.sh   # build + deploy over ssh
#   scripts/esim/build-esim-bundle.sh 192.168.0.1           # same, gateway as argument
#
# The install kit (onboard/build-kit.sh) calls the --out mode itself.
# Requires: zig (cross-compiler for the shim; or Docker, see ZIG below), python3.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
OUT=""
if [ "${1:-}" = "--out" ]; then
  OUT="${2:?--out needs a file name}"
  case "$OUT" in /*) ;; *) OUT="$PWD/$OUT" ;; esac
else
  GATEWAY="${1:-${GATEWAY:-}}"
  [ -n "$GATEWAY" ] || { echo "usage: $0 --out FILE   |   GATEWAY=<device ip> $0" >&2; exit 2; }
  SSH_PORT="${SSH_PORT:-2222}"
  SSH="ssh -p ${SSH_PORT} root@${GATEWAY}"
fi
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# zig on the host if present, otherwise the same zig inside Docker
if command -v zig >/dev/null 2>&1; then
  zigcc() { zig cc "$@"; }
else
  # paths under scripts/ and $WORK are remapped to the container's /s and /w
  zigcc() {
    local a args=() top
    top="$(cd "$HERE/.." && pwd)"
    for a in "$@"; do
      case "$a" in
        "$top"/*) args+=("/s/${a#"$top"/}") ;;
        "$WORK"/*) args+=("/w/${a#"$WORK"/}") ;;
        *) args+=("$a") ;;
      esac
    done
    docker run --rm -v "$top:/s" -v "$WORK:/w" -w /s messense/cargo-zigbuild:latest zig cc "${args[@]}"
  }
fi

echo "==> Fetching pinned Alpine packages (alpine.lock)"
python3 "$HERE/alpine_closure.py"
PKGS_DIR="${ALPINE_PKGS_DIR:-$HERE/alpine-pkgs}"

echo "==> Extracting lpac + shared libraries"
mkdir -p "$WORK/x" "$WORK/stage/lib"
grep -v '^#' "$HERE/alpine.lock" | while read -r _ name ver _; do
  [ -z "$name" ] || tar xzf "$PKGS_DIR/$name-$ver.apk" -C "$WORK/x" 2>/dev/null || true
done
cp -a "$WORK"/x/usr/lib/*.so* "$WORK/stage/lib/" 2>/dev/null || true
cp -a "$WORK"/x/lib/*.so* "$WORK/stage/lib/" 2>/dev/null || true
cp "$WORK/x/usr/bin/lpac" "$WORK/stage/lpac"
chmod 755 "$WORK/stage/lpac"

echo "==> Building statx compat shim (zig cc, aarch64-linux-musl)"
zigcc -target aarch64-linux-musl -shared -fPIC -O2 \
  "$HERE/musl_compat_shim.c" -o "$WORK/stage/lib/libmusl-compat.so"

echo "==> Building qmi_uim_probe (UIM power-cycle helper for the no-reboot switch)"
zigcc -target aarch64-linux-musl -static -O2 \
  "$(cd "$HERE/.." && pwd)/qmi_uim_probe.c" -o "$WORK/stage/qmi_uim_probe"

cat > "$WORK/stage/lpac.sh" <<'WRAP'
#!/bin/sh
export LD_LIBRARY_PATH=/data/esim/lib
export LD_PRELOAD=/data/esim/lib/libmusl-compat.so
export LPAC_APDU=qmi_qrtr
export LPAC_HTTP=curl
exec /data/esim/lpac "$@"
WRAP
chmod 755 "$WORK/stage/lpac.sh"

if [ -n "$OUT" ]; then
  COPYFILE_DISABLE=1 tar czf "$OUT" -C "$WORK/stage" .
  echo "==> Wrote $OUT ($(du -h "$OUT" | cut -f1))"
  exit 0
fi

echo "==> Deploying to ${GATEWAY}:/data/esim ($(du -sh "$WORK/stage" | cut -f1))"
COPYFILE_DISABLE=1 tar czf - -C "$WORK/stage" . | $SSH 'rm -rf /data/esim && mkdir -p /data/esim && tar xzf - -C /data/esim'

echo "==> Verifying (chip info)"
$SSH '/data/esim/lpac.sh chip info' | head -c 400
echo
echo "==> Done. The agent's eSIM page (Mobile Network → eSIM) is now live."
