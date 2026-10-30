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
        handles: Default::default(),
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
            // The viewer's CPU encode, dithered as the GPU stack's
            // display encode is (#2072): like with like.
            let expected = klipp_color::display::display_transform_ws_dithered_to(
                &expected,
                klipp_color::display::ToneMapOp::None,
                klipp_core::WorkingColorspace::Rec709Linear,
                klipp_core::SdrTransfer::Srgb,
            );
            let before = klipp_effects::gpu::thread_readback_stats();
            let (actual, _) = gpu
                .render_display_u8_on_gpu(&seq, at, w, h, &mut NoMedia, RenderOptions::default())
                .expect("GPU route");
            let after = klipp_effects::gpu::thread_readback_stats();
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
                //
                // Both routes dither before they round (#2072), and a
                // value the CPU composes exactly onto a code (left
                // undithered) can come out of the GPU's blend a hair off
                // it and take the dither: a single level, at a fraction of
                // a percent of the samples (a mean of 0.003 to 0.005 here,
                // against 0.0004 to 0.001 with neither dithered). So the seam bound is on
                // what lies beyond a level, as strict as it was, and the
                // single levels are bounded on their own.
                let beyond_a_level = actual
                    .iter()
                    .zip(&expected)
                    .map(|(a, b)| f64::from(a.abs_diff(*b).saturating_sub(1)))
                    .sum::<f64>()
                    / actual.len() as f64;
                let single = actual.iter().zip(&expected).filter(|(a, b)| a.abs_diff(**b) == 1).count() as f64
                    / actual.len() as f64;
                assert!(changed <= 8 && worst <= 75 && beyond_a_level < 0.002, "seams: changed={changed}, worst={worst}, beyond a level {beyond_a_level}");
                assert!(single < 0.01, "{:.3}% of samples a level apart", single * 100.0);
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
                    // The viewer's bytes are dithered before they round
                    // (#2072) and the export's 4:2:0 kernel rounds without
                    // it. The dither moves each channel by at most a code,
                    // all three the same way, so the luma made from them by
                    // at most 219/255 of a code: one level more than the
                    // routes' own bar, and nothing else between them.
                    assert!(y.abs_diff(want) <= 2, "export and viewer luma differ");
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
            let before = klipp_effects::gpu::thread_readback_stats();
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
            let after = klipp_effects::gpu::thread_readback_stats();
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
    // Two 4K titles alternate and scrub without preparing either face again.
    let mut other = title.clone();
    other.primary_text_mut().unwrap().text = "Other light".into();
    let mut ids = [0; 2];
    for (pass, t) in [0.0, 1.0, 0.25].into_iter().enumerate() {
        for (i, td) in [&title, &other].into_iter().enumerate() {
            let source = r.style_source(td, 3840, 2160, t, Some(3.0)).unwrap();
            if pass == 0 { ids[i] = source.id; }
            else {
                assert_eq!(source.id, ids[i], "alternating titles retain their own faces");
                assert_eq!(source.face_work, klipp_title::StyleFaceWork::Reused);
            }
        }
    }
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

#[test]
fn title_post_and_grade_stay_resident_in_working_spaces() {
    use klipp_core::WorkingColorspace;
    use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
    use klipp_timeline::GradeAccess;
    let (w, h) = (320, 180);
    const VARIANTS:usize=66;
    // Isolate a failing case locally without weakening the default regression.
    let selected=std::env::var("KLIPP_TEST_TITLE_VARIANT").ok().map(|v| {let n=v.parse::<usize>().expect("title variant number");assert!(n<VARIANTS,"unknown title variant");n});
    for ws in [WorkingColorspace::Rec709Linear, WorkingColorspace::AcesCG] {
        for graded in [false, true] {
          for variant in 0..VARIANTS {
            if selected.is_some_and(|n|n!=variant) {continue;}
            let mut title = TitleData::single_text_fx(TextBox {
                text: "Glitch".into(), font_size: 180.0, color: [190, 120, 60, 255],
                glitch_intensity: 0.6, chromatic_aberration_px: 12.0, ..Default::default()
            }, LayerFx {
                outlines: vec![TextOutline { enabled: true, color: [60, 180, 230, 255], width: 3.0, width_keyframes: vec![] }],
                ..Default::default()
            });
            if variant==6 { title.objects[0].fx_mut().edge_fade=klipp_core::title_motion::EdgeFade { left:0.45,right:0.4,top:0.4,bottom:0.4 }; }
            if (9..12).contains(&variant) { let text=title.primary_text_mut().unwrap(); text.glitch_intensity=0.0; text.chromatic_aberration_px=0.0; text.font_size_keyframes=vec![key(0.0,120.0),key(1.0,220.0)]; text.vertical_layout=variant==10; }
            if variant>0 && variant!=6 && variant<9 {
                let text=title.primary_text_mut().unwrap();
                text.glitch_intensity=0.0; text.chromatic_aberration_px=0.0;
                if variant==3 { text.text="First\nSecond".into(); text.line_spacing_keyframes=vec![key(0.0,0.8),key(1.0,1.4)]; }
                if variant==1 { title.objects[0].fx_mut().loops=vec![klipp_timeline::TitleLoop::ColorCycle{rate_hz:0.5}]; }
                if variant==2 { title.objects[0].fx_mut().loops=vec![klipp_timeline::TitleLoop::Counter{from:10.0,to:50.0,duration_secs:2.0,decimals:1}]; }
                if variant>=4 {
                    title.objects[0].fx_mut().loops=vec![klipp_timeline::TitleLoop::ColorBlend{color_a:[190,120,60,80],color_b:[80,190,200,210],rate_hz:0.5}];
                    if variant==4 { title.primary_text_mut().unwrap().outline_write_on=0.5; }
                    if variant>=5 {
                        title.objects[0].fx_mut().loops.clear();
                        title.primary_text_mut().unwrap().line_spacing_keyframes=vec![key(0.0,0.8),key(1.0,1.4)];
                        title.primary_text_mut().unwrap().text="First\nSecond".into();
                        title.primary_text_mut().unwrap().color[3]=120;
                        let mut other=title.objects[0].clone();
                        other.id=klipp_timeline::TitleObjectId(2);
                        other.opacity=0.6;
                        other.transform.center.x += 0.15;
                        title.objects.push(other);
                        if variant==7 { title.objects[1].blend=klipp_core::BlendMode::Screen; }
                        if variant==8 {
                            title.objects[0].mask=Some(klipp_timeline::LayerMask { source:title.objects[1].id,kind:klipp_core::layer_mask::LayerMaskKind::InvertedLuma });
                            title.objects[1].visible=false;
                        }
                    }
                }
            }
            if variant>=12 {
                let text=title.primary_text_mut().unwrap();text.glitch_intensity=0.0;text.chromatic_aberration_px=0.0;
                title.objects[0].fx_mut().loops=vec![klipp_timeline::TitleLoop::ColorCycle{rate_hz:0.5},klipp_timeline::TitleLoop::Wave{amplitude:0.1,rate_hz:1.0}];
                if variant==13 {
                    let text=title.primary_text_mut().unwrap();text.glitch_intensity=0.6;text.chromatic_aberration_px=12.0;
                    title.objects[0].fx_mut().edge_fade=klipp_core::title_motion::EdgeFade {left:0.45,right:0.4,top:0.4,bottom:0.4};
                }
            }
            if variant==14 {
                let text=title.primary_text_mut().unwrap();text.underline=true;text.strikethrough=true;text.write_on=0.6;
                text.font_size_keyframes=vec![key(0.0,120.0),key(1.0,220.0)];
            }
            if variant==15 {
                title.background=klipp_timeline::TitleBackground::FullFrame{color:[60,80,110,150]};
                title.objects[0].blend=klipp_core::BlendMode::Screen;
                title.objects[0].opacity=0.6;
            }
            if variant>=16 {
                title.background=klipp_timeline::TitleBackground::Color{color:[40,90,120,120],padding:24.0};
                title.objects[0].opacity=0.6;
                if variant==17 {let mut other=title.objects[0].clone();other.id=klipp_timeline::TitleObjectId(2);other.transform.center.y+=0.25;title.objects.push(other);}
            }
            if variant>=18 {
                title.objects[0].transform.rotation_keyframes=vec![key(0.0,-25.0),key(1.0,40.0)];
                title.objects[0].transform.anchor_norm.x=0.35;
                if variant==19 {
                    title.primary_text_mut().unwrap().glitch_intensity=0.6;
                    title.primary_text_mut().unwrap().chromatic_aberration_px=12.0;
                    title.objects[0].fx_mut().edge_fade=klipp_core::title_motion::EdgeFade {left:0.45,right:0.4,top:0.4,bottom:0.4};
                }
            }
            if variant==20 { title.objects[0].fx_mut().loops.push(klipp_timeline::TitleLoop::Pulse{rate_hz:0.7,min_opacity:0.2}); }
            if variant==21 { title.objects[0].fx_mut().loops.push(klipp_timeline::TitleLoop::Sway{rate_hz:0.7,amplitude:0.2}); }
            if variant>=22 {
                let text=title.primary_text_mut().unwrap();
                text.font_size_keyframes=vec![key(0.0,120.0),key(1.0,220.0)];
                if variant==25 {
                    text.pattern_fill_enabled=true;
                    text.pattern_fill=Some(klipp_timeline::TextPattern::Checkerboard{color_a:[200,60,20,170],color_b:[30,100,230,255],cell_size:25.0});
                } else {
                    text.gradient=Some(klipp_timeline::TextGradient {
                        top_color:[200,60,20,170],bottom_color:[30,100,230,255],enabled:true,
                        stops:if variant==22 {vec![]} else {vec![klipp_timeline::GradientStop{position:0.1,color:[200,60,20,170]},klipp_timeline::GradientStop{position:0.4,color:[10,220,100,0]},klipp_timeline::GradientStop{position:0.9,color:[30,100,230,255]}]},
                        kind:if variant==23 {klipp_timeline::GradientKind::Linear{angle_deg:32.0}} else if variant==24 {klipp_timeline::GradientKind::Radial{centre:[0.3,0.6],radius:0.7}} else {Default::default()},stop_keyframes:vec![],
                    });
                }
            }
            if variant>=26 {
                title.primary_text_mut().unwrap().texture_fill=Some(klipp_timeline::TextureFill {
                    source:klipp_timeline::TextureSource::Preset(klipp_timeline::TexturePresetId::DiagonalHatch),
                    tile_mode:match variant {26=>klipp_timeline::TextureTileMode::Stretch,27=>klipp_timeline::TextureTileMode::Tile,28=>klipp_timeline::TextureTileMode::Fit,_=>klipp_timeline::TextureTileMode::Cover},
                    opacity:0.7,scale:0.8,rotation:20.0,offset_x:-12.0,offset_y:9.0,tint:[160,255,200,255],..Default::default()
                });
            }
            if variant>=30 {
                title.objects[0].fx_mut().motion=klipp_timeline::TitleMotion::enter(klipp_timeline::Motion::new(if variant==30 {klipp_timeline::MotionKind::Fade} else {klipp_timeline::MotionKind::ScaleUp}).duration(0.8).stagger(klipp_timeline::StaggerUnit::Char,0.05));
                if variant>=32 {title.objects[0].fx_mut().animators=vec![klipp_timeline::Animator{props:klipp_timeline::AnimatorProps{dx:0.1,dy:0.1,scale:0.8,opacity:0.7,..Default::default()},..Default::default()}];}
            }
            if variant>=33 {title.objects[0].fx_mut().animators[0].props.rotation=25.0;}
            if variant>=34 {title.objects[0].fx_mut().animators[0].props.blur=if variant==34 {0.1}else{0.5};}
            if variant==35 {title.primary_text_mut().unwrap().text="Soft Title".into();}
            if variant==36 {title.objects[0].fx_mut().animators[0].props.rotation=90.0;}
            if variant==37 {let text=title.primary_text_mut().unwrap();text.glitch_intensity=0.6;text.chromatic_aberration_px=12.0;}
            if variant>=38 {
                let text=title.primary_text_mut().unwrap();text.text="Colour Wave".into();text.gradient=None;text.pattern_fill=None;text.texture_fill=None;
                title.objects[0].fx_mut().animators[0].props.colour=Some([30,180,240,140]);
                if variant==39 {title.objects[0].fx_mut().animators[0].props.blur=0.0;title.objects[0].fx_mut().animators[0].props.rotation=0.0;}
            }
            if variant>=40 {
                title.objects[0].fx_mut().animators.clear();title.objects[0].fx_mut().motion=Default::default();
                let text=title.primary_text_mut().unwrap();text.text="Rich text".into();
                text.style_runs=vec![klipp_timeline::StyleRun{start:0,end:4,color:Some([220,40,100,130]),font_size:Some(160.0),underline:if variant==41 {Some(true)}else{None},..klipp_timeline::StyleRun::default_for_range(0,4)}];
            }
            if (42..45).contains(&variant) {
                let text=title.primary_text_mut().unwrap();text.text="Path text".into();text.style_runs.clear();
                text.text_path=Some(match variant {42=>klipp_timeline::TextPath::Arc{radius:55.0,start_angle:200.0,sweep:160.0},43=>klipp_timeline::TextPath::Circle{radius:45.0},_=>klipp_timeline::TextPath::Wave{amplitude:20.0,frequency:1.5,phase:30.0}});
            }
            if (45..48).contains(&variant) {
                let shape=klipp_core::title_motion::RollShape{focus:Some(klipp_core::title_motion::FocusBand{band:0.2,edge_opacity:0.1,blur:if variant==45 {0.0}else{30.0}})};
                title.objects[0].fx_mut().loops.push(if variant==47 {klipp_timeline::TitleLoop::Crawl{direction:klipp_core::title_motion::CrawlDirection::Left,speed_px_per_sec:300.0,repeat:true,shape}}else{klipp_timeline::TitleLoop::Scroll{direction:klipp_core::title_motion::ScrollDirection::Up,speed_px_per_sec:300.0,shape}});
                title.objects[0].fx_mut().edge_fade=klipp_core::title_motion::EdgeFade{left:0.3,right:0.4,top:0.25,bottom:0.35};
            }
            if (48..50).contains(&variant) {
                use klipp_core::vector_path::{PathContour,PathPose,PathVertex};
                let contour=|y:f32|PathPose {stroke_glow:None,contours:vec![PathContour{closed:false,vertices:vec![
                    PathVertex{point:[0.05,y],incoming:[0.05,y],outgoing:[0.3,y-0.3]},
                    PathVertex{point:[0.95,y],incoming:[0.7,y+0.3],outgoing:[0.95,y]},
                ]}],fill:[0.0;4],stroke:[0.0;4],stroke_width:0.0};
                let mut path=klipp_timeline::VectorPath{trim:None,pose:contour(0.5),keyframes:vec![]};
                if variant==49 {path.key(0.0,contour(0.3)).unwrap();path.key(1.0,contour(0.7)).unwrap();}
                let layer=title.add_object(klipp_timeline::TitleObjectContent::Shape(klipp_timeline::Shape{kind:klipp_timeline::ShapeKind::Path(path),fill:None,gradient:None,stroke:None,fit:None}),klipp_timeline::ObjectTransform{size:klipp_timeline::TitlePosition{x:1.0,y:1.0},..klipp_timeline::default_text_transform()});
                title.objects.last_mut().unwrap().visible=false;
                let text=title.primary_text_mut().unwrap();text.text="Linked path".into();text.style_runs.clear();
                text.text_path=Some(klipp_timeline::TextPath::Layer{layer,contour:0,offset:0.1,offset_keyframes:vec![key(0.0,0.0),key(1.0,0.2)],align:klipp_timeline::PathTextAlign::Start,flip:false});
            }
            if variant>=50 {
                let target=title.objects[0].id;
                title.add_object(klipp_timeline::TitleObjectContent::Shape(klipp_timeline::Shape{kind:klipp_timeline::ShapeKind::Rectangle{corner_radius:18.0},fill:Some([40,80,200,130]),gradient:None,stroke:Some(klipp_timeline::ShapeStroke{color:[180,40,90,200],width:4.0}),fit:Some(klipp_timeline::ShapeFit{target,padding:klipp_timeline::TitlePosition{x:12.0,y:8.0}})}),klipp_timeline::default_text_transform());
                if variant==51 {title.objects[0].visible=false;}
                if variant==53 {title.objects[1].blend=klipp_core::BlendMode::Screen;title.objects[1].opacity_keyframes=vec![key(0.0,0.3),key(1.0,0.7)];}
                if variant==54 {title.objects[0].mask=Some(klipp_timeline::LayerMask{source:title.objects[1].id,kind:klipp_core::layer_mask::LayerMaskKind::Luma});title.objects[1].visible=false;title.objects[1].blend=klipp_core::BlendMode::Screen;title.objects[1].opacity_keyframes=vec![key(0.0,0.3),key(1.0,0.7)];}
                if variant!=52 {title.objects.swap(0,1);}
            }
            if variant==41 {
                title.objects[0].fx_mut().animators=vec![klipp_timeline::Animator{props:klipp_timeline::AnimatorProps{opacity:0.0,colour:Some([30,180,240,140]),..Default::default()},..Default::default()}];
            }
            if variant>=55 {
                title=TitleData::single_text_fx(TextBox{text:"Soft text".into(),font_size:180.0,color:[190,120,60,180],..Default::default()},LayerFx::default());
                if variant==57 {
                    title.objects[0].fx_mut().motion=klipp_timeline::TitleMotion::enter(klipp_timeline::Motion::new(klipp_timeline::MotionKind::Blur).duration(0.8).stagger(klipp_timeline::StaggerUnit::Char,0.05));
                } else {
                    title.objects[0].fx_mut().animators=vec![klipp_timeline::Animator{props:klipp_timeline::AnimatorProps{blur:if variant==55 {0.5}else{0.0},rotation:25.0,colour:if variant==56 {Some([30,180,240,140])}else{None},..Default::default()},..Default::default()}];
                }
            }
            if variant==58 {
                title=TitleData::single_text_fx(TextBox{text:"Path motion".into(),font_size:180.0,..Default::default()},LayerFx::default());
                use klipp_core::vector_path::{PathContour,PathPose,PathVertex};
                let pose=PathPose {stroke_glow:None,contours:vec![PathContour{closed:false,vertices:vec![
                    PathVertex{point:[0.1,0.5],incoming:[0.1,0.5],outgoing:[0.3,0.2]},
                    PathVertex{point:[0.9,0.5],incoming:[0.7,0.8],outgoing:[0.9,0.5]},
                ]}],fill:[0.0;4],stroke:[0.0;4],stroke_width:0.0};
                let layer=title.add_object(klipp_timeline::TitleObjectContent::Shape(klipp_timeline::Shape{kind:klipp_timeline::ShapeKind::Path(klipp_timeline::VectorPath{trim:None,pose,keyframes:vec![]}),fill:None,gradient:None,stroke:None,fit:None}),klipp_timeline::ObjectTransform{size:klipp_timeline::TitlePosition{x:1.0,y:1.0},..klipp_timeline::default_text_transform()});
                title.objects.last_mut().unwrap().visible=false;
                title.primary_text_mut().unwrap().text_path=Some(klipp_timeline::TextPath::Layer{layer,contour:0,offset:0.1,offset_keyframes:vec![],align:klipp_timeline::PathTextAlign::Start,flip:false});
                title.objects[0].fx_mut().motion=klipp_timeline::TitleMotion::enter(klipp_timeline::Motion::new(klipp_timeline::MotionKind::Rise).duration(0.8).stagger(klipp_timeline::StaggerUnit::Char,0.05));
            }
            if variant>=59 {
                title=TitleData::single_text_fx(TextBox{text:"Panel".into(),font_size:180.0,..Default::default()},LayerFx::default());
                title.add_object(klipp_timeline::TitleObjectContent::Shape(klipp_timeline::Shape{kind:klipp_timeline::ShapeKind::Rectangle{corner_radius:24.0},fill:Some([40,80,200,130]),gradient:None,stroke:Some(klipp_timeline::ShapeStroke{color:[180,40,90,200],width:4.0}),fit:None}),klipp_timeline::ObjectTransform{
                    size:klipp_timeline::TitlePosition{x:0.7,y:0.5},
                    center_x_keyframes:vec![key(0.0,0.35),key(1.0,0.65)],
                    size_y_keyframes:vec![key(0.0,0.3),key(1.0,0.6)],
                    rotation_keyframes:vec![key(0.0,-15.0),key(1.0,25.0)],
                    anchor_norm:klipp_timeline::TitlePosition{x:0.3,y:0.7},
                    ..klipp_timeline::default_text_transform()});
                if variant==59 || variant==62 {title.objects.remove(0);title.objects[0].opacity_keyframes=vec![key(0.0,0.3),key(1.0,0.7)];}
                if variant==60 || variant==63 {title.objects[1].blend=klipp_core::BlendMode::Screen;title.objects[1].opacity_keyframes=vec![key(0.0,0.3),key(1.0,0.7)];}
                if variant==61 || variant==64 {title.objects[0].mask=Some(klipp_timeline::LayerMask{source:title.objects[1].id,kind:klipp_core::layer_mask::LayerMaskKind::Luma});title.objects[1].visible=false;}
            }
            if variant>=62 {
                for object in &mut title.objects {
                    if let klipp_timeline::TitleObjectContent::Shape(shape)=&mut object.content {
                        shape.gradient=Some(klipp_timeline::TextGradient{enabled:true,top_color:[230,30,50,180],bottom_color:[20,180,220,80],stop_keyframes:vec![],stops:vec![],kind:if variant==63 {klipp_timeline::GradientKind::Radial{centre:[0.3,0.6],radius:0.7}}else{klipp_timeline::GradientKind::Linear{angle_deg:35.0}}});
                    }
                }
            }
            if variant==65 {
                title.primary_text_mut().unwrap().font_size_keyframes=vec![key(0.0,140.0),key(1.0,220.0)];
                let target=title.objects[0].id;
                if let klipp_timeline::TitleObjectContent::Shape(shape)=&mut title.objects[1].content {
                    shape.fit=Some(klipp_timeline::ShapeFit{target,padding:klipp_timeline::TitlePosition{x:12.0,y:8.0}});
                    shape.gradient.as_mut().unwrap().stops=(0..16).map(|i|klipp_timeline::GradientStop{position:i as f32/15.0,color:[(20+i*12) as u8,100,180,(60+i*10) as u8]}).collect();
                }
                title.objects.swap(0,1);
            }
            if variant==11 {let text=title.primary_text_mut().unwrap();text.font_name="Variable Test".into();text.font_axes=vec![("wght".into(),650.0)];}
            let mut clip = Clip { kind: ClipKind::Title(title), ..Clip::gap(RationalTime::from_secs_f64(0.0, 30), RationalTime::from_secs_f64(3.0, 30)) };
            if graded {
                let node = |kind, params| Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] };
                let nodes = vec![node(NodeKind::Source, NodeParams::default()), node(NodeKind::Exposure, NodeParams { exposure: Some(0.4), ..Default::default() }), node(NodeKind::Output, NodeParams::default())];
                let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i+1].id, to_port: 0 }).collect();
                clip.set_grade(NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] });
            }
            let mut seq = Sequence::new_default();
            seq.resolution = Resolution { width: w, height: h };
            let mut track = Track::new("title", TrackKind::Video); track.clips.push(clip); seq.tracks = vec![track];
            let make_renderer=|| {
                if variant!=11 {return renderer();}
                let path=std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"),"/../klipp-title/tests/fonts/InterVariable-subset.ttf"));
                let traits=klipp_title::variable::face_traits(&std::fs::read(&path).unwrap()).unwrap();
                let info=klipp_title::font_discovery::FontInfo {family:"Variable Test".into(),style:"Regular".into(),path,weight:traits.weight,italic:traits.italic,axes:traits.axes};
                let registry=std::sync::Arc::new([("Variable Test".into(),vec![info])].into_iter().collect());
                Renderer::new(klipp_plugin::OfxHost::new(),klipp_title::TitleRenderer::with_font_registry(registry))
            };
            let mut gpu = make_renderer(); gpu.set_gpu_enabled(true); gpu.set_working_colorspace(ws);
            assert!(gpu.executor().gpu_pipeline_or_init().is_some(), "GPU required");
            let mut cpu = make_renderer(); cpu.set_gpu_enabled(false); cpu.set_working_colorspace(ws);
            for (sample,t) in [0.2, 0.7, 0.2].into_iter().enumerate() {
                let at = RationalTime::from_secs_f64(t,30);
                let (want, _) = cpu.render_inspected(&seq, at, w, h, &mut NoMedia, RenderOptions::default(), Default::default());
                let want = klipp_color::display::display_transform_ws_dithered_to(&want, klipp_color::display::ToneMapOp::None, ws, klipp_core::SdrTransfer::Srgb);
                let before = klipp_effects::gpu::thread_readback_stats();
                let scope = klipp_effects::residency::frame("test", t);
                let (got, _) = gpu.render_display_u8_on_gpu(&seq, at, w, h, &mut NoMedia, RenderOptions::default()).expect("resident title");
                drop(scope);
                let events = klipp_effects::residency::last_frame().events;
                assert!(!events.iter().any(|e| e.work == klipp_effects::residency::Work::Fallback), "{events:?}");
                assert_eq!(klipp_effects::gpu::thread_readback_stats().count-before.count,1,"only verification download");
                let worst = got.iter().zip(&want).map(|(a,b)| a.abs_diff(*b)).max().unwrap();
                if worst>3 {let i=got.iter().zip(&want).position(|(a,b)|a.abs_diff(*b)==worst).unwrap()/4; eprintln!("worst pixel {},{} got {:?} want {:?}",i%w as usize,i/w as usize,&got[i*4..i*4+4],&want[i*4..i*4+4]);}
                assert!(worst<=3,"{ws:?} graded={graded} variant={variant} at{t}: {worst}");
                // A still text is laid out on the glyph route, with no
                // resting raster (#2335): the glitched face under its post
                // pass (0), the edge-faded text (6) and the still text by a
                // moving panel (60, 61, 63, 64) were each one CPU raster.
                // Only a staggered text on a linked path still rests (58).
                assert_eq!(gpu.title_renderer().resting_renders(),u64::from(variant==58),"cached sources, not full per-frame rasters");
                if sample==2 { assert!(events.iter().all(|e| e.work!=klipp_effects::residency::Work::Preparation),"warm backward seek: {events:?}"); }
            }
        }
    }
    }
}

