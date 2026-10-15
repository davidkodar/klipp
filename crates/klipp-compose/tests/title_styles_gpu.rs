//! End-to-end styled text: scrubbing, 3D coverage and no mask downloads.
use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame};
use klipp_core::{Interpolation, Keyframe, RationalTime, Resolution};
use klipp_timeline::{
    Clip, ClipKind, LayerFx, Sequence, Text3d, TextBox, TextGlow, TextOutline, TitleData, Track,
    TrackKind,
};
struct NoMedia;
impl FrameSource for NoMedia {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        None
    }
}
fn key(time_secs: f64, value: f32) -> Keyframe {
    Keyframe {
        time_secs,
        value,
        interpolation: Interpolation::Linear,
        tangent_out: (0.0, 0.0),
        tangent_in: (0.0, 0.0),
    }
}
fn renderer() -> Renderer {
    Renderer::new(
        klipp_plugin::OfxHost::new(),
        klipp_title::TitleRenderer::new(),
    )
}
#[test]
fn animated_styles_match_cpu_when_scrubbing_flat_and_3d_titles() {
    let (w, h) = (640, 360);
    for solid in [false, true] {
        let mut text = TextBox {
            text: "Light".into(),
            font_size: 220.0,
            color: [170, 200, 240, 255],
            ..Default::default()
        };
        if solid {
            let mut d = Text3d::default();
            d.keys.rotate_y = vec![key(0.0, -35.0), key(2.0, 35.0)];
            text.three_d = Some(d);
        }
        let fx = LayerFx {
            motion: if solid {
                klipp_timeline::TitleMotion::enter(
                    klipp_timeline::Motion::new(klipp_timeline::MotionKind::Fade).duration(0.5),
                )
            } else {
                Default::default()
            },
            glows: vec![TextGlow {
                enabled: true,
                color: [80, 150, 255, 220],
                radius: 15.0,
                radius_keyframes: vec![key(0.0, 8.0), key(2.0, 24.0)],
                intensity: 0.9,
                intensity_keyframes: vec![],
            }],
            outlines: vec![TextOutline {
                enabled: true,
                color: [240, 245, 255, 230],
                width: 3.5,
                width_keyframes: vec![],
            }],
            ..Default::default()
        };
        let title = TitleData::single_text_fx(text, fx);
        let mut seq = Sequence::new_default();
        seq.resolution = Resolution {
            width: w,
            height: h,
        };
        let mut track = Track::new("title", TrackKind::Video);
        track.clips.push(Clip {
            kind: ClipKind::Title(title.clone()),
            ..Clip::gap(
                RationalTime::from_secs_f64(0.0, 30),
                RationalTime::from_secs_f64(3.0, 30),
            )
        });
        seq.tracks = vec![track];
        let mut gpu = renderer();
        gpu.set_gpu_enabled(true);
        if gpu.executor().gpu_pipeline_or_init().is_none() {
            assert!(
                std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(),
                "GPU required"
            );
            return;
        }
        let mut cpu = renderer();
        cpu.set_gpu_enabled(false);
        for time in [0.2, 1.0, 2.0, 0.5, 0.2] {
            assert!(
                gpu.title_renderer()
                    .style_source(&title, w, h, time as f32, Some(3.0))
                    .is_some(),
                "fixture must use resident styling"
            );
            let at = RationalTime::from_secs_f64(time, 30);
            let (expected, _) = cpu.render_inspected(
                &seq,
                at,
                w,
                h,
                &mut NoMedia,
                RenderOptions::default(),
                Default::default(),
            );
            let expected = klipp_effects::linear_f32_rgba_to_srgb_u8(&expected);
            let before = klipp_effects::gpu::readback_stats();
            let (actual, _) = gpu
                .render_display_u8_on_gpu(&seq, at, w, h, &mut NoMedia, RenderOptions::default())
                .expect("GPU route");
            let after = klipp_effects::gpu::readback_stats();
            assert_eq!(
                after.count - before.count,
                1,
                "only the final verification frame downloads"
            );
            assert!(
                gpu.executor()
                    .gpu_pipeline_or_init()
                    .unwrap()
                    .title_style_scratch_bytes()
                    > 0
            );
            let worst = actual
                .iter()
                .zip(&expected)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap();
            let changed = actual
                .chunks_exact(4)
                .zip(expected.chunks_exact(4))
                .filter(|(a, b)| a.iter().zip(*b).any(|(a, b)| a.abs_diff(*b) > 2))
                .count();
            let mean = actual
                .iter()
                .zip(&expected)
                .map(|(a, b)| f64::from(a.abs_diff(*b)))
                .sum::<f64>()
                / actual.len() as f64;
            eprintln!("3D={solid}, t={time}: worst={worst}, changed={changed}, mean={mean}");
            if solid {
                // Fixed-point hardware coverage can choose a different edge
                // sample. The solid-level test bounds this to triangle seams;
                // here also bound its effect after glow/outline composition.
                assert!(changed <= 8 && worst <= 75 && mean < 0.002);
                assert_eq!(
                    gpu.title_3d_geometry_uploads(),
                    1,
                    "camera changes and backward seeks retain geometry"
                );
                let target = klipp_effects::gpu::Yuv420Target {
                    tone_map_op: 0,
                    odt_tag: 0,
                    scene: None,
                    gamut_op: 0,
                    hdr_op: 0,
                    peak_nits: 0.0,
                    luma_coefs: [0.2126, 0.7152, 0.0722],
                    semi_planar: false,
                    bits: 8,
                    sample_shift: 0,
                };
                let (planes, _) = gpu
                    .render_yuv420_on_gpu(
                        &seq,
                        at,
                        w,
                        h,
                        &mut NoMedia,
                        RenderOptions::default(),
                        target,
                    )
                    .expect("solid export stays on GPU");
                for (y, p) in planes[..(w * h) as usize]
                    .iter()
                    .zip(actual.chunks_exact(4))
                {
                    let want = (16.0
                        + 219.0
                            * (0.2126 * f32::from(p[0])
                                + 0.7152 * f32::from(p[1])
                                + 0.0722 * f32::from(p[2]))
                            / 255.0)
                        .round() as u8;
                    assert!(y.abs_diff(want) <= 1, "export and viewer luma differ");
                }
            } else {
                assert!(worst <= 2);
            }
        }
    }
}

