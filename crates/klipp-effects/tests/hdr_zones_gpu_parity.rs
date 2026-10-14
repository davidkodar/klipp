//! The HDR zones' (#240) standalone GPU pass, the one a node takes off
//! the chain, draws what `cpu_grade::hdr_zones` draws, within a part in
//! ten thousand, for each zone moved alone. Control: the same zones a
//! quarter stop off draw far from it. Without a GPU it says so; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_grade_model::hdr_zones::{Terms, ZONES};

const W: u32 = 48;
const H: u32 = 4;
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];
const BOUNDARY: [f32; ZONES] = [0.0, -4.5, -2.5, -1.0, 1.0, 2.5, 4.5];

fn frame() -> Vec<f32> {
    (0..W * H)
        .flat_map(|i| {
            let (x, y) = (i % W, i / W);
            let v = 0.18 * (-10.0 + 18.0 * x as f32 / (W - 1) as f32).exp2();
            [v * (1.0 + 0.3 * y as f32), v, v * (1.2 - 0.25 * y as f32), 1.0]
        })
        .collect()
}

fn terms(zone: usize, shift: f32) -> Terms {
    let (mut e, mut c, mut s) = ([0.0; ZONES], [[0.0; 3]; ZONES], [1.0; ZONES]);
    e[zone] = -0.6;
    c[zone] = [0.0, 0.2, -0.25];
    s[zone] = 0.7;
    Terms::new(e, c, s, BOUNDARY.map(|b| b + shift), [1.0; ZONES], LUMA)
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs() / y.abs().max(1.0)).fold(0.0, f32::max)
}

#[test]
fn the_standalone_pass_draws_the_cpu_zones() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    };
    let input = frame();
    for zone in 0..ZONES {
        let mut want = input.clone();
        klipp_effects::cpu_grade::hdr_zones(&mut want, &terms(zone, 0.0));
        assert!(want != input, "zone {zone} changes the picture");
        let got = gpu.apply_hdr_zones_gpu(&input, W, H, &terms(zone, 0.0));
        let w = worst(&got, &want);
        eprintln!("zone {zone}: worst {w}");
        assert!(w <= 1e-4, "zone {zone}: worst {w}");
        if zone > 0 {
            let mut off = input.clone();
            klipp_effects::cpu_grade::hdr_zones(&mut off, &terms(zone, 0.25));
            assert!(worst(&got, &off) > 1e-3, "zone {zone}: a quarter stop off is told apart");
        }
    }
}
