//! #1508: title text follows a drawn path, as the path is drawn at each
//! moment.

use klipp_core::vector_path::{PathContour, PathPose, PathVertex};
use klipp_timeline::{
    ObjectTransform, PathTextAlign, Shape, ShapeKind, TextBox, TextPath, TitleData, TitleObjectContent, TitleObjectId, TitlePosition, VectorPath,
};
use klipp_title::TitleRenderer;

const W: u32 = 640;
const H: u32 = 360;

/// An open contour through `points` (box fractions), each a vertex with
/// its handles a third of the way to its neighbours: a smooth curve.
fn smooth(points: &[[f32; 2]], stroke: f32) -> PathPose {
    let n = points.len();
    let vertices = (0..n)
        .map(|i| {
            let p = points[i];
            let prev = points[i.saturating_sub(1)];
            let next = points[(i + 1).min(n - 1)];
            let (dx, dy) = ((next[0] - prev[0]) / 6.0, (next[1] - prev[1]) / 6.0);
            PathVertex { point: p, incoming: [p[0] - dx, p[1] - dy], outgoing: [p[0] + dx, p[1] + dy] }
        })
        .collect();
    PathPose { stroke_glow: None, contours: vec![PathContour { closed: false, vertices }], fill: [0.0; 4], stroke: [1.0, 1.0, 1.0, stroke], stroke_width: if stroke > 0.0 { 3.0 } else { 0.0 } }
}

fn s_curve(y: f32, stroke: f32) -> PathPose {
    smooth(&[[0.1, y], [0.35, y - 0.2], [0.65, y + 0.2], [0.9, y]], stroke)
}

/// A title with a path layer (`path`) and, unless `text` is empty, a
/// text on it set by `on`.
fn title(path: VectorPath, text: &str, on: impl Fn(TitleObjectId) -> TextPath, tracking: f32) -> TitleData {
    let mut td = TitleData::default();
    td.objects.clear();
    let whole = ObjectTransform { center: TitlePosition { x: 0.5, y: 0.5 }, size: TitlePosition { x: 1.0, y: 1.0 }, ..klipp_timeline::default_text_transform() };
    let layer = td.add_object(TitleObjectContent::Shape(Shape { kind: ShapeKind::Path(path), fill: None, gradient: None, stroke: None, fit: None }), whole);
    if !text.is_empty() {
        let tb = TextBox { text: text.into(), font_size: 120.0, tracking, text_path: Some(on(layer)), ..Default::default() };
        td.add_object(TitleObjectContent::TextBox(tb), klipp_timeline::default_text_transform());
    }
    td
}

fn on(offset: f32) -> impl Fn(TitleObjectId) -> TextPath {
    move |layer| TextPath::Layer { layer, contour: 0, offset, offset_keyframes: Vec::new(), align: PathTextAlign::Start, flip: false }
}

/// The inked pixels of a frame, as (x, y).
fn ink(px: &[u8]) -> Vec<(f32, f32)> {
    px.chunks_exact(4).enumerate().filter(|(_, p)| p[3] > 100).map(|(i, _)| ((i as u32 % W) as f32, (i as u32 / W) as f32)).collect()
}

fn centroid(points: &[(f32, f32)]) -> (f32, f32) {
    let n = points.len().max(1) as f32;
    (points.iter().map(|p| p.0).sum::<f32>() / n, points.iter().map(|p| p.1).sum::<f32>() / n)
}

/// The text on a drawn S-curve follows it: every inked pixel lies within
/// a letter's height of the curve, and the letters run along it.
#[test]
fn text_follows_a_drawn_s_curve() {
    let mut r = TitleRenderer::new();
    let curve = ink(&r.render_with_time(&title(VectorPath { trim: None, pose: s_curve(0.5, 1.0), keyframes: Vec::new() }, "", on(0.0), 0.0), W, H, None, 0.0));
    let text = ink(&r.render_with_time(&title(VectorPath { trim: None, pose: s_curve(0.5, 0.0), keyframes: Vec::new() }, "FOLLOW THE CURVE", on(0.05), 0.0), W, H, None, 0.0));
    assert!(curve.len() > 200 && text.len() > 200, "{} {}", curve.len(), text.len());
    let near = |p: &(f32, f32)| curve.iter().map(|c| (c.0 - p.0).powi(2) + (c.1 - p.1).powi(2)).fold(f32::MAX, f32::min).sqrt();
    let far = text.iter().map(near).fold(0.0, f32::max);
    assert!(far < 40.0, "every letter stands on the curve: the farthest ink is {far} px off");
    let (xs, ys): (Vec<f32>, Vec<f32>) = text.iter().copied().unzip();
    let span = |v: &[f32]| v.iter().copied().fold(f32::MIN, f32::max) - v.iter().copied().fold(f32::MAX, f32::min);
    assert!(span(&xs) > 250.0 && span(&ys) > 60.0, "the letters run along it, up and down: {} x {}", span(&xs), span(&ys));
}

