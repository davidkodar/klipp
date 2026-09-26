//! A 3D LUT node looks the same on the CPU fallback as on the GPU.
//!
//! The GPU LUT shader looks a `.cube` up on the sRGB encoding of the
//! linear frame, as the looks are built. The CPU fallback used to look
//! it up on the linear values, which put everything below mid-grey into
//! the first cells of a 13³ LUT: the same look came out in other colours
//! and banded (#414).

use klipp_effects::gpu::GpuEffectsPipeline;

const W: u32 = 128;
const H: u32 = 64;

/// A smooth ramp from black to white across, hue turning down the frame.
fn frame() -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let t = x as f32 / (W - 1) as f32;
            let u = y as f32 / (H - 1) as f32;
            v.extend_from_slice(&[t * t, t * (0.6 + 0.4 * u), t * (1.0 - 0.5 * u), 1.0]);
        }
    }
    v
}

#[test]
fn a_bundled_look_is_the_same_on_the_cpu_and_the_gpu() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/luts/fuji-xtrans/fuji_xtrans_iii_classic_chrome.cube");
    let lut = klipp_color::load_cube(&path).expect("the bundled look loads");
    for (opacity, tetrahedral) in [(1.0, true), (0.4, true), (1.0, false)] {
        let input = frame();
        let on_gpu = gpu.apply_3d_lut_gpu(&input, W, H, &lut.data, lut.size as u32, opacity, tetrahedral);
        let mut on_cpu = input.clone();
        klipp_color::apply_3d_lut_linear_ws(&mut on_cpu, &lut, opacity, klipp_core::WorkingColorspace::Rec709Linear, tetrahedral);
        let worst = on_gpu
            .chunks_exact(4)
            .zip(on_cpu.chunks_exact(4))
            .flat_map(|(g, c)| (0..3).map(move |i| (klipp_color::transfer::linear_to_srgb(g[i]) - klipp_color::transfer::linear_to_srgb(c[i])).abs()))
            .fold(0.0f32, f32::max);
        assert!(worst * 255.0 < 1.0, "opacity {opacity} tetrahedral {tetrahedral}: CPU and GPU differ by {:.2} display levels", worst * 255.0);
    }
}
