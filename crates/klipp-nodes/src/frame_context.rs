//! What a non-pure node reads besides its input (M4 step 1): source frames
//! at other times, its analysis, and its plug-in instance, each given to it
//! explicitly and scoped to it, in place of the renderer's special stages
//! and their shared preparation state.
//!
//! - [`SourceProvider`]: the clip's source at another time, in one of two
//!   contracts that are never interchangeable: [`EncodedFrame`] (sRGB RGBA8
//!   in the authored primaries, as a plug-in reads it) and [`LinearFrame`]
//!   (linear RGBA f32 in the working space, as the denoiser reads it).
//!   [`BoundedSources`] holds every request to the clip's source range,
//!   the render size and a per-node budget, and says why a frame is
//!   missing rather than standing another in for it.
//! - [`AnalysisInputs`]: a node's analysis by its id, at the schema it
//!   reads: absent, stale, or ready.
//! - [`PluginInstances`]: plug-in instances owned by one render and keyed
//!   by clip and node, so two clips, two nodes or two renders never share
//!   one.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use klipp_core::ClipId;

use crate::graph::NodeId;

/// Everything a non-pure node is given for one frame.
pub struct FrameContext<'a> {
    /// The clip being rendered: a plug-in instance's slot is its.
    pub clip: ClipId,
    /// The timeline frame index rendered: continuity is counted in frames.
    pub frame: i64,
    /// The clip's media and its source in-point (a nil media for a clip
    /// without one): a plug-in's state is reset when either changes.
    pub source: (klipp_core::MediaId, f64),
    /// The clip's time, as the executor is given it.
    pub clip_time_secs: f64,
    /// The source time of the frame being rendered: where a node's
    /// neighbours are counted from.
    pub source_secs: f64,
    /// The rendered frame's presentation time in the media, when known: a
    /// frame blend weighs from it, and can't blend without it.
    pub source_pts: Option<f64>,
    pub frame_number: u64,
    pub fps: f64,
    pub sources: &'a mut dyn SourceProvider,
    pub analysis: &'a dyn AnalysisInputs,
    pub instances: &'a mut PluginInstances,
    /// The OFX host the render lends its plug-in nodes (M4 step 3): none,
    /// and every OFX node is refused as missing.
    pub ofx: Option<&'a dyn crate::plugin::OfxNodeHost>,
    /// The source time the clip shows at a clip time, in seconds: its
    /// speed, ramp and direction. Where a plug-in's other frames, named in
    /// clip time, are read from.
    pub source_at: &'a dyn Fn(f64) -> f64,
}

/// A source frame asked for, or why there is none. Never a substitute.
#[derive(Debug, Clone, PartialEq)]
pub enum Neighbour<F> {
    Frame(F),
    Missing(Missing),
}

/// Why a source frame asked for isn't given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// Outside the clip's source range.
    OutOfRange,
    /// The source gave a frame of another size than the render's; it is
    /// dropped, never resampled.
    WrongSize { width: u32, height: u32 },
    /// The source has no frame there (a decode failure, a source that
    /// can't seek).
    Unavailable,
    /// The node asked for more frames this frame than its budget.
    OverBudget,
}

/// A source frame as a plug-in reads it: sRGB-encoded RGBA8 in the
/// authored primaries.
#[derive(Debug, Clone, PartialEq)]
pub struct EncodedFrame {
    pub rgba8: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// The source time it is the frame of.
    pub source_secs: f64,
}

/// A source frame as the denoiser reads it: linear RGBA f32 in the
/// working space, rendered as the current frame is.
#[derive(Debug, Clone, PartialEq)]
pub struct LinearFrame {
    /// Shared with whoever holds it: a parked viewer's held neighbour
    /// (#1863) reaches the node without a copy. Read only.
    pub rgba: std::sync::Arc<Vec<f32>>,
    pub width: u32,
    pub height: u32,
    /// The source time it is the frame of.
    pub source_secs: f64,
    /// Its presentation time in the media, when the reader knows it: what
    /// a frame blend weighs it by.
    pub pts: Option<f64>,
}

