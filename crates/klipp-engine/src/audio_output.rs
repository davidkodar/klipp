use crate::audio_decoder::AudioConsumer;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::Consumer;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{error, info};

pub struct AudioOutput {
    _stream: cpal::Stream,
    pub sample_rate: u32,
    pub channels: u16,
    paused: Arc<AtomicBool>,
    flushing: Arc<AtomicBool>,
    /// Bumped by the engine after a seek. The cpal callback detects the change
    /// and discards all buffered samples to prevent playing stale audio.
    seek_gen: Arc<AtomicU64>,
    /// When true, play ring buffer samples even while paused (for scrub chirps).
    /// Automatically cleared by the cpal callback when the ring buffer empties.
    chirp_active: Arc<AtomicBool>,
    /// Master volume applied in the cpal callback (lock-free, instant response).
    master_volume: Arc<AtomicU32>,
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
        let seek_gen = Arc::new(AtomicU64::new(0));
        let seek_gen_flag = seek_gen.clone();
        let chirp_active = Arc::new(AtomicBool::new(false));
        let chirp_flag = chirp_active.clone();
        let master_volume = Arc::new(AtomicU32::new(f32::to_bits(1.0)));
        let master_vol_flag = master_volume.clone();

        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    // Check for seek — if generation changed, drain stale buffer
                    let current_gen = seek_gen_flag.load(Ordering::Acquire);
                    // Use a thread-local to track last seen generation
                    // (cpal callbacks always run on the same thread)
                    thread_local! {
                        static LAST_GEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
                    }
                    LAST_GEN.with(|lg| {
                        if lg.get() != current_gen {
                            lg.set(current_gen);
                            // Drain all stale samples from the ring buffer
                            let mut discard = vec![0.0f32; 8192];
                            loop {
                                let n = consumer.pop_slice(&mut discard);
                                if n == 0 {
                                    break;
                                }
                            }
                            // Output silence for this callback
                            data.fill(0.0);
                            return;
                        }

                        let is_paused = paused_flag.load(Ordering::Acquire);
                        let is_chirping = chirp_flag.load(Ordering::Acquire);

                        if is_paused && is_chirping {
                            // Chirp mode: play ring buffer samples even while paused.
                            // When buffer empties, chirp is done.
                            let filled = consumer.pop_slice(data);
                            data[filled..].fill(0.0);
                            if filled == 0 {
                                chirp_flag.store(false, Ordering::Release);
                            }
                            return;
                        }
                        if is_paused {
                            // Paused — output silence, don't drain ring buffer
                            data.fill(0.0);
                            return;
                        }
                        if flushing_flag.load(Ordering::Acquire) {
                            // Flushing — drain ring buffer but output silence
                            let _ = consumer.pop_slice(data);
                            data.fill(0.0);
                            return;
                        }
                        let filled = consumer.pop_slice(data);
                        data[filled..].fill(0.0);
                        // Apply master volume (lock-free, instant response)
                        let vol = f32::from_bits(master_vol_flag.load(Ordering::Relaxed));
                        if (vol - 1.0).abs() > 0.001 {
                            for sample in data[..filled].iter_mut() {
                                *sample *= vol;
                            }
                        }
                        // NO clock update — master clock is external
                    });
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
            seek_gen,
            chirp_active,
            master_volume,
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

    /// Signal a seek happened — cpal callback will drain stale samples.
    pub fn notify_seek(&self) {
        self.seek_gen.fetch_add(1, Ordering::Release);
    }

    /// Enable chirp mode — play ring buffer samples even while paused.
    /// Automatically deactivates when the ring buffer empties.
    pub fn start_chirp(&self) {
        self.chirp_active.store(true, Ordering::Release);
    }

    /// Set master volume (applied instantly in cpal callback, no buffer lag).
    pub fn set_master_volume(&self, vol: f32) {
        self.master_volume.store(f32::to_bits(vol), Ordering::Relaxed);
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
