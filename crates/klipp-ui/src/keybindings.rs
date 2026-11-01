//! Keyboard shortcut remapping system.
//!
//! Stores a mapping from action names to key combinations. Persisted to
//! `~/.config/klipp/keybindings.json`.
//!
//! The map is built in layers: Klipp's own defaults, then a
//! [`KeymapPreset`] (the layout of another NLE, for an editor whose hands
//! already know one), then the user's own changes. The file stores only
//! the preset's name and the changes, so switching preset keeps them.

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
                || self.shifted_punctuation_pressed(egui_key, input)
                || self.clipboard_event_pressed(egui_key, input)
        } else {
            // Text-based keys (punctuation: ,  .  ;  '  etc.). Ctrl and
            // Alt suppress the text event, so a modified one is read as
            // a key press instead; and Shift types another character (`<`
            // for `,`), so Shift+punctuation is read on its key too (#2934:
            // a layout's Shift+, never fired).
            input.events.iter().any(|e| matches!(e, egui::Event::Text(s) if *s == self.key && !self.shift))
                || ((self.ctrl || self.alt || self.shift)
                    && egui::Key::from_name(&self.key).is_some_and(|k| {
                        input.key_pressed(k) || self.shifted_punctuation_pressed(k, input)
                    }))
        }
    }

    /// The window layer turns Ctrl+X, Ctrl+C and Ctrl+V, with Shift or
    /// Alt held or not, into Cut, Copy and Paste events and sends no key
    /// press for them, so those combos are matched on the event: read as
    /// key presses, Copy, Paste and Copy/Paste Attributes never fired.
    fn clipboard_event_pressed(&self, key: egui::Key, input: &egui::InputState) -> bool {
        use egui::Key as K;
        self.ctrl
            && input.events.iter().any(|e| {
                matches!(
                    (key, e),
                    (K::X, egui::Event::Cut) | (K::C, egui::Event::Copy) | (K::V, egui::Event::Paste(_))
                )
            })
    }

    /// Shift changes what a punctuation key reports (`[` arrives as `{`
    /// on a US layout), so Shift+punctuation is matched on the physical
    /// key. Letters and digits never take this path: on a non-US layout
    /// their physical position is a different letter.
    fn shifted_punctuation_pressed(&self, key: egui::Key, input: &egui::InputState) -> bool {
        use egui::Key as K;
        self.shift
            && matches!(
                key,
                K::OpenBracket | K::CloseBracket | K::Semicolon | K::Quote | K::Comma
                    | K::Period | K::Slash | K::Backslash | K::Backtick | K::Minus | K::Equals
            )
            && input.events.iter().any(|e| {
                matches!(e, egui::Event::Key { physical_key: Some(k), pressed: true, .. } if *k == key)
            })
    }

    /// Parse the display form back: `"Ctrl+Shift+Z"`, `"Alt+["`, `"F10"`.
    pub fn parse(s: &str) -> Option<Self> {
        let mut combo = Self::new("");
        let mut rest = s;
        loop {
            if let Some(r) = rest.strip_prefix("Ctrl+") { combo.ctrl = true; rest = r; }
            else if let Some(r) = rest.strip_prefix("Shift+") { combo.shift = true; rest = r; }
            else if let Some(r) = rest.strip_prefix("Alt+") { combo.alt = true; rest = r; }
            else { break; }
        }
        if rest.is_empty() {
            return None;
        }
        combo.key = rest.to_string();
        Some(combo)
    }
}

fn published_id() -> egui::Id {
    egui::Id::new("klipp_key_bindings_in_effect")
}

/// Makes `kb` the keys a tooltip names this frame (#2933): set by the app
/// before anything is drawn.
pub fn publish(ctx: &egui::Context, kb: &KeyBindings) {
    let kb = std::sync::Arc::new(kb.clone());
    ctx.data_mut(|d| d.insert_temp(published_id(), kb));
}

/// `label` with `action`'s key in the layout in effect, `"Loop playback
/// (Ctrl+L)"`, or `label` alone when the action has no key there.
pub fn tip(ctx: &egui::Context, label: &str, action: &str) -> String {
    let kb = ctx.data(|d| d.get_temp::<std::sync::Arc<KeyBindings>>(published_id()));
    let key = kb.map_or_else(|| KeyBindings::defaults().display_for(action), |kb| kb.display_for(action));
    if key.is_empty() { label.to_string() } else { format!("{label} ({key})") }
}

/// Where a frame's keys are read (`app::shortcuts::key_scope`), as far as
/// which actions can fire there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Place {
    /// No project open: the launch screen.
    Launch,
    /// A text field has the keys.
    Typing,
    /// The picture's node canvas has the keys.
    PictureCanvas,
    /// The grade's node canvas has the keys.
    GradeCanvas,
    /// The editor: the timeline, the viewers and the panels.
    Editor,
}

impl Place {
    pub const ALL: [Place; 5] = [Place::Launch, Place::Typing, Place::PictureCanvas, Place::GradeCanvas, Place::Editor];

    pub fn label(self) -> &'static str {
        match self {
            Place::Launch => "on the launch screen",
            Place::Typing => "while typing",
            Place::PictureCanvas => "on the picture canvas",
            Place::GradeCanvas => "on the grade canvas",
            Place::Editor => "in the editor",
        }
    }
}

/// The actions every place reads, a project open or not
/// (`shortcuts_app`).
pub const APP_ACTIONS: &[&str] = &["new_project", "open_project", "preferences", "quit"];
/// The Ctrl combinations, read while typing and on the canvases too
/// (`shortcuts_ctrl_global`).
pub const CTRL_ACTIONS: &[&str] = &[
    "close_project", "command_palette", "export", "focus_panel_search", "go_to_timecode", "group_clips", "import_media", "redo",
    "save_project", "save_project_as", "toggle_audio_mixer", "toggle_subtitle_editor", "undo", "ungroup_clips",
];
/// The grade's keys, on the grade canvas and in the editor
/// (`grade_node_keys`).
pub const GRADE_ACTIONS: &[&str] = &[
    "next_color_node", "prev_color_node", "printer_blue_down", "printer_blue_up", "printer_green_down", "printer_green_up",
    "printer_master_down", "printer_master_up", "printer_red_down", "printer_red_up", "reset_current_node", "toggle_node_bypass",
];
/// Playback, read wherever no text field or dialog has the keys
/// (`panels::transport`).
pub const TRANSPORT_ACTIONS: &[&str] = &[
    "play_pause", "step_backward", "step_forward", "go_to_start", "go_to_end", "shuttle_reverse", "shuttle_pause",
    "shuttle_forward", "next_marker", "prev_marker", "play_in_to_out",
];

