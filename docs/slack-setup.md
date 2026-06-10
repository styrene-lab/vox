# Vox Slack Connector Setup

## Prerequisites

- A Slack workspace you have admin access to
- omegon with secrets subsystem configured

## 1. Create the Slack App

1. Go to https://api.slack.com/apps
2. **Create New App** → **From scratch**
3. Name: `omegon-vox` (or whatever you prefer)
4. Select your workspace

## 2. Enable Socket Mode

Socket Mode lets vox receive events over WebSocket without a public URL.
This is ideal for lab/airgapped deployments.

1. **Settings → Socket Mode** → Enable
2. Generate an **App-Level Token** with scope `connections:write`
3. Save the `xapp-...` token

## 3. Configure Bot Token Scopes

**Features → OAuth & Permissions → Bot Token Scopes:**

| Scope | Purpose |
|-------|---------|
| `app_mentions:read` | Receive @-mentions in channels |
| `chat:write` | Send messages |
| `channels:history` | Read channel messages |
| `groups:history` | Read private channel messages |
| `im:history` | Read DM messages |
| `im:read` | Access DM channel list |
| `im:write` | Open DM conversations |
| `reactions:write` | Add emoji reactions |
| `users:read` | Resolve user display names |

## 4. Subscribe to Events

**Features → Event Subscriptions → Enable Events → Subscribe to bot events:**

- `app_mention` — @-mentions in channels
- `message.im` — direct messages to the bot

## 5. Install to Workspace

**Settings → Install App → Install to Workspace**

Copy the **Bot User OAuth Token** (`xoxb-...`).

## 6. Store Secrets

Preferred production deployments mount token files (Vault/VSO, Kubernetes
Secrets, SOPS, or another external secret manager):

```toml
[slack]
oauth_token_file = "/run/omegon/secrets/slack_oauth_token"   # xoxb-...
socket_token_file = "/run/omegon/secrets/slack_socket_token" # xapp-...
```

Local development can still use omegon's keyring:

```bash
omegon secret set VOX_SLACK_BOT_TOKEN    # xoxb-...
omegon secret set VOX_SLACK_APP_TOKEN    # xapp-...
```

## 7. Configure vox.toml

```toml
[slack]
workspace = "your-workspace-name"
require_mention = true          # only respond to @mentions in channels
allowed_users = []              # empty = allow all, or ["U12345", "U67890"]
oauth_token_file = "/run/omegon/secrets/slack_oauth_token"
socket_token_file = "/run/omegon/secrets/slack_socket_token"
# default_channel = "C12345"   # optional fallback channel
```

## 8. Run

```bash
# Build with slack feature
cd /path/to/vox
cargo build --release -p vox --features slack

# Install extension
mkdir -p ~/.omegon/extensions/vox
cp target/release/vox ~/.omegon/extensions/vox/
cp manifest.toml ~/.omegon/extensions/vox/

# Start omegon in daemon mode
omegon serve
```

## Security Notes

- Secrets are delivered to vox via JSON-RPC `bootstrap_secrets` over stdio pipe
- Vox subprocess is spawned with `env_clear()` — no env var leakage
- Secrets stored in memory with `secrecy::SecretString` (zeroized on drop)
- Socket Mode WebSocket uses TLS — no plaintext credentials on the wire
- `require_mention = true` prevents the bot from processing every message
- `allowed_users` restricts who can interact with the bot