#[test]
fn animated_size_reuses_outlines_at_4k() {
    let (w,h)=(3840,2160);
    let title=TitleData::single_text_fx(TextBox {
        text:"Outline".into(),font_size:180.0,
        font_size_keyframes:vec![key(0.0,120.0),key(1.0,220.0)],
        ..Default::default()
    },LayerFx::default());
    let clip=Clip {kind:ClipKind::Title(title),..Clip::gap(RationalTime::from_secs_f64(0.0,30),RationalTime::from_secs_f64(3.0,30))};
    let mut seq=Sequence::new_default();seq.resolution=Resolution {width:w,height:h};
    let mut track=Track::new("title",TrackKind::Video);track.clips.push(clip);seq.tracks=vec![track];
    let mut gpu=renderer();gpu.set_gpu_enabled(true);
    assert!(gpu.executor().gpu_pipeline_or_init().is_some(),"GPU required");
    for (sample,time) in [0.2,0.7,0.2].into_iter().enumerate() {
        let at=RationalTime::from_secs_f64(time,30);
        let scope=klipp_effects::residency::frame("outline timing",time);
        let started=std::time::Instant::now();
        let (pixels,_)=gpu.render_display_u8_on_gpu(&seq,at,w,h,&mut NoMedia,RenderOptions::default()).expect("resident outline");
        let elapsed=started.elapsed();drop(scope);
        assert_eq!(pixels.len(),(w*h*4) as usize);
        let events=klipp_effects::residency::last_frame().events;
        assert!(!events.iter().any(|e|e.work==klipp_effects::residency::Work::Fallback),"{events:?}");
        if sample>0 {assert!(!events.iter().any(|e|e.work==klipp_effects::residency::Work::Preparation),"retained geometry at a new size: {events:?}");}
        eprintln!("4K title size sample {sample}: {elapsed:?} including final verification download");
    }
    assert_eq!(gpu.title_renderer().resting_renders(),0);
}

fn edge_faded_panel(fitted: bool, mode: u8, width: u32, height: u32) -> Sequence {
    use klipp_timeline::{Shape, ShapeFit, ShapeKind, ShapeStroke, TitleObjectContent, TitlePosition};
    let mut title = TitleData::single_text_fx(TextBox {
        text: "Fading panel".into(), font_size: 180.0,
        font_size_keyframes: vec![key(0.0, 120.0), key(1.0, 230.0)],
        ..Default::default()
    }, LayerFx::default());
    let target = title.objects[0].id;
    title.add_object(TitleObjectContent::Shape(Shape {
        kind: ShapeKind::Rectangle { corner_radius: 24.0 },
        fill: Some([40, 80, 200, 130]),
        gradient: (mode == 1).then(|| klipp_timeline::TextGradient {
            enabled: true, top_color: [230,30,50,180], bottom_color: [20,180,220,80],
            kind: klipp_timeline::GradientKind::Linear { angle_deg: 35.0 }, stops: vec![], stop_keyframes: vec![]
        }),
        stroke: Some(ShapeStroke { color: [180,40,90,200], width: 4.0 }),
        fit: fitted.then_some(ShapeFit { target, padding: TitlePosition { x: 30.0, y: 30.0 } }),
    }), klipp_timeline::ObjectTransform {
        size: TitlePosition { x: 0.8, y: 0.6 },
        center_x_keyframes: vec![key(0.0,0.3),key(1.0,0.65)],
        size_y_keyframes: vec![key(0.0,0.3),key(1.0,0.7)],
        rotation_keyframes: vec![key(0.0,-20.0),key(1.0,30.0)],
        anchor_norm: TitlePosition { x: 0.3, y: 0.7 },
        ..klipp_timeline::default_text_transform()
    });
    title.objects[1].fx_mut().edge_fade = klipp_core::title_motion::EdgeFade { left: 0.45, right: 0.4, top: 0.4, bottom: 0.4 };
    title.objects[1].opacity_keyframes = vec![key(0.0,0.3), key(1.0,0.8)];
    if fitted && mode == 0 { title.objects[1].opacity_keyframes.clear(); }
    if mode == 1 { title.objects[1].blend = klipp_core::BlendMode::Screen; }
    if mode == 2 {
        title.objects[0].mask = Some(klipp_timeline::LayerMask { source: title.objects[1].id, kind: klipp_core::layer_mask::LayerMaskKind::Luma });
        title.objects[1].visible = false;
    } else { title.objects.swap(0,1); }
    let clip = Clip { kind: ClipKind::Title(title), ..Clip::gap(RationalTime::from_secs_f64(0.0,30), RationalTime::from_secs_f64(3.0,30)) };
    let mut seq = Sequence::new_default();
    seq.resolution = Resolution { width, height };
    let mut track = Track::new("title", TrackKind::Video);
    track.clips.push(clip); seq.tracks = vec![track]; seq
}

#[test]
fn edge_faded_rectangles_match_cpu_and_remain_resident() {
    let (w,h) = (320,180);
    for fitted in [false,true] {
        for mode in 0..2 {
            for ws in [klipp_core::WorkingColorspace::Rec709Linear, klipp_core::WorkingColorspace::AcesCG] {
                let seq = edge_faded_panel(fitted,mode,w,h);
                let mut cpu = renderer(); cpu.set_gpu_enabled(false); cpu.set_working_colorspace(ws);
                let mut gpu = renderer(); gpu.set_gpu_enabled(true); gpu.set_working_colorspace(ws);
                assert!(gpu.executor().gpu_pipeline_or_init().is_some(), "GPU required");
                for t in [0.2,0.7,0.2] {
                    let at = RationalTime::from_secs_f64(t,30);
                    let (want,_) = cpu.render_inspected(&seq,at,w,h,&mut NoMedia,RenderOptions::default(),Default::default());
                    let want = klipp_color::display::display_transform_ws_dithered_to(&want,klipp_color::display::ToneMapOp::None,ws,klipp_core::SdrTransfer::Srgb);
                    let before = klipp_effects::gpu::thread_readback_stats();
                    let scope = klipp_effects::residency::frame("edge-faded rectangle",t);
                    let (got,_) = gpu.render_display_u8_on_gpu(&seq,at,w,h,&mut NoMedia,RenderOptions::default()).expect("GPU frame");
                    drop(scope);
                    let events = klipp_effects::residency::last_frame().events;
                    assert!(!events.iter().any(|e|e.work==klipp_effects::residency::Work::Fallback), "{events:?}");
                    assert_eq!(klipp_effects::gpu::thread_readback_stats().count-before.count,1,"only verification download");
                    let worst = got.iter().zip(&want).map(|(a,b)|a.abs_diff(*b)).max().unwrap();
                    if worst>3 {let i=got.iter().zip(&want).position(|(a,b)|a.abs_diff(*b)==worst).unwrap()/4;eprintln!("pixel {},{} got {:?}, want {:?}",i%w as usize,i/w as usize,&got[i*4..i*4+4],&want[i*4..i*4+4]);}
                    assert!(worst<=3,"fitted={fitted} mode={mode} {ws:?} time={t}: {worst} levels apart");
                }
                assert_eq!(gpu.title_renderer().resting_renders(),0,"no full title raster");
            }
        }
    }
}

#[test]
fn edge_faded_fitted_rectangle_stays_resident_at_4k() {
    let (w,h)=(3840,2160);
    let seq=edge_faded_panel(true,1,w,h);
    let mut gpu=renderer();gpu.set_gpu_enabled(true);
    assert!(gpu.executor().gpu_pipeline_or_init().is_some(),"GPU required");
    for (sample,t) in [0.2,0.7,0.2].into_iter().enumerate() {
        let scope=klipp_effects::residency::frame("4K faded panel",t);
        let before=klipp_effects::gpu::thread_readback_stats();
        let started=std::time::Instant::now();
        gpu.render_display_u8_on_gpu(&seq,RationalTime::from_secs_f64(t,30),w,h,&mut NoMedia,RenderOptions::default()).expect("GPU frame");
        let elapsed=started.elapsed();drop(scope);
        let events=klipp_effects::residency::last_frame().events;
        assert!(!events.iter().any(|e|e.work==klipp_effects::residency::Work::Fallback),"{events:?}");
        assert_eq!(klipp_effects::gpu::thread_readback_stats().count-before.count,1,"only verification download");
        if sample>0 {assert!(!events.iter().any(|e|e.work==klipp_effects::residency::Work::Preparation),"warm source: {events:?}");}
        eprintln!("4K edge-faded fitted rectangle sample {sample}: {elapsed:?}, including verification download");
    }
    assert_eq!(gpu.title_renderer().resting_renders(),0);
}

#[test]
fn edge_faded_rectangle_mattes_match_cpu_and_stay_resident() {
    check_rectangle_motion_fades(|fitted,mode,w,h| {
        let mut seq=faded_rectangle_matte(fitted,mode,w,h);
        let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
        for object in &mut title.objects {object.fx_mut().motion=Default::default();}
        seq
    });
}

fn motion_faded_panel(fitted: bool, mode: u8, width: u32, height: u32) -> Sequence {
    use klipp_timeline::{Motion, MotionKind, StaggerUnit, TitleMotion, TitleObjectContent};
    let mut seq=edge_faded_panel(fitted,mode,width,height);
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    for object in &mut title.objects {
        if matches!(object.content,TitleObjectContent::Shape(_)) {
            object.fx_mut().motion=TitleMotion::both(
                Motion::new(MotionKind::Fade).duration(0.8).stagger(StaggerUnit::Char,0.1),
                Motion::new(MotionKind::Fade).duration(0.8).stagger(StaggerUnit::Word,0.1),
            );
            object.fx_mut().edge_fade=Default::default();
        }
    }
    seq
}

/// #3160: a rectangle of a two-layer title that blinks, pulses or
/// flickers: its loop fades it on the GPU shape route as the CPU fades
/// it, on and off.
fn looping_panel(fitted: bool, mode: u8, width: u32, height: u32) -> Sequence {
    use klipp_timeline::{TitleLoop, TitleObjectContent};
    let mut seq=edge_faded_panel(fitted,0,width,height);
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    for object in &mut title.objects {
        if matches!(object.content,TitleObjectContent::Shape(_)) {
            object.fx_mut().edge_fade=Default::default();
            object.fx_mut().loops=vec![match mode {
                0=>TitleLoop::Blink { rate_hz: 2.0, duty: 0.5 },
                1=>TitleLoop::Pulse { rate_hz: 0.5, min_opacity: 0.9 },
                _=>TitleLoop::Flicker { rate_hz: 6.0, depth: 0.6 },
            }];
        }
    }
    seq
}

#[test]
fn rectangle_loops_fade_it_on_the_gpu_as_on_the_cpu() {
    check_rectangle_motion_samples(looping_panel,3,&[0.1,0.3,0.6,1.0,2.2]);
}

