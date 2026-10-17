//! Which decoders run in software when the GPU's decoder is full (#2116).
//!
//! The GPU's video decoder is one engine shared by every stream. Four 4K
//! layers playing while a fifth seeks from a keyframe seconds back asked
//! it for more frames than it gives (about 200 4K frames a second on the
//! machine measured), and every layer fell behind together: the picture
//! froze for most of a second at each cut where a layer came up. The CPU
//! decodes 4K faster than that with its cores free, so a stream opened
//! while the GPU's decoder is full decodes in software instead.
//!
//! The choice is made once, as a stream opens; a stream never moves
//! between the two while it plays. Every open decoder is counted here by
//! its pixel rate (width × height × frames a second): one asking for more
//! than the budget left goes to software, while the threads software
//! decoding may take, half the machine's, last.
//!
//! The budget is measured, not known: while two or more hardware decoders
//! all decode flat out (none waiting on a full queue or paused), the
//! pixels they decode a second are what the GPU's decoder gives. The
//! first such window sets the budget; later ones move it toward theirs.
//! Until one is measured, a stream is moved only past [`DEFAULT_BUDGET`].
//!
//! Export's decoders (klipp-render's) are not counted and never moved:
//! an export does not push live streams to software, nor they it. While
//! one decodes on the GPU, no window is measured.
//!
//! On unless `KLIPP_DECODE_OVERFLOW=0`, which has every stream ask the
//! GPU, to compare. At the Stress Test's 22 s cut, where four 4K layers
//! play while another opens, its worst second played at 4–15 frames and
//! the picture waited 0.56–0.70 s; moved, at 24–25 and 0.10 s.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The budget before anything is measured, in pixels a second: 120 4K
/// frames. A starting point only, replaced by the first busy window.
pub const DEFAULT_BUDGET: f64 = 120.0 * 3840.0 * 2160.0;

/// The budget never falls below this, however a window measured: 30 1080p
/// frames a second, which any hardware decoder gives.
const MIN_BUDGET: f64 = 30.0 * 1920.0 * 1080.0;

/// A stream opening seeks first, from the keyframe before its time, and
/// decodes the frames up to it as fast as it can while the others play:
/// counted at this many times its rate as it asks.
const OPENING_WEIGHT: f64 = 2.0;

/// How long a window of measured throughput is.
const WINDOW: Duration = Duration::from_millis(500);

/// A window counts as the GPU's decoder running flat out when its hardware
/// decoders waited less than this share of it.
const BUSY_IDLE_SHARE: f64 = 0.1;

/// How far each busy window after the first moves the budget toward what
/// it measured.
const EMA_WEIGHT: f64 = 0.3;

/// How a decoder decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoding {
    /// On the GPU when it can, as every decoder did before #2116.
    Gpu,
    /// In software, with this many threads.
    Software { threads: usize },
}

/// Who is asking, which decides who goes to software first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    /// The primary track's decoder, skims and anything else that always
    /// asks the GPU: counted, never moved.
    Fixed,
    /// A layer's stream, a stream opened ahead, or the preroll: moved to
    /// software when the GPU's decoder is full.
    Overflow,
}

struct Entry {
    id: u64,
    /// Pixels a second it asks for.
    rate: f64,
    /// Decoding on the GPU: counted against the budget.
    on_gpu: bool,
    /// Software threads it was given; 0 for one the ladder put on
    /// software itself (no GPU), which is not ours to count.
    threads: usize,
}

struct Registry {
    next: u64,
    entries: Vec<Entry>,
    budget: f64,
    /// A busy window has set `budget`.
    measured: bool,
    /// Where the current window started, and the counters there.
    window: Option<(Instant, u64, u64)>,
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Registry { next: 1, entries: Vec::new(), budget: DEFAULT_BUDGET, measured: false, window: None }))
}

/// Pixels the hardware decoders decoded, every frame counted, those a seek
/// decodes and skips too.
static GPU_PIXELS: AtomicU64 = AtomicU64::new(0);
/// Microseconds the hardware decoders spent waiting: queue full, paused,
/// at the end.
static GPU_IDLE_US: AtomicU64 = AtomicU64::new(0);

/// Set by [`set_enabled`]: 0 unset (the environment decides), 1 off, 2 on.
static ENABLED: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

fn enabled() -> bool {
    static FROM_ENV: OnceLock<bool> = OnceLock::new();
    match ENABLED.load(Relaxed) {
        0 => *FROM_ENV.get_or_init(|| std::env::var("KLIPP_DECODE_OVERFLOW").as_deref() != Ok("0")),
        v => v == 2,
    }
}

/// Turn moving streams to software on or off, over the environment.
pub fn set_enabled(on: bool) {
    ENABLED.store(if on { 2 } else { 1 }, Relaxed);
}

/// Half the machine's threads: all software decoding together takes at
/// most these, so the engine, the UI and audio keep the rest.
pub fn software_thread_cap() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get()) / 2
}

