use std::collections::HashMap;
use std::pin::Pin;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_core::Stream;
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    SelectAll { streams }
}

/// A simple select-all stream combinator that polls all inner streams round-robin.
struct SelectAll<'a> {
    streams: Vec<Pin<Box<dyn Stream<Item = InboundMessage> + Send + 'a>>>,
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
        for i in (0..len).rev() {
            match self.streams[i].as_mut().poll_next(cx) {
                Poll::Ready(Some(item)) => return Poll::Ready(Some(item)),
                Poll::Ready(None) => {
                    drop(self.streams.swap_remove(i));
                }
                Poll::Pending => {}
            }
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
