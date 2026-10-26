use klipp_audio::dsp::{AudioEffectChain, AudioProcessContext};
use klipp_core::AudioEffectInstance;
use ringbuf::traits::{Observer, Producer, Split};
use ringbuf::HeapRb;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::audio_decoder::{AudioConsumer, AudioProducer};

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: usize = 2;

/// Minimum fade duration (seconds) applied at clip boundaries to prevent click/pop artifacts.
/// Configurable via preferences (default 2.0ms). Set via `set_anti_click_fade_ms()`.
static ANTI_CLICK_FADE_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(
    (2.0f64).to_bits()
);

/// Set the anti-click fade duration from preferences.
pub fn set_anti_click_fade_ms(ms: f64) {
    ANTI_CLICK_FADE_MS.store(ms.to_bits(), std::sync::atomic::Ordering::Relaxed);
}

fn anti_click_fade_secs() -> f64 {
    f64::from_bits(ANTI_CLICK_FADE_MS.load(std::sync::atomic::Ordering::Relaxed)) / 1000.0
}

/// Apply a curve shape to a linear 0..1 value.
///
/// The shapes live on [`klipp_core::FadeCurve`] so this and the export
/// mixdown cannot disagree; both used to carry their own `match`.
fn apply_curve(t: f64, curve: u8) -> f64 {
    klipp_core::FadeCurve::from_index(curve as u32).gain(t)
}

/// Compute fade envelope multiplier (0.0..1.0) for a given position in the source.
/// Always applies a minimum 2ms linear ramp at clip start/end to prevent click artifacts,
/// even when no explicit fade is configured.
fn fade_envelope(
    pos_secs: f64, source_in: f64, clip_dur: f64,
    fade_in: f64, fade_out: f64,
    fade_in_curve: u8, fade_out_curve: u8,
    anti_click: bool,
) -> f32 {
    let clip_pos = pos_secs - source_in;
    let clip_end = clip_dur;

    let mut gain = 1.0;

    // Effective fade durations: at least ANTI_CLICK_FADE_SECS when anti-click is enabled
    let min_fade = if anti_click { anti_click_fade_secs() } else { 0.0 };
    let eff_fade_in = fade_in.max(min_fade);
    let eff_fade_out = fade_out.max(min_fade);

    // Fade in: ramp 0→1 over fade_in seconds from clip start
    if clip_pos < eff_fade_in {
        let t = (clip_pos / eff_fade_in).clamp(0.0, 1.0);
        // Use configured curve for explicit fades, linear for anti-click
        let curve = if fade_in > 0.001 { fade_in_curve } else { 0 };
        gain = apply_curve(t, curve);
    }

    // Fade out: ramp 1→0 over fade_out seconds to clip end
    // Only apply fade-out when there's an explicit fade_out configured.
    // For anti-click only (no explicit fade), don't fade to silence at clip_end —
    // contiguous clips need audio to continue seamlessly past the boundary.
    // The anti-click fade-out is only needed at the ACTUAL end of playback,
    // which is handled by the audio source stopping.
    if fade_out > 0.001 && clip_pos > clip_end - eff_fade_out {
        let remaining = clip_end - clip_pos;
        let t = (remaining / eff_fade_out).clamp(0.0, 1.0);
        gain *= apply_curve(t, fade_out_curve);
    }

    gain as f32
}

/// Commands for the PCM feeder thread.
/// How far ahead of playback a feed with audio effects renders, in
/// milliseconds: enough margin for the effect chain, short enough that a
/// setting changed while playing is heard within about a quarter second.
pub(crate) const EFFECT_READ_AHEAD_SECS_X1000: u32 = 250;

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
    pub fade_gain: Arc<AtomicU32>,     // pre-fader gain (f32 bits)
    pub anti_click: Arc<AtomicBool>,   // apply 2ms anti-click fade at boundaries
    /// The clip's speed and source range, for reading its sound at that
    /// speed (#642); see [`Varispeed`].
    pub varispeed: Arc<Varispeed>,
    /// New effect settings for the running chain; see
    /// [`crate::audio_source::AudioSource::update_effect_settings`].
    pub effect_settings: crate::audio_source::EffectSettingsSlot,
    /// Where the clip's sound stops and its effects ring on (#2780).
    pub ring: Arc<Ring>,
}

/// A clip's sound stopping at its out point, its effects then fed
/// silence for as long as their tail (#2780), as the export renders it:
/// a reverb rings on into a gap or under the next clip.
pub struct Ring {
    /// Whether the sound stops at the end of the fade envelope (the
    /// clip's out point, past a crossfade's handle): false while the
    /// next clip carries the same sound on.
    stops: AtomicBool,
    /// The tail, in frames, from where the feed reached the out point;
    /// `u64::MAX` before it has.
    tail_frames: AtomicU64,
}

impl Default for Ring {
    fn default() -> Self {
        Self { stops: AtomicBool::new(false), tail_frames: AtomicU64::new(u64::MAX) }
    }
}

impl Ring {
    pub fn set_stops(&self, stops: bool) {
        self.stops.store(stops, Ordering::Relaxed);
    }

    /// The tail in seconds, once the feed has reached the out point.
    pub fn tail_secs(&self) -> Option<f64> {
        match self.tail_frames.load(Ordering::Relaxed) {
            u64::MAX => None,
            n => Some(n as f64 / SAMPLE_RATE as f64),
        }
    }
}

/// A retimed clip's sound, read at its speed (#642): `speed` source
/// frames a frame, backward when negative, so its pitch moves with it as
/// tape's does and it stays on its picture. 1.0 reads straight through.
/// `source_in` and `source_out` are the clip's own, in seconds: where
/// fades and effects take its time from, as at 1x they take it from the
/// source position. With `keep_pitch` a forward clip's sound keeps its
/// pitch instead (#925), stretched by [`klipp_audio::dsp::stretch`] on a
/// grain grid anchored at `source_in`, as the export lays it.
pub struct Varispeed {
    speed: AtomicU64,
    source_in: AtomicU64,
    source_out: AtomicU64,
    keep_pitch: AtomicBool,
}

