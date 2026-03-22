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

// ── Global PCM cache ────────────────────────────────────────────────────

use std::sync::Mutex;

/// Global cache of loaded PCM data, keyed by file path.
/// Avoids re-reading 85MB+ files every time the mouse enters/leaves a clip.
#[allow(clippy::type_complexity)]
static PCM_CACHE: std::sync::LazyLock<Mutex<Vec<(PathBuf, Arc<Vec<f32>>)>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

/// Load PCM data from cache or disk. Caches up to 3 files (LRU eviction).
fn load_pcm_cached(pcm_path: &std::path::Path) -> Option<Arc<Vec<f32>>> {
    const MAX_CACHED: usize = 3;

    let mut cache = PCM_CACHE.lock().ok()?;

    // Check cache (move to front on hit = LRU)
    if let Some(pos) = cache.iter().position(|(p, _)| p == pcm_path) {
        let entry = cache.remove(pos);
        let data = entry.1.clone();
        cache.push(entry);
        return Some(data);
    }

    // Cache miss — load from disk
    let raw = std::fs::read(pcm_path).ok()?;
    if raw.len() < 8 || raw.len() % 4 != 0 {
        return None;
    }

    let pcm_data: Vec<f32> = bytemuck::cast_slice(&raw).to_vec();
    let duration = pcm_data.len() as f64 / (PCM_RATE as f64 * CHANNELS as f64);
    debug!("skim_audio: loaded {:.1}s PCM from {:?}", duration, pcm_path);

    let data = Arc::new(pcm_data);

    // Evict oldest if at capacity
    if cache.len() >= MAX_CACHED {
        cache.remove(0);
    }
    cache.push((pcm_path.to_path_buf(), data.clone()));

    Some(data)
}

// ── Session ──────────────────────────────────────────────────────────────

