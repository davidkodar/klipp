//! Ctrl+K command palette — single source of truth for the
//! searchable list of user-facing actions.
//!
//! ## Adding a new entry
//!
//! When you ship a new user-facing `AppCommand` variant or a new
//! state-toggle that has a menu / shortcut, add it here. The palette
//! is the discovery surface — if it's not in the palette, users who
//! haven't memorised the shortcut won't find it. The
//! `palette_anchors_present` test pins the high-value entries so
//! removing them by accident fails the build.
//!
//! ## Action types
//!
//! - [`PaletteAction::Cmd`] — push an `AppCommand` to the bus. The
//!   normal path: most editing operations go here.
//! - [`PaletteAction::ToggleState`] — flip a `bool` field on
//!   `AppState`. For panel-visibility toggles + UI-only feature
//!   flags that don't have a dedicated `AppCommand` variant.
//! - [`PaletteAction::OpenDialog`] — surface a system dialog
//!   (rfd file picker) that the palette itself triggers + dispatches
//!   the resulting AppCommand on success. Avoids the legacy
//!   "palette item is `None` and silently does nothing" trap.

use klipp_core::AppCommand;

use crate::state::AppState;

/// What to do when the user activates a palette entry.
pub enum PaletteAction {
    /// Push this command to the app's command bus. Normal case.
    Cmd(AppCommand),
    /// Flip a bool field on `AppState`. The fn pointer reads the
    /// current value, negates it, writes it back. Used for panel
    /// visibility toggles + a handful of UI-only feature flags
    /// (false-color, zebras, focus-peaking) that don't have
    /// dedicated AppCommand variants.
    ToggleState(fn(&mut AppState)),
    /// Set a bool field on `AppState` to a fixed value (without
    /// reading the previous value). Used when the palette entry
    /// represents an "open" / "show" verb rather than a toggle.
    SetState(fn(&mut AppState)),
    /// Open a file picker / dialog. The `kind` chooses which one;
    /// the palette execution code handles the rfd call inline so
    /// the palette stays self-contained.
    OpenDialog(DialogKind),
    /// Escape hatch for entries that need both `&mut AppState` AND
    /// the command bus. Used sparingly — most entries should fit
    /// `Cmd` / `ToggleState` / `SetState`. Currently used for
    /// "Split at Playhead", which has to walk every track at the
    /// playhead and push one `BladeClip` per hit.
    Custom(fn(&mut AppState, &mut Vec<AppCommand>)),
}

/// Discrete file-picker / system-dialog flavours the palette can
/// trigger directly. Each variant maps to one rfd `FileDialog` call
/// + a follow-up `AppCommand` on success.
#[derive(Debug, Clone, Copy)]
pub enum DialogKind {
    OpenProject,
    SaveProjectAs,
    ImportMedia,
    ImportSubtitles,
    ExportSubtitles,
    ExportFcpxml,
    ExportEdl,
    ExportKdenlive,
    ImportFcpxml,
    ImportKdenlive,
    ImportOtio,
}

pub struct PaletteEntry {
    pub label: &'static str,
    pub shortcut: &'static str,
    pub action: PaletteAction,
    /// When false the entry renders dimmed and ignores activation.
    /// Computed at build time from `AppState` — context-dependent
    /// entries (e.g. "Cut Clip" requires a selection) gate here.
    pub enabled: bool,
}