/// Each software stream's threads: the cap shared four ways, at least two.
pub fn threads_per_stream(cap: usize) -> usize {
    (cap / 4).max(2)
}

/// The choice for a stream asking for `rate` pixels a second (counted at
/// `OPENING_WEIGHT` times that as it opens) while `gpu_demand` is already
/// asked of the GPU and `threads_used` software threads are taken.
pub fn choose(rate: f64, gpu_demand: f64, budget: f64, threads_used: usize, thread_cap: usize) -> Decoding {
    let threads = threads_per_stream(thread_cap);
    if gpu_demand + rate * OPENING_WEIGHT <= budget || threads_used + threads > thread_cap {
        Decoding::Gpu
    } else {
        Decoding::Software { threads }
    }
}

/// A decoder's place in the count, given back when it drops.
#[derive(Debug)]
pub struct Lease {
    id: u64,
    pub decoding: Decoding,
}

impl Lease {
    pub fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
        r.entries.retain(|e| e.id != self.id);
    }
}

/// Count a decoder asking for `rate` pixels a second, choosing how it
/// decodes. Callers admit in the order they want the GPU kept: the first
/// keep it, the later go to software. Layers are admitted bottom first,
/// so the top-most go first.
// The smallest on screen would be the better one to move: a layer drawn
// small loses nothing to software, and its scaled frames are cheap. Not
// done yet: what a layer covers is the renderer's to say.
pub fn admit(rate: f64, priority: Priority) -> Lease {
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    let decoding = if priority == Priority::Overflow && enabled() && crate::video_decoder::hardware_decode_enabled() {
        let gpu_demand: f64 = r.entries.iter().filter(|e| e.on_gpu).map(|e| e.rate).sum();
        let used: usize = r.entries.iter().map(|e| e.threads).sum();
        choose(rate, gpu_demand, r.budget, used, software_thread_cap())
    } else {
        Decoding::Gpu
    };
    let id = r.next;
    r.next += 1;
    let (on_gpu, threads) = match decoding {
        Decoding::Gpu => (true, 0),
        Decoding::Software { threads } => (false, threads),
    };
    r.entries.push(Entry { id, rate, on_gpu, threads });
    if let Decoding::Software { threads } = decoding {
        tracing::info!("decode overflow: a stream of {:.0} Mpx/s decodes in software on {threads} threads (GPU asked {:.0} of {:.0} Mpx/s)", rate / 1e6, r.entries.iter().filter(|e| e.on_gpu).map(|e| e.rate).sum::<f64>() / 1e6, r.budget / 1e6);
    }
    Lease { id, decoding }
}

/// The decoder `id` was asked for on the GPU but its codec opened in
/// software (no GPU, or none for this codec): it no longer counts.
pub fn opened_in_software(id: u64) {
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(e) = r.entries.iter_mut().find(|e| e.id == id) {
        e.on_gpu = false;
    }
}

/// The decoder `id` reads frames of its file's size at its rate: `rate`
/// pixels a second, which replaces what it was admitted with.
pub fn set_rate(id: u64, rate: f64) {
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(e) = r.entries.iter_mut().find(|e| e.id == id) {
        e.rate = rate;
    }
}

/// A hardware decoder decoded a frame of `pixels`.
pub fn note_gpu_frame(pixels: u64) {
    GPU_PIXELS.fetch_add(pixels, Relaxed);
}

/// A hardware decoder waited `d`.
pub fn note_gpu_idle(d: Duration) {
    GPU_IDLE_US.fetch_add(d.as_micros() as u64, Relaxed);
}

/// Close the measuring window when it is long enough: a busy one moves the
/// budget toward what it measured. Called as streams are synced.
pub fn sample() {
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    let (pixels, idle) = (GPU_PIXELS.load(Relaxed), GPU_IDLE_US.load(Relaxed));
    let Some((start, p0, i0)) = r.window else {
        r.window = Some((now, pixels, idle));
        return;
    };
    let span = now.duration_since(start);
    if span < WINDOW {
        return;
    }
    r.window = Some((now, pixels, idle));
    // An export decoding on the GPU too: its share is not counted here,
    // so the window would measure the GPU as smaller than it is, and
    // move live streams to software for as long as the budget recovers.
    if klipp_render::hardware_decoders_open() > 0 {
        return;
    }
    let on_gpu = r.entries.iter().filter(|e| e.on_gpu).count();
    let rate = pixels.saturating_sub(p0) as f64 / span.as_secs_f64();
    if let Some(budget) = calibrated(r.measured.then_some(r.budget), rate, idle.saturating_sub(i0) as f64 / 1e6, span.as_secs_f64(), on_gpu) {
        tracing::debug!("decode overflow: GPU measured {:.0} Mpx/s busy; budget {:.0} → {:.0} Mpx/s", rate / 1e6, r.budget / 1e6, budget / 1e6);
        r.budget = budget;
        r.measured = true;
    }
}

