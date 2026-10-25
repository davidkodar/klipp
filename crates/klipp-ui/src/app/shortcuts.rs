//! Keyboard shortcuts: each frame's key presses read against the key
//! bindings and turned into commands; single keys wait while text is being
//! typed, Ctrl combinations do not (#826).

use klipp_timeline::GradeAccess;
use super::*;

impl KlippApp {
    pub(super) fn handle_global_shortcuts(&mut self, ctx: &egui::Context) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        // Skip single-key shortcuts when ANY text edit widget has focus (search bars,
        // title text editor, timecode entry, rename fields, etc.).
        // Ctrl+ shortcuts still work (standard convention: Ctrl combos always fire).
        let text_editing = ctx.wants_keyboard_input() || self.state.editing_timecode || self.state.viewer_text_edit;
        let scope = key_scope(&self.state, text_editing);
        match scope {
            KeyScope::Nothing => return cmds,
            KeyScope::AppOnly => {
                self.shortcuts_app(ctx, &mut cmds);
                return cmds;
            }
            _ => {}
        }
        self.shortcuts_ctrl_global(ctx, &mut cmds);
        match scope {
            KeyScope::CtrlOnly | KeyScope::Nothing | KeyScope::AppOnly => return cmds,
            KeyScope::GradeCanvas => {
                let kb = self.key_bindings.clone();
                ctx.input(|i| grade_node_keys(&mut self.state, &kb, i, &mut cmds));
                return cmds;
            }
            KeyScope::All => {}
        }
        // A trim amount being typed for the selected edit points takes the
        // keys until Enter or Escape: its digits are not angle switches,
        // its + and - not zooms (#913).
        if self.trim_entry_keys(ctx, &mut cmds) {
            return cmds;
        }
        self.shortcuts_magnetic_skim(ctx, &mut cmds);
        self.shortcuts_delete(ctx, &mut cmds);
        self.shortcuts_tools_and_clip_ops(ctx, &mut cmds);
        self.shortcuts_title_and_tools2(ctx, &mut cmds);
        self.shortcuts_multicam_and_io(ctx, text_editing, &mut cmds);
        self.shortcuts_edit_nav(ctx, &mut cmds);
        self.shortcuts_keyframe_and_zoom(ctx, &mut cmds);
        self.shortcuts_ai_tools(ctx, &mut cmds);
        cmds
    }

    fn shortcuts_ai_tools(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        let kb = self.key_bindings.clone();
        ctx.input(|i| {
            if kb.is_action_pressed("ai_remove_background", i) {
                cmds.push(AppCommand::ShowAiToolsPanel {
                    task: klipp_core::events::AiToolsTaskTag::RemoveBackground,
                });
            }
            if kb.is_action_pressed("ai_isolate_object", i) {
                cmds.push(AppCommand::ShowAiToolsPanel {
                    task: klipp_core::events::AiToolsTaskTag::IsolateObject,
                });
            }
            if kb.is_action_pressed("ai_detect_objects", i) {
                cmds.push(AppCommand::ShowAiToolsPanel {
                    task: klipp_core::events::AiToolsTaskTag::DetectObjects,
                });
            }
            if kb.is_action_pressed("ai_run_inference", i) {
                cmds.push(AppCommand::RunAiToolsInference);
            }
            if kb.is_action_pressed("ai_cancel_inference", i) {
                cmds.push(AppCommand::CancelAiToolsInference);
            }
            if kb.is_action_pressed("toggle_ai_tools_panel", i) {
                self.state.ai_tools_state.panel_visible =
                    !self.state.ai_tools_state.panel_visible;
                if self.state.ai_tools_state.panel_visible
                    && self.state.ai_tools_state.active_task.is_none()
                {
                    // Default the panel to Isolate Object so the
                    // open → click → Ctrl+Enter flow works without
                    // requiring the user to pick a task button first.
                    let default_task = crate::ai_tools::AiTask::IsolateObject;
                    self.state.ai_tools_state.active_task = Some(default_task);
                    self.state.ai_tools_state.selected_model_id = self
                        .state
                        .ai_tools_registry
                        .models_for(default_task)
                        .first()
                        .map(|m| m.id.clone());
                }
            }
        });
    }

    /// The keys that need no project: new, open, preferences and quit,
    /// all the welcome screen takes (#2551).
    fn shortcuts_app(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        let kb = self.key_bindings.clone();
        ctx.input(|i| {
            // Ctrl+N / Ctrl+Q. The menus advertised them for a long time
            // with nothing behind them, because `shortcut_text` only
            // draws a label.
            if kb.is_action_pressed("new_project", i) {
                cmds.push(AppCommand::NewProject);
            }
            if kb.is_action_pressed("quit", i) {
                cmds.push(AppCommand::Quit);
            }
            // Ctrl+O — Open Project
            if kb.is_action_pressed("open_project", i) {
                crate::file_dialog::spawn(crate::project_paths::pick_project_commands);
            }
            // Ctrl+, — Preferences
            if kb.is_action_pressed("preferences", i) {
                self.state.show_preferences = true;
            }
        });
    }

    fn shortcuts_ctrl_global(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        self.shortcuts_app(ctx, cmds);
        let kb = self.key_bindings.clone();
        ctx.input(|i| {
            // Ctrl+S — Save
            if kb.is_action_pressed("save_project", i) {
                cmds.push(AppCommand::SaveProject);
            }
            // Ctrl+Shift+M / Ctrl+Shift+T
            if kb.is_action_pressed("toggle_audio_mixer", i) {
                self.state.show_audio_mixer = !self.state.show_audio_mixer;
            }
            if kb.is_action_pressed("toggle_subtitle_editor", i) {
                self.state.show_subtitle_editor = !self.state.show_subtitle_editor;
            }
            // Ctrl+Shift+S — Save As
            if kb.is_action_pressed("save_project_as", i) {
                crate::file_dialog::spawn(move || {
                    rfd::FileDialog::new()
                        .set_title("Save Project As")
                        .add_filter("Klipp Project", &["klipp"])
                        .set_file_name("project.klipp")
                        .save_file()
                        .map(|path| vec![AppCommand::SaveProjectAs { path }])
                        .unwrap_or_default()
                });
            }
            // Ctrl+I — Import Media
            if kb.is_action_pressed("import_media", i) {
                crate::file_dialog::spawn(crate::command_palette::import_media_commands);
            }
            // Ctrl+E — Export
            if kb.is_action_pressed("export", i) {
                self.state.show_export_dialog = true;
                self.prepare_export_defaults();
            }
            // Ctrl+K — Command palette
            if kb.is_action_pressed("command_palette", i) {
                self.state.show_command_palette = true;
                self.state.command_palette_query.clear();
                self.state.command_palette_selected = 0;
            }
            // Ctrl+F — Focus the search field of the currently active
            // panel. Sets a one-shot flag that the target panel
            // consumes on its next show() (request_focus on its
            // TextEdit, clear the flag). When the active panel has no
            // search surface (Viewer / Inspector / Timeline) the
            // end-of-update watchdog below clears the flag silently.
            if kb.is_action_pressed("focus_panel_search", i) {
                self.state.focus_search_request = self.state.active_panel;
            }
            // Ctrl+W — Close Project (return to project selector)
            if kb.is_action_pressed("close_project", i) {
                cmds.push(AppCommand::CloseProject);
            }
            // Ctrl+Z — Undo, Ctrl+Shift+Z or Ctrl+Y — Redo
            if kb.is_action_pressed("undo", i) {
                cmds.push(AppCommand::Undo);
            }
            if kb.is_action_pressed("redo", i) {
                cmds.push(AppCommand::Redo);
            }

            // Group selected clips
            if !self.state.node_canvas_focused() && kb.is_action_pressed("group_clips", i) {
                let mut ids = self.state.selected_clips.clone();
                if let Some(primary) = self.state.selected_clip {
                    if !ids.contains(&primary) {
                        ids.push(primary);
                    }
                }
                if ids.len() >= 2 {
                    cmds.push(AppCommand::GroupClips { clip_ids: ids });
                }
            }
            // Ungroup
            if !self.state.node_canvas_focused() && kb.is_action_pressed("ungroup_clips", i) {
                if let Some(primary) = self.state.selected_clip {
                    let gid = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == primary)
                        .and_then(|c| c.group_id);
                    if let Some(gid) = gid {
                        cmds.push(AppCommand::UngroupClips { group_id: gid });
                    }
                }
            }

        });
    }

    fn shortcuts_magnetic_skim(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        let kb = self.key_bindings.clone();
        ctx.input(|i| {
            // S (no modifiers) — Toggle skimming
            if kb.is_action_pressed("toggle_skimming", i) {
                self.state.skimming_enabled = !self.state.skimming_enabled;
                // End audio skim session when disabling all skimming
                if !self.state.skimming_enabled {
                    if let Some(ref session) = self.state.skim_audio_session {
                        session.stop();
                    }
                    klipp_engine::audio_output::flush_skim_consumer(&self.skim_consumer);
                    klipp_engine::audio_output::set_skim_consumer(&self.skim_consumer, None);
                    self.state.skim_audio_session = None;
                    self.state.skim_speed_px_per_sec = 0.0;
                }
                let msg = if self.state.skimming_enabled { "Skimming On" } else { "Skimming Off" };
                self.state.toast.show(msg);
            }

            // G (no modifiers) — Connect selected clip to nearest primary clip
            if kb.is_action_pressed("connect_clip", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    self.connect_to_primary(clip_id);
                }
            }

            // Shift+G (no ctrl) — Disconnect selected clip
            if kb.is_action_pressed("disconnect_clip", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    self.disconnect_from_primary(clip_id);
                }
            }

            // Shift+S — Promote/Break storyline OR toggle audio skim.
            // Routed through the keybindings registry (action
            // `toggle_audio_skim`) so users can rebind via the prefs
            // panel; the storyline path takes priority when a clip on
            // a connected storyline is selected.
            if kb.is_action_pressed("toggle_audio_skim", i) {
                let handled_storyline = self.state.selected_clip.is_some_and(|clip_id| self.toggle_storyline(clip_id));
                // If no storyline action applied, toggle audio skim.
                // Routed through the AppCommand so the palette entry
                // and the keybinding share the cleanup path.
                if !handled_storyline {
                    cmds.push(AppCommand::ToggleAudioSkim);
                }
            }

        });
    }

    fn shortcuts_delete(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        // Delete over the Transcript panel is the panel's: it cuts the
        // selected words. Taking the selected clip as well would remove
        // a clip the cut has just split.
        if !delete_reaches_timeline(&self.state) {
            return;
        }
        let kb = self.key_bindings.clone();
        ctx.input(|i| {
            // Delete/Backspace — Remove selected keyframe (if one is selected on timeline)
            let mut keyframe_deleted = false;
            if kb.is_action_pressed("delete_clip", i) {
                // Several keys selected: every one, one step (#1958).
                if let Some(all) = crate::key_selection::delete_all(&self.state) {
                    cmds.extend(all);
                    keyframe_deleted = true;
                    crate::key_selection::clear(&mut self.state);
                } else if let Some(sk) = self.state.selected_keyframe.take() {
                    if let Some(remove) = remove_selected_keyframe(&self.state, &sk) {
                        cmds.extend(remove);
                        keyframe_deleted = true;
                    }
                }
            }

            // Shift+Delete — Lift (leave gap, no ripple) on magnetic tracks
            if !keyframe_deleted && kb.is_action_pressed("lift_clip", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    let track_idx = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                        .position(|t| t.clips.iter().any(|c| c.id == clip_id));
                    if let Some(ti) = track_idx {
                        if self.is_magnetic_track(ti) {
                            let result = klipp_timeline::magnetic::MagneticEditor::lift(
                                &mut self.state.sequences[self.state.active_sequence_idx], clip_id);
                            self.state.push_history(crate::history::EditCommand::Magnetic(result.command));
                            self.state.selected_clip = None;
                            self.sync_sequence_to_engine();
                            self.state.dirty = true;
                        } else {
                            // Off the magnetic track a removal leaves its
                            // gap, which is a lift; Shift+Delete did
                            // nothing there (#911).
                            cmds.push(AppCommand::RemoveClip { clip_id });
                        }
                    }
                }
            }
            // Delete/Backspace — remove selected clip OR extract range (plain, no shift)
            else if !keyframe_deleted && kb.is_action_pressed("delete_clip", i) {
                cmds.extend(delete_selection(&mut self.state));
            }

        });
    }

    fn shortcuts_tools_and_clip_ops(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        let kb = self.key_bindings.clone();
        ctx.input(|i| {
            // Shift+W — Insert/Overwrite. It was on bare W, which the
            // registry gives to trim_in_to_playhead; that one is wired,
            // so a single W did both.
            if kb.is_action_pressed("toggle_edit_mode", i) {
                self.state.edit_mode = match self.state.edit_mode {
                    klipp_timeline::magnetic::EditMode::Insert => klipp_timeline::magnetic::EditMode::Overwrite,
                    klipp_timeline::magnetic::EditMode::Overwrite => klipp_timeline::magnetic::EditMode::Insert,
                };
            }
            // Alt+[ — Ripple trim head to playhead (requires selection).
            if kb.is_action_pressed("ripple_trim_head", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    cmds.push(AppCommand::RippleTrimHead { clip_id });
                }
            }
            // Alt+] — Ripple trim tail to playhead (requires selection).
            if kb.is_action_pressed("ripple_trim_tail", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    cmds.push(AppCommand::RippleTrimTail { clip_id });
                }
            }
            // Q — Trim OUT to playhead (no selection required).
            if kb.is_action_pressed("trim_out_to_playhead", i) {
                cmds.push(AppCommand::TrimOutToPlayhead);
            }
            // W — Trim IN to playhead (no selection required).
            if kb.is_action_pressed("trim_in_to_playhead", i) {
                cmds.push(AppCommand::TrimInToPlayhead);
            }
            // Ctrl+Alt+S — Extract frame to file (dialog).
            if kb.is_action_pressed("extract_frame_to_file", i) {
                cmds.push(AppCommand::FrameGrab);
            }
            // Ctrl+Alt+B — Extract frame and import into the media bin.
            if kb.is_action_pressed("extract_frame_to_bin", i) {
                cmds.push(AppCommand::ExtractFrameToBin);
            }
            // F — Match Frame (program → source).
            if kb.is_action_pressed("match_frame_program_to_source", i) {
                cmds.push(AppCommand::MatchFrameProgramToSource);
            }
            // Shift+F — Match Frame (source → program), cycle to next.
            if kb.is_action_pressed("match_frame_source_to_program", i) {
                cmds.push(AppCommand::MatchFrameSourceToProgram);
            }
            // (`toggle_scrub_audio` action removed — Shift+S now drives
            // `toggle_audio_skim` in shortcuts_magnetic_skim. JKL
            // transport audio still flips through the prefs panel.)
            // Cut clip, Lift and Extract: the timeline's, unless the
            // Mask tool has the keys (#74).
            cmds.extend(removal_keys(&self.state, &kb, i));
            // Ctrl+Shift+B / Ctrl+Shift+I — collapse a side panel to its
            // rail. The View menu has advertised shortcuts for these
            // since it was written; `shortcut_text` only draws a label,
            // so until now they did nothing when pressed.
            if kb.is_action_pressed("toggle_browser_panel", i) {
                cmds.push(AppCommand::ToggleBrowserPanel);
            }
            if kb.is_action_pressed("toggle_inspector_panel", i) {
                cmds.push(AppCommand::ToggleInspectorPanel);
            }
            // B key (bare) — toggle before/after
            if kb.is_action_pressed("toggle_before_after", i) {
                cmds.push(AppCommand::ToggleBeforeAfter);
            }
            // C key (bare) — toggle color panel
            if kb.is_action_pressed("toggle_color_panel", i) {
                cmds.push(AppCommand::ToggleColorPanel);
            }
            // Z key (bare) — toggle zebra overlay
            if kb.is_action_pressed("toggle_zebra", i) {
                self.state.viewer_overlays.zebras = !self.state.viewer_overlays.zebras;
            }
            // Alt+F — toggle false color overlay
            if kb.is_action_pressed("toggle_false_color", i) {
                self.state.viewer_overlays.false_color = !self.state.viewer_overlays.false_color;
            }
            // Ctrl+P — toggle focus peaking overlay
            if i.modifiers.ctrl && !i.modifiers.alt && !i.modifiers.shift
                && i.key_pressed(egui::Key::P)
            {
                self.state.viewer_overlays.focus_peaking = !self.state.viewer_overlays.focus_peaking;
            }
            // Alt+V — toggle scopes: the dock in Color, the floating
            // window elsewhere (the timeline keeps its rows). Bare V is
            // the select tool.
            if kb.is_action_pressed("toggle_scopes", i) {
                self.state.toggle_scopes();
            }
            // Alt+S — toggle safe-areas overlay (action + title).
            if kb.is_action_pressed("toggle_safe_areas", i) {
                self.state.viewer_overlays.safe_areas = !self.state.viewer_overlays.safe_areas;
            }
            // Alt+G — toggle rule-of-thirds grid + centre crosshair overlay.
            if kb.is_action_pressed("toggle_grid_overlay", i) {
                self.state.viewer_overlays.grid = !self.state.viewer_overlays.grid;
            }
            // Shift+V — cycle scope mode
            if kb.is_action_pressed("cycle_scope_mode", i) {
                use crate::state::ScopeMode;
                self.state.scope_mode = match self.state.scope_mode {
                    ScopeMode::Waveform => ScopeMode::Parade,
                    ScopeMode::Parade => ScopeMode::Vectorscope,
                    ScopeMode::Vectorscope => ScopeMode::Histogram,
                    ScopeMode::Histogram => ScopeMode::Waveform,
                };
                if !self.state.scopes_shown() {
                    self.state.toggle_scopes();
                }
                let mode_byte = match self.state.scope_mode {
                    crate::state::ScopeMode::Waveform => 0u8,
                    crate::state::ScopeMode::Parade => 1,
                    crate::state::ScopeMode::Vectorscope => 2,
                    crate::state::ScopeMode::Histogram => 3,
                };
                cmds.push(AppCommand::SetScopeRenderMode { mode: mode_byte });
                cmds.push(AppCommand::SequenceUpdated);
            }
            // Edit points (#913): U selects the one nearest the playhead,
            // and again cycles its side; [ and ] trim it a frame, with
            // Shift ten; E takes it to the playhead.
            if kb.is_action_pressed("select_edit_point", i) {
                cmds.push(if self.state.selected_edits.is_empty() { AppCommand::SelectNearestEditPoint } else { AppCommand::CycleEditSide });
            }
            // Selected clips moved from the keyboard (#917).
            if kb.is_action_pressed("nudge_clips_back", i) {
                cmds.push(AppCommand::NudgeClips { frames: -1 });
            }
            if kb.is_action_pressed("nudge_clips_forward", i) {
                cmds.push(AppCommand::NudgeClips { frames: 1 });
            }
            if kb.is_action_pressed("nudge_clips_back_many", i) {
                cmds.push(AppCommand::NudgeClips { frames: -10 });
            }
            if kb.is_action_pressed("nudge_clips_forward_many", i) {
                cmds.push(AppCommand::NudgeClips { frames: 10 });
            }
            if kb.is_action_pressed("move_clips_up_a_track", i) {
                cmds.push(AppCommand::MoveClipsToTrack { up: true });
            }
            if kb.is_action_pressed("move_clips_down_a_track", i) {
                cmds.push(AppCommand::MoveClipsToTrack { up: false });
            }
            if kb.is_action_pressed("extend_edit", i) {
                cmds.push(AppCommand::ExtendEditPoints);
            }
            if kb.is_action_pressed("play_around_edit", i) {
                cmds.push(AppCommand::PlayAroundEdit);
            }
            if !self.state.selected_edits.is_empty() {
                if kb.is_action_pressed("trim_edit_back", i) {
                    cmds.push(AppCommand::TrimEditPoints { frames: -1 });
                }
                if kb.is_action_pressed("trim_edit_forward", i) {
                    cmds.push(AppCommand::TrimEditPoints { frames: 1 });
                }
                if kb.is_action_pressed("trim_edit_back_many", i) {
                    cmds.push(AppCommand::TrimEditPoints { frames: -10 });
                }
                if kb.is_action_pressed("trim_edit_forward_many", i) {
                    cmds.push(AppCommand::TrimEditPoints { frames: 10 });
                }
            }
            // [ / ], Alt+D, the printer lights and Ctrl+Shift+R: the
            // grade's keys, on the level the node canvas shows (#1742).
            grade_node_keys(&mut self.state, &kb, i, cmds);
            // Copy / Copy Attributes / Copy Color Nodes
            //
            // Two distinct rebindable actions share defaults at Ctrl+C:
            // `copy_clip` (timeline) and `copy_color_nodes` (node graph).
            // When both fire the same frame (the shared-default case),
            // panel context disambiguates — color path wins inside the
            // node graph / color inspector with a node selection;
            // otherwise the clip path wins. When the user explicitly
            // rebinds one to a distinct combo, only that action fires
            // and the panel gate is bypassed (the user's explicit binding
            // is treated as authoritative). egui's TextEdit consumes
            // Ctrl+C internally when a text field has focus, so this
            // dispatch never fights the OS-level text clipboard.
            let in_color_panel = matches!(
                self.state.active_panel,
                Some(crate::state::PanelKind::NodeGraph) | Some(crate::state::PanelKind::NodeInspector)
            );
            let has_node_selection = !self.state.selected_node_ids.is_empty()
                || self.state.selected_node_id.is_some();
            let copy_clip_pressed = kb.is_action_pressed("copy_clip", i);
            let copy_color_pressed = kb.is_action_pressed("copy_color_nodes", i);
            match (copy_color_pressed, copy_clip_pressed) {
                (true, true) => {
                    // Shared-default conflict — panel context decides.
                    if in_color_panel && has_node_selection {
                        if let Some(clip_id) = self.state.selected_clip {
                            cmds.push(AppCommand::CopyColorNodes { clip_id });
                        }
                    } else if let Some(clip_id) = self.state.selected_clip {
                        cmds.push(AppCommand::CopyClip { clip_id });
                    }
                }
                (true, false) => {
                    if let Some(clip_id) = self.state.selected_clip {
                        cmds.push(AppCommand::CopyColorNodes { clip_id });
                    }
                }
                (false, true) => {
                    if let Some(clip_id) = self.state.selected_clip {
                        cmds.push(AppCommand::CopyClip { clip_id });
                    }
                }
                (false, false) => {}
            }
            if kb.is_action_pressed("copy_attributes", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    cmds.push(AppCommand::CopyAttributes { clip_id });
                }
            }
            // Paste / Paste Attributes / Paste Color Nodes
            //
            // Same shape as Ctrl+C: distinct rebindable actions sharing
            // the Ctrl+V default; panel context disambiguates.
            if kb.is_action_pressed("paste_insert", i) {
                cmds.push(AppCommand::PasteClipInsert);
            }
            let paste_clip_pressed = kb.is_action_pressed("paste_clip", i);
            let paste_color_pressed = kb.is_action_pressed("paste_color_nodes", i);
            let color_clipboard_ready = self.state.grade_node_clipboard.as_ref()
                .is_some_and(|c| !c.nodes.is_empty());
            match (paste_color_pressed, paste_clip_pressed) {
                (true, true) => {
                    if in_color_panel && color_clipboard_ready {
                        if let Some(clip_id) = self.state.selected_clip {
                            cmds.push(AppCommand::PasteColorNodes { clip_id });
                        }
                    } else {
                        cmds.push(AppCommand::PasteClip);
                    }
                }
                (true, false) => {
                    if let Some(clip_id) = self.state.selected_clip {
                        cmds.push(AppCommand::PasteColorNodes { clip_id });
                    }
                }
                (false, true) => {
                    cmds.push(AppCommand::PasteClip);
                }
                (false, false) => {}
            }
            if kb.is_action_pressed("paste_attributes", i)
                && self.state.attributes_clipboard.is_some() && self.state.selected_clip.is_some() {
                    self.state.show_paste_attributes_dialog = true;
                }
            // Duplicate clip
            if kb.is_action_pressed("duplicate_clip", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    cmds.push(AppCommand::DuplicateClip { clip_id });
                }
            }
            // Split at playhead
            if kb.is_action_pressed("split_at_playhead", i) {
                cmds.extend(self.state.split_at_playhead_commands());
            }
            // Freeze frame at playhead (default 2s duration).
            // The default transition at the nearest cut (#918).
            if kb.is_action_pressed("add_default_transition", i) {
                cmds.push(AppCommand::AddDefaultTransition { audio: false });
            }
            if kb.is_action_pressed("add_audio_crossfade", i) {
                cmds.push(AppCommand::AddDefaultTransition { audio: true });
            }
            if kb.is_action_pressed("freeze_frame", i) {
                cmds.push(AppCommand::InsertFreezeFrame { duration_secs: 2.0 });
            }
            // Toggle loop playback
            if kb.is_action_pressed("toggle_loop", i) {
                cmds.push(AppCommand::ToggleLoopPlayback);
            }
        });
    }

    fn shortcuts_title_and_tools2(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        let kb = self.key_bindings.clone();
        let mut pending_perf_recording_toggle = false;
        ctx.input(|i| {
            // Ctrl+T — a title at the playhead.
            if kb.is_action_pressed("create_title", i) {
                cmds.push(AppCommand::AddTitleAtPlayhead { over_clip: None });
            }
            // B — Blade tool
            if kb.is_action_pressed("blade_tool", i) {
                tracing::debug!("Blade tool");
                cmds.push(AppCommand::SetActiveTool(Tool::Blade));
            }
            // V — Select tool
            if kb.is_action_pressed("select_tool", i) {
                tracing::debug!("Select tool");
                cmds.push(AppCommand::SetActiveTool(Tool::Select));
            }
            // R — Range tool toggle (Shift+R in the source viewer rejects
            // a stretch instead; see the marker keys)
            let rejecting = i.modifiers.shift && self.state.viewer_mode == ViewerMode::Source && self.state.source_media_id.is_some();
            if !rejecting && kb.is_action_pressed("range_tool", i) {
                if self.state.active_tool == Tool::Range {
                    cmds.push(AppCommand::SetActiveTool(Tool::Select));
                } else {
                    cmds.push(AppCommand::SetActiveTool(Tool::Range));
                }
            }
            // T — Trim tool (bare T; Ctrl+T handled elsewhere for Create Title)
            if kb.is_action_pressed("trim_tool", i) {
                tracing::debug!("Trim tool");
                cmds.push(AppCommand::SetActiveTool(Tool::Trim));
            }
            // Y — Text tool: a click or a drag in the viewer makes a
            // title there (#1713)
            if kb.is_action_pressed("text_tool", i) {
                cmds.push(AppCommand::SetActiveTool(Tool::Text));
            }
            // P — Mask tool: draw and edit a freeform mask in the viewer
            // (#74)
            if kb.is_action_pressed("mask_tool", i) {
                cmds.push(AppCommand::SetActiveTool(Tool::Mask));
            }
            // X — Set range to clip at playhead / Alt+X — Clear range
            // Clearing the range used to live in here as an Alt branch
            // as well as in its own clear_in_out handler, so Alt+X ran
            // it twice and left two entries in the history.
            if kb.is_action_pressed("set_range_to_clip", i) {
                {
                    // X: Set range to clip at playhead
                    let ph = self.state.playhead;
                    let pushed = {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        let mut found_clip: Option<(klipp_core::Timecode, klipp_core::Timecode, Option<klipp_core::Timecode>, Option<klipp_core::Timecode>)> = None;
                        for track in &seq.tracks {
                            if let Some((_, clip)) = track.clip_at_timecode(ph) {
                                found_clip = Some((clip.timeline_in, clip.timeline_out, seq.in_point, seq.out_point));
                                break;
                            }
                        }
                        found_clip
                    };
                    if let Some((tl_in, tl_out, old_in, old_out)) = pushed {
                        cmds.push(AppCommand::SetSequenceInPoint(tl_in));
                        cmds.push(AppCommand::SetSequenceOutPoint(tl_out));
                        self.state.push_history(crate::history::EditCommand::SetRange {
                            old_in, old_out,
                            new_in: Some(tl_in),
                            new_out: Some(tl_out),
                        });
                        self.state.toast.show("Range: clip selected");
                    }
                }
            }
            // Fullscreen viewer, on F11 — the combo every desktop
            // agrees on. It was on bare F, which is Match Frame, so a
            // single press ran both: you matched the frame and the
            // viewer went fullscreen underneath you.
            if kb.is_action_pressed("toggle_fullscreen", i) {
                cmds.push(AppCommand::ToggleFullscreen);
            }
            // Ctrl+Shift+P — Toggle performance overlay
            if kb.is_action_pressed("toggle_perf_overlay", i) {
                self.state.show_perf_overlay = !self.state.show_perf_overlay;
            }
            // Ctrl+Alt+P — Toggle perf recording session. Captured
            // as a boolean here; the actual ring-buffer mark/dump
            // happens in `update` after the input phase so the
            // borrow on self can release cleanly.
            if kb.is_action_pressed("toggle_perf_recording", i) {
                pending_perf_recording_toggle = true;
            }
        });
        if pending_perf_recording_toggle {
            self.toggle_perf_recording();
        }
    }

    fn shortcuts_multicam_and_io(&mut self, ctx: &egui::Context, text_editing: bool, cmds: &mut Vec<AppCommand>) {
        let kb = self.key_bindings.clone();
        ctx.input(|i| {
            // 1-9 — Switch multicam angle (via keybinding system for remapping support)
            if !text_editing {
                for angle in 1usize..=9 {
                    let action = format!("multicam_angle_{angle}");
                    if kb.is_action_pressed(&action, i) {
                        cmds.push(AppCommand::SwitchMulticamAngle { angle });
                    }
                }
                if kb.is_action_pressed("create_multicam", i) {
                    cmds.push(AppCommand::CreateMulticamClip);
                }
                // Multicam mode in the Source viewer (#1844).
                if kb.is_action_pressed("toggle_multicam_mode", i) {
                    cmds.push(crate::multicam_grid::switch_mode(self.state.multicam_mode));
                }
            }
            // N — Toggle snap (dispatches through AppCommand so layout
            // persists on flip, matching the toolbar / palette / View
            // menu surfaces).
            if kb.is_action_pressed("toggle_snap", i) {
                cmds.push(AppCommand::SetSnapMaster(!self.state.snap.master));
            }
            // I/O — In/Out markers (context-dependent on viewer mode)
            if kb.is_action_pressed("clear_in_point", i) || kb.is_action_pressed("set_in_point", i) {
                if kb.is_action_pressed("clear_in_point", i) {
                    tracing::debug!("Clear sequence in point");
                    let old_in = self.state.sequence().in_point;
                    let old_out = self.state.sequence().out_point;
                    cmds.push(AppCommand::ClearSequenceInPoint);
                    self.state.active_history_mut().push(crate::history::HistoryEntry {
                        command: crate::history::EditCommand::SetRange {
                            old_in, old_out, new_in: None, new_out: old_out,
                        },
                    });
                } else if self.state.viewer_mode == ViewerMode::Source {
                    tracing::debug!("I key (Source mode) -> Set source in point at {:?}", self.state.source_playhead);
                    cmds.push(AppCommand::SetSourceInPoint(self.state.source_playhead));
                } else {
                    tracing::debug!("I key (Program mode) -> Set sequence in point at {:?}", self.state.playhead);
                    let old_in = self.state.sequence().in_point;
                    let old_out = self.state.sequence().out_point;
                    let new_in = Some(self.state.playhead);
                    cmds.push(AppCommand::SetSequenceInPoint(self.state.playhead));
                    self.state.active_history_mut().push(crate::history::HistoryEntry {
                        command: crate::history::EditCommand::SetRange {
                            old_in, old_out, new_in, new_out: old_out,
                        },
                    });
                }
            }
            if kb.is_action_pressed("clear_out_point", i) || kb.is_action_pressed("set_out_point", i) {
                if kb.is_action_pressed("clear_out_point", i) {
                    tracing::debug!("Clear sequence out point");
                    let old_in = self.state.sequence().in_point;
                    let old_out = self.state.sequence().out_point;
                    cmds.push(AppCommand::ClearSequenceOutPoint);
                    self.state.active_history_mut().push(crate::history::HistoryEntry {
                        command: crate::history::EditCommand::SetRange {
                            old_in, old_out, new_in: old_in, new_out: None,
                        },
                    });
                } else if self.state.viewer_mode == ViewerMode::Source {
                    tracing::debug!("O key (Source mode) -> Set source out point at {:?}", self.state.source_playhead);
                    cmds.push(AppCommand::SetSourceOutPoint(self.state.source_playhead));
                } else {
                    tracing::debug!("O key (Program mode) -> Set sequence out point at {:?}", self.state.playhead);
                    let old_in = self.state.sequence().in_point;
                    let old_out = self.state.sequence().out_point;
                    let new_out = Some(self.state.playhead);
                    cmds.push(AppCommand::SetSequenceOutPoint(self.state.playhead));
                    self.state.active_history_mut().push(crate::history::HistoryEntry {
                        command: crate::history::EditCommand::SetRange {
                            old_in, old_out, new_in: old_in, new_out,
                        },
                    });
                }
            }

            // Alt+X — clear both in/out points (standard NLE shortcut)
            if kb.is_action_pressed("clear_in_out", i) {
                let old_in = self.state.sequence().in_point;
                let old_out = self.state.sequence().out_point;
                if old_in.is_some() || old_out.is_some() {
                    cmds.push(AppCommand::ClearSequenceInPoint);
                    cmds.push(AppCommand::ClearSequenceOutPoint);
                    self.state.active_history_mut().push(crate::history::HistoryEntry {
                        command: crate::history::EditCommand::SetRange {
                            old_in, old_out, new_in: None, new_out: None,
                        },
                    });
                }
            }

        });
    }

    fn shortcuts_edit_nav(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        let kb = self.key_bindings.clone();
        ctx.input(|i| {
            // Escape — deselect clips, clear range drag, or return Range tool to Select
            if i.key_pressed(egui::Key::Escape) {
                // Unlock scope cursor on Escape
                if self.state.mask_keys_owned() {
                    // The Mask tool's (#74): it drops the drawn shape or
                    // the point selection itself.
                } else if self.state.dynamic_trim.is_some() {
                    // A dynamic trim goes back to its cut (#914).
                    cmds.push(AppCommand::CancelDynamicTrim);
                } else if self.state.surface_tool.is_some() {
                    // The viewer's surface tool goes; the clip stays selected.
                    self.state.surface_tool = None;
                } else if self.state.scope_cursor_locked {
                    self.state.scope_cursor_locked = false;
                    self.state.scope_cursor_locked_pos = None;
                    self.state.scope_cursor_locked_data = None;
                } else if let Some(drag) = self.state.range_drag.take() {
                    self.state.sequences[self.state.active_sequence_idx].in_point = drag.original_in;
                    self.state.sequences[self.state.active_sequence_idx].out_point = drag.original_out;
                } else if !self.state.selected_edits.is_empty() {
                    self.state.selected_edits.clear();
                } else if self.state.selected_clip.is_some() || !self.state.selected_clips.is_empty() {
                    self.state.selected_clips.clear();
                    cmds.push(AppCommand::SelectClip(None));
                } else if matches!(self.state.active_tool, Tool::Range | Tool::Text | Tool::Mask) {
                    // Esc puts the Range, Text and Mask tools down (#1713,
                    // #74).
                    cmds.push(AppCommand::SetActiveTool(Tool::Select));
                }
                self.state.ruler_press_origin = None;
            }
            // Deselect All
            if kb.is_action_pressed("deselect_all", i) {
                self.state.selected_clips.clear();
                cmds.push(AppCommand::SelectClip(None));
            }
            // Select All clips
            if kb.is_action_pressed("select_all", i) {
                cmds.push(AppCommand::SelectAllClips);
            }
            // The selected clip's track: all of it, or from the playhead on
            // or back (#844).
            if kb.is_action_pressed("select_track_clips", i) {
                cmds.push(AppCommand::SelectClipsOnSelectedTrack { range: klipp_core::SelectRange::All });
            }
            if kb.is_action_pressed("select_track_forward", i) {
                cmds.push(AppCommand::SelectClipsOnSelectedTrack { range: klipp_core::SelectRange::Forward });
            }
            if kb.is_action_pressed("select_track_backward", i) {
                cmds.push(AppCommand::SelectClipsOnSelectedTrack { range: klipp_core::SelectRange::Backward });
            }
            if kb.is_action_pressed("select_forward", i) {
                cmds.push(AppCommand::SelectClipsForward);
            }
            if kb.is_action_pressed("select_backward", i) {
                cmds.push(AppCommand::SelectClipsBackward);
            }
            // Previous edit point
            // Ctrl+Up: out of an opened compound clip, to the sequence
            // it sits in (#910).
            if kb.is_action_pressed("leave_compound", i) && !self.state.compound_clip_stack.is_empty() {
                cmds.push(AppCommand::CloseCompoundClip);
            }
            if kb.is_action_pressed("prev_edit_point", i) {
                cmds.push(AppCommand::PrevEditPoint);
            }
            // Next edit point
            if kb.is_action_pressed("next_edit_point", i) {
                cmds.push(AppCommand::NextEditPoint);
            }
            // Insert edit (ripple)
            if kb.is_action_pressed("insert_edit", i)
            {
                cmds.push(AppCommand::InsertEdit);
            }
            // Overwrite edit
            if kb.is_action_pressed("overwrite_edit", i) {
                cmds.push(AppCommand::OverwriteEdit);
            }
            // Fit to fill, place on top, replace (#919)
            if kb.is_action_pressed("fit_to_fill_edit", i) {
                cmds.push(AppCommand::FitToFillEdit);
            }
            if kb.is_action_pressed("place_on_top_edit", i) {
                cmds.push(AppCommand::PlaceOnTopEdit);
            }
            if kb.is_action_pressed("replace_edit", i) {
                cmds.push(AppCommand::ReplaceEdit);
            }
        });
    }

    fn shortcuts_keyframe_and_zoom(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        let kb = self.key_bindings.clone();
        ctx.input(|i| {
            // Alt+M — Toggle matte preview
            if kb.is_action_pressed("toggle_matte_preview", i) {
                self.state.matte_preview_active = !self.state.matte_preview_active;
                cmds.push(AppCommand::SetMattePreview(self.state.matte_preview_active));
            }
            // K — Toggle keyframe at playhead for the active parameter
            if kb.is_action_pressed("toggle_keyframe", i) {
                let mut keys_in_frame = 0;
                if let Some(clip_id) = self.state.selected_clip {
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    // Find the clip and its track kind
                    let clip_and_kind = seq.tracks.iter().find_map(|t| {
                        t.clips.iter().find(|c| c.id == clip_id).map(|c| (c, t.kind))
                    });
                    if let Some((clip, track_kind)) = clip_and_kind {
                        let playhead_secs = self.state.playhead.to_secs_f64();
                        let clip_start_secs = clip.timeline_in.to_secs_f64();
                        let clip_end_secs = clip.timeline_out.to_secs_f64();
                        if playhead_secs >= clip_start_secs && playhead_secs <= clip_end_secs {
                            let clip_time_secs = playhead_secs - clip_start_secs;
                            let tolerance = crate::inspector::keyframe_selection::frame_tolerance(seq);
                            if track_kind == klipp_timeline::TrackKind::Audio {
                                match self.state.rubber_band_param.clone() {
                                    crate::state::RubberBandParam::ClipParam(param) => {
                                        let keyframes = match param {
                                            klipp_core::AudioParam::Volume => &clip.volume_keyframes,
                                            klipp_core::AudioParam::Pan => &clip.pan_keyframes,
                                            klipp_core::AudioParam::Gain => &clip.gain_keyframes,
                                        };
                                        keys_in_frame = crate::inspector::keyframe_selection::state(keyframes, clip_time_secs, tolerance).keys_in_frame;
                                        let existing = crate::inspector::keyframe_selection::nearest(keyframes, clip_time_secs, tolerance).map(|i| &keyframes[i]);
                                        if existing.is_some() {
                                            cmds.push(AppCommand::EditAudioKeyframeAtFrame { clip_id, param, time_secs: clip_time_secs, value: None });
                                        } else {
                                            let default_val = match param {
                                                klipp_core::AudioParam::Volume => clip.volume,
                                                klipp_core::AudioParam::Pan => clip.pan,
                                                klipp_core::AudioParam::Gain => clip.gain,
                                            };
                                            let value = klipp_core::keyframe::evaluate(keyframes, clip_time_secs, default_val);
                                            cmds.push(AppCommand::EditAudioKeyframeAtFrame { clip_id, param, time_secs: clip_time_secs, value: Some(value) });
                                        }
                                    }
                                    crate::state::RubberBandParam::EffectParam { effect_index, param_name } => {
                                        let key = format!("{effect_index}:{param_name}");
                                        let empty: Vec<klipp_core::Keyframe> = Vec::new();
                                        let kfs_ref = clip.audio_effect_keyframes.get(&key).unwrap_or(&empty);
                                        keys_in_frame = crate::inspector::keyframe_selection::state(kfs_ref, clip_time_secs, tolerance).keys_in_frame;
                                        let existing = crate::inspector::keyframe_selection::nearest(kfs_ref, clip_time_secs, tolerance).map(|i| &kfs_ref[i]);
                                        if let Some(kf) = existing {
                                            cmds.push(AppCommand::RemoveAudioEffectKeyframe {
                                                clip_id, effect_index, param_name: param_name.clone(), time_secs: kf.time_secs,
                                            });
                                        } else {
                                            let default_val = clip.audio_effects.get(effect_index)
                                                .and_then(|inst| inst.params.iter().find(|(k, _)| k == &param_name).map(|(_, v)| *v))
                                                .unwrap_or(0.0);
                                            let value = klipp_core::keyframe::evaluate(kfs_ref, clip_time_secs, default_val);
                                            cmds.push(AppCommand::AddAudioEffectKeyframe {
                                                clip_id, effect_index, param_name, time_secs: clip_time_secs, value,
                                            });
                                        }
                                    }
                                }
                            } else {
                                let param = self.state.video_rubber_band_param;
                                // The lane as the picture holds it (M4 step 12c).
                                let lane = crate::inspector::placement_by_path::param_keys(clip, param);
                                let keyframes = &lane[..];
                                keys_in_frame = crate::inspector::keyframe_selection::state(keyframes, clip_time_secs, tolerance).keys_in_frame;
                                        let existing = crate::inspector::keyframe_selection::nearest(keyframes, clip_time_secs, tolerance).map(|i| &keyframes[i]);
                                if let Some(kf) = existing {
                                    cmds.push(AppCommand::RemoveVideoKeyframe { clip_id, param, time_secs: kf.time_secs });
                                } else {
                                    // From the clip's placement and opacity as the
                                    // renderer reads them (M3 step 4).
                                    let default_val = clip.video_param_value(param);
                                    let value = klipp_core::keyframe::evaluate(keyframes, clip_time_secs, default_val);
                                    cmds.push(AppCommand::AddVideoKeyframe { clip_id, param, time_secs: clip_time_secs, value });
                                }
                            }
                        }
                    }
                }
                if keys_in_frame > 1 { self.state.toast.show(format!("{keys_in_frame} keys share this frame; K edits the nearest. Use Shift+K for individual subframe keys.")); }
            }
            // Shift+K — Toggle curve editor (keyframe editor) panel
            if kb.is_action_pressed("toggle_keyframe_editor", i) {
                self.state.show_curve_editor = !self.state.show_curve_editor;
                self.state.show_keyframe_editor = self.state.show_curve_editor;
            }
            // Ctrl+Shift+M — Toggle audio mixer panel
            // Audio mixer and subtitle editor moved to the keybinding
            // registry (toggle_audio_mixer / toggle_subtitle_editor) and
            // are handled in shortcuts_ctrl_global. Handling them here
            // too toggled each twice a frame, which is the same as not
            // at all.
            // Ctrl+Shift+N — Toggle Project Notes panel
            if kb.is_action_pressed("toggle_project_notes", i) {
                self.state.show_project_notes = !self.state.show_project_notes;
            }
            // The stacked timelines' focus swapped (#2324).
            if kb.is_action_pressed("swap_stacked_focus", i) {
                self.state.stacked_focus_swap = true;
            }
            // Alt+Left — Navigate to previous keyframe
            if kb.is_action_pressed("prev_keyframe", i) {
                if let Some(to) = keyframe_jump(&self.state, false) {
                    self.state.playhead = to;
                    let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
                }
            }
            // Alt+Right — Navigate to next keyframe
            if kb.is_action_pressed("next_keyframe", i) {
                if let Some(to) = keyframe_jump(&self.state, true) {
                    self.state.playhead = to;
                    let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
                }
            }
            // The selected keys' ease, wherever they are selected, one
            // step (#2307).
            use crate::widgets::scrubby_param::EasePreset;
            if kb.is_action_pressed("key_ease_in", i) {
                cmds.extend(key_ease_shortcut(&self.state, Some(EasePreset::EaseIn)));
            }
            if kb.is_action_pressed("key_ease_out", i) {
                cmds.extend(key_ease_shortcut(&self.state, Some(EasePreset::EaseOut)));
            }
            if kb.is_action_pressed("key_ease_in_out", i) {
                cmds.extend(key_ease_shortcut(&self.state, Some(EasePreset::EaseBoth)));
            }
            if kb.is_action_pressed("key_linear", i) {
                cmds.extend(key_ease_shortcut(&self.state, None));
            }
            // In the source viewer, marks go on the pool clip: M a note at
            // the playhead, Shift+F a favourite stretch, Shift+R a rejected
            // one — the stretch between the in and out points, or the
            // whole clip when none are set.
            let in_source = self.state.viewer_mode == ViewerMode::Source && self.state.source_media_id.is_some();
            if in_source && kb.is_action_pressed("add_marker", i) {
                if let Some(media_id) = self.state.source_media_id {
                    cmds.push(AppCommand::AddMediaMarker { media_id, marker: klipp_core::MediaMarker::note(self.state.source_playhead.to_secs_f64()) });
                }
            }
            for (key, kind) in [(egui::Key::F, klipp_core::MediaMarkerKind::Favourite), (egui::Key::R, klipp_core::MediaMarkerKind::Reject)] {
                if in_source && i.key_pressed(key) && i.modifiers.shift && !i.modifiers.ctrl && !i.modifiers.alt {
                    if let Some(marker) = self.source_range_marker(kind) {
                        if let Some(media_id) = self.state.source_media_id {
                            cmds.push(AppCommand::AddMediaMarker { media_id, marker });
                        }
                    }
                }
            }
            // M — Add sequence marker (no modifiers, no shift)
            if !in_source && kb.is_action_pressed("add_marker", i) {
                cmds.push(AppCommand::AddMarker {
                    position: self.state.playhead,
                    label: String::new(),
                    color: klipp_core::MarkerColor::Blue,
                });
            }
            // Shift+M — Add per-clip marker at playhead on selected clip
            if kb.is_action_pressed("add_clip_marker", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    // Find the clip and compute the time relative to source_in
                    let clip_info = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == clip_id)
                        .map(|c| (c.timeline_in, c.timeline_out));
                    if let Some((tl_in, tl_out)) = clip_info {
                        let playhead_secs = self.state.playhead.to_secs_f64();
                        let clip_start_secs = tl_in.to_secs_f64();
                        let clip_end_secs = tl_out.to_secs_f64();
                        if playhead_secs >= clip_start_secs && playhead_secs <= clip_end_secs {
                            let rel_time = playhead_secs - clip_start_secs;
                            cmds.push(AppCommand::AddClipMarker {
                                clip_id,
                                time_secs: rel_time,
                                name: String::new(),
                                color: klipp_core::MarkerColor::Green,
                            });
                        }
                    }
                }
            }
            // Z — Fit to window (no modifiers)
            if kb.is_action_pressed("fit_to_window", i) {
                cmds.push(AppCommand::FitTimelineToWindow);
            }
            // Shift+Z — Zoom to selection
            if kb.is_action_pressed("zoom_to_selection", i) {
                cmds.push(AppCommand::ZoomToSelection);
            }
            // + / = — Zoom in (playhead-centred)
            // `=` is also the zoom_in key, handled below: taking it here too
            // zoomed twice per press (#911).
            if i.events.iter().any(|e| matches!(e, egui::Event::Text(s) if s == "+" || s == "="))
                && !i.modifiers.ctrl
                && !kb.is_action_pressed("zoom_in", i)
            {
                let playhead_secs = self.state.playhead_smooth_secs as f32;
                let old_zoom = self.state.timeline_zoom;
                let new_zoom = (old_zoom * self.state.zoom_step).clamp(0.1, 2000.0);
                self.state.timeline_scroll_x = (playhead_secs * new_zoom
                    - (playhead_secs * old_zoom - self.state.timeline_scroll_x))
                    .max(0.0);
                self.state.timeline_zoom = new_zoom;
            }
            // - — Zoom out (playhead-centred)
            if kb.is_action_pressed("zoom_out", i) {
                self.zoom_timeline_by(1.0 / self.state.zoom_step);
            }
            // Zoom in had a binding and a menu entry but no handler at
            // all: the View menu said "=" and the key did nothing.
            if kb.is_action_pressed("zoom_in", i) {
                self.zoom_timeline_by(self.state.zoom_step);
            }
        });
    }

    /// The typed trim amount for the selected edit points: + or - starts
    /// it, digits and : extend it, Backspace takes one off, Enter trims by
    /// it and Escape drops it. True while it holds the keys.
    fn trim_entry_keys(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) -> bool {
        // For the selected edit points, else the selected clips: +12
        // moves them twelve frames later (#917).
        let has_clips = self.state.selected_clip.is_some() || !self.state.selected_clips.is_empty();
        if self.state.selected_edits.is_empty() && !has_clips {
            self.state.trim_entry = None;
            return false;
        }
        let edits = !self.state.selected_edits.is_empty();
        let fps = self.state.sequence().fps.as_f64();
        let entry = &mut self.state.trim_entry;
        let mut held = entry.is_some();
        ctx.input(|i| {
            for event in &i.events {
                match event {
                    egui::Event::Text(text) if !i.modifiers.ctrl && !i.modifiers.alt => {
                        for ch in text.chars() {
                            match (entry.as_mut(), ch) {
                                (None, '+' | '-') => {
                                    *entry = Some(ch.to_string());
                                    held = true;
                                }
                                (Some(typed), '0'..='9' | ':') => typed.push(ch),
                                _ => {}
                            }
                        }
                    }
                    egui::Event::Key { key: egui::Key::Backspace, pressed: true, .. } => {
                        if let Some(typed) = entry.as_mut() {
                            typed.pop();
                            if typed.is_empty() {
                                *entry = None;
                            }
                        }
                    }
                    egui::Event::Key { key: egui::Key::Enter, pressed: true, .. } => {
                        if let Some(frames) = entry.take().and_then(|typed| crate::edit_point::parse_amount(&typed, fps)) {
                            cmds.push(if edits { AppCommand::TrimEditPoints { frames } } else { AppCommand::NudgeClips { frames } });
                        }
                    }
                    egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } if entry.take().is_some() => {
                        held = true;
                    }
                    _ => {}
                }
            }
        });
        held
    }
}

