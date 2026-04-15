# Slack Proxy

Omegon daemon + vox Slack proxy connector. Monitors the operator's own Slack channels and surfaces messages to the agent as context.

This is not a bot — it's your personal Slack abstraction layer. The agent reads your channels, summarizes conversations, flags urgent messages, and (with elevated posture) can act on your behalf.

## Prerequisites

1. **Slack app with user token scopes** -- create at [api.slack.com/apps](https://api.slack.com/apps):

   **User Token Scopes** (OAuth & Permissions > User Token Scopes):
   - `channels:history` -- read public channel messages
   - `channels:read` -- list channels (for auto-discovery)
   - `groups:history` -- read private channel messages
   - `groups:read` -- list private channels
   - `im:history` -- read DMs
   - `im:read` -- list DM channels
   - `mpim:history` -- read group DMs
   - `mpim:read` -- list group DMs

   For **participate** posture (sending messages as you):
   - `chat:write` -- send messages

   Install the app to your workspace, then copy the **User OAuth Token** (`xoxp-...`) from the OAuth & Permissions page.

2. **LLM API key** -- Anthropic (`ANTHROPIC_API_KEY`) or OpenAI (`OPENAI_API_KEY`).

## Quick Start

### Container

Stage the binaries:

```sh
# From vox repo root
cargo build --release -p vox --features slack
mkdir -p examples/slack-proxy/bin
cp target/release/vox examples/slack-proxy/bin/vox

# From omegon repo
cargo build --release -p omegon
cp target/release/omegon examples/slack-proxy/bin/omegon
```

Build and run:

```sh
podman build -t slack-proxy examples/slack-proxy

podman run --rm -it \
  -e ANTHROPIC_API_KEY=sk-ant-... \
  -e VOX_SLACK_USER_TOKEN=xoxp-... \
  slack-proxy
```

### Local (no container)

```sh
# Terminal 1: start daemon with vox extension
omegon serve --control-port 7842

# Terminal 2: start vox in bridge mode
VOX_SLACK_USER_TOKEN=xoxp-... \
  vox --bridge --daemon-url http://localhost:7842
```

## Configuration

| Env Var | Description | Default |
|---------|-------------|---------|
| `VOX_SLACK_USER_TOKEN` | User OAuth Token (required) | -- |
| `VOX_SLACK_WORKSPACE` | Workspace name for display | `"default"` |
| `VOX_SLACK_POSTURE` | `observe` (read-only) or `participate` (read/write) | `observe` |
| `VOX_SLACK_WATCH_CHANNELS` | Comma-separated channel IDs to monitor (empty = all) | all joined channels |
| `VOX_SLACK_PROXY_POLL_SECS` | Poll interval in seconds | `30` |

## Posture

- **observe** (default) -- read-only. The agent sees all messages in your channels but cannot send. Safe to leave running.
- **participate** -- read/write. The agent can send messages as you. Requires `chat:write` scope on the user token. Messages appear as coming from your account.

## How It Works

1. On startup, vox resolves your identity via `auth.test` and discovers all channels you're in (or uses `watch_channels` if set).
2. Every `proxy_poll_secs` seconds, vox polls `conversations.history` for each watched channel.
3. Your own messages are filtered out. Messages from other people are forwarded to the agent.
4. All messages arrive as `TrustLevel::User` -- the agent treats them as external data, not instructions.
5. If posture is `participate`, the agent can call `vox_reply` to send messages as you.

Channels are rediscovered every 5 minutes by default (configurable via `channel_refresh_secs`). If you join a new channel, the proxy picks it up automatically.

## Rate Limits

Slack's `conversations.history` endpoint is Tier 3 (~50 requests/minute). With a 30-second poll interval:
- 20 channels = 40 req/min (safe)
- 50 channels = 100 req/min (exceeds limit)

For workspaces with many channels, either increase `proxy_poll_secs` or use `watch_channels` to limit scope. Vox logs a warning at startup if the projected rate exceeds safe thresholds.
