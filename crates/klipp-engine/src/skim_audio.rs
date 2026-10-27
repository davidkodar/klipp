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

    // Raw bytes from `std::fs::read` are only 1-byte aligned; bytemuck
    // panics on `cast_slice` when the underlying pointer isn't 4-byte
    // aligned. Allocate a `Vec<f32>` (guaranteed 4-byte aligned) and copy
    // the bytes in via the reverse cast — same final result, alignment-safe.
    let total_floats = raw.len() / 4;
    let mut pcm_data = vec![0f32; total_floats];
    bytemuck::cast_slice_mut::<f32, u8>(&mut pcm_data).copy_from_slice(&raw);
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
                crate::thread_guard::catch_panic("klipp-skim-audio", || {
                    skim_feeder_thread(pcm_data, t_pos, t_playing, t_active, t_vol, producer, output_rate);
                });
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

    /// Create a session that streams audio directly from a source file via
    /// on-demand ffmpeg decoding, **without** requiring a pre-conformed PCM
    /// cache or a `MediaId`. Used by the file browser hover-skim where files
    /// haven't been imported into the media pool.
    ///
    /// Internally this just decodes the entire source file into the same
    /// 48 kHz stereo f32 PCM format the existing `skim_feeder_thread`
    /// already consumes — that way we get all of its sophistication
    /// (linear interpolation, jump crossfading, drift-aware seek skipping)
    /// for free without writing a parallel live-decode feeder.
    ///
    /// First-hover cost is the decode time (~50-300ms for typical 3-10 min
    /// audio files). Subsequent hovers of the same file are instant because
    /// the decoded PCM is cached in `LIVE_PCM_CACHE` (LRU, 3 entries).
    ///
    /// Memory: ~50 MB per minute of audio at 48 kHz stereo f32. The cache
    /// keeps at most 3 files, so worst-case memory is bounded to roughly
    /// 3 × file_length × 50 MB/min.
    ///
    /// Returns `None` if:
    /// - the file can't be opened (missing, permission denied, malformed)
    /// - the file has no audio stream (e.g. user pointed at a video file
    ///   with no audio track, or a non-media file entirely)
    /// - the decoder/resampler can't be initialized
    pub fn new_from_source_path(
        source_path: PathBuf,
        output_rate: u32,
        skim_consumer: crate::audio_output::SkimConsumer,
        initial_volume: f32,
        initial_position_secs: f64,
    ) -> Option<Self> {
        let pcm_data = decode_audio_to_pcm(&source_path)?;

        let (producer, ring_consumer) = HeapRb::<f32>::new(Self::RING_BUFFER_FLOATS).split();
        crate::audio_output::set_skim_consumer(&skim_consumer, Some(ring_consumer));

        let position_secs = Arc::new(AtomicU64::new(initial_position_secs.to_bits()));
        let playing = Arc::new(AtomicBool::new(true)); // start playing immediately
        let active = Arc::new(AtomicBool::new(true));
        let volume = Arc::new(AtomicU32::new(initial_volume.to_bits()));

        let t_pos = position_secs.clone();
        let t_playing = playing.clone();
        let t_active = active.clone();
        let t_vol = volume.clone();

        std::thread::Builder::new()
            .name("klipp-skim-live".into())
            .spawn(move || {
                crate::thread_guard::catch_panic("klipp-skim-live", || {
                    skim_feeder_thread(pcm_data, t_pos, t_playing, t_active, t_vol, producer, output_rate);
                });
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
}

// ── Live PCM cache ────────────────────────────────────────────────────
//
// Separate from the existing PCM_CACHE (which is keyed by .pcm cache files
// produced by `audio_cache::conform_audio`). This one is keyed by the
// SOURCE audio file path so multiple hovers of the same .wav/.mp3 don't
// re-decode it. LRU with 3 entries.

#[allow(clippy::type_complexity)]
static LIVE_PCM_CACHE: std::sync::LazyLock<Mutex<Vec<(PathBuf, Arc<Vec<f32>>)>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

/// Decode an entire audio file to 48 kHz stereo f32 PCM via ffmpeg, with
/// LRU caching keyed by source path.
///
/// This is the bridge between live source files (the file browser use
/// case) and the existing `skim_feeder_thread` which expects an
/// `Arc<Vec<f32>>` of pre-decoded samples. Returns `None` on any decode
/// error so callers can fall back gracefully.
pub fn decode_audio_to_pcm(source_path: &std::path::Path) -> Option<Arc<Vec<f32>>> {
    const MAX_CACHED: usize = 3;

    // Cache hit (move to back = LRU "most recently used").
    if let Ok(mut cache) = LIVE_PCM_CACHE.lock() {
        if let Some(pos) = cache.iter().position(|(p, _)| p == source_path) {
            let entry = cache.remove(pos);
            let data = entry.1.clone();
            cache.push(entry);
            return Some(data);
        }
    }

    // Cache miss — decode from source.
    let mut format_ctx = match klipp_media::open_input(source_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("decode_audio_to_pcm: open failed for {:?}: {}", source_path, e);
            return None;
        }
    };
    let stream_index = match format_ctx.streams().best(ffmpeg_next::media::Type::Audio) {
        Some(s) => s.index(),
        None => {
            tracing::warn!("decode_audio_to_pcm: no audio stream in {:?}", source_path);
            return None;
        }
    };
    let params = format_ctx.stream(stream_index)?.parameters();
    let codec_ctx = match ffmpeg_next::codec::context::Context::from_parameters(params) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("decode_audio_to_pcm: codec context failed: {}", e);
            return None;
        }
    };
    let mut decoder = match codec_ctx.decoder().audio() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("decode_audio_to_pcm: audio decoder init failed: {}", e);
            return None;
        }
    };

    // ALWAYS derive the layout from the channel count via
    // `ChannelLayout::default(channels)`. This is more reliable than
    // `decoder.channel_layout()` which can return an empty layout for
    // PCM WAV streams whose container has `channel_layout=unknown`
    // (which is most WAV files in practice).
    //
    // We also capture the channel count separately because we need it
    // later to FIX UP each decoded frame: ffmpeg's modern AVChannelLayout
    // API leaves the layout empty on frames produced from streams with
    // unknown layout, but the resampler refuses to process such frames
    // ("Input changed" error). The fix is to call
    // `frame.set_channel_layout(...)` after each `receive_frame` and
    // before `resampler.run`. See `drain_into` below.
    let src_channels = decoder.channels();
    let src_layout = ffmpeg_next::ChannelLayout::default(src_channels as i32);
    let src_rate = decoder.rate();

    let mut resampler = match ffmpeg_next::software::resampling::Context::get(
        decoder.format(),
        src_layout,
        src_rate,
        ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed),
        ffmpeg_next::ChannelLayout::STEREO,
        PCM_RATE,
    ) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("decode_audio_to_pcm: resampler init failed: {}", e);
            return None;
        }
    };

    let mut all_samples: Vec<f32> = Vec::new();
    let mut frame = ffmpeg_next::frame::Audio::empty();

    // Two-phase decode loop (drain before send) — same pattern as
    // generate_audio_peaks and audio_cache::conform_audio.
    let mut packets = format_ctx.packets();
    let mut pending_packet: Option<ffmpeg_next::Packet> = None;
    let mut eof = false;

    // There are TWO latent bugs in ffmpeg-next's resampler wrapper that we
    // have to work around here:
    //
    // 1. Empty channel_layout on decoded frames.
    //    ffmpeg's modern AVChannelLayout API (FFmpeg 5.1+) leaves
    //    `ch_layout` empty on frames decoded from streams with unknown
    //    channel_layout (most PCM WAVs in the wild). The resampler was
    //    configured with a non-empty layout, so `swr_convert_frame`
    //    sees a mismatch and returns "Input changed". Fix: write the
    //    correct layout into each frame before handing it to the
    //    resampler.
    //
    // 2. Wrong output buffer pre-allocation.
    //    ffmpeg-next's `Context::run` does
    //    `output.alloc(out_format, input.samples(), out_layout)` when
    //    the output frame is empty. That sizes the output buffer for
    //    `input.samples()` STEREO frames — which is correct only when
    //    src_rate == dst_rate. When upsampling (16 kHz → 48 kHz, say),
    //    swr_convert_frame would need 3× as many output samples but
    //    the buffer is too small, so it silently emits only 1024
    //    samples per call and buffers the remainder internally.
    //    The leftover would be lost when we drop the resampler.
    //    Fix: pre-allocate `resampled` ourselves at the correct size
    //    so ffmpeg-next's wrapper sees a non-empty frame and skips
    //    its broken allocation.
    let dst_rate = PCM_RATE;
    let drain_into = |frame: &mut ffmpeg_next::frame::Audio,
                      resampler: &mut ffmpeg_next::software::resampling::Context,
                      all_samples: &mut Vec<f32>,
                      src_layout: ffmpeg_next::ChannelLayout,
                      src_rate: u32| {
        // Force the frame's channel_layout to match the resampler's
        // configured input layout — fixes AC-3 5.1(side) vs default 5.1
        // mismatch that would otherwise make every frame rejected by the
        // resampler. See the long comment in `audio_cache::conform_audio`
        // and `audio_decoder::audio_decode_thread` for the full story.
        frame.set_channel_layout(src_layout);
        // Worst-case output sample count for this input frame, plus a
        // small safety margin (matches FFmpeg's own internal `+ 32`
        // padding in swr_convert_frame's auto-allocation path).
        let out_samples = frame.samples() * dst_rate as usize / src_rate.max(1) as usize + 256;
        let mut resampled = ffmpeg_next::frame::Audio::new(
            ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed),
            out_samples,
            ffmpeg_next::ChannelLayout::STEREO,
        );
        if resampler.run(frame, &mut resampled).is_err() {
            return;
        }
        // Stereo f32 packed → 2 floats per sample frame.
        let n_samples = resampled.samples();
        let bytes_needed = n_samples * 2 * std::mem::size_of::<f32>();
        let raw = resampled.data(0);
        let usable = &raw[..bytes_needed.min(raw.len())];
        let floats: &[f32] = bytemuck::cast_slice(usable);
        all_samples.extend_from_slice(floats);
    };

    loop {
        // Phase 1: drain frames
        while decoder.receive_frame(&mut frame).is_ok() {
            drain_into(&mut frame, &mut resampler, &mut all_samples, src_layout, src_rate);
        }

        // Phase 2: feed next packet
        let packet = if let Some(p) = pending_packet.take() {
            Some(p)
        } else if eof {
            None
        } else {
            match packets.next() {
                Some((stream, p)) if stream.index() == stream_index => Some(p),
                Some(_) => continue,
                None => {
                    eof = true;
                    None
                }
            }
        };

        match packet {
            Some(p) => match decoder.send_packet(&p) {
                Ok(()) => {}
                Err(ffmpeg_next::Error::Other { errno: 11 }) => {
                    pending_packet = Some(p);
                }
                Err(_) => {} // bad packet — skip
            },
            None => {
                let _ = decoder.send_eof();
                while decoder.receive_frame(&mut frame).is_ok() {
                    drain_into(&mut frame, &mut resampler, &mut all_samples, src_layout, src_rate);
                }
                // Flush the resampler — there may still be buffered
                // samples even though our pre-allocation strategy aims
                // to prevent that.
                loop {
                    let mut flushed = ffmpeg_next::frame::Audio::new(
                        ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed),
                        4096,
                        ffmpeg_next::ChannelLayout::STEREO,
                    );
                    match resampler.flush(&mut flushed) {
                        Ok(Some(_)) | Ok(None) => {
                            let n_samples = flushed.samples();
                            if n_samples == 0 {
                                break;
                            }
                            let bytes_needed = n_samples * 2 * std::mem::size_of::<f32>();
                            let raw = flushed.data(0);
                            let usable = &raw[..bytes_needed.min(raw.len())];
                            let floats: &[f32] = bytemuck::cast_slice(usable);
                            all_samples.extend_from_slice(floats);
                        }
                        Err(_) => break,
                    }
                }
                break;
            }
        }
    }

    if all_samples.is_empty() {
        return None;
    }

    let data = Arc::new(all_samples);

    // Insert into cache with LRU eviction.
    if let Ok(mut cache) = LIVE_PCM_CACHE.lock() {
        if cache.len() >= MAX_CACHED {
            cache.remove(0);
        }
        cache.push((source_path.to_path_buf(), data.clone()));
    }

    Some(data)
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

