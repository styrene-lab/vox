#!/bin/sh
set -e

# Bootstrap secrets and start omegon serve.
#
# LLM auth: two modes
#   1. API key via env var (ANTHROPIC_API_KEY, OPENAI_API_KEY, etc.)
#   2. OAuth via mounted auth.json (~/.config/omegon/auth.json)
#      Mount: -v ~/.config/omegon:/config/omegon:ro
#
# Extension secrets: env vars → secrets.json recipes
#   The entrypoint writes "env:VAR" recipes so omegon's SecretsManager
#   resolves them at runtime and delivers to extensions via RPC pipe.

OMEGON_HOME="${OMEGON_HOME:-/data/omegon}"
OMEGON_CONFIG="${OMEGON_CONFIG:-/config/omegon}"
SECRETS_JSON="${OMEGON_HOME}/secrets.json"

mkdir -p "${OMEGON_HOME}"

# ── Secrets bootstrap ─────────────────────────────────────────────────
echo "{}" > "${SECRETS_JSON}"
chmod 600 "${SECRETS_JSON}"

add_recipe() {
    local name="$1"
    local env_var="${2:-$1}"
    local val
    val="$(printenv "$env_var" 2>/dev/null || true)"
    if [ -n "$val" ]; then
        # Simple JSON append without python
        local current
        current="$(cat "${SECRETS_JSON}")"
        if [ "$current" = "{}" ]; then
            echo "{\"${name}\": \"env:${env_var}\"}" > "${SECRETS_JSON}"
        else
            sed -i 's/}$/,"'"${name}"'": "env:'"${env_var}"'"}/' "${SECRETS_JSON}"
        fi
        echo "  ${name} -> env:${env_var}"
    fi
}

echo "Configuring secrets..."
# LLM provider keys (if provided via env)
add_recipe "ANTHROPIC_API_KEY"
add_recipe "OPENAI_API_KEY"
# Extension secrets
add_recipe "VOX_DISCORD_BOT_TOKEN"
add_recipe "VOX_SLACK_BOT_TOKEN"
add_recipe "VOX_SLACK_APP_TOKEN"
add_recipe "VOX_SIGNAL_PASSWORD"
add_recipe "VOX_EMAIL_PASSWORD"
add_recipe "VOX_MATRIX_PASSWORD"
add_recipe "VOX_LXMF_IDENTITY"
chmod 600 "${SECRETS_JSON}"

# ── Auth validation ───────────────────────────────────────────────────
# Check for LLM credentials: env var OR mounted auth.json
AUTH_JSON="${OMEGON_CONFIG}/auth.json"
HAS_ENV_KEY=false
HAS_AUTH_JSON=false

for var in ANTHROPIC_API_KEY OPENAI_API_KEY OPENROUTER_API_KEY; do
    if [ -n "$(printenv "$var" 2>/dev/null || true)" ]; then
        HAS_ENV_KEY=true
        break
    fi
done

if [ -f "$AUTH_JSON" ]; then
    HAS_AUTH_JSON=true
    echo "  auth.json mounted at ${AUTH_JSON}"
    # Symlink so omegon finds it at its canonical path
    mkdir -p "${HOME}/.config/omegon"
    ln -sf "${AUTH_JSON}" "${HOME}/.config/omegon/auth.json"
fi

if [ "$HAS_ENV_KEY" = false ] && [ "$HAS_AUTH_JSON" = false ]; then
    echo "ERROR: No LLM credentials found."
    echo "  Provide one of:"
    echo "    -e ANTHROPIC_API_KEY=sk-..."
    echo "    -e OPENAI_API_KEY=sk-..."
    echo "    -v ~/.config/omegon:/config/omegon:ro  (for OAuth tokens)"
    exit 1
fi

# ── Extension secret warnings ─────────────────────────────────────────
if [ -z "$(printenv VOX_DISCORD_BOT_TOKEN 2>/dev/null || true)" ]; then
    echo "  note: VOX_DISCORD_BOT_TOKEN not set — Discord connector will be degraded"
fi

# ── Start ──────────────────────────────────────────────────────────────
echo "Starting omegon daemon..."
exec omegon serve --control-port 7842 "$@"
