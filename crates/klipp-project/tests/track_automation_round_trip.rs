//! A track's own automation (#2306) is saved with the project and read
//! back; a track without any writes nothing for it.

use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{AutomationMode, Sequence, Track, TrackKind, TrackParam};

#[test]
fn a_tracks_automation_is_saved_and_read_back() {
    let key = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
    let mut seq = Sequence::new_default();
    let mut music = Track::new("Music", TrackKind::Audio);
    music.automation.mode = AutomationMode::Latch;
    music.automation.lanes.insert(TrackParam::Volume, vec![key(1.0, 1.0), key(3.0, 0.25)]);
    music.automation_lane_visible = Some(klipp_core::AudioParam::Pan);
    music.automation_lane_track = true;
    seq.tracks.push(music);
    let mut dialogue = Track::new("Dialogue", TrackKind::Audio);
    dialogue.automation_lane_visible = Some(klipp_core::AudioParam::Pan);
    seq.tracks.push(dialogue);
    let dir = std::env::temp_dir().join(format!("klipp_track_automation_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    save_project(&dir, &[seq.clone()], &MediaPool::new()).unwrap();
    let written = std::fs::read_to_string(dir.join("project.json")).unwrap();
    let (mut sequences, ..) = load_project(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let back = sequences.remove(0);
    let of = |name: &str| back.tracks.iter().find(|t| t.name == name).map(|t| t.automation.clone()).unwrap();
    assert_eq!(of("Music"), seq.tracks.iter().find(|t| t.name == "Music").unwrap().automation);
    assert!(of("Dialogue").is_default());
    let lane = |name: &str| back.tracks.iter().find(|t| t.name == name).map(|t| (t.automation_lane_visible, t.automation_lane_track)).unwrap();
    assert_eq!(lane("Music"), (Some(klipp_core::AudioParam::Pan), true), "the lane shows the track's own pan keys");
    assert_eq!(lane("Dialogue"), (Some(klipp_core::AudioParam::Pan), false), "the lane shows the clips' pan keys");
    assert_eq!(written.matches("\"automation\"").count(), 1, "only the automated track writes it");
}
