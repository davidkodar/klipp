//! A pinned clip's corner pin is on its surface (#684): read live from
//! the host clip's track in the file's time, on the file frame shown, and
//! carried through how the host is fitted and placed in the frame. A
//! trimmed, moved or bladed host still carries the pin on the same
//! picture, and a pin whose surface is gone stays on the reference
//! frame's corners.

use klipp_compose::eval::surface_pin_corners;
use klipp_compose::render::clip_effects;
use klipp_core::{MediaId, Timecode};
use klipp_effects::{corner_pin, BuiltinEffect};
use klipp_timeline::{Clip, Sequence, Surface, SurfaceKey, SurfacePin};
use klipp_timeline::test_clip::TestClip;

const FPS: f64 = 24.0;

fn tc(s: f64) -> Timecode {
    Timecode::from_secs_f64(s, 24)
}

fn quad(x: f32) -> [[f32; 2]; 4] {
    [[x, 0.2], [x + 0.2, 0.2], [x + 0.2, 0.4], [x, 0.4]]
}

fn none(_: &Clip) -> Option<klipp_compose::eval::ResolvedTransform> {
    None
}

/// A host on video 1 showing file seconds 10..14 from timeline 0, its
/// picture the frame's shape, with a surface tracked from file 10 s
/// (x 0.1) to 14 s (x 0.5); a pin above.
fn scene() -> (Sequence, Clip) {
    let mut seq = Sequence::new_default();
    let frame = [seq.resolution.width, seq.resolution.height];
    let mut host = Clip::media(MediaId::new(), tc(0.0), tc(4.0), tc(10.0), tc(14.0));
    let id = klipp_core::ClipId::new().0;
    host.surfaces.push(Surface {
        id,
        name: "Screen".into(),
        corners: quad(0.1),
        reference_secs: 10.0,
        keys: vec![SurfaceKey::at_corners(10.0, &quad(0.1), &quad(0.1)).unwrap(), SurfaceKey::at_corners(14.0, &quad(0.1), &quad(0.5)).unwrap()],
        source_size: frame,
        media_fps: 0.0,
        track_area: None,
        exclude: Vec::new(),
    });
    let mut pin = Clip::media(MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0));
    let [tl, tr, br, bl] = quad(0.1);
    pin = TestClip::of(pin).effect(BuiltinEffect::CornerPin { tl, tr, br, bl }).build();
    pin.surface_pin = Some(SurfacePin { host: host.id, surface: id, picture_size: None, fit: Default::default(), inset: 0.0, on_loss: Default::default(), light: 0.0, shade: 0.0, texture: 0.0 });
    seq.tracks[0].clips.push(host);
    (seq, pin)
}

fn close(a: [[f32; 2]; 4], b: [[f32; 2]; 4]) -> bool {
    a.iter().flatten().zip(b.iter().flatten()).all(|(x, y)| (x - y).abs() < 2e-4)
}

#[test]
fn the_pin_follows_the_surface_in_file_time() {
    let (seq, pin) = scene();
    let at = surface_pin_corners(&pin, &seq, tc(2.0), FPS, &none).expect("pinned");
    assert!((at[0][0] - 0.3).abs() < 1e-4, "halfway through the track: {:?}", at[0]);
    let (fx, _) = clip_effects(&pin, tc(2.0), FPS, None, Some(at));
    let corners = fx.iter().find_map(corner_pin::corners).expect("the pin is in the chain");
    assert_eq!(corners, at);
}

#[test]
fn a_moved_host_carries_the_track_with_its_picture() {
    let (mut seq, pin) = scene();
    // The host now starts a second later on the timeline, from the same file second.
    let host = &mut seq.tracks[0].clips[0];
    host.timeline_in = tc(1.0);
    host.timeline_out = tc(5.0);
    let at = surface_pin_corners(&pin, &seq, tc(3.0), FPS, &none).unwrap();
    assert!((at[0][0] - 0.3).abs() < 1e-4, "file 12 s is now at timeline 3 s: {:?}", at[0]);
}

/// Both halves of a bladed host carry the surface: the pin follows the
/// half that is under it, whichever kept the id.
#[test]
fn a_bladed_host_still_carries_the_pin() {
    let (mut seq, pin) = scene();
    let before = surface_pin_corners(&pin, &seq, tc(3.0), FPS, &none).unwrap();
    let host_id = seq.tracks[0].clips[0].id;
    seq.tracks[0].split_at(host_id, tc(2.0)).expect("split");
    assert!(seq.tracks[0].clips.iter().all(|c| c.surfaces.len() == 1), "both halves keep the surface");
    let after = surface_pin_corners(&pin, &seq, tc(3.0), FPS, &none).expect("still pinned after the blade");
    assert!(close(before, after), "{before:?} vs {after:?}");
}

