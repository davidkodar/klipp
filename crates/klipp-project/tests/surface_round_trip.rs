//! A tracked surface and the clip pinned to it come back from a project
//! as they went in (#684); a project without either writes neither.

use klipp_core::Timecode;
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{Clip, Sequence, Surface, SurfaceKey, SurfacePin};

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_surface_and_its_pin_read_back() {
    let dir = scratch("surface");
    let tc = |s: f64| Timecode::from_secs_f64(s, 24);
    let scene = Sequence::new_default();
    let mut outer = Sequence::new_default();
    let quad = [[0.3, 0.3], [0.6, 0.32], [0.62, 0.6], [0.28, 0.58]];
    let surface = Surface {
        id: uuid::Uuid::new_v4(),
        name: "Phone".into(),
        corners: quad,
        reference_secs: 2.5,
        keys: vec![SurfaceKey::at_corners(2.5, &quad, &quad).unwrap(), SurfaceKey { confidence: Some(0.4), state: klipp_timeline::TrackState::Low, ..SurfaceKey::at_corners(2.54, &quad, &[[0.31, 0.3], [0.61, 0.32], [0.63, 0.6], [0.29, 0.58]]).unwrap() }],
        source_size: [1080, 1920],
        media_fps: 25.0,
        // A larger area followed, with a part of it left out (#2754).
        track_area: Some([[0.2, 0.2], [0.7, 0.22], [0.72, 0.7], [0.18, 0.68]]),
        exclude: vec![vec![[0.4, 0.4], [0.5, 0.4], [0.45, 0.5]]],
    };
    let mut host = Clip::sequence(scene.id, tc(0.0), tc(4.0));
    host.surfaces.push(surface.clone());
    let mut pinned = Clip::sequence(scene.id, tc(0.0), tc(4.0));
    pinned.surface_pin = Some(SurfacePin { host: host.id, surface: surface.id, picture_size: Some([640, 640]), fit: klipp_timeline::PinFit::Fit, inset: 0.02, on_loss: klipp_timeline::PinLoss::Fade { secs: 0.5 }, light: 0.6 });
    let (host_id, pinned_id) = (host.id, pinned.id);
    outer.tracks[0].clips = vec![host];
    outer.tracks.insert(0, klipp_timeline::Track::new("V2", klipp_timeline::TrackKind::Video));
    outer.tracks[0].clips = vec![pinned];

    save_project(&dir, &[outer, scene], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    let find = |id| sequences[0].tracks.iter().find_map(|t| t.clips.iter().find(|c| c.id == id)).expect("the clip");
    assert_eq!(find(host_id).surfaces, vec![surface.clone()]);
    assert_eq!(find(pinned_id).surface_pin, Some(SurfacePin { host: host_id, surface: surface.id, picture_size: Some([640, 640]), fit: klipp_timeline::PinFit::Fit, inset: 0.02, on_loss: klipp_timeline::PinLoss::Fade { secs: 0.5 }, light: 0.6 }));
    assert!(find(host_id).surface_pin.is_none() && find(pinned_id).surfaces.is_empty());
    let written = std::fs::read_to_string(dir.join("project.json")).unwrap();
    assert_eq!(written.matches("\"surfaces\"").count(), 1, "only the host writes surfaces");
    assert_eq!(written.matches("\"surface_pin\"").count(), 1, "only the pin writes its link");
    let _ = std::fs::remove_dir_all(&dir);
}
