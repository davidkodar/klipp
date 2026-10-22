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
//!
//! ## AppState assumptions (integrate-time)
//!
//! The AI Tools palette entries assume the following `AppState`
//! shape — added in a follow-up integration commit, not in this file:
//!
//! - `state.ai_tools_state.panel_visible: bool` — toggled by the
//!   `Toggle AI Tools Panel` entry.
//!
//! Until that field exists the `ToggleState` closure here will fail
//! to compile, which is the desired tripwire — the integration PR
//! must wire the state field at the same time it lands the panel UI.

use std::borrow::Cow;

use klipp_core::AppCommand;
use klipp_core::events::AiToolsTaskTag;

use crate::state::AppState;

/// What to do when the user activates a palette entry.
// 224 bytes, carried by the AppCommand variant. The palette rebuilds its
// entry list as the user types, so boxing would trade a few kilobytes of
// stack for one heap allocation per entry per keystroke.
#[allow(clippy::large_enum_variant)]
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
    ExportOtio,
    ExportAaf,
    ExportKdenlive,
    ImportFcpxml,
    OpenFromAnotherEditor,
    ImportKdenlive,
    ImportOtio,
}

pub struct PaletteEntry {
    /// Cow lets static literals (the bulk of entries) cost zero
    /// allocation while still permitting dynamic labels — e.g. the
    /// per-model "Download AI Model: SAM2.1 Tiny" entries that are
    /// generated from the manifest at build time.
    pub label: Cow<'static, str>,
    pub shortcut: Cow<'static, str>,
    pub action: PaletteAction,
    /// When false the entry renders dimmed and ignores activation.
    /// Computed at build time from `AppState` — context-dependent
    /// entries (e.g. "Cut Clip" requires a selection) gate here.
    pub enabled: bool,
}