/// #3160: a keyed, turning rectangle of a two-layer title with a glow, or
/// with a shadow, an outline and an inner shadow: its stack is drawn
/// round it on the GPU shape route as the CPU draws it.
fn styled_panel(fitted: bool, mode: u8, width: u32, height: u32) -> Sequence {
    use klipp_timeline::{InnerShadow, TitleObjectContent, TitleShadow};
    let mut seq=edge_faded_panel(fitted,0,width,height);
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    for object in &mut title.objects {
        // Mode 0 glows round an opaque panel; mode 2 through a translucent one.
        if let (0,TitleObjectContent::Shape(shape))=(mode,&mut object.content) {
            shape.fill=Some([40,80,200,255]);
            shape.stroke=shape.stroke.map(|mut k|{k.color[3]=255;k});
        }
        if matches!(object.content,TitleObjectContent::Shape(_)) {
            let fx=object.fx_mut();
            fx.edge_fade=Default::default();
            if mode!=1 {
                fx.glows=vec![TextGlow { enabled: true, color: [255,200,80,255], radius: 20.0, radius_keyframes: vec![], intensity: 0.35, intensity_keyframes: vec![] }];
            } else {
                fx.shadows=vec![TitleShadow { enabled: true, color: [0,0,0,200], offset_x: 6.0, offset_x_keyframes: vec![], offset_y: 8.0, offset_y_keyframes: vec![], blur: 5.0, blur_keyframes: vec![] }];
                fx.outlines=vec![TextOutline { enabled: true, color: [250,250,250,255], width: 3.0, width_keyframes: vec![] }];
                fx.inner_shadows=vec![InnerShadow { enabled: true, color: [10,20,60,220], offset_x: 3.0, offset_x_keyframes: vec![], offset_y: 3.0, offset_y_keyframes: vec![], blur: 4.0, blur_keyframes: vec![] }];
            }
        }
    }
    seq
}

#[test]
fn shape_stacks_are_drawn_on_the_gpu_as_on_the_cpu() {
    check_rectangle_motion_samples(styled_panel,2,&[0.0,0.3,0.7,1.0,2.2]);
}

/// A glow seen through a translucent shape: on the GPU in Rec.709, where
/// it mixes as the CPU mixes it; outside it, with the title's raster.
#[test]
fn a_glow_through_a_translucent_shape_stays_on_the_cpu_outside_rec709() {
    let (w,h)=(320,180);
    let seq=styled_panel(false,2,w,h);
    for (ws,on_gpu) in [(klipp_core::WorkingColorspace::Rec709Linear,true),(klipp_core::WorkingColorspace::AcesCG,false)] {
        let mut cpu=renderer();cpu.set_gpu_enabled(false);cpu.set_working_colorspace(ws);
        let mut gpu=renderer();gpu.set_gpu_enabled(true);gpu.set_working_colorspace(ws);
        assert!(gpu.executor().gpu_pipeline_or_init().is_some(),"GPU required");
        for t in [0.3,1.0] {
            let at=RationalTime::from_secs_f64(t,30);
            let (want,_)=cpu.render_inspected(&seq,at,w,h,&mut NoMedia,RenderOptions::default(),Default::default());
            let want=klipp_color::display::display_transform_ws_dithered_to(&want,klipp_color::display::ToneMapOp::None,ws,klipp_core::SdrTransfer::Srgb);
            let scope=klipp_effects::residency::frame("glow through a translucent shape",t);
            let (got,_)=gpu.render_display_u8_on_gpu(&seq,at,w,h,&mut NoMedia,RenderOptions::default()).expect("GPU frame");
            drop(scope);
            let fell=klipp_effects::residency::last_frame().events.iter().any(|e|e.work==klipp_effects::residency::Work::Fallback);
            assert_eq!(fell,!on_gpu,"{ws:?} at {t}");
            let worst=got.iter().zip(&want).map(|(a,b)|a.abs_diff(*b)).max().unwrap();
            assert!(worst<=1,"{ws:?} at {t}: {worst} levels apart");
        }
    }
}

#[test]
fn rectangle_motion_fades_match_cpu_and_remain_resident() {
    check_rectangle_motion_fades(motion_faded_panel);
}

fn check_rectangle_motion_fades(panel: fn(bool,u8,u32,u32)->Sequence) {
    check_rectangle_motion_fade_modes(panel,2);
}

fn check_rectangle_motion_fade_modes(panel: fn(bool,u8,u32,u32)->Sequence, modes: u8) {
    check_rectangle_motion_samples(panel,modes,&[0.0,0.2,0.7,2.6,2.95,0.2]);
}

fn check_rectangle_motion_samples(panel: fn(bool,u8,u32,u32)->Sequence, modes: u8, times: &[f64]) {
    let (w,h) = (320,180);
    for fitted in [false,true] {
        for mode in 0..modes {
            for ws in [klipp_core::WorkingColorspace::Rec709Linear, klipp_core::WorkingColorspace::AcesCG] {
                let seq = panel(fitted,mode,w,h);
                let mut cpu = renderer(); cpu.set_gpu_enabled(false); cpu.set_working_colorspace(ws);
                let mut gpu = renderer(); gpu.set_gpu_enabled(true); gpu.set_working_colorspace(ws);
                assert!(gpu.executor().gpu_pipeline_or_init().is_some(), "GPU required");
                for &t in times {
                    let at = RationalTime::from_secs_f64(t,30);
                    let (want,_) = cpu.render_inspected(&seq,at,w,h,&mut NoMedia,RenderOptions::default(),Default::default());
                    let want = klipp_color::display::display_transform_ws_dithered_to(&want,klipp_color::display::ToneMapOp::None,ws,klipp_core::SdrTransfer::Srgb);
                    let before = klipp_effects::gpu::thread_readback_stats();
                    let scope = klipp_effects::residency::frame("motion-faded rectangle",t);
                    let (got,_) = gpu.render_display_u8_on_gpu(&seq,at,w,h,&mut NoMedia,RenderOptions::default()).expect("GPU frame");
                    drop(scope);
                    let events = klipp_effects::residency::last_frame().events;
                    assert!(!events.iter().any(|e|e.work==klipp_effects::residency::Work::Fallback), "{events:?}");
                    assert_eq!(klipp_effects::gpu::thread_readback_stats().count-before.count,1,"only verification download");
                    let worst = got.iter().zip(&want).map(|(a,b)|a.abs_diff(*b)).max().unwrap();
                    if worst>3 {let i=got.iter().zip(&want).position(|(a,b)|a.abs_diff(*b)==worst).unwrap()/4;eprintln!("pixel {},{} got {:?}, want {:?}",i%w as usize,i/w as usize,&got[i*4..i*4+4],&want[i*4..i*4+4]);}
                    assert!(worst<=3,"fitted={fitted} mode={mode} {ws:?} time={t}: {worst} levels apart");
                }
                assert_eq!(gpu.title_renderer().resting_renders(),0,"no full title raster");
            }
        }
    }
}

#[test]
fn rectangle_motion_fades_stay_resident_at_4k() {
    check_rectangle_motion_fades_at_4k(motion_faded_panel);
}

fn check_rectangle_motion_fades_at_4k(panel: fn(bool,u8,u32,u32)->Sequence) {
    let (w,h)=(3840,2160);
    let seq=panel(true,1,w,h);
    let mut gpu=renderer();gpu.set_gpu_enabled(true);
    assert!(gpu.executor().gpu_pipeline_or_init().is_some(),"GPU required");
    for (sample,t) in [0.0,0.2,0.7,2.6,2.95,0.2].into_iter().enumerate() {
        let scope=klipp_effects::residency::frame("4K faded panel",t);
        let before=klipp_effects::gpu::thread_readback_stats();
        let started=std::time::Instant::now();
        gpu.render_display_u8_on_gpu(&seq,RationalTime::from_secs_f64(t,30),w,h,&mut NoMedia,RenderOptions::default()).expect("GPU frame");
        let elapsed=started.elapsed();drop(scope);
        let events=klipp_effects::residency::last_frame().events;
        assert!(!events.iter().any(|e|e.work==klipp_effects::residency::Work::Fallback),"{events:?}");
        assert_eq!(klipp_effects::gpu::thread_readback_stats().count-before.count,1,"only verification download");
        if sample>0 {assert!(!events.iter().any(|e|e.work==klipp_effects::residency::Work::Preparation),"warm source: {events:?}");}
        eprintln!("4K motion-faded fitted rectangle sample {sample}: {elapsed:?}, including verification download");
    }
    assert_eq!(gpu.title_renderer().resting_renders(),0);
}


#[test]
fn styled_rectangle_motions_are_drawn_on_the_gpu() {
    // A shape's stack with a blurring entrance, a matte's glow among them:
    // drawn on the GPU since #3160, as the CPU draws them.
    use klipp_timeline::{MotionKind, TitleObjectContent};
    let (w,h)=(320,180);
    for case in 0..3 {
        let mut seq=motion_faded_panel(false,if case==0 {2}else{0},w,h);
        let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
        for object in &mut title.objects {
            if matches!(object.content,TitleObjectContent::Shape(_)) {
                object.fx_mut().motion.enter.as_mut().unwrap().kind=MotionKind::FocusIn;
                match case {
                    0=>object.fx_mut().glows.push(TextGlow {enabled:true,color:[255;4],radius:8.0,radius_keyframes:vec![],intensity:0.8,intensity_keyframes:vec![]}),
                    1=>object.fx_mut().shadows.push(klipp_timeline::TitleShadow {enabled:true,color:[0,0,0,255],offset_x:4.0,offset_y:3.0,blur:5.0,offset_x_keyframes:vec![],offset_y_keyframes:vec![],blur_keyframes:vec![]}),
                    _=>object.fx_mut().outlines.push(TextOutline {enabled:true,color:[255;4],width:3.0,width_keyframes:vec![]}),
                }
            }
        }
        let mut gpu=renderer();gpu.set_gpu_enabled(true);
        let mut cpu=renderer();cpu.set_gpu_enabled(false);
        assert!(gpu.executor().gpu_pipeline_or_init().is_some(),"GPU required");
        for t in [0.2,0.7,2.2] {
            let at=RationalTime::from_secs_f64(t,30);
            let (want,_)=cpu.render_inspected(&seq,at,w,h,&mut NoMedia,RenderOptions::default(),Default::default());
            let want=klipp_color::display::display_transform_ws_dithered_to(&want,klipp_color::display::ToneMapOp::None,klipp_core::WorkingColorspace::Rec709Linear,klipp_core::SdrTransfer::Srgb);
            let scope=klipp_effects::residency::frame("styled rectangle motion",t);
            let (got,_)=gpu.render_display_u8_on_gpu(&seq,at,w,h,&mut NoMedia,RenderOptions::default()).expect("GPU frame");
            drop(scope);
            let events=klipp_effects::residency::last_frame().events;
            assert!(!events.iter().any(|e|e.work==klipp_effects::residency::Work::Fallback),"case {case} at {t}: {events:?}");
            let worst=got.iter().zip(&want).map(|(a,b)|a.abs_diff(*b)).max().unwrap();
            // Measured: 1 level, but 2 for the shadow halfway into its
            // focus-in, where the dressed layer is blurred and scaled.
            assert!(worst<=if case==1 && t<0.5 {2} else {1},"case {case} at {t}: {worst} levels apart");
        }
    }
}

fn combined_faded_panel(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=motion_faded_panel(fitted,mode,w,h);
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    for object in &mut title.objects {
        if matches!(object.content,klipp_timeline::TitleObjectContent::Shape(_)) {
            object.fx_mut().edge_fade=klipp_core::title_motion::EdgeFade {left:0.45,right:0.4,top:0.4,bottom:0.4};
        }
    }
    seq
}

#[test]
fn combined_rectangle_fades_match_cpu_and_remain_resident() {
    check_rectangle_motion_fades(combined_faded_panel);
}

#[test]
fn combined_rectangle_fades_stay_resident_at_4k() {
    check_rectangle_motion_fades_at_4k(combined_faded_panel);
}

fn faded_rectangle_matte(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=combined_faded_panel(fitted,2,w,h);
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    title.objects[0].mask.as_mut().unwrap().kind=match mode {
        0=>klipp_core::layer_mask::LayerMaskKind::Alpha,
        1=>klipp_core::layer_mask::LayerMaskKind::Luma,
        2=>klipp_core::layer_mask::LayerMaskKind::InvertedAlpha,
        _=>klipp_core::layer_mask::LayerMaskKind::InvertedLuma,
    };
    if mode==2 {title.objects[1].fx_mut().edge_fade=Default::default();}
    if mode==3 {
        let klipp_timeline::TitleObjectContent::Shape(shape)=&mut title.objects[1].content else {unreachable!()};
        shape.gradient=Some(klipp_timeline::TextGradient {enabled:true,top_color:[230,30,50,180],bottom_color:[20,180,220,80],kind:klipp_timeline::GradientKind::Linear {angle_deg:35.0},stops:vec![],stop_keyframes:vec![]});
    }
    seq
}

#[test]
fn faded_rectangle_mattes_match_cpu_and_remain_resident() {
    check_rectangle_motion_fade_modes(faded_rectangle_matte,4);
}

#[test]
fn faded_rectangle_mattes_stay_resident_at_4k() {
    check_rectangle_motion_fades_at_4k(faded_rectangle_matte);
}

#[test]
fn faded_matte_scene_crop_preserves_faint_interior_pixels() {
    let mut gpu=klipp_effects::gpu::GpuEffectsPipeline::new().expect("GPU required");
    for (w,h,visible) in [(7u32,5u32,true),(7,5,false),(1,1,true),(1,1,false)] {
        let mut pixels=vec![0.0;(w*h*4) as usize];
        for p in pixels.chunks_exact_mut(4) {p.copy_from_slice(&[1.0,1.0,1.0,0.4/255.0]);}
        let bounds=if !visible {(0,0,0,0)} else if w==1 {
            pixels[3]=0.5/255.0;(0,0,1,1)
        } else {
            pixels[((1*w+1)*4+3) as usize]=0.5/255.0;
            pixels[((3*w+5)*4+3) as usize]=1.0;
            (1,1,6,4)
        };
        assert!(gpu.stack_begin(w,h,1));
        assert!(gpu.stack_layer_from_linear(0,&pixels));
        let before=gpu.readback_stats();
        assert!(gpu.stack_title_crop_to_alpha_bounds(0));
        assert_eq!(gpu.readback_stats(),before,"bounds must stay on GPU");
        assert!(gpu.stack_blend(0,1.0,0));
        assert!(gpu.stack_finish_into_chain(true));
        let got=gpu.stack_download_linear();
        for y in 0..h {for x in 0..w {
            let i=((y*w+x)*4+3) as usize;
            let inside=x>=bounds.0 && y>=bounds.1 && x<bounds.2 && y<bounds.3;
            let want=if inside {pixels[i]}else{0.0};
            assert!((got[i]-want).abs()<0.000001,"{w}x{h} visible={visible} at {x},{y}: {} vs {want}",got[i]);
        }}
        gpu.stack_end();
    }
}

fn add_rectangle_slides(seq: &mut Sequence, mode: u8) {
    use klipp_timeline::{MotionKind,TitleObjectContent};
    let kinds=[MotionKind::Rise,MotionKind::Drop,MotionKind::SlideLeft,MotionKind::SlideRight];
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    for object in &mut title.objects {
        if matches!(object.content,TitleObjectContent::Shape(_)) {
            let motion=&mut object.fx_mut().motion;
            motion.enter.as_mut().unwrap().kind=kinds[mode as usize];
            motion.exit.as_mut().unwrap().kind=kinds[(mode as usize+2)%4];
            if mode==3 {
                // Both directions contribute while entrance and exit overlap.
                motion.enter.as_mut().unwrap().duration_secs=2.0;
                motion.exit.as_mut().unwrap().duration_secs=2.0;
            }
        }
    }
}

fn sliding_rectangle_panel(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=combined_faded_panel(fitted,mode%2,w,h);
    add_rectangle_slides(&mut seq,mode);
    seq
}

fn sliding_rectangle_matte(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=faded_rectangle_matte(fitted,mode,w,h);
    add_rectangle_slides(&mut seq,mode);
    seq
}

#[test]
fn rectangle_slides_match_cpu_and_stay_resident() {
    check_rectangle_motion_fade_modes(sliding_rectangle_panel,4);
}

#[test]
fn rectangle_slide_mattes_match_cpu_and_stay_resident() {
    check_rectangle_motion_fade_modes(sliding_rectangle_matte,4);
}

#[test]
fn rectangle_slides_stay_resident_at_4k() {
    check_rectangle_motion_fades_at_4k(sliding_rectangle_panel);
}

#[test]
fn rectangle_slide_translation_matches_cpu_clipped_sprite() {
    check_rectangle_slide_translation(false,false);
}

#[test]
fn rectangle_blur_slide_zero_blur_matches_cpu_translation() {
    check_rectangle_slide_translation(true,false);
}

fn check_rectangle_slide_translation(blur: bool, clip_rows: bool) {
    let mut gpu=klipp_effects::gpu::GpuEffectsPipeline::new().expect("GPU required");
    let (w,h)=(7,5);
    for visible in [true,false] {
        let mut bytes=vec![0u8;(w*h*4) as usize];
        if visible {for y in 1..4 {for x in 0..5 {
            let i=((y*w+x)*4) as usize;bytes[i..i+4].copy_from_slice(&[80,140,220,255]);
        }}}
        let sprite=klipp_title::BoxRaster::from_bytes(&bytes,(w,h),(1,4),(0,5));
        for direction in [[0.0,1.0],[0.0,-1.0],[1.5,0.0],[-1.5,0.0]] {
            let offset=direction.map(|d:f32|(0.5*3.0*d).round() as i32);
            let want=klipp_title::pose_sprite(&sprite,w,h,offset[0],offset[1],1.0,1.0,(0.0,0.0),None,None,if clip_rows {(1,4)}else{(0,h)});
            assert!(gpu.stack_begin(w,h,1));
            assert!(gpu.stack_layer_from_srgb_u8(0,&bytes));
            let before=gpu.readback_stats();
            let travel=[0.5,1.0,direction[0],direction[1]];
            assert!(if blur {gpu.stack_title_motion_blur_translate(0,0.0,travel,[0.0;4],clip_rows)}
                else {gpu.stack_title_motion_translate(0,travel,[0.0;4],clip_rows)});
            assert_eq!(gpu.readback_stats(),before,"source and bounds remain resident");
            assert!(gpu.stack_blend(0,1.0,0));
            assert!(gpu.stack_finish_into_chain(true));
            let got=gpu.stack_download_linear();
            for (pixel,(got,want)) in got.chunks_exact(4).zip(want.chunks_exact(4)).enumerate() {
                assert!((got[3]*255.0-want[3]).abs()<0.001,"visible={visible} offset={offset:?} pixel={pixel}");
            }
            gpu.stack_end();
        }
    }
}

