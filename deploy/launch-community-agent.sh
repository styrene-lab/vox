#!/bin/sh
set -e

# Launch the Styrene Community Agent container.
# Resolves secrets from the omegon keychain and auth.json on the host,
# then passes them into the container.
#
# Prerequisites:
#   omegon secret set VOX_DISCORD_BOT_TOKEN --stdin
#   omegon secret set VOX_DISCORD_OPERATORS --stdin   (optional)
#
# Usage:
#   ./deploy/launch-community-agent.sh [--guild-id ID] [--require-mention true|false]

CONTAINER_NAME="${CONTAINER_NAME:-styrene-community}"
HOST_PORT="${HOST_PORT:-7843}"
IMAGE="${IMAGE:-localhost/auspex-agents:latest}"
AGENT_ID="styrene.community-agent"

# ── Resolve secrets from omegon keychain ──────────────────────────────

resolve_secret() {
    local name="$1"
    # Try omegon's keychain (service: sh.styrene.omegon)
    local val
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

DISCORD_TOKEN="$(resolve_secret VOX_DISCORD_BOT_TOKEN)"
DISCORD_OPERATORS="$(resolve_secret VOX_DISCORD_OPERATORS)"

if [ -z "$DISCORD_TOKEN" ]; then
    echo "ERROR: VOX_DISCORD_BOT_TOKEN not found in omegon keychain or environment."
    echo "  Store it:  omegon secret set VOX_DISCORD_BOT_TOKEN --stdin"
    echo "  Or pass:   VOX_DISCORD_BOT_TOKEN=... $0"
    exit 1
fi

# ── Parse flags ───────────────────────────────────────────────────────

GUILD_ID=""
REQUIRE_MENTION="true"

while [ $# -gt 0 ]; do
    case "$1" in
        --guild-id) GUILD_ID="$2"; shift 2 ;;
        --require-mention) REQUIRE_MENTION="$2"; shift 2 ;;
        *) echo "Unknown flag: $1"; exit 1 ;;
    esac
done

# ── Stop existing container ───────────────────────────────────────────

if podman ps -a --format '{{.Names}}' | grep -q "^${CONTAINER_NAME}$"; then
    echo "Stopping existing ${CONTAINER_NAME}..."
    podman stop "$CONTAINER_NAME" 2>/dev/null || true
    podman rm "$CONTAINER_NAME" 2>/dev/null || true
fi

# ── Build run command ─────────────────────────────────────────────────

RUN_ARGS="-d --name $CONTAINER_NAME -p ${HOST_PORT}:7842"
RUN_ARGS="$RUN_ARGS -v $HOME/.config/omegon:/config/omegon:ro"
RUN_ARGS="$RUN_ARGS -e VOX_DISCORD_BOT_TOKEN=$DISCORD_TOKEN"
RUN_ARGS="$RUN_ARGS -e VOX_DISCORD_REQUIRE_MENTION=$REQUIRE_MENTION"

if [ -n "$DISCORD_OPERATORS" ]; then
    RUN_ARGS="$RUN_ARGS -e VOX_DISCORD_OPERATORS=$DISCORD_OPERATORS"
fi

if [ -n "$GUILD_ID" ]; then
    RUN_ARGS="$RUN_ARGS -e VOX_DISCORD_GUILD_ID=$GUILD_ID"
fi

echo "Launching ${CONTAINER_NAME} on port ${HOST_PORT}..."
echo "  image:     $IMAGE"
echo "  agent:     $AGENT_ID"
echo "  auth:      volume mount (~/.config/omegon/auth.json)"
echo "  discord:   token from keychain, operators=${DISCORD_OPERATORS:-<not set>}"

# shellcheck disable=SC2086
podman run $RUN_ARGS "$IMAGE" --agent "$AGENT_ID"

# ── Wait for readiness ────────────────────────────────────────────────

echo "Waiting for readiness..."
for i in $(seq 1 20); do
    if curl -sf "http://127.0.0.1:${HOST_PORT}/api/readyz" >/dev/null 2>&1; then
        echo "Ready."
        curl -sf "http://127.0.0.1:${HOST_PORT}/api/readyz"
        echo ""
        exit 0
    fi
    sleep 1
done

echo "WARNING: container started but readyz not confirmed within 20s"
echo "Check logs: podman logs $CONTAINER_NAME"
