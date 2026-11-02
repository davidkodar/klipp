//! A GPU failing frame after frame is turned off for the session (#3406):
//! three failed frames in a row do it, a frame counts once however many
//! of its readbacks fail, and a good frame starts the count again.
//!
//! Its own binary: the GPU stays off for the rest of the process.

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
fn three_failed_frames_in_a_row_turn_the_gpu_off_and_a_good_one_resets_the_count() {
    let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let seq = sequence();
    let mut secs = 0.0;
    // Each frame at a time of its own, so none is a repeat.
    let mut draw = |r: &mut Renderer, fail: bool| {
        if fail {
            r.executor().gpu_pipeline_mut().expect("the stack's GPU pipeline").fail_next_readback();
        }
        secs += 0.1;
        r.render_display_u8_on_gpu(&seq, tc(secs), SIZE.0, SIZE.1, &mut Ramp, RenderOptions::default()).is_some()
    };
    if !draw(&mut r, false) {
        assert!(!required, "GPU required, but the stack declined a plain frame: {:?}", r.last_decline());
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    }

    // One frame with three failed readbacks is one failed frame.
    let pixels = (SIZE.0 * SIZE.1) as usize;
    let gpu = r.executor().gpu_pipeline_mut().expect("the stack's GPU pipeline");
    assert!(gpu.stack_begin(SIZE.0, SIZE.1, 1));
    for _ in 0..3 {
        gpu.fail_next_readback();
        gpu.flush_chain_and_download_linear(true, pixels);
    }
    assert_eq!(klipp_effects::gpu_context::gpu_turned_off(), None, "one frame failed, however many readbacks");
    assert!(draw(&mut r, false), "the frame after it: {:?}", r.last_decline());

    // Two failed, one good, two failed: never three in a row.
    for fail in [true, true, false, true, true] {
        assert_eq!(draw(&mut r, fail), !fail, "failing {fail}: {:?}", r.last_decline());
    }
    assert_eq!(klipp_effects::gpu_context::gpu_turned_off(), None);
    assert!(draw(&mut r, false), "still on: {:?}", r.last_decline());

    for _ in 0..3 {
        assert!(!draw(&mut r, true));
    }
    let why = klipp_effects::gpu_context::gpu_turned_off().expect("off after three in a row");
    assert!(why.contains("3 GPU frames"), "{why}");
    assert!(!draw(&mut r, false), "the next frame stays off the GPU");
    assert_eq!(r.last_decline(), Some("the GPU stack would not start"));

    // A pipeline built later doesn't turn it back on.
    let later = klipp_effects::gpu::GpuEffectsPipeline::new().expect("a pipeline on the same device");
    assert!(later.is_device_lost(), "off for the session");
}
