//! The project's life: making it new, opening, saving and closing it,
//! the lock that keeps two Klipps off one project, the unsaved-changes
//! dialog, and what the control socket is told of each (#826).

use super::*;

/// The id the unsaved-changes dialog's Save As picker answers under.
const UNSAVED_SAVE_PICKER: &str = "unsaved-dialog-save-as";

/// What the user chose when the project they opened was locked.
#[derive(Debug, Clone)]
pub(super) enum LockDecision {
    ReadOnly,
    /// Take it, recording whom from.
    TakeOver(Option<klipp_project::lock::LockInfo>),
}

/// What came of asking the user to save: the result a close, an open or a
/// quit is allowed to proceed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveOutcome {
    Saved,
    /// The save-as picker was dismissed.
    Cancelled,
    /// Writing failed; the user has been told.
    Failed,
}

/// The global LUT a project names, as the app holds it once the project
/// is open, and whether its file is there. A missing LUT is kept rather
/// than dropped, so the next save does not delete it from the project
/// (#1019): it is not applied, and Project Doctor offers to relink it.
///
/// Also what to tell the engine: load a LUT that is there, and clear it
/// otherwise. The engine keeps its LUT across projects, so sending nothing
/// went on grading through the previous project's while Project Doctor and
/// `check` said none was applied. The commands go to the engine alone
/// (`cmd_tx`); through the UI's handler, the clear would forget the path.
pub(super) fn opened_global_lut(saved: Option<&std::path::Path>, opacity: f32, enabled: bool) -> (Option<std::path::PathBuf>, Vec<AppCommand>) {
    match saved {
        Some(p) if p.exists() => (
            Some(p.to_path_buf()),
            vec![AppCommand::LoadGlobalLut { path: p.to_path_buf() }, AppCommand::SetGlobalLutOpacity(opacity), AppCommand::SetGlobalLutEnabled(enabled)],
        ),
        Some(p) => (Some(p.to_path_buf()), vec![AppCommand::ClearGlobalLut]),
        None => (None, vec![AppCommand::ClearGlobalLut]),
    }
}