/// A source frame already on the GPU, linear working-space f32 at the
/// render size on the executor's own device (#1988): what a temporal
/// denoise on the chain binds as a neighbour, with no readback and no
/// upload. `read` brings it to the CPU for a node that ends up there.
pub trait GpuFramePixels: Send + Sync {
    fn buffer(&self) -> &wgpu::Buffer;
    fn read(&self) -> std::sync::Arc<Vec<f32>>;
    /// Whether [`Self::buffer`] holds sRGB bytes, 4 a pixel, rather than
    /// linear floats: a viewer stream's decoded frame. The chain decodes
    /// it to linear into a buffer of its own before binding it.
    fn srgb8(&self) -> bool {
        false
    }
    /// The size of [`Self::srgb8`]'s bytes where it isn't the frame's
    /// (#2854): a viewer stream decoding below the render size, its frame
    /// fitted into the render on the GPU as the frame shown is. `None`
    /// (the default): the frame's size.
    fn source_size(&self) -> Option<(u32, u32)> {
        None
    }
}

/// A source frame on the GPU (#1988), as [`SourceProvider::linear_on_gpu`]
/// gives it.
#[derive(Clone)]
pub struct GpuLinearFrame {
    pub pixels: std::sync::Arc<dyn GpuFramePixels>,
    pub width: u32,
    pub height: u32,
    /// The source time it is the frame of.
    pub source_secs: f64,
}

impl std::fmt::Debug for GpuLinearFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuLinearFrame").field("width", &self.width).field("height", &self.height).field("source_secs", &self.source_secs).finish()
    }
}

/// A frame's size.
pub trait FrameSize {
    fn size(&self) -> (u32, u32);
}

impl FrameSize for EncodedFrame {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

impl FrameSize for LinearFrame {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

impl FrameSize for GpuLinearFrame {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

/// The clip's source at another time, in source seconds. Implementors
/// give what the source has; [`BoundedSources`] holds the requests to the
/// contract.
pub trait SourceProvider {
    /// The source range a request may name: `[in, out)`, in source seconds.
    fn bounds(&self) -> (f64, f64);
    /// The node whose requests follow, and the source frames its type
    /// declares it reads per frame. Only a bounding provider counts them.
    fn for_node(&mut self, _node: NodeId, _budget: usize) {}
    fn encoded(&mut self, source_secs: f64) -> Neighbour<EncodedFrame>;
    fn linear(&mut self, source_secs: f64) -> Neighbour<LinearFrame>;
    /// [`Self::linear`]'s frame where it is already on the GPU (#1988): a
    /// neighbour the chain binds as it is. `Unavailable` (the default)
    /// where the source has it only on the CPU, or not at all: the caller
    /// asks [`Self::linear`] then, and only that request is counted.
    fn linear_on_gpu(&mut self, _source_secs: f64) -> Neighbour<GpuLinearFrame> {
        Neighbour::Missing(Missing::Unavailable)
    }
    /// One of the media's frames, in seconds (1 / its rate), when known: a
    /// node's neighbours are the captures either side, whatever the
    /// sequence's rate. `None` when the source can't say.
    fn frame_secs(&self) -> Option<f64> {
        None
    }
    /// The media's next frame after the one rendered (shown at
    /// `source_secs`), from the reader it came from, at the render size,
    /// with its presentation time: a frame blend's other frame.
    fn next(&mut self, _source_secs: f64) -> Neighbour<LinearFrame> {
        Neighbour::Missing(Missing::Unavailable)
    }
    /// [`Self::next`]'s frame where it is already on the GPU (#2854), its
    /// `source_secs` its presentation time: a frame blend on the chain
    /// binds it as it is. `Unavailable` (the default) where the source has
    /// it only on the CPU, or not at all: the caller asks [`Self::next`]
    /// then, and only that request is counted.
    fn next_on_gpu(&mut self, _source_secs: f64) -> Neighbour<GpuLinearFrame> {
        Neighbour::Missing(Missing::Unavailable)
    }
}

/// No source: a run without one (an effects plan over a picture already
/// drawn, a title, an adjustment layer) gives a node that asks for a
/// source frame none. Its range is empty, so every time is past it
/// ([`Missing::OutOfRange`]): a node there runs with the frame it has,
/// declined as a fallback (step 10), never as a frame that failed to read.
pub struct NoSources;

impl SourceProvider for NoSources {
    fn bounds(&self) -> (f64, f64) {
        (0.0, 0.0)
    }
    fn encoded(&mut self, _source_secs: f64) -> Neighbour<EncodedFrame> {
        Neighbour::Missing(Missing::OutOfRange)
    }
    fn linear(&mut self, _source_secs: f64) -> Neighbour<LinearFrame> {
        Neighbour::Missing(Missing::OutOfRange)
    }
    fn next(&mut self, _source_secs: f64) -> Neighbour<LinearFrame> {
        Neighbour::Missing(Missing::OutOfRange)
    }
}

/// The most source frames any node may ask for in one frame, whatever
/// its type declares: past it, a runaway node is refused, visibly.
pub const NEIGHBOUR_CEILING: usize = 32;

/// A [`SourceProvider`] held to the contract: a request outside the
/// source range is [`Missing::OutOfRange`] without asking the source, a
/// frame not at `size` is [`Missing::WrongSize`], and past the node's
/// declared budget (at most [`NEIGHBOUR_CEILING`]) it is
/// [`Missing::OverBudget`].
///
/// One is made for each frame rendered: the budget is a node's for one
/// frame, so the next frame's requests are counted afresh by the next
/// one, never against this one's.
pub struct BoundedSources<'s> {
    inner: &'s mut dyn SourceProvider,
    size: (u32, u32),
    asked: HashMap<NodeId, usize>,
    /// The node asking, and the requests its type declares per frame.
    node: Option<(NodeId, usize)>,
}

impl<'s> BoundedSources<'s> {
    /// `inner` for a render at `size`.
    pub fn new(inner: &'s mut dyn SourceProvider, width: u32, height: u32) -> Self {
        Self { inner, size: (width, height), asked: HashMap::new(), node: None }
    }

