//! Each effect named in #2763 gives the look its name promises at the
//! op's defaults, measured on a frame made for it: a dark ground with a
//! sun, a sky, a sharp edge and a checkerboard. The effect is put on a
//! clip by the `effect` op and the frame rendered on the export's CPU path
//! and, where there is a GPU, on the GPU stack the viewer uses, which must
//! draw it without falling back; both must pass.

use klipp_document::ops;

const W: u32 = 192;
const H: u32 = 108;

/// The frame, linear light: dark ground, a sky across the top left, a sun
/// at (0.7, 0.3), a vertical edge in the lower left, a checkerboard in
/// the lower right.
fn frame() -> Vec<f32> {
    let mut px = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f32 / W as f32, y as f32 / H as f32);
            let mut c = [0.04, 0.045, 0.05];
            if fy < 0.45 && fx < 0.5 {
                c = [0.30 + 0.2 * fy, 0.42 + 0.2 * fy, 0.65];
            }
            if fy > 0.55 && fx < 0.45 {
                c = if fx < 0.22 { [0.08; 3] } else { [0.6; 3] };
            }
            if fy > 0.55 && fx > 0.55 {
                c = if ((x / 8) + (y / 8)) % 2 == 0 { [0.7; 3] } else { [0.03; 3] };
            }
            let d = ((fx - 0.7) * W as f32).hypot((fy - 0.3) * H as f32);
            if d < 4.0 {
                c = [6.0, 5.5, 4.5];
            }
            px.extend_from_slice(&[c[0], c[1], c[2], 1.0]);
        }
    }
    px
}

struct Frame;

impl klipp_compose::FrameSource for Frame {
    fn frame(&mut self, _: &klipp_timeline::Clip, _: f64, width: u32, height: u32) -> Option<klipp_compose::SourceFrame> {
        assert_eq!((width, height), (W, H), "drawn at the sequence's size");
        Some(klipp_compose::SourceFrame { pixels: klipp_compose::SourcePixels::Linear(std::sync::Arc::new(frame())), width, height, pts: None })
    }
}

/// A one-clip sequence with the `effect` op applied as the app applies it.
fn with_effect(kind: &str, amount: Option<f32>, params: serde_json::Value) -> klipp_timeline::Sequence {
    let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
    let mut pool = klipp_media::MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/effects-look.mov".into(), None);
    item.duration = tc(2.0);
    item.video_codec = "h264".into();
    let media = pool.add(item);
    let mut seq = klipp_timeline::Sequence::new_default();
    seq.fps = klipp_core::Fps::new(25, 1);
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut v = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
    v.clips = vec![klipp_timeline::Clip::media(media, tc(0.0), tc(2.0), tc(0.0), tc(2.0))];
    let clip = v.clips[0].id.0.to_string();
    seq.tracks = vec![v];
    let mut op = serde_json::json!({"op": "effect", "clip_id": clip, "kind": kind, "params": params});
    if let Some(a) = amount {
        op["amount"] = serde_json::json!(a);
    }
    let ops: Vec<ops::Op> = serde_json::from_value(serde_json::json!([op])).unwrap();
    let plan = ops::resolve(&seq, &pool, &ops).unwrap_or_else(|r| panic!("{kind}: {}", r.why));
    let mut fps = seq.fps;
    let mut sequences = vec![seq];
    let mut doc = klipp_document::apply::DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
    for step in &plan.steps {
        doc.apply_forward(&step.edit);
    }
    sequences.remove(0)
}

/// The frame with the effect on each path there is: the export's CPU
/// path, and the GPU stack the viewer draws with, which must draw it
/// without falling back to the CPU.
fn runs(kind: &str, amount: Option<f32>, params: serde_json::Value) -> Vec<(&'static str, Vec<f32>)> {
    let seq = with_effect(kind, amount, params);
    let at = klipp_core::RationalTime::from_secs_f64(0.5, 25);
    let mut cpu = klipp_compose::Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    cpu.set_gpu_enabled(false);
    let mut out = vec![("cpu", cpu.render(&seq, at, W, H, &mut Frame, klipp_compose::RenderOptions::default()))];
    if std::env::var_os("KLIPP_TEST_SKIP_GPU").is_none() {
        let mut gpu = klipp_compose::Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        gpu.set_gpu_enabled(true);
        match gpu.render_linear_on_gpu(&seq, at, W, H, &mut Frame, klipp_compose::RenderOptions::default()) {
            Some(px) => {
                // This thread's frame: the tests run side by side.
                let frame = klipp_effects::residency::last_frame();
                assert_eq!(frame.fallback_count(), 0, "{kind}: the GPU stack fell back to the CPU: {:?}", frame.events);
                out.push(("gpu", px));
            }
            None => assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "{kind}: KLIPP_TEST_REQUIRE_GPU=1, but the GPU stack declined"),
        }
    }
    out
}