impl Default for Varispeed {
    fn default() -> Self {
        Self { speed: AtomicU64::new(1.0f64.to_bits()), source_in: AtomicU64::new(0.0f64.to_bits()), source_out: AtomicU64::new(0.0f64.to_bits()), keep_pitch: AtomicBool::new(false) }
    }
}

impl Varispeed {
    pub fn set(&self, speed: f64, source_in: f64, source_out: f64, keep_pitch: bool) {
        self.keep_pitch.store(keep_pitch, Ordering::Relaxed);
        self.source_in.store(source_in.to_bits(), Ordering::Relaxed);
        self.source_out.store(source_out.to_bits(), Ordering::Relaxed);
        self.speed.store(speed.to_bits(), Ordering::Relaxed);
    }

    fn get(&self) -> (f64, f64, f64, bool) {
        let f = |a: &AtomicU64| f64::from_bits(a.load(Ordering::Relaxed));
        (f(&self.speed), f(&self.source_in), f(&self.source_out), self.keep_pitch.load(Ordering::Relaxed))
    }
}

/// Where a varispeed read at frame `pos` is in the clip, as the source
/// second 1x playback would be at: `source_in` plus the clip's own time
/// there. Fades and effects are timed by it, as they are at 1x.
pub(crate) fn varispeed_clip_secs(pos: f64, speed: f64, source_in: f64, source_out: f64) -> f64 {
    let at = pos / SAMPLE_RATE as f64;
    let elapsed = if speed < 0.0 { (source_out - at) / -speed } else { (at - source_in) / speed };
    source_in + elapsed.max(0.0)
}

/// PCM caches kept in memory once read, shared by every feeder that plays
/// them. A cut to another file used to stop every source and open each
/// track's cache again, reading the whole file: a 9-minute recording is
/// 213 MB and took 120-150 ms on the engine thread, at every cut (#572).
/// Now only the first open reads it.
///
/// Held strongly, since at a cut every feeder is dropped before the next
/// ones open, within [`PCM_CACHE_BUDGET`] bytes; the least recently used
/// go first. An entry is the file's as it was: a cache written again
/// (another size or modification time) is read again.
struct PcmCache {
    entries: Vec<PcmCacheEntry>,
    bytes: usize,
}

struct PcmCacheEntry {
    path: std::path::PathBuf,
    len: u64,
    modified: Option<std::time::SystemTime>,
    data: Arc<[f32]>,
}

/// What the PCM caches in memory may add up to before the least recently
/// used is let go: a feature's worth of stereo 48 kHz sound.
const PCM_CACHE_BUDGET: usize = 1 << 30;

static PCM_CACHE: std::sync::Mutex<PcmCache> = std::sync::Mutex::new(PcmCache { entries: Vec::new(), bytes: 0 });

/// The samples of the PCM cache at `path`: from memory when it was read
/// before and has not changed, else read now and kept.
fn load_pcm(path: &Path) -> Result<Arc<[f32]>, PcmFeederError> {
    let meta = std::fs::metadata(path).map_err(|e| PcmFeederError::Open(format!("read {path:?}: {e}")))?;
    let (len, modified) = (meta.len(), meta.modified().ok());
    {
        let mut cache = PCM_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = cache.entries.iter().position(|e| e.path == path && e.len == len && e.modified == modified) {
            // Most recently used last.
            let entry = cache.entries.remove(i);
            let data = entry.data.clone();
            cache.entries.push(entry);
            return Ok(data);
        }
    }
    let data = read_pcm(path)?;
    let size = data.len() * 4;
    let mut cache = PCM_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    // The same path at another size or time is a stale copy.
    let mut freed = 0;
    cache.entries.retain(|e| {
        let keep = e.path != path;
        if !keep {
            freed += e.data.len() * 4;
        }
        keep
    });
    cache.bytes -= freed;
    while cache.bytes + size > PCM_CACHE_BUDGET && !cache.entries.is_empty() {
        let gone = cache.entries.remove(0);
        cache.bytes -= gone.data.len() * 4;
    }
    if size <= PCM_CACHE_BUDGET {
        cache.bytes += size;
        cache.entries.push(PcmCacheEntry { path: path.to_path_buf(), len, modified, data: data.clone() });
    }
    Ok(data)
}

/// Let go of every PCM cache held in memory: on a project closed or
/// replaced, whose sound will not play again. A feeder still playing one
/// keeps its own copy until it stops. Kept over a pause or a stop, as
/// playing on would read the files again.
pub fn release_all() {
    let mut cache = PCM_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.entries.clear();
    cache.bytes = 0;
}

/// Load the PCM cache at `path` into memory ahead of need, so a feeder
/// opened on it later reads nothing: for the next clip's sound, from the
/// video preroll before a cut. Any thread; a second call for an unchanged
/// file does nothing. Two calls at once may both read it, and the cache
/// keeps one copy.
pub fn warm(path: &Path) -> Result<(), PcmFeederError> {
    load_pcm(path).map(|_| ())
}

