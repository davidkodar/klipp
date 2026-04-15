//! Keyboard shortcut remapping system.
//!
//! Stores a mapping from action names to key combinations. Persisted to
//! `~/.config/klipp/keybindings.json`. Missing entries fall back to defaults.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A single key binding: modifier flags + key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyCombo {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// egui key name (e.g. "A", "Space", "ArrowUp", "Delete", "F10").
    pub key: String,
}

impl KeyCombo {
    pub fn new(key: &str) -> Self {
        Self { ctrl: false, shift: false, alt: false, key: key.into() }
    }
    pub fn ctrl(key: &str) -> Self {
        Self { ctrl: true, shift: false, alt: false, key: key.into() }
    }
    pub fn shift(key: &str) -> Self {
        Self { ctrl: false, shift: true, alt: false, key: key.into() }
    }
    pub fn alt(key: &str) -> Self {
        Self { ctrl: false, shift: false, alt: true, key: key.into() }
    }
    pub fn ctrl_shift(key: &str) -> Self {
        Self { ctrl: true, shift: true, alt: false, key: key.into() }
    }
    pub fn ctrl_alt(key: &str) -> Self {
        Self { ctrl: true, shift: false, alt: true, key: key.into() }
    }

    /// Format for display in menus and tooltips.
    pub fn display(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl { parts.push("Ctrl"); }
        if self.shift { parts.push("Shift"); }
        if self.alt { parts.push("Alt"); }
        parts.push(&self.key);
        parts.join("+")
    }

    /// Check if this combo matches the current egui input state.
    pub fn is_pressed(&self, input: &egui::InputState) -> bool {
        if input.modifiers.ctrl != self.ctrl { return false; }
        if input.modifiers.shift != self.shift { return false; }
        if input.modifiers.alt != self.alt { return false; }

        // Try to parse as egui::Key
        if let Some(egui_key) = parse_egui_key(&self.key) {
            input.key_pressed(egui_key)
        } else {
            // Text-based keys (punctuation: ,  .  ;  '  etc.)
            input.events.iter().any(|e| matches!(e, egui::Event::Text(s) if *s == self.key))
        }
    }
}

/// Map action names → key combos.  Stored sorted for stable serialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyBindings {
    pub bindings: BTreeMap<String, Vec<KeyCombo>>,
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self::defaults()
    }
}

impl KeyBindings {
    /// Check if an action's key combo is currently pressed.
    pub fn is_action_pressed(&self, action: &str, input: &egui::InputState) -> bool {
        self.bindings.get(action)
            .map(|combos| combos.iter().any(|c| c.is_pressed(input)))
            .unwrap_or(false)
    }

    /// Get display string for an action's primary binding.
    pub fn display_for(&self, action: &str) -> String {
        self.bindings.get(action)
            .and_then(|c| c.first())
            .map(|c| c.display())
            .unwrap_or_default()
    }

