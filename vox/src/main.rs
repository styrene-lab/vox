use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use omegon_extension::Extension;
use serde_json::{json, Value};
use vox_core::{
    ConnectorRegistry, Error as VoxError, OutboundMessage, ReplyAddress, SecretStore, SessionKey,
    VoxConfig,
};

struct Vox {
    registry: ConnectorRegistry,
    secrets: Arc<SecretStore>,
    config: VoxConfig,
    config_is_default: bool,
}

impl Vox {
    fn new() -> Self {
        Self {
            registry: ConnectorRegistry::new(),
            secrets: Arc::new(SecretStore::new()),
            config: VoxConfig::default(),
            config_is_default: true,
        }
    }

    fn legacy_config_path() -> PathBuf {
        std::env::var("VOX_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::config_dir()
                    .unwrap_or_else(|| PathBuf::from("/etc/vox"))
                    .join("vox")
                    .join("vox.toml")
            })
    }

    fn load_legacy_config(&mut self) {
        let config_path = Self::legacy_config_path();
        self.config = VoxConfig::load(&config_path).unwrap_or_else(|e| {
            tracing::warn!(path = %config_path.display(), error = %e, "failed to load config, using defaults");
            VoxConfig::default()
        });
        self.config_is_default = false;
        tracing::info!(config = %config_path.display(), "vox config loaded");
    }

    fn handle_bootstrap_config(&mut self, params: &Value) -> omegon_extension::Result<Value> {
        if let Some(config) = params.get("vox_config").or_else(|| params.get("config")) {
            self.config = serde_json::from_value(config.clone()).map_err(|error| {
                omegon_extension::Error::invalid_params(format!(
                    "invalid vox_config bootstrap payload: {error}"
                ))
            })?;
            self.config_is_default = false;
            tracing::info!("vox config loaded from bootstrap_config payload");
            return Ok(json!({ "status": "ok", "source": "bootstrap_config" }));
        }

        if let Some(path) = params
            .get("vox_config_path")
            .or_else(|| params.get("config_path"))
            .and_then(Value::as_str)
        {
            let config_path = PathBuf::from(path);
            self.config = VoxConfig::load(&config_path).map_err(|error| {
                omegon_extension::Error::invalid_params(format!(
                    "failed to load vox_config_path '{}': {error}",
                    config_path.display()
                ))
            })?;
            self.config_is_default = false;
            tracing::info!(config = %config_path.display(), "vox config loaded from bootstrap_config path");
            return Ok(json!({ "status": "ok", "source": "bootstrap_config_path" }));
        }

        self.load_legacy_config();
        Ok(json!({ "status": "ok", "source": "legacy" }))
    }

    /// Initialize connectors based on config + available secrets.
    /// Called after bootstrap_secrets delivers credentials.
    async fn init_connectors(&mut self) {
        #[cfg(feature = "signal")]
        if let Some(ref cfg) = self.config.signal {
            let connector = vox_signal::SignalConnector::new(cfg.clone(), &self.secrets);
            self.registry.register(Box::new(connector));
            tracing::info!("signal connector registered");
        }

        #[cfg(feature = "email")]
        if let Some(ref cfg) = self.config.email {
            let connector = vox_email::EmailConnector::new(cfg.clone(), &self.secrets);
            self.registry.register(Box::new(connector));
            tracing::info!("email connector registered");
        }

        // LXMF is intentionally not compiled yet: vox-lxmf depends on styrene-rs
        // crates that are not published to crates.io. Keep parsing lxmf config in
        // vox-core, but do not reference the connector crate until the Cargo
        // feature is restored.

        #[cfg(feature = "voice")]
        if let Some(ref cfg) = self.config.voice {
            let connector = vox_voice::VoiceConnector::new(cfg.clone());
            self.registry.register(Box::new(connector));
            tracing::info!("voice connector registered");
        }

        #[cfg(feature = "slack")]
        if let Some(ref cfg) = self.config.slack {
            let connector = vox_slack::SlackConnector::new(cfg.clone(), &self.secrets);
            if let Err(e) = connector.start().await {
                tracing::error!(error = %e, "slack connector failed to start socket mode");
            }
            self.registry.register(Box::new(connector));
            tracing::info!("slack connector registered");
        }

        #[cfg(feature = "discord")]
        if let Some(ref cfg) = self.config.discord {
            let connector = vox_discord::DiscordConnector::new(cfg.clone(), &self.secrets);
            if let Err(e) = connector.start().await {
                tracing::error!(error = %e, "discord connector failed to start gateway");
            }
            self.registry.register(Box::new(connector));
            tracing::info!("discord connector registered");
        }

        let channels = self.registry.channels();
        tracing::info!(
            count = channels.len(),
            channels = ?channels.iter().map(|c| &c.name).collect::<Vec<_>>(),
            "connector initialization complete"
        );
    }

    fn tool_definitions(&self) -> Value {
        json!([
            {
                "name": "vox_channels",
                "label": "Vox Channels",
                "description": "List all available communication channels and their status",
                "parameters": {
                    "type": "object",
                    "properties": {},
                }
            },
            {
                "name": "vox_status",
                "label": "Vox Status",
                "description": "Get the connection status of one or all channels",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "channel": {
                            "type": "string",
                            "description": "Channel name. Omit to get status of all channels."
                        }
                    },
                }
            },
            {
                "name": "vox_send",
                "label": "Vox Send",
                "description": "Send a message through a communication channel. Supports email (with subject, CC/BCC), Signal (with groups, disappearing messages), Slack (with channels, threads), LXMF (mesh/Reticulum), and voice (TTS playback).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "channel": {
                            "type": "string",
                            "description": "Target connector (e.g. signal, email, slack, lxmf, voice)"
                        },
                        "envelope": {
                            "type": "object",
                            "description": "Addressing. Set 'kind' to: 'direct' (to), 'email' (to/cc/bcc), 'group' (group_id), or 'channel' (workspace/channel_id)",
                            "properties": {
                                "kind": { "type": "string", "enum": ["direct", "email", "group", "channel"] },
                                "to": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "id": { "type": "string" },
                                            "display_name": { "type": "string" }
                                        },
                                        "required": ["id"]
                                    }
                                },
                                "cc": { "type": "array", "items": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] } },
                                "bcc": { "type": "array", "items": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] } },
                                "group_id": { "type": "string" },
                                "workspace": { "type": "string" },
                                "channel_id": { "type": "string" }
                            },
                            "required": ["kind"]
                        },
                        "body": {
                            "type": "array",
                            "description": "Message content parts. Each has 'type': 'text' (content), 'rich' (content, format), or 'attachment' (name, mime, url)",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "type": { "type": "string", "enum": ["text", "rich", "attachment"] },
                                    "content": { "type": "string" },
                                    "format": { "type": "string", "enum": ["html", "markdown", "block_kit"] },
                                    "name": { "type": "string" },
                                    "mime": { "type": "string" },
                                    "url": { "type": "string" }
                                },
                                "required": ["type"]
                            }
                        },
                        "thread_id": { "type": "string", "description": "Thread to reply in" },
                        "reply_to": { "type": "string", "description": "Message ID to reply to" },
                        "reaction": {
                            "type": "object",
                            "description": "React to an existing message instead of sending content",
                            "properties": {
                                "emoji": { "type": "string" },
                                "target": { "type": "string" }
                            },
                            "required": ["emoji", "target"]
                        },
                        "hints": {
                            "type": "object",
                            "description": "Protocol-specific parameters. Set 'protocol' to 'email' (subject, headers), 'signal' (expiry, quote), or 'slack' (username, icon_emoji, unfurl)",
                            "properties": {
                                "protocol": { "type": "string", "enum": ["none", "email", "signal", "slack"] },
                                "subject": { "type": "string" },
                                "headers": { "type": "object" },
                                "expiry": { "type": "integer" },
                                "quote": { "type": "string" },
                                "username": { "type": "string" },
                                "icon_emoji": { "type": "string" },
                                "unfurl": { "type": "boolean" }
                            }
                        }
                    },
                    "required": ["channel", "envelope", "body"]
                }
            },
            {
                "name": "vox_poll",
                "label": "Vox Poll",
                "description": "Poll for new inbound messages from one or all channels",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "channel": {
                            "type": "string",
                            "description": "Channel to poll. Omit to poll all channels."
                        }
                    },
                }
            },
            {
                "name": "vox_route",
                "label": "Vox Route",
                "description": "Poll all channels and return messages annotated with session routing keys and reply addresses. Each message includes a session_key (channel + sender + thread) for session routing and a reply_address for sending responses back to the originating conversation. This is the primary ingress tool for daemon mode.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                }
            },
            {
                "name": "vox_reply",
                "label": "Vox Reply",
                "description": "Reply to a routed message using its reply_address. The reply_address contains all routing information (channel, envelope, thread, hints) so the agent only needs to provide text content.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "reply_address": {
                            "type": "object",
                            "description": "The reply_address object from a vox_route result"
                        },
                        "text": {
                            "type": "string",
                            "description": "Text content to send as the reply"
                        }
                    },
                    "required": ["reply_address", "text"]
                }
            }
        ])
    }

    async fn execute_channels(&self) -> omegon_extension::Result<Value> {
        let channels = self.registry.channels();
        serde_json::to_value(&channels)
            .map_err(|e| omegon_extension::Error::internal_error(e.to_string()))
    }

    async fn execute_status(&self, params: &Value) -> omegon_extension::Result<Value> {
        if let Some(name) = params.get("channel").and_then(|v| v.as_str()) {
            let connector = self
                .registry
                .get(name)
                .ok_or_else(|| omegon_extension::Error::invalid_params("unknown channel"))?;
            Ok(json!({
                "channel": name,
                "status": connector.status(),
            }))
        } else {
            self.execute_channels().await
        }
    }

    async fn execute_send(&self, params: &Value) -> omegon_extension::Result<Value> {
        let msg: OutboundMessage = serde_json::from_value(params.clone())
            .map_err(|e| omegon_extension::Error::invalid_params(e.to_string()))?;

        let connector = self
            .registry
            .get(&msg.channel)
            .ok_or_else(|| omegon_extension::Error::invalid_params("unknown channel"))?;

        let id = connector.send(msg).await.map_err(|e| match e {
            VoxError::NotSupported(m) => {
                omegon_extension::Error::internal_error(format!("not supported: {m}"))
            }
            other => omegon_extension::Error::internal_error(other.to_string()),
        })?;

        Ok(json!({ "message_id": id }))
    }

    async fn execute_poll(&self, params: &Value) -> omegon_extension::Result<Value> {
        if let Some(name) = params.get("channel").and_then(|v| v.as_str()) {
            let connector = self
                .registry
                .get(name)
                .ok_or_else(|| omegon_extension::Error::invalid_params("unknown channel"))?;
            let messages = connector
                .poll()
                .await
                .map_err(|e| omegon_extension::Error::internal_error(e.to_string()))?;
            serde_json::to_value(&messages)
                .map_err(|e| omegon_extension::Error::internal_error(e.to_string()))
        } else {
            let messages = self.registry.poll_all().await;
            serde_json::to_value(&messages)
                .map_err(|e| omegon_extension::Error::internal_error(e.to_string()))
        }
    }

    async fn execute_route(&self) -> omegon_extension::Result<Value> {
        let messages = self.registry.poll_all().await;
        let routed: Vec<Value> = messages
            .iter()
            .map(|msg| {
                let session_key = SessionKey::from_inbound(msg);
                let reply_address = ReplyAddress::from_inbound(msg);
                json!({
                    "session_key": session_key,
                    "reply_address": reply_address,
                    "message": msg,
                })
            })
            .collect();
        Ok(json!({ "messages": routed, "count": routed.len() }))
    }

    async fn execute_reply(&self, params: &Value) -> omegon_extension::Result<Value> {
        let reply_addr: ReplyAddress = serde_json::from_value(
            params
                .get("reply_address")
                .cloned()
                .ok_or_else(|| omegon_extension::Error::invalid_params("missing reply_address"))?,
        )
        .map_err(|e| {
            omegon_extension::Error::invalid_params(format!("invalid reply_address: {e}"))
        })?;

        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| omegon_extension::Error::invalid_params("missing text"))?;

        let outbound = reply_addr.text_reply(text.to_string());
        let connector = self.registry.get(&outbound.channel).ok_or_else(|| {
            omegon_extension::Error::invalid_params(format!(
                "no connector for channel '{}'",
                outbound.channel
            ))
        })?;

        let id = connector.send(outbound).await.map_err(|e| match e {
            VoxError::NotSupported(m) => {
                omegon_extension::Error::internal_error(format!("not supported: {m}"))
            }
            other => omegon_extension::Error::internal_error(other.to_string()),
        })?;

        Ok(json!({ "message_id": id }))
    }

    /// Dispatch a tool call by name (omegon standard extension protocol).
    async fn dispatch_tool(&self, name: &str, args: &Value) -> omegon_extension::Result<Value> {
        match name {
            "vox_channels" => self.execute_channels().await,
            "vox_status" => self.execute_status(args).await,
            "vox_send" => self.execute_send(args).await,
            "vox_poll" => self.execute_poll(args).await,
            "vox_route" => self.execute_route().await,
            "vox_reply" => self.execute_reply(args).await,
            _ => Err(omegon_extension::Error::method_not_found(name)),
        }
    }

    async fn handle_bootstrap_secrets(
        &mut self,
        params: &Value,
    ) -> omegon_extension::Result<Value> {
        let pairs: HashMap<String, String> = if let Some(obj) = params.as_object() {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        } else {
            HashMap::new()
        };

        let count = pairs.len();
        let names: Vec<&str> = pairs.keys().map(|s| s.as_str()).collect();
        tracing::info!(count, names = ?names, "bootstrap_secrets received");

        self.secrets.bootstrap(pairs);
        self.init_connectors().await;

        Ok(json!({ "status": "ok", "secrets_received": count }))
    }
}

