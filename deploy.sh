#!/bin/bash
set -e

PASSWORD="${1:?Usage: ./deploy.sh <agent-password>}"
DEVICE="${DEVICE:-192.168.0.1}"
SSH_PORT=2222
TARGET=aarch64-unknown-linux-musl
BINARY=target/$TARGET/release/zte-agent
REMOTE_BIN=/data/zte-agent
STARTUP_SCRIPT=/data/local/tmp/start_zte_agent.sh
SSH="ssh -p $SSH_PORT -o StrictHostKeyChecking=no root@$DEVICE"

# 1. Build
echo "Building..."
cargo build --release --target $TARGET -p zte-agent

# 2. Size comparison
LOCAL_SIZE=$(wc -c < "$BINARY")
REMOTE_SIZE=$($SSH "wc -c < $REMOTE_BIN 2>/dev/null" 2>/dev/null || echo 0)
DIFF=$((LOCAL_SIZE - REMOTE_SIZE))
if [ "$REMOTE_SIZE" -eq 0 ]; then
    echo "Binary size: $LOCAL_SIZE bytes (first deploy)"
else
    echo "Binary size: $REMOTE_SIZE -> $LOCAL_SIZE bytes ($( [ $DIFF -ge 0 ] && echo '+' )${DIFF} bytes)"
fi

# 3. Deploy binary
echo "Deploying binary..."
$SSH "killall zte-agent 2>/dev/null; sleep 1"
cat "$BINARY" | $SSH "cat > $REMOTE_BIN && chmod +x $REMOTE_BIN"

# 4. Update password in startup script
echo "Updating password..."
$SSH "sed -i \"s|^export ZTE_AGENT_PASSWORD=.*|export ZTE_AGENT_PASSWORD='$PASSWORD'|\" $STARTUP_SCRIPT"

# 5. Restart
echo "Restarting agent..."
$SSH "sh $STARTUP_SCRIPT"

# 6. Verify
echo "Verifying..."
sleep 2
TOKEN=$(curl -sf http://$DEVICE:9090/api/auth/login -H 'Content-Type: application/json' -d "{\"password\":\"$PASSWORD\"}" | python3 -c 'import sys,json; print(json.load(sys.stdin)["data"]["token"])')
curl -sf http://$DEVICE:9090/api/device -H "Authorization: Bearer $TOKEN" > /dev/null
echo "Deploy successful!"
