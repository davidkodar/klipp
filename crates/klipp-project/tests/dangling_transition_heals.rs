//! A transition whose clip is gone (#979) — left behind by a removal
//! before transitions went with their clips, and kept by every save — is
//! dropped when the project opens; one between two clips that are there
//! stays.

use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{Clip, ClipKind, Sequence, Transition, TransitionKind};

#[test]
fn a_transition_naming_a_gone_clip_is_dropped_on_opening() {
    let dir = std::env::temp_dir().join(format!("klipp_dangling_transition_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut seq = Sequence::new_default();
    let solid = klipp_core::GeneratorData::SolidColor { color: [10, 10, 30, 255] };
    let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let card = |a: f64, b: f64| Clip { kind: ClipKind::Generator(solid.clone()), ..Clip::gap(tc(a), tc(b)) };
    let (a, b) = (card(0.0, 2.0), card(2.0, 4.0));
    let dissolve = |from, to| Transition {
        id: klipp_core::TransitionId::new(),
        kind: TransitionKind::CrossDissolve,
        duration: tc(0.5),
        clip_a_id: from,
        clip_b_id: to,
        user_params: Default::default(),
        alignment: Default::default(),
    };
    let kept = dissolve(a.id, b.id);
    let gone = dissolve(b.id, klipp_core::ClipId::new());
    seq.tracks[0].transitions = vec![kept, gone];
    seq.tracks[0].clips = vec![a, b];
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    let ids: Vec<_> = sequences[0].tracks[0].transitions.iter().map(|t| t.id).collect();
    assert_eq!(ids, vec![kept.id], "the dangling one is dropped, the real one kept");
    let _ = std::fs::remove_dir_all(&dir);
}
