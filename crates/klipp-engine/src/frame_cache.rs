//! Whole frames drawn ahead (#3212): kept in the cache store under
//! [`NAMESPACE`] by [`klipp_compose::freeze::frame_key`] at the size drawn,
//! and shown from there in place of composing, while their key holds.
//!
//! The key covers every visible track's content and what the stack lays
//! over it, frozen tracks' keys among them, so a frame drawn ahead is
//! current exactly as long as what is in it is. A frame is kept linear
//! and before the display transform (ten bits through a fixed curve, half
//! floats where it isn't opaque or runs past the curve), so another
//! monitor or HDR setting never makes it stale.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use klipp_compose::freeze::{frame_key, FreezeEnv};
use klipp_compose::{FrameSource, Renderer};
use klipp_core::Timecode;
use klipp_jobs::{JobPriority, ResourceClass, ThreadJob};
use klipp_timeline::Sequence;

use crate::cache_store::{CacheStore, FrameAt, PutRefused};

/// The store's namespace for whole frames.
pub const NAMESPACE: &str = "frames";

/// How a kept frame is put on screen: the SDR display encode's settings.
/// A frame read ahead under other settings isn't the one asked for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayEncode {
    pub tone: klipp_core::ToneMapOp,
    pub working: klipp_core::WorkingColorspace,
    pub transfer: klipp_core::SdrTransfer,
}

impl DisplayEncode {
    /// A stored frame's display bytes: a ten-bit one through a table where
    /// the encode works channel by channel, as the encode makes them.
    pub fn bytes(&self, stored: crate::cache_store::Stored) -> Vec<u8> {
        use crate::cache_store::Stored;
        let encode = |linear: &[f32]| klipp_color::display::display_transform_ws_dithered_to(linear, self.tone, self.working, self.transfer);
        match (stored, klipp_color::display::per_channel_display(self.tone, self.working, self.transfer)) {
            (Stored::TenBit(words), Some(display)) => crate::cache_store::ten_bit_display(&words, display),
            (Stored::TenBit(words), None) => encode(&crate::cache_store::unpack_ten_bit(&words)),
            (Stored::Linear(linear), _) => encode(&linear),
        }
    }

    /// [`Self::bytes`], a ten-bit frame of `size` made on the GPU where
    /// `gpu` is there and can (#3300): the same bytes. Whether the GPU
    /// made them.
    pub fn bytes_on(&self, stored: crate::cache_store::Stored, size: (u32, u32), gpu: Option<&mut klipp_effects::gpu::TenBitDisplay>) -> (Vec<u8>, bool) {
        use crate::cache_store::Stored;
        if let (Stored::TenBit(words), Some(gpu), Some(display)) = (&stored, gpu, klipp_color::display::per_channel_display(self.tone, self.working, self.transfer)) {
            if let Some(bytes) = crate::cache_store::ten_bit_display_on_gpu(gpu, words, size, display) {
                return (bytes, true);
            }
        }
        (self.bytes(stored), false)
    }
}

/// What loading a kept frame took, ms, a running mean over the frames
/// read: the parts on the reading thread, and their sum.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
pub struct LoadCosts {
    pub read_ms: f32,
    pub lz4_ms: f32,
    pub unpack_ms: f32,
    pub encode_ms: f32,
    pub total_ms: f32,
    pub frames: u64,
    /// Of `frames`, those whose display bytes the GPU made (#3300).
    pub on_gpu: u64,
}

static LOAD_COSTS: Mutex<LoadCosts> = Mutex::new(LoadCosts { read_ms: 0.0, lz4_ms: 0.0, unpack_ms: 0.0, encode_ms: 0.0, total_ms: 0.0, frames: 0, on_gpu: 0 });

/// What loading a kept frame has taken, over the frames read so far.
pub fn load_costs() -> LoadCosts {
    *LOAD_COSTS.lock().unwrap_or_else(|e| e.into_inner())
}

