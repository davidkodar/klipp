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
use klipp_media::frame_reader::BackwardFrameReader;

/// A decoded frame the viewer keeps and may render from many times (a
/// paused frame is regraded on every slider tick). Conversions happen on
/// first use and are kept. A clone shares the pixels and the conversions.
#[derive(Clone)]
pub(crate) struct HeldFrame {
    pub width: u32,
    pub height: u32,
    pub pts: Option<f64>,
    pixels: Held,
    /// `Held::Srgb8` as linear light, once asked for. Behind an `Arc`, so
    /// the clones share it: a frame regraded on each slider step is handed
    /// over as a clone each time, and a cell cloned before it was filled
    /// would make the conversion again (#2352).
    linear: Arc<OnceLock<Arc<Vec<f32>>>>,
    /// `Held::Hdr` codes on the CPU, once asked for, shared as `linear`
    /// is: read back from the GPU once per frame, not once per step, and
    /// the same buffer each time, so the GPU keeps its upload of them.
    codes: Arc<OnceLock<Arc<klipp_color::hdr::SceneCodes>>>,
    /// The caches' own record of this frame, when it came from them —
    /// carrying the ungraded picture they already made.
    pregrade: Option<super::Pregrade>,
}

#[derive(Clone)]
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

/// A held frame's sRGB bytes on the GPU, as a temporal denoise's
/// neighbour (#1988): the chain decodes them to linear on the GPU.
impl klipp_nodes::frame_context::GpuFramePixels for GpuHeld {
    fn buffer(&self) -> &wgpu::Buffer {
        &self.frame.rgba_buffer
    }

    fn read(&self) -> Arc<Vec<f32>> {
        klipp_compose::GpuPixels::read(self)
    }

    fn srgb8(&self) -> bool {
        true
    }

    fn source_size(&self) -> Option<(u32, u32)> {
        Some((self.frame.width, self.frame.height))
    }
}

impl HeldFrame {
    /// The frame as a temporal denoise's neighbour on the GPU, where the
    /// decoder left it there as sRGB bytes (#1988); `None` for any other.
    pub fn gpu_neighbour(&self) -> Option<Arc<dyn klipp_nodes::frame_context::GpuFramePixels>> {
        match &self.pixels {
            Held::Gpu(g) => Some(Arc::clone(g) as Arc<dyn klipp_nodes::frame_context::GpuFramePixels>),
            _ => None,
        }
    }

    fn new(pixels: Held, width: u32, height: u32, pts: Option<f64>) -> Self {
        Self { width, height, pts, pixels, linear: Arc::default(), codes: Arc::default(), pregrade: None }
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
        let mut f = Self::new(pixels, width, height, p.pts);
        f.pregrade = Some(p.clone());
        f
    }

    /// Whether this frame and `other` hold the very same pixels (one
    /// buffer, not equal copies), wherever they are: the bytes, or the
    /// HDR codes on the CPU or on the GPU (#2352).
    pub fn same_pixels(&self, other: &HeldFrame) -> bool {
        use crate::types::HdrCodes;
        match (&self.pixels, &other.pixels) {
            (Held::Srgb8(a), Held::Srgb8(b)) => Arc::ptr_eq(a, b),
            (Held::Hdr(a), Held::Hdr(b)) => match (&a.codes, &b.codes) {
                (HdrCodes::Cpu(a), HdrCodes::Cpu(b)) => Arc::ptr_eq(a, b),
                (HdrCodes::Gpu { buffer: a, .. }, HdrCodes::Gpu { buffer: b, .. }) => Arc::ptr_eq(a, b),
                _ => false,
            },
            _ => false,
        }
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
        .with_pts(self.pts)
    }

    fn cpu_codes(&self, h: &HdrInput) -> Arc<klipp_color::hdr::SceneCodes> {
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

/// What a stream asks for as it opens (#2128): decode rate, pixels a
/// second; memory, the bytes of one of its decoded frames; and reading,
/// the bytes a second it reads from the drive its file is on.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub(crate) struct Cost {
    pub rate: f64,
    pub frame_bytes: f64,
    pub read_bytes: f64,
    pub drive: Option<crate::disk_budget::Drive>,
}

/// A layer on screen opened while memory is short holds at least this
/// many decoded frames: enough to play on through a slow frame.
const MIN_QUEUE_FRAMES: usize = 4;

/// The share of memory the viewer's streams' queues may hold together:
/// of what is free and what they hold already (#2128).
const STREAM_MEMORY_SHARE: f64 = 0.5;

/// The share of what a drive gives the viewer's streams may read from it
/// together (#2128).
const STREAM_DISK_SHARE: f64 = 0.8;

/// One media clip a frame needs, and where in its media.
#[derive(Clone)]
pub(crate) struct Want {
    pub clip: ClipId,
    pub media: MediaId,
    /// The clip's own rotation, for when its media has none recorded.
    pub rotation: i32,
    pub source_secs: f64,
    /// Coming up, not on screen yet: opened ahead, waited for by nothing.
    pub ahead: bool,
    /// The clip plays its source backward (#617): read by a
    /// [`BackwardFrameReader`], not a decoder that only reads forward and
    /// would seek on every frame.
    pub reversed: bool,
}

/// A reversed clip's source: its own forward decoder, read a window at
/// a time (#2798), or, where that decoder can't be opened, the CPU reader
/// of #617 with the rotation its frames are turned by.
enum Reversed {
    Gop(super::reverse_gop::ReverseGop),
    Cpu { reader: BackwardFrameReader<Vec<f32>>, rotation: i32 },
}

/// A decoder following one clip's source time.
struct ClipStream {
    /// Taken only as the stream ends, by the primary decoder taking it
    /// over ([`ClipStreams::take`]).
    decoder: Option<VideoDecoder>,
    /// The latest frame at or before `target`.
    held: Option<HeldFrame>,
    /// The frame `held` replaced as the stream played on: a temporal
    /// denoise's neighbour before (#1988). None after a seek.
    previous: Option<HeldFrame>,
    /// The source time last asked for.
    target: f64,
    /// A seek to this time is in flight: queued frames from before it are
    /// stale.
    seeking: Option<f64>,
    /// The media ran out; `held` is its last frame.
    ended: bool,
    /// `held` is the first frame a seek delivered: the nearest the
    /// decoder had, which may lie a little past the time asked for. Later
    /// than that is still the frame for it, as no earlier one is coming.
    held_from_seek: bool,
    /// Opened for a clip coming up, not on screen yet (#2108): no frame
    /// waits for it.
    ahead: bool,
    /// The file and rotation it decodes, for the shot after its own to go
    /// on with it (#2397). None for a decoder taken over from the primary.
    file: Option<(PathBuf, i32)>,
}

impl ClipStream {
    fn open(want: &Want, path: &std::path::Path, rotation: i32, lease: crate::decode_budget::Lease, frames: usize) -> Option<Self> {
        let decoder = VideoDecoder::open_with_queue(path, rotation, lease, frames)
            .map_err(|e| warn!("clip stream: cannot open {path:?}: {e}"))
            .ok()?;
        let target = want.source_secs.max(0.0);
        if target > 0.01 {
            decoder.seek(target);
        }
        decoder.resume();
        debug!("clip stream opened for {:?} at {target:.3}s", want.clip);
        Some(Self { decoder: Some(decoder), held: None, previous: None, target, seeking: Some(target), ended: false, held_from_seek: false, ahead: want.ahead, file: Some((path.to_path_buf(), rotation)) })
    }

    fn dec(&self) -> &VideoDecoder {
        self.decoder.as_ref().expect("a stream's decoder is taken only as it ends")
    }

    fn seek(&mut self, secs: f64) {
        self.dec().seek(secs);
        self.dec().resume();
        self.seeking = Some(secs);
        self.ended = false;
    }

