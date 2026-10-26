use serde::{Deserialize, Serialize};
use crate::theme::*;

// SnapConfig is shared across crates (used by klipp-timeline's
// magnetic-edit layer + klipp-ui's UI snap path) and lives in
// klipp-core. Re-exported here so existing call sites that imported
// it from `crate::layout_state::SnapConfig` keep working.
pub use klipp_core::SnapConfig;

/// Placement of the Colour workspace's node graph.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeGraphPlacement {
    #[default]
    Below,
    Right,
}

/// A temporary browser collapse for the side-by-side node layout.
/// Visibility preferences are never written by the borrow itself.
#[derive(Default)]
pub(crate) struct NodeBrowserBorrow {
    active: bool,
    previous: Option<bool>,
}

impl NodeBrowserBorrow {
    pub fn leave_if_inactive(&mut self, wanted: bool, visible: &mut bool) {
        if self.active && !wanted {
            self.active = false;
            if let Some(was) = self.previous.take() { *visible = was; }
        }
    }

    pub fn enter_if_active(&mut self, wanted: bool, visible: &mut bool) {
        if wanted && !self.active {
            self.active = true;
            self.previous = Some(*visible);
            *visible = false;
        } else if wanted && *visible {
            self.manual_override();
        }
    }

    pub fn manual_override(&mut self) { self.previous = None; }
}

/// Persistent UI layout state — panel sizes, active tabs, tool selection.
/// Saved to disk on every change and restored on launch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutState {
    pub browser_width: f32,
    pub inspector_width: f32,
    pub timeline_height: f32,
    pub active_browser_tab: BrowserTab,
    pub active_inspector_tab: InspectorTab,
    /// The Inspector's Keyframes column (#2305): shown, and how wide.
    #[serde(default)]
    pub inspector_keys: bool,
    #[serde(default = "default_inspector_keys_w")]
    pub inspector_keys_w: f32,
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
    /// The Audio Mixer's height, set by dragging its top edge.
    #[serde(default = "default_audio_mixer_height")]
    pub audio_mixer_height: f32,

    // ── Color workspace layout ──
    /// Fraction of center height allocated to viewer (vs node graph below). 0.0-1.0.
    #[serde(default = "default_color_viewer_ratio")]
    pub color_viewer_height_ratio: f32,
    /// Right-side inspector width in color workspace.
    #[serde(default = "default_color_inspector_width")]
    pub color_inspector_width: f32,

    /// Nodes stay below on compact windows; the preference remains Right.
    #[serde(default)]
    pub node_graph_placement: NodeGraphPlacement,
    #[serde(default = "default_node_width_ratio")]
    pub node_width_ratio: f32,

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

fn default_node_width_ratio() -> f32 { 0.42 }
fn default_files_split_ratio() -> f32 { 0.5 }
fn default_true() -> bool { true }
fn default_color_timeline_h() -> f32 { 112.0 }
fn default_color_panel_height() -> f32 { 200.0 }
fn default_audio_mixer_height() -> f32 { crate::theme::AUDIO_MIXER_DEFAULT_H }
fn default_color_viewer_ratio() -> f32 { 0.66 }
fn default_color_inspector_width() -> f32 { 300.0 }

fn default_inspector_keys_w() -> f32 {
    220.0
}

impl Default for LayoutState {
    fn default() -> Self {
        Self {
            browser_width: BROWSER_DEFAULT_W,
            inspector_width: INSPECTOR_DEFAULT_W,
            timeline_height: TIMELINE_H_UNSET,
            active_browser_tab: BrowserTab::Media,
            active_inspector_tab: InspectorTab::Video,
            inspector_keys: false,
            inspector_keys_w: default_inspector_keys_w(),
            active_tool: EditTool::Select,
            skimming_enabled: true,
            dual_viewers: true,
            color_dual_viewers: false,
            export_dual_viewers: false,
            color_timeline_height: default_color_timeline_h(),
            snap: SnapConfig::default(),
            color_panel_height: default_color_panel_height(),
            audio_mixer_height: default_audio_mixer_height(),
            color_viewer_height_ratio: default_color_viewer_ratio(),
            color_inspector_width: default_color_inspector_width(),
            node_graph_placement: NodeGraphPlacement::Below,
            node_width_ratio: default_node_width_ratio(),
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
    /// The Shared Library's collections of files, across projects (#614).
    Shared,
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
    Text,
    Mask,
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

#[cfg(test)]
mod node_layout_tests {
    use super::*;

    #[test]
    fn placement_and_split_survive_save_and_old_layouts_default_below() {
        let layout = LayoutState { node_graph_placement: NodeGraphPlacement::Right, node_width_ratio: 0.55, ..Default::default() };
        let mut value = serde_json::to_value(layout).unwrap();
        let restored: LayoutState = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(restored.node_graph_placement, NodeGraphPlacement::Right);
        assert_eq!(restored.node_width_ratio, 0.55);
        value.as_object_mut().unwrap().remove("node_graph_placement");
        value.as_object_mut().unwrap().remove("node_width_ratio");
        let old: LayoutState = serde_json::from_value(value).unwrap();
        assert_eq!(old.node_graph_placement, NodeGraphPlacement::Below);
        assert_eq!(old.node_width_ratio, 0.42);
    }

    #[test]
    fn browser_borrow_restores_visible_and_hidden_panels() {
        for original in [false, true] {
            let mut borrow = NodeBrowserBorrow::default();
            let mut visible = original;
            borrow.enter_if_active(true, &mut visible);
            assert!(!visible);
            for _ in 0..3 { borrow.enter_if_active(true, &mut visible); }
            borrow.leave_if_inactive(false, &mut visible);
            assert_eq!(visible, original);
        }
    }

    #[test]
    fn reopening_is_respected_even_when_user_closes_again() {
        let mut borrow = NodeBrowserBorrow::default();
        let mut visible = true;
        borrow.enter_if_active(true, &mut visible);
        visible = true; // rail, menu or Reveal in Browser
        borrow.enter_if_active(true, &mut visible);
        assert!(visible);
        visible = false;
        borrow.manual_override();
        borrow.enter_if_active(true, &mut visible);
        borrow.leave_if_inactive(false, &mut visible);
        assert!(!visible);
        // A deliberate new entry borrows again, not every idle frame.
        visible = true;
        borrow.enter_if_active(true, &mut visible);
        assert!(!visible);
        borrow.leave_if_inactive(false, &mut visible);
        assert!(visible);
    }

    #[test]
    fn nested_node_borrow_is_released_before_workspace_restore() {
        let mut visible = true;
        let workspace_previous = visible;
        visible = false; // Colour borrows first
        let mut nodes = NodeBrowserBorrow::default();
        nodes.enter_if_active(true, &mut visible);
        nodes.leave_if_inactive(false, &mut visible);
        assert!(!visible);
        visible = workspace_previous;
        assert!(visible);
    }
}