/// Where Alt+← (`forward` false) or Alt+→ takes the playhead: the previous
/// or next key, by the inspector rows' rule (`keyframe_selection::state`),
/// of the track being looked at on the selected clip. That is the curve
/// editor's selected effect track while it shows one on that clip (#1401),
/// else the clip's rubber-band parameter. `None` when there's no key that
/// way.
pub(super) fn keyframe_jump(state: &crate::state::AppState, forward: bool) -> Option<klipp_core::Timecode> {
    let clip_id = state.selected_clip?;
    let seq = &state.sequences[state.active_sequence_idx];
    let (clip, track_kind) = seq.tracks.iter().find_map(|t| t.clips.iter().find(|c| c.id == clip_id).map(|c| (c, t.kind)))?;
    let empty: Vec<klipp_core::Keyframe> = Vec::new();
    let node_keys: Vec<klipp_core::Keyframe>;
    // The video rubber band's lane as the picture holds it (M4 step 12c).
    let video_lane = crate::inspector::placement_by_path::param_keys(clip, state.video_rubber_band_param);
    // A mask's track (#1947): the curve editor's, else the one the
    // timeline's key lane shows on the clip.
    let mask_track = match &state.curve_editor_mask_track {
        Some((on, t)) if *on == clip_id && state.show_curve_editor => Some(t.clone()),
        _ => crate::timeline::mask_lane::shown(state, clip, ["opacity", "pos_x", "pos_y", "scale", "rotation"].iter().any(|k| !crate::inspector::placement_by_path::lane_keys(clip, k).is_empty())),
    };
    let keyframes: &[klipp_core::Keyframe] = match &state.curve_editor_track {
        Some((on, path, key)) if *on == clip_id && state.show_curve_editor => {
            node_keys = clip.node_key_state(path, key).map(|s| s.keys).unwrap_or_default();
            &node_keys
        }
        _ if mask_track.is_some() => {
            node_keys = mask_track.as_ref().map(|t| crate::mask_tracks::keys(clip, t)).unwrap_or_default();
            &node_keys
        }
        _ if track_kind == klipp_timeline::TrackKind::Audio => match &state.rubber_band_param {
            crate::state::RubberBandParam::ClipParam(param) => match param {
                klipp_core::AudioParam::Volume => &clip.volume_keyframes,
                klipp_core::AudioParam::Pan => &clip.pan_keyframes,
                klipp_core::AudioParam::Gain => &clip.gain_keyframes,
            },
            crate::state::RubberBandParam::EffectParam { effect_index, param_name } => clip.audio_effect_keyframes.get(&format!("{effect_index}:{param_name}")).unwrap_or(&empty),
        },
        _ => &video_lane,
    };
    let clip_time_secs = state.playhead.to_secs_f64() - clip.timeline_in.to_secs_f64();
    let around = crate::inspector::keyframe_selection::state(keyframes, clip_time_secs, crate::inspector::keyframe_selection::frame_tolerance(seq));
    let time = if forward { around.next_keyframe_time } else { around.prev_keyframe_time }?;
    Some(klipp_core::Timecode::from_secs_f64(clip.timeline_in.to_secs_f64() + time, state.playhead.rate))
}

