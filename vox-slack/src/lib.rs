use std::collections::{HashMap, HashSet, VecDeque};
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
    SecretStore, SlackConfig, SlackMode, SlackPosture,
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
    // ── Proxy mode state ────────────────────────────────────────────
    /// Authenticated user ID for proxy mode (resolved via auth.test).
    proxy_user_id: Option<String>,
    /// Per-channel "last seen" timestamp for proxy polling.
    channel_cursors: HashMap<String, String>,
    /// Channels being watched (explicit or auto-discovered).
    watched_channels: Vec<String>,
}

/// Slack connector supporting two operating modes:
///
/// **Bot mode** (default): Socket Mode WebSocket for receiving, Web API for sending.
///   - VOX_SLACK_BOT_TOKEN: xoxb-* Bot User OAuth Token
///   - VOX_SLACK_APP_TOKEN: xapp-* App-Level Token
///
/// **Proxy mode**: Polls conversations.history with a user token. Reads the
/// operator's channels and surfaces messages to the agent.
///   - VOX_SLACK_USER_TOKEN: xoxp-* User OAuth Token
pub struct SlackConnector {
    config: SlackConfig,
    bot_token: Option<String>,
    app_token: Option<String>,
    user_token: Option<String>,
    http: Client,
    state: Arc<Mutex<SlackState>>,
    notify: Arc<Notify>,
}

