use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ringbuf::traits::{Observer, Producer, Split};
use ringbuf::HeapRb;
pub use ringbuf::{HeapCons, HeapProd};

use tracing::debug;

const PCM_RATE: u32 = 48000;
const CHANNELS: usize = 2;

// ── Session ──────────────────────────────────────────────────────────────

/// A live audio skim session with a continuous feeder thread.
/// Audio streams forward from the current position at real-time rate.
/// When the mouse moves, the read position jumps to the new location
/// with a short crossfade — no gaps, no clicks.
pub struct SkimAudioSession {
    /// Current read position, encoded as f64 bits. Feeder reads atomically.
    position_secs: Arc<AtomicU64>,

    /// Whether the feeder should be actively pushing audio.
    playing: Arc<AtomicBool>,

    /// Whether the session is alive (feeder thread exits when false).
    active: Arc<AtomicBool>,

    /// Volume (f32 bits stored as u32) — updated dynamically.
    volume: Arc<AtomicU32>,

    /// Source path (for debug only).
    pub source_path: PathBuf,
}

impl SkimAudioSession {
    /// Speed threshold in px/sec below which mouse is "stopped".
    pub const CHIRP_THRESHOLD_PX_PER_SEC: f32 = 80.0;

    /// Ring buffer: ~0.5s stereo at output rate.
    const RING_BUFFER_FLOATS: usize = 48000;

    /// Create a new skim audio session. Loads PCM, spawns feeder thread.
    pub fn new(
        pcm_path: &std::path::Path,
        source_path: PathBuf,
        output_rate: u32,
        skim_consumer: crate::audio_output::SkimConsumer,
        initial_volume: f32,
    ) -> Option<Self> {
        let raw = std::fs::read(pcm_path).ok()?;
        if raw.len() < 8 || raw.len() % 4 != 0 {
            return None;
        }

        let pcm_data: Vec<f32> = bytemuck::cast_slice(&raw).to_vec();
        let duration = pcm_data.len() as f64 / (PCM_RATE as f64 * CHANNELS as f64);
        debug!("skim_audio: loaded {:.1}s PCM, output_rate={output_rate}", duration);

        let (producer, ring_consumer) = HeapRb::<f32>::new(Self::RING_BUFFER_FLOATS).split();
        crate::audio_output::set_skim_consumer(&skim_consumer, Some(ring_consumer));

        let position_secs = Arc::new(AtomicU64::new(0.0f64.to_bits()));
        let playing = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicBool::new(true));
        let volume = Arc::new(AtomicU32::new(initial_volume.to_bits()));

        let t_pcm = Arc::new(pcm_data);
        let t_pos = position_secs.clone();
        let t_playing = playing.clone();
        let t_active = active.clone();
        let t_vol = volume.clone();

        std::thread::Builder::new()
            .name("klipp-skim-audio".into())
            .spawn(move || {
                skim_feeder_thread(t_pcm, t_pos, t_playing, t_active, t_vol, producer, output_rate);
            })
            .ok();