/// The command that takes the selected keyframe `sk` off its track, as
/// Delete does: by the time of its key, which the track holds at its
/// index. `None` when the clip or the key is gone. One place, so the
/// shortcut and the curve editor's family apply it alike.
pub(super) fn remove_selected_keyframe(state: &crate::state::AppState, sk: &crate::state::SelectedKeyframe) -> Option<Vec<AppCommand>> {
    let seq = &state.sequences[state.active_sequence_idx];
    let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == sk.clip_id)?;
    let kf_time = match &sk.param_kind {
        crate::state::KeyframeParamKind::Title { object_id, param } => param.keyframes(clip, *object_id).get(sk.keyframe_index).map(|kf| kf.time_secs),
        crate::state::KeyframeParamKind::Audio(param) => {
            let kfs = match param {
                klipp_core::AudioParam::Volume => &clip.volume_keyframes,
                klipp_core::AudioParam::Pan => &clip.pan_keyframes,
                klipp_core::AudioParam::Gain => &clip.gain_keyframes,
            };
            kfs.get(sk.keyframe_index).map(|kf| kf.time_secs)
        }
        crate::state::KeyframeParamKind::Video(param) => {
            // The lane as the picture holds it (M4 step 12c).
            let kfs = crate::inspector::placement_by_path::param_keys(clip, *param);
            kfs.get(sk.keyframe_index).map(|kf| kf.time_secs)
        }
        crate::state::KeyframeParamKind::AudioEffect { effect_index, param_name } => {
            clip.audio_effect_keyframes
                .get(&format!("{effect_index}:{param_name}"))
                .and_then(|kfs| kfs.get(sk.keyframe_index).map(|kf| kf.time_secs))
        }
        crate::state::KeyframeParamKind::Node { path, key } => {
            clip.node_key_state(path, key).and_then(|s| s.keys.get(sk.keyframe_index).map(|kf| kf.time_secs))
        }
        crate::state::KeyframeParamKind::Mask(track) => crate::mask_tracks::keys(clip, track).get(sk.keyframe_index).map(|kf| kf.time_secs),
    };
    let time_secs = kf_time?;
    let cmd = match &sk.param_kind {
        crate::state::KeyframeParamKind::Title { object_id, param } => param.remove(sk.clip_id, *object_id, time_secs),
        crate::state::KeyframeParamKind::Audio(param) => {
            AppCommand::RemoveAudioKeyframe { clip_id: sk.clip_id, param: *param, time_secs }
        }
        crate::state::KeyframeParamKind::Video(param) => {
            AppCommand::RemoveVideoKeyframe { clip_id: sk.clip_id, param: *param, time_secs }
        }
        crate::state::KeyframeParamKind::AudioEffect { effect_index, param_name } => {
            AppCommand::RemoveAudioEffectKeyframe {
                clip_id: sk.clip_id,
                effect_index: *effect_index,
                param_name: param_name.clone(),
                time_secs,
            }
        }
        crate::state::KeyframeParamKind::Node { path, key } => {
            AppCommand::RemoveKeyAt { addr: path.clone(), key: key.clone(), time_secs }
        }
        // A mask's key; the shape's last one off keeps the shape (#1947).
        crate::state::KeyframeParamKind::Mask(track) => AppCommand::RemoveMaskKeyframe {
            clip_id: sk.clip_id,
            node_id: track.node,
            mask_index: track.mask,
            param: track.param.clone(),
            time_secs,
        },
    };
    // A placed kind's key (a Temporal Denoise's) goes through the clip's
    // effect list, as its card takes one off (#1419).
    Some(crate::inspector::stack_view::route_placed(clip, vec![cmd])).filter(|c| !c.is_empty())
}