impl SlackConnector {
    pub fn new(config: SlackConfig, secrets: &SecretStore) -> Self {
        let bot_token = match secrets
            .get_or_file("VOX_SLACK_BOT_TOKEN", config.oauth_token_file.as_deref())
        {
            Ok(token) => token,
            Err(e) => {
                tracing::warn!(error = %e, "slack bot connector degraded — failed to read bot token file");
                None
            }
        };
        let app_token = match secrets
            .get_or_file("VOX_SLACK_APP_TOKEN", config.socket_token_file.as_deref())
        {
            Ok(token) => token,
            Err(e) => {
                tracing::warn!(error = %e, "slack bot connector degraded — failed to read app token file");
                None
            }
        };
        let user_token = match secrets
            .get_or_file("VOX_SLACK_USER_TOKEN", config.user_token_file.as_deref())
        {
            Ok(token) => token,
            Err(e) => {
                tracing::warn!(error = %e, "slack proxy connector degraded — failed to read user token file");
                None
            }
        };

        let status = match config.mode {
            SlackMode::Bot => {
                if bot_token.is_some() && app_token.is_some() {
                    tracing::info!(
                        workspace = %config.workspace,
                        require_mention = config.require_mention,
                        "slack bot connector initialized"
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
                    tracing::warn!(?missing, "slack bot connector degraded — missing secrets");
                    ConnectorStatus::Degraded
                }
            }
            SlackMode::Proxy => {
                if user_token.is_some() {
                    tracing::info!(
                        workspace = %config.workspace,
                        posture = ?config.posture,
                        "slack proxy connector initialized"
                    );
                    ConnectorStatus::Initializing
                } else {
                    tracing::warn!("slack proxy connector degraded — missing VOX_SLACK_USER_TOKEN");
                    ConnectorStatus::Degraded
                }
            }
        };

        Self {
            config,
            bot_token,
            app_token,
            user_token,
            http: Client::new(),
            state: Arc::new(Mutex::new(SlackState {
                inbox: VecDeque::new(),
                status,
                bot_user_id: None,
                operator_user_ids: HashSet::new(),
                proxy_user_id: None,
                channel_cursors: HashMap::new(),
                watched_channels: Vec::new(),
            })),
            notify: Arc::new(Notify::new()),
        }
    }

    pub async fn start(&self) -> Result<()> {
        match self.config.mode {
            SlackMode::Bot => self.start_bot_mode().await,
            SlackMode::Proxy => self.start_proxy_mode().await,
        }
    }

    /// Bot mode: Socket Mode WebSocket listener.
    async fn start_bot_mode(&self) -> Result<()> {
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
            let mut op_ids: HashSet<String> = self.config.operators.iter().cloned().collect();

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

        tokio::spawn(async move {
            socket_mode_loop(http, app_token, state, notify, config, bot_user_id).await;
        });

        {
            let mut s = self.state.lock().await;
            s.status = ConnectorStatus::Connected;
        }

        tracing::info!(workspace = %self.config.workspace, "slack socket mode connected");
        Ok(())
    }

    /// Proxy mode: poll conversations.history with user token.
    async fn start_proxy_mode(&self) -> Result<()> {
        let Some(ref user_token) = self.user_token else {
            return Err(Error::SendFailed(
                "cannot start proxy mode without VOX_SLACK_USER_TOKEN".into(),
            ));
        };

        // Resolve authenticated user ID
        let proxy_user_id = resolve_user_identity(&self.http, user_token).await?;
        tracing::info!(user_id = %proxy_user_id, "resolved proxy user identity");

        // Discover channels to watch
        let channels = if self.config.watch_channels.is_empty() {
            let discovered = discover_user_channels(&self.http, user_token).await?;
            tracing::info!(count = discovered.len(), "auto-discovered user channels");
            discovered
        } else {
            self.config.watch_channels.clone()
        };

        // Warn about rate limits
        let polls_per_min = channels.len() as u64 * 60 / self.config.proxy_poll_secs.max(1);
        if polls_per_min > 45 {
            tracing::warn!(
                channels = channels.len(),
                poll_secs = self.config.proxy_poll_secs,
                requests_per_min = polls_per_min,
                "proxy poll rate may exceed Slack API limits — consider increasing proxy_poll_secs"
            );
        }

        // Initialize cursors to "now" — no history replay
        let now_ts = format!("{}", chrono::Utc::now().timestamp());
        let cursors: HashMap<String, String> = channels
            .iter()
            .map(|ch| (ch.clone(), now_ts.clone()))
            .collect();

        {
            let mut s = self.state.lock().await;
            s.proxy_user_id = Some(proxy_user_id.clone());
            s.watched_channels = channels;
            s.channel_cursors = cursors;
            s.status = ConnectorStatus::Connected;
        }

        let state = self.state.clone();
        let notify = self.notify.clone();
        let config = self.config.clone();
        let http = self.http.clone();
        let user_token = user_token.clone();

        tokio::spawn(async move {
            proxy_poll_loop(http, user_token, state, notify, config, proxy_user_id).await;
        });

        tracing::info!(workspace = %self.config.workspace, "slack proxy mode started");
        Ok(())
    }

    /// Return the active API token for the current mode.
    fn active_token(&self) -> Option<&str> {
        match self.config.mode {
            SlackMode::Bot => self.bot_token.as_deref(),
            SlackMode::Proxy => self.user_token.as_deref(),
        }
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
        let token = self
            .active_token()
            .ok_or_else(|| Error::SendFailed("no API token available".into()))?;

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
            .bearer_auth(token)
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
        let token = self
            .active_token()
            .ok_or_else(|| Error::SendFailed("no API token available".into()))?;

        let body = serde_json::json!({
            "channel": channel,
            "timestamp": timestamp,
            "name": emoji.trim_matches(':'),
        });

        let resp = self
            .http
            .post("https://slack.com/api/reactions.add")
            .bearer_auth(token)
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
                            tracing::warn!(
                                "slack inbox full ({MAX_INBOX_SIZE}), dropping oldest message"
                            );
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

// ---------------------------------------------------------------------------
// Proxy mode helpers
// ---------------------------------------------------------------------------

/// Resolve the authenticated user's ID from a user token via auth.test.
async fn resolve_user_identity(http: &Client, token: &str) -> Result<String> {
    let resp = http
        .get("https://slack.com/api/auth.test")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| Error::SendFailed(format!("auth.test failed: {e}")))?;

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| Error::SendFailed(format!("auth.test parse failed: {e}")))?;

    if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let err = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        return Err(Error::SendFailed(format!("auth.test error: {err}")));
    }

    body.get("user_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::SendFailed("auth.test response missing user_id".into()))
}

/// Discover all channels the authenticated user is a member of.
async fn discover_user_channels(http: &Client, token: &str) -> Result<Vec<String>> {
    let mut channels = Vec::new();
    let mut cursor = String::new();

    loop {
        let mut req = http
            .get("https://slack.com/api/conversations.list")
            .bearer_auth(token)
            .query(&[
                ("types", "public_channel,private_channel,im,mpim"),
                ("exclude_archived", "true"),
                ("limit", "200"),
            ]);

        if !cursor.is_empty() {
            req = req.query(&[("cursor", cursor.as_str())]);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| Error::SendFailed(format!("conversations.list failed: {e}")))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| Error::SendFailed(format!("conversations.list parse failed: {e}")))?;

        if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let err = body
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            return Err(Error::SendFailed(format!(
                "conversations.list error: {err}"
            )));
        }

