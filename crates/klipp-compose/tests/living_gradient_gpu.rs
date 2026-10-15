//! Native living gradients stay on the GPU and retain the CPU look.
use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame};
use klipp_core::{GeneratorData, RationalTime, Resolution};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

struct NoMedia;
impl FrameSource for NoMedia {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> { None }
}
fn sequence(w: u32, h: u32, glow: f32) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = Resolution { width: w, height: h };
    let tc = |s| RationalTime::from_secs_f64(s, seq.fps.rate());
    let mut track = Track::new("Gradient", TrackKind::Video);
    track.clips.push(Clip {
        kind: ClipKind::Generator(GeneratorData::LivingGradient {
            a: [12, 23, 32, 180], b: [29, 60, 65, 255], angle_deg: 25.0, drift_deg_per_sec: 1.5, glow,
        }),
        source_in: tc(0.0), source_out: tc(10.0), ..Clip::gap(tc(0.0), tc(10.0))
    });
    seq.tracks = vec![track];
    seq
}
fn renderer(gpu: bool) -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r
}
#[test]
fn gpu_living_gradient_matches_cpu_at_several_times() {
    let mut cpu = renderer(false);
    let mut gpu = renderer(true);
    for (glow, ws) in [(0.0, klipp_core::WorkingColorspace::Rec709Linear), (0.18, klipp_core::WorkingColorspace::Bt2020Linear), (1.0, klipp_core::WorkingColorspace::Bt2020Linear)] {
        cpu.set_working_colorspace(ws);
        gpu.set_working_colorspace(ws);
        let seq = sequence(128, 72, glow);
        for secs in [0.0, 1.2, 7.5] {
            let at = RationalTime::from_secs_f64(secs, seq.fps.rate());
            let (mut linear, _) = cpu.render_inspected(&seq, at, 128, 72, &mut NoMedia, RenderOptions::default(), Inspect::default());
            if let Some(matrix) = klipp_color::primaries::working_space_conversion_matrix(ws, klipp_core::WorkingColorspace::Rec709Linear) {
                for px in linear.chunks_exact_mut(4) {
                    let rgb = klipp_color::primaries::matrix3_mul_vec(matrix, [px[0], px[1], px[2]]);
                    px[..3].copy_from_slice(&rgb);
                }
            }
            let expected = klipp_effects::linear_f32_rgba_to_srgb_u8(&linear);
            let Some((actual, stats)) = gpu.render_display_u8_on_gpu(&seq, at, 128, 72, &mut NoMedia, RenderOptions::default()) else {
                assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required");
                eprintln!("unverified: no GPU stack"); return;
            };
            assert_eq!(stats.gpu_generators, 1);
            assert_eq!(stats.cpu_generator_ms, 0.0);
            let worst = actual.iter().zip(&expected).map(|(a,b)| (*a as i32 - *b as i32).abs()).max().unwrap();
            assert!(worst <= 1, "glow={glow} time={secs}: {worst} display levels");
        }
    }
}

/// Explicit measurement, not a timing assertion on shared CI hardware.
#[test]
#[ignore = "4K performance measurement"]
fn measure_4k_living_gradient() {
    let seq = sequence(3840, 2160, 0.18);
    let mut gpu = renderer(true);
    let mut samples = Vec::new();
    for n in 0..35 {
        let start = std::time::Instant::now();
        let at = RationalTime::from_secs_f64(n as f64 / 30.0, seq.fps.rate());
        let (_, stats) = gpu.render_display_u8_on_gpu(&seq, at, 3840, 2160, &mut NoMedia, RenderOptions::default()).expect("GPU stack");
        assert_eq!(stats.gpu_generators, 1);
        if n >= 5 { samples.push(start.elapsed().as_secs_f64() * 1000.0); }
    }
    samples.sort_by(f64::total_cmp);
    eprintln!("4K gradient GPU stack, display readback included: median {:.2}ms p95 {:.2}ms", samples[15], samples[28]);
    let start = std::time::Instant::now();
    for n in 0..10 {
        std::hint::black_box(klipp_core::generator::render_living_gradient_f16([12,23,32,180], [29,60,65,255], 25.0, 1.5, 0.18, 3840, 2160, n as f64 / 30.0));
    }
    eprintln!("4K CPU generator alone: {:.2}ms mean", start.elapsed().as_secs_f64() * 100.0);
}

#[test]
fn generated_cards_keep_finishing_effects_after_reload() {
    use klipp_effects::BuiltinEffect as E;
    use klipp_timeline::test_clip::TestClip;
    let mut seq = sequence(128, 72, 0.18);
    seq.tracks[0].clips[0].kind = ClipKind::Generator(GeneratorData::SolidColor { color: [90, 140, 220, 255] });
    seq.tracks[0].clips[0] = TestClip::of(seq.tracks[0].clips[0].clone())
        .effect(E::RoundedCorners { radius: 0.25, feather: 0.002 })
        .effect(E::OuterGlow { radius: 0.02, intensity: 0.3, color: [140, 150, 255] })
        .effect(E::DropShadow { offset_x: 0.0, offset_y: 0.01, blur: 0.02, color: [0, 0, 0], opacity: 0.4 })
        .build();
    let loaded: Sequence = serde_json::from_str(&serde_json::to_string(&seq).unwrap()).unwrap();
    let at = RationalTime::from_secs_f64(1.0, seq.fps.rate());
    let mut cpu = renderer(false);
    let (linear, _) = cpu.render_inspected(&loaded, at, 128, 72, &mut NoMedia, RenderOptions::default(), Inspect::default());
    let expected = klipp_effects::linear_f32_rgba_to_srgb_u8(&linear);
    assert!(expected[0] < 20, "rounded corner should reveal the background");
    assert!(expected[(36 * 128 + 64) * 4] > 60, "card centre must remain visible");
    let Some((actual, _)) = renderer(true).render_display_u8_on_gpu(&loaded, at, 128, 72, &mut NoMedia, RenderOptions::default()) else {
        assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required"); return;
    };
    let worst = actual.iter().zip(&expected).map(|(a,b)| (*a as i32 - *b as i32).abs()).max().unwrap();
    assert!(worst <= 2, "viewer/export paths differ by {worst} levels");
}
