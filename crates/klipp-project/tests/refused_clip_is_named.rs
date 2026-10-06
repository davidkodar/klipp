//! A clip whose picture can't be lifted refuses the save, and the refusal
//! names the clip and its track (#1089, M1b 2b), so the person can find it.

use klipp_core::{MediaId, RationalTime};
use klipp_media::MediaPool;
use klipp_project::save_project;
use klipp_timeline::{Clip, GradeAccess, Sequence};

#[test]
fn a_clip_that_cannot_be_lifted_is_named_when_the_save_is_refused() {
    let t = |n| RationalTime::from_frames(n, 24);
    let mut seq = Sequence::new_default();
    let mut clip = Clip::media(MediaId::new(), t(0), t(48), t(0), t(48));
    // A grade repeating a node id: the lift refuses it.
    clip.edit_grade(|g| {
        let again = g.nodes[0].clone();
        g.nodes.push(again);
    });
    let id = clip.id.0.to_string();
    seq.tracks[0].clips.push(clip);
    let track = seq.tracks[0].name.clone();
    let dir = std::env::temp_dir().join(format!("klipp_refused_clip_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let err = save_project(&dir, &[seq], &MediaPool::new()).unwrap_err();
    assert!(err.contains(&id), "names the clip: {err}");
    assert!(err.contains(&format!("\"{track}\"")), "names its track: {err}");
    assert!(err.contains("repeats node"), "says why: {err}");
    assert!(!dir.join("project.json").exists(), "and writes nothing");
    let _ = std::fs::remove_dir_all(&dir);
}
