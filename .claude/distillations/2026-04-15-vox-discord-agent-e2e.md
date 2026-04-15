# Session Distillation: Vox Discord Agent — End-to-End Container Deployment

Generated: 2026-04-15
Working Directory: /Users/cwilson/workspace/black-meridian/styrene-lab/vox
Repositories: styrene-lab/vox (comms extension), omegon (daemon agent)

## Session Overview

Picked up from the previous session's distillation and drove the omegon+vox discord agent from "uncommitted code" to a working containerized deployment responding to Discord messages via OpenAI Codex (GPT-5.4). Along the way: built the extension install CLI, secret management CLI, fixed multiple integration bugs (path traversal, multi-instance isolation, RPC handshake, tool schema mismatch, event drain, model routing), published `omegon-extension` to crates.io, created Nix flake infrastructure for both repos, and established the container build pipeline.

## Technical State

### Repository Status — vox (styrene-lab/vox)
- Branch: `main`
- 11 commits total (9 new this session)
- Uncommitted: Cargo.lock, deploy/Containerfile.discord-agent, deploy/discord-agent.toml, deploy/entrypoint.sh (iterative container fixes)

### Repository Status — omegon
- Branch: `fix/delegate-provider-inherit`
- 8 new commits this session (fa328856..2870925a)
- Uncommitted: only cleave files from a prior session

### Key Changes — vox

| File | Change |
|------|--------|
| `vox/Cargo.toml` | Switched omegon-extension from path dep to crates.io 0.15.24. TLS: native-tls → rustls. Disabled lxmf feature (styrene-rs not published). |
| `vox/src/main.rs` | Fixed tool definitions: added `label` field, renamed `input_schema` → `parameters` to match omegon's `ToolDefinition` struct. Added bridge mode exponential backoff. |
| `vox-discord/Cargo.toml` | tokio-tungstenite + reqwest switched to rustls for cross-compilation |
| `vox-slack/Cargo.toml` | Same rustls switch |
| `Cargo.toml` | Excluded vox-lxmf from workspace (styrene-rs path deps block nix builds) |
| `flake.nix` | Nix flake: crane Rust build, nix2container OCI images (oci, oci-install), dev shell. nixos-unstable for Rust 1.85+ |
| `nix/oci.nix` | Two OCI images: standalone vox, init-container installer for sidecar pattern |
| `deploy/Containerfile.discord-agent` | Runtime container: pre-built omegon + vox binaries, entrypoint with secret bootstrap |
| `deploy/entrypoint.sh` | Bootstraps secrets from env vars + mounted auth.json, validates LLM credentials |
| `deploy/manifest.container.toml` | Extension manifest with `binary = "bin/vox"` for container layout |
| `deploy/discord-agent.toml` | Discord connector config with security controls documented |
| `.containerignore` | Excludes everything except deploy/ for minimal build context |

### Key Changes — omegon (0.15.22 → 0.15.24)

| File | Change |
|------|--------|
| `extensions/mod.rs` | ExtensionPollingHandle, vox_route detection, spawn_from_manifest |
| `extensions/vox_bridge.rs` | New: daemon-side polling bridge (500ms), format_vox_event, DaemonEventEnvelope injection |
| `extension_cli.rs` | New: `omegon extension install/list/remove/update/enable/disable`. Path traversal protection. |
| `secret_cli.rs` | New: `omegon secret set/list/delete` with --stdin for secure input |
| `paths.rs` | Added `omegon_home()`: OMEGON_HOME env var for multi-instance isolation |
| `setup.rs` | Wired SecretToolsProvider into EventBus. Updated all path consumers to use omegon_home(). Extension secret resolution. |
| `tool_registry.rs` | Added secrets module (3 tools), TOOL_COUNT 57→60 |
| `main.rs` | Extension/Secret CLI dispatch. Event drain arm in serve dispatch loop. Model passthrough to run_embedded_command. |
| `prompt.rs` | Conditional vox system prompt section gated on vox_reply tool |
| `omegon-extension/rpc.rs` | Fixed: RPC id changed from Option<String> to Option<Value> (JSON-RPC 2.0 spec compliance) |
| `omegon-extension/Cargo.toml` | Dual-licensed MIT/Apache-2.0 for crates.io publication |
| `flake.nix` | Nix flake: crane build, composable toolset profiles, 6 pre-composed OCI images |
| `nix/profiles.nix` | 7 profiles: base, dev, python, node, rust, ops, network |
| `nix/oci.nix` | mkOmegonImage builder with layered profile composition |

### Versions
- omegon: 0.15.24 (on branch fix/delegate-provider-inherit)
- omegon-extension: 0.15.24 (published to crates.io, MIT/Apache-2.0)
- vox: 0.1.0

### Container Image
- `localhost/discord-agent:latest` — ~192MB (debian-slim + omegon 30MB + vox 6MB + curl + ca-certs)
- Architecture: linux/arm64 (built via cargo zigbuild on macOS)
- Working: Discord gateway connects, messages flow through vox bridge, Codex processes and replies

### Test Counts
- vox workspace: 17 tests (8 discord, 9 slack), 0 failures
- omegon: 1676 tests, 0 failures

## Decisions Made

1. **Container runtime is canonical** — even "bare metal" operators use podman quadlet under systemd. No bare-process deployment path.

2. **Extensions are sidecar images** — distributed as OCI images, composed into pods via init-container pattern. Omegon image stays lean.

3. **Secrets resolve locally or phone home to Vault** — no custom secrets-over-the-wire until Styrene RPC is proven end-to-end.