fn record_load(t: crate::cache_store::ReadTimes, encode_ms: f32, on_gpu: bool) {
    let mut c = LOAD_COSTS.lock().unwrap_or_else(|e| e.into_inner());
    // A running mean that settles on the last thirty or so frames.
    let k = 1.0 / (c.frames.min(29) + 1) as f32;
    let mix = |old: f32, new: f32| old + (new - old) * k;
    c.read_ms = mix(c.read_ms, t.read_ms);
    c.lz4_ms = mix(c.lz4_ms, t.lz4_ms);
    c.unpack_ms = mix(c.unpack_ms, t.unpack_ms);
    c.encode_ms = mix(c.encode_ms, encode_ms);
    c.total_ms = mix(c.total_ms, t.read_ms + t.lz4_ms + t.unpack_ms + encode_ms);
    c.frames += 1;
    c.on_gpu += u64::from(on_gpu);
}

/// Whether the GPU or the CPU makes a reader's display bytes (#3300):
/// the GPU's round trip (the words up, the bytes back) is quicker than
/// the CPU's table on a slow CPU with a near GPU (about 20 ms against a
/// few on an M1) and slower on a fast desktop (22 ms against 5 on a
/// 4080 machine, under load). Each is timed on the first frames and the
/// quicker kept, by its best time (the GPU's first frame makes its
/// buffers).
#[derive(Debug, Default)]
pub(crate) struct EncodeChoice {
    gpu_ms: Vec<f32>,
    cpu_ms: Vec<f32>,
}

/// Frames each way timed before choosing.
const TIMED: usize = 3;

impl EncodeChoice {
    /// Whether to try the GPU for the next frame.
    pub(crate) fn try_gpu(&self) -> bool {
        if self.gpu_ms.len() < TIMED {
            return true;
        }
        if self.cpu_ms.len() < TIMED {
            return false;
        }
        let best = |v: &[f32]| v.iter().copied().fold(f32::MAX, f32::min);
        best(&self.gpu_ms) < best(&self.cpu_ms)
    }

    /// What a frame took, and where.
    pub(crate) fn timed(&mut self, ms: f32, on_gpu: bool) {
        let v = if on_gpu { &mut self.gpu_ms } else { &mut self.cpu_ms };
        if v.len() < TIMED {
            v.push(ms);
        }
    }

    /// The GPU couldn't: the CPU's from now on.
    pub(crate) fn gpu_failed(&mut self) {
        self.gpu_ms = vec![f32::MAX; TIMED];
    }
}

/// A frame read ahead, ready to show: its display bytes.
struct Ready {
    at: FrameAt,
    encode: DisplayEncode,
    bytes: Vec<u8>,
}

/// Frames read ahead of playback (#3212): a thread reads each frame
/// asked for, decompresses and unpacks it and encodes it for the screen,
/// so the engine's thread only takes the bytes. A frame not ready when
/// it is wanted is drawn live: nothing is read on the engine's thread.
pub struct FrameReader {
    /// Let go on drop, which ends the thread: joined then, since it may
    /// hold the GPU (#3287).
    ask: Option<std::sync::mpsc::Sender<(std::path::PathBuf, FrameAt, DisplayEncode)>>,
    ready: Arc<Mutex<VecDeque<Ready>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Frames a [`FrameReader`] keeps ready.
const KEPT: usize = 8;

impl FrameReader {
    pub fn new() -> Self {
        let (ask, asked) = std::sync::mpsc::channel::<(std::path::PathBuf, FrameAt, DisplayEncode)>();
        let ready: Arc<Mutex<VecDeque<Ready>>> = Arc::default();
        let held = Arc::clone(&ready);
        let thread = std::thread::Builder::new().name("klipp-frame-read".into()).spawn(move || {
            // The GPU's encode, made at the first frame that wants it;
            // none where there's no GPU, and the CPU's encode then.
            let mut gpu: Option<Option<klipp_effects::gpu::TenBitDisplay>> = None;
            let mut choice = EncodeChoice::default();
            while let Ok(first) = asked.recv() {
                // The asks since the last frame, latest last: one read
                // each, the oldest dropped when they pile up behind.
                let mut asks = vec![first];
                asks.extend(asked.try_iter());
                let mut seen = std::collections::HashSet::new();
                let mut wanted: Vec<_> = asks.into_iter().rev().filter(|(_, at, _)| seen.insert(*at)).take(KEPT).collect();
                wanted.reverse();
                for (root, at, encode) in wanted {
                    if held.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|r| r.at == at && r.encode == encode) {
                        continue;
                    }
                    let Some((stored, times)) = crate::cache_store::read_stored_for_screen(&root, NAMESPACE, at) else { continue };
                    let t = std::time::Instant::now();
                    let ten_bit = matches!(stored, crate::cache_store::Stored::TenBit(_));
                    let gpu = gpu.get_or_insert_with(|| klipp_effects::gpu_context::GpuContext::shared().and_then(klipp_effects::gpu::TenBitDisplay::new));
                    let tried = choice.try_gpu() && gpu.is_some();
                    let (bytes, on_gpu) = encode.bytes_on(stored, at.size, gpu.as_mut().filter(|_| tried));
                    let ms = t.elapsed().as_secs_f32() * 1000.0;
                    if ten_bit {
                        if tried && !on_gpu {
                            choice.gpu_failed();
                        } else {
                            choice.timed(ms, on_gpu);
                        }
                    }
                    record_load(times, ms, on_gpu);
                    let mut held = held.lock().unwrap_or_else(|e| e.into_inner());
                    held.push_back(Ready { at, encode, bytes });
                    while held.len() > KEPT {
                        held.pop_front();
                    }
                }
            }
        });
        FrameReader { ask: Some(ask), ready, thread: thread.ok() }
    }

