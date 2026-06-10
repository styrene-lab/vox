use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use vox_core::{
    Address, BodyPart, ChannelHints, Connector, ConnectorCapabilities, ConnectorFactory,
    ConnectorStatus, DiscordConfig, Envelope, Error, InboundMessage, MessageId, OutboundMessage,
    Result, SecretStore,
};

const DISCORD_API: &str = "https://discord.com/api/v10";
const DISCORD_GATEWAY: &str = "wss://gateway.discord.gg/?v=10&encoding=json";

// ---------------------------------------------------------------------------
// Discord API types (subset)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GatewayPayload {
    op: u8,
    #[serde(default)]
    d: serde_json::Value,
    #[serde(default)]
    s: Option<u64>,
    #[serde(default)]
    t: Option<String>,
}

// Gateway payloads are constructed as raw serde_json::json! values below,
// so these typed structs are not needed. Kept as documentation of the wire
// format but suppressed from dead-code warnings.

#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct GatewayIdentify {
    token: String,
    intents: u64,
    properties: GatewayProperties,
}

#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct GatewayProperties {
    os: String,
    browser: String,
    device: String,
}

#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct GatewayHeartbeat {
    op: u8,
    d: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DiscordUser {
    id: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    bot: bool,
}

#[derive(Debug, Deserialize)]
struct ReadyEvent {
    user: DiscordUser,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    resume_gateway_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GuildMember {
    /// Role IDs the member has in this guild.
    #[serde(default)]
    roles: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DiscordMessage {
    id: String,
    channel_id: String,
    #[serde(default)]
    guild_id: Option<String>,
    author: DiscordUser,
    content: String,
    #[serde(default)]
    #[allow(dead_code)]
    timestamp: String,
    #[serde(default)]
    message_reference: Option<MessageReference>,
    /// Guild member info — present on MESSAGE_CREATE in guilds.
    /// Contains the sender's roles for permission checks.
    #[serde(default)]
    member: Option<GuildMember>,
}

#[derive(Debug, Deserialize)]
struct MessageReference {
    #[serde(default)]
    message_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DiscordApiResponse {
    id: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    code: Option<u64>,
}

// ---------------------------------------------------------------------------
// Discord Gateway intents
// ---------------------------------------------------------------------------

/// GUILDS | GUILD_MESSAGES | GUILD_MESSAGE_REACTIONS | DIRECT_MESSAGES | MESSAGE_CONTENT
const GATEWAY_INTENTS: u64 = (1 << 0) | (1 << 9) | (1 << 10) | (1 << 12) | (1 << 15);

// ---------------------------------------------------------------------------
// Connector state
// ---------------------------------------------------------------------------

const MAX_INBOX_SIZE: usize = 1000;

struct DiscordState {
    inbox: VecDeque<InboundMessage>,
    status: ConnectorStatus,
    bot_user_id: Option<String>,
    sequence: Option<u64>,
    session_id: Option<String>,
    resume_gateway_url: Option<String>,
}

/// Discord connector using the Gateway WebSocket for receiving events and
/// the REST API for sending messages. No public URL required.
///
/// Secrets consumed (via bootstrap_secrets):
///   - VOX_DISCORD_BOT_TOKEN: required — Bot token from the Discord Developer Portal
pub struct DiscordConnector {
    config: DiscordConfig,
    bot_token: Option<String>,
    http: Client,
    state: Arc<Mutex<DiscordState>>,
    notify: Arc<Notify>,
}

impl DiscordConnector {
    pub fn new(config: DiscordConfig, secrets: &SecretStore) -> Self {
        let bot_token = match secrets
            .get_or_file("VOX_DISCORD_BOT_TOKEN", config.bot_token_file.as_deref())
        {
            Ok(token) => token,
            Err(e) => {
                tracing::warn!(error = %e, "discord connector degraded — failed to read bot token file");
                None
            }
        };

        let status = if bot_token.is_some() {
            tracing::info!(
                guild_id = ?config.guild_id,
                "discord connector initialized"
            );
            ConnectorStatus::Initializing
        } else {
            tracing::warn!("discord connector degraded — missing VOX_DISCORD_BOT_TOKEN");
            ConnectorStatus::Degraded
        };

        Self {
            config,
            bot_token,
            http: Client::new(),
            state: Arc::new(Mutex::new(DiscordState {
                inbox: VecDeque::new(),
                status,
                bot_user_id: None,
                sequence: None,
                session_id: None,
                resume_gateway_url: None,
            })),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Start the Gateway WebSocket listener in a background task.
    pub async fn start(&self) -> Result<()> {
        let Some(ref token) = self.bot_token else {
            return Err(Error::SendFailed(
                "cannot start without VOX_DISCORD_BOT_TOKEN".into(),
            ));
        };

        let state = self.state.clone();
        let notify = self.notify.clone();
        let config = self.config.clone();
        let token = token.clone();

        tokio::spawn(async move {
            gateway_loop(token, state, notify, config).await;
        });

        Ok(())
    }

    async fn send_message(
        &self,
        channel_id: &str,
        content: &str,
        reply_to: Option<&str>,
    ) -> Result<MessageId> {
        let Some(ref token) = self.bot_token else {
            return Err(Error::SendFailed("no bot token".into()));
        };

        let mut body = serde_json::json!({ "content": content });

        if let Some(msg_id) = reply_to {
            body["message_reference"] = serde_json::json!({
                "message_id": msg_id,
            });
        }

        let resp = self
            .http
            .post(format!("{DISCORD_API}/channels/{channel_id}/messages"))
            .header("Authorization", format!("Bot {token}"))
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::SendFailed(format!("discord send failed: {e}")))?;

        let api_resp: DiscordApiResponse = resp
            .json()
            .await
            .map_err(|e| Error::SendFailed(format!("response parse failed: {e}")))?;

        if let Some(code) = api_resp.code {
            return Err(Error::SendFailed(format!(
                "discord API error {}: {}",
                code,
                api_resp.message.unwrap_or_default()
            )));
        }

        Ok(api_resp.id.unwrap_or_default())
    }

    async fn add_reaction(&self, channel_id: &str, message_id: &str, emoji: &str) -> Result<()> {
        let Some(ref token) = self.bot_token else {
            return Err(Error::SendFailed("no bot token".into()));
        };

        let encoded_emoji = urlencoding::encode(emoji.trim_matches(':'));

        let resp = self
            .http
            .put(format!(
                "{DISCORD_API}/channels/{channel_id}/messages/{message_id}/reactions/{encoded_emoji}/@me"
            ))
            .header("Authorization", format!("Bot {token}"))
            .send()
            .await
            .map_err(|e| Error::SendFailed(format!("discord reaction failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::SendFailed(format!(
                "discord reaction error {status}: {body}"
            )));
        }

        Ok(())
    }
}

/// Background task: connect to Discord Gateway, identify or resume, receive events.
async fn gateway_loop(
    token: String,
    state: Arc<Mutex<DiscordState>>,
    notify: Arc<Notify>,
    config: DiscordConfig,
) {
    loop {
        // Try resume URL first if we have a session, fall back to fresh gateway
        let (gateway_url, can_resume) = {
            let s = state.lock().await;
            if let (Some(ref url), Some(_), Some(_)) =
                (&s.resume_gateway_url, &s.session_id, &s.sequence)
            {
                (url.clone(), true)
            } else {
                (DISCORD_GATEWAY.to_string(), false)
            }
        };

        tracing::info!(resume = can_resume, "connecting to discord gateway");

        let connect_result = tokio_tungstenite::connect_async(&gateway_url).await;
        let (mut ws, _) = match connect_result {
            Ok(pair) => pair,
            Err(e) => {
                tracing::error!(error = %e, "gateway connection failed, retrying in 5s");
                {
                    let mut s = state.lock().await;
                    s.status = ConnectorStatus::Degraded;
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        let mut heartbeat_interval: Option<tokio::time::Interval> = None;
        let mut identified = false;

        loop {
            let ws_msg = if let Some(ref mut interval) = heartbeat_interval {
                tokio::select! {
                    msg = ws.next() => msg,
                    _ = interval.tick() => {
                        // Send heartbeat
                        let seq = { state.lock().await.sequence };
                        let hb = serde_json::json!({ "op": 1, "d": seq });
                        if let Err(e) = ws.send(WsMessage::Text(hb.to_string().into())).await {
                            tracing::warn!(error = %e, "heartbeat send failed");
                            break;
                        }
                        continue;
                    }
                }
            } else {
                ws.next().await
            };

            let msg = match ws_msg {
                Some(Ok(WsMessage::Text(t))) => t,
                Some(Ok(WsMessage::Close(_))) => {
                    tracing::info!("gateway closed by server");
                    break;
                }
                Some(Err(e)) => {
                    tracing::warn!(error = %e, "gateway read error");
                    break;
                }
                None => break,
                _ => continue,
            };

            let payload: GatewayPayload = match serde_json::from_str(&msg) {
                Ok(p) => p,
                Err(e) => {
                    tracing::debug!(error = %e, "unparseable gateway payload");
                    continue;
                }
            };

            // Track sequence for heartbeats and resume
            if let Some(seq) = payload.s {
                state.lock().await.sequence = Some(seq);
            }

            match payload.op {
                // Hello — start heartbeating, then resume or identify
                10 => {
                    let interval_ms = payload
                        .d
                        .get("heartbeat_interval")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(41250);
                    heartbeat_interval = Some(tokio::time::interval(
                        std::time::Duration::from_millis(interval_ms),
                    ));

                    if !identified {
                        // Attempt Resume if we have a prior session
                        let resume_data = {
                            let s = state.lock().await;
                            s.session_id
                                .as_ref()
                                .zip(s.sequence)
                                .map(|(sid, seq)| (sid.clone(), seq))
                        };

                        let payload_json = if let Some((session_id, seq)) = resume_data {
                            tracing::info!(session_id = %session_id, seq, "attempting gateway resume");
                            serde_json::json!({
                                "op": 6,
                                "d": {
                                    "token": token,
                                    "session_id": session_id,
                                    "seq": seq,
                                }
                            })
                        } else {
                            serde_json::json!({
                                "op": 2,
                                "d": {
                                    "token": token,
                                    "intents": GATEWAY_INTENTS,
                                    "properties": {
                                        "os": "linux",
                                        "browser": "vox",
                                        "device": "vox",
                                    }
                                }
                            })
                        };

                        if let Err(e) = ws
                            .send(WsMessage::Text(payload_json.to_string().into()))
                            .await
                        {
                            tracing::error!(error = %e, "identify/resume send failed");
                            break;
                        }
                        identified = true;
                    }
                }
                // Heartbeat ACK
                11 => {}
                // Dispatch (event)
                0 => {
                    let event_name = payload.t.as_deref().unwrap_or("");

                    match event_name {
                        "READY" => {
                            if let Ok(ready) = serde_json::from_value::<ReadyEvent>(payload.d) {
                                let mut s = state.lock().await;
                                s.bot_user_id = Some(ready.user.id.clone());
                                s.session_id = Some(ready.session_id.clone());
                                s.resume_gateway_url = ready.resume_gateway_url.clone();
                                s.status = ConnectorStatus::Connected;
                                tracing::info!(
                                    bot_user = %ready.user.username,
                                    bot_id = %ready.user.id,
                                    session_id = %ready.session_id,
                                    "discord gateway ready"
                                );
                            }
                        }
                        "RESUMED" => {
                            let mut s = state.lock().await;
                            s.status = ConnectorStatus::Connected;
                            tracing::info!("discord gateway resumed successfully");
                        }
                        "MESSAGE_CREATE" => {
                            if let Ok(msg) = serde_json::from_value::<DiscordMessage>(payload.d) {
                                let bot_id = { state.lock().await.bot_user_id.clone() };

                                if let Some(inbound) =
                                    parse_discord_message(&msg, &config, bot_id.as_deref())
                                {
                                    let mut s = state.lock().await;
                                    if s.inbox.len() >= MAX_INBOX_SIZE {
                                        tracing::warn!("discord inbox full ({MAX_INBOX_SIZE}), dropping oldest message");
                                        s.inbox.pop_front();
                                    }
                                    s.inbox.push_back(inbound);
                                    drop(s);
                                    notify.notify_one();
                                }
                            }
                        }
                        _ => {}
                    }
                }
                // Reconnect requested
                7 => {
                    tracing::info!("gateway requested reconnect");
                    break;
                }
                // Invalid session — clear resume state so next connect does fresh identify
                9 => {
                    tracing::warn!("invalid session, clearing resume state");
                    {
                        let mut s = state.lock().await;
                        s.session_id = None;
                        s.resume_gateway_url = None;
                        s.sequence = None;
                    }
                    // identified resets on next iteration of the outer reconnect loop
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    break;
                }
                _ => {}
            }
        }

        // Disconnected — reconnect
        {
            let mut s = state.lock().await;
            s.status = ConnectorStatus::Disconnected;
        }
        tracing::warn!("discord gateway disconnected, reconnecting in 5s");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

/// Determine trust level for a message sender.
///
/// Operator if:
///   1. User ID is in `config.operators`, OR
///   2. Any of the member's guild roles is in `config.operator_roles`
///
/// DMs have no member/roles — only explicit user ID grants operator in DMs.
fn classify_trust(
    config: &DiscordConfig,
    sender_id: &str,
    member: Option<&GuildMember>,
) -> vox_core::TrustLevel {
    // Explicit user ID always wins
    if config.operators.contains(&sender_id.to_string()) {
        return vox_core::TrustLevel::Operator;
    }
    // Check guild roles
    if !config.operator_roles.is_empty() {
        if let Some(member) = member {
            if member
                .roles
                .iter()
                .any(|r| config.operator_roles.contains(r))
            {
                return vox_core::TrustLevel::Operator;
            }
        }
    }
    vox_core::TrustLevel::User
}

/// Parse a Discord MESSAGE_CREATE into a vox InboundMessage.
fn parse_discord_message(
    msg: &DiscordMessage,
    config: &DiscordConfig,
    bot_user_id: Option<&str>,
) -> Option<InboundMessage> {
    // Skip bot messages (including our own)
    if msg.author.bot {
        return None;
    }

    // Guild filter — if configured, only process messages from that guild
    if let Some(ref filter_guild) = config.guild_id {
        if msg.guild_id.as_deref() != Some(filter_guild.as_str()) {
            return None;
        }
    }

    // Allowlist check
    if !config.allowed_users.is_empty() && !config.allowed_users.contains(&msg.author.id) {
        tracing::debug!(user = %msg.author.id, "message from non-allowlisted user, ignoring");
        return None;
    }

    let is_dm = msg.guild_id.is_none();

    // Mention check for guild channels (DMs always pass)
    if !is_dm && config.require_mention {
        if let Some(bot_id) = bot_user_id {
            let mention_pattern = format!("<@{bot_id}>");
            if !msg.content.contains(&mention_pattern) {
                return None;
            }
        } else {
            // Can't check mentions without bot ID, skip guild messages
            return None;
        }
    }

    // Strip bot mention from content
    let clean_content = if let Some(bot_id) = bot_user_id {
        let mention = format!("<@{bot_id}>");
        msg.content.replace(&mention, "").trim().to_string()
    } else {
        msg.content.clone()
    };

    if clean_content.is_empty() {
        return None;
    }

    let envelope = if is_dm {
        Envelope::Direct {
            to: vec![Address {
                id: msg.channel_id.clone(),
                display_name: None,
            }],
        }
    } else {
        Envelope::Channel {
            workspace: msg.guild_id.clone().unwrap_or_default(),
            channel_id: msg.channel_id.clone(),
        }
    };

    Some(InboundMessage {
        id: msg.id.clone(),
        channel: "discord".to_string(),
        sender: Address {
            id: msg.author.id.clone(),
            display_name: Some(msg.author.username.clone()),
        },
        timestamp: Utc::now(),
        envelope,
        body: vec![BodyPart::Text {
            content: clean_content,
        }],
        thread_id: None, // Discord threads would need GUILD_MESSAGE_TYPING intent
        reply_to: msg
            .message_reference
            .as_ref()
            .and_then(|r| r.message_id.clone()),
        reaction: None,
        hints: ChannelHints::None,
        trust_level: classify_trust(config, &msg.author.id, msg.member.as_ref()),
        metadata: serde_json::json!({
            "discord_message_id": msg.id,
            "discord_channel_id": msg.channel_id,
            "discord_guild_id": msg.guild_id,
            "discord_author_id": msg.author.id,
            "is_dm": is_dm,
        }),
    })
}

// Needed for URL-encoding emoji in reaction endpoint
mod urlencoding {
    pub fn encode(s: &str) -> String {
        let mut result = String::new();
        for c in s.chars() {
            match c {
                'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => result.push(c),
                _ => {
                    for b in c.to_string().as_bytes() {
                        result.push_str(&format!("%{:02X}", b));
                    }
                }
            }
        }
        result
    }
}

#[async_trait]
impl Connector for DiscordConnector {
    fn name(&self) -> &str {
        "discord"
    }

    fn status(&self) -> ConnectorStatus {
        match self.state.try_lock() {
            Ok(s) => s.status,
            Err(_) => ConnectorStatus::Connected,
        }
    }

    fn capabilities(&self) -> ConnectorCapabilities {
        ConnectorCapabilities {
            send: true,
            receive: true,
            threads: false, // TODO: Discord threads support
            reactions: true,
            attachments: false, // TODO: file uploads
            read_receipts: false,
            rich_text: true, // Discord markdown
            typing_indicators: false,
            disappearing_messages: false,
            groups: false,
            push: false, // uses poll() backed by gateway inbox
        }
    }

    async fn send(&self, message: OutboundMessage) -> Result<MessageId> {
        let channel_id = match &message.envelope {
            Envelope::Channel { channel_id, .. } => channel_id.clone(),
            Envelope::Direct { to } if !to.is_empty() => to[0].id.clone(),
            _ => return Err(Error::SendFailed("no target channel in envelope".into())),
        };

        // Handle reactions
        if let Some(ref reaction) = message.reaction {
            self.add_reaction(&channel_id, &reaction.target, &reaction.emoji)
                .await?;
            return Ok(reaction.target.clone());
        }

        // Extract text
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

        // Discord message limit is 2000 characters — truncate at char boundary
        let text = if text.len() > 2000 {
            tracing::warn!(
                len = text.len(),
                "truncating message to Discord 2k char limit"
            );
            let boundary = text
                .char_indices()
                .take_while(|(i, _)| *i <= 1980)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(0);
            format!("{}... [truncated]", &text[..boundary])
        } else {
            text
        };

        self.send_message(&channel_id, &text, message.reply_to.as_deref())
            .await
    }

    async fn poll(&self) -> Result<Vec<InboundMessage>> {
        let mut s = self.state.lock().await;
        let messages: Vec<InboundMessage> = s.inbox.drain(..).collect();
        Ok(messages)
    }
}

#[async_trait]
impl ConnectorFactory for DiscordConnector {
    async fn build(
        config: &serde_json::Value,
        secrets: &SecretStore,
    ) -> Result<Box<dyn Connector>> {
        let cfg: DiscordConfig =
            serde_json::from_value(config.clone()).map_err(|e| Error::Other(e.into()))?;
        let connector = DiscordConnector::new(cfg, secrets);
        Ok(Box::new(connector))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discord_config() -> DiscordConfig {
        DiscordConfig {
            guild_id: None,
            require_mention: true,
            allowed_users: vec![],
            operators: vec![],
            operator_roles: vec![],
            bot_token_file: None,
        }
    }

    fn make_msg(author_id: &str, content: &str, guild: Option<&str>) -> DiscordMessage {
        DiscordMessage {
            id: "msg1".into(),
            channel_id: "ch1".into(),
            guild_id: guild.map(|s| s.to_string()),
            author: DiscordUser {
                id: author_id.into(),
                username: "testuser".into(),
                bot: false,
            },
            content: content.into(),
            timestamp: String::new(),
            message_reference: None,
            member: None,
        }
    }

    #[test]
    fn parse_dm() {
        let msg = make_msg("U1", "hello", None);
        let result = parse_discord_message(&msg, &discord_config(), Some("BOT1"));
        assert!(result.is_some());
        assert_eq!(
            result.unwrap().body[0],
            BodyPart::Text {
                content: "hello".into()
            }
        );
    }

    #[test]
    fn guild_mention_required() {
        let msg = make_msg("U1", "hello no mention", Some("G1"));
        let result = parse_discord_message(&msg, &discord_config(), Some("BOT1"));
        assert!(result.is_none());
    }

    #[test]
    fn guild_mention_passes() {
        let msg = make_msg("U1", "<@BOT1> hello", Some("G1"));
        let result = parse_discord_message(&msg, &discord_config(), Some("BOT1"));
        assert!(result.is_some());
        assert_eq!(
            result.unwrap().body[0],
            BodyPart::Text {
                content: "hello".into()
            }
        );
    }

    #[test]
    fn skip_bot_author() {
        let mut msg = make_msg("U1", "<@BOT1> hello", Some("G1"));
        msg.author.bot = true;
        let result = parse_discord_message(&msg, &discord_config(), Some("BOT1"));
        assert!(result.is_none());
    }

    #[test]
    fn guild_filter() {
        let config = DiscordConfig {
            guild_id: Some("G2".into()),
            ..discord_config()
        };
        let msg = make_msg("U1", "<@BOT1> hello", Some("G1"));
        assert!(parse_discord_message(&msg, &config, Some("BOT1")).is_none());

        let msg2 = make_msg("U1", "<@BOT1> hello", Some("G2"));
        assert!(parse_discord_message(&msg2, &config, Some("BOT1")).is_some());
    }

    #[test]
    fn allowlist_blocks() {
        let config = DiscordConfig {
            allowed_users: vec!["U999".into()],
            ..discord_config()
        };
        let msg = make_msg("U1", "<@BOT1> hello", Some("G1"));
        assert!(parse_discord_message(&msg, &config, Some("BOT1")).is_none());
    }

    #[test]
    fn allowlist_permits() {
        let config = DiscordConfig {
            allowed_users: vec!["U1".into()],
            ..discord_config()
        };
        let msg = make_msg("U1", "<@BOT1> hello", Some("G1"));
        assert!(parse_discord_message(&msg, &config, Some("BOT1")).is_some());
    }

    #[test]
    fn empty_after_mention_strip_skipped() {
        let msg = make_msg("U1", "<@BOT1>", Some("G1"));
        assert!(parse_discord_message(&msg, &discord_config(), Some("BOT1")).is_none());
    }

    #[test]
    fn trust_default_is_user() {
        assert_eq!(
            classify_trust(&discord_config(), "U1", None),
            vox_core::TrustLevel::User
        );
    }

    #[test]
    fn trust_explicit_operator_user_id() {
        let config = DiscordConfig {
            operators: vec!["OP1".into()],
            ..discord_config()
        };
        assert_eq!(
            classify_trust(&config, "OP1", None),
            vox_core::TrustLevel::Operator
        );
        assert_eq!(
            classify_trust(&config, "U1", None),
            vox_core::TrustLevel::User
        );
    }

    #[test]
    fn trust_operator_role() {
        let config = DiscordConfig {
            operator_roles: vec!["ROLE_ADMIN".into()],
            ..discord_config()
        };
        let member_with_role = GuildMember {
            roles: vec!["ROLE_ADMIN".into(), "ROLE_OTHER".into()],
        };
        let member_without_role = GuildMember {
            roles: vec!["ROLE_OTHER".into()],
        };

        assert_eq!(
            classify_trust(&config, "U1", Some(&member_with_role)),
            vox_core::TrustLevel::Operator
        );
        assert_eq!(
            classify_trust(&config, "U1", Some(&member_without_role)),
            vox_core::TrustLevel::User
        );
        // DMs have no member — role check can't apply
        assert_eq!(
            classify_trust(&config, "U1", None),
            vox_core::TrustLevel::User
        );
    }

    #[test]
    fn trust_user_id_overrides_missing_role() {
        let config = DiscordConfig {
            operators: vec!["OP1".into()],
            operator_roles: vec!["ROLE_ADMIN".into()],
            ..discord_config()
        };
        // User ID match — operator even without roles (DM case)
        assert_eq!(
            classify_trust(&config, "OP1", None),
            vox_core::TrustLevel::Operator
        );
    }
}