#[async_trait]
impl Extension for Vox {
    fn name(&self) -> &str {
        "vox"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    async fn handle_rpc(&self, method: &str, params: Value) -> omegon_extension::Result<Value> {
        match method {
            "get_tools" => Ok(self.tool_definitions()),
            // Standard omegon extension protocol: execute_tool dispatches by name param
            "execute_tool" => {
                let tool_name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| omegon_extension::Error::invalid_params("missing tool name"))?;
                let args = params.get("args").cloned().unwrap_or(json!({}));
                self.dispatch_tool(tool_name, &args).await
            }
            // Direct method names (used by extension polling and legacy callers)
            "execute_vox_channels" => self.execute_channels().await,
            "execute_vox_status" => self.execute_status(&params).await,
            "execute_vox_send" => self.execute_send(&params).await,
            "execute_vox_poll" => self.execute_poll(&params).await,
            "execute_vox_route" => self.execute_route().await,
            "execute_vox_reply" => self.execute_reply(&params).await,
            "shutdown" => Ok(json!({ "status": "ok" })),
            _ => Err(omegon_extension::Error::method_not_found(method)),
        }
    }
}

/// Custom serve loop that handles bootstrap_secrets with &mut self.
async fn serve_vox(mut vox: Vox) -> omegon_extension::Result<()> {
    use omegon_extension::{RpcMessage, RpcResponse};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut reader = tokio::io::BufReader::new(stdin);
    let mut writer = tokio::io::BufWriter::new(stdout);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(());
        }

        let msg: RpcMessage = match serde_json::from_str(line.trim()) {
            Ok(msg) => msg,
            Err(e) => {
                let resp = RpcResponse::error(
                    None,
                    omegon_extension::ErrorCode::ParseError,
                    e.to_string(),
                );
                let json = serde_json::to_string(&resp)?;
                writer.write_all(json.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await?;
                continue;
            }
        };

        match msg {
            RpcMessage::Request(req) => {
                let response = if req.method == "bootstrap_config" {
                    match vox.handle_bootstrap_config(&req.params) {
                        Ok(value) => RpcResponse::success(req.id.clone(), value),
                        Err(e) => RpcResponse::error(req.id.clone(), e.code(), e.message()),
                    }
                } else if req.method == "bootstrap_secrets" {
                    if vox.config_is_default {
                        vox.load_legacy_config();
                    }
                    match vox.handle_bootstrap_secrets(&req.params).await {
                        Ok(value) => RpcResponse::success(req.id.clone(), value),
                        Err(e) => RpcResponse::error(req.id.clone(), e.code(), e.message()),
                    }
                } else {
                    match vox.handle_rpc(&req.method, req.params.clone()).await {
                        Ok(value) => RpcResponse::success(req.id.clone(), value),
                        Err(e) => RpcResponse::error(req.id.clone(), e.code(), e.message()),
                    }
                };

                let json = serde_json::to_string(&response)?;
                writer.write_all(json.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await?;
            }
            RpcMessage::Notification(_) => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Shared utilities
// ---------------------------------------------------------------------------

/// Load secrets from a file or environment variables.
/// Shared between agent and bridge modes.
fn load_secrets(
    secrets_file: Option<&PathBuf>,
    env_names: &[&str],
    mode_name: &str,
) -> HashMap<String, String> {
    let mut secrets = HashMap::new();

    if let Some(path) = secrets_file {
        // Check file permissions — refuse world-readable secrets files
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if let Ok(meta) = std::fs::metadata(path) {
                let mode = meta.mode() & 0o777;
                if mode & 0o044 != 0 {
                    tracing::error!(
                        path = %path.display(),
                        mode = format!("{:04o}", mode),
                        "secrets file is readable by group/others — refusing to load. \
                         Fix with: chmod 600 {}",
                        path.display()
                    );
                    std::process::exit(1);
                }
            }
        }

        match std::fs::read_to_string(path) {
            Ok(content) => {
                for line in content.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    if let Some((key, value)) = line.split_once('=') {
                        let key = key.trim();
                        let value = value.trim();
                        if !value.is_empty() {
                            secrets.insert(key.to_string(), value.to_string());
                        }
                    }
                }
                tracing::info!(
                    path = %path.display(),
                    secrets = secrets.len(),
                    "{mode_name}: loaded secrets from file"
                );
            }
            Err(e) => {
                tracing::error!(path = %path.display(), error = %e, "failed to read secrets file");
                std::process::exit(1);
            }
        }
    } else {
        for name in env_names {
            if let Ok(val) = std::env::var(name) {
                if !val.is_empty() {
                    secrets.insert(name.to_string(), val);
                }
            }
        }
        if !secrets.is_empty() {
            tracing::warn!(
                secrets = secrets.len(),
                "{mode_name}: secrets loaded from environment variables \
                 (use --secrets-file for better security)"
            );
        }
    }

    secrets
}

/// Spawn a task that cancels the token on SIGINT or SIGTERM.
fn spawn_shutdown_handler(cancel: tokio_util::sync::CancellationToken) {
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm =
                signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = sigterm.recv() => {},
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        tracing::info!("received shutdown signal");
        cancel_clone.cancel();
    });
}

