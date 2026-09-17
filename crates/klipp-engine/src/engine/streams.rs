//! Decoded frames for the viewer's renderer.
//!
//! The primary track's decoder drives playback: its frames set the
//! playhead, and it has its own clip-to-clip machinery (preroll, contiguous
//! cuts, audio). Every other media clip a timeline frame needs — an
//! overlay, a transition's other side running into its handle, a multicam
//! angle — gets a [`ClipStream`] here, keyed by clip, which follows its
//! clip's source time and re-seeks itself when that time jumps (a seek, a
//! loop, a step back). What a frame needs comes from
//! `klipp_compose::active_layers`, so the sources follow the renderer's
//! rules rather than restating them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use klipp_compose::SourcePixels;
use klipp_core::{ClipId, MediaId};
use tracing::{debug, warn};

use crate::types::{HdrInput, LinearFrame, SharedFrameQueue};
use crate::video_decoder::VideoDecoder;

/// A decoded frame the viewer keeps and may render from many times (a
/// paused frame is regraded on every slider tick). Conversions happen on
/// first use and are kept.
pub(crate) struct HeldFrame {
    pub width: u32,
    pub height: u32,
    pub pts: Option<f64>,
    pixels: Held,
    /// `Held::Srgb8` as linear light, once asked for.
    linear: OnceLock<Arc<Vec<f32>>>,
    /// `Held::Hdr` codes on the CPU, once asked for.
    codes: OnceLock<Arc<Vec<u32>>>,
    /// The caches' own record of this frame, when it came from them —
    /// carrying the ungraded picture they already made.
    pregrade: Option<super::Pregrade>,
}

enum Held {
    Linear(Arc<Vec<f32>>),
    Srgb8(Arc<Vec<u8>>),
    /// A frame the decoder left on the GPU.
    Gpu(Arc<GpuHeld>),
    Hdr(HdrInput),
}

/// An SDR frame the decoder left on the GPU as sRGB u8. The renderer seeds
/// a grade from it there; read back — once, as its sRGB bytes, a quarter of
/// a linear readback — when something needs it on the CPU.
struct GpuHeld {
    frame: Arc<crate::frame::GpuFrame>,
    srgb: OnceLock<Arc<Vec<u8>>>,
    linear: OnceLock<Arc<Vec<f32>>>,
}

impl GpuHeld {
    fn srgb(&self) -> Arc<Vec<u8>> {
        Arc::clone(self.srgb.get_or_init(|| Arc::new(crate::types::realise_srgb_u8_from_gpu(&self.frame))))
    }
}

impl klipp_compose::GpuPixels for GpuHeld {
    fn seed(&self, exec: &mut klipp_nodes::GraphExecutor, width: u32, height: u32) -> bool {
        if (self.frame.width, self.frame.height) == (width, height) {
            exec.prime_chain_from_gpu_u8_buffer(&self.frame.rgba_buffer, width, height)
        } else {
            // A source of another size — a 1440p clip over a 1080p
            // timeline — is fitted where it is, not read back to be.
            exec.prime_chain_fit_from_gpu_u8_buffer(&self.frame.rgba_buffer, self.frame.width, self.frame.height, width, height)
        }
    }

    fn read(&self) -> Arc<Vec<f32>> {
        Arc::clone(self.linear.get_or_init(|| Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&self.srgb()))))
    }
}

impl HeldFrame {
    fn new(pixels: Held, width: u32, height: u32, pts: Option<f64>) -> Self {
        Self { width, height, pts, pixels, linear: OnceLock::new(), codes: OnceLock::new(), pregrade: None }
    }

    fn gpu(g: Arc<crate::frame::GpuFrame>) -> Held {
        Held::Gpu(Arc::new(GpuHeld { frame: g, srgb: OnceLock::new(), linear: OnceLock::new() }))
    }

