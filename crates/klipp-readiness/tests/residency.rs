//! Residency classification is an obligation, independent of image parity.
use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_readiness::residency::{self, Residency};
use std::collections::BTreeSet;

fn gpu() -> Option<GpuEffectsPipeline> {
    let gpu = GpuEffectsPipeline::new();
    if gpu.is_none() {
        assert!(
            std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"),
            "GPU required for residency coverage"
        );
        eprintln!("UNVERIFIED: GPU residency probes need a GPU");
    }
    gpu
}

#[test]
fn title_authoring_fields_cannot_be_added_without_residency_review() {
    let src = include_str!("../../klipp-timeline/src/clip_kind.rs");
    for owner in [
        "TextBox",
        "LayerFx",
        "TitleObject",
        "ObjectTransform",
        "Shape",
        "ImageLayer",
    ] {
        let marker = format!("pub struct {owner} {{");
        let body = src
            .split_once(&marker)
            .unwrap()
            .1
            .split_once("\n}")
            .unwrap()
            .0;
        let declared: BTreeSet<_> = body
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("pub ")?
                    .split_once(':')
                    .map(|(name, _)| name)
            })
            .collect();
        let mut classified = BTreeSet::new();
        for family in residency::TITLE_FAMILIES
            .iter()
            .filter(|f| f.owner == owner)
        {
            assert!(!family.capability.reason.is_empty());
            assert!(
                family.capability.followup.is_some(),
                "nonresident/conditional title family needs an explicit exception"
            );
            for field in family.fields {
                assert!(classified.insert(*field), "duplicate {owner}.{field}");
            }
        }
        assert_eq!(
            declared, classified,
            "review added/removed fields of {owner}"
        );
    }
}

#[test]
fn built_in_catalogue_paths_are_explicit_and_gpu_probes_do_not_download() {
    let Some(mut g) = gpu() else { return };
    let (w, h) = (32, 16);
    let pixels: Vec<f32> = (0..w * h)
        .flat_map(|i| [0.1 + (i % 17) as f32 / 20.0, 0.3, 0.5, 1.0])
        .collect();
    for ty in klipp_effects::registry::effect_types() {
        let mut fx = (ty.sample)();
        // Zero-radius blur is legitimately identity with no uniform slots.
        // Exercise its active kernel instead of classifying that as delegation.
        if let klipp_effects::BuiltinEffect::GaussianBlur { radius } = &mut fx {
            *radius = 2.5;
        }
        let cap = residency::effect(&fx);
        assert!(!cap.reason.is_empty());
        println!("effect {}: {:?}: {}", ty.desc.id, cap.residency, cap.reason);
        if cap.residency != Residency::Gpu {
            assert!(
                cap.followup.is_some(),
                "{} needs an exception issue",
                ty.desc.id
            );
            continue;
        }
        assert!(
            klipp_effects::gpu::slots_needed(&fx).is_some_and(|slots| slots > 0),
            "{} cannot claim an empty/delegated recorder as a GPU kernel",
            ty.desc.id
        );
        assert!(g.stack_begin(w, h, 1));
        assert!(g.stack_layer_from_linear(0, &pixels));
        assert!(g.stack_blend(0, 1.0, 0));
        assert!(g.stack_finish_into_chain(true));
        g.stack_end();
        let before = g.readback_stats();
        g.begin_chain();
        let result = g.chain_run_effects(&[fx], true, w, h);
        g.submit_chain();
        assert!(result.complete, "{} declined", ty.desc.id);
        assert_eq!(result.breaks, 0, "{} broke GPU chain", ty.desc.id);
        assert_eq!(
            before,
            g.readback_stats(),
            "{} downloaded an intermediate picture",
            ty.desc.id
        );
        // Final verification download is deliberately outside the measured operation.
        let frame = g.download_chain_output_linear(result.in_a, (w * h) as usize);
        assert_eq!(frame.len(), pixels.len());
        assert!(
            frame.iter().all(|x| x.is_finite()),
            "{} produced nonfinite pixels",
            ty.desc.id
        );
    }
}

