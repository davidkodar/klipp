//! A Speed Ramp retimes its clip once. Its retiming is the clip's speed
//! curve, which the effect's add brings with it; the effect itself draws
//! nothing and is left out of the picture's effects. So the source is asked
//! for the frame the speed curve maps the timeline time to, not that frame
//! retimed a second time by the effect.

use std::sync::Arc;
use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime};
use klipp_timeline::{Clip, Sequence, Track, TrackKind};

const W: u32 = 64;
const H: u32 = 36;
const FPS: f64 = 24.0;

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

/// A flat-free picture, recording each source second it's asked for.
struct Recording(Vec<f64>);

impl FrameSource for Recording {
    fn frame(&mut self, _: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        self.0.push(secs);
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                pixels.extend_from_slice(&[x as f32 / w as f32, y as f32 / h as f32, 0.3, 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(pixels)), width: w, height: h, pts: None })
    }
}

/// A clip at twice its speed, as a Speed Ramp's add leaves it: the clip's
/// speed set, and the effect on its list.
fn sequence() -> (Sequence, Clip) {
    let mut c = Clip::media(MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(2.0));
    c.speed = 2.0;
    // The ramp at the same twice, as its add leaves it: retimed again by
    // the effect, the source would be asked for four times the time.
    let mut ramp = (klipp_effects::registry::effect_type("speed_ramp").expect("Speed Ramp is a catalog type").sample)();
    match &mut ramp {
        klipp_effects::BuiltinEffect::SpeedRamp { speed, .. } => *speed = 2.0,
        other => panic!("the Speed Ramp sample is {other:?}"),
    }
    c.set_effect_list(vec![ramp], vec![true]);
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![c.clone()];
    seq.tracks = vec![track];
    (seq, c)
}

#[test]
fn speed_ramp_retimes_once() {
    let (seq, clip) = sequence();
    for gpu in [false, true] {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(gpu);
        for at in [0.25, 0.5, 0.75] {
            let mut source = Recording(Vec::new());
            r.render_inspected(&seq, tc(at), W, H, &mut source, RenderOptions::default(), Inspect::default());
            let want = clip.source_secs_at(at, FPS);
            assert!(!source.0.is_empty(), "gpu {gpu}, {at} s: the source was never read");
            for got in &source.0 {
                assert!((got - want).abs() < 1e-6, "gpu {gpu}, {at} s: the source was asked for {got} s, the speed curve maps to {want} s (asked: {:?})", source.0);
            }
            // At twice the speed, once: neither the timeline time nor four
            // times it.
            assert!((want - 2.0 * at).abs() < 1e-6, "{at} s: the clip's speed curve is twice the speed ({want} s)");
        }
    }
}