// ---------------------------------------------------------------------------
// Bridge mode
// ---------------------------------------------------------------------------

const BRIDGE_MAX_EVENTS_PER_CYCLE: usize = 10;
const BRIDGE_MAX_BUFFER: usize = 500;

async fn run_bridge(
    vox: Vox,
    daemon_url: String,
    auth_token: Option<String>,
    poll_interval_ms: u64,
    cancel: tokio_util::sync::CancellationToken,
) {
    let http = reqwest::Client::new();
    let events_url = format!("{}/api/events", daemon_url.trim_end_matches('/'));
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(poll_interval_ms));
    let mut pending: std::collections::VecDeque<vox_core::InboundMessage> =
        std::collections::VecDeque::new();

    tracing::info!(
        daemon = %daemon_url,
        poll_ms = poll_interval_ms,
        "vox bridge started — polling connectors and pushing to daemon"
    );

    let mut consecutive_failures: u32 = 0;
    const MAX_BACKOFF_SECS: u64 = 60;

    loop {
        // Exponential backoff on consecutive daemon push failures:
        // base poll interval + 2^failures seconds, capped at MAX_BACKOFF_SECS.
        let backoff = if consecutive_failures > 0 {
            let delay_secs = (1u64 << consecutive_failures.min(6)).min(MAX_BACKOFF_SECS);
            std::time::Duration::from_secs(delay_secs)
        } else {
            std::time::Duration::ZERO
        };

        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("vox bridge shutting down");
                return;
            }
            _ = interval.tick() => {}
        }

        if !backoff.is_zero() {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!("vox bridge shutting down");
                    return;
                }
                _ = tokio::time::sleep(backoff) => {}
            }
        }

        let new_messages = vox.registry.poll_all().await;
        for msg in new_messages {
            if pending.len() >= BRIDGE_MAX_BUFFER {
                tracing::warn!("bridge buffer full ({BRIDGE_MAX_BUFFER}), dropping oldest");
                pending.pop_front();
            }
            pending.push_back(msg);
        }

        if pending.is_empty() {
            continue;
        }

        let batch_size = pending.len().min(BRIDGE_MAX_EVENTS_PER_CYCLE);
        // Take the batch but requeue on failure — don't drain until delivery succeeds.
        let batch: Vec<_> = pending.drain(..batch_size).collect();
        let mut failed: Vec<vox_core::InboundMessage> = Vec::new();

        for msg in batch {
            let session_key = vox_core::SessionKey::from_inbound(&msg);
            let reply_address = vox_core::ReplyAddress::from_inbound(&msg);

            let text: String = msg
                .body
                .iter()
                .filter_map(|part| match part {
                    vox_core::BodyPart::Text { content } => Some(content.as_str()),
                    vox_core::BodyPart::Rich { content, .. } => Some(content.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");

            if text.is_empty() {
                continue;
            }

            let envelope = serde_json::json!({
                "event_id": uuid::Uuid::new_v4().to_string(),
                "source": format!("vox:{}", msg.channel),
                "trigger_kind": "prompt",
                "payload": {
                    "text": text,
                    "reply_address": reply_address,
                },
                "source_user": session_key.sender_id,
                "source_channel": session_key.channel,
                "source_thread": session_key.thread_id,
            });

            let mut req = http.post(&events_url).json(&envelope);
            if let Some(ref token) = auth_token {
                req = req.bearer_auth(token);
            }

            match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    tracing::info!(
                        session = %session_key,
                        sender = %msg.sender.id,
                        "pushed event to daemon"
                    );
                }
                Ok(resp) => {
                    tracing::warn!(
                        status = %resp.status(),
                        session = %session_key,
                        "daemon rejected event — requeuing"
                    );
                    failed.push(msg);
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to push event to daemon — requeuing");
                    failed.push(msg);
                }
            }
        }

        // Requeue failed messages at the front so they retry next cycle.
        let batch_had_failure = !failed.is_empty();
        for msg in failed.into_iter().rev() {
            pending.push_front(msg);
        }

        if batch_had_failure {
            consecutive_failures = consecutive_failures.saturating_add(1);
            if consecutive_failures == 1 {
                tracing::warn!("daemon push failures detected — enabling exponential backoff");
            }
        } else {
            if consecutive_failures > 0 {
                tracing::info!("daemon push succeeded — backoff reset");
            }
            consecutive_failures = 0;
        }
    }
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(clap::Parser)]
#[command(name = "vox", about = "Omegon communication connector")]
struct Cli {
    /// Run as omegon extension (JSON-RPC over stdio)
    #[arg(long)]
    rpc: bool,