    /// All default bindings.
    pub fn defaults() -> Self {
        let mut b = BTreeMap::new();

        let bind = |b: &mut BTreeMap<String, Vec<KeyCombo>>, name: &str, combos: Vec<KeyCombo>| {
            b.insert(name.into(), combos);
        };

        // File
        bind(&mut b, "save_project", vec![KeyCombo::ctrl("S")]);
        bind(&mut b, "save_project_as", vec![KeyCombo::ctrl_shift("S")]);
        bind(&mut b, "open_project", vec![KeyCombo::ctrl("O")]);
        bind(&mut b, "import_media", vec![KeyCombo::ctrl("I")]);
        bind(&mut b, "export", vec![KeyCombo::ctrl("E")]);
        bind(&mut b, "preferences", vec![KeyCombo::ctrl(",")]);

        // Edit
        bind(&mut b, "undo", vec![KeyCombo::ctrl("Z")]);
        bind(&mut b, "redo", vec![KeyCombo::ctrl("Y"), KeyCombo::ctrl_shift("Z")]);
        bind(&mut b, "cut_clip", vec![KeyCombo::ctrl("X")]);
        bind(&mut b, "copy_clip", vec![KeyCombo::ctrl("C")]);
        bind(&mut b, "paste_clip", vec![KeyCombo::ctrl("V")]);
        bind(&mut b, "copy_attributes", vec![KeyCombo::ctrl_shift("C")]);
        bind(&mut b, "paste_attributes", vec![KeyCombo::ctrl_shift("V")]);
        bind(&mut b, "duplicate_clip", vec![KeyCombo::ctrl("D")]);
        bind(&mut b, "select_all", vec![KeyCombo::ctrl("A")]);
        bind(&mut b, "deselect_all", vec![KeyCombo::ctrl_shift("A")]);
        bind(&mut b, "delete_clip", vec![KeyCombo::new("Delete"), KeyCombo::new("Backspace")]);
        bind(&mut b, "lift_clip", vec![KeyCombo::shift("Delete"), KeyCombo::shift("Backspace")]);
        bind(&mut b, "split_at_playhead", vec![KeyCombo::ctrl("B")]);
        bind(&mut b, "freeze_frame", vec![KeyCombo::shift("F")]);
        bind(&mut b, "group_clips", vec![KeyCombo::ctrl("G")]);
        bind(&mut b, "ungroup_clips", vec![KeyCombo::ctrl_shift("G")]);

        // Tools
        bind(&mut b, "select_tool", vec![KeyCombo::new("V")]);
        bind(&mut b, "blade_tool", vec![KeyCombo::new("B")]);
        bind(&mut b, "range_tool", vec![KeyCombo::new("R")]);
        bind(&mut b, "trim_tool", vec![KeyCombo::new("T")]);

        // Timeline editing
        bind(&mut b, "insert_edit", vec![KeyCombo::new(","), KeyCombo::new("F10")]);
        bind(&mut b, "overwrite_edit", vec![KeyCombo::new("."), KeyCombo::new("F11")]);
        bind(&mut b, "lift_edit", vec![KeyCombo::new(";")]);
        bind(&mut b, "extract_edit", vec![KeyCombo::new("'")]);
        bind(&mut b, "toggle_edit_mode", vec![KeyCombo::shift("W")]);
        bind(&mut b, "ripple_trim_head", vec![KeyCombo::alt("[")]);
        bind(&mut b, "ripple_trim_tail", vec![KeyCombo::alt("]")]);
        bind(&mut b, "trim_out_to_playhead", vec![KeyCombo::new("Q")]);
        bind(&mut b, "trim_in_to_playhead", vec![KeyCombo::new("W")]);
        bind(&mut b, "extract_frame_to_file", vec![KeyCombo::ctrl_alt("S")]);
        bind(&mut b, "extract_frame_to_bin", vec![KeyCombo::ctrl_alt("B")]);
        bind(&mut b, "toggle_scrub_audio", vec![KeyCombo::shift("S")]);

        // Navigation
        bind(&mut b, "next_edit_point", vec![KeyCombo::new("ArrowDown")]);
        bind(&mut b, "prev_edit_point", vec![KeyCombo::new("ArrowUp")]);
        bind(&mut b, "set_in_point", vec![KeyCombo::new("I")]);
        bind(&mut b, "set_out_point", vec![KeyCombo::new("O")]);
        bind(&mut b, "clear_in_point", vec![KeyCombo::shift("I")]);
        bind(&mut b, "clear_out_point", vec![KeyCombo::shift("O")]);
        bind(&mut b, "clear_in_out", vec![KeyCombo::alt("X")]);
        bind(&mut b, "set_range_to_clip", vec![KeyCombo::new("X")]);

        // Transport
        bind(&mut b, "play_pause", vec![KeyCombo::new("Space")]);
        bind(&mut b, "step_forward", vec![KeyCombo::new("ArrowRight")]);
        bind(&mut b, "step_backward", vec![KeyCombo::new("ArrowLeft")]);
        bind(&mut b, "go_to_start", vec![KeyCombo::new("Home")]);
        bind(&mut b, "go_to_end", vec![KeyCombo::new("End")]);
        bind(&mut b, "shuttle_reverse", vec![KeyCombo::new("J")]);
        bind(&mut b, "shuttle_pause", vec![KeyCombo::new("K")]);
        bind(&mut b, "shuttle_forward", vec![KeyCombo::new("L")]);
        bind(&mut b, "toggle_loop", vec![KeyCombo::ctrl("L")]);

        // Viewer
        bind(&mut b, "toggle_fullscreen", vec![KeyCombo::new("F")]);
        bind(&mut b, "toggle_before_after", vec![KeyCombo::new("B")]);
        bind(&mut b, "toggle_color_panel", vec![KeyCombo::new("C")]);
        bind(&mut b, "toggle_scopes", vec![KeyCombo::new("V")]);
        bind(&mut b, "toggle_zebra", vec![KeyCombo::new("Z")]);
        bind(&mut b, "toggle_false_color", vec![KeyCombo::alt("F")]);
        bind(&mut b, "toggle_perf_overlay", vec![KeyCombo::new("P")]);
        bind(&mut b, "toggle_matte_preview", vec![KeyCombo::alt("M")]);

        // Window panels
        bind(&mut b, "toggle_project_notes", vec![KeyCombo::ctrl_shift("N")]);

        // Timeline display
        bind(&mut b, "toggle_snap", vec![KeyCombo::new("N")]);
        bind(&mut b, "toggle_skimming", vec![KeyCombo::new("S")]);
        bind(&mut b, "fit_to_window", vec![KeyCombo::new("Z")]);
        bind(&mut b, "zoom_to_selection", vec![KeyCombo::shift("Z")]);
        bind(&mut b, "zoom_in", vec![KeyCombo::new("=")]);
        bind(&mut b, "zoom_out", vec![KeyCombo::new("-")]);

        // Markers & Keyframes
        bind(&mut b, "add_marker", vec![KeyCombo::new("M")]);
        bind(&mut b, "add_clip_marker", vec![KeyCombo::shift("M")]);
        bind(&mut b, "toggle_keyframe", vec![KeyCombo::new("K")]);
        bind(&mut b, "toggle_keyframe_editor", vec![KeyCombo::shift("K")]);
        bind(&mut b, "prev_keyframe", vec![KeyCombo::alt("ArrowLeft")]);
        bind(&mut b, "next_keyframe", vec![KeyCombo::alt("ArrowRight")]);

        // Multicam
        bind(&mut b, "multicam_angle_1", vec![KeyCombo::new("1")]);
        bind(&mut b, "multicam_angle_2", vec![KeyCombo::new("2")]);
        bind(&mut b, "multicam_angle_3", vec![KeyCombo::new("3")]);
        bind(&mut b, "multicam_angle_4", vec![KeyCombo::new("4")]);
        bind(&mut b, "multicam_angle_5", vec![KeyCombo::new("5")]);
        bind(&mut b, "multicam_angle_6", vec![KeyCombo::new("6")]);
        bind(&mut b, "multicam_angle_7", vec![KeyCombo::new("7")]);
        bind(&mut b, "multicam_angle_8", vec![KeyCombo::new("8")]);
        bind(&mut b, "multicam_angle_9", vec![KeyCombo::new("9")]);
        bind(&mut b, "create_multicam", vec![KeyCombo::alt("C")]);

        // Color grading
        bind(&mut b, "prev_color_node", vec![KeyCombo::new("[")]);
        bind(&mut b, "next_color_node", vec![KeyCombo::new("]")]);
        bind(&mut b, "toggle_node_bypass", vec![KeyCombo::alt("D")]);
        bind(&mut b, "reset_current_node", vec![KeyCombo::ctrl_shift("R")]);
        bind(&mut b, "cycle_scope_mode", vec![KeyCombo::shift("V")]);

        // Misc
        bind(&mut b, "create_title", vec![KeyCombo::ctrl("T")]);
        bind(&mut b, "connect_clip", vec![KeyCombo::new("G")]);
        bind(&mut b, "disconnect_clip", vec![KeyCombo::shift("G")]);

        Self { bindings: b }
    }

