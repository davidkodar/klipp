//! Save/load round-trip for the project-level `working_colorspace`
//! setting. The format-level back-compat (legacy `project.json`
//! without the field deserialises as `Rec709Linear`) is pinned at
//! the `klipp_project::format::tests` layer; this test verifies the
//! whole pipeline — `ColorSettings` → `save_project_with_color` →
//! `load_project` — round-trips a non-default value cleanly.

use klipp_core::WorkingColorspace;
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project_with_color, ColorSettings};
use klipp_timeline::Sequence;

fn project_dir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "klipp_ws_test_{}_{}",
        label,
        std::process::id(),
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn acescg_setting_round_trips_through_save_load() {
    let dir = project_dir("acescg");
    let seq = Sequence::new_default();
    let pool = MediaPool::new();

    let color = ColorSettings {
        working_colorspace: WorkingColorspace::AcesCG,
        ..Default::default()
    };

    save_project_with_color(&dir, &[seq], &pool, &color, None, None, "").unwrap();

    let (_seqs, _pool, loaded_color, _active, _created, _notes, _zoom, _scroll, _track_scale, _assets) =
        load_project(&dir).unwrap();
    assert_eq!(
        loaded_color.working_colorspace,
        WorkingColorspace::AcesCG,
        "AcesCG should round-trip through ColorSettings → ProjectJson → load",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn default_setting_round_trips_as_rec709_linear() {
    let dir = project_dir("default");
    let seq = Sequence::new_default();
    let pool = MediaPool::new();

    // Default ColorSettings — never touch working_colorspace, then
    // confirm it's Rec709Linear on the way back. This is the path
    // every existing project takes today.
    let color = ColorSettings::default();
    save_project_with_color(&dir, &[seq], &pool, &color, None, None, "").unwrap();

    let (_seqs, _pool, loaded_color, _active, _created, _notes, _zoom, _scroll, _track_scale, _assets) =
        load_project(&dir).unwrap();
    assert_eq!(
        loaded_color.working_colorspace,
        WorkingColorspace::Rec709Linear,
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn legacy_project_without_working_colorspace_field_loads_as_rec709_linear() {
    // Simulates a project file written by a pre-Phase-E build: the
    // `working_colorspace` field is missing from `project.json`.
    // Serde's `#[serde(default)]` should give us `Rec709Linear`.
    let dir = project_dir("legacy");
    let seq = Sequence::new_default();
    let pool = MediaPool::new();

    // First save normally.
    save_project_with_color(
        &dir, &[seq], &pool, &ColorSettings::default(), None, None, "",
    ).unwrap();

    // Rewrite project.json with the field stripped.
    let project_json_path = dir.join("project.json");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&project_json_path).unwrap()).unwrap();
    let mut json = json;
    if let Some(obj) = json.as_object_mut() {
        obj.remove("working_colorspace");
    }
    std::fs::write(&project_json_path, serde_json::to_string_pretty(&json).unwrap()).unwrap();

    let (_seqs, _pool, loaded_color, _active, _created, _notes, _zoom, _scroll, _track_scale, _assets) =
        load_project(&dir).unwrap();
    assert_eq!(
        loaded_color.working_colorspace,
        WorkingColorspace::Rec709Linear,
        "legacy project.json (no working_colorspace field) should load as Rec709Linear via serde default",
    );

    let _ = std::fs::remove_dir_all(&dir);
}