    /// A decoded frame whose picture is on the GPU, without taking it
    /// over: the frame stays with the caller for the paths it may still
    /// take. `None` for a frame with CPU or HDR pixels, which
    /// [`Self::from_frame`] would have to own.
    pub fn from_gpu_frame(f: &LinearFrame) -> Option<Self> {
        if f.hdr.is_some() || !f.data.is_empty() {
            return None;
        }
        let g = f.gpu_source.clone()?;
        Some(Self::new(Self::gpu(g), f.width, f.height, Some(f.pts_secs())))
    }

    /// A decoded frame, taken over.
    pub fn from_frame(f: LinearFrame) -> Self {
        let pts = Some(f.pts_secs());
        let pixels = match (f.hdr, f.data.is_empty(), f.gpu_source) {
            (Some(h), _, _) => Held::Hdr(h),
            (None, false, _) => Held::Linear(Arc::new(f.data)),
            (None, true, Some(g)) => Self::gpu(g),
            (None, true, None) => Held::Linear(Arc::new(Vec::new())),
        };
        Self::new(pixels, f.width, f.height, pts)
    }

    /// A copy of a frame still in a decoder's queue (frame blending peeks
    /// at the next one without taking it).
    pub fn copy_of(f: &LinearFrame) -> Self {
        let pts = Some(f.pts_secs());
        let pixels = match (&f.hdr, f.data.is_empty(), &f.gpu_source) {
            (Some(h), _, _) => Held::Hdr(h.clone()),
            (None, false, _) => Held::Linear(Arc::new(f.data.clone())),
            (None, true, Some(g)) => Self::gpu(Arc::clone(g)),
            (None, true, None) => Held::Linear(Arc::new(Vec::new())),
        };
        Self::new(pixels, f.width, f.height, pts)
    }

    /// A frame from the pre-grade caches, keeping their record of it so
    /// its ungraded picture is not made a second time.
    pub fn from_pregrade(p: &super::Pregrade, width: u32, height: u32) -> Self {
        let pixels = match &p.pixels {
            super::PregradePixels::Srgb8(u8s) => Held::Srgb8(Arc::clone(u8s)),
            super::PregradePixels::Hdr(h) => Held::Hdr(h.clone()),
        };
        let mut f = Self::new(pixels, width, height, None);
        f.pregrade = Some(p.clone());
        f
    }

    /// Whether this frame is `p` itself — the same pixels, not an equal
    /// copy: a regrade of the parked frame, whose pre-grade picture is
    /// already made.
    pub fn is_pregrade(&self, p: &super::Pregrade) -> bool {
        match (&self.pixels, &p.pixels) {
            (Held::Srgb8(a), super::PregradePixels::Srgb8(b)) => Arc::ptr_eq(a, b),
            (Held::Hdr(a), super::PregradePixels::Hdr(b)) => match (&a.codes, &b.codes) {
                (crate::types::HdrCodes::Cpu(a), crate::types::HdrCodes::Cpu(b)) => Arc::ptr_eq(a, b),
                _ => false,
            },
            _ => false,
        }
    }

    /// What the pre-grade caches keep of this frame: theirs when it came
    /// from them, else made from its pixels — the same bytes for SDR, and
    /// for scene-referred sources the codes. The ungraded picture itself
    /// is made on demand, by `Pregrade::display`.
    pub fn pregrade(&self) -> super::Pregrade {
        if let Some(p) = &self.pregrade {
            return p.clone();
        }
        super::Pregrade::new(match &self.pixels {
            Held::Srgb8(u8s) => super::PregradePixels::Srgb8(Arc::clone(u8s)),
            Held::Gpu(g) => super::PregradePixels::Srgb8(g.srgb()),
            Held::Linear(l) => super::PregradePixels::Srgb8(Arc::new(klipp_effects::linear_f32_rgba_to_srgb_u8(l))),
            Held::Hdr(h) => super::PregradePixels::Hdr(HdrInput {
                codes: crate::types::HdrCodes::Cpu(self.cpu_codes(h)),
                ..h.clone()
            }),
        })
    }

