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
        // These four were advertised by the File/Window/Help menus for a
        // long time without ever being bound — `shortcut_text` only draws
        // a label. They are the platform-standard combos for what they do,
        // so they are bound rather than un-advertised.
        bind(&mut b, "new_project", vec![KeyCombo::ctrl("N")]);
        bind(&mut b, "quit", vec![KeyCombo::ctrl("Q")]);
        bind(&mut b, "toggle_audio_mixer", vec![KeyCombo::ctrl_shift("M")]);
        bind(&mut b, "toggle_subtitle_editor", vec![KeyCombo::ctrl_shift("T")]);
        // Handled ad hoc in app.rs until now, with the combo written out
        // twice — once in the handler and once in the menu label. Here
        // so there is one copy, and so they are remappable like the rest.
        bind(&mut b, "close_project", vec![KeyCombo::ctrl("W")]);
        bind(&mut b, "command_palette", vec![KeyCombo::ctrl("K")]);
        bind(&mut b, "toggle_browser_panel", vec![KeyCombo::ctrl_shift("B")]);
        bind(&mut b, "toggle_inspector_panel", vec![KeyCombo::ctrl_shift("I")]);

        // Edit
        bind(&mut b, "undo", vec![KeyCombo::ctrl("Z")]);
        bind(&mut b, "redo", vec![KeyCombo::ctrl("Y"), KeyCombo::ctrl_shift("Z")]);
        bind(&mut b, "cut_clip", vec![KeyCombo::ctrl("X")]);
        bind(&mut b, "copy_clip", vec![KeyCombo::ctrl("C")]);
        bind(&mut b, "paste_clip", vec![KeyCombo::ctrl("V")]);
        // Same default key combo as copy/paste_clip on purpose — the
        // dispatcher resolves the conflict by panel context. When the
        // shared combo fires and the active panel is the node graph
        // (with nodes selected) the color-node path wins; everywhere
        // else the clip path wins. A user who wants distinct bindings
        // remaps these in Prefs → Keybindings and the dispatcher fires
        // each independently.
        bind(&mut b, "copy_color_nodes", vec![KeyCombo::ctrl("C")]);
        bind(&mut b, "paste_color_nodes", vec![KeyCombo::ctrl("V")]);
        bind(&mut b, "copy_attributes", vec![KeyCombo::ctrl_shift("C")]);
        bind(&mut b, "paste_attributes", vec![KeyCombo::ctrl_shift("V")]);
        bind(&mut b, "duplicate_clip", vec![KeyCombo::ctrl("D")]);
        bind(&mut b, "select_all", vec![KeyCombo::ctrl("A")]);
        bind(&mut b, "deselect_all", vec![KeyCombo::ctrl_shift("A")]);
        bind(&mut b, "delete_clip", vec![KeyCombo::new("Delete"), KeyCombo::new("Backspace")]);
        bind(&mut b, "lift_clip", vec![KeyCombo::shift("Delete"), KeyCombo::shift("Backspace")]);
        bind(&mut b, "split_at_playhead", vec![KeyCombo::ctrl("B")]);
        // Freeze Frame moved off Shift+F so the NLE-standard
        // Match Frame Reverse can claim it.
        bind(&mut b, "freeze_frame", vec![KeyCombo::alt("H")]);
        bind(&mut b, "group_clips", vec![KeyCombo::ctrl("G")]);
        bind(&mut b, "ungroup_clips", vec![KeyCombo::ctrl_shift("G")]);

        // Tools
        bind(&mut b, "select_tool", vec![KeyCombo::new("V")]);
        bind(&mut b, "blade_tool", vec![KeyCombo::new("B")]);
        bind(&mut b, "range_tool", vec![KeyCombo::new("R")]);
        bind(&mut b, "trim_tool", vec![KeyCombo::new("T")]);

        // Timeline editing
        bind(&mut b, "insert_edit", vec![KeyCombo::new(","), KeyCombo::new("F10")]);
        bind(&mut b, "overwrite_edit", vec![KeyCombo::new(".")]);
        bind(&mut b, "lift_edit", vec![KeyCombo::new(";")]);
        bind(&mut b, "extract_edit", vec![KeyCombo::new("'")]);
        bind(&mut b, "toggle_edit_mode", vec![KeyCombo::shift("W")]);
        bind(&mut b, "ripple_trim_head", vec![KeyCombo::alt("[")]);
        bind(&mut b, "ripple_trim_tail", vec![KeyCombo::alt("]")]);
        bind(&mut b, "trim_out_to_playhead", vec![KeyCombo::new("Q")]);
        bind(&mut b, "trim_in_to_playhead", vec![KeyCombo::new("W")]);
        bind(&mut b, "extract_frame_to_file", vec![KeyCombo::ctrl_alt("S")]);
        bind(&mut b, "extract_frame_to_bin", vec![KeyCombo::ctrl_alt("B")]);
        // Shift+S — toggle audio playback during hover skimming. Mirrors
        // the FCP X convention (S = skim feature on/off, Shift+S =
        // audio specifically). When a clip on a connected storyline
        // is selected the same key invokes Promote/Break Storyline
        // instead — that priority lives in the handler.
        bind(&mut b, "toggle_audio_skim", vec![KeyCombo::shift("S")]);

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
        // Fullscreen toggle moved off F (which is the NLE-standard
        // Match Frame shortcut) onto F11 (the OS convention).
        bind(&mut b, "toggle_fullscreen", vec![KeyCombo::new("F11")]);
        bind(&mut b, "toggle_before_after", vec![KeyCombo::alt("B")]);
        bind(&mut b, "toggle_color_panel", vec![KeyCombo::new("C")]);
        bind(&mut b, "toggle_scopes", vec![KeyCombo::new("Alt+V")]);
        bind(&mut b, "toggle_zebra", vec![KeyCombo::alt("Z")]);
        bind(&mut b, "toggle_false_color", vec![KeyCombo::alt("F")]);
        bind(&mut b, "toggle_perf_overlay", vec![KeyCombo::new("P")]);
        bind(&mut b, "toggle_perf_recording", vec![KeyCombo::ctrl_shift("P")]);
        bind(&mut b, "toggle_matte_preview", vec![KeyCombo::alt("M")]);
        bind(&mut b, "toggle_safe_areas", vec![KeyCombo::alt("S")]);
        bind(&mut b, "toggle_grid_overlay", vec![KeyCombo::alt("G")]);

        // Window panels
        bind(&mut b, "toggle_project_notes", vec![KeyCombo::ctrl_shift("N")]);
        // Per-panel search focus. Hits the search field of whichever
        // panel the user last clicked into (resolved via active_panel
        // / PanelKind). When the active panel has no search surface
        // (Inspector / Viewer / Timeline) the action is a silent
        // no-op — see app.rs dispatcher for the watchdog clear.
        bind(&mut b, "focus_panel_search", vec![KeyCombo::ctrl("F")]);

        // Timeline display
        bind(&mut b, "toggle_snap", vec![KeyCombo::new("N")]);
        bind(&mut b, "toggle_skimming", vec![KeyCombo::new("S")]);
        bind(&mut b, "fit_to_window", vec![KeyCombo::new("Z")]);
        bind(&mut b, "zoom_to_selection", vec![KeyCombo::shift("Z")]);
        bind(&mut b, "zoom_in", vec![KeyCombo::new("=")]);
        bind(&mut b, "zoom_out", vec![KeyCombo::new("-")]);

        // Match Frame — NLE staple. F finds the source frame
        // corresponding to the program playhead and loads it in the
        // source viewer; Shift+F walks the timeline forward to the
        // next occurrence of the source viewer's current frame.
        bind(&mut b, "match_frame_program_to_source", vec![KeyCombo::new("F")]);
        bind(&mut b, "match_frame_source_to_program", vec![KeyCombo::shift("F")]);

        // Markers & Keyframes
        bind(&mut b, "add_marker", vec![KeyCombo::new("M")]);
        bind(&mut b, "add_clip_marker", vec![KeyCombo::shift("M")]);
        bind(&mut b, "toggle_keyframe", vec![KeyCombo::alt("K")]);
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

        // AI Tools — task launchers + run/cancel.
        // Conflict notes:
        //   * `B` is `blade_tool` and `toggle_before_after` already, so the
        //     AI launchers all use Shift+<letter>. Shift+B / Shift+J /
        //     Shift+D are otherwise unbound. Shift+F is taken by
        //     match_frame_source_to_program — we deliberately don't reuse it.
        //   * Ctrl+A is `select_all` and Ctrl+Shift+A is `deselect_all`,
        //     so the panel toggle uses Alt+A (otherwise free) instead.
        //   * Escape is implicitly used everywhere (close dialog / deselect
        //     / exit fullscreen) and the keybinding system is global, so
        //     we bind cancel to Ctrl+. (Ctrl+period, otherwise unbound)
        //     and rely on a UI button as the primary cancel surface.
        //   * Ctrl+Enter is unbound elsewhere — picked as "run with
        //     current settings" because it mirrors the "submit form"
        //     muscle memory.
        bind(&mut b, "ai_remove_background", vec![KeyCombo::shift("B")]);
        bind(&mut b, "ai_isolate_object", vec![KeyCombo::shift("J")]);
        bind(&mut b, "ai_detect_objects", vec![KeyCombo::shift("D")]);
        bind(&mut b, "ai_run_inference", vec![KeyCombo::ctrl("Enter")]);
        bind(&mut b, "ai_cancel_inference", vec![KeyCombo::ctrl(".")]);
        bind(&mut b, "toggle_ai_tools_panel", vec![KeyCombo::alt("A")]);

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
            "copy_color_nodes" => "Copy Colour Nodes",
            "paste_color_nodes" => "Paste Colour Nodes",
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
            "toggle_audio_skim" => "Toggle Audio Skim",
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
            "toggle_loop" => "Toggle Loop Playback",
            "toggle_fullscreen" => "Toggle Fullscreen",
            "toggle_before_after" => "Toggle Before/After",
            "toggle_color_panel" => "Toggle Colour Panel",
            "toggle_scopes" => "Toggle Scopes",
            "toggle_zebra" => "Toggle Zebra Overlay",
            "toggle_false_color" => "Toggle False Colour",
            "toggle_perf_overlay" => "Toggle Perf Overlay",
            "toggle_perf_recording" => "Toggle Perf Recording",
            "toggle_matte_preview" => "Toggle Matte Preview",
            "toggle_safe_areas" => "Toggle Safe Areas",
            "toggle_grid_overlay" => "Toggle Grid & Crosshair",
            "toggle_project_notes" => "Toggle Project Notes",
            "focus_panel_search" => "Focus Panel Search",
            "toggle_snap" => "Toggle Snap",
            "toggle_skimming" => "Toggle Skimming",
            "fit_to_window" => "Fit to Window",
            "zoom_to_selection" => "Zoom to Selection",
            "zoom_in" => "Zoom In",
            "zoom_out" => "Zoom Out",
            "add_marker" => "Add Marker",
            "add_clip_marker" => "Add Clip Marker",
            "shuttle_reverse" => "Shuttle Reverse (J)",
            "shuttle_pause" => "Shuttle Pause (K)",
            "shuttle_forward" => "Shuttle Forward (L)",
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
            "prev_color_node" => "Previous Colour Node",
            "next_color_node" => "Next Colour Node",
            "toggle_node_bypass" => "Toggle Node Bypass",
            "reset_current_node" => "Reset Current Node",
            "cycle_scope_mode" => "Cycle Scope Mode",
            "create_title" => "Create Title Clip",
            "connect_clip" => "Connect to Primary",
            "disconnect_clip" => "Disconnect from Primary",
            "ai_remove_background" => "AI: Remove Background",
            "ai_isolate_object" => "AI: Isolate Object",
            "ai_detect_objects" => "AI: Detect Objects",
            "ai_run_inference" => "AI: Run Inference",
            "ai_cancel_inference" => "AI: Cancel Inference",
            "toggle_ai_tools_panel" => "Toggle AI Tools Panel",
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
            ("Transport", &["play_pause", "step_forward", "step_backward", "go_to_start", "go_to_end", "shuttle_reverse", "shuttle_pause", "shuttle_forward", "toggle_loop"]),
            ("Viewer", &["toggle_fullscreen", "toggle_before_after", "toggle_color_panel", "toggle_scopes", "toggle_zebra", "toggle_false_color", "toggle_perf_overlay", "toggle_perf_recording", "toggle_matte_preview", "toggle_safe_areas", "toggle_grid_overlay", "extract_frame_to_file", "extract_frame_to_bin"]),
            ("Window", &["toggle_project_notes", "focus_panel_search"]),
            ("Display", &["toggle_snap", "toggle_skimming", "toggle_audio_skim", "fit_to_window", "zoom_to_selection", "zoom_in", "zoom_out"]),
            ("Markers", &["add_marker", "add_clip_marker", "toggle_keyframe", "toggle_keyframe_editor", "prev_keyframe", "next_keyframe"]),
            ("Multicam", &["multicam_angle_1", "multicam_angle_2", "multicam_angle_3",
                "multicam_angle_4", "multicam_angle_5", "multicam_angle_6",
                "multicam_angle_7", "multicam_angle_8", "multicam_angle_9",
                "create_multicam"]),
            ("Colour", &["prev_color_node", "next_color_node", "toggle_node_bypass", "reset_current_node", "cycle_scope_mode", "copy_color_nodes", "paste_color_nodes"]),
            ("AI Tools", &["ai_remove_background", "ai_isolate_object", "ai_detect_objects", "ai_run_inference", "ai_cancel_inference", "toggle_ai_tools_panel"]),
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

