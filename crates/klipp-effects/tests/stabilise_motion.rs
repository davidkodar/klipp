//! Stabilisation's analysis measured against a camera whose shake is
//! known (#2754): each frame-to-frame motion it finds, in the frame's
//! units, against the one the frames were drawn with, as how far apart
//! the two put the frame's corners and centre, in pixels, with a dark
//! disc crossing the picture as a subject would.

use klipp_effects::gpu::planar::PlanarGpu;
use klipp_effects::rolling_shutter::Homography;
use klipp_effects::stabilization::analyze_clip_on;

const W: u32 = 640;
const H: u32 = 360;
const FRAMES: usize = 30;

fn texture(x: f64, y: f64) -> f64 {
    128.0 + 40.0 * (x * 0.11).sin() * (y * 0.09).cos() + 30.0 * (x * 0.031 + y * 0.047).sin() + 25.0 * ((x - y) * 0.07).cos() + 15.0 * (x * 0.3).sin() * (y * 0.27).sin()
}

/// The camera on frame `k`: the world's pixels to the frame's, a shake
/// of move, turn and size about the frame's centre.
fn camera(k: usize) -> Homography {
    let f = k as f64;
    let (dx, dy) = (6.0 * (f * 1.7).sin() + 2.0 * f, 5.0 * (f * 2.3 + 1.0).cos());
    let (turn, size) = ((0.6 * (f * 1.3).sin()).to_radians(), 1.0 + 0.006 * (f * 2.9).cos());
    let (a, b) = (size * turn.cos(), size * turn.sin());
    let (cx, cy) = (W as f64 / 2.0, H as f64 / 2.0);
    Homography { h: [[a, -b, cx + dx - a * cx + b * cy], [b, a, cy + dy - b * cx - a * cy], [0.0, 0.0, 1.0]] }
}

fn frame(k: usize) -> Vec<u8> {
    let back = camera(k).inverse().unwrap();
    let disc = (80.0 + 14.0 * k as f64, 200.0);
    let mut rgba = vec![255u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let (wx, wy) = back.transform(x as f64, y as f64);
            let v = if (x as f64 - disc.0).hypot(y as f64 - disc.1) < 45.0 { 25.0 } else { texture(wx, wy) };
            let i = ((y * W + x) * 4) as usize;
            rgba[i..i + 3].fill(v.clamp(0.0, 255.0) as u8);
        }
    }
    rgba
}

/// The frame's units: centred, its height 1.
fn units() -> Homography {
    let k = 1.0 / H as f64;
    Homography { h: [[k, 0.0, -0.5 * W as f64 * k], [0.0, k, -0.5], [0.0, 0.0, 1.0]] }
}

/// How far each frame-to-frame motion found on `gpu` (the CPU without
/// one) is from the camera's, pixels.
fn errors(gpu: Option<&mut PlanarGpu>) -> Vec<f64> {
    let frames: Vec<Vec<u8>> = (0..FRAMES).map(frame).collect();
    let mut decode = |t: f64| frames.get((t * 24.0).round() as usize).map(|f| (f.clone(), W, H));
    let data = analyze_clip_on(gpu, 24.0, FRAMES, &mut decode, &|_| true).expect("analysed");
    let (to, back) = (units(), units().inverse().unwrap());
    let probes = [(0.0, 0.0), (W as f64, 0.0), (W as f64, H as f64), (0.0, H as f64), (W as f64 / 2.0, H as f64 / 2.0)];
    data.frame_transforms
        .iter()
        .enumerate()
        .map(|(k, found)| {
            let truth = camera(k + 1).multiply(&camera(k).inverse().unwrap());
            // The found motion in this frame's pixels.
            let found = back.multiply(found).multiply(&to);
            let e = probes.iter().map(|p| {
                let (a, b) = (found.transform(p.0, p.1), truth.transform(p.0, p.1));
                (a.0 - b.0).hypot(a.1 - b.1)
            });
            e.sum::<f64>() / probes.len() as f64
        })
        .collect()
}

fn check(on: &str, errors: &[f64]) {
    let mean = errors.iter().sum::<f64>() / errors.len() as f64;
    let worst = errors.iter().copied().fold(0.0, f64::max);
    eprintln!("stabilisation motion error on the {on}: mean {mean:.3} px, worst {worst:.3} px; {}", errors.iter().map(|e| format!("{e:.2}")).collect::<Vec<_>>().join(" "));
    assert!(mean < 0.05 && worst < 0.1, "{on}: mean {mean:.3} px, worst {worst:.3} px");
}

#[test]
fn stabilisation_finds_the_cameras_shake() {
    check("CPU", &errors(None));
    match PlanarGpu::new() {
        Some(mut g) => check("GPU", &errors(Some(&mut g))),
        None => {
            assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required");
            eprintln!("UNVERIFIED on the GPU: none here");
        }
    }
}