/// Build the full palette entry list against the current state.
/// Called per palette frame so entries reflect live enable/disable
/// gates (selected clip, source loaded, history depth, etc.).
pub fn build(state: &AppState) -> Vec<PaletteEntry> {
    let mut e = Vec::with_capacity(96);
    let has_clip = state.selected_clip.is_some();
    let has_source = state.source_media_id.is_some();
    let has_clipboard = state.clipboard.is_some();
    let has_attr_clipboard = state.attributes_clipboard.is_some();
    let can_undo = state.history.can_undo();
    let can_redo = state.history.can_redo();
    let has_in = state.sequence().in_point.is_some();
    let has_out = state.sequence().out_point.is_some();

    // ── File ────────────────────────────────────────────────
    e.push(PaletteEntry { label: "New Project", shortcut: "Ctrl+N",
        action: PaletteAction::Cmd(AppCommand::NewProject), enabled: true });
    e.push(PaletteEntry { label: "Open Project…", shortcut: "Ctrl+O",
        action: PaletteAction::OpenDialog(DialogKind::OpenProject), enabled: true });
    e.push(PaletteEntry { label: "Save Project", shortcut: "Ctrl+S",
        action: PaletteAction::Cmd(AppCommand::SaveProject), enabled: true });
    e.push(PaletteEntry { label: "Save Project As…", shortcut: "Ctrl+Shift+S",
        action: PaletteAction::OpenDialog(DialogKind::SaveProjectAs), enabled: true });
    e.push(PaletteEntry { label: "Close Project", shortcut: "Ctrl+W",
        action: PaletteAction::Cmd(AppCommand::CloseProject), enabled: true });
    e.push(PaletteEntry { label: "Project Properties…", shortcut: "",
        action: PaletteAction::SetState(|s| s.show_project_info = true), enabled: true });
    e.push(PaletteEntry { label: "Import Media…", shortcut: "Ctrl+I",
        action: PaletteAction::OpenDialog(DialogKind::ImportMedia), enabled: true });
    e.push(PaletteEntry { label: "Import Subtitles…", shortcut: "",
        action: PaletteAction::OpenDialog(DialogKind::ImportSubtitles), enabled: true });
    e.push(PaletteEntry { label: "Import FCPXML…", shortcut: "",
        action: PaletteAction::OpenDialog(DialogKind::ImportFcpxml), enabled: true });
    e.push(PaletteEntry { label: "Import Kdenlive Project…", shortcut: "",
        action: PaletteAction::OpenDialog(DialogKind::ImportKdenlive), enabled: true });
    e.push(PaletteEntry { label: "Import OpenTimelineIO (.otio)…", shortcut: "",
        action: PaletteAction::OpenDialog(DialogKind::ImportOtio), enabled: true });
    e.push(PaletteEntry { label: "Export…", shortcut: "Ctrl+E",
        action: PaletteAction::Cmd(AppCommand::ShowExportDialog), enabled: true });
    e.push(PaletteEntry { label: "Export Subtitles…", shortcut: "",
        action: PaletteAction::OpenDialog(DialogKind::ExportSubtitles), enabled: true });
    e.push(PaletteEntry { label: "Export FCPXML…", shortcut: "",
        action: PaletteAction::OpenDialog(DialogKind::ExportFcpxml), enabled: true });
    e.push(PaletteEntry { label: "Export EDL…", shortcut: "",
        action: PaletteAction::OpenDialog(DialogKind::ExportEdl), enabled: true });
    e.push(PaletteEntry { label: "Export Kdenlive Project…", shortcut: "",
        action: PaletteAction::OpenDialog(DialogKind::ExportKdenlive), enabled: true });
    e.push(PaletteEntry { label: "Render Queue", shortcut: "",
        action: PaletteAction::Cmd(AppCommand::ShowRenderQueue), enabled: true });
    e.push(PaletteEntry { label: "Relink Media…", shortcut: "",
        action: PaletteAction::Cmd(AppCommand::ShowRelinkDialog), enabled: true });
    e.push(PaletteEntry { label: "Preferences…", shortcut: "Ctrl+,",
        action: PaletteAction::Cmd(AppCommand::ShowPreferences), enabled: true });
    e.push(PaletteEntry { label: "Quit", shortcut: "Ctrl+Q",
        action: PaletteAction::Cmd(AppCommand::Quit), enabled: true });

    // ── Edit ────────────────────────────────────────────────
    e.push(PaletteEntry { label: "Undo", shortcut: "Ctrl+Z",
        action: PaletteAction::Cmd(AppCommand::Undo), enabled: can_undo });
    e.push(PaletteEntry { label: "Redo", shortcut: "Ctrl+Shift+Z",
        action: PaletteAction::Cmd(AppCommand::Redo), enabled: can_redo });
    if let Some(cid) = state.selected_clip {
        e.push(PaletteEntry { label: "Cut Clip", shortcut: "Ctrl+X",
            action: PaletteAction::Cmd(AppCommand::CutClip { clip_id: cid }), enabled: true });
        e.push(PaletteEntry { label: "Copy Clip", shortcut: "Ctrl+C",
            action: PaletteAction::Cmd(AppCommand::CopyClip { clip_id: cid }), enabled: true });
        e.push(PaletteEntry { label: "Duplicate Clip", shortcut: "Ctrl+D",
            action: PaletteAction::Cmd(AppCommand::DuplicateClip { clip_id: cid }), enabled: true });
        e.push(PaletteEntry { label: "Copy Attributes", shortcut: "Ctrl+Shift+C",
            action: PaletteAction::Cmd(AppCommand::CopyAttributes { clip_id: cid }), enabled: true });
        e.push(PaletteEntry { label: "Delete Clip", shortcut: "Del",
            action: PaletteAction::Cmd(AppCommand::RemoveClip { clip_id: cid }), enabled: true });
    } else {
        e.push(PaletteEntry { label: "Cut Clip", shortcut: "Ctrl+X",
            action: PaletteAction::Cmd(AppCommand::Undo), enabled: false }); // dummy cmd, gated off
        e.push(PaletteEntry { label: "Copy Clip", shortcut: "Ctrl+C",
            action: PaletteAction::Cmd(AppCommand::Undo), enabled: false });
        e.push(PaletteEntry { label: "Duplicate Clip", shortcut: "Ctrl+D",
            action: PaletteAction::Cmd(AppCommand::Undo), enabled: false });
        e.push(PaletteEntry { label: "Delete Clip", shortcut: "Del",
            action: PaletteAction::Cmd(AppCommand::Undo), enabled: false });
    }
    e.push(PaletteEntry { label: "Paste", shortcut: "Ctrl+V",
        action: PaletteAction::Cmd(AppCommand::PasteClip), enabled: has_clipboard });
    // Paste Attributes opens the attribute-picker dialog rather
    // than dispatching a one-shot AppCommand — the inline state
    // flag matches what the menu bar entry does.
    e.push(PaletteEntry { label: "Paste Attributes…", shortcut: "Ctrl+Shift+V",
        action: PaletteAction::SetState(|s| s.show_paste_attributes_dialog = true),
        enabled: has_attr_clipboard && has_clip });
    e.push(PaletteEntry { label: "Select All", shortcut: "Ctrl+A",
        action: PaletteAction::Cmd(AppCommand::SelectAllClips), enabled: true });
    e.push(PaletteEntry { label: "Select Forward", shortcut: "",
        action: PaletteAction::Cmd(AppCommand::SelectClipsForward), enabled: true });
    e.push(PaletteEntry { label: "Select Backward", shortcut: "",
        action: PaletteAction::Cmd(AppCommand::SelectClipsBackward), enabled: true });
    e.push(PaletteEntry { label: "Deselect All", shortcut: "Ctrl+Shift+A",
        action: PaletteAction::Cmd(AppCommand::SelectClip(None)), enabled: true });
    if !state.selected_clips.is_empty() {
        let cids: Vec<_> = state.selected_clips.iter().copied().collect();
        e.push(PaletteEntry { label: "Group Clips", shortcut: "Ctrl+G",
            action: PaletteAction::Cmd(AppCommand::GroupClips { clip_ids: cids }),
            enabled: state.selected_clips.len() >= 2 });
    } else {
        e.push(PaletteEntry { label: "Group Clips", shortcut: "Ctrl+G",
            action: PaletteAction::Cmd(AppCommand::Undo), enabled: false });
    }
    if let Some(cid) = state.selected_clip {
        e.push(PaletteEntry { label: "Ungroup Clip", shortcut: "Ctrl+Shift+G",
            action: PaletteAction::Cmd(AppCommand::UngroupClip { clip_id: cid }),
            enabled: true });
    } else {
        e.push(PaletteEntry { label: "Ungroup Clip", shortcut: "Ctrl+Shift+G",
            action: PaletteAction::Cmd(AppCommand::Undo), enabled: false });
    }

    // ── Timeline editing ────────────────────────────────────
    e.push(PaletteEntry { label: "Insert Edit", shortcut: "W",
        action: PaletteAction::Cmd(AppCommand::InsertEdit), enabled: has_source });
    e.push(PaletteEntry { label: "Overwrite Edit", shortcut: "D",
        action: PaletteAction::Cmd(AppCommand::OverwriteEdit), enabled: has_source });
    e.push(PaletteEntry { label: "Lift Edit", shortcut: ";",
        action: PaletteAction::Cmd(AppCommand::LiftEdit), enabled: has_in && has_out });
    e.push(PaletteEntry { label: "Extract Edit", shortcut: "'",
        action: PaletteAction::Cmd(AppCommand::ExtractEdit), enabled: has_in && has_out });
    // Split at Playhead walks every track + clip under the playhead
    // and pushes one BladeClip per hit; this matches the toolbar's
    // Clip → Split at Playhead seam exactly.
    e.push(PaletteEntry { label: "Split at Playhead", shortcut: "S",
        action: PaletteAction::Custom(|state, cmds| {
            let ph = state.playhead;
            for (ti, t) in state.sequence().tracks.iter().enumerate() {
                if let Some((_, c)) = t.clip_at_timecode(ph) {
                    cmds.push(AppCommand::BladeClip {
                        track_index: ti, clip_id: c.id, cut_at: ph,
                    });
                }
            }
        }),
        enabled: true });
    e.push(PaletteEntry { label: "Add Adjustment Layer", shortcut: "",
        action: PaletteAction::Cmd(AppCommand::CreateAdjustmentLayer), enabled: true });
    e.push(PaletteEntry { label: "Create Multicam Clip", shortcut: "",
        action: PaletteAction::Cmd(AppCommand::CreateMulticamClip), enabled: true });
    e.push(PaletteEntry { label: "Add Video Track", shortcut: "",
        action: PaletteAction::Cmd(AppCommand::AddVideoTrack), enabled: true });
    e.push(PaletteEntry { label: "Add Audio Track", shortcut: "",
        action: PaletteAction::Cmd(AppCommand::AddAudioTrack), enabled: true });

    // ── Mark / Navigation ───────────────────────────────────
    e.push(PaletteEntry { label: "Add Marker", shortcut: "M",
        action: PaletteAction::Cmd(AppCommand::AddMarker {
            position: state.playhead, label: String::new(),
            color: klipp_core::MarkerColor::Blue }), enabled: true });
    e.push(PaletteEntry { label: "Set In Point", shortcut: "I",
        action: PaletteAction::Cmd(AppCommand::SetSequenceInPoint(state.playhead)), enabled: true });
    e.push(PaletteEntry { label: "Set Out Point", shortcut: "O",
        action: PaletteAction::Cmd(AppCommand::SetSequenceOutPoint(state.playhead)), enabled: true });
    e.push(PaletteEntry { label: "Clear In Point", shortcut: "Alt+I",
        action: PaletteAction::Cmd(AppCommand::ClearSequenceInPoint), enabled: has_in });
    e.push(PaletteEntry { label: "Clear Out Point", shortcut: "Alt+O",
        action: PaletteAction::Cmd(AppCommand::ClearSequenceOutPoint), enabled: has_out });
    e.push(PaletteEntry { label: "Go to Start", shortcut: "Home",
        action: PaletteAction::Cmd(AppCommand::SeekTo(klipp_core::RationalTime::from_frames(0, state.fps.rate()))), enabled: true });
    e.push(PaletteEntry { label: "Go to End", shortcut: "End",
        action: PaletteAction::Cmd(AppCommand::SeekTo(state.sequence_duration())), enabled: true });
    e.push(PaletteEntry { label: "Next Edit Point", shortcut: "↓",
        action: PaletteAction::Cmd(AppCommand::NextEditPoint), enabled: true });
    e.push(PaletteEntry { label: "Previous Edit Point", shortcut: "↑",
        action: PaletteAction::Cmd(AppCommand::PrevEditPoint), enabled: true });

    // ── Match Frame ─────────────────────────────────────────
    e.push(PaletteEntry { label: "Match Frame", shortcut: "F",
        action: PaletteAction::Cmd(AppCommand::MatchFrameProgramToSource), enabled: true });
    e.push(PaletteEntry { label: "Match Frame Reverse", shortcut: "Shift+F",
        action: PaletteAction::Cmd(AppCommand::MatchFrameSourceToProgram), enabled: has_source });

    // ── View / Workspace ────────────────────────────────────
    e.push(PaletteEntry { label: "Fit Timeline to Window", shortcut: "Shift+Z",
        action: PaletteAction::Cmd(AppCommand::FitTimelineToWindow), enabled: true });
    e.push(PaletteEntry { label: "Toggle Loop Playback", shortcut: "Ctrl+L",
        action: PaletteAction::Cmd(AppCommand::ToggleLoopPlayback), enabled: true });
    e.push(PaletteEntry { label: "Toggle Snap", shortcut: "N",
        action: PaletteAction::ToggleState(|s| s.snap_enabled = !s.snap_enabled), enabled: true });
    e.push(PaletteEntry { label: "Toggle Skimming", shortcut: "S",
        action: PaletteAction::ToggleState(|s| s.skimming_enabled = !s.skimming_enabled), enabled: true });
    e.push(PaletteEntry { label: "Toggle Color Panel", shortcut: "C",
        action: PaletteAction::Cmd(AppCommand::ToggleColorPanel), enabled: true });
    e.push(PaletteEntry { label: "Toggle Scopes", shortcut: "V",
        action: PaletteAction::ToggleState(|s| s.show_scopes = !s.show_scopes), enabled: true });
    e.push(PaletteEntry { label: "Toggle Inspector Panel", shortcut: "Ctrl+Shift+I",
        action: PaletteAction::ToggleState(|s| s.show_inspector_panel = !s.show_inspector_panel), enabled: true });
    e.push(PaletteEntry { label: "Toggle Effects Browser", shortcut: "Ctrl+Shift+E",
        action: PaletteAction::ToggleState(|s| s.show_browser_panel = !s.show_browser_panel), enabled: true });
    e.push(PaletteEntry { label: "Toggle Audio Mixer", shortcut: "Ctrl+Shift+M",
        action: PaletteAction::ToggleState(|s| s.show_audio_mixer = !s.show_audio_mixer), enabled: true });
    e.push(PaletteEntry { label: "Toggle Curve Editor", shortcut: "Shift+K",
        action: PaletteAction::ToggleState(|s| s.show_curve_editor = !s.show_curve_editor), enabled: true });
    e.push(PaletteEntry { label: "Toggle Markers Panel", shortcut: "",
        action: PaletteAction::ToggleState(|s| s.show_markers_panel = !s.show_markers_panel), enabled: true });
    e.push(PaletteEntry { label: "Toggle History Panel", shortcut: "",
        action: PaletteAction::ToggleState(|s| s.show_history_panel = !s.show_history_panel), enabled: true });
    e.push(PaletteEntry { label: "Toggle Subtitle Editor", shortcut: "Ctrl+Shift+T",
        action: PaletteAction::ToggleState(|s| s.show_subtitle_editor = !s.show_subtitle_editor), enabled: true });
    e.push(PaletteEntry { label: "Toggle Performance Overlay", shortcut: "P",
        action: PaletteAction::ToggleState(|s| s.show_perf_overlay = !s.show_perf_overlay), enabled: true });
    e.push(PaletteEntry { label: "Toggle Zebras", shortcut: "Z",
        action: PaletteAction::ToggleState(|s| s.viewer_overlays.zebras = !s.viewer_overlays.zebras),
        enabled: true });
    e.push(PaletteEntry { label: "Toggle False Color", shortcut: "Alt+F",
        action: PaletteAction::ToggleState(|s| s.viewer_overlays.false_color = !s.viewer_overlays.false_color),
        enabled: true });
    e.push(PaletteEntry { label: "Toggle Focus Peaking", shortcut: "Ctrl+P",
        action: PaletteAction::ToggleState(|s| s.viewer_overlays.focus_peaking = !s.viewer_overlays.focus_peaking),
        enabled: true });
    e.push(PaletteEntry { label: "Toggle Safe Areas", shortcut: "Alt+S",
        action: PaletteAction::ToggleState(|s| s.viewer_overlays.safe_areas = !s.viewer_overlays.safe_areas),
        enabled: true });
    e.push(PaletteEntry { label: "Toggle Grid Overlay", shortcut: "Alt+G",
        action: PaletteAction::ToggleState(|s| s.viewer_overlays.grid = !s.viewer_overlays.grid),
        enabled: true });

    // ── Capture ─────────────────────────────────────────────
    e.push(PaletteEntry { label: "Frame Grab (Save PNG)", shortcut: "Ctrl+Alt+S",
        action: PaletteAction::Cmd(AppCommand::FrameGrab), enabled: true });
    e.push(PaletteEntry { label: "Extract Frame to Bin", shortcut: "Ctrl+Alt+B",
        action: PaletteAction::Cmd(AppCommand::ExtractFrameToBin), enabled: true });

    e
}

