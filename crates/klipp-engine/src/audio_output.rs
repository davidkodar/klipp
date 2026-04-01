use crate::audio_decoder::AudioConsumer;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::Consumer;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tracing::{error, info};

/// Consumer side of the skim audio ring buffer, shared with the cpal callback.
pub type SkimConsumer = Arc<Mutex<Option<ringbuf::HeapCons<f32>>>>;

/// Preferred audio device name (set from preferences). None = system default.
pub(crate) static PREFERRED_DEVICE: Mutex<Option<String>> = Mutex::new(None);

/// Set the preferred audio output device name. Call before creating AudioOutput.
/// If the device isn't found at runtime, silently falls back to the system default.
pub fn set_preferred_device(name: Option<String>) {
    *PREFERRED_DEVICE.lock().unwrap() = name;
}

/// Flush all samples from a skim consumer (drains to silence).
pub fn flush_skim_consumer(skim: &SkimConsumer) {
    if let Ok(mut guard) = skim.lock() {
        if let Some(ref mut cons) = *guard {
            let mut discard = [0.0f32; 4096];
            while cons.pop_slice(&mut discard) > 0 {}
        }
    }
}

/// Replace the skim consumer contents (None disconnects, Some connects a new consumer).
pub fn set_skim_consumer(skim: &SkimConsumer, consumer: Option<ringbuf::HeapCons<f32>>) {
    if let Ok(mut guard) = skim.lock() {
        *guard = consumer;
    }
}

/// Probe the default cpal output device sample rate. Returns 48000 if unavailable.
pub fn probe_sample_rate() -> u32 {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    host.default_output_device()
        .and_then(|d| d.default_output_config().ok())
        .map(|c| c.sample_rate().0)
        .unwrap_or(48000)
}

/// Check if an audio output device is available.
pub fn probe_audio_available() -> bool {
    use cpal::traits::HostTrait;
    let host = cpal::default_host();
    host.default_output_device().is_some()
}

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
    /// Per-clip volume applied in the cpal callback (lock-free, instant response).
    clip_volume: Arc<AtomicU32>,
    /// Per-clip pan applied in the cpal callback (-1.0 left, 0.0 center, 1.0 right).
    clip_pan: Arc<AtomicU32>,
    /// Per-clip pre-fader gain applied in the cpal callback.
    clip_gain: Arc<AtomicU32>,
    /// Per-clip channel config (0=Default, 1=MonoDownmix, 2=SwapLR).
    clip_channel_config: Arc<AtomicU32>,
    /// Consumer side of the skim audio ring buffer.
    /// Mixed into the cpal callback output alongside playback audio.
    skim_consumer: SkimConsumer,
}

impl AudioOutput {
    /// Create a cpal output stream that reads from the ring buffer consumer.
    /// Pure consumer — no clock duties. The master clock is external.
    /// `skim_consumer` is shared with the app layer so it can inject skim audio.
    pub fn new(mut consumer: AudioConsumer, skim_consumer: SkimConsumer) -> Result<Self, AudioOutputError> {
        Self::new_with_device(consumer, skim_consumer, None)
    }

