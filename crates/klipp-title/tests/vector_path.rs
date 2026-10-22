use klipp_timeline::{Shape, ShapeKind, TitleData, TitleObjectContent, TitlePosition, VectorPath};
use klipp_title::TitleRenderer;
fn title(path: VectorPath) -> TitleData {
    let mut td = TitleData::default();
    td.objects.clear();
    td.add_object(
        TitleObjectContent::Shape(Shape {
            kind: ShapeKind::Path(path),
            fill: None,
            gradient: None,
            stroke: None,
            fit: None,
        }),
        klipp_timeline::ObjectTransform {
            center: TitlePosition { x: 0.5, y: 0.5 },
            size: TitlePosition { x: 1.0, y: 1.0 },
            ..klipp_timeline::default_text_transform()
        },
    );
    td
}
#[test]
fn geometry_and_paint_change_without_stale_raster_cache() {
    let mut p = VectorPath::default();
    p.pose.stroke_width = 0.0;
    p.pose.fill = [1.0, 0.0, 0.0, 1.0];
    p.key(0.0, p.pose.clone()).unwrap();
    let mut end = p.pose.clone();
    for v in &mut end.contours[0].vertices {
        for a in [&mut v.point, &mut v.incoming, &mut v.outgoing] {
            a[0] = 0.5 + (a[0] - 0.5) * 0.5;
        }
    }
    end.fill = [0.0, 0.0, 1.0, 1.0];
    p.key(1.0, end).unwrap();
    let td = title(p);
    let mut r = TitleRenderer::new();
    let a = r.render_with_time(&td, 160, 90, None, 0.0);
    let b = r.render_with_time(&td, 160, 90, None, 1.0);
    assert_ne!(a, b);
    let ink = |p: &[u8]| p.chunks_exact(4).filter(|v| v[3] > 128).count();
    assert!((ink(&a) as f32 / ink(&b) as f32 - 2.0).abs() < 0.1);
    assert_eq!(r.render_with_time(&td, 160, 90, None, 0.0), a);
}
#[test]
fn curved_open_strokes_have_antialiasing_and_scale_with_output() {
    let mut p = VectorPath::default();
    p.pose.contours[0].closed = false;
    p.pose.contours[0].vertices.truncate(2);
    p.pose.contours[0].vertices[0].outgoing = [0.2, 0.9];
    p.pose.contours[0].vertices[1].incoming = [0.8, 0.9];
    p.pose.fill = [1.0; 4];
    p.pose.stroke_width = 18.0;
    let td = title(p);
    let mut r = TitleRenderer::new();
    let a = r.render_with_time(&td, 320, 180, None, 0.0);
    let b = r.render_with_time(&td, 640, 360, None, 0.0);
    let area = |p: &[u8]| p.chunks_exact(4).map(|p| p[3] as f64 / 255.0).sum::<f64>();
    assert!(a.chunks_exact(4).any(|p| p[3] > 0 && p[3] < 255));
    assert!((area(&b) / area(&a) - 4.0).abs() < 0.2);
    assert!(
        area(&a) < 320.0 * 180.0 * 0.1,
        "open contours must not fill"
    );
}

#[test]
fn trim_keys_bypass_cache_and_glow_only_follows_revealed_stroke() {
    let mut p = VectorPath::default();
    p.pose.fill = [0.0; 4];
    p.pose.stroke_width = 24.0;
    p.pose.stroke_glow = Some(klipp_timeline::StrokeGlow {
        radius: 32.0,
        strength: 1.0,
    });
    let mut trim = klipp_timeline::PathTrim::default();
    trim.key(
        0.0,
        klipp_timeline::TrimRange {
            end: 0.0,
            ..Default::default()
        },
    )
    .unwrap();
    trim.key(1.0, klipp_timeline::TrimRange::default()).unwrap();
    p.trim = Some(trim);
    let mut renderer = TitleRenderer::new();
    let td = title(p.clone());
    let empty = renderer.render_with_time(&td, 320, 180, None, 0.0);
    assert!(empty.chunks_exact(4).all(|p| p[3] == 0));
    let full = renderer.render_with_time(&td, 320, 180, None, 1.0);
    let partial = renderer.render_with_time(&td, 320, 180, None, 0.25);
    let area = |p: &[u8]| p.chunks_exact(4).map(|p| p[3] as f64).sum::<f64>();
    assert!(area(&full) > area(&partial) * 2.0);
    // First quarter is the top edge; the bottom edge has neither stroke nor halo.
    assert!(partial[(155 * 320 + 160) * 4 + 3] == 0);
    p.pose.stroke_glow.as_mut().unwrap().strength = 0.0;
    let sharp = renderer.render_with_time(&title(p), 320, 180, None, 0.25);
    assert!(area(&partial) > area(&sharp));
    assert_eq!(renderer.render_with_time(&td, 320, 180, None, 0.0), empty);
}

