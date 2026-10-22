//! A picture-truth clip's placement and opacity, set and keyed as the
//! Video tab and the viewer's gizmo set them (by path, on the clip's
//! placement node), are what the renderer evaluates. Such a clip's edits
//! land in its picture alone; its fields' lanes are a view the edit never
//! writes, so a renderer reading them would leave the clip unmoved.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime, Timecode};
use klipp_effects::BuiltinEffect;
use klipp_graph_format::op::GraphOp;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::{ParamKey, ParamValue};
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::GradeAccess;
use klipp_timeline::{Clip, Sequence, Track, TrackKind};
use serde_json::json;

const FPS: f64 = 25.0;

fn t(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 25)
}

/// A clip with a blur, made picture-truth by a canvas edit only its
/// picture keeps (a node placed on the canvas), and its placement path.
fn truth_clip() -> (Clip, NodePath) {
    truth_clip_with(&[])
}

/// [`truth_clip`], with `more` effects after its blur.
fn truth_clip_with(more: &[BuiltinEffect]) -> (Clip, NodePath) {
    let c = Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
    let mut c = TestClip::of(c).effect(BuiltinEffect::GaussianBlur { radius: 2.0 }).effects(more.iter().cloned()).build();
    let blur = c.picture_v2().unwrap().picture.graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("gaussian_blur")).unwrap().id;
    c.apply_picture_ops(&[GraphOp::SetNodeMeta { path: NodePath::new(Root::Clip { clip: c.id.0 }, [blur]), label: None, position: Some(json!({"x": 40.0, "y": 0.0})) }]).unwrap();
    assert!(c.is_picture_truth(), "a canvas edit makes the clip picture-truth");
    let at = c.placement_path().expect("a placement node");
    (c, at)
}

/// A write to the clip's fields, which lifts them again: its grade, left
/// as it was.
fn write_to_the_fields(c: &mut Clip) {
    c.edit_grade(|_| {});
}

fn key(time_secs: f64, value: f32) -> serde_json::Value {
    serde_json::to_value(klipp_core::Keyframe { time_secs, value, interpolation: klipp_core::Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() }).unwrap()
}

fn transform_at(c: &Clip, secs: f64) -> klipp_compose::eval::ResolvedTransform {
    let time = Timecode::from_secs_f64(secs, 25);
    klipp_compose::eval::evaluate_clip_own_transform(c, secs, &klipp_compose::eval::eval_context(c, time, FPS))
}

fn opacity_at(c: &Clip, secs: f64) -> f32 {
    klipp_compose::eval::evaluate_clip_opacity(c, Timecode::from_secs_f64(secs, 25), FPS)
}

/// A keyed position moves the clip, frame by frame, and a value set with
/// no key on its lane places it.
#[test]
fn a_picture_truth_clips_keyed_position_moves_it() {
    let (mut c, at) = truth_clip();
    c.apply_picture_ops(&[
        GraphOp::AddKeyAt { path: at.clone(), key: ParamKey::from("pos_x"), keyframe: key(0.0, 0.0) },
        GraphOp::AddKeyAt { path: at.clone(), key: ParamKey::from("pos_x"), keyframe: key(1.0, 100.0) },
        GraphOp::SetParam { path: at.clone(), key: ParamKey::from("pos_y"), value: ParamValue::Float(50.0) },
    ])
    .unwrap();
    assert!(c.is_picture_truth());
    let (start, half, end) = (transform_at(&c, 0.0), transform_at(&c, 0.5), transform_at(&c, 1.0));
    assert!((start.0 - 0.0).abs() < 1e-3 && (half.0 - 50.0).abs() < 1e-3 && (end.0 - 100.0).abs() < 1e-3, "the keyed x: {start:?} {half:?} {end:?}");
    assert!((start.1 - 50.0).abs() < 1e-3, "the set y: {start:?}");
    assert!(c.has_render_keyframes() && c.has_animation(), "the render gates see the keys");
}

/// The opacity set, then keyed, is what the clip draws at.
#[test]
fn a_picture_truth_clips_opacity_is_its_pictures() {
    let (mut c, at) = truth_clip();
    c.apply_picture_ops(&[GraphOp::SetParam { path: at.clone(), key: ParamKey::from("opacity"), value: ParamValue::Float(0.4) }]).unwrap();
    assert!((opacity_at(&c, 0.5) - 0.4).abs() < 1e-3, "the set opacity: {}", opacity_at(&c, 0.5));
    c.apply_picture_ops(&[
        GraphOp::AddKeyAt { path: at.clone(), key: ParamKey::from("opacity"), keyframe: key(0.0, 1.0) },
        GraphOp::AddKeyAt { path: at, key: ParamKey::from("opacity"), keyframe: key(1.0, 0.0) },
    ])
    .unwrap();
    assert!((opacity_at(&c, 0.0) - 1.0).abs() < 1e-3 && (opacity_at(&c, 0.4) - 0.6).abs() < 1e-3, "the keyed opacity: {} {}", opacity_at(&c, 0.0), opacity_at(&c, 0.4));
}

/// A keyed tilt leans the clip as the renderer draws it, and the gates
/// see it.
#[test]
fn a_picture_truth_clips_keyed_tilt_leans_it() {
    let (mut c, at) = truth_clip();
    c.apply_picture_ops(&[
        GraphOp::AddKeyAt { path: at.clone(), key: ParamKey::from("tilt_x"), keyframe: key(0.0, 0.0) },
        GraphOp::AddKeyAt { path: at, key: ParamKey::from("tilt_x"), keyframe: key(1.0, 40.0) },
    ])
    .unwrap();
    assert!(c.has_tilt_keyframes(), "the gates see the tilt");
    let [x, _] = c.tilt_lanes();
    assert!((klipp_core::keyframe::evaluate(&x, 0.5, 0.0) - 20.0).abs() < 1e-3, "the renderer's tilt lane is the picture's");
}

