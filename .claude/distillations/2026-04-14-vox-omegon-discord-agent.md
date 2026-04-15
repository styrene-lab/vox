# Session Distillation: Vox + Omegon Discord Agent Integration

Generated: 2026-04-14
Working Directory: /Users/cwilson/workspace/black-meridian/styrene-lab/vox
Repositories: styrene-lab/vox (comms extension), omegon (daemon agent)

## Session Overview

Built the end-to-end integration for driving an omegon agent via Discord through the vox communication extension. Vox provides the Discord Gateway connector and message routing; omegon provides the LLM agent brain. The daemon-side vox event bridge polls the extension for inbound messages and injects them as agent prompts with reply context. An initial detour into a standalone `--agent` mode in vox (with its own OpenAI client) was identified as architecturally wrong and reverted — omegon owns the model, vox owns the wire.

## Technical State

### Repository Status — vox (styrene-lab/vox)
- Branch: `main` (1 prior commit: `5a12ae4 Initial scaffold`)
- Uncommitted changes: all new connectors, deploy artifacts, core abstractions, Containerfile

### Repository Status — omegon
- Branch: `main` (last release: `9b10b33b chore(release): 0.15.22`)
- Uncommitted changes: vox bridge module, polling handle, prompt injection, version bump to 0.15.23
- Also has unrelated staged cleave files from a prior session

### Key Changes — vox

| File | Change |
|------|--------|
| `vox/src/main.rs` | Added `execute_tool` RPC handler (omegon standard protocol), `dispatch_tool()` by name, `load_secrets()` shared fn, `spawn_shutdown_handler` with SIGTERM+SIGINT |
| `vox-core/src/lib.rs` | Core abstractions (Connector trait, ConnectorRegistry, SessionKey, ReplyAddress, config types). Warning fix on SelectAll::swap_remove |
| `vox-discord/src/lib.rs` | Complete Discord Gateway WebSocket connector — identify, resume, heartbeat, message parsing, mention filtering, guild filtering, allowlist, reactions, 2k truncation. 8 tests. Warning fixes. |
| `vox-slack/src/lib.rs` | Complete Slack Socket Mode connector. 9 tests. Warning fixes. |
| `vox-signal/`, `vox-email/`, `vox-lxmf/`, `vox-voice/` | Stub connectors — compile, register, return NotSupported. Warning fixes. |
| `manifest.toml` | Extension manifest with optional secrets (connector tokens) |
| `config.example.toml` | Documented config for all connectors |
| `Containerfile` | Multi-stage build, defaults to `--rpc` extension mode |
| `deploy/` | discord-agent.sh (bridge mode), vox-bridge.service (systemd), com.styrene.vox-bridge.plist (launchd), discord-agent.toml, remote.md, vox-bridge-remote.sh |

### Key Changes — omegon (0.15.22 → 0.15.23)

| File | Change |
|------|--------|
| `extensions/mod.rs` | `ExtensionPollingHandle` struct (Clone, shareable RPC handle). `polling_handle()` on `ExtensionFeature`. `vox_polling_handle: Option<ExtensionPollingHandle>` on `SpawnedExtension`. Extracted in both `spawn_native()` and `spawn_container()` when extension provides `vox_route` tool. |
| `extensions/vox_bridge.rs` | **New module.** `VoxBridgeConfig` (poll_interval_ms default 500). `start_vox_bridge()` spawns background task. `format_vox_event()` converts vox_route results into `DaemonEventEnvelope` with `<vox_reply_context>` embedded in prompt. 3 tests. |
| `setup.rs` | `discover_and_register_extensions()` returns `Vec<ExtensionPollingHandle>` as third tuple element. `SetupResult.vox_polling_handles` field. |
| `main.rs` | Daemon serve startup: clones `daemon_events` Arc, starts vox bridge for each polling handle after cancellation token creation. |
| `prompt.rs` | Conditional "Vox Extension" section in `build_base_prompt_with_breakdown()` — only included when `vox_reply` is in the tool list. Loads `data/vox-extension-context.md`. |
| `data/vox-extension-context.md` | **New.** System prompt instructions for handling `<vox_reply_context>` blocks and using `vox_reply` tool. |
| `core/Cargo.toml` | Version 0.15.22 → 0.15.23 |

### Versions
- omegon: 0.15.23 (uncommitted)
- omegon-extension SDK: 0.15.23 (path dep from vox)
- vox: 0.1.0
- vox-core: 0.1.0
- Rust edition: 2021 (vox), 2024 (omegon)

### Test Counts
- vox workspace: 17 tests (8 discord, 9 slack), 0 warnings
- omegon: 1662 tests (including 3 vox_bridge, 41 prompt), 0 failures

## Decisions Made

1. **Vox is a pure comms extension, not an agent.** An initial `--agent` mode with built-in OpenAI client was built, assessed, and reverted. Omegon owns the LLM provider, model selection, session management, and tool dispatch. Vox owns the wire protocols.

2. **Extension mode (option 1) is the correct architecture.** Omegon spawns vox as a subprocess via JSON-RPC. No separate bridge process needed. The daemon-side vox bridge polls `vox_route` directly via the extension's RPC handle.