#[cfg(test)]
mod registry_coverage {
    //! Which bindings actually do anything.
    //!
    //! A binding in this table only has an effect if some handler asks
    //! for it by name with `is_action_pressed`. Most of these keys *do*
    //! work — but from a hardcoded `key_pressed(Key::X)` in `app.rs`
    //! rather than through here, and the two have drifted. The visible
    //! consequence is that **Preferences → Keybindings silently does
    //! nothing for these actions**: the row is there, it accepts a new
    //! combo, and the key carries on doing what the hardcoded check
    //! says.
    //!
    //! The fix is to convert each hardcoded check to
    //! `is_action_pressed`, which is mechanical but touches a lot of
    //! call sites. Until then this is a ratchet: the list may shrink,
    //! never grow.

    use super::KeyBindings;

    /// Actions declared here that no handler reads. Empty, and the two
    /// tests below keep it that way.
    ///
    /// It was 54. Most were real: the key worked from a hardcoded
    /// `key_pressed` check while the registry entry sat inert, so
    /// Preferences -> Keybindings accepted a remap and changed nothing.
    /// Eleven were not — the multicam angles are dispatched through a
    /// name built at run time, which a literal search cannot see, and
    /// `is_dispatched` now accounts for that.
    const UNWIRED: &[&str] = &[];

