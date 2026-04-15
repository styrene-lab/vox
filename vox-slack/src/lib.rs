use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing;
use vox_core::{
    Address, BodyPart, ChannelHints, Connector, ConnectorCapabilities, ConnectorFactory,
    ConnectorStatus, Envelope, Error, InboundMessage, MessageId, OutboundMessage, Result,
    SecretStore, SlackConfig,
};

// ---------------------------------------------------------------------------
// Slack API types (subset)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SlackApiResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    ts: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    channel: Option<String>,
}

/// Envelope from Slack Socket Mode WebSocket.
#[derive(Debug, Deserialize)]
struct SocketEnvelope {
    envelope_id: String,
    #[serde(rename = "type")]
    envelope_type: String,
    #[serde(default)]
    payload: serde_json::Value,
    #[serde(default)]
    #[allow(dead_code)]
    accepts_response_payload: bool,
}

/// Acknowledgement sent back to Slack for every envelope.
#[derive(Serialize)]
struct SocketAck {
    envelope_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<serde_json::Value>,
}

/// Parsed event from the Socket Mode payload.
#[derive(Debug, Deserialize)]
struct EventCallback {
    #[serde(default)]
    event: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Connector state
// ---------------------------------------------------------------------------

const MAX_INBOX_SIZE: usize = 1000;

struct SlackState {
    inbox: VecDeque<InboundMessage>,
    status: ConnectorStatus,
    bot_user_id: Option<String>,
    /// Cached set of user IDs with operator trust. Built at startup from
    /// config.operators + resolved config.operator_groups membership.
    operator_user_ids: HashSet<String>,
}

/// Slack connector using Socket Mode (WebSocket) for receiving events and
/// the Web API for sending messages. No public URL required.
///
/// Secrets consumed (via bootstrap_secrets):
///   - VOX_SLACK_BOT_TOKEN: required — xoxb-* Bot User OAuth Token
///   - VOX_SLACK_APP_TOKEN: required — xapp-* App-Level Token (Socket Mode)
pub struct SlackConnector {
    config: SlackConfig,
    bot_token: Option<String>,
    app_token: Option<String>,
    http: Client,
    state: Arc<Mutex<SlackState>>,
    notify: Arc<Notify>,
}

impl SlackConnector {
    pub fn new(config: SlackConfig, secrets: &SecretStore) -> Self {
        let bot_token = secrets.get("VOX_SLACK_BOT_TOKEN");
        let app_token = secrets.get("VOX_SLACK_APP_TOKEN");

        let status = if bot_token.is_some() && app_token.is_some() {
            tracing::info!(
                workspace = %config.workspace,
                require_mention = config.require_mention,
                "slack connector initialized"
            );
            ConnectorStatus::Initializing
        } else {
            let missing: Vec<&str> = [
                (!bot_token.is_some()).then_some("VOX_SLACK_BOT_TOKEN"),
                (!app_token.is_some()).then_some("VOX_SLACK_APP_TOKEN"),
            ]
            .into_iter()
            .flatten()
            .collect();
            tracing::warn!(?missing, "slack connector degraded — missing secrets");
            ConnectorStatus::Degraded
        };

        Self {
            config,
            bot_token,
            app_token,
            http: Client::new(),
            state: Arc::new(Mutex::new(SlackState {
                inbox: VecDeque::new(),
                status,
                bot_user_id: None,
                operator_user_ids: HashSet::new(),
            })),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Start the Socket Mode WebSocket listener in a background task.
    /// Call this after construction to begin receiving events.
    pub async fn start(&self) -> Result<()> {
        let Some(ref app_token) = self.app_token else {
            return Err(Error::SendFailed(
                "cannot start socket mode without VOX_SLACK_APP_TOKEN".into(),
            ));
        };
        let Some(ref bot_token) = self.bot_token else {
            return Err(Error::SendFailed(
                "cannot start without VOX_SLACK_BOT_TOKEN".into(),
            ));
        };

        // Resolve bot user ID for mention filtering
        self.resolve_bot_identity(bot_token).await;

        // Build operator user ID set: explicit IDs + resolved usergroup members
        {
            let mut op_ids: HashSet<String> =
                self.config.operators.iter().cloned().collect();

            for group_id in &self.config.operator_groups {
                match self.resolve_usergroup_members(bot_token, group_id).await {
                    Ok(members) => {
                        tracing::info!(
                            group = %group_id,
                            members = members.len(),
                            "resolved slack operator group"
                        );
                        op_ids.extend(members);
                    }
                    Err(e) => {
                        tracing::warn!(
                            group = %group_id,
                            error = %e,
                            "failed to resolve operator group — group members won't have operator trust"
                        );
                    }
                }
            }

            if !op_ids.is_empty() {
                tracing::info!(count = op_ids.len(), "slack operator user IDs resolved");
            }

            self.state.lock().await.operator_user_ids = op_ids;
        }

        let state = self.state.clone();
        let notify = self.notify.clone();
        let config = self.config.clone();
        let http = self.http.clone();
        let app_token = app_token.clone();
        let bot_user_id = {
            let s = state.lock().await;
            s.bot_user_id.clone()
        };

        // Spawn the WebSocket listener — it refreshes the URL on each reconnect
        tokio::spawn(async move {
            socket_mode_loop(http, app_token, state, notify, config, bot_user_id).await;
        });

        // Mark as connected
        {
            let mut s = self.state.lock().await;
            s.status = ConnectorStatus::Connected;
        }

        tracing::info!(workspace = %self.config.workspace, "slack socket mode connected");
        Ok(())
    }

    async fn resolve_bot_identity(&self, bot_token: &str) {
        let resp = self
            .http
            .get("https://slack.com/api/auth.test")
            .bearer_auth(bot_token)
            .send()
            .await;

        match resp {
            Ok(r) => {
                if let Ok(body) = r.json::<serde_json::Value>().await {
                    if let Some(user_id) = body.get("user_id").and_then(|v| v.as_str()) {
                        let mut s = self.state.lock().await;
                        s.bot_user_id = Some(user_id.to_string());
                        tracing::info!(bot_user_id = user_id, "resolved slack bot identity");
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "failed to resolve bot identity"),
        }
    }

    /// Fetch members of a Slack User Group via usergroups.users.list API.
    async fn resolve_usergroup_members(
        &self,
        bot_token: &str,
        group_id: &str,
    ) -> std::result::Result<Vec<String>, String> {
        let resp = self
            .http
            .get("https://slack.com/api/usergroups.users.list")
            .bearer_auth(bot_token)
            .query(&[("usergroup", group_id)])
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("response parse failed: {e}"))?;

        if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let err = body
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            return Err(format!("API error: {err}"));
        }

        let users = body
            .get("users")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        Ok(users)
    }

    async fn post_message(
        &self,
        channel: &str,
        text: &str,
        thread_ts: Option<&str>,
    ) -> Result<MessageId> {
        let Some(ref bot_token) = self.bot_token else {
            return Err(Error::SendFailed("no bot token".into()));
        };

        let mut body = serde_json::json!({
            "channel": channel,
            "text": text,
        });

        if let Some(ts) = thread_ts {
            body["thread_ts"] = serde_json::Value::String(ts.to_string());
        }

        let resp = self
            .http
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(bot_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::SendFailed(format!("chat.postMessage failed: {e}")))?;

        let api_resp: SlackApiResponse = resp
            .json()
            .await
            .map_err(|e| Error::SendFailed(format!("response parse failed: {e}")))?;

        if !api_resp.ok {
            return Err(Error::SendFailed(format!(
                "chat.postMessage error: {}",
                api_resp.error.unwrap_or_default()
            )));
        }

        Ok(api_resp.ts.unwrap_or_default())
    }

    async fn add_reaction(&self, channel: &str, timestamp: &str, emoji: &str) -> Result<()> {
        let Some(ref bot_token) = self.bot_token else {
            return Err(Error::SendFailed("no bot token".into()));
        };

        let body = serde_json::json!({
            "channel": channel,
            "timestamp": timestamp,
            "name": emoji.trim_matches(':'),
        });

        let resp = self
            .http
            .post("https://slack.com/api/reactions.add")
            .bearer_auth(bot_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::SendFailed(format!("reactions.add failed: {e}")))?;

        let api_resp: SlackApiResponse = resp
            .json()
            .await
            .map_err(|e| Error::SendFailed(format!("response parse failed: {e}")))?;

        if !api_resp.ok {
            let err = api_resp.error.unwrap_or_default();
            // "already_reacted" is not a real error
            if err != "already_reacted" {
                return Err(Error::SendFailed(format!("reactions.add error: {err}")));
            }
        }

        Ok(())
    }
}

/// Background task: connect to Slack Socket Mode WebSocket, receive events,
/// acknowledge them, parse messages, and push to the inbox.
/// Refreshes the WebSocket URL on each reconnect (Socket Mode URLs are single-use).
async fn socket_mode_loop(
    http: Client,
    app_token: String,
    state: Arc<Mutex<SlackState>>,
    notify: Arc<Notify>,
    config: SlackConfig,
    bot_user_id: Option<String>,
) {
    loop {
        tracing::info!("connecting to slack socket mode");

        // Fetch a fresh WebSocket URL — Slack Socket Mode URLs are single-use.
        let ws_url = match open_socket_url(&http, &app_token).await {
            Ok(url) => url,
            Err(e) => {
                tracing::error!(error = %e, "failed to open socket mode connection, retrying in 5s");
                {
                    let mut s = state.lock().await;
                    s.status = ConnectorStatus::Degraded;
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        let connect_result = tokio_tungstenite::connect_async(&ws_url).await;
        let (mut ws, _) = match connect_result {
            Ok(pair) => pair,
            Err(e) => {
                tracing::error!(error = %e, "socket mode connection failed, retrying in 5s");
                {
                    let mut s = state.lock().await;
                    s.status = ConnectorStatus::Degraded;
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        {
            let mut s = state.lock().await;
            s.status = ConnectorStatus::Connected;
        }

        while let Some(msg_result) = ws.next().await {
            let msg = match msg_result {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(error = %e, "socket mode read error");
                    break;
                }
            };

            let text = match msg {
                WsMessage::Text(t) => t,
                WsMessage::Ping(data) => {
                    let _ = ws.send(WsMessage::Pong(data)).await;
                    continue;
                }
                WsMessage::Close(_) => {
                    tracing::info!("socket mode connection closed by server");
                    break;
                }
                _ => continue,
            };

            let envelope: SocketEnvelope = match serde_json::from_str(&text) {
                Ok(e) => e,
                Err(e) => {
                    tracing::debug!(error = %e, "unparseable socket mode message");
                    continue;
                }
            };

            // Always acknowledge immediately
            let ack = SocketAck {
                envelope_id: envelope.envelope_id.clone(),
                payload: None,
            };
            if let Ok(ack_json) = serde_json::to_string(&ack) {
                let _ = ws.send(WsMessage::Text(ack_json.into())).await;
            }

            // Process events_api envelopes
            if envelope.envelope_type == "events_api" {
                if let Ok(callback) = serde_json::from_value::<EventCallback>(envelope.payload) {
                    let op_ids = { state.lock().await.operator_user_ids.clone() };
                    if let Some(inbound) =
                        parse_slack_event(&callback.event, &config, bot_user_id.as_deref(), &op_ids)
                    {
                        let mut s = state.lock().await;
                        if s.inbox.len() >= MAX_INBOX_SIZE {
                            tracing::warn!("slack inbox full ({MAX_INBOX_SIZE}), dropping oldest message");
                            s.inbox.pop_front();
                        }
                        s.inbox.push_back(inbound);
                        drop(s);
                        notify.notify_one();
                    }
                }
            }
        }

        // Disconnected — reconnect after delay
        {
            let mut s = state.lock().await;
            s.status = ConnectorStatus::Disconnected;
        }
        tracing::warn!("socket mode disconnected, reconnecting in 5s");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

/// Fetch a fresh Socket Mode WebSocket URL from Slack.
async fn open_socket_url(http: &Client, app_token: &str) -> Result<String> {
    let resp = http
        .post("https://slack.com/api/apps.connections.open")
        .bearer_auth(app_token)
        .send()
        .await
        .map_err(|e| Error::SendFailed(format!("socket mode open failed: {e}")))?;

    let body: SlackApiResponse = resp
        .json()
        .await
        .map_err(|e| Error::SendFailed(format!("socket mode response parse failed: {e}")))?;

    if !body.ok {
        return Err(Error::SendFailed(format!(
            "socket mode open failed: {}",
            body.error.unwrap_or_default()
        )));
    }

    body.url
        .ok_or_else(|| Error::SendFailed("no WebSocket URL in response".into()))
}

/// Parse a Slack event into a vox InboundMessage, applying mention/DM/allowlist filtering.
fn parse_slack_event(
    event: &serde_json::Value,
    config: &SlackConfig,
    bot_user_id: Option<&str>,
    operator_user_ids: &HashSet<String>,
) -> Option<InboundMessage> {
    let event_type = event.get("type")?.as_str()?;

    // Only handle message events (not message_changed, etc.)
    if event_type != "message" {
        return None;
    }

    // Skip bot messages (including our own) and subtypes like message_changed
    if event.get("subtype").is_some() || event.get("bot_id").is_some() {
        return None;
    }

    let user = event.get("user")?.as_str()?;
    let text = event.get("text")?.as_str()?;
    let channel = event.get("channel")?.as_str()?;
    let ts = event.get("ts")?.as_str()?;
    let thread_ts = event.get("thread_ts").and_then(|v| v.as_str());
    let channel_type = event.get("channel_type").and_then(|v| v.as_str());

    // Allowlist check
    if !config.allowed_users.is_empty() && !config.allowed_users.contains(&user.to_string()) {
        tracing::debug!(user, "message from non-allowlisted user, ignoring");
        return None;
    }

    let is_dm = channel_type == Some("im");

    // Mention check for channel messages (DMs always pass)
    if !is_dm && config.require_mention {
        if let Some(bot_id) = bot_user_id {
            let mention_pattern = format!("<@{bot_id}>");
            if !text.contains(&mention_pattern) {
                return None;
            }
        }
    }

    // Strip bot mention from the text for cleaner agent input
    let clean_text = if let Some(bot_id) = bot_user_id {
        let mention_pattern = format!("<@{bot_id}>");
        text.replace(&mention_pattern, "").trim().to_string()
    } else {
        text.to_string()
    };

    if clean_text.is_empty() {
        return None;
    }

    Some(InboundMessage {
        id: ts.to_string(),
        channel: "slack".to_string(),
        sender: Address {
            id: user.to_string(),
            display_name: None,
        },
        timestamp: Utc::now(),
        envelope: Envelope::Channel {
            workspace: config.workspace.clone(),
            channel_id: channel.to_string(),
        },
        body: vec![BodyPart::Text {
            content: clean_text,
        }],
        thread_id: thread_ts.map(|s| s.to_string()),
        reply_to: None,
        reaction: None,
        hints: ChannelHints::Slack {
            username: None,
            icon_emoji: None,
            unfurl: false,
        },
        trust_level: if operator_user_ids.contains(user) {
            vox_core::TrustLevel::Operator
        } else {
            vox_core::TrustLevel::User
        },
        metadata: serde_json::json!({
            "slack_ts": ts,
            "slack_channel": channel,
            "slack_user": user,
            "is_dm": is_dm,
        }),
    })
}

#[async_trait]
impl Connector for SlackConnector {
    fn name(&self) -> &str {
        "slack"
    }

    fn status(&self) -> ConnectorStatus {
        // Return cached status without blocking
        // (the real status is updated by the socket mode loop)
        match self.state.try_lock() {
            Ok(s) => s.status,
            Err(_) => ConnectorStatus::Connected, // assume ok if lock contended
        }
    }

    fn capabilities(&self) -> ConnectorCapabilities {
        ConnectorCapabilities {
            send: true,
            receive: true,
            threads: true,
            reactions: true,
            attachments: false, // TODO: file uploads
            read_receipts: false,
            rich_text: true,
            typing_indicators: false,
            disappearing_messages: false,
            groups: false,
            push: false, // uses poll() backed by internal inbox from socket mode
        }
    }

    async fn send(&self, message: OutboundMessage) -> Result<MessageId> {
        // Determine target channel
        let channel_id = match &message.envelope {
            Envelope::Channel { channel_id, .. } => channel_id.clone(),
            Envelope::Direct { to } if !to.is_empty() => {
                // For DMs, the "to" id is the channel ID (Slack DM channel)
                to[0].id.clone()
            }
            _ => self
                .config
                .default_channel
                .clone()
                .ok_or_else(|| Error::SendFailed("no target channel and no default set".into()))?,
        };

        // Handle reactions
        if let Some(ref reaction) = message.reaction {
            self.add_reaction(&channel_id, &reaction.target, &reaction.emoji)
                .await?;
            return Ok(reaction.target.clone());
        }

        // Extract text from body parts
        let text: String = message
            .body
            .iter()
            .filter_map(|part| match part {
                BodyPart::Text { content } => Some(content.as_str()),
                BodyPart::Rich { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        if text.is_empty() {
            return Err(Error::SendFailed("empty message body".into()));
        }

        // Slack message limit is 40,000 characters — truncate at char boundary
        let text = if text.len() > 40_000 {
            tracing::warn!(len = text.len(), "truncating message to Slack 40k char limit");
            let boundary = text.char_indices()
                .take_while(|(i, _)| *i <= 39_980)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(0);
            format!("{}... [truncated]", &text[..boundary])
        } else {
            text
        };

        self.post_message(&channel_id, &text, message.thread_id.as_deref())
            .await
    }

    async fn poll(&self) -> Result<Vec<InboundMessage>> {
        let mut s = self.state.lock().await;
        let messages: Vec<InboundMessage> = s.inbox.drain(..).collect();
        Ok(messages)
    }
}

#[async_trait]
impl ConnectorFactory for SlackConnector {
    async fn build(
        config: &serde_json::Value,
        secrets: &SecretStore,
    ) -> Result<Box<dyn Connector>> {
        let cfg: SlackConfig =
            serde_json::from_value(config.clone()).map_err(|e| Error::Other(e.into()))?;
        let connector = SlackConnector::new(cfg, secrets);
        // Note: caller must call connector.start() after build to begin receiving.
        // This is handled by the vox binary's init_connectors path.
        Ok(Box::new(connector))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slack_config() -> SlackConfig {
        SlackConfig {
            workspace: "test".into(),
            default_channel: None,
            require_mention: true,
            allowed_users: vec![],
            operators: vec![],
            operator_groups: vec![],
        }
    }

    fn empty_ops() -> HashSet<String> {
        HashSet::new()
    }

    fn message_event(user: &str, text: &str, channel: &str, ts: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "message",
            "user": user,
            "text": text,
            "channel": channel,
            "ts": ts,
            "channel_type": "channel"
        })
    }

    #[test]
    fn parse_basic_mention() {
        let event = message_event("U123", "<@BOT1> hello", "C456", "1234.5678");
        let msg = parse_slack_event(&event, &slack_config(), Some("BOT1"), &empty_ops());
        assert!(msg.is_some());
        let msg = msg.unwrap();
        assert_eq!(msg.sender.id, "U123");
        assert_eq!(msg.body[0], BodyPart::Text { content: "hello".into() });
    }

    #[test]
    fn skip_without_mention_when_required() {
        let event = message_event("U123", "hello without mention", "C456", "1234.5678");
        let msg = parse_slack_event(&event, &slack_config(), Some("BOT1"), &empty_ops());
        assert!(msg.is_none());
    }

    #[test]
    fn dm_bypasses_mention_check() {
        let mut event = message_event("U123", "hello", "D456", "1234.5678");
        event["channel_type"] = serde_json::json!("im");
        let msg = parse_slack_event(&event, &slack_config(), Some("BOT1"), &empty_ops());
        assert!(msg.is_some());
    }

    #[test]
    fn skip_bot_messages() {
        let mut event = message_event("U123", "bot says hi", "C456", "1234.5678");
        event["bot_id"] = serde_json::json!("B123");
        let msg = parse_slack_event(&event, &slack_config(), Some("BOT1"), &empty_ops());
        assert!(msg.is_none());
    }

    #[test]
    fn skip_subtypes() {
        let mut event = message_event("U123", "edited", "C456", "1234.5678");
        event["subtype"] = serde_json::json!("message_changed");
        let msg = parse_slack_event(&event, &slack_config(), Some("BOT1"), &empty_ops());
        assert!(msg.is_none());
    }

    #[test]
    fn allowlist_blocks_unknown_user() {
        let config = SlackConfig {
            allowed_users: vec!["U999".into()],
            ..slack_config()
        };
        let event = message_event("U123", "<@BOT1> hello", "C456", "1234.5678");
        let msg = parse_slack_event(&event, &config, Some("BOT1"), &empty_ops());
        assert!(msg.is_none());
    }

    #[test]
    fn allowlist_permits_known_user() {
        let config = SlackConfig {
            allowed_users: vec!["U123".into()],
            ..slack_config()
        };
        let event = message_event("U123", "<@BOT1> hello", "C456", "1234.5678");
        let msg = parse_slack_event(&event, &config, Some("BOT1"), &empty_ops());
        assert!(msg.is_some());
    }

    #[test]
    fn empty_after_mention_strip_is_skipped() {
        let event = message_event("U123", "<@BOT1>", "C456", "1234.5678");
        let msg = parse_slack_event(&event, &slack_config(), Some("BOT1"), &empty_ops());
        assert!(msg.is_none());
    }

    #[test]
    fn thread_ts_becomes_thread_id() {
        let mut event = message_event("U123", "<@BOT1> in thread", "C456", "1234.5678");
        event["thread_ts"] = serde_json::json!("1111.2222");
        let msg = parse_slack_event(&event, &slack_config(), Some("BOT1"), &empty_ops()).unwrap();
        assert_eq!(msg.thread_id, Some("1111.2222".into()));
    }
}