    /// Take the frames that are due, keeping the latest.
    fn pump(&mut self, half_frame: f64) {
        // The field, not `dec`: the fields below change under the lock.
        let Some(decoder) = self.decoder.as_ref() else { return };
        let epoch = decoder.requested_seek();
        let mut q = decoder.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
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
                self.previous = None;
                self.held = q.pop().map(HeldFrame::from_frame);
                self.held_from_seek = true;
                continue;
            }
            if pts > self.target + half_frame {
                break;
            }
            self.previous = self.held.take();
            self.held = q.pop().map(HeldFrame::from_frame);
            self.held_from_seek = false;
        }
    }

    /// Holding nothing, with no seek in flight, and the queue's first
    /// frame past `target`: no frame for it is coming, and only a seek
    /// brings one (#1494: a decoder taken over at a cut, its frame there
    /// already taken). A held frame past the target is `held_ahead`'s.
    fn starved(&self, target: f64, half_frame: f64) -> bool {
        if self.held.is_some() || self.seeking.is_some() || self.ended {
            return false;
        }
        let epoch = self.dec().requested_seek();
        let q = self.dec().frame_queue().lock().unwrap_or_else(|e| e.into_inner());
        q.front().is_some_and(|f| f.seek_epoch >= epoch && !f.is_eof && f.pts_secs() > target + half_frame)
    }

    /// Whether `held` is the frame for `target`.
    fn ready(&self, half_frame: f64) -> bool {
        if self.ended && self.seeking.is_none() {
            return true;
        }
        let Some(held) = self.held.as_ref().filter(|_| self.seeking.is_none()) else { return false };
        // Within half a frame of the target. (A seek lands up to 50 ms
        // early — the frame before, at 25 fps — and pumping moves on.) Not
        // past it either: a frame from later is another frame, and taking
        // it as this one published a dissolve with its other side ahead
        // as complete (#411). `pump` never takes one that far on.
        let pts = held.pts.unwrap_or(f64::MAX);
        if pts > self.target + half_frame && held.pts.is_some() && !self.held_from_seek {
            return false;
        }
        if pts >= self.target - half_frame {
            return true;
        }
        // Behind the target, but the next frame is past it: a source with
        // fewer frames than the timeline, and `held` is right.
        let q = self.dec().frame_queue().lock().unwrap_or_else(|e| e.into_inner());
        q.front().is_some_and(|f| f.is_eof || f.pts_secs() > self.target + half_frame)
    }
}

impl Drop for ClipStream {
    fn drop(&mut self) {
        if let Some(d) = &mut self.decoder {
            d.stop_detached();
        }
    }
}

/// `wants` with each frame of a file wanted once: the clips that show the
/// same frame of the same file as an earlier one, by that one's clip, and
/// the wants left, one a frame. The one kept is on screen when any of
/// them is. A reversed clip keeps its own reader, which reads a window
/// back from its time.
fn sharing(wants: &[Want]) -> (Vec<Want>, HashMap<ClipId, ClipId>) {
    // Within a tenth of a millisecond: a frame is tens of them, and a
    // scene's time worked out through each level lands a hair apart.
    const SAME: f64 = 1e-4;
    let mut kept: Vec<Want> = Vec::with_capacity(wants.len());
    let mut shared = HashMap::new();
    for w in wants {
        let same = |k: &Want| !w.reversed && !k.reversed && k.clip != w.clip && k.media == w.media && k.rotation == w.rotation && (k.source_secs - w.source_secs).abs() < SAME;
        if let Some(k) = kept.iter_mut().find(|k| same(k)) {
            k.ahead &= w.ahead;
            shared.insert(w.clip, k.clip);
        } else if !kept.iter().any(|k| k.clip == w.clip) {
            kept.push(w.clone());
        }
    }
    (kept, shared)
}

/// The viewer's decoders for everything but the primary track's clip.
pub(crate) struct ClipStreams {
    streams: HashMap<ClipId, ClipStream>,
    /// Streams being opened on a thread of their own (#2108): opening a
    /// decoder (probing the file, opening the codec, the first seek) took
    /// 20–100 ms each on the engine's thread, four of them a frame's stall
    /// of 150 ms where four layers come up together. Each lands in
    /// `streams` once open; until then it is wanted and not ready.
    opening: HashMap<ClipId, (std::sync::mpsc::Receiver<Option<ClipStream>>, bool)>,
    /// Reversed clips' readers (#617).
    reversed: HashMap<ClipId, Reversed>,
    /// The pixels a second each stream asks for, open or opening (#2128).
    rates: HashMap<ClipId, f64>,
    /// The bytes each one's queue of decoded frames may hold, open or
    /// opening (#2128): its frames at their size.
    bytes: HashMap<ClipId, f64>,
    /// Memory free for more: [`crate::decode_budget::free_memory`], or a
    /// test's.
    free_memory: fn() -> f64,
    /// The bytes on the GPU each queue may hold, of the streams decoded
    /// there, whose frames stay on it: counted here, not in `bytes`.
    vram: HashMap<ClipId, f64>,
    /// The GPU's memory free for more: [`crate::decode_budget::free_vram`],
    /// `None` where it can't be read, or a test's.
    free_vram: fn() -> Option<f64>,
    /// The drive each stream reads from and the bytes a second it reads,
    /// open or opening (#2128).
    reads: HashMap<ClipId, (u64, f64)>,
    /// The frames a stream queues when memory allows: the preference's,
    /// or a test's.
    queue_frames: fn() -> usize,
    /// The pixels a second all streams together may ask for:
    /// [`crate::decode_budget::stream_capacity`], or a test's.
    capacity: fn() -> f64,
    /// What the GPU's decoder has left: [`crate::decode_budget::gpu_headroom`],
    /// or a test's.
    gpu_headroom: fn() -> f64,
    /// Media sources' readers (#1746), kept across frames.
    pub(crate) media: klipp_media::frame_reader::MediaReaders,
    /// Clips that show the same frame of the same file as another, by the
    /// clip whose stream they read: a scene placed in several places at
    /// once, every level of a nest of templates over the same shots,
    /// decoded once instead of once a clip, each decoder's frames taking
    /// hundreds of megabytes on the GPU.
    shared: HashMap<ClipId, ClipId>,
    /// OFX plug-ins' neighbour readers (#2013), by file and log format,
    /// kept across frames: the one read longest ago closed first.
    neighbours: Vec<(PathBuf, Option<klipp_core::LogFormat>, std::sync::Arc<std::sync::Mutex<klipp_media::frame_reader::SeekableFrameReader>>)>,
    /// How often each clip's decoder panicked since the person last
    /// sought or played (#3181): one that panicked [`super::PANIC_RESTARTS`]
    /// times is not opened again until then.
    panics: HashMap<ClipId, u32>,
    /// A clip reached that since [`Self::take_given_up`] last asked.
    given_up: bool,
}

impl ClipStreams {
    pub fn new() -> Self {
        Self { streams: HashMap::new(), opening: HashMap::new(), reversed: HashMap::new(), rates: HashMap::new(), bytes: HashMap::new(), free_memory: crate::decode_budget::free_memory, vram: HashMap::new(), free_vram: crate::decode_budget::free_vram, reads: HashMap::new(), queue_frames: crate::video_decoder::frame_queue_size, capacity: crate::decode_budget::stream_capacity, gpu_headroom: crate::decode_budget::gpu_headroom, media: klipp_media::frame_reader::MediaReaders::new(klipp_compose::MEDIA_READERS), shared: HashMap::new(), neighbours: Vec::new(), panics: HashMap::new(), given_up: false }
    }

    /// The reader of `path`'s frames at any time for an OFX plug-in's
    /// neighbours (#2013), colour-managed as export's is, opened on first
    /// use and kept: a plug-in reading the frames either side on every
    /// frame used to open the file on every frame.
    pub(crate) fn neighbour_reader(&mut self, path: &std::path::Path, log_format: Option<klipp_core::LogFormat>) -> Option<std::sync::Arc<std::sync::Mutex<klipp_media::frame_reader::SeekableFrameReader>>> {
        const KEPT: usize = 4;
        if let Some(i) = self.neighbours.iter().position(|(p, l, _)| p == path && *l == log_format) {
            let held = self.neighbours.remove(i);
            self.neighbours.push(held);
        } else {
            let reader = klipp_media::frame_reader::SeekableFrameReader::open_colour_managed(path, log_format)?;
            if self.neighbours.len() >= KEPT {
                self.neighbours.remove(0);
            }
            self.neighbours.push((path.to_path_buf(), log_format, std::sync::Arc::new(std::sync::Mutex::new(reader))));
        }
        self.neighbours.last().map(|(_, _, r)| std::sync::Arc::clone(r))
    }

    /// How many neighbour readers are open.
    #[cfg(test)]
    pub(crate) fn neighbour_readers(&self) -> usize {
        self.neighbours.len()
    }

