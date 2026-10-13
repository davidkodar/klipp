//! The grade library (#899): whole grades saved across projects, each with
//! a graded thumbnail of the clip it was taken from, to browse and apply
//! to any clip. Kept with the whole-grade presets in the user's config
//! folder (`presets/color`): `<name>.json`, the grade, and `<name>.png`,
//! its thumbnail; and `<name>.labels`, its labels as JSON (#614): the camera
//! and profile it was made for, and notes. A label is only a note to
//! search by: nothing grades footage from its camera by itself.

use std::path::{Path, PathBuf};

use klipp_nodes::NodeGraph;

/// What a saved grade is labelled with (#614): words to find it by.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GradeLabels {
    #[serde(default)]
    pub camera: String,
    #[serde(default)]
    pub profile: String,
    #[serde(default)]
    pub notes: String,
}

impl GradeLabels {
    pub fn is_empty(&self) -> bool {
        self.camera.trim().is_empty() && self.profile.trim().is_empty() && self.notes.trim().is_empty()
    }

    /// The labels as one line: camera, profile, then notes.
    pub fn line(&self) -> String {
        [&self.camera, &self.profile, &self.notes].into_iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("  \u{b7}  ")
    }
}

/// One saved grade.
pub struct LibraryGrade {
    pub name: String,
    pub graph: NodeGraph,
    /// Its labels (#614).
    pub labels: GradeLabels,
    /// Its nodes' colour labels, by path inside it (#1866).
    pub node_labels: Vec<klipp_timeline::GradeNodeLabel>,
    /// Its thumbnail, sRGB RGBA8, when it has one.
    pub thumbnail: Option<klipp_render::still::Still>,
    pub texture: Option<egui::TextureHandle>,
}

/// The library's folder.
pub fn dir() -> Option<PathBuf> {
    crate::presets::user_presets_dir()
}

/// The library's grades in `dir`, by name, each with its thumbnail when
/// one was saved.
pub fn load_in(dir: &Path) -> Vec<LibraryGrade> {
    crate::presets::load_labelled_presets_in(dir)
        .into_iter()
        .map(|(name, graph, node_labels)| {
            let thumbnail = image::open(dir.join(format!("{}.png", crate::presets::preset_stem(&name)))).ok().map(|img| {
                let rgba = img.to_rgba8();
                klipp_render::still::Still { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() }
            });
            let labels = std::fs::read_to_string(dir.join(format!("{}.labels", crate::presets::preset_stem(&name))))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default();
            LibraryGrade { name, graph, labels, node_labels, thumbnail, texture: None }
        })
        .collect()
}

/// Save `graph` to the library in `dir` as `name`, with `thumbnail`: a
/// grade of that name already there is replaced (as a preset of a newer
/// Klipp is never). An empty name, or a grade that wouldn't save whole,
/// is refused.
pub fn save_in(dir: &Path, name: &str, graph: &NodeGraph, thumbnail: Option<&klipp_render::still::Still>) -> Result<(), String> {
    save_labelled_in(dir, name, graph, &[], thumbnail)
}

/// [`save_in`], with the grade's node colour labels kept in its file
/// (#1866).
pub fn save_labelled_in(dir: &Path, name: &str, graph: &NodeGraph, node_labels: &[klipp_timeline::GradeNodeLabel], thumbnail: Option<&klipp_render::still::Still>) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give the grade a name".into());
    }
    crate::presets::save_labelled_preset_in(dir, name, graph, node_labels)?;
    let png = dir.join(format!("{}.png", crate::presets::preset_stem(name)));
    match thumbnail {
        Some(t) => image::save_buffer(&png, &t.rgba, t.width, t.height, image::ExtendedColorType::Rgba8).map_err(|e| format!("Could not save its thumbnail: {e}"))?,
        None => {
            let _ = std::fs::remove_file(&png);
        }
    }
    Ok(())
}

impl LibraryGrade {
    /// Whether `query` (any case) is in its name or its labels; an empty
    /// query matches every grade.
    pub fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        q.is_empty() || [&self.name, &self.labels.camera, &self.labels.profile, &self.labels.notes].iter().any(|s| s.to_lowercase().contains(&q))
    }
}

/// Label the grade `name` in the library in `dir` (#614): its labels
/// replaced, or taken off when `labels` is empty.
pub fn set_labels_in(dir: &Path, name: &str, labels: &GradeLabels) -> Result<(), String> {
    let stem = crate::presets::preset_stem(name);
    if !dir.join(format!("{stem}.json")).exists() {
        return Err(format!("No grade \"{name}\" in the library"));
    }
    let path = dir.join(format!("{stem}.labels"));
    if labels.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    let json = serde_json::to_string_pretty(labels).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("Could not label \"{name}\": {e}"))
}

