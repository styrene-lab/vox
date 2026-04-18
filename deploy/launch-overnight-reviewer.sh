#!/usr/bin/env bash
# Launch: Overnight Code Reviewer (oneshot)
#
# Starts omegon with the overnight-reviewer agent, which scans target repos,
# posts findings to Discord via vox, then exits.
#
# Designed to be triggered by launchd calendar interval (daily at 03:00).
#
# Prerequisites:
#   omegon secret set VOX_DISCORD_BOT_TOKEN --stdin
#   OpenAI Codex OAuth token in ~/.config/omegon/auth.json
#
# Environment overrides:
#   OMEGON_PORT        — daemon port (default: 7845)
#   REVIEW_REPOS       — comma-separated repo paths (default: vox,nex)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
AGENT_DIR="${SCRIPT_DIR}/catalog/styrene.overnight-reviewer"
MODEL="openai-codex:gpt-5.4"
OMEGON_PORT="${OMEGON_PORT:-7845}"
DAEMON_URL="http://127.0.0.1:${OMEGON_PORT}"
VOX_CONFIG="${SCRIPT_DIR}/overnight-reviewer.toml"
LOG_DIR="${HOME}/.local/state/styrene/logs"
WORKSPACE="${HOME}/workspace/black-meridian/styrene-lab"

# Target repos
REVIEW_REPOS="${REVIEW_REPOS:-vox}"

# Discord channel for findings
REVIEW_CHANNEL_ID="1490751199241179387"

# ── Resolve secrets ──────────────────────────────────────────────────

resolve_secret() {
    local name="$1"
    local val
    val="$(security find-generic-password -s "sh.styrene.omegon" -a "$name" -w 2>/dev/null || true)"
    if [ -n "$val" ]; then
        echo "$val"
        return
    fi
    val="$(printenv "$name" 2>/dev/null || true)"
    if [ -n "$val" ]; then
        echo "$val"
    fi
}

DISCORD_TOKEN="$(resolve_secret VOX_DISCORD_BOT_TOKEN)"

if [ -z "$DISCORD_TOKEN" ]; then
    echo "ERROR: VOX_DISCORD_BOT_TOKEN not found"
    exit 1
fi

# OpenRouter — resolved from ~/.config/omegon/auth.json
if [ ! -f "${HOME}/.config/omegon/auth.json" ]; then
    echo "WARNING: No auth.json found — OpenRouter key may not be available"
fi

# ── Ensure log directory ─────────────────────────────────────────────

mkdir -p "$LOG_DIR"

# ── Check port ───────────────────────────────────────────────────────

if curl -sf "${DAEMON_URL}/api/healthz" >/dev/null 2>&1; then
    echo "ERROR: port ${OMEGON_PORT} already in use"
    exit 1
fi

# ── Pull latest for target repos ─────────────────────────────────────

echo "=== Pulling target repos ==="
IFS=',' read -ra REPOS <<< "$REVIEW_REPOS"
for repo in "${REPOS[@]}"; do
    repo_path="${WORKSPACE}/${repo}"
    if [ -d "$repo_path/.git" ]; then
        echo "  pulling ${repo}..."
        git -C "$repo_path" pull --ff-only --quiet 2>/dev/null || echo "  warning: pull failed for ${repo}, using local state"
    else
        echo "  warning: ${repo_path} is not a git repo, skipping pull"
    fi
done

# ── Build the prompt ─────────────────────────────────────────────────

# Gather recent git activity for each repo
REPO_CONTEXT=""
for repo in "${REPOS[@]}"; do
    repo_path="${WORKSPACE}/${repo}"
    if [ -d "$repo_path/.git" ]; then
        recent=$(git -C "$repo_path" log --oneline --since="24 hours ago" 2>/dev/null || echo "(no recent commits)")
        REPO_CONTEXT="${REPO_CONTEXT}
--- ${repo} (${repo_path}) ---
Recent commits (24h):
${recent}
"
    fi
done