/// The budget after a window of `span` seconds in which `decoders` hardware
/// decoders decoded `rate` pixels a second and waited `idle` seconds
/// together, from the `measured` one (`None` before the first, which the
/// window then sets outright); `None` when the window says nothing about
/// the GPU's limit (fewer than two decoders, or any of them waiting).
pub fn calibrated(measured: Option<f64>, rate: f64, idle: f64, span: f64, decoders: usize) -> Option<f64> {
    if decoders < 2 || idle > BUSY_IDLE_SHARE * span * decoders as f64 || rate <= 0.0 {
        return None;
    }
    let budget = match measured {
        Some(b) => b * (1.0 - EMA_WEIGHT) + rate * EMA_WEIGHT,
        None => rate,
    };
    Some(budget.max(MIN_BUDGET))
}

/// How many open decoders decode on the GPU and in software, for the
/// perf overlay.
pub fn counts() -> (usize, usize) {
    let r = registry().lock().unwrap_or_else(|e| e.into_inner());
    let gpu = r.entries.iter().filter(|e| e.on_gpu).count();
    (gpu, r.entries.len() - gpu)
}

/// The budget now, in pixels a second.
pub fn budget() -> f64 {
    registry().lock().unwrap_or_else(|e| e.into_inner()).budget
}

#[cfg(test)]
mod tests {
    use super::*;

    const K4: f64 = 3840.0 * 2160.0;

    #[test]
    fn a_stream_the_gpu_has_room_for_stays_on_it() {
        assert_eq!(choose(30.0 * K4, 60.0 * K4, 200.0 * K4, 0, 16), Decoding::Gpu);
    }

    #[test]
    fn a_stream_past_the_budget_goes_to_software() {
        // Four 4K layers at 30 asked, a fifth opening.
        assert_eq!(choose(30.0 * K4, 150.0 * K4, 200.0 * K4, 0, 16), Decoding::Software { threads: 4 });
    }

    #[test]
    fn software_threads_stop_at_the_cap() {
        assert_eq!(choose(30.0 * K4, 300.0 * K4, 200.0 * K4, 12, 16), Decoding::Software { threads: 4 });
        // Half the machine taken: the GPU it is, full or not.
        assert_eq!(choose(30.0 * K4, 300.0 * K4, 200.0 * K4, 16, 16), Decoding::Gpu);
        // A small machine still gets one, with two threads.
        assert_eq!(choose(30.0 * K4, 300.0 * K4, 200.0 * K4, 0, 2), Decoding::Software { threads: 2 });
        assert_eq!(choose(30.0 * K4, 300.0 * K4, 200.0 * K4, 2, 2), Decoding::Gpu);
    }

    #[test]
    fn the_first_busy_window_sets_the_budget() {
        assert_eq!(calibrated(None, 200.0 * K4, 0.01, 0.5, 4), Some(200.0 * K4));
        assert_eq!(calibrated(None, 60.0 * K4, 0.0, 0.5, 2), Some(60.0 * K4));
    }

    #[test]
    fn later_busy_windows_move_it_toward_theirs() {
        let up = calibrated(Some(150.0 * K4), 200.0 * K4, 0.01, 0.5, 4).expect("busy");
        assert!(up > 150.0 * K4 && up < 200.0 * K4, "{up}");
        let down = calibrated(Some(150.0 * K4), 60.0 * K4, 0.0, 0.5, 2).expect("busy");
        assert!(down < 150.0 * K4 && down > 60.0 * K4, "{down}");
    }

    #[test]
    fn an_idle_or_lone_window_says_nothing() {
        // Waiting on full queues: the GPU kept up, its limit is unknown.
        assert_eq!(calibrated(Some(DEFAULT_BUDGET), 50.0 * K4, 0.5, 0.5, 4), None);
        // One decoder alone is bound by itself, not the GPU's decoder.
        assert_eq!(calibrated(None, 50.0 * K4, 0.0, 0.5, 1), None);
    }

    #[test]
    fn the_budget_has_a_floor() {
        assert_eq!(calibrated(None, 1.0, 0.0, 0.5, 2), Some(MIN_BUDGET));
    }

    fn on_gpu(id: u64) -> Option<bool> {
        registry().lock().unwrap().entries.iter().find(|e| e.id == id).map(|e| e.on_gpu)
    }

    #[test]
    fn a_lease_is_counted_until_it_drops() {
        let fixed = admit(1.0, Priority::Fixed);
        assert_eq!(fixed.decoding, Decoding::Gpu);
        let id = fixed.id();
        assert_eq!(on_gpu(id), Some(true));
        opened_in_software(id);
        assert_eq!(on_gpu(id), Some(false));
        drop(fixed);
        assert_eq!(on_gpu(id), None);
    }
}