    /// Read frame `at` ahead, for the screen as `encode` puts it there.
    pub fn ask(&self, root: &std::path::Path, at: FrameAt, encode: DisplayEncode) {
        if let Some(ask) = &self.ask {
            let _ = ask.send((root.to_path_buf(), at, encode));
        }
    }

    /// Frame `at`'s display bytes, when read ahead under `encode`: taken,
    /// not copied. `None` when it isn't ready yet.
    pub fn take(&self, at: FrameAt, encode: DisplayEncode) -> Option<Vec<u8>> {
        let mut held = self.ready.lock().unwrap_or_else(|e| e.into_inner());
        let i = held.iter().position(|r| r.at == at && r.encode == encode)?;
        held.remove(i).map(|r| r.bytes)
    }
}

impl Drop for FrameReader {
    fn drop(&mut self) {
        self.ask = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Default for FrameReader {
    fn default() -> Self {
        Self::new()
    }
}

/// What keys a whole frame: the identities of the files and plug-ins in
/// it, and the render environment ([`crate::freeze::environment`]).
pub struct Keying {
    pub media: Arc<HashMap<String, u64>>,
    pub environment: u64,
}

impl Keying {
    /// Frame `at`'s place in the store at `size`.
    pub fn place(&self, seq: &Sequence, at: Timecode, size: (u32, u32)) -> FrameAt {
        let media = |id: &str| self.media.get(id).copied();
        let env = FreezeEnv { sequences: &[], media: &media, plugin: &crate::freeze::plugin_identity, environment: self.environment };
        FrameAt { tick: at.value, key: frame_key(seq, at, &env), size }
    }
}

/// What drawing frames ahead is asked to do.
pub struct FrameRequest {
    /// The sequence as playback draws it.
    pub seq: Arc<Sequence>,
    pub frames: Vec<Timecode>,
    /// The sizes each frame is kept at: playing, and parked when that
    /// differs.
    pub sizes: Vec<(u32, u32)>,
    pub working: klipp_core::WorkingColorspace,
    pub keying: Keying,
    /// Asked between frames: true stops the run, as a cancel does. What
    /// stops drawing ahead on the person's input within a frame, without
    /// waiting on the engine's loop.
    pub stop: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    /// Make room by letting go of the oldest frames kept (a range asked
    /// for), or stop when full (drawing ahead while idle).
    pub evict: bool,
}

/// What a run drew.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FramesTally {
    pub stored: usize,
    /// Already kept under their key.
    pub current: usize,
    pub failed: usize,
    /// The frames gone through, of [`FrameRequest::frames`].
    pub reached: usize,
    pub full: bool,
    pub cancelled: bool,
}

/// How one frame at one size went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameOutcome {
    Stored,
    Current,
    Full,
    Failed(String),
}

