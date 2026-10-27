//! A precomposition is a clip kind of its own in the project file, appended
//! after every earlier kind. An editorial compound (`sequence`) reads back
//! as it always did, a precomposition reads back as a precomposition, and a
//! reader from before the kind existed refuses the file by name instead of
//! reading the scene as something it is not.

use klipp_core::{TemplateInstance, TemplateValue, Timecode};
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{Clip, ClipKind, Sequence};
use std::collections::BTreeMap;

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_compound_and_a_precomposition_each_come_back_as_themselves() {
    let dir = scratch("precomp");
    let tc = |s: f64| Timecode::from_secs_f64(s, 24);
    let scene = Sequence::new_default();
    let mut outer = Sequence::new_default();
    outer.tracks[0].clips = vec![Clip::sequence(scene.id, tc(0.0), tc(2.0)), Clip::precomposition(scene.id, tc(2.0), tc(4.0))];
    let (compound_id, precomp_id) = (outer.tracks[0].clips[0].id, outer.tracks[0].clips[1].id);

    save_project(&dir, &[outer, scene.clone()], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    let clips = &sequences[0].tracks[0].clips;
    assert_eq!(clips.len(), 2);
    assert_eq!(clips[0].id, compound_id);
    assert!(matches!(clips[0].kind, ClipKind::Sequence { sequence_id, .. } if sequence_id == scene.id), "the compound is still a compound: {:?}", clips[0].kind);
    assert_eq!(clips[1].id, precomp_id);
    assert!(matches!(clips[1].kind, ClipKind::Precomposition { sequence_id, .. } if sequence_id == scene.id), "the precomposition is still one: {:?}", clips[1].kind);
    assert_eq!(clips[1].timeline_in, tc(2.0));
    assert_eq!(clips[1].source_in, tc(0.0));

    let written = std::fs::read_to_string(dir.join("project.json")).unwrap();
    assert!(written.contains("\"precomposition\""), "the kind is written by its own name");
    assert!(written.contains("\"sequence\""), "and the compound by its old one");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The clip kinds a reader had before this one existed: the same tag and
/// the same names, without `precomposition`. What happens when such a
/// reader meets the new kind is the guarantee the appended variant makes.
#[test]
fn a_reader_from_before_the_kind_refuses_it_by_name() {
    #[derive(Debug, serde::Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    #[allow(dead_code)]
    enum Before {
        Media { media_id: String },
        Gap,
        Sequence { sequence_id: String },
        AdjustmentLayer,
    }
    let precomp = serde_json::json!({ "type": "precomposition", "sequence_id": "2b1e5d7c-6d7a-4a3b-9d4e-0f1a2b3c4d5e" }).to_string();
    let err = serde_json::from_str::<Before>(&precomp).unwrap_err().to_string();
    assert!(err.contains("precomposition"), "the refusal names the kind: {err}");

    let compound = serde_json::json!({ "type": "sequence", "sequence_id": "2b1e5d7c-6d7a-4a3b-9d4e-0f1a2b3c4d5e" }).to_string();
    assert!(matches!(serde_json::from_str::<Before>(&compound), Ok(Before::Sequence { .. })), "the compound still reads as before");

    let template_precomp = serde_json::json!({
        "type": "template_precomposition",
        "sequence_id": "2b1e5d7c-6d7a-4a3b-9d4e-0f1a2b3c4d5e",
        "template_instance": { "template_id": "2b1e5d7c-6d7a-4a3b-9d4e-0f1a2b3c4d5e", "overrides": {}, "media_slots": {} }
    }).to_string();
    let err = serde_json::from_str::<Before>(&template_precomp).unwrap_err().to_string();
    assert!(err.contains("template_precomposition"), "the old reader refuses the live template kind by name: {err}");
}

#[test]
fn a_template_instance_survives_project_round_trip() {
    let dir = scratch("template_instance");
    let tc = |s: f64| Timecode::from_secs_f64(s, 24);
    let scene = Sequence::new_default();
    let template_id = uuid::Uuid::new_v4();
    let mut clip = Clip::precomposition(scene.id, tc(0.0), tc(2.0));
    clip.kind = ClipKind::TemplatePrecomposition {
        sequence_id: scene.id,
        template_instance: TemplateInstance {
            template_id,
            bindings: BTreeMap::from([("command_text".into(), "object:1.text".into())]),
            overrides: BTreeMap::from([(
                "command_text".into(),
                TemplateValue::Text("xytx --launch".into()),
            )]),
            media_slots: BTreeMap::new(),
            links: Vec::new(),
        },
    };
    let mut outer = Sequence::new_default();
    outer.tracks[0].clips = vec![clip];
    save_project(&dir, &[outer, scene], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    let ClipKind::TemplatePrecomposition {
        template_instance: instance,
        ..
    } = &sequences[0].tracks[0].clips[0].kind
    else {
        panic!("template instance was not persisted");
    };
    assert_eq!(instance.template_id, template_id);
    assert_eq!(
        instance.overrides.get("command_text"),
        Some(&TemplateValue::Text("xytx --launch".into()))
    );
    let _ = std::fs::remove_dir_all(&dir);
}