impl KlippApp {
    /// Where project templates are kept (#618).
    pub(super) fn project_templates_dir(&self) -> std::path::PathBuf {
        self.state.project_templates_dir.clone().unwrap_or_else(|| {
            klipp_project::template::templates_dir(&dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from(".")))
        })
    }

    /// The open project's colour setup, as a template keeps it.
    pub(super) fn template_colour(&self) -> klipp_project::template::TemplateColour {
        klipp_project::template::TemplateColour {
            working_colorspace: self.state.project_working_colorspace,
            grade_encoding: self.state.project_grade_encoding,
            global_lut_path: self.state.global_lut_path.clone(),
            global_lut_opacity: self.state.global_lut_opacity,
            global_lut_enabled: self.state.global_lut_enabled,
        }
    }

    /// A project made from a template, in place of the fresh project's
    /// one sequence (#618): its sequences, its bins and smart bins, and its
    /// colour setup, given to the engine as opening a project gives it.
    pub(super) fn apply_template(&mut self, made: klipp_project::template::FromTemplate) {
        self.state.sequences = made.sequences;
        self.state.media_pool = made.pool;
        self.sync_media_pool_to_engine();
        let colour = made.colour;
        self.state.project_working_colorspace = colour.working_colorspace;
        klipp_engine::video_decoder::set_working_colorspace(colour.working_colorspace);
        self.state.project_grade_encoding = colour.grade_encoding;
        klipp_core::set_grade_encoding(colour.grade_encoding);
        self.state.global_lut_opacity = colour.global_lut_opacity;
        self.state.global_lut_enabled = colour.global_lut_enabled;
        let (kept, to_engine) = opened_global_lut(colour.global_lut_path.as_deref(), colour.global_lut_opacity, colour.global_lut_enabled);
        if let Some(lut) = kept.as_ref().filter(|p| !p.exists()) {
            let name = lut.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            self.state.toast.show(format!("The template's LUT {name} is missing: relink it in Project Doctor"));
        }
        self.state.global_lut_path = kept;
        for cmd in to_engine {
            self.cmd_tx.send(cmd).ok();
        }
    }

    pub(super) fn do_new_project(&mut self) {
        self.reset_project_state();
        self.sync_sequence_to_engine();
        // The socket's clients are told, as for a save or an open: an
        // agent mid-edit found its media gone and nothing said why.
        self.note_project_event(ProjectEvent::Replaced, None);
    }

    /// Close the current project and return to the Project Manager (launch screen).
    /// The projects root (#1864), worked out again only when the
    /// preference changes.
    pub(super) fn projects_root(&mut self) -> PathBuf {
        let pref = self.prefs.general.default_project_dir.clone();
        match &self.projects_root {
            Some((from, root)) if *from == pref => root.clone(),
            _ => {
                let root = crate::project_paths::projects_root(pref.as_ref());
                self.projects_root = Some((pref, root.clone()));
                root
            }
        }
    }

    /// An Untitled project's first real edit writes it, under a free
    /// "Untitled" in the projects root, and lists it in Recent (#1864).
    pub(super) fn write_untitled_on_first_edit(&mut self) {
        let root = self.projects_root();
        let unsaved = self.is_unsaved();
        let Some(path) = crate::project_paths::first_write(self.state.project_path.as_deref(), unsaved, &root) else { return };
        if let Err(e) = std::fs::create_dir_all(&path) {
            tracing::error!("Could not write the untitled project to {path:?}: {e}");
            return;
        }
        // The template's copied media, from the cache into the project.
        // Only once it is there does the staging folder go; when it could
        // not be moved, the folder is kept for good, since the media's
        // paths still lead into it.
        if let Some(staging) = self.untitled_staging.take() {
            match crate::project_paths::finish_staging(&staging, &path, &crate::project_paths::untitled_cache_dir()) {
                Ok(true) => self.repoint_moved_media(&staging, &path),
                Ok(false) => {}
                Err(e) => {
                    tracing::error!("Could not move the template's media into {path:?}: {e}");
                    crate::project_paths::keep_staging(&staging, &path);
                }
            }
        }
        self.do_save_project(&path);
        self.unnamed_project = Some(path);
    }

    /// Let a background auto-save finish before the project's folder is
    /// moved, so it does not write into the old place mid-move.
    pub(super) fn wait_for_autosave(&mut self) {
        if let Some(rx) = self.autosave_rx.take() {
            match rx.recv_timeout(std::time::Duration::from_secs(60)) {
                Ok(Ok(path)) => tracing::info!("Auto-saved to {:?} before the move", path),
                Ok(Err(e)) => tracing::warn!("Auto-save failed: {e}"),
                Err(e) => tracing::warn!("Auto-save did not finish before the move: {e}"),
            }
        }
    }

    /// The Untitled project's folder is at `to` now: it is that project,
    /// its media and auto-save follow it, and Recent names it. The lock is
    /// retaken there by `sync_project_lock` on the next frame, which sees
    /// the path change: a renamed folder carries this session's lock with
    /// it (taken again as ours), a copied one is locked afresh, and the
    /// keeper of the old place is let go then.
    pub(super) fn finish_project_move(&mut self, from: &Path, to: &Path, renamed: bool) {
        self.unnamed_project = None;
        if renamed {
            self.prefs.general.recent_projects.retain(|rp| rp.path != *from);
            self.prefs_dirty = true;
            if let Err(e) = crate::project_paths::move_autosave(&autosave_path_for(from), &autosave_path_for(to)) {
                tracing::warn!("Could not move the auto-save of {from:?}: {e}");
            }
        }
        self.state.project_path = Some(to.to_path_buf());
        self.repoint_moved_media(from, to);
        self.do_save_project(to);
    }

    /// Take a finished background copy of the project (Save As across
    /// filesystems), and say how far one still running has got.
    pub(super) fn poll_project_copy(&mut self) {
        let Some(copy) = &self.project_copy else { return };
        match copy.done.try_recv() {
            Ok(Ok(())) => {
                let copy = self.project_copy.take().expect("checked above");
                self.state.toast.show(format!("Copied to {}", copy.to.display()));
                self.finish_project_move(&copy.from, &copy.to, false);
            }
            Ok(Err(e)) => {
                let copy = self.project_copy.take().expect("checked above");
                tracing::error!("Could not copy {:?} to {:?}: {e}", copy.from, copy.to);
                self.state.toast.show(format!("The project could not be copied there: {e}. It stays where it was."));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if copy.shown_at.elapsed().as_secs() >= 1 {
                    let pct = (copy.copied.load(std::sync::atomic::Ordering::Relaxed) * 100).checked_div(copy.total.max(1)).unwrap_or(0).min(100);
                    self.state.toast.show(format!("Copying the project to {}\u{2026} {pct}%", copy.to.display()));
                    if let Some(c) = &mut self.project_copy {
                        c.shown_at = std::time::Instant::now();
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.project_copy = None;
                self.state.toast.show("The project copy stopped unexpectedly. It stays where it was.");
            }
        }
    }

    /// Point the media whose files were inside `from` at the same files
    /// under `to`, now that the folder moved, and reopen each in the
    /// engine, which would otherwise keep reading the old paths: the
    /// same `LoadMediaFile` a relink sends.
    pub(super) fn repoint_moved_media(&mut self, from: &Path, to: &Path) {
        let moved = crate::project_paths::rewrite_media_paths(&mut self.state.media_pool, from, to);
        if moved.is_empty() {
            return;
        }
        for (media_id, path) in moved {
            if path.exists() {
                let _ = self.cmd_tx.send(AppCommand::LoadMediaFile { path, media_id });
            }
        }
        self.sync_media_pool_to_engine();
    }

    pub(super) fn prepare_control_project_switch(&mut self, expected_identity: u64, save: bool, discard_changes: bool) -> bool {
        let current = {
            let mut current = self.state.control_shared.project.lock().unwrap_or_else(|e|e.into_inner()).clone();
            // The published snapshot can lag edits queued before this command.
            current.dirty = self.is_unsaved();
            current.path = self.state.project_path.clone();
            current
        };
        if save && current.dirty && (crate::project_paths::is_untitled(current.path.as_deref()) || self.unnamed_project.as_ref().is_some_and(|p| Some(p) == current.path.as_ref())) {
            self.note_project_event(ProjectEvent::Replaced, Some("save_project to a named path before closing or switching this untitled project".into()));
            return false;
        }
        match current.replacement_save_path(expected_identity, save, discard_changes) {
            Ok(Some(path)) => self.do_save_project(&path),
            Ok(None) => true,
            Err(error) => { self.note_project_event(ProjectEvent::Replaced, Some(error)); false }
        }
    }

    pub(super) fn do_close_project(&mut self) {
        // An Untitled project closed untouched leaves nothing: its staged
        // template media was the cache's copy.
        if let Some(staging) = self.untitled_staging.take() {
            crate::project_paths::discard_staging(&staging, &crate::project_paths::untitled_cache_dir());
        }
        self.unnamed_project = None;
        // Drop file watcher
        self.media_watcher = None;
        self.reset_project_state();
        self.state.fullscreen_viewer = false;
        self.sync_sequence_to_engine();
        self.note_project_event(ProjectEvent::Replaced, None);
    }

    /// Wipe all project-scoped state on both the UI and engine sides so the
    /// next project starts from a clean slate. Used by both new-project and
    /// close-project flows.
    pub(super) fn reset_project_state(&mut self) {
        // 1. Tell the engine to drop everything it's holding (decoders, caches,
        //    audio sources, frame buffer pixels). Sequence reset comes after.
        let _ = self.cmd_tx.send(AppCommand::Stop);
        let _ = self.cmd_tx.send(AppCommand::ResetEngine);

        // 2. Replace project-scoped data
        self.state.project_epoch += 1;
        self.retire_project_media();
        self.state.sequences = vec![Sequence::new_default()];
        self.state.active_sequence_idx = 0;
        self.state.media_pool = klipp_media::MediaPool::new();
        self.state.project_default_odt = klipp_core::OdtChoice::default();
        self.state.project_default_input_format = None;
        self.state.project_default_input_lut = None;
        self.state.project_viewing_lut = None;
        self.state.project_scopes_after_viewing_lut = false;
        // The engine resolves clips through its own copy of the pool.
        self.sync_media_pool_to_engine();
        self.state.title_assets.clear();
        self.state.looks.clear();
        self.state.grade_stills.clear();
        self.state.active_grade_still = None;
        self.state.grade_still_compare = false;
        self.state.offline_media_ids.clear();
        self.state.pending_skim_transcodes.clear();
        self.state.source_media_id = None;
        self.state.project_path = None;
        self.state.project_notes = String::new();
        self.state.last_import_report = None;
        self.state.show_import_report = false;
        *self.state.control_shared.import_outcome.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.mark_saved();

        // 3. Clear undo/redo history — entries reference clip IDs that no
        //    longer exist in the new project.
        self.state.command_histories.clear();
        self.state.project_history = crate::history::CommandHistory::new();
        self.state.history_ops_since_save = 0;

        // 4. Clear selection / hover / drag state
        self.state.selected_clip = None;
        self.state.selected_clips.clear();
        self.state.hovered_clip = None;
        self.state.dragging_media = None;
        self.state.dragging_media_group.clear();
        self.state.dragging_audio_keyframe = None;
        self.state.dragging_video_keyframe = None;
        self.state.dragging_rubber_band = None;
        self.state.dragging_bezier_handle = None;
        self.state.dragging_video_bezier_handle = None;
        self.state.dragging_fade_handle = None;

        // 5. Reset playback / playhead
        self.drop_automation_pass();
        self.state.playback = PlaybackState::Idle;
        let seq_rate = self.state.sequences[self.state.active_sequence_idx].fps.rate();
        self.state.playhead = klipp_core::RationalTime::from_frames(0, seq_rate);

        // 6. Clear thumbnail and filmstrip caches (textures reference dropped media)
        self.state.thumbnail_cache = klipp_media::ThumbnailCache::new();
        let thumb_drain: Vec<_> = self.state.thumbnail_textures.drain().map(|(_, t)| t).collect();
        self.defer_drop_textures(thumb_drain);
        let strip_drain: Vec<_> = self.state.filmstrip_textures.drain()
            .flat_map(|(_, v)| v.into_iter().map(|(_, t)| t)).collect();
        self.defer_drop_textures(strip_drain);
        self.state.filmstrip_in_progress.clear();
        self.state.filmstrip_queue.clear();

        // 6b. Drop program-viewer textures so the viewer doesn't
        //     keep rendering the OLD project's last frame while the
        //     engine resets and the user starts importing into the
        //     new project. Without this, the texture handle survives
        //     the project switch and the viewer paints stale pixels
        //     from the previous project until a fresh decode lands —
        //     which never happens for an audio-only or empty new
        //     project, leaving the OLD video frozen on screen while
        //     the NEW audio plays. Defer-drop (not raw drop) per
        //     `feedback_defer_drop_texture` — wgpu panics on next
        //     submit if a TextureHandle is removed mid-frame outside
        //     the deferred-drop ring.
        if let Some(old) = self.viewer.video_texture.take() {
            self.defer_drop_texture(old);
        }
        if let Some(old) = self.viewer.title_overlay_texture.take() {
            self.defer_drop_texture(old);
        }

        // 7. Invalidate media usage cache (clip-counts per media item)
        self.state.media_usage_cache.clear();
        self.state.media_usage_cache_dirty = true;

        // 8. Clear render-cache range markers (no clips → no ranges)
        self.state.render_cache_ranges.clear();
        self.state.render_status.clear();
    }

    /// Mark every item of the open project, and every import still
    /// being probed for it, as belonging to a project that is going
    /// away, so a probe that lands after the switch is dropped rather
    /// than added to the next project.
    fn retire_project_media(&mut self) {
        let ids: Vec<klipp_core::MediaId> = self.state.media_pool.all().map(|m| m.id)
            .chain(self.state.pending_media_drop.keys().copied())
            .chain(self.state.pending_skip_analysis.iter().copied())
            .chain(self.state.pending_undo_imports.iter().copied())
            .collect();
        self.state.retired_media_ids.extend(ids);
        self.state.pending_media_drop.clear();
        self.state.pending_skip_analysis.clear();
        self.state.pending_undo_imports.clear();
        self.state.pending_media_path = None;
    }

    /// Tell the control socket how a save or an open went: it waits for
    /// the generation to move, and reads the path and the error then.
    fn note_project_event(&self, event: ProjectEvent, error: Option<String>) {
        if let Ok(mut project) = self.state.control_shared.project.lock() {
            if project_identity_moves(event, project.path.as_deref(), self.state.project_path.as_deref(), error.is_some()) {
                project.identity += 1;
            }
            project.recent_projects = self.prefs.general.recent_projects.iter().map(crate::prefs::RecentProject::control_summary).collect();
            project.path = self.state.project_path.clone();
            project.dirty = self.state.unsaved;
            project.global_lut = self.state.global_lut_path.clone();
            project.generation += 1;
            project.last_error = error;
            if !project.dirty {
                project.changed_by = None;
            }
        }
    }

    /// Whether a save may write `path`: not when the project is open
    /// read-only, and not when someone has taken its lock since. Says
    /// why when it may not.
    fn may_write_project(&mut self, path: &std::path::Path) -> bool {
        if let Some(keeper) = self.lock_keeper.as_ref().filter(|k| k.project() == path) {
            if let Err(taker) = keeper.check_now() {
                self.lose_lock(taker);
            }
        }
        let read_only = self.read_only_project.as_deref() == Some(path);
        let ours = self.lock_keeper.as_ref().is_some_and(|k| k.project() == path);
        match save_refusal(read_only, ours, || self.lock_held_elsewhere(path)) {
            Some(why) => {
                self.state.toast.show(why);
                false
            }
            None => true,
        }
    }

    /// Ask what to do about a project someone else has open.
    pub(super) fn show_lock_prompt(&mut self, ctx: &egui::Context) {
        use klipp_project::lock::{describe_secs, now_secs, Liveness};
        let Some((path, info, liveness)) = self.lock_prompt.clone() else { return };
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let quiet = describe_secs(info.quiet_for(now_secs()));
        let mut choice = None;
        let mut cancelled = false;
        let mut open = true;
        egui::Window::new("Project In Use")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                match liveness {
                    Liveness::Active => {
                        ui.label(format!("\u{201C}{name}\u{201D} is open in Klipp for {}, active {quiet} ago.", info.who()));
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("If you both save, whoever saves last replaces the other's work. Open it read-only to look, or ask them to close it first.")
                                .size(11.0)
                                .color(self.theme.text_dim),
                        );
                    }
                    _ => {
                        ui.label(format!("\u{201C}{name}\u{201D} was left open by {}, last seen {quiet} ago.", info.who()));
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("Klipp there most likely quit without closing it, or the machine went to sleep. Taking it over is safe if nobody is working in it.")
                                .size(11.0)
                                .color(self.theme.text_dim),
                        );
                    }
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let take = ui.button("Take Over").on_hover_text(format!(
                        "Open it for editing. {} is switched to read-only on their next check, and the takeover is recorded in the project.",
                        info.who()
                    ));
                    if take.clicked() {
                        choice = Some(LockDecision::TakeOver(Some(info.clone())));
                    }
                    if ui.button("Open Read-Only").clicked() {
                        choice = Some(LockDecision::ReadOnly);
                    }
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                });
            });
        if let Some(decision) = choice {
            self.lock_prompt = None;
            self.lock_decision = Some((path.clone(), decision));
            self.do_open_project(&path);
        } else if cancelled || !open {
            self.lock_prompt = None;
        }
    }

    /// Who has `path` open, when someone other than this session does
    /// and their lock is not dead.
    pub(super) fn lock_held_elsewhere(&self, path: &std::path::Path) -> Option<String> {
        use klipp_project::lock::{self, Liveness, LockState};
        match lock::inspect(path, &self.lock_holder) {
            LockState::Held(info, Liveness::Active | Liveness::Stale) => Some(info.who()),
            _ => None,
        }
    }

    /// The open project's folder, for the writers that put files beside
    /// the project (freeze frames, extracted stills, grade stills) or
    /// clear what is beside it (the auto-save, on close and Don't Save) —
    /// `None` when it is open read-only, whose folder belongs to the
    /// editor holding it. The writers then use a temporary folder, as
    /// they do for a project never saved, and nothing is cleared.
    pub(super) fn writable_project_dir(&self) -> Option<PathBuf> {
        self.state.project_path.clone().filter(|p| self.read_only_project.as_ref() != Some(p))
    }

    /// Someone took the open project's lock: stop saving to it.
    fn lose_lock(&mut self, taker: klipp_project::lock::LockInfo) {
        let Some(keeper) = self.lock_keeper.take() else { return };
        self.read_only_project = Some(keeper.project().to_path_buf());
        // Dropping the keeper does not remove a lock that is not ours.
        drop(keeper);
        self.state.toast.show(format!(
            "{} took over this project. It is read-only here now; Save As keeps your changes in a copy.",
            taker.who()
        ));
    }

    /// Hold the lock on whichever project is open, and only that one.
    /// Runs every frame, but touches the disk only when the open project
    /// changes: the heartbeat runs on the keeper's thread.
    pub(super) fn sync_project_lock(&mut self) {
        use klipp_project::lock;
        if let Some(taker) = self.lock_keeper.as_ref().and_then(|k| k.lost()) {
            self.lose_lock(taker);
        }
        if self.state.project_path == self.lock_seen_path {
            return;
        }
        self.lock_seen_path = self.state.project_path.clone();
        // Another project, or none: whatever was held is given up.
        self.lock_keeper = None;
        if self.read_only_project != self.state.project_path {
            self.read_only_project = None;
        }
        // An untitled project has no folder to lock until its first save.
        // A save that creates the folder asks for this again.
        let Some(path) = self.state.project_path.clone().filter(|p| p.is_dir()) else {
            return;
        };
        let decision = self.lock_decision.take().filter(|(p, _)| *p == path).map(|(_, d)| d);
        let already_read_only = self.read_only_project.as_ref() == Some(&path);
        match opened_lock_step(already_read_only, decision, || lock::acquire(&path, &self.lock_holder)) {
            LockStep::Nothing => {}
            LockStep::Held => self.lock_keeper = Some(lock::Keeper::start(path, self.lock_holder.clone())),
            LockStep::ReadOnly => self.read_only_project = Some(path),
            LockStep::TakeOver(from) => self.take_over_lock(path, from.as_ref()),
            LockStep::HeldElsewhere(info) => {
                self.read_only_project = Some(path);
                self.state.toast.show(format!(
                    "{} has this project open, so it is read-only here. Save As keeps your changes in a copy.",
                    info.who()
                ));
            }
        }
    }

    /// Take the lock on `path`, or, when it cannot be written, say so and
    /// open read-only: a keeper started on a lock that was never taken
    /// would blame the holder for a takeover thirty seconds later.
    fn take_over_lock(&mut self, path: PathBuf, from: Option<&klipp_project::lock::LockInfo>) {
        match klipp_project::lock::take_over(&path, &self.lock_holder, from) {
            Ok(()) => self.lock_keeper = Some(klipp_project::lock::Keeper::start(path, self.lock_holder.clone())),
            Err(e) => {
                tracing::warn!("project lock: could not take over {}: {e}", path.display());
                self.read_only_project = Some(path);
                self.state.toast.show(format!(
                    "Could not take the project over ({e}), so it is open read-only. Save As keeps your changes in a copy."
                ));
            }
        }
    }

    /// Copy every still under the temporary still folders into
    /// `project_dir` and point whatever holds it there: media items, and
    /// the freeze frames on any sequence's tracks, which are generator
    /// clips rather than pool items. A still that could not be copied is
    /// said so in a toast, since the save itself still goes through and
    /// would otherwise report a clean success for a project pointing
    /// into `/tmp`.
    fn adopt_temp_stills(&mut self, project_dir: &std::path::Path) {
        let report = klipp_project::adopt_temp_stills(project_dir, &mut self.state.media_pool, &mut self.state.sequences);
        if report.is_empty() {
            return;
        }
        if report.adopted > 0 {
            self.sync_media_pool_to_engine();
            self.sync_sequence_to_engine();
        }
        if let Some((path, e)) = report.failed.first() {
            let n = report.failed.len();
            self.state.toast.show(format!(
                "Could not copy {n} still{} into the project ({}: {e}); {} still in the temp folder",
                if n == 1 { "" } else { "s" },
                path.display(),
                if n == 1 { "it is" } else { "they are" },
            ));
        }
    }

    /// Music, effects, narration and recordings Klipp made or fetched
    /// for the project, copied into its folder on every save (#730): kept
    /// where a cache clean or a moved project cannot leave them behind.
    fn adopt_made_media(&mut self, project_dir: &std::path::Path) {
        let report = klipp_project::adopt_made_media(project_dir, &mut self.state.media_pool);
        if report.is_empty() {
            return;
        }
        if report.adopted > 0 {
            self.sync_media_pool_to_engine();
        }
        if let Some((path, e)) = report.failed.first() {
            let n = report.failed.len();
            self.state.toast.show(format!(
                "Could not copy {n} downloaded or recorded file{} into the project ({}: {e}); the project still points outside it",
                if n == 1 { "" } else { "s" },
                path.display(),
            ));
        }
    }

    /// Write the project to `path`. `true` when it is on disk; a failure
    /// is shown to the user, not just logged — the caller may be about to
    /// close the project on the strength of this.
    pub(super) fn do_save_project(&mut self, path: &std::path::Path) -> bool {
        if !self.may_write_project(path) {
            // The control socket waits on a project event for every save.
            self.note_project_event(ProjectEvent::Saved, Some("not saved: the project is open read-only or locked by someone else".into()));
            return false;
        }
        let color = self.state.project_color_settings();
        // Stills made while there was no folder of our own to write to —
        // a project never saved, or one open read-only — come along, or
        // the project would point into the temp folder. On every save,
        // not only one to a new place: an extracted frame reaches the
        // pool only when its probe lands, which can be after the Save As
        // that should have brought it, and an undo can put a clip with a
        // temp path back. With nothing under the temp folders this does
        // nothing.
        let new_home = self.state.project_path.as_deref() != Some(path);
        self.adopt_temp_stills(path);
        self.adopt_made_media(path);
        let active_seq_id = self.state.sequences[self.state.active_sequence_idx].id.to_string();
        match klipp_project::save_project_full(path, &self.state.sequences, &self.state.media_pool, &color, Some(&active_seq_id), self.state.created_at.as_deref(), &self.state.project_notes, self.prefs.editing.backup_count, Some(self.state.timeline_zoom), Some(self.state.timeline_scroll_x), Some(self.state.timeline_track_scale), &self.state.title_assets) {
            Ok(()) => {
                if let Err(e) = klipp_project::import_report::save(path, self.state.last_import_report.as_ref()) {
                    self.state.toast.show(format!("Project saved, but the import report could not be saved: {e}"));
                }
                if let Err(e) = klipp_project::save_looks(path, &self.state.looks) {
                    // Said, not only logged: a Look that isn't saved is
                    // lost on close (#1135).
                    tracing::warn!("Failed to persist Looks library: {e}");
                    self.state.toast.show(format!("The project saved, but not every Look did: {e}"));
                }
                // A first save has just made the folder there is to lock.
                if self.lock_keeper.is_none() && self.read_only_project.is_none() {
                    self.lock_seen_path = None;
                }
                self.state.project_path = Some(path.to_path_buf());
                // Grade stills kept only in memory (no folder, or a
                // read-only one) are written with the project's new home.
                if new_home {
                    self.persist_grade_stills();
                }
                // What was just written is the saved project (#2626).
                self.mark_saved();
                self.state.reset_ops_since_save();
                tracing::info!("Project saved to {:?}", path);
                self.note_project_event(ProjectEvent::Saved, None);
                let rp = crate::prefs::RecentProject {
                    path: path.to_path_buf(),
                    // The project's own name, from the file on disk —
                    // not the active sequence's.
                    //
                    // It used to be the sequence name, so the browser
                    // labelled every project by whatever sequence
                    // happened to be open in it. Three projects still
                    // carrying the default "Main Sequence" drew three
                    // identical cards for three different files, and
                    // renaming a sequence renamed the project in the
                    // browser without touching anything on disk.
                    name: path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| {
                            self.state.sequences[self.state.active_sequence_idx].name.clone()
                        }),
                    last_saved: Some(crate::app::iso8601_now()),
                    duration_secs: Some(self.state.sequence_duration().to_secs_f64()),
                    fps: {
                        let f = self.state.sequences[self.state.active_sequence_idx].fps;
                        if f.den == 1 {
                            Some(format!("{}fps", f.num))
                        } else {
                            Some(format!("{:.3}fps", f.num as f64 / f.den as f64))
                        }
                    },
                    resolution: Some(format!(
                        "{}×{}",
                        self.state.sequences[self.state.active_sequence_idx].resolution.width,
                        self.state.sequences[self.state.active_sequence_idx].resolution.height
                    )),
                    thumbnail_path: generate_poster_frame(&self.frame_buffer, path, &self.state.media_pool, self.state.sequence_duration().to_secs_f64()),
                    total_size_bytes: Some(self.state.media_pool.all().map(|m| m.file_size_bytes).sum()),
                    clip_count: Some(self.state.sequences[self.state.active_sequence_idx].tracks.iter().map(|t| t.clips.len() as u32).sum()),
                    track_count: Some(self.state.sequences[self.state.active_sequence_idx].tracks.len() as u32),
                    missing_media: Some(self.state.media_pool.all().any(|m| m.offline || !m.path.exists())),
                    sequence_count: Some(self.state.sequences.len() as u32),
                    pinned: false,
                };
                if let Some(poster) = rp.thumbnail_path.as_deref() {
                    spawn_skim_strip(&self.state.sequences[self.state.active_sequence_idx], &self.state.media_pool, poster);
                }
                self.prefs.add_recent_project(rp);
                self.prefs_dirty = true;
                // Delete autosave file on clean save
                let autosave = autosave_path_for(path);
                if autosave.exists() {
                    let _ = std::fs::remove_dir_all(&autosave);
                    tracing::info!("Removed autosave {:?}", autosave);
                }
                true
            }
            Err(e) => {
                tracing::error!("Failed to save project: {e}");
                self.note_project_event(ProjectEvent::Saved, Some(format!("could not save: {e}")));
                self.state.toast.show(format!("Could not save the project: {e}"));
                false
            }
        }
    }

    /// Save the project for the unsaved-changes dialog, so a close, an
    /// open or a quit can act on the outcome. A project saved before
    /// saves now. One never saved needs a place first: the picker runs
    /// off the UI thread, and until it answers this is `None` and the
    /// dialog waits for it.
    /// `clicked` is this frame's Save click. The picker opens on the
    /// click only; while it is open this only collects its answer, so
    /// it can never open twice, and a picker refused because another
    /// is open leaves the dialog as it was rather than waiting.
    fn save_project_interactively(&mut self, clicked: bool) -> Option<SaveOutcome> {
        use crate::file_dialog::{await_path, Awaited, Picked};
        let path = match self.state.project_path.clone() {
            Some(p) if clicked => p,
            Some(_) => {
                // Saved before, so nothing to wait for.
                self.unsaved_save_waiting = false;
                return None;
            }
            None => {
                let step = await_path(UNSAVED_SAVE_PICKER.into(), self.unsaved_save_waiting, clicked, || {
                    rfd::FileDialog::new().set_title("Save Project").pick_folder()
                });
                self.unsaved_save_waiting = step == Awaited::Waiting;
                match step {
                    Awaited::Idle | Awaited::Waiting => return None,
                    Awaited::Answered(Picked::Cancelled) => return Some(SaveOutcome::Cancelled),
                    Awaited::Answered(Picked::Path(p)) => ensure_klipp_extension(p),
                }
            }
        };
        Some(if self.do_save_project(&path) { SaveOutcome::Saved } else { SaveOutcome::Failed })
    }

    pub(super) fn do_open_project(&mut self, path: &std::path::Path) {
        // Someone else has it open: ask before loading, unless this open
        // is the answer to that question.
        let decided = self.lock_decision.as_ref().is_some_and(|(p, _)| p == path);
        if !decided {
            use klipp_project::lock::{self, Liveness, LockState};
            match lock::inspect(path, &self.lock_holder) {
                // Klipp on this machine died holding it: nobody to ask.
                LockState::Held(info, Liveness::Dead) => {
                    self.state.toast.show(format!(
                        "Took over the lock left by Klipp (pid {}), which is no longer running",
                        info.pid
                    ));
                    self.lock_decision = Some((path.to_path_buf(), LockDecision::TakeOver(Some(info))));
                }
                LockState::Held(info, liveness) => {
                    self.lock_prompt = Some((path.to_path_buf(), info, liveness));
                    return;
                }
                LockState::Free | LockState::Ours => {}
            }
        }
        // Check for newer autosave — not when opening read-only: the
        // holder's live auto-save is always newer, and it is theirs.
        // Restoring it would load their work in progress, and keeping
        // the saved version retires it, which takes their crash
        // recovery away.
        let read_only = matches!(&self.lock_decision, Some((p, LockDecision::ReadOnly)) if p == path);
        // Judged by the saves themselves, not the folders' times, which
        // the lock file moves (#2789).
        if let Some(found) = klipp_project::recovery::newer_autosave(path).filter(|_| !read_only) {
            self.state.show_autosave_recovery = true;
            self.state.autosave_recovery_path = Some(found.autosave);
            self.state.autosave_original_path = Some(path.to_path_buf());
            return;
        }
        self.do_open_project_inner(path);
    }

    /// The lock and the auto-save, for an open from the control socket
    /// (#2789), which has nobody to ask: a lock left by a Klipp that died
    /// is taken over; one another Klipp holds refuses the open, as does an
    /// auto-save newer than the last save when the request didn't say
    /// whether to restore it. A refusal is the open's outcome, with why.
    pub(super) fn control_open_checks(&mut self, path: &std::path::Path, recovery: Option<bool>) -> bool {
        match self.open_refusal(path, recovery) {
            Some(why) => {
                self.note_project_event(ProjectEvent::Replaced, Some(why));
                false
            }
            None => true,
        }
    }

    fn open_refusal(&mut self, path: &std::path::Path, recovery: Option<bool>) -> Option<String> {
        use klipp_project::lock::{self, Liveness, LockState};
        match lock::inspect(path, &self.lock_holder) {
            LockState::Held(info, Liveness::Dead) => {
                self.state.toast.show(format!("Took over the lock left by Klipp (pid {}), which is no longer running", info.pid));
                self.lock_decision = Some((path.to_path_buf(), LockDecision::TakeOver(Some(info))));
            }
            LockState::Held(info, _) => {
                return Some(format!("another Klipp (pid {} on {}) has {} open: close it there first", info.pid, info.host, path.display()));
            }
            LockState::Free | LockState::Ours => {}
        }
        match (klipp_project::recovery::newer_autosave(path), recovery) {
            (Some(found), None) => Some(format!(
                "{} has an auto-save from {} newer than its last save ({}): unsaved work, usually from a crash. Open it again with recovery restore (the auto-save) or keep (the saved version)",
                path.display(),
                found.autosaved_at.as_deref().unwrap_or("an unknown time"),
                found.saved_at.as_deref().unwrap_or("never saved"),
            )),
            _ => None,
        }
    }

    /// Open the auto-save of `original` in its place (#2789): the work in
    /// it unsaved until saved, under the project's own name.
    pub(super) fn restore_autosave(&mut self, original: &std::path::Path) {
        let Some(found) = klipp_project::recovery::newer_autosave(original) else {
            self.do_open_project_inner(original);
            return;
        };
        self.do_open_project_inner(&klipp_project::recovery::source_for(original, true));
        // The project's own path, not the auto-save's.
        self.state.project_path = Some(original.to_path_buf());
        // Unsaved until it is saved explicitly: what is on disk is not what
        // was restored.
        self.state.saved_digest = None;
        self.state.dirty = true;
        self.state.toast.show(format!("Unsaved work was recovered from the auto-save of {}: save to keep it", found.autosaved_at.as_deref().unwrap_or("before the crash")));
    }

    /// Open `original` as saved, its auto-save set aside so it isn't
    /// offered again (#2789).
    pub(super) fn keep_saved_version(&mut self, original: &std::path::Path) {
        retire_autosave(original);
        self.do_open_project_inner(original);
    }

    /// Read the shot metadata of every item whose file was imported
    /// before the probe read it — a project saved by an earlier build.
    /// Each goes through the same re-probe a relink uses, which keeps
    /// what the user said about the item; it happens once, since the
    /// probe marks the item read.
    fn probe_unread_metadata(&mut self) {
        let unread: Vec<(klipp_core::MediaId, std::path::PathBuf)> = self
            .state
            .media_pool
            .all()
            .filter(|m| m.wants_metadata_probe())
            .map(|m| (m.id, m.path.clone()))
            .collect();
        if unread.is_empty() {
            return;
        }
        tracing::info!("Reading the shot metadata of {} clips imported before it was kept", unread.len());
        for (media_id, path) in unread {
            let _ = self.cmd_tx.send(AppCommand::LoadMediaFile { path, media_id });
        }
    }

    pub(super) fn do_open_project_inner(&mut self, path: &std::path::Path) {
        match klipp_project::load_project(path) {
            Ok((sequences, media_pool, color_settings, active_id, created_at, notes,
                timeline_zoom, timeline_scroll_x, timeline_track_scale, title_assets)) => {
                self.state.fps = sequences.first().map(|s| s.fps).unwrap_or(klipp_core::Fps { num: 24, den: 1 });
                let active_idx = active_id.as_deref()
                    .and_then(|id| sequences.iter().position(|s| s.id.to_string() == id))
                    .unwrap_or(0);
                self.state.sequences = sequences;
                self.state.active_sequence_idx = active_idx;
                self.state.created_at = created_at;
                self.state.project_notes = notes;
                self.state.last_import_report = klipp_project::import_report::read(path).unwrap_or_else(|e| { tracing::warn!("Could not read last import report: {e}"); None });
                self.state.show_import_report = false;
                *self.state.control_shared.import_outcome.lock().unwrap_or_else(|e| e.into_inner()) = self.state.last_import_report.as_ref().map(|r| serde_json::json!({"report":r}));
                self.state.project_epoch += 1;
                self.retire_project_media();
                self.state.media_pool = media_pool;
                // Reopening a project brings its own media back.
                for item in self.state.media_pool.all() {
                    self.state.retired_media_ids.remove(&item.id);
                }
                self.state.title_assets = title_assets;
                self.state.looks = klipp_project::load_looks(path);
                self.probe_unread_metadata();
                // Restore saved timeline view-state (zoom + scroll). Legacy
                // project files don't carry these — leave the in-memory
                // defaults alone in that case.
                if let Some(z) = timeline_zoom {
                    self.state.timeline_zoom = z.clamp(0.1, 2000.0);
                }
                if let Some(sx) = timeline_scroll_x {
                    self.state.timeline_scroll_x = sx.max(0.0);
                }
                if let Some(scale) = timeline_track_scale {
                    self.state.timeline_track_scale = scale.clamp(0.1, 1.0);
                }

                // Fix media resolutions for rotated videos (phone portrait clips)
                // Older projects may have stored raw codec dimensions without rotation
                {
                    let mut fixes = Vec::new();
                    for item in self.state.media_pool.all() {
                        if item.path.exists() && !item.video_codec.is_empty() {
                            let rotation = klipp_engine::detect_rotation_from_path(&item.path);
                            if rotation == 90 || rotation == 270 {
                                // Check if dimensions need swapping (width > height = landscape encoded)
                                if item.resolution.width > item.resolution.height {
                                    fixes.push((item.id, item.resolution.height, item.resolution.width));
                                }
                            }
                        }
                    }
                    for (id, new_w, new_h) in fixes {
                        if let Some(item) = self.state.media_pool.get_mut(&id) {
                            item.resolution.width = new_w;
                            item.resolution.height = new_h;
                            tracing::info!("Fixed rotation-aware resolution for {:?}: {}x{}", item.filename, new_w, new_h);
                        }
                    }
                }

                // Aspect versions not yet reframed when the project was
                // closed wait again for their files' framing (#2231).
                self.state.pending_refits = super::aspect_version::waiting_refits(&self.state.sequences, &self.state.media_pool);

                // Restore global LUT
                self.state.global_lut_opacity = color_settings.global_lut_opacity;
                self.state.global_lut_enabled = color_settings.global_lut_enabled;
                let (kept, to_engine) = opened_global_lut(
                    color_settings.global_lut_path.as_deref(),
                    color_settings.global_lut_opacity,
                    color_settings.global_lut_enabled,
                );
                self.state.global_lut_path = kept.clone();
                for cmd in to_engine {
                    self.cmd_tx.send(cmd).ok();
                }
                if let Some(lut_path) = kept {
                    if !lut_path.exists() {
                        tracing::warn!("Global LUT not found, kept for relinking: {:?}", lut_path);
                        let name = lut_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        self.state.toast.show(format!("The project's LUT {name} is missing: relink it in Project Doctor"));
                    }
                }
                // Restore track header width
                self.state.track_header_width = color_settings.track_header_width;

                // Restore per-project media generation overrides
                self.state.project_auto_filmstrips = color_settings.auto_generate_filmstrips;
                self.state.project_auto_skim = color_settings.auto_generate_skim;

                // Restore the project's working colorspace and push it
                // into the engine's atomic so the decoder + grade chain
                // pick the right luma vector. Old project files that
                // lack the field load as `Rec709Linear` via serde
                // back-compat — byte-identical to the pre-Phase-E
                // pipeline.
                self.state.project_working_colorspace = color_settings.working_colorspace;
                klipp_engine::video_decoder::set_working_colorspace(
                    color_settings.working_colorspace,
                );
                // And what the grade acts in (#467): BT.1886 for every
                // file written before it.
                self.state.project_grade_encoding = color_settings.grade_encoding;
                self.state.project_default_odt = color_settings.default_odt;
                self.state.project_default_input_format = color_settings.default_input_format;
                self.state.project_default_input_lut = color_settings.default_input_lut.clone();
                self.state.project_viewing_lut = color_settings.viewing_lut.clone();
                self.state.project_scopes_after_viewing_lut = color_settings.scopes_after_viewing_lut;
                klipp_core::set_grade_encoding(color_settings.grade_encoding);

                self.state.project_path = Some(path.to_path_buf());
                // The project as opened is the saved one (#2626): taken
                // now, after what the open itself does and before any
                // re-read or analysis arrives, so a file that changed on
                // disk still shows as a change.
                self.mark_saved();
                // Fonts the titles name that are not installed (#1720).
                let missing = self.state.missing_fonts(&crate::missing_fonts::installed);
                // The catalogue's families are fetched, not asked about (#2732).
                let missing = crate::missing_fonts::fetch_from_catalogue(missing);
                self.state.missing_fonts_dialog = (!missing.is_empty()).then(|| crate::missing_fonts::MissingFontsDialog { fonts: missing, chosen: Default::default() });
                self.state.selected_clip = None;
                // Preserve existing thumbnail and last_saved when re-opening
                let existing = self.prefs.general.recent_projects.iter()
                    .find(|rp| rp.path == path);
                let existing_thumb = existing
                    .and_then(|rp| rp.thumbnail_path.clone())
                    .filter(|p| p.exists());
                let existing_saved = existing
                    .and_then(|rp| rp.last_saved.clone());
                let rp = crate::prefs::RecentProject {
                    path: path.to_path_buf(),
                    // The project's own name, from the file on disk —
                    // not the active sequence's.
                    //
                    // It used to be the sequence name, so the browser
                    // labelled every project by whatever sequence
                    // happened to be open in it. Three projects still
                    // carrying the default "Main Sequence" drew three
                    // identical cards for three different files, and
                    // renaming a sequence renamed the project in the
                    // browser without touching anything on disk.
                    name: path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| {
                            self.state.sequences[self.state.active_sequence_idx].name.clone()
                        }),
                    last_saved: existing_saved,
                    duration_secs: Some(self.state.sequence_duration().to_secs_f64()),
                    fps: {
                        let f = self.state.sequences[self.state.active_sequence_idx].fps;
                        if f.den == 1 {
                            Some(format!("{}fps", f.num))
                        } else {
                            Some(format!("{:.3}fps", f.num as f64 / f.den as f64))
                        }
                    },
                    resolution: Some(format!(
                        "{}×{}",
                        self.state.sequences[self.state.active_sequence_idx].resolution.width,
                        self.state.sequences[self.state.active_sequence_idx].resolution.height
                    )),
                    thumbnail_path: existing_thumb,
                    total_size_bytes: Some(self.state.media_pool.all().map(|m| m.file_size_bytes).sum()),
                    clip_count: Some(self.state.sequences[self.state.active_sequence_idx].tracks.iter().map(|t| t.clips.len() as u32).sum()),
                    track_count: Some(self.state.sequences[self.state.active_sequence_idx].tracks.len() as u32),
                    missing_media: Some(self.state.media_pool.all().any(|m| m.offline || !m.path.exists())),
                    sequence_count: Some(self.state.sequences.len() as u32),
                    pinned: false,
                };
                self.prefs.add_recent_project(rp);
                self.prefs_dirty = true;
                self.drop_automation_pass();
                self.state.playback = PlaybackState::Idle;
                let seq_rate = self.state.sequences[self.state.active_sequence_idx].fps.rate();
                self.state.playhead = klipp_core::RationalTime::from_frames(0, seq_rate);
                self.state.thumbnail_cache = klipp_media::ThumbnailCache::new();
                let thumb_drain: Vec<_> = self.state.thumbnail_textures.drain().map(|(_, t)| t).collect();
                self.defer_drop_textures(thumb_drain);
                let strip_drain: Vec<_> = self.state.filmstrip_textures.drain()
                    .flat_map(|(_, v)| v.into_iter().map(|(_, t)| t)).collect();
                self.defer_drop_textures(strip_drain);
                self.state.filmstrip_in_progress.clear();
                self.state.filmstrip_queue.clear();
                // Stop the engine so decoders are released and frame buffer is stale-free
                self.cmd_tx.send(AppCommand::Stop).ok();
                // Clear the shared frame buffer so the viewer doesn't show the old frame
                if let Ok(mut fb) = self.frame_buffer.lock() {
                    fb.clear();
                }
                // Pre-populate skim_file paths from disk cache before syncing to engine
                {
                    let mut skim_updates: Vec<(klipp_core::MediaId, std::path::PathBuf)> = Vec::new();
                    for item in self.state.media_pool.all() {
                        if item.skim_file.is_none() {
                            // The same key the transcode writes under —
                            // with its HDR and rotation markers. The bare
                            // content key found exactly the stale skims
                            // those markers exist to skip.
                            if let Some(key) = skim_cache_key_for(item) {
                                let skim_path = klipp_media::disk_cache::skim_transcode_path(&key);
                                if skim_path.exists() {
                                    if skim_file_is_complete(&skim_path, item.duration.to_secs_f64()) {
                                        skim_updates.push((item.id, skim_path));
                                    } else {
                                        let _ = std::fs::remove_file(&skim_path);
                                    }
                                }
                            }
                        }
                    }
                    for (id, path) in skim_updates {
                        if let Some(item) = self.state.media_pool.get_mut(&id) {
                            item.skim_file = Some(path);
                            tracing::info!("Pre-loaded skim path for {:?}", item.filename);
                        }
                    }
                }

                self.sync_sequence_to_engine();
                self.sync_media_pool_to_engine();

                // Detect offline media
                self.state.offline_media_ids = self.state.media_pool.all()
                    .filter(|item| item.offline)
                    .map(|item| item.id)
                    .collect();
                if !self.state.offline_media_ids.is_empty() {
                    self.state.show_relink_dialog = true;
                }

                // Load grade stills (PNG sidecars in <project>/media/stills/).
                // Reset comparison state — the previous project's selection is
                // meaningless for the new project's still list.
                self.state.grade_stills = klipp_project::load_grade_stills(path)
                    .into_iter()
                    .map(|d| crate::state::GradeStill {
                        name: d.name,
                        filename: d.filename,
                        rgba: d.rgba,
                        width: d.width,
                        height: d.height,
                        source: d.source,
                        grade: d.grade,
                        grade_labels: d.grade_labels,
                        texture: None,
                    })
                    .collect();
                self.state.active_grade_still = None;
                self.state.grade_still_compare = false;

                // Reload media files into the engine and trigger thumbnails/filmstrips/audio/skim
                let media_items: Vec<_> = self
                    .state
                    .media_pool
                    .all()
                    .filter(|item| !item.offline)
                    .map(|item| (item.id, item.path.clone(), item.duration, item.has_audio))
                    .collect();
                for (id, source_path, duration, has_audio) in media_items {
                    // Always use the original source for timeline playback (full quality + audio)
                    self.cmd_tx.send(AppCommand::LoadMediaFile {
                        path: source_path.clone(),
                        media_id: id,
                    }).ok();
                    self.spawn_thumbnail(id, source_path.clone(), duration);
                    self.spawn_filmstrip(id, source_path.clone(), duration);
                    if has_audio {
                        self.spawn_audio_conform(id, source_path.clone());
                    }
                    self.state.pending_skim_transcodes.push(id);
                }
                if !self.state.filmstrip_queue.is_empty() || !self.state.filmstrip_in_progress.is_empty() {
                    self.state.filmstrip_generating = true;
                }

                tracing::info!("Project loaded from {:?}", path);
                self.note_project_event(ProjectEvent::Replaced, None);

                // Show the frame the playhead is on.
                //
                // Opening a project set the playhead to zero, cleared
                // the frame buffer and stopped the engine — and then
                // asked nobody to decode anything, so the program
                // viewer stayed black until the user happened to move
                // the playhead, which is the first thing that issues a
                // seek. A new project opening onto an empty viewer
                // reads as a project that failed to load.
                //
                // Sent after the sequence and media pool are in the
                // engine, so there is something to decode.
                self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead)).ok();

                // Start file watcher for all media paths
                let media_paths: Vec<std::path::PathBuf> = self.state.media_pool.all()
                    .filter(|m| !m.offline)
                    .map(|m| m.path.clone())
                    .collect();
                self.media_watcher = crate::media_watcher::start_media_watcher(
                    &media_paths,
                    self.proxy_event_tx.clone(), // reuse proxy event channel for file watcher events
                );
            }
            Err(e) => {
                tracing::error!("Failed to load project: {e}");
                // What was decided about its lock was for this open.
                self.lock_decision = None;
                self.state.toast.show(format!("Could not open {}: {e}", path.display()));
                self.last_open_error = Some(e.to_string());
                self.note_project_event(ProjectEvent::Replaced, Some(format!("could not open: {e}")));
            }
        }
    }

    pub(super) fn show_unsaved_dialog(&mut self, ctx: &egui::Context) -> Vec<AppCommand> {
        let cmds = Vec::new();
        if !self.state.show_unsaved_dialog {
            return cmds;
        }

        let mut open = true;
        let mut answer: Option<UnsavedAnswer> = None;
        let project_name = self.state.project_path.as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled".to_string());

        egui::Window::new("Save Changes?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Do you want to save changes to \"{}\" before closing?",
                    project_name
                ));
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("Your changes will be lost if you don't save them.")
                        .size(11.0)
                        .color(self.theme.text_dim),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Don't Save").clicked() {
                        eprintln!("unsaved dialog: Don't Save clicked");
                        tracing::info!("unsaved dialog: Don't Save clicked");
                        answer = Some(UnsavedAnswer::DontSave);
                    }
                    if ui.button("Cancel").clicked() {
                        tracing::info!("unsaved dialog: Cancel clicked");
                        answer = answer.or(Some(UnsavedAnswer::Cancel));
                    }
                    let save_clicked = ui
                        .add_enabled(!self.unsaved_save_waiting, egui::Button::new("Save"))
                        .clicked();
                    if save_clicked {
                        tracing::info!("unsaved dialog: Save clicked");
                    }
                    if answer.is_none() && (save_clicked || self.unsaved_save_waiting) {
                        // The save happens here, now, and the action
                        // follows only if it succeeded. It used to be
                        // queued as a command and the action taken at
                        // once: Quit exited before the queue was
                        // drained, so nothing was saved; New and Open
                        // replaced the project first, so the save wrote
                        // the *new* one. Either way the edits the user
                        // had just chosen to keep were gone.
                        answer = Some(UnsavedAnswer::Save(self.save_project_interactively(save_clicked)));
                    }
                });
            });

        // The window's close button answers unless the project was just
        // saved or let go in the same frame.
        if !open && !matches!(answer, Some(UnsavedAnswer::DontSave | UnsavedAnswer::Save(Some(SaveOutcome::Saved)))) {
            answer = Some(UnsavedAnswer::Dismissed);
        }
        match answer.map(unsaved_step) {
            None | Some(UnsavedStep::Ask) => {}
            Some(UnsavedStep::Drop) => {
                self.state.show_unsaved_dialog = false;
                self.state.unsaved_dialog_action = UnsavedAction::None;
                self.unsaved_save_waiting = false;
            }
            Some(UnsavedStep::Proceed { discard }) => {
                self.unsaved_save_waiting = false;
                self.state.show_unsaved_dialog = false;
                if discard {
                    self.mark_saved();
                    // Clean up autosave backup since user chose not to save
                    // — only our own: read-only, it is the holder's
                    // crash recovery.
                    if let Some(ref path) = self.writable_project_dir() {
                        let autosave = autosave_path_for(path);
                        if autosave.exists() {
                            let _ = std::fs::remove_dir_all(&autosave);
                        }
                    }
                }
                let action = std::mem::replace(&mut self.state.unsaved_dialog_action, UnsavedAction::None);
                self.take_unsaved_action(action, discard);
            }
        }

        cmds
    }

    /// Go on with what the unsaved-changes dialog held back, once the
    /// changes are saved or let go (`discarded`).
    fn take_unsaved_action(&mut self, action: UnsavedAction, discarded: bool) {
        match action {
            UnsavedAction::NewProject => self.do_new_project(),
            UnsavedAction::OpenNewProjectDialog => {
                let root = self.projects_root();
                self.new_project_dialog.reset(Some(&root));
                let templates = self.project_templates_dir();
                self.new_project_dialog.load_templates(&templates);
                self.state.show_new_project_dialog = true;
            }
            UnsavedAction::OpenProject(p) => self.do_open_project(&p),
            UnsavedAction::OpenFromAnotherEditor(p) => self.open_from_another_editor_now(&p, uuid::Uuid::nil()),
            UnsavedAction::CloseProject => self.do_close_project(),
            UnsavedAction::Quit => {
                // HARD EXIT — bypass eframe's
                // window-close/wgpu-teardown chain
                // entirely. After 8+ rounds of
                // plugging specific wgpu drop-order
                // races, the SIGSEGV during teardown
                // turns out to be unfixable from app
                // code (engine thread's GpuEffectsPipeline
                // drop races eframe's wgpu state). The
                // OS reaps us cleanly.
                if discarded {
                    eprintln!("Klipp: Don't Save → exit(0) (skipping eframe teardown to avoid wgpu drop-race SEGV)");
                } else {
                    eprintln!("Klipp: Save → saved, exit(0)");
                }
                self.kill_children_for_exit();
                self.layout.save();
                self.exit_after_engine_shutdown();
            }
            UnsavedAction::None => {}
        }
    }
}

