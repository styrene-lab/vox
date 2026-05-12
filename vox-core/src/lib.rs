use std::collections::HashMap;
use std::pin::Pin;
use std::sync::RwLock;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_core::Stream;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("connector not found: {0}")]
    ConnectorNotFound(String),

    #[error("send failed: {0}")]
    SendFailed(String),

    #[error("poll failed: {0}")]
    PollFailed(String),

    #[error("not supported: {0}")]
    NotSupported(String),

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

pub type Result<T> = std::result::Result<T, Error>;

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// Opaque identifier for a message within its channel.
pub type MessageId = String;

/// Opaque identifier for a conversation thread.
pub type ThreadId = String;

// ---------------------------------------------------------------------------
// Addressing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Address {
    /// Channel-native identifier (e.g. email address, phone number, user id).
    pub id: String,
    /// Optional human-readable display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// Protocol-shaped addressing. Each variant captures the addressing model
/// native to a particular channel type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Envelope {
    /// Point-to-point addressing by contact identity (Signal 1:1, generic).
    Direct {
        to: Vec<Address>,
    },
    /// Email-style with To/CC/BCC distinction.
    Email {
        to: Vec<Address>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        cc: Vec<Address>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        bcc: Vec<Address>,
    },
    /// Group conversation (Signal groups).
    Group {
        group_id: String,
    },
    /// Workspace + channel (Slack, Discord).
    Channel {
        workspace: String,
        channel_id: String,
    },
}

// ---------------------------------------------------------------------------
// Message body
// ---------------------------------------------------------------------------

/// A single part of a message body. Messages carry a `Vec<BodyPart>` to
/// support multipart content (e.g. email: text + html + attachments).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BodyPart {
    /// Plain text content.
    Text { content: String },
    /// Rich/formatted content.
    Rich { content: String, format: RichFormat },
    /// File attachment.
    Attachment {
        name: String,
        mime: String,
        /// URL or local path to the content.
        url: String,
        /// Optional thumbnail URL (e.g. Signal generates these).
        #[serde(skip_serializing_if = "Option::is_none")]
        thumbnail: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RichFormat {
    Html,
    Markdown,
    BlockKit,
}

// ---------------------------------------------------------------------------
// Reactions
// ---------------------------------------------------------------------------

/// A reaction gesture on an existing message. Separate from body content
/// because a reaction is not message content — it's a gesture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reaction {
    pub emoji: String,
    pub target: MessageId,
}

// ---------------------------------------------------------------------------
// Channel hints — typed protocol-specific parameters
// ---------------------------------------------------------------------------

/// Protocol-specific parameters that don't fit the universal model.
/// Connectors ignore hints they don't understand.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "snake_case")]
pub enum ChannelHints {
    None,
    Email {
        #[serde(skip_serializing_if = "Option::is_none")]
        subject: Option<String>,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        headers: HashMap<String, String>,
    },
    Signal {
        /// Disappearing message timer in seconds. 0 = off.
        #[serde(skip_serializing_if = "Option::is_none")]
        expiry: Option<u64>,
        /// Quote-reply: the message being quoted.
        #[serde(skip_serializing_if = "Option::is_none")]
        quote: Option<MessageId>,
    },
    Slack {
        /// Post as a specific bot/app username.
        #[serde(skip_serializing_if = "Option::is_none")]
        username: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        icon_emoji: Option<String>,
        /// If true, unfurl links in the message.
        #[serde(default)]
        unfurl: bool,
    },
}