    fn admit(&mut self, source_secs: f64) -> Result<(), Missing> {
        let (from, to) = self.inner.bounds();
        if !(source_secs >= from && source_secs < to) {
            return Err(Missing::OutOfRange);
        }
        // No node named: nothing declared, so nothing admitted.
        let (node, budget) = self.node.unwrap_or_default();
        let asked = self.asked.entry(node).or_default();
        if *asked >= budget {
            return Err(Missing::OverBudget);
        }
        *asked += 1;
        Ok(())
    }

    fn sized<F: FrameSize>(&self, got: Neighbour<F>) -> Neighbour<F> {
        match got {
            Neighbour::Frame(f) if f.size() != self.size => {
                let (width, height) = f.size();
                Neighbour::Missing(Missing::WrongSize { width, height })
            }
            other => other,
        }
    }
}

impl SourceProvider for BoundedSources<'_> {
    fn bounds(&self) -> (f64, f64) {
        self.inner.bounds()
    }

    fn frame_secs(&self) -> Option<f64> {
        self.inner.frame_secs()
    }

    /// The node whose requests follow, and the source frames its type
    /// declares it reads per frame (a denoiser its window, a temporal
    /// plug-in what its metadata says), held to [`NEIGHBOUR_CEILING`].
    fn for_node(&mut self, node: NodeId, budget: usize) {
        self.node = Some((node, budget.min(NEIGHBOUR_CEILING)));
    }

    fn encoded(&mut self, source_secs: f64) -> Neighbour<EncodedFrame> {
        if let Err(m) = self.admit(source_secs) {
            return Neighbour::Missing(m);
        }
        let got = self.inner.encoded(source_secs);
        self.sized(got)
    }

    fn linear(&mut self, source_secs: f64) -> Neighbour<LinearFrame> {
        if let Err(m) = self.admit(source_secs) {
            return Neighbour::Missing(m);
        }
        let got = self.inner.linear(source_secs);
        self.sized(got)
    }

    /// Counted against the budget only when it gives a frame: one it
    /// hasn't on the GPU is asked for again with [`Self::linear`], which
    /// counts then, so a frame read either way is one request.
    fn linear_on_gpu(&mut self, source_secs: f64) -> Neighbour<GpuLinearFrame> {
        if let Err(m) = self.admit(source_secs) {
            return Neighbour::Missing(m);
        }
        let got = self.inner.linear_on_gpu(source_secs);
        match self.sized(got) {
            Neighbour::Missing(Missing::Unavailable) => {
                let (node, _) = self.node.unwrap_or_default();
                if let Some(asked) = self.asked.get_mut(&node) {
                    *asked = asked.saturating_sub(1);
                }
                Neighbour::Missing(Missing::Unavailable)
            }
            got => got,
        }
    }

