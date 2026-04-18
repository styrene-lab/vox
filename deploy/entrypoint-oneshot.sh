#!/bin/sh
# Oneshot entrypoint for CronJob-mode omegon agents.
# Starts daemon, submits a prompt, waits for completion, exits.
#
# Env vars:
#   REVIEW_PROMPT    — prompt text (default: auto-generated review prompt)
#   REVIEW_CHANNEL   — Discord channel ID for findings
#   REVIEW_GUILD     — Discord guild/workspace ID
#   REVIEW_TIMEOUT   — max seconds to wait (default: 600)
#   OMEGON_PORT      — daemon control port (default: 7842)

set -eu

OMEGON_HOME="${OMEGON_HOME:-/data/omegon}"
OMEGON_CONFIG="${OMEGON_CONFIG:-/config/omegon}"
OMEGON_PORT="${OMEGON_PORT:-7842}"
DAEMON_URL="http://127.0.0.1:${OMEGON_PORT}"
REVIEW_TIMEOUT="${REVIEW_TIMEOUT:-600}"
REVIEW_CHANNEL="${REVIEW_CHANNEL:-}"
REVIEW_GUILD="${REVIEW_GUILD:-}"
SECRETS_JSON="${OMEGON_HOME}/secrets.json"

# ── Secrets bootstrap (same as entrypoint.sh) ────────────────────────
mkdir -p "${OMEGON_HOME}"

echo "{}" > "${SECRETS_JSON}"
chmod 600 "${SECRETS_JSON}"

write_recipe() {
    local name="$1" recipe="$2" current
    current="$(cat "${SECRETS_JSON}")"
    if [ "$current" = "{}" ]; then
        echo "{\"${name}\": \"${recipe}\"}" > "${SECRETS_JSON}"
    else
        sed -i 's|}$|,"'"${name}"'": "'"${recipe}"'"}|' "${SECRETS_JSON}"
    fi
}

add_recipe() {
    local name="$1" vault_var="${1}_VAULT" vault_path val
    vault_path="$(printenv "$vault_var" 2>/dev/null || true)"
    if [ -n "$vault_path" ]; then
        write_recipe "$name" "vault:${vault_path}"
        return
    fi
    val="$(printenv "$name" 2>/dev/null || true)"
    if [ -n "$val" ]; then
        write_recipe "$name" "env:${name}"
    fi
}

echo "Configuring secrets..."
add_recipe "ANTHROPIC_API_KEY"
add_recipe "OPENAI_API_KEY"
add_recipe "VOX_DISCORD_BOT_TOKEN"
add_recipe "VOX_SLACK_BOT_TOKEN"
add_recipe "VOX_SLACK_APP_TOKEN"
chmod 600 "${SECRETS_JSON}"

# Auth.json mount
AUTH_JSON="${OMEGON_CONFIG}/auth.json"
if [ -f "$AUTH_JSON" ]; then
    mkdir -p "${HOME}/.config/omegon"
    ln -sf "${AUTH_JSON}" "${HOME}/.config/omegon/auth.json"
    echo "  auth.json mounted"
fi

# ── Build prompt ──────────────────────────────────────────────────────
if [ -z "${REVIEW_PROMPT:-}" ]; then
    REVIEW_PROMPT="You are running as the overnight reviewer. Today's date is $(date +%Y-%m-%d).

Your task:
1. Read the codebase starting with recently changed files.
2. Identify 2-3 actionable findings (fewer if the code is clean).
3. Post your findings to Discord using vox_send."

    if [ -n "$REVIEW_CHANNEL" ] && [ -n "$REVIEW_GUILD" ]; then
        REVIEW_PROMPT="${REVIEW_PROMPT}

Use vox_send exactly like this:
- channel: \"discord\"
- envelope: { \"kind\": \"channel\", \"workspace\": \"${REVIEW_GUILD}\", \"channel_id\": \"${REVIEW_CHANNEL}\" }
- body: [{ \"type\": \"text\", \"content\": \"<your formatted message>\" }]"
    fi

    REVIEW_PROMPT="${REVIEW_PROMPT}

Begin your review now."
fi

# ── Start daemon in background ────────────────────────────────────────
echo "Starting omegon daemon..."
omegon serve --control-port "$OMEGON_PORT" "$@" &
OMEGON_PID=$!

# ── Wait for readiness ────────────────────────────────────────────────
echo "Waiting for daemon..."
AUTH_TOKEN=""
for i in $(seq 1 30); do
    STARTUP=$(curl -sf "${DAEMON_URL}/api/startup" 2>/dev/null || true)
    if [ -n "$STARTUP" ]; then
        AUTH_TOKEN=$(echo "$STARTUP" | jq -r '.ws_url // empty' 2>/dev/null | sed 's/.*token=//')
        if [ -n "$AUTH_TOKEN" ]; then
            break
        fi
    fi
    sleep 1
done

if [ -z "$AUTH_TOKEN" ]; then
    echo "ERROR: Daemon failed to start within 30s"
    kill "$OMEGON_PID" 2>/dev/null || true
    exit 1
fi
echo "  daemon ready (token: $(echo "$AUTH_TOKEN" | cut -c1-8)...)"

# ── Submit prompt ─────────────────────────────────────────────────────
echo "Submitting review prompt..."
HTTP_CODE=$(curl -sf -o /dev/null -w "%{http_code}" -X POST "${DAEMON_URL}/api/events" \
    -H "Content-Type: application/json" \
    -H "Authorization: Bearer ${AUTH_TOKEN}" \
    -d "$(jq -n --arg prompt "$REVIEW_PROMPT" '{
        "event_id": "oneshot-review-'"$(date +%Y%m%d-%H%M)"'",
        "source": "scheduler",
        "trigger_kind": "prompt",
        "payload": {"text": $prompt}
    }')" 2>/dev/null || echo "000")

if [ "$HTTP_CODE" != "200" ] && [ "$HTTP_CODE" != "201" ] && [ "$HTTP_CODE" != "202" ]; then
    echo "ERROR: Failed to submit prompt (HTTP $HTTP_CODE)"
    kill "$OMEGON_PID" 2>/dev/null || true
    exit 1
fi
echo "  prompt submitted"

# ── Wait for completion ───────────────────────────────────────────────
echo "Waiting for review to complete (timeout: ${REVIEW_TIMEOUT}s)..."
ELAPSED=0
while [ $ELAPSED -lt $REVIEW_TIMEOUT ]; do
    if ! kill -0 "$OMEGON_PID" 2>/dev/null; then
        echo "Omegon exited."
        break
    fi

    STATUS=$(curl -sf -H "Authorization: Bearer ${AUTH_TOKEN}" "${DAEMON_URL}/api/readyz" 2>/dev/null || echo "")
    if echo "$STATUS" | grep -q '"idle"'; then
        echo "Review complete."
        break
    fi

    sleep 10
    ELAPSED=$((ELAPSED + 10))
done

if [ $ELAPSED -ge $REVIEW_TIMEOUT ]; then
    echo "WARNING: Review timed out after ${REVIEW_TIMEOUT}s"
fi

# ── Shutdown ──────────────────────────────────────────────────────────
echo "Shutting down daemon..."
kill "$OMEGON_PID" 2>/dev/null || true
wait "$OMEGON_PID" 2>/dev/null || true
echo "Done."
