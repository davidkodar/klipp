//! Shader compilation and pixel parity for resident rounded cards and spills.
use klipp_effects::{
    apply_effects, gpu::GpuEffectsPipeline, has_frei0r, BuiltinEffect as E,
};

fn pipeline() -> Option<GpuEffectsPipeline> {
    let adapters = klipp_effects::gpu_context::instance().enumerate_adapters(klipp_effects::gpu_context::backends());
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
    assert!(!has_frei0r(effects));
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
        // Three active casts traverse both scratch-buffer directions;
        // an invisible cast in the middle must not break the run or
        // change which original silhouette the later casts sample.
        vec![
            shadow.clone(),
            E::OuterGlow {
                radius: 0.1,
                intensity: 0.0,
                color: [255, 255, 255],
            },
            glow.clone(),
            E::DropShadow {
                offset_x: 6.0 / w as f32,
                offset_y: -4.0 / w as f32,
                blur: 2.0 / w as f32,
                color: [80, 140, 220],
                opacity: 0.6,
            },
        ],
    ] {
        compare(&mut gpu, &p, w, h, &effects);
    }
    // Glows wide enough that each box is slid along a whole row or
    // column from one seed, past the frame's edges at the widest
    // (#3176).
    for radius in [0.5, 2.0, 50.0] {
        compare(&mut gpu, &p, w, h, &[E::OuterGlow { radius, intensity: 0.8, color: [250, 150, 50] }]);
        compare(&mut gpu, &p, w, h, &[shadow.clone(), E::OuterGlow { radius, intensity: 0.8, color: [250, 150, 50] }]);
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
    // The casts are worked only near the layer (#2107): a small card in
    // a large frame, one by the frame's edge (its glow's reach cut by the
    // edge, its shadow thrown out of the frame), and a layer with nothing
    // in it, where the spills lay nothing but their colour at no alpha.
    let (bw, bh) = (161, 97);
    for (x0, y0) in [(70, 40), (0, 3), (bw - 12, bh - 9)] {
        let mut small = vec![0.0; (bw * bh * 4) as usize];
        for y in y0..(y0 + 9).min(bh) {
            for x in x0..(x0 + 12).min(bw) {
                let i = ((y * bw + x) * 4) as usize;
                small[i..i + 4].copy_from_slice(&[0.3, 0.8, 0.5, if (x + y) % 3 == 0 { 0.5 } else { 1.0 }]);
            }
        }
        let glow = E::OuterGlow { radius: 4.0 / bw as f32, intensity: 0.7, color: [250, 150, 50] };
        let shadow = E::DropShadow { offset_x: 9.0 / bw as f32, offset_y: 6.0 / bw as f32, blur: 2.5 / bw as f32, color: [20, 30, 50], opacity: 0.8 };
        compare(&mut gpu, &small, bw, bh, std::slice::from_ref(&glow));
        compare(&mut gpu, &small, bw, bh, &[shadow.clone(), glow.clone()]);
        compare(&mut gpu, &small, bw, bh, &[E::RoundedCorners { radius: 0.3, feather: 0.0 }, glow, shadow]);
    }
    let nothing = vec![0.0; (bw * bh * 4) as usize];
    compare(&mut gpu, &nothing, bw, bh, &[E::OuterGlow { radius: 4.0 / bw as f32, intensity: 0.7, color: [250, 150, 50] }]);
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
    gpu.begin_chain();
    let run = gpu.chain_run_effects(&effects, true, w, h);
    gpu.submit_chain();
    assert!(run.complete);
    assert!(gpu.stack_layer_from_chain(0, run.in_a));
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

/// An upright picture in a wide frame, the shape Fill Bars is for:
/// colour that changes across it and down it, so a fill sampled in the
/// wrong place shows.
fn upright(w: u32, h: u32) -> Vec<f32> {
    let mut p = vec![0.0; (w * h * 4) as usize];
    let (x0, x1) = (w * 2 / 5, w * 3 / 5);
    for y in 0..h {
        for x in x0..x1 {
            let i = ((y * w + x) * 4) as usize;
            let (u, v) = ((x - x0) as f32 / (x1 - x0) as f32, y as f32 / h as f32);
            p[i..i + 4].copy_from_slice(&[0.2 + u, 0.9 - v * 0.7, 0.3 + u * v, 1.0]);
        }
    }
    p
}

/// #680: the bars filled on the GPU as on the CPU, at a size built
/// whole and at one built an eighth the size; with a picture that fills
/// the frame, and with a cropped, rounded card.
#[test]
fn fill_bars_matches_cpu_on_gpu() {
    let Some(mut gpu) = pipeline() else { return };
    let fill = |blur: f32, zoom: f32, darken: f32| E::FillBars { blur, zoom, darken };
    for (w, h) in [(65u32, 49u32), (160, 90), (203, 115)] {
        let p = upright(w, h);
        for effects in [
            vec![fill(0.0, 1.0, 0.0)],
            vec![fill(0.04, 1.0, 0.3)],
            vec![fill(0.1, 1.6, 0.6)],
            vec![E::RoundedCorners { radius: 0.2, feather: 0.0 }, fill(0.03, 1.2, 0.25)],
        ] {
            compare(&mut gpu, &p, w, h, &effects);
        }
        compare(&mut gpu, &card(w, h), w, h, &[fill(0.05, 1.0, 0.25)]);
        let full: Vec<f32> = (0..w * h).flat_map(|_| [0.4, 0.5, 0.6, 1.0]).collect();
        compare(&mut gpu, &full, w, h, &[fill(0.05, 1.0, 0.25)]);
    }
}