#[test]
#[ignore = "4K completed-GPU timing; run alone after builds finish"]
fn title_styles_4k_timing() {
    use std::time::Instant;
    let (w, h) = (3840, 2160);
    for solid in [false, true] {
        for resident in [false, true] {
            let mut text = TextBox {
                text: "Light & motion".into(),
                font_size: 190.0,
                color: [170, 200, 240, 255],
                ..Default::default()
            };
            if solid {
                let mut d = Text3d::default();
                d.keys.rotate_y = vec![key(0.0, -30.0), key(2.0, 30.0)];
                text.three_d = Some(d);
            }
            let fx = LayerFx {
                glows: vec![TextGlow {
                    enabled: true,
                    color: [80, 150, 255, 220],
                    radius: 20.0,
                    radius_keyframes: vec![key(0.0, 15.0), key(2.0, 25.0)],
                    intensity: 0.9,
                    intensity_keyframes: vec![],
                }],
                ..Default::default()
            };
            let mut title = TitleData::single_text_fx(text, fx);
            if !resident {
                title.objects[0].opacity_keyframes = vec![key(0.0, 1.0)];
            }
            let mut seq = Sequence::new_default();
            seq.resolution = Resolution {
                width: w,
                height: h,
            };
            let mut track = Track::new("title", TrackKind::Video);
            track.clips.push(Clip {
                kind: ClipKind::Title(title),
                ..Clip::gap(
                    RationalTime::from_secs_f64(0.0, 30),
                    RationalTime::from_secs_f64(3.0, 30),
                )
            });
            seq.tracks = vec![track];
            let mut r = renderer();
            r.set_gpu_enabled(true);
            r.executor()
                .gpu_pipeline_or_init()
                .expect("timing needs GPU");
            r.gpu_pipeline().expect("baseline GPU mask callbacks");
            let before = klipp_effects::gpu::readback_stats();
            let mut samples = Vec::new();
            for frame in 0..21 {
                let start = Instant::now();
                r.render_display_on_gpu(
                    &seq,
                    RationalTime::from_secs_f64(f64::from(frame) / 10.0, 30),
                    w,
                    h,
                    &mut NoMedia,
                    RenderOptions::default(),
                    klipp_compose::DisplayDelivery {
                        texture: None,
                        bytes: false,
                    },
                )
                .expect("resident frame");
                let p = r.executor().gpu_pipeline_or_init().unwrap();
                let _ = p.device().poll(wgpu::Maintain::Wait);
                if frame > 0 {
                    samples.push(start.elapsed().as_secs_f64() * 1000.0);
                }
            }
            let after = klipp_effects::gpu::readback_stats();
            if solid && resident {
                assert_eq!(r.title_3d_geometry_uploads(), 1);
            }
            samples.sort_by(f64::total_cmp);
            let scratch = r
                .executor()
                .gpu_pipeline_or_init()
                .unwrap()
                .title_style_scratch_bytes();
            eprintln!("styles4K 3D={solid} resident={resident} n={} median_ms={:.3} p95_ms={:.3} readbacks={} bytes={} scratch_bytes={scratch}",samples.len(),samples[10],samples[18],after.count-before.count,after.bytes-before.bytes);
            if resident {
                assert_eq!(after.count, before.count);
                assert!(scratch > 0);
            }
        }
    }
}

