//! A Speed Ramp retimes its clip once. Its retiming is the clip's speed
//! curve, which the effect's add brings with it; the effect itself draws
//! nothing and is left out of the picture's effects. So the source is asked
//! for the frame the speed curve maps the timeline time to, not that frame
//! retimed a second time by the effect.

use std::sync::Arc;
use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime};
use klipp_timeline::{Clip, Sequence, Track, TrackKind};
use klipp_timeline::test_clip::TestClip;

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
    c = TestClip::of(c).effect(ramp).build();
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

/// #2479: a clip set to blend or follow its in-between frames does so at
/// full speed too, where a file of a lower frame rate than the timeline's
/// is conformed; not sped up, reversed, or with nothing set.
#[test]
fn a_blending_clip_conforms_at_full_speed() {
    use klipp_compose::frame_blends;
    let blend = |speed: f64| {
        let c = Clip { speed, ..Clip::media(MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0)) };
        TestClip::of(c).effect(klipp_effects::BuiltinEffect::SpeedRamp { speed: 1.0, blend_mode: 2 }).build()
    };
    assert!(frame_blends(&blend(1.0), tc(1.0), Some(0.8 * FPS), FPS), "at full speed from a lower frame rate");
    assert!(frame_blends(&blend(0.5), tc(1.0), None, FPS), "slowed");
    assert!(!frame_blends(&blend(2.0), tc(1.0), Some(0.8 * FPS), FPS), "sped up");
    let plain = Clip::media(MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0));
    assert!(!frame_blends(&plain, tc(1.0), Some(0.8 * FPS), FPS), "nothing set");
}

/// A blending clip whose every frame falls on one of its file's mixes
/// nothing, and stays on the GPU stack: at full speed from a file of the
/// timeline's rate, or slowed by half from one of twice it. Since #2479 a
/// blending clip at full speed was declined by the stack for nothing.
#[test]
fn a_blending_clip_on_its_files_frames_does_not_blend() {
    use klipp_compose::frame_blends;
    let blend = |speed: f64| {
        let c = Clip { speed, ..Clip::media(MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0)) };
        TestClip::of(c).effect(klipp_effects::BuiltinEffect::SpeedRamp { speed: 1.0, blend_mode: 1 }).build()
    };
    assert!(!frame_blends(&blend(1.0), tc(1.0), Some(FPS), FPS), "full speed, the timeline's rate");
    assert!(!frame_blends(&blend(1.0), tc(1.0), None, FPS), "full speed, the file's rate not known");
    assert!(!frame_blends(&blend(0.5), tc(1.0), Some(2.0 * FPS), FPS), "slowed by half from twice the rate");
    assert!(frame_blends(&blend(0.5), tc(1.0), Some(FPS), FPS), "slowed by half from the timeline's rate");
}