    /// Create audio output with an optional device name preference.
    /// Falls back silently to the system default device if the named device isn't found.
    pub fn new_with_device(mut consumer: AudioConsumer, skim_consumer: SkimConsumer, preferred_device: Option<&str>) -> Result<Self, AudioOutputError> {
        let host = cpal::default_host();

        // Try to find preferred device by name, fall back to default
        let device = if let Some(name) = preferred_device {
            use cpal::traits::HostTrait;
            host.output_devices()
                .ok()
                .and_then(|mut devices| devices.find(|d| {
                    use cpal::traits::DeviceTrait;
                    d.name().ok().as_deref() == Some(name)
                }))
                .or_else(|| {
                    tracing::warn!("Audio device '{}' not found, using default", name);
                    host.default_output_device()
                })
                .ok_or(AudioOutputError::NoDevice)?
        } else {
            host.default_output_device()
                .ok_or(AudioOutputError::NoDevice)?
        };

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
        let clip_volume = Arc::new(AtomicU32::new(f32::to_bits(1.0)));
        let clip_vol_flag = clip_volume.clone();
        let clip_pan = Arc::new(AtomicU32::new(f32::to_bits(0.0)));
        let clip_pan_flag = clip_pan.clone();
        let clip_gain = Arc::new(AtomicU32::new(f32::to_bits(1.0)));
        let clip_gain_flag = clip_gain.clone();
        let clip_channel_config = Arc::new(AtomicU32::new(0));
        let clip_channel_config_flag = clip_channel_config.clone();
        let skim_consumer_flag = skim_consumer.clone();

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
                        // Apply gain + clip volume + master volume + clip pan (lock-free, instant)
                        let master_vol = f32::from_bits(master_vol_flag.load(Ordering::Relaxed));
                        let clip_vol = f32::from_bits(clip_vol_flag.load(Ordering::Relaxed));
                        let clip_gn = f32::from_bits(clip_gain_flag.load(Ordering::Relaxed));
                        let pan = f32::from_bits(clip_pan_flag.load(Ordering::Relaxed));
                        let vol = master_vol * clip_vol * clip_gn;

                        // Balance-style pan: center = unity, hard L/R attenuates opposite
                        let pan_l = if pan <= 0.0 { 1.0 } else { 1.0 - pan };
                        let pan_r = if pan >= 0.0 { 1.0 } else { 1.0 + pan };

                        let need_vol = (vol - 1.0).abs() > 0.001;
                        let need_pan = pan.abs() > 0.001;

                        if need_vol || need_pan {
                            for pair in data[..filled].chunks_mut(2) {
                                if let Some(l) = pair.get_mut(0) {
                                    *l *= vol * pan_l;
                                }
                                if let Some(r) = pair.get_mut(1) {
                                    *r *= vol * pan_r;
                                }
                            }
                        }

                        // Apply channel config (after gain/volume/pan)
                        let ch_config = clip_channel_config_flag.load(Ordering::Relaxed);
                        if ch_config != 0 {
                            for pair in data[..filled].chunks_mut(2) {
                                if pair.len() == 2 {
                                    match ch_config {
                                        1 => { // MonoDownmix
                                            let mono = (pair[0] + pair[1]) * 0.5;
                                            pair[0] = mono;
                                            pair[1] = mono;
                                        }
                                        2 => { // SwapLR
                                            pair.swap(0, 1);
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        // NO clock update — master clock is external
                    });

                    // Mix in skim audio (independent of playback state).
                    // Uses thread-local buffer to avoid heap allocation in real-time callback.
                    if let Ok(mut skim_guard) = skim_consumer_flag.try_lock() {
                        if let Some(ref mut skim_cons) = *skim_guard {
                            thread_local! {
                                static SKIM_BUF: std::cell::RefCell<Vec<f32>> = const { std::cell::RefCell::new(Vec::new()) };
                            }
                            SKIM_BUF.with(|buf| {
                                let mut buf = buf.borrow_mut();
                                buf.resize(data.len(), 0.0);
                                let read = skim_cons.pop_slice(&mut buf[..data.len()]);
                                for i in 0..read {
                                    data[i] = (data[i] + buf[i]).clamp(-1.0, 1.0);
                                }
                            });
                        }
                    }
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
            clip_volume,
            clip_pan,
            clip_gain,
            clip_channel_config,
            skim_consumer,
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

    /// Set per-clip volume (applied instantly in cpal callback, no buffer lag).
    pub fn set_clip_volume(&self, vol: f32) {
        self.clip_volume.store(f32::to_bits(vol), Ordering::Relaxed);
    }

    /// Set per-clip pan (applied instantly in cpal callback, no buffer lag).
    /// -1.0 = hard left, 0.0 = center, 1.0 = hard right.
    pub fn set_clip_pan(&self, pan: f32) {
        self.clip_pan.store(f32::to_bits(pan), Ordering::Relaxed);
    }

    /// Set per-clip pre-fader gain (applied instantly in cpal callback, no buffer lag).
    pub fn set_clip_gain(&self, gain: f32) {
        self.clip_gain.store(f32::to_bits(gain), Ordering::Relaxed);
    }

    /// Set per-clip channel config (0=Default, 1=MonoDownmix, 2=SwapLR).
    pub fn set_channel_config(&self, config: u32) {
        self.clip_channel_config.store(config, Ordering::Relaxed);
    }

    /// Wire a skim audio consumer into the cpal callback for mixing.
    pub fn set_skim_consumer(&self, consumer: Option<ringbuf::HeapCons<f32>>) {
        if let Ok(mut guard) = self.skim_consumer.lock() {
            *guard = consumer;
        }
    }

    /// Flush (drain) the skim ring buffer — called when skim position changes
    /// or session ends, to prevent stale audio from playing.
    pub fn flush_skim_buffer(&self) {
        if let Ok(mut guard) = self.skim_consumer.lock() {
            if let Some(ref mut cons) = *guard {
                let mut discard = [0.0f32; 4096];
                while cons.pop_slice(&mut discard) > 0 {}
            }
        }
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