#[cfg(test)]
mod live_decode_tests {
    use super::*;

    /// Hand-roll a minimal valid WAV file containing N samples of 16-bit
    /// signed mono PCM at the given sample rate. No external WAV writer
    /// dependency needed — the format is well-documented and trivial.
    fn write_test_wav(path: &std::path::Path, samples: &[i16], sample_rate: u32) {
        let n = samples.len();
        let byte_rate = sample_rate * 2; // mono, 16-bit = 2 bytes/sample
        let data_size = (n * 2) as u32;
        let mut bytes = Vec::with_capacity(44 + data_size as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&byte_rate.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes()); // block align (mono * 2 bytes)
        bytes.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_size.to_le_bytes());
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    /// Generate a 1-second sine wave at 440 Hz at the given sample rate.
    fn sine_wave_samples(sample_rate: u32, duration_secs: f64) -> Vec<i16> {
        let n = (sample_rate as f64 * duration_secs) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / sample_rate as f64;
                let v = (t * 440.0 * std::f64::consts::TAU).sin();
                (v * 16000.0) as i16
            })
            .collect()
    }

    fn temp_wav_path(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "klipp_skim_audio_test_{}_{}",
            label,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!("{label}.wav"))
    }

    fn cleanup(path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn decode_audio_to_pcm_returns_none_for_missing_file() {
        crate::init_ffmpeg().expect("ffmpeg init");
        let result = decode_audio_to_pcm(std::path::Path::new("/nonexistent/path.wav"));
        assert!(result.is_none());
    }

    #[test]
    fn decode_audio_to_pcm_returns_none_for_non_audio_file() {
        crate::init_ffmpeg().expect("ffmpeg init");
        // Try to decode the JPEG fixture as audio — should return None
        // because there's no audio stream in an image file.
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let jpeg = manifest.join("../../assets/effect_preview_reference.jpg");
        let result = decode_audio_to_pcm(&jpeg);
        assert!(
            result.is_none(),
            "image file should not decode as audio"
        );
    }

    #[test]
    fn decode_audio_to_pcm_decodes_synthetic_wav_at_target_rate() {
        crate::init_ffmpeg().expect("ffmpeg init");
        let path = temp_wav_path("at_target_rate");
        // 1 second of mono sine at 48 kHz (matches target rate, no resampling).
        let samples = sine_wave_samples(48_000, 1.0);
        write_test_wav(&path, &samples, 48_000);

        let pcm = decode_audio_to_pcm(&path).expect("should decode synthetic WAV");

        // Output is 48 kHz STEREO f32 → 96000 floats per second.
        // Allow ±200 floats slack for resampler buffering edges.
        let expected = 48_000 * 2;
        assert!(
            (pcm.len() as i64 - expected as i64).abs() < 200,
            "expected ~{} floats, got {}",
            expected,
            pcm.len()
        );

        // Verify it's not all zeros (the sine wave should produce non-zero samples).
        let has_signal = pcm.iter().any(|&s| s.abs() > 0.01);
        assert!(has_signal, "decoded PCM should contain non-zero samples");

        cleanup(&path);
    }

    #[test]
    fn decode_audio_to_pcm_resamples_from_lower_rate() {
        crate::init_ffmpeg().expect("ffmpeg init");
        let path = temp_wav_path("resample");
        // 1 second of mono sine at 16 kHz — must be resampled to 48 kHz stereo.
        let samples = sine_wave_samples(16_000, 1.0);
        write_test_wav(&path, &samples, 16_000);

        let pcm = decode_audio_to_pcm(&path).expect("should decode + resample");

        // Output should still be 48 kHz stereo for ~1 second = 96000 floats.
        let expected = 48_000 * 2;
        let diff = (pcm.len() as i64 - expected as i64).abs();
        assert!(
            diff < 500,
            "resampled output should be ~{} floats, got {}",
            expected,
            pcm.len()
        );

        cleanup(&path);
    }

    #[test]
    fn decode_audio_to_pcm_caches_repeat_decodes() {
        crate::init_ffmpeg().expect("ffmpeg init");
        let path = temp_wav_path("cache");
        let samples = sine_wave_samples(48_000, 0.5);
        write_test_wav(&path, &samples, 48_000);

        let first = decode_audio_to_pcm(&path).expect("first decode");
        let second = decode_audio_to_pcm(&path).expect("second decode");

        // Cache hit means both Arcs point at the same allocation.
        assert!(
            Arc::ptr_eq(&first, &second),
            "second decode should be a cache hit (same Arc)"
        );

        cleanup(&path);
    }

    #[test]
    fn decode_audio_to_pcm_returns_stereo_floats() {
        crate::init_ffmpeg().expect("ffmpeg init");
        let path = temp_wav_path("stereo_check");
        let samples = sine_wave_samples(48_000, 0.5);
        write_test_wav(&path, &samples, 48_000);

        let pcm = decode_audio_to_pcm(&path).expect("decode");

        // Stereo packed: length must be even (each frame = L + R).
        assert_eq!(
            pcm.len() % 2,
            0,
            "stereo PCM should have an even number of floats (L/R interleaved)"
        );

        cleanup(&path);
    }

    /// Diagnostic: invoke `klipp_media::audio_cache::conform_audio_to_paths`
    /// directly to verify the EXISTING audio pipeline works for the same
    /// file we're testing. If conform also fails, the bug is in shared
    /// ffmpeg infrastructure. If conform succeeds, the bug is specific to
    /// the new live-decode path.
    #[test]
    fn diagnostic_conform_audio_against_real_wav() {
        let path_str = match std::env::var("KLIPP_TEST_AUDIO_FILE") {
            Ok(p) => p,
            Err(_) => {
                eprintln!("[skipped] set KLIPP_TEST_AUDIO_FILE to a real audio file path");
                return;
            }
        };
        crate::init_ffmpeg().expect("ffmpeg init");
        let src = std::path::PathBuf::from(path_str);
        let dir = std::env::temp_dir().join(format!("klipp_diag_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pcm = dir.join("conform.pcm");
        let peaks = dir.join("conform.peaks");
        let mid = klipp_core::MediaId::new();
        let result = klipp_media::audio_cache::conform_audio_to_paths(mid, &src, &pcm, &peaks);
        match result {
            Ok(entry) => {
                let pcm_size = std::fs::metadata(&pcm).map(|m| m.len()).unwrap_or(0);
                eprintln!(
                    "[conform] {} → {} peaks, pcm bytes = {}",
                    src.display(),
                    entry.peaks.len(),
                    pcm_size
                );
                assert!(pcm_size > 1000, "conform produced empty PCM — bug is shared with audio_cache");
            }
            Err(e) => {
                panic!("conform_audio failed: {:?}", e);
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Opt-in test against a real audio file from the user's filesystem.
    /// Set `KLIPP_TEST_AUDIO_FILE=/path/to/file.wav` to enable.
    #[test]
    fn decode_audio_to_pcm_against_real_audio_file() {
        let path = match std::env::var("KLIPP_TEST_AUDIO_FILE") {
            Ok(p) => std::path::PathBuf::from(p),
            Err(_) => {
                eprintln!("[skipped] set KLIPP_TEST_AUDIO_FILE to a real audio file path");
                return;
            }
        };
        crate::init_ffmpeg().expect("ffmpeg init");
        let pcm = decode_audio_to_pcm(&path).expect("real audio file should decode");
        let duration_secs = pcm.len() as f64 / (PCM_RATE as f64 * CHANNELS as f64);
        eprintln!(
            "[OK] {} → {} stereo floats ({:.1}s @ 48 kHz)",
            path.display(),
            pcm.len(),
            duration_secs
        );
        assert!(!pcm.is_empty());
    }
}
