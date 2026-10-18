//! Temporal noise reduction on the GPU chain (#1988) draws what the CPU
//! pass draws: `temporal_denoise::denoise_frame`, within a level of 8-bit
//! sRGB, over noisy frames with no, one and two neighbours and several
//! thresholds and strengths, in each working space's luma (#2019), and
//! the alpha untouched.

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::temporal_denoise::denoise_frame;

const W: u32 = 61;
const H: u32 = 37;

fn pipeline() -> Option<GpuEffectsPipeline> {
    let g = GpuEffectsPipeline::new();
    if g.is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required but no GPU");
        eprintln!("UNVERIFIED: no GPU");
    }
    g
}

/// A picture with structure (a ramp each way) plus seeded noise, so some
/// pixels' neighbours are near and some far.
fn frame(seed: u32, noise: f32) -> Vec<f32> {
    let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    let mut out = Vec::with_capacity((W * H * 4) as usize);
    for i in 0..W * H {
        let (px, py) = ((i % W) as f32 / W as f32, (i / W) as f32 / H as f32);
        let mut n = || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            ((x >> 8) as f32 / 8_388_608.0 - 1.0) * noise
        };
        let (r, g, b) = ((0.05 + 0.8 * px + n()).max(0.0), (0.1 + 0.6 * py + n()).max(0.0), (0.3 + n()).max(0.0));
        out.extend_from_slice(&[r, g, b, 0.25 + 0.5 * px]);
    }
    out
}

fn srgb8(v: f32) -> i32 {
    (klipp_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as i32
}

fn on_gpu(g: &mut GpuEffectsPipeline, cur: &[f32], neighbours: &[&[f32]], threshold: f32, strength: f32, luma: [f32; 3]) -> Vec<f32> {
    g.ensure_chain_buffers(u64::from(W * H) * 16);
    g.upload_chain_input_linear(cur);
    let held: Vec<_> = neighbours.iter().map(|n| g.upload_neighbour(n).expect("a neighbour buffer")).collect();
    let bufs: Vec<&wgpu::Buffer> = held.iter().map(|b| b.as_ref()).collect();
    g.begin_chain();
    assert!(g.apply_temporal_denoise_chain(true, W, H, &bufs, threshold, strength, luma), "recorded");
    g.flush_chain_and_download_linear(false, (W * H) as usize)
}

/// The working spaces each case runs in, by their luma weights.
const SPACES: [klipp_core::WorkingColorspace; 3] = [
    klipp_core::WorkingColorspace::Rec709Linear,
    klipp_core::WorkingColorspace::AcesCG,
    klipp_core::WorkingColorspace::Bt2020Linear,
];

#[test]
fn the_gpu_draws_what_the_cpu_draws() {
    let Some(mut g) = pipeline() else { return };
    for ws in SPACES {
        draws_what_the_cpu_draws(&mut g, klipp_color::linear::luma_coefs(ws));
    }
}

fn draws_what_the_cpu_draws(g: &mut GpuEffectsPipeline, luma: [f32; 3]) {
    let (cur, before, after) = (frame(1, 0.03), frame(2, 0.03), frame(3, 0.03));
    let mut worst = 0;
    for count in 0..=2usize {
        let all = [before.as_slice(), after.as_slice()];
        let neighbours = &all[..count];
        for threshold in [0.0, 0.02, 0.15, 1.0] {
            for strength in [0.3, 1.0] {
                let want = denoise_frame(&cur, neighbours, threshold, strength, luma);
                let got = on_gpu(g, &cur, neighbours, threshold, strength, luma);
                assert_eq!(got.len(), want.len());
                for (i, (a, b)) in got.chunks_exact(4).zip(want.chunks_exact(4)).enumerate() {
                    assert_eq!(a[3], b[3], "pixel {i}: alpha untouched");
                    let off = (0..3).map(|c| (srgb8(a[c]) - srgb8(b[c])).abs()).max().unwrap();
                    worst = worst.max(off);
                    assert!(off <= 1, "luma {luma:?}, {count} neighbours, threshold {threshold}, strength {strength}: pixel {i} is {off} levels off ({a:?} vs {b:?})");
                }
                if count > 0 && threshold > 0.0 {
                    assert!(got != cur, "{count} neighbours at {threshold}: the picture changed");
                }
            }
        }
    }
    eprintln!("GPU against CPU, luma {luma:?}: worst {worst} levels");
}

/// More neighbours than the kernel binds, or one of the wrong size, are
/// refused with nothing recorded: the caller takes the CPU route.
#[test]
fn a_neighbour_it_cannot_bind_is_refused() {
    let Some(mut g) = pipeline() else { return };
    let cur = frame(1, 0.03);
    g.ensure_chain_buffers(u64::from(W * H) * 16);
    g.upload_chain_input_linear(&cur);
    let full = g.upload_neighbour(&cur).unwrap();
    let small = g.upload_neighbour(&cur[..cur.len() / 2]).unwrap();
    g.begin_chain();
    assert!(!g.apply_temporal_denoise_chain(true, W, H, &[&full, &full, &full], 0.1, 1.0, klipp_color::linear::luma_coefs(klipp_core::WorkingColorspace::Rec709Linear)), "three neighbours");
    assert!(!g.apply_temporal_denoise_chain(true, W, H, &[&small], 0.1, 1.0, klipp_color::linear::luma_coefs(klipp_core::WorkingColorspace::Rec709Linear)), "a neighbour of the wrong size");
    g.submit_chain();
}