3. **Polling handle via Arc<Mutex<ProcessHandles>> sharing.** The ExtensionPollingHandle clones the Arc'd handles from ExtensionFeature, allowing daemon background tasks to call RPC methods on the extension subprocess without going through the EventBus or agent turn.

4. **System prompt injection gated on tool presence.** The vox reply instructions only appear when `vox_reply` is in the tool list — meaning the extension is loaded and active. No config flag needed.

5. **Reply context embedded in prompt text.** The `<vox_reply_context>` XML block in the user prompt carries the `reply_address` and `session_key`. The agent extracts it and passes to `vox_reply`. This avoids modifying `WebCommand::UserPrompt(String)` or the core agent loop.

6. **Dual RPC dispatch in vox.** Vox handles both `execute_tool` (omegon standard protocol, dispatches by `name` param) and direct method names (`execute_vox_route`, etc.) for backward compatibility and direct polling.

## Pending Items

### Incomplete Work — Extension Installation Contract

The next session's primary objective. Identified gaps:

1. **No `omegon extension install` CLI** — operators must manually copy binary + manifest to `~/.omegon/extensions/vox/`. Need a canonical install command that handles binary path, manifest validation, and secret setup hints.

2. **No `omegon secret set` CLI** (or it exists outside analyzed scope) — the SecretsManager API supports keyring, env, file, vault, and shell recipes. But the operator-facing command to set `VOX_DISCORD_BOT_TOKEN` is unclear. The error message references `omegon secret set` but the subcommand wasn't found.

3. **Manifest binary path** — `target/release/vox` assumes dev build. Installed extensions need `bin/vox` or equivalent. The install command should handle this.

4. **Extension update/remove lifecycle** — plugin CLI has install/list/remove/update for TOML plugins. Extensions (native Rust binaries) need the same.

### Known Issues

- Single-session daemon: all Discord users' messages go to one conversation. Multi-session routing (keyed by SessionKey) is not yet implemented. There's a cleave task file for this in omegon: `.cleave-implement-daemon-session-router-in-omego/`.
- No typing indicator — Discord users see no activity while the LLM thinks (5-30s).
- No health endpoint for the daemon+vox deployment (the agent-mode health server was removed with the agent mode).
- `vox_polling_handle` extraction only checks for `vox_route` tool name — should this be a manifest declaration instead?

### Planned Next Steps

**Immediate: Extension contract & install CLI** — define the canonical interface for native extensions (manifest schema, install path, binary resolution, secret declaration, lifecycle commands). Scribe-rpc and vox are the two existing extensions to validate against.

**Then: `omegon secret set` verification** — confirm or implement the secret management CLI.

**Then: Commit and release** — both repos have uncommitted work. Vox needs its initial full commit. Omegon needs 0.15.23 tagged.

## Critical Context

- The operator (Chris) explicitly corrected the architecture mid-session: "vox is not what should be defining the model. Omegon is the deployable daemon agent, vox is just the rust built first-class extension." This led to removing ~400 lines of agent code from vox and building the daemon-side bridge in omegon instead.

- The assessment identified 5 critical issues (C1-C5) for long-running deployment. Most were addressed in the agent code that was later removed, but the underlying concerns (session memory, timeouts, concurrency, SIGTERM, health probes) still apply to the omegon daemon side — they're omegon's responsibility now.

- Scribe-rpc is the other existing native extension. Its manifest and install pattern should be the reference for the extension contract work.

- The `clean_command()` in omegon strips all env vars except SAFE_INHERIT_ENVS. VOX_CONFIG is NOT in the safe list, so vox falls back to `~/.config/vox/vox.toml` via HOME (which IS inherited). This is by design but needs documentation.

## File Reference

Key files for continuation:

**vox repo** (`~/workspace/black-meridian/styrene-lab/vox/`):
- `vox/src/main.rs`: Extension binary — RPC serve loop, tool dispatch, bridge mode
- `vox-core/src/lib.rs`: Core abstractions — Connector trait, SessionKey, ReplyAddress, config
- `vox-discord/src/lib.rs`: Discord Gateway connector (complete, tested)
- `manifest.toml`: Extension manifest (needs binary path fix for installed mode)

**omegon repo** (`~/workspace/black-meridian/omegon/`):
- `core/crates/omegon/src/extensions/mod.rs`: ExtensionFeature, ExtensionPollingHandle, spawn_from_manifest
- `core/crates/omegon/src/extensions/vox_bridge.rs`: Daemon-side polling bridge
- `core/crates/omegon/src/extensions/manifest.rs`: ExtensionManifest, RuntimeConfig, native_binary_path()
- `core/crates/omegon/src/setup.rs`: discover_and_register_extensions (returns polling handles)
- `core/crates/omegon/src/prompt.rs`: Conditional vox system prompt section
- `core/crates/omegon/src/main.rs`: Daemon serve loop, vox bridge startup
- `core/crates/omegon-secrets/src/lib.rs`: SecretsManager (resolve, set_recipe, set_keyring_secret)
- `core/crates/omegon/src/plugins/`: Plugin CLI (install/list/remove/update) — reference for extension CLI
- `data/vox-extension-context.md`: System prompt instructions for vox_reply
