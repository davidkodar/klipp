//! End-to-end GPU-first dispatch guard. All cases use the production renderer,
//! active values, multiple times and warm/cold sources; no media files required.
use klipp_compose::{DisplayDelivery, FrameSource, RenderOptions, Renderer, SourceFrame};
use klipp_core::{GeneratorData as G, RationalTime, Resolution};
use klipp_effects::{residency as diag, BuiltinEffect as E};
use klipp_readiness::residency::{self, Capability, Residency};
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, ClipKind, LayerFx, Sequence, TextBox, TitleData, Track, TrackKind};
use std::collections::BTreeMap;
struct SyntheticSources;
impl FrameSource for SyntheticSources {
    fn frame(&mut self, clip: &Clip, _: f64, width: u32, height: u32) -> Option<SourceFrame> {
        matches!(clip.kind, ClipKind::Media { .. }).then(|| SourceFrame {
            width,
            height,
            pts: None,
            pixels: klipp_compose::SourcePixels::Linear(std::sync::Arc::new(
                (0..width * height)
                    .flat_map(|i| {
                        [
                            0.05 + 0.7 * (i % width) as f32 / width as f32,
                            0.1 + 0.5 * (i / width) as f32 / height as f32,
                            0.2,
                            0.5 + 0.4 * (i % 11) as f32 / 11.0,
                        ]
                    })
                    .collect(),
            )),
        })
    }
}
fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 30)
}
fn still_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("klipp-policy-still-{}.png", std::process::id()))
}
struct StillFixture;
impl Drop for StillFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(still_path());
    }
}

fn generators() -> Vec<G> {
    vec![
        G::SolidColor {
            color: [20, 60, 90, 255],
        },
        G::LivingGradient {
            a: [12, 23, 32, 255],
            b: [60, 110, 120, 255],
            angle_deg: 25.,
            drift_deg_per_sec: 8.,
            glow: 0.3,
            grain: klipp_core::DEFAULT_GENERATOR_GRAIN,
        },
        G::Grain {
            color: [30, 50, 80, 255],
            amount: 0.3,
            size_px: 1.,
            speed: 24.,
        },
        G::Placeholder {
            label: "Shot".into(),
            color: [20, 30, 40, 255],
        },
        G::Gradient {
            top: [10, 20, 30, 255],
            bottom: [80, 90, 120, 255],
            angle_deg: 25.,
            four_point: None,
            grain: klipp_core::DEFAULT_GENERATOR_GRAIN,
        },
        G::ColorBars,
        G::ColorBars100,
        G::Countdown,
        G::Timer {
            mode: Default::default(),
            initial_seconds: 10.,
            format: Default::default(),
            text_color: [255; 4],
            background_color: [0, 0, 0, 255],
        },
        G::Slate {
            project: "test".into(),
            scene: "1".into(),
            take: "1".into(),
            director: String::new(),
            camera: String::new(),
            date: String::new(),
            framerate: "30".into(),
            notes: String::new(),
            text_color: [255; 4],
            background_color: [0, 0, 0, 255],
        },
        G::StillFrame {
            png_path: still_path(),
            width: 96,
            height: 54,
        },
        G::Polygon {
            sides: 5,
            color: [90, 120, 200, 255],
            outline_width_px: 2.,
            fill: true,
        },
        G::Star {
            points: 5,
            inner_radius_pct: 0.45,
            color: [200, 120, 90, 255],
            outline_width_px: 1.,
            fill: true,
        },
    ]
}
#[test]
fn generator_corpus_covers_every_variant() {
    let source = include_str!("../../klipp-core/src/types.rs");
    let body = source
        .split_once("pub enum GeneratorData {")
        .unwrap()
        .1
        .split_once("\n}")
        .unwrap()
        .0;
    let declared: std::collections::BTreeSet<_> = body
        .lines()
        .filter_map(|l| {
            let l = l.strip_prefix("    ")?;
            if !l.chars().next()?.is_ascii_uppercase() {
                return None;
            }
            Some(l.split([' ', '{', ',']).next().unwrap().to_string())
        })
        .collect();
    let exercised = generators().iter().map(variant).collect();
    assert_eq!(
        declared, exercised,
        "new generator needs an active playback fixture"
    );
}

