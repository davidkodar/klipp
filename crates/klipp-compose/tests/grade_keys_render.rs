//! A grade keyed over a shot (#891) renders the value its keys give at
//! each frame: a Corrector's exposure keyed from 0 to 2 stops renders, at
//! two times, what the same Corrector with that exposure set renders, and
//! the two times differ. An eased key renders its curve as the curve
//! editor draws it. Every render runs twice, CPU paths only and the GPU
//! where available (the viewer's and the export's renderer), and both
//! must agree.

use std::collections::HashMap;
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ClipId, RationalTime, Timecode};
use klipp_nodes::{NodeGraph, NodeKind};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

const W: u32 = 32;
const H: u32 = 18;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

/// Every media clip a flat grey.
#[derive(Default)]
struct Grey(HashMap<ClipId, f32>);

impl FrameSource for Grey {
    fn frame(&mut self, clip: &Clip, _source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let v = *self.0.get(&clip.id)?;
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new([v, v, v, 1.0].repeat((width * height) as usize))), width, height, pts: None })
    }
}

/// A clip whose grade is a Corrector, as the Colour page makes one, with
/// its exposure `edit`ed (a value, keys or both).
fn graded(edit: impl FnOnce(&mut klipp_nodes::Node)) -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(4.0)) };
    let mut g = NodeGraph::default();
    let id = g.ensure_corrector();
    edit(g.nodes.iter_mut().find(|n| n.id == id && n.kind == NodeKind::Corrector).expect("the Corrector"));
    c.set_grade(g);
    c
}

/// The centre pixel of `clip` alone at `t`, CPU paths only and GPU.
fn render(clip: &Clip, t: f64) -> [[f32; 3]; 2] {
    let mut seq = Sequence::new_default();
    let mut track = Track::new("V1", TrackKind::Video);
    track.clips = vec![clip.clone()];
    seq.tracks = vec![track];
    let mut src = Grey::default();
    src.0.insert(clip.id, 0.18);
    [false, true].map(|gpu| {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(gpu);
        let img = r.render(&seq, tc(t), W, H, &mut src, RenderOptions::default());
        let i = (((H / 2) * W + W / 2) * 4) as usize;
        [img[i], img[i + 1], img[i + 2]]
    })
}

fn close(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
    (0..3).all(|c| (a[c] - b[c]).abs() <= tol)
}

/// The exposure's keys: 0 stops at 0 s, 2 at 2 s.
fn ramp() -> Vec<klipp_core::Keyframe> {
    let mut keys = Vec::new();
    klipp_core::keyframe::upsert_keyframe(&mut keys, 0.0, 0.0);
    klipp_core::keyframe::upsert_keyframe(&mut keys, 2.0, 2.0);
    keys
}

/// `keys` on the exposure: at each time, the picture of the exposure set
/// to what the keys give there, and the two times' pictures apart.
fn renders_as_keyed(keys: Vec<klipp_core::Keyframe>, what: &str) {
    let keyed = graded(|n| n.keyframes.track_mut("exposure").keyframes = keys.clone());
    let mut seen = Vec::new();
    for t in [0.5, 1.5] {
        let value = klipp_core::keyframe::evaluate(&keys, t, 0.0);
        let set = graded(|n| n.params.exposure = Some(value));
        let [cpu, gpu] = render(&keyed, t);
        let [want_cpu, want_gpu] = render(&set, t);
        assert!(close(cpu, want_cpu, 1e-4), "{what} at {t} s, CPU: keyed {cpu:?}, exposure {value} set {want_cpu:?}");
        assert!(close(gpu, want_gpu, 1e-3), "{what} at {t} s, GPU: keyed {gpu:?}, exposure {value} set {want_gpu:?}");
        assert!(close(cpu, gpu, 2e-3), "{what} at {t} s: CPU {cpu:?} and GPU {gpu:?} disagree");
        seen.push(cpu);
    }
    assert!(seen[1][1] > seen[0][1] + 0.05, "{what}: the keys didn't move the picture: {seen:?}");
}

#[test]
fn a_keyed_exposure_renders_its_value_at_each_frame() {
    renders_as_keyed(ramp(), "linear keys");
}

/// An eased key (slow into the 2-stop key) renders its curve: at each
/// time, the value the one evaluator gives, which the curve editor draws.
#[test]
fn an_eased_exposure_key_renders_its_curve() {
    let mut keys = ramp();
    klipp_core::keyframe::ease_key(&mut keys, 1, klipp_core::KeyEase::In);
    renders_as_keyed(keys, "eased keys");
}
