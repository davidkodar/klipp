//! Full typed-picture parity, using only disposable project copies.
use klipp_graph_format::{
    path::{NodePath, Root},
    types,
};
use klipp_graph_lift::ops::GraphOp;
use std::path::Path;

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
#[ignore = "requires KLIPP_PROJECT_LIST naming exactly 18 project directories"]
fn on_a_linear_spine_remove_equals_the_old_splice() {
    let manifest =
        std::fs::read_to_string(std::env::var_os("KLIPP_PROJECT_LIST").expect("corpus manifest"))
            .unwrap();
    let projects: Vec<_> = manifest
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(projects.len(), 18);
    let scratch_root = std::env::var_os("KLIPP_PLANNER_SCRATCH_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let scratch = Scratch(scratch_root.join(format!("planner-corpus-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&scratch.0).unwrap();
    // Conversion scanners must never mistake harness copies for user projects.
    // This is the converter's SCRATCH_MARK; only this new directory is marked.
    std::fs::write(scratch.0.join(".klipp-scratch"), []).unwrap();
    let (mut clips, mut removals, mut placed) = (0, 0, 0);
    for (index, directory) in projects.iter().enumerate() {
        let original = Path::new(directory).join("project.json");
        let bytes = std::fs::read(&original).unwrap();
        let copy = scratch.0.join(format!("{index}.klipp"));
        std::fs::create_dir_all(&copy).unwrap();
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            if entry.path().is_file() {
                std::fs::copy(entry.path(), copy.join(entry.file_name())).unwrap();
            }
        }
        let media = Path::new(directory).join("media/manifest.json");
        if media.is_file() {
            std::fs::create_dir_all(copy.join("media")).unwrap();
            std::fs::copy(media, copy.join("media/manifest.json")).unwrap();
        }
        let stored: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        if stored["format_revision"] == 2 {
            let revised =
                klipp_graph_lift::revision3::project_text(std::str::from_utf8(&bytes).unwrap())
                    .expect("scratch revision-3 conversion");
            std::fs::write(copy.join("project.json"), revised).unwrap();
        }
        let (sequences, ..) =
            klipp_project::load_project(&copy).unwrap_or_else(|e| panic!("project {index}: {e}"));
        for c in sequences
            .iter()
            .flat_map(|s| &s.tracks)
            .flat_map(|t| &t.clips)
        {
            let before = c.picture_v2().unwrap();
            assert!(
                !c.is_picture_truth(),
                "the independent old-list oracle requires a fields-stored corpus clip"
            );
            clips += 1;
            for (slot, effect) in c.builtin_effects().iter().enumerate() {
                if !klipp_timeline::takes_picture_ops(effect) {
                    placed += 1;
                    continue;
                }
                let node = before
                    .picture
                    .graph
                    .nodes
                    .iter()
                    .find(|n| {
                        n.type_id.as_str() == types::LEGACY_EFFECT
                            && n.legacy.as_ref().and_then(|l| l.slot) == Some(slot as u32)
                    })
                    .expect("effect row");
                let mut old = c.clone();
                assert!(old.remove_builtin_effect(slot).is_some());
                let mut planned = c.clone();
                let edit = planned
                    .apply_picture_ops(&[GraphOp::RemoveNode {
                        path: NodePath::new(Root::Clip { clip: c.id.0 }, [node.id]),
                    }])
                    .unwrap_or_else(|e| panic!("project {index}, slot {slot}: {e}"));
                assert_eq!(
                    *planned.picture_v2().unwrap(),
                    *old.picture_v2().unwrap(),
                    "project {index}, slot {slot}: complete picture and grade versions"
                );
                planned.restore_picture(&edit.before);
                assert_eq!(
                    *planned.picture_v2().unwrap(),
                    *before,
                    "undo restores the complete picture"
                );
                removals += 1;
            }
        }
        assert_eq!(
            std::fs::read(original).unwrap(),
            bytes,
            "original project changed"
        );
    }
    assert!(clips > 0 && removals > 0);
    eprintln!("18 projects: {clips} clips, {removals} ordinary removals; {placed} placed-kind entries stay on their existing mutator route");
}