fn add_rectangle_scale(seq: &mut Sequence, mode: u8) {
    use klipp_timeline::{MotionKind,TitleObjectContent};
    use klipp_core::title_motion::MotionAnchor;
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    for object in &mut title.objects {
        if matches!(object.content,TitleObjectContent::Shape(_)) {
            let motion=&mut object.fx_mut().motion;
            let enter=motion.enter.as_mut().unwrap();
            enter.kind=if mode==1 {MotionKind::ScaleDown}else{MotionKind::ScaleUp};
            if mode>0 {enter.anchor=Some(MotionAnchor::Point{x:0.0,y:1.0});}
            if mode==2 {enter.amount=Some(0.15);enter.anchor=Some(MotionAnchor::Point{x:1.4,y:-0.5});}
            let exit=motion.exit.as_mut().unwrap();
            exit.kind=MotionKind::ScaleDown;
            exit.anchor=Some(MotionAnchor::Point{x:0.8,y:0.2});
            if mode==3 {
                enter.kind=MotionKind::SlideLeft;
                enter.duration_secs=2.0;exit.duration_secs=2.0;
            }
        }
    }
}

fn scaling_rectangle_panel(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=combined_faded_panel(fitted,mode%2,w,h);
    add_rectangle_scale(&mut seq,mode);seq
}

fn scaling_rectangle_matte(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=faded_rectangle_matte(fitted,mode,w,h);
    add_rectangle_scale(&mut seq,mode);seq
}

#[test]
fn rectangle_scale_matches_cpu_and_stays_resident() {
    check_rectangle_motion_fade_modes(scaling_rectangle_panel,4);
}

#[test]
fn rectangle_scale_mattes_match_cpu_and_stay_resident() {
    check_rectangle_motion_fade_modes(scaling_rectangle_matte,4);
}

#[test]
fn rectangle_scale_stays_resident_at_4k() {
    check_rectangle_motion_fades_at_4k(scaling_rectangle_panel);
}

fn add_rectangle_blur(seq: &mut Sequence, mode: u8) {
    use klipp_timeline::{MotionKind,TitleObjectContent};
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    for object in &mut title.objects {
        if matches!(object.content,TitleObjectContent::Shape(_)) {
            let motion=&mut object.fx_mut().motion;
            if mode!=1 {motion.enter.as_mut().unwrap().kind=MotionKind::Blur;}
            if mode!=2 {motion.exit.as_mut().unwrap().kind=MotionKind::Blur;}
            if mode==3 {motion.enter.as_mut().unwrap().duration_secs=2.0;motion.exit.as_mut().unwrap().duration_secs=2.0;}
        }
    }
}
fn blurring_rectangle_panel(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=combined_faded_panel(fitted,mode%2,w,h);add_rectangle_blur(&mut seq,mode);seq
}
fn blurring_rectangle_matte(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=faded_rectangle_matte(fitted,mode,w,h);add_rectangle_blur(&mut seq,mode);seq
}
#[test]
fn rectangle_blur_matches_cpu_and_stays_resident() {check_rectangle_motion_fade_modes(blurring_rectangle_panel,4);}
#[test]
fn rectangle_blur_mattes_match_cpu_and_stay_resident() {check_rectangle_motion_fade_modes(blurring_rectangle_matte,4);}
#[test]
fn rectangle_blur_stays_resident_at_4k() {check_rectangle_motion_fades_at_4k(blurring_rectangle_panel);}

#[test]
fn rectangle_blur_handles_empty_small_and_threshold_sources() {
    let mut gpu=klipp_effects::gpu::GpuEffectsPipeline::new().expect("GPU required");
    for (w,h,visible) in [(1,1,true),(7,7,false),(7,7,true)] {
        let mut bytes=vec![0u8;(w*h*4) as usize];
        if visible {for y in 0..h.min(5) {
            let i=((y*w+w/2)*4) as usize;bytes[i..i+4].copy_from_slice(&[80,140,220,255]);
        }}
        for away in [0.0,0.8,0.9,1.0] {
            assert!(gpu.stack_begin(w,h,1));
            assert!(gpu.stack_layer_from_srgb_u8(0,&bytes));
            let before=gpu.readback_stats();
            assert!(gpu.stack_title_motion_blur(0,away));
            assert_eq!(gpu.readback_stats(),before,"bounds and blur remain resident");
            assert!(gpu.stack_blend(0,1.0,0));assert!(gpu.stack_finish_into_chain(true));
            let got=gpu.stack_download_linear();
            if !visible || h==1 || away<=0.8 {
                for (got,want) in got.chunks_exact(4).zip(bytes.chunks_exact(4)) {
                    assert!((got[3]*255.0-f32::from(want[3])).abs()<0.001,"sigma <= 0.5 preserves coverage");
                }
            } else {
                assert!(got[((w/2-1)*4+3) as usize]>0.0,"sigma > 0.5 spreads coverage");
            }
            gpu.stack_end();
        }
    }
}


fn add_rectangle_blur_slide(seq: &mut Sequence, mode: u8) {
    use klipp_timeline::{MotionKind,TitleObjectContent};
    let directions=[MotionKind::Rise,MotionKind::Drop,MotionKind::SlideLeft,MotionKind::SlideRight];
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    for object in &mut title.objects {
        if matches!(object.content,TitleObjectContent::Shape(_)) {
            let motion=&mut object.fx_mut().motion;
            let enter=motion.enter.as_mut().unwrap();let exit=motion.exit.as_mut().unwrap();
            enter.kind=if mode<4 {MotionKind::Blur}else{directions[(mode%4) as usize]};
            exit.kind=if mode<4 {directions[(mode%4) as usize]}else{MotionKind::Blur};
            // Both effects are active together at the parity samples.
            enter.duration_secs=3.0;exit.duration_secs=3.0;
        }
    }
}
fn blur_sliding_rectangle_panel(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=combined_faded_panel(fitted,mode%2,w,h);add_rectangle_blur_slide(&mut seq,mode);seq
}
fn blur_sliding_rectangle_matte(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=faded_rectangle_matte(fitted,mode%4,w,h);add_rectangle_blur_slide(&mut seq,mode);seq
}
#[test]
fn rectangle_blur_slides_match_cpu_and_stay_resident() {check_rectangle_motion_fade_modes(blur_sliding_rectangle_panel,8);}
#[test]
fn rectangle_blur_slide_mattes_match_cpu_and_stay_resident() {check_rectangle_motion_fade_modes(blur_sliding_rectangle_matte,8);}
#[test]
fn rectangle_blur_slides_stay_resident_at_4k() {check_rectangle_motion_fades_at_4k(blur_sliding_rectangle_panel);}

fn add_rectangle_blur_scale(seq: &mut Sequence, mode: u8) {
    use klipp_timeline::{MotionKind,TitleObjectContent};
    use klipp_core::title_motion::MotionAnchor;
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    for object in &mut title.objects {
        if matches!(object.content,TitleObjectContent::Shape(_)) {
            let motion=&mut object.fx_mut().motion;
            let enter=motion.enter.as_mut().unwrap();let exit=motion.exit.as_mut().unwrap();
            enter.kind=if mode<4 {MotionKind::Blur}else if mode.is_multiple_of(2) {MotionKind::ScaleUp}else{MotionKind::ScaleDown};
            exit.kind=if mode>=4 {MotionKind::Blur}else if mode.is_multiple_of(2) {MotionKind::ScaleUp}else{MotionKind::ScaleDown};
            enter.duration_secs=3.0;exit.duration_secs=3.0;
            let scaling=if mode<4 {exit}else{enter};
            if mode%4==1 {scaling.anchor=Some(MotionAnchor::Point{x:0.0,y:1.0});}
            if mode%4==2 {scaling.anchor=Some(MotionAnchor::Point{x:1.4,y:-0.5});scaling.amount=Some(0.15);}
            if mode%4==3 {scaling.anchor=Some(MotionAnchor::Point{x:0.8,y:0.2});scaling.amount=Some(2.0);}
        }
    }
}
fn blur_scaling_rectangle_panel(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=combined_faded_panel(fitted,mode%2,w,h);add_rectangle_blur_scale(&mut seq,mode);seq
}
fn blur_scaling_rectangle_matte(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=faded_rectangle_matte(fitted,mode%4,w,h);add_rectangle_blur_scale(&mut seq,mode);seq
}
#[test]
fn rectangle_blur_scale_matches_cpu_and_stays_resident() {check_rectangle_motion_fade_modes(blur_scaling_rectangle_panel,8);}
#[test]
fn rectangle_blur_scale_mattes_match_cpu_and_stay_resident() {check_rectangle_motion_fade_modes(blur_scaling_rectangle_matte,8);}
#[test]
fn rectangle_blur_scale_stays_resident_at_4k() {check_rectangle_motion_fades_at_4k(blur_scaling_rectangle_panel);}

#[test]
fn rectangle_blur_scale_handles_zero_blur_and_unit_scale() {
    check_rectangle_motion_fades(|fitted,mode,w,h| {
        let mut seq=blur_scaling_rectangle_panel(fitted,mode+4,w,h);
        let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
        for object in &mut title.objects {
            if matches!(object.content,klipp_timeline::TitleObjectContent::Shape(_)) {
                let motion=&mut object.fx_mut().motion;
                motion.enter.as_mut().unwrap().duration_secs=0.8;
                motion.exit.as_mut().unwrap().duration_secs=0.8;
                // Entrance scales with no blur; exit blurs with scale at one.
                // Also exercise a custom unit scale throughout the entrance.
                if mode==1 {motion.enter.as_mut().unwrap().amount=Some(1.0);}
            }
        }
        seq
    });
}

