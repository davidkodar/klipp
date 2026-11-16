//! A title scene of text and rectangles runs its in-place passes (each
//! object's fade and precision, its laying over the scene, the scene's
//! precision) over the part of the frame its objects paint, not the whole
//! frame. Drawn on the GPU, every built-in template, and the same titles
//! with glows, shadows and blurs reaching far past their layers, is the
//! same to the byte with the bounds as without them; and bounds a few
//! pixels too tight change the picture, so the test would see bounds that
//! miss a pixel.

#[path = "support/cpu_reference.rs"]
#[allow(dead_code)]
mod cpu_reference;
use cpu_reference::no_gpu;
use std::collections::HashMap;
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ClipId, RationalTime, Timecode};
use klipp_timeline::{title_library, Clip, ClipKind, MotionKind, Sequence, TextGlow, TitleData, TitleObjectContent, Track, TrackKind};

const W: u32 = 960;
const H: u32 = 540;
const LENGTH: f64 = 5.0;
/// Entrance, rest, exit.
const TIMES: [f64; 6] = [0.1, 0.35, 0.7, 1.2, 2.5, 4.85];

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 30)
}

fn clip(kind: ClipKind, from: f64, to: f64) -> Clip {
    Clip { kind, ..Clip::gap(tc(from), tc(to)) }
}

#[derive(Default)]
struct Flat(HashMap<ClipId, Arc<Vec<f32>>>);

impl FrameSource for Flat {
    fn frame(&mut self, clip: &Clip, _source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        self.0.get(&clip.id).map(|p| SourceFrame { pixels: SourcePixels::Linear(Arc::clone(p)), width, height, pts: None })
    }
}

/// `td` over a ramp with light and dark bands, from 0 s to `LENGTH`.
fn over_a_picture(td: TitleData) -> (Sequence, Flat) {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut text = Track::new("Text", TrackKind::Text);
    text.clips = vec![clip(ClipKind::Title(td), 0.0, LENGTH)];
    let mut base = Track::new("V1", TrackKind::Video);
    let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 10.0);
    let picture: Vec<f32> = (0..W * H).flat_map(|i| {
        let (fx, fy) = ((i % W) as f32 / W as f32, (i / W) as f32 / H as f32);
        let band = if (i / W) % 135 < 30 { 0.9 } else if (i / W) % 135 < 60 { 0.02 } else { 0.0 };
        [fx * 0.8 + band, fy * 0.6 + band, (fx + fy) * 0.35 + band, 1.0]
    }).collect();
    let mut src = Flat::default();
    src.0.insert(b.id, Arc::new(picture));
    base.clips = vec![b];
    seq.tracks = vec![text, base];
    (seq, src)
}

fn template(name: &str) -> TitleData {
    let t = title_library::TITLES.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("no template {name}"));
    title_library::for_frame(t.build(), W, H)
}

/// Every built-in template, and titles whose styles and motions reach far
/// past their layers: a wide glow and a far shadow on text and on a
/// rectangle, a blur entrance, a long rise.
fn titles() -> Vec<(String, TitleData)> {
    let mut all: Vec<(String, TitleData)> = title_library::TITLES.iter().map(|t| (t.name.clone(), title_library::for_frame(t.build(), W, H))).collect();
    let shadow = template("Thanks for Watching").objects[0].fx().shadows[0].clone();
    let glow = TextGlow { enabled: true, color: [255, 220, 120, 255], radius: 24.0, radius_keyframes: Vec::new(), intensity: 1.0, intensity_keyframes: Vec::new() };
    for (name, base) in [("Title Card", template("Title Card")), ("Statement", template("Statement"))] {
        let mut far = base.clone();
        for o in &mut far.objects {
            let mut s = shadow.clone();
            s.offset_x = 40.0;
            s.offset_y = 36.0;
            s.blur = 8.0;
            o.fx_mut().shadows = vec![s];
            o.fx_mut().glows = vec![glow.clone()];
        }
        all.push((format!("{name}, far shadow and wide glow"), far));
        let mut rising = base.clone();
        for o in &mut rising.objects {
            let text = matches!(o.content, TitleObjectContent::TextBox(_));
            {
                let m = o.fx_mut().motion.enter.get_or_insert_with(|| klipp_timeline::Motion::new(MotionKind::Rise));
                m.kind = if text { MotionKind::Blur } else { MotionKind::Rise };
                // Whole, not letter by letter: placed as one sprite.
                m.stagger = klipp_timeline::StaggerUnit::None;
                m.distance = 1.0;
                m.duration_secs = 1.5;
            }
        }
        all.push((format!("{name}, blurred and rising far"), rising));
    }
    all
}

/// Each title at each time, drawn on the GPU with the bounds as `on` and
/// `shrink` say: its display bytes, or `None` when the stack declined.
fn drawn(on: bool, shrink: u32) -> Option<Vec<(String, f64, Vec<u8>)>> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    r.set_title_bounds_for_test(on, shrink);
    let mut out = Vec::new();
    for (name, td) in titles() {
        let (seq, mut src) = over_a_picture(td);
        for at in TIMES {
            let (bytes, _) = r.render_display_u8_on_gpu(&seq, tc(at), W, H, &mut src, RenderOptions::default())?;
            out.push((name.clone(), at, bytes));
        }
    }
    Some(out)
}

#[test]
fn a_scene_over_its_bounds_is_the_scene_over_the_whole_frame() {
    let (Some(whole), Some(bounded)) = (drawn(false, 0), drawn(true, 0)) else {
        no_gpu("the GPU stack declined (no usable GPU?)");
        return;
    };
    let differ: Vec<String> = whole.iter().zip(&bounded).filter(|(a, b)| a.2 != b.2).map(|(a, _)| format!("{} at {} s", a.0, a.1)).collect();
    assert!(differ.is_empty(), "drawn over their bounds, these differ from the whole frame: {differ:?}");
}

/// Bounds a few pixels short of what the objects paint leave pixels out:
/// the test above would see bounds that miss.
#[test]
fn bounds_too_tight_change_the_picture() {
    let (Some(whole), Some(tight)) = (drawn(false, 0), drawn(true, 4)) else {
        no_gpu("the GPU stack declined (no usable GPU?)");
        return;
    };
    let differ = |name: &str| whole.iter().zip(&tight).filter(|(a, b)| a.0 == name && a.2 != b.2).count();
    for name in ["Title Card", "Title Card, far shadow and wide glow", "Statement, far shadow and wide glow", "Title Card, blurred and rising far", "Statement, blurred and rising far"] {
        assert!(differ(name) > 0, "{name}: bounds 4 pixels too tight drew the same picture");
    }
}