impl Default for ChannelHints {
    fn default() -> Self {
        Self::None
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundMessage {
    pub id: MessageId,
    pub channel: String,
    pub sender: Address,
    pub timestamp: DateTime<Utc>,
    pub envelope: Envelope,
    #[serde(default)]
    pub body: Vec<BodyPart>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<MessageId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reaction: Option<Reaction>,
    #[serde(default)]
    pub hints: ChannelHints,
    /// Trust level for this message. Determines how omegon frames it
    /// in the agent conversation (instruction vs data plane).
    #[serde(default)]
    pub trust_level: TrustLevel,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundMessage {
    /// Which connector handles this (e.g. "email", "signal", "slack").
    pub channel: String,
    /// Protocol-shaped addressing.
    pub envelope: Envelope,
    /// Message content parts. May be empty for pure reactions.
    #[serde(default)]
    pub body: Vec<BodyPart>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<MessageId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reaction: Option<Reaction>,
    #[serde(default)]
    pub hints: ChannelHints,
    /// Escape hatch for unforeseen protocol-specific data.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

// ---------------------------------------------------------------------------
// Connector capabilities & status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectorCapabilities {
    pub send: bool,
    pub receive: bool,
    pub threads: bool,
    pub reactions: bool,
    pub attachments: bool,
    pub read_receipts: bool,
    pub rich_text: bool,
    pub typing_indicators: bool,
    pub disappearing_messages: bool,
    pub groups: bool,
    /// Connector supports push delivery via `stream()`.
    pub push: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorStatus {
    Initializing,
    Connected,
    Degraded,
    Disconnected,
}

// ---------------------------------------------------------------------------
// Trust levels — instruction plane vs data plane
// ---------------------------------------------------------------------------

/// Trust level for an inbound message. Determines how omegon frames the
/// message when injecting it into the agent conversation.
///
/// - `Operator`: full instruction authority — the agent treats the message
///   as a command from its operator. Only assigned to sender IDs listed in
///   the connector's `operators` config.
///
/// - `User`: the message is external input — the agent responds helpfully
///   but does NOT follow instructions embedded in it. This is the default
///   for all senders not in the `operators` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// Full instruction authority. Messages treated as operator commands.
    Operator,
    /// External input only. Agent responds but does not follow instructions.
    User,
}

impl Default for TrustLevel {
    fn default() -> Self {
        Self::User
    }
}

impl std::fmt::Display for TrustLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Operator => write!(f, "operator"),
            Self::User => write!(f, "user"),
        }
    }
}

// ---------------------------------------------------------------------------
// Connector trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait Connector: Send + Sync {
    /// Channel identifier (e.g. "signal", "email", "slack").
    fn name(&self) -> &str;

    /// Current connection status.
    fn status(&self) -> ConnectorStatus;

    /// Declared capabilities for this channel.
    fn capabilities(&self) -> ConnectorCapabilities;

    /// Send a message through this channel.
    async fn send(&self, message: OutboundMessage) -> Result<MessageId>;

    /// Pull: check for new inbound messages since last poll.
    async fn poll(&self) -> Result<Vec<InboundMessage>>;

    /// Push: return a stream of inbound messages for real-time connectors.
    /// Poll-only connectors return `None` (the default).
    fn stream(&self) -> Option<Pin<Box<dyn Stream<Item = InboundMessage> + Send + '_>>> {
        None
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Holds registered connectors, keyed by channel name.
pub struct ConnectorRegistry {
    connectors: HashMap<String, Box<dyn Connector>>,
}

impl ConnectorRegistry {
    pub fn new() -> Self {
        Self {
            connectors: HashMap::new(),
        }
    }

    pub fn register(&mut self, connector: Box<dyn Connector>) {
        let name = connector.name().to_owned();
        self.connectors.insert(name, connector);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Connector> {
        self.connectors.get(name).map(|c| c.as_ref())
    }

    pub fn channels(&self) -> Vec<ChannelInfo> {
        self.connectors
            .values()
            .map(|c| ChannelInfo {
                name: c.name().to_owned(),
                status: c.status(),
                capabilities: c.capabilities(),
            })
            .collect()
    }

    pub async fn poll_all(&self) -> Vec<InboundMessage> {
        let mut all = Vec::new();
        for connector in self.connectors.values() {
            if let Ok(msgs) = connector.poll().await {
                all.extend(msgs);
            }
        }
        all.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        all
    }

    /// Merge all push-capable connector streams into a single stream.
    /// Returns `None` if no connectors support push.
    pub fn merged_stream(&self) -> Option<Pin<Box<dyn Stream<Item = InboundMessage> + Send + '_>>> {
        let streams: Vec<_> = self
            .connectors
            .values()
            .filter_map(|c| c.stream())
            .collect();

        if streams.is_empty() {
            return None;
        }

        let merged = futures_core_select_all(streams);
        Some(Box::pin(merged))
    }
}