    /// Config file path: ~/.config/klipp/keybindings.json
    fn config_path() -> Option<std::path::PathBuf> {
        dirs::config_dir().map(|d| d.join("klipp").join("keybindings.json"))
    }

    /// Load from disk. Missing entries fall back to defaults.
    pub fn load() -> Self {
        let defaults = Self::defaults();
        let Some(path) = Self::config_path() else { return defaults; };

        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<KeyBindings>(&contents) {
                Ok(mut loaded) => {
                    // Merge: add any default keys not present in the loaded file
                    for (action, combos) in &defaults.bindings {
                        loaded.bindings.entry(action.clone()).or_insert_with(|| combos.clone());
                    }
                    tracing::info!("Key bindings loaded from {}", path.display());
                    loaded
                }
                Err(e) => {
                    tracing::warn!("Failed to parse keybindings ({}), using defaults", e);
                    defaults
                }
            },
            Err(_) => defaults,
        }
    }

    /// Save to disk.
    pub fn save(&self) -> Result<(), String> {
        let Some(path) = Self::config_path() else {
            return Err("Could not determine config directory".into());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config directory: {e}"))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize keybindings: {e}"))?;
        std::fs::write(&path, json)
            .map_err(|e| format!("Failed to write keybindings: {e}"))?;
        tracing::info!("Key bindings saved to {}", path.display());
        Ok(())
    }

    /// Find conflicts: actions that share the same key combo.
    pub fn find_conflicts(&self) -> Vec<(String, String, KeyCombo)> {
        let mut conflicts = Vec::new();
        let actions: Vec<_> = self.bindings.iter().collect();
        for i in 0..actions.len() {
            for j in (i + 1)..actions.len() {
                for combo_a in actions[i].1 {
                    for combo_b in actions[j].1 {
                        if combo_a == combo_b {
                            conflicts.push((
                                actions[i].0.clone(),
                                actions[j].0.clone(),
                                combo_a.clone(),
                            ));
                        }
                    }
                }
            }
        }
        conflicts
    }

    /// Get a human-readable label for an action name.
    pub fn action_label(action: &str) -> &str {
        match action {
            "save_project" => "Save Project",
            "save_project_as" => "Save Project As",
            "open_project" => "Open Project",
            "import_media" => "Import Media",
            "export" => "Export",
            "preferences" => "Preferences",
            "undo" => "Undo",
            "redo" => "Redo",
            "cut_clip" => "Cut Clip",
            "copy_clip" => "Copy Clip",
            "paste_clip" => "Paste Clip",
            "copy_attributes" => "Copy Attributes",
            "paste_attributes" => "Paste Attributes",
            "duplicate_clip" => "Duplicate Clip",
            "select_all" => "Select All",
            "deselect_all" => "Deselect All",
            "delete_clip" => "Delete Clip",
            "lift_clip" => "Lift Clip (Leave Gap)",
            "split_at_playhead" => "Split at Playhead",
            "freeze_frame" => "Freeze Frame",
            "group_clips" => "Group Clips",
            "ungroup_clips" => "Ungroup Clips",
            "select_tool" => "Select Tool",
            "blade_tool" => "Blade Tool",
            "range_tool" => "Range Tool",
            "trim_tool" => "Trim Tool",
            "insert_edit" => "Insert Edit",
            "overwrite_edit" => "Overwrite Edit",
            "lift_edit" => "Lift Edit",
            "extract_edit" => "Extract Edit",
            "toggle_edit_mode" => "Toggle Insert/Overwrite",
            "ripple_trim_head" => "Ripple Trim Head",
            "ripple_trim_tail" => "Ripple Trim Tail",
            "trim_out_to_playhead" => "Trim Out to Playhead",
            "trim_in_to_playhead" => "Trim In to Playhead",
            "extract_frame_to_file" => "Extract Frame to File",
            "extract_frame_to_bin" => "Extract Frame to Bin",
            "toggle_scrub_audio" => "Toggle Audio Scrubbing",
            "next_edit_point" => "Next Edit Point",
            "prev_edit_point" => "Previous Edit Point",
            "set_in_point" => "Set In Point",
            "set_out_point" => "Set Out Point",
            "clear_in_point" => "Clear In Point",
            "clear_out_point" => "Clear Out Point",
            "clear_in_out" => "Clear In/Out Points",
            "set_range_to_clip" => "Set Range to Clip",
            "play_pause" => "Play / Pause",
            "step_forward" => "Step Forward",
            "step_backward" => "Step Backward",
            "go_to_start" => "Go to Start",
            "go_to_end" => "Go to End",
            "shuttle_reverse" => "Shuttle Reverse (J)",
            "shuttle_pause" => "Shuttle Pause (K)",
            "shuttle_forward" => "Shuttle Forward (L)",
            "toggle_loop" => "Toggle Loop Playback",
            "toggle_fullscreen" => "Toggle Fullscreen",
            "toggle_before_after" => "Toggle Before/After",
            "toggle_color_panel" => "Toggle Color Panel",
            "toggle_scopes" => "Toggle Scopes",
            "toggle_zebra" => "Toggle Zebra Overlay",
            "toggle_false_color" => "Toggle False Color",
            "toggle_perf_overlay" => "Toggle Perf Overlay",
            "toggle_matte_preview" => "Toggle Matte Preview",
            "toggle_project_notes" => "Toggle Project Notes",
            "toggle_snap" => "Toggle Snap",
            "toggle_skimming" => "Toggle Skimming",
            "fit_to_window" => "Fit to Window",
            "zoom_to_selection" => "Zoom to Selection",
            "zoom_in" => "Zoom In",
            "zoom_out" => "Zoom Out",
            "add_marker" => "Add Marker",
            "add_clip_marker" => "Add Clip Marker",
            "toggle_keyframe" => "Toggle Keyframe",
            "toggle_keyframe_editor" => "Toggle Keyframe Editor",
            "prev_keyframe" => "Previous Keyframe",
            "next_keyframe" => "Next Keyframe",
            "multicam_angle_1" => "Multicam Angle 1",
            "multicam_angle_2" => "Multicam Angle 2",
            "multicam_angle_3" => "Multicam Angle 3",
            "multicam_angle_4" => "Multicam Angle 4",
            "multicam_angle_5" => "Multicam Angle 5",
            "multicam_angle_6" => "Multicam Angle 6",
            "multicam_angle_7" => "Multicam Angle 7",
            "multicam_angle_8" => "Multicam Angle 8",
            "multicam_angle_9" => "Multicam Angle 9",
            "create_multicam" => "Create Multicam Clip",
            "prev_color_node" => "Previous Color Node",
            "next_color_node" => "Next Color Node",
            "toggle_node_bypass" => "Toggle Node Bypass",
            "reset_current_node" => "Reset Current Node",
            "cycle_scope_mode" => "Cycle Scope Mode",
            "create_title" => "Create Title Clip",
            "connect_clip" => "Connect to Primary",
            "disconnect_clip" => "Disconnect from Primary",
            _ => action,
        }
    }

    /// Group actions by category for the preferences UI.
    pub fn categories() -> &'static [(&'static str, &'static [&'static str])] {
        &[
            ("File", &["save_project", "save_project_as", "open_project", "import_media", "export", "preferences"]),
            ("Edit", &["undo", "redo", "cut_clip", "copy_clip", "paste_clip", "copy_attributes", "paste_attributes", "duplicate_clip", "select_all", "deselect_all", "delete_clip", "lift_clip", "split_at_playhead", "group_clips", "ungroup_clips"]),
            ("Tools", &["select_tool", "blade_tool", "range_tool", "trim_tool"]),
            ("Timeline", &["insert_edit", "overwrite_edit", "lift_edit", "extract_edit", "toggle_edit_mode", "ripple_trim_head", "ripple_trim_tail", "trim_out_to_playhead", "trim_in_to_playhead", "freeze_frame"]),
            ("Navigation", &["next_edit_point", "prev_edit_point", "set_in_point", "set_out_point", "clear_in_point", "clear_out_point", "clear_in_out", "set_range_to_clip"]),
            ("Transport", &["play_pause", "step_forward", "step_backward", "go_to_start", "go_to_end", "shuttle_reverse", "shuttle_pause", "shuttle_forward", "toggle_loop", "toggle_scrub_audio"]),
            ("Viewer", &["toggle_fullscreen", "toggle_before_after", "toggle_color_panel", "toggle_scopes", "toggle_zebra", "toggle_false_color", "toggle_perf_overlay", "toggle_matte_preview", "extract_frame_to_file", "extract_frame_to_bin"]),
            ("Window", &["toggle_project_notes"]),
            ("Display", &["toggle_snap", "toggle_skimming", "fit_to_window", "zoom_to_selection", "zoom_in", "zoom_out"]),
            ("Markers", &["add_marker", "add_clip_marker", "toggle_keyframe", "toggle_keyframe_editor", "prev_keyframe", "next_keyframe"]),
            ("Multicam", &["multicam_angle_1", "multicam_angle_2", "multicam_angle_3",
                "multicam_angle_4", "multicam_angle_5", "multicam_angle_6",
                "multicam_angle_7", "multicam_angle_8", "multicam_angle_9",
                "create_multicam"]),
            ("Color", &["prev_color_node", "next_color_node", "toggle_node_bypass", "reset_current_node", "cycle_scope_mode"]),
            ("Other", &["create_title", "connect_clip", "disconnect_clip"]),
        ]
    }
}

