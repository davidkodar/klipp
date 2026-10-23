//! The picture a project draws is the same after it is saved and opened
//! again (#2461): a multicam group cutting between its angles, a luma
//! matte, a stencil, a title that follows a clip's transform, keyed
//! opacity, and a picture-truth clip whose blur is masked and mixed, each
//! drawn on the CPU renderer before the save and after the open, the same
//! to the byte.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{GroupId, MediaId, RationalTime};
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_media::MediaPool;
use klipp_project::{load_project_strict, save_project};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};
use serde_json::json;

const W: u32 = 64;
const H: u32 = 36;

fn t(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

/// Each file a picture of its own: a gradient tinted by its media id, that
/// moves with the source time, so a lost cut, range or angle shows.
struct Painted;

impl FrameSource for Painted {
    fn frame(&mut self, clip: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let tint = clip.media_id().map(|m| m.0.as_bytes()[0] as f32 / 255.0).unwrap_or(0.5);
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let u = x as f32 / w as f32;
                let v = y as f32 / h as f32;
                px.extend_from_slice(&[(u + secs as f32 * 0.05).fract(), v, tint, 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(px)), width: w, height: h, pts: None })
    }
}

fn title(text: &str, from: f64, to: f64) -> Clip {
    let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: text.into(), font_size: 60.0, ..Default::default() });
    Clip { kind: ClipKind::Title(td), source_in: t(0.0), source_out: t(to - from), ..Clip::gap(t(from), t(to)) }
}

fn sequence() -> Sequence {
    let (cam_a, cam_b) = (MediaId::new(), MediaId::new());
    let group = GroupId::new();
    let mut primary = Clip::media(cam_a, t(0.0), t(6.0), t(2.0), t(8.0));
    (primary.multicam_group, primary.multicam_angle, primary.multicam_cuts) = (Some(group), 0, vec![(2.0, 1), (4.0, 0)]);
    let mut angle = Clip::media(cam_b, t(0.0), t(6.0), t(1.0), t(7.0));
    (angle.multicam_group, angle.multicam_angle) = (Some(group), 1);
    primary.edit_opacity_keyframes(|k| *k = vec![serde_json::from_value(json!({ "time_secs": 0.0, "value": 1.0, "interpolation": "Linear" })).unwrap(), serde_json::from_value(json!({ "time_secs": 6.0, "value": 0.4, "interpolation": "Linear" })).unwrap()]);

    // A picture-truth clip: its blur mixed at a half, which only its
    // picture can hold.
    let mut truth = Clip::media(cam_b, t(6.0), t(10.0), t(0.0), t(4.0));
    truth.push_effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 6.0 });
    let blur = truth.picture_v2().unwrap().picture.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::types::LEGACY_EFFECT).unwrap().id;
    truth.apply_picture_ops(&[GraphOp::SetMix { path: NodePath::new(Root::Clip { clip: truth.id.0 }, [blur]), mix: Some(0.5) }]).unwrap();
    assert!(truth.is_picture_truth());

    let mut matte = Clip::media(cam_a, t(6.0), t(10.0), t(3.0), t(7.0));
    matte.consumed_as_matte = true;
    let mut keyed = Clip::media(cam_a, t(6.0), t(10.0), t(0.0), t(4.0));
    keyed.matte_source = Some(serde_json::from_value(json!({ "source_clip_id": matte.id, "channel": "Luminance", "invert": false, "feather_px": 1.0, "choke_px": 0.0 })).unwrap());

    let mut over = title("Over", 0.0, 6.0);
    over.transform_link = Some(serde_json::from_value(json!({ "target": primary.id, "mode": "Additive" })).unwrap());
    let mut stencil = title("STENCIL", 6.0, 10.0);
    stencil.matte_stencil = Some(serde_json::from_value(json!({ "channel": "Alpha", "invert": false, "feather_px": 0.0, "choke_px": 0.0 })).unwrap());

    let track = |name: &str, kind, clips: Vec<Clip>| {
        let mut tr = Track::new(name, kind);
        tr.clips = clips;
        tr
    };
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = vec![
        track("T1", TrackKind::Text, vec![over, stencil]),
        track("V3", TrackKind::Video, vec![angle, keyed]),
        track("V2", TrackKind::Video, vec![truth]),
        track("V1", TrackKind::Video, vec![primary, matte]),
    ];
    seq
}

fn frames(seq: &Sequence) -> Vec<Vec<f32>> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    [0.5, 2.5, 4.5, 6.5, 8.75].iter().map(|s| r.render(seq, t(*s), W, H, &mut Painted, RenderOptions::default())).collect()
}

#[test]
fn a_project_draws_the_same_after_a_save_and_an_open() {
    let made = sequence();
    let before = frames(&made);
    assert!(before.windows(2).all(|w| w[0] != w[1]), "each moment its own picture");
    let dir = std::env::temp_dir().join(format!("klipp_frames_survive_save_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    save_project(&dir, std::slice::from_ref(&made), &MediaPool::new()).unwrap();
    let (back, ..) = load_project_strict(&dir).unwrap();
    assert!(back[0].tracks[2].clips[0].is_picture_truth(), "the truth clip is still one");
    let after = frames(&back[0]);
    for (k, (b, a)) in before.iter().zip(&after).enumerate() {
        assert!(b == a, "frame {k}: the picture changed through the save");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