/// Merge multiple streams into one, polling all of them fairly.
fn futures_core_select_all<'a>(
    streams: Vec<Pin<Box<dyn Stream<Item = InboundMessage> + Send + 'a>>>,
) -> impl Stream<Item = InboundMessage> + Send + 'a {
    SelectAll { streams, next_start: 0 }
}

/// A fair select-all stream combinator that rotates the start index
/// each poll to prevent starvation of later-registered connectors.
struct SelectAll<'a> {
    streams: Vec<Pin<Box<dyn Stream<Item = InboundMessage> + Send + 'a>>>,
    next_start: usize,
}

impl<'a> Stream for SelectAll<'a> {
    type Item = InboundMessage;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;

        if self.streams.is_empty() {
            return Poll::Ready(None);
        }

        let len = self.streams.len();
        let start = self.next_start % len;
        let mut exhausted = Vec::new();

        // Poll in rotating order for fairness, track exhausted streams by index
        for offset in 0..len {
            let i = (start + offset) % len;
            match self.streams[i].as_mut().poll_next(cx) {
                Poll::Ready(Some(item)) => {
                    self.next_start = i + 1;
                    return Poll::Ready(Some(item));
                }
                Poll::Ready(None) => {
                    exhausted.push(i);
                }
                Poll::Pending => {}
            }
        }

        // Remove exhausted streams in reverse index order to preserve indices
        exhausted.sort_unstable_by(|a, b| b.cmp(a));
        for i in exhausted {
            let _ = self.streams.swap_remove(i);
        }

        if self.streams.is_empty() {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }
}

impl Default for ConnectorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInfo {
    pub name: String,
    pub status: ConnectorStatus,
    pub capabilities: ConnectorCapabilities,
}

// ---------------------------------------------------------------------------
// Session routing — universal key derivation from any connector's messages
// ---------------------------------------------------------------------------

/// Canonical session key derived from any vox InboundMessage. This is the
/// identity that omegon's SessionRouter uses to map messages to isolated
/// agent sessions. The key is connector-agnostic — Slack, Signal, email,
/// LXMF, and voice all produce the same key shape.
///
/// Keying rules:
///   - Threaded conversations: (channel, sender.id, thread_id)
///   - Unthreaded conversations: (channel, sender.id, None)
///   - Voice (always single-user): ("voice", "local", None)
///
/// Two messages with the same SessionKey land in the same agent session,
/// preserving conversational context across turns.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionKey {
    /// Connector name: "slack", "signal", "email", "lxmf", "voice"
    pub channel: String,
    /// Sender identity within that channel
    pub sender_id: String,
    /// Thread/conversation identifier (if the channel supports threads)
    pub thread_id: Option<String>,
}

impl SessionKey {
    /// Derive a session key from an inbound message.
    pub fn from_inbound(msg: &InboundMessage) -> Self {
        Self {
            channel: msg.channel.clone(),
            sender_id: msg.sender.id.clone(),
            thread_id: msg.thread_id.clone(),
        }
    }

    /// Singleton key for contexts with no identity (backward compat).
    pub fn anonymous() -> Self {
        Self {
            channel: "anonymous".to_string(),
            sender_id: "default".to_string(),
            thread_id: None,
        }
    }

    /// Stable string representation for logging and map keys.
    pub fn as_routing_key(&self) -> String {
        match &self.thread_id {
            Some(tid) => format!("{}:{}:{}", self.channel, self.sender_id, tid),
            None => format!("{}:{}", self.channel, self.sender_id),
        }
    }
}

impl std::fmt::Display for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_routing_key())
    }
}

/// Reply address extracted from an inbound message. Captures everything
/// needed for vox_send to route the agent's response back to the right
/// place, regardless of which connector originated the message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplyAddress {
    /// Which connector to send through
    pub channel: String,
    /// Full envelope for addressing (preserved from inbound)
    pub envelope: Envelope,
    /// Thread to reply in (if threaded)
    pub thread_id: Option<ThreadId>,
    /// Original message ID (for reply-to threading)
    pub reply_to: Option<MessageId>,
    /// Protocol-specific hints (preserved from inbound)
    pub hints: ChannelHints,
}

