//! Every enabled Opacity of a clip multiplies its opacity (#1386), a
//! clip's and an adjustment layer's by one rule: Opacities at 0.5 and 0.8
//! draw as one at 0.4, a disabled one counts for nothing, on the CPU and
//! on the GPU stack, and the two routes agree.

use std::sync::Arc;
use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime};
use klipp_effects::BuiltinEffect as E;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 32;
const H: u32 = 18;

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

/// A flat light grey source.
struct Grey;

impl FrameSource for Grey {
    fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let pixels = [0.6f32, 0.6, 0.6, 1.0].repeat((w * h) as usize);
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(pixels)), width: w, height: h, pts: None })
    }
}

fn opacities(of: &[(f32, bool)]) -> (Vec<E>, Vec<bool>) {
    (of.iter().map(|(v, _)| E::Opacity { value: *v }).collect(), of.iter().map(|(_, on)| *on).collect())
}

fn sequence(tracks: Vec<Track>) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = tracks;
    seq
}

/// The grey clip with `of` Opacities.
fn a_clip(of: &[(f32, bool)]) -> Sequence {
    let mut c = Clip::media(MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    let (fx, on) = opacities(of);
    c.set_effect_list(fx, on);
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![c];
    sequence(vec![track])
}

/// The grey clip under an adjustment layer that inverts, with `of`
/// Opacities.
fn an_adjustment(of: &[(f32, bool)]) -> Sequence {
    let mut seq = a_clip(&[]);
    let mut a = Clip { kind: ClipKind::AdjustmentLayer, ..Clip::gap(tc(0.0), tc(2.0)) };
    let (mut fx, mut on) = opacities(of);
    fx.insert(0, E::Invert { mix: 1.0 });
    on.insert(0, true);
    a.set_effect_list(fx, on);
    let mut adj = Track::new("Adj", TrackKind::Video);
    adj.clips = vec![a];
    seq.tracks.insert(0, adj);
    seq
}

fn draw(seq: &Sequence, gpu: bool) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.render_inspected(seq, tc(0.5), W, H, &mut Grey, RenderOptions::default(), Inspect::default()).0
}

fn apart(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// `two` draws as `one` on both routes, the routes agree, and it differs
/// from `unweighted`: the opacities did something.
fn draws_as(two: &Sequence, one: &Sequence, unweighted: &Sequence, what: &str) {
    let level = 1.0 / 255.0;
    for gpu in [false, true] {
        let (a, b) = (draw(two, gpu), draw(one, gpu));
        assert_eq!(a.len(), b.len());
        assert!(apart(&a, &b) <= level, "{what}, gpu {gpu}: {} apart", apart(&a, &b));
        assert!(apart(&a, &draw(unweighted, gpu)) > 10.0 * level, "{what}, gpu {gpu}: the opacities changed nothing");
    }
    assert!(apart(&draw(two, false), &draw(two, true)) <= level, "{what}: the CPU and the GPU stack disagree");
}

#[test]
fn two_opacities_draw_as_their_product_on_both_routes() {
    draws_as(&a_clip(&[(0.5, true), (0.8, true)]), &a_clip(&[(0.4, true)]), &a_clip(&[(1.0, true)]), "a clip at 0.5 × 0.8");
    draws_as(&a_clip(&[(0.5, false), (0.8, true)]), &a_clip(&[(0.8, true)]), &a_clip(&[(1.0, true)]), "a clip with a disabled 0.5");
}

#[test]
fn an_adjustment_layer_weighs_by_the_same_rule() {
    let full = an_adjustment(&[(1.0, true)]);
    draws_as(&an_adjustment(&[(0.5, true), (0.8, true)]), &an_adjustment(&[(0.4, true)]), &full, "an adjustment at 0.5 × 0.8");
    draws_as(&an_adjustment(&[(0.5, false), (0.8, true)]), &an_adjustment(&[(0.8, true)]), &full, "an adjustment with a disabled 0.5");
}
