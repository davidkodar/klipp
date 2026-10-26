//! The planar tracker on the GPU follows what the CPU's does (#2754):
//! on the same frames, each frame's corners within a twentieth of a pixel
//! of the CPU's, the same states, and as close to the truth. A missing
//! GPU is reported as unverified; with `KLIPP_TEST_REQUIRE_GPU=1` it
//! fails.

use klipp_effects::gpu::planar::PlanarGpu;
use klipp_effects::homography::Homography;
use klipp_effects::planar::{track, Options, State, Tracked};
use klipp_effects::window_track::GrayFrame;

fn texture(x: f64, y: f64) -> f64 {
    128.0 + 40.0 * (x * 0.21).sin() * (y * 0.17).cos() + 30.0 * (x * 0.05 + y * 0.07).sin() + 25.0 * ((x - y) * 0.11).cos() + 15.0 * (x * 0.6).sin() * (y * 0.5).sin()
}

fn seen(w: usize, h: usize, map: &Homography, world: &dyn Fn(f64, f64) -> f64) -> GrayFrame {
    let back = map.inverse().unwrap();
    let mut data = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut v = 0.0;
            for (ox, oy) in [(-0.25, -0.25), (0.25, -0.25), (-0.25, 0.25), (0.25, 0.25)] {
                let (rx, ry) = back.transform(x as f64 + ox, y as f64 + oy);
                v += world(rx, ry) / 4.0;
            }
            data[y * w + x] = v.clamp(0.0, 255.0) as u8;
        }
    }
    GrayFrame { data, width: w, height: h }
}

fn course_map(quad: &[(f64, f64); 4], course: &[(f64, f64); 4], k: usize) -> Homography {
    let to: [(f64, f64); 4] = std::array::from_fn(|i| (quad[i].0 + course[i].0 * k as f64, quad[i].1 + course[i].1 * k as f64));
    Homography::from_4_points(quad, &to).unwrap()
}

fn corner_error(quad: &[(f64, f64); 4], a: &Homography, b: &Homography) -> f64 {
    quad.iter().map(|p| { let (x, y) = (a.transform(p.0, p.1), b.transform(p.0, p.1)); (x.0 - y.0).hypot(x.1 - y.1) }).sum::<f64>() / 4.0
}

fn gpu() -> Option<PlanarGpu> {
    let g = PlanarGpu::new();
    if g.is_none() {
        assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required");
        eprintln!("UNVERIFIED: no GPU");
    }
    g
}

/// Both trackers over the same frames.
fn both(gpu: &mut PlanarGpu, reference: &GrayFrame, quad: [(f64, f64); 4], n: usize, frame: &dyn Fn(usize) -> GrayFrame) -> (Vec<Tracked>, Vec<Tracked>) {
    let frames: Vec<usize> = (1..=n).collect();
    let cpu = track(reference, quad, &[], &frames, &mut |k| Some(frame(k)), &Options::default(), &|_| true);
    let on_gpu = gpu.track(reference, quad, &[], &frames, &mut |k| Some(frame(k)), &Options::default(), &|_| true);
    (cpu, on_gpu)
}

fn agree(name: &str, quad: &[(f64, f64); 4], cpu: &[Tracked], gpu: &[Tracked], truth: &dyn Fn(usize) -> Homography, bound: f64) {
    assert_eq!(cpu.len(), gpu.len(), "{name}: as many frames");
    for (c, g) in cpu.iter().zip(gpu) {
        assert_eq!(c.frame, g.frame);
        assert_eq!(c.state, g.state, "{name} frame {}: {:?} on the CPU, {:?} on the GPU ({} vs {})", c.frame, c.state, g.state, c.confidence, g.confidence);
        if c.state != State::Lost {
            let apart = corner_error(quad, &c.h, &g.h);
            assert!(apart < 0.05, "{name} frame {}: the GPU's corners {apart:.4} px from the CPU's", c.frame);
            assert!(corner_error(quad, &g.h, &truth(g.frame)) < bound, "{name} frame {}", g.frame);
        }
    }
}

#[test]
fn the_gpu_follows_a_surface_as_the_cpu_does() {
    let Some(mut g) = gpu() else { return };
    // A textured surface turning away in perspective.
    let (w, h) = (320, 220);
    let quad = [(100.0, 60.0), (220.0, 70.0), (215.0, 160.0), (95.0, 150.0)];
    let course = [(0.9, -0.4), (0.5, 0.6), (0.7, -0.2), (1.1, 0.5)];
    let reference = seen(w, h, &Homography::identity(), &texture);
    let (cpu, on_gpu) = both(&mut g, &reference, quad, 20, &|k| seen(w, h, &course_map(&quad, &course, k), &texture));
    agree("textured", &quad, &cpu, &on_gpu, &|k| course_map(&quad, &course, k), 0.6);

    // A plain surface held by its outline.
    let plain_quad = [(110.0, 70.0), (210.0, 75.0), (205.0, 150.0), (105.0, 145.0)];
    let plain = move |x: f64, y: f64| {
        let inside = (0..4).all(|i| {
            let (a, b) = (plain_quad[i], plain_quad[(i + 1) % 4]);
            (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0) >= 0.0
        });
        if inside { 200.0 } else { 70.0 + 10.0 * ((x * 0.02).sin() + (y * 0.03).cos()) }
    };
    let course = [(0.8, 0.3), (0.6, -0.2), (0.9, 0.4), (0.5, 0.1)];
    let reference = seen(w, h, &Homography::identity(), &plain);
    let (cpu, on_gpu) = both(&mut g, &reference, plain_quad, 16, &|k| seen(w, h, &course_map(&plain_quad, &course, k), &plain));
    agree("plain", &plain_quad, &cpu, &on_gpu, &|k| course_map(&plain_quad, &course, k), 1.2);
}

/// A loss is flagged on the GPU where it is on the CPU, and the surface
/// picked up again where the CPU picks it up.
#[test]
fn the_gpu_loses_and_finds_the_surface_as_the_cpu_does() {
    let Some(mut g) = gpu() else { return };
    let (w, h) = (300, 200);
    let quad = [(90.0, 60.0), (200.0, 60.0), (200.0, 140.0), (90.0, 140.0)];
    let reference = seen(w, h, &Homography::identity(), &texture);
    let flat = GrayFrame { data: vec![128; w * h], width: w, height: h };
    let shift = |k: usize| Homography { h: [[1.0, 0.0, 0.6 * k as f64], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] };
    let (cpu, on_gpu) = both(&mut g, &reference, quad, 16, &|k| if (6..10).contains(&k) { flat.clone() } else { seen(w, h, &shift(k), &texture) });
    agree("lost", &quad, &cpu, &on_gpu, &shift, 0.8);
}