#[test]
fn trim_never_erases_the_fill() {
    let mut p = VectorPath::default();
    p.pose.stroke = [0.0; 4];
    p.pose.fill = [1.0, 0.0, 0.0, 1.0];
    let mut renderer = TitleRenderer::new();
    let original = renderer.render_with_time(&title(p.clone()), 160, 90, None, 0.0);
    p.trim = Some(klipp_timeline::PathTrim {
        range: klipp_timeline::TrimRange {
            end: 0.0,
            ..Default::default()
        },
        ..Default::default()
    });
    assert_eq!(
        renderer.render_with_time(&title(p), 160, 90, None, 0.0),
        original
    );
}

#[test]
fn untrimmed_dots_survive_and_subpixel_glow_does_not_thicken_edges() {
    let mut p = VectorPath::default();
    p.pose.fill = [0.0; 4];
    p.pose.stroke_width = 20.0;
    p.pose.contours[0].closed = false;
    p.pose.contours[0].vertices.truncate(2);
    for v in &mut p.pose.contours[0].vertices {
        v.point = [0.5, 0.5];
        v.incoming = v.point;
        v.outgoing = v.point;
    }
    let mut renderer = TitleRenderer::new();
    let original = renderer.render_with_time(&title(p.clone()), 320, 180, None, 0.0);
    assert!(original.chunks_exact(4).any(|p| p[3] > 0));
    p.pose.stroke_glow = Some(klipp_timeline::StrokeGlow {
        radius: 2.0,
        strength: 1.0,
    });
    assert_eq!(
        renderer.render_with_time(&title(p), 320, 180, None, 0.0),
        original
    );
}

/// A still path moved or faded whole — risen in, flown out, pulsed,
/// shaken — is its resting render posed: one raster for every frame,
/// and the frames the same to the byte as the path rastered afresh at
/// each time (#2069), which a constant opacity key asks for.
#[test]
fn a_still_path_moved_whole_is_its_resting_render_posed() {
    use klipp_timeline::{Motion, MotionKind, TitleLoop, TitleMotion};
    let mut path = VectorPath::default();
    path.pose.fill = [0.2, 0.6, 1.0, 1.0];
    path.pose.stroke_width = 6.0;
    let cases = [
        (TitleMotion::both(Motion::new(MotionKind::Rise).duration(1.0), Motion::exit(MotionKind::FlyRight).duration(0.8)), vec![]),
        (TitleMotion::enter(Motion::new(MotionKind::Bounce).duration(1.2)), vec![TitleLoop::Pulse { rate_hz: 1.0, min_opacity: 0.4 }]),
        (TitleMotion::default(), vec![TitleLoop::Shake { amplitude: 0.05, rate_hz: 12.0 }, TitleLoop::Blink { rate_hz: 2.0, duty: 0.7 }]),
    ];
    for (motion, loops) in cases {
        let mut td = title(path.clone());
        let fx = td.objects[0].fx_mut();
        fx.motion = motion;
        fx.loops = loops;
        let mut keyed = td.clone();
        keyed.objects[0].opacity_keyframes = vec![klipp_core::Keyframe {
            time_secs: 0.0,
            value: 1.0,
            interpolation: klipp_core::Interpolation::Linear,
            tangent_out: (0.0, 0.0),
            tangent_in: (0.0, 0.0),
            handles: Default::default(),
        }];
        let mut placed = TitleRenderer::new();
        let mut rastered = TitleRenderer::new();
        for t in [0.0f32, 0.15, 0.4, 0.7, 1.1, 2.0, 3.5, 3.9] {
            assert!(matches!(placed.frame_with_window(&td, 320, 180, None, t, Some(4.0)), klipp_title::TitleFrame::Placed { .. }), "at {t} s the path is placed");
            let got = placed.render_with_window(&td, 320, 180, None, t, Some(4.0));
            let want = rastered.render_with_window(&keyed, 320, 180, None, t, Some(4.0));
            let off = got.iter().zip(&want).filter(|(a, b)| a != b).count();
            assert_eq!(off, 0, "at {t} s the posed path differs from its raster in {off} bytes");
        }
        assert_eq!(placed.resting_renders(), 1, "one resting render posed at every time");
    }
}