    /// Counted against the budget like any request; the next frame past
    /// the media's end is out of range, and so is none at all where one
    /// media frame on is past it (a reader at its end has nothing after).
    fn next(&mut self, source_secs: f64) -> Neighbour<LinearFrame> {
        if let Err(m) = self.admit(source_secs) {
            return Neighbour::Missing(m);
        }
        let end = self.inner.bounds().1;
        match self.inner.next(source_secs) {
            Neighbour::Frame(f) if f.pts.is_some_and(|p| p >= end) => Neighbour::Missing(Missing::OutOfRange),
            Neighbour::Missing(Missing::Unavailable) if self.inner.frame_secs().is_some_and(|d| source_secs + d >= end - 1e-6) => Neighbour::Missing(Missing::OutOfRange),
            got => self.sized(got),
        }
    }

    /// [`Self::next`] on the GPU (#2854): past the media's end as `next`
    /// is, and counted only when it gives a frame, as `linear_on_gpu` is.
    fn next_on_gpu(&mut self, source_secs: f64) -> Neighbour<GpuLinearFrame> {
        if let Err(m) = self.admit(source_secs) {
            return Neighbour::Missing(m);
        }
        let end = self.inner.bounds().1;
        match self.inner.next_on_gpu(source_secs) {
            Neighbour::Frame(f) if f.source_secs >= end => Neighbour::Missing(Missing::OutOfRange),
            got => match self.sized(got) {
                Neighbour::Missing(Missing::Unavailable) => {
                    let (node, _) = self.node.unwrap_or_default();
                    if let Some(asked) = self.asked.get_mut(&node) {
                        *asked = asked.saturating_sub(1);
                    }
                    Neighbour::Missing(Missing::Unavailable)
                }
                got => got,
            },
        }
    }
}

/// A node's analysis at the schema it reads.
pub enum Analysis<'a> {
    /// The node has none.
    Absent,
    /// The node has one at another schema: re-analyse, never read it.
    Stale { found: u32 },
    Ready(&'a AnalysisBlob),
}

/// An analysis result, at the schema it was made with.
pub struct AnalysisBlob {
    pub schema: u32,
    pub data: Arc<dyn Any + Send + Sync>,
}

/// The schema an analysis node's blob is given at: its data an
/// `Arc<klipp_effects::node_analysis::NodeAnalysis>`, its settings applied
/// (M4 step 9). One found at another schema is stale.
pub const NODE_ANALYSIS_SCHEMA: u32 = 1;

/// The analyses a render's nodes read, by node id.
pub trait AnalysisInputs {
    fn lookup(&self, node: NodeId, schema: u32) -> Analysis<'_>;
    /// `node`'s analysis by the effect it runs: what an analysis node
    /// reads. A provider keyed by the effect (its kind and the settings its
    /// derivation takes) answers here; by default, by the node.
    fn lookup_effect(&self, node: NodeId, _effect: &klipp_effects::BuiltinEffect, schema: u32) -> Analysis<'_> {
        self.lookup(node, schema)
    }
}

/// No analyses: every lookup is [`Analysis::Absent`].
pub struct NoAnalysis;

impl AnalysisInputs for NoAnalysis {
    fn lookup(&self, _node: NodeId, _schema: u32) -> Analysis<'_> {
        Analysis::Absent
    }
}

/// Analyses held by node id.
#[derive(Default)]
pub struct AnalysisStore {
    by_node: HashMap<NodeId, AnalysisBlob>,
}

impl AnalysisStore {
    pub fn insert(&mut self, node: NodeId, blob: AnalysisBlob) {
        self.by_node.insert(node, blob);
    }
}

impl AnalysisInputs for AnalysisStore {
    fn lookup(&self, node: NodeId, schema: u32) -> Analysis<'_> {
        match self.by_node.get(&node) {
            None => Analysis::Absent,
            Some(blob) if blob.schema != schema => Analysis::Stale { found: blob.schema },
            Some(blob) => Analysis::Ready(blob),
        }
    }
}

/// What a plug-in instance was last run with: enough for its owner to
/// decide a reset (a seek, a reverse, a new size, another source, the node
/// switched).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlotStamp {
    /// The timeline frame index rendered: continuity is counted in frames,
    /// never derived from seconds (which round at NTSC rates).
    pub frame: i64,
    pub clip_time_secs: f64,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    /// The clip's media and its source in-point.
    pub source: (klipp_core::MediaId, f64),
    pub enabled: bool,
}