/// The path's morph keys move the text with the path: drawn together,
/// the text keeps its place against the drawn curve at every moment.
#[test]
fn text_moves_with_the_paths_morph_and_never_drifts_from_it() {
    let keyed = |stroke: f32| {
        let mut p = VectorPath { trim: None, pose: s_curve(0.3, stroke), keyframes: Vec::new() };
        p.key(0.0, s_curve(0.3, stroke)).unwrap();
        p.key(2.0, s_curve(0.7, stroke)).unwrap();
        p
    };
    let mut r = TitleRenderer::new();
    let (mut gaps, mut heights) = (Vec::new(), Vec::new());
    for t in [0.0, 1.0, 2.0] {
        let curve = centroid(&ink(&r.render_with_time(&title(keyed(1.0), "", on(0.0), 0.0), W, H, None, t)));
        let text = centroid(&ink(&r.render_with_time(&title(keyed(0.0), "MORPHING", on(0.2), 0.0), W, H, None, t)));
        gaps.push((text.0 - curve.0, text.1 - curve.1));
        heights.push(text.1);
    }
    assert!(heights[2] - heights[0] > 0.3 * H as f32, "the text goes down with the path: {heights:?}");
    for g in &gaps {
        assert!((g.0 - gaps[0].0).abs() < 1.5 && (g.1 - gaps[0].1).abs() < 1.5, "the text keeps its place against the drawn path: {gaps:?}");
    }
}

/// The offset keys make the text travel along the path.
#[test]
fn keyed_offset_makes_the_text_travel() {
    let straight = PathPose { stroke_glow: None, contours: vec![PathContour { closed: false, vertices: vec![
        PathVertex { point: [0.05, 0.5], incoming: [0.05, 0.5], outgoing: [0.05, 0.5] },
        PathVertex { point: [0.95, 0.5], incoming: [0.95, 0.5], outgoing: [0.95, 0.5] },
    ] }], fill: [0.0; 4], stroke: [0.0; 4], stroke_width: 0.0 };
    let travel = |layer| TextPath::Layer {
        layer,
        contour: 0,
        offset: 0.0,
        offset_keyframes: {
            let mut keys = Vec::new();
            klipp_core::keyframe::upsert_keyframe(&mut keys, 0.0, 0.0);
            klipp_core::keyframe::upsert_keyframe(&mut keys, 2.0, 0.6);
            keys
        },
        align: PathTextAlign::Start,
        flip: false,
    };
    let td = title(VectorPath { trim: None, pose: straight.clone(), keyframes: Vec::new() }, "GO", travel, 0.0);
    let mut r = TitleRenderer::new();
    let x0 = centroid(&ink(&r.render_with_time(&td, W, H, None, 0.0))).0;
    let x1 = centroid(&ink(&r.render_with_time(&td, W, H, None, 2.0))).0;
    let along = W as f32 * 0.9;
    assert!((x1 - x0 - 0.6 * along).abs() < 6.0, "travelled 0.6 of the path: {x0} to {x1}");

    // Flipped, the text stands below the path, read the other way.
    let side = |flip: bool| move |layer| TextPath::Layer { layer, contour: 0, offset: 0.5, offset_keyframes: Vec::new(), align: PathTextAlign::Center, flip };
    let up = centroid(&ink(&r.render_with_time(&title(VectorPath { trim: None, pose: straight.clone(), keyframes: Vec::new() }, "SIDE", side(false), 0.0), W, H, None, 0.0)));
    let down = centroid(&ink(&r.render_with_time(&title(VectorPath { trim: None, pose: straight.clone(), keyframes: Vec::new() }, "SIDE", side(true), 0.0), W, H, None, 0.0)));
    assert!(up.1 < H as f32 * 0.5 && down.1 > H as f32 * 0.5, "above, then below: {up:?} {down:?}");
    assert!((up.0 - W as f32 * 0.5).abs() < 6.0 && (down.0 - W as f32 * 0.5).abs() < 6.0, "centred on the middle both ways");

    // Tracking spaces the letters along the path.
    let mut spread = |tracking| {
        let px = r.render_with_time(&title(VectorPath { trim: None, pose: straight.clone(), keyframes: Vec::new() }, "WIDE", on(0.1), tracking), W, H, None, 0.0);
        let xs: Vec<f32> = ink(&px).iter().map(|p| p.0).collect();
        xs.iter().copied().fold(f32::MIN, f32::max) - xs.iter().copied().fold(f32::MAX, f32::min)
    };
    let (tight, loose) = (spread(0.0), spread(20.0));
    // Three gaps, each 20 px of a 1080-line frame wider: 20 px at 360.
    assert!((loose - tight - 20.0).abs() < 3.0, "tracking widens each gap along the path: {tight} vs {loose}");
}

