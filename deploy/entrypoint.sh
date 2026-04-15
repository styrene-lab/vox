#!/bin/sh
set -e

# Bootstrap secrets from environment variables into omegon's SecretsManager.
# This runs once at container startup so `omegon serve` can resolve them
# for the vox extension via bootstrap_secrets RPC.
#
# Secrets passed as env vars are converted to "env:VAR" recipes in
# $OMEGON_HOME/secrets.json. omegon's SecretsManager then resolves them
# from the process environment at runtime and delivers them to extensions
# via the RPC pipe — they never appear in the extension subprocess env.

OMEGON_HOME="${OMEGON_HOME:-/data/omegon}"
SECRETS_JSON="${OMEGON_HOME}/secrets.json"

mkdir -p "${OMEGON_HOME}"

# Build secrets.json from env vars (only if they're set)
if [ ! -f "${SECRETS_JSON}" ]; then
    echo "{}" > "${SECRETS_JSON}"
    chmod 600 "${SECRETS_JSON}"
fi

add_recipe() {
    local name="$1"
    local env_var="${2:-$1}"
    if [ -n "$(eval echo \$$env_var)" ]; then
        # Use python-free JSON manipulation via temp file
        local tmp=$(mktemp)
        # Read existing, add recipe, write back
        if command -v python3 >/dev/null 2>&1; then
            python3 -c "
import json, sys
with open('${SECRETS_JSON}') as f:
    d = json.load(f)
d['${name}'] = 'env:${env_var}'
with open('${SECRETS_JSON}', 'w') as f:
    json.dump(d, f)
"
        else
            # Fallback: sed-based for minimal images
            if grep -q '"'${name}'"' "${SECRETS_JSON}" 2>/dev/null; then
                : # already present
            elif [ "$(cat ${SECRETS_JSON})" = "{}" ]; then
                echo "{\"${name}\": \"env:${env_var}\"}" > "${SECRETS_JSON}"
            else
                sed -i 's/}$/,"'"${name}"'": "env:'"${env_var}"'"}/' "${SECRETS_JSON}"
            fi
        fi
        echo "  ${name} -> env:${env_var}"
    fi
}

echo "Configuring secrets..."
add_recipe "ANTHROPIC_API_KEY"
add_recipe "VOX_DISCORD_BOT_TOKEN"
add_recipe "VOX_SLACK_BOT_TOKEN"
add_recipe "VOX_SLACK_APP_TOKEN"
add_recipe "VOX_SIGNAL_PASSWORD"
add_recipe "VOX_EMAIL_PASSWORD"
add_recipe "VOX_MATRIX_PASSWORD"
add_recipe "VOX_LXMF_IDENTITY"
chmod 600 "${SECRETS_JSON}"

# Validate minimum requirements
if [ -z "${ANTHROPIC_API_KEY}" ]; then
    echo "ERROR: ANTHROPIC_API_KEY is required"
    exit 1
fi

if [ -z "${VOX_DISCORD_BOT_TOKEN}" ]; then
    echo "WARNING: VOX_DISCORD_BOT_TOKEN not set — Discord connector will be degraded"
fi

echo "Starting omegon daemon (serve mode)..."
exec omegon serve --control-port 7842 "$@"