/// A plug-in's instance for one call, and the stamp of its last call
/// (`None` on its first).
pub struct Slot<'a, T> {
    pub instance: &'a mut T,
    pub last: Option<SlotStamp>,
}

/// The state a plug-in instance keeps, in bytes (a frei0r instance: its
/// size × 4 × the frames it holds), for the owner's cap.
pub trait SlotBytes {
    fn bytes(&self) -> usize;
}

/// How far forward a frame may be from a slot's last and still be play:
/// further, or backward, is a seek away, and the slot goes.
pub const CONTINUATION_FRAMES: i64 = 4;

/// The most state one owner keeps across its instances; past it the least
/// recently rendered go first.
pub const MAX_BYTES: usize = 1 << 30;

/// The most slots one owner holds, a backstop to [`MAX_BYTES`].
pub const MAX_SLOTS: usize = 256;

struct Held {
    instance: Box<dyn Any + Send>,
    bytes: fn(&dyn Any) -> usize,
    stamp: SlotStamp,
    /// When it was last rendered, by the owner's call count.
    used: u64,
}

/// Plug-in instances owned by one render (the viewer's, each export
/// job's and each preview's are separate owners), keyed by clip and node,
/// so viewer and export never share one. A slot is used under `&mut`, one
/// call at a time.
#[derive(Default)]
pub struct PluginInstances {
    by_key: HashMap<(ClipId, NodeId), Held>,
    calls: u64,
}

impl PluginInstances {
    /// The instance of `node` on `clip`, made by `make` the first time (or
    /// when the slot holds another type), with the stamp of its last call;
    /// this call's `stamp` is kept for the next.
    pub fn slot<T: Send + SlotBytes + 'static>(&mut self, clip: ClipId, node: NodeId, stamp: SlotStamp, make: impl FnOnce() -> T) -> Slot<'_, T> {
        let key = (clip, node);
        self.calls += 1;
        let used = self.calls;
        let last = match self.by_key.get_mut(&key).filter(|h| h.instance.is::<T>()) {
            Some(held) => {
                held.used = used;
                Some(std::mem::replace(&mut held.stamp, stamp))
            }
            None => {
                if !self.by_key.contains_key(&key) && self.by_key.len() >= MAX_SLOTS {
                    self.evict_least_recent(key);
                }
                let bytes: fn(&dyn Any) -> usize = |a| a.downcast_ref::<T>().map_or(0, T::bytes);
                self.by_key.insert(key, Held { instance: Box::new(make()), bytes, stamp, used });
                None
            }
        };
        let held = self.by_key.get_mut(&key).expect("held");
        Slot { instance: held.instance.downcast_mut::<T>().expect("the slot holds a T"), last }
    }

    /// The render finished timeline frame `frame`: a slot last rendered
    /// more than [`CONTINUATION_FRAMES`] before it, or after it (a seek
    /// away, or idle since), goes; then, past [`MAX_BYTES`], the least
    /// recently rendered go.
    pub fn end_frame(&mut self, frame: i64) {
        self.by_key.retain(|_, h| (0..=CONTINUATION_FRAMES).contains(&(frame - h.stamp.frame)));
        while self.bytes() > MAX_BYTES && self.by_key.len() > 1 {
            self.evict_least_recent((ClipId::nil(), NodeId::nil()));
        }
    }

    /// The state held, in bytes.
    pub fn bytes(&self) -> usize {
        self.by_key.values().map(|h| (h.bytes)(h.instance.as_ref())).sum()
    }

    /// Drop the least recently rendered slot but `keep`'s.
    fn evict_least_recent(&mut self, keep: (ClipId, NodeId)) {
        if let Some(key) = self.by_key.iter().filter(|(k, _)| **k != keep).min_by_key(|(_, h)| h.used).map(|(k, _)| *k) {
            self.by_key.remove(&key);
        }
    }

    /// Drop `clip`'s instances.
    pub fn reset(&mut self, clip: ClipId) {
        self.by_key.retain(|(c, _), _| *c != clip);
    }

    /// How many instances are held.
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source from 2 s to 3 s, its frames at `size`, each stamped with
    /// the time asked for; it counts what it was asked.
    struct Fake {
        size: (u32, u32),
        asked: usize,
    }

