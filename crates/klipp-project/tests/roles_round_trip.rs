//! Roles saved and opened again (#3211): a clip's role, none set by hand
//! told from none ever set, the project's own roles and colours; a file
//! from before roles were data read as the roles its clips' had are now;
//! and the roles of a file someone else wrote made sound.

use klipp_core::roles::{RoleRef, RoleSet};
use klipp_core::{RationalTime, RoleSource};
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{Clip, Sequence, TrackKind};

fn project_dir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_roles_test_{label}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

/// Four clips on the first audio track: an interview set by hand, one
/// set to no role by hand, one never set, and one in a role the project
/// added.
fn saved(dir: &std::path::Path) -> (Sequence, MediaPool) {
    let mut pool = MediaPool::new();
    let media = pool.add(klipp_media::MediaItem::synthetic_for_test("/tmp/roles.wav".into(), None));
    let trailer = pool.roles.add(None, "Trailer VO").unwrap();
    pool.roles.set_colour(&RoleRef::of("music"), Some([10, 20, 30, 255])).unwrap();
    let mut seq = Sequence::new_default();
    let audio = seq.tracks.iter().position(|t| t.kind == TrackKind::Audio).unwrap();
    let clip = |n: f64, role: Option<RoleRef>, source: RoleSource| Clip { role, role_source: source, ..Clip::media(media, tc(n), tc(n + 1.0), tc(0.0), tc(1.0)) };
    seq.tracks[audio].clips = vec![
        clip(0.0, RoleRef::parse("dialogue.interview"), RoleSource::ByHand),
        clip(1.0, None, RoleSource::ByHand),
        clip(2.0, None, RoleSource::Unset),
        clip(3.0, Some(trailer), RoleSource::ByHand),
    ];
    save_project(dir, std::slice::from_ref(&seq), &pool).unwrap();
    (seq, pool)
}

fn roles_of(seq: &Sequence) -> Vec<(Option<String>, RoleSource)> {
    let audio = seq.tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap();
    audio.clips.iter().map(|c| (c.role.as_ref().map(|r| r.to_string()), c.role_source)).collect()
}

#[test]
fn clip_roles_and_the_projects_roles_save_and_open_again() {
    let dir = project_dir("round");
    let (seq, pool) = saved(&dir);
    let (seqs, loaded, ..) = load_project(&dir).unwrap();
    assert_eq!(roles_of(&seqs[0]), roles_of(&seq), "each clip's role, and none by hand kept apart from none ever set");
    assert_eq!(loaded.roles, pool.roles, "the project's roles and colours");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A project saved before roles were data: a clip's old role name, a
/// role set by hand that was Dialogue (written as nothing), and a clip
/// whose role was never set, opened as the roles they are now.
#[test]
fn a_file_from_before_roles_were_data_opens_with_the_roles_now() {
    let dir = project_dir("legacy");
    saved(&dir);
    let path = dir.join("project.json");
    let mut project: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let clips = project["sequences"][0]["tracks"].as_array_mut().unwrap().iter_mut().find(|t| t["clips"].as_array().is_some_and(|c| c.len() == 4)).unwrap()["clips"].as_array_mut().unwrap();
    for c in clips.iter_mut() {
        c.as_object_mut().unwrap().remove("role_id");
    }
    clips[0]["role"] = "Voiceover".into();
    clips[3]["role"] = "BRoll".into();
    std::fs::write(&path, serde_json::to_vec(&project).unwrap()).unwrap();
    let (seqs, ..) = load_project(&dir).unwrap();
    assert_eq!(
        roles_of(&seqs[0]),
        vec![
            (Some("dialogue.narration".into()), RoleSource::ByHand),
            (Some("dialogue".into()), RoleSource::ByHand),
            (None, RoleSource::Unset),
            (Some("effects.ambience".into()), RoleSource::ByHand),
        ],
        "Voiceover is narration; nothing written but set was Dialogue; never set is none; B-roll is ambience",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The roles of a file someone else wrote: ids that aren't ids, one
/// given twice, a name of control characters, far too many, and the
/// built-in roles left out. It opens; what can't be a role is left out,
/// the built-in roles are there, and there are no more than the most.
#[test]
fn a_hostile_files_roles_are_made_sound() {
    let dir = project_dir("hostile");
    saved(&dir);
    let path = dir.join("media").join("manifest.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut roles = vec![
        serde_json::json!({"id": "../../etc", "name": "Escape"}),
        serde_json::json!({"id": "dup", "name": "One"}),
        serde_json::json!({"id": "dup", "name": "Two"}),
        serde_json::json!({"id": "ctrl", "name": "\u{7}\u{1b}[2J", "subs": [{"id": "Bad Id", "name": "x"}, {"id": "ok", "name": "Fine"}]}),
    ];
    roles.extend((0..200).map(|n| serde_json::json!({"id": format!("r{n}"), "name": "n".repeat(500)})));
    manifest["roles"] = serde_json::json!({ "roles": roles });
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();

    let (_, pool, ..) = load_project(&dir).unwrap();
    let ids: Vec<&str> = pool.roles.roles.iter().map(|r| r.id.as_str()).collect();
    assert!(ids.len() <= klipp_core::roles::MAX_ROLES, "at most the most: {}", ids.len());
    assert!(!ids.contains(&"../../etc"), "an id that isn't one is left out");
    assert_eq!(ids.iter().filter(|i| **i == "dup").count(), 1, "an id given twice, once");
    for b in klipp_core::roles::BUILT_IN {
        assert!(ids.contains(&b), "{b} put back");
    }
    let ctrl = pool.roles.main("ctrl").unwrap();
    assert_eq!((ctrl.name.as_str(), ctrl.subs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>()), ("[2J", vec!["ok"]), "control characters taken out; a sub-role whose id isn't one left out");
    assert!(pool.roles.roles.iter().all(|r| r.name.chars().count() <= klipp_core::roles::MAX_NAME_CHARS), "names cut to length");
    assert_ne!(pool.roles, RoleSet::built_in());
    let _ = std::fs::remove_dir_all(&dir);
}
