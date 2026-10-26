//! A clip's effects, and whether each is enabled, come back from the app's
//! revision-2 save as they went in (#1089), including absent, empty, short
//! and surplus enabled-flag inputs.
//!
//! What is stored is made explicit once, by the conversion: one flag per
//! effect. Every reader takes a missing flag as enabled and ignores a
//! surplus one (`render_gates.rs`, `effect_slots.rs`), so an absent or
//! short list is filled with `true` and a long one is cut, and nothing
//! renders differently. The bytes a clip's picture fields save as are
//! pinned in klipp-timeline's `picture_v1` tests.

use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::Sequence;

#[test]
fn a_clips_effects_and_flags_survive_a_revision_2_save() {
    let input = include_str!("fixtures/effect_list/legacy_sequence.json");
    let seq: Sequence = serde_json::from_str(input).unwrap();
    // Each effect with whether it is enabled, as every reader takes it.
    let enabled = |c: &klipp_timeline::Clip| -> Vec<_> {
        c.builtin_effects().iter().enumerate().map(|(i, e)| (e.clone(), c.builtin_effects_enabled().get(i).copied().unwrap_or(true))).collect()
    };
    let before: Vec<_> = seq.all_clips().map(enabled).collect();
    let dir = std::env::temp_dir().join(format!("klipp-effect-list-rt-{}", uuid::Uuid::new_v4()));
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let project: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).unwrap()).unwrap();
    assert_eq!(project["format_revision"], klipp_project::FORMAT_REVISION, "the app writes today's revision");
    let (loaded, ..) = load_project(&dir).unwrap();
    assert_eq!(loaded[0].all_clips().map(enabled).collect::<Vec<_>>(), before);
    // The stored flags, made explicit: absent and empty become all enabled,
    // short is filled, surplus is cut.
    let flags: Vec<_> = loaded[0].all_clips().map(|c| c.builtin_effects_enabled().to_vec()).collect();
    assert_eq!(flags, vec![vec![true, true], vec![true, true], vec![false, true], vec![true, false]]);
    std::fs::remove_dir_all(dir).unwrap();
}
