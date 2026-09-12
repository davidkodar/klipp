//! The GPU's ACES Rec.709 output transform is the CPU's.
//!
//! This test is the point of the exercise. Klipp's ACES ODTs are
//! verified against OpenColorIO, and that verification is worth
//! nothing if the GPU runs a copy that has drifted from them — which
//! is why the last WGSL copy of ACES here was removed. The shader
//! exists again because the drift-prone parts do not get copied: the
//! two tone curves and every derived matrix arrive as data baked from
//! `klipp_color::aces`, and what is hand-written is short arithmetic.
//!
//! The remaining risk is a transcription slip in that arithmetic — a
//! sign, a swapped basis column, a constant off by a digit. The whole
//! job of this file is to find one, so it tests where such a slip
//! would hide: across eighteen stops, across the full saturation
//! range, on the pure primaries and on neutrals, and on the specific
//! inputs that reach the awkward branches — the glow's piecewise
//! shadow region, the red modifier's hue bell, the gamut clamps.

use klipp_color::aces::{aces_rec709_gpu_data, acescg_to_rec709_sdr};
use klipp_effects::{gpu::GpuEffectsPipeline, gpu_context::GpuContext};

/// 1024 entries a curve — the size measured at 0.041 code values in
/// `klipp_color`'s own table tests.
const CURVE_ENTRIES: usize = 1024;

/// Run a batch of AcesCG colours through the shader.
fn gpu(colours: &[[f32; 3]]) -> Option<Vec<[f32; 3]>> {
    let ctx = GpuContext::new()?;
    let mut pipeline = GpuEffectsPipeline::with_context(&ctx)?;

    // One row of pixels; width must cover the batch.
    let w = colours.len() as u32;
    let h = 1u32;
    let mut linear = Vec::with_capacity(colours.len() * 4);
    for c in colours {
        linear.extend_from_slice(&[c[0], c[1], c[2], 1.0]);
    }
    pipeline.ensure_chain_buffers((w as u64) * (h as u64) * 16);
    pipeline.upload_chain_input_linear(&linear);

    let data = aces_rec709_gpu_data(CURVE_ENTRIES);
    let bytes = pipeline.encode_chain_to_aces_rec709_u8(true, w, h, &data)?;
    Some(
        bytes
            .chunks_exact(4)
            .map(|p| [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0])
            .collect(),
    )
}

/// The CPU answer, quantised the same way the shader quantises.
fn cpu_u8(c: [f32; 3]) -> [u8; 3] {
    let e = acescg_to_rec709_sdr(c);
    [0, 1, 2].map(|i| (e[i] * 255.0 + 0.5).clamp(0.0, 255.0) as u8)
}

fn compare(label: &str, colours: &[[f32; 3]], tolerance: i32) {
    let Some(got) = gpu(colours) else {
        eprintln!("skipping {label}: no usable GPU");
        return;
    };
    let mut worst = 0i32;
    let mut worst_at = 0usize;
    let mut differing = 0usize;
    for (i, (g, c)) in got.iter().zip(colours.iter()).enumerate() {
        let want = cpu_u8(*c);
        let mut d = 0i32;
        for ch in 0..3 {
            d = d.max(((g[ch] * 255.0).round() as i32 - want[ch] as i32).abs());
        }
        if d > 0 {
            differing += 1;
        }
        if d > worst {
            worst = d;
            worst_at = i;
        }
    }
    let c = colours[worst_at];
    eprintln!(
        "{label}: worst {worst} of 255 over {} colours ({differing} differ); \
         worst at [{:.5}, {:.5}, {:.5}]",
        colours.len(),
        c[0],
        c[1],
        c[2],
    );
    assert!(
        worst <= tolerance,
        "{label}: the shader is out by {worst} code values at [{:.5}, {:.5}, {:.5}] \
         — gpu {:?} vs cpu {:?}",
        c[0],
        c[1],
        c[2],
        got[worst_at].map(|v| (v * 255.0).round() as i32),
        cpu_u8(c),
    );
}