impl ReplyAddress {
    /// Extract reply address from an inbound message.
    pub fn from_inbound(msg: &InboundMessage) -> Self {
        Self {
            channel: msg.channel.clone(),
            envelope: msg.envelope.clone(),
            thread_id: msg.thread_id.clone(),
            reply_to: Some(msg.id.clone()),
            hints: msg.hints.clone(),
        }
    }

    /// Build an OutboundMessage from a text reply using this address.
    pub fn text_reply(&self, text: String) -> OutboundMessage {
        OutboundMessage {
            channel: self.channel.clone(),
            envelope: self.envelope.clone(),
            body: vec![BodyPart::Text { content: text }],
            thread_id: self.thread_id.clone(),
            reply_to: self.reply_to.clone(),
            reaction: None,
            hints: self.hints.clone(),
            metadata: serde_json::Value::Null,
        }
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Top-level vox configuration loaded from vox.toml.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct VoxConfig {
    #[serde(default)]
    pub signal: Option<SignalConfig>,
    #[serde(default)]
    pub email: Option<EmailConfig>,
    #[serde(default)]
    pub lxmf: Option<LxmfConfig>,
    #[serde(default)]
    pub voice: Option<VoiceConfig>,
    #[serde(default)]
    pub matrix: Option<MatrixConfig>,
    #[serde(default)]
    pub slack: Option<SlackConfig>,
    #[serde(default)]
    pub discord: Option<DiscordConfig>,
}

impl VoxConfig {
    /// Load from a TOML file, returning default if file doesn't exist.
    ///
    /// Only treats `NotFound` as "use defaults". Other I/O errors (permission
    /// denied, disk failures) are surfaced so bad deploys don't silently run
    /// with an empty config.
    pub fn load(path: &std::path::Path) -> std::result::Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(content) => toml::from_str(&content).map_err(|e| e.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SignalConfig {
    /// Path to signal-cli data directory or presage state.
    pub data_dir: String,
    /// Phone number registered with Signal (e.g. "+15551234567").
    pub phone_number: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EmailConfig {
    /// IMAP server for receiving.
    pub imap_host: String,
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    /// SMTP server for sending.
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    /// Email address used as sender identity.
    pub address: String,
    /// Username for authentication (defaults to address if omitted).
    #[serde(default)]
    pub username: Option<String>,
}

fn default_imap_port() -> u16 {
    993
}
fn default_smtp_port() -> u16 {
    587
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LxmfConfig {
    /// Path to Reticulum config directory.
    #[serde(default = "default_rns_config")]
    pub rns_config_dir: String,
    /// LXMF display name for this node.
    pub display_name: String,
    /// Optional styrened RPC socket for delegated operation.
    #[serde(default)]
    pub styrened_socket: Option<String>,
}

fn default_rns_config() -> String {
    "~/.reticulum".to_string()
}

/// Speech-to-text engine selection.
///
/// Tiered deployment:
///   - Pi Zero 2W: Whisper (tiny Q4) or Vosk (lowest latency)
///   - Pi 4B: Moonshine (causal streaming) or Whisper (distil-small Q4)
///   - x86 server: Whisper (distil-large Q8)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SttEngine {
    /// whisper.cpp via whisper-rs — GGUF models, chunked sliding window.
    /// Best quantization range (Q4-Q8), proven Rust bindings.
    #[default]
    Whisper,
    /// Vosk via vosk crate — Kaldi-based, true native streaming.
    /// Lowest latency, weakest accuracy. Best for command recognition.
    Vosk,
    /// Moonshine via ort — causal encoder, autoregressive decoder.
    /// Purpose-built for edge (27-60M params). True streaming.
    Moonshine,
}

impl std::fmt::Display for SttEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Whisper => "whisper",
            Self::Vosk => "vosk",
            Self::Moonshine => "moonshine",
        })
    }
}

/// Text-to-speech engine selection.
///
/// Tiered deployment:
///   - Pi Zero 2W: Espeak (formant synthesis, near-instant)
///   - Pi 4B: Piper medium quality via ONNX
///   - x86 server: Piper high or Kokoro
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TtsEngine {
    /// Piper via ort + espeakng-sys phonemizer — VITS ONNX models.
    /// Natural quality, sentence-level streaming.
    #[default]
    Piper,
    /// espeak-ng via espeakng-sys FFI — formant synthesis.
    /// Robotic but instant. Universal fallback, also Piper's phonemizer.
    Espeak,
    /// Kokoro via ort — StyleTTS2-based, 82M params.
    /// Highest quality for size. Community ONNX exports.
    Kokoro,
}

impl std::fmt::Display for TtsEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Piper => "piper",
            Self::Espeak => "espeak",
            Self::Kokoro => "kokoro",
        })
    }
}