#[test]
fn style_source_cache_invalidates_and_unsupported_combinations_decline() {
    let mut r = klipp_title::TitleRenderer::new();
    let mut title = TitleData::single_text_fx(
        TextBox {
            text: "Cached light".into(),
            font_size: 140.0,
            ..Default::default()
        },
        LayerFx {
            glows: vec![TextGlow {
                enabled: true,
                color: [100, 180, 255, 255],
                radius: 10.0,
                radius_keyframes: vec![key(0.0, 10.0), key(2.0, 30.0)],
                intensity: 1.0,
                intensity_keyframes: vec![],
            }],
            ..Default::default()
        },
    );
    let first = r.style_source(&title, 640, 360, 0.0, Some(3.0)).unwrap();
    let next = r.style_source(&title, 640, 360, 1.0, Some(3.0)).unwrap();
    assert_eq!(first.id, next.id);
    assert!(std::sync::Arc::ptr_eq(&first.pixels, &next.pixels));
    title.primary_text_mut().unwrap().color = [30, 90, 200, 255];
    let changed = r.style_source(&title, 640, 360, 1.0, Some(3.0)).unwrap();
    assert_ne!(changed.id, next.id);
    let resized = r.style_source(&title, 1280, 720, 1.0, Some(3.0)).unwrap();
    assert_ne!(changed.id, resized.id);
    r.invalidate_cache();
    let cleared = r.style_source(&title, 1280, 720, 1.0, Some(3.0)).unwrap();
    assert_ne!(resized.id, cleared.id);
    let mut moving = title.clone();
    moving.primary_text_mut().unwrap().three_d = Some(Text3d::default());
    moving.objects[0].fx.as_mut().unwrap().motion = klipp_timeline::TitleMotion::enter(
        klipp_timeline::Motion::new(klipp_timeline::MotionKind::Fade).duration(1.0),
    );
    let a = r.style_source(&moving, 640, 360, 0.2, Some(3.0)).unwrap();
    let b = r.style_source(&moving, 640, 360, 0.4, Some(3.0)).unwrap();
    assert_ne!(
        a.id, b.id,
        "time-varying 3D motion must not reuse a frozen face"
    );
    assert_ne!(a.pixels.texels, b.pixels.texels);
    title.primary_text_mut().unwrap().color[3] = 150;
    assert!(r.style_source(&title, 640, 360, 1.0, Some(3.0)).is_none());
    title.primary_text_mut().unwrap().color[3] = 255;
    title.objects[0].transform.rotation = 15.0;
    assert!(r.style_source(&title, 640, 360, 1.0, Some(3.0)).is_none());
}
