use serde::{Deserialize, Serialize};
use crate::theme::*;

// SnapConfig is shared across crates (used by klipp-timeline's
// magnetic-edit layer + klipp-ui's UI snap path) and lives in
// klipp-core. Re-exported here so existing call sites that imported
// it from `crate::layout_state::SnapConfig` keep working.
pub use klipp_core::SnapConfig;

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
    /// Two viewers side by side, or one.
    #[serde(default = "default_true")]
    pub dual_viewers: bool,
    /// The Color workspace's viewer layout: one program viewer by default.
    #[serde(default)]
    pub color_dual_viewers: bool,
    /// The Export workspace's viewer layout: one program viewer by default.
    #[serde(default)]
    pub export_dual_viewers: bool,
    /// The Color workspace's timeline is a strip under the viewer.
    #[serde(default = "default_color_timeline_h")]
    pub color_timeline_height: f32,
    /// Snap configuration: master + per-target booleans. Older
    /// layout.json files carry a bare `snap_enabled: bool` instead;
    /// `LayoutState::load()` migrates them on read into `SnapConfig
    /// { master: <legacy>, ...defaults }`.
    #[serde(default)]
    pub snap: SnapConfig,
    #[serde(default = "default_color_panel_height")]
    pub color_panel_height: f32,

    // ── Color workspace layout ──
    /// Fraction of center height allocated to viewer (vs node graph below). 0.0-1.0.
    #[serde(default = "default_color_viewer_ratio")]
    pub color_viewer_height_ratio: f32,
    /// Right-side inspector width in color workspace.
    #[serde(default = "default_color_inspector_width")]
    pub color_inspector_width: f32,

    // ── Panel visibility ──
    // Persisted, because a hidden panel that comes back on restart is
    // not hidden, it is flickering. These mirror
    // `AppState::show_browser_panel` / `show_inspector_panel`, which
    // stay the runtime source of truth (a workspace preset writes them
    // too); this is where that state survives a restart.
    #[serde(default = "default_true")]
    pub browser_visible: bool,
    #[serde(default = "default_true")]
    pub inspector_visible: bool,

    // ── Split browser (pinned files) ──
    /// When true, the FILES browser stays visible alongside MEDIA/LIBRARY.
    #[serde(default)]
    pub files_pinned: bool,
    /// Fraction of browser_width allocated to the left pane (MEDIA/LIBRARY) when files are pinned.
    #[serde(default = "default_files_split_ratio")]
    pub files_split_ratio: f32,
}

fn default_files_split_ratio() -> f32 { 0.5 }
fn default_true() -> bool { true }
fn default_color_timeline_h() -> f32 { 112.0 }
fn default_color_panel_height() -> f32 { 200.0 }
fn default_color_viewer_ratio() -> f32 { 0.66 }
fn default_color_inspector_width() -> f32 { 300.0 }

