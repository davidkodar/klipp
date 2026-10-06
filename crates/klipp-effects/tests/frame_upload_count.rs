//! Every whole frame the pipeline uploads from the CPU is counted in
//! `frame_upload_count`, which `ChainStats::uploads` is read from (#1173):
//! the per-node grade entry points and a composite's port-1 picture
//! included. Seeds the executor uploads before a run starts are not.
//!
//! A missing GPU is reported as unverified; with `KLIPP_TEST_REQUIRE_GPU=1`
//! it fails instead.

use klipp_effects::gpu::GpuEffectsPipeline;

const W: u32 = 16;
const H: u32 = 8;
const ID: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

#[test]
fn each_per_node_entry_point_counts_one_frame_up() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required but no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    };
    let px = vec![0.4f32; (W * H * 4) as usize];
    let ups = |what: &str, gpu: &mut GpuEffectsPipeline, f: &dyn Fn(&mut GpuEffectsPipeline)| {
        let before = gpu.frame_upload_count();
        f(gpu);
        assert_eq!(gpu.frame_upload_count() - before, 1, "{what} uploads one frame");
    };
    let z = (0.0, 0.0, 0.0);
    ups("apply_color_grade", &mut gpu, &|g| {
        g.apply_color_grade(&px, W, H, 0.2, 0.0, 0.18, 1.0, ID, 0.0, 0.0, 0.0, 0.0, z, z, z, z, None, Default::default(), 0.0, 1.0);
    });
    ups("apply_color_balance_gpu", &mut gpu, &|g| {
        g.apply_color_balance_gpu(&px, W, H, ID, 10.0, 1.0);
    });
    ups("apply_wheel_sat_bri_gpu", &mut gpu, &|g| {
        g.apply_wheel_sat_bri_gpu(&px, W, H, 0.1, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    });
    let lut: [f32; 1024] = std::array::from_fn(|i| (i % 256) as f32 / 255.0);
    ups("apply_rgb_curves_gpu", &mut gpu, &|g| {
        g.apply_rgb_curves_gpu(&px, W, H, &lut, false, 1.0);
    });
    ups("upload_composite_src_linear", &mut gpu, &|g| {
        assert!(g.upload_composite_src_linear(&px, W, H));
    });
}