    /// Run as daemon bridge: poll connectors, push events to omegon serve
    #[arg(long)]
    bridge: bool,

    /// Omegon daemon URL for bridge mode [default: http://127.0.0.1:7842]
    #[arg(long, default_value = "http://127.0.0.1:7842")]
    daemon_url: String,

    /// Auth token for daemon event API (optional, from omegon's web auth)
    #[arg(long, env = "VOX_DAEMON_TOKEN")]
    daemon_token: Option<String>,

    /// Poll interval in milliseconds for bridge mode [default: 500]
    #[arg(long, default_value = "500")]
    poll_ms: u64,

    /// Path to a secrets file (key=value, one per line).
    /// Preferred over environment variables. File permissions are checked.
    #[arg(long)]
    secrets_file: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// Entrypoint
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    use clap::Parser;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("vox=info".parse().unwrap()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    if cli.bridge {
        // Bridge mode: standalone process
        let mut vox = Vox::new();

        let secrets = load_secrets(
            cli.secrets_file.as_ref(),
            &[
                "VOX_SLACK_BOT_TOKEN",
                "VOX_SLACK_APP_TOKEN",
                "VOX_SLACK_USER_TOKEN",
                "VOX_DISCORD_BOT_TOKEN",
                "VOX_SIGNAL_PASSWORD",
                "VOX_EMAIL_PASSWORD",
                "VOX_MATRIX_PASSWORD",
                "VOX_LXMF_IDENTITY",
            ],
            "bridge mode",
        );

        vox.secrets.bootstrap(secrets);
        vox.init_connectors().await;

        let cancel = tokio_util::sync::CancellationToken::new();
        spawn_shutdown_handler(cancel.clone());

        run_bridge(vox, cli.daemon_url, cli.daemon_token, cli.poll_ms, cancel).await;
    } else {
        // Extension mode (default): JSON-RPC over stdio
        let vox = Vox::new();
        serve_vox(vox).await.expect("vox extension loop failed");
    }
}
