use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceJson {
    pub version: String,
    pub name: String,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
}

/// Check if a directory is a workspace (contains workspace.json).
pub fn is_workspace(path: &Path) -> bool {
    path.is_dir() && path.join("workspace.json").exists()
}

/// Check if a directory is a Klipp project (contains project.json).
pub fn is_project(path: &Path) -> bool {
    path.is_dir() && path.join("project.json").exists()
}

/// Load workspace metadata from a directory.
pub fn load_workspace(path: &Path) -> Option<WorkspaceJson> {
    let json_path = path.join("workspace.json");
    let contents = std::fs::read_to_string(json_path).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Create a new workspace in the given directory.
pub fn create_workspace(path: &Path, name: &str) -> Result<(), String> {
    std::fs::create_dir_all(path)
        .map_err(|e| format!("Failed to create workspace directory: {e}"))?;

    let ws = WorkspaceJson {
        version: "1.0".to_string(),
        name: name.to_string(),
        created_at: Some(crate::save::iso8601_now()),
        description: None,
        color: None,
    };

    let json = serde_json::to_string_pretty(&ws)
        .map_err(|e| format!("Failed to serialize workspace: {e}"))?;
    std::fs::write(path.join("workspace.json"), json)
        .map_err(|e| format!("Failed to write workspace.json: {e}"))?;

    Ok(())
}

/// Find all Klipp projects inside a workspace directory.
pub fn projects_in_workspace(workspace_path: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(workspace_path) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_project(p))
        .collect()
}