impl FrameRequest {
    /// Draw frame `at` at `size` into `store` unless it is kept already:
    /// on the GPU stack, on the CPU where the stack declines.
    pub fn draw(&self, renderer: &mut Renderer, store: &Mutex<CacheStore>, at: Timecode, size: (u32, u32), src: &mut dyn FrameSource) -> FrameOutcome {
        let place = self.keying.place(&self.seq, at, size);
        if store.lock().unwrap_or_else(|e| e.into_inner()).contains(NAMESPACE, place) {
            return FrameOutcome::Current;
        }
        let (w, h) = size;
        let pixels = match renderer.render_linear_on_gpu(&self.seq, at, w, h, src, klipp_compose::RenderOptions::default()) {
            Some(p) => p,
            None => renderer.render(&self.seq, at, w, h, src, klipp_compose::RenderOptions::default()),
        };
        let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
        let put = if self.evict { store.put_frame(NAMESPACE, place, &pixels) } else { store.put_frame_within_cap(NAMESPACE, place, &pixels) };
        match put {
            Ok(()) => FrameOutcome::Stored,
            Err(PutRefused::Full) => FrameOutcome::Full,
            Err(why) => FrameOutcome::Failed(why.to_string()),
        }
    }
}

/// Draw `req`'s frames ahead in a background job, a frame (at each size)
/// at a time: waiting while the scheduler pauses it, stopping between
/// frames when cancelled with every kept frame whole. `source` makes the
/// job's pictures on its own thread; `done` hears the tally. The job's
/// progress is the share of frames gone through.
pub fn spawn_frames<S: FrameSource + 'static>(req: FrameRequest, store: Arc<Mutex<CacheStore>>, source: impl FnOnce() -> S + Send + 'static, done: impl FnOnce(FramesTally) + Send + 'static) -> Arc<ThreadJob> {
    ThreadJob::spawn("Render frames ahead", JobPriority::Background, ResourceClass::GpuMixed, move |pause, progress| {
        // Told once its decoders and renderer are let go (#3287).
        let tally = {
            let mut src = source();
            let mut renderer = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            renderer.set_gpu_enabled(true);
            renderer.set_working_colorspace(req.working);
            let mut tally = FramesTally::default();
            let n = req.frames.len().max(1);
            'frames: for (i, at) in req.frames.iter().enumerate() {
                for size in &req.sizes {
                    pause.wait_while_paused();
                    if pause.is_cancelled() || req.stop.as_ref().is_some_and(|stop| stop()) {
                        tally.cancelled = true;
                        break 'frames;
                    }
                    match req.draw(&mut renderer, &store, *at, *size, &mut src) {
                        FrameOutcome::Stored => tally.stored += 1,
                        FrameOutcome::Current => tally.current += 1,
                        FrameOutcome::Failed(why) => {
                            tracing::warn!("frames ahead: at {:.3} s: {why}", at.to_secs_f64());
                            tally.failed += 1;
                        }
                        FrameOutcome::Full => {
                            tally.full = true;
                            break 'frames;
                        }
                    }
                }
                tally.reached = i + 1;
                progress.set((i + 1) as f32 / n as f32);
            }
            tally
        };
        done(tally);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #3300: the GPU is timed first, then the CPU; the quicker is kept,
    /// by its best time, and a GPU that can't is given up.
    #[test]
    fn the_quicker_encode_is_kept() {
        let mut slow_gpu = EncodeChoice::default();
        for ms in [40.0, 22.0, 21.0] {
            assert!(slow_gpu.try_gpu());
            slow_gpu.timed(ms, true);
        }
        for ms in [6.0, 5.0, 5.5] {
            assert!(!slow_gpu.try_gpu(), "the CPU timed next");
            slow_gpu.timed(ms, false);
        }
        assert!(!slow_gpu.try_gpu(), "a fast CPU keeps its encode");
        let mut quick_gpu = EncodeChoice::default();
        for ms in [40.0, 4.0, 4.5] {
            quick_gpu.timed(ms, true);
        }
        for ms in [20.0, 19.0, 21.0] {
            quick_gpu.timed(ms, false);
        }
        assert!(quick_gpu.try_gpu(), "a slow CPU hands it to the GPU, its first frame's 40 ms aside");
        quick_gpu.gpu_failed();
        assert!(!quick_gpu.try_gpu());
    }
}