fn variant<T: std::fmt::Debug>(v: &T) -> String {
    format!("{v:?}")
        .split([' ', '{', '('])
        .next()
        .unwrap()
        .to_string()
}
fn inventory() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut add = |id: String, c: Capability| {
        if c.residency != Residency::Gpu {
            out.insert(
                id,
                format!("{:?} #{}", c.residency, c.followup.unwrap_or(0)),
            );
        }
    };
    for t in klipp_effects::registry::effect_types() {
        add(t.desc.id.into(), residency::effect(&(t.sample)()));
    }
    for g in generators() {
        add(
            format!("generator.{}", variant(&g)),
            residency::generator(&g),
        );
    }
    for f in residency::TITLE_FAMILIES {
        for field in f.fields {
            add(format!("title.{}.{field}", f.owner), f.capability);
        }
    }
    for m in klipp_core::title_motion::MotionKind::ALL {
        add(format!("motion.{m:?}"), residency::motion(m));
    }
    for l in klipp_core::title_motion::TitleLoop::catalogue() {
        add(format!("loop.{}", variant(&l)), residency::title_loop(&l));
    }
    out
}
#[test]
fn nonresident_routes_are_an_identity_ratchet_not_a_count() {
    let pinned: BTreeMap<String, String> =
        serde_json::from_str(include_str!("gpu_policy_exceptions.json")).unwrap();
    let actual = inventory();
    for (id, state) in &actual {
        assert_eq!(
            pinned.get(id),
            Some(state),
            "undeclared route {id} = {state}; explicit reviewed exception required. Inventory: {}",
            serde_json::to_string_pretty(&actual).unwrap()
        );
    }
    // Remove resolved pins too, so a later regression cannot reuse an obsolete allowance.
    assert_eq!(
        actual.len(),
        pinned.len(),
        "remove resolved exception identities"
    );
}
fn active(mut fx: E) -> E {
    if klipp_effects::is_identity_effect(&fx) {
        match &mut fx {
            E::Opacity { value } => *value = 0.7,
            E::ColorFade {
                progress,
                duration_secs,
                ..
            } => {
                *progress = 0.4;
                *duration_secs = 2.;
            }
            E::GaussianBlur { radius } => *radius = 2.5,
            E::FocusBlur { radius, band, .. } => {
                *radius = 3.;
                *band = 0.3;
            }
            E::RoundedCorners { radius, .. } => *radius = 8.,
            E::DropShadow {
                offset_x, opacity, ..
            } => {
                *offset_x = 4.;
                *opacity = 0.5;
            }
            E::OuterGlow {
                radius, intensity, ..
            } => {
                *radius = 5.;
                *intensity = 0.4;
            }
            E::Sharpen { amount } => *amount = 0.4,
            E::Gamma { gamma } => *gamma = 1.2,
            E::Crop { left, .. } => *left = 0.05,
            E::Transform { rotation, .. } | E::PictureTransform { rotation, .. } => *rotation = 7.,
            E::CornerPin { tl, .. } => tl[0] = 0.05,
            E::Lut3D { opacity, .. } => *opacity = 0.7,
            E::Flicker { amount, hold, .. } => {
                *amount = 0.2;
                *hold = 0.3;
            }
            E::Particles {
                amount,
                opacity,
                size,
                ..
            } => {
                *amount = 5.;
                *opacity = 0.5;
                *size = 4.;
            }
            E::Merge { opacity, .. } => *opacity = 0.7,
            E::ColourSpaceTransform { output_curve, .. } => *output_curve = 1,
            _ => panic!("identity fixture must activate {}", diag::effect_id(&fx)),
        }
    }
    assert!(!klipp_effects::is_identity_effect(&fx));
    fx
}

