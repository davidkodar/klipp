//! Every built-in effect the Effects browser lists does what its name
//! says as it is added (#3243): its starting values change the picture.
//! The Gaussian Blur once started at radius 0 and changed nothing. The
//! entries whose start is neutral on purpose, a place to set from, are
//! listed with why.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Resolution};
use klipp_effects::registry::{effect_types, Browser};
use klipp_effects::BuiltinEffect;
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 640;
const H: u32 = 360;

/// Started neutral on purpose, with why.
const NEUTRAL: &[(&str, &str)] = &[
    ("opacity", "starts fully opaque, to be lowered"),
    ("gamma", "starts at 1, to be moved either way"),
    ("levels", "starts with the picture's own black, white and middle, to be set"),
    ("channel_mixer", "starts with each channel from itself, to be mixed"),
    ("h_s_v_adjust", "starts at the picture's own hue, saturation and brightness, to be moved either way"),
    ("lut_3d", "has no LUT until a file is chosen"),
    ("colour_space_transform", "starts from the sequence's space to itself, to be pointed at the clip's"),
    ("crop", "starts uncropped, to be pulled in"),
    ("transform", "starts at the picture's own place and size"),
    ("picture_transform", "starts at the picture's own place and size"),
    ("rounded_corners", "starts square, to be rounded"),
    ("corner_pin", "starts with the corners on the picture's corners, to be dragged"),
    ("speed_ramp", "starts at the clip's own speed: a speed set on adding would retime the clip unasked"),
    // These correct what changes from frame to frame, measured from the
    // footage; a still chart has nothing to correct.
    ("rolling_shutter_correction", "corrects motion, which a still has none of"),
    ("video_stabilization", "corrects shake, which a still has none of"),
    ("temporal_denoise", "averages with the frames either side, which on a still are the same"),
    ("deflicker", "corrects brightness pumping between frames, which a still has none of"),
];

/// Hue across, brightness down to highlights at the top, hard-edged
/// blocks and a little fixed noise: edges for a blur or a sharpen,
/// highlights for a glow, noise for a denoiser, colour for a grade.
struct Chart;

impl FrameSource for Chart {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (u, t) = (x as f32 / w as f32, y as f32 / h as f32);
                let block = if ((u * 8.0) as u32 + (t * 4.0) as u32).is_multiple_of(2) { 1.0 } else { 0.55 };
                let hue = u * 6.0;
                let f = |n: f32| {
                    let k = (n + hue) % 6.0;
                    1.0 - (k.min(4.0 - k).clamp(0.0, 1.0)) * 0.7
                };
                let level = (0.05 + 1.0 * (1.0 - t)) * block;
                let noise = ((x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663)) % 1000) as f32 / 1000.0 - 0.5;
                let px = |c: f32| (c * level + noise * 0.06).max(0.0);
                v.extend_from_slice(&[px(f(5.0)), px(f(3.0)), px(f(1.0)), 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

/// `effect` on the chart, rendered on the CPU, the reference path, half a
/// second into the clip. Two ways: the chart filling the frame, for an
/// effect on the picture (a vignette, bars); and the chart at 60% over a
/// grey track, for one cast round the layer or seen through it (a shadow,
/// a key).
fn render(effect: Option<&BuiltinEffect>, as_layer: bool) -> Vec<f32> {
    let shrink = BuiltinEffect::Transform {
        position_x: 0.0,
        position_y: 0.0,
        scale_x: 0.6,
        scale_y: 0.6,
        rotation: 0.0,
        anchor_x: 0.5,
        anchor_y: 0.5,
        rotation_x: 0.0,
        rotation_y: 0.0,
        perspective: 0.0,
        focal_ref: None,
    };
    let clip = TestClip::of(Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) })
        .effects(as_layer.then_some(shrink).into_iter().chain(effect.cloned()))
        .build();
    let mut s = Sequence::new_default();
    s.resolution = Resolution { width: 1920, height: 1080 };
    let mut track = Track::new("V2", TrackKind::Video);
    track.clips = vec![clip];
    s.tracks = vec![track];
    if as_layer {
        let mut below = Track::new("V1", TrackKind::Video);
        below.clips = vec![Clip { kind: ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color: [128, 128, 128, 255] }), source_in: tc(0.0), source_out: tc(2.0), ..Clip::gap(tc(0.0), tc(2.0)) }];
        s.tracks.push(below);
    }
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    r.render(&s, tc(0.5), W, H, &mut Chart, RenderOptions::default())
}

/// The share of pixels a channel of which moved by more than 1%. The bar
/// is low: it catches an entry that leaves the picture as it was, and
/// sparse dust (Particles) changes a tenth of a percent.
fn changed(a: &[f32], b: &[f32]) -> f32 {
    let n = a.chunks(4).zip(b.chunks(4)).filter(|(p, q)| (0..3).any(|c| (p[c] - q[c]).abs() > 0.01)).count();
    n as f32 / (a.len() / 4) as f32
}

#[test]
fn every_listed_effect_changes_the_picture_as_it_is_added() {
    let plain = [render(None, false), render(None, true)];
    let mut unchanged = Vec::new();
    let mut seen = Vec::new();
    for t in effect_types() {
        let Browser::Listed { start, .. } = t.browser else { continue };
        let effect = start.map_or_else(|| (t.sample)(), |s| s());
        let d = changed(&render(Some(&effect), false), &plain[0]).max(changed(&render(Some(&effect), true), &plain[1]));
        seen.push(format!("{} {d:.3}", t.tag));
        let neutral = NEUTRAL.iter().any(|(tag, _)| *tag == t.tag);
        if d < 0.0005 && !neutral {
            unchanged.push(format!("{} ({d:.4})", t.tag));
        }
        if neutral {
            assert!(start.is_none(), "{} is listed as neutral but given a start", t.tag);
        }
    }
    eprintln!("share of pixels changed: {}", seen.join(", "));
    assert!(unchanged.is_empty(), "added from the browser, these leave the picture as it was: {unchanged:?}");
    for (tag, _) in NEUTRAL {
        assert!(effect_types().any(|t| t.tag == *tag && matches!(t.browser, Browser::Listed { .. })), "{tag} is not a listed effect");
    }
}
