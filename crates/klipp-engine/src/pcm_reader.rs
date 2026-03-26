use ringbuf::traits::{Observer, Producer, Split};
use ringbuf::HeapRb;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use tracing::{debug, info};

use crate::audio_decoder::{AudioConsumer, AudioProducer};

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: usize = 2;

/// Apply a curve shape to a linear 0..1 value.
/// 0 = Linear, 1 = Exponential, 2 = Logarithmic, 3 = S-Curve
fn apply_curve(t: f64, curve: u8) -> f64 {
    match curve {
        1 => t * t,                           // Exponential: slow start, fast end
        2 => 1.0 - (1.0 - t) * (1.0 - t),   // Logarithmic: fast start, slow end
        3 => t * t * (3.0 - 2.0 * t),        // S-Curve (smoothstep): slow-fast-slow
        _ => t,                               // Linear
    }
}

/// Compute fade envelope multiplier (0.0..1.0) for a given position in the source.
fn fade_envelope(
    pos_secs: f64, source_in: f64, clip_dur: f64,
    fade_in: f64, fade_out: f64,
    fade_in_curve: u8, fade_out_curve: u8,
) -> f32 {
    let clip_pos = pos_secs - source_in;
    let clip_end = clip_dur;

    let mut gain = 1.0;

    // Fade in: ramp 0→1 over fade_in seconds from clip start
    if fade_in > 0.001 && clip_pos < fade_in {
        let t = (clip_pos / fade_in).clamp(0.0, 1.0);
        gain = apply_curve(t, fade_in_curve);
    }

    // Fade out: ramp 1→0 over fade_out seconds to clip end
    if fade_out > 0.001 && clip_pos > clip_end - fade_out {
        let remaining = clip_end - clip_pos;
        let t = (remaining / fade_out).clamp(0.0, 1.0);
        gain *= apply_curve(t, fade_out_curve);
    }

    gain as f32
}

/// Commands for the PCM feeder thread.
enum PcmCommand {
    SeekTo(f64),
    Pause,
    Resume,
    Stop,
    /// Play a short audio chirp at the given position (scrub/frame step).
    /// Duration in seconds (typically 0.02-0.05s).
    Chirp { position_secs: f64, duration_secs: f64 },
}

/// A PCM audio feeder that reads from a pre-conformed .pcm cache file.
/// Replaces AudioDecoder for clips that have been conformed to raw PCM.
/// Seeks are O(1) — just a byte offset calculation.
pub struct PcmAudioFeeder {
    thread: Option<JoinHandle<()>>,
    cmd_tx: crossbeam_channel::Sender<PcmCommand>,
    pub volume: Arc<AtomicU32>,
    pub muted: Arc<AtomicBool>,
    pub post_seek_pts_us: Arc<AtomicU64>,
    pub seek_generation: Arc<AtomicU64>,
    pub fade_source_in: Arc<AtomicU64>,
    pub fade_clip_dur: Arc<AtomicU64>,
    pub fade_in_secs: Arc<AtomicU64>,
    pub fade_out_secs: Arc<AtomicU64>,
    pub fade_in_curve: Arc<AtomicU32>,  // 0=Linear, 1=Exp, 2=Log, 3=SCurve
    pub fade_out_curve: Arc<AtomicU32>,
}