fn sequence(kind: ClipKind) -> Sequence {
    let mut s = Sequence::new_default();
    s.resolution = Resolution {
        width: 192,
        height: 108,
    };
    let mut t = Track::new("picture", TrackKind::Video);
    t.clips.push(Clip {
        kind,
        ..Clip::gap(tc(0.), tc(4.))
    });
    s.tracks = vec![t];
    s
}
fn title() -> TitleData {
    TitleData::single_text(TextBox {
        text: "GPU policy".into(),
        font_size: 150.,
        ..Default::default()
    })
}
fn verify(r: &mut Renderer, s: &Sequence, label: &str, allowed: &[&str]) {
    let pins: serde_json::Value =
        serde_json::from_str(include_str!("gpu_policy_runtime_exceptions.json")).unwrap();
    let pin = &pins[label];
    let mut seen = std::collections::BTreeSet::new();
    let mut most_readbacks = 0;
    for time in [0.2, 0.5, 1.1, 0.5] {
        let before = klipp_effects::gpu::readback_stats();
        let ok = r
            .render_display_on_gpu(
                s,
                tc(time),
                192,
                108,
                &mut SyntheticSources,
                RenderOptions::default(),
                DisplayDelivery {
                    texture: None,
                    bytes: false,
                },
            )
            .is_some();
        let frame = diag::last_frame();
        assert_eq!(frame.overflow, 0);
        if label == "title.flat-styles" {
            let prepared = frame
                .events
                .iter()
                .any(|e| e.feature == "title.style_face" && e.work == diag::Work::Preparation);
            assert_eq!(
                prepared,
                time == 0.2,
                "style-face cold preparation must be distinguished from warm reuse"
            );
        }

        for e in &frame.events {
            if e.work == diag::Work::Fallback {
                assert!(
                    pin["issue"].as_u64().is_some_and(|v| v > 0),
                    "{label}: new fallback needs an explicit issue: {e:?}"
                );
                assert!(
                    pin["features"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|v| v.as_str() == Some(e.feature)),
                    "{label}: unpinned route: {e:?}"
                );
                assert!(e.count <= 1, "{label}: repeated fallback work: {e:?}");
                seen.insert(e.feature);
            }
        }
        for e in &frame.events {
            if e.work == diag::Work::Fallback {
                assert!(
                    allowed.contains(&e.feature),
                    "{label}: unexpected fallback {e:?}"
                );
            }
        }
        assert!(ok, "{label}: GPU declined: {:?}", r.last_decline());
        let after = klipp_effects::gpu::readback_stats();
        // Equalize and the pinned source/correction combinations have bounded
        // downloads. Title callback pipelines are included in the process counter.
        let max_readbacks = pin["max_readbacks"].as_u64().unwrap_or(0);
        assert!(
            after.count - before.count <= max_readbacks,
            "{label}: unexpected intermediate readback: {before:?} -> {after:?}"
        );
        assert_eq!(before.failures, after.failures);
        most_readbacks = most_readbacks.max(after.count - before.count);
    }
    if let Some(features) = pin["features"].as_array() {
        let expected: std::collections::BTreeSet<_> =
            features.iter().map(|f| f.as_str().unwrap()).collect();
        assert_eq!(
            seen, expected,
            "{label}: remove resolved runtime exceptions"
        );
        assert_eq!(
            most_readbacks,
            pin["max_readbacks"].as_u64().unwrap(),
            "{label}: tighten resolved transfer allowance"
        );
    }
    // Complete GPU work and check delivery outside intermediate-transfer counts.
    let (bytes, _) = r
        .render_display_u8_on_gpu(
            s,
            tc(0.5),
            192,
            108,
            &mut SyntheticSources,
            RenderOptions::default(),
        )
        .expect("corpus delivery");
    assert_eq!(bytes.len(), 192 * 108 * 4);
}
// Both GPU corpora assert process-wide readback counters.
static GPU_CORPUS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn playback_corpus_exposes_declared_cpu_sources_and_guards_gpu_routes() {
    let _guard = GPU_CORPUS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _still = StillFixture;
    image::RgbaImage::from_pixel(4, 4, image::Rgba([30, 80, 120, 255]))
        .save(still_path())
        .unwrap();
    let mut r = Renderer::new(
        klipp_plugin::OfxHost::new(),
        klipp_title::TitleRenderer::new(),
    );
    r.set_gpu_enabled(true);
    if r.executor().gpu_pipeline_or_init().is_none() {
        assert!(
            std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref() != Ok("1"),
            "GPU required"
        );
        return;
    }
    for g in generators() {
        let label = format!("generator.{}", variant(&g));
        // Every native generator is drawn on the GPU or cached (#2067,
        // #2068): none is a CPU source a frame.
        let allowed: Vec<&str> = vec![];
        let s = sequence(ClipKind::Generator(g));
        verify(&mut r, &s, &label, &allowed);
        if !allowed.is_empty() {
            let f = diag::last_frame();
            assert!(
                f.events
                    .iter()
                    .any(|e| e.feature == allowed[0] && e.clip == Some(s.tracks[0].clips[0].id.0)),
                "CPU source inside a successful GPU stack must be visible"
            );
        }
    }
    for t in klipp_effects::registry::effect_types() {
        let fx = active((t.sample)());
        let cap = residency::effect(&fx);
        if cap.residency == Residency::Conditional || matches!(fx, E::Frei0r { .. }) {
            continue;
        } // Separate source/plugin families, pinned above.
        let mut s = sequence(ClipKind::Generator(G::SolidColor {
            color: [50, 100, 150, 255],
        }));
        s.tracks[0].clips[0] = TestClip::of(s.tracks[0].clips[0].clone())
            .effects([fx])
            .build();
        let allowed = if cap.residency == Residency::RoundTrip {
            vec![t.desc.id]
        } else if [
            "klipp.fx.rolling_shutter_correction",
            "klipp.fx.video_stabilization",
        ]
        .contains(&t.desc.id)
        {
            vec!["generator.layer", "generator.solid"]
        } else {
            vec![]
        };
        verify(&mut r, &s, t.desc.id, &allowed);
    }
    let mut media = sequence(ClipKind::Media {
        media_id: klipp_core::MediaId::new(),
    });
    media.tracks[0].clips[0] = TestClip::of(media.tracks[0].clips[0].clone())
        .effects([E::GaussianBlur { radius: 3. }, E::Opacity { value: 0.7 }])
        .build();
    verify(&mut r, &media, "media.cpu-decoded-gpu-effects", &[]);
    let (actual, _) = r
        .render_display_u8_on_gpu(
            &media,
            tc(0.5),
            192,
            108,
            &mut SyntheticSources,
            RenderOptions::default(),
        )
        .unwrap();
    let mut cpu = Renderer::new(
        klipp_plugin::OfxHost::new(),
        klipp_title::TitleRenderer::new(),
    );
    cpu.set_gpu_enabled(false);
    let reference = cpu.render(
        &media,
        tc(0.5),
        192,
        108,
        &mut SyntheticSources,
        RenderOptions::default(),
    );
    let reference = klipp_color::display::display_transform_ws_to(
        &reference,
        klipp_color::display::ToneMapOp::None,
        klipp_core::WorkingColorspace::Rec709Linear,
        klipp_core::SdrTransfer::Srgb,
    );
    assert_eq!(actual.len(), reference.len());
    assert!(
        actual
            .iter()
            .zip(reference)
            .all(|(a, b)| a.abs_diff(b) <= 1),
        "retaining CPU-decoded media on GPU changed pixels or alpha"
    );

    // A pending seek can supply no CPU pixels after a valid GPU frame.
    // Never interpret that missing input as resident output from the old run.
    struct MissingSource;
    impl FrameSource for MissingSource {
        fn frame(&mut self, _: &Clip, _: f64, width: u32, height: u32) -> Option<SourceFrame> {
            Some(SourceFrame {
                width,
                height,
                pts: None,
                pixels: klipp_compose::SourcePixels::Linear(std::sync::Arc::new(Vec::new())),
            })
        }
    }
    assert!(
        r.render_display_u8_on_gpu(
            &media,
            tc(0.6),
            192,
            108,
            &mut MissingSource,
            RenderOptions::default(),
        )
        .is_none(),
        "a missing CPU frame reused the previous GPU output"
    );
    assert!(diag::last_frame().events.iter().any(|e| {
        e.feature == "composition.gpu_stack" && e.reason == "CPU source has no pixels"
    }));

    for spec in klipp_core::transition_registry::TRANSITION_SPECS {
        // A smooth cut morphs two held source frames by optical flow, run
        // on the GPU from the frames the CPU holds: the stack declines it
        // (#2219), and its own residency is a round trip.
        if spec.kind == klipp_core::TransitionKind::SmoothCut {
            continue;
        }
        let mut s = sequence(ClipKind::Generator(G::SolidColor {
            color: [20, 60, 90, 255],
        }));
        let a = &mut s.tracks[0].clips[0];
        a.timeline_out = tc(2.);
        let aid = a.id;
        let b = Clip {
            kind: ClipKind::Generator(G::SolidColor {
                color: [150, 80, 40, 255],
            }),
            ..Clip::gap(tc(2.), tc(4.))
        };
        let bid = b.id;
        s.tracks[0].clips.push(b);
        s.tracks[0].transitions.push(klipp_timeline::Transition {
            id: klipp_core::TransitionId::new(),
            clip_a_id: aid,
            clip_b_id: bid,
            kind: spec.kind,
            duration: tc(2.),
            alignment: Default::default(),
            user_params: spec.default_params,
        });
        // Cover the overlap, not only the clip outside the transition.
        for time in [1.3, 1.8, 2.3] {
            let before = klipp_effects::gpu::readback_stats();
            assert!(
                r.render_display_on_gpu(
                    &s,
                    tc(time),
                    192,
                    108,
                    &mut SyntheticSources,
                    RenderOptions::default(),
                    DisplayDelivery {
                        texture: None,
                        bytes: false
                    }
                )
                .is_some(),
                "{}",
                spec.string_key
            );
            assert_eq!(
                diag::last_frame().fallback_count(),
                0,
                "{}",
                spec.string_key
            );
            assert_eq!(
                before,
                klipp_effects::gpu::readback_stats(),
                "{}",
                spec.string_key
            );
        }
    }
}

