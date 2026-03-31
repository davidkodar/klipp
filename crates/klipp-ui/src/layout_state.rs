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
    #[serde(default = "default_color_panel_height")]
    pub color_panel_height: f32,

    // ── Color workspace layout ──
    /// Fraction of center height allocated to viewer (vs node graph below). 0.0-1.0.
    #[serde(default = "default_color_viewer_ratio")]
    pub color_viewer_height_ratio: f32,
    /// Right-side inspector width in color workspace.
    #[serde(default = "default_color_inspector_width")]
    pub color_inspector_width: f32,

    // ── Split browser (pinned files) ──
    /// When true, the FILES browser stays visible alongside MEDIA/LIBRARY.
    #[serde(default)]
    pub files_pinned: bool,
    /// Fraction of browser_width allocated to the left pane (MEDIA/LIBRARY) when files are pinned.
    #[serde(default = "default_files_split_ratio")]
    pub files_split_ratio: f32,
}

fn default_files_split_ratio() -> f32 { 0.5 }
fn default_color_panel_height() -> f32 { 200.0 }
fn default_color_viewer_ratio() -> f32 { 0.55 }
fn default_color_inspector_width() -> f32 { 300.0 }

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
            color_panel_height: default_color_panel_height(),
            color_viewer_height_ratio: default_color_viewer_ratio(),
            color_inspector_width: default_color_inspector_width(),
            files_pinned: false,
            files_split_ratio: default_files_split_ratio(),
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
    Files,
    Media,
    Library,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LibraryCategory {
    #[default]
    VideoFx,
    AudioFx,
    Titles,
    Transitions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InspectorTab {
    Video,
    Audio,
    Color,
    Effects,
    Title,
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