    fn cpu_codes(&self, h: &HdrInput) -> Arc<Vec<u32>> {
        Arc::clone(self.codes.get_or_init(|| h.cpu_codes()))
    }

    /// The frame as the renderer takes it.
    pub fn source_pixels(&self) -> SourcePixels {
        match &self.pixels {
            Held::Linear(l) => SourcePixels::Linear(Arc::clone(l)),
            Held::Srgb8(u8s) => SourcePixels::Linear(Arc::clone(
                self.linear.get_or_init(|| Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(u8s))),
            )),
            Held::Gpu(g) => SourcePixels::Gpu(Arc::clone(g) as Arc<dyn klipp_compose::GpuPixels>),
            Held::Hdr(h) => SourcePixels::SceneReferred {
                codes: self.cpu_codes(h),
                transfer: h.transfer,
                source_to_working: h.source_to_working,
            },
        }
    }
}

/// One media clip a frame needs, and where in its media.
pub(crate) struct Want {
    pub clip: ClipId,
    pub media: MediaId,
    /// The clip's own rotation, for when its media has none recorded.
    pub rotation: i32,
    pub source_secs: f64,
}

/// A decoder following one clip's source time.
struct ClipStream {
    decoder: VideoDecoder,
    /// The latest frame at or before `target`.
    held: Option<HeldFrame>,
    /// The source time last asked for.
    target: f64,
    /// A seek to this time is in flight: queued frames from before it are
    /// stale.
    seeking: Option<f64>,
    /// The media ran out; `held` is its last frame.
    ended: bool,
}

impl ClipStream {
    fn open(want: &Want, path: &std::path::Path, rotation: i32) -> Option<Self> {
        let decoder = VideoDecoder::open(path, rotation)
            .map_err(|e| warn!("clip stream: cannot open {path:?}: {e}"))
            .ok()?;
        let target = want.source_secs.max(0.0);
        if target > 0.01 {
            decoder.seek(target);
        }
        decoder.resume();
        debug!("clip stream opened for {:?} at {target:.3}s", want.clip);
        Some(Self { decoder, held: None, target, seeking: Some(target), ended: false })
    }

    fn seek(&mut self, secs: f64) {
        self.decoder.seek(secs);
        self.decoder.resume();
        self.seeking = Some(secs);
        self.ended = false;
    }

    /// Take the frames that are due, keeping the latest.
    fn pump(&mut self, half_frame: f64) {
        let epoch = self.decoder.requested_seek();
        let mut q = self.decoder.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
        while let Some(f) = q.front() {
            // Decoded before the seek we are waiting on — the decode thread
            // clears its queue when it takes a seek, but a frame converted
            // just before it can still land. Its timestamp says nothing
            // about that, so the epoch is the only thing that can tell.
            //
            // Guessing from the timestamp instead let a frame up to two
            // seconds from the seek target pass as the frame at it: with a
            // dissolve's incoming side running into its handle, that put a
            // side of the transition tens of frames from where the
            // composite said it was, and the viewer disagreed with the
            // export by a hundred levels.
            if f.seek_epoch < epoch {
                q.pop();
                continue;
            }
            if f.is_eof {
                q.pop();
                self.ended = true;
                break;
            }
            let pts = f.pts_secs();
            if self.seeking.is_some() {
                // The first frame of the seek: the nearest there is.
                self.seeking = None;
                self.held = q.pop().map(HeldFrame::from_frame);
                continue;
            }
            if pts > self.target + half_frame {
                break;
            }
            self.held = q.pop().map(HeldFrame::from_frame);
        }
    }

    /// Whether `held` is the frame for `target`.
    fn ready(&self, half_frame: f64) -> bool {
        if self.ended && self.seeking.is_none() {
            return true;
        }
        let Some(held) = self.held.as_ref().filter(|_| self.seeking.is_none()) else { return false };
        // Within half a frame of the target. (A seek lands up to 50 ms
        // early — the frame before, at 25 fps — and pumping moves on.)
        if held.pts.unwrap_or(f64::MAX) >= self.target - half_frame {
            return true;
        }
        // Behind the target, but the next frame is past it: a source with
        // fewer frames than the timeline, and `held` is right.
        let q = self.decoder.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
        q.front().is_some_and(|f| f.is_eof || f.pts_secs() > self.target + half_frame)
    }
}