impl PcmAudioFeeder {
    /// Open a PCM cache file and create a feeder + ring buffer consumer.
    /// The PCM file must be 48kHz stereo f32 interleaved (as produced by conform_audio).
    pub fn open(pcm_path: &Path, output_rate: u32) -> Result<(Self, AudioConsumer), PcmFeederError> {
        let pcm_data = std::fs::read(pcm_path)
            .map_err(|e| PcmFeederError::Open(format!("read {:?}: {e}", pcm_path)))?;

        if pcm_data.len() % 4 != 0 {
            return Err(PcmFeederError::Open("PCM file size not aligned to f32".into()));
        }

        let total_floats = pcm_data.len() / 4;
        let total_mono_samples = total_floats / CHANNELS;
        let duration_secs = total_mono_samples as f64 / SAMPLE_RATE as f64;

        info!(
            "PcmAudioFeeder: opened {:?}, {:.1}s, {} samples",
            pcm_path, duration_secs, total_mono_samples,
        );

        // Ring buffer: ~2s of stereo f32 at output_rate
        let buf_size = (output_rate as usize) * 2 * 2;
        let (producer, consumer) = HeapRb::<f32>::new(buf_size).split();

        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let volume = Arc::new(AtomicU32::new(100));
        let muted = Arc::new(AtomicBool::new(false));
        let post_seek_pts_us = Arc::new(AtomicU64::new(0));
        let seek_generation = Arc::new(AtomicU64::new(0));
        // Fade params: source_in, clip_duration, fade_in, fade_out (all in seconds as f64 bits)
        let fade_source_in = Arc::new(AtomicU64::new(0.0f64.to_bits()));
        let fade_clip_dur = Arc::new(AtomicU64::new(f64::MAX.to_bits()));
        let fade_in_secs = Arc::new(AtomicU64::new(0.0f64.to_bits()));
        let fade_out_secs = Arc::new(AtomicU64::new(0.0f64.to_bits()));
        let fade_in_curve = Arc::new(AtomicU32::new(0)); // Linear
        let fade_out_curve = Arc::new(AtomicU32::new(0));

        let vol = volume.clone();
        let mute = muted.clone();
        let pts_clone = post_seek_pts_us.clone();
        let gen_clone = seek_generation.clone();
        let fsi = fade_source_in.clone();
        let fcd = fade_clip_dur.clone();
        let ffi = fade_in_secs.clone();
        let ffo = fade_out_secs.clone();
        let fic = fade_in_curve.clone();
        let foc = fade_out_curve.clone();

        let needs_resample = output_rate != SAMPLE_RATE;

        let thread = std::thread::Builder::new()
            .name("klipp-pcm-feed".into())
            .spawn(move || {
                pcm_feed_thread(
                    pcm_data,
                    output_rate,
                    needs_resample,
                    producer,
                    cmd_rx,
                    vol,
                    mute,
                    pts_clone,
                    gen_clone,
                    fsi,
                    fcd,
                    ffi,
                    ffo,
                    fic,
                    foc,
                );
            })
            .map_err(|e| PcmFeederError::Open(format!("spawn thread: {e}")))?;

        Ok((
            Self {
                thread: Some(thread),
                cmd_tx,
                volume,
                muted,
                post_seek_pts_us,
                seek_generation,
                fade_source_in,
                fade_clip_dur,
                fade_in_secs,
                fade_out_secs,
                fade_in_curve,
                fade_out_curve,
            },
            consumer,
        ))
    }

    /// Set fade parameters for per-clip fade in/out.
    pub fn set_fade(
        &self, source_in_secs: f64, clip_duration_secs: f64,
        fade_in: f64, fade_out: f64,
        fade_in_curve: u32, fade_out_curve: u32,
    ) {
        self.fade_source_in.store(source_in_secs.to_bits(), Ordering::Relaxed);
        self.fade_clip_dur.store(clip_duration_secs.to_bits(), Ordering::Relaxed);
        self.fade_in_secs.store(fade_in.to_bits(), Ordering::Relaxed);
        self.fade_out_secs.store(fade_out.to_bits(), Ordering::Relaxed);
        self.fade_in_curve.store(fade_in_curve, Ordering::Relaxed);
        self.fade_out_curve.store(fade_out_curve, Ordering::Relaxed);
    }

    pub fn seek(&self, secs: f64) {
        let _ = self.cmd_tx.send(PcmCommand::SeekTo(secs));
    }

    pub fn resume(&self) {
        let _ = self.cmd_tx.send(PcmCommand::Resume);
    }

    pub fn pause(&self) {
        let _ = self.cmd_tx.send(PcmCommand::Pause);
    }

    /// Play a short audio snippet at the given position (for scrub/frame step).
    pub fn chirp(&self, position_secs: f64, duration_secs: f64) {
        let _ = self.cmd_tx.send(PcmCommand::Chirp { position_secs, duration_secs });
    }