/// What happened to the open project, for the control socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectEvent {
    /// Written to disk: the document is the same one, wherever it went.
    Saved,
    /// Replaced in memory: opened, reopened, made new or closed.
    Replaced,
}

/// Whether a save or an open made this a different document for a
/// control client. A save where it already was — Ctrl+S — keeps every
/// id, and said "the project changed under you" to the next call for
/// nothing; a save to another path is a different document. Anything
/// that replaces the document in memory is, even the same file opened
/// again (Revert to Saved): its timeline is the one on disk. Nothing
/// moves for a save or an open that failed.
fn project_identity_moves(event: ProjectEvent, before: Option<&std::path::Path>, after: Option<&std::path::Path>, failed: bool) -> bool {
    !failed && (event == ProjectEvent::Replaced || before != after)
}

#[cfg(test)]
mod project_identity_tests {
    use super::{project_identity_moves, ProjectEvent::{Replaced, Saved}};
    use std::path::Path;

    #[test]
    fn a_save_where_it_already_was_keeps_the_identity() {
        let p = Path::new("/p/a.klipp");
        assert!(!project_identity_moves(Saved, Some(p), Some(p), false));
    }

    #[test]
    fn a_save_as_elsewhere_moves_it() {
        assert!(project_identity_moves(Saved, Some(Path::new("/p/a.klipp")), Some(Path::new("/p/b.klipp")), false));
        assert!(project_identity_moves(Saved, None, Some(Path::new("/p/a.klipp")), false));
    }

