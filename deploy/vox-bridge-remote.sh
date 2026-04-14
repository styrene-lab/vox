#!/usr/bin/env bash
# Deploy vox bridge pointing at a remote omegon daemon.
#
# This script runs ONLY the vox bridge — the omegon daemon runs elsewhere.
# Transport security is the operator's responsibility (SSH tunnel, Tailscale, etc.)
#
# Required env:
#   VOX_DISCORD_BOT_TOKEN   — Discord bot token
#   OMEGON_DAEMON_URL       — URL of the remote omegon daemon (e.g. https://omegon.tail12345.ts.net:7842)
#
# Optional env:
#   VOX_DAEMON_TOKEN        — Bearer token for daemon event API
#   VOX_SLACK_BOT_TOKEN     — Slack bot token (if Slack connector enabled)
#   VOX_SLACK_APP_TOKEN     — Slack app-level token (if Slack connector enabled)
#   VOX_POLL_MS             — Poll interval in ms (default: 500)
#
# Usage:
#   export OMEGON_DAEMON_URL="https://omegon-host.tail12345.ts.net:7842"
#   export VOX_DISCORD_BOT_TOKEN="your-token"
#   ./deploy/vox-bridge-remote.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
VOX_ROOT="$(dirname "$SCRIPT_DIR")"
VOX_BIN="${VOX_BIN:-${VOX_ROOT}/target/release/vox}"
POLL_MS="${VOX_POLL_MS:-500}"

# Load .env if present
if [ -f "${VOX_ROOT}/.env" ]; then
    set -a; source "${VOX_ROOT}/.env"; set +a
fi

# Validate
if [ ! -f "$VOX_BIN" ]; then
    echo "ERROR: vox binary not found at $VOX_BIN"
    echo "Build: cd $VOX_ROOT && cargo build --release -p vox --features discord,slack"
    exit 1
fi

if [ -z "${OMEGON_DAEMON_URL:-}" ]; then
    echo "ERROR: OMEGON_DAEMON_URL not set"
    echo "Example: export OMEGON_DAEMON_URL=https://omegon-host.tail12345.ts.net:7842"
    exit 1
fi

if [ -z "${VOX_DISCORD_BOT_TOKEN:-}" ] && [ -z "${VOX_SLACK_BOT_TOKEN:-}" ]; then
    echo "ERROR: No connector tokens set. Set at least one of:"
    echo "  VOX_DISCORD_BOT_TOKEN"
    echo "  VOX_SLACK_BOT_TOKEN + VOX_SLACK_APP_TOKEN"
    exit 1
fi

ARGS=(--bridge --daemon-url "$OMEGON_DAEMON_URL" --poll-ms "$POLL_MS")

if [ -n "${VOX_DAEMON_TOKEN:-}" ]; then
    ARGS+=(--daemon-token "$VOX_DAEMON_TOKEN")
fi

echo "=== vox bridge ==="
echo "  daemon:  ${OMEGON_DAEMON_URL}"
echo "  poll:    ${POLL_MS}ms"
[ -n "${VOX_DISCORD_BOT_TOKEN:-}" ] && echo "  discord: enabled"
[ -n "${VOX_SLACK_BOT_TOKEN:-}" ]   && echo "  slack:   enabled"
echo ""

exec "$VOX_BIN" "${ARGS[@]}"
