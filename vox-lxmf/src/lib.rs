use async_trait::async_trait;
use vox_core::{
    Connector, ConnectorCapabilities, ConnectorFactory, ConnectorStatus, Error, InboundMessage,
    LxmfConfig, MessageId, OutboundMessage, Result, SecretStore,
};

// Re-export styrene types used by this connector.
pub use lxmf_core;
pub use rns_core;

/// LXMF connector for Reticulum mesh networks, built on styrene-rs.
///
/// Two operation modes:
///   1. Direct — vox owns the RNS transport instance and LXMF router via
///      styrene-rns and styrene-lxmf crates (Rust-native, no Python deps)
///   2. Delegated — vox talks to a running styrened daemon via Unix socket RPC
///      (useful when styrened already manages the RNS transport)
///
/// Secrets consumed (via bootstrap_secrets):
///   - VOX_LXMF_IDENTITY: optional — base64-encoded RNS identity for this node.
///     If absent, a new identity is generated and persisted in rns_config_dir.
///
/// This connector is the mesh-native transport for airgapped deployments
/// where no internet is available. All traffic is PQC-encrypted via
/// Reticulum's built-in cryptographic primitives (X25519 + Ed25519 + AES-GCM).
pub struct LxmfConnector {
    #[allow(dead_code)]
    config: LxmfConfig,
    status: ConnectorStatus,
}

impl LxmfConnector {
    pub fn new(config: LxmfConfig, _secrets: &SecretStore) -> Self {
        if let Some(ref socket) = config.styrened_socket {
            tracing::info!(
                socket = %socket,
                display_name = %config.display_name,
                "lxmf connector initialized in delegated mode (styrened RPC)"
            );
        } else {
            tracing::info!(
                rns_config = %config.rns_config_dir,
                display_name = %config.display_name,
                "lxmf connector initialized in direct mode (styrene-rs native)"
            );
        }

        // TODO: In direct mode, initialize:
        //   1. rns_core::Identity from config or generate new
        //   2. rns_core::Transport with config.rns_config_dir
        //   3. lxmf_core::Router with display_name
        //   4. Register message handler for inbound LXMF messages
        //
        // In delegated mode, connect to styrened via Unix socket RPC
        // and register as an LXMF message consumer.

        Self {
            config,
            status: ConnectorStatus::Disconnected,
        }
    }
}

#[async_trait]
impl Connector for LxmfConnector {
    fn name(&self) -> &str {
        "lxmf"
    }

    fn status(&self) -> ConnectorStatus {
        self.status
    }

    fn capabilities(&self) -> ConnectorCapabilities {
        ConnectorCapabilities {
            send: true,
            receive: true,
            threads: false,
            reactions: false,
            attachments: true,
            read_receipts: true,
            rich_text: false,
            typing_indicators: false,
            disappearing_messages: false,
            groups: true,
            push: true,
        }
    }

    async fn send(&self, message: OutboundMessage) -> Result<MessageId> {
        // TODO: Convert OutboundMessage → lxmf_core::LxmfMessage,
        // route via Transport, return delivery receipt ID.
        let _ = &message;
        Err(Error::NotSupported("lxmf connector not yet implemented".into()))
    }

    async fn poll(&self) -> Result<Vec<InboundMessage>> {
        Ok(vec![])
    }

    // TODO: implement stream() — register as LXMF message handler,
    // yield InboundMessage for each received LXMF message.
    // In direct mode: rns transport callback → channel → stream.
    // In delegated mode: styrened RPC subscription → channel → stream.
}

#[async_trait]
impl ConnectorFactory for LxmfConnector {
    async fn build(
        config: &serde_json::Value,
        secrets: &SecretStore,
    ) -> Result<Box<dyn Connector>> {
        let cfg: LxmfConfig = serde_json::from_value(config.clone())
            .map_err(|e| Error::Other(e.into()))?;
        Ok(Box::new(Self::new(cfg, secrets)))
    }
}
