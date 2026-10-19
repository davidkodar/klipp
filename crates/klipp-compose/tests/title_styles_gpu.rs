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
            // The viewer's CPU encode, dithered as the GPU stack's
            // display encode is (#2072): like with like.
            let expected = klipp_color::display::display_transform_ws_dithered_to(
                &expected,
                klipp_color::display::ToneMapOp::None,
                klipp_core::WorkingColorspace::Rec709Linear,
                klipp_core::SdrTransfer::Srgb,
            );
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
    for ws in [WorkingColorspace::Rec709Linear, WorkingColorspace::AcesCG] {
        for graded in [false, true] {
          for variant in 0..38 {
            let mut title = TitleData::single_text_fx(TextBox {
                text: "Glitch".into(), font_size: 180.0, color: [190, 120, 60, 255],
                glitch_intensity: 0.6, chromatic_aberration_px: 12.0, ..Default::default()
            }, LayerFx {
                outlines: vec![TextOutline { enabled: true, color: [60, 180, 230, 255], width: 3.0, width_keyframes: vec![] }],
                ..Default::default()
            });
            if variant==6 { title.objects[0].fx_mut().edge_fade=klipp_core::title_motion::EdgeFade { left:0.45,right:0.4,top:0.4,bottom:0.4 }; }
            if variant>=9 && variant<12 { let text=title.primary_text_mut().unwrap(); text.glitch_intensity=0.0; text.chromatic_aberration_px=0.0; text.font_size_keyframes=vec![key(0.0,120.0),key(1.0,220.0)]; text.vertical_layout=variant==10; }
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
            if variant==36 {title.objects[0].fx_mut().animators[0].props.rotation=90.0;}
            if variant==37 {let text=title.primary_text_mut().unwrap();text.glitch_intensity=0.6;text.chromatic_aberration_px=12.0;}
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
                let before = klipp_effects::gpu::readback_stats();
                let scope = klipp_effects::residency::frame("test", t);
                let (got, _) = gpu.render_display_u8_on_gpu(&seq, at, w, h, &mut NoMedia, RenderOptions::default()).expect("resident title");
                drop(scope);
                let events = klipp_effects::residency::last_frame().events;
                assert!(!events.iter().any(|e| e.work == klipp_effects::residency::Work::Fallback), "{events:?}");
                assert_eq!(klipp_effects::gpu::readback_stats().count-before.count,1,"only verification download");
                let worst = got.iter().zip(&want).map(|(a,b)| a.abs_diff(*b)).max().unwrap();
                assert!(worst<=3,"{ws:?} graded={graded} variant={variant} at{t}: {worst}");
                assert_eq!(gpu.title_renderer().resting_renders(),u64::from(variant==0 || variant==6),"cached sources, not full per-frame rasters");
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
