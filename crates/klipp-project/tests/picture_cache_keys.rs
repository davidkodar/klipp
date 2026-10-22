//! A clip's cached picture is written back only for the clip it belongs to
//! (#1172 review): a duplicated sequence keeps its clips' ids, and a
//! split's halves are clones given new ids.

use klipp_core::{Fps, Interpolation, Keyframe, MediaId, RationalTime, SequenceId};
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{Clip, Sequence};

fn key(time_secs: f64, value: f32) -> Keyframe {
    Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_picture_keys_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn reopen(dir: &std::path::Path, seqs: &[Sequence]) -> Vec<Sequence> {
    save_project(dir, seqs, &MediaPool::new()).unwrap();
    load_project(dir).unwrap().0
}

fn one_clip_sequence() -> Sequence {
    let t = |n| RationalTime::from_frames(n, 24);
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips.push(Clip::media(MediaId::new(), t(0), t(96), t(0), t(96)));
    seq
}

#[test]
fn editing_a_clip_in_a_duplicated_sequence_leaves_the_original_alone() {
    let dir = scratch("duplicate");
    let original = one_clip_sequence();
    // As Duplicate Sequence makes it: a new sequence, the same clip ids.
    let mut copy = original.clone();
    copy.id = SequenceId::new();
    let mut seqs = reopen(&dir, &[original, copy]);
    assert!(seqs.iter().all(|s| s.tracks[0].clips[0].has_lifted_picture()), "both keep their stored pictures");
    seqs[1].tracks[0].clips[0].edit_opacity_keyframes(|v| v.push(key(0.0, 0.25)));
    let seqs = reopen(&dir, &seqs);
    assert!(seqs[0].tracks[0].clips[0].opacity_keyframes().is_empty(), "the original's clip is as it was");
    assert_eq!(seqs[1].tracks[0].clips[0].opacity_keyframes().len(), 1, "the copy's clip has its edit");
    assert!(seqs.iter().all(|s| s.tracks[0].clips[0].has_lifted_picture()), "and each file picture is its clip's own");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_clip_split_after_loading_reopens_with_each_half_its_own_picture() {
    let dir = scratch("blade");
    let mut seq = one_clip_sequence();
    seq.tracks[0].clips[0].edit_opacity_keyframes(|v| v.extend([key(0.0, 1.0), key(3.0, 0.5)]));
    let mut seqs = reopen(&dir, &[seq]);
    let id = seqs[0].tracks[0].clips[0].id;
    assert!(seqs[0].tracks[0].clips[0].has_lifted_picture());
    seqs[0].tracks[0].blade_at(id, RationalTime::from_frames(48, 24), Fps { num: 24, den: 1 }).expect("the cut");
    assert_eq!(seqs[0].tracks[0].clips.len(), 2);
    // Reopening adopts a stored picture only if lifting the clip gives
    // exactly it (a mismatch stops a test): each half's own.
    let seqs = reopen(&dir, &seqs);
    let clips = &seqs[0].tracks[0].clips;
    assert_eq!(clips.len(), 2);
    assert_ne!(clips[0].id, clips[1].id);
    assert!(clips.iter().all(Clip::has_lifted_picture));
    let _ = std::fs::remove_dir_all(&dir);
}
