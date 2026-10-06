//! Frozen output from eb3b9207, before EffectList replaced the raw fields.
//! Only per-save metadata is normalized; project field order and bytes stay
//! pinned, including absent, empty, short and surplus enabled-flag inputs.

use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::Sequence;

#[test]
fn encapsulating_effects_keeps_the_saved_project_bytes() {
    let input = include_str!("fixtures/effect_list/legacy_sequence.json");
    let legacy: serde_json::Value = serde_json::from_str(input).unwrap();
    let clips = &legacy["tracks"][0]["clips"];
    assert!(clips[0].get("builtin_effects_enabled").is_none());
    assert_eq!(clips[1]["builtin_effects_enabled"], serde_json::json!([]));
    let mut seq: Sequence = serde_json::from_str(input).unwrap();
    for clip in seq.all_clips_mut() {
        // Allocating session ids must not normalize persisted flags or save ids.
        assert_eq!(clip.effect_entries().len(), 2);
    }
    let dir = std::env::temp_dir().join(format!("klipp-effect-list-{}", uuid::Uuid::new_v4()));
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let mut written = std::fs::read_to_string(dir.join("project.json")).unwrap();
    let project: serde_json::Value = serde_json::from_str(&written).unwrap();
    for key in ["generation", "saved_at", "platform", "klipp_version"] {
        let field = format!("\"{key}\": {}", serde_json::to_string(&project[key]).unwrap());
        written = written.replace(&field, &format!("\"{key}\": \"{key}\""));
    }
    assert_eq!(written, include_str!("fixtures/effect_list/legacy_project.json"));
    let (loaded, ..) = load_project(&dir).unwrap();
    let flags: Vec<_> = loaded[0].all_clips().map(|c| c.builtin_effects_enabled().to_vec()).collect();
    assert_eq!(flags, vec![vec![], vec![], vec![false], vec![true, false, true]]);
    std::fs::remove_dir_all(dir).unwrap();
}
