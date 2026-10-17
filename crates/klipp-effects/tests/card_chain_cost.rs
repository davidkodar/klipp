//! What each pass of a placed card's chain costs at 4K on the GPU
//! (#2107): the card's corners, its placement flat and tilted, small
//! and whole, and its glow and shadow after it, each as the GPU's own
//! time (recorded, submitted and waited out, nothing read back) over a
//! one-pass chain's. Run on demand:
//! `cargo test -p klipp-effects --test card_chain_cost -- --ignored --nocapture`
use klipp_effects::{gpu::GpuEffectsPipeline, BuiltinEffect as E};

const W: u32 = 3840;
const H: u32 = 2160;

fn place(scale: f32, tilt: f32) -> E {
    E::Transform {
        position_x: 0.0, position_y: 0.0, scale_x: scale, scale_y: scale, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5,
        rotation_x: 0.0, rotation_y: tilt, perspective: if tilt != 0.0 { 1.0 } else { 0.0 }, focal_ref: None,
    }
}

#[test]
#[ignore = "4K performance measurement"]
fn measure_card_chain_passes() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("no GPU");
        return;
    };
    let card: Vec<f32> = (0..W * H).flat_map(|i| [(i % 97) as f32 / 97.0, 0.4, 0.6, 1.0]).collect();
    let corners = E::RoundedCorners { radius: 0.055, feather: 0.002 };
    let glow = E::OuterGlow { radius: 0.018, intensity: 0.3, color: [140, 150, 255] };
    let shadow = E::DropShadow { offset_x: 0.0, offset_y: 0.01, blur: 0.02, color: [0, 0, 0], opacity: 0.5 };
    // The GPU's time alone: the card uploaded once, each run's chain
    // recorded, submitted and waited out, nothing read back.
    gpu.ensure_chain_buffers(u64::from(W) * u64::from(H) * 16);
    gpu.upload_chain_input_linear(&card);
    let mut time = |fx: &[E]| {
        let mut ms: Vec<f64> = (0..15)
            .map(|_| {
                let _ = gpu.wait_for_submitted_work();
                let t = std::time::Instant::now();
                gpu.begin_chain();
                let _ = gpu.chain_run_effects(fx, true, W, H);
                gpu.submit_chain();
                let _ = gpu.wait_for_submitted_work();
                t.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        // The least disturbed run: other work on the machine only adds.
        ms.sort_by(f64::total_cmp);
        ms[0]
    };
    let base = time(&[E::Opacity { value: 0.999 }]);
    eprintln!("CARD baseline (one pass, GPU time): {base:.2} ms");
    for (name, fx) in [
        ("corners", vec![corners.clone()]),
        ("place 1.0 flat", vec![place(1.0, 0.0)]),
        ("place 0.46 flat", vec![place(0.46, 0.0)]),
        ("place 0.3 flat", vec![place(0.3, 0.0)]),
        ("place 0.46 tilt 15", vec![place(0.46, 15.0)]),
        ("place 1.0 tilt 15", vec![place(1.0, 15.0)]),
        ("glow+shadow (whole frame)", vec![glow.clone(), shadow.clone()]),
        ("0.46 flat, glow+shadow", vec![place(0.46, 0.0), glow.clone(), shadow.clone()]),
        ("0.46 tilt 15, glow+shadow", vec![place(0.46, 15.0), glow.clone(), shadow.clone()]),
        ("0.46 glow only", vec![place(0.46, 0.0), glow.clone()]),
        ("0.46 shadow only", vec![place(0.46, 0.0), shadow.clone()]),
        ("0.46 glow tiny radius", vec![place(0.46, 0.0), E::OuterGlow { radius: 0.0005, intensity: 0.3, color: [140, 150, 255] }]),
        ("0.46 glow 4x radius", vec![place(0.46, 0.0), E::OuterGlow { radius: 0.072, intensity: 0.3, color: [140, 150, 255] }]),
        ("film card flat", vec![corners.clone(), place(0.46, 0.0), glow.clone(), shadow.clone()]),
        ("film card tilted", vec![corners.clone(), place(0.46, 15.0), glow.clone(), shadow.clone()]),
    ] {
        eprintln!("CARD {name:>28}: {:6.2} ms over the baseline", time(&fx) - base);
    }
}


/// A frame of the stress project's four flying cards at 4K, as GPU work
/// (#2107): what the decode threads hand the queue for each stream's
/// frame (its NV12 planes uploaded and converted to RGBA), and what the
/// stack records for each layer (its picture seeded into linear light,
/// its chain, its copy into the stack and its blend). Each part's GPU
/// time, waited out, the least disturbed of 15 runs.
/// `cargo test -p klipp-effects --test card_chain_cost frame_budget -- --ignored --nocapture`
#[test]
#[ignore = "4K performance measurement"]
fn frame_budget_of_four_cards() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("no GPU");
        return;
    };
    let best = |mut run: Box<dyn FnMut() -> f64 + '_>| -> f64 {
        let _ = run();
        let mut ms: Vec<f64> = (0..15).map(|_| run()).collect();
        ms.sort_by(f64::total_cmp);
        ms[0]
    };
    // The decode side: one stream's frame, NV12 to RGBA on the GPU.
    if let Some(mut nv12) = klipp_effects::nv12_gpu::GpuNv12Converter::new() {
        let y = vec![120u8; (W * H) as usize];
        let uv = vec![128u8; (W * H / 2) as usize];
        let device = nv12.device();
        let ms = best(Box::new(|| {
            let t = std::time::Instant::now();
            let out = nv12.convert_nv12_on_gpu(&y, &uv, W, H, W, W, W, H, 0, [0, 0, 0, 0], klipp_effects::color_wgsl::IDENTITY_PRIMARIES_MATRIX_PACKED);
            device.poll(wgpu::Maintain::Wait);
            drop(out);
            t.elapsed().as_secs_f64() * 1000.0
        }));
        eprintln!("FRAME one stream's NV12 frame uploaded and converted: {ms:.2} ms (x4 streams: {:.2} ms)", ms * 4.0);
    }
    let corners = E::RoundedCorners { radius: 0.055, feather: 0.002 };
    let glow = E::OuterGlow { radius: 0.018, intensity: 0.3, color: [140, 150, 255] };
    let shadow = E::DropShadow { offset_x: 0.0, offset_y: 0.01, blur: 0.02, color: [0, 0, 0], opacity: 0.5 };
    let card = [corners, place(0.46, 15.0), glow, shadow];
    // The decoded picture, RGBA on the GPU as the decode side leaves it.
    let device = gpu.device();
    let rgba = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("decoded"),
        size: u64::from(W) * u64::from(H) * 4,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let seeded = best(Box::new(|| {
        let _ = gpu.wait_for_submitted_work();
        let t = std::time::Instant::now();
        gpu.bridge_u8_to_linear_into_chain(&rgba, W, H);
        gpu.flush();
        let _ = gpu.wait_for_submitted_work();
        t.elapsed().as_secs_f64() * 1000.0
    }));
    eprintln!("FRAME one layer's picture seeded into linear light: {seeded:.2} ms");
    let frame = best(Box::new(|| {
        let _ = gpu.wait_for_submitted_work();
        let t = std::time::Instant::now();
        gpu.stack_begin(W, H, 4);
        for i in 0..4 {
            gpu.bridge_u8_to_linear_into_chain(&rgba, W, H);
            gpu.begin_chain();
            let run = gpu.chain_run_effects(&card, true, W, H);
            gpu.submit_chain();
            gpu.stack_layer_from_chain(i, run.in_a);
        }
        for i in 0..4 {
            gpu.stack_blend(i, 1.0, 0);
        }
        gpu.stack_end();
        let _ = gpu.wait_for_submitted_work();
        t.elapsed().as_secs_f64() * 1000.0
    }));
    eprintln!("FRAME four layers: seed, film card chain, into the stack, blended: {frame:.2} ms");
}