#[test]
fn equalize_is_a_measured_round_trip_not_a_gpu_resident_claim() {
    let Some(mut g) = gpu() else { return };
    let (w, h) = (32, 16);
    let fx = klipp_effects::BuiltinEffect::Equalize { strength: 0.7 };
    assert_eq!(residency::effect(&fx).residency, Residency::RoundTrip);
    assert!(g.stack_begin(w, h, 1));
    let pixels: Vec<f32> = (0..w * h)
        .flat_map(|i| [(i % 16) as f32 / 16.0, 0.2, 0.3, 1.0])
        .collect();
    assert!(g.stack_layer_from_linear(0, &pixels));
    assert!(g.stack_blend(0, 1.0, 0));
    assert!(g.stack_finish_into_chain(true));
    g.stack_end();
    let before = g.readback_stats();
    g.begin_chain();
    let result = g.chain_run_effects(&[fx], true, w, h);
    g.submit_chain();
    assert!(result.complete && result.breaks > 0);
    let after = g.readback_stats();
    assert!(after.count > before.count && after.bytes > before.bytes);
}

#[test]
fn transition_catalogue_runs_without_intermediate_readbacks() {
    let Some(mut g) = gpu() else { return };
    let (w, h) = (32, 16);
    for spec in klipp_core::transition_registry::TRANSITION_SPECS {
        // A smooth cut morphs held source frames, uploaded and read back
        // (#2219): a measured round trip, not one of the stack's kinds.
        if spec.kind == klipp_core::TransitionKind::SmoothCut {
            assert_eq!(residency::transition(spec.kind).residency, Residency::RoundTrip);
            continue;
        }
        assert_eq!(residency::transition(spec.kind).residency, Residency::Gpu);
        assert!(g.stack_begin(w, h, 3));
        assert!(g.stack_layer_from_linear(0, &[0.1, 0.3, 0.6, 1.0].repeat((w * h) as usize)));
        assert!(g.stack_layer_from_linear(1, &[0.8, 0.2, 0.1, 1.0].repeat((w * h) as usize)));
        let p = spec.default_params;
        let before = g.readback_stats();
        assert!(
            g.stack_transition(
                0,
                1,
                2,
                spec.kind_id as u32,
                0.43,
                [p.param0, p.param1, p.param2, p.param3],
                p.color
            ),
            "{} declined",
            spec.string_key
        );
        assert!(g.stack_blend(2, 1.0, 0));
        assert!(g.stack_finish_into_chain(true));
        g.stack_end();
        assert_eq!(before, g.readback_stats(), "{} read back", spec.string_key);
        let frame = g.stack_download_linear();
        assert!(
            frame.iter().all(|x| x.is_finite()),
            "{} produced nonfinite pixels",
            spec.string_key
        );
    }
}

#[test]
fn static_title_cache_and_time_varying_cpu_route_are_distinct() {
    use klipp_timeline::{TextBox, TitleData};
    use klipp_title::{TitleFrame, TitleRenderer};
    let mut renderer = TitleRenderer::new();
    let mut title = TitleData::single_text(TextBox {
        text: "Coverage".into(),
        ..Default::default()
    });
    let TitleFrame::Placed { sprite: first, .. } =
        renderer.frame_with_window(&title, 320, 180, None, 0.5, Some(4.0))
    else {
        panic!("static title is cacheable")
    };
    let TitleFrame::Placed { sprite: next, .. } =
        renderer.frame_with_window(&title, 320, 180, None, 1.0, Some(4.0))
    else {
        panic!("static title is cacheable")
    };
    assert_eq!(first.id, next.id);
    title.primary_text_mut().unwrap().glitch_intensity = 0.7;
    assert!(
        matches!(
            renderer.frame_with_window(&title, 320, 180, None, 1.0, Some(4.0)),
            TitleFrame::Rendered { .. }
        ),
        "update the CPU exception when glitch becomes resident"
    );
}

#[test]
fn title_mask_gpu_callback_is_a_round_trip_only_on_cache_miss() {
    use klipp_timeline::{LayerFx, TextBox, TextGlow, TitleData};
    let Some(mut g) = gpu() else { return };
    let title = TitleData::single_text_fx(
        TextBox {
            text: "Mask coverage".into(),
            font_size: 320.0,
            ..Default::default()
        },
        LayerFx {
            glows: vec![TextGlow {
                enabled: true,
                color: [255, 120, 30, 255],
                radius: 80.0,
                intensity: 0.7,
                radius_keyframes: vec![],
                intensity_keyframes: vec![],
            }],
            ..Default::default()
        },
    );
    let mut renderer = klipp_title::TitleRenderer::new();
    let layout = renderer.render_layout(&title, 1920, 1080);
    assert!(
        layout.mask_w * layout.mask_h > 250_000,
        "fixture must reach the GPU mask path"
    );
    let before = g.readback_stats();
    {
        let mut masks = klipp_compose::synth::GpuAlphaMaskAdapter(&mut g);
        renderer.frame_with_window(&title, 1920, 1080, Some(&mut masks), 1.0, Some(4.0));
    }
    let cold = g.readback_stats();
    assert!(
        cold.count > before.count && cold.bytes > before.bytes,
        "update #2063 exception when masks become resident"
    );
    {
        let mut masks = klipp_compose::synth::GpuAlphaMaskAdapter(&mut g);
        renderer.frame_with_window(&title, 1920, 1080, Some(&mut masks), 2.0, Some(4.0));
    }
    assert_eq!(
        cold,
        g.readback_stats(),
        "a warm static style must not redownload its mask"
    );
}

