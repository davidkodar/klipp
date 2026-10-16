//! The native Grain, Countdown and Timer generators draw on the GPU in the
//! stack (#2067), as the living gradient does (#2058): no CPU raster a
//! frame, and the CPU's drawing of each is the picture they keep.
use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame};
use klipp_core::{GeneratorData, RationalTime, Resolution, TimerFormat, TimerMode};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

struct NoMedia;
impl FrameSource for NoMedia {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> { None }
}

fn sequence(g: GeneratorData, w: u32, h: u32) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = Resolution { width: w, height: h };
    let tc = |s| RationalTime::from_secs_f64(s, seq.fps.rate());
    let mut track = Track::new("Generator", TrackKind::Video);
    track.clips.push(Clip { kind: ClipKind::Generator(g), source_in: tc(0.0), source_out: tc(10.0), ..Clip::gap(tc(0.0), tc(10.0)) });
    seq.tracks = vec![track];
    seq
}

fn renderer(gpu: bool) -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r
}

fn generators() -> Vec<(&'static str, GeneratorData)> {
    vec![
        ("grain", GeneratorData::Grain { color: [90, 70, 50, 255], amount: 0.6, size_px: 3.0, speed: 12.0 }),
        ("grain on a see-through colour", GeneratorData::Grain { color: [200, 30, 30, 120], amount: 1.0, size_px: 1.0, speed: 24.0 }),
        ("countdown", GeneratorData::Countdown),
        ("timer", GeneratorData::Timer { mode: TimerMode::CountUp, initial_seconds: 3595.0, format: TimerFormat::HhMmSsMs, text_color: [250, 200, 20, 255], background_color: [10, 20, 40, 200] }),
        ("timer counting down", GeneratorData::Timer { mode: TimerMode::CountDown, initial_seconds: 5.0, format: TimerFormat::MmSs, text_color: [255; 4], background_color: [0, 0, 0, 255] }),
    ]
}

/// Each draws on the GPU, nothing on the CPU, and shows the CPU's picture
/// at several times and in two working spaces: within a display level,
/// the dither's (#2072). Grain may round one byte the other way where its
/// noise lands on a whole code, so two levels on a few of its pixels.
#[test]
fn gpu_animated_generators_match_the_cpu() {
    let (w, h) = (160, 90);
    let mut cpu = renderer(false);
    let mut gpu = renderer(true);
    for ws in [klipp_core::WorkingColorspace::Rec709Linear, klipp_core::WorkingColorspace::Bt2020Linear] {
        cpu.set_working_colorspace(ws);
        gpu.set_working_colorspace(ws);
        for (name, g) in generators() {
            let seq = sequence(g, w, h);
            for secs in [0.0, 1.27, 4.5, 9.96] {
                let at = RationalTime::from_secs_f64(secs, seq.fps.rate());
                let (linear, _) = cpu.render_inspected(&seq, at, w, h, &mut NoMedia, RenderOptions::default(), Inspect::default());
                let expected = klipp_color::display::display_transform_ws_dithered_to(&linear, klipp_core::ToneMapOp::None, ws, klipp_core::SdrTransfer::Srgb);
                let Some((actual, stats)) = gpu.render_display_u8_on_gpu(&seq, at, w, h, &mut NoMedia, RenderOptions::default()) else {
                    assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required");
                    eprintln!("unverified: no GPU stack");
                    return;
                };
                assert_eq!((stats.gpu_generators, stats.cpu_generator_ms), (1, 0.0), "{name}: drawn on the GPU");
                let diffs: Vec<i32> = actual.iter().zip(&expected).map(|(a, b)| (*a as i32 - *b as i32).abs()).collect();
                let worst = *diffs.iter().max().unwrap();
                let over_one = diffs.iter().filter(|d| **d > 1).count();
                assert!(worst <= 2 && over_one * 1000 <= diffs.len(), "{name} {ws:?} at {secs}: worst {worst}, {over_one} values over one level");
                if !name.starts_with("grain") {
                    assert!(worst <= 1, "{name} {ws:?} at {secs}: {worst} levels");
                }
            }
        }
    }
}

