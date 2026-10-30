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

/// What one software decoding thread is taken to decode, in pixels a
/// second, until a software stream is measured. On 4 threads ffmpeg decoded
/// a 97 Mb/s 10-bit HEVC 4K file at about 85 per thread, seeks included,
/// and 8-bit H.264 4K at 240 and more: the slow end, so a 4K 24 stream
/// fits on four threads and a 4096×2160 59.94 one does not.
pub const DEFAULT_SOFTWARE_PER_THREAD: f64 = 80e6;

/// A stream goes to software only if its threads decode this much more
/// than it asks: a decoder just keeping up falls behind at every seek.
const SOFTWARE_HEADROOM: f64 = 1.25;

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
    /// What a software decoding thread decodes, pixels a second, and
    /// whether a software stream has measured it.
    software_per_thread: f64,
    software_measured: bool,
    /// Where the current window started, and the counters there.
    window: Option<(Instant, u64, u64)>,
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Registry { next: 1, entries: Vec::new(), budget: DEFAULT_BUDGET, measured: false, software_per_thread: DEFAULT_SOFTWARE_PER_THREAD, software_measured: false, window: None }))
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
/// asked of the GPU and `threads_used` software threads are taken, with a
/// software thread decoding `per_thread` pixels a second.
///
/// A stream its threads cannot keep up with stays on the GPU, full or not:
/// a 4096×2160 59.94 file moved to four threads played the section around
/// it at 18–23 frames a second instead of 25–30. So the streams that move
/// are the smaller and slower ones.
pub fn choose(rate: f64, gpu_demand: f64, budget: f64, threads_used: usize, thread_cap: usize, per_thread: f64) -> Decoding {
    let threads = threads_per_stream(thread_cap);
    let fits = rate * SOFTWARE_HEADROOM <= threads as f64 * per_thread;
    if gpu_demand + rate * OPENING_WEIGHT <= budget || threads_used + threads > thread_cap || !fits {
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
        choose(rate, gpu_demand, r.budget, used, software_thread_cap(), r.software_per_thread)
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

/// A software decoder moved off the GPU, decoding flat out, decoded
/// `per_thread` pixels a second on each of its threads. The first such
/// measure replaces the default; later ones move it by moving average.
pub fn note_software_rate(per_thread: f64) {
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    let was = r.software_per_thread;
    r.software_per_thread = if r.software_measured { was * (1.0 - EMA_WEIGHT) + per_thread * EMA_WEIGHT } else { per_thread };
    r.software_measured = true;
    tracing::debug!("decode overflow: a software thread measured {:.0} Mpx/s; {:.0} → {:.0}", per_thread / 1e6, was / 1e6, r.software_per_thread / 1e6);
}

/// One decode thread's measure of what it decodes and waits (#2116): a
/// hardware decoder's frames and waits are the GPU's measure, and a
/// software decoder decoding flat out measures what its threads give.
pub struct Meter {
    on_gpu: bool,
    /// The threads of a software decoder moved off the GPU.
    threads: Option<usize>,
    start: Instant,
    pixels: u64,
    idle: Duration,
}

impl Meter {
    pub fn new(on_gpu: bool, threads: Option<usize>) -> Self {
        Self { on_gpu, threads, start: Instant::now(), pixels: 0, idle: Duration::ZERO }
    }

    /// The thread waited `d`: a full queue, paused, at the end.
    pub fn idle(&mut self, d: Duration) {
        if self.on_gpu {
            note_gpu_idle(d);
        }
        self.idle += d;
    }

    /// The thread decoded a frame of `pixels`, those a seek skips too.
    pub fn frame(&mut self, pixels: u64) {
        if self.on_gpu {
            note_gpu_frame(pixels);
        }
        self.pixels += pixels;
        let span = self.start.elapsed();
        if span < WINDOW {
            return;
        }
        if let Some(threads) = self.threads.filter(|_| self.idle.as_secs_f64() < BUSY_IDLE_SHARE * span.as_secs_f64()) {
            note_software_rate(self.pixels as f64 / span.as_secs_f64() / threads as f64);
        }
        *self = Self::new(self.on_gpu, self.threads);
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
    let busy = is_busy(idle.saturating_sub(i0) as f64 / 1e6, span.as_secs_f64(), on_gpu);
    if let Some(budget) = calibrated(r.budget, r.measured, rate, busy) {
        tracing::debug!("decode overflow: GPU measured {:.0} Mpx/s{}; budget {:.0} → {:.0} Mpx/s", rate / 1e6, if busy { " busy" } else { "" }, r.budget / 1e6, budget / 1e6);
        r.budget = budget;
        r.measured |= busy;
    }
}

/// Whether a window of `span` seconds in which `decoders` hardware
/// decoders waited `idle` seconds together had them all decoding flat
/// out: then what they decoded is the GPU's limit. One decoder alone is
/// bound by itself, not the GPU's decoder.
pub fn is_busy(idle: f64, span: f64, decoders: usize) -> bool {
    decoders >= 2 && idle <= BUSY_IDLE_SHARE * span * decoders as f64
}

/// The budget after a window in which the hardware decoders decoded `rate`
/// pixels a second, from `budget` (`measured` once a busy window set it);
/// `None` to keep it.
///
/// What the GPU decoded is never more than it gives, so any window
/// decoding more than the budget raises it there: live, decoders mostly
/// wait on full queues, and a busy window alone never came, which left
/// the starting guess in place for a whole session. Only a busy window,
/// the GPU's limit itself, sets the budget (the first) or moves it down.
pub fn calibrated(budget: f64, measured: bool, rate: f64, busy: bool) -> Option<f64> {
    if rate <= 0.0 {
        return None;
    }
    let next = match (busy, measured) {
        (true, false) => rate,
        (true, true) => (budget * (1.0 - EMA_WEIGHT) + rate * EMA_WEIGHT).max(rate.min(budget)),
        (false, _) if rate > budget => rate,
        (false, _) => return None,
    };
    Some(next.max(MIN_BUDGET))
}

/// The pixels a second the viewer's streams may ask for together (#2128):
/// what the GPU's decoder gives, and what software decoding takes on when
/// it is full. Hardware decode off, software alone, on every thread.
pub fn stream_capacity() -> f64 {
    let r = registry().lock().unwrap_or_else(|e| e.into_inner());
    if !crate::video_decoder::hardware_decode_enabled() {
        return 2.0 * software_thread_cap().max(1) as f64 * r.software_per_thread;
    }
    let software = if enabled() { software_thread_cap() as f64 * r.software_per_thread } else { 0.0 };
    r.budget + software
}

/// What the system has free for new memory, in bytes: `MemAvailable`,
/// read as it is asked, or [`UNKNOWN_FREE_MEMORY`] where it cannot be
/// read. What limits how many frames the viewer's streams queue (#2128).
pub fn free_memory() -> f64 {
    std::fs::read_to_string("/proc/meminfo").ok().as_deref().and_then(mem_available).unwrap_or(UNKNOWN_FREE_MEMORY)
}

/// How often the GPU's free memory is read again.
const VRAM_REFRESH: Duration = Duration::from_secs(2);

/// What the GPU has free for new memory, in bytes (#2128): where a stream
/// decoded on it keeps its queue of frames. `None` where frames don't stay
/// on the GPU (hardware decode or GPU frames off) or its memory can't be
/// read; its frames are then counted in [`free_memory`]. Read on a thread
/// of its own every `VRAM_REFRESH`, as reading NVIDIA's takes a
/// program's start: the engine never waits for it.
pub fn free_vram() -> Option<f64> {
    static FREE: AtomicU64 = AtomicU64::new(u64::MAX);
    static READER: OnceLock<()> = OnceLock::new();
    if !crate::video_decoder::hardware_decode_enabled() || !crate::video_decoder::should_attach_gpu_source() {
        return None;
    }
    READER.get_or_init(|| {
        let spawned = std::thread::Builder::new().name("vram-free".into()).spawn(|| loop {
            FREE.store(read_free_vram().map_or(u64::MAX, f64::to_bits), Relaxed);
            std::thread::sleep(VRAM_REFRESH);
        });
        if let Err(e) = spawned {
            tracing::warn!("the GPU's free memory won't be read: {e}");
        }
    });
    match FREE.load(Relaxed) {
        u64::MAX => None,
        bits => Some(f64::from_bits(bits)),
    }
}

/// The GPU's free memory now, bytes: AMD's from sysfs, NVIDIA's from
/// `nvidia-smi`; the first card that says.
fn read_free_vram() -> Option<f64> {
    let cards = std::fs::read_dir("/sys/class/drm").ok()?.flatten().map(|e| e.path().join("device"));
    for dev in cards {
        let read = |name: &str| std::fs::read_to_string(dev.join(name)).ok();
        if let (Some(total), Some(used)) = (read("mem_info_vram_total"), read("mem_info_vram_used")) {
            if let Some(free) = sysfs_free(&total, &used) {
                return Some(free);
            }
        }
    }
    let out = std::process::Command::new("nvidia-smi").args(["--query-gpu=memory.free", "--format=csv,noheader,nounits"]).output().ok()?;
    out.status.success().then(|| nvidia_smi_free(&String::from_utf8_lossy(&out.stdout))).flatten()
}

/// Free bytes from AMD's `mem_info_vram_total` and `mem_info_vram_used`.
pub fn sysfs_free(total: &str, used: &str) -> Option<f64> {
    let (total, used) = (total.trim().parse::<f64>().ok()?, used.trim().parse::<f64>().ok()?);
    (total > 0.0).then(|| (total - used).max(0.0))
}

/// Free bytes from `nvidia-smi --query-gpu=memory.free`'s answer, MiB a
/// line, the first GPU's.
pub fn nvidia_smi_free(out: &str) -> Option<f64> {
    out.lines().next()?.trim().parse::<f64>().ok().map(|mib| mib * 1024.0 * 1024.0)
}

/// Free memory taken where `/proc/meminfo` cannot be read: 4 GiB.
pub const UNKNOWN_FREE_MEMORY: f64 = 4.0 * 1024.0 * 1024.0 * 1024.0;

/// `MemAvailable` in bytes from the text of `/proc/meminfo`.
pub fn mem_available(meminfo: &str) -> Option<f64> {
    let kb = meminfo.lines().find_map(|l| l.strip_prefix("MemAvailable:"))?.trim().strip_suffix("kB")?.trim().parse::<f64>().ok()?;
    Some(kb * 1024.0)
}

/// What the GPU's decoder has left, pixels a second: the budget less
/// what the decoders on it ask.
pub fn gpu_headroom() -> f64 {
    let r = registry().lock().unwrap_or_else(|e| e.into_inner());
    r.budget - r.entries.iter().filter(|e| e.on_gpu).map(|e| e.rate).sum::<f64>()
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

    #[test]
    fn free_memory_is_read_from_meminfo() {
        let meminfo = "MemTotal:       65536000 kB\nMemFree:         1000000 kB\nMemAvailable:   32000000 kB\n";
        assert_eq!(mem_available(meminfo), Some(32_000_000.0 * 1024.0));
        assert_eq!(mem_available("MemTotal: 1 kB\n"), None);
        assert!(free_memory() > 0.0);
    }

    /// The GPU's free memory, from AMD's sysfs counters or nvidia-smi's
    /// answer (#2128).
    #[test]
    fn free_vram_is_read_from_sysfs_or_nvidia_smi() {
        assert_eq!(sysfs_free("17163091968\n", "1163091968\n"), Some(16_000_000_000.0));
        assert_eq!(sysfs_free("0\n", "0\n"), None);
        assert_eq!(sysfs_free("x", "1"), None);
        assert_eq!(nvidia_smi_free("12034\n11000\n"), Some(12034.0 * 1024.0 * 1024.0));
        assert_eq!(nvidia_smi_free("[N/A]\n"), None);
    }

    const K4: f64 = 3840.0 * 2160.0;

    #[test]
    fn a_stream_the_gpu_has_room_for_stays_on_it() {
        assert_eq!(choose(30.0 * K4, 60.0 * K4, 200.0 * K4, 0, 16, 100e6), Decoding::Gpu);
    }

    #[test]
    fn a_stream_past_the_budget_goes_to_software() {
        // Four 4K layers at 30 asked, a fifth opening.
        assert_eq!(choose(30.0 * K4, 150.0 * K4, 200.0 * K4, 0, 16, 100e6), Decoding::Software { threads: 4 });
    }

    #[test]
    fn a_stream_its_threads_cannot_keep_up_with_stays_on_the_gpu() {
        // 4096×2160 at 59.94, 530 Mpx/s, on four threads of 60 Mpx/s.
        let big = 4096.0 * 2160.0 * 59.94;
        assert_eq!(choose(big, 600.0 * K4, 200.0 * K4, 0, 16, DEFAULT_SOFTWARE_PER_THREAD), Decoding::Gpu);
        // A 1080p 24 one, 50 Mpx/s, fits.
        let small = 1920.0 * 1080.0 * 24.0;
        assert_eq!(choose(small, 600.0 * K4, 200.0 * K4, 0, 16, DEFAULT_SOFTWARE_PER_THREAD), Decoding::Software { threads: 4 });
        // Measured faster, the big one fits too.
        assert_eq!(choose(big, 600.0 * K4, 200.0 * K4, 0, 16, 200e6), Decoding::Software { threads: 4 });
    }

    #[test]
    fn software_threads_stop_at_the_cap() {
        assert_eq!(choose(30.0 * K4, 300.0 * K4, 200.0 * K4, 12, 16, 100e6), Decoding::Software { threads: 4 });
        // Half the machine taken: the GPU it is, full or not.
        assert_eq!(choose(30.0 * K4, 300.0 * K4, 200.0 * K4, 16, 16, 100e6), Decoding::Gpu);
        // A small machine still gets one, with two threads.
        assert_eq!(choose(30.0 * K4, 300.0 * K4, 200.0 * K4, 0, 2, 200e6), Decoding::Software { threads: 2 });
        assert_eq!(choose(30.0 * K4, 300.0 * K4, 200.0 * K4, 2, 2, 200e6), Decoding::Gpu);
    }

    #[test]
    fn the_first_busy_window_sets_the_budget() {
        assert_eq!(calibrated(DEFAULT_BUDGET, false, 200.0 * K4, true), Some(200.0 * K4));
        assert_eq!(calibrated(DEFAULT_BUDGET, false, 60.0 * K4, true), Some(60.0 * K4));
    }

    #[test]
    fn later_busy_windows_move_it_toward_theirs() {
        let up = calibrated(150.0 * K4, true, 200.0 * K4, true).expect("busy");
        assert!(up > 150.0 * K4 && up < 200.0 * K4, "{up}");
        let down = calibrated(150.0 * K4, true, 60.0 * K4, true).expect("busy");
        assert!(down < 150.0 * K4 && down > 60.0 * K4, "{down}");
    }

    #[test]
    fn any_window_decoding_more_than_the_budget_raises_it() {
        // Waiting on full queues, yet decoding more than the budget: the
        // GPU gives at least that.
        assert_eq!(calibrated(DEFAULT_BUDGET, false, 1.5 * DEFAULT_BUDGET, false), Some(1.5 * DEFAULT_BUDGET));
        assert_eq!(calibrated(DEFAULT_BUDGET, true, 1.5 * DEFAULT_BUDGET, false), Some(1.5 * DEFAULT_BUDGET));
    }

    #[test]
    fn an_idle_window_under_the_budget_says_nothing() {
        assert_eq!(calibrated(DEFAULT_BUDGET, false, 50.0 * K4, false), None);
        assert!(!is_busy(0.5, 0.5, 4), "waiting on full queues");
        assert!(!is_busy(0.0, 0.5, 1), "one decoder alone");
        assert!(is_busy(0.05, 0.5, 2));
    }

    #[test]
    fn the_budget_has_a_floor() {
        assert_eq!(calibrated(DEFAULT_BUDGET, false, 1.0, true), Some(MIN_BUDGET));
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
