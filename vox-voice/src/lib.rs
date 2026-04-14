use async_trait::async_trait;
use vox_core::{
    Connector, ConnectorCapabilities, ConnectorFactory, ConnectorStatus, Error, InboundMessage,
    MessageId, OutboundMessage, Result, SecretStore, VoiceConfig,
};

/// Voice I/O connector — local STT (whisper/vosk) and TTS (piper/sherpa-onnx).
///
/// From the agent's perspective, voice is just another channel: inbound messages
/// are transcribed text, outbound messages are synthesized to audio. The agent
/// doesn't know it's talking to a microphone.
///
/// Secrets consumed (via bootstrap_secrets): none required.
/// All models are local files referenced via config paths — no API keys needed.
/// This connector is fully airgap-compatible.
pub struct VoiceConnector {
    #[allow(dead_code)]
    config: VoiceConfig,
    status: ConnectorStatus,
}

impl VoiceConnector {
    pub fn new(config: VoiceConfig) -> Self {
        // Validate model paths exist at init time
        let stt_ok = std::path::Path::new(&config.stt_model_path).exists();
        let tts_ok = std::path::Path::new(&config.tts_model_path).exists();

        let status = if stt_ok && tts_ok {
            tracing::info!(
                stt_engine = %config.stt_engine,
                stt_model = %config.stt_model_path,
                tts_engine = %config.tts_engine,
                tts_model = %config.tts_model_path,
                "voice connector initialized (stub)"
            );
            ConnectorStatus::Disconnected
        } else {
            if !stt_ok {
                tracing::warn!(path = %config.stt_model_path, "STT model not found");
            }
            if !tts_ok {
                tracing::warn!(path = %config.tts_model_path, "TTS model not found");
            }
            ConnectorStatus::Degraded
        };

        Self { config, status }
    }
}

#[async_trait]
impl Connector for VoiceConnector {
    fn name(&self) -> &str {
        "voice"
    }

    fn status(&self) -> ConnectorStatus {
        self.status
    }

    fn capabilities(&self) -> ConnectorCapabilities {
        ConnectorCapabilities {
            send: true,       // TTS playback
            receive: true,    // STT capture
            threads: false,
            reactions: false,
            attachments: false,
            read_receipts: false,
            rich_text: false,
            typing_indicators: false,
            disappearing_messages: false,
            groups: false,
            push: true, // continuous listening via stream()
        }
    }

    async fn send(&self, message: OutboundMessage) -> Result<MessageId> {
        // TODO: extract text from body parts, run TTS, play audio
        let _ = &message;
        Err(Error::NotSupported("voice connector not yet implemented".into()))
    }

    async fn poll(&self) -> Result<Vec<InboundMessage>> {
        // Voice uses stream() for push delivery, not poll
        Ok(vec![])
    }

    // TODO: implement stream() — continuous audio capture → STT → InboundMessage
    // This is where the real work lives: audio device → ring buffer → VAD →
    // whisper/vosk transcription → yield InboundMessage with transcribed text.
}

#[async_trait]
impl ConnectorFactory for VoiceConnector {
    async fn build(
        config: &serde_json::Value,
        secrets: &SecretStore,
    ) -> Result<Box<dyn Connector>> {
        let _ = secrets; // voice connector has no secrets — all local models
        let cfg: VoiceConfig = serde_json::from_value(config.clone())
            .map_err(|e| Error::Other(e.into()))?;
        Ok(Box::new(Self::new(cfg)))
    }
}