/// Explicit measurement, not a timing assertion: one 4K number each.
#[test]
#[ignore = "4K performance measurement"]
fn measure_4k_animated_generators() {
    for (name, g) in generators().into_iter().filter(|(n, _)| ["grain", "countdown", "timer"].contains(n)) {
        let seq = sequence(g.clone(), 3840, 2160);
        let mut gpu = renderer(true);
        let mut samples = Vec::new();
        for n in 0..35 {
            let at = RationalTime::from_secs_f64(n as f64 / 30.0, seq.fps.rate());
            let start = std::time::Instant::now();
            let (_, stats) = gpu.render_display_u8_on_gpu(&seq, at, 3840, 2160, &mut NoMedia, RenderOptions::default()).expect("GPU stack");
            if n >= 5 { samples.push(start.elapsed().as_secs_f64() * 1000.0); }
            std::hint::black_box(stats);
        }
        samples.sort_by(f64::total_cmp);
        let start = std::time::Instant::now();
        for n in 0..10 {
            std::hint::black_box(klipp_core::generator::render_generator_f16(&g, 3840, 2160, n as f64 / 30.0, 10.0));
        }
        eprintln!("MEASURE 4K {name}: GPU stack frame (display readback included) median {:.2} ms; CPU raster alone {:.2} ms", samples[15], start.elapsed().as_secs_f64() * 100.0);
    }
}

/// #2068: a still frame is read from its file once and kept while the
/// file stands, and read again when it is rewritten, deleted (magenta) or
/// back: the residency record shows each read as a cache miss.
#[test]
fn a_still_frame_is_kept_until_its_file_changes() {
    let dir = std::env::temp_dir().join(format!("klipp-still-frame-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("still.png");
    let write = |rgb: [u8; 3], side: u32| image::RgbaImage::from_pixel(side, side, image::Rgba([rgb[0], rgb[1], rgb[2], 255])).save(&path).unwrap();
    write([200, 20, 20], 2);
    let seq = sequence(GeneratorData::StillFrame { png_path: path.clone(), width: 2, height: 2 }, 64, 36);
    let mut gpu = renderer(true);
    let mut frame = |secs: f64| -> Option<([u8; 3], bool)> {
        let at = RationalTime::from_secs_f64(secs, seq.fps.rate());
        let (px, _) = gpu.render_display_u8_on_gpu(&seq, at, 64, 36, &mut NoMedia, RenderOptions::default())?;
        let read = klipp_effects::residency::last_frame().events.iter().any(|e| e.feature == "generator.still_frame");
        let i = (18 * 64 + 32) * 4;
        Some(([px[i], px[i + 1], px[i + 2]], read))
    };
    let Some((first, read)) = frame(0.0) else {
        assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required");
        eprintln!("unverified: no GPU stack");
        return;
    };
    assert!(first[0] > 150 && first[2] < 60 && read, "read and drawn red: {first:?}");
    let (again, read) = frame(1.0).unwrap();
    assert!(again == first && !read, "the same file: kept, not read again");
    write([20, 20, 200], 4);
    let (blue, read) = frame(2.0).unwrap();
    assert!(blue[2] > 150 && blue[0] < 60 && read, "rewritten: read again, blue: {blue:?}");
    std::fs::remove_file(&path).unwrap();
    let (gone, read) = frame(3.0).unwrap();
    assert!(gone[0] > 150 && gone[1] < 60 && gone[2] > 150 && read, "gone: magenta: {gone:?}");
    let (still_gone, read) = frame(3.5).unwrap();
    assert!(still_gone == gone && !read, "still gone: kept");
    write([20, 200, 20], 2);
    let (back, read) = frame(4.0).unwrap();
    assert!(back[1] > 150 && back[0] < 60 && read, "back: read again: {back:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