const W: u32 = 32;
const H: u32 = 18;

/// A flat light grey source, so wherever the clip is not, the frame is
/// black.
struct Grey;

impl FrameSource for Grey {
    fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let pixels = [0.6f32, 0.6, 0.6, 1.0].repeat((w * h) as usize);
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(pixels)), width: w, height: h, pts: None })
    }
}

fn on_a_sequence(c: Clip) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![c];
    seq.tracks = vec![track];
    seq
}

fn draw(seq: &Sequence, secs: f64, gpu: bool) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.render(seq, Timecode::from_secs_f64(secs, 25), W, H, &mut Grey, RenderOptions::default())
}

fn apart(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// The whole frame, as the viewer and export render it, on the CPU and
/// on the GPU stack: a truth clip keyed by path to move, lean and fade
/// draws as the same clip without the keys where the keys start at
/// rest, and apart from it where they have moved it; the unkeyed clip
/// stays put; and the two routes agree.
#[test]
fn a_keyed_picture_truth_clip_moves_in_the_rendered_frame() {
    let level = 1.0 / 255.0;
    let (still, at) = truth_clip();
    let lanes: [(&str, f32, f32); 3] = [("pos_x", 0.0, W as f32 / 2.0), ("tilt_x", 0.0, 50.0), ("opacity", 1.0, 0.2)];
    for (lane, from, to) in lanes {
        let mut keyed = still.clone();
        keyed.apply_picture_ops(&[
            GraphOp::AddKeyAt { path: at.clone(), key: ParamKey::from(lane), keyframe: key(0.0, from) },
            GraphOp::AddKeyAt { path: at.clone(), key: ParamKey::from(lane), keyframe: key(1.0, to) },
        ])
        .unwrap();
        let (keyed, still) = (on_a_sequence(keyed), on_a_sequence(still.clone()));
        for gpu in [false, true] {
            let (rest, moved) = (draw(&keyed, 0.0, gpu), draw(&keyed, 1.0, gpu));
            assert!(apart(&rest, &draw(&still, 0.0, gpu)) <= level, "{lane}, gpu {gpu}: at its first key the clip is where it would be unkeyed");
            assert!(apart(&moved, &draw(&still, 1.0, gpu)) > 10.0 * level, "{lane}, gpu {gpu}: the keys did not reach the frame");
            assert!(apart(&draw(&still, 0.0, gpu), &draw(&still, 1.0, gpu)) <= level, "{lane}, gpu {gpu}: the unkeyed clip stays put");
        }
        assert!(apart(&draw(&keyed, 1.0, false), &draw(&keyed, 1.0, true)) <= level, "{lane}: the CPU and the GPU stack disagree");
    }
}

/// A key set by path on a picture-truth clip outlives a later write to the
/// clip's fields, which lifts them again: the key is written through to
/// them. Undoing the write keeps it too.
#[test]
fn a_key_set_by_path_survives_a_write_to_the_fields() {
    let (mut c, at) = truth_clip();
    c.apply_picture_ops(&[
        GraphOp::AddKeyAt { path: at.clone(), key: ParamKey::from("pos_x"), keyframe: key(0.0, 0.0) },
        GraphOp::AddKeyAt { path: at, key: ParamKey::from("pos_x"), keyframe: key(1.0, 100.0) },
    ])
    .unwrap();
    let keyed = c.picture.clone();
    let keys = |c: &Clip| c.truth_lane(&klipp_core::ParamId::PositionX).map(|(k, _)| k.len());
    write_to_the_fields(&mut c);
    assert!(c.is_picture_truth(), "still picture-truth");
    assert_eq!(keys(&c), Some(2), "the keys outlive the write");
    assert!((transform_at(&c, 0.5).0 - 50.0).abs() < 1e-3, "and still move the clip");
    c.restore_picture(&keyed);
    assert_eq!(keys(&c), Some(2), "undoing the write keeps them");
}

/// A value set by path on a picture-truth clip, with no entry of its
/// fields to hold it, outlives a later write to the fields: the
/// placement's move and the clip's opacity alike.
#[test]
fn a_value_set_by_path_survives_a_write_to_the_fields() {
    let (mut c, at) = truth_clip();
    c.apply_picture_ops(&[
        GraphOp::SetParam { path: at.clone(), key: ParamKey::from("pos_y"), value: ParamValue::Float(50.0) },
        GraphOp::SetParam { path: at, key: ParamKey::from("opacity"), value: ParamValue::Float(0.4) },
    ])
    .unwrap();
    write_to_the_fields(&mut c);
    assert!(c.is_picture_truth(), "still picture-truth");
    assert!((transform_at(&c, 0.4).1 - 50.0).abs() < 1e-3, "the move outlives the write: {:?}", transform_at(&c, 0.4));
    assert!((opacity_at(&c, 0.4) - 0.4).abs() < 1e-3, "the opacity outlives the write: {}", opacity_at(&c, 0.4));
}

/// Where an entry of the fields holds the value (an enabled Opacity), the
/// value set by path is written through to it, so a later write to the
/// fields, which lifts the entry again, keeps it.
#[test]
fn a_value_an_entry_holds_is_written_through() {
    let (mut c, at) = truth_clip_with(&[BuiltinEffect::Opacity { value: 1.0 }]);
    c.apply_picture_ops(&[GraphOp::SetParam { path: at, key: ParamKey::from("opacity"), value: ParamValue::Float(0.3) }]).unwrap();
    write_to_the_fields(&mut c);
    assert!((opacity_at(&c, 0.4) - 0.3).abs() < 1e-3, "the opacity outlives the write: {}", opacity_at(&c, 0.4));
}