        if let Some(arr) = body.get("channels").and_then(|v| v.as_array()) {
            for ch in arr {
                if let Some(id) = ch.get("id").and_then(|v| v.as_str()) {
                    channels.push(id.to_string());
                }
            }
        }

        // Paginate
        let next = body
            .pointer("/response_metadata/next_cursor")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if next.is_empty() {
            break;
        }
        cursor = next.to_string();
    }

    Ok(channels)
}

/// Fetch messages from a channel newer than `oldest` timestamp.
async fn fetch_channel_history(
    http: &Client,
    token: &str,
    channel: &str,
    oldest: &str,
) -> std::result::Result<Vec<serde_json::Value>, String> {
    let resp = http
        .get("https://slack.com/api/conversations.history")
        .bearer_auth(token)
        .query(&[("channel", channel), ("oldest", oldest), ("limit", "100")])
        .send()
        .await
        .map_err(|e| format!("conversations.history failed: {e}"))?;

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("conversations.history parse failed: {e}"))?;

    if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let err = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        return Err(format!("conversations.history error: {err}"));
    }

    Ok(body
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default())
}

/// Background loop for proxy mode: poll channels for new messages.
async fn proxy_poll_loop(
    http: Client,
    user_token: String,
    state: Arc<Mutex<SlackState>>,
    notify: Arc<Notify>,
    config: SlackConfig,
    proxy_user_id: String,
) {
    let poll_interval = std::time::Duration::from_secs(config.proxy_poll_secs.max(5));
    let refresh_interval = std::time::Duration::from_secs(config.channel_refresh_secs);
    let auto_discover = config.watch_channels.is_empty();
    let mut last_refresh = std::time::Instant::now();

    loop {
        tokio::time::sleep(poll_interval).await;

        // Periodic channel rediscovery
        if auto_discover && last_refresh.elapsed() >= refresh_interval {
            match discover_user_channels(&http, &user_token).await {
                Ok(channels) => {
                    let mut s = state.lock().await;
                    // Initialize cursors for any new channels
                    let now_ts = format!("{}", chrono::Utc::now().timestamp());
                    for ch in &channels {
                        s.channel_cursors
                            .entry(ch.clone())
                            .or_insert_with(|| now_ts.clone());
                    }
                    s.watched_channels = channels;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "proxy channel rediscovery failed");
                }
            }
            last_refresh = std::time::Instant::now();
        }

        // Snapshot channels and cursors
        let (channels, mut cursors) = {
            let s = state.lock().await;
            (s.watched_channels.clone(), s.channel_cursors.clone())
        };

        let mut new_messages = Vec::new();

        for channel_id in &channels {
            let oldest = cursors.get(channel_id).map(|s| s.as_str()).unwrap_or("0");

            match fetch_channel_history(&http, &user_token, channel_id, oldest).await {
                Ok(messages) => {
                    // conversations.history returns newest-first; reverse for chronological order
                    for msg in messages.iter().rev() {
                        if let Some(inbound) =
                            parse_proxy_message(msg, &config, &proxy_user_id, channel_id)
                        {
                            new_messages.push(inbound);
                        }
                        // Update cursor to this message's ts (newest wins)
                        if let Some(ts) = msg.get("ts").and_then(|v| v.as_str()) {
                            cursors.insert(channel_id.clone(), ts.to_string());
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!(channel = %channel_id, error = %e, "proxy poll failed for channel");
                }
            }

            // Brief pause between channels to avoid rate limit bursts
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // Push to inbox
        if !new_messages.is_empty() {
            let mut s = state.lock().await;
            s.channel_cursors = cursors;
            for msg in new_messages {
                if s.inbox.len() >= MAX_INBOX_SIZE {
                    tracing::warn!("slack proxy inbox full ({MAX_INBOX_SIZE}), dropping oldest");
                    s.inbox.pop_front();
                }
                s.inbox.push_back(msg);
            }
            drop(s);
            notify.notify_one();
        } else {
            // Still update cursors even if no new messages
            let mut s = state.lock().await;
            s.channel_cursors = cursors;
        }
    }
}

/// Parse a message from conversations.history into an InboundMessage for proxy mode.
/// No mention filtering — all messages from other users are surfaced.
/// All messages are TrustLevel::User (external data to the operator).
fn parse_proxy_message(
    msg: &serde_json::Value,
    config: &SlackConfig,
    proxy_user_id: &str,
    channel_id: &str,
) -> Option<InboundMessage> {
    // Skip subtypes (joins, edits, etc.) and bot messages
    if msg.get("subtype").is_some() || msg.get("bot_id").is_some() {
        return None;
    }

    let user = msg.get("user")?.as_str()?;
    let text = msg.get("text")?.as_str()?;
    let ts = msg.get("ts")?.as_str()?;
    let thread_ts = msg.get("thread_ts").and_then(|v| v.as_str());

    // Filter out the operator's own messages
    if user == proxy_user_id {
        return None;
    }

    if text.is_empty() {
        return None;
    }

    Some(InboundMessage {
        id: ts.to_string(),
        channel: "slack".to_string(),
        sender: Address {
            id: user.to_string(),
            display_name: None,
        },
        timestamp: chrono::Utc::now(),
        envelope: Envelope::Channel {
            workspace: config.workspace.clone(),
            channel_id: channel_id.to_string(),
        },
        body: vec![BodyPart::Text {
            content: text.to_string(),
        }],
        thread_id: thread_ts.map(|s| s.to_string()),
        reply_to: None,
        reaction: None,
        hints: ChannelHints::Slack {
            username: None,
            icon_emoji: None,
            unfurl: false,
        },
        trust_level: vox_core::TrustLevel::User,
        metadata: serde_json::json!({
            "slack_ts": ts,
            "slack_channel": channel_id,
            "slack_user": user,
            "proxy_mode": true,
        }),
    })
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
        let is_proxy = self.config.mode == SlackMode::Proxy;
        ConnectorCapabilities {
            send: if is_proxy {
                self.config.posture == SlackPosture::Participate
            } else {
                true
            },
            receive: true,
            threads: true,
            reactions: !is_proxy,
            attachments: false,
            read_receipts: false,
            rich_text: true,
            typing_indicators: false,
            disappearing_messages: false,
            groups: false,
            push: false, // uses poll() backed by internal inbox from socket mode
        }
    }

    async fn send(&self, message: OutboundMessage) -> Result<MessageId> {
        if self.config.mode == SlackMode::Proxy && self.config.posture == SlackPosture::Observe {
            return Err(Error::SendFailed(
                "proxy mode is in observe posture — sending is disabled".into(),
            ));
        }

        // Determine target channel
        let channel_id =
            match &message.envelope {
                Envelope::Channel { channel_id, .. } => channel_id.clone(),
                Envelope::Direct { to } if !to.is_empty() => {
                    // For DMs, the "to" id is the channel ID (Slack DM channel)
                    to[0].id.clone()
                }
                _ => self.config.default_channel.clone().ok_or_else(|| {
                    Error::SendFailed("no target channel and no default set".into())
                })?,
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
            tracing::warn!(
                len = text.len(),
                "truncating message to Slack 40k char limit"
            );
            let boundary = text
                .char_indices()
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
            mode: SlackMode::Bot,
            default_channel: None,
            require_mention: true,
            allowed_users: vec![],
            operators: vec![],
            operator_groups: vec![],
            posture: SlackPosture::Observe,
            watch_channels: vec![],
            proxy_poll_secs: 30,
            channel_refresh_secs: 300,
            oauth_token_file: None,
            socket_token_file: None,
            user_token_file: None,
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
        assert_eq!(
            msg.body[0],
            BodyPart::Text {
                content: "hello".into()
            }
        );
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

    // ── Proxy mode tests ───────────────────────────────────────────

    fn proxy_config() -> SlackConfig {
        SlackConfig {
            mode: SlackMode::Proxy,
            posture: SlackPosture::Observe,
            ..slack_config()
        }
    }

    fn history_message(user: &str, text: &str, channel: &str, ts: &str) -> serde_json::Value {
        serde_json::json!({
            "user": user,
            "text": text,
            "channel": channel,
            "ts": ts,
        })
    }

    #[test]
    fn proxy_skips_own_messages() {
        let msg = history_message("OPERATOR1", "my own message", "C456", "1234.5678");
        assert!(parse_proxy_message(&msg, &proxy_config(), "OPERATOR1", "C456").is_none());
    }

    #[test]
    fn proxy_passes_other_users() {
        let msg = history_message("U999", "hello from someone else", "C456", "1234.5678");
        let inbound = parse_proxy_message(&msg, &proxy_config(), "OPERATOR1", "C456");
        assert!(inbound.is_some());
        let inbound = inbound.unwrap();
        assert_eq!(inbound.sender.id, "U999");
    }

    #[test]
    fn proxy_skips_bot_messages() {
        let mut msg = history_message("UBOT", "bot says hi", "C456", "1234.5678");
        msg["bot_id"] = serde_json::json!("B123");
        assert!(parse_proxy_message(&msg, &proxy_config(), "OPERATOR1", "C456").is_none());
    }

    #[test]
    fn proxy_skips_subtypes() {
        let mut msg = history_message("U999", "joined", "C456", "1234.5678");
        msg["subtype"] = serde_json::json!("channel_join");
        assert!(parse_proxy_message(&msg, &proxy_config(), "OPERATOR1", "C456").is_none());
    }

    #[test]
    fn proxy_all_messages_are_user_trust() {
        let msg = history_message("U999", "some message", "C456", "1234.5678");
        let inbound = parse_proxy_message(&msg, &proxy_config(), "OPERATOR1", "C456").unwrap();
        assert_eq!(inbound.trust_level, vox_core::TrustLevel::User);
    }

    #[test]
    fn proxy_no_mention_filtering() {
        // In proxy mode, messages without any mention are still processed
        let msg = history_message("U999", "no mention here", "C456", "1234.5678");
        assert!(parse_proxy_message(&msg, &proxy_config(), "OPERATOR1", "C456").is_some());
    }

    #[test]
    fn proxy_preserves_thread_ts() {
        let mut msg = history_message("U999", "thread reply", "C456", "1234.5678");
        msg["thread_ts"] = serde_json::json!("1111.2222");
        let inbound = parse_proxy_message(&msg, &proxy_config(), "OPERATOR1", "C456").unwrap();
        assert_eq!(inbound.thread_id, Some("1111.2222".into()));
    }

    #[test]
    fn proxy_text_not_stripped() {
        // Mentions in text should be preserved as-is (no bot mention stripping)
        let msg = history_message("U999", "hey <@U123> check this", "C456", "1234.5678");
        let inbound = parse_proxy_message(&msg, &proxy_config(), "OPERATOR1", "C456").unwrap();
        assert_eq!(
            inbound.body[0],
            BodyPart::Text {
                content: "hey <@U123> check this".into()
            }
        );
    }

    #[test]
    fn proxy_metadata_includes_proxy_flag() {
        let msg = history_message("U999", "hello", "C456", "1234.5678");
        let inbound = parse_proxy_message(&msg, &proxy_config(), "OPERATOR1", "C456").unwrap();
        assert_eq!(inbound.metadata["proxy_mode"], true);
    }

    #[test]
    fn observe_posture_disables_send() {
        let config = SlackConfig {
            mode: SlackMode::Proxy,
            posture: SlackPosture::Observe,
            ..slack_config()
        };
        let connector = SlackConnector::new(config, &vox_core::SecretStore::new());
        assert!(!connector.capabilities().send);
    }

    #[test]
    fn participate_posture_enables_send() {
        let config = SlackConfig {
            mode: SlackMode::Proxy,
            posture: SlackPosture::Participate,
            ..slack_config()
        };
        let connector = SlackConnector::new(config, &vox_core::SecretStore::new());
        assert!(connector.capabilities().send);
    }

    #[test]
    fn default_mode_is_bot() {
        let toml = r#"workspace = "test""#;
        let config: SlackConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.mode, SlackMode::Bot);
    }

    #[test]
    fn proxy_mode_parses() {
        let toml = r#"
workspace = "test"
mode = "proxy"
posture = "participate"
"#;
        let config: SlackConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.mode, SlackMode::Proxy);
        assert_eq!(config.posture, SlackPosture::Participate);
    }

    #[test]
    fn existing_config_unchanged() {
        // A config with no mode field deserializes with Bot defaults
        let toml = r#"
workspace = "test"
require_mention = false
operators = ["U1"]
"#;
        let config: SlackConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.mode, SlackMode::Bot);
        assert!(!config.require_mention);
        assert_eq!(config.operators, vec!["U1".to_string()]);
        assert_eq!(config.posture, SlackPosture::Observe);
        assert!(config.watch_channels.is_empty());
    }
}