/// Where `action`'s key fires. Every action not named above is the
/// editor's alone.
pub fn action_places(action: &str) -> &'static [Place] {
    use Place::*;
    if APP_ACTIONS.contains(&action) {
        &[Launch, Typing, PictureCanvas, GradeCanvas, Editor]
    } else if CTRL_ACTIONS.contains(&action) {
        &[Typing, PictureCanvas, GradeCanvas, Editor]
    } else if TRANSPORT_ACTIONS.contains(&action) {
        &[PictureCanvas, GradeCanvas, Editor]
    } else if GRADE_ACTIONS.contains(&action) {
        &[GradeCanvas, Editor]
    } else {
        &[Editor]
    }
}

/// The first place both actions fire, if any.
pub fn shared_place(a: &str, b: &str) -> Option<Place> {
    action_places(a).iter().copied().find(|p| action_places(b).contains(p))
}

/// The edit-point trims, which the grade's keys step aside for.
const TRIM_ACTIONS: &[&str] = &["trim_edit_back", "trim_edit_forward", "trim_edit_back_many", "trim_edit_forward_many"];

/// Two actions the dispatcher tells apart when they share a key, so one
/// key never runs both: why, or `None` for an ordinary pair.
pub fn resolved(a: &str, b: &str) -> Option<&'static str> {
    let pair = |x: &str, y: &str| (a == x && b == y) || (a == y && b == x);
    if pair("copy_clip", "copy_color_nodes") || pair("paste_clip", "paste_color_nodes") {
        return Some("the colour panel takes it for the grade's nodes, anywhere else the clip (`clipboard_copy`, `clipboard_paste`)");
    }
    let trim_and_grade = (TRIM_ACTIONS.contains(&a) && GRADE_ACTIONS.contains(&b)) || (TRIM_ACTIONS.contains(&b) && GRADE_ACTIONS.contains(&a));
    if trim_and_grade {
        return Some("with edit points selected the trim takes it, else the grade (`editor_grade_keys`)");
    }
    // One command in an editor's layout, two actions here: the selection
    // picks (`app::shortcuts::shared_key_losers`).
    let nudge = |t: &str, n: &str| pair(t, n);
    if nudge("trim_edit_back", "nudge_clips_back") || nudge("trim_edit_forward", "nudge_clips_forward") || nudge("trim_edit_back_many", "nudge_clips_back_many") || nudge("trim_edit_forward_many", "nudge_clips_forward_many") {
        return Some("with edit points selected they are trimmed, else the selected clips nudged");
    }
    if pair("delete_clip", "extract_edit") {
        return Some("Delete takes a selected clip out, else the marked range");
    }
    if pair("lift_clip", "lift_edit") {
        return Some("a selected clip is lifted, else the marked range");
    }
    if pair("add_marker", "add_clip_marker") {
        return Some("a marker on the selected clip, else on the timeline");
    }
    pair("match_frame_program_to_source", "match_frame_source_to_program").then_some("from the viewer shown: the source's frame in the timeline, or the timeline's in its source")
}

/// Two actions on one key in one place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub a: String,
    pub b: String,
    pub combo: KeyCombo,
    pub place: Place,
}

/// A named starting layout: Klipp's own, or the defaults of the NLE an
/// editor is arriving from. Only actions Klipp has are mapped; a key a
/// preset claims is taken off whichever Klipp action held it, so no
/// press runs two things.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeymapPreset {
    #[default]
    Klipp,
    Premiere,
    Avid,
    Resolve,
    FinalCut,
    /// The open-source editor's published defaults (#3027).
    Kdenlive,
}

impl KeymapPreset {
    pub const ALL: [KeymapPreset; 6] = [
        KeymapPreset::Klipp,
        KeymapPreset::Premiere,
        KeymapPreset::Avid,
        KeymapPreset::Resolve,
        KeymapPreset::FinalCut,
        KeymapPreset::Kdenlive,
    ];

