use async_trait::async_trait;
use vox_core::{
    Connector, ConnectorCapabilities, ConnectorFactory, ConnectorStatus, Error, InboundMessage,
    MessageId, OutboundMessage, Result, SecretStore, SignalConfig,
};

/// Signal connector using signal-cli or presage as the backend.
///
/// Secrets consumed (via bootstrap_secrets):
///   - VOX_SIGNAL_PASSWORD: optional Signal registration password
///
/// All session state lives in `config.data_dir` — never in memory beyond
/// what's needed for the active session.
pub struct SignalConnector {
    #[allow(dead_code)]
    config: SignalConfig,
    status: ConnectorStatus,
}

impl SignalConnector {
    pub fn new(config: SignalConfig, _secrets: &SecretStore) -> Self {
        // TODO: initialize signal-cli subprocess or presage session
        // using config.data_dir and config.phone_number.
        // Secret VOX_SIGNAL_PASSWORD used for registration if present.
        tracing::info!(
            phone = %config.phone_number,
            data_dir = %config.data_dir,
            "signal connector initialized (stub)"
        );
        Self {
            config,
            status: ConnectorStatus::Disconnected,
        }
    }
}

#[async_trait]
impl Connector for SignalConnector {
    fn name(&self) -> &str {
        "signal"
    }

    fn status(&self) -> ConnectorStatus {
        self.status
    }

    fn capabilities(&self) -> ConnectorCapabilities {
        ConnectorCapabilities {
            send: true,
            receive: true,
            threads: false,
            reactions: true,
            attachments: true,
            read_receipts: true,
            rich_text: false,
            typing_indicators: true,
            disappearing_messages: true,
            groups: true,
            push: true,
        }
    }

    async fn send(&self, message: OutboundMessage) -> Result<MessageId> {
        let _ = &message;
        Err(Error::NotSupported("signal connector not yet implemented".into()))
    }

    async fn poll(&self) -> Result<Vec<InboundMessage>> {
        Ok(vec![])
    }
}

#[async_trait]
impl ConnectorFactory for SignalConnector {
    async fn build(
        config: &serde_json::Value,
        secrets: &SecretStore,
    ) -> Result<Box<dyn Connector>> {
        let cfg: SignalConfig = serde_json::from_value(config.clone())
            .map_err(|e| Error::Other(e.into()))?;
        Ok(Box::new(Self::new(cfg, secrets)))
    }
}