/// The grade's keys (#1742), acting on the level of the grade the node
/// canvas shows (`graph_descent_path`), not only its top:
///
/// - `[` / `]` select the level's previous or next node, Source and
///   Output passed over.
/// - Alt+D bypasses the selected node and Ctrl+Shift+R resets it, by
///   command, so each is one undo step wherever the node is.
/// - The printer lights.
///
/// With the grade canvas focused, its own copy, paste and duplicate are
/// the canvas's (#1745), which the clip's copy and paste step aside for.
pub(super) fn grade_node_keys(
    state: &mut crate::state::AppState,
    kb: &crate::keybindings::KeyBindings,
    i: &egui::InputState,
    cmds: &mut Vec<AppCommand>,
) {
    let Some(clip_id) = state.selected_clip else { return };
    if kb.is_action_pressed("prev_color_node", i) || kb.is_action_pressed("next_color_node", i) {
        let forward = kb.is_action_pressed("next_color_node", i);
        let level: Vec<klipp_nodes::NodeId> = state
            .sequence()
            .all_clips()
            .find(|c| c.id == clip_id)
            .and_then(|c| grade_level(c.grade(), &state.graph_descent_path))
            .map(|g| g.nodes.iter().filter(|n| !matches!(n.kind, klipp_nodes::NodeKind::Source | klipp_nodes::NodeKind::Output)).map(|n| n.id).collect())
            .unwrap_or_default();
        if !level.is_empty() {
            let at = state.selected_node_id.and_then(|sel| level.iter().position(|id| *id == sel));
            let next = match at {
                Some(at) if forward => (at + 1) % level.len(),
                Some(at) => (at + level.len() - 1) % level.len(),
                None if forward => 0,
                None => level.len() - 1,
            };
            // One node selected, as a keyboard step through a list is.
            state.selected_node_id = Some(level[next]);
            state.selected_node_ids.clear();
            state.selected_node_ids.insert(level[next]);
        }
    }
    if let Some(node_id) = state.selected_node_id {
        if kb.is_action_pressed("toggle_node_bypass", i) {
            cmds.push(AppCommand::ToggleNodeEnabled { clip_id, node_id });
        }
        if kb.is_action_pressed("reset_current_node", i) {
            cmds.push(AppCommand::ResetNode { clip_id, node_id });
        }
    }
    // Printer lights (#241): a channel's offset up or down a point.
    for (pressed, channel, points) in [
        (kb.is_action_pressed("printer_red_up", i), 0u8, 1),
        (kb.is_action_pressed("printer_red_down", i), 0, -1),
        (kb.is_action_pressed("printer_green_up", i), 1, 1),
        (kb.is_action_pressed("printer_green_down", i), 1, -1),
        (kb.is_action_pressed("printer_blue_up", i), 2, 1),
        (kb.is_action_pressed("printer_blue_down", i), 2, -1),
        (kb.is_action_pressed("printer_master_up", i), 3, 1),
        (kb.is_action_pressed("printer_master_down", i), 3, -1),
    ] {
        if pressed {
            cmds.push(AppCommand::PrinterLight { channel, points });
        }
    }
}

