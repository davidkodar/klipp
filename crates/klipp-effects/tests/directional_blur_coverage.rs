//! The Directional Blur averages premultiplied (#2021): a picture over
//! transparency smears at its coverage, as the shutter's samples are
//! averaged (#2017). Averaged straight, the clear pixels' colour (black,
//! here) was mixed in, and the trail composited darker than the picture.
//! An opaque picture blurs as it did, to the bit, and the GPU draws what
//! the CPU draws.

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::BuiltinEffect;

const W: u32 = 64;
const H: u32 = 16;

fn blur() -> [BuiltinEffect; 1] {
    [BuiltinEffect::DirectionalBlur { angle: 0.0, distance: 16.0, samples: 16 }]
}

/// An opaque red block on the left half, transparent black on the right.
fn red_over_clear() -> Vec<f32> {
    (0..W * H).flat_map(|i| if i % W < W / 2 { [1.0, 0.0, 0.0, 1.0] } else { [0.0, 0.0, 0.0, 0.0] }).collect()
}

/// Every pixel of `blurred`, composited over blue, is red at its coverage
/// over blue: no darker, no lighter. The pixels the edge smeared over (a
/// coverage strictly between 0 and 1) number at least `smeared`, so the
/// check isn't vacuous.
fn composites_at_its_coverage(blurred: &[f32], what: &str) {
    let mut smeared = 0;
    for (i, p) in blurred.chunks_exact(4).enumerate() {
        let a = p[3];
        let over_blue = [p[0] * a, p[1] * a, p[2] * a + (1.0 - a)];
        let want = [a, 0.0, 1.0 - a];
        for c in 0..3 {
            assert!((over_blue[c] - want[c]).abs() < 1e-4, "{what}: pixel {i} (coverage {a}) composites to {over_blue:?}, not {want:?}");
        }
        smeared += usize::from(a > 0.01 && a < 0.99);
    }
    assert!(smeared >= (H * 8) as usize, "{what}: only {smeared} pixels on the smeared edge");
}

#[test]
fn a_blurred_edge_over_transparency_composites_at_its_coverage() {
    composites_at_its_coverage(&klipp_effects::apply_effects(&blur(), red_over_clear(), W, H), "CPU");
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required");
        eprintln!("UNVERIFIED: no GPU");
        return;
    };
    composites_at_its_coverage(&gpu.apply_effects(&blur(), &red_over_clear(), W, H), "GPU");
}

/// A soft picture with transparency, coloured and feathered: the GPU's
/// blur is the CPU's within a level of 8-bit display, everywhere.
#[test]
fn the_gpu_blurs_transparency_as_the_cpu_does() {
    let px: Vec<f32> = (0..W * H).flat_map(|i| {
        let (x, y) = ((i % W) as f32 / W as f32, (i / W) as f32 / H as f32);
        [0.2 + 0.7 * x, 0.6 * y, 0.9 - 0.5 * x, (1.5 - 2.5 * x).clamp(0.0, 1.0) * (0.5 + 0.5 * y)]
    }).collect();
    let fx = [BuiltinEffect::DirectionalBlur { angle: 30.0, distance: 12.0, samples: 12 }];
    let cpu = klipp_effects::linear_f32_rgba_to_srgb_u8(&klipp_effects::apply_effects(&fx, px.clone(), W, H));
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required");
        eprintln!("UNVERIFIED: no GPU");
        return;
    };
    let on_gpu = klipp_effects::linear_f32_rgba_to_srgb_u8(&gpu.apply_effects(&fx, &px, W, H));
    let worst = cpu.iter().zip(&on_gpu).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    assert!(worst <= 1, "the GPU's blur is {worst} levels from the CPU's");
}

/// An opaque picture blurs exactly as before (#2021): the CPU's result
/// hashes to what it was before the premultiplied mean (taken on main,
/// 1d46d67ce, from these calls; the GPU's was checked the same way and
/// matches too). The control: a picture with transparency is another.
#[test]
fn an_opaque_picture_blurs_as_before() {
    let fnv = |v: &[f32]| v.iter().flat_map(|x| x.to_bits().to_le_bytes()).fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    let (w, h) = (64u32, 36u32);
    let picture = |alpha: f32| -> Vec<f32> { (0..w * h).flat_map(|i| { let v = (i % 97) as f32 / 97.0; [v, 0.4 * v, 1.0 - v, if i % 5 == 0 { alpha } else { 1.0 }] }).collect() };
    let fx = [BuiltinEffect::DirectionalBlur { angle: 20.0, distance: 9.0, samples: 12 }];
    assert_eq!(fnv(&klipp_effects::apply_effects(&fx, picture(1.0), w, h)), 0x18da_20e9_8ce5_948c, "an opaque picture's blur moved");
    let straight = |px: Vec<f32>| -> Vec<f32> {
        // The straight mean, as the blur took it before: what a picture
        // with transparency must no longer be.
        let n = 12usize;
        let (dx, dy) = { let (s, c) = 20.0f32.to_radians().sin_cos(); (c * 9.0 / n as f32, s * 9.0 / n as f32) };
        let mut out = vec![0.0f32; px.len()];
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let mut s = [0.0f32; 4];
                for i in 0..n {
                    let qx = ((x as f32 + dx * i as f32).round_ties_even() as i32).clamp(0, w as i32 - 1);
                    let qy = ((y as f32 + dy * i as f32).round_ties_even() as i32).clamp(0, h as i32 - 1);
                    let q = ((qy as u32 * w + qx as u32) * 4) as usize;
                    (0..4).for_each(|c| s[c] += px[q + c]);
                }
                let o = ((y as u32 * w + x as u32) * 4) as usize;
                (0..4).for_each(|c| out[o + c] = s[c] / n as f32);
            }
        }
        out
    };
    assert_eq!(fnv(&straight(picture(1.0))), 0x18da_20e9_8ce5_948c, "the straight mean written out is the blur as it was");
    let clear = picture(0.0);
    assert_ne!(klipp_effects::apply_effects(&fx, clear.clone(), w, h), straight(clear), "the control: with transparency, no longer the straight mean");
}
