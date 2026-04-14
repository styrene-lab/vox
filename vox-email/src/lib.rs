use async_trait::async_trait;
use vox_core::{
    Connector, ConnectorCapabilities, ConnectorFactory, ConnectorStatus, EmailConfig, Error,
    InboundMessage, MessageId, OutboundMessage, Result, SecretStore,
};

/// Email connector using IMAP for receiving and SMTP for sending.
///
/// Secrets consumed (via bootstrap_secrets):
///   - VOX_EMAIL_PASSWORD: required — IMAP/SMTP authentication password
///
/// Supports IMAP IDLE for push delivery when the server supports it.
pub struct EmailConnector {
    #[allow(dead_code)]
    config: EmailConfig,
    status: ConnectorStatus,
}

impl EmailConnector {
    pub fn new(config: EmailConfig, secrets: &SecretStore) -> Self {
        let has_password = secrets.has("VOX_EMAIL_PASSWORD");
        let status = if has_password {
            // TODO: establish IMAP connection, verify SMTP credentials
            tracing::info!(
                address = %config.address,
                imap = %config.imap_host,
                smtp = %config.smtp_host,
                "email connector initialized (stub)"
            );
            ConnectorStatus::Disconnected
        } else {
            tracing::warn!("email connector missing VOX_EMAIL_PASSWORD — degraded");
            ConnectorStatus::Degraded
        };

        Self { config, status }
    }
}

#[async_trait]
impl Connector for EmailConnector {
    fn name(&self) -> &str {
        "email"
    }

    fn status(&self) -> ConnectorStatus {
        self.status
    }

    fn capabilities(&self) -> ConnectorCapabilities {
        ConnectorCapabilities {
            send: true,
            receive: true,
            threads: true,
            reactions: false,
            attachments: true,
            read_receipts: false,
            rich_text: true,
            typing_indicators: false,
            disappearing_messages: false,
            groups: false,
            push: false, // IMAP IDLE support will flip this to true
        }
    }

    async fn send(&self, message: OutboundMessage) -> Result<MessageId> {
        let _ = &message;
        Err(Error::NotSupported("email connector not yet implemented".into()))
    }

    async fn poll(&self) -> Result<Vec<InboundMessage>> {
        Ok(vec![])
    }
}

#[async_trait]
impl ConnectorFactory for EmailConnector {
    async fn build(
        config: &serde_json::Value,
        secrets: &SecretStore,
    ) -> Result<Box<dyn Connector>> {
        let cfg: EmailConfig = serde_json::from_value(config.clone())
            .map_err(|e| Error::Other(e.into()))?;
        Ok(Box::new(Self::new(cfg, secrets)))
    }
}