    #[test]
    fn reopening_the_same_file_moves_it() {
        let p = Path::new("/p/a.klipp");
        assert!(project_identity_moves(Replaced, Some(p), Some(p), false));
    }

    #[test]
    fn a_failed_save_or_open_moves_nothing() {
        assert!(!project_identity_moves(Saved, Some(Path::new("/p/a.klipp")), Some(Path::new("/p/b.klipp")), true));
        assert!(!project_identity_moves(Replaced, None, Some(Path::new("/p/a.klipp")), true));
    }
}

/// Why a save may not write a project, or `None` when it may: not one
/// open read-only here, and not one someone else has open (`holder`,
/// asked only when the lock is not ours), whose work a Save As over it
/// would replace as surely as two saves of one project.
fn save_refusal(read_only: bool, ours: bool, holder: impl FnOnce() -> Option<String>) -> Option<String> {
    if read_only {
        return Some("This project is open read-only, so it was not saved. Save As keeps your changes in a copy.".to_string());
    }
    if ours {
        return None;
    }
    holder().map(|who| format!("{who} has that project open, so it was not saved over. Choose another name."))
}

/// What to do about the lock of a project just opened.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LockStep {
    /// Already read-only here: leave it so.
    Nothing,
    /// The lock was free and is ours now: keep it alive.
    Held,
    /// The user chose to look without editing.
    ReadOnly,
    /// Take the lock, recording whom from.
    TakeOver(Option<klipp_project::lock::LockInfo>),
    /// Someone has it open and nobody asked the user (an agent or a
    /// restore opened it, or they opened it in the same instant): open
    /// read-only and say who.
    HeldElsewhere(klipp_project::lock::LockInfo),
}

