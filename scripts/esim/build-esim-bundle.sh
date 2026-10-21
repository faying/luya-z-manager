#!/usr/bin/env bash
#
# Package lpac for the U60 Pro / MU5250 (aarch64, musl) as the /data/esim bundle.
#
#   build-esim-bundle.sh --out FILE           write the bundle as a .tgz, no device involved
#                                             (this is what onboard/build-kit.sh uses)
#   build-esim-bundle.sh <gateway>            build, then install to /data/esim over ssh
#   GATEWAY=<gateway> build-esim-bundle.sh    same; SSH_PORT defaults to 2222
#
# Why Alpine's lpac: the modem in this device (SDX75) has no /dev/cdc-wdm, so the
# OpenWrt lpac (AT / uqmi backends only) cannot reach the card. Alpine builds lpac
# with the qmi_qrtr backend, which speaks QMI UIM over a QRTR socket, the route
# that works here. Its libcurl looks for /etc/ssl/certs/ca-certificates.crt, which
# the device already has.
#
# Bundle layout (paths inside the tarball start with ./):
#   lpac               Alpine binary
#   lpac.sh            wrapper: library path, statx shim preload, driver selection
#   lib/               lpac's shared-library closure + libmusl-compat.so
#   qmi_uim_probe      static helper that power-cycles the UIM (switch without reboot)
#
# Package versions and hashes come from alpine.lock (fetched by alpine_closure.py,
# cache dir ALPINE_PKGS_DIR). Needs python3 and zig; without a local zig the
# compiler runs in the messense/cargo-zigbuild Docker image.
set -euo pipefail

ESIM_DIR="$(cd "$(dirname "$0")" && pwd)"
SCRIPTS_DIR="$(cd "$ESIM_DIR/.." && pwd)"
CACHE="${ALPINE_PKGS_DIR:-$ESIM_DIR/alpine-pkgs}"

out_file=""
gateway=""
case "${1:-}" in
  --out)
    out_file="${2:?--out needs a file name}"
    [ "${out_file#/}" != "$out_file" ] || out_file="$PWD/$out_file"
    ;;
  *)
    gateway="${1:-${GATEWAY:-}}"
    if [ -z "$gateway" ]; then
      echo "usage: $0 --out FILE   |   GATEWAY=<device ip> $0" >&2
      exit 2
    fi
    ;;
esac

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
unpack="$tmp/unpack"
bundle="$tmp/bundle"
mkdir -p "$unpack" "$bundle/lib"

to_device() { ssh -p "${SSH_PORT:-2222}" "root@$gateway" "$@"; }

# Cross compiler: zig cc on the host, or the same zig in a container. In the
# container the scripts tree is mounted at /s and the temp dir at /w, so any
# argument under those two directories is rewritten before it is passed on.
cross_cc() {
  if command -v zig >/dev/null 2>&1; then
    zig cc "$@"
    return
  fi
  local arg mapped=()
  for arg in "$@"; do
    case "$arg" in
      "$SCRIPTS_DIR"/*) mapped+=("/s/${arg#"$SCRIPTS_DIR"/}") ;;
      "$tmp"/*)         mapped+=("/w/${arg#"$tmp"/}") ;;
      *)                mapped+=("$arg") ;;
    esac
  done
  docker run --rm -v "$SCRIPTS_DIR:/s" -v "$tmp:/w" -w /s \
    messense/cargo-zigbuild:latest zig cc "${mapped[@]}"
}

echo "==> Syncing pinned Alpine packages (alpine.lock)"
ALPINE_PKGS_DIR="$CACHE" python3 "$ESIM_DIR/alpine_closure.py"

echo "==> Unpacking lpac and its libraries"
while read -r _repo pkg ver _sha; do
  case "$_repo" in ''|'#'*) continue ;; esac
  tar xzf "$CACHE/$pkg-$ver.apk" -C "$unpack" 2>/dev/null || true
done < "$ESIM_DIR/alpine.lock"
for libdir in "$unpack/usr/lib" "$unpack/lib"; do
  [ -d "$libdir" ] || continue
  find "$libdir" -maxdepth 1 -name '*.so*' -exec cp -a {} "$bundle/lib/" \;
done
install -m 755 "$unpack/usr/bin/lpac" "$bundle/lpac"

echo "==> Compiling libmusl-compat.so (statx shim)"
cross_cc -target aarch64-linux-musl -shared -fPIC -O2 \
  "$ESIM_DIR/musl_compat_shim.c" -o "$bundle/lib/libmusl-compat.so"

echo "==> Compiling qmi_uim_probe (static)"
cross_cc -target aarch64-linux-musl -static -O2 \
  "$SCRIPTS_DIR/qmi_uim_probe.c" -o "$bundle/qmi_uim_probe"

printf '%s\n' \
  '#!/bin/sh' \
  '# lpac launcher: Alpine libraries, statx shim, QMI-over-QRTR card access, curl for HTTP' \
  'export LD_LIBRARY_PATH=/data/esim/lib' \
  'export LD_PRELOAD=/data/esim/lib/libmusl-compat.so' \
  'export LPAC_APDU=qmi_qrtr' \
  'export LPAC_HTTP=curl' \
  'exec /data/esim/lpac "$@"' > "$bundle/lpac.sh"
chmod 755 "$bundle/lpac.sh"

if [ -n "$out_file" ]; then
  COPYFILE_DISABLE=1 tar czf "$out_file" -C "$bundle" .
  echo "==> Wrote $out_file ($(du -h "$out_file" | cut -f1))"
  exit 0
fi

echo "==> Installing to $gateway:/data/esim ($(du -sh "$bundle" | cut -f1))"
COPYFILE_DISABLE=1 tar czf - -C "$bundle" . \
  | to_device 'rm -rf /data/esim && mkdir -p /data/esim && tar xzf - -C /data/esim'

echo "==> Checking the card (lpac chip info)"
to_device '/data/esim/lpac.sh chip info' | head -c 400
echo
echo "==> Done: the eSIM page in the admin web (Mobile network -> eSIM) can use the card now."