/// Resolve a [`DialogKind`] into an `AppCommand` by surfacing the
/// matching system file picker. Blocks (modal) until the user picks
/// or cancels — same behaviour as the menu-bar entries that opened
/// these dialogs before. Returns `None` on cancel; the palette
/// callers push the command on Some.
pub fn dispatch_dialog(kind: DialogKind) -> Option<AppCommand> {
    match kind {
        DialogKind::OpenProject => rfd::FileDialog::new()
            .set_title("Open Project")
            .pick_folder()
            .map(|p| AppCommand::OpenProject { path: p }),
        DialogKind::SaveProjectAs => rfd::FileDialog::new()
            .set_title("Save Project As")
            .add_filter("Klipp Project", &["klipp"])
            .set_file_name("project.klipp")
            .save_file()
            .map(|p| AppCommand::SaveProjectAs { path: p }),
        DialogKind::ImportMedia => rfd::FileDialog::new()
            .set_title("Import Media")
            .pick_file()
            .map(|p| AppCommand::ImportMedia { path: p }),
        DialogKind::ImportSubtitles => rfd::FileDialog::new()
            .set_title("Import Subtitles")
            .add_filter("Subtitles", &["srt", "vtt", "ass", "ssa"])
            .pick_file()
            .map(|p| AppCommand::ImportSubtitles { path: p }),
        DialogKind::ExportSubtitles => rfd::FileDialog::new()
            .set_title("Export Subtitles")
            .add_filter("SRT", &["srt"])
            .add_filter("WebVTT", &["vtt"])
            .add_filter("ASS", &["ass"])
            .add_filter("SSA", &["ssa"])
            .set_file_name("subtitles.srt")
            .save_file()
            .map(|p| AppCommand::ExportSubtitles { path: p }),
        DialogKind::ExportFcpxml => rfd::FileDialog::new()
            .set_title("Export FCPXML")
            .add_filter("FCPXML", &["fcpxml"])
            .set_file_name("project.fcpxml")
            .save_file()
            .map(|p| AppCommand::ExportFcpxml { path: p }),
        DialogKind::ExportEdl => rfd::FileDialog::new()
            .set_title("Export EDL")
            .add_filter("EDL", &["edl"])
            .set_file_name("project.edl")
            .save_file()
            .map(|p| AppCommand::ExportEdl { path: p }),
        DialogKind::ExportKdenlive => rfd::FileDialog::new()
            .set_title("Export Kdenlive Project")
            .add_filter("Kdenlive Project", &["kdenlive"])
            .set_file_name("project.kdenlive")
            .save_file()
            .map(|p| AppCommand::ExportKdenlive { path: p }),
        DialogKind::ImportFcpxml => rfd::FileDialog::new()
            .set_title("Import FCPXML")
            .add_filter("FCPXML", &["fcpxml"])
            .pick_file()
            .map(|p| AppCommand::ImportFcpxml { path: p }),
        DialogKind::ImportKdenlive => rfd::FileDialog::new()
            .set_title("Import Kdenlive Project")
            .add_filter("Kdenlive Project", &["kdenlive"])
            .pick_file()
            .map(|p| AppCommand::ImportKdenlive { path: p }),
        DialogKind::ImportOtio => rfd::FileDialog::new()
            .set_title("Import OpenTimelineIO")
            .add_filter("OpenTimelineIO", &["otio"])
            .pick_file()
            .map(|p| AppCommand::ImportOtio { path: p }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;

    /// High-value commands that MUST appear in the palette. If you
    /// remove an entry from `build()`, this test catches it. Add to
    /// this list when shipping a new must-discover feature; remove
    /// only when intentionally retiring one.
    ///
    /// The labels are matched substring-insensitively so a wording
    /// nudge ("Match Frame" → "Match Frame to Source") doesn't break
    /// the test — the test pins the *concept*, not the exact string.
    const ANCHOR_LABELS: &[&str] = &[
        "Save Project",
        "Open Project",
        "Undo",
        "Redo",
        "Cut Clip",
        "Copy Clip",
        "Paste",
        "Duplicate Clip",
        "Select All",
        "Match Frame",
        "Set In Point",
        "Set Out Point",
        "Add Marker",
        "Toggle Snap",
        "Frame Grab",
        "Export",
        "Import Media",
        "Preferences",
        "Quit",
    ];

    #[test]
    fn palette_anchors_present() {
        let state = AppState::default();
        let entries = build(&state);
        let labels: Vec<&str> = entries.iter().map(|e| e.label).collect();
        for anchor in ANCHOR_LABELS {
            let found = labels.iter().any(|l| l.contains(anchor));
            assert!(
                found,
                "command palette is missing anchor '{anchor}'. \
                 If you intentionally removed it, remove it from \
                 ANCHOR_LABELS too. Current entries: {labels:?}",
            );
        }
    }

    #[test]
    fn palette_has_substantial_entry_count() {
        // The palette is the discovery surface for keyboard-shy
        // users. It should expose every menu-bar action and every
        // keyboard-bound command. 50 is the floor; today's list is
        // ~70. If this fails, a section probably got commented out.
        let state = AppState::default();
        let entries = build(&state);
        assert!(
            entries.len() >= 50,
            "command palette has only {} entries — expected at least 50. \
             Check that no section is unintentionally short-circuited.",
            entries.len(),
        );
    }

    #[test]
    fn no_duplicate_labels() {
        // Duplicate labels are confusing in the filtered list and
        // usually mean the same command was added twice. Catches
        // copy-paste mistakes.
        let state = AppState::default();
        let entries = build(&state);
        let mut seen = std::collections::HashSet::new();
        for e in &entries {
            assert!(seen.insert(e.label),
                "duplicate palette label: {:?}", e.label);
        }
    }
}