fn solid_anchor_rectangle(fitted: bool, mode: u8, w: u32, h: u32, matte: bool) -> Sequence {
    let mut seq=if matte {faded_rectangle_matte(fitted,mode%4,w,h)}else{combined_faded_panel(fitted,mode%2,w,h)};
    if mode<4 {add_rectangle_scale(&mut seq,mode);}else {add_rectangle_blur_scale(&mut seq,if mode<6 {mode-4}else{mode-2});}
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    for object in &mut title.objects {
        if let klipp_timeline::TitleObjectContent::Shape(shape)=&mut object.content {
            if mode==0 || mode>=4 {shape.gradient=None;shape.fill=Some([40,80,200,255]);}
            if mode==1 {shape.gradient=None;shape.fill=None;}
            if mode==2 {shape.gradient=Some(klipp_timeline::TextGradient {
                enabled:true,top_color:[230,30,50,255],bottom_color:[20,180,220,0],
                kind:klipp_timeline::GradientKind::Linear{angle_deg:35.0},stops:vec![],stop_keyframes:vec![],
            });}
            if mode==3 {
                shape.gradient=None;shape.fill=Some([40,80,200,127]);
                if let Some(stroke)=&mut shape.stroke {stroke.color[3]=127;}
            } else if let Some(stroke)=&mut shape.stroke {stroke.color[3]=255;}
            let motion=&mut object.fx_mut().motion;
            for m in motion.enter.iter_mut().chain(motion.exit.iter_mut()) {m.anchor=Some(klipp_core::title_motion::MotionAnchor::Solid);}
        }
    }
    seq
}
fn solid_anchor_panel(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {solid_anchor_rectangle(fitted,mode,w,h,false)}
fn solid_anchor_matte(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {solid_anchor_rectangle(fitted,mode,w,h,true)}
#[test]
fn rectangle_solid_anchor_matches_cpu_and_stays_resident() {check_rectangle_motion_fade_modes(solid_anchor_panel,8);}
#[test]
fn rectangle_solid_anchor_mattes_match_cpu_and_stay_resident() {check_rectangle_motion_fade_modes(solid_anchor_matte,8);}
#[test]
fn rectangle_solid_anchor_stays_resident_at_4k() {check_rectangle_motion_fades_at_4k(solid_anchor_panel);}

#[test]
fn rectangle_solid_anchor_ties_empty_and_translucent_sources() {
    let mut gpu=klipp_effects::gpu::GpuEffectsPipeline::new().expect("GPU required");
    let (w,h)=(7,5);
    for alpha in [0u8,127,255] {
        let mut bytes=vec![0u8;(w*h*4) as usize];
        for y in 1..4 {for x in 1..6 {let i=((y*w+x)*4) as usize;bytes[i..i+4].copy_from_slice(&[80,140,220,alpha]);}}
        let sprite=klipp_title::BoxRaster::from_bytes(&bytes,(w,h),(1,4),(1,6));
        // Last equal maximum is (4,2); no majority-solid cells uses the centre.
        let anchor=if alpha==255 {(4.5,2.5)}else{(3.5,2.5)};
        let want=klipp_title::pose_sprite(&sprite,w,h,0,0,0.7,1.7,anchor,None,None,(0,h));
        assert!(gpu.stack_begin(w,h,1));assert!(gpu.stack_layer_from_srgb_u8(0,&bytes));
        let before=gpu.readback_stats();
        assert!(gpu.stack_title_motion_scale(0,[0.0;4],[0.0;4],1.7,0.7,[0.5;2],true,false));
        assert_eq!(gpu.readback_stats(),before,"anchor stays on GPU");
        assert!(gpu.stack_blend(0,1.0,0));assert!(gpu.stack_finish_into_chain(true));
        let got=gpu.stack_download_linear();
        for (pixel,(got,want)) in got.chunks_exact(4).zip(want.chunks_exact(4)).enumerate() {
            assert!((got[3]*255.0-want[3].round()).abs()<0.01,"alpha={alpha} pixel={pixel}");
        }
        gpu.stack_end();
    }
}

fn add_rectangle_mask_reveal(seq: &mut Sequence, mode: u8, drop: bool) {
    use klipp_timeline::{MotionKind,TitleObjectContent};
    let reveal=if drop {MotionKind::MaskDrop}else{MotionKind::MaskRise};
    let pairs=[(reveal,MotionKind::Fade),(MotionKind::Fade,reveal),
        (reveal,reveal),(reveal,MotionKind::SlideLeft),
        (MotionKind::ScaleUp,reveal),(reveal,MotionKind::ScaleDown),
        (MotionKind::Blur,reveal),(reveal,MotionKind::Blur),
        (MotionKind::MaskRise,MotionKind::MaskDrop)];
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    for object in &mut title.objects {
        if matches!(object.content,TitleObjectContent::Shape(_)) {
            let motion=&mut object.fx_mut().motion;
            let enter=motion.enter.as_mut().unwrap();let exit=motion.exit.as_mut().unwrap();
            (enter.kind,exit.kind)=pairs[mode as usize];
            for m in [enter,exit] {
                if mode>=2 {m.duration_secs=3.0;}
                // Both mask reveals ignore configurable slide distance on CPU.
                if matches!(m.kind,MotionKind::MaskRise|MotionKind::MaskDrop) {m.distance=9.0;}
                if mode==4 {m.anchor=Some(klipp_core::title_motion::MotionAnchor::Solid);}
                if mode==5 {m.anchor=Some(klipp_core::title_motion::MotionAnchor::Point{x:0.2,y:0.8});}
            }
        }
    }
}
fn maskrise_rectangle_panel(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=combined_faded_panel(fitted,mode%2,w,h);add_rectangle_mask_reveal(&mut seq,mode,false);seq
}
fn maskrise_rectangle_matte(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=faded_rectangle_matte(fitted,mode%4,w,h);add_rectangle_mask_reveal(&mut seq,mode,false);seq
}
#[test]
fn rectangle_maskrise_matches_cpu_and_stays_resident() {check_rectangle_motion_fade_modes(maskrise_rectangle_panel,8);}
#[test]
fn rectangle_maskrise_mattes_match_cpu_and_stay_resident() {check_rectangle_motion_fade_modes(maskrise_rectangle_matte,8);}
#[test]
fn rectangle_maskrise_stays_resident_at_4k() {check_rectangle_motion_fades_at_4k(maskrise_rectangle_panel);}
#[test]
fn rectangle_maskrise_original_rows_clip_translated_and_empty_sources() {
    check_rectangle_slide_translation(false,true);
    check_rectangle_slide_translation(true,true);
}

fn maskdrop_rectangle_panel(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=combined_faded_panel(fitted,mode%2,w,h);add_rectangle_mask_reveal(&mut seq,mode,true);seq
}
fn maskdrop_rectangle_matte(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=faded_rectangle_matte(fitted,mode%4,w,h);add_rectangle_mask_reveal(&mut seq,mode,true);seq
}
#[test]
fn rectangle_maskdrop_matches_cpu_and_stays_resident() {check_rectangle_motion_fade_modes(maskdrop_rectangle_panel,9);}
#[test]
fn rectangle_maskdrop_mattes_match_cpu_and_stay_resident() {check_rectangle_motion_fade_modes(maskdrop_rectangle_matte,9);}
#[test]
fn rectangle_maskdrop_stays_resident_at_4k() {check_rectangle_motion_fades_at_4k(maskdrop_rectangle_panel);}

fn add_rectangle_scale_fade(seq: &mut Sequence, mode: u8, kind: klipp_timeline::MotionKind) {
    use klipp_timeline::{MotionKind,TitleObjectContent};
    use klipp_core::title_motion::{Easing,MotionAnchor};
    let pairs=[(kind,MotionKind::Fade),(MotionKind::Fade,kind),
        (kind,kind),(kind,MotionKind::SlideLeft),
        (MotionKind::ScaleUp,kind),(kind,MotionKind::ScaleDown),
        (MotionKind::Blur,kind),(kind,MotionKind::Blur),
        (kind,MotionKind::MaskRise),(MotionKind::MaskDrop,kind),
        (MotionKind::Pop,MotionKind::Slam)];
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    for object in &mut title.objects {
        if matches!(object.content,TitleObjectContent::Shape(_)) {
            let motion=&mut object.fx_mut().motion;
            let enter=motion.enter.as_mut().unwrap();let exit=motion.exit.as_mut().unwrap();
            (enter.kind,exit.kind)=pairs[mode as usize];
            enter.easing=enter.kind.default_easing();
            exit.easing=exit.kind.default_exit_easing();
            for m in [enter,exit] {
                if mode>=2 {m.duration_secs=3.0;}
                if m.kind==kind {
                    // Both motions ignore custom scale amounts.
                    m.amount=Some(0.05);
                    // At 0.2 s, a linear entrance reaches its alpha-ramp knee.
                    if mode<2 {m.duration_secs=if kind==MotionKind::Slam {0.5}else{0.6};m.easing=Easing::Linear;}
                }
                if mode==3 {m.anchor=Some(MotionAnchor::Point{x:1.4,y:-0.5});}
                if mode==4 {m.anchor=Some(MotionAnchor::Solid);}
                if mode==5 {m.anchor=Some(MotionAnchor::Point{x:0.2,y:0.8});}
            }
        }
    }
}
fn pop_rectangle_panel(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=combined_faded_panel(fitted,mode%2,w,h);add_rectangle_scale_fade(&mut seq,mode,klipp_timeline::MotionKind::Pop);seq
}
fn pop_rectangle_matte(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=faded_rectangle_matte(fitted,mode%4,w,h);add_rectangle_scale_fade(&mut seq,mode,klipp_timeline::MotionKind::Pop);seq
}
#[test]
fn rectangle_pop_matches_cpu_and_stays_resident() {check_rectangle_motion_fade_modes(pop_rectangle_panel,10);}
#[test]
fn rectangle_pop_mattes_match_cpu_and_stay_resident() {check_rectangle_motion_fade_modes(pop_rectangle_matte,10);}
#[test]
fn rectangle_pop_stays_resident_at_4k() {check_rectangle_motion_fades_at_4k(pop_rectangle_panel);}

fn slam_rectangle_panel(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=combined_faded_panel(fitted,mode%2,w,h);add_rectangle_scale_fade(&mut seq,mode,klipp_timeline::MotionKind::Slam);seq
}
fn slam_rectangle_matte(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=faded_rectangle_matte(fitted,mode%4,w,h);add_rectangle_scale_fade(&mut seq,mode,klipp_timeline::MotionKind::Slam);seq
}
#[test]
fn rectangle_slam_matches_cpu_and_stays_resident() {check_rectangle_motion_fade_modes(slam_rectangle_panel,11);}
#[test]
fn rectangle_slam_mattes_match_cpu_and_stay_resident() {check_rectangle_motion_fade_modes(slam_rectangle_matte,11);}
#[test]
fn rectangle_slam_stays_resident_at_4k() {check_rectangle_motion_fades_at_4k(slam_rectangle_panel);}

// Unlike Pop/Slam, these motions preserve alpha and honour custom scale amounts.
fn add_rectangle_opaque_scale(seq: &mut Sequence, mode: u8) {
    use klipp_timeline::{MotionKind,TitleObjectContent};
    let kind=if mode<11 {MotionKind::Larger}else{MotionKind::Smaller};
    let case=mode%11;
    add_rectangle_scale_fade(seq,case,kind);
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    for object in &mut title.objects {
        if matches!(object.content,TitleObjectContent::Shape(_)) {
            let motion=&mut object.fx_mut().motion;
            if case==10 {
                motion.enter.as_mut().unwrap().kind=MotionKind::Larger;
                motion.exit.as_mut().unwrap().kind=MotionKind::Smaller;
            }
            for m in motion.enter.iter_mut().chain(motion.exit.iter_mut()) {
                m.easing=if case<2 {klipp_core::title_motion::Easing::Linear}else{m.kind.default_easing()};
                if matches!(m.kind,MotionKind::Larger|MotionKind::Smaller) {
                    m.amount=match case {3=>Some(12.0),4=>Some(0.0),5=>Some(0.05),_=>None};
                }
            }
            let exit=motion.exit.as_mut().unwrap();
            exit.easing=if case<2 {klipp_core::title_motion::Easing::Linear}else{exit.kind.default_exit_easing()};
        }
    }
}
fn opaque_scale_rectangle_panel(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=combined_faded_panel(fitted,mode%2,w,h);add_rectangle_opaque_scale(&mut seq,mode);seq
}
fn opaque_scale_rectangle_matte(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=faded_rectangle_matte(fitted,mode%4,w,h);add_rectangle_opaque_scale(&mut seq,mode);seq
}
#[test]
fn rectangle_opaque_scale_matches_cpu_and_stays_resident() {check_rectangle_motion_fade_modes(opaque_scale_rectangle_panel,22);}
#[test]
fn rectangle_opaque_scale_mattes_match_cpu_and_stay_resident() {check_rectangle_motion_fade_modes(opaque_scale_rectangle_matte,22);}
#[test]
fn rectangle_opaque_scale_stays_resident_at_4k() {
    check_rectangle_motion_fades_at_4k(opaque_scale_rectangle_panel);
    check_rectangle_motion_fades_at_4k(|fitted,mode,w,h|opaque_scale_rectangle_panel(fitted,mode+11,w,h));
}

#[test]
fn zero_scale_sprite_is_empty_and_preserves_background() {
    let raster=klipp_title::BoxRaster::from_bytes(&[255,0,0,255],(1,1),(0,1),(0,1));
    for scale in [0.0,-0.0] {
        let pose=|under|klipp_title::pose_sprite(&raster,1,1,0,0,1.0,scale,(0.5,0.5),under,None,(0,1));
        assert_eq!(pose(None),vec![0.0;4]);
        let background=pose(Some([0,255,0,128]));
        for (got,want) in background.iter().zip([0.0,255.0,0.0,128.0]) {
            assert!((got-want).abs()<0.001,"background must survive a collapsed sprite");
        }
    }
}

fn add_rectangle_fly(seq: &mut Sequence, mode: u8) {
    use klipp_timeline::{MotionKind,TitleObjectContent};
    use klipp_core::title_motion::{Easing,MotionAnchor};
    let directions=[MotionKind::FlyLeft,MotionKind::FlyRight,MotionKind::FlyTop,MotionKind::FlyBottom];
    let direction=(mode/8) as usize;let case=mode%8;let kind=directions[direction];
    let pairs=[(kind,MotionKind::Fade),(MotionKind::Fade,kind),
        (kind,MotionKind::Blur),(MotionKind::Blur,kind),
        (kind,MotionKind::Larger),(MotionKind::Smaller,kind),
        (kind,MotionKind::MaskDrop),(directions[(direction+1)%4],kind)];
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    for object in &mut title.objects {
        if matches!(object.content,TitleObjectContent::Shape(_)) {
            let motion=&mut object.fx_mut().motion;
            let enter=motion.enter.as_mut().unwrap();let exit=motion.exit.as_mut().unwrap();
            (enter.kind,exit.kind)=pairs[case as usize];
            enter.easing=enter.kind.default_easing();exit.easing=exit.kind.default_exit_easing();
            for m in [enter,exit] {
                m.duration_secs=if case<2 {0.6}else{3.0};
                m.distance=if case==0 {0.02}else if case==1 {1.0}else{0.13};
                // Linear progress samples the fast alpha ramp at its knee (0.2s).
                if case==0 {m.easing=Easing::Linear;}
                if case==4 {m.anchor=Some(MotionAnchor::Point{x:0.2,y:0.8});}
                if case==5 {m.anchor=Some(MotionAnchor::Solid);}
            }
        }
    }
}
fn flying_rectangle_panel(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=combined_faded_panel(fitted,mode%2,w,h);add_rectangle_fly(&mut seq,mode);seq
}
fn flying_rectangle_matte(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=faded_rectangle_matte(fitted,mode%4,w,h);add_rectangle_fly(&mut seq,mode);seq
}
#[test]
fn rectangle_fly_matches_cpu_and_stays_resident() {check_rectangle_motion_fade_modes(flying_rectangle_panel,32);}
#[test]
fn rectangle_fly_mattes_match_cpu_and_stay_resident() {check_rectangle_motion_fade_modes(flying_rectangle_matte,32);}
#[test]
fn rectangle_fly_stays_resident_at_4k() {
    for direction in 0..4 {
        // The shared 4K helper takes a function pointer; each wrapper selects a direction.
        let panel:fn(bool,u8,u32,u32)->Sequence=match direction {
            0=>flying_rectangle_panel,
            1=>|f,m,w,h|flying_rectangle_panel(f,m+8,w,h),
            2=>|f,m,w,h|flying_rectangle_panel(f,m+16,w,h),
            _=>|f,m,w,h|flying_rectangle_panel(f,m+24,w,h),
        };
        check_rectangle_motion_fades_at_4k(panel);
    }
}

fn add_rectangle_bounce(seq: &mut Sequence, mode: u8) {
    use klipp_timeline::{MotionKind,TitleObjectContent};
    add_rectangle_fly(seq,mode);
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    for object in &mut title.objects {
        if matches!(object.content,TitleObjectContent::Shape(_)) {
            let motion=&mut object.fx_mut().motion;
            for m in motion.enter.iter_mut().chain(motion.exit.iter_mut()) {
                if matches!(m.kind,MotionKind::FlyLeft|MotionKind::FlyRight) {
                    m.kind=MotionKind::Bounce;
                    // At 0.2s, linear progress reaches Bounce's alpha knee.
                    if mode==0 {m.duration_secs=0.8;}
                }
            }
            if mode!=0 {
                let enter=motion.enter.as_mut().unwrap();enter.easing=enter.kind.default_easing();
                let exit=motion.exit.as_mut().unwrap();exit.easing=exit.kind.default_exit_easing();
            }
        }
    }
}
fn bouncing_rectangle_panel(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=combined_faded_panel(fitted,mode%2,w,h);add_rectangle_bounce(&mut seq,mode);seq
}
fn bouncing_rectangle_matte(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=faded_rectangle_matte(fitted,mode%4,w,h);add_rectangle_bounce(&mut seq,mode);seq
}
#[test]
fn rectangle_bounce_matches_cpu_and_stays_resident() {check_rectangle_motion_fade_modes(bouncing_rectangle_panel,8);}
#[test]
fn rectangle_bounce_mattes_match_cpu_and_stay_resident() {check_rectangle_motion_fade_modes(bouncing_rectangle_matte,8);}
#[test]
fn rectangle_bounce_stays_resident_at_4k() {check_rectangle_motion_fades_at_4k(bouncing_rectangle_panel);}

fn add_rectangle_final_motion(seq: &mut Sequence, mode: u8) {
    use klipp_timeline::{MotionKind,TitleObjectContent};
    let kinds=[MotionKind::Flicker,MotionKind::Typewriter,MotionKind::FocusIn,MotionKind::SoftRise];
    let kind=kinds[(mode/8) as usize];let case=mode%8;
    add_rectangle_fly(seq,case);
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    for object in &mut title.objects {
        if matches!(object.content,TitleObjectContent::Shape(_)) {
            let motion=&mut object.fx_mut().motion;
            for m in motion.enter.iter_mut().chain(motion.exit.iter_mut()) {
                if matches!(m.kind,MotionKind::FlyLeft|MotionKind::FlyRight) {m.kind=kind;}
            }
            // Explicitly cover simultaneous blur, translation and scaling.
            if case==7 {motion.enter.as_mut().unwrap().kind=MotionKind::FocusIn;motion.exit.as_mut().unwrap().kind=kind;}
            let enter=motion.enter.as_mut().unwrap();enter.easing=enter.kind.default_easing();
            let exit=motion.exit.as_mut().unwrap();exit.easing=exit.kind.default_exit_easing();
        }
    }
}
fn final_motion_rectangle_panel(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=combined_faded_panel(fitted,mode%2,w,h);add_rectangle_final_motion(&mut seq,mode);seq
}
fn final_motion_rectangle_matte(fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq=faded_rectangle_matte(fitted,mode%4,w,h);add_rectangle_final_motion(&mut seq,mode);seq
}
#[test]
fn rectangle_final_motions_match_cpu_and_stay_resident() {check_rectangle_motion_fade_modes(final_motion_rectangle_panel,32);}
#[test]
fn rectangle_final_motions_mattes_match_cpu_and_stay_resident() {check_rectangle_motion_fade_modes(final_motion_rectangle_matte,32);}
#[test]
fn rectangle_final_motions_stay_resident_at_4k() {
    for panel in [final_motion_rectangle_panel as fn(bool,u8,u32,u32)->Sequence,
        |f,m,w,h|final_motion_rectangle_panel(f,m+8,w,h),
        |f,m,w,h|final_motion_rectangle_panel(f,m+16,w,h),
        |f,m,w,h|final_motion_rectangle_panel(f,m+24,w,h)] {
        check_rectangle_motion_fades_at_4k(panel);
    }
}

#[test]
fn rectangle_opacity_motion_step_boundaries_match_cpu() {
    // RationalTime samples at 30 fps: use actual frames on both sides of each
    // opacity step, keeping the final sample inside the three-second clip.
    let mut frames=std::collections::BTreeSet::from([0,1,17,18,72,73,88,89]);
    for step in 1..14 {
        let frame=(18.0*f64::from(step)/14.0).floor() as i32;
        frames.extend([frame,frame+1,89-frame,90-frame]);
    }
    let times:Vec<_>=frames.into_iter().map(|f|f64::from(f)/30.0).collect();
    check_rectangle_motion_samples(|f,m,w,h|final_motion_rectangle_panel(f,if m<2 {m}else{m+6},w,h),4,&times);
}

fn add_rectangle_pop_blur_overshoot(seq: &mut Sequence, mode: u8) {
    use klipp_timeline::{MotionKind,TitleObjectContent};
    let blur=[MotionKind::Blur,MotionKind::FocusIn,MotionKind::SoftRise][(mode/2) as usize];
    add_rectangle_scale_fade(seq,6+mode%2,MotionKind::Pop);
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    for object in &mut title.objects {
        if matches!(object.content,TitleObjectContent::Shape(_)) {
            let motion=&mut object.fx_mut().motion;
            for m in motion.enter.iter_mut().chain(motion.exit.iter_mut()) {
                if m.kind==MotionKind::Blur {m.kind=blur;}
                if m.kind==MotionKind::Pop {
                    // Exercise overshoot on either side of the blur pair.
                    m.easing=klipp_core::title_motion::Easing::OutBack;
                    assert!(m.pose(m.easing.apply(0.7),1.0).3>1.0);
                }
            }
        }
    }
}
#[test]
fn rectangle_pop_blur_overshoot_pairs_stay_resident() {
    check_rectangle_motion_fade_modes(|f,m,w,h| {
        let mut seq=combined_faded_panel(f,m%2,w,h);
        add_rectangle_pop_blur_overshoot(&mut seq,m);seq
    },6);
}
#[test]
fn rectangle_pop_blur_overshoot_mattes_stay_resident() {
    check_rectangle_motion_fade_modes(|f,m,w,h| {
        let mut seq=faded_rectangle_matte(f,m%4,w,h);
        add_rectangle_pop_blur_overshoot(&mut seq,m);seq
    },6);
}

fn path_opacity_sequence(keyed: bool, mode: u8, w:u32, h:u32) -> Sequence {
    use klipp_timeline::{Motion,MotionKind,StaggerUnit,TextPath};
    let variant=mode%4;
    let mut title=TitleData::single_text_fx(TextBox {text:"Path Fade".into(),font_size:180.0,color:[190,120,60,255],
        text_path:Some(match mode/4 {
            0=>TextPath::Arc{radius:55.0,start_angle:200.0,sweep:160.0},
            1=>TextPath::Circle{radius:45.0},
            _=>TextPath::Wave{amplitude:20.0,frequency:1.5,phase:30.0},
        }),..Default::default()},LayerFx::default());
    if mode/4==3 {
        use klipp_core::vector_path::{PathContour,PathPose,PathVertex};
        let path=klipp_timeline::VectorPath{trim:None,keyframes:vec![],pose:PathPose {
            stroke_glow:None,fill:[0.0;4],stroke:[0.0;4],stroke_width:0.0,
            contours:vec![PathContour{closed:false,vertices:vec![
                PathVertex{point:[0.05,0.5],incoming:[0.05,0.5],outgoing:[0.3,0.2]},
                PathVertex{point:[0.95,0.5],incoming:[0.7,0.8],outgoing:[0.95,0.5]},
            ]}],
        }};
        let layer=title.add_object(klipp_timeline::TitleObjectContent::Shape(klipp_timeline::Shape{kind:klipp_timeline::ShapeKind::Path(path),fill:None,gradient:None,stroke:None,fit:None}),klipp_timeline::ObjectTransform{size:klipp_timeline::TitlePosition{x:1.0,y:1.0},..klipp_timeline::default_text_transform()});
        title.objects.last_mut().unwrap().visible=false;
        title.primary_text_mut().unwrap().text_path=Some(TextPath::Layer{layer,contour:0,offset:0.1,offset_keyframes:vec![key(0.0,0.0),key(3.0,0.2)],align:klipp_timeline::PathTextAlign::Start,flip:false});
    }
    title.primary_text_mut().unwrap().font_size_keyframes=vec![key(0.0,150.0),key(3.0,if keyed {210.0}else{150.0})];
    let kind=match variant {1=>MotionKind::Flicker,2=>MotionKind::Typewriter,_=>MotionKind::Fade};
    title.objects[0].fx_mut().motion=klipp_timeline::TitleMotion {
        enter:Some(Motion::new(kind).duration(0.8).stagger(StaggerUnit::Char,0.03)),
        exit:Some(Motion::new(MotionKind::Fade).duration(0.8).stagger(StaggerUnit::Word,0.03)),
    };
    if variant==3 {
        title.objects[0].fx_mut().animators=vec![klipp_timeline::Animator {props:klipp_timeline::AnimatorProps{opacity:0.4,..Default::default()},..Default::default()}];
    }
    let mut seq=Sequence::new_default();seq.resolution=Resolution{width:w,height:h};
    let mut track=Track::new("path",TrackKind::Video);
    track.clips.push(Clip{kind:ClipKind::Title(title),..Clip::gap(RationalTime::from_secs_f64(0.0,30),RationalTime::from_secs_f64(3.0,30))});
    seq.tracks=vec![track];seq
}
#[test]
fn path_opacity_matches_cpu_and_stays_resident() {check_rectangle_motion_fade_modes(path_opacity_sequence,16);}
#[test]
fn path_opacity_stays_resident_at_4k() {check_rectangle_motion_fades_at_4k(path_opacity_sequence);}

#[test]
fn path_source_restrictions_and_cpu_fade() {
    let seq=path_opacity_sequence(true,0,320,180);
    let ClipKind::Title(mut title)=seq.tracks[0].clips[0].kind.clone() else {unreachable!()};
    let mut renderer=klipp_title::TitleRenderer::new();
    let hidden=renderer.render_with_window(&title,320,180,None,0.0,Some(3.0));
    assert!(hidden.chunks_exact(4).all(|p|p[3]==0),"keyed path entrance must start hidden");
    let visible=renderer.render_with_window(&title,320,180,None,1.5,Some(3.0));
    assert!(visible.chunks_exact(4).any(|p|p[3]>0),"settled path text must be visible");
    title.objects[0].fx_mut().motion_blur=Some(klipp_timeline::MotionBlur{enabled:true,shutter_angle:180.0,shutter_angle_keyframes:vec![],samples:4});
    assert!(renderer.glyph_source(&title,320,180,0.2,Some(3.0)).is_none(),"source shutter sampling remains outside glyph poses");

}

fn path_translation_sequence(keyed: bool, mode: u8, w:u32, h:u32) -> Sequence {
    use klipp_timeline::{MotionKind,Animator,AnimatorProps};
    let mut seq=path_opacity_sequence(keyed,(mode/10)*4,w,h);
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    let kinds=[MotionKind::Rise,MotionKind::Drop,MotionKind::SlideLeft,MotionKind::SlideRight,
        MotionKind::FlyLeft,MotionKind::FlyRight,MotionKind::FlyTop,MotionKind::FlyBottom,MotionKind::Bounce];
    let case=(mode%10) as usize;
    let motion=&mut title.objects[0].fx_mut().motion;
    if case<9 {
        let enter=motion.enter.as_mut().unwrap();enter.kind=kinds[case];enter.easing=enter.kind.default_easing();enter.distance=0.6;
        let exit=motion.exit.as_mut().unwrap();exit.kind=kinds[(case+1)%9];exit.easing=exit.kind.default_exit_easing();exit.distance=0.3;
    }else{
        title.objects[0].fx_mut().animators=vec![Animator{props:AnimatorProps{dx:0.4,dy:-0.6,opacity:0.7,..Default::default()},..Default::default()}];
    }
    seq
}
#[test]
fn path_translation_matches_cpu_and_stays_resident() {
    check_rectangle_motion_fade_modes(path_translation_sequence,40);
}
#[test]
fn path_translation_stays_resident_at_4k() {check_rectangle_motion_fades_at_4k(path_translation_sequence);}
#[test]
fn path_translation_cpu_moves_glyphs() {
    let seq=path_translation_sequence(true,9,320,180);
    let ClipKind::Title(mut title)=seq.tracks[0].clips[0].kind.clone() else {unreachable!()};
    title.objects[0].fx_mut().motion=Default::default();
    let mut renderer=klipp_title::TitleRenderer::new();
    let moved=renderer.render_with_window(&title,320,180,None,1.5,Some(3.0));
    title.objects[0].fx_mut().animators[0].props.dx=0.0;
    title.objects[0].fx_mut().animators[0].props.dy=0.0;
    let rest=renderer.render_with_window(&title,320,180,None,1.5,Some(3.0));
    let centre=|pixels:&[u8]| {
        let mut total=0.0;let mut xy=[0.0;2];
        for (i,p) in pixels.chunks_exact(4).enumerate() {
            let a=f64::from(p[3]);total+=a;xy[0]+=(i%320) as f64*a;xy[1]+=(i/320) as f64*a;
        }
        assert!(total>0.0);[xy[0]/total,xy[1]/total]
    };
    let moved=centre(&moved);let rest=centre(&rest);
    assert!(moved[0]>rest[0]+1.0 && moved[1]<rest[1]-1.0,"position animator must move the path text");
    for (dx,dy) in [(-100.0,0.0),(100.0,0.0),(0.0,-100.0),(0.0,100.0)] {
        let props=&mut title.objects[0].fx_mut().animators[0].props;
        props.dx=dx;props.dy=dy;
        let offscreen=renderer.render_with_window(&title,320,180,None,1.5,Some(3.0));
        assert!(offscreen.chunks_exact(4).all(|p|p[3]==0),"fully offscreen text must be empty");
    }

}

fn path_remaining_pose_sequence(keyed:bool,mode:u8,w:u32,h:u32)->Sequence {
    use klipp_timeline::{MotionKind,Animator,AnimatorProps};
    let mut seq=path_opacity_sequence(keyed,if mode<14 {0}else{12},w,h);
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    let case=mode%14;
    let kinds=[MotionKind::ScaleUp,MotionKind::ScaleDown,MotionKind::Pop,MotionKind::Slam,MotionKind::Larger,MotionKind::Smaller,
        MotionKind::MaskRise,MotionKind::MaskDrop,MotionKind::Blur,MotionKind::FocusIn,MotionKind::SoftRise];
    if case<11 {
        let motion=&mut title.objects[0].fx_mut().motion;
        for m in motion.enter.iter_mut().chain(motion.exit.iter_mut()) {m.kind=kinds[case as usize];}
        motion.enter.as_mut().unwrap().easing=kinds[case as usize].default_easing();
        motion.exit.as_mut().unwrap().easing=kinds[case as usize].default_exit_easing();
    }else{
        title.objects[0].fx_mut().animators=vec![Animator{props:match case {
            11=>AnimatorProps{rotation:38.0,..Default::default()},
            12=>AnimatorProps{colour:Some([30,180,230,150]),opacity:0.6,..Default::default()},
            _=>AnimatorProps{dx:0.2,dy:-0.3,scale:1.4,rotation:25.0,blur:0.1,colour:Some([30,180,230,150]),opacity:0.7,backing:None},
        },..Default::default()}];
        if case==13 {title.objects[0].fx_mut().motion.enter.as_mut().unwrap().kind=MotionKind::MaskRise;}
    }
    seq
}
#[test]
fn path_remaining_poses_match_cpu_and_stay_resident() {
    check_rectangle_motion_fade_modes(path_remaining_pose_sequence,28);
}
#[test]
fn path_remaining_poses_stay_resident_at_4k() {
    check_rectangle_motion_fades_at_4k(|f,_,w,h|path_remaining_pose_sequence(f,27,w,h));
}
#[test]
fn path_remaining_poses_zero_scale_is_empty() {
    let seq=path_remaining_pose_sequence(true,5,320,180);
    let ClipKind::Title(mut title)=seq.tracks[0].clips[0].kind.clone() else {unreachable!()};
    title.objects[0].fx_mut().motion.enter.as_mut().unwrap().stagger=klipp_timeline::StaggerUnit::None;
    let mut r=klipp_title::TitleRenderer::new();
    let pixels=r.render_with_window(&title,320,180,None,0.0,Some(3.0));
    assert!(pixels.chunks_exact(4).all(|p|p[3]==0));
    let pixels=r.render_with_window(&title,320,180,None,1.5,Some(3.0));
    assert!(pixels.chunks_exact(4).any(|p|p[3]>0));
}

#[test]
fn path_remaining_poses_preserve_mixed_glyph_sizes() {
    use klipp_timeline::{Animator,AnimatorProps,StyleRun};
    let seq=path_opacity_sequence(false,0,1920,1080);
    let ClipKind::Title(mut title)=seq.tracks[0].clips[0].kind.clone() else {unreachable!()};
    let tb=title.primary_text_mut().unwrap();
    tb.text="HH".into();
    tb.font_size=180.0;
    tb.font_size_keyframes=vec![key(0.0,180.0),key(3.0,180.0)];
    tb.style_runs=vec![StyleRun{font_size:Some(360.0),..StyleRun::default_for_range(1,2)}];
    title.objects[0].fx_mut().motion=Default::default();
    let mut renderer=klipp_title::TitleRenderer::new();
    let rest=renderer.glyph_source(&title,1920,1080,1.5,Some(3.0)).unwrap();
    assert_eq!(rest.outlines.len(),2);
    for axis in 0..2 {
        assert!(rest.outlines[1].source_size[axis] as f32>1.8*rest.outlines[0].source_size[axis] as f32);
    }
    // Check actual dimensions against each glyph's own resting dimensions:
    // CPU/GPU parity alone misses a shared reset to the base text size.
    for factor in [0.5,0.99,1.01,1.5] {
        title.objects[0].fx_mut().animators=vec![Animator{props:AnimatorProps{scale:factor,..Default::default()},..Default::default()}];
        let posed=renderer.glyph_source(&title,1920,1080,1.5,Some(3.0)).unwrap();
        assert_eq!(posed.outlines.len(),2);
        for (glyph,(base,draw)) in rest.outlines.iter().zip(&posed.outlines).enumerate() {
            for axis in 0..2 {
                let expected=base.source_size[axis] as f32*factor;
                assert!((draw.source_size[axis] as f32-expected).abs()<=2.0,
                    "glyph {glyph}, axis {axis}, scale {factor}: {:?} versus resting {:?}",draw.source_size,base.source_size);
            }
        }
    }
}

fn bitmap_colour_sequence(translucent:bool,mode:u8,w:u32,h:u32)->Sequence {
    let mut title=TitleData::single_text_fx(TextBox{text:"A 🌈 B".into(),font_size:180.0,
        color:[190,120,60,if translucent {100}else{255}],..Default::default()},LayerFx::default());
    match mode {
        0=>title.objects[0].fx_mut().loops=vec![klipp_timeline::TitleLoop::ColorCycle{rate_hz:0.5}],
        1=>title.primary_text_mut().unwrap().tracking_keyframes=vec![key(0.0,0.0),key(3.0,10.0)],
        _=>{
            let tb=title.primary_text_mut().unwrap();tb.text="A 🌈\nB 🌈".into();
            tb.line_spacing_keyframes=vec![key(0.0,1.1),key(3.0,1.5)];
        }
    }
    let mut seq=Sequence::new_default();seq.resolution=Resolution{width:w,height:h};
    let mut track=Track::new("bitmap colour",TrackKind::Video);
    track.clips.push(Clip{kind:ClipKind::Title(title),..Clip::gap(RationalTime::from_secs_f64(0.0,30),RationalTime::from_secs_f64(3.0,30))});
    seq.tracks=vec![track];seq
}
#[test]
fn bitmap_colour_glyphs_match_cpu_and_stay_resident() {
    check_rectangle_motion_fade_modes(bitmap_colour_sequence,3);
}
#[test]
fn bitmap_colour_glyphs_stay_resident_at_4k() {
    check_rectangle_motion_fades_at_4k(bitmap_colour_sequence);
}
#[test]
fn bitmap_colour_glyph_cache_reuses_native_pixels_and_guards_combinations() {
    let seq=bitmap_colour_sequence(true,1,320,180);
    let ClipKind::Title(title)=seq.tracks[0].clips[0].kind.clone() else {unreachable!()};
    let mut r=klipp_title::TitleRenderer::new();
    let first=r.glyph_source(&title,320,180,0.0,Some(3.0)).expect("plain bitmap source");
    let native=first.glyphs.iter().find(|g|g.native_colour).expect("CBDT emoji font required");
    let native_id=native.id;
    assert!(first.prepared>0);
    for t in [0.7,2.6,0.0] {
        let next=r.glyph_source(&title,320,180,t,Some(3.0)).unwrap();
        assert_eq!(next.prepared,0,"tracking must reuse the fixed-size glyphs");
        let g=next.glyphs.iter().find(|g|g.native_colour).unwrap();
        assert_eq!(g.id,native_id);
        assert!(std::sync::Arc::ptr_eq(&g.pixels,&native.pixels));
    }
    for case in 0..7 {
        let mut unsupported=title.clone();
        match case {
            0=>unsupported.primary_text_mut().unwrap().style_runs=vec![klipp_timeline::StyleRun{color:Some([255,0,0,255]),..klipp_timeline::StyleRun::default_for_range(0,1)}],
            1=>unsupported.objects[0].fx_mut().motion=klipp_timeline::TitleMotion::enter(klipp_timeline::Motion::new(klipp_timeline::MotionKind::Rise)),
            2=>unsupported.objects[0].fx_mut().outlines=vec![TextOutline{enabled:true,color:[255;4],width:3.0,width_keyframes:vec![]}],
            3=>unsupported.primary_text_mut().unwrap().text_path=Some(klipp_timeline::TextPath::Circle{radius:45.0}),
            4=>unsupported.primary_text_mut().unwrap().write_on=0.5,
            5=>{
                unsupported.primary_text_mut().unwrap().underline=true;
            }
            _=>{
                unsupported.primary_text_mut().unwrap().strikethrough=true;
            }
        }
        // A rise poses the colour glyphs (#2203), and an outline, an
        // underline and a strikethrough are styled on the GPU with their
        // own coverage; the rest falls back.
        let resident=matches!(case,1|2|5|6);
        assert_eq!(r.glyph_source(&unsupported,320,180,0.3,Some(3.0)).is_some(),resident,"case {case}");
    }
}


fn bitmap_colour_overlap_sequence(translucent:bool,mode:u8,w:u32,h:u32)->Sequence {
    let mut seq=bitmap_colour_sequence(translucent,1,w,h);
    let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
    let tb=title.primary_text_mut().unwrap();
    tb.text=match mode {0=>"🌈🌈",1=>"W🌈W",2=>"🌈🔥🌈",_=>"W🌈🔥W"}.into();
    tb.tracking_keyframes=vec![key(0.0,-90.0),key(3.0,-140.0)];
    if mode==4 {title.objects[0].transform.center.x=0.0;}
    seq
}
#[test]
fn bitmap_colour_overlap_matches_cpu_and_stays_resident() {
    check_rectangle_motion_fade_modes(bitmap_colour_overlap_sequence,5);
}
#[test]
fn bitmap_colour_overlap_stays_resident_at_4k() {
    check_rectangle_motion_fades_at_4k(|t,_,w,h|bitmap_colour_overlap_sequence(t,3,w,h));
}
#[test]
fn bitmap_colour_overlap_reuses_glyphs_with_real_ink_overlap() {
    for mode in 0..4 {
        let seq=bitmap_colour_overlap_sequence(true,mode,320,180);
        let ClipKind::Title(title)=&seq.tracks[0].clips[0].kind else {unreachable!()};
        let mut r=klipp_title::TitleRenderer::new();
        let source=r.glyph_source(title,320,180,0.0,Some(3.0)).unwrap();
        assert!(source.glyphs.iter().any(|g|g.native_colour),"CBDT emoji required");
        assert!(source.glyphs.iter().enumerate().any(|(i,a)|source.glyphs.iter().skip(i+1).any(|b| {
            if !a.native_colour && !b.native_colour {return false;}
            let x0=a.origin[0].max(b.origin[0]);let y0=a.origin[1].max(b.origin[1]);
            let x1=(a.origin[0]+a.pixels.w as i32).min(b.origin[0]+b.pixels.w as i32);
            let y1=(a.origin[1]+a.pixels.h as i32).min(b.origin[1]+b.pixels.h as i32);
            (y0..y1).any(|y|(x0..x1).any(|x| {
                let alpha=|g:&klipp_title::GlyphDraw| {
                    let index=((y-g.origin[1]) as u32*g.pixels.w+(x-g.origin[0]) as u32) as usize;
                    (g.pixels.texels[index*2+1]>>16)!=0
                };
                alpha(a)&&alpha(b)
            }))
        })),"mode {mode} must exercise overlapping nonzero alpha");
        for t in [0.7,2.6,0.0] {
            let next=r.glyph_source(title,320,180,t,Some(3.0)).unwrap();
            assert_eq!(next.prepared,0);
            assert_eq!(next.glyphs.iter().map(|g|g.id).collect::<Vec<_>>(),source.glyphs.iter().map(|g|g.id).collect::<Vec<_>>());
        }
    }
}


fn bitmap_colour_size_sequence(translucent: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq = bitmap_colour_sequence(translucent, 1, w, h);
    let ClipKind::Title(title) = &mut seq.tracks[0].clips[0].kind else {
        unreachable!()
    };
    let tb = title.primary_text_mut().unwrap();
    tb.font_size_keyframes = vec![key(0.0, 80.0), key(3.0, 240.0)];
    tb.text = match mode {
        0 => "🌈",
        1 => "W 🌈 W",
        2 => "🌈🔥",
        _ => "W🌈🔥W",
    }
    .into();
    tb.tracking_keyframes = vec![
        key(0.0, if mode >= 2 { -45.0 } else { 0.0 }),
        key(3.0, if mode >= 2 { -120.0 } else { 10.0 }),
    ];
    title.objects[0].transform.size.x = 1.0;
    title.objects[0].transform.size.y = 1.0;
    seq
}
#[test]
fn bitmap_colour_size_matches_cpu_and_stays_resident() {
    check_rectangle_motion_fade_modes(bitmap_colour_size_sequence, 4);
}
#[test]
fn bitmap_colour_size_stays_resident_at_4k() {
    check_rectangle_motion_fades_at_4k(|t, _, w, h| bitmap_colour_size_sequence(t, 3, w, h));
}
#[test]
fn bitmap_colour_size_reuses_strikes_without_cpu_resizing() {
    let seq = bitmap_colour_size_sequence(true, 3, 640, 360);
    let ClipKind::Title(title) = &seq.tracks[0].clips[0].kind else {
        unreachable!()
    };
    let mut r = klipp_title::TitleRenderer::new();
    let before = klipp_title::colour_bitmap_rasterizations();
    let first = r.glyph_source(title, 640, 360, 0.0, Some(3.0)).unwrap();
    assert_eq!(first.glyphs.len(), 2, "two native colour glyphs required");
    assert_eq!(
        first.outlines.len(),
        2,
        "ordinary letters must use scalable outlines"
    );
    for t in [0.7, 2.95, 1.2, 0.0] {
        let next = r.glyph_source(title, 640, 360, t, Some(3.0)).unwrap();
        assert_eq!(
            next.prepared, 0,
            "retain strikes and outlines across size changes"
        );
        for (a, b) in first.glyphs.iter().zip(&next.glyphs) {
            assert_eq!(a.id, b.id);
            assert!(std::sync::Arc::ptr_eq(&a.pixels, &b.pixels));
            if t > 0.0 {
                assert_ne!(
                    a.target_size, b.target_size,
                    "draw dimensions must follow font size"
                );
            }
        }
    }
    assert_eq!(
        klipp_title::colour_bitmap_rasterizations(),
        before,
        "GPU layout must not resize colour pixels on CPU"
    );
}

#[test]
fn bitmap_colour_size_upscale_matches_cpu() {
    for (w, h) in [(640, 360), (160, 90)] {
        let mut seq = bitmap_colour_size_sequence(true, 0, w, h);
        let ClipKind::Title(title) = &mut seq.tracks[0].clips[0].kind else {
            unreachable!()
        };
        title.primary_text_mut().unwrap().font_size_keyframes =
            vec![key(0.0, 360.0), key(3.0, 720.0)];
        let mut layout = klipp_title::TitleRenderer::new();
        let source = layout.glyph_source(title, w, h, 2.95, Some(3.0)).unwrap();
        if w == 640 {
            assert!(
                source
                    .glyphs
                    .iter()
                    .all(|g| g.target_size.unwrap()[0] > g.pixels.w),
                "exercise enlargement beyond the source strike"
            );
        } else {
            assert!(
                source.glyphs.iter().all(|g| g.pixels.h > h),
                "source strike must exceed small preview dimensions"
            );
        }
        for ws in [
            klipp_core::WorkingColorspace::Rec709Linear,
            klipp_core::WorkingColorspace::AcesCG,
        ] {
            let mut cpu = renderer();
            cpu.set_gpu_enabled(false);
            cpu.set_working_colorspace(ws);
            let mut gpu = renderer();
            gpu.set_gpu_enabled(true);
            gpu.set_working_colorspace(ws);
            for t in [0.0, 0.7, 2.95, 0.7] {
                let at = RationalTime::from_secs_f64(t, 30);
                let (want, _) = cpu.render_inspected(
                    &seq,
                    at,
                    w,
                    h,
                    &mut NoMedia,
                    RenderOptions::default(),
                    Default::default(),
                );
                let want = klipp_color::display::display_transform_ws_dithered_to(
                    &want,
                    klipp_color::display::ToneMapOp::None,
                    ws,
                    klipp_core::SdrTransfer::Srgb,
                );
                let resized = klipp_title::colour_bitmap_rasterizations();
                let scope = klipp_effects::residency::frame("upscaled colour glyph", t);
                let (got, _) = gpu
                    .render_display_u8_on_gpu(
                        &seq,
                        at,
                        w,
                        h,
                        &mut NoMedia,
                        RenderOptions::default(),
                    )
                    .unwrap();
                drop(scope);
                assert_eq!(klipp_title::colour_bitmap_rasterizations(), resized);
                assert!(!klipp_effects::residency::last_frame()
                    .events
                    .iter()
                    .any(|e| e.work == klipp_effects::residency::Work::Fallback));
                let worst = got
                    .iter()
                    .zip(&want)
                    .map(|(a, b)| a.abs_diff(*b))
                    .max()
                    .unwrap();
                assert!(worst <= 3, "{ws:?} time {t}: {worst} levels apart");
            }
        }
    }
}


fn bitmap_colour_fit_sequence(translucent: bool, mode: u8, w: u32, h: u32) -> Sequence {
    let mut seq = bitmap_colour_sequence(translucent, 0, w, h);
    let ClipKind::Title(title) = &mut seq.tracks[0].clips[0].kind else {
        unreachable!()
    };
    let tb = title.primary_text_mut().unwrap();
    tb.text = if mode < 3 {
        "W🌈W🌈"
    } else {
        "W 🌈\nW 🌈"
    }
    .into();
    tb.font_size = 220.0;
    tb.fit = match mode % 3 {
        0 => klipp_timeline::TextFit::Wrap,
        1 => klipp_timeline::TextFit::Grow,
        _ => klipp_timeline::TextFit::Shrink { min_size: 48.0 },
    };
    let transform = &mut title.objects[0].transform;
    if mode < 3 {
        transform.size.y = 0.9;
        transform.size_x_keyframes = vec![key(0.0, 0.12), key(3.0, 0.9)];
    } else if mode < 6 {
        transform.size.x = 0.45;
        transform.size_y_keyframes = vec![key(0.0, 0.08), key(3.0, 0.9)];
    } else {
        transform.size.x = 0.12;
        transform.size.y = 0.12;
    }
    seq
}
#[test]
fn bitmap_colour_fit_matches_cpu_and_stays_resident() {
    check_rectangle_motion_fade_modes(bitmap_colour_fit_sequence, 9);
}
#[test]
fn bitmap_colour_fit_stays_resident_at_4k() {
    check_rectangle_motion_fades_at_4k(|t, _, w, h| bitmap_colour_fit_sequence(t, 2, w, h));
}
#[test]
fn bitmap_colour_fit_changes_dimensions_without_cpu_resizing() {
    for mode in [0, 1, 2, 3, 5, 6, 7, 8] {
        let seq = bitmap_colour_fit_sequence(true, mode, 640, 360);
        let ClipKind::Title(title) = &seq.tracks[0].clips[0].kind else {
            unreachable!()
        };
        let mut r = klipp_title::TitleRenderer::new();
        let before = klipp_title::colour_bitmap_rasterizations();
        let first = r.glyph_source(title, 640, 360, 0.0, Some(3.0)).unwrap();
        assert_eq!(
            first.glyphs.len(),
            2,
            "native glyphs use the retained-strike route"
        );
        assert!(first
            .glyphs
            .iter()
            .all(|g| g.native_colour && g.target_size.is_some()));
        for t in [0.7, 2.95, 0.0] {
            let next = r.glyph_source(title, 640, 360, t, Some(3.0)).unwrap();
            assert_eq!(next.prepared, 0, "mode {mode}: reuse strikes and outlines");
            for (a, b) in first.glyphs.iter().zip(&next.glyphs) {
                assert_eq!(a.id, b.id);
                assert!(std::sync::Arc::ptr_eq(&a.pixels, &b.pixels));
                if t == 2.95 && mode < 6 {
                    assert!(
                        b.target_size.unwrap()[1] > a.target_size.unwrap()[1],
                        "mode {mode}: fitting should enlarge glyphs as the box opens"
                    );
                }
            }
        }
        assert_eq!(
            klipp_title::colour_bitmap_rasterizations(),
            before,
            "mode {mode}: no CPU bitmap resize"
        );
    }
}

fn trim_path_sequence(keyed: bool) -> Sequence {
    let contour = serde_json::json!([{"closed":true,"vertices":[
        {"point":[0.8,0.5],"incoming":[0.8,0.33],"outgoing":[0.8,0.67]},
        {"point":[0.5,0.8],"incoming":[0.67,0.8],"outgoing":[0.33,0.8]},
        {"point":[0.2,0.5],"incoming":[0.2,0.67],"outgoing":[0.2,0.33]},
        {"point":[0.5,0.2],"incoming":[0.33,0.2],"outgoing":[0.67,0.2]}]}]);
    let mut path = serde_json::json!({"pose":{"contours":contour,"fill":[0.9,0.4,0.2,1.0],"stroke":[1,1,1,1],"stroke_width":4}});
    if keyed {
        path["trim"] = serde_json::json!({"range":{"start":0,"end":1,"offset":0},"keyframes":[
            {"time_secs":0,"range":{"start":0,"end":0,"offset":0},"easing":"Linear"},
            {"time_secs":3,"range":{"start":0,"end":1,"offset":0},"easing":"Linear"}]});
    }
    let mut title = TitleData::default();
    title.objects.clear();
    title.add_object(klipp_timeline::TitleObjectContent::Shape(klipp_timeline::Shape {
        kind: klipp_timeline::ShapeKind::Path(serde_json::from_value(path).unwrap()), fill: None, gradient: None, stroke: None, fit: None,
    }), klipp_timeline::ObjectTransform { size: klipp_timeline::TitlePosition { x: 0.6, y: 0.6 }, ..klipp_timeline::default_text_transform() });
    let mut seq = Sequence::new_default(); seq.resolution = Resolution { width: 320, height: 180 };
    let mut track = Track::new("path", TrackKind::Video);
    track.clips.push(Clip { kind: ClipKind::Title(title), ..Clip::gap(RationalTime::from_secs_f64(0.0, 30), RationalTime::from_secs_f64(3.0, 30)) });
    seq.tracks = vec![track]; seq
}
#[test]
fn keyed_path_geometry_is_drawn_on_the_stack_with_or_without_a_gradient() {
    let path_fallback = |seq: &Sequence, gpu: &mut Renderer, t: f64| {
        let scope = klipp_effects::residency::frame("keyed path", t);
        gpu.render_display_u8_on_gpu(seq, RationalTime::from_secs_f64(t, 30), 320, 180, &mut NoMedia, RenderOptions::default()).expect("placed path");
        drop(scope);
        klipp_effects::residency::last_frame().events.iter().any(|e| e.feature == "title.path_geometry" && e.work == klipp_effects::residency::Work::Fallback)
    };
    let mut gpu = renderer(); gpu.set_gpu_enabled(true);
    assert!(gpu.executor().gpu_pipeline_or_init().is_some(), "GPU required");
    // Solid geometry is scanned on the GPU at every pose (#2201).
    let keyed = trim_path_sequence(true);
    for t in [0.5, 1.5, 2.5] {
        assert!(!path_fallback(&keyed, &mut gpu, t), "a new trim pose at {t} is drawn on the stack");
    }
    assert_eq!(gpu.title_renderer().resting_renders(), 0, "no title raster");
    // A gradient fill too, at its box fractions (#2208).
    let mut gradient = trim_path_sequence(true);
    let ClipKind::Title(title) = &mut gradient.tracks[0].clips[0].kind else { unreachable!() };
    let klipp_timeline::TitleObjectContent::Shape(shape) = &mut title.objects[0].content else { unreachable!() };
    shape.gradient = Some(klipp_timeline::TextGradient { enabled: true, top_color: [230, 30, 50, 255], bottom_color: [20, 180, 220, 255], kind: klipp_timeline::GradientKind::Linear { angle_deg: 35.0 }, stops: vec![], stop_keyframes: vec![] });
    let mut gpu = renderer(); gpu.set_gpu_enabled(true);
    for t in [0.5, 1.5, 2.5] {
        assert!(!path_fallback(&gradient, &mut gpu, t), "a new gradient trim pose at {t} is drawn on the stack");
    }
    assert_eq!(gpu.title_renderer().resting_renders(), 0, "no title raster");
}

fn blurred_title(case: u8, w: u32, h: u32) -> Sequence {
    use klipp_timeline::{Motion, MotionBlur, MotionKind, TitleMotion};
    let blur = Some(MotionBlur { enabled: true, shutter_angle: 180.0, shutter_angle_keyframes: vec![], samples: 8 });
    let mut fx = LayerFx { motion_blur: blur, ..Default::default() };
    if case != 1 && case != 5 { fx.motion = TitleMotion::enter(Motion::new(if case == 4 { MotionKind::FlyLeft } else { MotionKind::Rise }).duration(0.8)); }
    if case == 2 { fx.outlines = vec![TextOutline { enabled: true, color: [20, 30, 40, 255], width: 4.0, width_keyframes: vec![] }]; }
    let mut title = TitleData::single_text_fx(TextBox { text: "Blur".into(), font_size: 180.0, color: [230, 190, 80, 255], ..Default::default() }, fx);
    if case == 3 {
        title.add_object(klipp_timeline::TitleObjectContent::Shape(klipp_timeline::Shape { kind: klipp_timeline::ShapeKind::Rectangle { corner_radius: 0.0 }, fill: Some([40, 80, 200, 160]), gradient: None, stroke: None, fit: None }),
            klipp_timeline::ObjectTransform { size: klipp_timeline::TitlePosition { x: 0.8, y: 0.3 }, ..klipp_timeline::default_text_transform() });
        title.objects.swap(0, 1);
    }
    // The layer moved by its own keys, its drawing otherwise still (#3160):
    // the glyph source draws it on the GPU, the shutter blurs what it is.
    if case == 5 {
        title.objects[0].transform.center_x_keyframes = vec![key(0.0, 0.4), key(3.0, 0.6)];
    }
    let mut seq = Sequence::new_default(); seq.resolution = Resolution { width: w, height: h };
    let mut track = Track::new("blur", TrackKind::Video);
    let mut clip = Clip { kind: ClipKind::Title(title), ..Clip::gap(RationalTime::from_secs_f64(0.0, 30), RationalTime::from_secs_f64(3.0, 30)) };
    // The clip moving, its drawing still: drawn once, placed at each sample.
    if case == 1 { clip = klipp_timeline::test_clip::TestClip::of(clip).lane("pos_x", 0.0, -400.0).lane("pos_x", 3.0, 400.0).build(); }
    track.clips.push(clip);
    seq.tracks = vec![track]; seq
}
#[test]
fn title_motion_blur_matches_cpu_on_the_stack() {
    let (w, h) = (320, 180);
    for case in 0..6u8 {
        let seq = blurred_title(case, w, h);
        let mut gpu = renderer(); gpu.set_gpu_enabled(true);
        assert!(gpu.executor().gpu_pipeline_or_init().is_some(), "GPU required");
        let mut cpu = renderer(); cpu.set_gpu_enabled(false);
        for t in [0.1, 0.35, 0.7, 2.0] {
            let at = RationalTime::from_secs_f64(t, 30);
            let (want, _) = cpu.render_inspected(&seq, at, w, h, &mut NoMedia, RenderOptions::default(), Default::default());
            let want = klipp_color::display::display_transform_ws_dithered_to(&want, klipp_color::display::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb);
            let before = klipp_effects::gpu::thread_readback_stats();
            let scope = klipp_effects::residency::frame("title motion blur", t);
            let (got, _) = gpu.render_display_u8_on_gpu(&seq, at, w, h, &mut NoMedia, RenderOptions::default()).expect("the stack takes a blurred title");
            drop(scope);
            let events = klipp_effects::residency::last_frame().events;
            // A text moving in a several-layer scene is drawn on the CPU and
            // uploaded while it moves (#2069 follow-up); the blur stays on the stack.
            if case != 3 {
                assert!(!events.iter().any(|e| e.work == klipp_effects::residency::Work::Fallback), "case {case} at {t}: {events:?}");
                assert_eq!(klipp_effects::gpu::thread_readback_stats().count - before.count, 1, "only verification download");
            }
            assert!(!events.iter().any(|e| e.feature.starts_with("composition.")), "case {case} at {t}: the frame left the stack: {events:?}");
            let worst = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            assert!(worst <= 3, "case {case} at {t}: {worst} levels from the CPU");
        }
    }
}
#[test]
fn title_motion_blur_stays_on_the_stack_at_4k() {
    let (w, h) = (3840, 2160);
    let seq = blurred_title(0, w, h);
    let mut gpu = renderer(); gpu.set_gpu_enabled(true);
    assert!(gpu.executor().gpu_pipeline_or_init().is_some(), "GPU required");
    for (sample, t) in [0.1, 0.25, 0.4, 0.25].into_iter().enumerate() {
        let scope = klipp_effects::residency::frame("4K title motion blur", t);
        let before = klipp_effects::gpu::thread_readback_stats();
        let started = std::time::Instant::now();
        gpu.render_display_u8_on_gpu(&seq, RationalTime::from_secs_f64(t, 30), w, h, &mut NoMedia, RenderOptions::default()).expect("the stack takes a blurred title");
        let elapsed = started.elapsed(); drop(scope);
        let events = klipp_effects::residency::last_frame().events;
        assert!(!events.iter().any(|e| e.work == klipp_effects::residency::Work::Fallback), "{events:?}");
        assert_eq!(klipp_effects::gpu::thread_readback_stats().count - before.count, 1, "only verification download");
        eprintln!("4K blurred rising title sample {sample}: {elapsed:?}, including verification download");
    }
}

fn colour_glyph_combo_sequence(_fitted: bool, mode: u8, w: u32, h: u32) -> Sequence {
    // One combination alone, to isolate it locally.
    let mode = std::env::var("KLIPP_TEST_COMBO").ok().and_then(|m| m.parse::<u8>().ok()).unwrap_or(mode);
    use klipp_timeline::{Motion, MotionKind, StaggerUnit, TitleMotion, TitleLoop, TitleShadow};
    let mut fx = LayerFx::default();
    match mode {
        0 => { fx.outlines = vec![TextOutline { enabled: true, color: [20, 20, 20, 255], width: 4.0, width_keyframes: vec![] }]; fx.loops = vec![TitleLoop::ColorCycle { rate_hz: 0.5 }]; }
        1 => fx.loops = vec![TitleLoop::ColorCycle { rate_hz: 0.5 }],
        2 => fx.motion = TitleMotion::enter(Motion::new(MotionKind::Rise).duration(0.8)),
        3 => fx.motion = TitleMotion::enter(Motion::new(MotionKind::Blur).duration(0.8).stagger(StaggerUnit::Char, 0.05)),
        4 => fx.motion = TitleMotion::enter(Motion::new(MotionKind::Rise).duration(0.8)),
        5 => {}
        9 => fx.motion = TitleMotion::enter(Motion::new(MotionKind::Rise).duration(0.8).stagger(StaggerUnit::Char, 0.05)),
        10 => fx.loops = vec![TitleLoop::Wave { amplitude: 0.15, rate_hz: 1.0 }],
        7 => { fx.glows = vec![klipp_timeline::TextGlow { enabled: true, color: [255, 220, 120, 255], radius: 8.0, radius_keyframes: vec![], intensity: 0.8, intensity_keyframes: vec![] }]; fx.loops = vec![TitleLoop::ColorCycle { rate_hz: 0.5 }]; }
        8 => { fx.inner_shadows = vec![klipp_timeline::InnerShadow { enabled: true, color: [0, 0, 0, 200], offset_x: 4.0, offset_x_keyframes: vec![], offset_y: 4.0, offset_y_keyframes: vec![], blur: 3.0, blur_keyframes: vec![] }]; fx.loops = vec![TitleLoop::ColorCycle { rate_hz: 0.5 }]; }
        _ => { fx.shadows = vec![TitleShadow { enabled: true, color: [0, 0, 0, 180], offset_x: 6.0, offset_x_keyframes: vec![], offset_y: 6.0, offset_y_keyframes: vec![], blur: 4.0, blur_keyframes: vec![] }]; fx.loops = vec![TitleLoop::ColorCycle { rate_hz: 0.5 }]; }
    }
    let mut title = TitleData::single_text_fx(TextBox { text: "A 🌈 B".into(), font_size: 180.0, color: [190, 120, 60, 255], ..Default::default() }, fx);
    let tb = title.primary_text_mut().unwrap();
    match mode {
        1 => tb.underline = true,
        4 => tb.font_size_keyframes = vec![key(0.0, 120.0), key(3.0, 220.0)],
        9 | 10 => tb.tracking_keyframes = vec![key(0.0, 0.0), key(3.0, 10.0)],
        5 => { tb.text_path = Some(klipp_timeline::TextPath::Circle { radius: 45.0 }); tb.tracking_keyframes = vec![key(0.0, 0.0), key(3.0, 10.0)]; }
        _ => {}
    }
    let mut seq = Sequence::new_default(); seq.resolution = Resolution { width: w, height: h };
    let mut track = Track::new("colour glyphs", TrackKind::Video);
    track.clips.push(Clip { kind: ClipKind::Title(title), ..Clip::gap(RationalTime::from_secs_f64(0.0, 30), RationalTime::from_secs_f64(3.0, 30)) });
    seq.tracks = vec![track]; seq
}
#[test]
fn colour_glyph_combinations_match_cpu_and_stay_resident() {
    // Outline and underline with a colour loop stay resident, and so do
    // colour glyphs that rise with keyed size or rise letter by letter
    // with keyed tracking (#2203). Shadow, glow and inner shadow read the
    // colour glyphs' coverage, which the face leaves out (#2209).
    check_rectangle_motion_samples(colour_glyph_combo_sequence, 2, &[0.0, 0.2, 0.7, 2.6]);
    check_rectangle_motion_samples(|f, _, w, h| colour_glyph_combo_sequence(f, 6, w, h), 1, &[0.0, 0.7, 2.6]);
    check_rectangle_motion_samples(|f, _, w, h| colour_glyph_combo_sequence(f, 7, w, h), 1, &[0.0, 0.7, 2.6]);
    check_rectangle_motion_samples(|f, _, w, h| colour_glyph_combo_sequence(f, 8, w, h), 1, &[0.0, 0.7, 2.6]);
    check_rectangle_motion_samples(|f, _, w, h| colour_glyph_combo_sequence(f, 10, w, h), 1, &[0.0, 0.2, 0.7, 2.6]);
    check_rectangle_motion_samples(|f, _, w, h| colour_glyph_combo_sequence(f, 4, w, h), 1, &[0.0, 0.2, 0.5, 0.7, 2.6]);
    check_rectangle_motion_samples(|f, _, w, h| colour_glyph_combo_sequence(f, 9, w, h), 1, &[0.0, 0.2, 0.5, 0.7, 1.0, 2.6]);
}
#[test]
fn outlined_colour_glyphs_in_a_colour_loop_stay_resident_at_4k() {
    check_rectangle_motion_fades_at_4k(|f, _, w, h| colour_glyph_combo_sequence(f, 0, w, h));
}
#[test]
fn colour_glyphs_under_shadows_and_inner_shadows_stay_resident_at_4k() {
    check_rectangle_motion_fades_at_4k(|f, _, w, h| colour_glyph_combo_sequence(f, 6, w, h));
    check_rectangle_motion_fades_at_4k(|f, _, w, h| colour_glyph_combo_sequence(f, 8, w, h));
}

/// A lone still path moved or faded whole by its motion and loops: rise
/// and fly, bounce and pulse, shake and blink.
fn still_path_motion_sequence(mode: u8, w: u32, h: u32) -> Sequence {
    use klipp_core::vector_path::{PathContour, PathPose, PathVertex};
    use klipp_timeline::{Motion, MotionKind, TitleLoop, TitleMotion};
    let path = klipp_timeline::VectorPath { trim: None, keyframes: vec![], pose: PathPose {
        stroke_glow: None, fill: [0.2, 0.6, 1.0, 1.0], stroke: [1.0, 0.9, 0.3, 1.0], stroke_width: 6.0,
        contours: vec![PathContour { closed: true, vertices: vec![
            PathVertex { point: [0.3, 0.3], incoming: [0.25, 0.4], outgoing: [0.4, 0.2] },
            PathVertex { point: [0.7, 0.35], incoming: [0.6, 0.2], outgoing: [0.8, 0.5] },
            PathVertex { point: [0.5, 0.75], incoming: [0.7, 0.7], outgoing: [0.3, 0.8] },
        ]}],
    }};
    let mut title = TitleData::single_text_fx(TextBox::default(), LayerFx::default());
    title.objects.clear();
    title.add_object(klipp_timeline::TitleObjectContent::Shape(klipp_timeline::Shape { kind: klipp_timeline::ShapeKind::Path(path), fill: None, gradient: None, stroke: None, fit: None }),
        klipp_timeline::ObjectTransform { size: klipp_timeline::TitlePosition { x: 1.0, y: 1.0 }, ..klipp_timeline::default_text_transform() });
    let fx = title.objects[0].fx_mut();
    match mode {
        0 => fx.motion = TitleMotion::both(Motion::new(MotionKind::Rise).duration(0.8), Motion::exit(MotionKind::FlyRight).duration(0.8)),
        1 => { fx.motion = TitleMotion::enter(Motion::new(MotionKind::Bounce).duration(1.0)); fx.loops = vec![TitleLoop::Pulse { rate_hz: 1.0, min_opacity: 0.4 }]; }
        _ => fx.loops = vec![TitleLoop::Shake { amplitude: 0.05, rate_hz: 12.0 }, TitleLoop::Blink { rate_hz: 2.0, duty: 0.7 }],
    }
    let mut seq = Sequence::new_default(); seq.resolution = Resolution { width: w, height: h };
    let mut track = Track::new("path", TrackKind::Video);
    track.clips.push(Clip { kind: ClipKind::Title(title), ..Clip::gap(RationalTime::from_secs_f64(0.0, 30), RationalTime::from_secs_f64(3.0, 30)) });
    seq.tracks = vec![track]; seq
}

/// Its resting render posed on the GPU (#2069): the export's frame, no
/// fallback, one raster for the whole clip, no preparation once warm.
#[test]
fn a_still_path_moved_whole_stays_resident() {
    let (w, h) = (320, 180);
    for mode in 0..3 {
        for ws in [klipp_core::WorkingColorspace::Rec709Linear, klipp_core::WorkingColorspace::AcesCG] {
            let seq = still_path_motion_sequence(mode, w, h);
            let mut cpu = renderer(); cpu.set_gpu_enabled(false); cpu.set_working_colorspace(ws);
            let mut gpu = renderer(); gpu.set_gpu_enabled(true); gpu.set_working_colorspace(ws);
            assert!(gpu.executor().gpu_pipeline_or_init().is_some(), "GPU required");
            for (sample, t) in [0.1, 0.35, 0.6, 1.4, 2.4, 2.75, 0.35].into_iter().enumerate() {
                let at = RationalTime::from_secs_f64(t, 30);
                let (want, _) = cpu.render_inspected(&seq, at, w, h, &mut NoMedia, RenderOptions::default(), Default::default());
                let want = klipp_color::display::display_transform_ws_dithered_to(&want, klipp_color::display::ToneMapOp::None, ws, klipp_core::SdrTransfer::Srgb);
                let before = klipp_effects::gpu::thread_readback_stats();
                let scope = klipp_effects::residency::frame("posed still path", t);
                let (got, _) = gpu.render_display_u8_on_gpu(&seq, at, w, h, &mut NoMedia, RenderOptions::default()).expect("GPU frame");
                drop(scope);
                let events = klipp_effects::residency::last_frame().events;
                assert!(!events.iter().any(|e| e.work == klipp_effects::residency::Work::Fallback), "mode={mode} t={t}: {events:?}");
                assert_eq!(klipp_effects::gpu::thread_readback_stats().count - before.count, 1, "only verification download");
                if sample > 0 { assert!(!events.iter().any(|e| e.work == klipp_effects::residency::Work::Preparation), "mode={mode} t={t} warm: {events:?}"); }
                let worst = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
                assert!(worst <= 1, "mode={mode} {ws:?} t={t}: {worst} levels apart");
            }
            assert_eq!(gpu.title_renderer().resting_renders(), 1, "mode={mode}: one resting raster for the clip");
        }
    }
}

#[test]
fn a_still_path_moved_whole_stays_resident_at_4k() {
    let (w, h) = (3840, 2160);
    for mode in 0..3 {
        let seq = still_path_motion_sequence(mode, w, h);
        let mut gpu = renderer(); gpu.set_gpu_enabled(true);
        assert!(gpu.executor().gpu_pipeline_or_init().is_some(), "GPU required");
        for (sample, t) in [0.1, 0.35, 0.6, 1.4, 2.4, 2.75].into_iter().enumerate() {
            let scope = klipp_effects::residency::frame("4K posed still path", t);
            let started = std::time::Instant::now();
            gpu.render_display_u8_on_gpu(&seq, RationalTime::from_secs_f64(t, 30), w, h, &mut NoMedia, RenderOptions::default()).expect("GPU frame");
            let elapsed = started.elapsed(); drop(scope);
            let events = klipp_effects::residency::last_frame().events;
            assert!(!events.iter().any(|e| e.work == klipp_effects::residency::Work::Fallback), "{events:?}");
            eprintln!("4K posed still path mode {mode} sample {sample}: {elapsed:?}, including verification download");
        }
    }
}
#[test]
fn overlapping_colour_glyphs_rising_apart_keep_the_cpu() {
    // A colour glyph is drawn as far as the whole mask reaches, so where a
    // letter rising at another alpha overlaps it the GPU's own posed alpha
    // is not enough: those frames keep the CPU, and the rest stay resident.
    use klipp_timeline::{Motion, MotionKind, StaggerUnit, TitleMotion};
    let (w,h)=(320,180);
    for mode in [1u8,3] {
        let mut seq=bitmap_colour_overlap_sequence(false,mode,w,h);
        let ClipKind::Title(title)=&mut seq.tracks[0].clips[0].kind else {unreachable!()};
        title.objects[0].fx_mut().motion=TitleMotion::enter(Motion::new(MotionKind::Rise).duration(0.8).stagger(StaggerUnit::Char,0.15));
        let mut cpu=renderer(); cpu.set_gpu_enabled(false);
        let mut gpu=renderer(); gpu.set_gpu_enabled(true);
        assert!(gpu.executor().gpu_pipeline_or_init().is_some(), "GPU required");
        for (t,staggered) in [(0.3,true),(0.5,true),(2.0,false)] {
            let at=RationalTime::from_secs_f64(t,30);
            let (want,_)=cpu.render_inspected(&seq,at,w,h,&mut NoMedia,RenderOptions::default(),Default::default());
            let want=klipp_color::display::display_transform_ws_dithered_to(&want,klipp_color::display::ToneMapOp::None,klipp_core::WorkingColorspace::Rec709Linear,klipp_core::SdrTransfer::Srgb);
            let scope=klipp_effects::residency::frame("overlapping colour glyphs",t);
            let (got,_)=gpu.render_display_u8_on_gpu(&seq,at,w,h,&mut NoMedia,RenderOptions::default()).expect("GPU frame");
            drop(scope);
            let fallback=klipp_effects::residency::last_frame().events.iter().any(|e|e.work==klipp_effects::residency::Work::Fallback);
            assert_eq!(fallback,staggered,"mode {mode} at {t}");
            let worst=got.iter().zip(&want).map(|(a,b)|a.abs_diff(*b)).max().unwrap();
            assert!(worst<=1,"mode {mode} at {t}: {worst} levels from the CPU");
        }
    }
}

/// A lone vector path whose geometry or placement changes over the clip
/// (#2201): morph keys, trim keys on a closed path wrapping its offset and
/// on an open one, a trim with a stroke glow, a self-crossing even-odd
/// fill, an open stroke alone, keyed centre and rotation, a keyed trim
/// with a glow rising in, and a still glowing path rising in.
fn gpu_path_sequence(case: u8, w: u32, h: u32) -> Sequence {
    use serde_json::json;
    let v = |p: [f32; 2]| json!({"point": p, "incoming": p, "outgoing": p});
    let blob = json!([{"closed": true, "vertices": [
        {"point": [0.8, 0.5], "incoming": [0.8, 0.33], "outgoing": [0.8, 0.67]},
        {"point": [0.5, 0.8], "incoming": [0.67, 0.8], "outgoing": [0.33, 0.8]},
        {"point": [0.2, 0.5], "incoming": [0.2, 0.67], "outgoing": [0.2, 0.33]},
        {"point": [0.5, 0.2], "incoming": [0.33, 0.2], "outgoing": [0.67, 0.2]}]}]);
    let bowtie = json!([{"closed": true, "vertices": [v([0.2, 0.2]), v([0.8, 0.8]), v([0.8, 0.2]), v([0.2, 0.8])]}]);
    let open = json!([{"closed": false, "vertices": [
        {"point": [0.1, 0.7], "incoming": [0.1, 0.7], "outgoing": [0.3, 0.1]},
        {"point": [0.9, 0.4], "incoming": [0.6, 0.9], "outgoing": [0.9, 0.4]}]}]);
    let pose = |contours: &serde_json::Value, fill: [f32; 4]| json!({"contours": contours, "fill": fill, "stroke": [1.0, 0.95, 0.7, 1.0], "stroke_width": 5});
    let fill = [0.9, 0.4, 0.2, 1.0];
    let none = [0.0; 4];
    let trim = |range: [f32; 3], keyed: [f32; 3]| json!({"range": {"start": range[0], "end": range[1], "offset": range[2]}, "keyframes": [
        {"time_secs": 0, "range": {"start": range[0], "end": range[1], "offset": range[2]}, "easing": "Linear"},
        {"time_secs": 3, "range": {"start": keyed[0], "end": keyed[1], "offset": keyed[2]}, "easing": "Linear"}]});
    let mut path = match case {
        0 | 11 => json!({"pose": pose(&blob, fill), "keyframes": [
            {"time_secs": 0, "pose": pose(&blob, fill), "easing": "Linear"},
            {"time_secs": 3, "pose": pose(&bowtie, fill), "easing": "Linear"}]}),
        1 | 3 | 8 | 10 => json!({"pose": pose(&blob, fill), "trim": trim([0.0, 0.6, 0.7], [0.0, 0.6, 1.6])}),
        2 => json!({"pose": pose(&open, none), "trim": trim([0.0, 0.0, 0.2], [0.0, 1.0, 0.2])}),
        4 => json!({"pose": pose(&bowtie, fill)}),
        5 => json!({"pose": pose(&open, none)}),
        _ => json!({"pose": pose(&blob, fill)}),
    };
    if matches!(case, 3 | 8 | 9) {
        path["pose"]["stroke_glow"] = json!({"radius": 6.0, "strength": 0.8});
    }
    let mut transform = klipp_timeline::ObjectTransform { size: klipp_timeline::TitlePosition { x: 0.6, y: 0.6 }, ..klipp_timeline::default_text_transform() };
    match case {
        6 => transform.center_x_keyframes = vec![key(0.0, 0.3), key(3.0, 0.7)],
        7 | 12 => transform.rotation_keyframes = vec![key(0.0, 0.0), key(3.0, 90.0)],
        _ => {}
    }
    let mut title = TitleData::default();
    title.objects.clear();
    let id = title.add_object(klipp_timeline::TitleObjectContent::Shape(klipp_timeline::Shape {
        kind: klipp_timeline::ShapeKind::Path(serde_json::from_value(path).unwrap()), fill: None, gradient: None, stroke: None, fit: None,
    }), transform);
    // Gradient fills over the box, evaluated per pixel (#2208).
    if case >= 10 {
        let klipp_timeline::TitleObjectContent::Shape(shape) = &mut title.object_mut(id).unwrap().content else { unreachable!() };
        let stop = |position: f32, color: [u8; 4]| klipp_timeline::GradientStop { position, color };
        shape.gradient = Some(klipp_timeline::TextGradient { enabled: true, top_color: [230, 30, 50, 255], bottom_color: [20, 180, 220, 255],
            kind: if case == 11 { klipp_timeline::GradientKind::Radial { centre: [0.4, 0.55], radius: 0.6 } } else { klipp_timeline::GradientKind::Linear { angle_deg: 35.0 } },
            stops: if case == 11 { vec![stop(0.0, [250, 220, 60, 255]), stop(0.5, [200, 40, 120, 140]), stop(1.0, [30, 60, 200, 255])] } else { vec![] },
            stop_keyframes: vec![] });
    }
    if matches!(case, 8 | 9) {
        title.object_mut(id).unwrap().fx_mut().motion = klipp_timeline::TitleMotion::enter(klipp_timeline::Motion::new(klipp_timeline::MotionKind::Rise).duration(0.8));
    }
    let mut seq = Sequence::new_default(); seq.resolution = Resolution { width: w, height: h };
    let mut track = Track::new("path", TrackKind::Video);
    track.clips.push(Clip { kind: ClipKind::Title(title), ..Clip::gap(RationalTime::from_secs_f64(0.0, 30), RationalTime::from_secs_f64(3.0, 30)) });
    seq.tracks = vec![track]; seq
}
#[test]
fn vector_paths_are_drawn_on_the_stack_as_the_cpu_paints_them() {
    let (w, h) = (320, 180);
    for case in 0..13u8 {
        let seq = gpu_path_sequence(case, w, h);
        for ws in [klipp_core::WorkingColorspace::Rec709Linear, klipp_core::WorkingColorspace::AcesCG] {
            let mut cpu = renderer(); cpu.set_gpu_enabled(false); cpu.set_working_colorspace(ws);
            let mut gpu = renderer(); gpu.set_gpu_enabled(true); gpu.set_working_colorspace(ws);
            assert!(gpu.executor().gpu_pipeline_or_init().is_some(), "GPU required");
            for t in [0.0, 0.4, 1.1, 2.3] {
                let at = RationalTime::from_secs_f64(t, 30);
                let (want, _) = cpu.render_inspected(&seq, at, w, h, &mut NoMedia, RenderOptions::default(), Default::default());
                let want = klipp_color::display::display_transform_ws_dithered_to(&want, klipp_color::display::ToneMapOp::None, ws, klipp_core::SdrTransfer::Srgb);
                let before = klipp_effects::gpu::thread_readback_stats();
                let scope = klipp_effects::residency::frame("vector path", t);
                let (got, _) = gpu.render_display_u8_on_gpu(&seq, at, w, h, &mut NoMedia, RenderOptions::default()).expect("GPU frame");
                drop(scope);
                let events = klipp_effects::residency::last_frame().events;
                assert!(!events.iter().any(|e| e.work == klipp_effects::residency::Work::Fallback), "case {case} at {t}: {events:?}");
                assert_eq!(klipp_effects::gpu::thread_readback_stats().count - before.count, 1, "only verification download");
                let worst = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
                if worst > 1 {
                    let i = got.iter().zip(&want).position(|(a, b)| a.abs_diff(*b) > 1).unwrap() / 4;
                    eprintln!("case {case} {ws:?} at {t}: first pixel {},{} got {:?}, want {:?}", i % w as usize, i / w as usize, &got[i * 4..i * 4 + 4], &want[i * 4..i * 4 + 4]);
                }
                // Keyed geometry rising in is quantised, moved and faded as
                // the stack moves a rectangle, which lands dim glow pixels up
                // to three levels from the CPU's posed byte sprite mid-fade,
                // as the rectangle motion tests allow; at rest it is exact.
                let tolerance = if case == 8 && t < 0.8 { 3 } else { 1 };
                assert!(worst <= tolerance, "case {case} {ws:?} at {t}: {worst} levels from the CPU");
            }
            // The still path rising in is its resting render posed (#2204).
            assert_eq!(gpu.title_renderer().resting_renders(), u64::from(case == 9), "case {case}: title rasters");
        }
    }
}
#[test]
fn vector_paths_changing_shape_stay_resident_at_4k() {
    let (w, h) = (3840, 2160);
    for case in [0u8, 3] {
        let seq = gpu_path_sequence(case, w, h);
        let mut gpu = renderer(); gpu.set_gpu_enabled(true);
        assert!(gpu.executor().gpu_pipeline_or_init().is_some(), "GPU required");
        // Every time is a new pose: the geometry is scanned again each frame.
        for t in [0.3, 0.9, 1.6, 2.4] {
            let scope = klipp_effects::residency::frame("4K vector path", t);
            let before = klipp_effects::gpu::thread_readback_stats();
            let started = std::time::Instant::now();
            gpu.render_display_u8_on_gpu(&seq, RationalTime::from_secs_f64(t, 30), w, h, &mut NoMedia, RenderOptions::default()).expect("the stack takes a path");
            let elapsed = started.elapsed(); drop(scope);
            let events = klipp_effects::residency::last_frame().events;
            assert!(!events.iter().any(|e| e.work == klipp_effects::residency::Work::Fallback), "case {case} at {t}: {events:?}");
            assert_eq!(klipp_effects::gpu::thread_readback_stats().count - before.count, 1, "only verification download");
            eprintln!("4K path case {case} at {t}: {elapsed:?}, including verification download");
        }
        assert_eq!(gpu.title_renderer().resting_renders(), 0, "case {case}: no title raster");
    }
}