/// 30 fps footage in a 24 fps sequence: at a sequence frame that falls
/// inside a file frame, the pin is on that file frame's key, as the
/// picture is, not partway to the next.
#[test]
fn at_mixed_rates_the_pin_holds_with_the_picture() {
    let (mut seq, pin) = scene();
    let s = &mut seq.tracks[0].clips[0].surfaces[0];
    s.media_fps = 30.0;
    s.keys = (0..=120).map(|f| SurfaceKey::at_corners(10.0 + f as f64 / 30.0, &quad(0.1), &quad(0.1 + 0.002 * f as f32)).unwrap()).collect();
    // Sequence frame 5 is file second 10.2083: 6.25 file frames in, so file frame 6 shows.
    let at = surface_pin_corners(&pin, &seq, tc(5.0 / 24.0), FPS, &none).unwrap();
    assert!(close(at, quad(0.1 + 0.002 * 6.0)), "{at:?}");
}

/// A portrait host in a wide frame is fitted in the middle: the surface's
/// corners, fractions of its picture, land there in the frame. A square
/// picture pinned to it has its fitted square, not the frame, on them.
#[test]
fn corners_follow_the_hosts_fit_and_placement() {
    let (mut seq, mut pin) = scene();
    let (w, h) = (seq.resolution.width as f32, seq.resolution.height as f32);
    let full = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    {
        let s = &mut seq.tracks[0].clips[0].surfaces[0];
        s.source_size = [1080, 1920];
        s.keys = vec![SurfaceKey::at_corners(10.0, &quad(0.1), &full).unwrap()];
    }
    let at = surface_pin_corners(&pin, &seq, tc(1.0), FPS, &none).unwrap();
    let shown_w = (1080.0 * h / 1920.0).round();
    let left = ((w - shown_w) / 2.0).floor() / w;
    assert!((at[0][0] - left).abs() < 1e-3 && (at[1][0] - (left + shown_w / w)).abs() < 1e-3, "{at:?}");

    // The host moved right by a tenth of the frame: the pin goes with it.
    let moved = surface_pin_corners(&pin, &seq, tc(1.0), FPS, &|_| Some((w * 0.1, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5))).unwrap();
    assert!((moved[0][0] - (left + 0.1)).abs() < 1e-3, "{moved:?}");

    // A square picture pinned there: the frame corners reach past the
    // quad, so the fitted square is what lands on it.
    pin.surface_pin.as_mut().unwrap().picture_size = Some([500, 500]);
    let square = surface_pin_corners(&pin, &seq, tc(1.0), FPS, &none).unwrap();
    assert!(square[0][0] < at[0][0] && square[1][0] > at[1][0], "{square:?} reaches past {at:?}");
}

#[test]
fn without_its_surface_the_pin_keeps_the_reference_corners() {
    let (mut seq, pin) = scene();
    seq.tracks[0].clips[0].surfaces.clear();
    assert_eq!(surface_pin_corners(&pin, &seq, tc(2.0), FPS, &none), None);
    let (fx, _) = clip_effects(&pin, tc(2.0), FPS, None, None);
    assert_eq!(fx.iter().find_map(corner_pin::corners), Some(quad(0.1)));

    let (mut seq, pin) = scene();
    seq.tracks[0].clips.clear();
    assert_eq!(surface_pin_corners(&pin, &seq, tc(2.0), FPS, &none), None, "the host is gone");
}

/// A square picture fitted to a wide surface, with a margin: its corner
/// pin puts the fitted square inside the surface, short of its edges,
/// where a stretch would reach them.
#[test]
fn a_fitted_pin_keeps_its_shape_inside_the_surface() {
    let (mut seq, mut pin) = scene();
    let full = [[0.1, 0.2], [0.9, 0.2], [0.9, 0.6], [0.1, 0.6]];
    let s = &mut seq.tracks[0].clips[0].surfaces[0];
    s.corners = full;
    s.keys = vec![SurfaceKey::at_corners(10.0, &full, &full).unwrap()];
    let stretched = surface_pin_corners(&pin, &seq, tc(1.0), FPS, &none).unwrap();
    assert!(close(stretched, full));
    let p = pin.surface_pin.as_mut().unwrap();
    p.picture_size = Some([seq.resolution.width, seq.resolution.height]);
    p.fit = klipp_timeline::PinFit::Fit;
    p.inset = 0.05;
    let fitted = surface_pin_corners(&pin, &seq, tc(1.0), FPS, &none).unwrap();
    // The surface is 0.8 x 0.4 of a 16:9 frame: 3.56:1, wider than the
    // picture, so the picture keeps the inset height and is narrower.
    assert!(fitted[0][1] > 0.2 && fitted[3][1] < 0.6, "inset from top and bottom: {fitted:?}");
    assert!(fitted[0][0] > 0.2 && fitted[1][0] < 0.8, "narrower than the surface: {fitted:?}");
}
