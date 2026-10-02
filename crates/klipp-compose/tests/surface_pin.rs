//! A pinned clip's corner pin is on its surface (#684): read live from
//! the host clip's track in the file's time, so a trimmed host still
//! carries the pin on the same picture, and a pin whose host is gone
//! stays on the reference frame's corners.

use klipp_compose::eval::surface_pin_corners;
use klipp_compose::render::clip_effects;
use klipp_core::{MediaId, Timecode};
use klipp_effects::{corner_pin, BuiltinEffect};
use klipp_timeline::{Clip, Sequence, Surface, SurfaceKey, SurfacePin};

const FPS: f64 = 24.0;

fn tc(s: f64) -> Timecode {
    Timecode::from_secs_f64(s, 24)
}

fn quad(x: f32) -> [[f32; 2]; 4] {
    [[x, 0.2], [x + 0.2, 0.2], [x + 0.2, 0.4], [x, 0.4]]
}

/// A host on video 1 showing file seconds 10..14 from timeline 0, with a
/// surface tracked from file 10 s (x 0.1) to 14 s (x 0.5); a pin above.
fn scene() -> (Sequence, Clip) {
    let mut seq = Sequence::new_default();
    let mut host = Clip::media(MediaId::new(), tc(0.0), tc(4.0), tc(10.0), tc(14.0));
    let id = klipp_core::ClipId::new().0;
    host.surfaces.push(Surface {
        id,
        name: "Screen".into(),
        corners: quad(0.1),
        reference_secs: 10.0,
        keys: vec![SurfaceKey { source_secs: 10.0, corners: quad(0.1) }, SurfaceKey { source_secs: 14.0, corners: quad(0.5) }],
    });
    let mut pin = Clip::media(MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0));
    let [tl, tr, br, bl] = quad(0.1);
    pin.builtin_effects.push(BuiltinEffect::CornerPin { tl, tr, br, bl });
    pin.surface_pin = Some(SurfacePin { host: host.id, surface: id });
    seq.tracks[0].clips.push(host);
    (seq, pin)
}

#[test]
fn the_pin_follows_the_surface_in_file_time() {
    let (seq, pin) = scene();
    let at = surface_pin_corners(&pin, &seq, tc(2.0), FPS).expect("pinned");
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
    let at = surface_pin_corners(&pin, &seq, tc(3.0), FPS).unwrap();
    assert!((at[0][0] - 0.3).abs() < 1e-4, "file 12 s is now at timeline 3 s: {:?}", at[0]);
}

#[test]
fn without_its_surface_the_pin_keeps_the_reference_corners() {
    let (mut seq, pin) = scene();
    seq.tracks[0].clips[0].surfaces.clear();
    assert_eq!(surface_pin_corners(&pin, &seq, tc(2.0), FPS), None);
    let (fx, _) = clip_effects(&pin, tc(2.0), FPS, None, None);
    assert_eq!(fx.iter().find_map(corner_pin::corners), Some(quad(0.1)));

    let (mut seq, pin) = scene();
    seq.tracks[0].clips.clear();
    assert_eq!(surface_pin_corners(&pin, &seq, tc(2.0), FPS), None, "the host is gone");
}
