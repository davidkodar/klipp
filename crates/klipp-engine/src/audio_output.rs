use crate::audio_decoder::AudioConsumer;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::Consumer;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{error, info};

pub struct AudioOutput {
    _stream: cpal::Stream,
    pub sample_rate: u32,
    pub channels: u16,
    paused: Arc<AtomicBool>,
    flushing: Arc<AtomicBool>,
}

impl AudioOutput {
    /// Create a cpal output stream that reads from the ring buffer consumer.
    /// Pure consumer — no clock duties. The master clock is external.
    pub fn new(mut consumer: AudioConsumer) -> Result<Self, AudioOutputError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(AudioOutputError::NoDevice)?;

        let supported_config = device
            .default_output_config()
            .map_err(|e| AudioOutputError::Config(e.to_string()))?;

        let sample_rate = supported_config.sample_rate().0;
        let channels = supported_config.channels();

        let config = cpal::StreamConfig {
            channels,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        info!(
            "AudioOutput: device={:?}, rate={sample_rate}, channels={channels}",
            device.name().unwrap_or_default()
        );

        let paused = Arc::new(AtomicBool::new(false));
        let paused_flag = paused.clone();
        let flushing = Arc::new(AtomicBool::new(false));
        let flushing_flag = flushing.clone();

        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    if paused_flag.load(Ordering::Acquire)
                        || flushing_flag.load(Ordering::Acquire)
                    {
                        // Drain ring buffer but output silence
                        let _ = consumer.pop_slice(data);
                        data.fill(0.0);
                        return;
                    }
                    let filled = consumer.pop_slice(data);
                    data[filled..].fill(0.0);
                    // NO clock update — master clock is external
                },
                move |err| {
                    error!("cpal stream error: {err}");
                },
                None,
            )
            .map_err(|e| AudioOutputError::Build(e.to_string()))?;

        stream
            .play()
            .map_err(|e| AudioOutputError::Play(e.to_string()))?;

        Ok(Self {
            _stream: stream,
            sample_rate,
            channels,
            paused,
            flushing,
        })
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::Release);
    }

    pub fn resume(&self) {
        self.flushing.store(false, Ordering::Release);
        self.paused.store(false, Ordering::Release);
    }

    /// Start flushing: drain ring buffer silently.
    pub fn start_flush(&self) {
        self.flushing.store(true, Ordering::Release);
    }
}

#[derive(Debug)]
pub enum AudioOutputError {
    NoDevice,
    Config(String),
    Build(String),
    Play(String),
}

impl std::fmt::Display for AudioOutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDevice => write!(f, "no audio output device"),
            Self::Config(e) => write!(f, "audio config: {e}"),
            Self::Build(e) => write!(f, "build stream: {e}"),
            Self::Play(e) => write!(f, "play stream: {e}"),
        }
    }
}