impl Drop for ClipStream {
    fn drop(&mut self) {
        self.decoder.stop_detached();
    }
}

/// The viewer's decoders for everything but the primary track's clip.
pub(crate) struct ClipStreams {
    streams: HashMap<ClipId, ClipStream>,
    /// At most this many decoders at once.
    cap: usize,
}

impl ClipStreams {
    pub fn new(cap: usize) -> Self {
        Self { streams: HashMap::new(), cap }
    }

    /// Follow `wants`: open streams for new clips, close those no longer
    /// wanted, seek any whose time jumped, then take the frames that are
    /// due. `frame_secs` is the timeline's frame duration; `resolve` gives
    /// the file and rotation to decode a clip's media from, asked only
    /// when a stream opens.
    pub fn sync(&mut self, wants: &[Want], frame_secs: f64, resolve: &dyn Fn(&Want) -> Option<(PathBuf, i32)>) {
        self.streams.retain(|id, _| wants.iter().any(|w| w.clip == *id));
        for w in wants {
            let target = w.source_secs.max(0.0);
            let full = self.streams.len() >= self.cap;
            match self.streams.get_mut(&w.clip) {
                Some(s) => {
                    // A step back or a jump ahead: seek. Playback moves a
                    // frame a tick, which the queue already holds.
                    if target + frame_secs * 0.5 < s.target || target > s.target + 1.0 {
                        s.seek(target);
                    }
                    s.target = target;
                }
                None if full => {
                    debug!("clip streams: {} decoders open, not opening one for {:?}", self.cap, w.clip);
                }
                None => {
                    if let Some(s) = resolve(w).and_then(|(path, rot)| ClipStream::open(w, &path, rot)) {
                        self.streams.insert(w.clip, s);
                    }
                }
            }
        }
        for s in self.streams.values_mut() {
            s.pump(frame_secs * 0.5);
        }
    }

    /// Take whatever frames arrived, without changing any target.
    pub fn pump(&mut self, frame_secs: f64) {
        for s in self.streams.values_mut() {
            s.pump(frame_secs * 0.5);
        }
    }

    pub fn has(&self, clip: ClipId) -> bool {
        self.streams.contains_key(&clip)
    }

    pub fn frame(&self, clip: ClipId) -> Option<&HeldFrame> {
        self.streams.get(&clip)?.held.as_ref()
    }

    /// A copy of the frame after `clip`'s held one, if it is decoded.
    pub fn next_frame(&self, clip: ClipId) -> Option<HeldFrame> {
        let s = self.streams.get(&clip)?;
        let q = s.decoder.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
        q.front().filter(|f| !f.is_eof).map(HeldFrame::copy_of)
    }

    /// Every stream holds its frame for its current time.
    pub fn ready(&self, frame_secs: f64) -> bool {
        self.streams.values().all(|s| s.ready(frame_secs * 0.5))
    }

    /// Take over the primary decoder at a cut, when the clip it was
    /// playing is still on screen — a transition's outgoing side, running
    /// into its handle. The decoder is already there; opening another and
    /// seeking would leave that side empty for the first frames.
    pub fn adopt(&mut self, clip: ClipId, decoder: VideoDecoder, source_secs: f64) {
        self.streams.insert(clip, ClipStream { decoder, held: None, target: source_secs.max(0.0), seeking: None, ended: false });
    }

    pub fn clear(&mut self) {
        self.streams.clear();
    }

    pub fn len(&self) -> usize {
        self.streams.len()
    }

    /// The queues of the open decoders, for telemetry.
    pub fn queues(&self) -> impl Iterator<Item = &SharedFrameQueue> {
        self.streams.values().map(|s| s.decoder.frame_queue())
    }
}