#[test]
fn native_generators_distinguish_gpu_cached_cpu_and_per_frame_cpu() {
    use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame};
    use klipp_core::{GeneratorData as G, RationalTime, Resolution};
    use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};
    use std::sync::atomic::Ordering;
    struct NoMedia;
    impl FrameSource for NoMedia {
        fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
            None
        }
    }
    // Other probes in this binary do not invoke native generator rasterization.
    let draws = || klipp_compose::render::CPU_GENERATOR_DRAWS.load(Ordering::Relaxed);
    for (data, expected) in [
        (
            G::LivingGradient {
                a: [10, 20, 30, 255],
                b: [80, 90, 100, 255],
                angle_deg: 20.0,
                drift_deg_per_sec: 1.0,
                glow: 0.1,
                grain: klipp_core::DEFAULT_GENERATOR_GRAIN,
            },
            Residency::Gpu,
        ),
        (
            G::SolidColor {
                color: [20, 80, 100, 255],
            },
            Residency::CpuCached,
        ),
        // Grain, a countdown and a timer draw on the GPU too (#2067): no
        // CPU draw over three frames, where Grain made three.
        (
            G::Grain {
                color: [20, 80, 100, 255],
                amount: 0.2,
                size_px: 1.0,
                speed: 24.0,
            },
            Residency::Gpu,
        ),
        (G::Countdown, Residency::Gpu),
        (
            G::Timer {
                mode: Default::default(),
                initial_seconds: 10.0,
                format: Default::default(),
                text_color: [255; 4],
                background_color: [0, 0, 0, 255],
            },
            Residency::Gpu,
        ),
    ] {
        assert_eq!(residency::generator(&data).residency, expected);
        let mut seq = Sequence::new_default();
        seq.resolution = Resolution {
            width: 128,
            height: 72,
        };
        let tc = |s| RationalTime::from_secs_f64(s, seq.fps.rate());
        let mut track = Track::new("Source", TrackKind::Video);
        track.clips.push(Clip {
            kind: ClipKind::Generator(data),
            source_in: tc(0.0),
            source_out: tc(4.0),
            ..Clip::gap(tc(0.0), tc(4.0))
        });
        seq.tracks = vec![track];
        let mut r = Renderer::new(
            klipp_plugin::OfxHost::new(),
            klipp_title::TitleRenderer::new(),
        );
        r.set_gpu_enabled(true);
        let before = draws();
        for secs in [0.0, 0.5, 1.0] {
            let at = RationalTime::from_secs_f64(secs, seq.fps.rate());
            let Some((_, stats)) = r.render_display_u8_on_gpu(
                &seq,
                at,
                128,
                72,
                &mut NoMedia,
                RenderOptions::default(),
            ) else {
                assert!(
                    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"),
                    "GPU stack required"
                );
                eprintln!("UNVERIFIED: native-generator GPU stack unavailable");
                return;
            };
            assert_eq!(stats.gpu_generators, u32::from(expected == Residency::Gpu));
        }
        let expected_draws = match expected {
            Residency::Gpu => 0,
            Residency::CpuCached => 1,
            Residency::CpuPerFrame => 3,
            _ => unreachable!(),
        };
        assert_eq!(draws() - before, expected_draws, "{expected:?}");
    }
}

#[test]
fn title_motion_catalogues_have_explicit_composition_conditions() {
    for kind in klipp_core::title_motion::MotionKind::ALL {
        let cap = residency::motion(kind);
        assert!(cap.followup.is_some() && !cap.reason.is_empty());
    }
    for value in klipp_core::title_motion::TitleLoop::catalogue() {
        let cap = residency::title_loop(&value);
        assert!(cap.followup.is_some() && !cap.reason.is_empty());
    }
    for family in residency::TITLE_FAMILIES {
        println!(
            "title {} {:?}: {:?}: {}",
            family.owner, family.fields, family.capability.residency, family.capability.reason
        );
    }
}