    pub fn label(self) -> &'static str {
        match self {
            KeymapPreset::Klipp => "Klipp",
            KeymapPreset::Premiere => "Premiere Pro",
            KeymapPreset::Avid => "Avid Media Composer",
            KeymapPreset::Resolve => "DaVinci Resolve",
            KeymapPreset::FinalCut => "Final Cut Pro",
            KeymapPreset::Kdenlive => "Kdenlive",
        }
    }

    /// The actions this preset binds differently from Klipp.
    fn table(self) -> &'static [(&'static str, &'static [&'static str])] {
        match self {
            KeymapPreset::Klipp => &[],
            KeymapPreset::Premiere => crate::keymap_presets::PREMIERE,
            KeymapPreset::Avid => crate::keymap_presets::AVID,
            KeymapPreset::Resolve => crate::keymap_presets::RESOLVE,
            KeymapPreset::FinalCut => crate::keymap_presets::FINAL_CUT,
            KeymapPreset::Kdenlive => crate::keymap_presets::KDENLIVE,
        }
    }

    /// The keys the editor uses for commands Klipp lacks: no Klipp action
    /// keeps one under this preset.
    pub(crate) fn taken(self) -> &'static [&'static str] {
        match self {
            KeymapPreset::Klipp => &[],
            KeymapPreset::Premiere => crate::keymap_presets::PREMIERE_TAKEN,
            KeymapPreset::Avid => crate::keymap_presets::AVID_TAKEN,
            KeymapPreset::Resolve => crate::keymap_presets::RESOLVE_TAKEN,
            KeymapPreset::FinalCut => crate::keymap_presets::FINAL_CUT_TAKEN,
            KeymapPreset::Kdenlive => crate::keymap_presets::KDENLIVE_TAKEN,
        }
    }

    /// How many actions the preset gives a key.
    pub fn mapped_count(self) -> usize {
        self.table().iter().filter(|(_, combos)| !combos.is_empty()).count()
    }

    /// Klipp actions that have a key of their own but none under this
    /// preset, because it gave their keys to something else.
    pub fn displaced(self) -> Vec<String> {
        let klipp = KeyBindings::klipp_defaults();
        self.bindings()
            .into_iter()
            .filter(|(action, combos)| combos.is_empty() && klipp.get(action).is_some_and(|k| !k.is_empty()))
            .map(|(action, _)| action)
            .collect()
    }

    /// The full map: Klipp's defaults with this preset's keys laid over
    /// them. A key the preset claims is removed from any other action,
    /// which is left with its remaining keys, or unbound.
    pub fn bindings(self) -> BTreeMap<String, Vec<KeyCombo>> {
        let mut map = KeyBindings::klipp_defaults();
        let table: Vec<(&str, Vec<KeyCombo>)> = self
            .table()
            .iter()
            .map(|(a, combos)| (*a, combos.iter().filter_map(|c| KeyCombo::parse(c)).collect()))
            .collect();
        let taken: Vec<KeyCombo> = self.taken().iter().filter_map(|c| KeyCombo::parse(c)).collect();
        let claimed: Vec<&KeyCombo> = table.iter().flat_map(|(_, c)| c).chain(&taken).collect();
        for (action, combos) in map.iter_mut() {
            if !table.iter().any(|(a, _)| a == action) {
                combos.retain(|c| !claimed.contains(&c));
            }
        }
        for (action, combos) in table {
            map.insert(action.to_string(), combos);
        }
        map
    }
}

/// Map action names → key combos, as in effect: preset plus the user's
/// changes.
#[derive(Debug, Clone)]
pub struct KeyBindings {
    /// The layout the map starts from.
    pub preset: KeymapPreset,
    pub bindings: BTreeMap<String, Vec<KeyCombo>>,
}