/// Voice activity detection engine selection.
///
/// Silero requires ONNX Runtime (shared with Piper/Moonshine).
/// WebRTC VAD is a lightweight C library for minimal deployments.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VadEngine {
    /// Silero VAD v5 via ort — 2MB ONNX model, 32ms chunks.
    /// ~189μs/chunk on x86, ~1-3ms on Pi 4B.
    #[default]
    Silero,
    /// WebRTC VAD — lightweight C library, no ONNX dependency.
    /// For minimal deployments (Pi Zero 2W) that avoid ONNX Runtime.
    Webrtc,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VoiceConfig {
    /// STT engine selection.
    #[serde(default)]
    pub stt_engine: SttEngine,
    /// Path to STT model files.
    pub stt_model_path: String,

    /// TTS engine selection.
    #[serde(default)]
    pub tts_engine: TtsEngine,
    /// Path to TTS model/voice files. None is valid for Espeak (no model needed).
    #[serde(default)]
    pub tts_model_path: Option<String>,

    /// Voice activity detection engine.
    #[serde(default)]
    pub vad_engine: VadEngine,
    /// VAD speech probability threshold (0.0–1.0). Default 0.5.
    #[serde(default = "default_vad_threshold")]
    pub vad_threshold: f32,

    /// Audio input device name (None = system default).
    #[serde(default)]
    pub input_device: Option<String>,
    /// Audio output device name (None = system default).
    #[serde(default)]
    pub output_device: Option<String>,
    /// Audio sample rate in Hz. Default 16000 (required by Whisper/Moonshine/Silero).
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
}

fn default_vad_threshold() -> f32 {
    0.5
}
fn default_sample_rate() -> u32 {
    16000
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MatrixConfig {
    /// Homeserver URL (e.g. "https://matrix.example.com").
    pub homeserver: String,
    /// Matrix user ID (e.g. "@bot:example.com").
    pub user_id: String,
}

/// Slack operating mode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlackMode {
    /// Bot identity — uses bot + app tokens, Socket Mode WebSocket.
    #[default]
    Bot,
    /// Operator proxy — uses user token, polls conversations.history.
    Proxy,
}

/// Proxy mode posture — controls write capability.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlackPosture {
    /// Read-only. Agent can see messages but not send.
    #[default]
    Observe,
    /// Read/write. Agent can send messages as the operator.
    Participate,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SlackConfig {
    /// Workspace identifier for display purposes.
    pub workspace: String,
    /// Operating mode: "bot" (default) or "proxy" (user token polling).
    #[serde(default)]
    pub mode: SlackMode,

    // ── Bot mode fields ─────────────────────────────────────────────
    /// Default channel ID to post to when no channel is specified.
    #[serde(default)]
    pub default_channel: Option<String>,
    /// Only respond to messages that mention the bot (in channels).
    /// DMs are always processed regardless of this setting.
    #[serde(default = "default_true")]
    pub require_mention: bool,
    /// Allowlist of Slack user IDs permitted to interact. Empty = allow all.
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// Operator user IDs — full instruction authority. See DiscordConfig.operators.
    #[serde(default)]
    pub operators: Vec<String>,
    /// Operator Slack User Group IDs (e.g. "S0615G0PL"). Members of these
    /// groups get operator trust. Resolved at connector startup via
    /// usergroups.users.list API — no per-message API calls.
    #[serde(default)]
    pub operator_groups: Vec<String>,

    // ── Proxy mode fields ───────────────────────────────────────────
    /// Posture: "observe" (read-only, default) or "participate" (read/write).
    #[serde(default)]
    pub posture: SlackPosture,
    /// Channel IDs to watch in proxy mode. Empty = auto-discover all
    /// channels the user is a member of via conversations.list.
    #[serde(default)]
    pub watch_channels: Vec<String>,
    /// Poll interval in seconds for proxy mode (default: 30).
    #[serde(default = "default_proxy_poll_secs")]
    pub proxy_poll_secs: u64,
    /// Channel rediscovery interval in seconds when watch_channels is
    /// empty (default: 300). Ignored when watch_channels is explicit.
    #[serde(default = "default_channel_refresh_secs")]
    pub channel_refresh_secs: u64,
}