/// The graph at `path` in a grade: each step a compound's own graph.
pub(super) fn grade_level<'a>(root: &'a klipp_nodes::NodeGraph, path: &[klipp_nodes::NodeId]) -> Option<&'a klipp_nodes::NodeGraph> {
    path.iter().try_fold(root, |g, id| g.nodes.iter().find(|n| n.id == *id)?.params.compound_subgraph.as_ref())
}

/// Which of the app's own shortcuts a frame's keys run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeyScope {
    /// Every one: nothing else has the keys.
    All,
    /// The Ctrl combinations only: text is being typed, or the picture
    /// canvas has the keys.
    CtrlOnly,
    /// The Ctrl combinations and the grade's keys: the grade canvas has
    /// the keys.
    GradeCanvas,
    /// New, open, preferences and quit: no project is open, and the
    /// welcome screen shows (#2551).
    AppOnly,
    /// None: a dialog has the keys (#2551).
    Nothing,
}

/// The shortcuts that run under `state`'s focus (#1742). A node canvas
/// owns its single keys: Delete, F, L, M and the digits are its, and the
/// timeline's shortcuts must not act on the clip behind it (Delete removed
/// the clip as well as the nodes). The grade canvas keeps the grade's own
/// keys, on the level it shows.
pub(super) fn key_scope(state: &crate::state::AppState, text_editing: bool) -> KeyScope {
    match state.active_panel {
        _ if state.dialog_open() => KeyScope::Nothing,
        _ if state.project_path.is_none() => KeyScope::AppOnly,
        _ if text_editing => KeyScope::CtrlOnly,
        Some(crate::state::PanelKind::NodeGraph) => KeyScope::GradeCanvas,
        Some(crate::state::PanelKind::PictureGraph) => KeyScope::CtrlOnly,
        _ => KeyScope::All,
    }
}