/// The lock step for a project just opened: a read-only open stays so,
/// the user's answer to the lock prompt is followed, and only without
/// one is the lock tried (`acquire`), a dead holder's being taken over.
fn opened_lock_step(
    already_read_only: bool,
    decision: Option<LockDecision>,
    acquire: impl FnOnce() -> Result<(), Box<(klipp_project::lock::LockInfo, klipp_project::lock::Liveness)>>,
) -> LockStep {
    use klipp_project::lock::Liveness;
    match decision {
        _ if already_read_only => LockStep::Nothing,
        Some(LockDecision::ReadOnly) => LockStep::ReadOnly,
        Some(LockDecision::TakeOver(from)) => LockStep::TakeOver(from),
        None => match acquire().map_err(|held| *held) {
            Ok(()) => LockStep::Held,
            Err((info, Liveness::Dead)) => LockStep::TakeOver(Some(info)),
            Err((info, _)) => LockStep::HeldElsewhere(info),
        },
    }
}

/// How the unsaved-changes dialog was answered this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnsavedAnswer {
    DontSave,
    Cancel,
    /// Closed with the window's close button.
    Dismissed,
    /// Save, and what came of it: `None` while its Save As picker is
    /// open, or when there was nothing to wait for.
    Save(Option<SaveOutcome>),
}