/// A live audio skim session with a continuous feeder thread.
/// Audio streams forward from the current position at real-time rate.
/// Position jumps get crossfaded — no gaps, no clicks.
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

    /// Create a new skim audio session. Uses cached PCM data, spawns feeder thread.
    pub fn new(
        pcm_path: &std::path::Path,
        source_path: PathBuf,
        output_rate: u32,
        skim_consumer: crate::audio_output::SkimConsumer,
        initial_volume: f32,
    ) -> Option<Self> {
        let pcm_data = load_pcm_cached(pcm_path)?;

        let (producer, ring_consumer) = HeapRb::<f32>::new(Self::RING_BUFFER_FLOATS).split();
        crate::audio_output::set_skim_consumer(&skim_consumer, Some(ring_consumer));

        let position_secs = Arc::new(AtomicU64::new(0.0f64.to_bits()));
        let playing = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicBool::new(true));
        let volume = Arc::new(AtomicU32::new(initial_volume.to_bits()));

        let t_pos = position_secs.clone();
        let t_playing = playing.clone();
        let t_active = active.clone();
        let t_vol = volume.clone();

        std::thread::Builder::new()
            .name("klipp-skim-audio".into())
            .spawn(move || {
                skim_feeder_thread(pcm_data, t_pos, t_playing, t_active, t_vol, producer, output_rate);
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

    /// Seek to position and start playing.
    pub fn seek_and_play(&self, secs: f64) {
        self.position_secs.store(secs.to_bits(), Ordering::Release);
        self.playing.store(true, Ordering::Release);
    }

    /// Pause audio (mouse stopped).
    pub fn silence(&self) {
        self.playing.store(false, Ordering::Release);
    }

    /// Update volume dynamically.
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

/// Linear interpolation between two source frames for smoother resampling.
#[inline]
fn lerp_sample(pcm: &[f32], frame_a: usize, frame_b: usize, frac: f32, ch: usize) -> f32 {
    let total = pcm.len();
    let ia = frame_a * CHANNELS + ch;
    let ib = frame_b * CHANNELS + ch;
    let a = if ia < total { pcm[ia] } else { 0.0 };
    let b = if ib < total { pcm[ib] } else { a };
    a + (b - a) * frac
}

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

    // Read position tracked as fractional source frame for lerp precision
    let mut read_pos: f64 = 0.0;
    let mut last_seek_secs: f64 = -1.0;

    // Crossfade state
    let base_crossfade = (output_rate as usize / 200).max(32); // ~5ms base
    let mut crossfade_len: usize = base_crossfade;
    let mut crossfade_remaining: usize = 0;
    let mut old_read_pos: f64 = 0.0;

    loop {
        if !active.load(Ordering::SeqCst) {
            break;
        }

        if !playing.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }

        // Check for position update (seek).
        // Only seek if the target is far from where we're currently reading.
        // If the feeder has naturally played forward to near the target, skip the seek.
        let target_secs = f64::from_bits(position_secs.load(Ordering::Acquire));
        if (target_secs - last_seek_secs).abs() > 0.001 {
            last_seek_secs = target_secs;
            let target_pos = target_secs * PCM_RATE as f64;

            // How far is the target from our current read position?
            let drift = (target_pos - read_pos).abs();
            let drift_ms = drift / PCM_RATE as f64 * 1000.0;

            // Only seek if we're more than 150ms away from target.
            // For slow mouse movement, the feeder naturally stays close
            // to the cursor — no seek needed, just smooth forward playback.
            if drift_ms > 150.0 {
                old_read_pos = read_pos;
                read_pos = target_pos;

                // Crossfade duration scales with jump size
                if drift_ms > 500.0 {
                    crossfade_len = (output_rate as usize / 125).max(32); // ~8ms
                } else {
                    crossfade_len = (output_rate as usize / 250).max(32); // ~4ms
                };

                if (old_read_pos * CHANNELS as f64) < total_floats as f64 {
                    crossfade_remaining = crossfade_len;
                }
            }
        }

        // Check ring buffer space
        let vacant = producer.vacant_len();
        if vacant < chunk_floats {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }

        let vol = f32::from_bits(volume.load(Ordering::Relaxed));

        // Produce output frames with linear interpolation
        let frames_to_produce = chunk_frames.min(vacant / CHANNELS);
        let mut buf = Vec::with_capacity(frames_to_produce * CHANNELS);

        for i in 0..frames_to_produce {
            // Source position with sub-frame precision
            let src_pos = read_pos + i as f64 * rate_ratio;
            let src_frame = src_pos as usize;
            let frac = (src_pos - src_frame as f64) as f32;

            if (src_frame + 1) * CHANNELS >= total_floats {
                break;
            }

            if crossfade_remaining > 0 {
                let fade_in = 1.0 - (crossfade_remaining as f32 / crossfade_len as f32);
                let fade_out = 1.0 - fade_in;

                let old_src_pos = old_read_pos + i as f64 * rate_ratio;
                let old_frame = old_src_pos as usize;
                let old_frac = (old_src_pos - old_frame as f64) as f32;

                for ch in 0..CHANNELS {
                    let new_s = lerp_sample(&pcm_data, src_frame, src_frame + 1, frac, ch);
                    let old_s = if (old_frame + 1) * CHANNELS < total_floats {
                        lerp_sample(&pcm_data, old_frame, old_frame + 1, old_frac, ch)
                    } else {
                        0.0
                    };
                    buf.push((new_s * fade_in + old_s * fade_out) * vol);
                }
                crossfade_remaining -= 1;
            } else {
                for ch in 0..CHANNELS {
                    buf.push(lerp_sample(&pcm_data, src_frame, src_frame + 1, frac, ch) * vol);
                }
            }
        }

        if !buf.is_empty() {
            producer.push_slice(&buf);
        }

        // Advance read position
        read_pos += frames_to_produce as f64 * rate_ratio;
        if crossfade_remaining > 0 {
            old_read_pos += frames_to_produce as f64 * rate_ratio;
        }

        // Pace: ~10ms chunks, sleep ~8ms
        std::thread::sleep(Duration::from_millis(8));
    }
}
