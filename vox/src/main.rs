use async_trait::async_trait;
use omegon_extension::Extension;
use serde_json::{json, Value};
use vox_core::{ConnectorRegistry, Error as VoxError, OutboundMessage};

struct Vox {
    registry: ConnectorRegistry,
}

impl Vox {
    fn new() -> Self {
        Self {
            registry: ConnectorRegistry::new(),
        }
    }

    fn tool_definitions(&self) -> Value {
        json!([
            {
                "name": "vox_channels",
                "description": "List all available communication channels and their status",
                "input_schema": {
                    "type": "object",
                    "properties": {},
                }
            },
            {
                "name": "vox_status",
                "description": "Get the connection status of one or all channels",
                "input_schema": {
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
                "description": "Send a message through a communication channel. Supports email (with subject, CC/BCC), Signal (with groups, disappearing messages), Slack (with channels, threads), and more.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "channel": {
                            "type": "string",
                            "description": "Target connector (e.g. signal, email, slack)"
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
                "description": "Poll for new inbound messages from one or all channels",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "channel": {
                            "type": "string",
                            "description": "Channel to poll. Omit to poll all channels."
                        }
                    },
                }
            }
        ])
    }

    async fn execute_channels(&self) -> omegon_extension::Result<Value> {
        let channels = self.registry.channels();
        Ok(serde_json::to_value(&channels).unwrap())
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
            let messages = connector.poll().await.map_err(|e| {
                omegon_extension::Error::internal_error(e.to_string())
            })?;
            Ok(serde_json::to_value(&messages).unwrap())
        } else {
            let messages = self.registry.poll_all().await;
            Ok(serde_json::to_value(&messages).unwrap())
        }
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
            "execute_vox_channels" => self.execute_channels().await,
            "execute_vox_status" => self.execute_status(&params).await,
            "execute_vox_send" => self.execute_send(&params).await,
            "execute_vox_poll" => self.execute_poll(&params).await,
            "shutdown" => Ok(json!({ "status": "ok" })),
            _ => Err(omegon_extension::Error::method_not_found(method)),
        }
    }
}

#[tokio::main]
async fn main() {
    let vox = Vox::new();
    omegon_extension::serve(vox)
        .await
        .expect("vox extension loop failed");
}