fn shown(v: f32) -> f32 {
    v.clamp(0.0, 1.0).powf(1.0 / 2.2)
}

fn luma(p: &[f32]) -> f32 {
    shown(0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])
}

/// Mean shown luma over a region, `x` and `y` as frame fractions.
fn region(f: &[f32], x: (f32, f32), y: (f32, f32)) -> f32 {
    let (mut s, mut n) = (0.0, 0.0);
    for py in (y.0 * H as f32) as u32..(y.1 * H as f32) as u32 {
        for px in (x.0 * W as f32) as u32..(x.1 * W as f32) as u32 {
            let i = ((py * W + px) * 4) as usize;
            s += luma(&f[i..i + 4]);
            n += 1.0;
        }
    }
    s / n
}

/// The spread of shown luma over a region.
fn spread(f: &[f32], x: (f32, f32), y: (f32, f32)) -> f32 {
    let mean = region(f, x, y);
    let (mut s, mut n) = (0.0, 0.0);
    for py in (y.0 * H as f32) as u32..(y.1 * H as f32) as u32 {
        for px in (x.0 * W as f32) as u32..(x.1 * W as f32) as u32 {
            let i = ((py * W + px) * 4) as usize;
            s += (luma(&f[i..i + 4]) - mean).powi(2);
            n += 1.0;
        }
    }
    (s / n).sqrt()
}

const NEAR_SUN: ((f32, f32), (f32, f32)) = ((0.62, 0.66), (0.22, 0.38));
const SKY: ((f32, f32), (f32, f32)) = ((0.05, 0.45), (0.05, 0.4));
const CHECKER_CORNER: ((f32, f32), (f32, f32)) = ((0.88, 1.0), (0.85, 1.0));

#[test]
fn glow_lights_round_the_highlights_not_the_frame() {
    let base = frame();
    for (path, out) in runs("glow", None, serde_json::json!({})) {
        // The sky, mid-bright and far from the sun, is not hazed; round
        // the sun the light spreads.
        let haze = (region(&out, (0.05, 0.3), SKY.1) - region(&base, (0.05, 0.3), SKY.1)).abs();
        let halo = region(&out, NEAR_SUN.0, NEAR_SUN.1) - region(&base, NEAR_SUN.0, NEAR_SUN.1);
        println!("glow {path}: sky {haze:+.3}, round the sun {halo:+.3}");
        assert!(haze < 0.01 && halo > 0.03, "glow {path}: the sky {haze:+.3}, round the sun {halo:+.3}");
    }
}

#[test]
fn a_lens_flare_is_a_flare_where_it_is_put() {
    let base = frame();
    for (path, out) in runs("lens_flare", None, serde_json::json!({})) {
        let veil = region(&out, (0.0, 0.1), (0.85, 1.0)) - region(&base, (0.0, 0.1), (0.85, 1.0));
        let at = region(&out, (0.67, 0.73), (0.25, 0.35)) - region(&base, (0.67, 0.73), (0.25, 0.35));
        println!("lens_flare {path}: far corner {veil:+.3}, at its source {at:+.3}");
        assert!(veil < 0.03, "lens_flare {path} veils the frame: far corner {veil:+.3}");
    }
    for (path, out) in runs("lens_flare", None, serde_json::json!({"position_x": 0.2, "position_y": 0.8})) {
        let moved = region(&out, (0.17, 0.23), (0.75, 0.85)) - region(&base, (0.17, 0.23), (0.75, 0.85));
        println!("lens_flare {path} at (0.2, 0.8): there {moved:+.3}");
        assert!(moved > 0.05, "lens_flare {path}: put at (0.2, 0.8) by params, {moved:+.3} there");
    }
}

