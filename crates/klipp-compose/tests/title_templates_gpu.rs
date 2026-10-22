//! Every built-in title template, played in, is the same picture on the
//! GPU stack (the viewer, and a GPU export) as from the CPU renderer an
//! export falls back to (#2369). Before, a moving text layer of several
//! was rastered with its motion on the CPU, while the GPU stack placed
//! each layer as its one-layer title's frame: 21 of 38 templates came in
//! up to 246 levels apart.

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

/// Display levels between the two. A text staggered by line is posed
/// glyph by glyph, bilinear on the GPU and on the CPU, and on the faint
/// edge of a dark glyph near black, where the display encode magnifies a
/// tiny difference, a few pixels land up to 6 levels apart.
const LEVELS: u8 = 3;
const STAGGERED_EDGE_LEVELS: u8 = 6;
const STAGGERED_EDGE_PIXELS: usize = 64;

#[test]
fn every_template_plays_in_on_the_gpu_as_on_the_cpu() {
    let (w, h) = (640, 360);
    let (start, end) = (RationalTime::from_secs_f64(0.0, 30), RationalTime::from_secs_f64(5.0, 30));
    for t in title_library::TITLES.iter() {
        let title = t.build();
        let staggered = title.objects.iter().any(|o| o.fx().motion.enter.iter().chain(o.fx().motion.exit.iter()).any(|m| m.stagger != klipp_timeline::StaggerUnit::None));
        let blurs = title.objects.iter().any(|o| o.fx().motion.enter.iter().any(|m| m.kind.blurs()));
        // Over black, and a blur entrance also over a bright colour: a blur
        // in straight alpha that pulls in the colour of transparent pixels
        // shows there as a dark halo, and over black it would not (#2370).
        let backgrounds: &[Option<[u8; 4]>] = if blurs { &[None, Some([235, 205, 120, 255])] } else { &[None] };
        for bg in backgrounds {
            let mut seq = Sequence::new_default();
            seq.resolution = Resolution { width: w, height: h };
            let mut track = Track::new("title", TrackKind::Video);
            track.clips.push(Clip { kind: ClipKind::Title(title.clone()), ..Clip::gap(start, end) });
            seq.tracks = vec![track];
            if let Some(color) = *bg {
                let mut under = Track::new("background", TrackKind::Video);
                under.clips.push(Clip { kind: ClipKind::Generator(GeneratorData::SolidColor { color }), source_in: start, source_out: end, ..Clip::gap(start, end) });
                seq.tracks.push(under);
            }
            let mut gpu = renderer(true);
            if gpu.executor().gpu_pipeline_or_init().is_none() {
                assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(), "GPU required");
                return;
            }
            let mut cpu = renderer(false);
            let name = if bg.is_some() { format!("{} over a colour", t.name) } else { t.name.to_string() };
            // Mid-entrance, and at rest.
            for secs in [0.2, 0.5, 1.0, 2.5] {
                let at = RationalTime::from_secs_f64(secs, 30);
                let (want, _) = cpu.render_inspected(&seq, at, w, h, &mut NoMedia, RenderOptions::default(), Default::default());
                let want = klipp_color::display::display_transform_ws_dithered_to(&want, klipp_color::display::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb);
                let (got, _) = gpu.render_display_u8_on_gpu(&seq, at, w, h, &mut NoMedia, RenderOptions::default()).expect("resident title");
                let events = klipp_effects::residency::last_frame().events;
                assert!(!events.iter().any(|e| e.work == klipp_effects::residency::Work::Fallback), "{name} at {secs}: {events:?}");
                let worst = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
                let over = got.chunks_exact(4).zip(want.chunks_exact(4)).filter(|(a, b)| a.iter().zip(*b).any(|(x, y)| x.abs_diff(*y) > LEVELS)).count();
                let ok = worst <= LEVELS || (staggered && worst <= STAGGERED_EDGE_LEVELS && over < STAGGERED_EDGE_PIXELS);
                assert!(ok, "{name} at {secs}: the GPU differs from the CPU by {worst} levels, {over} pixels over {LEVELS}");
            }
        }
    }
}
