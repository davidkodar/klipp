//! OFX plug-ins as nodes (M4 step 3): the host as a node runs it, behind
//! [`OfxNodeHost`] and [`OfxInstance`], and [`run_ofx`], one node's run.
//!
//! A node's instance is its own, kept in the render owner's
//! [`crate::frame_context::PluginInstances`] under its clip and node. The
//! frames it reads at other times it names first (its own
//! `GetFramesNeeded`), and they're read from the clip's source through the
//! frame context, held to the plug-in's budget, and handed in: a time not
//! handed in has no image. The plug-in takes 8-bit sRGB in the authored
//! primaries (BT.709): the picture leaves the working space for it and
//! comes back. A plug-in that's missing, can't run as a filter, or fails
//! is a [`Refusal`]; the node then passes its input on.

use std::collections::HashMap;

use klipp_core::{OfxValue, WorkingColorspace};

use crate::frame_context::{EncodedFrame, FrameContext, Neighbour, SlotBytes, SlotStamp};
use crate::graph::NodeId;

/// Why a plug-in didn't run.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    /// No plug-in by that name, or no host to run it.
    Missing,
    /// It can't run here, and why (a context it hasn't, a depth).
    Unsupported(String),
    /// Its render failed, with the status it gave.
    Failed(i32),
}

/// What a plug-in declares.
#[derive(Debug, Clone, PartialEq)]
pub struct OfxDescription {
    /// It offers the Filter context.
    pub filter: bool,
    /// The frames at other times it may ask for one frame (0: none).
    pub budget: usize,
    /// It keeps history between frames: never cached.
    pub sequential_render: bool,
    /// Its render thread safety is "unsafe".
    pub unsafe_threads: bool,
}

/// The OFX host as a node runs it.
pub trait OfxNodeHost: Send {
    fn describe(&self, plugin: &str) -> Result<OfxDescription, Refusal>;
    /// A new instance of `plugin` in the Filter context, at the clip's
    /// `fps` (its frame rate, OFX's time unit), owned by its slot;
    /// dropping it destroys it.
    fn instance(&self, plugin: &str, fps: f64) -> Result<Box<dyn OfxInstance>, Refusal>;
}

/// One plug-in instance, held in a `PluginInstances` slot.
pub trait OfxInstance: Send {
    /// Its `GetFramesNeeded` for the frame at `time`: the other times it
    /// reads. OFX time is in frames, the clip's: frame 0 its first.
    fn frames_needed(&mut self, params: &HashMap<String, OfxValue>, time: f64) -> Vec<f64>;
    /// The frame at `time` (the clip's frames) from `rgba8` (sRGB RGBA8,
    /// `width × height`). `frames` are the other times it asked for, as
    /// the source gave them; a time not there has no image.
    #[allow(clippy::too_many_arguments)]
    fn render(&mut self, params: &HashMap<String, OfxValue>, rgba8: &[u8], width: u32, height: u32, time: f64, render_scale: f64, frames: &[(f64, EncodedFrame)]) -> Result<Vec<u8>, Refusal>;
    /// The state it keeps, for the owner's cap.
    fn bytes(&self) -> usize;
}

/// An OFX node's plug-in and its values.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OfxNodeParams {
    pub plugin: String,
    pub values: HashMap<String, OfxValue>,
    /// The entry asks for the Filter context, the only one a clip's node
    /// runs a plug-in in: a transition or a generator applied to a clip
    /// is refused, never run as a filter.
    #[serde(default = "filter_context")]
    pub filter: bool,
}

fn filter_context() -> bool {
    true
}

/// A node's slot: the plug-in it was made for, and its instance or why it
/// couldn't be made (kept, so a refusal isn't asked again every frame).
struct OfxSlot {
    plugin: String,
    made: Result<Box<dyn OfxInstance>, Refusal>,
}

impl SlotBytes for OfxSlot {
    fn bytes(&self) -> usize {
        self.made.as_ref().map_or(0, |i| i.bytes())
    }
}

/// `m` × each pixel's colour, alpha kept.
fn convert(linear: &mut [f32], m: Option<[[f32; 3]; 3]>) {
    let Some(m) = m else { return };
    for px in linear.chunks_exact_mut(4) {
        let c = klipp_color::primaries::matrix3_mul_vec(m, [px[0], px[1], px[2]]);
        px[..3].copy_from_slice(&c);
    }
}