fn default_proxy_poll_secs() -> u64 {
    30
}

fn default_channel_refresh_secs() -> u64 {
    300
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DiscordConfig {
    /// Guild/server ID to operate in (optional, None = all guilds).
    #[serde(default)]
    pub guild_id: Option<String>,
    /// Only respond to messages that mention the bot (in guild channels).
    /// DMs are always processed regardless. Defaults to true.
    #[serde(default = "default_true")]
    pub require_mention: bool,
    /// Allowlist of Discord user IDs permitted to interact. Empty = allow all.
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// Operator user IDs — messages from these users have full instruction
    /// authority. Messages from all other users are treated as external
    /// input (the agent responds but won't follow embedded instructions).
    #[serde(default)]
    pub operators: Vec<String>,
    /// Operator role IDs — any guild member with one of these Discord roles
    /// gets operator trust level. Checked in addition to `operators`.
    /// Use Discord Developer Mode to copy role IDs: Server Settings →
    /// Roles → right-click → Copy Role ID.
    #[serde(default)]
    pub operator_roles: Vec<String>,
}

// ---------------------------------------------------------------------------
// Secret store — in-memory, zeroized on drop, never persisted
// ---------------------------------------------------------------------------

/// Holds secrets delivered via bootstrap_secrets RPC. Values are wrapped in
/// `SecretString` for automatic zeroization on drop. Never written to disk.
pub struct SecretStore {
    secrets: RwLock<HashMap<String, SecretString>>,
}

impl SecretStore {
    pub fn new() -> Self {
        Self {
            secrets: RwLock::new(HashMap::new()),
        }
    }

    /// Ingest secrets from the bootstrap_secrets RPC payload.
    /// Clears any previously stored secrets before inserting new ones
    /// to prevent stale secrets from persisting across restarts.
    pub fn bootstrap(&self, pairs: HashMap<String, String>) {
        let mut store = self.secrets.write().unwrap();
        store.clear();
        for (name, value) in pairs {
            store.insert(name, SecretString::from(value));
        }
    }

    /// Retrieve a secret value. Caller is responsible for not logging it.
    pub fn get(&self, name: &str) -> Option<String> {
        let store = self.secrets.read().unwrap();
        store.get(name).map(|s| s.expose_secret().to_string())
    }

    /// Check if a secret exists without exposing its value.
    pub fn has(&self, name: &str) -> bool {
        self.secrets.read().unwrap().contains_key(name)
    }

    /// List secret names (never values).
    pub fn names(&self) -> Vec<String> {
        self.secrets.read().unwrap().keys().cloned().collect()
    }
}

impl Default for SecretStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Connector factory
// ---------------------------------------------------------------------------

/// Trait for building a connector from config + secrets. Each connector crate
/// implements this so the vox binary can initialize connectors generically.
#[async_trait]
pub trait ConnectorFactory: Send + Sync {
    /// Build and return a boxed connector, or an error if required
    /// secrets/config are missing.
    async fn build(
        config: &Value,
        secrets: &SecretStore,
    ) -> Result<Box<dyn Connector>>
    where
        Self: Sized;
}