/// A text whose layer is gone is laid straight, not lost; a static path's
/// resting render is its frame at any time, as export renders it.
#[test]
fn a_missing_layer_lays_straight_and_a_resting_render_is_the_frame() {
    let mut r = TitleRenderer::new();
    let gone = title(VectorPath { trim: None, pose: s_curve(0.5, 0.0), keyframes: Vec::new() }, "STILL HERE", |_| TextPath::Layer {
        layer: TitleObjectId(99),
        contour: 0,
        offset: 0.0,
        offset_keyframes: Vec::new(),
        align: PathTextAlign::Start,
        flip: false,
    }, 0.0);
    let px = r.render_with_time(&gone, W, H, None, 0.0);
    assert!(ink(&px).len() > 100, "the text still draws");

    let td = title(VectorPath { trim: None, pose: s_curve(0.5, 1.0), keyframes: Vec::new() }, "RESTING", on(0.1), 0.0);
    let resting = r.render(&td, W, H);
    assert_eq!(resting, r.render_with_time(&td, W, H, None, 0.0));
    assert_eq!(resting, r.render_with_time(&td, W, H, None, 1.5));
}

/// Drawn in one title, a keyed path and the red text on it move as one:
/// the text keeps its place against the white curve at every moment.
#[test]
fn a_keyed_path_and_its_text_drawn_together_never_drift() {
    let mut p = VectorPath { trim: None, pose: s_curve(0.3, 1.0), keyframes: Vec::new() };
    p.key(0.0, s_curve(0.3, 1.0)).unwrap();
    p.key(2.0, s_curve(0.7, 1.0)).unwrap();
    let mut td = title(p, "TOGETHER", on(0.2), 0.0);
    if let TitleObjectContent::TextBox(tb) = &mut td.objects[1].content {
        tb.color = [255, 0, 0, 255];
    }
    let mut r = TitleRenderer::new();
    let mut gaps = Vec::new();
    for t in [0.0, 0.5, 1.0, 1.5, 2.0] {
        let px = r.render_with_time(&td, W, H, None, t);
        let of = |red: bool| -> Vec<(f32, f32)> {
            px.chunks_exact(4).enumerate()
                .filter(|(_, p)| p[3] > 200 && ((p[0] > 200 && p[1] < 60) == red) && (red || p[1] > 200))
                .map(|(i, _)| ((i as u32 % W) as f32, (i as u32 / W) as f32))
                .collect()
        };
        let (red, white) = (of(true), of(false));
        assert!(red.len() > 100 && white.len() > 100, "both drawn at {t} s: {} red, {} white", red.len(), white.len());
        let (text, curve) = (centroid(&red), centroid(&white));
        gaps.push((text.0 - curve.0, text.1 - curve.1));
    }
    for g in &gaps {
        assert!((g.0 - gaps[0].0).abs() < 2.0 && (g.1 - gaps[0].1).abs() < 2.0, "drawn together, they move as one: {gaps:?}");
    }
}