/// Node `node`'s plug-in `entry` on `input` (linear RGBA in `working`,
/// `width × height`), in its frame context: the plug-in's picture, back in
/// the working space, or why it didn't run.
pub fn run_ofx(node: NodeId, entry: &OfxNodeParams, input: &[f32], width: u32, height: u32, working: WorkingColorspace, render_scale: f64, ctx: Option<&mut FrameContext<'_>>) -> Result<Vec<f32>, Refusal> {
    run_ofx_noting(node, entry, input, width, height, working, render_scale, ctx, &mut Vec::new())
}

/// [`run_ofx`], and each frame at another time it asked for that the
/// source couldn't give, in `missed` (step 10): the plug-in ran without
/// them.
#[allow(clippy::too_many_arguments)]
pub fn run_ofx_noting(node: NodeId, entry: &OfxNodeParams, input: &[f32], width: u32, height: u32, working: WorkingColorspace, render_scale: f64, ctx: Option<&mut FrameContext<'_>>, missed: &mut Vec<crate::frame_context::Missing>) -> Result<Vec<f32>, Refusal> {
    let ctx = ctx.ok_or(Refusal::Missing)?;
    let host = ctx.ofx.ok_or(Refusal::Missing)?;
    if !entry.filter {
        return Err(Refusal::Unsupported("context: the clip's entry isn't a Filter effect".into()));
    }
    let description = host.describe(&entry.plugin)?;
    if !description.filter {
        return Err(Refusal::Unsupported("context: it offers no Filter context".into()));
    }
    let stamp = SlotStamp { frame: ctx.frame, clip_time_secs: ctx.clip_time_secs, fps: ctx.fps, width, height, source: ctx.source, enabled: true };
    // OFX time is in the clip's frames at the sequence's rate, the rate the
    // instance is made at: the clip's frame, and the frames it names read
    // from the source the clip shows at those times (its speed, ramp and
    // direction).
    let fps = ctx.fps.max(1.0);
    let (clip, time, source_at) = (ctx.clip, ctx.clip_time_secs * fps, ctx.source_at);
    let FrameContext { instances, sources, .. } = ctx;
    // A sequential-render plug-in's history is its instance's: after a seek
    // or a step back, `end_frame` has dropped the slot (it keeps only an
    // instance used within the last few frames), so the plug-in starts
    // fresh rather than carrying frames from elsewhere.
    let make = || OfxSlot { plugin: entry.plugin.clone(), made: host.instance(&entry.plugin, fps) };
    let slot = instances.slot(clip, node, stamp, make);
    // An instance is made at its clip's rate and for its plug-in: another
    // rate, or a node now running another plug-in, is another instance,
    // never one plug-in's state handed to another.
    if slot.last.is_some_and(|last| last.fps != stamp.fps) || slot.instance.plugin != entry.plugin {
        *slot.instance = make();
    }
    let instance = slot.instance.made.as_mut().map_err(|r| r.clone())?;
    // The other times it reads, from the source, held to its budget.
    sources.for_node(node, description.budget);
    let frames: Vec<(f64, EncodedFrame)> = instance
        .frames_needed(&entry.values, time)
        .into_iter()
        .filter_map(|t| match sources.encoded(source_at(t / fps)) {
            Neighbour::Frame(f) => Some((t, f)),
            Neighbour::Missing(m) => {
                missed.push(m);
                None
            }
        })
        .collect();
    let mut authored = input.to_vec();
    convert(&mut authored, klipp_color::primaries::working_space_conversion_matrix(working, WorkingColorspace::Rec709Linear));
    let bytes = klipp_effects::linear_f32_rgba_to_srgb_u8(&authored);
    let out = instance.render(&entry.values, &bytes, width, height, time, render_scale, &frames)?;
    let mut linear = klipp_effects::srgb_u8_to_linear_f32_rgba(&out);
    convert(&mut linear, klipp_color::primaries::working_space_conversion_matrix(WorkingColorspace::Rec709Linear, working));
    Ok(linear)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame_context::{BoundedSources, LinearFrame, NoAnalysis, PluginInstances, SourceProvider};
    use klipp_core::{ClipId, MediaId};
    use std::sync::{Arc, Mutex};

    /// A host of test plug-ins: `channels` (B, 255 − R, G, A); `temporal`,
    /// which asks for the frames either side and one far away and records
    /// what it was handed; `transition`, which offers no Filter context;
    /// `failing`, whose render fails. Each instance counts its renders.
    #[derive(Default)]
    struct Double {
        handed: Arc<Mutex<Vec<f64>>>,
        read: Arc<Mutex<Vec<f64>>>,
        made: Arc<Mutex<usize>>,
    }

    struct Instance {
        plugin: String,
        renders: usize,
        handed: Arc<Mutex<Vec<f64>>>,
        read: Arc<Mutex<Vec<f64>>>,
    }

    impl OfxNodeHost for Double {
        fn describe(&self, plugin: &str) -> Result<OfxDescription, Refusal> {
            let filter = plugin != "transition";
            let budget = if plugin == "temporal" { 2 } else { 0 };
            match plugin {
                "channels" | "temporal" | "transition" | "failing" => Ok(OfxDescription { filter, budget, sequential_render: false, unsafe_threads: false }),
                _ => Err(Refusal::Missing),
            }
        }
        fn instance(&self, plugin: &str, _fps: f64) -> Result<Box<dyn OfxInstance>, Refusal> {
            *self.made.lock().unwrap() += 1;
            Ok(Box::new(Instance { plugin: plugin.into(), renders: 0, handed: Arc::clone(&self.handed), read: Arc::clone(&self.read) }))
        }
    }

    impl OfxInstance for Instance {
        fn frames_needed(&mut self, _: &HashMap<String, OfxValue>, time: f64) -> Vec<f64> {
            if self.plugin == "temporal" { vec![time - 1.0, time + 1.0, time + 2500.0] } else { vec![] }
        }
        fn render(&mut self, _: &HashMap<String, OfxValue>, rgba8: &[u8], _: u32, _: u32, _: f64, _: f64, frames: &[(f64, EncodedFrame)]) -> Result<Vec<u8>, Refusal> {
            self.renders += 1;
            self.handed.lock().unwrap().extend(frames.iter().map(|(t, _)| *t));
            self.read.lock().unwrap().extend(frames.iter().map(|(_, f)| f.source_secs));
            match self.plugin.as_str() {
                "failing" => Err(Refusal::Failed(3)),
                "channels" => Ok(rgba8.chunks_exact(4).flat_map(|p| [p[2], 255 - p[0], p[1], p[3]]).collect()),
                _ => Ok(rgba8.iter().map(|_| self.renders as u8).collect()),
            }
        }
        fn bytes(&self) -> usize {
            0
        }
    }

    /// A media of 10 s, its frames 4 × 2 of a grey.
    struct Media;

    impl SourceProvider for Media {
        fn bounds(&self) -> (f64, f64) {
            (0.0, 10.0)
        }
        fn encoded(&mut self, source_secs: f64) -> Neighbour<EncodedFrame> {
            Neighbour::Frame(EncodedFrame { rgba8: vec![9; 32], width: 4, height: 2, source_secs })
        }
        fn linear(&mut self, source_secs: f64) -> Neighbour<LinearFrame> {
            Neighbour::Frame(LinearFrame { rgba: vec![0.1; 32], width: 4, height: 2, source_secs, pts: None })
        }
    }

    fn entry(plugin: &str) -> OfxNodeParams {
        OfxNodeParams { plugin: plugin.into(), values: HashMap::new(), filter: true }
    }

    fn picture() -> Vec<f32> {
        (0..8).flat_map(|i| [i as f32 / 8.0, 1.0 - i as f32 / 8.0, 0.5, 1.0]).collect()
    }

    /// `f` with a frame context of `host` for `clip`, at clip time 2 s of a
    /// clip from source 1 s at speed 1, on frame 50.
    fn in_context<R>(host: Option<&dyn OfxNodeHost>, instances: &mut PluginInstances, clip: ClipId, f: impl FnOnce(&mut FrameContext<'_>) -> R) -> R {
        retimed(host, instances, clip, &|t| 1.0 + t, f)
    }

    /// [`in_context`] for a clip whose source time at a clip time is
    /// `source_at`.
    fn retimed<R>(host: Option<&dyn OfxNodeHost>, instances: &mut PluginInstances, clip: ClipId, source_at: &dyn Fn(f64) -> f64, f: impl FnOnce(&mut FrameContext<'_>) -> R) -> R {
        let mut media = Media;
        let mut sources = BoundedSources::new(&mut media, 4, 2);
        let mut ctx = FrameContext {
            clip,
            frame: 50,
            source: (MediaId::nil(), 1.0),
            clip_time_secs: 2.0,
            source_secs: 3.0,
            source_pts: None,
            frame_number: 50,
            fps: 25.0,
            sources: &mut sources,
            analysis: &NoAnalysis,
            instances,
            ofx: host,
            source_at,
        };
        f(&mut ctx)
    }

    /// The plug-in gets the picture in 8-bit sRGB and its result comes
    /// back in the working space: a Rec. 709 working space is the authored
    /// one, so it's the sRGB round trip around the plug-in's rule.
    #[test]
    fn a_plugin_runs_on_the_picture_in_srgb8() {
        let host = Double::default();
        let mut instances = PluginInstances::default();
        let got = in_context(Some(&host), &mut instances, ClipId::new(), |ctx| run_ofx(NodeId::new_v4(), &entry("channels"), &picture(), 4, 2, WorkingColorspace::Rec709Linear, 1.0, Some(ctx))).unwrap();
        let bytes: Vec<u8> = klipp_effects::linear_f32_rgba_to_srgb_u8(&picture()).chunks_exact(4).flat_map(|p| [p[2], 255 - p[0], p[1], p[3]]).collect();
        assert_eq!(got, klipp_effects::srgb_u8_to_linear_f32_rgba(&bytes));
    }

    /// A plug-in not there, no host, no context: missing. One without the
    /// Filter context: unsupported. A failed render: failed, its status.
    #[test]
    fn a_plugin_that_cant_run_is_refused() {
        let host = Double::default();
        let run = |plugin: &str, host: Option<&dyn OfxNodeHost>| {
            let mut instances = PluginInstances::default();
            in_context(host, &mut instances, ClipId::new(), |ctx| run_ofx(NodeId::new_v4(), &entry(plugin), &picture(), 4, 2, WorkingColorspace::Rec709Linear, 1.0, Some(ctx)))
        };
        assert_eq!(run("absent", Some(&host)), Err(Refusal::Missing));
        assert_eq!(run("channels", None), Err(Refusal::Missing));
        assert_eq!(run_ofx(NodeId::new_v4(), &entry("channels"), &picture(), 4, 2, WorkingColorspace::Rec709Linear, 1.0, None), Err(Refusal::Missing));
        assert!(matches!(run("transition", Some(&host)), Err(Refusal::Unsupported(_))));
        assert_eq!(run("failing", Some(&host)), Err(Refusal::Failed(3)));
    }

    /// An entry applied to the clip in another context than Filter (a
    /// transition, a generator) is refused, never run as a filter: its
    /// plug-in offers Filter too, and would have drawn.
    #[test]
    fn an_entry_in_another_context_is_refused_not_run_as_a_filter() {
        let host = Double::default();
        let mut instances = PluginInstances::default();
        let transition = OfxNodeParams { filter: false, ..entry("channels") };
        let got = in_context(Some(&host), &mut instances, ClipId::new(), |ctx| run_ofx(NodeId::new_v4(), &transition, &picture(), 4, 2, WorkingColorspace::Rec709Linear, 1.0, Some(ctx)));
        assert!(matches!(got, Err(Refusal::Unsupported(_))), "{got:?}");
        assert_eq!(*host.made.lock().unwrap(), 0, "no instance made");
    }

    /// A node keeps its instance across frames; another node, or the same
    /// node on another clip, has its own.
    #[test]
    fn each_node_on_each_clip_has_its_own_instance() {
        let host = Double::default();
        let mut instances = PluginInstances::default();
        let (a, b, clip, other) = (NodeId::new_v4(), NodeId::new_v4(), ClipId::new(), ClipId::new());
        // The temporal test plug-in draws how many times its instance rendered.
        let run = |node, clip, instances: &mut PluginInstances| {
            in_context(Some(&host), instances, clip, |ctx| run_ofx(node, &entry("temporal"), &picture(), 4, 2, WorkingColorspace::Rec709Linear, 1.0, Some(ctx))).unwrap()[0]
        };
        let count = |linear: f32| klipp_effects::linear_f32_rgba_to_srgb_u8(&[linear, 0.0, 0.0, 1.0])[0];
        assert_eq!(count(run(a, clip, &mut instances)), 1);
        assert_eq!(count(run(a, clip, &mut instances)), 2, "kept");
        assert_eq!(count(run(b, clip, &mut instances)), 1, "another node");
        assert_eq!(count(run(a, other, &mut instances)), 1, "another clip");
        assert_eq!(*host.made.lock().unwrap(), 3);
    }

    /// The frames a plug-in names, in the clip's frames, are read from the
    /// source at the clip's in point plus their seconds, within its budget
    /// and the media: one past the media isn't handed in, and nothing
    /// stands in for it.
    #[test]
    fn the_frames_a_plugin_names_are_handed_in_within_its_budget_and_the_media() {
        let host = Double::default();
        let mut instances = PluginInstances::default();
        in_context(Some(&host), &mut instances, ClipId::new(), |ctx| run_ofx(NodeId::new_v4(), &entry("temporal"), &picture(), 4, 2, WorkingColorspace::Rec709Linear, 1.0, Some(ctx))).unwrap();
        let handed = host.handed.lock().unwrap().clone();
        // Clip time 2 s at 25 fps is frame 50: frames 49 and 51, from the
        // source at the in point plus their seconds.
        assert_eq!(handed, [49.0, 51.0], "the one 100 s on is past the media");
        let read = host.read.lock().unwrap().clone();
        assert!(read.len() == 2 && (read[0] - 2.96).abs() < 1e-9 && (read[1] - 3.04).abs() < 1e-9, "source 1 s plus clip times 1.96 s and 2.04 s: {read:?}");
    }

    /// A frame it names that the source hasn't is reported, not handed in
    /// (step 10): the one 100 s on is past the media, a fallback; the
    /// plug-in still runs on the frames there are.
    #[test]
    fn a_frame_a_plugin_names_that_the_source_hasnt_is_reported() {
        let host = Double::default();
        let mut instances = PluginInstances::default();
        let mut missed = Vec::new();
        let ran = in_context(Some(&host), &mut instances, ClipId::new(), |ctx| run_ofx_noting(NodeId::new_v4(), &entry("temporal"), &picture(), 4, 2, WorkingColorspace::Rec709Linear, 1.0, Some(ctx), &mut missed));
        assert!(ran.is_ok(), "{ran:?}");
        assert_eq!(missed, [crate::frame_context::Missing::OutOfRange]);
        assert_eq!(crate::decline::neighbour_outcome(&missed).map(|(o, _)| o), Some(crate::DeclineOutcome::Fallback));
    }

    /// The frames a plug-in names are read where the clip shows them: on a
    /// clip at twice the speed a frame either side is two source frames
    /// away, and on a reversed one the frame after is earlier in the
    /// source.
    #[test]
    fn a_plugins_other_frames_follow_the_clips_speed_and_direction() {
        for (name, source_at, want) in [
            ("twice the speed", &(|t: f64| 1.0 + 2.0 * t) as &dyn Fn(f64) -> f64, [4.92, 5.08]),
            ("reversed", &|t: f64| 8.0 - t, [6.04, 5.96]),
        ] {
            let host = Double::default();
            let mut instances = PluginInstances::default();
            retimed(Some(&host), &mut instances, ClipId::new(), source_at, |ctx| run_ofx(NodeId::new_v4(), &entry("temporal"), &picture(), 4, 2, WorkingColorspace::Rec709Linear, 1.0, Some(ctx))).unwrap();
            let read = host.read.lock().unwrap().clone();
            assert_eq!(read.len(), 2, "{name}: {read:?}");
            for (got, want) in read.iter().zip(want) {
                assert!((got - want).abs() < 1e-9, "{name}: read {read:?}, not {want:?}");
            }
        }
    }
}