/// Whether Delete goes to the timeline's selection: not while the
/// Transcript panel under the pointer takes it for its words, nor while
/// the viewer's Mask tool has points selected or a shape being drawn
/// (#74), whose Delete it is. The clip behind them stays.
pub(super) fn delete_reaches_timeline(state: &crate::state::AppState) -> bool {
    !(state.show_transcript_panel && state.transcript_panel_hovered) && !state.mask_keys_owned()
}

/// What a plain Delete removes from the timeline's selection: a selected
/// transition, a selected gap, the selected clip, or the marked range.
pub(super) fn delete_selection(state: &mut crate::state::AppState) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    // A selected transition goes, not the clips around it (#918).
    if let (Some((track_index, transition_id)), None) = (state.selected_transition, state.selected_clip) {
        cmds.push(AppCommand::RemoveTransition { track_index, transition_id });
        state.selected_transition = None;
    } else if let (Some((track_index, gap_start, gap_end)), None) = (state.selected_gap.take(), state.selected_clip) {
        // A selected gap closes, the clips after it moving up (#925).
        let still = state.sequence().tracks.get(track_index).and_then(|t| crate::selection::gap_at(t, gap_start)) == Some((gap_start, gap_end));
        if still {
            cmds.push(AppCommand::CloseGap { track_index, gap_start, gap_end });
        }
    } else if let Some(clip_id) = state.selected_clip {
        cmds.push(AppCommand::RemoveClip { clip_id });
    } else if state.selected_clips.is_empty()
        && state.sequence().in_point.is_some()
        && state.sequence().out_point.is_some()
    {
        cmds.push(AppCommand::ExtractEdit);
    }
    cmds
}