    pub fn stop(&mut self) {
        let _ = self.cmd_tx.send(PcmCommand::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    pub fn stop_detached(&mut self) {
        let _ = self.cmd_tx.send(PcmCommand::Stop);
        self.thread.take();
    }
}

impl Drop for PcmAudioFeeder {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug)]
pub enum PcmFeederError {
    Open(String),
}

impl std::fmt::Display for PcmFeederError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(e) => write!(f, "pcm feeder open: {e}"),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn pcm_feed_thread(
    pcm_data: Vec<u8>,
    output_rate: u32,
    needs_resample: bool,
    mut producer: AudioProducer,
    cmd_rx: crossbeam_channel::Receiver<PcmCommand>,
    volume: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
    post_seek_pts_us: Arc<AtomicU64>,
    seek_generation: Arc<AtomicU64>,
    fade_source_in: Arc<AtomicU64>,
    fade_clip_dur: Arc<AtomicU64>,
    fade_in_secs: Arc<AtomicU64>,
    fade_out_secs: Arc<AtomicU64>,
    fade_in_curve: Arc<AtomicU32>,
    fade_out_curve: Arc<AtomicU32>,
) {
    // Interpret raw bytes as f32 samples
    let samples: &[f32] = bytemuck::cast_slice(&pcm_data);
    let total_floats = samples.len();

    let mut paused = true;
    let mut eof = false;
    // Current read position in the samples array (in floats, i.e., sample_index * CHANNELS)
    let mut read_pos: usize = 0;

    // For resampling: if output_rate != 48000, we need to adjust the stride.
    // Simple approach: nearest-neighbor resampling (no interpolation).
    // For most systems output_rate == 48000, so this is a rare path.
    let rate_ratio = SAMPLE_RATE as f64 / output_rate as f64;

    // How many output stereo frames to produce per iteration (target ~10ms chunks)
    let chunk_frames = (output_rate as usize / 100).max(64);
    let chunk_floats = chunk_frames * CHANNELS;

    loop {
        // Process commands
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                PcmCommand::SeekTo(secs) => {
                    let sample_index = (secs * SAMPLE_RATE as f64) as usize;
                    read_pos = (sample_index * CHANNELS).min(total_floats);
                    eof = false;

                    // Report seek position
                    let actual_secs = (read_pos / CHANNELS) as f64 / SAMPLE_RATE as f64;
                    post_seek_pts_us.store((actual_secs * 1_000_000.0) as u64, Ordering::Release);
                    seek_generation.fetch_add(1, Ordering::Release);

                    debug!("pcm_feed: seek to {secs:.3}s → pos={read_pos} ({actual_secs:.3}s)");
                }
                PcmCommand::Pause => paused = true,
                PcmCommand::Resume => {
                    paused = false;
                    eof = false;
                }
                PcmCommand::Stop => {
                    debug!("pcm_feed: stop");
                    return;
                }
                PcmCommand::Chirp { position_secs, duration_secs } => {
                    // Push a short audio snippet at the given position.
                    // Works even when paused — that's the point of scrub audio.
                    debug!("pcm_feed: chirp at {position_secs:.3}s, {duration_secs:.3}s");
                    let chirp_samples = (duration_secs * SAMPLE_RATE as f64) as usize;
                    let start_idx = (position_secs * SAMPLE_RATE as f64) as usize * CHANNELS;
                    let end_idx = (start_idx + chirp_samples * CHANNELS).min(total_floats);
                    if start_idx < total_floats {
                        let vol = volume.load(Ordering::Relaxed) as f32 / 100.0;
                        let is_muted = muted.load(Ordering::Relaxed);
                        let snippet = &samples[start_idx..end_idx];

                        // Apply short fade-in/fade-out to avoid clicks (2ms each)
                        let fade_samples = (SAMPLE_RATE as usize / 500).min(snippet.len() / CHANNELS / 2);
                        let mut chirp_buf: Vec<f32> = Vec::with_capacity(snippet.len());
                        let total_frames = snippet.len() / CHANNELS;
                        for i in 0..total_frames {
                            let fade = if i < fade_samples {
                                i as f32 / fade_samples as f32
                            } else if i >= total_frames - fade_samples {
                                (total_frames - 1 - i) as f32 / fade_samples as f32
                            } else {
                                1.0
                            };
                            for ch in 0..CHANNELS {
                                let s = snippet[i * CHANNELS + ch];
                                if is_muted {
                                    chirp_buf.push(0.0);
                                } else {
                                    chirp_buf.push(s * vol * fade);
                                }
                            }
                        }

                        // Push to ring buffer (best effort — don't block)
                        let vacant = producer.vacant_len();
                        let to_push = chirp_buf.len().min(vacant);
                        if to_push > 0 {
                            producer.push_slice(&chirp_buf[..to_push]);
                        }
                    }
                }
            }
        }