    impl SourceProvider for Fake {
        fn bounds(&self) -> (f64, f64) {
            (2.0, 3.0)
        }
        fn encoded(&mut self, source_secs: f64) -> Neighbour<EncodedFrame> {
            self.asked += 1;
            let (width, height) = self.size;
            Neighbour::Frame(EncodedFrame { rgba8: vec![7; (width * height * 4) as usize], width, height, source_secs })
        }
        fn linear(&mut self, source_secs: f64) -> Neighbour<LinearFrame> {
            self.asked += 1;
            let (width, height) = self.size;
            Neighbour::Frame(LinearFrame { rgba: std::sync::Arc::new(vec![0.5; (width * height * 4) as usize]), width, height, source_secs, pts: None })
        }
    }

    fn node() -> NodeId {
        NodeId::new_v4()
    }

    /// A reader that has no frame after the one shown, its frames 0.04 s
    /// apart, in a source from 2 s to 3 s.
    struct Ended;

    impl SourceProvider for Ended {
        fn bounds(&self) -> (f64, f64) {
            (2.0, 3.0)
        }
        fn encoded(&mut self, _source_secs: f64) -> Neighbour<EncodedFrame> {
            Neighbour::Missing(Missing::Unavailable)
        }
        fn linear(&mut self, _source_secs: f64) -> Neighbour<LinearFrame> {
            Neighbour::Missing(Missing::Unavailable)
        }
        fn frame_secs(&self) -> Option<f64> {
            Some(0.04)
        }
        fn next(&mut self, _source_secs: f64) -> Neighbour<LinearFrame> {
            Neighbour::Missing(Missing::Unavailable)
        }
    }

    /// No next frame where one media frame on is past the source is its
    /// edge, out of range (a fallback, step 10); none inside it is a frame
    /// that couldn't be read.
    #[test]
    fn no_next_frame_at_the_sources_end_is_out_of_range() {
        let mut ended = Ended;
        let mut s = BoundedSources::new(&mut ended, 4, 2);
        s.for_node(node(), 8);
        assert_eq!(s.next(2.97), Neighbour::Missing(Missing::OutOfRange), "the last frame");
        assert_eq!(s.next(2.5), Neighbour::Missing(Missing::Unavailable), "mid-source");
    }

    /// No source at all: every time is past it.
    #[test]
    fn no_sources_is_out_of_range_everywhere() {
        let mut none = NoSources;
        assert_eq!(none.linear(1.0), Neighbour::Missing(Missing::OutOfRange));
        assert_eq!(none.encoded(1.0), Neighbour::Missing(Missing::OutOfRange));
        assert_eq!(none.next(1.0), Neighbour::Missing(Missing::OutOfRange));
    }

    /// Outside `[in, out)` is out of range, and the source isn't asked.
    #[test]
    fn a_request_outside_the_source_range_is_out_of_range() {
        let mut fake = Fake { size: (4, 2), asked: 0 };
        let mut s = BoundedSources::new(&mut fake, 4, 2);
        s.for_node(node(), 8);
        for t in [1.99, 3.0, 4.0, f64::NAN] {
            assert_eq!(s.linear(t), Neighbour::Missing(Missing::OutOfRange), "{t}");
            assert_eq!(s.encoded(t), Neighbour::Missing(Missing::OutOfRange), "{t}");
        }
        drop(s);
        assert_eq!(fake.asked, 0);
    }

    /// A frame at its source time; one of another size than the render's
    /// is dropped and said so, never resampled.
    #[test]
    fn a_frame_carries_its_time_and_another_size_is_missing() {
        let mut fake = Fake { size: (4, 2), asked: 0 };
        let mut s = BoundedSources::new(&mut fake, 4, 2);
        s.for_node(node(), 8);
        match s.linear(2.5) {
            Neighbour::Frame(f) => assert_eq!((f.source_secs, f.width, f.height), (2.5, 4, 2)),
            other => panic!("{other:?}"),
        }
        let mut other = Fake { size: (8, 4), asked: 0 };
        let mut s = BoundedSources::new(&mut other, 4, 2);
        s.for_node(node(), 8);
        assert_eq!(s.encoded(2.5), Neighbour::Missing(Missing::WrongSize { width: 8, height: 4 }));
        assert_eq!(s.linear(2.5), Neighbour::Missing(Missing::WrongSize { width: 8, height: 4 }));
    }