/// The keys besides Delete that take clips out of the timeline: Cut
/// (Ctrl+X), Lift and Extract. None while the viewer's Mask tool has the
/// keys (#74), as Delete is not: its selected points are what the
/// person is working on.
pub(super) fn removal_keys(state: &crate::state::AppState, kb: &crate::keybindings::KeyBindings, i: &egui::InputState) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    if state.mask_keys_owned() {
        return cmds;
    }
    if kb.is_action_pressed("cut_clip", i) {
        if let Some(clip_id) = state.selected_clip {
            cmds.push(AppCommand::CutClip { clip_id });
        }
    }
    if kb.is_action_pressed("lift_edit", i) {
        cmds.push(AppCommand::LiftEdit);
    }
    if kb.is_action_pressed("extract_edit", i) {
        cmds.push(AppCommand::ExtractEdit);
    }
    cmds
}

/// What an ease shortcut does (#2307): `preset` on every key it acts on
/// ([`crate::key_selection::ease_targets`]), or Linear when `None`, in one
/// batch. Nothing when no key is selected or in focus.
pub(crate) fn key_ease_shortcut(state: &crate::state::AppState, preset: Option<crate::widgets::scrubby_param::EasePreset>) -> Vec<AppCommand> {
    let targets = crate::key_selection::ease_targets(state);
    if targets.is_empty() {
        return Vec::new();
    }
    let mut cmds = vec![AppCommand::BeginHistoryBatch];
    for m in &targets {
        cmds.extend(match preset {
            Some(p) => crate::panels::timeline::key_ease_commands(state, m, p),
            None => crate::panels::timeline::key_interpolation_commands(state, m, klipp_core::Interpolation::Linear),
        });
    }
    // Named for what was done, and how many keys (#2358).
    let what = match preset {
        Some(crate::widgets::scrubby_param::EasePreset::EaseIn) => "Ease In",
        Some(crate::widgets::scrubby_param::EasePreset::EaseOut) => "Ease Out",
        Some(crate::widgets::scrubby_param::EasePreset::EaseBoth) => "Ease In-Out",
        None => "Linear",
    };
    let label = if targets.len() > 1 { format!("{what} ({} keys)", targets.len()) } else { what.to_string() };
    cmds.push(AppCommand::EndHistoryBatchAs { label });
    cmds
}