4. **OMEGON_HOME env var for multi-instance isolation** — each container/pod gets its own state directory. Extensions, secrets, plugins all scoped per-instance.

5. **omegon-extension dual-licensed MIT/Apache-2.0** — SDK is permissively licensed to enable third-party extensions while core omegon stays BUSL-1.1.

6. **Nix composable toolset profiles** — container binary surface = agent sandbox. Profiles (base, dev, python, node, rust, ops, network) define what tools the agent can use.

7. **rustls over native-tls** — enables cross-compilation without OpenSSL sysroot. All TLS in vox uses rustls.

8. **vox-lxmf excluded from workspace** — depends on unpublished styrene-rs crates. Will be re-added when those are on crates.io.

## Bugs Found and Fixed

1. **C1: Path traversal in extension_cli** — `omegon extension remove ../../.ssh` could delete arbitrary directories. Fixed with validate_name().

2. **RPC id type mismatch** — omegon sends numeric ids (`"id": 1`), omegon-extension expected `Option<String>`. Silent deserialization failure killed the handshake. Fixed: `Option<Value>`. Published as 0.15.24.

3. **ToolDefinition schema mismatch** — vox returned `input_schema`, omegon expected `parameters`. Missing `label` field. Silent fallback to empty tools → no vox_route detected → no bridge started.

4. **Daemon event drain missing** — vox bridge pushed events to `daemon_events` Vec but the serve dispatch loop never read them. Messages accumulated forever. Fixed: added 250ms poll arm.

5. **Model flag ignored in serve** — `run_embedded_command` hardcoded `anthropic:claude-sonnet-4-6`, ignoring `--model` CLI arg. All daemon prompts went to Anthropic regardless of what was specified.

6. **VOX_CONFIG stripped by clean_command** — omegon strips non-safe env vars from extension subprocesses. VOX_CONFIG never reached vox. Fixed: place config at default fallback path `~/.config/vox/vox.toml`.

7. **install_local silent overwrite** — replaced existing extensions without warning. Now requires explicit `remove` first.

## Pending Items

### Immediate: Security Hardening
- **allowed_users** in discord-agent.toml needs operator's Discord user ID(s)
- **guild_id** should be set to restrict to specific server
- **Rate limiting** — no per-user or global rate limits on message processing
- **Prompt injection defense** — Discord messages go directly to LLM as user prompts with minimal sanitization
- **Tool restrictions** — the agent has bash access inside the container; the container sandbox IS the defense

### Multi-Session Routing
- All Discord users share one conversation. SessionKey routing exists in vox-core but omegon's daemon dispatch loop doesn't use it yet.
- Cleave task file exists: `.cleave-implement-daemon-session-router-in-omego/`

### Container Improvements
- Move from debian-slim to Nix-based minimal images (profiles.nix ready, OCI build needs Linux CI)
- Publish vox and omegon container images to ghcr.io
- Create podman quadlet files for systemd deployment

### Crate Publishing
- styrene-rs crates need publishing to unblock vox-lxmf
- omegon-extension 0.15.24 is published

### Auspex Integration
- First-run wizard design complete (not implemented)
- Worker profiles with extension + auth config designed
- Pod composition model (init-container sidecar) established

## Critical Context

- The omegon binary in the container must be rebuilt whenever omegon source changes. The `cargo zigbuild` + `cp deploy/bin/omegon` + `podman build --no-cache` cycle takes ~5 min (LTO link dominates).

- The Discord bot token was exposed in conversation logs. It should be rotated.

- Codex OAuth tokens auto-refresh from `auth.json`. The container mounts `~/.config/omegon:/config/omegon:ro` and the entrypoint symlinks auth.json to the canonical path.

- The `clean_command()` in omegon strips ALL env vars except SAFE_INHERIT_ENVS. Extension config must go at default fallback paths reachable via HOME (which IS inherited).

- omegon's `run_embedded_command` was hardcoded to Anthropic — this was only fixed in the last commit of this session. The fix passes `cli.model` through.

## File Reference

**vox repo** (`~/workspace/black-meridian/styrene-lab/vox/`):
- `vox/src/main.rs`: Extension binary — RPC serve loop, tool dispatch, bridge mode with backoff
- `vox-core/src/lib.rs`: Core abstractions — Connector trait, SessionKey, ReplyAddress, SecretStore
- `vox-discord/src/lib.rs`: Discord Gateway connector (complete, tested, rustls)
- `flake.nix`: Nix build — crane + nix2container
- `deploy/Containerfile.discord-agent`: Runtime container for discord agent
- `deploy/entrypoint.sh`: Secret bootstrap + auth.json mount + omegon serve
- `deploy/discord-agent.toml`: Discord connector config with security controls

**omegon repo** (`~/workspace/black-meridian/omegon/`):
- `core/crates/omegon/src/main.rs`: Daemon serve loop with vox event drain + model passthrough
- `core/crates/omegon/src/extensions/vox_bridge.rs`: Polling bridge (500ms)
- `core/crates/omegon/src/extension_cli.rs`: Extension lifecycle CLI
- `core/crates/omegon/src/secret_cli.rs`: Secret management CLI
- `core/crates/omegon/src/paths.rs`: OMEGON_HOME resolution
- `core/crates/omegon-extension/src/rpc.rs`: RPC types (fixed id: Option<Value>)
- `flake.nix`: Nix build with composable container profiles
- `nix/profiles.nix`: 7 toolset profiles for agent sandboxing
- `nix/oci.nix`: mkOmegonImage builder