/// What follows an answer to the unsaved-changes dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnsavedStep {
    /// The question stays open.
    Ask,
    /// The dialog closes and what it held back (a close, an open, a
    /// quit) does not happen.
    Drop,
    /// The dialog closes and what it held back goes ahead; `discard`
    /// when the changes, and their auto-save, are let go.
    Proceed { discard: bool },
}

/// The unsaved-changes dialog's rule: the project is closed, replaced or
/// quit only once its changes are saved or the user lets them go. A
/// failed save keeps the question open (the failure was said in a
/// toast), and a dismissed Save As picker is a change of mind about the
/// whole thing.
fn unsaved_step(answer: UnsavedAnswer) -> UnsavedStep {
    match answer {
        UnsavedAnswer::DontSave => UnsavedStep::Proceed { discard: true },
        UnsavedAnswer::Save(Some(SaveOutcome::Saved)) => UnsavedStep::Proceed { discard: false },
        UnsavedAnswer::Cancel | UnsavedAnswer::Dismissed | UnsavedAnswer::Save(Some(SaveOutcome::Cancelled)) => UnsavedStep::Drop,
        UnsavedAnswer::Save(None | Some(SaveOutcome::Failed)) => UnsavedStep::Ask,
    }
}

#[cfg(test)]
mod decision_tests {
    use super::*;
    use klipp_project::lock::{LockInfo, Liveness};

