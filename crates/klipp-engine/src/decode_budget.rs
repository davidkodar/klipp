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

/// What one software thread is taken to deliver for a class of stream
/// not measured yet, in pixels a second, frames on the queue: decoded,
/// repacked and uploaded. Under a heavy stretch's load a 4K 8-bit H.264
/// stream on four threads delivered 18–20 frames a second, about 40 a
/// thread, though its decoder alone had read 241. HEVC,
/// AV1 and deep streams cost about twice that.
pub const DEFAULT_DELIVERED_PER_THREAD: f64 = 40e6;
const DEFAULT_DELIVERED_PER_THREAD_HEAVY: f64 = 20e6;

/// A busy window never sets the budget below this share of the most the
/// hardware decoders were seen to decode this session: they gave that
/// much, so a window that read less was held back by something else (a
/// render loading the GPU the frames are converted on).
const PEAK_FLOOR: f64 = 0.9;

/// Windows in a row a stream moved to software must deliver less than it
/// asks, decoding flat out, before its class is held on the GPU.
const SHORT_WINDOWS: u32 = 2;

/// How far each busy window after the first moves the budget toward what
/// it measured.
const EMA_WEIGHT: f64 = 0.3;

/// A kind of stream, as far as software decoding it costs: its codec,
/// whether its samples are deeper than 8 bits, and its size. A software
/// rate measured on one class says nothing of another: 1080p H.264
/// stood in for 4K and moved a stream software could not keep up with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Class {
    pub codec: Codec,
    pub deep: bool,
    pub band: SizeBand,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
    ProRes,
    #[default]
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum SizeBand {
    /// Up to 1920×1080's pixels.
    #[default]
    Hd,
    /// Up to 4096×2160's.
    Uhd,
    Larger,
}

impl Class {
    /// The class of a stream of `codec` (the probe's name) at `width` ×
    /// `height`.
    pub fn of(codec: &str, deep: bool, width: u32, height: u32) -> Class {
        let codec = match codec.to_ascii_lowercase().as_str() {
            "h264" | "avc" | "avc1" => Codec::H264,
            "hevc" | "h265" => Codec::Hevc,
            "av1" => Codec::Av1,
            "prores" => Codec::ProRes,
            _ => Codec::Other,
        };
        let px = width as u64 * height as u64;
        let band = if px <= 1920 * 1080 { SizeBand::Hd } else if px <= 4096 * 2160 { SizeBand::Uhd } else { SizeBand::Larger };
        Class { codec, deep, band }
    }

    fn default_per_thread(&self) -> f64 {
        if self.deep || matches!(self.codec, Codec::Hevc | Codec::Av1) { DEFAULT_DELIVERED_PER_THREAD_HEAVY } else { DEFAULT_DELIVERED_PER_THREAD }
    }
}

/// Whether a file's samples are deeper than 8 bits, from the probe's bit
/// depth, else its pixel format's name (`yuv420p10le`); `None` when the
/// probe said neither.
pub fn deep_samples(bit_depth: Option<u8>, pixel_format: Option<&str>) -> Option<bool> {
    match (bit_depth, pixel_format) {
        (Some(d), _) => Some(d >= 10),
        (None, Some(pf)) => Some(["p10", "p12", "p14", "p16", "10le", "12le", "16le", "10be", "12be", "16be"].iter().any(|t| pf.contains(t))),
        (None, None) => None,
    }
}

/// What software decoding delivers, by class: measured where a stream of
/// the class was moved and decoded flat out, the class's default until
/// then; nothing for a class a moved stream fell short on.
#[derive(Default)]
pub struct SoftwareRates {
    rates: std::collections::HashMap<Class, (f64, bool)>,
}

impl SoftwareRates {
    /// What a thread delivers for `class`, pixels a second; 0 when a
    /// stream of it fell short, so none of it moves again.
    pub fn per_thread(&self, class: Class) -> f64 {
        match self.rates.get(&class) {
            Some((_, true)) => 0.0,
            Some((rate, false)) => *rate,
            None => class.default_per_thread(),
        }
    }

    /// A stream of `class` delivered `per_thread` a thread, flat out. The
    /// first measure replaces the default; later ones move by average.
    pub fn note(&mut self, class: Class, per_thread: f64) -> f64 {
        let e = self.rates.entry(class).or_insert((f64::NAN, false));
        e.0 = if e.0.is_nan() { per_thread } else { e.0 * (1.0 - EMA_WEIGHT) + per_thread * EMA_WEIGHT };
        e.0
    }