/// Synthesise the palette form of a cut-template's bare canonical
/// label. The right-click context menu shows the bare form (it's
/// already nested under "Cut Templates"); the palette is a flat
/// searchable list and needs the verb prefix so users typing "apply"
/// find the templates.
fn template_palette_label(bare: &str) -> Cow<'static, str> {
    Cow::Owned(format!("Apply template → {bare}"))
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
    let can_undo = state.next_undo().is_some();
    let can_redo = state.next_redo().is_some();
    let has_in = state.sequence().in_point.is_some();
    let has_out = state.sequence().out_point.is_some();

    // ── File ────────────────────────────────────────────────
    e.push(PaletteEntry { label: Cow::Borrowed("New Project"), shortcut: Cow::Borrowed("Ctrl+N"),
        action: PaletteAction::Cmd(AppCommand::NewProject), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Open Project…"), shortcut: Cow::Borrowed("Ctrl+O"),
        action: PaletteAction::OpenDialog(DialogKind::OpenProject), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Open Project from Another Editor…"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::OpenDialog(DialogKind::OpenFromAnotherEditor), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Save Project"), shortcut: Cow::Borrowed("Ctrl+S"),
        action: PaletteAction::Cmd(AppCommand::SaveProject), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Save Project As…"), shortcut: Cow::Borrowed("Ctrl+Shift+S"),
        action: PaletteAction::OpenDialog(DialogKind::SaveProjectAs), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Close Project"), shortcut: Cow::Borrowed("Ctrl+W"),
        action: PaletteAction::Cmd(AppCommand::CloseProject), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Project Properties…"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::SetState(|s| s.show_project_info = true), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Colour Management…"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::SetState(|s| s.show_colour_management = true), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Import Media…"), shortcut: Cow::Borrowed("Ctrl+I"),
        action: PaletteAction::OpenDialog(DialogKind::ImportMedia), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Import Subtitles…"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::OpenDialog(DialogKind::ImportSubtitles), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Import FCPXML…"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::OpenDialog(DialogKind::ImportFcpxml), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Import Kdenlive Project…"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::OpenDialog(DialogKind::ImportKdenlive), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Import OpenTimelineIO (.otio)…"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::OpenDialog(DialogKind::ImportOtio), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Export…"), shortcut: Cow::Borrowed("Ctrl+E"),
        action: PaletteAction::Cmd(AppCommand::ShowExportDialog), enabled: true });
    // Per-clip export — opens the dialog with range_mode pre-set to
    // "Selected clips" so the right edit lands without the user
    // having to hunt for the radio. Gated on having any selection;
    // a 0-clip dispatch toasts and bails.
    {
        let sel_count = state.selected_clips.len();
        let label_text = if sel_count == 1 {
            "Export Selected Clip\u{2026}".to_string()
        } else {
            format!("Export Selected Clips ({sel_count})\u{2026}")
        };
        e.push(PaletteEntry {
            label: Cow::Owned(label_text),
            shortcut: Cow::Borrowed(""),
            action: PaletteAction::Cmd(AppCommand::OpenExportDialogForSelectedClips),
            enabled: sel_count > 0,
        });
    }
    e.push(PaletteEntry { label: Cow::Borrowed("Export Subtitles…"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::OpenDialog(DialogKind::ExportSubtitles), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Export FCPXML…"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::OpenDialog(DialogKind::ExportFcpxml), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Export EDL…"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::OpenDialog(DialogKind::ExportEdl), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Export OpenTimelineIO (.otio)…"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::OpenDialog(DialogKind::ExportOtio), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Export AAF (via OpenTimelineIO)…"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::OpenDialog(DialogKind::ExportAaf), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Export Kdenlive Project…"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::OpenDialog(DialogKind::ExportKdenlive), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Render Queue"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Cmd(AppCommand::ShowRenderQueue), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Relink Media…"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Cmd(AppCommand::ShowRelinkDialog), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Preferences…"), shortcut: Cow::Borrowed("Ctrl+,"),
        action: PaletteAction::Cmd(AppCommand::ShowPreferences), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Quit"), shortcut: Cow::Borrowed("Ctrl+Q"),
        action: PaletteAction::Cmd(AppCommand::Quit), enabled: true });

    // ── Edit ────────────────────────────────────────────────
    e.push(PaletteEntry { label: Cow::Borrowed("Undo"), shortcut: Cow::Borrowed("Ctrl+Z"),
        action: PaletteAction::Cmd(AppCommand::Undo), enabled: can_undo });
    e.push(PaletteEntry { label: Cow::Borrowed("Redo"), shortcut: Cow::Borrowed("Ctrl+Shift+Z"),
        action: PaletteAction::Cmd(AppCommand::Redo), enabled: can_redo });
    if let Some(cid) = state.selected_clip {
        e.push(PaletteEntry { label: Cow::Borrowed("Cut Clip"), shortcut: Cow::Borrowed("Ctrl+X"),
            action: PaletteAction::Cmd(AppCommand::CutClip { clip_id: cid }), enabled: true });
        e.push(PaletteEntry { label: Cow::Borrowed("Copy Clip"), shortcut: Cow::Borrowed("Ctrl+C"),
            action: PaletteAction::Cmd(AppCommand::CopyClip { clip_id: cid }), enabled: true });
        e.push(PaletteEntry { label: Cow::Borrowed("Duplicate Clip"), shortcut: Cow::Borrowed("Ctrl+D"),
            action: PaletteAction::Cmd(AppCommand::DuplicateClip { clip_id: cid }), enabled: true });
        e.push(PaletteEntry { label: Cow::Borrowed("Copy Attributes"), shortcut: Cow::Borrowed("Ctrl+Shift+C"),
            action: PaletteAction::Cmd(AppCommand::CopyAttributes { clip_id: cid }), enabled: true });
        e.push(PaletteEntry { label: Cow::Borrowed("Delete Clip"), shortcut: Cow::Borrowed("Del"),
            action: PaletteAction::Cmd(AppCommand::RemoveClip { clip_id: cid }), enabled: true });
    } else {
        e.push(PaletteEntry { label: Cow::Borrowed("Cut Clip"), shortcut: Cow::Borrowed("Ctrl+X"),
            action: PaletteAction::Cmd(AppCommand::Undo), enabled: false }); // dummy cmd, gated off
        e.push(PaletteEntry { label: Cow::Borrowed("Copy Clip"), shortcut: Cow::Borrowed("Ctrl+C"),
            action: PaletteAction::Cmd(AppCommand::Undo), enabled: false });
        e.push(PaletteEntry { label: Cow::Borrowed("Duplicate Clip"), shortcut: Cow::Borrowed("Ctrl+D"),
            action: PaletteAction::Cmd(AppCommand::Undo), enabled: false });
        e.push(PaletteEntry { label: Cow::Borrowed("Delete Clip"), shortcut: Cow::Borrowed("Del"),
            action: PaletteAction::Cmd(AppCommand::Undo), enabled: false });
    }
    // Swap Selected Clips — exchange two adjacent clips on the same
    // track. Gates purely on selected_clips.len() == 2; the AppCommand
    // handler runs the full validation (same-track / adjacency /
    // group / linked-partner symmetry) and surfaces failures as
    // toasts. Lives outside the single-selection block because it
    // depends on the multi-select list, not the primary selection.
    let swap_pair: Option<(klipp_core::ClipId, klipp_core::ClipId)> =
        if state.selected_clips.len() == 2 {
            Some((state.selected_clips[0], state.selected_clips[1]))
        } else {
            None
        };
    if let Some((a, b)) = swap_pair {
        e.push(PaletteEntry {
            label: Cow::Borrowed("Swap Selected Clips"),
            shortcut: Cow::Borrowed(""),
            action: PaletteAction::Cmd(AppCommand::SwapAdjacentClips { clip_a: a, clip_b: b }),
            enabled: true,
        });
    } else {
        e.push(PaletteEntry {
            label: Cow::Borrowed("Swap Selected Clips"),
            shortcut: Cow::Borrowed(""),
            action: PaletteAction::Cmd(AppCommand::Undo), // dummy, gated off
            enabled: false,
        });
    }
    e.push(PaletteEntry { label: Cow::Borrowed("Paste"), shortcut: Cow::Borrowed("Ctrl+V"),
        action: PaletteAction::Cmd(AppCommand::PasteClip), enabled: has_clipboard });
    // Paste Attributes opens the attribute-picker dialog rather
    // than dispatching a one-shot AppCommand — the inline state
    // flag matches what the menu bar entry does.
    e.push(PaletteEntry { label: Cow::Borrowed("Paste Attributes…"), shortcut: Cow::Borrowed("Ctrl+Shift+V"),
        action: PaletteAction::SetState(|s| s.show_paste_attributes_dialog = true),
        enabled: has_attr_clipboard && has_clip });
    e.push(PaletteEntry { label: Cow::Borrowed("Select All"), shortcut: Cow::Borrowed("Ctrl+A"),
        action: PaletteAction::Cmd(AppCommand::SelectAllClips), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Select Forward"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Cmd(AppCommand::SelectClipsForward), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Select Backward"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Cmd(AppCommand::SelectClipsBackward), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Deselect All"), shortcut: Cow::Borrowed("Ctrl+Shift+A"),
        action: PaletteAction::Cmd(AppCommand::SelectClip(None)), enabled: true });
    if !state.selected_clips.is_empty() {
        let cids: Vec<_> = state.selected_clips.to_vec();
        e.push(PaletteEntry { label: Cow::Borrowed("Group Clips"), shortcut: Cow::Borrowed("Ctrl+G"),
            action: PaletteAction::Cmd(AppCommand::GroupClips { clip_ids: cids }),
            enabled: state.selected_clips.len() >= 2 });
    } else {
        e.push(PaletteEntry { label: Cow::Borrowed("Group Clips"), shortcut: Cow::Borrowed("Ctrl+G"),
            action: PaletteAction::Cmd(AppCommand::Undo), enabled: false });
    }
    if let Some(cid) = state.selected_clip {
        e.push(PaletteEntry { label: Cow::Borrowed("Ungroup Clip"), shortcut: Cow::Borrowed("Ctrl+Shift+G"),
            action: PaletteAction::Cmd(AppCommand::UngroupClip { clip_id: cid }),
            enabled: true });
    } else {
        e.push(PaletteEntry { label: Cow::Borrowed("Ungroup Clip"), shortcut: Cow::Borrowed("Ctrl+Shift+G"),
            action: PaletteAction::Cmd(AppCommand::Undo), enabled: false });
    }

    // ── Timeline editing ────────────────────────────────────
    e.push(PaletteEntry { label: Cow::Borrowed("Insert Edit"), shortcut: Cow::Borrowed("W"),
        action: PaletteAction::Cmd(AppCommand::InsertEdit), enabled: has_source });
    e.push(PaletteEntry { label: Cow::Borrowed("Overwrite Edit"), shortcut: Cow::Borrowed("D"),
        action: PaletteAction::Cmd(AppCommand::OverwriteEdit), enabled: has_source });
    e.push(PaletteEntry { label: Cow::Borrowed("Lift Edit"), shortcut: Cow::Borrowed(";"),
        action: PaletteAction::Cmd(AppCommand::LiftEdit), enabled: has_in && has_out });
    e.push(PaletteEntry { label: Cow::Borrowed("Extract Edit"), shortcut: Cow::Borrowed("'"),
        action: PaletteAction::Cmd(AppCommand::ExtractEdit), enabled: has_in && has_out });
    // Split at Playhead pushes one BladeClip per clip under the playhead
    // (linked partners ride along with their clip); this matches the
    // toolbar's Clip → Split at Playhead seam exactly.
    e.push(PaletteEntry { label: Cow::Borrowed("Split at Playhead"), shortcut: Cow::Borrowed("S"),
        action: PaletteAction::Custom(|state, cmds| {
            cmds.extend(state.split_at_playhead_commands());
        }),
        enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Add Adjustment Layer"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Cmd(AppCommand::CreateAdjustmentLayer), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Create Multicam Clip"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Cmd(AppCommand::CreateMulticamClip), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Add Video Track"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Cmd(AppCommand::AddVideoTrack), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Add Audio Track"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Cmd(AppCommand::AddAudioTrack), enabled: true });
    // "Insert N seconds of silence" — three common durations, targeting the
    // first audio track so the entry has a single deterministic effect from
    // the palette. Power users with multi-track edits can right-click the
    // track header (later UX) for arbitrary track targeting.
    let first_audio_idx = state.sequences.get(state.active_sequence_idx)
        .and_then(|seq| seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio));
    e.push(PaletteEntry { label: Cow::Borrowed("Insert 1 s Silence on First Audio Track"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Custom(|state, cmds| {
            if let Some(idx) = state.sequences.get(state.active_sequence_idx)
                .and_then(|seq| seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio))
            {
                cmds.push(AppCommand::InsertGapOnTrack { track_idx: idx, duration_secs: 1.0 });
            }
        }),
        enabled: first_audio_idx.is_some() });
    e.push(PaletteEntry { label: Cow::Borrowed("Insert 2 s Silence on First Audio Track"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Custom(|state, cmds| {
            if let Some(idx) = state.sequences.get(state.active_sequence_idx)
                .and_then(|seq| seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio))
            {
                cmds.push(AppCommand::InsertGapOnTrack { track_idx: idx, duration_secs: 2.0 });
            }
        }),
        enabled: first_audio_idx.is_some() });
    e.push(PaletteEntry { label: Cow::Borrowed("Insert 5 s Silence on First Audio Track"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Custom(|state, cmds| {
            if let Some(idx) = state.sequences.get(state.active_sequence_idx)
                .and_then(|seq| seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio))
            {
                cmds.push(AppCommand::InsertGapOnTrack { track_idx: idx, duration_secs: 5.0 });
            }
        }),
        enabled: first_audio_idx.is_some() });

    // ── Mark / Navigation ───────────────────────────────────
    e.push(PaletteEntry { label: Cow::Borrowed("Add Marker"), shortcut: Cow::Borrowed("M"),
        action: PaletteAction::Cmd(AppCommand::AddMarker {
            position: state.playhead, label: String::new(),
            color: klipp_core::MarkerColor::Blue }), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Add Title"), shortcut: Cow::Borrowed("Ctrl+T"),
        action: PaletteAction::Cmd(AppCommand::AddTitleAtPlayhead { over_clip: None }), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Set In Point"), shortcut: Cow::Borrowed("I"),
        action: PaletteAction::Cmd(AppCommand::SetSequenceInPoint(state.playhead)), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Set Out Point"), shortcut: Cow::Borrowed("O"),
        action: PaletteAction::Cmd(AppCommand::SetSequenceOutPoint(state.playhead)), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Clear In Point"), shortcut: Cow::Borrowed("Alt+I"),
        action: PaletteAction::Cmd(AppCommand::ClearSequenceInPoint), enabled: has_in });
    e.push(PaletteEntry { label: Cow::Borrowed("Clear Out Point"), shortcut: Cow::Borrowed("Alt+O"),
        action: PaletteAction::Cmd(AppCommand::ClearSequenceOutPoint), enabled: has_out });
    e.push(PaletteEntry { label: Cow::Borrowed("Go to Start"), shortcut: Cow::Borrowed("Home"),
        action: PaletteAction::Cmd(AppCommand::SeekTo(klipp_core::RationalTime::from_frames(0, state.fps.rate()))), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Go to End"), shortcut: Cow::Borrowed("End"),
        action: PaletteAction::Cmd(AppCommand::SeekTo(state.sequence_duration())), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Next Edit Point"), shortcut: Cow::Borrowed("↓"),
        action: PaletteAction::Cmd(AppCommand::NextEditPoint), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Previous Edit Point"), shortcut: Cow::Borrowed("↑"),
        action: PaletteAction::Cmd(AppCommand::PrevEditPoint), enabled: true });

    // ── Match Frame ─────────────────────────────────────────
    e.push(PaletteEntry { label: Cow::Borrowed("Match Frame"), shortcut: Cow::Borrowed("F"),
        action: PaletteAction::Cmd(AppCommand::MatchFrameProgramToSource), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Match Frame Reverse"), shortcut: Cow::Borrowed("Shift+F"),
        action: PaletteAction::Cmd(AppCommand::MatchFrameSourceToProgram), enabled: has_source });

    // ── View / Workspace ────────────────────────────────────
    e.push(PaletteEntry { label: Cow::Borrowed("Fit Timeline to Window"), shortcut: Cow::Borrowed("Shift+Z"),
        action: PaletteAction::Cmd(AppCommand::FitTimelineToWindow), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Loop Playback"), shortcut: Cow::Borrowed("Ctrl+L"),
        action: PaletteAction::Cmd(AppCommand::ToggleLoopPlayback), enabled: true });
    // Toggle Snap dispatches the master toggle through AppCommand
    // (was a direct ToggleState mutator) so the central handler can
    // persist `state.snap` → `layout.snap` on every flip. The four
    // snap-master surfaces (palette, N shortcut, timeline toolbar,
    // View menu) all route through `AppCommand::SetSnapMaster` after
    // commit 2.
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Snap"), shortcut: Cow::Borrowed("N"),
        action: PaletteAction::Custom(|s, cmds| { cmds.push(AppCommand::SetSnapMaster(!s.snap.master)); }), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Skimming"), shortcut: Cow::Borrowed("S"),
        action: PaletteAction::ToggleState(|s| s.skimming_enabled = !s.skimming_enabled), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Audio Skim"), shortcut: Cow::Borrowed("Shift+S"),
        action: PaletteAction::Cmd(AppCommand::ToggleAudioSkim), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Colour Panel"), shortcut: Cow::Borrowed("C"),
        action: PaletteAction::Cmd(AppCommand::ToggleColorPanel), enabled: true });
    // Node-graph auto-layout — gated on a selected clip so the
    // entry doesn't fire silently against an empty workspace.
    e.push(PaletteEntry {
        label: Cow::Borrowed("Node graph: Auto-layout"),
        shortcut: Cow::Borrowed("L"),
        action: PaletteAction::Custom(|state, cmds| {
            if let Some(clip_id) = state.selected_clip {
                cmds.push(AppCommand::AutoLayoutNodeGraph { clip_id });
            }
        }),
        enabled: state.selected_clip.is_some(),
    });
    // Node-graph view bookmarks. 9 jump entries + 9 set entries; the
    // hotkeys 1..9 / Shift+1..9 only fire when the node-graph canvas
    // is focused, so the palette is the cross-context discovery
    // surface for users who don't know the keys yet.
    for n in 1..=9_u8 {
        let jump_label = Cow::Owned(format!("Node graph: Jump to bookmark {n}"));
        let jump_shortcut = Cow::Owned(format!("{n}"));
        e.push(PaletteEntry {
            label: jump_label,
            shortcut: jump_shortcut,
            action: PaletteAction::Cmd(AppCommand::JumpNodeViewBookmark { slot: n - 1 }),
            enabled: true,
        });
        let set_label = Cow::Owned(format!("Node graph: Set bookmark {n} to current view"));
        let set_shortcut = Cow::Owned(format!("Shift+{n}"));
        e.push(PaletteEntry {
            label: set_label,
            shortcut: set_shortcut,
            action: PaletteAction::Cmd(AppCommand::SetNodeViewBookmark { slot: n - 1 }),
            enabled: true,
        });
    }
    // Copy / paste color nodes between clips. Both gated on having a
    // selected clip — Copy needs a source, Paste needs a target.
    e.push(PaletteEntry { label: Cow::Borrowed("Copy Colour Nodes"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Custom(|state, cmds| {
            if let Some(clip_id) = state.selected_clip {
                cmds.push(AppCommand::CopyColorNodes { clip_id });
            }
        }),
        enabled: state.selected_clip.is_some() });
    e.push(PaletteEntry { label: Cow::Borrowed("Paste Colour Nodes"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Custom(|state, cmds| {
            if let Some(clip_id) = state.selected_clip {
                cmds.push(AppCommand::PasteColorNodes { clip_id });
            }
        }),
        enabled: state.selected_clip.is_some()
            && state.grade_node_clipboard.as_ref().is_some_and(|c| !c.nodes.is_empty()) });
    // Bake Grade — closure form because we need the selected clip + chosen
    // size + a synchronous save-file dialog, none of which fits the simple
    // Cmd / ToggleState shapes.
    // Fade-from / fade-to colour presets — both append a `BuiltinEffect::ColorFade`
    // to the selected clip with the conventional 1-second duration. The user can
    // then tweak colour + duration in the inspector. Disabled when no clip is
    // selected (the AddBuiltinEffect handler would no-op anyway, but the palette
    // staying greyed-out matches the rest of the clip-targeted entries).
    e.push(PaletteEntry { label: Cow::Borrowed("Fade From Black (1 s)"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Custom(|state, cmds| {
            add_to_selection(state, cmds, "Fade", klipp_effects::BuiltinEffect::ColorFade {
                        color: [0, 0, 0, 255],
                        duration_secs: 1.0,
                        mode: klipp_effects::ColorFadeMode::In,
                        progress: 0.0,
                    });
        }),
        enabled: state.selected_clip.is_some() });
    e.push(PaletteEntry { label: Cow::Borrowed("Fade To Black (1 s)"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Custom(|state, cmds| {
            add_to_selection(state, cmds, "Fade", klipp_effects::BuiltinEffect::ColorFade {
                        color: [0, 0, 0, 255],
                        duration_secs: 1.0,
                        mode: klipp_effects::ColorFadeMode::Out,
                        progress: 0.0,
                    });
        }),
        enabled: state.selected_clip.is_some() });
    e.push(PaletteEntry { label: Cow::Borrowed("Fade From White (1 s)"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Custom(|state, cmds| {
            add_to_selection(state, cmds, "Fade", klipp_effects::BuiltinEffect::ColorFade {
                        color: [255, 255, 255, 255],
                        duration_secs: 1.0,
                        mode: klipp_effects::ColorFadeMode::In,
                        progress: 0.0,
                    });
        }),
        enabled: state.selected_clip.is_some() });
    e.push(PaletteEntry { label: Cow::Borrowed("Fade To White (1 s)"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Custom(|state, cmds| {
            add_to_selection(state, cmds, "Fade", klipp_effects::BuiltinEffect::ColorFade {
                        color: [255, 255, 255, 255],
                        duration_secs: 1.0,
                        mode: klipp_effects::ColorFadeMode::Out,
                        progress: 0.0,
                    });
        }),
        enabled: state.selected_clip.is_some() });
    e.push(PaletteEntry { label: Cow::Borrowed("Bake Grade as 3D LUT (.cube)"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Custom(|state, _cmds| {
            let Some(clip_id) = state.selected_clip else { return; };
            let size = state.bake_lut_size;
            crate::file_dialog::spawn(move || {
                rfd::FileDialog::new()
                    .set_title("Save baked grade as .cube")
                    .add_filter("3D LUT (.cube)", &["cube"])
                    .set_file_name(format!("klipp_grade_{size}.cube"))
                    .save_file()
                    .map(|path| {
                        let output_path = if path.extension().is_some() { path } else { path.with_extension("cube") };
                        vec![AppCommand::BakeGradeToLut { clip_id, output_path, lut_size: size }]
                    })
                    .unwrap_or_default()
            });
        }),
        enabled: state.selected_clip.is_some() });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Scopes"), shortcut: Cow::Borrowed("Alt+V"),
        action: PaletteAction::ToggleState(|s| s.toggle_scopes()), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Scope: Toggle Low-Pass Filter"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::ToggleState(|s| s.scope_low_pass = !s.scope_low_pass), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Scope: Toggle Bloom"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::ToggleState(|s| s.scope_bloom = !s.scope_bloom), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Scope: Histogram Linear Levels"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::ToggleState(|s| s.scope_options.histogram_levels = klipp_core::HistogramLevels::Linear), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Scope: Histogram Logarithmic Levels"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::ToggleState(|s| s.scope_options.histogram_levels = klipp_core::HistogramLevels::Logarithmic), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Scope: Reset Options"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::ToggleState(|s| s.scope_options = Default::default()), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Inspector Panel"), shortcut: Cow::Borrowed("Ctrl+Shift+I"),
        action: PaletteAction::ToggleState(|s| s.show_inspector_panel = !s.show_inspector_panel), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Effects Browser"), shortcut: Cow::Borrowed("Ctrl+Shift+E"),
        action: PaletteAction::ToggleState(|s| s.show_browser_panel = !s.show_browser_panel), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Audio Mixer"), shortcut: Cow::Borrowed("Ctrl+Shift+M"),
        action: PaletteAction::ToggleState(|s| s.show_audio_mixer = !s.show_audio_mixer), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Curve Editor"), shortcut: Cow::Borrowed("Shift+K"),
        action: PaletteAction::ToggleState(|s| s.show_curve_editor = !s.show_curve_editor), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Markers Panel"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::ToggleState(|s| s.show_markers_panel = !s.show_markers_panel), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Edit List"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::ToggleState(|s| s.show_edit_list = !s.show_edit_list), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Colour Check"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::ToggleState(|s| s.show_colour_check = !s.show_colour_check), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Delivery Check"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::ToggleState(|s| s.show_delivery_check = !s.show_delivery_check), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Sound Over Time"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::ToggleState(|s| s.show_sound_over_time = !s.show_sound_over_time), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Narrate"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::ToggleState(|s| s.show_narrate = !s.show_narrate), enabled: true });
    // Focus the search field of whichever panel was last clicked.
    // Routes through the same one-shot focus_search_request flag the
    // Ctrl+F dispatcher uses, so the next show() of the active panel
    // grabs focus and clears the request.
    e.push(PaletteEntry { label: Cow::Borrowed("Focus Search in Current Panel"), shortcut: Cow::Borrowed("Ctrl+F"),
        action: PaletteAction::ToggleState(|s| { s.focus_search_request = s.active_panel; }), enabled: true });
    // Timeline-specific find entries. "Find in Timeline" opens the
    // search bar (Ctrl+F when the timeline is the active panel does
    // the same — this is the discoverability anchor for users who
    // haven't clicked the timeline yet). "Find Next" / "Find Previous"
    // cycle through hits when the bar is already open; they noop
    // otherwise per the timeline_search_next/_prev contract.
    e.push(PaletteEntry { label: Cow::Borrowed("Find in Timeline"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::ToggleState(|s| {
            s.focus_search_request = Some(crate::state::PanelKind::Timeline);
        }), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Find Next"), shortcut: Cow::Borrowed("Enter"),
        action: PaletteAction::ToggleState(|s| { s.timeline_search_next = true; }), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Find Previous"), shortcut: Cow::Borrowed("Shift+Enter"),
        action: PaletteAction::ToggleState(|s| { s.timeline_search_prev = true; }), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Find and Replace in Titles\u{2026}"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::SetState(|s| s.show_title_find = true), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle History Panel"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::ToggleState(|s| s.show_history_panel = !s.show_history_panel), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Subtitle Editor"), shortcut: Cow::Borrowed("Ctrl+Shift+T"),
        action: PaletteAction::ToggleState(|s| s.show_subtitle_editor = !s.show_subtitle_editor), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Transcript"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::ToggleState(|s| s.show_transcript_panel = !s.show_transcript_panel), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Captions from Transcript\u{2026}"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::ToggleState(|s| {
            s.show_subtitle_editor = true;
            s.subtitle_editor.captions_form.get_or_insert_with(Default::default);
        }), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Performance Overlay"), shortcut: Cow::Borrowed("P"),
        action: PaletteAction::ToggleState(|s| s.show_perf_overlay = !s.show_perf_overlay), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Zebras"), shortcut: Cow::Borrowed("Z"),
        action: PaletteAction::ToggleState(|s| s.viewer_overlays.zebras = !s.viewer_overlays.zebras),
        enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle False Colour"), shortcut: Cow::Borrowed("Alt+F"),
        action: PaletteAction::ToggleState(|s| s.viewer_overlays.false_color = !s.viewer_overlays.false_color),
        enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Focus Peaking"), shortcut: Cow::Borrowed("Ctrl+P"),
        action: PaletteAction::ToggleState(|s| s.viewer_overlays.focus_peaking = !s.viewer_overlays.focus_peaking),
        enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Safe Areas"), shortcut: Cow::Borrowed("Alt+S"),
        action: PaletteAction::ToggleState(|s| s.viewer_overlays.safe_areas = !s.viewer_overlays.safe_areas),
        enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Grid Overlay"), shortcut: Cow::Borrowed("Alt+G"),
        action: PaletteAction::ToggleState(|s| s.viewer_overlays.grid = !s.viewer_overlays.grid),
        enabled: true });

    // ── Capture ─────────────────────────────────────────────
    e.push(PaletteEntry { label: Cow::Borrowed("Frame Grab (Save PNG)"), shortcut: Cow::Borrowed("Ctrl+Alt+S"),
        action: PaletteAction::Cmd(AppCommand::FrameGrab), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Extract Frame to Bin"), shortcut: Cow::Borrowed("Ctrl+Alt+B"),
        action: PaletteAction::Cmd(AppCommand::ExtractFrameToBin), enabled: true });

    // ── AI Models ───────────────────────────────────────────
    // Per-model "Download …" entries auto-derived from the active
    // manifest. Adding a model entry to assets/models.toml makes a
    // new palette entry appear automatically — no code change here.
    // Manifest load failures here are silently ignored (the rest
    // of the palette still renders); the manifest is parse-checked
    // by klipp-ml's own tests, so failure means a build-time bug.
    if let Ok(manifest) = klipp_ml::load_effective() {
        for model in &manifest.models {
            // Static lifetime conversion for label/shortcut — we
            // leak the strings since the palette is rebuilt every
            // open and these entries are bounded by manifest length.
            // Cheap; manifest typically has < 10 models.
            e.push(PaletteEntry {
                label: Cow::Owned(format!("Download AI Model: {}", model.display_name)),
                shortcut: Cow::Borrowed(""),
                action: PaletteAction::Cmd(AppCommand::OpenModelDownloadDialog {
                    model_id: model.id.clone(),
                }),
                enabled: true,
            });
        }
    }

    // ── AI Tools — task launchers ──────────────────────────
    // Each task launcher pushes `ShowAiToolsPanel { task }` so the
    // panel opens (or switches focus) on the right task. Track Object
    // is intentionally disabled — the tracker model lands later in the
    // roadmap and we want the discoverability surface in place now,
    // greyed, rather than appearing in v2 with no warning.
    e.push(PaletteEntry {
        label: Cow::Borrowed("AI: Remove Background"),
        shortcut: Cow::Borrowed("Shift+B"),
        action: PaletteAction::Cmd(AppCommand::ShowAiToolsPanel {
            task: AiToolsTaskTag::RemoveBackground,
        }),
        enabled: true,
    });
    e.push(PaletteEntry {
        label: Cow::Borrowed("AI: Isolate Object"),
        shortcut: Cow::Borrowed("Shift+J"),
        action: PaletteAction::Cmd(AppCommand::ShowAiToolsPanel {
            task: AiToolsTaskTag::IsolateObject,
        }),
        enabled: true,
    });
    e.push(PaletteEntry {
        label: Cow::Borrowed("AI: Detect Objects"),
        shortcut: Cow::Borrowed("Shift+D"),
        action: PaletteAction::Cmd(AppCommand::ShowAiToolsPanel {
            task: AiToolsTaskTag::DetectObjects,
        }),
        enabled: true,
    });
    e.push(PaletteEntry {
        label: Cow::Borrowed("AI: Track Object"),
        shortcut: Cow::Borrowed(""),
        action: PaletteAction::Cmd(AppCommand::ShowAiToolsPanel {
            task: AiToolsTaskTag::TrackObject,
        }),
        // Greyed until the tracker model lands. The entry is here on
        // purpose — discoverability beats a silent gap.
        enabled: false,
    });
    e.push(PaletteEntry {
        label: Cow::Borrowed("AI: Run Inference (Active Task)"),
        shortcut: Cow::Borrowed("Ctrl+Enter"),
        action: PaletteAction::Cmd(AppCommand::RunAiToolsInference),
        // Always-enabled for now; gating on "is there an active task?"
        // can be added once the AI Tools state field lands and the
        // palette can read it.
        enabled: true,
    });
    e.push(PaletteEntry {
        label: Cow::Borrowed("AI: Cancel Inference"),
        shortcut: Cow::Borrowed("Ctrl+."),
        action: PaletteAction::Cmd(AppCommand::CancelAiToolsInference),
        enabled: true,
    });
    e.push(PaletteEntry {
        label: Cow::Borrowed("Toggle AI Tools Panel"),
        shortcut: Cow::Borrowed("Alt+A"),
        // Assumes `AppState::ai_tools_state.panel_visible: bool` —
        // see the module doc-comment. Integrate-time tripwire: this
        // file won't compile until the field lands.
        action: PaletteAction::ToggleState(|s| {
            s.ai_tools_state.panel_visible = !s.ai_tools_state.panel_visible
        }),
        enabled: true,
    });
    e.push(PaletteEntry {
        label: Cow::Borrowed("Manage AI Models…"),
        shortcut: Cow::Borrowed(""),
        action: PaletteAction::Cmd(AppCommand::OpenAiModelsDialog),
        enabled: true,
    });
    e.push(PaletteEntry {
        label: Cow::Borrowed("Manage Audio Plugins…"),
        shortcut: Cow::Borrowed(""),
        action: PaletteAction::Cmd(AppCommand::OpenAudioPluginsDialog),
        enabled: true,
    });
    e.push(PaletteEntry {
        label: Cow::Borrowed("Rescan Audio Plugins"),
        shortcut: Cow::Borrowed(""),
        action: PaletteAction::Cmd(AppCommand::RescanAudioPlugins),
        enabled: true,
    });
    e.push(PaletteEntry {
        label: Cow::Borrowed("Re-enable all Audio Plugins"),
        shortcut: Cow::Borrowed(""),
        action: PaletteAction::Cmd(AppCommand::ClearAllAudioPluginsDisabled),
        enabled: true,
    });
    e.push(PaletteEntry {
        label: Cow::Borrowed("System Probe…"),
        shortcut: Cow::Borrowed(""),
        action: PaletteAction::Cmd(AppCommand::OpenSystemProbe),
        enabled: true,
    });
    e.push(PaletteEntry {
        label: Cow::Borrowed("Project Doctor…"),
        shortcut: Cow::Borrowed(""),
        action: PaletteAction::Cmd(AppCommand::OpenProjectDoctor),
        enabled: true,
    });

    // ── Compositing (track matte / stencil / blend mode) ───
    // Discoverability layer for the track-matte feature. Each entry
    // gates on `selected_clip` being set; greyed when nothing is
    // selected so users see "this is here, just need a clip" rather
    // than the entry vanishing.
    if let Some(cid) = state.selected_clip {
        e.push(PaletteEntry {
            label: Cow::Borrowed("Use clip above as matte (Auto)"),
            shortcut: Cow::Borrowed(""),
            action: PaletteAction::Cmd(AppCommand::UseClipAboveAsMatte { clip_id: cid, channel: 1 }),
            enabled: true,
        });
        e.push(PaletteEntry {
            label: Cow::Borrowed("Use clip above as matte (Alpha)"),
            shortcut: Cow::Borrowed(""),
            action: PaletteAction::Cmd(AppCommand::UseClipAboveAsMatte { clip_id: cid, channel: 0 }),
            enabled: true,
        });
        e.push(PaletteEntry {
            label: Cow::Borrowed("Use clip above as matte (Luma)"),
            shortcut: Cow::Borrowed(""),
            action: PaletteAction::Cmd(AppCommand::UseClipAboveAsMatte { clip_id: cid, channel: 1 }),
            enabled: true,
        });
        e.push(PaletteEntry {
            label: Cow::Borrowed("Clear track matte"),
            shortcut: Cow::Borrowed(""),
            action: PaletteAction::Cmd(AppCommand::ClearClipMatte { clip_id: cid }),
            enabled: true,
        });
        e.push(PaletteEntry {
            label: Cow::Borrowed("Toggle stencil mode"),
            shortcut: Cow::Borrowed(""),
            action: PaletteAction::Cmd(AppCommand::ToggleClipStencil { clip_id: cid }),
            enabled: true,
        });
        // Transform link is a target-bound binding, so palette entries
        // for "Set transform link → <picked>" need a target-picker UI
        // we don't have in the palette today. The clear / unbind path
        // is parameterless on the focused clip and useful as a quick
        // rescue when a wired-up parent gets in the way.
        e.push(PaletteEntry {
            label: Cow::Borrowed("Clear transform link"),
            shortcut: Cow::Borrowed(""),
            action: PaletteAction::Cmd(AppCommand::ClearClipTransformLink { clip_id: cid }),
            enabled: true,
        });
        // Hyperzoom: same target-picker constraint, so the palette
        // hosts the easy-path variant (auto target = next clip on
        // this track, focal = clip centre, default duration + peak).
        // Power users tweak focal/duration in the inspector before
        // applying.
        e.push(PaletteEntry {
            label: Cow::Borrowed("Apply hyperzoom (auto, centre)"),
            shortcut: Cow::Borrowed(""),
            action: PaletteAction::Cmd(AppCommand::ApplyHyperzoom {
                source_clip_id: cid,
                target_clip_id: None,
                focal_x_norm: 0.5,
                focal_y_norm: 0.5,
                duration_secs: 0.5,
                max_scale: 50.0,
                // Palette easy-path keeps every polish layer at its
                // back-compat default (Linear easing, no hold, no
                // radial blur, no glyph lock) so it's reachable
                // without typing into the inspector. Power users
                // tweak there.
                easing: 0,
                hold_secs: 0.0,
                add_radial_blur: false,
                radial_blur_amount: 0.0,
                focal_ref: None,
                // Palette easy-path stays whoosh-free for the same
                // reason as the other polish layers — power users
                // tick the box in the inspector after applying.
                add_whoosh: false,
                whoosh_variant: 0,
                whoosh_custom_path: None,
                add_chromatic_aberration: false,
                chromatic_aberration_amount: 0.0,
                add_light_leak: false,
                light_leak_intensity: 0.0,
            }),
            enabled: true,
        });
        // Hum-removal presets — drop the `hum_removal` AudioEffect
        // onto the selected clip with the fundamental pre-tuned for
        // the user's region. Each variant pushes AddAudioEffect plus
        // a follow-up UpdateAudioEffectParam at the post-add index
        // (current audio_effects.len() — AddAudioEffect appends, so
        // the new slot lands at the previous len). Both palette
        // entries route through AppCommand so undo can reverse them.
        // Two explicit entries (rather than a loop) because
        // PaletteAction::Custom takes a `fn` pointer that can't
        // capture the per-row fundamental_hz.
        e.push(PaletteEntry {
            label: Cow::Borrowed("Hum Removal — 50 Hz (Europe/Asia)"),
            shortcut: Cow::Borrowed(""),
            action: PaletteAction::Custom(|state, cmds| {
                let Some(cid) = state.selected_clip else { return };
                let new_index = state
                    .sequence()
                    .all_clips()
                    .find(|c| c.id == cid)
                    .map(|c| c.audio_effects.len())
                    .unwrap_or(0);
                cmds.push(AppCommand::AddAudioEffect {
                    clip_id: cid,
                    effect_type: "hum_removal".into(),
                });
                cmds.push(AppCommand::UpdateAudioEffectParam {
                    clip_id: cid,
                    effect_index: new_index,
                    param_name: "fundamental_hz".into(),
                    value: 50.0,
                });
            }),
            enabled: true,
        });
        e.push(PaletteEntry {
            label: Cow::Borrowed("Hum Removal — 60 Hz (Americas)"),
            shortcut: Cow::Borrowed(""),
            action: PaletteAction::Custom(|state, cmds| {
                let Some(cid) = state.selected_clip else { return };
                let new_index = state
                    .sequence()
                    .all_clips()
                    .find(|c| c.id == cid)
                    .map(|c| c.audio_effects.len())
                    .unwrap_or(0);
                cmds.push(AppCommand::AddAudioEffect {
                    clip_id: cid,
                    effect_type: "hum_removal".into(),
                });
                cmds.push(AppCommand::UpdateAudioEffectParam {
                    clip_id: cid,
                    effect_index: new_index,
                    param_name: "fundamental_hz".into(),
                    value: 60.0,
                });
            }),
            enabled: true,
        });
        // Camera-wind preset: a conservative rumble cut followed by a gate
        // for pauses. Each effect is still editable in the Audio inspector.
        e.push(PaletteEntry {
            label: Cow::Borrowed("Camera Wind — rumble reduction"),
            shortcut: Cow::Borrowed(""),
            action: PaletteAction::Custom(|state, cmds| {
                let Some(cid) = state.selected_clip else { return };
                let base = state.sequence().all_clips().find(|c| c.id == cid).map(|c| c.audio_effects.len()).unwrap_or(0);
                cmds.push(AppCommand::AddAudioEffect { clip_id: cid, effect_type: "high_pass".into() });
                cmds.push(AppCommand::UpdateAudioEffectParam { clip_id: cid, effect_index: base, param_name: "frequency".into(), value: 100.0 });
                cmds.push(AppCommand::UpdateAudioEffectParam { clip_id: cid, effect_index: base, param_name: "slope".into(), value: 3.0 });
                cmds.push(AppCommand::AddAudioEffect { clip_id: cid, effect_type: "gate".into() });
                cmds.push(AppCommand::UpdateAudioEffectParam { clip_id: cid, effect_index: base + 1, param_name: "threshold".into(), value: -45.0 });
                cmds.push(AppCommand::UpdateAudioEffectParam { clip_id: cid, effect_index: base + 1, param_name: "range".into(), value: -18.0 });
                cmds.push(AppCommand::UpdateAudioEffectParam { clip_id: cid, effect_index: base + 1, param_name: "release".into(), value: 120.0 });
            }),
            enabled: true,
        });
        // PiP corner presets — one entry per corner. Drops a 30 %-
        // scale Transform onto the selected clip with the clip
        // anchored to the named corner of the frame, with offsets
        // computed from the project resolution (so 720p / 1080p /
        // 4K all land cornered correctly). Same AppCommand the
        // timeline + toolbar Motion Presets submenus dispatch.
        for (label, preset) in [
            ("PiP: Top Left", "pip_top_left"),
            ("PiP: Top Right", "pip_top_right"),
            ("PiP: Bottom Left", "pip_bottom_left"),
            ("PiP: Bottom Right", "pip_bottom_right"),
        ] {
            e.push(PaletteEntry {
                label: Cow::Borrowed(label),
                shortcut: Cow::Borrowed(""),
                action: PaletteAction::Cmd(AppCommand::ApplyMotionPreset {
                    clip_id: cid,
                    preset: preset.into(),
                }),
                enabled: true,
            });
        }
        // Auto-link follow-up: ramp window matches the hyperzoom
        // default (0.5 s); 'fly with parent' canonical mode (Absolute,
        // all flags on). Inspector exposes per-clip refinement.
        e.push(PaletteEntry {
            label: Cow::Borrowed("Link clips beneath hyperzoom source"),
            shortcut: Cow::Borrowed(""),
            action: PaletteAction::Cmd(AppCommand::AutoLinkClipsBeneathSource {
                source_clip_id: cid,
                ramp_secs: 0.5,
                mode: 0,
                inherit_scale: true,
                inherit_position: true,
                inherit_rotation: true,
                inherit_anchor: true,
            }),
            enabled: true,
        });
        // Cut templates — multi-step recipes that compose existing
        // primitives (matte / transform_link / hyperzoom / keyframes)
        // into one-click operations. Iterates the shared
        // `cut_templates::MENU_ROWS` table (also driving the timeline
        // right-click submenu) so adding a sixth template is a
        // one-place change. Each entry's `enabled` flag is the
        // template's own `can_apply` predicate so unrelated
        // templates aren't greyed together.
        let _ = cid; // gating uses template_context, not cid directly
        let template_ctx = crate::cut_templates::template_context(state);
        for row in crate::cut_templates::MENU_ROWS {
            let enabled = crate::cut_templates::can_apply(&row.kind, &template_ctx);
            e.push(PaletteEntry {
                label: template_palette_label(row.label),
                shortcut: Cow::Borrowed(""),
                action: PaletteAction::Custom(row.dispatch),
                enabled,
            });
        }
        // Common blend modes only — the full 13-mode list would
        // dilute the palette. Users who want the full set go through
        // the Inspector COMPOSITING section.
        for &(label, mode) in &[
            ("Set blend mode → Normal", klipp_core::BlendMode::Normal),
            ("Set blend mode → Multiply", klipp_core::BlendMode::Multiply),
            ("Set blend mode → Screen", klipp_core::BlendMode::Screen),
            ("Set blend mode → Overlay", klipp_core::BlendMode::Overlay),
            ("Set blend mode → Add", klipp_core::BlendMode::Add),
            ("Set blend mode → Difference", klipp_core::BlendMode::Difference),
        ] {
            e.push(PaletteEntry {
                label: Cow::Borrowed(label),
                shortcut: Cow::Borrowed(""),
                action: PaletteAction::Cmd(AppCommand::SetClipBlendMode { clip_id: cid, mode }),
                enabled: true,
            });
        }
    } else {
        // Greyed placeholders so users still find the feature when
        // nothing is selected.
        e.push(PaletteEntry {
            label: Cow::Borrowed("Use clip above as matte (Auto)"),
            shortcut: Cow::Borrowed(""),
            action: PaletteAction::Cmd(AppCommand::Undo),
            enabled: false,
        });
        e.push(PaletteEntry {
            label: Cow::Borrowed("Toggle stencil mode"),
            shortcut: Cow::Borrowed(""),
            action: PaletteAction::Cmd(AppCommand::Undo),
            enabled: false,
        });
        e.push(PaletteEntry {
            label: Cow::Borrowed("Clear transform link"),
            shortcut: Cow::Borrowed(""),
            action: PaletteAction::Cmd(AppCommand::Undo),
            enabled: false,
        });
        e.push(PaletteEntry {
            label: Cow::Borrowed("Apply hyperzoom (auto, centre)"),
            shortcut: Cow::Borrowed(""),
            action: PaletteAction::Cmd(AppCommand::Undo),
            enabled: false,
        });
        e.push(PaletteEntry {
            label: Cow::Borrowed("Link clips beneath hyperzoom source"),
            shortcut: Cow::Borrowed(""),
            action: PaletteAction::Cmd(AppCommand::Undo),
            enabled: false,
        });
        // Greyed hum-removal placeholders — same anchors-test
        // substring discipline as the PiP twins below.
        for label in [
            "Hum Removal — 50 Hz (Europe/Asia)",
            "Hum Removal — 60 Hz (Americas)",
        ] {
            e.push(PaletteEntry {
                label: Cow::Borrowed(label),
                shortcut: Cow::Borrowed(""),
                action: PaletteAction::Cmd(AppCommand::Undo),
                enabled: false,
            });
        }
        // Greyed PiP placeholders so the anchors-test substring
        // search still finds the labels at default state, and the
        // user discovers the feature exists even before selecting
        // a clip.
        for label in [
            "PiP: Top Left",
            "PiP: Top Right",
            "PiP: Bottom Left",
            "PiP: Bottom Right",
        ] {
            e.push(PaletteEntry {
                label: Cow::Borrowed(label),
                shortcut: Cow::Borrowed(""),
                action: PaletteAction::Cmd(AppCommand::Undo),
                enabled: false,
            });
        }
        // Cut templates — greyed twins so the labels are reachable
        // from search even with no selection (anchors test runs
        // against default state). Same MENU_ROWS table as the
        // active branch so labels can't drift.
        for row in crate::cut_templates::MENU_ROWS {
            e.push(PaletteEntry {
                label: template_palette_label(row.label),
                shortcut: Cow::Borrowed(""),
                action: PaletteAction::Cmd(AppCommand::Undo),
                enabled: false,
            });
        }
    }

    e
}

/// Ask for media files to import: several at once, and every kind the
/// pool takes (video, audio, stills), not only video. Empty on cancel.
///
/// Every import route (File menu, palette, Ctrl+I, the media tab's
/// menu) goes through here so they cannot drift apart again: they
/// once offered one file per dialog, and Ctrl+I only video.
pub fn pick_media_files() -> Vec<std::path::PathBuf> {
    rfd::FileDialog::new()
        .set_title("Import Media")
        .add_filter("All media", &klipp_core::media_extensions::all_media_extensions())
        .add_filter("Video", klipp_core::media_extensions::VIDEO_EXTENSIONS)
        .add_filter("Audio", klipp_core::media_extensions::AUDIO_EXTENSIONS)
        .add_filter("Image", klipp_core::media_extensions::IMAGE_EXTENSIONS)
        .pick_files()
        .unwrap_or_default()
}

/// [`pick_media_files`], as the import commands for what was picked.
/// Shaped for `file_dialog::spawn`.
pub fn import_media_commands() -> Vec<AppCommand> {
    pick_media_files().into_iter().map(|path| AppCommand::ImportMedia { path }).collect()
}

/// Resolve a [`DialogKind`] into `AppCommand`s by surfacing the
/// matching system file picker. Blocks until the user picks or
/// cancels, so callers run it through `file_dialog::spawn`, off the
/// UI thread. Empty on cancel; more than one command only
/// for pickers that take several files.
pub fn dispatch_dialog(kind: DialogKind) -> Vec<AppCommand> {
    let single = match kind {
        DialogKind::OpenProject => return crate::project_paths::pick_project_commands(),
        DialogKind::SaveProjectAs => rfd::FileDialog::new()
            .set_title("Save Project As")
            .add_filter("Klipp Project", &["klipp"])
            .set_file_name("project.klipp")
            .save_file()
            .map(|p| AppCommand::SaveProjectAs { path: p }),
        DialogKind::ImportMedia => {
            return import_media_commands();
        }
        DialogKind::ImportSubtitles => rfd::FileDialog::new()
            .set_title("Import Subtitles")
            .add_filter("Subtitles", &["srt", "vtt", "ass", "ssa", "ttml", "dfxp", "xml", "itt", "scc", "stl"])
            .pick_file()
            .map(|p| AppCommand::ImportSubtitles { path: p }),
        DialogKind::ExportSubtitles => rfd::FileDialog::new()
            .set_title("Export Subtitles")
            .add_filter("SRT", &["srt"])
            .add_filter("WebVTT", &["vtt"])
            .add_filter("ASS", &["ass"])
            .add_filter("SSA", &["ssa"])
            .add_filter("TTML (IMSC)", &["ttml"])
            .add_filter("SCC (CEA-608)", &["scc"])
            .add_filter("EBU-STL", &["stl"])
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
        DialogKind::ExportOtio => rfd::FileDialog::new()
            .set_title("Export OpenTimelineIO")
            .add_filter("OpenTimelineIO", &["otio"])
            .set_file_name("project.otio")
            .save_file()
            .map(|p| AppCommand::ExportOtio { path: p }),
        DialogKind::ExportAaf => rfd::FileDialog::new()
            .set_title("Export AAF")
            .add_filter("AAF", &["aaf"])
            .set_file_name("project.aaf")
            .save_file()
            .map(|p| AppCommand::ExportAaf { path: p }),
        DialogKind::ExportKdenlive => rfd::FileDialog::new()
            .set_title("Export Kdenlive Project")
            .add_filter("Kdenlive Project", &["kdenlive"])
            .set_file_name("project.kdenlive")
            .save_file()
            .map(|p| AppCommand::ExportKdenlive { path: p }),
        DialogKind::OpenFromAnotherEditor => rfd::FileDialog::new()
            .set_title("Open Project from Another Editor")
            .add_filter("Timelines", &["fcpxml", "xml", "otio", "kdenlive", "mlt", "edl", "aaf"])
            .add_filter("Any file", &["*"])
            .pick_file()
            .map(|path| AppCommand::OpenFromAnotherEditor { path, request_id: uuid::Uuid::nil() }),
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
    };
    single.into_iter().collect()
}

/// `effect` on every selected picture that can take it, as one undo step
/// (#842), with the toast saying where it went: by path (M3 step 19), or
/// refused whole with the reason.
fn add_to_selection(state: &mut AppState, cmds: &mut Vec<AppCommand>, name: &str, effect: klipp_effects::BuiltinEffect) {
    let targets = crate::effect_targets::targets(state.sequence(), state.selected_clip, &state.selected_clips, None, crate::effect_targets::EffectKind::Picture);
    if targets.clips.is_empty() {
        return;
    }
    let adds = crate::effect_targets::all_or_none(&targets, |clip_id| {
        let clip = state.sequence().tracks.iter().flat_map(|t| &t.clips).find(|c| c.id == clip_id).ok_or("the clip isn't in the sequence")?;
        crate::inspector::stack_view::add_effects_command(state, clip, std::slice::from_ref(&effect))
    });
    match adds {
        Ok(adds) => {
            cmds.extend(adds);
            state.toast.show(crate::effect_targets::said(name, &targets));
        }
        Err(why) => state.toast.show(format!("\"{name}\" wasn't applied: {why}")),
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
        "Toggle Audio Skim",
        "Frame Grab",
        "Export",
        "Import Media",
        "Preferences",
        "Quit",
        "AI: Remove Background",
        "AI: Run Inference",
        "Toggle AI Tools Panel",
        // Phase 5 — track-matte discoverability anchors. The greyed
        // "Use clip above as matte" entry is always rendered (even
        // with no selection) so the substring is reachable in the
        // anchors test against the no-clip default state.
        "Use clip above as matte",
        "Toggle stencil mode",
        // transform_link discovery anchor — the parameterless clear
        // path is what's reachable from the palette today (set-link
        // wants a target picker we don't have here yet). Pinned to
        // keep the entry from disappearing into a refactor.
        "Clear transform link",
        // Hyperzoom discovery anchor — the easy-path variant (auto
        // target, focal = clip centre, default duration + peak) is
        // what the palette exposes. Power users tweak in the
        // inspector before applying.
        "Apply hyperzoom",
        // Auto-link follow-up — the "fly with parent" canonical
        // helper. Pinned so the easy-path discovery surface stays
        // visible across refactors.
        "Link clips beneath",
        // Cut-template anchors — five one-click multi-step recipes.
        // Pinned individually because each is a distinct concept
        // (different gating, different output) and the discovery
        // value of the palette as a tutorial surface is in the
        // names being typeable.
        "Hyperzoom Letter Transition",
        "Track Matte Reveal",
        "Hyperzoom Bridge",
        "Whip Pan (left)",
        "Whip Pan (right)",
        // Per-panel search hotkey discovery anchor. Pinned so the
        // Ctrl+F entry can't disappear into a refactor.
        "Focus Search in Current Panel",
        // Timeline find-bar anchors. Three substrings so each variant
        // (open, next, previous) stays reachable for keyboard-shy
        // users typing into the palette.
        "Find in Timeline",
        "Find Next",
        "Find Previous",
        // Swap-clips discovery anchor — entry is rendered greyed
        // when the multi-select isn't a 2-clip pair, so the substring
        // is reachable in the no-selection default state.
        "Swap Selected Clips",
        // Per-clip export — entry is rendered greyed when nothing
        // is selected, so the substring "Export Selected Clip"
        // (which the dynamic label always starts with) is reachable
        // in the default state. Pin so a refactor can't quietly
        // remove the discovery surface.
        "Export Selected Clip",
        // PiP corner presets — only render when a clip is selected
        // (the four entries live inside the `if let Some(cid)`
        // block). Pin so a refactor can't quietly delete them.
        "PiP: Top Left",
        "PiP: Top Right",
        "PiP: Bottom Left",
        "PiP: Bottom Right",
        // Hum-removal preset entries — clip-gated with greyed
        // twins in the no-selection branch so the substrings are
        // reachable in the anchors test default state.
        "Hum Removal — 50 Hz",
        "Hum Removal — 60 Hz",
    ];

    #[test]
    fn palette_anchors_present() {
        let state = AppState::default();
        let entries = build(&state);
        let labels: Vec<&str> = entries.iter().map(|e| e.label.as_ref()).collect();
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
            assert!(seen.insert(e.label.as_ref().to_string()),
                "duplicate palette label: {:?}", e.label);
        }
    }
}