impl Default for LayoutState {
    fn default() -> Self {
        Self {
            browser_width: BROWSER_DEFAULT_W,
            inspector_width: INSPECTOR_DEFAULT_W,
            timeline_height: TIMELINE_H_UNSET,
            active_browser_tab: BrowserTab::Media,
            active_inspector_tab: InspectorTab::Video,
            active_tool: EditTool::Select,
            skimming_enabled: true,
            dual_viewers: true,
            color_dual_viewers: false,
            export_dual_viewers: false,
            color_timeline_height: default_color_timeline_h(),
            snap: SnapConfig::default(),
            color_panel_height: default_color_panel_height(),
            color_viewer_height_ratio: default_color_viewer_ratio(),
            color_inspector_width: default_color_inspector_width(),
            browser_visible: true,
            inspector_visible: true,
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
    ///
    /// Pre-SnapConfig layouts carry a bare `snap_enabled: bool`; we
    /// rewrite that into the `snap` struct on the JSON value before
    /// `serde_json::from_value` so the modern struct type-checks.
    /// Modern layouts (with `snap` already present) skip the migration.
    pub fn load() -> Self {
        let Some(path) = Self::config_path() else { return Self::default(); };
        let Ok(raw) = std::fs::read_to_string(path) else { return Self::default(); };
        let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&raw) else { return Self::default(); };
        if let Some(obj) = v.as_object_mut() {
            // The Color split used to default to 0.55, which left the
            // picture smaller than the scope dock. A file still carrying
            // exactly that default takes the new one; a dragged value stays.
            if obj.get("color_viewer_height_ratio").and_then(|r| r.as_f64()).is_some_and(|r| (r - 0.55).abs() < 1e-6) {
                obj.insert("color_viewer_height_ratio".to_string(), serde_json::json!(default_color_viewer_ratio()));
            }
            if !obj.contains_key("snap") {
                if let Some(legacy) = obj.remove("snap_enabled").and_then(|val| val.as_bool()) {
                    let default = SnapConfig::default();
                    obj.insert("snap".to_string(), serde_json::json!({
                        "master": legacy,
                        "clips": default.clips,
                        "playhead": default.playhead,
                        "markers": default.markers,
                        "in_out": default.in_out,
                        "grid": default.grid,
                    }));
                }
            }
        }
        serde_json::from_value(v).unwrap_or_default()
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
    Generators,
    /// Royalty-free music and sound-effect search (Jamendo, Freesound).
    Audio,
    /// User-authored composition templates with exposed parameters.
    Templates,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InspectorTab {
    Video,
    Audio,
    Color,
    Effects,
    Title,
    Generator,
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

#[cfg(test)]
mod tests {
    use super::*;

    // Pinned: layout.json that predates the SnapConfig migration
    // carries a bare `snap_enabled: bool`. Loading it must produce
    // a SnapConfig whose `master` mirrors the legacy value, with the
    // other booleans at their `Default::default()` values. This is
    // the load-bearing back-compat assertion.
    #[test]
    fn legacy_snap_enabled_true_loads_as_snap_config_master_true() {
        let legacy = serde_json::json!({
            "browser_width": 200.0,
            "inspector_width": 280.0,
            "timeline_height": 280.0,
            "active_browser_tab": "Media",
            "active_inspector_tab": "Video",
            "active_tool": "Select",
            "skimming_enabled": true,
            "snap_enabled": true,
            "color_panel_height": 200.0
        });
        // Mirror LayoutState::load's migration step in-process so the
        // test does not depend on a layout.json on disk.
        let mut v = legacy.clone();
        if let Some(obj) = v.as_object_mut() {
            if !obj.contains_key("snap") {
                if let Some(legacy_b) = obj.remove("snap_enabled").and_then(|val| val.as_bool()) {
                    let default = SnapConfig::default();
                    obj.insert("snap".to_string(), serde_json::json!({
                        "master": legacy_b,
                        "clips": default.clips,
                        "playhead": default.playhead,
                        "markers": default.markers,
                        "in_out": default.in_out,
                        "grid": default.grid,
                    }));
                }
            }
        }
        let loaded: LayoutState = serde_json::from_value(v).expect("legacy + migration deserialises");
        assert!(loaded.snap.master);
        assert!(loaded.snap.clips);
        assert!(loaded.snap.playhead);
        assert!(loaded.snap.markers);
        assert!(loaded.snap.in_out);
        assert!(!loaded.snap.grid);
    }

    #[test]
    fn legacy_snap_enabled_false_preserves_master_off() {
        let mut v = serde_json::json!({
            "browser_width": 200.0,
            "inspector_width": 280.0,
            "timeline_height": 280.0,
            "active_browser_tab": "Media",
            "active_inspector_tab": "Video",
            "active_tool": "Select",
            "skimming_enabled": true,
            "snap_enabled": false,
            "color_panel_height": 200.0
        });
        if let Some(obj) = v.as_object_mut() {
            if !obj.contains_key("snap") {
                if let Some(legacy_b) = obj.remove("snap_enabled").and_then(|val| val.as_bool()) {
                    let default = SnapConfig::default();
                    obj.insert("snap".to_string(), serde_json::json!({
                        "master": legacy_b,
                        "clips": default.clips,
                        "playhead": default.playhead,
                        "markers": default.markers,
                        "in_out": default.in_out,
                        "grid": default.grid,
                    }));
                }
            }
        }
        let loaded: LayoutState = serde_json::from_value(v).expect("deserialises");
        assert!(!loaded.snap.master, "legacy snap_enabled=false must produce master=false");
    }

    // Pinned: round-trip preserves SnapConfig exactly. If a future
    // refactor changes the on-disk shape, this fails.
    #[test]
    fn snap_config_round_trip_through_json() {
        let mut layout = LayoutState::default();
        layout.snap.master = false;
        layout.snap.grid = true;
        layout.snap.clips = false;
        let json = serde_json::to_string(&layout).expect("serialise");
        let back: LayoutState = serde_json::from_str(&json).expect("deserialise");
        assert!(!back.snap.master);
        assert!(back.snap.grid);
        assert!(!back.snap.clips);
        assert!(back.snap.playhead, "untouched fields preserve their values");
    }

    // Pinned: a layout.json with no snap field at all (truly fresh
    // first launch) loads with the SnapConfig default. Catches a
    // regression where serde(default) is dropped from the field.
    #[test]
    fn missing_snap_field_uses_default() {
        let json = serde_json::json!({
            "browser_width": 200.0,
            "inspector_width": 280.0,
            "timeline_height": 280.0,
            "active_browser_tab": "Media",
            "active_inspector_tab": "Video",
            "active_tool": "Select",
            "skimming_enabled": true,
            "color_panel_height": 200.0
        });
        let loaded: LayoutState = serde_json::from_value(json).expect("missing snap is OK");
        assert_eq!(loaded.snap, SnapConfig::default());
    }
}