    fn sources() -> String {
        // Every file that dispatches a key. Adding a handler somewhere
        // else means adding it here too, or the ratchet quietly stops
        // seeing it — which is how the transport keys looked unwired
        // after they had been converted.
        format!(
            "{}{}{}",
            include_str!("app.rs"),
            include_str!("keybindings.rs"),
            include_str!("panels/transport.rs"),
        )
    }

    /// Combos deliberately shared by more than one action, each with
    /// the reason it is safe. Anything else is a bug: two handlers fire
    /// on one press, which is how bare B ran the Blade tool *and*
    /// toggled before/after, and bare Z drew zebras *and* zoomed to fit.
    const SHARED_ON_PURPOSE: &[(&str, &str)] = &[
        ("Ctrl+C", "copy_clip / copy_color_nodes — resolved by active panel"),
        ("Ctrl+V", "paste_clip / paste_color_nodes — resolved by active panel"),
    ];

    /// Whether any handler dispatches `action`.
    ///
    /// Not just a search for the literal name: the multicam angles are
    /// dispatched through a name built at run time
    /// (`format!("multicam_angle_{n}")`), so a literal-only search
    /// reported all nine as unwired when they have always worked. Any
    /// action whose name shares a prefix with a `format!` used at a
    /// dispatch site counts as reached.
    fn is_dispatched(action: &str, src: &str) -> bool {
        if src.contains(&format!("is_action_pressed(\"{action}\"")) {
            return true;
        }
        // A dispatched format! string: take the literal part before the
        // first `{` and see whether this action starts with it.
        for (i, _) in src.match_indices("let action = format!(\"") {
            let rest = &src[i + "let action = format!(\"".len()..];
            let Some(end) = rest.find('"') else { continue };
            let pattern = &rest[..end];
            let prefix = pattern.split('{').next().unwrap_or("");
            if !prefix.is_empty() && action.starts_with(prefix) {
                return true;
            }
        }
        false
    }