PROMPT=$(cat <<EOF
You are running as the overnight reviewer. Today's date is $(date +%Y-%m-%d).

Target repositories and recent activity:
${REPO_CONTEXT}

Your task:
1. For each repo, read the codebase. Start with recently changed files if there are commits in the last 24h, otherwise pick a module to cold-review.
2. Identify 2-3 actionable findings per repo (fewer if the code is clean).
3. Post your findings to Discord channel ${REVIEW_CHANNEL_ID} using vox_send.

Use vox_send exactly like this:
- channel: "discord"
- envelope: { "kind": "channel", "workspace": "1113581684231778366", "channel_id": "${REVIEW_CHANNEL_ID}" }
- body: [{ "type": "text", "content": "<your formatted message>" }]

Begin your review now.
EOF
)

# ── Run omegon oneshot ───────────────────────────────────────────────

echo "=== Starting overnight review ==="
echo "  model:   ${MODEL}"
echo "  repos:   ${REVIEW_REPOS}"
echo "  channel: ${REVIEW_CHANNEL_ID}"
echo "  log:     ${LOG_DIR}/overnight-reviewer.log"

STARTUP_FILE=$(mktemp)

VOX_DISCORD_BOT_TOKEN="$DISCORD_TOKEN" \
VOX_CONFIG="$VOX_CONFIG" \
    omegon serve \
        --model "$MODEL" \
        --control-port "$OMEGON_PORT" \
        --strict-port \
        --agent "$AGENT_DIR" \
        --log-file "${LOG_DIR}/overnight-reviewer.log" \
        > "$STARTUP_FILE" 2>&1 &
OMEGON_PID=$!

# Wait for daemon health and extract auth token from startup JSON
echo "Waiting for daemon..."
AUTH_TOKEN=""
for i in $(seq 1 30); do
    if [ -s "$STARTUP_FILE" ]; then
        AUTH_TOKEN=$(grep -m1 '"type":"omegon.startup"' "$STARTUP_FILE" 2>/dev/null \
            | jq -r '.ws_url' 2>/dev/null \
            | sed 's/.*token=//' || true)
        if [ -n "$AUTH_TOKEN" ]; then
            break
        fi
    fi
    sleep 1
done
rm -f "$STARTUP_FILE"

if [ -z "$AUTH_TOKEN" ]; then
    echo "ERROR: Daemon failed to start (no startup JSON within 30s)"
    kill "$OMEGON_PID" 2>/dev/null || true
    exit 1
fi

echo "  token:   ${AUTH_TOKEN:0:8}..."

# Submit the review prompt to the daemon
echo "Submitting review prompt..."
curl -sf -X POST "${DAEMON_URL}/api/events" \
    -H "Content-Type: application/json" \
    -H "Authorization: Bearer ${AUTH_TOKEN}" \
    -d "$(jq -n --arg prompt "$PROMPT" '{
        "event_id": "overnight-review-'"$(date +%Y%m%d)"'",
        "source": "scheduler",
        "trigger_kind": "prompt",
        "payload": {"text": $prompt}
    }')" >/dev/null

# Wait for the agent to finish (poll readyz for idle state, timeout after 10 min)
echo "Waiting for review to complete..."
TIMEOUT=600
ELAPSED=0
while [ $ELAPSED -lt $TIMEOUT ]; do
    # Check if omegon is still running
    if ! kill -0 "$OMEGON_PID" 2>/dev/null; then
        echo "Omegon exited."
        break
    fi

    # Check if the agent is idle (no active session)
    STATUS=$(curl -sf -H "Authorization: Bearer ${AUTH_TOKEN}" "${DAEMON_URL}/api/readyz" 2>/dev/null || echo "")
    if echo "$STATUS" | grep -q '"idle"'; then
        echo "Review complete."
        break
    fi

    sleep 10
    ELAPSED=$((ELAPSED + 10))
done

if [ $ELAPSED -ge $TIMEOUT ]; then
    echo "WARNING: Review timed out after ${TIMEOUT}s"
fi

# Shut down
echo "Shutting down daemon..."
kill "$OMEGON_PID" 2>/dev/null || true
wait "$OMEGON_PID" 2>/dev/null || true
echo "Done. Check Discord for findings."
