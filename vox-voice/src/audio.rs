//! Audio capture and playback via cpal.
//!
//! The capture pipeline: microphone → ring buffer → VAD segmentation →
//! STT transcription → yield InboundMessage with transcribed text.
//!
//! Playback: TTS output samples → audio output device.

use std::sync::Arc;

use tokio::sync::Mutex;
use vox_core::{
    Address, BodyPart, Envelope, InboundMessage, TrustLevel, VoiceConfig,
};

use crate::stt::SttBackend;
use crate::vad::VadBackend;

/// Play audio samples to the configured output device.
pub fn play(config: &VoiceConfig, output: &crate::tts::TtsOutput) -> Result<(), String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();

    let device = if let Some(ref name) = config.output_device {
        host.output_devices()
            .map_err(|e| format!("enumerate outputs: {e}"))?
            .find(|d| d.name().map(|n| n == *name).unwrap_or(false))
            .ok_or_else(|| format!("output device not found: {name}"))?
    } else {
        host.default_output_device()
            .ok_or("no default output device")?
    };

    let sample_rate = cpal::SampleRate(output.sample_rate);
    let stream_config = cpal::StreamConfig {
        channels: 1,
        sample_rate,
        buffer_size: cpal::BufferSize::Default,
    };

    let samples = output.samples.clone();
    let pos = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let pos_clone = pos.clone();
    let done_clone = done.clone();
    let samples_len = samples.len();

    let stream = device
        .build_output_stream(
            &stream_config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                for sample in data.iter_mut() {
                    let idx = pos_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if idx < samples_len {
                        *sample = samples[idx];
                    } else {
                        *sample = 0.0;
                        done_clone.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            },
            |e| tracing::error!("audio output error: {e}"),
            None,
        )
        .map_err(|e| format!("build output stream: {e}"))?;

    stream.play().map_err(|e| format!("play stream: {e}"))?;

    // Wait for playback to finish
    while !done.load(std::sync::atomic::Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // Small tail to let the device flush
    std::thread::sleep(std::time::Duration::from_millis(50));

    Ok(())
}

/// Start audio capture on a dedicated OS thread, sending samples through a channel.
///
/// cpal::Stream is not Send (contains platform-specific raw pointers), so the
/// entire device setup and stream lifetime live on a single background thread.
/// The returned channel receives audio chunks from the callback.
fn start_capture(
    config: &VoiceConfig,
) -> Result<tokio::sync::mpsc::Receiver<Vec<f32>>, String> {
    let (sample_tx, sample_rx) = tokio::sync::mpsc::channel::<Vec<f32>>(64);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    let input_device_name = config.input_device.clone();
    let sample_rate = config.sample_rate;

    std::thread::Builder::new()
        .name("vox-audio-capture".into())
        .spawn(move || {
            use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

            let host = cpal::default_host();

            let device = if let Some(ref name) = input_device_name {
                match host.input_devices() {
                    Ok(mut devices) => {
                        match devices.find(|d| d.name().map(|n| n == *name).unwrap_or(false)) {
                            Some(d) => d,
                            None => {
                                let _ = ready_tx.send(Err(format!("input device not found: {name}")));
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("enumerate inputs: {e}")));
                        return;
                    }
                }
            } else {
                match host.default_input_device() {
                    Some(d) => d,
                    None => {
                        let _ = ready_tx.send(Err("no default input device".into()));
                        return;
                    }
                }
            };

            let device_name = device.name().unwrap_or_default();
            let stream_config = cpal::StreamConfig {
                channels: 1,
                sample_rate: cpal::SampleRate(sample_rate),
                buffer_size: cpal::BufferSize::Default,
            };

            let stream = match device.build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if sample_tx.try_send(data.to_vec()).is_err() {
                        static DROPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                        let count = DROPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        if count.is_power_of_two() {
                            tracing::warn!(total_drops = count, "audio capture channel full — dropping samples");
                        }
                    }
                },
                |e| tracing::error!("audio input error: {e}"),
                None,
            ) {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("build input stream: {e}")));
                    return;
                }
            };

            if let Err(e) = stream.play() {
                let _ = ready_tx.send(Err(format!("start capture: {e}")));
                return;
            }

            tracing::info!(device = device_name, sample_rate, "audio capture started");
            let _ = ready_tx.send(Ok(()));

            // Keep the stream alive by parking this thread indefinitely.
            // The stream handle must not be dropped or capture stops.
            let _stream = stream;
            loop {
                std::thread::park();
            }
        })
        .map_err(|e| format!("spawn capture thread: {e}"))?;

    // Wait for the background thread to report success or failure
    ready_rx
        .recv()
        .map_err(|_| "capture thread died during setup".to_string())?
        .map(|()| sample_rx)
}

