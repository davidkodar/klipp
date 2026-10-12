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
    Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) }
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
            TitleFrame::Rendered { pixels, .. } => pixels,
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
    for (w, h) in [(1920u32, 1080u32), (3840, 2160)] {
        let mut td = turning("KLIPP 3D");
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
        eprintln!("COST {w}x{h}: {:.1} ms a frame", start.elapsed().as_secs_f64() * 1000.0 / n as f64);
    }
}
