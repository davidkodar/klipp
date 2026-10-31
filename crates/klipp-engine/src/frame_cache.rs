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

/// Frames read ahead of playback: a thread reads and decodes the frames
/// the engine asks for next. Holds the last few it read.
pub struct FrameReader {
    ask: std::sync::mpsc::Sender<(std::path::PathBuf, FrameAt)>,
    ready: Arc<Mutex<VecDeque<(FrameAt, Arc<Vec<f32>>)>>>,
}

/// Frames a [`FrameReader`] keeps.
const KEPT: usize = 12;

impl FrameReader {
    pub fn new() -> Self {
        let (ask, asked) = std::sync::mpsc::channel::<(std::path::PathBuf, FrameAt)>();
        let ready: Arc<Mutex<VecDeque<(FrameAt, Arc<Vec<f32>>)>>> = Arc::default();
        let held = Arc::clone(&ready);
        let _ = std::thread::Builder::new().name("klipp-frame-read".into()).spawn(move || {
            while let Ok((root, at)) = asked.recv() {
                if held.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|(a, _)| *a == at) {
                    continue;
                }
                if let Some(pixels) = crate::cache_store::read_stored(&root, NAMESPACE, at) {
                    let mut held = held.lock().unwrap_or_else(|e| e.into_inner());
                    held.push_back((at, Arc::new(pixels)));
                    while held.len() > KEPT {
                        held.pop_front();
                    }
                }
            }
        });
        FrameReader { ask, ready }
    }

    /// Read frame `at` ahead.
    pub fn ask(&self, root: &std::path::Path, at: FrameAt) {
        let _ = self.ask.send((root.to_path_buf(), at));
    }

    /// Frame `at`: read ahead, or read now.
    pub fn take(&self, root: &std::path::Path, at: FrameAt) -> Option<Arc<Vec<f32>>> {
        let found = self.ready.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(a, _)| *a == at).map(|(_, p)| Arc::clone(p));
        found.or_else(|| crate::cache_store::read_stored(root, NAMESPACE, at).map(Arc::new))
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
        match store.lock().unwrap_or_else(|e| e.into_inner()).put_frame(NAMESPACE, place, &pixels) {
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
        let mut src = source();
        let mut renderer = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        renderer.set_gpu_enabled(true);
        renderer.set_working_colorspace(req.working);
        let mut tally = FramesTally::default();
        let n = req.frames.len().max(1);
        'frames: for (i, at) in req.frames.iter().enumerate() {
            for size in &req.sizes {
                pause.wait_while_paused();
                if pause.is_cancelled() {
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
        done(tally);
    })
}
