use klipp_nodes::NodeGraph;
use std::path::{Path, PathBuf};

/// Get the user presets directory, creating it if needed.
pub fn user_presets_dir() -> Option<PathBuf> {
    let dir = dirs::config_dir()?.join("klipp").join("presets").join("color");
    if !dir.exists() {
        std::fs::create_dir_all(&dir).ok()?;
    }
    Some(dir)
}

/// Load all presets: built-in + user directory.
/// NOTE: Session 1 stub — returns empty list until node-based presets are implemented.
pub fn load_all_presets() -> Vec<(String, NodeGraph)> {
    // TODO(Session 4+): Implement node-graph based presets.
    // Old ClipColorGrade presets are no longer compatible.
    let mut presets = Vec::new();
    presets.extend(load_user_presets());
    presets
}

/// Save a grade preset to the user directory.
pub fn save_preset(name: &str, graph: &NodeGraph) -> Result<(), String> {
    let dir = user_presets_dir().ok_or("Cannot find config directory")?;
    let safe_name: String = name.chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let path = dir.join(format!("{safe_name}.json"));
    let json = serde_json::to_string_pretty(graph)
        .map_err(|e| format!("Serialize error: {e}"))?;
    std::fs::write(&path, json)
        .map_err(|e| format!("Write error: {e}"))?;
    Ok(())
}

/// Load presets from user directory.
fn load_user_presets() -> Vec<(String, NodeGraph)> {
    let Some(dir) = user_presets_dir() else { return Vec::new() };
    let mut presets = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else { return presets };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            if let Some(preset) = load_preset_file(&path) {
                presets.push(preset);
            }
        }
    }
    presets.sort_by(|a, b| a.0.cmp(&b.0));
    presets
}

fn load_preset_file(path: &Path) -> Option<(String, NodeGraph)> {
    let text = std::fs::read_to_string(path).ok()?;
    let graph: NodeGraph = serde_json::from_str(&text).ok()?;
    let name = path.file_stem()?.to_string_lossy().to_string();
    Some((name, graph))
}
