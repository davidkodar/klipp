//! A still text is laid out on the GPU's glyph route (#2335), in the
//! viewer and in an export alike: its first frame waits on no CPU
//! raster, and the viewer, the export's GPU route and the CPU render
//! (the fallback, and the reference) draw the same picture.

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame};
use klipp_core::{RationalTime, Resolution};
use klipp_timeline::*;

struct NoMedia;
impl FrameSource for NoMedia {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        None
    }
}

fn renderer(gpu: bool) -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r
}

const W: u32 = 640;
const H: u32 = 360;

fn sequence(title: TitleData) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = Resolution { width: W, height: H };
    let mut track = Track::new("title", TrackKind::Video);
    track.clips.push(Clip { kind: ClipKind::Title(title), ..Clip::gap(RationalTime::from_secs_f64(0.0, 30), RationalTime::from_secs_f64(4.0, 30)) });
    seq.tracks = vec![track];
    seq
}

fn styled() -> TitleData {
    TitleData::single_text_fx(
        TextBox { text: "Still and styled".into(), font_size: 110.0, color: [235, 210, 120, 255], ..Default::default() },
        LayerFx {
            glows: vec![TextGlow { enabled: true, color: [80, 150, 255, 220], radius: 14.0, radius_keyframes: vec![], intensity: 0.9, intensity_keyframes: vec![] }],
            shadows: vec![TitleShadow { enabled: true, color: [0, 0, 0, 180], offset_x: 4.0, offset_y: 6.0, blur: 5.0, offset_x_keyframes: vec![], offset_y_keyframes: vec![], blur_keyframes: vec![] }],
            outlines: vec![TextOutline { enabled: true, color: [240, 245, 255, 230], width: 3.0, width_keyframes: vec![] }],
            ..Default::default()
        },
    )
}

#[test]
fn a_still_styled_text_is_one_picture_in_the_viewer_and_the_export() {
    let seq = sequence(styled());
    assert!(klipp_title::takes_glyph_route(&styled(), None), "a still lone text takes the glyph route");
    let mut gpu = renderer(true);
    if gpu.executor().gpu_pipeline_or_init().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(), "GPU required");
        return;
    }
    let mut cpu = renderer(false);
    for t in [0.5, 1.5, 3.0] {
        let at = RationalTime::from_secs_f64(t, 30);
        let (want, _) = cpu.render_inspected(&seq, at, W, H, &mut NoMedia, RenderOptions::default(), Default::default());
        let want_display = klipp_color::display::display_transform_ws_dithered_to(&want, klipp_color::display::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb);
        let (viewer, _) = gpu.render_display_u8_on_gpu(&seq, at, W, H, &mut NoMedia, RenderOptions::default()).expect("resident title");
        let events = klipp_effects::residency::last_frame().events;
        assert!(!events.iter().any(|e| e.work == klipp_effects::residency::Work::Fallback), "at {t}: {events:?}");
        assert!(!events.iter().any(|e| e.feature == "title.resting"), "at {t}: no resting raster waited on: {events:?}");
        let worst = viewer.iter().zip(&want_display).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        assert!(worst <= 3, "at {t}: the viewer differs from the CPU render by {worst} levels");
        // The export's own GPU route, when it takes the frame, lays the
        // same picture out: its luma against the CPU frame's.
        let target = klipp_effects::gpu::Yuv420Target { tone_map_op: 0, odt_tag: 0, scene: None, gamut_op: 0, hdr_op: 0, peak_nits: 0.0, luma_coefs: [0.2126, 0.7152, 0.0722], semi_planar: false, bits: 8, sample_shift: 0 };
        if let Some((planes, _)) = gpu.render_yuv420_on_gpu(&seq, at, W, H, &mut NoMedia, RenderOptions::default(), target) {
            let export_events = klipp_effects::residency::last_frame().events;
            assert!(!export_events.iter().any(|e| e.work == klipp_effects::residency::Work::Fallback || e.feature == "title.resting"), "export at {t}: {export_events:?}");
            let display = klipp_color::display::ExportDisplay::new(klipp_core::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::OdtChoice::MatchViewer);
            let luma: Vec<u8> = want.chunks_exact(4).map(|p| {
                let e = display.encode([p[0], p[1], p[2]]);
                (16.0 + (0.2126 * e[0] + 0.7152 * e[1] + 0.0722 * e[2]) * 219.0 + 0.5).clamp(0.0, 255.0) as u8
            }).collect();
            let d = planes[..(W * H) as usize].iter().zip(&luma).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            assert!(d <= 2, "at {t}: the export's luma differs from the CPU frame's by {d} levels");
        }
    }
    assert_eq!(gpu.title_renderer().resting_renders(), 0, "no CPU raster of the title on the GPU routes");
}
