//! Real 3D text drawn (#1512): an extruded, bevelled word turning in y
//! under a perspective camera, lit from a direction, the same frame in
//! the viewer and in an export.

use klipp_core::{Interpolation, Keyframe};
use klipp_timeline::{Bevel, BevelProfile, Text3d, TextBox, TitleData};
use klipp_title::{TitleFrame, TitleRenderer};

const W: u32 = 1280;
const H: u32 = 720;
const SIDE: [u8; 4] = [0, 0, 255, 255];

fn key(time_secs: f64, value: f32) -> Keyframe {
    Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
}

/// "I" in 3D, deep and bevelled, lit from the right and 30° towards
/// the viewer, turned in y from -60° to 60° over two seconds.
fn turning(text: &str) -> TitleData {
    let mut t = Text3d {
        depth: 0.3,
        front_bevel: Some(Bevel { size: 0.03, segments: 3, profile: BevelProfile::Round }),
        side_color: SIDE,
        face_color: Some([230, 230, 230, 255]),
        bevel_color: Some([0, 255, 0, 255]),
        ..Default::default()
    };
    t.light.azimuth = 0.0;
    t.light.elevation = 30.0;
    t.light.specular = 0.0;
    t.keys.rotate_y = vec![key(0.0, -60.0), key(2.0, 60.0)];
    TitleData::single_text(TextBox { text: text.into(), font_size: 300.0, three_d: Some(t), ..Default::default() })
}