#[cfg(test)]
mod key_scope_tests {
    use super::{key_scope, KeyScope};
    use crate::state::{AppState, PanelKind};

    /// #2551: with no project open the welcome screen shows, and only
    /// new, open, preferences and quit run; the untitled project behind
    /// it took edits and filled its undo history.
    #[test]
    fn the_welcome_screen_takes_only_the_app_keys() {
        let mut state = AppState { project_path: None, active_panel: Some(PanelKind::Timeline), ..Default::default() };
        assert_eq!(key_scope(&state, false), KeyScope::AppOnly);
        state.project_path = Some(crate::project_paths::UNTITLED.into());
        assert_eq!(key_scope(&state, false), KeyScope::All);
    }

    /// #2551: a dialog keeps its keys, typed into a field or not; a
    /// shortcut behind the New Project dialog opened the scopes.
    #[test]
    fn a_dialog_keeps_its_keys() {
        let mut state = AppState {
            project_path: Some(crate::project_paths::UNTITLED.into()),
            active_panel: Some(PanelKind::Timeline),
            show_new_project_dialog: true,
            ..Default::default()
        };
        assert_eq!(key_scope(&state, false), KeyScope::Nothing);
        assert_eq!(key_scope(&state, true), KeyScope::Nothing);
        state.project_path = None;
        assert_eq!(key_scope(&state, false), KeyScope::Nothing, "the welcome screen's New Project dialog too");
        state.show_new_project_dialog = false;
        state.show_preferences = true;
        assert_eq!(key_scope(&state, false), KeyScope::Nothing);
    }
}