    #[test]
    fn no_combo_runs_two_actions() {
        use std::collections::BTreeMap;
        let kb = KeyBindings::defaults();
        let mut by_combo: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (action, combos) in &kb.bindings {
            for c in combos {
                by_combo.entry(c.display()).or_default().push(action.clone());
            }
        }
        let allowed: std::collections::BTreeSet<&str> =
            SHARED_ON_PURPOSE.iter().map(|(c, _)| *c).collect();
        let clashes: Vec<String> = by_combo
            .iter()
            .filter(|(c, acts)| acts.len() > 1 && !allowed.contains(c.as_str()))
            .map(|(c, acts)| format!("{c} -> {}", acts.join(", ")))
            .collect();
        assert!(
            clashes.is_empty(),
            "these combos would fire more than one action on a single press: {clashes:#?}"
        );
    }

    #[test]
    fn no_new_unwired_actions() {
        let src = sources();
        let kb = KeyBindings::defaults();
        let unwired: Vec<String> = kb
            .bindings
            .keys()
            .filter(|a| !is_dispatched(a, &src))
            .cloned()
            .collect();

        let known: std::collections::BTreeSet<&str> = UNWIRED.iter().copied().collect();
        let new: Vec<&String> = unwired.iter().filter(|a| !known.contains(a.as_str())).collect();
        assert!(
            new.is_empty(),
            "new keybinding actions that no handler reads — they will look remappable in \
             Preferences and do nothing: {new:?}"
        );
    }