fn px(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

/// The inked columns' span.
fn width(buf: &[u8]) -> u32 {
    let cols: Vec<u32> = (0..W).filter(|&x| (0..H).any(|y| px(buf, x, y)[3] > 128)).collect();
    cols.last().unwrap() - cols.first().unwrap()
}

/// The face's mean brightness: the solid grey pixels (the walls are
/// blue, the bevel green), and how many there are.
fn face(buf: &[u8]) -> (f32, usize) {
    let grey: Vec<u8> = buf.chunks_exact(4).filter(|p| p[3] == 255 && p[0].abs_diff(p[1]) < 6 && p[1].abs_diff(p[2]) < 6).map(|p| p[0]).collect();
    (grey.iter().map(|&v| v as f32).sum::<f32>() / grey.len().max(1) as f32, grey.len())
}

/// Pixels drawn in the wall's colour (blue over grey faces).
fn wall_pixels(buf: &[u8]) -> usize {
    buf.chunks_exact(4).filter(|p| p[3] > 200 && p[2] as u16 > p[0] as u16 + 40).count()
}

/// The done-when: across the turn the word narrows and widens as it
/// turns, its face is lit by the angle it makes with the light (the
/// face turned towards the light brightest), its walls show only once
/// it turns, and the viewer's frame is the export's at every moment.
#[test]
fn an_extruded_bevelled_word_turns_in_perspective_and_lights_across_the_turn() {
    let td = turning("I");
    let mut r = TitleRenderer::new();
    let (mut widths, mut faces, mut walls) = (Vec::new(), Vec::new(), Vec::new());
    for t in [0.0f32, 0.5, 1.0, 1.5, 2.0] {
        let export = r.render_with_window(&td, W, H, None, t, Some(4.0));
        let viewer = match r.frame_with_window(&td, W, H, None, t, Some(4.0)) {
            TitleFrame::Rendered { pixels, .. } => pixels.to_bytes(),
            _ => panic!("at t={t} a turning 3D text is drawn for its time"),
        };
        assert_eq!(viewer, export, "at t={t} the viewer's frame is the export's");
        widths.push(width(&export));
        faces.push(face(&export));
        walls.push(wall_pixels(&export));
    }
    // -60°, -30°, 0°, 30°, 60°: the face takes sin(30° - θ) of the
    // light: 1, 0.87, 0.5, then none at 30° (edge on to it) and past
    // it, where the ambient alone lights it.
    assert!(faces.iter().all(|f| f.1 > 150), "the face shows at every angle: {faces:?}");
    let lum: Vec<f32> = faces.iter().map(|f| f.0).collect();
    assert!(lum[0] > lum[1] + 5.0 && lum[1] > lum[2] + 5.0 && lum[2] > lum[3] + 5.0, "darkening as it turns from the light: {lum:?}");
    assert!((lum[3] - lum[4]).abs() < 1.0, "turned away from the light, the ambient alone: {lum:?}");
    assert!(lum[0] > 200.0 && lum[4] < 140.0, "{lum:?}");
    // Unturned it is narrowest from the front of an I... and its walls
    // hidden; turned either way the depth shows as a wall.
    assert_eq!(walls[2], 0, "no wall faces the camera unturned: {walls:?}");
    assert!(walls[0] > 500 && walls[4] > 500, "walls show turned: {walls:?}");
    // A turned I shows its depth: wider turned than face on.
    assert!(widths[0] > widths[2] && widths[4] > widths[2], "{widths:?}");
    // The same frame on every render.
    assert_eq!(TitleRenderer::new().render_with_window(&td, W, H, None, 0.7, Some(4.0)), r.render_with_window(&td, W, H, None, 0.7, Some(4.0)));
}

/// A wide word, unturned and orthographic, covers where its flat self
/// does, near enough: the 3D text sits where the text sits.
#[test]
fn an_unturned_3d_word_sits_where_the_flat_word_does() {
    let flat = TitleData::single_text(TextBox { text: "WORD".into(), font_size: 200.0, ..Default::default() });
    let mut solid = flat.clone();
    if let klipp_timeline::TitleObjectContent::TextBox(tb) = &mut solid.objects[0].content {
        let mut t = Text3d { front_bevel: None, ..Default::default() };
        t.camera.orthographic = true;
        tb.three_d = Some(t);
    }
    let mut r = TitleRenderer::new();
    let (a, b) = (r.render_with_window(&flat, W, H, None, 1.0, Some(4.0)), r.render_with_window(&solid, W, H, None, 1.0, Some(4.0)));
    let ink = |buf: &[u8]| -> (u32, u32, usize) {
        let cols: Vec<u32> = (0..W).filter(|&x| (0..H).any(|y| px(buf, x, y)[3] > 128)).collect();
        (*cols.first().unwrap(), *cols.last().unwrap(), buf.chunks_exact(4).filter(|p| p[3] > 128).count())
    };
    let ((fl, fr, fa), (sl, sr, sa)) = (ink(&a), ink(&b));
    assert!(fl.abs_diff(sl) <= 2 && fr.abs_diff(sr) <= 2, "flat {fl}..{fr}, 3D {sl}..{sr}");
    assert!((fa as f32 - sa as f32).abs() / fa as f32 <= 0.03, "flat {fa} px, 3D {sa} px");
}

/// The per-frame cost of a 3D word at 1080p and 4K, printed for the
/// record (`cargo test -p klipp-title --test text3d cost -- --ignored --nocapture`).
#[test]
#[ignore]
fn cost() {
    use klipp_timeline::{LayerFx, TextGlow, TextOutline, TitleShadow};
    let dressed = LayerFx {
        shadows: vec![TitleShadow { enabled: true, color: [0, 0, 0, 200], offset_x: 14.0, offset_x_keyframes: vec![], offset_y: 18.0, offset_y_keyframes: vec![], blur: 10.0, blur_keyframes: vec![] }],
        glows: vec![TextGlow { enabled: true, color: [255, 120, 0, 255], radius: 16.0, radius_keyframes: vec![], intensity: 0.6, intensity_keyframes: vec![] }],
        outlines: vec![TextOutline { enabled: true, color: [20, 20, 20, 255], width: 3.0, width_keyframes: vec![] }],
        ..Default::default()
    };
    for (what, fx) in [("plain", LayerFx::default()), ("shadow, glow, outline", dressed)] {
        for (w, h) in [(1920u32, 1080u32), (3840, 2160)] {
            let mut td = turning("KLIPP 3D");
            td.objects[0].fx = Some(fx.clone());
            if let klipp_timeline::TitleObjectContent::TextBox(tb) = &mut td.objects[0].content {
                tb.font_size = 180.0;
            }
            let mut r = TitleRenderer::new();
            let _ = r.render_with_window(&td, w, h, None, 0.3, Some(4.0));
            let n = 20;
            let start = std::time::Instant::now();
            for k in 0..n {
                let _ = r.render_with_window(&td, w, h, None, 0.1 * k as f32, Some(4.0));
            }
            eprintln!("COST {what} {w}x{h}: {:.1} ms a frame", start.elapsed().as_secs_f64() * 1000.0 / n as f64);
        }
    }
}

/// A word unturned and without perspective, in 3D or flat, with `fx`
/// on its layer and `bg` behind it.
fn word(three_d: bool, fx: klipp_timeline::LayerFx, bg: klipp_timeline::TitleBackground) -> TitleData {
    let mut t = Text3d { front_bevel: None, depth: 0.2, face_color: Some([240, 240, 240, 255]), ..Default::default() };
    t.camera.orthographic = true;
    t.light.ambient = 1.0;
    t.light.intensity = 0.0;
    t.light.specular = 0.0;
    let tb = TextBox { text: "WORD".into(), font_size: 200.0, color: [240, 240, 240, 255], three_d: three_d.then_some(t), ..Default::default() };
    let mut td = TitleData::single_text_fx(tb, fx);
    td.background = bg;
    td
}

/// The pixels where `with` differs from `without`, and how many of
/// them are mostly `colour`.
fn added(with: &[u8], without: &[u8], colour: [u8; 3]) -> (usize, usize) {
    let mut changed = 0;
    let mut coloured = 0;
    for (a, b) in with.chunks_exact(4).zip(without.chunks_exact(4)) {
        if a != b {
            changed += 1;
            if a[3] > 40 && (0..3).all(|k| a[k].abs_diff(colour[k]) < 70) {
                coloured += 1;
            }
        }
    }
    (changed, coloured)
}

/// Slice 2b: a 3D text keeps its layer's drop shadow, glow, outline and
/// inner shadow, drawn from its solid's coverage, near what the flat
/// text draws from its glyphs'; and its backing panel and full-frame
/// card. Turned, the outline follows the turned silhouette.
#[test]
fn a_3d_text_keeps_its_shadow_glow_outline_and_backing() {
    use klipp_timeline::{InnerShadow, LayerFx, TextGlow, TextOutline, TitleBackground, TitleShadow};
    let mut r = TitleRenderer::new();
    let draw = |r: &mut TitleRenderer, td: &TitleData| r.render_with_window(td, W, H, None, 1.0, Some(4.0));
    let none = || TitleBackground::None;
    let shadow = LayerFx {
        shadows: vec![TitleShadow { enabled: true, color: [200, 0, 0, 255], offset_x: 12.0, offset_x_keyframes: Vec::new(), offset_y: 12.0, offset_y_keyframes: Vec::new(), blur: 0.0, blur_keyframes: Vec::new() }],
        ..Default::default()
    };
    let glow = LayerFx { glows: vec![TextGlow { enabled: true, color: [0, 200, 0, 255], radius: 12.0, radius_keyframes: Vec::new(), intensity: 1.0, intensity_keyframes: Vec::new() }], ..Default::default() };
    let outline = LayerFx { outlines: vec![TextOutline { enabled: true, color: [0, 0, 220, 255], width: 6.0, width_keyframes: Vec::new() }], ..Default::default() };
    let inner = LayerFx {
        inner_shadows: vec![InnerShadow { enabled: true, color: [0, 0, 0, 255], offset_x: 6.0, offset_x_keyframes: Vec::new(), offset_y: 6.0, offset_y_keyframes: Vec::new(), blur: 2.0, blur_keyframes: Vec::new() }],
        ..Default::default()
    };
    for (name, fx, colour) in [("shadow", shadow, [200u8, 0, 0]), ("glow", glow, [0, 200, 0]), ("outline", outline, [0, 0, 220])] {
        let (solid, flat) = (draw(&mut r, &word(true, Default::default(), none())), draw(&mut r, &word(false, Default::default(), none())));
        let (solid_fx, flat_fx) = (draw(&mut r, &word(true, fx.clone(), none())), draw(&mut r, &word(false, fx, none())));
        let (s3, sc) = added(&solid_fx, &solid, colour);
        let (f2, fc) = added(&flat_fx, &flat, colour);
        assert!(sc > 1000, "{name}: the 3D text has its {name}: {sc} of {s3} pixels in its colour");
        let near = (sc as f32 - fc as f32).abs() / fc as f32;
        assert!(near < 0.08, "{name}: as much as the flat text's: 3D {sc}, flat {fc} ({f2} changed)");
    }
    // The inner shadow darkens inside the face, not round it.
    let (plain, shaded) = (draw(&mut r, &word(true, Default::default(), none())), draw(&mut r, &word(true, inner, none())));
    let darker = plain.chunks_exact(4).zip(shaded.chunks_exact(4)).filter(|(a, b)| a[3] == 255 && (b[0] as u16) + 30 < a[0] as u16).count();
    let outside = plain.chunks_exact(4).zip(shaded.chunks_exact(4)).filter(|(a, b)| a[3] == 0 && b[3] > 0).count();
    assert!(darker > 500 && outside == 0, "inside {darker}, outside {outside}");
    // A backing panel round the solid, and a full-frame card under it.
    let backed = draw(&mut r, &word(true, Default::default(), TitleBackground::Color { color: [10, 20, 30, 255], padding: 12.0 }));
    let (changed, panel) = added(&backed, &draw(&mut r, &word(true, Default::default(), none())), [10, 20, 30]);
    // Every change is the panel but the letters' soft edges, now over it.
    assert!(panel > 5000 && panel as f32 >= changed as f32 * 0.9, "a panel round the word: {panel} of {changed}");
    let card = draw(&mut r, &word(true, Default::default(), TitleBackground::FullFrame { color: [5, 60, 90, 255] }));
    assert!(card.chunks_exact(4).all(|p| p[3] == 255), "the card fills the frame");
    assert_eq!(&card[..4], &[5, 60, 90, 255]);
    // Turned, the outline goes round the turned silhouette: past the
    // solid's turned edge, not where the flat word's was.
    let mut turned = word(true, LayerFx { outlines: vec![TextOutline { enabled: true, color: [0, 0, 220, 255], width: 6.0, width_keyframes: Vec::new() }], ..Default::default() }, none());
    if let klipp_timeline::TitleObjectContent::TextBox(tb) = &mut turned.objects[0].content {
        let t = tb.three_d.as_mut().unwrap();
        t.rotation = [0.0, 50.0, 0.0];
        t.camera.orthographic = false;
    }
    let mut bare = turned.clone();
    bare.objects[0].fx = Some(Default::default());
    let (with, without) = (draw(&mut r, &turned), draw(&mut r, &bare));
    let solid_cols = |buf: &[u8]| -> (usize, usize) {
        let cols: Vec<usize> = (0..W as usize).filter(|&x| (0..H as usize).any(|y| buf[(y * W as usize + x) * 4 + 3] > 128)).collect();
        (cols[0], *cols.last().unwrap())
    };
    let ((sl, sr), (ol, or)) = (solid_cols(&without), solid_cols(&with));
    assert!(ol + 3 <= sl && or >= sr + 3, "the outline round the turned word: solid {sl}..{sr}, with outline {ol}..{or}");
}

/// "WORD" in 3D, unturned and orthographic, lit by its ambient alone so
/// a face shows its colours as they are, `set` on its box; and its
/// flat self, without 3D (#1735).
fn styled(three_d: bool, set: impl FnOnce(&mut TextBox)) -> TitleData {
    let mut td = word(three_d, Default::default(), klipp_timeline::TitleBackground::None);
    if let klipp_timeline::TitleObjectContent::TextBox(tb) = &mut td.objects[0].content {
        tb.three_d.iter_mut().for_each(|t| t.side_color = [0, 160, 0, 255]);
        set(tb);
    }
    td
}

fn turned(mut td: TitleData) -> TitleData {
    if let klipp_timeline::TitleObjectContent::TextBox(tb) = &mut td.objects[0].content {
        let t = tb.three_d.as_mut().unwrap();
        t.rotation = [0.0, 50.0, 0.0];
        t.camera.orthographic = false;
    }
    td
}

/// The inked box `(x0, y0, x1, y1)` and the inked pixel count.
fn ink(buf: &[u8]) -> (u32, u32, u32, u32, usize) {
    let (mut x0, mut y0, mut x1, mut y1, mut n) = (W, H, 0, 0, 0);
    for y in 0..H {
        for x in 0..W {
            if px(buf, x, y)[3] > 128 {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                n += 1;
            }
        }
    }
    (x0, y0, x1, y1, n)
}

/// The solid inked pixels of columns `xs` and rows `ys`.
fn solid_in(buf: &[u8], xs: std::ops::Range<u32>, ys: std::ops::Range<u32>) -> Vec<[u8; 4]> {
    ys.flat_map(|y| xs.clone().map(move |x| (x, y))).map(|(x, y)| px(buf, x, y)).filter(|p| p[3] == 255).collect()
}

fn draw_at(td: &TitleData, t: f32) -> Vec<u8> {
    TitleRenderer::new().render_with_window(td, W, H, None, t, Some(4.0))
}

/// A gradient face goes on the solid's front face, top to bottom as
/// the flat face has it; turned, the walls keep their own colour.
#[test]
fn a_3d_text_keeps_its_gradient_face_and_its_walls_their_colour() {
    let red_to_blue = |tb: &mut TextBox| tb.gradient = Some(klipp_timeline::TextGradient::pair([255, 0, 0, 255], [0, 0, 255, 255], true));
    let solid = draw_at(&styled(true, red_to_blue), 1.0);
    let (x0, y0, x1, y1, _) = ink(&solid);
    let band = (y1 - y0) / 5;
    let (top, bottom) = (solid_in(&solid, x0..x1, y0..y0 + band), solid_in(&solid, x0..x1, y1 - band..y1));
    let reddish = |ps: &[[u8; 4]]| ps.iter().filter(|p| p[0] as u16 > p[2] as u16 + 60).count() as f32 / ps.len().max(1) as f32;
    let bluish = |ps: &[[u8; 4]]| ps.iter().filter(|p| p[2] as u16 > p[0] as u16 + 60).count() as f32 / ps.len().max(1) as f32;
    assert!(top.len() > 500 && reddish(&top) > 0.9, "the top of the face is red: {} of {}", reddish(&top), top.len());
    assert!(bottom.len() > 500 && bluish(&bottom) > 0.9, "the bottom of the face is blue: {} of {}", bluish(&bottom), bottom.len());
    // No white face left anywhere: the fill is the face.
    assert!(solid.chunks_exact(4).filter(|p| p[3] == 255 && p[0] > 200 && p[1] > 200 && p[2] > 200).count() == 0);
    // Turned, the walls are green, and the face still red to blue.
    let solid = draw_at(&turned(styled(true, red_to_blue)), 1.0);
    let green = solid.chunks_exact(4).filter(|p| p[3] > 200 && p[1] as u16 > p[0] as u16 + 40 && p[1] as u16 > p[2] as u16 + 40).count();
    assert!(green > 500, "the walls keep their colour: {green}");
    let red = solid.chunks_exact(4).filter(|p| p[3] > 200 && p[0] as u16 > p[1] as u16 + 60 && p[0] as u16 > p[2] as u16 + 60).count();
    assert!(red > 500, "the turned face keeps its fill: {red}");
    // The control: a plain face is the face colour.
    let plain = draw_at(&styled(true, |_| {}), 1.0);
    let (x0, y0, x1, y1, _) = ink(&plain);
    assert!(solid_in(&plain, x0..x1, y0..y1).iter().all(|p| p[0] > 230 && p[2] > 230));
}

/// A styled run's colour is its glyphs' faces' colour.
#[test]
fn a_3d_text_keeps_its_runs_colours() {
    let solid = draw_at(
        &styled(true, |tb| tb.style_runs.push(klipp_timeline::StyleRun { color: Some([255, 0, 0, 255]), ..klipp_timeline::StyleRun::default_for_range(0, 2) })),
        1.0,
    );
    let (x0, y0, x1, y1, _) = ink(&solid);
    let third = (x1 - x0) / 3;
    let (left, right) = (solid_in(&solid, x0..x0 + third, y0..y1), solid_in(&solid, x1 - third..x1, y0..y1));
    let red = |p: &&[u8; 4]| p[0] > 200 && p[1] < 60 && p[2] < 60;
    let white = |p: &&[u8; 4]| p[0] > 200 && p[1] > 200 && p[2] > 200;
    assert!(left.len() > 1000 && left.iter().filter(red).count() as f32 > left.len() as f32 * 0.95, "W is red");
    assert!(right.len() > 1000 && right.iter().filter(white).count() as f32 > right.len() as f32 * 0.95, "D is white");
}

/// Underline and strikethrough are thin solids on the front plane,
/// where and as long as the flat text draws them.
#[test]
fn a_3d_text_draws_its_underline_and_strikethrough() {
    for (name, set) in [("underline", (|tb: &mut TextBox| tb.underline = true) as fn(&mut TextBox)), ("strikethrough", |tb: &mut TextBox| tb.strikethrough = true)] {
        let gained = |three_d: bool| {
            let (with, without) = (draw_at(&styled(three_d, set), 1.0), draw_at(&styled(three_d, |_| {}), 1.0));
            let rows: Vec<u32> = (0..H).filter(|&y| (0..W).any(|x| px(&with, x, y)[3] > 128 && px(&without, x, y)[3] <= 128)).collect();
            let count = with.chunks_exact(4).zip(without.chunks_exact(4)).filter(|(a, b)| a[3] > 128 && b[3] <= 128).count();
            (count, rows.first().copied().unwrap_or(0), rows.last().copied().unwrap_or(0))
        };
        let ((s, s0, s1), (f, f0, f1)) = (gained(true), gained(false));
        assert!(f > 500, "{name}: the flat text's stripe: {f}");
        assert!((s as f32 - f as f32).abs() / f as f32 <= 0.1, "{name}: as much stripe in 3D as flat: {s} vs {f}");
        assert!(s0.abs_diff(f0) <= 2 && s1.abs_diff(f1) <= 2, "{name}: at the flat stripe's rows: {s0}..{s1} vs {f0}..{f1}");
    }
    // Turned, the underline has depth too: a wall under the word.
    let td = turned(styled(true, |tb| tb.underline = true));
    let (with, without) = (draw_at(&td, 1.0), draw_at(&turned(styled(true, |_| {})), 1.0));
    let (_, _, _, wy, _) = ink(&without);
    let below = (wy + 2..H).flat_map(|y| (0..W).map(move |x| (x, y))).filter(|&(x, y)| px(&with, x, y)[3] > 128).count();
    assert!(below > 300, "the turned underline is drawn below the word: {below}");
}

/// A write-on reveals the solids from the start as far as it reveals
/// the flat text.
#[test]
fn a_3d_text_writes_on_as_the_flat_text_does() {
    let half = |tb: &mut TextBox| tb.write_on = 0.45;
    let (solid, flat, whole) = (draw_at(&styled(true, half), 1.0), draw_at(&styled(false, half), 1.0), draw_at(&styled(true, |_| {}), 1.0));
    let ((sx0, _, sx1, _, sn), (fx0, _, fx1, _, fnn), (wx0, _, wx1, _, wn)) = (ink(&solid), ink(&flat), ink(&whole));
    assert!(sx0.abs_diff(fx0) <= 2 && sx1.abs_diff(fx1) <= 2, "revealed to where the flat text is: 3D {sx0}..{sx1}, flat {fx0}..{fx1}");
    assert!(sx0.abs_diff(wx0) <= 2 && sx1 + 100 < wx1, "from the start, not to the end: {sx0}..{sx1} of {wx0}..{wx1}");
    assert!((sn as f32 - fnn as f32).abs() / fnn as f32 <= 0.05 && sn < wn * 3 / 5, "3D {sn}, flat {fnn}, whole {wn}");
    // Turned, the cut solid shows its cut, and still stops short.
    let (cut, all) = (draw_at(&turned(styled(true, half)), 1.0), draw_at(&turned(styled(true, |_| {})), 1.0));
    assert!(ink(&cut).4 < ink(&all).4 * 3 / 5 && ink(&cut).4 > 1000);
    // None written: nothing drawn.
    assert_eq!(ink(&draw_at(&styled(true, |tb| tb.write_on = 0.0), 1.0)).4, 0);
}

/// A staggered rise poses each glyph's solid on its own: mid-entrance
/// the first letters stand higher than the last, as on the flat text;
/// settled, the word is its resting self. The viewer draws the frame
/// the export does.
#[test]
fn a_3d_text_rises_letter_by_letter() {
    use klipp_timeline::{Easing, LayerFx, Motion, MotionKind, StaggerUnit, TitleMotion};
    let rising = |three_d: bool| {
        let mut td = styled(three_d, |_| {});
        td.objects[0].fx = Some(LayerFx { motion: TitleMotion::enter(Motion::new(MotionKind::Rise).duration(0.6).easing(Easing::Linear).stagger(StaggerUnit::Char, 0.2)), ..Default::default() });
        td
    };
    let mut r = TitleRenderer::new();
    let (td, flat) = (rising(true), rising(false));
    let rest = draw_at(&styled(true, |_| {}), 3.0);
    let (x0, _, x1, _, _) = ink(&rest);
    let quarter = (x1 - x0) / 4;
    // The top of the ink in the first and the last letter's columns.
    let tops = |buf: &[u8]| {
        let top = |xs: std::ops::Range<u32>| (0..H).find(|&y| xs.clone().any(|x| px(buf, x, y)[3] > 128));
        (top(x0..x0 + quarter), top(x1 - quarter..x1))
    };
    for t in [0.35f32, 0.5] {
        let export = r.render_with_window(&td, W, H, None, t, Some(4.0));
        let viewer = match r.frame_with_window(&td, W, H, None, t, Some(4.0)) {
            TitleFrame::Rendered { pixels, .. } => pixels.to_bytes(),
            _ => panic!("at t={t} a rising 3D text is drawn for its time"),
        };
        assert_eq!(viewer, export, "at t={t} the viewer's frame is the export's");
        let ((first, last), (flat_first, flat_last)) = (tops(&export), tops(&draw_at(&flat, t)));
        let (first, last) = (first.expect("the first letter shows"), last.unwrap_or(H));
        assert!(first + 10 < last, "at t={t} the first letter has risen further: {first} vs {last}");
        let (flat_first, flat_last) = (flat_first.unwrap(), flat_last.unwrap_or(H));
        assert!(first.abs_diff(flat_first) <= 4, "at t={t} the first letter stands as the flat one does: {first} vs {flat_first}");
        assert!(last.min(H).abs_diff(flat_last.min(H)) <= 6, "at t={t} so does the last: {last} vs {flat_last}");
    }
    assert_eq!(draw_at(&td, 3.0), rest, "settled, the word is at rest");
}

/// An animator turns each glyph's solid about its own centre, as it
/// turns each flat glyph: the word takes the same room it takes flat.
#[test]
fn an_animator_turns_each_3d_glyph_on_its_own() {
    use klipp_timeline::{Animator, AnimatorProps, LayerFx};
    let spun = |three_d: bool| {
        let mut td = styled(three_d, |_| {});
        td.objects[0].fx = Some(LayerFx { animators: vec![Animator { props: AnimatorProps { rotation: 30.0, ..Default::default() }, ..Default::default() }], ..Default::default() });
        td
    };
    let (solid, flat, still) = (draw_at(&spun(true), 1.0), draw_at(&spun(false), 1.0), draw_at(&styled(true, |_| {}), 1.0));
    assert_ne!(solid, still, "the animator poses the solids");
    let (s, f) = (ink(&solid), ink(&flat));
    assert!(s.1.abs_diff(f.1) <= 4 && s.3.abs_diff(f.3) <= 4, "turned glyphs reach the rows flat ones do: 3D {s:?}, flat {f:?}");
    assert!((s.4 as f32 - f.4 as f32).abs() / f.4 as f32 <= 0.05, "as much ink: 3D {}, flat {}", s.4, f.4);
}

/// On a path, each glyph's solid stands where its flat glyph does,
/// turned along the path.
#[test]
fn a_3d_text_on_a_path_stands_each_glyph_on_it() {
    let arc = |tb: &mut TextBox| tb.text_path = Some(klipp_timeline::TextPath::Arc { radius: 300.0, start_angle: 200.0, sweep: 140.0 });
    let (solid, flat) = (draw_at(&styled(true, arc), 1.0), draw_at(&styled(false, arc), 1.0));
    let (s, f) = (ink(&solid), ink(&flat));
    let both = solid.chunks_exact(4).zip(flat.chunks_exact(4)).filter(|(a, b)| a[3] > 128 && b[3] > 128).count();
    assert!(f.4 > 5000 && both as f32 >= f.4 as f32 * 0.9, "the solids cover the flat glyphs: {both} of {}", f.4);
    assert!((s.4 as f32 - f.4 as f32).abs() / f.4 as f32 <= 0.06, "and not much more: 3D {}, flat {}", s.4, f.4);
    // Not where the straight word stands.
    let straight = draw_at(&styled(true, |_| {}), 1.0);
    assert!(ink(&straight).1.abs_diff(s.1) > 20 || ink(&straight).3.abs_diff(s.3) > 20);
}

/// Chromatic aberration and glitch go on the drawn 3D layer as on a
/// flat one.
#[test]
fn a_3d_text_keeps_its_post_effects() {
    let (plain, split) = (draw_at(&styled(true, |_| {}), 1.0), draw_at(&styled(true, |tb| tb.chromatic_aberration_px = 8.0), 1.0));
    let fringe = |buf: &[u8]| buf.chunks_exact(4).filter(|p| p[3] > 60 && p[0].abs_diff(p[2]) > 100).count();
    assert_eq!(fringe(&plain), 0);
    assert!(fringe(&split) > 500, "the channels split: {}", fringe(&split));
    let glitched = draw_at(&styled(true, |tb| tb.glitch_intensity = 1.0), 1.0);
    assert_ne!(glitched, plain, "the glitch shifts the drawn solid");
}

/// The done-when: a 3D word with a gradient face, a run in another
/// colour, an underline, a write-on and a staggered rise draws each of
/// them, the same in the viewer and the export; with 3D off, the text
/// draws as it does without 3D.
#[test]
fn a_dressed_3d_word_draws_all_of_its_look() {
    use klipp_timeline::{Easing, LayerFx, Motion, MotionKind, StaggerUnit, TitleMotion};
    let dressed = |three_d: bool| {
        let mut td = styled(three_d, |tb| {
            tb.gradient = Some(klipp_timeline::TextGradient::pair([255, 255, 0, 255], [255, 120, 0, 255], true));
            tb.style_runs.push(klipp_timeline::StyleRun { color: Some([0, 0, 255, 255]), ..klipp_timeline::StyleRun::default_for_range(3, 4) });
            tb.underline = true;
            tb.write_on = 0.8;
        });
        td.objects[0].fx = Some(LayerFx { motion: TitleMotion::enter(Motion::new(MotionKind::Rise).duration(0.6).easing(Easing::Linear).stagger(StaggerUnit::Char, 0.2)), ..Default::default() });
        td
    };
    let td = dressed(true);
    let mut r = TitleRenderer::new();
    for t in [0.4f32, 2.0] {
        let export = r.render_with_window(&td, W, H, None, t, Some(4.0));
        let viewer = match r.frame_with_window(&td, W, H, None, t, Some(4.0)) {
            TitleFrame::Rendered { pixels, .. } => pixels.to_bytes(),
            TitleFrame::Placed { .. } | TitleFrame::Glyphs { .. } | TitleFrame::Layers { .. } => r.render_with_window(&td, W, H, None, t, Some(4.0)),
        };
        assert_eq!(viewer, export, "at t={t} the viewer's frame is the export's");
    }
    let settled = draw_at(&td, 2.0);
    let count = |f: fn(&[u8]) -> bool| settled.chunks_exact(4).filter(|p| p[3] == 255 && f(p)).count();
    assert!(count(|p| p[0] > 200 && p[1] > 200 && p[2] < 80) > 300, "the gradient's yellow top");
    assert!(count(|p| p[0] > 200 && p[1] > 90 && p[1] < 170 && p[2] < 80) > 300, "its orange bottom");
    // Each face pixel is the flat text's: its fill and its run as the
    // flat text has them (a gradient over the run's colour), the write-on
    // cutting both at the same column.
    let flat = draw_at(&dressed(false), 2.0);
    let (mut same, mut faces) = (0, 0);
    for (a, b) in settled.chunks_exact(4).zip(flat.chunks_exact(4)) {
        if a[3] == 255 && b[3] == 255 {
            faces += 1;
            same += usize::from((0..3).all(|k| a[k].abs_diff(b[k]) <= 12));
        }
    }
    assert!(faces > 5000 && same as f32 >= faces as f32 * 0.97, "the face is the flat face: {same} of {faces}");
    let (rest, underlined) = (ink(&draw_at(&dressed(true), 2.0)), ink(&draw_at(&{
        let mut bare = td.clone();
        if let klipp_timeline::TitleObjectContent::TextBox(tb) = &mut bare.objects[0].content {
            tb.underline = false;
        }
        bare
    }, 2.0)));
    assert!(rest.3 > underlined.3 + 5, "the underline below the word: {} vs {}", rest.3, underlined.3);
    // Mid-rise the first letter stands higher than the last.
    let rising = draw_at(&td, 0.4);
    let (x0, _, x1, _, _) = ink(&settled);
    let q = (x1 - x0) / 5;
    let top = |xs: std::ops::Range<u32>| (0..H).find(|&y| xs.clone().any(|x| px(&rising, x, y)[3] > 128)).unwrap_or(H);
    assert!(top(x0..x0 + q) + 10 < top(x1 - q..x1), "staggered: {} vs {}", top(x0..x0 + q), top(x1 - q..x1));
    // 3D off: the flat text, as drawn without any 3D settings.
    let mut off = dressed(true);
    if let klipp_timeline::TitleObjectContent::TextBox(tb) = &mut off.objects[0].content {
        tb.three_d.as_mut().unwrap().enabled = false;
    }
    for t in [0.4f32, 2.0] {
        assert_eq!(draw_at(&off, t), draw_at(&dressed(false), t), "at t={t} a text with 3D off draws flat");
    }
}