/// Read a PCM cache file whole, as 32-bit float samples.
fn read_pcm(pcm_path: &Path) -> Result<Arc<[f32]>, PcmFeederError> {
    let pcm_bytes = std::fs::read(pcm_path)
        .map_err(|e| PcmFeederError::Open(format!("read {:?}: {e}", pcm_path)))?;

    if pcm_bytes.len() % 4 != 0 {
        return Err(PcmFeederError::Open("PCM file size not aligned to f32".into()));
    }
    // Treat zero-length (or too-short-to-contain-a-stereo-sample) PCM
    // files as a load failure so the engine falls back to AudioDecoder
    // instead of silently succeeding with a feeder that produces
    // silence. Previously a ghost 0-byte cache file was opened here,
    // `total_floats == 0`, and the clip played mute on the timeline
    // even though the source media had audio.
    if pcm_bytes.len() < 16 {
        return Err(PcmFeederError::Open(
            format!("PCM cache file too small ({} bytes) — treating as absent", pcm_bytes.len()),
        ));
    }

    // Copy the raw bytes into f32 samples: the allocation is 4-byte
    // aligned, while a plain `Vec<u8>` is only 1-byte aligned and
    // `bytemuck::cast_slice` panics if the pointer isn't aligned to the
    // target type.
    let mut pcm_data = vec![0f32; pcm_bytes.len() / 4];
    bytemuck::cast_slice_mut::<f32, u8>(&mut pcm_data).copy_from_slice(&pcm_bytes);
    Ok(pcm_data.into())
}