/// Parse a key name string to egui::Key. Returns None for text-event keys (punctuation).
fn parse_egui_key(name: &str) -> Option<egui::Key> {
    use egui::Key;
    match name {
        "A" => Some(Key::A), "B" => Some(Key::B), "C" => Some(Key::C), "D" => Some(Key::D),
        "E" => Some(Key::E), "F" => Some(Key::F), "G" => Some(Key::G), "H" => Some(Key::H),
        "I" => Some(Key::I), "J" => Some(Key::J), "K" => Some(Key::K), "L" => Some(Key::L),
        "M" => Some(Key::M), "N" => Some(Key::N), "O" => Some(Key::O), "P" => Some(Key::P),
        "Q" => Some(Key::Q), "R" => Some(Key::R), "S" => Some(Key::S), "T" => Some(Key::T),
        "U" => Some(Key::U), "V" => Some(Key::V), "W" => Some(Key::W), "X" => Some(Key::X),
        "Y" => Some(Key::Y), "Z" => Some(Key::Z),
        "0" | "Num0" => Some(Key::Num0), "1" | "Num1" => Some(Key::Num1),
        "2" | "Num2" => Some(Key::Num2), "3" | "Num3" => Some(Key::Num3),
        "4" | "Num4" => Some(Key::Num4), "5" | "Num5" => Some(Key::Num5),
        "6" | "Num6" => Some(Key::Num6), "7" | "Num7" => Some(Key::Num7),
        "8" | "Num8" => Some(Key::Num8), "9" | "Num9" => Some(Key::Num9),
        "Space" => Some(Key::Space),
        "Enter" => Some(Key::Enter),
        "Escape" => Some(Key::Escape),
        "Tab" => Some(Key::Tab),
        "Delete" => Some(Key::Delete),
        "Backspace" => Some(Key::Backspace),
        "Home" => Some(Key::Home),
        "End" => Some(Key::End),
        "PageUp" => Some(Key::PageUp),
        "PageDown" => Some(Key::PageDown),
        "ArrowLeft" => Some(Key::ArrowLeft),
        "ArrowRight" => Some(Key::ArrowRight),
        "ArrowUp" => Some(Key::ArrowUp),
        "ArrowDown" => Some(Key::ArrowDown),
        "F1" => Some(Key::F1), "F2" => Some(Key::F2), "F3" => Some(Key::F3),
        "F4" => Some(Key::F4), "F5" => Some(Key::F5), "F6" => Some(Key::F6),
        "F7" => Some(Key::F7), "F8" => Some(Key::F8), "F9" => Some(Key::F9),
        "F10" => Some(Key::F10), "F11" => Some(Key::F11), "F12" => Some(Key::F12),
        "=" => Some(Key::Equals),
        "-" => Some(Key::Minus),
        "[" => Some(Key::OpenBracket),
        "]" => Some(Key::CloseBracket),
        _ => None, // Punctuation like , . ; ' handled via text events
    }
}

