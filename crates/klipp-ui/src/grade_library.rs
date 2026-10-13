//! The grade library (#899): whole grades saved across projects, each with
//! a graded thumbnail of the clip it was taken from, to browse and apply
//! to any clip. Kept with the whole-grade presets in the user's config
//! folder (`presets/color`): `<name>.json`, the grade, and `<name>.png`,
//! its thumbnail.

use std::path::{Path, PathBuf};

use klipp_nodes::NodeGraph;

/// One saved grade.
pub struct LibraryGrade {
    pub name: String,
    pub graph: NodeGraph,
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
    crate::presets::load_presets_in(dir)
        .into_iter()
        .map(|(name, graph)| {
            let thumbnail = image::open(dir.join(format!("{}.png", crate::presets::preset_stem(&name)))).ok().map(|img| {
                let rgba = img.to_rgba8();
                klipp_render::still::Still { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() }
            });
            LibraryGrade { name, graph, thumbnail, texture: None }
        })
        .collect()
}

/// Save `graph` to the library in `dir` as `name`, with `thumbnail`: a
/// grade of that name already there is replaced (as a preset of a newer
/// Klipp is never). An empty name, or a grade that wouldn't save whole,
/// is refused.
pub fn save_in(dir: &Path, name: &str, graph: &NodeGraph, thumbnail: Option<&klipp_render::still::Still>) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give the grade a name".into());
    }
    crate::presets::save_preset_in(dir, name, graph)?;
    let png = dir.join(format!("{}.png", crate::presets::preset_stem(name)));
    match thumbnail {
        Some(t) => image::save_buffer(&png, &t.rgba, t.width, t.height, image::ExtendedColorType::Rgba8).map_err(|e| format!("Could not save its thumbnail: {e}"))?,
        None => {
            let _ = std::fs::remove_file(&png);
        }
    }
    Ok(())
}

/// Take `name` out of the library in `dir`: its grade and its thumbnail.
pub fn delete_in(dir: &Path, name: &str) -> Result<(), String> {
    let stem = crate::presets::preset_stem(name);
    std::fs::remove_file(dir.join(format!("{stem}.json"))).map_err(|e| format!("Could not delete \"{name}\": {e}"))?;
    let _ = std::fs::remove_file(dir.join(format!("{stem}.png")));
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
}