/// Deterministic noise across eighteen stops and the whole saturation
/// range — the input shape a 3D LUT could not handle, so the one that
/// matters most here.
fn noise(n: usize) -> Vec<[f32; 3]> {
    let mut st = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        st ^= st << 13;
        st ^= st >> 7;
        st ^= st << 17;
        (st >> 11) as f32 / (1u64 << 53) as f32
    };
    (0..n)
        .map(|_| {
            let base = 2.0f32.powf(next() * 18.0 - 12.0);
            [base * (0.2 + 1.6 * next()), base * (0.2 + 1.6 * next()), base * (0.2 + 1.6 * next())]
        })
        .collect()
}

#[test]
fn aces_rec709_matches_the_cpu_on_noise() {
    compare("noise", &noise(8192), 1);
}

/// Neutrals, where the glow and the red modifier must do nothing and
/// the hue is undefined. A sign slip in the sweeteners shows here as a
/// cast on grey, which is the most visible failure there is.
#[test]
fn aces_rec709_matches_the_cpu_on_neutrals() {
    let greys: Vec<[f32; 3]> =
        (0..256).map(|i| { let v = 2.0f32.powf(i as f32 / 255.0 * 18.0 - 12.0); [v, v, v] }).collect();
    compare("neutrals", &greys, 1);
}

/// The pure primaries and secondaries at a range of exposures. These
/// are where the red modifier's hue bell peaks and where AP1 gamut
/// clamping bites, so a swapped basis column or a missing clamp lands
/// here rather than in the noise.
#[test]
fn aces_rec709_matches_the_cpu_on_primaries() {
    let mut cases = Vec::new();
    for stops in [-6.0f32, -3.0, -1.0, 0.0, 1.0, 3.0, 6.0] {
        let k = 2.0f32.powf(stops) * 0.18;
        for axis in [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 1.0],
            [1.0, 0.0, 1.0],
        ] {
            cases.push([axis[0] * k, axis[1] * k, axis[2] * k]);
        }
    }
    compare("primaries", &cases, 1);
}

/// The glow's three branches and the red modifier's bell, swept
/// deliberately rather than hoped for.
///
/// `glow_fwd` is piecewise on `yc_in` about `2/3 * glow_mid` and
/// `2 * glow_mid` with `glow_mid = 0.08`, so the interesting band is
/// roughly 0.05 to 0.16 — deep shadows, which is also where a
/// saturation-gated boost is least forgiving. The red bell is 135°
/// wide about pure red.
#[test]
fn aces_rec709_matches_the_cpu_through_the_awkward_branches() {
    let mut cases = Vec::new();
    // Straddle both glow knees at several saturations.
    for i in 0..64 {
        let yc = 0.02 + i as f32 / 63.0 * 0.20;
        for sat in [0.0f32, 0.2, 0.39, 0.41, 0.5, 0.59, 0.61, 0.9] {
            cases.push([yc * (1.0 + sat), yc * (1.0 - sat * 0.5), yc * (1.0 - sat)]);
        }
    }
    // Sweep hue right around the red bell's edges at 0 +/- 67.5.
    for i in 0..180 {
        let deg = i as f32 * 2.0;
        let r = deg.to_radians();
        let v = 0.18;
        cases.push([
            v * (1.0 + 0.9 * r.cos()),
            v * (1.0 + 0.9 * (r - 2.094).cos()),
            v * (1.0 + 0.9 * (r + 2.094).cos()),
        ]);
    }
    compare("glow and red-modifier branches", &cases, 1);
}

/// Values that should clip, and values that should not quite. The
/// transform ends in a clamp and a gamma, so the ends of the range are
/// where an off-by-one in the quantiser or a missing clamp shows.
#[test]
fn aces_rec709_matches_the_cpu_at_the_extremes() {
    let mut cases = vec![
        [0.0, 0.0, 0.0],
        [1e-8, 1e-8, 1e-8],
        [1.0, 1.0, 1.0],
        [16.0, 16.0, 16.0],
        [65504.0, 65504.0, 65504.0],
        // Negatives, which a wide-gamut colour arrives as.
        [-0.1, 0.2, 0.3],
        [0.4, -0.05, -0.02],
    ];
    for stops in -14..14 {
        let v = 2.0f32.powf(stops as f32);
        cases.push([v, v * 0.5, v * 0.25]);
    }
    compare("extremes", &cases, 1);
}
