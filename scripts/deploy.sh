#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# deploy.sh — one-command deploy for the ZTE U60 Pro toolkit
#
#   ./scripts/deploy.sh web      # build web/ and push to /data/admin (LAN UI)
#   ./scripts/deploy.sh agent    # cross-compile + push + restart zte-agent
#   ./scripts/deploy.sh all      # web + agent
#   ./scripts/deploy.sh verify   # just check pages respond (on-device)
#
# Config (device IP + SSH password) is read from scripts/.deploy.env if present,
# else from the DEVICE_HOST / DEVICE_PASS environment variables. The password is
# NEVER committed — see scripts/.deploy.env.example.
#
# Notes baked in from hard-won experience:
#  • The device's HTTP :9090 is often unreachable from the Mac because the Mac's
#    default route goes through the ShellCrash TUN — so we verify ON the device
#    via `wget http://127.0.0.1:9090`, not from here.
#  • SSH (dropbear, port 2222) rate-limits rapid reconnects; this script uses one
#    connection per phase.
#  • Agent cross-compile uses zig (Homebrew musl-cross is unreliable on macOS).
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# ── Config ───────────────────────────────────────────────────────────────────
[ -f "scripts/.deploy.env" ] && . "scripts/.deploy.env"
DEVICE_PASS="${DEVICE_PASS:-}"
DEVICE_HOST="${DEVICE_HOST:-}"
PORT="${DEVICE_PORT:-2222}"
CANDIDATES="${DEVICE_HOST} 192.168.0.1 192.168.1.1"

if [ -z "$DEVICE_PASS" ]; then
  echo "✗ No device password. Create scripts/.deploy.env with:" >&2
  echo "    DEVICE_PASS='your-ssh-password'" >&2
  echo "  (optional) DEVICE_HOST='192.168.0.1'" >&2
  exit 1
fi
command -v sshpass >/dev/null || { echo "✗ sshpass not found (brew install sshpass)"; exit 1; }

SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8 -p $PORT"
_ssh() { SSHPASS="$DEVICE_PASS" sshpass -e ssh $SSH_OPTS "root@$HOST" "$@" 2>/dev/null; }
_quiet() { grep -vE "Warning|post-quantum|vulnerable|upgraded|openssh" || true; }

# ── Find the device ──────────────────────────────────────────────────────────
detect_host() {
  for ip in $CANDIDATES; do
    [ -z "$ip" ] && continue
    if SSHPASS="$DEVICE_PASS" sshpass -e ssh $SSH_OPTS "root@$ip" 'echo ok' >/dev/null 2>&1; then
      HOST="$ip"; return 0
    fi
  done
  echo "✗ Could not reach the device over SSH on: $CANDIDATES" >&2
  echo "  Set DEVICE_HOST in scripts/.deploy.env to the right IP." >&2
  exit 1
}

# ── Web ──────────────────────────────────────────────────────────────────────
deploy_web() {
  echo "▶ Building web…"
  ( cd web && npm run build >/dev/null 2>&1 ) || { echo "✗ web build failed (run 'cd web && npm run build' to see why)"; exit 1; }
  [ -f web/out/index.html ] || { echo "✗ web/out/index.html missing after build"; exit 1; }
  tar czf /tmp/u60-admin.tgz -C web/out .
  echo "▶ Deploying web to root@$HOST:/data/admin…"
  _ssh 'rm -rf /data/admin && mkdir -p /data/admin && cd /data/admin && tar xzf - && echo "  deployed $(ls | wc -l) entries"' < /tmp/u60-admin.tgz | _quiet
  verify_web
}

# ── Agent ──────────────────────────────────────────────────────────────────
deploy_agent() {
  echo "▶ Cross-compiling zte-agent (aarch64-musl via zig)…"
  export PATH="$HOME/.cargo/bin:$PATH"
  command -v cargo >/dev/null || { echo "✗ cargo not found (install rustup)"; exit 1; }
  cargo zigbuild --release --target aarch64-unknown-linux-musl -p zte-agent 2>&1 | tail -2
  BIN="target/aarch64-unknown-linux-musl/release/zte-agent"
  [ -f "$BIN" ] || { echo "✗ agent binary missing: $BIN"; exit 1; }
  echo "▶ Uploading + restarting agent on root@$HOST…"
  cat "$BIN" | _ssh '
    cat > /data/zte-agent.new && chmod +x /data/zte-agent.new
    if [ -x /etc/init.d/zte-agent ]; then
      mv /data/zte-agent.new /data/zte-agent
      /etc/init.d/zte-agent restart && sleep 1
    else
      kill $(cat /tmp/zte-agent.pid 2>/dev/null) 2>/dev/null; sleep 1
      mv /data/zte-agent.new /data/zte-agent
      sh /data/local/tmp/start_zte_agent.sh && sleep 1
    fi
    pidof zte-agent >/dev/null && echo "  agent restarted (pid $(pidof zte-agent))" || echo "  ✗ agent not running after restart"
  ' | _quiet
}

# ── Verify (always runs ON the device — Mac HTTP may be TUN-blocked) ──────────
verify_web() {
  echo "▶ Verifying pages (on-device localhost)…"
  _ssh 'for p in / /sms/ /router/wifi/ /services/shellcrash/; do
          if wget -q -O /dev/null "http://127.0.0.1:9090$p"; then echo "  OK   $p"; else echo "  FAIL $p"; fi
        done' | _quiet
}

# ── Main ─────────────────────────────────────────────────────────────────────
CMD="${1:-all}"
detect_host
echo "● device: root@$HOST:$PORT"
case "$CMD" in
  web)    deploy_web ;;
  agent)  deploy_agent ;;
  all)    deploy_web; deploy_agent ;;
  verify) verify_web ;;
  *) echo "usage: $0 {web|agent|all|verify}"; exit 1 ;;
esac
echo "✓ done"