        if paused || eof {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }

        // Check ring buffer space
        let vacant = producer.vacant_len();
        if vacant < chunk_floats {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }

        let vol = volume.load(Ordering::Relaxed) as f32 / 100.0;
        let is_muted = muted.load(Ordering::Relaxed);

        // Read fade params (lock-free)
        let src_in = f64::from_bits(fade_source_in.load(Ordering::Relaxed));
        let clip_dur = f64::from_bits(fade_clip_dur.load(Ordering::Relaxed));
        let fi_secs = f64::from_bits(fade_in_secs.load(Ordering::Relaxed));
        let fo_secs = f64::from_bits(fade_out_secs.load(Ordering::Relaxed));
        let fi_curve = fade_in_curve.load(Ordering::Relaxed) as u8;
        let fo_curve = fade_out_curve.load(Ordering::Relaxed) as u8;
        let has_fade = fi_secs > 0.001 || fo_secs > 0.001;

        if read_pos >= total_floats {
            eof = true;
            continue;
        }

        if !needs_resample {
            // Fast path: PCM is already at output_rate (48kHz == 48kHz)
            let remaining = total_floats - read_pos;
            let to_push = chunk_floats.min(remaining).min(vacant);

            if is_muted {
                let zeros = vec![0.0f32; to_push];
                producer.push_slice(&zeros);
            } else if !has_fade && (vol - 1.0).abs() < 0.01 {
                producer.push_slice(&samples[read_pos..read_pos + to_push]);
            } else {
                let scaled: Vec<f32> = samples[read_pos..read_pos + to_push]
                    .chunks(CHANNELS)
                    .enumerate()
                    .flat_map(|(i, pair)| {
                        let frame_idx = (read_pos / CHANNELS) + i;
                        let pos_secs = frame_idx as f64 / SAMPLE_RATE as f64;
                        let fade = fade_envelope(pos_secs, src_in, clip_dur, fi_secs, fo_secs, fi_curve, fo_curve);
                        let gain = vol * fade;
                        pair.iter().map(move |s| s * gain)
                    })
                    .collect();
                producer.push_slice(&scaled);
            }
            read_pos += to_push;
        } else {
            // Slow path: nearest-neighbor resample from 48kHz to output_rate
            let frames_to_produce = chunk_frames.min(vacant / CHANNELS);
            let mut buf = Vec::with_capacity(frames_to_produce * CHANNELS);

            for i in 0..frames_to_produce {
                let src_frame = ((read_pos / CHANNELS) as f64 + i as f64 * rate_ratio) as usize;
                let src_idx = src_frame * CHANNELS;
                if src_idx + CHANNELS > total_floats {
                    eof = true;
                    break;
                }
                let pos_secs = src_frame as f64 / SAMPLE_RATE as f64;
                let fade = fade_envelope(pos_secs, src_in, clip_dur, fi_secs, fo_secs, fi_curve, fo_curve);
                let gain = vol * fade;
                for ch in 0..CHANNELS {
                    let s = samples[src_idx + ch];
                    if is_muted {
                        buf.push(0.0);
                    } else {
                        buf.push(s * gain);
                    }
                }
            }

            if !buf.is_empty() {
                producer.push_slice(&buf);
                // Advance read_pos by the source frames consumed
                let source_frames_consumed = (buf.len() / CHANNELS) as f64 * rate_ratio;
                read_pos += (source_frames_consumed as usize) * CHANNELS;
                read_pos = read_pos.min(total_floats);
            }
        }
    }
}
