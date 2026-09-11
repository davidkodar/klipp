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
    /// `Held::Srgb8` / `Held::Gpu` as linear light, once asked for.
    linear: OnceLock<Arc<Vec<f32>>>,
    /// `Held::Gpu` read back as its sRGB bytes, once asked for: the
    /// pre-grade caches and the linear pixels both come from one readback.
    srgb: OnceLock<Arc<Vec<u8>>>,
    /// `Held::Hdr` codes on the CPU, once asked for.
    codes: OnceLock<Arc<Vec<u32>>>,
}

enum Held {
    Linear(Arc<Vec<f32>>),
    Srgb8(Arc<Vec<u8>>),
    /// A frame the decoder left on the GPU, not yet read back.
    Gpu(Arc<crate::frame::GpuFrame>),
    Hdr(HdrInput),
}

impl HeldFrame {
    fn new(pixels: Held, width: u32, height: u32, pts: Option<f64>) -> Self {
        Self { width, height, pts, pixels, linear: OnceLock::new(), srgb: OnceLock::new(), codes: OnceLock::new() }
    }

    /// A decoded frame, taken over.
    pub fn from_frame(f: LinearFrame) -> Self {
        let pts = Some(f.pts_secs());
        let pixels = match (f.hdr, f.data.is_empty(), f.gpu_source) {
            (Some(h), _, _) => Held::Hdr(h),
            (None, false, _) => Held::Linear(Arc::new(f.data)),
            (None, true, Some(g)) => Held::Gpu(g),
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
            (None, true, Some(g)) => Held::Gpu(Arc::clone(g)),
            (None, true, None) => Held::Linear(Arc::new(Vec::new())),
        };
        Self::new(pixels, f.width, f.height, pts)
    }

    /// A frame from the pre-grade caches.
    pub fn from_pregrade(p: &super::PregradePixels, width: u32, height: u32) -> Self {
        let pixels = match p {
            super::PregradePixels::Srgb8(u8s) => Held::Srgb8(Arc::clone(u8s)),
            super::PregradePixels::Hdr(h) => Held::Hdr(h.clone()),
        };
        Self::new(pixels, width, height, None)
    }

    /// What the pre-grade caches keep of this frame, and the picture the
    /// UI shows as "before": the same bytes for SDR; for scene-referred
    /// sources the codes, and their display tone map.
    pub fn to_pregrade(&self) -> (super::PregradePixels, Arc<Vec<u8>>) {
        match &self.pixels {
            Held::Srgb8(u8s) => (super::PregradePixels::Srgb8(Arc::clone(u8s)), Arc::clone(u8s)),
            Held::Gpu(g) => {
                let u8s = self.gpu_srgb(g);
                (super::PregradePixels::Srgb8(Arc::clone(&u8s)), u8s)
            }
            Held::Linear(l) => {
                let u8s = Arc::new(klipp_effects::linear_f32_rgba_to_srgb_u8(l));
                (super::PregradePixels::Srgb8(Arc::clone(&u8s)), u8s)
            }
            Held::Hdr(h) => {
                let cpu = HdrInput { codes: crate::types::HdrCodes::Cpu(self.cpu_codes(h)), ..h.clone() };
                let mut display =
                    klipp_color::hdr::hdr10_codes_to_scene_linear_rgba(&self.cpu_codes(h), h.transfer, h.source_to_working);
                klipp_color::hdr::apply_hdr_display_tonemap(&mut display, h.transfer);
                let u8s = Arc::new(klipp_effects::linear_f32_rgba_to_srgb_u8(&display));
                (super::PregradePixels::Hdr(cpu), u8s)
            }
        }
    }

    /// The decoder's buffer is sRGB u8 already: read back as it is, a
    /// quarter of a linear readback, and expanded on the CPU when linear
    /// is wanted — `realise_linear_from_gpu` does the same.
    fn gpu_srgb(&self, g: &crate::frame::GpuFrame) -> Arc<Vec<u8>> {
        Arc::clone(self.srgb.get_or_init(|| Arc::new(crate::types::realise_srgb_u8_from_gpu(g))))
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
            Held::Gpu(g) => SourcePixels::Linear(Arc::clone(
                self.linear.get_or_init(|| Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&self.gpu_srgb(g)))),
            )),
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
        self.decoder.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.decoder.resume();
        self.seeking = Some(secs);
        self.ended = false;
    }

    /// Take the frames that are due, keeping the latest.
    fn pump(&mut self, half_frame: f64) {
        let mut q = self.decoder.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
        while let Some(f) = q.front() {
            if f.is_eof {
                q.pop();
                self.ended = true;
                break;
            }
            let pts = f.pts_secs();
            if let Some(s) = self.seeking {
                // The decode thread clears its queue when it takes a seek,
                // but a frame converted just before can still land.
                if pts < s - 0.06 || pts > s + 2.0 {
                    q.pop();
                    continue;
                }
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
