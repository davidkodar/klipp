//! A clip keeps every edit of Klipp's through a composition: trimmed, at
//! another speed, with effects, with its opacity, scale and position keyed,
//! a clip shown through a composition that only reads its footage renders
//! exactly as it does without one, on the GPU and on the CPU. The
//! composition's time is the media's, so a trim or a speed reads the frame
//! of the media it would without one.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime, WorkingColorspace};
use klipp_effects::BuiltinEffect as E;
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, CompositionId, Sequence, Track, TrackKind};
use klipp_veylvik::{Composition, Compositions, Compositor, Time};

const W: u32 = 96;
const H: u32 = 54;
const FPS: u32 = 24;

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, FPS)
}

/// A chart that changes every frame of the media: a wrong frame is seen.
struct Frames;
impl FrameSource for Frames {
    fn frame(&mut self, _: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let index = (secs * f64::from(FPS) + 1e-6).floor() as f32;
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                px.extend_from_slice(&[x as f32 / w as f32, y as f32 / h as f32, (index / 96.0).fract(), 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(px)), width: w, height: h, pts: Some(secs) })
    }
    fn media_fps(&mut self, _: &Clip) -> Option<f64> {
        Some(f64::from(FPS))
    }
    fn media_range(&mut self, _: &Clip) -> Option<(f64, f64)> {
        Some((0.0, 20.0))
    }
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

fn sequence(clip: Clip) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    seq
}

fn render(seq: &Sequence, src: &mut dyn FrameSource, gpu: bool, at: f64) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.set_working_colorspace(WorkingColorspace::Rec709Linear);
    r.render_inspected(seq, tc(at), W, H, src, RenderOptions::default(), Inspect::default()).0
}

#[test]
fn a_clip_keeps_every_edit_of_klipps_through_a_composition() {
    let Some(compositor) = Compositor::shared() else {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    };
    let pass = CompositionId(uuid::Uuid::from_u128(1));
    let c = Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, "Clip", None, [W, H], Time { ticks: i64::from(FPS), rate: 1 }, 480).unwrap();
    compositor.set_compositions([(pass, Arc::<[u8]>::from(serde_json::to_vec(&c).unwrap()))].into());
    let media = MediaId(uuid::Uuid::from_u128(7));
    let base = || Clip::media(media, tc(0.0), tc(4.0), tc(0.0), tc(4.0));
    let trimmed = || Clip::media(media, tc(0.0), tc(4.0), tc(2.5), tc(6.5));
    let cases: Vec<(&str, Clip)> = vec![
        ("trimmed", trimmed()),
        ("at double speed", {
            let mut c = Clip::media(media, tc(0.0), tc(4.0), tc(1.0), tc(9.0));
            c.speed = 2.0;
            c
        }),
        ("with effects", TestClip::of(base()).effects([E::Gamma { gamma: 1.3 }, E::Vignette { amount: 0.5, size: 0.5, softness: 0.5, roundness: 0.5 }]).build()),
        ("keyed opacity, scale and position", TestClip::of(base()).lane("opacity", 0.0, 1.0).lane("opacity", 4.0, 0.4).lane("scale", 0.0, 1.0).lane("scale", 4.0, 0.6).lane("pos_x", 0.0, 0.0).lane("pos_x", 4.0, 20.0).build()),
        ("all of them", {
            let mut c = TestClip::of(trimmed()).effect(E::Gamma { gamma: 1.3 }).lane("opacity", 0.0, 1.0).lane("opacity", 4.0, 0.5).lane("scale", 0.0, 0.8).build();
            c.speed = 1.5;
            c
        }),
    ];
    let mut failed = Vec::new();
    for (name, clip) in cases {
        for gpu in [true, false] {
            for at in [0.5, 2.25] {
                let plain = render(&sequence(clip.clone()), &mut Frames, gpu, at);
                let mut composed_clip = clip.clone();
                composed_clip.composition = Some(pass);
                let mut frames = Frames;
                let before = compositor.composed();
                let through = render(&sequence(composed_clip), &mut compositor.source(&mut frames, WorkingColorspace::Rec709Linear), gpu, at);
                // Shown through the composition, not the media in its place.
                if compositor.composed() == before {
                    failed.push(format!("{name}, gpu={gpu}, at {at} s: the media was shown, not the composition"));
                }
                let off = worst(&plain, &through);
                if off > 1e-5 {
                    failed.push(format!("{name}, gpu={gpu}, at {at} s: {off}"));
                }
            }
        }
    }
    compositor.set_compositions(Compositions::new());
    assert!(failed.is_empty(), "{failed:#?}");
}