/// Continuous audio capture stream: mic → VAD → STT → InboundMessage.
///
/// The stream runs forever, yielding transcribed utterances as they complete.
/// VAD segments speech from silence; once an utterance ends (speech probability
/// drops below threshold for enough consecutive chunks), the accumulated audio
/// is sent to STT for transcription.
pub fn capture_stream(
    config: &VoiceConfig,
    stt: Arc<Mutex<Box<dyn SttBackend>>>,
    vad: Arc<Mutex<Box<dyn VadBackend>>>,
) -> impl futures_core::Stream<Item = InboundMessage> + Send + '_ {
    let threshold = config.vad_threshold;
    let sample_rate = config.sample_rate;

    async_stream::stream! {
        let mut sample_rx = match start_capture(config) {
            Ok(rx) => rx,
            Err(e) => {
                tracing::error!("failed to start audio capture: {e}");
                return;
            }
        };

        let chunk_size = vad.lock().await.chunk_size();
        let mut audio_buffer: Vec<f32> = Vec::new();
        let mut utterance: Vec<f32> = Vec::new();
        let mut in_speech = false;
        // Number of consecutive non-speech chunks needed to end an utterance
        let silence_chunks_to_end: usize = (sample_rate as usize / chunk_size) / 2; // ~500ms
        let mut silence_count: usize = 0;

        while let Some(samples) = sample_rx.recv().await {
            audio_buffer.extend_from_slice(&samples);

            while audio_buffer.len() >= chunk_size {
                let chunk: Vec<f32> = audio_buffer.drain(..chunk_size).collect();

                let prob = {
                    let mut v = vad.lock().await;
                    v.detect(&chunk).unwrap_or(0.0)
                };

                if prob >= threshold {
                    silence_count = 0;
                    if !in_speech {
                        in_speech = true;
                        utterance.clear();
                        tracing::debug!("speech started");
                    }
                    utterance.extend_from_slice(&chunk);
                } else if in_speech {
                    // Still accumulate during trailing silence
                    utterance.extend_from_slice(&chunk);
                    silence_count += 1;

                    if silence_count >= silence_chunks_to_end {
                        in_speech = false;
                        silence_count = 0;

                        let duration_ms = (utterance.len() as f32 / sample_rate as f32 * 1000.0) as u64;
                        tracing::debug!(duration_ms, samples = utterance.len(), "utterance complete, transcribing");

                        // Transcribe
                        let text = {
                            let mut s = stt.lock().await;
                            s.transcribe(&utterance).await
                        };

                        match text {
                            Ok(text) if !text.is_empty() => {
                                let msg = InboundMessage {
                                    id: uuid::Uuid::new_v4().to_string(),
                                    channel: "voice".to_string(),
                                    sender: Address {
                                        id: "local".to_string(),
                                        display_name: None,
                                    },
                                    timestamp: chrono::Utc::now(),
                                    envelope: Envelope::Direct {
                                        to: vec![Address {
                                            id: "local".to_string(),
                                            display_name: None,
                                        }],
                                    },
                                    body: vec![BodyPart::Text { content: text.clone() }],
                                    thread_id: None,
                                    reply_to: None,
                                    reaction: None,
                                    hints: vox_core::ChannelHints::None,
                                    trust_level: TrustLevel::Operator,
                                    metadata: serde_json::json!({
                                        "duration_ms": duration_ms,
                                        "stt_engine": "voice",
                                    }),
                                };
                                tracing::info!(text = %text, duration_ms, "transcribed utterance");
                                yield msg;
                            }
                            Ok(_) => {
                                tracing::debug!("empty transcription, skipping");
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "transcription failed");
                            }
                        }

                        utterance.clear();
                        {
                            let mut v = vad.lock().await;
                            v.reset();
                        }
                    }
                }
            }
        }

        tracing::info!("audio capture stream ended");
    }
}