        Some(Self {
            position_secs,
            playing,
            active,
            volume,
            source_path,
        })
    }

    /// Seek to position and start playing. Called on every mouse move.
    pub fn seek_and_play(&self, secs: f64) {
        self.position_secs.store(secs.to_bits(), Ordering::Release);
        self.playing.store(true, Ordering::Release);
    }

    /// Pause audio (mouse stopped).
    pub fn silence(&self) {
        self.playing.store(false, Ordering::Release);
    }

    /// Update volume dynamically (e.g. when master volume slider changes).
    pub fn set_volume(&self, vol: f32) {
        self.volume.store(vol.to_bits(), Ordering::Relaxed);
    }

    /// Stop session. Feeder thread exits.
    pub fn stop(&self) {
        self.active.store(false, Ordering::SeqCst);
        self.playing.store(false, Ordering::Release);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkimMode {
    Playing,
    Silent,
}

pub fn determine_skim_mode(speed_px_per_sec: f32) -> SkimMode {
    if speed_px_per_sec < 1.0 {
        SkimMode::Silent
    } else {
        SkimMode::Playing
    }
}

// ── Feeder thread ────────────────────────────────────────────────────────

fn skim_feeder_thread(
    pcm_data: Arc<Vec<f32>>,
    position_secs: Arc<AtomicU64>,
    playing: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    volume: Arc<AtomicU32>,
    mut producer: HeapProd<f32>,
    output_rate: u32,
) {
    let total_floats = pcm_data.len();
    let rate_ratio = PCM_RATE as f64 / output_rate as f64;

    // ~10ms chunks
    let chunk_frames = (output_rate as usize / 100).max(64);
    let chunk_floats = chunk_frames * CHANNELS;

    let mut read_frame: usize = 0;
    let mut last_seek_secs: f64 = -1.0;

    // Crossfade state: when seeking, fade out old audio and fade in new
    let crossfade_frames = (output_rate as usize / 500).max(32); // ~2ms
    let mut crossfade_remaining: usize = 0;
    let mut old_read_frame: usize = 0;

    loop {
        if !active.load(Ordering::SeqCst) {
            break;
        }

        if !playing.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }

        // Check for position update (seek)
        let target_secs = f64::from_bits(position_secs.load(Ordering::Acquire));
        if (target_secs - last_seek_secs).abs() > 0.001 {
            old_read_frame = read_frame;
            read_frame = (target_secs * PCM_RATE as f64) as usize;
            last_seek_secs = target_secs;

            // Only crossfade if the jump is significant (>20ms) and we have valid old audio
            let jump_frames = read_frame.abs_diff(old_read_frame);
            if jump_frames > (PCM_RATE as usize / 50) && old_read_frame * CHANNELS < total_floats {
                crossfade_remaining = crossfade_frames;
            } else {
                crossfade_remaining = 0;
            }
        }

        // Check ring buffer space
        let vacant = producer.vacant_len();
        if vacant < chunk_floats {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }

        let vol = f32::from_bits(volume.load(Ordering::Relaxed));

        // Produce output frames
        let frames_to_produce = chunk_frames.min(vacant / CHANNELS);
        let mut buf = Vec::with_capacity(frames_to_produce * CHANNELS);

        for i in 0..frames_to_produce {
            let src_frame = read_frame + (i as f64 * rate_ratio) as usize;
            let si = src_frame * CHANNELS;
            if si + CHANNELS > total_floats {
                break;
            }

            if crossfade_remaining > 0 {
                // Crossfade: blend old position (fading out) with new position (fading in)
                let fade_in = 1.0 - (crossfade_remaining as f32 / crossfade_frames as f32);
                let fade_out = 1.0 - fade_in;

                let old_src_frame = old_read_frame + (i as f64 * rate_ratio) as usize;
                let old_si = old_src_frame * CHANNELS;

                for ch in 0..CHANNELS {
                    let new_sample = pcm_data[si + ch];
                    let old_sample = if old_si + ch < total_floats {
                        pcm_data[old_si + ch]
                    } else {
                        0.0
                    };
                    buf.push((new_sample * fade_in + old_sample * fade_out) * vol);
                }
                crossfade_remaining -= 1;
            } else {
                for ch in 0..CHANNELS {
                    buf.push(pcm_data[si + ch] * vol);
                }
            }
        }

        if !buf.is_empty() {
            producer.push_slice(&buf);
        }

        // Advance read position
        read_frame += (frames_to_produce as f64 * rate_ratio) as usize;
        // Also advance old_read_frame during crossfade
        if crossfade_remaining > 0 {
            old_read_frame += (frames_to_produce as f64 * rate_ratio) as usize;
        }

        // Pace: ~10ms chunks, sleep ~8ms
        std::thread::sleep(Duration::from_millis(8));
    }
}
