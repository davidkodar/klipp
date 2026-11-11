//! A still generator's picture is kept on the GPU, not sent up every frame:
//! a solid colour behind the stress test's whole timeline sent a 4K frame
//! up every frame, about 7 ms of each. Its picture is the CPU's, in the
//! viewer and in export, and a changed colour shows at once.
use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame};
use klipp_core::{GeneratorData, RationalTime, Resolution};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

struct NoMedia;
impl FrameSource for NoMedia {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        None
    }
}

const W: u32 = 320;
const H: u32 = 180;

fn sequence(g: GeneratorData) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = Resolution { width: W, height: H };
    let tc = |s| RationalTime::from_secs_f64(s, seq.fps.rate());
    let mut track = Track::new("Background", TrackKind::Video);
    track.clips.push(Clip { kind: ClipKind::Generator(g), source_in: tc(0.0), source_out: tc(10.0), ..Clip::gap(tc(0.0), tc(10.0)) });
    seq.tracks = vec![track];
    seq
}

fn renderer(gpu: bool) -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// Drawn by `gpu` at `secs`, through the viewer's route and export's, each
/// against the CPU's picture; and the frames the stack sent up for it.
fn frame(gpu: &mut Renderer, cpu: &mut Renderer, seq: &Sequence, secs: f64) -> Option<u64> {
    let at = RationalTime::from_secs_f64(secs, seq.fps.rate());
    let (linear, _) = cpu.render_inspected(seq, at, W, H, &mut NoMedia, RenderOptions::default(), Inspect::default());
    let expected = klipp_color::display::display_transform_ws_dithered_to(&linear, klipp_core::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb);
    let before = gpu.stack_frame_uploads();
    let (shown, _) = gpu.render_display_u8_on_gpu(seq, at, W, H, &mut NoMedia, RenderOptions::default())?;
    let uploads = gpu.stack_frame_uploads() - before;
    let off = shown.iter().zip(&expected).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
    assert!(off <= 1, "the viewer's picture is the CPU's at {secs} s: {off} levels");
    let exported = gpu.render_linear_on_gpu(seq, at, W, H, &mut NoMedia, RenderOptions::default())?;
    assert!(worst(&exported, &linear) <= 2e-3, "export's picture is the CPU's at {secs} s: {}", worst(&exported, &linear));
    Some(uploads)
}

#[test]
fn a_still_generator_goes_up_once_and_a_changed_one_again() {
    let mut cpu = renderer(false);
    let mut gpu = renderer(true);
    for g in [
        GeneratorData::SolidColor { color: [14, 16, 24, 255] },
        GeneratorData::Gradient { top: [200, 40, 30, 255], bottom: [20, 60, 220, 255], angle_deg: 0.0, four_point: None, grain: 0.0 },
    ] {
        let seq = sequence(g.clone());
        let Some(first) = frame(&mut gpu, &mut cpu, &seq, 0.5) else {
            assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required");
            eprintln!("unverified: no GPU stack");
            return;
        };
        assert!(first >= 1, "{g:?}: its picture goes up the first time");
        for secs in [0.53, 1.0, 4.2, 9.9] {
            assert_eq!(frame(&mut gpu, &mut cpu, &seq, secs), Some(0), "{g:?} at {secs} s: kept, not sent up again");
        }
    }
    // A changed colour is a new picture: up again, and shown.
    let seq = sequence(GeneratorData::SolidColor { color: [220, 180, 20, 255] });
    let changed = frame(&mut gpu, &mut cpu, &seq, 0.5).expect("drawn");
    assert!(changed >= 1, "the changed colour goes up");
    assert_eq!(frame(&mut gpu, &mut cpu, &seq, 2.0), Some(0));
}
