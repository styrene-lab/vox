# Discord Agent

Omegon daemon + vox Discord connector in a single container.

## Prerequisites

1. **Discord bot token** — create an application at [discord.com/developers](https://discord.com/developers/applications), add a bot, copy the token.

   Required bot intents (under Bot > Privileged Gateway Intents):
   - Message Content Intent
   - Server Members Intent (if using `operator_roles`)

   Bot permissions (use the URL Generator under OAuth2):
   - Send Messages
   - Read Message History
   - Use Slash Commands (optional)

2. **LLM API key** — Anthropic (`ANTHROPIC_API_KEY`) or OpenAI (`OPENAI_API_KEY`).

3. **Your Discord user ID** — enable Developer Mode in Discord settings, right-click your name, Copy User ID.

## Quick Start

### Container

Stage the binaries:

```sh
# From vox repo root
cargo build --release -p vox --features discord
mkdir -p examples/discord/bin
cp target/release/vox examples/discord/bin/vox

# From omegon repo
cargo build --release -p omegon
cp target/release/omegon examples/discord/bin/omegon
```

Build and run:

```sh
podman build -t discord-agent examples/discord

podman run --rm -it \
  -e ANTHROPIC_API_KEY=sk-ant-... \
  -e VOX_DISCORD_BOT_TOKEN=MTIz... \
  -e VOX_DISCORD_OPERATORS=YOUR_USER_ID \
  discord-agent
```

### Local (no container)

```sh
# Terminal 1: start daemon with vox extension
omegon serve --control-port 7842

# Terminal 2: start vox in bridge mode
VOX_DISCORD_BOT_TOKEN=MTIz... \
  vox --bridge --daemon-url http://localhost:7842
```

## Configuration

All config is overridable via environment variables at runtime. The baked-in `vox.toml` provides defaults.

| Env Var | Description | Default |
|---------|-------------|---------|
| `VOX_DISCORD_BOT_TOKEN` | Bot token (required) | — |
| `VOX_DISCORD_OPERATORS` | Comma-separated user IDs with operator trust | `[]` |
| `VOX_DISCORD_OPERATOR_ROLES` | Comma-separated role IDs granting operator trust | `[]` |
| `VOX_DISCORD_ALLOWED_USERS` | Comma-separated user IDs allowed to interact (empty = all) | `[]` |
| `VOX_DISCORD_GUILD_ID` | Restrict to a single server | all servers |
| `VOX_DISCORD_REQUIRE_MENTION` | Require @mention in channels | `true` |

### Vault-backed secrets

For production deployments, use Vault instead of raw env vars:

```sh
podman run --rm -it \
  -e VAULT_ADDR=https://vault.example.com \
  -e VAULT_TOKEN=hvs.xxx \
  -e ANTHROPIC_API_KEY_VAULT=secret/data/omegon/api#anthropic \
  -e VOX_DISCORD_BOT_TOKEN_VAULT=secret/data/vox/discord#bot_token \
  -e VOX_DISCORD_OPERATORS=YOUR_USER_ID \
  discord-agent
```

## Trust Model

Messages are classified by trust level before reaching the agent:

- **Operator** — user ID in `operators` or has a role in `operator_roles`. Messages are treated as direct instructions to the agent.
- **User** — everyone else. Messages are wrapped in containment tags that tell the agent to treat the content as external data, not instructions.

Without any operators configured, **all messages are User-trust** — the agent responds but won't follow embedded instructions from anyone.

## Session Routing

Each Discord user gets an isolated conversation session. Messages from different users don't cross-contaminate. Sessions are evicted after 5 minutes of inactivity.
