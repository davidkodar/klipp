//! One failed GPU frame costs that frame only (#3406): a readback that
//! never finishes, as a GPU job past its time limit leaves one, falls
//! that frame back, and the next frame draws on the GPU again.
//!
//! Its own binary: it fails a readback on the process's shared device.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Resolution};
use klipp_effects::BuiltinEffect;
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const SIZE: (u32, u32) = (160, 90);

struct Ramp;

impl FrameSource for Ramp {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let v = (0..w * h).flat_map(|i| {
            let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            [x, y, 1.0 - x, 1.0]
        });
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v.collect())), width: w, height: h, pts: None })
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn sequence() -> Sequence {
    let clip = TestClip::of(Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) })
        .effect(BuiltinEffect::Gamma { gamma: 1.4 })
        .build();
    let mut s = Sequence::new_default();
    s.resolution = Resolution { width: SIZE.0, height: SIZE.1 };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    s.tracks = vec![track];
    s
}

#[test]
fn a_frame_after_a_failed_one_draws_on_the_gpu() {
    let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let seq = sequence();
    // Each frame at a time of its own, so none is a repeat the renderer
    // could keep; the ramp is the same picture at every time.
    let draw = |r: &mut Renderer, secs: f64| r.render_display_u8_on_gpu(&seq, tc(secs), SIZE.0, SIZE.1, &mut Ramp, RenderOptions::default());
    let Some(before) = draw(&mut r, 0.5) else {
        assert!(!required, "GPU required, but the stack declined a plain frame: {:?}", r.last_decline());
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };

    r.executor().gpu_pipeline_mut().expect("the stack's GPU pipeline").fail_next_readback();
    assert!(draw(&mut r, 1.0).is_none(), "the failed frame falls back");
    assert_eq!(r.last_decline(), Some("the display encode failed"));

    let after = draw(&mut r, 1.5);
    assert!(after.is_some(), "the next frame declined: {:?}", r.last_decline());
    assert!(after.is_some_and(|(bytes, _)| bytes == before.0), "the next frame is the same picture");
    assert!(!r.executor().gpu_pipeline_mut().expect("the stack's GPU pipeline").is_device_lost(), "the GPU is still on");
}