    /// A node gets the requests its type declares, never past the ceiling,
    /// each node its own; a node that declares none gets none.
    #[test]
    fn each_node_is_held_to_its_declared_budget() {
        let mut fake = Fake { size: (1, 1), asked: 0 };
        let mut s = BoundedSources::new(&mut fake, 1, 1);
        let (a, b) = (node(), node());
        s.for_node(a, 2);
        assert!(matches!(s.linear(2.1), Neighbour::Frame(_)));
        assert!(matches!(s.encoded(2.2), Neighbour::Frame(_)));
        assert_eq!(s.linear(2.3), Neighbour::Missing(Missing::OverBudget));
        s.for_node(b, 1000);
        let given = (0..100).filter(|i| matches!(s.linear(2.0 + *i as f64 / 200.0), Neighbour::Frame(_))).count();
        assert_eq!(given, NEIGHBOUR_CEILING);
        s.for_node(node(), 0);
        assert_eq!(s.linear(2.5), Neighbour::Missing(Missing::OverBudget));
        let mut none = BoundedSources::new(&mut fake, 1, 1);
        assert_eq!(none.linear(2.5), Neighbour::Missing(Missing::OverBudget), "no node named");
    }

    /// The budget is per frame: the next frame's bounded sources give a
    /// node its full budget again.
    #[test]
    fn each_frames_bounded_sources_count_afresh() {
        let mut fake = Fake { size: (1, 1), asked: 0 };
        let n = node();
        for frame in 0..3 {
            let mut s = BoundedSources::new(&mut fake, 1, 1);
            s.for_node(n, 2);
            assert!(matches!(s.linear(2.1), Neighbour::Frame(_)), "frame {frame}");
            assert!(matches!(s.linear(2.2), Neighbour::Frame(_)), "frame {frame}");
            assert_eq!(s.linear(2.3), Neighbour::Missing(Missing::OverBudget), "frame {frame}");
        }
        assert_eq!(fake.asked, 6, "two a frame, three frames");
    }

    /// No analysis is absent; a stored one is ready at its schema and
    /// stale at another, never read.
    #[test]
    fn an_analysis_is_absent_stale_or_ready() {
        let n = node();
        assert!(matches!(NoAnalysis.lookup(n, 1), Analysis::Absent));
        let mut store = AnalysisStore::default();
        assert!(matches!(store.lookup(n, 1), Analysis::Absent));
        store.insert(n, AnalysisBlob { schema: 2, data: Arc::new(vec![1.0f32, 2.0]) });
        assert!(matches!(store.lookup(n, 1), Analysis::Stale { found: 2 }));
        match store.lookup(n, 2) {
            Analysis::Ready(blob) => assert_eq!(blob.data.downcast_ref::<Vec<f32>>(), Some(&vec![1.0, 2.0])),
            _ => panic!("not ready at its own schema"),
        }
        assert!(matches!(store.lookup(node(), 2), Analysis::Absent), "another node's");
    }

    /// A count, and the bytes the instance says it keeps.
    struct Kept(u32, usize);

    impl SlotBytes for Kept {
        fn bytes(&self) -> usize {
            self.1
        }
    }

    fn stamp(frame: i64) -> SlotStamp {
        SlotStamp { frame, clip_time_secs: frame as f64 / 24.0, fps: 24.0, width: 4, height: 2, source: (klipp_core::MediaId::nil(), 0.0), enabled: true }
    }