    fn someone() -> LockInfo {
        LockInfo { user: "ana".into(), host: "studio".into(), ..Default::default() }
    }

    #[test]
    fn the_project_goes_only_once_its_changes_are_saved_or_let_go() {
        assert_eq!(unsaved_step(UnsavedAnswer::DontSave), UnsavedStep::Proceed { discard: true });
        assert_eq!(unsaved_step(UnsavedAnswer::Save(Some(SaveOutcome::Saved))), UnsavedStep::Proceed { discard: false });
        // A failed save must not close, replace or quit the project.
        assert_eq!(unsaved_step(UnsavedAnswer::Save(Some(SaveOutcome::Failed))), UnsavedStep::Ask);
        assert_eq!(unsaved_step(UnsavedAnswer::Save(None)), UnsavedStep::Ask, "the picker is still open");
        for answer in [UnsavedAnswer::Cancel, UnsavedAnswer::Dismissed, UnsavedAnswer::Save(Some(SaveOutcome::Cancelled))] {
            assert_eq!(unsaved_step(answer), UnsavedStep::Drop, "{answer:?}");
        }
    }

    #[test]
    fn a_save_refuses_a_read_only_project_and_one_someone_else_has_open() {
        let read_only = save_refusal(true, false, || panic!("a read-only project needs no lock check")).unwrap();
        assert!(read_only.contains("read-only"), "{read_only}");
        assert_eq!(save_refusal(false, true, || panic!("our own lock needs no check")), None);
        let theirs = save_refusal(false, false, || Some(someone().who())).unwrap();
        assert!(theirs.starts_with("ana@studio has that project open"), "{theirs}");
        assert_eq!(save_refusal(false, false, || None), None, "nobody has it open");
    }

