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
    e.push(PaletteEntry { label: Cow::Borrowed("New Project"), shortcut: Cow::Borrowed("Ctrl+N"),
        action: PaletteAction::Cmd(AppCommand::NewProject), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Open Project…"), shortcut: Cow::Borrowed("Ctrl+O"),
        action: PaletteAction::OpenDialog(DialogKind::OpenProject), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Save Project"), shortcut: Cow::Borrowed("Ctrl+S"),
        action: PaletteAction::Cmd(AppCommand::SaveProject), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Save Project As…"), shortcut: Cow::Borrowed("Ctrl+Shift+S"),
        action: PaletteAction::OpenDialog(DialogKind::SaveProjectAs), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Close Project"), shortcut: Cow::Borrowed("Ctrl+W"),
        action: PaletteAction::Cmd(AppCommand::CloseProject), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Project Properties…"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::SetState(|s| s.show_project_info = true), enabled: true });
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
    e.push(PaletteEntry { label: Cow::Borrowed("Export Subtitles…"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::OpenDialog(DialogKind::ExportSubtitles), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Export FCPXML…"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::OpenDialog(DialogKind::ExportFcpxml), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Export EDL…"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::OpenDialog(DialogKind::ExportEdl), enabled: true });
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
        let cids: Vec<_> = state.selected_clips.iter().copied().collect();
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
    // Split at Playhead walks every track + clip under the playhead
    // and pushes one BladeClip per hit; this matches the toolbar's
    // Clip → Split at Playhead seam exactly.
    e.push(PaletteEntry { label: Cow::Borrowed("Split at Playhead"), shortcut: Cow::Borrowed("S"),
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
    e.push(PaletteEntry { label: Cow::Borrowed("Add Adjustment Layer"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Cmd(AppCommand::CreateAdjustmentLayer), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Create Multicam Clip"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Cmd(AppCommand::CreateMulticamClip), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Add Video Track"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Cmd(AppCommand::AddVideoTrack), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Add Audio Track"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::Cmd(AppCommand::AddAudioTrack), enabled: true });

    // ── Mark / Navigation ───────────────────────────────────
    e.push(PaletteEntry { label: Cow::Borrowed("Add Marker"), shortcut: Cow::Borrowed("M"),
        action: PaletteAction::Cmd(AppCommand::AddMarker {
            position: state.playhead, label: String::new(),
            color: klipp_core::MarkerColor::Blue }), enabled: true });
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
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Snap"), shortcut: Cow::Borrowed("N"),
        action: PaletteAction::ToggleState(|s| s.snap_enabled = !s.snap_enabled), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Skimming"), shortcut: Cow::Borrowed("S"),
        action: PaletteAction::ToggleState(|s| s.skimming_enabled = !s.skimming_enabled), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Audio Skim"), shortcut: Cow::Borrowed("Shift+S"),
        action: PaletteAction::Cmd(AppCommand::ToggleAudioSkim), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Color Panel"), shortcut: Cow::Borrowed("C"),
        action: PaletteAction::Cmd(AppCommand::ToggleColorPanel), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Scopes"), shortcut: Cow::Borrowed("V"),
        action: PaletteAction::ToggleState(|s| s.show_scopes = !s.show_scopes), enabled: true });
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
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle History Panel"), shortcut: Cow::Borrowed(""),
        action: PaletteAction::ToggleState(|s| s.show_history_panel = !s.show_history_panel), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Subtitle Editor"), shortcut: Cow::Borrowed("Ctrl+Shift+T"),
        action: PaletteAction::ToggleState(|s| s.show_subtitle_editor = !s.show_subtitle_editor), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Performance Overlay"), shortcut: Cow::Borrowed("P"),
        action: PaletteAction::ToggleState(|s| s.show_perf_overlay = !s.show_perf_overlay), enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle Zebras"), shortcut: Cow::Borrowed("Z"),
        action: PaletteAction::ToggleState(|s| s.viewer_overlays.zebras = !s.viewer_overlays.zebras),
        enabled: true });
    e.push(PaletteEntry { label: Cow::Borrowed("Toggle False Color"), shortcut: Cow::Borrowed("Alt+F"),
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
    }

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