/// Convert an egui key press event to a KeyCombo (for the "press a key" remapping UI).
pub fn key_event_to_combo(input: &egui::InputState) -> Option<KeyCombo> {
    // Check all known keys
    let keys = [
        ("A", egui::Key::A), ("B", egui::Key::B), ("C", egui::Key::C), ("D", egui::Key::D),
        ("E", egui::Key::E), ("F", egui::Key::F), ("G", egui::Key::G), ("H", egui::Key::H),
        ("I", egui::Key::I), ("J", egui::Key::J), ("K", egui::Key::K), ("L", egui::Key::L),
        ("M", egui::Key::M), ("N", egui::Key::N), ("O", egui::Key::O), ("P", egui::Key::P),
        ("Q", egui::Key::Q), ("R", egui::Key::R), ("S", egui::Key::S), ("T", egui::Key::T),
        ("U", egui::Key::U), ("V", egui::Key::V), ("W", egui::Key::W), ("X", egui::Key::X),
        ("Y", egui::Key::Y), ("Z", egui::Key::Z),
        ("Space", egui::Key::Space), ("Enter", egui::Key::Enter), ("Escape", egui::Key::Escape),
        ("Delete", egui::Key::Delete), ("Backspace", egui::Key::Backspace),
        ("Home", egui::Key::Home), ("End", egui::Key::End),
        ("ArrowLeft", egui::Key::ArrowLeft), ("ArrowRight", egui::Key::ArrowRight),
        ("ArrowUp", egui::Key::ArrowUp), ("ArrowDown", egui::Key::ArrowDown),
        ("F1", egui::Key::F1), ("F2", egui::Key::F2), ("F3", egui::Key::F3),
        ("F4", egui::Key::F4), ("F5", egui::Key::F5), ("F6", egui::Key::F6),
        ("F7", egui::Key::F7), ("F8", egui::Key::F8), ("F9", egui::Key::F9),
        ("F10", egui::Key::F10), ("F11", egui::Key::F11), ("F12", egui::Key::F12),
        ("=", egui::Key::Equals), ("-", egui::Key::Minus),
        ("[", egui::Key::OpenBracket), ("]", egui::Key::CloseBracket),
    ];

    for (name, key) in keys {
        if input.key_pressed(key) {
            return Some(KeyCombo {
                ctrl: input.modifiers.ctrl,
                shift: input.modifiers.shift,
                alt: input.modifiers.alt,
                key: name.into(),
            });
        }
    }

    // Check text events for punctuation
    for event in &input.events {
        if let egui::Event::Text(s) = event {
            let ch = s.as_str();
            if matches!(ch, "," | "." | ";" | "'" | "/" | "\\" | "`") {
                return Some(KeyCombo {
                    ctrl: input.modifiers.ctrl,
                    shift: input.modifiers.shift,
                    alt: input.modifiers.alt,
                    key: ch.into(),
                });
            }
        }
    }

    None
}