#[test]
fn a_radial_blur_keeps_the_picture() {
    let base = frame();
    for (path, out) in runs("radial_blur", Some(0.6), serde_json::json!({})) {
        let kept = spread(&out, CHECKER_CORNER.0, CHECKER_CORNER.1) / spread(&base, CHECKER_CORNER.0, CHECKER_CORNER.1);
        let centre = (region(&out, (0.45, 0.55), (0.45, 0.55)) - region(&base, (0.45, 0.55), (0.45, 0.55))).abs();
        println!("radial_blur 0.6 {path}: corner detail kept x{kept:.2}, centre moved {centre:.3}");
        assert!(kept > 0.25 && centre < 0.05, "radial_blur 0.6 {path}: corner detail x{kept:.2}, centre {centre:.3}");
    }
}

#[test]
fn a_lens_distortion_bends_without_smearing_the_edges() {
    let base = frame();
    for (path, out) in runs("lens_distortion", None, serde_json::json!({})) {
        let kept = spread(&out, CHECKER_CORNER.0, CHECKER_CORNER.1) / spread(&base, CHECKER_CORNER.0, CHECKER_CORNER.1);
        let moved = (0..out.len()).step_by(4).filter(|i| (luma(&out[*i..*i + 4]) - luma(&base[*i..*i + 4])).abs() > 0.02).count() as f32 / (W * H) as f32;
        println!("lens_distortion {path}: corner checker kept x{kept:.2}, {:.0}% of pixels moved", moved * 100.0);
        assert!(kept > 0.6 && moved > 0.05, "lens_distortion {path}: corner checker x{kept:.2}, {:.0}% moved", moved * 100.0);
    }
}

#[test]
fn binocular_masks_above_and_below_alike() {
    let base = frame();
    for (path, out) in runs("binocular", None, serde_json::json!({})) {
        // How much of each corner is left, against the frame: the sky is
        // in the top one, so the share, not the level.
        let left = |y: (f32, f32)| region(&out, (0.0, 0.05), y) / region(&base, (0.0, 0.05), y);
        let (top, bottom) = (left((0.0, 0.05)), left((0.95, 1.0)));
        let middle = region(&out, (0.2, 0.3), (0.45, 0.55)) / region(&base, (0.2, 0.3), (0.45, 0.55));
        println!("binocular {path}: corners keep top x{top:.2}, bottom x{bottom:.2}; an eye's middle x{middle:.2}");
        assert!(top < 0.1 && bottom < 0.1 && (top - bottom).abs() < 0.05 && middle > 0.9, "binocular {path}: corners top x{top:.2}, bottom x{bottom:.2}, eye x{middle:.2}");
    }
}

#[test]
fn emboss_shows_relief_at_an_edge() {
    let base = frame();
    for (path, out) in runs("emboss", None, serde_json::json!({})) {
        // The edge at x 0.22 in the lower left: a ridge there against the
        // flat on either side.
        let ridge = (region(&out, (0.215, 0.225), (0.6, 0.95)) - region(&out, (0.1, 0.15), (0.6, 0.95))).abs();
        let contrast = spread(&out, (0.0, 1.0), (0.0, 1.0)) / spread(&base, (0.0, 1.0), (0.0, 1.0));
        println!("emboss {path}: ridge {ridge:.3}, contrast x{contrast:.2}");
        assert!(ridge > 0.1 && contrast > 0.5, "emboss {path}: ridge {ridge:.3}, contrast x{contrast:.2}");
    }
}

#[test]
fn edge_detect_draws_the_edges() {
    for (path, out) in runs("edge_detect", None, serde_json::json!({})) {
        let line = region(&out, (0.215, 0.225), (0.6, 0.95));
        let flat = region(&out, (0.28, 0.4), (0.6, 0.95)).max(region(&out, SKY.0, SKY.1));
        println!("edge_detect {path}: on the edge {line:.3}, flat areas {flat:.3}");
        assert!(line - flat > 0.3, "edge_detect {path}: on the edge {line:.3}, flat areas {flat:.3}");
    }
}

#[test]
fn invert_by_default_is_the_negative() {
    let base = frame();
    for (path, out) in runs("invert", None, serde_json::json!({})) {
        let contrast = spread(&out, (0.0, 1.0), (0.0, 1.0)) / spread(&base, (0.0, 1.0), (0.0, 1.0));
        let sky = region(&out, SKY.0, SKY.1);
        println!("invert {path}: contrast x{contrast:.2}, sky {sky:.3}");
        assert!(contrast > 0.8 && sky < 0.5, "invert {path}: contrast x{contrast:.2}, sky {sky:.3}");
    }
}