/// Take `name` out of the library in `dir`: its grade, its thumbnail and
/// its labels.
pub fn delete_in(dir: &Path, name: &str) -> Result<(), String> {
    let stem = crate::presets::preset_stem(name);
    std::fs::remove_file(dir.join(format!("{stem}.json"))).map_err(|e| format!("Could not delete \"{name}\": {e}"))?;
    let _ = std::fs::remove_file(dir.join(format!("{stem}.png")));
    let _ = std::fs::remove_file(dir.join(format!("{stem}.labels")));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klipp-grade-library-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A grade saved with its thumbnail comes back with both, by name, in
    /// name order; saved again under its name it is replaced; deleted, it
    /// and its thumbnail go. A blank name is refused.
    #[test]
    fn a_saved_grade_comes_back_with_its_thumbnail() {
        let dir = temp_dir("round-trip");
        let mut warm = NodeGraph::default();
        let id = warm.ensure_corrector();
        warm.nodes.iter_mut().find(|n| n.id == id).unwrap().params.temperature = Some(20.0);
        let thumb = klipp_render::still::Still { rgba: [200, 120, 40, 255].repeat(8 * 4), width: 8, height: 4 };
        save_in(&dir, "Warm dusk", &warm, Some(&thumb)).unwrap();
        save_in(&dir, "A plain", &NodeGraph::default(), None).unwrap();
        let lib = load_in(&dir);
        assert_eq!(lib.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), vec!["A plain", "Warm dusk"]);
        let dusk = &lib[1];
        assert_eq!(dusk.graph, warm);
        assert_eq!(dusk.thumbnail.as_ref(), Some(&thumb));
        assert!(lib[0].thumbnail.is_none());

        let mut warmer = warm.clone();
        warmer.nodes.iter_mut().find(|n| n.id == id).unwrap().params.temperature = Some(40.0);
        save_in(&dir, "Warm dusk", &warmer, Some(&thumb)).unwrap();
        assert_eq!(load_in(&dir).iter().find(|g| g.name == "Warm dusk").unwrap().graph, warmer, "saved again, replaced");

        delete_in(&dir, "Warm dusk").unwrap();
        assert_eq!(load_in(&dir).iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), vec!["A plain"]);
        assert!(!dir.join("Warm dusk.png").exists(), "its thumbnail goes with it");
        assert!(save_in(&dir, "  ", &warm, None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #614: a grade's labels come back with it, find it by any of its
    /// words, survive saving the grade again, and go with it; a label on
    /// a grade not in the library is refused, and empty labels are taken
    /// off.
    #[test]
    fn a_grades_labels_come_back_and_find_it() {
        let dir = temp_dir("labels");
        save_in(&dir, "Night blue", &NodeGraph::default(), None).unwrap();
        save_in(&dir, "Warm dusk", &NodeGraph::default(), None).unwrap();
        let labels = GradeLabels { camera: "FX3".into(), profile: "S-Log3".into(), notes: "for interviews".into() };
        set_labels_in(&dir, "Night blue", &labels).unwrap();
        assert!(set_labels_in(&dir, "Nowhere", &labels).is_err());
        let lib = load_in(&dir);
        assert_eq!(lib[0].labels, labels);
        assert!(lib[1].labels.is_empty());
        let found = |q: &str| load_in(&dir).into_iter().filter(|g| g.matches(q)).map(|g| g.name).collect::<Vec<_>>();
        assert_eq!(found("slog"), Vec::<String>::new());
        assert_eq!(found("s-log"), vec!["Night blue"]);
        assert_eq!(found("INTERVIEW"), vec!["Night blue"]);
        assert_eq!(found("dusk"), vec!["Warm dusk"]);
        assert_eq!(found(""), vec!["Night blue", "Warm dusk"]);
        assert_eq!(lib[0].labels.line(), "FX3  \u{b7}  S-Log3  \u{b7}  for interviews");

        save_in(&dir, "Night blue", &NodeGraph::default(), None).unwrap();
        assert_eq!(load_in(&dir)[0].labels, labels, "saving the grade again keeps its labels");
        set_labels_in(&dir, "Night blue", &GradeLabels::default()).unwrap();
        assert!(!dir.join(format!("{}.labels", crate::presets::preset_stem("Night blue"))).exists() && load_in(&dir)[0].labels.is_empty());
        set_labels_in(&dir, "Night blue", &labels).unwrap();
        delete_in(&dir, "Night blue").unwrap();
        assert!(std::fs::read_dir(&dir).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().contains("labels")), "its labels go with it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #1866: a grade's node colour labels are kept in its own file and
    /// come back with it; saved again without them, it has none; a grade
    /// saved before there were any, or by the old preset path, has none
    /// and loads as before; deleted, nothing of it is left.
    #[test]
    fn a_grades_node_labels_come_back_with_it() {
        let dir = temp_dir("node-labels");
        let mut grade = NodeGraph::default();
        let node = grade.ensure_corrector();
        let labels = vec![klipp_timeline::GradeNodeLabel { nodes: vec![node], colour: 3 }];
        save_labelled_in(&dir, "Warm dusk", &grade, &labels, None).unwrap();
        save_in(&dir, "Plain", &grade, None).unwrap();
        let lib = load_in(&dir);
        assert_eq!(lib.iter().map(|g| (g.name.as_str(), g.node_labels.clone())).collect::<Vec<_>>(), vec![("Plain", Vec::new()), ("Warm dusk", labels.clone())]);
        assert_eq!(lib[1].graph, grade, "the graph reads as before");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "one file each");
        save_in(&dir, "Warm dusk", &grade, None).unwrap();
        assert!(load_in(&dir)[1].node_labels.is_empty(), "saved again without, none");
        delete_in(&dir, "Warm dusk").unwrap();
        delete_in(&dir, "Plain").unwrap();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "nothing of it is left");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
