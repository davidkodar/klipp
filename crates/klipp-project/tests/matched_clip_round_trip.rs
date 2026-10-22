//! A clip kept matched to a shot is still kept matched to it when the
//! project is opened again (#2236), and a clip that is not writes nothing
//! for it.

use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{Clip, Sequence, TrackKind};

#[test]
fn a_clip_kept_matched_is_kept_matched_when_reopened() {
    let dir = std::env::temp_dir().join(format!("klipp_matched_clip_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut seq = Sequence::new_default();
    seq.tracks.insert(0, klipp_timeline::Track::new("V2", TrackKind::Video));
    let shot = Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0));
    let mut title = Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0));
    title.matched_to = Some(shot.id);
    let (shot_id, title_id) = (shot.id, title.id);
    seq.tracks[1].clips = vec![shot];
    seq.tracks[0].clips = vec![title];
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let written = std::fs::read_to_string(dir.join("project.json")).unwrap();
    assert_eq!(written.matches("\"matched_to\"").count(), 1, "only the matched clip writes it");
    let (sequences, ..) = load_project(&dir).unwrap();
    let matched = |id| sequences[0].all_clips().find(|c| c.id == id).unwrap().matched_to;
    assert_eq!((matched(title_id), matched(shot_id)), (Some(shot_id), None));
    let _ = std::fs::remove_dir_all(&dir);
}
