//! The title templates being moved to files (#2886) draw on the GPU
//! stack what their code drew: on a wide, a tall and a square frame,
//! mid-entry, at rest and mid-exit, within a level, with no frame falling
//! back.

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame};
use klipp_core::{RationalTime, Resolution};
use klipp_timeline::*;

struct NoMedia;
impl FrameSource for NoMedia {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        None
    }
}

fn frame(gpu: &mut Renderer, title: &TitleData, w: u32, h: u32, secs: f64, name: &str) -> Vec<u8> {
    let (start, end) = (RationalTime::from_secs_f64(0.0, 30), RationalTime::from_secs_f64(5.0, 30));
    let mut seq = Sequence::new_default();
    seq.resolution = Resolution { width: w, height: h };
    let mut track = Track::new("title", TrackKind::Video);
    track.clips.push(Clip { kind: ClipKind::Title(title.clone()), ..Clip::gap(start, end) });
    seq.tracks = vec![track];
    let (got, _) = gpu.render_display_u8_on_gpu(&seq, RationalTime::from_secs_f64(secs, 30), w, h, &mut NoMedia, RenderOptions::default()).expect("resident title");
    let events = klipp_effects::residency::last_frame().events;
    assert!(!events.iter().any(|e| e.work == klipp_effects::residency::Work::Fallback), "{name} at {secs}: {events:?}");
    got
}

#[test]
fn every_moved_file_draws_what_its_code_drew() {
    let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    gpu.set_gpu_enabled(true);
    if gpu.executor().gpu_pipeline_or_init().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(), "GPU required");
        return;
    }
    let coded = title_library::coded_for_check();
    let mut worst_seen = 0;
    for c in &coded {
        let file = title_library::builtin(&c.name).expect("listed");
        for (w, h) in [(640, 360), (360, 640), (480, 480)] {
            let (want, got) = (c.data_for(w, h), file.data_for(w, h));
            // Mid-entry, at rest, and mid-exit of a five-second clip.
            for secs in [0.2, 2.0, 4.85] {
                let name = format!("{} at {w}x{h}", c.name);
                let (a, b) = (frame(&mut gpu, &want, w, h, secs, &name), frame(&mut gpu, &got, w, h, secs, &name));
                let worst = a.iter().zip(&b).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
                assert!(worst <= 1, "{name} at {secs}: the file draws {worst} levels from the code");
                worst_seen = worst_seen.max(worst);
                if secs == 2.0 {
                    let drawn = b.chunks_exact(4).filter(|p| p[..3].iter().any(|v| *v > 30)).count();
                    assert!(drawn > 100, "{name}: something is drawn at rest ({drawn} pixels)");
                }
            }
        }
    }
    println!("moved titles, file against code: worst {worst_seen} levels over {} templates x 3 frames x 3 times", coded.len());
}
