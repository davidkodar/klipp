use serde::{Deserialize, Serialize};
use crate::theme::*;

/// Persistent UI layout state — panel sizes, active tabs, tool selection.
/// Saved to disk on every change and restored on launch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutState {
    pub browser_width: f32,
    pub inspector_width: f32,
    pub timeline_height: f32,
    pub active_browser_tab: BrowserTab,
    pub active_inspector_tab: InspectorTab,
    pub active_tool: EditTool,
    pub skimming_enabled: bool,
    pub snap_enabled: bool,
}

impl Default for LayoutState {
    fn default() -> Self {
        Self {
            browser_width: BROWSER_DEFAULT_W,
            inspector_width: INSPECTOR_DEFAULT_W,
            timeline_height: TIMELINE_DEFAULT_H,
            active_browser_tab: BrowserTab::Media,
            active_inspector_tab: InspectorTab::Video,
            active_tool: EditTool::Select,
            skimming_enabled: true,
            snap_enabled: true,
        }
    }
}

impl LayoutState {
    /// Path to the layout state file in the user config directory.
    fn config_path() -> Option<std::path::PathBuf> {
        dirs::config_dir().map(|d| d.join("klipp").join("layout.json"))
    }

    /// Load from disk, falling back to defaults.
    pub fn load() -> Self {
        Self::config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Persist to disk. Errors are logged, not propagated.
    pub fn save(&self) {
        if let Some(path) = Self::config_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match serde_json::to_string_pretty(self) {
                Ok(json) => {
                    if let Err(e) = std::fs::write(&path, json) {
                        tracing::warn!("Failed to save layout state: {e}");
                    }
                }
                Err(e) => tracing::warn!("Failed to serialize layout state: {e}"),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrowserTab {
    Media,
    Effects,
    AudioFx,
    Titles,
    Transitions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InspectorTab {
    Video,
    Audio,
    Effects,
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditTool {
    Select,
    Trim,
    Blade,
    Range,
    Zoom,
}