/// What `keybindings.json` holds: the preset, and only the actions the
/// user changed from it. A file from before presets has no `preset` and
/// the whole map under `bindings`; it loads as Klipp plus whatever in it
/// differs from Klipp's defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct KeymapFile {
    #[serde(default)]
    preset: KeymapPreset,
    #[serde(default)]
    bindings: BTreeMap<String, Vec<KeyCombo>>,
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

    /// Whether an action's key is held down now, with its modifiers:
    /// for keys that act while held, like J and L with K (#921).
    pub fn is_action_down(&self, action: &str, input: &egui::InputState) -> bool {
        self.bindings.get(action).is_some_and(|combos| {
            combos.iter().any(|c| {
                input.modifiers.ctrl == c.ctrl
                    && input.modifiers.shift == c.shift
                    && input.modifiers.alt == c.alt
                    && parse_egui_key(&c.key).is_some_and(|k| input.key_down(k))
            })
        })
    }

    /// These bindings with `actions` unbound: for one frame's dispatch.
    pub fn without(&self, actions: &[&str]) -> Self {
        let mut kb = self.clone();
        for a in actions {
            kb.bindings.remove(*a);
        }
        kb
    }

    /// Get display string for an action's primary binding.
    pub fn display_for(&self, action: &str) -> String {
        self.bindings.get(action)
            .and_then(|c| c.first())
            .map(|c| c.display())
            .unwrap_or_default()
    }

    /// Klipp's own layout, with no changes.
    pub fn defaults() -> Self {
        Self::from_preset(KeymapPreset::Klipp)
    }

    /// A preset's layout, with no changes.
    pub fn from_preset(preset: KeymapPreset) -> Self {
        Self { preset, bindings: preset.bindings() }
    }

    /// The actions whose keys differ from the preset's.
    pub fn overrides(&self) -> BTreeMap<String, Vec<KeyCombo>> {
        let base = self.preset.bindings();
        self.bindings
            .iter()
            .filter(|(action, combos)| base.get(*action) != Some(*combos))
            .map(|(a, c)| (a.clone(), c.clone()))
            .collect()
    }

    /// Switch to another preset, keeping the user's changes on top.
    pub fn set_preset(&mut self, preset: KeymapPreset) {
        let overrides = self.overrides();
        *self = Self::from_preset(preset);
        self.bindings.extend(overrides);
    }

    /// Drop the user's changes, keeping the preset.
    pub fn reset_to_preset(&mut self) {
        *self = Self::from_preset(self.preset);
    }

    fn to_file(&self) -> KeymapFile {
        KeymapFile { preset: self.preset, bindings: self.overrides() }
    }

    fn from_file(file: KeymapFile) -> Self {
        let mut kb = Self::from_preset(file.preset);
        kb.bindings.extend(file.bindings);
        kb
    }

    /// Klipp's default map.
    fn klipp_defaults() -> BTreeMap<String, Vec<KeyCombo>> {
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
        bind(&mut b, "add_default_transition", vec![KeyCombo::shift("T")]);
        bind(&mut b, "add_audio_crossfade", vec![KeyCombo::alt("T")]);
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
        // Every combo that would suit it is taken here or in a preset;
        // bindable in Preferences (#845).
        bind(&mut b, "paste_insert", vec![]);
        bind(&mut b, "duplicate_clip", vec![KeyCombo::ctrl("D")]);
        // Auditions (#3209): no keys of Klipp's own, as Ctrl+Y and Y are
        // Redo and the Text tool; the Final Cut layout gives them its.
        bind(&mut b, "create_audition", vec![]);
        bind(&mut b, "duplicate_as_audition", vec![]);
        bind(&mut b, "finalize_audition", vec![]);
        bind(&mut b, "open_audition", vec![]);
        bind(&mut b, "select_all", vec![KeyCombo::ctrl("A")]);
        bind(&mut b, "deselect_all", vec![KeyCombo::ctrl_shift("A")]);
        // The selected clip's track, all of it or from the playhead on or
        // back; and every track from the playhead (#844).
        bind(&mut b, "select_track_clips", vec![KeyCombo::ctrl_alt("A")]);
        bind(&mut b, "select_track_forward", vec![KeyCombo::ctrl_alt("ArrowRight")]);
        bind(&mut b, "select_track_backward", vec![KeyCombo::ctrl_alt("ArrowLeft")]);
        bind(&mut b, "select_forward", vec![]);
        bind(&mut b, "select_backward", vec![]);
        bind(&mut b, "delete_clip", vec![KeyCombo::new("Delete"), KeyCombo::new("Backspace")]);
        bind(&mut b, "lift_clip", vec![KeyCombo::shift("Delete"), KeyCombo::shift("Backspace")]);
        bind(&mut b, "split_at_playhead", vec![KeyCombo::ctrl("B")]);
        // Freeze Frame moved off Shift+F so the NLE-standard
        // Match Frame Reverse can claim it.
        bind(&mut b, "freeze_frame", vec![KeyCombo::alt("H")]);
        bind(&mut b, "freeze_track", vec![KeyCombo::ctrl_shift("F")]);
        bind(&mut b, "group_clips", vec![KeyCombo::ctrl("G")]);
        bind(&mut b, "ungroup_clips", vec![KeyCombo::ctrl_shift("G")]);

        // Tools
        bind(&mut b, "select_tool", vec![KeyCombo::new("V")]);
        bind(&mut b, "blade_tool", vec![KeyCombo::new("B")]);
        bind(&mut b, "range_tool", vec![KeyCombo::new("R")]);
        bind(&mut b, "trim_tool", vec![KeyCombo::new("T")]);
        // Shift+R: R is the Range tool here (#3055).
        bind(&mut b, "stretch_tool", vec![KeyCombo::shift("R")]);
        // The Text tool (#1713). T is the Trim tool's and Shift+T, Alt+T
        // and Ctrl+T are taken, so it has Y, otherwise free; the Premiere
        // layout gives it T.
        bind(&mut b, "text_tool", vec![KeyCombo::new("Y")]);
        // The Mask tool (#74): P, the pen's key in the common keymaps.
        bind(&mut b, "mask_tool", vec![KeyCombo::new("P")]);

        // Timeline editing
        bind(&mut b, "insert_edit", vec![KeyCombo::new(","), KeyCombo::new("F10")]);
        bind(&mut b, "overwrite_edit", vec![KeyCombo::new(".")]);
        bind(&mut b, "fit_to_fill_edit", vec![KeyCombo::shift("F11")]);
        bind(&mut b, "place_on_top_edit", vec![KeyCombo::new("F12")]);
        bind(&mut b, "replace_edit", vec![KeyCombo::shift("F12")]);
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
        bind(&mut b, "leave_compound", vec![KeyCombo::ctrl("ArrowUp")]);
        bind(&mut b, "set_in_point", vec![KeyCombo::new("I")]);
        bind(&mut b, "set_out_point", vec![KeyCombo::new("O")]);
        bind(&mut b, "clear_in_point", vec![KeyCombo::shift("I")]);
        bind(&mut b, "clear_out_point", vec![KeyCombo::shift("O")]);
        bind(&mut b, "clear_in_out", vec![KeyCombo::alt("X")]);
        bind(&mut b, "set_range_to_clip", vec![KeyCombo::new("X")]);
        bind(&mut b, "go_to_timecode", vec![KeyCombo::ctrl("J")]);

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
        bind(&mut b, "play_in_to_out", vec![KeyCombo::ctrl_shift("Space")]);
        bind(&mut b, "next_marker", vec![KeyCombo::shift("ArrowDown")]);
        bind(&mut b, "prev_marker", vec![KeyCombo::shift("ArrowUp")]);

        // Viewer
        // Fullscreen toggle moved off F (which is the NLE-standard
        // Match Frame shortcut) onto F11 (the OS convention).
        bind(&mut b, "toggle_fullscreen", vec![KeyCombo::new("F11")]);
        bind(&mut b, "toggle_before_after", vec![KeyCombo::alt("B")]);
        bind(&mut b, "toggle_color_panel", vec![KeyCombo::new("C")]);
        bind(&mut b, "toggle_scopes", vec![KeyCombo::alt("V")]);
        bind(&mut b, "toggle_zebra", vec![KeyCombo::alt("Z")]);
        bind(&mut b, "toggle_false_color", vec![KeyCombo::alt("F")]);
        // Developer aids, off the single keys: P is the Mask tool's (#74).
        bind(&mut b, "toggle_perf_overlay", vec![KeyCombo::ctrl_shift("P")]);
        bind(&mut b, "toggle_perf_recording", vec![KeyCombo::ctrl_alt("P")]);
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
        // Which of the stacked timelines takes edits, swapped (#2324).
        bind(&mut b, "swap_stacked_focus", vec![KeyCombo::alt("`")]);
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
        // The selected keys' ease, one key each (#2307).
        bind(&mut b, "key_ease_in", vec![KeyCombo::ctrl_alt("I")]);
        bind(&mut b, "key_ease_out", vec![KeyCombo::ctrl_alt("O")]);
        bind(&mut b, "key_ease_in_out", vec![KeyCombo::ctrl_alt("E")]);
        bind(&mut b, "key_linear", vec![KeyCombo::ctrl_alt("L")]);

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
        bind(&mut b, "toggle_multicam_mode", vec![KeyCombo::parse("Shift+Alt+M").expect("a combo")]);

        // Color grading
        bind(&mut b, "nudge_clips_back", vec![KeyCombo::alt(",")]);
        bind(&mut b, "nudge_clips_forward", vec![KeyCombo::alt(".")]);
        bind(&mut b, "nudge_clips_back_many", vec![KeyCombo::parse("Shift+Alt+,").expect("a combo")]);
        bind(&mut b, "nudge_clips_forward_many", vec![KeyCombo::parse("Shift+Alt+.").expect("a combo")]);
        bind(&mut b, "move_clips_up_a_track", vec![KeyCombo::alt("ArrowUp")]);
        bind(&mut b, "move_clips_down_a_track", vec![KeyCombo::alt("ArrowDown")]);
        bind(&mut b, "select_edit_point", vec![KeyCombo::new("U")]);
        bind(&mut b, "play_around_edit", vec![KeyCombo::ctrl_shift("K")]);
        bind(&mut b, "extend_edit", vec![KeyCombo::new("E")]);
        bind(&mut b, "trim_edit_back", vec![KeyCombo::new("[")]);
        bind(&mut b, "trim_edit_forward", vec![KeyCombo::new("]")]);
        bind(&mut b, "trim_edit_back_many", vec![KeyCombo::shift("[")]);
        bind(&mut b, "trim_edit_forward_many", vec![KeyCombo::shift("]")]);
        bind(&mut b, "prev_color_node", vec![KeyCombo::new("[")]);
        bind(&mut b, "next_color_node", vec![KeyCombo::new("]")]);
        bind(&mut b, "toggle_node_bypass", vec![KeyCombo::alt("D")]);
        bind(&mut b, "reset_current_node", vec![KeyCombo::ctrl_shift("R")]);
        // Printer lights (#241), laid out as the numeric keypad's rows:
        // 7 8 9 raise red, green, blue, 4 5 6 lower them, 3 and 1 the
        // master.
        bind(&mut b, "printer_red_up", vec![KeyCombo::ctrl_alt("7")]);
        bind(&mut b, "printer_red_down", vec![KeyCombo::ctrl_alt("4")]);
        bind(&mut b, "printer_green_up", vec![KeyCombo::ctrl_alt("8")]);
        bind(&mut b, "printer_green_down", vec![KeyCombo::ctrl_alt("5")]);
        bind(&mut b, "printer_blue_up", vec![KeyCombo::ctrl_alt("9")]);
        bind(&mut b, "printer_blue_down", vec![KeyCombo::ctrl_alt("6")]);
        bind(&mut b, "printer_master_up", vec![KeyCombo::ctrl_alt("3")]);
        bind(&mut b, "printer_master_down", vec![KeyCombo::ctrl_alt("1")]);
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

        b
    }

    /// Config file path: ~/.config/klipp/keybindings.json
    fn config_path() -> Option<std::path::PathBuf> {
        dirs::config_dir().map(|d| d.join("klipp").join("keybindings.json"))
    }

    /// Load from disk. Actions the file does not mention take the
    /// preset's keys.
    pub fn load() -> Self {
        let defaults = Self::defaults();
        let Some(path) = Self::config_path() else { return defaults; };

        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<KeymapFile>(&contents) {
                Ok(file) => {
                    tracing::info!("Key bindings loaded from {}", path.display());
                    Self::from_file(file)
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
        let json = serde_json::to_string_pretty(&self.to_file())
            .map_err(|e| format!("Failed to serialize keybindings: {e}"))?;
        std::fs::write(&path, json)
            .map_err(|e| format!("Failed to write keybindings: {e}"))?;
        tracing::info!("Key bindings saved to {}", path.display());
        Ok(())
    }

    /// Two actions on one key where both are read (#2934): the same
    /// combination, in a [`Place`] where both fire. One key for a
    /// timeline action and another for the grade canvas alone is not a
    /// conflict; one key for two editor actions is.
    pub fn find_conflicts(&self) -> Vec<Conflict> {
        let mut conflicts = Vec::new();
        let actions: Vec<_> = self.bindings.iter().collect();
        for i in 0..actions.len() {
            for j in (i + 1)..actions.len() {
                let Some(place) = shared_place(actions[i].0, actions[j].0) else { continue };
                if resolved(actions[i].0, actions[j].0).is_some() {
                    continue;
                }
                for combo_a in actions[i].1 {
                    if actions[j].1.contains(combo_a) {
                        conflicts.push(Conflict { a: actions[i].0.clone(), b: actions[j].0.clone(), combo: combo_a.clone(), place });
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
            "new_project" => "New Project",
            "close_project" => "Close Project",
            "quit" => "Quit",
            "command_palette" => "Command Palette",
            "toggle_browser_panel" => "Toggle Browser Panel",
            "toggle_inspector_panel" => "Toggle Inspector Panel",
            "toggle_audio_mixer" => "Toggle Audio Mixer",
            "toggle_subtitle_editor" => "Toggle Subtitle Editor",
            "match_frame_program_to_source" => "Match Frame",
            "match_frame_source_to_program" => "Reverse Match Frame",
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
            "paste_insert" => "Paste Insert",
            "copy_color_nodes" => "Copy Colour Nodes",
            "paste_color_nodes" => "Paste Colour Nodes",
            "copy_attributes" => "Copy Attributes",
            "paste_attributes" => "Paste Attributes",
            "duplicate_clip" => "Duplicate Clip",
            "create_audition" => "Create Audition",
            "duplicate_as_audition" => "Duplicate as Audition",
            "finalize_audition" => "Finalize Audition",
            "open_audition" => "Open Audition",
            "select_all" => "Select All",
            "deselect_all" => "Deselect All",
            "select_track_clips" => "Select All on Track",
            "select_track_forward" => "Select Forward on Track",
            "select_track_backward" => "Select Backward on Track",
            "select_forward" => "Select Forward (All Tracks)",
            "select_backward" => "Select Backward (All Tracks)",
            "delete_clip" => "Delete Clip",
            "lift_clip" => "Lift Clip (Leave Gap)",
            "split_at_playhead" => "Split at Playhead",
            "freeze_frame" => "Freeze Frame",
            "freeze_track" => "Freeze or Unfreeze Track",
            "group_clips" => "Group Clips",
            "ungroup_clips" => "Ungroup Clips",
            "select_tool" => "Select Tool",
            "blade_tool" => "Blade Tool",
            "range_tool" => "Range Tool",
            "trim_tool" => "Trim Tool",
            "stretch_tool" => "Rate Stretch Tool",
            "text_tool" => "Text Tool",
            "mask_tool" => "Mask Tool",
            "insert_edit" => "Insert Edit",
            "overwrite_edit" => "Overwrite Edit",
            "fit_to_fill_edit" => "Fit to Fill",
            "place_on_top_edit" => "Place on Top",
            "replace_edit" => "Replace Edit",
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
            "next_marker" => "Next Marker",
            "prev_marker" => "Previous Marker",
            "play_in_to_out" => "Play In to Out",
            "prev_edit_point" => "Previous Edit Point",
            "leave_compound" => "Leave Compound Clip",
            "add_default_transition" => "Add Default Transition",
            "add_audio_crossfade" => "Add Audio Crossfade",
            "select_edit_point" => "Select Edit Point / Cycle Its Side",
            "nudge_clips_back" => "Nudge Clips Back a Frame",
            "nudge_clips_forward" => "Nudge Clips Forward a Frame",
            "nudge_clips_back_many" => "Nudge Clips Back Ten Frames",
            "nudge_clips_forward_many" => "Nudge Clips Forward Ten Frames",
            "move_clips_up_a_track" => "Move Clips Up a Track",
            "move_clips_down_a_track" => "Move Clips Down a Track",
            "extend_edit" => "Extend Edit to Playhead",
            "play_around_edit" => "Play Around Edit (loops)",
            "trim_edit_back" => "Trim Edit Point Back a Frame",
            "trim_edit_forward" => "Trim Edit Point Forward a Frame",
            "trim_edit_back_many" => "Trim Edit Point Back Ten Frames",
            "trim_edit_forward_many" => "Trim Edit Point Forward Ten Frames",
            "set_in_point" => "Set In Point",
            "set_out_point" => "Set Out Point",
            "clear_in_point" => "Clear In Point",
            "clear_out_point" => "Clear Out Point",
            "clear_in_out" => "Clear In/Out Points",
            "set_range_to_clip" => "Set Range to Clip",
            "go_to_timecode" => "Go to Timecode",
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
            "swap_stacked_focus" => "Swap Stacked Timeline Focus",
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
            "key_ease_in" => "Ease In Selected Keys",
            "key_ease_out" => "Ease Out Selected Keys",
            "key_ease_in_out" => "Ease In-Out Selected Keys",
            "key_linear" => "Linear Selected Keys",
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
            "toggle_multicam_mode" => "Multicam Mode (Source Viewer Angles)",
            "prev_color_node" => "Previous Colour Node",
            "next_color_node" => "Next Colour Node",
            "toggle_node_bypass" => "Toggle Node Bypass",
            "reset_current_node" => "Reset Current Node",
            "printer_red_up" => "Printer Light: Red Up",
            "printer_red_down" => "Printer Light: Red Down",
            "printer_green_up" => "Printer Light: Green Up",
            "printer_green_down" => "Printer Light: Green Down",
            "printer_blue_up" => "Printer Light: Blue Up",
            "printer_blue_down" => "Printer Light: Blue Down",
            "printer_master_up" => "Printer Light: Master Up",
            "printer_master_down" => "Printer Light: Master Down",
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
            ("File", &["new_project", "open_project", "save_project", "save_project_as", "close_project", "import_media", "export", "preferences", "quit"]),
            ("Edit", &["undo", "redo", "cut_clip", "copy_clip", "paste_clip", "paste_insert", "copy_attributes", "paste_attributes", "duplicate_clip", "create_audition", "duplicate_as_audition", "finalize_audition", "open_audition", "select_all", "deselect_all", "select_track_clips", "select_track_forward", "select_track_backward", "select_forward", "select_backward", "delete_clip", "lift_clip", "split_at_playhead", "group_clips", "ungroup_clips"]),
            ("Tools", &["select_tool", "blade_tool", "range_tool", "trim_tool", "stretch_tool", "text_tool", "mask_tool"]),
            ("Timeline", &["add_default_transition", "add_audio_crossfade", "nudge_clips_back", "nudge_clips_forward", "nudge_clips_back_many", "nudge_clips_forward_many", "move_clips_up_a_track", "move_clips_down_a_track", "select_edit_point", "extend_edit", "play_around_edit", "trim_edit_back", "trim_edit_forward", "trim_edit_back_many", "trim_edit_forward_many", "insert_edit", "overwrite_edit", "fit_to_fill_edit", "place_on_top_edit", "replace_edit", "lift_edit", "extract_edit", "toggle_edit_mode", "ripple_trim_head", "ripple_trim_tail", "trim_out_to_playhead", "trim_in_to_playhead", "freeze_frame", "freeze_track"]),
            ("Navigation", &["next_edit_point", "prev_edit_point", "leave_compound", "next_marker", "prev_marker", "set_in_point", "set_out_point", "clear_in_point", "clear_out_point", "clear_in_out", "set_range_to_clip", "go_to_timecode", "match_frame_program_to_source", "match_frame_source_to_program"]),
            ("Transport", &["play_pause", "step_forward", "step_backward", "go_to_start", "go_to_end", "shuttle_reverse", "shuttle_pause", "shuttle_forward", "toggle_loop", "play_in_to_out"]),
            ("Viewer", &["toggle_fullscreen", "toggle_before_after", "toggle_color_panel", "toggle_scopes", "toggle_zebra", "toggle_false_color", "toggle_perf_overlay", "toggle_perf_recording", "toggle_matte_preview", "toggle_safe_areas", "toggle_grid_overlay", "extract_frame_to_file", "extract_frame_to_bin"]),
            ("Window", &["command_palette", "toggle_browser_panel", "toggle_inspector_panel", "toggle_audio_mixer", "toggle_subtitle_editor", "toggle_project_notes", "focus_panel_search"]),
            ("Display", &["swap_stacked_focus", "toggle_snap", "toggle_skimming", "toggle_audio_skim", "fit_to_window", "zoom_to_selection", "zoom_in", "zoom_out"]),
            ("Markers", &["add_marker", "add_clip_marker", "toggle_keyframe", "toggle_keyframe_editor", "prev_keyframe", "next_keyframe", "key_ease_in", "key_ease_out", "key_ease_in_out", "key_linear"]),
            ("Multicam", &["multicam_angle_1", "multicam_angle_2", "multicam_angle_3",
                "multicam_angle_4", "multicam_angle_5", "multicam_angle_6",
                "multicam_angle_7", "multicam_angle_8", "multicam_angle_9",
                "create_multicam", "toggle_multicam_mode"]),
            ("Colour", &["prev_color_node", "next_color_node", "toggle_node_bypass", "reset_current_node", "printer_red_up", "printer_red_down", "printer_green_up", "printer_green_down", "printer_blue_up", "printer_blue_down", "printer_master_up", "printer_master_down", "cycle_scope_mode", "copy_color_nodes", "paste_color_nodes"]),
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

    // Ctrl+X, Ctrl+C and Ctrl+V arrive as Cut, Copy and Paste events,
    // with no key press (see `KeyCombo::clipboard_event_pressed`).
    if input.modifiers.ctrl {
        let clipboard_key = input.events.iter().find_map(|e| match e {
            egui::Event::Cut => Some("X"),
            egui::Event::Copy => Some("C"),
            egui::Event::Paste(_) => Some("V"),
            _ => None,
        });
        if let Some(name) = clipboard_key {
            return Some(KeyCombo {
                ctrl: true,
                shift: input.modifiers.shift,
                alt: input.modifiers.alt,
                key: name.into(),
            });
        }
    }

    // Shift changes what punctuation reports (`[` arrives as `{`), so
    // Shift+punctuation is read from the physical key.
    if input.modifiers.shift {
        for name in [",", ".", ";", "'", "/", "\\", "`", "[", "]", "=", "-"] {
            let Some(key) = egui::Key::from_name(name) else { continue };
            let pressed = input.events.iter().any(|e| {
                matches!(e, egui::Event::Key { physical_key: Some(k), pressed: true, .. } if *k == key)
            });
            if pressed {
                return Some(KeyCombo {
                    ctrl: input.modifiers.ctrl,
                    shift: true,
                    alt: input.modifiers.alt,
                    key: name.into(),
                });
            }
        }
    }

    // Ctrl and Alt suppress text events, so modified punctuation is
    // read as a key press.
    if input.modifiers.ctrl || input.modifiers.alt {
        for name in [",", ".", ";", "'", "/", "\\", "`"] {
            if egui::Key::from_name(name).is_some_and(|k| input.key_pressed(k)) {
                return Some(KeyCombo {
                    ctrl: input.modifiers.ctrl,
                    shift: input.modifiers.shift,
                    alt: input.modifiers.alt,
                    key: name.into(),
                });
            }
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
            "{}{}{}{}",
            include_str!("app.rs"),
            // The frame's key presses, since #826 moved them out of app.rs.
            include_str!("app/shortcuts.rs"),
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
        ("[", "trim_edit_back / prev_color_node — the trim needs a selected edit point, the node step a selected clip, and selecting one clears the other"),
        ("]", "trim_edit_forward / next_color_node — as ["),
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
            ("trim_edit_back", "prev_color_node"),
            ("trim_edit_forward", "next_color_node"),
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

#[cfg(test)]
mod preset_tests {
    use super::{KeyBindings, KeyCombo, KeymapFile, KeymapPreset};

    /// Every bound action needs a readable name: the keyboard page and
    /// the layout note print `action_label`, and without one the raw id
    /// (`command_palette`) is what the user reads.
    #[test]
    fn every_action_has_a_label() {
        let kb = KeyBindings::defaults();
        let raw: Vec<&String> = kb
            .bindings
            .keys()
            .filter(|a| KeyBindings::action_label(a) == a.as_str())
            .collect();
        assert!(raw.is_empty(), "actions with no label, shown by their id: {raw:?}");
    }

    #[test]
    fn combos_parse_back_from_their_display_form() {
        for s in ["Z", "Ctrl+Shift+Z", "Alt+[", "Shift+Delete", "Ctrl+\\", "F10", ","] {
            assert_eq!(KeyCombo::parse(s).unwrap().display(), s);
        }
        assert!(KeyCombo::parse("Ctrl+").is_none());
    }

    #[test]
    fn every_preset_names_real_actions_and_readable_keys() {
        let klipp = KeyBindings::defaults();
        for preset in KeymapPreset::ALL {
            let mut seen = std::collections::BTreeSet::new();
            for (action, combos) in preset.table() {
                assert!(klipp.bindings.contains_key(*action), "{preset:?}: no Klipp action {action}");
                assert!(seen.insert(*action), "{preset:?}: {action} listed twice");
                for c in *combos {
                    let combo = KeyCombo::parse(c).unwrap_or_else(|| panic!("{preset:?}: bad combo {c}"));
                    assert!(
                        super::parse_egui_key(&combo.key).is_some()
                            || egui::Key::from_name(&combo.key).is_some(),
                        "{preset:?}: {action} = {c} names a key Klipp cannot read"
                    );
                }
            }
        }
    }

    /// A preset may take a key from a Klipp action, but must never leave
    /// one press running two actions.
    #[test]
    fn no_preset_makes_one_press_run_two_actions() {
        use std::collections::BTreeMap;
        // A pair the dispatcher tells apart is one action a press (#913,
        // #2934: `resolved`).
        for preset in KeymapPreset::ALL {
            let mut by_combo: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for (action, combos) in preset.bindings() {
                for c in combos {
                    by_combo.entry(c.display()).or_default().push(action.clone());
                }
            }
            let clashes: Vec<String> = by_combo
                .iter()
                .filter(|(_, acts)| acts.iter().enumerate().any(|(i, a)| acts[i + 1..].iter().any(|b| super::shared_place(a, b).is_some() && super::resolved(a, b).is_none())))
                .map(|(c, acts)| format!("{c} -> {}", acts.join(", ")))
                .collect();
            assert!(clashes.is_empty(), "{preset:?}: {clashes:#?}");
        }
    }

    #[test]
    fn switching_preset_keeps_the_users_changes() {
        let mut kb = KeyBindings::defaults();
        kb.bindings.insert("add_marker".into(), vec![KeyCombo::new("F7")]);
        kb.set_preset(KeymapPreset::Premiere);
        assert_eq!(kb.preset, KeymapPreset::Premiere);
        assert_eq!(kb.bindings["add_marker"], vec![KeyCombo::new("F7")]);
        assert_eq!(kb.overrides().len(), 1);
        kb.reset_to_preset();
        assert!(kb.overrides().is_empty());
        assert_eq!(kb.bindings, KeymapPreset::Premiere.bindings());
    }

    #[test]
    fn the_file_holds_the_preset_and_only_the_changes() {
        let mut kb = KeyBindings::from_preset(KeymapPreset::Resolve);
        kb.bindings.insert("add_marker".into(), vec![KeyCombo::new("F7")]);
        let json = serde_json::to_string(&kb.to_file()).unwrap();
        let file: KeymapFile = serde_json::from_str(&json).unwrap();
        assert_eq!(file.preset, KeymapPreset::Resolve);
        assert_eq!(file.bindings.len(), 1);
        assert_eq!(KeyBindings::from_file(file).bindings, kb.bindings);
    }

    /// Files written before presets hold the whole map and no preset.
    /// They load as Klipp, and only real changes count as changes, so a
    /// later switch of preset is not pinned to Klipp's old keys.
    #[test]
    fn a_file_from_before_presets_loads_as_klipp_with_its_changes() {
        let mut full = KeyBindings::defaults().bindings;
        full.insert("add_marker".into(), vec![KeyCombo::new("F7")]);
        let json = serde_json::json!({ "bindings": full }).to_string();
        let kb = KeyBindings::from_file(serde_json::from_str(&json).unwrap());
        assert_eq!(kb.preset, KeymapPreset::Klipp);
        assert_eq!(kb.overrides().keys().collect::<Vec<_>>(), ["add_marker"]);
    }
}

#[cfg(test)]
mod displaced_report {
    use super::KeymapPreset;

    /// Prints what each preset leaves unbound, for review:
    /// `cargo test -p klipp-ui --lib displaced_report -- --nocapture`.
    #[test]
    fn klipp_keeps_every_key_of_its_own() {
        for p in KeymapPreset::ALL {
            println!("{:?} ({} mapped): {:?}", p, p.mapped_count(), p.displaced());
        }
        assert!(KeymapPreset::Klipp.displaced().is_empty());
    }
}

#[cfg(test)]
mod shifted_punctuation {
    use super::{key_event_to_combo, KeyCombo};

    /// One frame with Ctrl+Shift+\ held, as a US keyboard reports it:
    /// the logical key is `|`, the physical key is `\`.
    fn input() -> egui::InputState {
        let ctx = egui::Context::default();
        let modifiers = egui::Modifiers { ctrl: true, shift: true, command: true, ..Default::default() };
        let raw = egui::RawInput {
            modifiers,
            events: vec![egui::Event::Key {
                key: egui::Key::Pipe,
                physical_key: Some(egui::Key::Backslash),
                pressed: true,
                repeat: false,
                modifiers,
            }],
            ..Default::default()
        };
        let _ = ctx.run(raw, |_| {});
        ctx.input(|i| i.clone())
    }

    #[test]
    fn a_shifted_punctuation_combo_fires_and_can_be_captured() {
        let i = input();
        let combo = KeyCombo::parse("Ctrl+Shift+\\").unwrap();
        assert!(combo.is_pressed(&i));
        assert_eq!(key_event_to_combo(&i), Some(combo));
        assert!(!KeyCombo::parse("Ctrl+\\").unwrap().is_pressed(&i), "Shift must still count");
    }
}

#[cfg(test)]
mod clipboard_events {
    use super::{key_event_to_combo, KeyCombo};

    /// One frame holding `event` with `modifiers`, as the window layer
    /// reports Ctrl+X/C/V: the event alone, no key press.
    fn input(modifiers: egui::Modifiers, event: egui::Event) -> egui::InputState {
        let ctx = egui::Context::default();
        let raw = egui::RawInput { modifiers, events: vec![event], ..Default::default() };
        let _ = ctx.run(raw, |_| {});
        ctx.input(|i| i.clone())
    }

    const CTRL: egui::Modifiers = egui::Modifiers { ctrl: true, command: true, alt: false, shift: false, mac_cmd: false };
    const CTRL_SHIFT: egui::Modifiers = egui::Modifiers { ctrl: true, command: true, alt: false, shift: true, mac_cmd: false };

    #[test]
    fn ctrl_x_c_v_fire_from_the_clipboard_events() {
        for (event, combo) in [
            (egui::Event::Cut, "Ctrl+X"),
            (egui::Event::Copy, "Ctrl+C"),
            (egui::Event::Paste("text".into()), "Ctrl+V"),
        ] {
            let i = input(CTRL, event);
            let combo = KeyCombo::parse(combo).unwrap();
            assert!(combo.is_pressed(&i), "{combo:?}");
            assert_eq!(key_event_to_combo(&i), Some(combo));
        }
    }

    #[test]
    fn shift_still_counts_and_other_keys_stay_quiet() {
        let i = input(CTRL_SHIFT, egui::Event::Copy);
        assert!(KeyCombo::parse("Ctrl+Shift+C").unwrap().is_pressed(&i));
        assert!(!KeyCombo::parse("Ctrl+C").unwrap().is_pressed(&i));
        assert!(!KeyCombo::parse("Ctrl+Shift+V").unwrap().is_pressed(&i));
        assert!(!KeyCombo::parse("C").unwrap().is_pressed(&input(Default::default(), egui::Event::Copy)));
    }
}

#[cfg(test)]
mod category_coverage {
    use super::KeyBindings;

    /// Every bound action belongs to a category. The categories are
    /// what Preferences → Keyboard lists and what the shortcut page in
    /// the manual is generated from, so an action missing here is an
    /// action nobody can find.
    #[test]
    fn every_action_is_in_a_category() {
        let kb = KeyBindings::defaults();
        let listed: std::collections::BTreeSet<&str> = KeyBindings::categories()
            .iter()
            .flat_map(|(_, actions)| actions.iter().copied())
            .collect();
        let orphans: Vec<&String> = kb.bindings.keys().filter(|a| !listed.contains(a.as_str())).collect();
        assert!(orphans.is_empty(), "actions in no category: {orphans:?}");
    }
}
