# Slack Agent

Omegon daemon + vox Slack connector in a single container. Uses Socket Mode (WebSocket) for real-time message delivery.

## Prerequisites

1. **Slack app with Socket Mode** — create at [api.slack.com/apps](https://api.slack.com/apps):

   **Bot Token Scopes** (OAuth & Permissions):
   - `chat:write` — send messages
   - `reactions:write` — add reactions
   - `im:history` — read DMs
   - `channels:history` — read channel messages
   - `groups:history` — read private channel messages
   - `usergroups:read` — resolve operator groups (if using `operator_groups`)

   **Event Subscriptions** (subscribe to bot events):
   - `message.channels` — messages in public channels
   - `message.groups` — messages in private channels
   - `message.im` — direct messages

   **Socket Mode**: enable under Settings > Socket Mode, then generate an app-level token with `connections:write` scope.

2. **Two tokens**:
   - `VOX_SLACK_BOT_TOKEN` — Bot User OAuth Token (`xoxb-...`), from OAuth & Permissions page
   - `VOX_SLACK_APP_TOKEN` — App-Level Token (`xapp-...`), from Socket Mode settings

3. **LLM API key** — Anthropic (`ANTHROPIC_API_KEY`) or OpenAI (`OPENAI_API_KEY`).

4. **Your Slack user ID** — click your profile photo > Profile > **...** > Copy member ID.

## Quick Start

### Container

Stage the binaries:

```sh
# From vox repo root
cargo build --release -p vox --features slack
mkdir -p examples/slack/bin
cp target/release/vox examples/slack/bin/vox

# From omegon repo
cargo build --release -p omegon
cp target/release/omegon examples/slack/bin/omegon
```

Build and run:

```sh
podman build -t slack-agent examples/slack

podman run --rm -it \
  -e ANTHROPIC_API_KEY=sk-ant-... \
  -e VOX_SLACK_BOT_TOKEN=xoxb-... \
  -e VOX_SLACK_APP_TOKEN=xapp-... \
  -e VOX_SLACK_OPERATORS=YOUR_USER_ID \
  -e VOX_SLACK_WORKSPACE=my-workspace \
  slack-agent
```

### Local (no container)

```sh
# Terminal 1: start daemon with vox extension
omegon serve --control-port 7842

# Terminal 2: start vox in bridge mode
VOX_SLACK_BOT_TOKEN=xoxb-... \
VOX_SLACK_APP_TOKEN=xapp-... \
  vox --bridge --daemon-url http://localhost:7842
```

## Configuration

All config is overridable via environment variables at runtime. The baked-in `vox.toml` provides defaults.

| Env Var | Description | Default |
|---------|-------------|---------|
| `VOX_SLACK_BOT_TOKEN` | Bot User OAuth Token (required) | -- |
| `VOX_SLACK_APP_TOKEN` | App-Level Token for Socket Mode (required) | -- |
| `VOX_SLACK_WORKSPACE` | Workspace name for display | `"default"` |
| `VOX_SLACK_OPERATORS` | Comma-separated user IDs with operator trust | `[]` |
| `VOX_SLACK_OPERATOR_GROUPS` | Comma-separated User Group IDs granting operator trust | `[]` |
| `VOX_SLACK_ALLOWED_USERS` | Comma-separated user IDs allowed to interact (empty = all) | `[]` |
| `VOX_SLACK_REQUIRE_MENTION` | Require @mention in channels | `true` |

### Vault-backed secrets

```sh
podman run --rm -it \
  -e VAULT_ADDR=https://vault.example.com \
  -e VAULT_TOKEN=hvs.xxx \
  -e ANTHROPIC_API_KEY_VAULT=secret/data/omegon/api#anthropic \
  -e VOX_SLACK_BOT_TOKEN_VAULT=secret/data/vox/slack#bot_token \
  -e VOX_SLACK_APP_TOKEN_VAULT=secret/data/vox/slack#app_token \
  -e VOX_SLACK_OPERATORS=YOUR_USER_ID \
  slack-agent
```

## Trust Model

Messages are classified by trust level before reaching the agent:

- **Operator** -- user ID in `operators`, or member of a group in `operator_groups`. Messages are treated as direct instructions to the agent.
- **User** -- everyone else. Messages are wrapped in containment tags that tell the agent to treat the content as external data, not instructions.

Operator groups are resolved once at startup via the `usergroups.users.list` API. No per-message API calls are made for trust classification.

Without any operators configured, **all messages are User-trust** -- the agent responds but won't follow embedded instructions from anyone.

## Session Routing

Each Slack user gets an isolated conversation session. Thread context is preserved -- messages in the same thread route to the same session. Sessions are evicted after 5 minutes of inactivity.