impl PcmAudioFeeder {
    /// Open a PCM cache file and create a feeder + ring buffer consumer.
    /// The PCM file must be 48kHz stereo f32 interleaved (as produced by conform_audio).
    /// `audio_effects` are the per-clip DSP effects to apply before ring buffer push.
    /// `audio_effect_keyframes` are the per-clip param/bypass/wet-dry keyframe
    /// tracks (`clip.audio_effect_keyframes`); when non-empty the chain runs
    /// in sub-chunked evaluation mode.
    pub fn open(
        pcm_path: &Path,
        output_rate: u32,
        audio_effects: Vec<AudioEffectInstance>,
        audio_effect_keyframes: std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
    ) -> Result<(Self, AudioConsumer), PcmFeederError> {
        // The samples are read on the feeder's own thread, not here (#2136):
        // the engine opens a clip's sound ahead of its cut, on its own
        // thread, and a 35-minute film's cache read whole took 0.85 s, the
        // picture frozen and Pause unheard. Until they are in, the mix
        // hears silence: a few milliseconds at a cut, not a stall.
        std::fs::metadata(pcm_path).map_err(|e| PcmFeederError::Open(format!("read {pcm_path:?}: {e}")))?;
        let pcm_path = pcm_path.to_path_buf();

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
        let fade_gain = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let anti_click = Arc::new(AtomicBool::new(true));
        let varispeed = Arc::new(Varispeed::default());
        let ring = Arc::new(Ring::default());

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
        let fg = fade_gain.clone();
        let ac = anti_click.clone();
        let vs = varispeed.clone();
        let rg = ring.clone();
        let effect_settings: crate::audio_source::EffectSettingsSlot = Default::default();
        let settings_rx = effect_settings.clone();

        let needs_resample = output_rate != SAMPLE_RATE;

        let thread = std::thread::Builder::new()
            .name("klipp-pcm-feed".into())
            .spawn(move || {
                crate::thread_guard::catch_panic("klipp-pcm-feed", || {
                    let pcm_data = match load_pcm(&pcm_path) {
                        Ok(data) => data,
                        Err(e) => {
                            warn!("PcmAudioFeeder: cannot read {pcm_path:?}: {e}");
                            return;
                        }
                    };
                    info!(
                        "PcmAudioFeeder: opened {:?}, {:.1}s, {} samples",
                        pcm_path,
                        (pcm_data.len() / CHANNELS) as f64 / SAMPLE_RATE as f64,
                        pcm_data.len() / CHANNELS,
                    );
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
                        fg,
                        ac,
                        vs,
                        rg,
                        audio_effects,
                        audio_effect_keyframes,
                        settings_rx,
                    );
                });
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
                fade_gain,
                anti_click,
                varispeed,
                effect_settings,
                ring,
            },
            consumer,
        ))
    }

    /// Set fade parameters for per-clip fade in/out.
    pub fn set_fade(
        &self, source_in_secs: f64, clip_duration_secs: f64,
        fade_in: f64, fade_out: f64,
        fade_in_curve: u32, fade_out_curve: u32,
        gain: f32,
    ) {
        self.fade_source_in.store(source_in_secs.to_bits(), Ordering::Relaxed);
        self.fade_clip_dur.store(clip_duration_secs.to_bits(), Ordering::Relaxed);
        self.fade_in_secs.store(fade_in.to_bits(), Ordering::Relaxed);
        self.fade_out_secs.store(fade_out.to_bits(), Ordering::Relaxed);
        self.fade_in_curve.store(fade_in_curve, Ordering::Relaxed);
        self.fade_out_curve.store(fade_out_curve, Ordering::Relaxed);
        self.fade_gain.store(gain.to_bits(), Ordering::Relaxed);
    }

    pub fn set_anti_click(&self, enabled: bool) {
        self.anti_click.store(enabled, Ordering::Relaxed);
    }

    /// Read the clip's sound at `speed`, from its `source_in`..`source_out`
    /// (#642).
    pub fn set_varispeed(&self, speed: f64, source_in: f64, source_out: f64, keep_pitch: bool) {
        self.varispeed.set(speed, source_in, source_out, keep_pitch);
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
    pcm_data: Arc<[f32]>,
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
    _fade_gain: Arc<AtomicU32>,
    anti_click: Arc<AtomicBool>,
    varispeed: Arc<Varispeed>,
    ring: Arc<Ring>,
    audio_effects: Vec<AudioEffectInstance>,
    audio_effect_keyframes: std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
    effect_settings: crate::audio_source::EffectSettingsSlot,
) {
    // Flush denormals on this DSP thread — same reason
    // as in audio_decoder. A silent PCM region with biquads engaged
    // would otherwise accumulate denormal state and stall the FPU.
    klipp_audio::flush_denormals_to_zero();

    // PCM data already comes in as `Vec<f32>` (converted at open() time so
    // the buffer is guaranteed 4-byte aligned — see the comment in
    // `PcmAudioFeeder::open`). No per-thread cast needed.
    let samples: &[f32] = &pcm_data[..];
    let total_floats = samples.len();

    // Build audio effect chain from per-clip effects + keyframe tracks.
    let mut effect_chain = if !audio_effects.is_empty() {
        // Effects run at the PCM's own 48 kHz, before any conversion to
        // the device rate, on both paths.
        let mut chain = AudioEffectChain::from_instances_with_kfs(
            &audio_effects,
            &audio_effect_keyframes,
            SAMPLE_RATE,
        );
        // Start costly effects now, while paused, not on the first block
        // after Play, where it would starve the sound card.
        chain.prepare(CHANNELS);
        debug!(
            "pcm_feed: {} audio effects active ({} kf tracks)",
            chain.slot_count(),
            audio_effect_keyframes.len(),
        );
        Some(chain)
    } else {
        None
    };

    let mut paused = true;
    // Conversion to the device rate on the slow path; rebuilt after a seek.
    let mut resampler: Option<PlaybackResampler> = None;
    let mut eof = false;
    // Current read position in the samples array (in floats, i.e., sample_index * CHANNELS)
    let mut read_pos: usize = 0;
    // A varispeed read's position, in frames, fractional; `read_pos` is
    // taken up from it when the speed comes back to 1 (#642).
    let mut vpos: f64 = 0.0;
    let mut vari = false;
    // A forward read with its pitch kept (#925), carried between chunks.
    let mut stretcher: Option<klipp_audio::dsp::stretch::Stretcher> = None;
    // The frames of tail still to ring, once past the out point (#2780).
    let mut tail_left: Option<usize> = None;

    // Source frames per device frame, when the device is not at 48 kHz
    // (the slow path below resamples with a windowed sinc).
    let rate_ratio = SAMPLE_RATE as f64 / output_rate as f64;

    // How many output stereo frames to produce per iteration.
    // 2048 frames ≈ 42ms at 48kHz — large enough for DSP effects with internal
    // state (pitch shift, reverb, delay, chorus) while staying well within the
    // ring buffer capacity (~2s).
    let chunk_frames = if effect_chain.is_some() {
        2048
    } else {
        (output_rate as usize / 100).max(64) // ~10ms when no effects
    };
    let chunk_floats = chunk_frames * CHANNELS;

    loop {
        // New settings for the running chain (a slider moved), in place.
        if let Some(ref mut chain) = effect_chain {
            if let Some(settings) = effect_settings.try_lock().ok().and_then(|mut p| p.take()) {
                let applied = chain.apply_settings(&settings);
                debug!("pcm_feed: effect settings applied in place ({applied}), {} ms queued", producer.occupied_len() / CHANNELS * 1000 / output_rate as usize);
            }
        }
        // Process commands
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                PcmCommand::SeekTo(secs) => {
                    let sample_index = (secs * SAMPLE_RATE as f64) as usize;
                    read_pos = (sample_index * CHANNELS).min(total_floats);
                    vpos = secs.max(0.0) * SAMPLE_RATE as f64;
                    eof = false;

                    // Report seek position
                    let actual_secs = (read_pos / CHANNELS) as f64 / SAMPLE_RATE as f64;
                    post_seek_pts_us.store((actual_secs * 1_000_000.0) as u64, Ordering::Release);
                    seek_generation.fetch_add(1, Ordering::Release);

                    // Reset effect chain state on seek
                    if let Some(ref mut chain) = effect_chain {
                        chain.reset();
                    }
                    resampler = None;
                    tail_left = None;
                    ring.tail_frames.store(u64::MAX, Ordering::Relaxed);

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
        // With effects, render only a little ahead: a setting changed
        // while playing reaches audio not yet rendered, so two seconds of
        // read-ahead made a slider change audible two seconds late.
        if effect_chain.is_some() && producer.occupied_len() >= EFFECT_READ_AHEAD_SECS_X1000 as usize * output_rate as usize * CHANNELS / 1000 {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }

        let vol = volume.load(Ordering::Relaxed) as f32 / 100.0;
        let is_muted = muted.load(Ordering::Relaxed);

        // Read fade params (lock-free) — gain is NOT applied here, it is
        // applied with the clip's volume in the mix thread (SourceMix)
        let src_in = f64::from_bits(fade_source_in.load(Ordering::Relaxed));
        let clip_dur = f64::from_bits(fade_clip_dur.load(Ordering::Relaxed));
        let fi_secs = f64::from_bits(fade_in_secs.load(Ordering::Relaxed));
        let fo_secs = f64::from_bits(fade_out_secs.load(Ordering::Relaxed));
        let fi_curve = fade_in_curve.load(Ordering::Relaxed) as u8;
        let fo_curve = fade_out_curve.load(Ordering::Relaxed) as u8;
        let ac = anti_click.load(Ordering::Relaxed);
        let has_fade = fi_secs > 0.001 || fo_secs > 0.001 || ac;

        // A retimed clip (#642): its sound read at its speed, backward when
        // reversed, then faded, effected and sent on as at 1x, its time
        // being the clip's own.
        let (speed, vs_in, vs_out, keep_pitch) = varispeed.get();
        let retimed = (speed - 1.0).abs() > 1e-6 && speed.is_finite() && speed != 0.0;
        if retimed && !vari {
            vpos = (read_pos / CHANNELS) as f64;
            vari = true;
        } else if !retimed && vari {
            read_pos = ((vpos.max(0.0) as usize) * CHANNELS).min(total_floats);
            vari = false;
        }
        // Where the feed is in the clip, on the fade envelope's clock,
        // and the frames left before the clip's sound stops there.
        let at = if vari { varispeed_clip_secs(vpos, speed, vs_in, vs_out) } else { (read_pos / CHANNELS) as f64 / SAMPLE_RATE as f64 };
        let to_stop = if ring.stops.load(Ordering::Relaxed) { ((src_in + clip_dur - at) * SAMPLE_RATE as f64).round().max(0.0) as usize } else { usize::MAX };
        if to_stop == 0 {
            // Past the out point: the effects ring on with silence in
            // for their tail, at the settings they ended on, then the
            // feed ends. Without effects it ends here.
            let Some(ref mut chain) = effect_chain else {
                eof = true;
                continue;
            };
            let left = *tail_left.get_or_insert_with(|| {
                let frames = chain.tail_frames(SAMPLE_RATE);
                ring.tail_frames.store(frames as u64, Ordering::Relaxed);
                frames
            });
            if left == 0 {
                eof = true;
                continue;
            }
            let room = if needs_resample { ((vacant / CHANNELS).saturating_sub(2) as f64 * rate_ratio) as usize } else { vacant / CHANNELS };
            let n = chunk_frames.min(room).min(left);
            if n == 0 {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            let mut buf = vec![0.0f32; n * CHANNELS];
            chain.process_aligned(&mut buf, &AudioProcessContext { sample_rate: SAMPLE_RATE, channels: CHANNELS as u32, clip_time_secs: (at - src_in).max(0.0) });
            if needs_resample {
                let out = resampler.get_or_insert_with(|| PlaybackResampler::new(output_rate)).process(&buf);
                producer.push_slice(&out);
            } else {
                producer.push_slice(&buf);
            }
            tail_left = Some(left - n);
            if vari {
                vpos += n as f64 * speed;
            } else {
                read_pos += n * CHANNELS;
            }
            continue;
        }
        if vari {
            let room = if needs_resample { ((vacant / CHANNELS).saturating_sub(2) as f64 * rate_ratio) as usize } else { vacant / CHANNELS };
            let n = chunk_frames.min(room).min(to_stop);
            if n == 0 {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            let mut buf = if keep_pitch && speed > 0.0 {
                // Output frames counted from the clip's start, where the
                // grain grid is anchored: the export's grid too.
                let anchor = vs_in * SAMPLE_RATE as f64;
                let s = match stretcher.take() {
                    Some(s) if s.reads(anchor, speed) => stretcher.insert(s),
                    _ => stretcher.insert(klipp_audio::dsp::stretch::Stretcher::new(anchor, speed)),
                };
                s.read(samples, CHANNELS, ((vpos - anchor) / speed).round() as i64, n)
            } else {
                klipp_audio::dsp::varispeed::read(samples, CHANNELS, vpos, speed, n)
            };
            let got = buf.len() / CHANNELS;
            if got == 0 {
                eof = true;
                continue;
            }
            for (i, pair) in buf.chunks_mut(CHANNELS).enumerate() {
                let at = varispeed_clip_secs(vpos + i as f64 * speed, speed, vs_in, vs_out);
                let fade = fade_envelope(at, src_in, clip_dur, fi_secs, fo_secs, fi_curve, fo_curve, ac);
                let mult = if is_muted { 0.0 } else { vol * fade };
                pair.iter_mut().for_each(|s| *s *= mult);
            }
            if let Some(ref mut chain) = effect_chain {
                let fx_ctx = AudioProcessContext {
                    sample_rate: SAMPLE_RATE,
                    channels: CHANNELS as u32,
                    clip_time_secs: (varispeed_clip_secs(vpos, speed, vs_in, vs_out) - src_in).max(0.0),
                };
                chain.process_aligned(&mut buf, &fx_ctx);
            }
            if needs_resample {
                let out = resampler.get_or_insert_with(|| PlaybackResampler::new(output_rate)).process(&buf);
                producer.push_slice(&out);
            } else {
                producer.push_slice(&buf);
            }
            vpos += got as f64 * speed;
            if got < n {
                eof = true;
            }
            continue;
        }

        if read_pos >= total_floats {
            eof = true;
            continue;
        }

        if !needs_resample {
            // Fast path: PCM is already at output_rate (48kHz == 48kHz)
            let remaining = total_floats - read_pos;
            let to_push = chunk_floats.min(remaining).min(vacant).min(to_stop.saturating_mul(CHANNELS));

            if is_muted && effect_chain.is_none() {
                let zeros = vec![0.0f32; to_push];
                producer.push_slice(&zeros);
            } else if effect_chain.is_none() && !has_fade && (vol - 1.0).abs() < 0.01 {
                producer.push_slice(&samples[read_pos..read_pos + to_push]);
            } else {
                let mut scaled: Vec<f32> = samples[read_pos..read_pos + to_push]
                    .chunks(CHANNELS)
                    .enumerate()
                    .flat_map(|(i, pair)| {
                        let frame_idx = (read_pos / CHANNELS) + i;
                        let pos_secs = frame_idx as f64 / SAMPLE_RATE as f64;
                        let fade = fade_envelope(pos_secs, src_in, clip_dur, fi_secs, fo_secs, fi_curve, fo_curve, ac);
                        // Muted audio still runs through the chain (as
                        // silence) so its alignment survives the mute.
                        let mult = if is_muted { 0.0 } else { vol * fade };
                        pair.iter().map(move |s| s * mult)
                    })
                    .collect();
                // Apply audio effects chain after volume/fade.
                // clip_time_secs = source position - clip's source_in, so
                // keyframe tracks store time relative to clip start (matches
                // the video keyframe convention in keyframe.rs:18-19).
                if let Some(ref mut chain) = effect_chain {
                    let chunk_start_src_secs = (read_pos / CHANNELS) as f64 / SAMPLE_RATE as f64;
                    let fx_ctx = AudioProcessContext {
                        sample_rate: SAMPLE_RATE,
                        channels: CHANNELS as u32,
                        clip_time_secs: (chunk_start_src_secs - src_in).max(0.0),
                    };
                    // Aligned: effects that look ahead would otherwise
                    // play late against the picture by their latency.
                    chain.process_aligned(&mut scaled, &fx_ctx);
                }
                producer.push_slice(&scaled);
            }
            read_pos += to_push;
        } else {
            // Slow path: the device runs at another rate (44.1 kHz is
            // common). Volume, fades and effects run at 48 kHz exactly as
            // on the fast path; the result then goes through a windowed-
            // sinc resampler to the device rate. Picking the nearest
            // source sample instead aliased everything that played.
            let resampler = resampler.get_or_insert_with(|| PlaybackResampler::new(output_rate));
            // Whole source frames, as many as fit in the ring once converted.
            // Two frames of headroom: a chunk can come out one frame longer
            // than the ratio says, and the ring would drop what does not fit.
            let fit = ((vacant / CHANNELS).saturating_sub(2) as f64 * rate_ratio) as usize;
            let frames = chunk_frames.min(fit).min((total_floats - read_pos) / CHANNELS).min(to_stop);
            if frames == 0 {
                eof = read_pos + CHANNELS > total_floats;
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            let to_take = frames * CHANNELS;
            let mut scaled: Vec<f32> = samples[read_pos..read_pos + to_take]
                .chunks(CHANNELS)
                .enumerate()
                .flat_map(|(i, pair)| {
                    let frame_idx = (read_pos / CHANNELS) + i;
                    let pos_secs = frame_idx as f64 / SAMPLE_RATE as f64;
                    let fade = fade_envelope(pos_secs, src_in, clip_dur, fi_secs, fo_secs, fi_curve, fo_curve, ac);
                    let mult = if is_muted { 0.0 } else { vol * fade };
                    pair.iter().map(move |s| s * mult)
                })
                .collect();
            if let Some(ref mut chain) = effect_chain {
                let chunk_start_src_secs = (read_pos / CHANNELS) as f64 / SAMPLE_RATE as f64;
                let fx_ctx = AudioProcessContext {
                    sample_rate: SAMPLE_RATE,
                    channels: CHANNELS as u32,
                    clip_time_secs: (chunk_start_src_secs - src_in).max(0.0),
                };
                chain.process_aligned(&mut scaled, &fx_ctx);
            }
            let out = resampler.process(&scaled);
            producer.push_slice(&out);
            read_pos += to_take;
        }
    }
}

/// Stereo 48 kHz → device-rate conversion for the playback feed, aligned:
/// the resampler's own delay (well under a millisecond) is dropped from the
/// head of the output once, so what plays lines up with the source
/// position, as the effect chain's latency is (`process_aligned`).
struct PlaybackResampler {
    channels: Vec<klipp_audio::dsp::resample::Resampler>,
    trim_frames: usize,
    planar_out: Vec<Vec<f32>>,
}

impl PlaybackResampler {
    fn new(output_rate: u32) -> Self {
        let make = || klipp_audio::dsp::resample::Resampler::new(SAMPLE_RATE, output_rate)
            .expect("48 kHz to a device rate is a supported conversion");
        // Pad the delay onto a whole device sample (in the resampler's own
        // fine steps), so trimming it leaves no fractional offset.
        let to_out = output_rate as f64 / SAMPLE_RATE as f64;
        let probe = make();
        let delay_out = probe.delay() * to_out;
        let whole = delay_out.ceil();
        let steps = ((whole - delay_out) / (probe.delay_step() * to_out)).round() as u64;
        let channels: Vec<_> = (0..CHANNELS).map(|_| make().with_extra_delay(steps)).collect();
        Self { trim_frames: whole as usize, channels, planar_out: vec![Vec::new(); CHANNELS] }
    }

    /// Convert interleaved stereo 48 kHz to interleaved device-rate audio.
    fn process(&mut self, interleaved: &[f32]) -> Vec<f32> {
        let frames = interleaved.len() / CHANNELS;
        for (ch, r) in self.channels.iter_mut().enumerate() {
            let input: Vec<f32> = (0..frames).map(|i| interleaved[i * CHANNELS + ch]).collect();
            self.planar_out[ch].clear();
            r.push(&input, &mut self.planar_out[ch]);
        }
        let n = self.planar_out.iter().map(Vec::len).min().unwrap_or(0);
        let skip = self.trim_frames.min(n);
        self.trim_frames -= skip;
        let mut out = Vec::with_capacity((n - skip) * CHANNELS);
        for i in skip..n {
            for ch in 0..CHANNELS {
                out.push(self.planar_out[ch][i]);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_pcm(path: &Path, samples: &[f32]) {
        std::fs::write(path, bytemuck::cast_slice::<f32, u8>(samples)).unwrap();
    }

    /// #572: a cut reopened every track's PCM cache from disk on the
    /// engine thread. A second open of an unchanged file shares the copy
    /// in memory; a rewritten one is read again.
    #[test]
    fn pcm_cache_shares_an_unchanged_file_and_rereads_a_changed_one() {
        let path = std::env::temp_dir().join(format!("klipp-pcm-cache-test-{}.pcm", std::process::id()));
        write_pcm(&path, &[0.25; 8]);
        let first = load_pcm(&path).unwrap();
        let second = load_pcm(&path).unwrap();
        assert!(Arc::ptr_eq(&first, &second), "an unchanged file is read once");

        write_pcm(&path, &[0.5; 12]);
        let third = load_pcm(&path).unwrap();
        assert!(!Arc::ptr_eq(&first, &third));
        assert_eq!(&third[..], &[0.5; 12]);
        let cache = PCM_CACHE.lock().unwrap();
        assert_eq!(cache.entries.iter().filter(|e| e.path == path).count(), 1, "the stale copy is dropped");
        drop(cache);

        // A closed project's sound is let go; a feeder's copy lives on.
        release_all();
        let cache = PCM_CACHE.lock().unwrap();
        assert!(cache.entries.iter().all(|e| e.path != path));
        drop(cache);
        assert_eq!(&third[..], &[0.5; 12]);
        assert!(!Arc::ptr_eq(&third, &load_pcm(&path).unwrap()), "read again after the release");
        let _ = std::fs::remove_file(&path);
    }

    /// #2136: opening a long source's feeder does not wait for its
    /// samples. The engine opens a clip's sound ahead of its cut on its
    /// own thread; a 35-minute film's cache read whole there took 0.85 s.
    /// Opened, the feeder plays from a seek ten minutes in once the file
    /// is read, on its own thread.
    #[test]
    fn a_long_source_opens_without_waiting_for_its_samples() {
        use ringbuf::traits::Consumer;
        let path = std::env::temp_dir().join(format!("klipp-pcm-long-{}.pcm", std::process::id()));
        // Twelve minutes, 276 MB: reading it takes far longer than a frame.
        let frames = SAMPLE_RATE as usize * 60 * 12;
        let mut samples = vec![0.25f32; frames * CHANNELS];
        let at = SAMPLE_RATE as usize * 600;
        samples[at * CHANNELS..(at + SAMPLE_RATE as usize) * CHANNELS].fill(0.75);
        write_pcm(&path, &samples);
        drop(samples);
        release_all();
        let t = std::time::Instant::now();
        let (feeder, mut consumer) = PcmAudioFeeder::open(&path, SAMPLE_RATE, vec![], Default::default()).unwrap();
        let opened = t.elapsed();
        feeder.set_fade(0.0, f64::MAX, 0.0, 0.0, 0, 0, 1.0);
        feeder.anti_click.store(false, Ordering::Relaxed);
        feeder.seek(600.0);
        feeder.resume();
        assert!(opened < Duration::from_millis(15), "open waited {opened:?} for the file");
        let mut first = None;
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut buf = vec![0.0f32; 4096];
        while first.is_none() && std::time::Instant::now() < deadline {
            let n = consumer.pop_slice(&mut buf);
            first = buf[..n].first().copied();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(first, Some(0.75), "plays from ten minutes in");
        let _ = std::fs::remove_file(&path);
    }

    /// #642: a retimed clip's feeder reads its sound at its speed: from
    /// a seek to 1 s, at 2x each output frame is two source frames on, and
    /// reversed each is one back. The PCM is a ramp, each frame its own
    /// index, so the output says which source frame it is.
    #[test]
    fn a_feeder_reads_at_its_clips_speed_both_ways() {
        use ringbuf::traits::Consumer;
        let path = std::env::temp_dir().join(format!("klipp-pcm-varispeed-{}.pcm", std::process::id()));
        let frames = SAMPLE_RATE as usize * 4;
        let ramp: Vec<f32> = (0..frames).flat_map(|i| [i as f32 / frames as f32; CHANNELS]).collect();
        write_pcm(&path, &ramp);
        let first_frames = |speed: f64| -> Vec<f64> {
            let (feeder, mut consumer) = PcmAudioFeeder::open(&path, SAMPLE_RATE, vec![], Default::default()).unwrap();
            feeder.set_fade(0.0, f64::MAX, 0.0, 0.0, 0, 0, 1.0);
            feeder.anti_click.store(false, Ordering::Relaxed);
            feeder.set_varispeed(speed, 0.5, 3.5, false);
            feeder.seek(1.0);
            feeder.resume();
            let mut got = Vec::new();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut buf = vec![0.0f32; 4096];
            while got.len() < 64 && std::time::Instant::now() < deadline {
                let n = consumer.pop_slice(&mut buf);
                got.extend(buf[..n].chunks(CHANNELS).map(|f| f[0] as f64 * frames as f64));
                std::thread::sleep(Duration::from_millis(5));
            }
            got.truncate(64);
            got
        };
        let fast = first_frames(2.0);
        assert!((fast[0] - SAMPLE_RATE as f64).abs() < 1.0, "starts at the seek: frame {}", fast[0]);
        assert!(fast.windows(2).all(|w| (w[1] - w[0] - 2.0).abs() < 0.01), "2x steps two frames: {:?}", &fast[..4]);
        let back = first_frames(-1.0);
        assert!(back.windows(2).all(|w| (w[1] - w[0] + 1.0).abs() < 0.01), "reversed steps one back: {:?}", &back[..4]);
        assert_eq!(first_frames(1.0).windows(2).filter(|w| (w[1] - w[0] - 1.0).abs() > 0.01).count(), 0, "1x as ever");
        let _ = std::fs::remove_file(&path);
    }

    /// #2780: a clip's reverb rings past its out point in playback as in
    /// the export, a fade-out going in before it in both. Measured: the
    /// feeder's samples against the export's, the tail included, and the
    /// feed ending where the tail does.
    #[test]
    fn a_tail_plays_as_the_export_renders_it() {
        use ringbuf::traits::Consumer;
        // Three seconds of a 440 Hz tone; the clip is its first second.
        let tone: Vec<f32> = (0..SAMPLE_RATE as usize * 3).flat_map(|i| [0.25 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / SAMPLE_RATE as f32).sin(); CHANNELS]).collect();
        let dir = klipp_testing::scratch::scratch_dir("klipp-pcm-tail");
        let pcm = dir.join("tone.pcm");
        write_pcm(&pcm, &tone);
        // The same samples as a float WAV, for the export to decode.
        let wav = dir.join("tone.wav");
        let data: &[u8] = bytemuck::cast_slice(&tone);
        let mut file = Vec::new();
        file.extend(b"RIFF");
        file.extend((36 + data.len() as u32).to_le_bytes());
        file.extend(b"WAVEfmt ");
        file.extend(16u32.to_le_bytes());
        file.extend(3u16.to_le_bytes());
        file.extend((CHANNELS as u16).to_le_bytes());
        file.extend(SAMPLE_RATE.to_le_bytes());
        file.extend((SAMPLE_RATE * 8).to_le_bytes());
        file.extend(8u16.to_le_bytes());
        file.extend(32u16.to_le_bytes());
        file.extend(b"data");
        file.extend((data.len() as u32).to_le_bytes());
        file.extend(data);
        std::fs::write(&wav, file).unwrap();
        let reverb = klipp_core::AudioEffectInstance { effect_type: "reverb".into(), enabled: true, wet_dry: 1.0, params: vec![] };

        // Playback: the clip's envelope, its sound stopping at its out point.
        let (feeder, mut consumer) = PcmAudioFeeder::open(&pcm, SAMPLE_RATE, vec![reverb.clone()], Default::default()).unwrap();
        feeder.set_fade(0.0, 1.0, 0.0, 0.5, 0, 0, 1.0);
        feeder.ring.set_stops(true);
        feeder.seek(0.0);
        feeder.resume();
        let mut played = Vec::new();
        let mut buf = vec![0.0f32; 8192];
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut quiet_since = std::time::Instant::now();
        while std::time::Instant::now() < deadline && quiet_since.elapsed() < Duration::from_millis(500) {
            let n = consumer.pop_slice(&mut buf);
            if n > 0 {
                played.extend_from_slice(&buf[..n]);
                quiet_since = std::time::Instant::now();
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let tail = feeder.ring.tail_secs().expect("the feed reached the out point");

        // The export of the same clip, alone on a six-second timeline.
        let clip = klipp_render::snapshot::AudioClipSnapshot {
            timeline_start_frame: 0,
            timeline_end_frame: 25,
            source_start_secs: 0.0,
            path: wav,
            audio_effects: vec![reverb],
            audio_effect_keyframes: Default::default(),
            source_range: (0.0, 1.0),
            shape: klipp_render::snapshot::AudioClipShape { clip_duration_secs: 1.0, fade_out_secs: 0.5, ..Default::default() },
            varispeed: None,
            exact_secs: None,
        };
        let snapshot = klipp_render::snapshot::SequenceSnapshot {
            fps: 25.0,
            fps_num: 25,
            fps_den: 1,
            width: 64,
            height: 64,
            sample_rate: SAMPLE_RATE,
            duration_frames: 150,
            export_range: None,
            video: Default::default(),
            audio_tracks: vec![klipp_render::snapshot::AudioTrackSnapshot { volume: 1.0, pan: 0.0, muted: false, solo: false, clips: vec![clip], automation: Default::default(), effects: Vec::new() }],
            pan_law: Default::default(),
            global_lut_path: None,
            subtitle_tracks: vec![],
            chapters: vec![],
            working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
            grade_encoding: klipp_core::GradeEncoding::Bt1886,
        };
        let mut mix = klipp_render::AudioMixdown::new(&snapshot).unwrap();
        let exported: Vec<f32> = (0..150).flat_map(|f| mix.get_frame_audio(f)).collect();

        let out = SAMPLE_RATE as usize * CHANNELS;
        assert!(tail > 1.0 && tail < 3.0, "the default reverb's tail: {tail} s");
        assert_eq!(played.len(), out + (tail * SAMPLE_RATE as f64).round() as usize * CHANNELS, "the clip, then its tail, then nothing");
        let loud = |s: &[f32]| s.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(loud(&played[out..out + 4800]) > 0.01, "it rings past the out point: {}", loud(&played[out..out + 4800]));
        let worst = played.iter().zip(&exported).map(|(p, e)| (p - e).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-5, "playback and the export differ by {worst}");
        assert!(loud(&exported[played.len()..]) < 1e-4, "the export is silent past the tail");
    }

    /// Where a varispeed read is in its clip: forward, the source run at
    /// the speed from its in point; reversed, back from its out point.
    #[test]
    fn a_varispeed_position_is_the_clips_own_time() {
        let at = |secs: f64| secs * SAMPLE_RATE as f64;
        assert!((varispeed_clip_secs(at(3.0), 2.0, 1.0, 5.0) - 2.0).abs() < 1e-9, "2x: source 3 is a second in");
        assert!((varispeed_clip_secs(at(4.0), -1.0, 1.0, 5.0) - 2.0).abs() < 1e-9, "reversed: source 4 is a second in");
        assert!((varispeed_clip_secs(at(0.5), 2.0, 1.0, 5.0) - 1.0).abs() < 1e-9, "before the clip: its start");
    }

    /// #421: the slow path converts to the device rate with a windowed
    /// sinc and trims the conversion's delay, so a tone arrives on time and
    /// clean; nearest-neighbour picking left an error around -20 dB.
    #[test]
    fn playback_resampler_delivers_a_clean_tone_on_time_at_44_1_khz() {
        let out_rate = 44_100u32;
        let hz = 1000.0f64;
        let tone: Vec<f32> = (0..SAMPLE_RATE as usize)
            .flat_map(|i| {
                let v = ((2.0 * std::f64::consts::PI * hz * i as f64 / SAMPLE_RATE as f64).sin() * 0.5) as f32;
                [v, v]
            })
            .collect();
        let mut r = PlaybackResampler::new(out_rate);
        let mut out = Vec::new();
        for block in tone.chunks(2048 * CHANNELS) {
            out.extend(r.process(block));
        }
        let frames = out.len() / CHANNELS;
        assert!((frames as i64 - out_rate as i64).abs() < 40, "{frames} frames for one second");
        let (mut err, mut sig) = (0.0f64, 0.0f64);
        for n in 2000..frames - 100 {
            let want = (2.0 * std::f64::consts::PI * hz * n as f64 / out_rate as f64).sin() * 0.5;
            err += (out[n * CHANNELS] as f64 - want).powi(2);
            sig += want * want;
        }
        let db = 10.0 * (err / sig).log10();
        assert!(db < -40.0, "the tone should arrive on time and clean, residual {db:.1} dB");
    }
}