    /// Two clips, two nodes of one plug-in, and two renders each have
    /// their own instance; a reset drops only its clip's.
    #[test]
    fn instances_are_isolated_by_clip_node_and_render() {
        let (c1, c2, n1, n2) = (ClipId::new(), ClipId::new(), node(), node());
        let mut viewer = PluginInstances::default();
        let bump = |o: &mut PluginInstances, c, n| {
            let s = o.slot(c, n, stamp(0), || Kept(0, 0));
            s.instance.0 += 1;
            s.instance.0
        };
        bump(&mut viewer, c1, n1);
        assert_eq!(bump(&mut viewer, c1, n1), 2, "kept across frames");
        assert_eq!(bump(&mut viewer, c2, n1), 1, "another clip");
        assert_eq!(bump(&mut viewer, c1, n2), 1, "another node");
        // Two renders at once, each owning its instances.
        let export = std::thread::spawn(move || {
            let mut export = PluginInstances::default();
            export.slot(c1, n1, stamp(0), || Kept(100, 0)).instance.0 += 1;
            export.slot(c1, n1, stamp(0), || Kept(100, 0)).instance.0
        });
        assert_eq!(bump(&mut viewer, c1, n1), 3);
        assert_eq!(export.join().unwrap(), 101);
        assert_eq!(viewer.slot(c1, n1, stamp(0), || Kept(0, 0)).instance.0, 3, "the other render's didn't touch it");
        viewer.reset(c1);
        assert_eq!(viewer.len(), 1, "only the other clip's is kept");
        assert_eq!(viewer.slot(c1, n1, stamp(0), || Kept(0, 0)).instance.0, 0);
    }

    /// A slot gives back its last call's stamp, for the plug-in to decide a
    /// reset, and keeps this call's.
    #[test]
    fn a_slot_hands_back_its_last_stamp() {
        let (c, n) = (ClipId::new(), node());
        let mut o = PluginInstances::default();
        assert_eq!(o.slot(c, n, stamp(10), || Kept(0, 0)).last, None);
        assert_eq!(o.slot(c, n, stamp(11), || Kept(0, 0)).last, Some(stamp(10)));
        let resized = SlotStamp { width: 8, ..stamp(12) };
        assert_eq!(o.slot(c, n, resized, || Kept(0, 0)).last, Some(stamp(11)));
        assert_eq!(o.slot(c, n, stamp(13), || Kept(0, 0)).last, Some(resized));
    }

    /// Up to four frames forward is play and keeps a slot; further, or a
    /// step back, is a seek away and drops it.
    #[test]
    fn a_slot_is_dropped_when_the_render_moves_away_from_it() {
        let c = ClipId::new();
        let (playing, ahead, behind) = (node(), node(), node());
        let mut o = PluginInstances::default();
        o.slot(c, playing, stamp(100), || Kept(0, 0));
        o.slot(c, ahead, stamp(96), || Kept(0, 0));
        o.slot(c, behind, stamp(95), || Kept(0, 0));
        o.end_frame(100);
        assert_eq!(o.len(), 2, "the slot five frames back went");
        o.slot(c, playing, stamp(101), || Kept(0, 0));
        o.end_frame(101);
        assert!(o.slot(c, playing, stamp(102), || Kept(0, 0)).last.is_some(), "playing on keeps it");
        o.end_frame(102);
        assert_eq!(o.len(), 1, "the one not rendered since frame 96 went");
        o.end_frame(50);
        assert!(o.is_empty(), "a seek back drops it");
    }

    /// A paused repaint ends the same frame again, however often: the slot
    /// stays.
    #[test]
    fn a_paused_repaint_keeps_the_slot() {
        let (c, n) = (ClipId::new(), node());
        let mut o = PluginInstances::default();
        o.slot(c, n, stamp(40), || Kept(0, 0));
        for _ in 0..1000 {
            o.end_frame(40);
        }
        assert_eq!(o.slot(c, n, stamp(40), || Kept(0, 0)).last, Some(stamp(40)));
    }

    /// Past the state cap, the least recently rendered go first; past the
    /// slot cap, likewise.
    #[test]
    fn the_least_recently_rendered_go_past_the_caps() {
        let c = ClipId::new();
        let (old, new) = (node(), node());
        let mut o = PluginInstances::default();
        o.slot(c, old, stamp(0), || Kept(0, MAX_BYTES / 2 + 1));
        o.slot(c, new, stamp(0), || Kept(0, MAX_BYTES / 2 + 1));
        o.end_frame(0);
        assert_eq!(o.len(), 1);
        assert!(o.slot(c, new, stamp(1), || Kept(0, 0)).last.is_some(), "the newer kept");
        let mut o = PluginInstances::default();
        let first = node();
        o.slot(c, first, stamp(0), || Kept(0, 0));
        for _ in 1..=MAX_SLOTS {
            o.slot(c, node(), stamp(0), || Kept(0, 0));
        }
        assert_eq!(o.len(), MAX_SLOTS);
        assert!(o.slot(c, first, stamp(0), || Kept(0, 0)).last.is_none(), "the least recently used went");
    }
}