    #[test]
    fn an_opened_project_follows_the_users_answer_and_tries_the_lock_only_without_one() {
        let untried = || -> Result<(), Box<(LockInfo, Liveness)>> { panic!("the lock is not tried") };
        assert_eq!(opened_lock_step(true, Some(LockDecision::TakeOver(None)), untried), LockStep::Nothing, "read-only stays so");
        assert_eq!(opened_lock_step(false, Some(LockDecision::ReadOnly), untried), LockStep::ReadOnly);
        assert_eq!(opened_lock_step(false, Some(LockDecision::TakeOver(Some(someone()))), untried), LockStep::TakeOver(Some(someone())));
        assert_eq!(opened_lock_step(false, None, || Ok(())), LockStep::Held);
        assert_eq!(opened_lock_step(false, None, || Err(Box::new((someone(), Liveness::Dead)))), LockStep::TakeOver(Some(someone())), "a dead holder's lock is taken");
        for alive in [Liveness::Active, Liveness::Stale] {
            assert_eq!(opened_lock_step(false, None, || Err(Box::new((someone(), alive)))), LockStep::HeldElsewhere(someone()), "{alive:?}");
        }
    }
}

/// Count how many clips across all sequences reference this media_id.
/// Persist the current custom workspace list to
/// `~/.config/klipp/workspaces.json`. Errors are swallowed because this
/// is a best-effort UI persistence write — if it fails the user gets
/// the runtime state for this session and a warning in the log.
pub(super) fn persist_workspaces(workspaces: &[crate::state::SavedWorkspaceLayout]) {
    if let Ok(json) = serde_json::to_string_pretty(workspaces) {
        let path = dirs::config_dir()
            .unwrap_or_default()
            .join("klipp/workspaces.json");
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")));
        if let Err(e) = std::fs::write(&path, json) {
            tracing::warn!("Failed to persist workspaces.json: {e}");
        }
    }
}

/// Returns the autosave path for a given project path: `<stem>_autosave.klipp`
pub(crate) fn autosave_path_for(project_path: &Path) -> PathBuf {
    klipp_project::recovery::autosave_path_for(project_path)
}

/// Whether a path is an auto-save folder rather than a project.
pub(crate) fn is_autosave_path(path: &Path) -> bool {
    path.file_stem().is_some_and(|s| s.to_string_lossy().ends_with("_autosave"))
}

/// The project an auto-save folder belongs to: `X_autosave.klipp` → `X.klipp`.
pub(crate) fn project_for_autosave(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_string_lossy();
    let main = stem.strip_suffix("_autosave").filter(|m| !m.is_empty())?;
    Some(path.with_file_name(format!("{main}.klipp")))
}

/// Mark a project's auto-save as seen after the user chose the saved
/// version over it, so neither the open prompt nor the browser's "Unsaved
/// work recovered" badge offers it again; a later auto-save is offered
/// (#2789). The auto-save itself is left on disk, and goes the way it
/// always has, on the next clean save or close.
pub(crate) fn retire_autosave(project_path: &Path) {
    if let Err(e) = klipp_project::recovery::keep_saved(project_path) {
        tracing::warn!("Could not mark the auto-save of {project_path:?} as seen: {e}");
    }
}

/// What to say at startup when projects among `recent` hold unsaved work
/// in an auto-save newer than their last save (#2789), which a clean
/// close never leaves: Klipp stopped without saving.
pub(crate) fn crash_notice(recent: &[PathBuf]) -> Option<String> {
    let kept: Vec<String> = recent.iter().filter(|p| has_newer_autosave(p)).map(|p| p.file_stem().unwrap_or_default().to_string_lossy().into_owned()).collect();
    match kept.as_slice() {
        [] => None,
        [one] => Some(format!("Unsaved work in \u{201c}{one}\u{201d} was recovered: open it to restore or set it aside")),
        many => Some(format!("Unsaved work was recovered in {} projects ({}): open one to restore or set it aside", many.len(), many.join(", "))),
    }
}

/// Whether a project has an auto-save newer than its last save: work
/// that was never saved, usually because Klipp closed unexpectedly.
pub(crate) fn has_newer_autosave(project_path: &Path) -> bool {
    klipp_project::recovery::newer_autosave(project_path).is_some()
}

/// Generate an ISO 8601 UTC timestamp string.
pub fn iso8601_now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

pub(super) fn ensure_klipp_extension(mut path: PathBuf) -> PathBuf {
    if path.extension().and_then(|e| e.to_str()) != Some("klipp") {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if !name.ends_with(".klipp") {
            path.set_file_name(format!("{}.klipp", name));
        }
    }
    path
}

#[cfg(test)]
mod global_lut_tests {
    use super::opened_global_lut;

    /// A project whose LUT is not on this machine keeps naming it, so a
    /// save writes it back; it is not applied, and the engine is told to
    /// drop the previous project's LUT rather than keep grading with it
    /// (#1019).
    #[test]
    fn a_missing_global_lut_is_kept_and_not_applied() {
        use klipp_core::AppCommand;
        let missing = std::path::Path::new("/nonexistent/klipp-1019/look.cube");
        let (kept, to_engine) = opened_global_lut(Some(missing), 0.5, true);
        assert_eq!(kept, Some(missing.to_path_buf()));
        assert!(matches!(to_engine.as_slice(), [AppCommand::ClearGlobalLut]), "a missing LUT clears the engine's");

        let (kept, to_engine) = opened_global_lut(None, 1.0, true);
        assert_eq!(kept, None);
        assert!(matches!(to_engine.as_slice(), [AppCommand::ClearGlobalLut]), "no LUT clears the engine's too");

        let here = std::env::current_exe().unwrap();
        let (kept, to_engine) = opened_global_lut(Some(&here), 0.5, false);
        assert_eq!(kept, Some(here.clone()));
        assert!(matches!(
            to_engine.as_slice(),
            [AppCommand::LoadGlobalLut { path }, AppCommand::SetGlobalLutOpacity(o), AppCommand::SetGlobalLutEnabled(false)] if *path == here && *o == 0.5
        ));
    }
}