    #[test]
    fn the_unwired_list_has_no_stale_entries() {
        // Keeps the ratchet honest: once an action is wired up, its
        // entry has to go, or the list stops meaning anything.
        let src = sources();
        let stale: Vec<&&str> = UNWIRED
            .iter()
            .filter(|a| is_dispatched(a, &src))
            .collect();
        assert!(
            stale.is_empty(),
            "these are wired up now and should be removed from UNWIRED: {stale:?}"
        );
    }

    /// Two actions on one key, on purpose and only on purpose.
    ///
    /// Ctrl+C and Ctrl+V each drive two actions — a clip or a grade's
    /// nodes — and `app.rs` resolves them by panel context, matching on
    /// the pair and letting the colour panel win when it holds a node
    /// selection. That is deliberate and documented there.
    ///
    /// Nothing stopped a third pair appearing by accident. The other
    /// menu tests check that an action a menu names *is* bound; none of
    /// them notice two actions claiming the same combination, where the
    /// symptom is one of them silently never firing.
    #[test]
    fn only_the_known_shared_defaults_collide() {
        use std::collections::BTreeMap;

        // Pairs that share a default and are disambiguated at dispatch.
        const INTENTIONAL: &[(&str, &str)] = &[
            ("copy_clip", "copy_color_nodes"),
            ("paste_clip", "paste_color_nodes"),
        ];

        let kb = KeyBindings::defaults();
        let mut owners: BTreeMap<String, Vec<&str>> = BTreeMap::new();
        for (action, combos) in &kb.bindings {
            for c in combos {
                let key = format!(
                    "{}{}{}{}",
                    if c.ctrl { "Ctrl+" } else { "" },
                    if c.shift { "Shift+" } else { "" },
                    if c.alt { "Alt+" } else { "" },
                    c.key,
                );
                owners.entry(key).or_default().push(action.as_str());
            }
        }

        let mut unexpected = Vec::new();
        for (combo, mut actions) in owners {
            actions.sort_unstable();
            actions.dedup();
            if actions.len() < 2 {
                continue;
            }
            let known = actions.len() == 2
                && INTENTIONAL.iter().any(|(a, b)| {
                    let mut pair = [*a, *b];
                    pair.sort_unstable();
                    pair[..] == actions[..]
                });
            if !known {
                unexpected.push(format!("{combo} -> {actions:?}"));
            }
        }

        assert!(
            unexpected.is_empty(),
            "these combinations are claimed by more than one action, so one of them \
             will not fire. Either rebind, or disambiguate at dispatch the way Ctrl+C \
             does and add the pair to INTENTIONAL:\n{}",
            unexpected.join("\n"),
        );
    }
}