    /// Follow `wants`: open streams for new clips, close those no longer
    /// wanted, seek any whose time jumped, then take the frames that are
    /// due. `frame_secs` is the timeline's frame duration; `resolve` gives
    /// the file and rotation to decode a clip's media from, and the pixels
    /// a second it decodes, asked only when a stream opens.
    ///
    /// Streams open in `wants`' order, and each is counted against the
    /// GPU's decoder as it does (#2116): when it is full, the later ones
    /// decode in software. Layers come bottom first and those opened ahead
    /// last, so those go to software first, then the top-most layers.
    pub fn sync(&mut self, wants: &[Want], frame_secs: f64, resolve: &dyn Fn(&Want) -> Option<(PathBuf, i32, Cost)>) {
        let (wants, shared) = sharing(wants);
        self.shared = shared;
        let wants = &wants[..];
        crate::decode_budget::sample();
        // A stream whose decoder ended with a panic is let go, and opened
        // again below like any wanted stream not open (#3181), until it
        // has panicked too often.
        let (panics, given_up) = (&mut self.panics, &mut self.given_up);
        self.streams.retain(|id, s| {
            if !s.dec().has_panicked() {
                return true;
            }
            let n = panics.entry(*id).or_insert(0);
            *n += 1;
            if *n >= super::PANIC_RESTARTS {
                *given_up = true;
                tracing::error!("a layer's video decoder panicked {n} times over; it is not opened again until the playhead is moved or played");
            } else {
                tracing::warn!("a layer's video decoder panicked; it is opened again");
            }
            false
        });
        let still: Vec<Want>;
        let wants = if self.panics.is_empty() {
            wants
        } else {
            still = wants.iter().filter(|w| self.panics.get(&w.clip).is_none_or(|n| *n < super::PANIC_RESTARTS)).cloned().collect();
            &still[..]
        };
        self.hand_on(wants, frame_secs, resolve);
        self.streams.retain(|id, _| wants.iter().any(|w| w.clip == *id && !w.reversed));
        // One no longer wanted is let go: opened, it is dropped on its own
        // thread, its decoder stopped there.
        self.opening.retain(|id, _| wants.iter().any(|w| w.clip == *id && !w.reversed));
        self.land_opened();
        self.forget_rates();
        self.reversed.retain(|id, _| wants.iter().any(|w| w.clip == *id && w.reversed));
        for w in wants.iter().filter(|w| w.reversed) {
            let r = match self.reversed.entry(w.clip) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let Some((path, rotation, cost)) = resolve(w) else { continue };
                    // The decoder forward playback uses, its frames on the
                    // GPU where there is one, held a window at a time.
                    let lease = crate::decode_budget::admit(cost.rate, crate::decode_budget::Priority::Overflow);
                    match VideoDecoder::open_with_queue(&path, rotation, lease, MIN_QUEUE_FRAMES * 2) {
                        Ok(decoder) => {
                            let free = window_free(crate::video_decoder::should_attach_gpu_source(), (self.free_vram)(), (self.free_memory)());
                            e.insert(Reversed::Gop(super::reverse_gop::ReverseGop::for_memory(decoder, cost.frame_bytes, free, 0.0)))
                        }
                        Err(err) => {
                            warn!("clip streams: no decoder for {path:?} ({err}); reading it backward on the CPU");
                            let log_format = crate::video_decoder::log_format_for_path(&path);
                            let Some(reader) = BackwardFrameReader::open_linear(&path, log_format, crate::video_decoder::working_colorspace()) else {
                                warn!("clip streams: cannot open {path:?} to read it backward");
                                continue;
                            };
                            e.insert(Reversed::Cpu { reader, rotation })
                        }
                    }
                }
            };
            // Its frames for the time wanted start decoding now, off this
            // thread: a clip about to come up is ready when it does.
            match r {
                Reversed::Gop(g) => {
                    // The GPU's free memory is read on a thread of its own:
                    // a reader opened before the first reading guessed.
                    g.learn_free(window_free(crate::video_decoder::should_attach_gpu_source(), (self.free_vram)(), (self.free_memory)()));
                    g.want(w.source_secs, w.ahead)
                }
                Reversed::Cpu { reader, .. } => reader.prepare(w.source_secs),
            }
        }
        for w in wants.iter().filter(|w| !w.reversed) {
            let target = w.source_secs.max(0.0);
            if let Some((_, ahead)) = self.opening.get_mut(&w.clip) {
                *ahead = w.ahead;
                continue;
            }
            match self.streams.get_mut(&w.clip) {
                Some(s) => {
                    s.ahead = w.ahead;
                    // A step back or a jump ahead: seek. Playback moves a
                    // frame a tick, which the queue already holds. And a
                    // held frame already past the new time: playback can
                    // leave a stream a frame or two ahead of where it is
                    // parked, and no later frame will bring it back (#411).
                    // Not while a seek is in flight: `held` is the old
                    // frame until it lands, and seeking again on every
                    // sync would restart a slow seek for ever.
                    // Half of one of the file's frames, as export decides
                    // which frame a time shows (#696).
                    let half = s.dec().half_source_frame(1.0 / frame_secs.max(1e-6));
                    let held_ahead = s.seeking.is_none() && !s.held_from_seek && s.held.as_ref().and_then(|h| h.pts).is_some_and(|p| p > target + half);
                    if target + half < s.target || target > s.target + 1.0 || held_ahead || s.starved(target, half) {
                        s.seek(target);
                    }
                    s.target = target;
                }
                None => {
                    // Opened off this thread; it lands on a later sync.
                    if let Some((path, rot, cost)) = resolve(w) {
                        let Some((frames, on_gpu)) = self.make_room(cost, w.ahead) else {
                            debug!(
                                "clip streams: no room ({:.0} of {:.0} Mpx/s, {:.0} MB queued, {} streams), not opening one ahead for {:?}",
                                self.rates.values().sum::<f64>() / 1e6, (self.capacity)() / 1e6, self.bytes.values().sum::<f64>() / 1e6, self.streams.len() + self.opening.len(), w.clip,
                            );
                            continue;
                        };
                        let (tx, rx) = std::sync::mpsc::channel();
                        let want = w.clone();
                        // Admitted here, in order, not on the thread.
                        let lease = crate::decode_budget::admit(cost.rate, crate::decode_budget::Priority::Overflow);
                        // Counted while it opens: a process exiting while
                        // ffmpeg or the GPU is mid-open crashes (#2261).
                        let alive = crate::video_decoder::DecodeThreadToken::new();
                        let spawned = std::thread::Builder::new().name("clip stream open".into()).spawn(move || {
                            let _alive = alive;
                            let _ = tx.send(ClipStream::open(&want, &path, rot, lease, frames));
                        });
                        if spawned.is_ok() {
                            super::STREAMS_OPENED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            self.opening.insert(w.clip, (rx, w.ahead));
                            self.rates.insert(w.clip, cost.rate);
                            if let Some(drive) = cost.drive {
                                self.reads.insert(w.clip, (drive.device, cost.read_bytes));
                            }
                            let held = if on_gpu { &mut self.vram } else { &mut self.bytes };
                            held.insert(w.clip, cost.frame_bytes * frames as f64);
                        }
                    }
                }
            }
        }
        for s in self.streams.values_mut() {
            let half = s.dec().half_source_frame(1.0 / frame_secs.max(1e-6));
            s.pump(half);
        }
        self.forget_rates();
    }

    /// A shot coming on screen that goes on from one leaving it, a little
    /// further in the same file, takes over the leaving one's stream, and
    /// its seek decodes on (#2397): nothing was opened ahead for it, as
    /// [`super::decodes_on`] says. Two files cut against each other on a
    /// layer opened a stream for every shot.
    fn hand_on(&mut self, wants: &[Want], frame_secs: f64, resolve: &dyn Fn(&Want) -> Option<(PathBuf, i32, Cost)>) {
        // As far on as the cut may land, past the last frame the leaving
        // shot was asked for.
        let reach = super::DECODE_ON_CUT_SECS + 2.0 * frame_secs;
        for w in wants {
            if w.reversed || self.streams.contains_key(&w.clip) || self.opening.contains_key(&w.clip) {
                continue;
            }
            let target = w.source_secs.max(0.0);
            let leaving = |id: &ClipId, s: &ClipStream, file: &(PathBuf, i32)| {
                !wants.iter().any(|x| x.clip == *id) && s.file.as_ref() == Some(file) && target - s.target > -1e-3 && target - s.target <= reach
            };
            let Some((path, rotation, _)) = resolve(w) else { continue };
            let file = (path, rotation);
            let Some(from) = self.streams.iter().find(|(id, s)| leaving(id, s, &file)).map(|(id, _)| *id) else { continue };
            let Some(mut s) = self.streams.remove(&from) else { continue };
            debug!("clip streams: {:?} goes on with {from:?}'s stream at {target:.3}s", w.clip);
            s.ahead = w.ahead;
            s.seek(target);
            s.target = target;
            self.streams.insert(w.clip, s);
            for held in [&mut self.rates, &mut self.bytes, &mut self.vram] {
                if let Some(v) = held.remove(&from) {
                    held.insert(w.clip, v);
                }
            }
            if let Some(r) = self.reads.remove(&from) {
                self.reads.insert(w.clip, r);
            }
            super::STREAMS_HANDED_ON.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Room for a stream asking `cost` (#2128): how many frames it may
    /// queue, or `None` to wait. The limits are what the decoders give and
    /// the memory their queues hold, not a count: six decoders were the
    /// limit, and a 3×3 grid of 1080p clips, a fraction of a 4K layer
    /// each, played with two tiles black.
    ///
    /// A stream opened ahead fits with its whole queue or waits. One on
    /// screen is never left black: streams opened ahead are closed to make
    /// room, the biggest first, and past that it opens all the same, with
    /// a shorter queue when memory is what is short.
    fn make_room(&mut self, cost: Cost, ahead: bool) -> Option<(usize, bool)> {
        let capacity = (self.capacity)();
        let full = (self.queue_frames)().max(MIN_QUEUE_FRAMES);
        // Where its frames will be: on the GPU when its decoder has room
        // for it and the GPU's memory can be read, else in memory.
        let free_vram = (self.free_vram)();
        let on_gpu = free_vram.is_some() && cost.rate <= (self.gpu_headroom)();
        let rate_fits = |s: &Self| s.rates.values().sum::<f64>() + cost.rate <= capacity;
        let frames_fit = |s: &Self| {
            if cost.frame_bytes <= 0.0 {
                return full;
            }
            let (held, free): (f64, f64) = match free_vram.filter(|_| on_gpu) {
                Some(free) => (s.vram.values().sum(), free),
                None => (s.bytes.values().sum(), (s.free_memory)()),
            };
            let left = STREAM_MEMORY_SHARE * (free + held) - held;
            (left / cost.frame_bytes).floor().clamp(0.0, full as f64) as usize
        };
        let reads_fit = |s: &Self| s.reads_fit(cost);
        let fits = |s: &Self| rate_fits(s) && frames_fit(s) >= full && reads_fit(s);
        if ahead {
            // A stream opened ahead also waits for the GPU's decoder to
            // have room for it: with no count to stop them, every clip
            // coming up within its lead was opened, eleven at a time, all
            // seeking on the GPU's decoder while the section on screen
            // needed it, and the codec section played at 15–22 frames.
            return (fits(self) && cost.rate <= (self.gpu_headroom)()).then_some((full, on_gpu));
        }
        while !fits(self) {
            // The biggest in what is short: the memory its frames take, the
            // GPU's or the system's, else decode rate, else what it reads
            // from this stream's drive (one on another drive frees nothing).
            let by_bytes = frames_fit(self) < full;
            let by_reads = !by_bytes && rate_fits(self) && !reads_fit(self);
            let device = cost.drive.map(|d| d.device);
            let held = if on_gpu { &self.vram } else { &self.bytes };
            let size = |id: &ClipId| {
                if by_bytes {
                    held.get(id).copied().unwrap_or(0.0)
                } else if by_reads {
                    self.reads.get(id).filter(|(d, _)| Some(*d) == device).map_or(0.0, |(_, b)| *b)
                } else {
                    self.rate(*id)
                }
            };
            let opened_ahead = self.streams.iter().filter(|(_, s)| s.ahead).map(|(id, _)| *id);
            let opening_ahead = self.opening.iter().filter(|(_, (_, a))| *a).map(|(id, _)| *id);
            let Some(biggest) = opened_ahead.chain(opening_ahead).filter(|id| !by_reads || size(id) > 0.0).max_by(|a, b| size(a).total_cmp(&size(b))) else { break };
            debug!("clip streams: closing {biggest:?}, opened ahead, for a layer on screen");
            self.streams.remove(&biggest);
            self.opening.remove(&biggest);
            self.rates.remove(&biggest);
            self.bytes.remove(&biggest);
            self.vram.remove(&biggest);
            self.reads.remove(&biggest);
        }
        if !reads_fit(self) {
            // Opened all the same: a layer on screen is never left black.
            super::STREAMS_DISK_SHORT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if let Some(drive) = cost.drive {
                tracing::info!(
                    "clip streams: the {} is short ({:.0} MB/s read of {:.0}), a layer on screen opens all the same",
                    drive.kind.name(), (self.read_on(drive.device) + cost.read_bytes) / 1e6, drive.kind.read_rate() / 1e6,
                );
            }
        }
        let frames = frames_fit(self).max(MIN_QUEUE_FRAMES);
        if frames < full {
            super::STREAMS_SHORTENED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (what, free, queued) = match free_vram.filter(|_| on_gpu) {
                Some(free) => ("GPU memory", free, self.vram.values().sum::<f64>()),
                None => ("memory", (self.free_memory)(), self.bytes.values().sum::<f64>()),
            };
            tracing::info!(
                "clip streams: {what} short ({:.0} MB free, {:.0} MB queued), a layer on screen queues {frames} of {full} frames",
                free / 1e6, queued / 1e6,
            );
        }
        Some((frames, on_gpu))
    }

    /// The bytes a second the streams open or opening read from `device`.
    fn read_on(&self, device: u64) -> f64 {
        self.reads.values().filter(|(d, _)| *d == device).map(|(_, b)| b).sum()
    }

    /// Whether a stream asking `cost` fits what its drive gives, with the
    /// others reading from it. One alone on its drive always fits, however
    /// much it reads: a 4K ProRes file on a share is still opened ahead of
    /// its cut, rather than never.
    fn reads_fit(&self, cost: Cost) -> bool {
        match cost.drive {
            Some(drive) if cost.read_bytes > 0.0 => {
                let others = self.read_on(drive.device);
                others == 0.0 || others + cost.read_bytes <= STREAM_DISK_SHARE * drive.kind.read_rate()
            }
            _ => true,
        }
    }

    fn rate(&self, clip: ClipId) -> f64 {
        self.rates.get(&clip).copied().unwrap_or(0.0)
    }

    /// The rates of streams no longer open or opening.
    fn forget_rates(&mut self) {
        let (streams, opening) = (&self.streams, &self.opening);
        self.rates.retain(|id, _| streams.contains_key(id) || opening.contains_key(id));
        self.bytes.retain(|id, _| streams.contains_key(id) || opening.contains_key(id));
        self.vram.retain(|id, _| streams.contains_key(id) || opening.contains_key(id));
        self.reads.retain(|id, _| streams.contains_key(id) || opening.contains_key(id));
    }

    /// Streams whose opening finished, put with the others; one that could
    /// not be opened is forgotten, and asked for again by the next sync.
    fn land_opened(&mut self) {
        let mut done = Vec::new();
        for (id, (rx, _)) in &self.opening {
            match rx.try_recv() {
                Ok(opened) => done.push((*id, opened)),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => done.push((*id, None)),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        for (id, opened) in done {
            self.opening.remove(&id);
            if let Some(s) = opened {
                self.streams.insert(id, s);
            }
        }
    }

    /// Take whatever frames arrived, without changing any target.
    pub fn pump(&mut self, frame_secs: f64) {
        self.land_opened();
        for r in self.reversed.values_mut() {
            if let Reversed::Gop(g) = r {
                g.pump();
            }
        }
        for s in self.streams.values_mut() {
            let half = s.dec().half_source_frame(1.0 / frame_secs.max(1e-6));
            s.pump(half);
        }
    }

    /// The clip whose stream `clip` reads: its own, or the one it shares.
    fn reader(&self, clip: ClipId) -> ClipId {
        self.shared.get(&clip).copied().unwrap_or(clip)
    }

    /// What each stream open or opening holds (#3207): its clip, and its
    /// queued frames' bytes on the GPU and in memory.
    pub(crate) fn held(&self) -> Vec<(ClipId, u64, u64)> {
        let clips: std::collections::HashSet<ClipId> = self.streams.keys().chain(self.opening.keys()).copied().collect();
        clips.into_iter().map(|c| (c, self.vram.get(&c).copied().unwrap_or(0.0) as u64, self.bytes.get(&c).copied().unwrap_or(0.0) as u64)).collect()
    }

    pub fn has(&self, clip: ClipId) -> bool {
        let clip = self.reader(clip);
        self.streams.contains_key(&clip) || self.opening.contains_key(&clip) || self.reversed.contains_key(&clip)
    }

    /// A reversed clip's frame at `source_secs`, fitted into `width` ×
    /// `height` and upright (#617). Read when asked: its reader decodes a
    /// window back from the time and answers the next frames from it.
    pub fn reversed_frame(&mut self, clip: &klipp_timeline::Clip, source_secs: f64, width: u32, height: u32) -> Option<klipp_compose::SourceFrame> {
        match self.reversed.get_mut(&clip.id)? {
            // The same frame, its GPU picture shared, for as long as it is
            // shown (#2798).
            Reversed::Gop(g) => g.frame(source_secs).map(|f| klipp_compose::SourceFrame { pixels: f.source_pixels(), width: f.width, height: f.height, pts: f.pts }),
            Reversed::Cpu { reader, rotation } => {
                let (pixels, w, h, pts) = reader.decode_upright(source_secs, (width, height), *rotation)?;
                Some(klipp_compose::SourceFrame { pixels: SourcePixels::Linear(Arc::new(pixels)), width: w, height: h, pts: Some(pts) })
            }
        }
    }


    /// The frame after the one [`Self::reversed_frame`] shows at
    /// `source_secs`, for a frame blend (#2772).
    pub fn reversed_frame_after(&mut self, clip: &klipp_timeline::Clip, source_secs: f64, width: u32, height: u32) -> Option<klipp_compose::SourceFrame> {
        match self.reversed.get_mut(&clip.id)? {
            Reversed::Gop(g) => g.frame_after(source_secs).map(|f| klipp_compose::SourceFrame { pixels: f.source_pixels(), width: f.width, height: f.height, pts: f.pts }),
            Reversed::Cpu { reader, rotation } => {
                let step = reader.frame_duration();
                let (_, _, _, shown) = reader.decode_upright(source_secs, (width, height), *rotation)?;
                let (pixels, w, h, pts) = reader.decode_upright(shown + step, (width, height), *rotation)?;
                (pts > shown + step * 0.5).then(|| klipp_compose::SourceFrame { pixels: SourcePixels::Linear(Arc::new(pixels)), width: w, height: h, pts: Some(pts) })
            }
        }
    }

    /// [`Self::reversed_frame_after`]'s frame where the window holds it on
    /// the GPU (#2854): its time and its pixels as the chain binds them,
    /// fitting them to the render there when they are another size.
    pub fn reversed_frame_after_on_gpu(&mut self, clip: &klipp_timeline::Clip, source_secs: f64) -> Option<(f64, Arc<dyn klipp_nodes::frame_context::GpuFramePixels>)> {
        match self.reversed.get_mut(&clip.id)? {
            Reversed::Gop(g) => g.frame_after(source_secs).and_then(|f| Some((f.pts?, f.gpu_neighbour()?))),
            Reversed::Cpu { .. } => None,
        }
    }

    pub fn frame(&self, clip: ClipId) -> Option<&HeldFrame> {
        self.streams.get(&self.reader(clip))?.held.as_ref()
    }

    /// The frame `clip`'s stream held before its current one, as it played
    /// on (#1988).
    pub fn previous_frame(&self, clip: ClipId) -> Option<&HeldFrame> {
        self.streams.get(&self.reader(clip))?.previous.as_ref()
    }

    /// A copy of the frame after `clip`'s held one, if it is decoded.
    pub fn next_frame(&self, clip: ClipId) -> Option<HeldFrame> {
        let s = self.streams.get(&self.reader(clip))?;
        let q = s.dec().frame_queue().lock().unwrap_or_else(|e| e.into_inner());
        q.front().filter(|f| !f.is_eof).map(HeldFrame::copy_of)
    }

    /// Every stream holds its frame for its current time.
    pub fn ready(&self, frame_secs: f64) -> bool {
        // Only what is on screen: a stream opened for a clip coming up is
        // nothing a frame shows yet.
        self.opening.values().all(|(_, ahead)| *ahead)
            && self.streams.values().filter(|s| !s.ahead).all(|s| s.ready(s.dec().half_source_frame(1.0 / frame_secs.max(1e-6))))
            && self.reversed.values().all(|r| match r {
                Reversed::Gop(g) => g.ready(),
                Reversed::Cpu { .. } => true,
            })
    }

    /// Take over the primary decoder at a cut, when the clip it was
    /// playing is still on screen — a transition's outgoing side, running
    /// into its handle. The decoder is already there; opening another and
    /// seeking would leave that side empty for the first frames.
    /// `held` is the frame the decoder gave at the cut, when the caller
    /// took it from the queue (#1494).
    pub fn adopt(&mut self, clip: ClipId, decoder: VideoDecoder, source_secs: f64, held: Option<HeldFrame>) {
        self.streams.insert(clip, ClipStream { decoder: Some(decoder), held, previous: None, target: source_secs.max(0.0), seeking: None, ended: false, held_from_seek: false, ahead: false, file: None });
    }

    /// `clip`'s decoder, open and seeked where its stream is, and the frame
    /// it holds, for the primary decoder to take over rather than open the
    /// file again (#2097). Its seek may still be in flight: then it holds
    /// no frame for here yet, and the primary waits for the first. One
    /// still opening is waited for up to 300 ms.
    pub fn take(&mut self, clip: ClipId) -> Option<(VideoDecoder, Option<HeldFrame>)> {
        // Still opening (#2112): waited for, a moment. The open is a probe
        // and a codec, tens of milliseconds, its seek already asked for;
        // opening the file again cold would start that seek over.
        if let Some((rx, _)) = self.opening.remove(&clip) {
            match rx.recv_timeout(std::time::Duration::from_millis(300)) {
                Ok(Some(s)) => {
                    self.streams.insert(clip, s);
                }
                _ => return None,
            }
        }
        // Still seeking: taken over all the same, its seek going on in the
        // primary's hands (#2108). Refused, the shot was opened again cold
        // and its seek, from a keyframe seconds back in a long-GOP 4K file,
        // was done twice while the picture froze.
        let mut s = self.streams.remove(&clip)?;
        // A frame held from before its seek is not the one for here.
        let held = if s.seeking.is_some() { None } else { s.held.take() };
        s.decoder.take().map(|d| (d, held))
    }

    /// Whether a clip's decoder panicked too often since last asked
    /// (#3181), for saying so once.
    pub fn take_given_up(&mut self) -> bool {
        std::mem::take(&mut self.given_up)
    }

    /// `clip` is not opened until the person seeks or plays (#3181): its
    /// file's decoder panicked too often where the engine plays it itself.
    pub fn give_up(&mut self, clip: ClipId) {
        self.panics.insert(clip, super::PANIC_RESTARTS);
        self.streams.remove(&clip);
        self.opening.remove(&clip);
    }

    /// Clips whose decoders panicked too often may be opened again (#3181).
    pub fn forgive_panics(&mut self) {
        self.panics.clear();
    }

    pub fn clear(&mut self) {
        self.shared.clear();
        self.streams.clear();
        self.opening.clear();
        self.rates.clear();
        self.bytes.clear();
        self.vram.clear();
        self.reversed.clear();
    }

    pub fn len(&self) -> usize {
        self.streams.len()
    }

    /// The queues of the open decoders, for telemetry.
    pub fn queues(&self) -> impl Iterator<Item = &SharedFrameQueue> {
        self.streams.values().map(|s| s.dec().frame_queue())
    }
}

/// The free memory a reversed window's frames are counted against
/// (#3174): the GPU's where frames are held on the GPU, decoded there or
/// uploaded (`None` where it can't be read, and the window guesses), else
/// memory's. It used to follow the hardware decode choice: frames decoded
/// in software and uploaded were counted against nothing.
fn window_free(frames_on_gpu: bool, vram: Option<f64>, memory: f64) -> Option<f64> {
    if frames_on_gpu { vram } else { Some(memory) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #3174: a reversed window is counted against the GPU's free memory
    /// where frames are held on the GPU, however they were decoded, and
    /// guesses where that can't be read; against memory where frames stay
    /// in memory.
    #[test]
    fn a_reversed_window_is_counted_where_its_frames_are_held() {
        assert_eq!(window_free(true, Some(2e9), 16e9), Some(2e9));
        assert_eq!(window_free(true, None, 16e9), None, "the GPU's can't be read: a guess, not memory's");
        assert_eq!(window_free(false, Some(2e9), 16e9), Some(16e9));
    }

    /// Clips at the same frame of the same file read one stream, the one
    /// kept on screen if any of them is; another time, another file,
    /// another rotation or a clip read backward keeps its own.
    #[test]
    fn clips_at_one_frame_of_a_file_share_a_stream() {
        let media = MediaId::new();
        let want = |secs: f64, ahead: bool| Want { clip: ClipId::new(), media, rotation: 0, source_secs: secs, ahead, reversed: false };
        let first = want(2.5, true);
        let same = Want { source_secs: 2.5 + 2e-5, ..want(0.0, false) };
        let later = want(2.54, false);
        let other_file = Want { media: MediaId::new(), ..want(2.5, false) };
        let turned = Want { rotation: 90, ..want(2.5, false) };
        let backward = Want { reversed: true, ..want(2.5, false) };
        let (kept, shared) = sharing(&[first.clone(), same.clone(), later.clone(), other_file.clone(), turned.clone(), backward.clone()]);
        assert_eq!(shared, HashMap::from([(same.clip, first.clip)]));
        let kept: Vec<ClipId> = kept.iter().map(|w| w.clip).collect();
        assert_eq!(kept, [first.clip, later.clip, other_file.clip, turned.clip, backward.clip]);
        let (kept, _) = sharing(&[first.clone(), same]);
        assert!(!kept[0].ahead, "on screen, as the clip sharing it is");
    }

    /// A clone shares the conversions made after it was taken (#2352):
    /// the parked frame is handed to each slider step as a clone, and one
    /// that made its conversion again cost a 4K PQ step a read of its
    /// codes back from the GPU and a fresh upload of them.
    #[test]
    fn a_clone_shares_a_conversion_made_later() {
        let frame = HeldFrame::new(Held::Srgb8(Arc::new(vec![128u8; 4 * 4])), 2, 2, None);
        let step = frame.clone();
        let SourcePixels::Linear(made) = step.source_pixels() else { panic!("bytes go in as linear light") };
        let SourcePixels::Linear(again) = frame.source_pixels() else { panic!("bytes go in as linear light") };
        assert!(Arc::ptr_eq(&made, &again), "the original made the conversion again");
        let SourcePixels::Linear(third) = frame.clone().source_pixels() else { panic!() };
        assert!(Arc::ptr_eq(&made, &third));
        // A clone holds the very same pixels; an equal copy does not.
        assert!(frame.same_pixels(&step));
        assert!(!frame.same_pixels(&HeldFrame::new(Held::Srgb8(Arc::new(vec![128u8; 4 * 4])), 2, 2, None)));
    }

    /// Two seconds of FFV1 at 25 fps, or `None` without ffmpeg.
    fn source() -> Option<std::path::PathBuf> {
        let out = std::env::temp_dir().join("klipp-streams-held-ahead.mkv");
        if out.exists() {
            return Some(out);
        }
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=64x36:r=25:d=2", "-c:v", "ffv1"])
            .arg(&out)
            .status()
            .ok()?
            .success();
        ok.then_some(out)
    }

    /// Waits, as a test ends however it ends, for the decoders it opened
    /// (and those of tests beside it) to finish (#2261): one still making
    /// its GPU pipeline as the process exited crashed in the driver's
    /// teardown, after every test had passed. Declared first, it drops
    /// after the test's streams.
    struct Joined;

    impl Drop for Joined {
        fn drop(&mut self) {
            crate::video_decoder::wait_for_decode_threads(std::time::Duration::from_secs(10));
        }
    }

    /// #2108: a stream opened for a clip coming up holds no frame up,
    /// opening or open and still seeking; one on screen is waited for.
    #[test]
    fn a_stream_opened_ahead_waits_for_nothing() {
        let _joined = Joined;
        let Some(path) = source() else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let _one = BUDGETED.lock().unwrap_or_else(|e| e.into_inner());
        let frame = 1.0 / 25.0;
        let resolve = |_: &Want| Some((path.clone(), 0, Cost { rate: 0.0, frame_bytes: 0.0, ..Default::default() }));
        let ahead = Want { clip: ClipId::new(), media: MediaId::new(), rotation: 0, source_secs: 1.0, reversed: false, ahead: true };
        let mut streams = ClipStreams::new();
        // Whatever other tests ask of the process's decode budget.
        streams.capacity = || f64::INFINITY;
        streams.gpu_headroom = || f64::INFINITY;
        streams.sync(std::slice::from_ref(&ahead), frame, &resolve);
        assert!(streams.has(ahead.clip) && streams.ready(frame), "opening ahead: nothing waits for it");
        // An open that fails, as one can on a busy GPU, is forgotten and
        // asked for again by the next sync, as playback does (#2222).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !streams.streams.contains_key(&ahead.clip) {
            assert!(std::time::Instant::now() < deadline, "the stream never opened");
            std::thread::sleep(std::time::Duration::from_millis(5));
            streams.sync(std::slice::from_ref(&ahead), frame, &resolve);
            streams.pump(frame);
        }
        streams.streams.get_mut(&ahead.clip).unwrap().seeking = Some(1.0);
        assert!(streams.ready(frame), "open ahead, still seeking: nothing waits for it");
        let on_screen = Want { clip: ClipId::new(), ahead: false, ..ahead.clone() };
        streams.sync(&[on_screen.clone(), ahead.clone()], frame, &resolve);
        assert!(!streams.ready(frame), "the one on screen, opening, is waited for");
    }

    /// Tests that open streams the decode budget counts, one at a time: its
    /// count is the process's, and one's streams change another's choice
    /// (taking the software threads another's stream was to get).
    static BUDGETED: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// #2128: room is also what a drive gives. One ahead waits while its
    /// drive is spent; a layer on screen on that drive closes one opened
    /// ahead on the same drive, not one on another; past that it opens all
    /// the same, counted as short of its drive.
    #[test]
    fn a_drive_short_of_reads_closes_its_own_ahead_streams() {
        use crate::disk_budget::{Drive, DriveKind};
        let _joined = Joined;
        let Some(path) = source() else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let _one = BUDGETED.lock().unwrap_or_else(|e| e.into_inner());
        let frame = 1.0 / 25.0;
        let mut streams = ClipStreams::new();
        streams.capacity = || f64::INFINITY;
        streams.gpu_headroom = || f64::INFINITY;
        // A hard disk (120 MB/s, 96 for the streams) and an NVMe drive; each
        // stream reads 40 MB/s.
        let (disk, fast) = (MediaId::new(), MediaId::new());
        let drive = |m: MediaId| if m == disk { Drive { device: 1, kind: DriveKind::Hdd } } else { Drive { device: 2, kind: DriveKind::Nvme } };
        let resolve = |w: &Want| Some((path.clone(), 0, Cost { read_bytes: 40e6, drive: Some(drive(w.media)), ..Default::default() }));
        // Each at a time of its own: clips at one frame of a file share a
        // stream.
        let at = std::cell::Cell::new(0.0);
        let want = |media: MediaId, ahead: bool| {
            at.set(at.get() + 0.5);
            Want { clip: ClipId::new(), media, rotation: 0, source_secs: at.get(), reversed: false, ahead }
        };
        let (a1, a2, a3) = (want(disk, true), want(disk, true), want(disk, true));
        // Each 40 of the disk's 96: two fit, the third doesn't.
        let other = want(fast, true);
        let all = [a1.clone(), a2.clone(), a3.clone(), other.clone()];
        streams.sync(&all, frame, &resolve);
        assert!(streams.has(a1.clip) && streams.has(a2.clip), "two fit the disk");
        // However much one reads, it is opened when its drive is idle.
        let mut alone = ClipStreams::new();
        alone.capacity = || f64::INFINITY;
        alone.gpu_headroom = || f64::INFINITY;
        let huge = |_: &Want| Some((path.clone(), 0, Cost { read_bytes: 500e6, drive: Some(drive(disk)), ..Default::default() }));
        let first = want(disk, true);
        alone.sync(std::slice::from_ref(&first), frame, &huge);
        assert!(alone.has(first.clip), "alone on its drive, it is opened ahead");
        assert!(!streams.has(a3.clip), "a third ahead waits for it");
        assert!(streams.has(other.clip), "the other drive is not this one's");
        let short = super::super::streams_disk_short();
        let tile = want(disk, false);
        let mut with_tile = all.to_vec();
        with_tile.push(tile.clone());
        streams.sync(&with_tile, frame, &resolve);
        assert!(streams.has(tile.clip), "the layer on screen opens");
        assert_eq!([a1.clip, a2.clip].iter().filter(|c| streams.has(**c)).count(), 1, "one on its drive closed for it");
        assert!(streams.has(other.clip), "not the one on another drive");
        assert_eq!(super::super::streams_disk_short(), short, "it fitted once one was closed");
        // Three on screen on the disk: the third finds nothing ahead to
        // close and opens all the same, counted.
        let tiles: Vec<Want> = (0..3).map(|_| want(disk, false)).collect();
        streams.sync(&tiles, frame, &resolve);
        assert!(tiles.iter().all(|t| streams.has(t.clip)), "never left black");
        assert!(super::super::streams_disk_short() > short, "counted as short of its drive");
    }

    /// #2128: room is decode rate, not a count. Nine tiles on screen open
    /// though they ask more than the capacity, closing the stream opened
    /// ahead to make room; one more ahead then waits for room.
    #[test]
    fn layers_on_screen_open_past_the_rate_and_close_one_opened_ahead() {
        let _joined = Joined;
        let Some(path) = source() else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let _one = BUDGETED.lock().unwrap_or_else(|e| e.into_inner());
        let frame = 1.0 / 25.0;
        let mut streams = ClipStreams::new();
        // Room for four streams of 100.
        streams.capacity = || 400.0;
        streams.gpu_headroom = || f64::INFINITY;
        let resolve = |_: &Want| Some((path.clone(), 0, Cost { rate: 100.0, frame_bytes: 0.0, ..Default::default() }));
        let want = |ahead: bool| Want { clip: ClipId::new(), media: MediaId::new(), rotation: 0, source_secs: 0.0, reversed: false, ahead };
        let early = want(true);
        streams.sync(std::slice::from_ref(&early), frame, &resolve);
        assert!(streams.has(early.clip), "room for one ahead");
        let tiles: Vec<Want> = (0..9).map(|_| want(false)).collect();
        let mut all = tiles.clone();
        all.push(early.clone());
        streams.sync(&all, frame, &resolve);
        assert!(tiles.iter().all(|t| streams.has(t.clip)), "every tile on screen has a stream");
        assert!(!streams.has(early.clip), "the one ahead was closed for them");
        let later = want(true);
        all.push(later.clone());
        streams.sync(&all, frame, &resolve);
        assert!(!streams.has(later.clip), "one ahead waits for room");
        // The tiles gone, there is room again.
        streams.sync(std::slice::from_ref(&later), frame, &resolve);
        assert!(streams.has(later.clip));
        // Room for it, but the GPU's decoder is spent: one ahead waits, one
        // on screen opens.
        streams.gpu_headroom = || 0.0;
        let (waits, opens) = (want(true), want(false));
        streams.sync(&[later.clone(), waits.clone(), opens.clone()], frame, &resolve);
        assert!(!streams.has(waits.clip), "ahead waits for the GPU's decoder");
        assert!(streams.has(opens.clip), "on screen opens");
    }

    /// #2128: memory, not a count, as decode rate is. A stream opened ahead
    /// needs room for its whole queue or waits; a layer on screen closes
    /// one opened ahead for room, and past that opens with a shorter
    /// queue, never fewer than four frames, rather than not at all.
    #[test]
    fn layers_on_screen_open_with_a_shorter_queue_when_memory_is_short() {
        let _joined = Joined;
        let Some(path) = source() else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let _one = BUDGETED.lock().unwrap_or_else(|e| e.into_inner());
        let frame = 1.0 / 25.0;
        let mut streams = ClipStreams::new();
        streams.capacity = || f64::INFINITY;
        streams.gpu_headroom = || f64::INFINITY;
        streams.queue_frames = || 16;
        // Half of what is free and what the queues hold: 16 frames of 100
        // bytes with nothing held. Frames in memory, not on the GPU.
        streams.free_memory = || 3200.0;
        streams.free_vram = || None;
        let resolve = |_: &Want| Some((path.clone(), 0, Cost { rate: 1.0, frame_bytes: 100.0, ..Default::default() }));
        let want = |ahead: bool| Want { clip: ClipId::new(), media: MediaId::new(), rotation: 0, source_secs: 0.0, reversed: false, ahead };
        let frames = |s: &ClipStreams, w: &Want| s.bytes.get(&w.clip).map(|b| (b / 100.0) as usize);
        let shortened = super::super::streams_shortened();

        let early = want(true);
        let mut all = vec![early.clone()];
        streams.sync(&all, frame, &resolve);
        assert_eq!(frames(&streams, &early), Some(16), "room for one ahead, whole");
        let first = want(false);
        all.push(first.clone());
        streams.sync(&all, frame, &resolve);
        assert!(!streams.has(early.clip), "the one ahead was closed for a layer on screen");
        assert_eq!(frames(&streams, &first), Some(16), "which then has its whole queue");
        let second = want(false);
        all.push(second.clone());
        streams.sync(&all, frame, &resolve);
        assert_eq!(frames(&streams, &second), Some(8), "the next on screen, a shorter one");
        let later = want(true);
        all.push(later.clone());
        streams.sync(&all, frame, &resolve);
        assert!(!streams.has(later.clip), "one ahead waits for room for its whole queue");
        let (third, fourth) = (want(false), want(false));
        all.extend([third.clone(), fourth.clone()]);
        streams.sync(&all, frame, &resolve);
        assert_eq!((frames(&streams, &third), frames(&streams, &fourth)), (Some(4), Some(4)), "on screen, never refused and never under four frames");
        assert!(super::super::streams_shortened() >= shortened + 3, "and each is counted");
    }

    /// #2128: a stream decoded on the GPU keeps its frames there, so its
    /// queue is held to the GPU's free memory, not the system's: short of
    /// it, layers on screen open with shorter queues as they do short of
    /// memory. One past the GPU decoder's room decodes in software, its
    /// frames in memory.
    #[test]
    fn layers_decoded_on_the_gpu_are_held_to_its_memory() {
        let _joined = Joined;
        let Some(path) = source() else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let _one = BUDGETED.lock().unwrap_or_else(|e| e.into_inner());
        let frame = 1.0 / 25.0;
        let mut streams = ClipStreams::new();
        streams.capacity = || f64::INFINITY;
        streams.gpu_headroom = || f64::INFINITY;
        streams.queue_frames = || 16;
        streams.free_memory = || f64::INFINITY;
        // Half of the GPU's free memory and what is queued there: 16
        // frames of 100 bytes with nothing held.
        streams.free_vram = || Some(3200.0);
        let resolve = |_: &Want| Some((path.clone(), 0, Cost { rate: 1.0, frame_bytes: 100.0, ..Default::default() }));
        let want = || Want { clip: ClipId::new(), media: MediaId::new(), rotation: 0, source_secs: 0.0, reversed: false, ahead: false };
        let on_gpu = |s: &ClipStreams, w: &Want| s.vram.get(&w.clip).map(|b| (b / 100.0) as usize);
        let shortened = super::super::streams_shortened();

        let (first, second, third) = (want(), want(), want());
        let mut all = vec![first.clone()];
        streams.sync(&all, frame, &resolve);
        all.push(second.clone());
        streams.sync(&all, frame, &resolve);
        all.push(third.clone());
        streams.sync(&all, frame, &resolve);
        assert_eq!([&first, &second, &third].map(|w| on_gpu(&streams, w)), [Some(16), Some(8), Some(4)], "held to the GPU's memory, never under four");
        assert!(streams.bytes.is_empty(), "nothing counted in memory");
        assert!(super::super::streams_shortened() >= shortened + 2, "each shortened one counted");

        // Past the GPU decoder's room: decoded in software, in memory.
        streams.gpu_headroom = || 0.0;
        let soft = want();
        all.push(soft.clone());
        streams.sync(&all, frame, &resolve);
        assert_eq!(streams.bytes.get(&soft.clip).map(|b| (b / 100.0) as usize), Some(16), "a software stream's queue is in memory");
        assert!(!streams.vram.contains_key(&soft.clip));
    }

    /// #2116: a stream asking more than the GPU's decoder has left opens
    /// in software, and gives its frame at its time as any other.
    #[test]
    fn a_stream_past_the_gpu_budget_decodes_in_software() {
        let _joined = Joined;
        let Some(path) = source() else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let frame = 1.0 / 25.0;
        let w = Want { clip: ClipId::new(), media: MediaId::new(), rotation: 0, source_secs: 1.0, reversed: false, ahead: false };
        let _one = BUDGETED.lock().unwrap_or_else(|e| e.into_inner());
        crate::decode_budget::set_enabled(true);
        // The GPU asked for more than any gives, by a decoder that is never
        // moved; then a small stream, which software keeps up with.
        let _full = crate::decode_budget::admit(1e15, crate::decode_budget::Priority::Fixed);
        let mut streams = ClipStreams::new();
        let resolve = |_: &Want| Some((path.clone(), 0, Cost { rate: 64.0 * 36.0 * 25.0, frame_bytes: 0.0, ..Default::default() }));
        streams.sync(std::slice::from_ref(&w), frame, &resolve);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !streams.ready(frame) || !streams.streams.contains_key(&w.clip) {
            assert!(std::time::Instant::now() < deadline, "the stream never gave its frame");
            std::thread::sleep(std::time::Duration::from_millis(5));
            streams.sync(std::slice::from_ref(&w), frame, &resolve);
        }
        let decoding = streams.streams[&w.clip].dec().decoding();
        assert!(matches!(decoding, crate::decode_budget::Decoding::Software { threads } if threads >= 2), "{decoding:?}");
        let pts = streams.frame(w.clip).and_then(|h| h.pts).expect("a frame");
        assert!((pts - 1.0).abs() < frame, "the frame at {pts}, not 1 s");
    }

    /// #2108: a stream is opened off the caller's thread: wanted, it is
    /// there at once as opening, and not ready until it has opened and
    /// holds its frame; the sync that asked for it does not wait.
    #[test]
    fn a_stream_opens_off_the_callers_thread() {
        let _joined = Joined;
        let Some(path) = source() else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let _one = BUDGETED.lock().unwrap_or_else(|e| e.into_inner());
        let frame = 1.0 / 25.0;
        let w = Want { clip: ClipId::new(), media: MediaId::new(), rotation: 0, source_secs: 1.0, reversed: false, ahead: false };
        let mut streams = ClipStreams::new();
        let t = std::time::Instant::now();
        streams.sync(std::slice::from_ref(&w), frame, &|_: &Want| Some((path.clone(), 0, Cost { rate: 0.0, frame_bytes: 0.0, ..Default::default() })));
        let asked = t.elapsed();
        assert!(streams.has(w.clip), "wanted: counted while it opens");
        assert!(streams.opening.contains_key(&w.clip) && !streams.streams.contains_key(&w.clip), "still opening when the sync returns");
        assert!(!streams.ready(frame), "not ready while opening");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !streams.ready(frame) {
            assert!(std::time::Instant::now() < deadline, "the stream never held its frame");
            std::thread::sleep(std::time::Duration::from_millis(5));
            streams.pump(frame);
        }
        assert!(streams.opening.is_empty() && streams.streams.contains_key(&w.clip));
        eprintln!("sync returned in {asked:?}");
        // A stream still opening is taken over once it lands. Back in the
        // file from the one leaving: not its stream going on (#2397).
        let late = Want { clip: ClipId::new(), source_secs: 0.0, ..w.clone() };
        streams.sync(std::slice::from_ref(&late), frame, &|_: &Want| Some((path.clone(), 0, Cost { rate: 0.0, frame_bytes: 0.0, ..Default::default() })));
        assert!(streams.opening.contains_key(&late.clip));
        assert!(streams.take(late.clip).is_some(), "taken over while it opened");
        assert!(!streams.has(late.clip));
        // A stream no longer wanted while it opens is let go.
        let other = Want { clip: ClipId::new(), ..w.clone() };
        streams.sync(std::slice::from_ref(&other), frame, &|_: &Want| Some((path.clone(), 0, Cost { rate: 0.0, frame_bytes: 0.0, ..Default::default() })));
        streams.sync(&[], frame, &|_: &Want| Some((path.clone(), 0, Cost { rate: 0.0, frame_bytes: 0.0, ..Default::default() })));
        assert!(!streams.has(other.clip) && !streams.has(w.clip), "nothing wanted, nothing kept");
    }

    /// #2108: a stream still seeking is taken over all the same, its
    /// decoder with no frame (a frame from before the seek is not the one
    /// for here), and its first frame comes from that same decoder: the
    /// seek is not thrown away and done again.
    #[test]
    fn a_stream_still_seeking_is_taken_over() {
        let _joined = Joined;
        let Some(path) = source() else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let _one = BUDGETED.lock().unwrap_or_else(|e| e.into_inner());
        let frame = 1.0 / 25.0;
        let w = Want { clip: ClipId::new(), media: MediaId::new(), rotation: 0, source_secs: 1.0, reversed: false, ahead: false };
        let mut streams = ClipStreams::new();
        streams.sync(std::slice::from_ref(&w), frame, &|_: &Want| Some((path.clone(), 0, Cost { rate: 0.0, frame_bytes: 0.0, ..Default::default() })));
        // Opened off this thread: it lands on a pump.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !streams.streams.contains_key(&w.clip) {
            assert!(std::time::Instant::now() < deadline, "the stream never opened");
            std::thread::sleep(std::time::Duration::from_millis(5));
            streams.pump(frame);
        }
        let s = streams.streams.get_mut(&w.clip).unwrap();
        // As a seek in flight leaves it: still seeking, holding an older frame.
        s.seeking = Some(1.0);
        s.held = Some(HeldFrame::new(Held::Linear(Arc::new(vec![0.0; 4])), 1, 1, Some(0.2)));
        let (dec, held) = streams.take(w.clip).expect("taken while seeking");
        assert!(held.is_none(), "the frame from before the seek is not handed over");
        assert!(!streams.has(w.clip), "the stream is the primary's now");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let first = loop {
            if let Some(f) = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner()).front().map(|f| f.pts_secs()) {
                break f;
            }
            assert!(std::time::Instant::now() < deadline, "the taken decoder never gave a frame");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert!((first - 1.0).abs() < 0.05, "its first frame is the seek's: {first}");
    }

    /// #411 review: a stream holding a frame past its new time is sought
    /// once, and not again by every sync while that seek is in flight.
    #[test]
    fn a_stream_held_ahead_is_sought_once() {
        let _joined = Joined;
        let Some(path) = source() else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let _one = BUDGETED.lock().unwrap_or_else(|e| e.into_inner());
        let frame = 1.0 / 25.0;
        let want = |secs: f64| Want { clip: ClipId::new(), media: MediaId::new(), rotation: 0, source_secs: secs, reversed: false, ahead: false };
        let w = want(1.0);
        let mut streams = ClipStreams::new();
        let resolve = |_: &Want| Some((path.clone(), 0, Cost { rate: 0.0, frame_bytes: 0.0, ..Default::default() }));
        // Until it is open and holds its frame: an open that fails, as
        // one can on a busy GPU, leaves nothing to wait for, and is asked
        // for again by the next sync, as playback does (#2222).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        streams.sync(std::slice::from_ref(&w), frame, &resolve);
        while !(streams.streams.contains_key(&w.clip) && streams.ready(frame)) {
            assert!(std::time::Instant::now() < deadline, "the stream never held its frame");
            std::thread::sleep(std::time::Duration::from_millis(5));
            streams.sync(std::slice::from_ref(&w), frame, &resolve);
            streams.pump(frame);
        }
        let s = streams.streams.get_mut(&w.clip).unwrap();
        // As playback leaves it: the target a few frames back of the
        // frame it holds, with no seek behind that frame.
        s.target = 0.88;
        s.held_from_seek = false;
        let back = Want { source_secs: 0.88, ..w };
        let before = s.dec().requested_seek();
        streams.sync(std::slice::from_ref(&back), frame, &resolve);
        let s = &streams.streams[&back.clip];
        assert_eq!(s.dec().requested_seek(), before + 1, "a stream held ahead was not sought");
        let once = s.dec().requested_seek();
        // The seek is still in flight, or has just landed; either way a
        // second sync at the same time must not seek again.
        // Put it back in flight whatever has landed: the seek pending, the
        // old frame still held, ahead of the time.
        let s = streams.streams.get_mut(&back.clip).unwrap();
        s.seeking = Some(0.88);
        s.held_from_seek = false;
        if let Some(h) = s.held.as_mut() {
            h.pts = Some(1.0);
        }
        streams.sync(std::slice::from_ref(&back), frame, &resolve);
        assert_eq!(streams.streams[&back.clip].dec().requested_seek(), once, "sought again while the seek was in flight");
    }

    /// #1494: a decoder taken over with its frame for the target already
    /// taken (playback took it at the cut), its queue starting a frame
    /// later. Holding nothing, it is sought, and holds its frame; handed
    /// that frame, it holds it at once.
    #[test]
    fn a_stream_taken_over_past_its_frame_still_gets_it() {
        let _joined = Joined;
        let Some(path) = source() else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let _one = BUDGETED.lock().unwrap_or_else(|e| e.into_inner());
        let frame = 1.0 / 25.0;
        // A decoder as playback leaves it at a cut: decoded on from 0, the
        // frame at the cut popped, the next ones queued.
        let taken_over = |at: usize| {
            let dec = VideoDecoder::open(&path, 0).expect("opens");
            dec.resume();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while dec.frame_queue().lock().unwrap().len() < at + 2 {
                assert!(std::time::Instant::now() < deadline, "the decoder never queued its frames");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let mut q = dec.frame_queue().lock().unwrap();
            for _ in 0..at {
                q.pop();
            }
            let cut = q.pop().expect("the frame at the cut");
            drop(q);
            (dec, cut)
        };
        for hand_on in [false, true] {
            let (dec, cut) = taken_over(5);
            let target = cut.pts_secs();
            let clip = ClipId::new();
            let mut streams = ClipStreams::new();
            streams.adopt(clip, dec, target, hand_on.then(|| HeldFrame::from_frame(cut)));
            let w = Want { clip, media: MediaId::new(), rotation: 0, source_secs: target, reversed: false, ahead: false };
            let resolve = |_: &Want| Some((path.clone(), 0, Cost { rate: 0.0, frame_bytes: 0.0, ..Default::default() }));
            if hand_on {
                streams.sync(std::slice::from_ref(&w), frame, &resolve);
                assert!(streams.ready(frame), "handed its frame at the cut, it holds it");
                continue;
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                streams.sync(std::slice::from_ref(&w), frame, &resolve);
                if streams.ready(frame) {
                    break;
                }
                assert!(std::time::Instant::now() < deadline, "a stream holding nothing, its queue past its time, never got its frame");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let held = streams.frame(clip).and_then(|h| h.pts).expect("a frame held");
            assert!((held - target).abs() < frame / 2.0, "the frame for {target:.3}s, not {held:.3}s");
        }
    }
}
