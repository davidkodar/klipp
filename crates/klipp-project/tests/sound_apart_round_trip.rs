//! A picture whose sound was put on a sound clip of its own plays none of
//! its own, linked or not; it stays so in the saved project, and a clip
//! without it writes nothing, so a project from before reads as it did.

use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::Sequence;

#[test]
fn a_pictures_sound_kept_apart_comes_back_with_the_project() {
    let dir = std::env::temp_dir().join(format!("klipp_sound_apart_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut seq = Sequence::new_default();
    let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let media = klipp_core::MediaId::new();
    let mut apart = klipp_timeline::Clip::media(media, tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    apart.sound_apart = true;
    let plain = klipp_timeline::Clip::media(media, tc(2.0), tc(4.0), tc(0.0), tc(2.0));
    seq.tracks[0].clips = vec![apart, plain];
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    let kept: Vec<bool> = sequences[0].tracks[0].clips.iter().map(|c| c.sound_apart).collect();
    assert_eq!(kept, vec![true, false]);
    let written = std::fs::read_to_string(dir.join("project.json")).unwrap();
    assert_eq!(written.matches("\"sound_apart\"").count(), 1, "only the picture kept apart writes it");
    let _ = std::fs::remove_dir_all(&dir);
}
