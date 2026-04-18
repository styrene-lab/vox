#!/usr/bin/env bash
# Launch: Styrene Community Agent on Ollama Cloud (llama4:scout)
#
# Two-process local deployment: omegon daemon + vox Discord bridge.
# Secrets resolved from omegon keychain (macOS) or environment.
#
# Prerequisites:
#   1. omegon and vox binaries installed (~/.local/bin/)
#   2. Discord bot with MESSAGE_CONTENT intent enabled
#   3. Secrets stored:
#        omegon secret set OLLAMA_API_KEY --stdin
#        omegon secret set VOX_DISCORD_BOT_TOKEN --stdin
#        omegon secret set VOX_DISCORD_OPERATORS --stdin
#
# Usage:
#   ./deploy/launch-discord-community-ollama.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
VOX_ROOT="$(dirname "$SCRIPT_DIR")"
AGENT_ID="styrene.discord-community-ollama"
AGENT_DIR="${SCRIPT_DIR}/catalog/${AGENT_ID}"
MODEL="ollama-cloud:llama4:scout"
OMEGON_PORT="${OMEGON_PORT:-7844}"
DAEMON_URL="http://127.0.0.1:${OMEGON_PORT}"
VOX_CONFIG="${VOX_CONFIG:-${SCRIPT_DIR}/discord-community-ollama.toml}"
LOG_DIR="${HOME}/.local/state/styrene/logs"

# ── Resolve secrets from omegon keychain ──────────────────────────────

resolve_secret() {
    local name="$1"
    local val
    # Try omegon's keychain (service: sh.styrene.omegon)
    val="$(security find-generic-password -s "sh.styrene.omegon" -a "$name" -w 2>/dev/null || true)"
    if [ -n "$val" ]; then
        echo "$val"
        return
    fi
    # Fall back to environment
    val="$(printenv "$name" 2>/dev/null || true)"
    if [ -n "$val" ]; then
        echo "$val"
    fi
}

OLLAMA_API_KEY="$(resolve_secret OLLAMA_API_KEY)"
DISCORD_TOKEN="$(resolve_secret VOX_DISCORD_BOT_TOKEN)"
DISCORD_OPERATORS="$(resolve_secret VOX_DISCORD_OPERATORS)"

if [ -z "$OLLAMA_API_KEY" ]; then
    echo "ERROR: OLLAMA_API_KEY not found in omegon keychain or environment."
    echo "  Store it:  omegon secret set OLLAMA_API_KEY --stdin"
    exit 1
fi

if [ -z "$DISCORD_TOKEN" ]; then
    echo "ERROR: VOX_DISCORD_BOT_TOKEN not found in omegon keychain or environment."
    echo "  Store it:  omegon secret set VOX_DISCORD_BOT_TOKEN --stdin"
    exit 1
fi

# ── Ensure log directory exists ───────────────────────────────────────

mkdir -p "$LOG_DIR"

# ── Check for port conflict ──────────────────────────────────────────

if curl -sf "${DAEMON_URL}/api/healthz" >/dev/null 2>&1; then
    echo "ERROR: port ${OMEGON_PORT} already in use"
    echo "  Another omegon instance may be running. Check: lsof -i :${OMEGON_PORT}"
    exit 1
fi

# ── Start omegon daemon ──────────────────────────────────────────────

echo "=== Starting omegon daemon ==="
echo "  agent:  ${AGENT_ID}"
echo "  model:  ${MODEL}"
echo "  port:   ${OMEGON_PORT}"

OLLAMA_API_KEY="$OLLAMA_API_KEY" \
VOX_DISCORD_BOT_TOKEN="$DISCORD_TOKEN" \
    omegon serve \
        --model "$MODEL" \
        --control-port "$OMEGON_PORT" \
        --strict-port \
        --agent "$AGENT_DIR" \
        --log-file "${LOG_DIR}/omegon-discord.log" \
        &
OMEGON_PID=$!

# Wait for daemon health
echo "Waiting for daemon..."
READY=false
for i in $(seq 1 30); do
    if curl -sf "${DAEMON_URL}/api/healthz" >/dev/null 2>&1; then
        echo "Daemon ready."
        READY=true
        break
    fi
    sleep 1
done
if [ "$READY" != "true" ]; then
    echo "ERROR: Daemon failed to start within 30s"
    echo "  Check logs: ${LOG_DIR}/omegon-discord.log"
    kill "$OMEGON_PID" 2>/dev/null || true
    exit 1
fi

# ── Start vox bridge ─────────────────────────────────────────────────

echo "=== Starting vox bridge (Discord) ==="
echo "  config: ${VOX_CONFIG}"
echo "  guild:  1113581684231778366"
if [ -n "$DISCORD_OPERATORS" ]; then
    echo "  operators: ${DISCORD_OPERATORS}"
fi

VOX_DISCORD_BOT_TOKEN="$DISCORD_TOKEN" \
VOX_DISCORD_OPERATORS="${DISCORD_OPERATORS:-}" \
    vox --bridge \
        --daemon-url "$DAEMON_URL" \
        --config "$VOX_CONFIG" \
        --poll-ms 500 \
        2>>"${LOG_DIR}/vox-discord.log" \
        &
VOX_PID=$!

echo ""
echo "Running:"
echo "  omegon daemon  PID=${OMEGON_PID}  port=${OMEGON_PORT}  model=${MODEL}"
echo "  vox bridge     PID=${VOX_PID}     discord → guild 1113581684231778366"
echo ""
echo "Logs:"
echo "  omegon: ${LOG_DIR}/omegon-discord.log"
echo "  vox:    ${LOG_DIR}/vox-discord.log"
echo ""
echo "Stop: kill ${OMEGON_PID} ${VOX_PID}"

# ── Trap and wait ────────────────────────────────────────────────────

cleanup() {
    echo ""
    echo "Shutting down..."
    kill "$VOX_PID" 2>/dev/null || true
    kill "$OMEGON_PID" 2>/dev/null || true
    wait
}
trap cleanup INT TERM

wait -n "$OMEGON_PID" "$VOX_PID" 2>/dev/null || true
echo "Process exited, shutting down..."
cleanup
