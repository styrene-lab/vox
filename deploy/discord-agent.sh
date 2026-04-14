#!/usr/bin/env bash
# Deploy: omegon daemon with vox Discord bridge
#
# Omegon is the agent (LLM, sessions, tools). Vox is the comms layer.
#
# Prerequisites:
#   1. omegon installed
#   2. vox built: cd /path/to/vox && cargo build --release -p vox --features discord
#   3. vox extension installed: cp -r /path/to/vox ~/.omegon/extensions/vox/
#   4. Discord bot created with MESSAGE_CONTENT intent enabled
#   5. VOX_DISCORD_BOT_TOKEN set in environment or .env
#
# Usage:
#   export VOX_DISCORD_BOT_TOKEN="your-bot-token-here"
#   ./deploy/discord-agent.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
VOX_ROOT="$(dirname "$SCRIPT_DIR")"
VOX_BIN="${VOX_ROOT}/target/release/vox"
VOX_CONFIG="${VOX_CONFIG:-${HOME}/.config/vox/vox.toml}"
OMEGON_PORT="${OMEGON_PORT:-7842}"
DAEMON_URL="http://127.0.0.1:${OMEGON_PORT}"

# Load .env if present
if [ -f "${VOX_ROOT}/.env" ]; then
    set -a; source "${VOX_ROOT}/.env"; set +a
fi

# Validate
if [ ! -f "$VOX_BIN" ]; then
    echo "ERROR: vox binary not found at $VOX_BIN"
    echo "Build it: cd $VOX_ROOT && cargo build --release -p vox --features discord"
    exit 1
fi

if [ -z "${VOX_DISCORD_BOT_TOKEN:-}" ]; then
    echo "ERROR: VOX_DISCORD_BOT_TOKEN not set"
    echo "Get one from https://discord.com/developers/applications"
    exit 1
fi

# Ensure config exists
mkdir -p "$(dirname "$VOX_CONFIG")"
if [ ! -f "$VOX_CONFIG" ]; then
    cp "${SCRIPT_DIR}/discord-agent.toml" "$VOX_CONFIG"
    chmod 600 "$VOX_CONFIG"
    echo "Created $VOX_CONFIG — edit to set guild_id if needed"
fi

echo "=== Starting omegon daemon on port ${OMEGON_PORT} ==="
omegon serve --control-port "$OMEGON_PORT" &
OMEGON_PID=$!

# Wait for daemon to be ready
echo "Waiting for daemon..."
READY=false
for i in $(seq 1 30); do
    HEALTH=$(curl -sf "${DAEMON_URL}/api/healthz" 2>/dev/null || true)
    if echo "$HEALTH" | grep -q '"ok"'; then
        echo "Daemon ready."
        READY=true
        break
    fi
    sleep 1
done
if [ "$READY" != "true" ]; then
    echo "WARNING: Daemon may not be fully ready after 30s"
fi

echo "=== Starting vox bridge (Discord) ==="
"$VOX_BIN" --bridge \
    --daemon-url "$DAEMON_URL" \
    --poll-ms 500 &
VOX_PID=$!

echo ""
echo "Running:"
echo "  omegon daemon  PID=$OMEGON_PID  port=$OMEGON_PORT"
echo "  vox bridge     PID=$VOX_PID     discord connected"
echo ""
echo "Stop with: kill $OMEGON_PID $VOX_PID"

# Wait for either to exit
wait -n "$OMEGON_PID" "$VOX_PID" 2>/dev/null || true
echo "Process exited, shutting down..."
kill "$OMEGON_PID" "$VOX_PID" 2>/dev/null || true
wait
