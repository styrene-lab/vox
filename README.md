# Vox

Unified communication connector extension for [Omegon](https://github.com/styrene-lab/omegon) — aggregates email, Signal, Slack, Discord, and more into a single agent-accessible interface.

## Install

```sh
omegon extension install https://github.com/styrene-lab/vox.git
```

Requires a Rust toolchain — the extension builds from source on install.

## What it does

Vox bridges external communication channels into Omegon's agent loop. Inbound messages arrive as prompts; the agent replies using vox tools, and Vox routes responses back to the originating channel.

```
Discord/Slack/Signal/Email
        ↓ inbound
      [ vox ]
        ↓ vox_route
      [ omegon agent ]
        ↓ vox_reply
      [ vox ]
        ↓ outbound
Discord/Slack/Signal/Email
```

### Tools

| Tool | Description |
|------|-------------|
| `vox_route` | Polled by Omegon for inbound messages |
| `vox_reply` | Send a response back to a channel/thread |
| `vox_send` | Send a new outbound message |
| `vox_channels` | List available channels and their status |

### Connectors

| Connector | Status |
|-----------|--------|
| Discord | Stable |
| Slack | Stable |
| Signal | Stable |
| Email (IMAP/SMTP) | Stable |
| LXMF (Reticulum) | Experimental |

## Configuration

Copy `config.example.toml` to `~/.config/vox/vox.toml` and configure the connectors you need. Each connector requires its own credentials — see the example config for details.

Secrets are resolved through Omegon's `SecretsManager` (keyring, Vault, env, shell commands) and delivered securely via the extension RPC bootstrap. They are never written to disk or passed as environment variables.

### Required secrets (per connector)

| Secret | Connector |
|--------|-----------|
| `VOX_DISCORD_BOT_TOKEN` | Discord |
| `VOX_SLACK_BOT_TOKEN` | Slack |
| `VOX_SLACK_APP_TOKEN` | Slack (Socket Mode) |
| `VOX_SIGNAL_PASSWORD` | Signal |
| `VOX_EMAIL_PASSWORD` | Email |

All secrets are optional — connectors whose secrets are missing start in a degraded state rather than failing the extension.

## Development

```sh
cargo build --release
omegon extension install .
```

## License

MIT OR Apache-2.0
