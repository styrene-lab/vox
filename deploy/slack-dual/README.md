# Slack Dual Deploy

Two omegon agents from one image, one compose file:

| Container | Mode | Token | Purpose |
|-----------|------|-------|---------|
| `slack-bot` | bot | `xoxb-*` + `xapp-*` | Dedicated bot identity. Responds to users in channels via Socket Mode. |
| `slack-proxy` | proxy | `xoxp-*` | Monitors the operator's channels. Surfaces messages to the agent as context. Read-only by default. |

## Setup

### 1. Create the Slack app

At [api.slack.com/apps](https://api.slack.com/apps), create one app (or two separate apps — your call).

**Bot Token Scopes** (for `slack-bot`):
- `chat:write`, `reactions:write`
- `im:history`, `channels:history`, `groups:history`
- `usergroups:read` (if using `operator_groups`)

**User Token Scopes** (for `slack-proxy`):
- `channels:history`, `channels:read`
- `groups:history`, `groups:read`
- `im:history`, `im:read`
- `mpim:history`, `mpim:read`
- `chat:write` (only if using `participate` posture)

**Socket Mode** (for `slack-bot`): enable, generate app-level token with `connections:write`.

**Event Subscriptions** (for `slack-bot`): subscribe to `message.channels`, `message.groups`, `message.im`.

### 2. Stage binaries

```sh
# From vox repo root
cargo build --release -p vox --features slack
mkdir -p deploy/slack-dual/bin
cp target/release/vox deploy/slack-dual/bin/vox

# From omegon repo
cargo build --release -p omegon
cp target/release/omegon deploy/slack-dual/bin/omegon
```

### 3. Build the image

```sh
podman build -t slack-agent deploy/slack-dual
```

### 4. Configure

```sh
cp deploy/slack-dual/.env.example deploy/slack-dual/.env
# Edit .env with your tokens
```

### 5. Run

```sh
podman compose -f deploy/slack-dual/compose.yml up
```

Bot control plane at `http://localhost:7842`, proxy at `http://localhost:7843`.

## Architecture

```
                    ┌──────────────────┐
Slack users ──────> │  slack-bot        │ ── omegon daemon (full agent)
  @mention bot      │  xoxb + xapp     │    port 7842
                    │  Socket Mode WS  │
                    └──────────────────┘

                    ┌──────────────────┐
Slack channels ──> │  slack-proxy      │ ── omegon daemon (monitoring agent)
  all messages      │  xoxp             │    port 7843
                    │  polls history    │
                    └──────────────────┘
```

Both containers run independent omegon daemons with their own session routing, conversation state, and tool access. They share only the LLM API key.

## Configuration

Edit the `.toml` files for baked-in config, or override via env vars in `.env`.

**Bot config** (`bot.toml`): operators, operator_groups, require_mention, allowed_users.

**Proxy config** (`proxy.toml`): posture, watch_channels, proxy_poll_secs.

See the [Discord example](../../examples/discord/README.md) and [Slack proxy example](../../examples/slack-proxy/README.md) for full config reference.
