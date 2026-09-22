//! Shader compilation and pixel parity for resident rounded cards and spills.
use klipp_effects::{
    apply_effects, chain_supports_gpu, gpu::GpuEffectsPipeline, BuiltinEffect as E,
};

fn pipeline() -> Option<GpuEffectsPipeline> {
    let adapters = wgpu::Instance::default().enumerate_adapters(wgpu::Backends::all());
    if adapters.is_empty() {
        eprintln!("skipping layer parity: no GPU adapter");
        return None;
    }
    for adapter in adapters {
        eprintln!("layer parity adapter: {:?}", adapter.get_info());
    }
    Some(GpuEffectsPipeline::new().expect("adapter exists: layer shaders must compile"))
}

fn card(w: u32, h: u32) -> Vec<f32> {
    let mut p = vec![0.0; (w * h * 4) as usize];
    for y in 5..h - 7 {
        for x in 7..w - 11 {
            let i = ((y * w + x) * 4) as usize;
            p[i..i + 4].copy_from_slice(&[1.7, 0.25, 0.06, if x % 5 == 0 { 0.35 } else { 1.0 }]);
        }
    }
    p
}

fn compare(gpu: &mut GpuEffectsPipeline, p: &[f32], w: u32, h: u32, effects: &[E]) {
    assert!(chain_supports_gpu(effects));
    let expected = apply_effects(effects, p.to_vec(), w, h);
    let got = gpu.apply_effects(effects, p, w, h);
    let (i, error) = expected
        .iter()
        .zip(&got)
        .enumerate()
        .map(|(i, (a, b))| (i, (a - b).abs()))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    assert!(
        error < 2e-4,
        "{effects:?}: channel {i}: CPU {}, GPU {}, error {error}",
        expected[i],
        got[i]
    );
}

#[test]
fn rounded_and_grouped_spills_match_cpu_on_gpu() {
    let Some(mut gpu) = pipeline() else { return };
    let (w, h) = (65, 49); // partial workgroup and non-square cropped picture
    let p = card(w, h);
    let shadow = E::DropShadow {
        offset_x: -2.5 / w as f32,
        offset_y: 8.5 / w as f32,
        blur: 1.0 / w as f32,
        color: [20, 30, 50],
        opacity: 0.85,
    };
    let glow = E::OuterGlow {
        radius: 3.0 / w as f32,
        intensity: 0.5,
        color: [250, 150, 50],
    };
    for radius in [0.0, 0.01, 0.25, 0.7] {
        for feather in [0.0, 0.08] {
            compare(&mut gpu, &p, w, h, &[E::RoundedCorners { radius, feather }]);
        }
    }
    for effects in [
        vec![shadow.clone()],
        vec![glow.clone()],
        vec![shadow.clone(), glow.clone()],
        vec![glow.clone(), shadow.clone()],
        vec![
            E::RoundedCorners {
                radius: 0.25,
                feather: 0.0,
            },
            shadow.clone(),
            glow.clone(),
        ],
        vec![shadow.clone(), E::Opacity { value: 0.5 }, glow.clone()],
    ] {
        compare(&mut gpu, &p, w, h, &effects);
    }
    // Transparent RGB and alpha below the bbox threshold survive corners.
    let empty = vec![0.0005; p.len()];
    compare(
        &mut gpu,
        &empty,
        w,
        h,
        &[E::RoundedCorners {
            radius: 0.5,
            feather: 0.0,
        }],
    );
    // Border-clamped blur, offset clipping and opaque full-frame bounds.
    let full = vec![1.0; p.len()];
    compare(&mut gpu, &full, w, h, &[shadow.clone(), glow.clone()]);
    for (offset_x, blur, opacity) in [
        (0.0, 0.0, 1.0),
        (0.1, 0.0, 0.0),
        (0.1, 0.0, 0.7),
        (2.0, 0.05, 1.0),
    ] {
        compare(
            &mut gpu,
            &p,
            w,
            h,
            &[E::DropShadow {
                offset_x,
                offset_y: 0.0,
                blur,
                color: [40, 90, 160],
                opacity,
            }],
        );
    }
    // Reuse scratch storage after changing frame dimensions.
    compare(&mut gpu, &card(96, 80), 96, 80, &[shadow, glow]);
}

#[test]
fn treated_layer_stays_in_the_resident_stack() {
    let Some(mut gpu) = pipeline() else { return };
    let (w, h) = (64, 48);
    let p = card(w, h);
    let effects = [
        E::RoundedCorners {
            radius: 0.25,
            feather: 0.03,
        },
        E::DropShadow {
            offset_x: 0.06,
            offset_y: 0.1,
            blur: 0.025,
            color: [0, 0, 0],
            opacity: 0.8,
        },
        E::OuterGlow {
            radius: 0.06,
            intensity: 0.5,
            color: [255, 230, 160],
        },
    ];
    let expected = apply_effects(&effects, p.clone(), w, h);
    assert!(gpu.stack_begin(w, h, 1));
    gpu.upload_chain_input_linear(&p);
    let slot_a = gpu.run_effects_in_chain(&effects, w, h);
    assert!(gpu.stack_layer_from_chain(0, slot_a));
    assert!(gpu.stack_blend(0, 1.0, 0));
    assert!(gpu.stack_finish_into_chain(true));
    let got = gpu.stack_download_linear();
    let error = expected
        .iter()
        .zip(got)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max);
    assert!(error < 2e-4, "resident layer differs by {error}");
}

// Run explicitly when measuring the issue's 1080p workload; excluded
// from the normal parity gate because wall-clock timing is host-specific.
#[test]
#[ignore]
fn layer_1080p_timing() {
    let Some(mut gpu) = pipeline() else { return };
    let (w, h) = (1920, 1080);
    let p = card(w, h);
    let effects = [
        E::RoundedCorners {
            radius: 0.06,
            feather: 0.0,
        },
        E::DropShadow {
            offset_x: 0.012,
            offset_y: 0.025,
            blur: 0.012,
            color: [0, 0, 0],
            opacity: 0.75,
        },
        E::OuterGlow {
            radius: 0.02,
            intensity: 0.4,
            color: [255, 220, 160],
        },
    ];
    let start = std::time::Instant::now();
    let expected = apply_effects(&effects, p.clone(), w, h);
    let cpu = start.elapsed();
    let start = std::time::Instant::now();
    let got = gpu.apply_effects(&effects, &p, w, h);
    let gpu_time = start.elapsed();
    let error = expected
        .iter()
        .zip(got)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max);
    assert!(error < 2e-4, "1080p error {error}");
    eprintln!("1080p single-card corners+shadow+glow: CPU {cpu:?}; GPU incl upload/download {gpu_time:?}; max error {error}");
}