#[test]
fn title_corpus_guards_resident_sources_and_declared_fallbacks() {
    let _guard = GPU_CORPUS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    if r.executor().gpu_pipeline_or_init().is_none() {
        assert!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref() != Ok("1"), "GPU required");
        return;
    }
    for m in klipp_core::title_motion::MotionKind::ALL {
        let t = TitleData::single_text_fx(
            title().primary_text().unwrap().clone(),
            LayerFx {
                motion: klipp_timeline::TitleMotion::enter(
                    klipp_timeline::Motion::new(m).duration(2.),
                ),
                ..Default::default()
            },
        );
        verify(
            &mut r,
            &sequence(ClipKind::Title(t)),
            &format!("title.motion.{m:?}"),
            &["title.raster"],
        );
    }
    for l in klipp_core::title_motion::TitleLoop::catalogue() {
        let label = format!("title.loop.{}", variant(&l));
        let mut t = title();
        t.objects[0].fx.get_or_insert_with(Default::default).loops = vec![l];
        verify(
            &mut r,
            &sequence(ClipKind::Title(t)),
            &label,
            &["title.raster"],
        );
    }
    let key = |time_secs, value| klipp_core::Keyframe {
        time_secs,
        value,
        interpolation: klipp_core::Interpolation::Linear,
        tangent_out: (0., 0.),
        tangent_in: (0., 0.),
    };
    let fx = LayerFx {
        glows: vec![klipp_timeline::TextGlow {
            enabled: true,
            color: [80, 150, 255, 220],
            radius: 15.,
            radius_keyframes: vec![key(0., 8.), key(2., 24.)],
            intensity: 0.9,
            intensity_keyframes: vec![],
        }],
        shadows: vec![klipp_timeline::TitleShadow {
            enabled: true,
            color: [0, 0, 0, 180],
            offset_x: 4.,
            offset_y: 6.,
            blur: 5.,
            offset_x_keyframes: vec![],
            offset_y_keyframes: vec![],
            blur_keyframes: vec![],
        }],
        outlines: vec![klipp_timeline::TextOutline {
            enabled: true,
            color: [240, 245, 255, 230],
            width: 3.5,
            width_keyframes: vec![],
        }],
        inner_shadows: vec![klipp_timeline::InnerShadow {
            enabled: true,
            color: [0, 0, 0, 120],
            offset_x: 2.,
            offset_y: 3.,
            blur: 3.,
            offset_x_keyframes: vec![],
            offset_y_keyframes: vec![],
            blur_keyframes: vec![],
        }],
        ..Default::default()
    };
    for solid in [false, true] {
        let mut tb = title().primary_text().unwrap().clone();
        if solid {
            let mut d = klipp_timeline::Text3d::default();
            d.keys.rotate_y = vec![key(0., -25.), key(2., 25.)];
            tb.three_d = Some(d);
        }
        verify(
            &mut r,
            &sequence(ClipKind::Title(TitleData::single_text_fx(tb, fx.clone()))),
            if solid {
                "title.3d-styles"
            } else {
                "title.flat-styles"
            },
            &[],
        );
    }
    let mut keyed = title().primary_text().unwrap().clone();
    keyed.font_size_keyframes = vec![key(0., 140.), key(2., 180.)];
    verify(
        &mut r,
        &sequence(ClipKind::Title(TitleData::single_text_fx(
            keyed,
            fx.clone(),
        ))),
        "title.keyed-size-styles",
        &[],
    );
    let mut tb = title().primary_text().unwrap().clone();
    tb.tracking_keyframes = vec![key(0., 0.), key(2., 8.)];
    verify(
        &mut r,
        &sequence(ClipKind::Title(TitleData::single_text(tb))),
        "title.tracking",
        &[],
    );
    let mut t = title();
    t.primary_text_mut().unwrap().glitch_intensity = 0.7;
    let s = sequence(ClipKind::Title(t));
    verify(&mut r, &s, "title.glitch", &[]);
    // Cold static title mask preparation is allowed (#2069), but it must not
    // repeat at 4K: the large-mask callback threshold is resolution dependent.
    let static_fx = LayerFx {
        glows: vec![klipp_timeline::TextGlow {
            enabled: true,
            color: [80, 150, 255, 220],
            radius: 25.,
            radius_keyframes: vec![],
            intensity: 0.8,
            intensity_keyframes: vec![],
        }],
        ..Default::default()
    };
    let mut tb = title().primary_text().unwrap().clone();
    tb.font_size = 320.;
    let s = sequence(ClipKind::Title(TitleData::single_text_fx(tb, static_fx)));
    for (i, time) in [0.1, 0.5, 1.0].into_iter().enumerate() {
        let before = klipp_effects::gpu::readback_stats();
        assert!(r
            .render_display_on_gpu(
                &s,
                tc(time),
                3840,
                2160,
                &mut SyntheticSources,
                RenderOptions::default(),
                DisplayDelivery {
                    texture: None,
                    bytes: false
                }
            )
            .is_some());
        let after = klipp_effects::gpu::readback_stats();
        assert_eq!(
            diag::last_frame().fallback_count(),
            0,
            "static source must cache"
        );
        assert_eq!(before.failures, after.failures);
        assert!(
            after.count - before.count <= if i == 0 { 2 } else { 0 },
            "4K static title preparation readbacks repeated"
        );
    }
    r.set_gpu_enabled(false);
    assert!(r
        .render_display_on_gpu(
            &s,
            tc(1.),
            192,
            108,
            &mut SyntheticSources,
            RenderOptions::default(),
            DisplayDelivery {
                texture: None,
                bytes: false
            }
        )
        .is_none());
    assert!(
        diag::last_frame()
            .events
            .iter()
            .any(|e| e.feature == "composition.gpu_stack" && e.reason == "GPU off"),
        "GPU-unavailable decline must be visible"
    );
}
