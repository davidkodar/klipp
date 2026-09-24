use klipp_timeline::{Shape, ShapeKind, TitleData, TitleObjectContent, TitlePosition, VectorPath};
use klipp_title::TitleRenderer;
fn title(path: VectorPath) -> TitleData {
    let mut td = TitleData::default();
    td.objects.clear();
    td.add_object(
        TitleObjectContent::Shape(Shape {
            kind: ShapeKind::Path(path),
            fill: None,
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