    /// A stream of `class` moved to software could not deliver its rate.
    pub fn fell_short(&mut self, class: Class) {
        let e = self.rates.entry(class).or_insert((class.default_per_thread(), false));
        e.1 = true;
    }
}

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
    class: Class,
    /// Moved to software and, in its last window, short of its rate:
    /// nothing else moves while one is.
    short: bool,
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
    /// What software delivers, by class.
    classes: SoftwareRates,
    /// The most the hardware decoders were seen to decode, pixels a
    /// second, in any window this session.
    peak: f64,
    /// Where the current window started, and the counters there.
    window: Option<(Instant, u64, u64)>,
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Registry { next: 1, entries: Vec::new(), budget: DEFAULT_BUDGET, measured: false, software_per_thread: DEFAULT_SOFTWARE_PER_THREAD, software_measured: false, classes: SoftwareRates::default(), peak: 0.0, window: None }))
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
    admit_class(rate, Class::default(), priority)
}

/// [`admit`] for a stream of `class`: it moves to software only if what
/// software was measured delivering for its class (or the class's
/// default) holds its rate, never while another moved stream is short.
pub fn admit_class(rate: f64, class: Class, priority: Priority) -> Lease {
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    let decoding = if priority == Priority::Overflow && enabled() && crate::video_decoder::hardware_decode_enabled() {
        let gpu_demand: f64 = r.entries.iter().filter(|e| e.on_gpu).map(|e| e.rate).sum();
        let used: usize = r.entries.iter().map(|e| e.threads).sum();
        let per_thread = if r.entries.iter().any(|e| e.short) { 0.0 } else { r.classes.per_thread(class) };
        choose(rate, gpu_demand, r.budget, used, software_thread_cap(), per_thread)
    } else {
        Decoding::Gpu
    };
    let id = r.next;
    r.next += 1;
    let (on_gpu, threads) = match decoding {
        Decoding::Gpu => (true, 0),
        Decoding::Software { threads } => (false, threads),
    };
    r.entries.push(Entry { id, rate, on_gpu, threads, class, short: false });
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

/// The decoder `id`, moved off the GPU and decoding flat out, delivered
/// `per_thread` pixels a second on each of its threads: frames on its
/// queue, repacked and uploaded, not what its decoder alone read. Kept
/// for its class; `short` when that was less than the stream asks, and
/// [`SHORT_WINDOWS`] such in a row hold its class on the GPU.
pub fn note_software_rate(id: u64, per_thread: f64, short_windows: u32) {
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    let Some(i) = r.entries.iter().position(|e| e.id == id) else { return };
    let class = r.entries[i].class;
    let was = r.software_per_thread;
    let now = r.classes.note(class, per_thread);
    r.software_per_thread = if r.software_measured { was * (1.0 - EMA_WEIGHT) + per_thread * EMA_WEIGHT } else { per_thread };
    r.software_measured = true;
    r.entries[i].short = short_windows > 0;
    if short_windows >= SHORT_WINDOWS {
        r.classes.fell_short(class);
        tracing::info!("decode overflow: a {class:?} stream in software delivered {:.0} of the {:.0} Mpx/s it asks; its kind stays on the GPU from now", per_thread * r.entries[i].threads as f64 / 1e6, r.entries[i].rate / 1e6);
    }
    tracing::debug!("decode overflow: a software thread delivered {:.0} Mpx/s ({class:?} now {:.0})", per_thread / 1e6, now / 1e6);
}

/// One decode thread's measure of what it decodes and waits (#2116): a
/// hardware decoder's frames and waits are the GPU's measure, and a
/// software decoder decoding flat out measures what it delivers.
pub struct Meter {
    on_gpu: bool,
    /// The threads of a software decoder moved off the GPU.
    threads: Option<usize>,
    /// What the stream asks, pixels a second: a software window short of
    /// it counts toward holding its class on the GPU.
    rate: f64,
    start: Instant,
    pixels: u64,
    idle: Duration,
    /// Software windows in a row short of `rate`.
    short: u32,
}

impl Meter {
    pub fn new(on_gpu: bool, threads: Option<usize>, rate: f64) -> Self {
        Self::at(on_gpu, threads, rate, Instant::now())
    }

    fn at(on_gpu: bool, threads: Option<usize>, rate: f64, start: Instant) -> Self {
        Self { on_gpu, threads, rate, start, pixels: 0, idle: Duration::ZERO, short: 0 }
    }

    /// The stream asks `rate` pixels a second, once its file is read.
    pub fn set_rate(&mut self, rate: f64) {
        self.rate = rate;
    }

    /// The thread waited `d`: a full queue, paused, at the end.
    pub fn idle(&mut self, d: Duration) {
        if self.on_gpu {
            note_gpu_idle(d);
        }
        self.idle += d;
    }

    /// The thread spent `d` downloading and converting a hardware frame:
    /// work on the GPU's compute, which a render loads, not its decoder.
    /// Time the decoder is not what holds this thread, so not counted as
    /// the decoder running flat out.
    pub fn converted(&mut self, d: Duration) {
        if self.on_gpu {
            note_gpu_idle(d);
        }
    }

    /// The decoder gave a frame of `pixels`, those a seek decodes and
    /// skips too: the GPU decoder's work.
    pub fn decoded(&mut self, pixels: u64) {
        if self.on_gpu {
            note_gpu_frame(pixels);
        }
    }

    /// A seek's first frame reached the queue: the delivered measure
    /// starts again here, the frames decoded on the way to it not counted.
    pub fn seeked(&mut self) {
        self.seeked_at(Instant::now());
    }

    fn seeked_at(&mut self, now: Instant) {
        *self = Self { short: self.short, ..Self::at(self.on_gpu, self.threads, self.rate, now) };
    }

    /// A frame of `pixels` reached the queue. A software decoder's window
    /// closes past [`WINDOW`]: decoding flat out (waiting less than
    /// [`BUSY_IDLE_SHARE`] of it), it answers what each thread delivered
    /// and how many windows in a row that fell short of its rate.
    pub fn delivered(&mut self, pixels: u64) -> Option<(f64, u32)> {
        self.delivered_at(pixels, Instant::now())
    }

    fn delivered_at(&mut self, pixels: u64, now: Instant) -> Option<(f64, u32)> {
        self.pixels += pixels;
        let span = now.duration_since(self.start);
        if span < WINDOW {
            return None;
        }
        let busy = self.idle.as_secs_f64() < BUSY_IDLE_SHARE * span.as_secs_f64();
        let measured = self.threads.filter(|_| busy).map(|threads| {
            let total = self.pixels as f64 / span.as_secs_f64();
            self.short = if total < self.rate { self.short + 1 } else { 0 };
            (total / threads as f64, self.short)
        });
        *self = Self { short: self.short, ..Self::at(self.on_gpu, self.threads, self.rate, now) };
        measured
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
    r.peak = r.peak.max(rate);
    if let Some(budget) = calibrated(r.budget, r.measured, rate, busy, r.peak) {
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
/// the GPU's limit itself, sets the budget (the first) or moves it down,
/// and never below [`PEAK_FLOOR`] of `peak`, the most seen decoded: a
/// first busy window read 820 under a heavy render where 2,170 had just
/// been decoded, and moved every stream after it to software.
pub fn calibrated(budget: f64, measured: bool, rate: f64, busy: bool, peak: f64) -> Option<f64> {
    if rate <= 0.0 {
        return None;
    }
    let next = match (busy, measured) {
        (true, false) => rate.max(PEAK_FLOOR * peak),
        (true, true) => (budget * (1.0 - EMA_WEIGHT) + rate * EMA_WEIGHT).max(rate.min(budget)).max(PEAK_FLOOR * peak),
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

/// Whether the GPU's free memory is what frames are counted against
/// (#3174): wherever frames are held on the GPU, decoded there or decoded
/// in software and uploaded, not only with hardware decode chosen. A
/// machine without a decoder Klipp can use uploads its frames all the
/// same, and they were counted against nothing there.
pub fn reads_free_vram(frames_on_gpu: bool) -> bool {
    frames_on_gpu
}

/// What the GPU has free for new memory, in bytes (#2128): where a stream
/// whose frames are held on it keeps its queue of frames. `None` where
/// frames don't stay on the GPU (GPU frames off) or its memory can't be
/// read; its frames are then counted in [`free_memory`]. Read on a thread
/// of its own every `VRAM_REFRESH`, as reading NVIDIA's takes a
/// program's start: the engine never waits for it.
pub fn free_vram() -> Option<f64> {
    static FREE: AtomicU64 = AtomicU64::new(u64::MAX);
    static READER: OnceLock<()> = OnceLock::new();
    if !reads_free_vram(crate::video_decoder::should_attach_gpu_source()) {
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
    let out = match std::process::Command::new("nvidia-smi").args(["--query-gpu=memory.free", "--format=csv,noheader,nounits"]).output() {
        Ok(out) => out,
        Err(e) => {
            // No AMD figure and no nvidia-smi (a Flatpak's sandbox, a
            // machine without NVIDIA's tools): decoders are budgeted
            // without the GPU's free memory. Said once.
            static SAID: std::sync::Once = std::sync::Once::new();
            SAID.call_once(|| tracing::info!("the GPU's free memory isn't read: no sysfs figure, and nvidia-smi can't run ({e})"));
            return None;
        }
    };
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

    /// #3174: frames held on the GPU are counted against its memory
    /// whatever decodes them; the hardware decode choice is not asked.
    #[test]
    fn the_gpu_budget_follows_where_frames_are_held() {
        assert!(reads_free_vram(true));
        assert!(!reads_free_vram(false));
    }

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
        assert_eq!(calibrated(DEFAULT_BUDGET, false, 200.0 * K4, true, 0.0), Some(200.0 * K4));
        assert_eq!(calibrated(DEFAULT_BUDGET, false, 60.0 * K4, true, 0.0), Some(60.0 * K4));
    }

    #[test]
    fn later_busy_windows_move_it_toward_theirs() {
        let up = calibrated(150.0 * K4, true, 200.0 * K4, true, 0.0).expect("busy");
        assert!(up > 150.0 * K4 && up < 200.0 * K4, "{up}");
        let down = calibrated(150.0 * K4, true, 60.0 * K4, true, 0.0).expect("busy");
        assert!(down < 150.0 * K4 && down > 60.0 * K4, "{down}");
    }

    #[test]
    fn any_window_decoding_more_than_the_budget_raises_it() {
        // Waiting on full queues, yet decoding more than the budget: the
        // GPU gives at least that.
        assert_eq!(calibrated(DEFAULT_BUDGET, false, 1.5 * DEFAULT_BUDGET, false, 0.0), Some(1.5 * DEFAULT_BUDGET));
        assert_eq!(calibrated(DEFAULT_BUDGET, true, 1.5 * DEFAULT_BUDGET, false, 0.0), Some(1.5 * DEFAULT_BUDGET));
    }

    #[test]
    fn an_idle_window_under_the_budget_says_nothing() {
        assert_eq!(calibrated(DEFAULT_BUDGET, false, 50.0 * K4, false, 0.0), None);
        assert!(!is_busy(0.5, 0.5, 4), "waiting on full queues");
        assert!(!is_busy(0.0, 0.5, 1), "one decoder alone");
        assert!(is_busy(0.05, 0.5, 2));
    }

    #[test]
    fn the_budget_has_a_floor() {
        assert_eq!(calibrated(DEFAULT_BUDGET, false, 1.0, true, 0.0), Some(MIN_BUDGET));
    }

    /// A hardware decoder's thread spending its frame downloading and
    /// converting on a GPU a render loads is not its decoder running flat
    /// out: that time counts as the decoder waiting.
    #[test]
    fn a_window_spent_converting_is_not_the_decoder_flat_out() {
        let before = GPU_IDLE_US.load(Relaxed);
        let mut m = Meter::new(true, None, 30.0 * K4);
        m.converted(Duration::from_micros(18_600));
        assert!(GPU_IDLE_US.load(Relaxed) - before >= 18_600, "converting is not decoding");
        // Two decoders, each converting 18.6 ms of each 33 ms frame:
        // over half the window not on the decoder.
        assert!(!is_busy(2.0 * 0.5 * 18.6 / 33.3, 0.5, 2));
    }

    /// The first busy window, read under a heavy render, does not take the
    /// budget below what the hardware decoders were seen to decode: 820
    /// after 2,170 moved every stream after it to software.
    #[test]
    fn a_busy_window_does_not_drop_the_budget_far_below_the_peak() {
        let (peak, low) = (2170e6, 820e6);
        let after = calibrated(peak, false, low, true, peak).expect("busy");
        assert!(after >= PEAK_FLOOR * peak, "{:.0}", after / 1e6);
        let later = calibrated(after, true, low, true, peak).expect("busy");
        assert!(later >= PEAK_FLOOR * peak, "{:.0}", later / 1e6);
        // With nothing seen above it, a busy window still sets it.
        assert_eq!(calibrated(DEFAULT_BUDGET, false, low, true, low), Some(low));
    }

    /// What a software stream delivers is frames on its queue, the seek's
    /// burst of decoded and skipped frames not counted: the decoder read
    /// 241 a thread at the seek, the queue got 18–20 4K frames a second.
    #[test]
    fn a_software_stream_is_measured_by_what_it_delivers() {
        let t0 = Instant::now();
        let mut m = Meter::at(false, Some(4), 30.0 * K4, t0);
        // The seek: 58 frames decoded in half a second, none delivered.
        for _ in 0..58 {
            m.decoded(K4 as u64);
        }
        let ms = |n: u64| t0 + Duration::from_millis(n);
        m.seeked_at(ms(500));
        // Then 20 frames a second for a second, flat out.
        let mut got = None;
        for f in 1..=20u64 {
            got = got.or(m.delivered_at(K4 as u64, ms(500 + f * 50)));
        }
        let (per_thread, short) = got.expect("a window closed");
        let total = per_thread * 4.0;
        assert!((total - 20.0 * K4).abs() < 0.15 * 20.0 * K4, "{:.0} Mpx/s delivered", total / 1e6);
        assert_eq!(short, 1, "20 frames a second is short of 30");
    }

    /// A 4K 30 H.264 stream, 249 Mpx/s, stays on the GPU, over its budget,
    /// when its class delivered 160 on four threads: software would play
    /// it at 18–20.
    #[test]
    fn a_stream_software_cannot_hold_stays_on_the_gpu_over_budget() {
        let k4_h264 = Class::of("h264", false, 3840, 2160);
        let mut rates = SoftwareRates::default();
        rates.note(k4_h264, 40e6);
        assert_eq!(choose(30.0 * K4, 1600e6, 2022e6, 0, 16, rates.per_thread(k4_h264)), Decoding::Gpu);
        // Unmeasured, its default does not move it either.
        assert_eq!(choose(30.0 * K4, 1600e6, 2022e6, 0, 16, SoftwareRates::default().per_thread(k4_h264)), Decoding::Gpu);
        // A 1080p 25 one, 52 Mpx/s, fits its class's default.
        let hd = Class::of("h264", false, 1920, 1080);
        assert_eq!(choose(1920.0 * 1080.0 * 25.0, 1600e6, 1650e6, 0, 16, rates.per_thread(hd)), Decoding::Software { threads: 4 });
    }

    /// A class a moved stream fell short on is not moved again, however
    /// its average reads.
    #[test]
    fn a_class_that_fell_short_is_never_moved_again() {
        let hd = Class::of("h264", false, 1920, 1080);
        let mut rates = SoftwareRates::default();
        rates.note(hd, 100e6);
        rates.fell_short(hd);
        rates.note(hd, 200e6);
        assert_eq!(rates.per_thread(hd), 0.0);
        assert_eq!(choose(1920.0 * 1080.0 * 25.0, 1600e6, 1650e6, 0, 16, rates.per_thread(hd)), Decoding::Gpu);
    }

    /// A 10-bit file is deep by its probed depth, whatever its colour
    /// space: an SDR log file is classed as the heavier decode it is.
    #[test]
    fn a_files_depth_comes_from_its_probe() {
        assert_eq!(deep_samples(Some(8), Some("yuv420p")), Some(false));
        assert_eq!(deep_samples(Some(10), None), Some(true));
        assert_eq!(deep_samples(None, Some("yuv422p10le")), Some(true));
        assert_eq!(deep_samples(None, Some("yuv420p")), Some(false));
        assert_eq!(deep_samples(None, None), None);
    }

    /// A rate measured on 1080p H.264 says nothing of 4K, nor of HEVC.
    #[test]
    fn a_1080p_measure_does_not_move_a_4k_stream() {
        let (hd, k4, hevc) = (Class::of("h264", false, 1920, 1080), Class::of("h264", false, 3840, 2160), Class::of("hevc", true, 3840, 2160));
        let mut rates = SoftwareRates::default();
        rates.note(hd, 241e6);
        assert_eq!(rates.per_thread(k4), DEFAULT_DELIVERED_PER_THREAD);
        assert_eq!(rates.per_thread(hevc), DEFAULT_DELIVERED_PER_THREAD_HEAVY);
        assert_eq!(choose(30.0 * K4, 1600e6, 2022e6, 0, 16, rates.per_thread(k4)), Decoding::Gpu);
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
