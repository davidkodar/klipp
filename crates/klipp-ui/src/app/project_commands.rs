//! The project's commands: new, open, save and close asked for, recent
//! projects, and the track settings and their attributes (#826).

use super::*;

impl KlippApp {
    /// Reset to a new project's first sequence, in this format or from
    /// the template file (its copied media going into `project_dir`).
    /// `false`, with the reason shown, when the template does not read;
    /// the project open stays open then.
    #[allow(clippy::too_many_arguments)]
    fn start_new_project(
        &mut self,
        name: &str,
        sequence_name: &str,
        (fps_num, fps_den): (u32, u32),
        (width, height): (u32, u32),
        template: Option<&std::path::Path>,
        project_dir: &std::path::Path,
        hdr: bool,
    ) -> bool {
        let (fps_num, fps_den, width, height) = (&fps_num, &fps_den, &width, &height);
        // From a template (#618): read before anything is reset, so
        // one that no longer reads leaves the open project open.
        // Its starter content's copied media goes into the new
        // project's folder.
        let made = template.map(|p| klipp_project::template::ProjectTemplate::load(p).and_then(|t| t.instantiate_at(p, project_dir)));
        let from_template = match made {
            None => None,
            Some(Ok(t)) => Some(t),
            Some(Err(e)) => {
                self.state.toast.show(format!("The template could not be read: {e}"));
                return false;
            }
        };
        // "Automatic" FPS: use 24fps as placeholder, auto-set from first clip
        let fps = if *fps_num == 0 || *fps_den == 0 {
            klipp_core::Fps::new(24, 1)
        } else {
            klipp_core::Fps::new(*fps_num, *fps_den)
        };
        // "Automatic" resolution: use 1920×1080 as placeholder, auto-set from first clip
        let resolution = if *width == 0 || *height == 0 {
            klipp_core::Resolution { width: 1920, height: 1080 }
        } else {
            klipp_core::Resolution { width: *width, height: *height }
        };
        let seq_name = if sequence_name.is_empty() { name } else { sequence_name }.to_string();
        let seq = klipp_timeline::Sequence {
            // A new sequence starts without a camera; a nest that goes
            // back into its parent is moved by the parent's.
            camera: klipp_timeline::Camera::default(),
            timeline_graph: None,
            grade_groups: Vec::new(),
            title_nits: klipp_timeline::DEFAULT_TITLE_NITS,
            node_labels: Vec::new(),
            drop_frame: klipp_timeline::Sequence::default_drop_frame(fps),
            id: klipp_core::SequenceId::new(),
            name: seq_name,
            fps,
            resolution,
            first_clip_autoset: klipp_timeline::FirstClipAutoSet {
                resolution: *width == 0 || *height == 0,
                fps: *fps_num == 0 || *fps_den == 0,
            },
            tracks: {
                let mut tracks = vec![
                    klipp_timeline::Track::new("Video 1", klipp_timeline::TrackKind::Video),
                    klipp_timeline::Track::new("Audio 1", klipp_timeline::TrackKind::Audio),
                ];
                tracks[0].magnetic_role = klipp_timeline::TrackRole::Primary;
                tracks[1].magnetic_role = klipp_timeline::TrackRole::Connected;
                tracks
            },
            in_point: None,
            out_point: None,
            markers: Vec::new(),
            last_playhead: None,
            brief: None,
            edit_plan: None,
            reviews: Vec::new(),
            awaiting_refit: false,
            version_of: None,
        };
        // The same reset closing a project does. Resetting only the
        // UI's copy left the engine on the last project: its
        // decoders, parked frame and playhead, and its media pool,
        // so the new timeline was drawn with the old footage.
        self.media_watcher = None;
        self.reset_project_state();
        self.state.sequences = vec![seq];
        let mut fps = fps;
        if let Some(made) = from_template {
            self.apply_template(made);
            fps = self.state.sequences[0].fps;
        }
        self.state.active_sequence_idx = 0;
        self.state.fps = fps;
        self.state.created_at = Some(klipp_project::iso8601_now());
        self.state.filmstrip_generating = false;
        let seq_rate = self.state.sequences[self.state.active_sequence_idx].fps.rate();
        self.state.playhead = klipp_core::RationalTime::from_frames(0, seq_rate);
        self.state.filmstrip_total_queued = 0;
        self.state.filmstrip_completed = 0;
        if hdr {
            self.state.project_working_colorspace = klipp_core::WorkingColorspace::Bt2020Linear;
        }
        self.sync_sequence_to_engine();
        true
    }

    /// A track's attributes copied and pasted, and its own effects added,
    /// switched and taken off (#846): each change one undo step, through
    /// `SetTrackSettings`.
    fn handle_track_settings_commands(&mut self, cmd: &AppCommand) -> bool {
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let track = |id: &klipp_core::TrackId| seq.tracks.iter().find(|t| t.id == *id);
        // One track's settings changed by `change`: the edit, or `None`
        // when nothing changed.
        let one = |id: &klipp_core::TrackId, change: &dyn Fn(&mut klipp_timeline::TrackSettings)| -> Option<EditCommand> {
            let t = track(id)?;
            let old = t.settings();
            let mut new = old.clone();
            change(&mut new);
            (new != old).then(|| EditCommand::SetTrackSettings { changes: vec![(t.id, old, new)] })
        };
        let (edit, label): (Option<EditCommand>, &str) = match cmd {
            AppCommand::CopyTrackAttributes { track_id } => {
                if let Some(t) = track(track_id) {
                    self.state.copied_track_settings = Some((t.name.clone(), t.settings()));
                    self.state.toast.show(format!("Copied the attributes of {}", t.name));
                }
                return true;
            }
            AppCommand::PasteTrackAttributes { track_ids, levels, picture, sound } => {
                let Some((_, copied)) = self.state.copied_track_settings.as_ref() else {
                    self.state.toast.show("Copy a track's attributes first");
                    return true;
                };
                let what = klipp_timeline::TrackPaste { levels: *levels, picture: *picture, sound: *sound };
                // A locked track is left as it is, as a locked track's clips
                // are, and said.
                let (locked, open): (Vec<_>, Vec<_>) = track_ids.iter().filter_map(track).partition(|t| t.locked);
                if !locked.is_empty() {
                    let n = locked.len();
                    self.state.toast.show(if open.is_empty() {
                        format!("{} is locked; nothing was pasted", locked[0].name)
                    } else {
                        format!("{n} locked track{} left as {}", if n == 1 { "" } else { "s" }, if n == 1 { "it was" } else { "they were" })
                    });
                }
                let changes: Vec<_> = open
                    .into_iter()
                    .map(|t| (t.id, t.settings(), t.pasted(copied, what)))
                    .filter(|(_, old, new)| old != new)
                    .collect();
                ((!changes.is_empty()).then_some(EditCommand::SetTrackSettings { changes }), "Paste Track Attributes")
            }
            AppCommand::AddTrackVideoEffect { track_id, kind } => match klipp_document::ops::named_picture_effect(kind, None) {
                Ok((fx, _)) => (
                    one(track_id, &|s| {
                        // One of a kind: a second vignette replaces the first,
                        // and is on, as the op leaves it.
                        s.effects.edit_video_effects(|video, enabled| {
                            let at = video.iter().position(|e| std::mem::discriminant(e) == std::mem::discriminant(&fx));
                            match at {
                                Some(k) => {
                                    video[k] = fx.clone();
                                    enabled.resize(video.len(), true);
                                    enabled[k] = true;
                                }
                                None => video.push(fx.clone()),
                            }
                        });
                    }),
                    "Add Track Effect",
                ),
                Err((why, _)) => {
                    self.state.toast.show(why);
                    return true;
                }
            },
            AppCommand::AddTrackAudioEffect { track_id, effect_type } => {
                let fx = self.default_audio_effect(effect_type);
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let edit = seq.tracks.iter().find(|t| t.id == *track_id).map(|t| {
                    let old = t.settings();
                    let mut new = old.clone();
                    new.effects.audio.push(fx);
                    EditCommand::SetTrackSettings { changes: vec![(t.id, old, new)] }
                });
                (edit, "Add Track Audio Effect")
            }
            AppCommand::ToggleTrackEffect { track_id, audio, index } => (
                one(track_id, &|s| {
                    if *audio {
                        if let Some(e) = s.effects.audio.get_mut(*index) {
                            e.enabled = !e.enabled;
                        }
                    } else {
                        s.effects.edit_video_effects(|video, enabled| {
                            if *index < video.len() {
                                enabled.resize(video.len(), true);
                                enabled[*index] = !enabled[*index];
                            }
                        });
                    }
                }),
                "Toggle Track Effect",
            ),
            AppCommand::RemoveTrackEffect { track_id, audio, index } => (
                one(track_id, &|s| {
                    if *audio {
                        if *index < s.effects.audio.len() {
                            s.effects.audio.remove(*index);
                        }
                    } else {
                        s.effects.edit_video_effects(|video, enabled| {
                            if *index < video.len() {
                                video.remove(*index);
                                if *index < enabled.len() {
                                    enabled.remove(*index);
                                }
                            }
                        });
                    }
                }),
                "Remove Track Effect",
            ),
            AppCommand::ClearTrackEffects { track_id } => (one(track_id, &|s| s.effects = Default::default()), "Clear Track Effects"),
            _ => return false,
        };
        if let Some(edit) = edit {
            self.apply_edits_as_one(&[edit], label);
        }
        true
    }

    /// Make sequence `sequence_idx` the one shown, as its tab does
    /// ([`crate::state::AppState::switch_sequence`]), then stop playback
    /// and give the engine the sequence.
    fn switch_active_sequence(&mut self, sequence_idx: usize, keep_view: bool) {
        if self.state.switch_sequence(sequence_idx, keep_view) {
            self.cmd_tx.send(AppCommand::Stop).ok();
            self.sync_sequence_to_engine();
        }
    }

    pub(super) fn handle_project_commands(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        if self.handle_track_settings_commands(cmd) {
            return true;
        }
        match cmd {
            AppCommand::NewProject => {
                if self.is_unsaved() {
                    self.state.show_unsaved_dialog = true;
                    self.state.unsaved_dialog_action = UnsavedAction::OpenNewProjectDialog;
                    return true;
                }
                let root = self.projects_root();
                self.new_project_dialog.reset(Some(&root));
                let templates = self.project_templates_dir();
                self.new_project_dialog.load_templates(&templates);
                self.state.show_new_project_dialog = true;
                true
            }
            AppCommand::CloseProject => {
                if self.state.project_path.is_none() {
                    return true; // Already on launch screen
                }
                if self.is_unsaved() {
                    self.state.show_unsaved_dialog = true;
                    self.state.unsaved_dialog_action = UnsavedAction::CloseProject;
                    return true;
                }
                self.do_close_project();
                true
            }
            AppCommand::SaveProject => {
                let path = self.state.project_path.clone();
                let untitled = crate::project_paths::is_untitled(path.as_deref());
                match path {
                    Some(path) if !untitled && self.unnamed_project.as_ref() != Some(&path) => {
                        self.do_save_project(&path);
                    }
                    // Never named: ask its name and where, off the UI
                    // thread, starting in the projects root (#1864). The
                    // answer comes back as Save As.
                    _ => {
                        let root = self.projects_root();
                        // A name not yet taken there, so accepting the
                        // offer never lands on another project.
                        let name = crate::project_paths::free_project_name(&root, crate::project_paths::UNTITLED);
                        crate::file_dialog::spawn(move || {
                            rfd::FileDialog::new()
                                .set_title("Save Project")
                                .set_directory(&root)
                                .set_file_name(&name)
                                .save_file()
                                .map(|path| vec![AppCommand::SaveProjectAs { path }])
                                .unwrap_or_default()
                        });
                    }
                }
                true
            }
            AppCommand::SaveProjectAs { path } => {
                let path = ensure_klipp_extension(path.clone());
                // Never over another project: it would write its
                // project.json over that one's.
                if let Some(why) = crate::project_paths::save_as_refusal(&path, self.state.project_path.as_deref()) {
                    self.state.toast.show(why);
                    return true;
                }
                if self.project_copy.is_some() {
                    self.state.toast.show("The project is still being copied; save again once it is done.");
                    return true;
                }
                // The Untitled one written on its first edit moves to the
                // name and place given, rather than leaving a copy behind.
                // The lock and the unnamed mark stay with it until the
                // move has happened; on a failure it stays the locked
                // Untitled project it was.
                if let Some(written) = self.unnamed_project.clone().filter(|w| *w != path && self.state.project_path.as_ref() == Some(w)) {
                    self.wait_for_autosave();
                    match crate::project_paths::rename_project_folder(&written, &path) {
                        Ok(crate::project_paths::RenameOutcome::Renamed) => {
                            self.lock_keeper.take();
                            self.finish_project_move(&written, &path, true);
                        }
                        // Another filesystem: copied on a thread of its
                        // own, with progress; the original is kept.
                        Ok(crate::project_paths::RenameOutcome::NeedsCopy) => {
                            let (tx, done) = std::sync::mpsc::channel();
                            let copied = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
                            let total = crate::project_paths::tree_size(&written);
                            let (from, to, counter) = (written.clone(), path.clone(), copied.clone());
                            let spawned = std::thread::Builder::new().name("project-copy".into()).spawn(move || {
                                let _ = tx.send(crate::project_paths::copy_project_tree_counting(&from, &to, &counter));
                            });
                            match spawned {
                                Ok(_) => {
                                    self.project_copy = Some(ProjectCopy { from: written, to: path, done, copied, total, shown_at: std::time::Instant::now() });
                                }
                                Err(e) => self.state.toast.show(format!("The project could not be copied there: {e}")),
                            }
                        }
                        Err(e) => {
                            tracing::error!("Could not move {written:?} to {path:?}: {e}");
                            self.state.toast.show(format!("The project could not be moved there: {e}. It stays where it was."));
                        }
                    }
                    return true;
                }
                self.do_save_project(&path);
                true
            }
            AppCommand::CloseProjectNow { save, discard_changes, expected_identity } => {
                if self.prepare_control_project_switch(*expected_identity, *save, *discard_changes) {
                    self.do_close_project();
                }
                true
            }
            AppCommand::OpenProjectNow { path, save, discard_changes, expected_identity, recovery } => {
                // As OpenProject resolves it: the control socket's open
                // names a project.json as readily as its folder (#1143).
                let path = crate::project_paths::project_dir_of(path);
                // The lock and the auto-save before anything is saved or
                // closed: a refused open leaves the open project as it was.
                if !self.control_open_checks(&path, *recovery) {
                    return true;
                }
                if !self.prepare_control_project_switch(*expected_identity, *save, *discard_changes) { return true; }
                match (klipp_project::recovery::newer_autosave(&path), recovery) {
                    (Some(_), Some(true)) => self.restore_autosave(&path),
                    (Some(_), Some(false)) => self.keep_saved_version(&path),
                    _ => self.do_open_project_inner(&path),
                }
                true
            }
            AppCommand::OpenProject { path } => {
                // Whatever was picked or named, the project it is in (#1143).
                let path = &crate::project_paths::project_dir_of(path);
                if self.is_unsaved() {
                    self.state.show_unsaved_dialog = true;
                    self.state.unsaved_dialog_action =
                        UnsavedAction::OpenProject(path.clone());
                    return true;
                }
                self.do_open_project(path);
                true
            }
            AppCommand::SetProjectsFolder { path } => {
                self.prefs.general.default_project_dir = Some(path.clone());
                self.prefs_dirty = true;
                true
            }
            AppCommand::RemoveRecentProject { path } => {
                self.prefs.general.recent_projects.retain(|rp| rp.path != *path);
                self.prefs_dirty = true;
                true
            }
            // Part of the project's colour set-up: an undo step (#1875).
            AppCommand::SetProjectViewingLut { path } => {
                let mut colour = crate::panels::colour_management::project_colour(&self.state);
                colour.viewing_lut = path.clone();
                self.set_project_colour(colour, false);
                true
            }
            AppCommand::SetProjectInputLut { path } => {
                let mut colour = crate::panels::colour_management::project_colour(&self.state);
                colour.input_lut = path.clone();
                self.set_project_colour(colour, false);
                true
            }
            AppCommand::SetFormatRate { rate } => {
                self.prefs.general.last_format_rate = Some(*rate);
                self.prefs_dirty = true;
                true
            }
            AppCommand::OpenPreferences { page } => {
                if let Some(page) = page {
                    self.preferences_panel.open_page(page);
                }
                self.state.show_preferences = true;
                true
            }
            AppCommand::PinRecentProject { path, pinned } => {
                if let Some(rp) = self.prefs.general.recent_projects.iter_mut().find(|rp| rp.path == *path) {
                    rp.pinned = *pinned;
                    self.prefs_dirty = true;
                }
                true
            }
            AppCommand::DuplicateProject { path } => {
                match crate::project_paths::duplicate_project(path) {
                    Ok(copy) => {
                        let name = copy.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                        let mut rp = self.prefs.general.recent_projects.iter()
                            .find(|rp| rp.path == *path)
                            .cloned()
                            .unwrap_or_else(|| crate::prefs::RecentProject::from_path(path.clone()));
                        rp.path = copy;
                        rp.name = name.clone();
                        rp.pinned = false;
                        rp.last_saved = Some(crate::app::iso8601_now());
                        self.prefs.add_recent_project(rp);
                        self.prefs_dirty = true;
                        self.state.toast.show(format!("Duplicated as {name}"));
                    }
                    Err(e) => {
                        tracing::error!("Failed to duplicate project {path:?}: {e}");
                        self.state.toast.show(format!("The project could not be duplicated: {e}"));
                    }
                }
                true
            }
            AppCommand::MoveProjectToTrash { path } => {
                if let Some(who) = self.lock_held_elsewhere(path) {
                    self.state.toast.show(format!("{who} has that project open, so it was not moved to the trash"));
                    return true;
                }
                match crate::trash::move_to_trash(path) {
                    Ok(_) => {
                        tracing::info!("Moved project {:?} to the trash", path);
                        self.prefs.general.recent_projects.retain(|rp| rp.path != *path);
                        self.prefs_dirty = true;
                        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                        self.state.toast.show(format!("{name} moved to the trash"));
                    }
                    Err(e) => {
                        tracing::error!("Failed to move project {path:?} to the trash: {e}");
                        self.state.toast.show(format!("The project could not be moved to the trash: {e}"));
                    }
                }
                true
            }
            AppCommand::RenameProjectOnDisk { old_path, new_name } => {
                if let Some(who) = self.lock_held_elsewhere(old_path) {
                    self.state.toast.show(format!("{who} has that project open, so it was not renamed"));
                    return true;
                }
                if let Some(parent) = old_path.parent() {
                    let new_dir_name = format!("{new_name}.klipp");
                    let new_path = parent.join(&new_dir_name);
                    if new_path.exists() {
                        tracing::error!("Cannot rename: {:?} already exists", new_path);
                    } else if let Err(e) = std::fs::rename(old_path, &new_path) {
                        tracing::error!("Failed to rename project: {e}");
                    } else {
                        tracing::info!("Renamed project {:?} -> {:?}", old_path, new_path);
                        // Update recent projects entry
                        for rp in &mut self.prefs.general.recent_projects {
                            if rp.path == *old_path {
                                rp.path = new_path.clone();
                                rp.name = new_name.clone();
                                break;
                            }
                        }
                        // Update current project path if this is the open project
                        if self.state.project_path.as_ref() == Some(old_path) {
                            self.state.project_path = Some(new_path);
                        }
                        self.prefs_dirty = true;
                    }
                }
                true
            }
            AppCommand::SetProjectThumbnailFromViewer => {
                if let Some(ref project_path) = self.state.project_path.clone() {
                    if let Some(thumb_path) = capture_viewer_frame_as_thumbnail(&self.frame_buffer, project_path) {
                        // Update recent project entry
                        for rp in &mut self.prefs.general.recent_projects {
                            if rp.path == *project_path {
                                // Clear cached texture so it reloads
                                if let Some(old) = rp.thumbnail_path.as_ref() {
                                    self.launch_screen.invalidate_thumbnail(old);
                                }
                                rp.thumbnail_path = Some(thumb_path.clone());
                                break;
                            }
                        }
                        self.prefs_dirty = true;
                        self.state.toast.show("Project thumbnail set from current frame");
                        tracing::info!("Set project thumbnail from viewer: {:?}", thumb_path);
                    } else {
                        self.state.toast.show("No frame available in viewer");
                    }
                }
                true
            }
            AppCommand::SetProjectThumbnailFromFile { path } => {
                if let Some(ref project_path) = self.state.project_path.clone() {
                    if let Some(thumb_path) = set_thumbnail_from_image_file(path, project_path) {
                        for rp in &mut self.prefs.general.recent_projects {
                            if rp.path == *project_path {
                                if let Some(old) = rp.thumbnail_path.as_ref() {
                                    self.launch_screen.invalidate_thumbnail(old);
                                }
                                rp.thumbnail_path = Some(thumb_path.clone());
                                break;
                            }
                        }
                        self.prefs_dirty = true;
                        self.state.toast.show("Project thumbnail updated");
                        tracing::info!("Set project thumbnail from file: {:?}", thumb_path);
                    } else {
                        self.state.toast.show("Failed to load image for thumbnail");
                    }
                }
                true
            }
            AppCommand::CreateProject {
                name, location, fps_num, fps_den, width, height, sequence_name, template, hdr,
            } => {
                let project_dir = location.join(format!("{name}.klipp"));
                if !self.start_new_project(name, sequence_name, (*fps_num, *fps_den), (*width, *height), template.as_deref(), &project_dir, *hdr) {
                    return true;
                }
                self.state.project_path = Some(project_dir.clone());
                // Save immediately so the .klipp directory is created. The
                // projects root is the user's to choose in Preferences: a
                // place one project was made is not where every next one
                // goes (#1864).
                self.do_save_project(&project_dir);
                tracing::info!("Created new project at {:?}", project_dir);
                true
            }
            AppCommand::NewUntitledProject { fps_num, fps_den, width, height, template, hdr } => {
                // Unsaved and Untitled: written on its first real edit or a
                // save, and closed untouched it leaves nothing (#1864). A
                // template's copied media waits in the cache till then.
                let staging = match crate::project_paths::make_staging(&crate::project_paths::untitled_cache_dir(), &self.lock_holder) {
                    Ok(staging) => staging,
                    Err(e) => {
                        self.state.toast.show(format!("A new project could not be started: {e}"));
                        return true;
                    }
                };
                let untitled = crate::project_paths::UNTITLED;
                if !self.start_new_project(untitled, untitled, (*fps_num, *fps_den), (*width, *height), template.as_deref(), &staging, *hdr) {
                    return true;
                }
                self.state.project_path = Some(PathBuf::from(crate::project_paths::UNTITLED));
                self.untitled_staging = Some(staging);
                self.mark_saved();
                self.state.reset_ops_since_save();
                true
            }
            AppCommand::SaveProjectTemplate { name, description, starter, package_media } => {
                let setup = klipp_project::template::ProjectSetup {
                    sequences: &self.state.sequences,
                    pool: &self.state.media_pool,
                    colour: self.template_colour(),
                };
                let choice = klipp_project::template::StarterChoice { sequences: starter.clone(), package_media: *package_media };
                let (template, files) = klipp_project::template::ProjectTemplate::capture_with_starter(name, description, &setup, &choice);
                match template.save_with_starter(&self.project_templates_dir(), files.as_ref()) {
                    Ok(_) => self.state.toast.show(format!("Saved the project template \"{}\": {}", template.name, template.summary())),
                    Err(e) => self.state.toast.show(format!("The template could not be saved: {e}")),
                }
                true
            }
            AppCommand::DeleteProjectTemplate { path } => {
                // Only a template in the templates folder; one with starter
                // content is a folder.
                if path.parent() == Some(self.project_templates_dir().as_path()) {
                    let removed = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
                    match removed {
                        Ok(()) => self.state.toast.show("Template deleted"),
                        Err(e) => self.state.toast.show(format!("The template could not be deleted: {e}")),
                    }
                }
                true
            }
            AppCommand::RenameProjectTemplate { path, name } => {
                if path.parent() == Some(self.project_templates_dir().as_path()) {
                    match klipp_project::template::rename_template(path, name) {
                        Ok(_) => self.state.toast.show(format!("Template renamed \"{}\"", name.trim())),
                        Err(e) => self.state.toast.show(format!("The template could not be renamed: {e}")),
                    }
                }
                let dir = self.project_templates_dir();
                self.new_project_dialog.load_templates(&dir);
                true
            }
            AppCommand::ExportProjectTemplate { path, to } => {
                match klipp_project::template::export_template(path, to) {
                    Ok(()) => self.state.toast.show(format!("Template exported to {}", to.display())),
                    Err(e) => self.state.toast.show(format!("The template could not be exported: {e}")),
                }
                true
            }
            AppCommand::ImportProjectTemplate { from } => {
                let dir = self.project_templates_dir();
                match klipp_project::template::import_template(from, &dir) {
                    Ok(path) => {
                        let name = klipp_project::template::ProjectTemplate::load(&path).map(|t| t.name).unwrap_or_default();
                        self.state.toast.show(format!("Imported the template \"{name}\""));
                    }
                    Err(e) => self.state.toast.show(format!("The template could not be imported: {e}")),
                }
                self.new_project_dialog.load_templates(&dir);
                true
            }
            AppCommand::AddToSharedLibrary { .. }
            | AppCommand::AddMediaToSharedLibrary { .. }
            | AppCommand::RemoveFromSharedLibrary { .. }
            | AppCommand::RelinkSharedLibraryItem { .. }
            | AppCommand::CreateSharedCollection { .. }
            | AppCommand::RenameSharedCollection { .. }
            | AppCommand::DeleteSharedCollection { .. } => {
                crate::shared_library::run_command(&mut self.state, cmd)
            }
            AppCommand::SetActiveSequence { sequence_idx } => {
                self.switch_active_sequence(*sequence_idx, false);
                true
            }
            AppCommand::FocusSequence { sequence_idx } => {
                self.switch_active_sequence(*sequence_idx, true);
                true
            }
            AppCommand::AddSequenceAndOpen { name, fps_num, fps_den, width, height } => {
                let index = self.state.sequences.len();
                self.send_commands(vec![
                    AppCommand::AddSequence { name: name.clone(), fps_num: *fps_num, fps_den: *fps_den, width: *width, height: *height },
                    AppCommand::SetActiveSequence { sequence_idx: index },
                ]);
                true
            }
            AppCommand::AddSequence { name, fps_num, fps_den, width, height } => {
                let seq = klipp_timeline::Sequence {
                    // A new sequence starts without a camera; a nest that goes
                    // back into its parent is moved by the parent's.
                    camera: klipp_timeline::Camera::default(),
                    timeline_graph: None,
                    grade_groups: Vec::new(),
                    title_nits: klipp_timeline::DEFAULT_TITLE_NITS,
                    node_labels: Vec::new(),
                    drop_frame: klipp_timeline::Sequence::default_drop_frame(
                        klipp_core::Fps::new(*fps_num, *fps_den),
                    ),
                    id: klipp_core::SequenceId::new(),
                    name: name.clone(),
                    fps: klipp_core::Fps::new(*fps_num, *fps_den),
                    resolution: klipp_core::Resolution { width: *width, height: *height },
                    first_clip_autoset: klipp_timeline::FirstClipAutoSet { resolution: false, fps: false },
                    tracks: {
                        let mut tracks = vec![
                            klipp_timeline::Track::new("Video 1", klipp_timeline::TrackKind::Video),
                            klipp_timeline::Track::new("Audio 1", klipp_timeline::TrackKind::Audio),
                        ];
                        tracks[0].magnetic_role = klipp_timeline::TrackRole::Primary;
                        tracks[1].magnetic_role = klipp_timeline::TrackRole::Connected;
                        tracks
                    },
                    in_point: None,
                    out_point: None,
                    markers: Vec::new(),
                    last_playhead: None,
                    brief: None,
                    edit_plan: None,
                    reviews: Vec::new(),
                    awaiting_refit: false,
                    version_of: None,
                };
                self.state.sequences.push(seq);
                self.state.dirty = true;
                true
            }
            AppCommand::DuplicateSequence { sequence_idx, new_name } => {
                if *sequence_idx < self.state.sequences.len() {
                    let mut dup = self.state.sequences[*sequence_idx].clone();
                    dup.id = klipp_core::SequenceId::new();
                    dup.name = new_name.clone();
                    // A copy of a version still to be reframed waits too.
                    self.state.pending_refits.extend(super::aspect_version::waiting_refits(std::slice::from_ref(&dup), &self.state.media_pool));
                    self.state.sequences.push(dup);
                    self.state.dirty = true;
                }
                true
            }
            AppCommand::NewAspectVersion { sequence_idx, width, height, name } => {
                if let Some(index) = super::aspect_version::add_version(&mut self.state, *sequence_idx, (*width, *height), name) {
                    let id = self.state.sequences[index].id;
                    let waiting = super::aspect_version::request_framing(&self.state.sequences[index], &self.state.media_pool);
                    if !waiting.is_empty() {
                        self.state.toast.show(format!("{name}: finding the subjects in {} files; it is reframed when that is done", waiting.len()));
                    } else if !klipp_analysis::subject_detection_enabled() {
                        self.state.toast.show(format!("{name}: subject detection is off in Preferences, so its pictures are centred for review"));
                    }
                    self.state.pending_refits.push(crate::state::PendingRefit { sequence: id, waiting });
                    self.send_commands(vec![AppCommand::SetActiveSequence { sequence_idx: index }]);
                }
                true
            }
            AppCommand::RenameSequence { sequence_idx, new_name } => {
                // usize::MAX names the active sequence: the control socket
                // does not know the index, only that it is the one open.
                let idx = if *sequence_idx == usize::MAX { self.state.active_sequence_idx } else { *sequence_idx };
                if idx < self.state.sequences.len() {
                    self.state.sequences[idx].name = new_name.clone();
                    self.state.dirty = true;
                    if idx == self.state.active_sequence_idx {
                        self.sync_sequence_to_engine();
                    }
                }
                true
            }
            AppCommand::DeleteSequence { sequence_id } => {
                let Some(idx) = self.state.sequences.iter().position(|s| s.id == *sequence_id) else { return true };
                let sequence_idx = &idx;
                if self.state.sequences.len() > 1 {
                    // Refuse to delete a sequence that is used as a compound
                    // clip elsewhere — otherwise the compound clip's
                    // ClipKind::Sequence { sequence_id, .. } reference would
                    // dangle and double-click to re-open would silently fail.
                    let target_id = self.state.sequences[*sequence_idx].id;
                    let referenced_by: Option<String> = self
                        .state
                        .sequences
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| *i != *sequence_idx)
                        .find_map(|(_, seq)| {
                            let used = seq.tracks.iter().any(|t| {
                                t.clips.iter().any(|c| {
                                        matches!(c.kind, klipp_timeline::ClipKind::Sequence {
                                            sequence_id, ..
                                    } if sequence_id == target_id)
                                })
                            });
                            if used { Some(seq.name.clone()) } else { None }
                        });
                    if let Some(parent_name) = referenced_by {
                        self.state.toast.show(format!(
                            "Cannot delete: sequence is used as a compound clip in \"{parent_name}\". Switch to that sequence and remove the compound clip first."
                        ));
                        return true;
                    }
                    let removed_id = self.state.sequences[*sequence_idx].id;
                    self.state.sequences.remove(*sequence_idx);
                    // Drop the deleted sequence's undo/redo stack — it can
                    // never be replayed once its clips and tracks are
                    // gone.
                    self.state.command_histories.remove(&removed_id);
                    self.state.closed_sequence_tabs.remove(&removed_id);
                    // Drop any compound_clip_stack entries pointing at the
                    // removed sequence and renumber indices that shifted.
                    self.state.compound_clip_stack.retain(|(idx, _)| *idx != *sequence_idx);
                    for entry in self.state.compound_clip_stack.iter_mut() {
                        if entry.0 > *sequence_idx {
                            entry.0 -= 1;
                        }
                    }
                    // Adjust active index
                    if self.state.active_sequence_idx >= self.state.sequences.len() {
                        self.state.active_sequence_idx = self.state.sequences.len() - 1;
                    } else if self.state.active_sequence_idx > *sequence_idx {
                        self.state.active_sequence_idx -= 1;
                    }
                    self.state.fps = self.state.sequences[self.state.active_sequence_idx].fps;
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                }
                true
            }
            AppCommand::RegenerateAudioCacheForAll => {
                // Walk the media pool, clear stale audio_cache references, and
                // re-trigger conform for every item with audio. Used when the
                // user wipes on-disk caches via the prefs Clear buttons —
                // without this, MediaItems still reference deleted PCM files
                // and timeline playback either falls back to the slower
                // ffmpeg decode path or produces silence depending on the
                // engine's state.
                let to_regen: Vec<(klipp_core::MediaId, std::path::PathBuf)> = self
                    .state
                    .media_pool
                    .all()
                    .filter(|m| m.has_audio && !m.is_still)
                    .map(|m| (m.id, m.path.clone()))
                    .collect();
                for id in to_regen.iter().map(|(id, _)| *id) {
                    if let Some(item) = self.state.media_pool.get_mut(&id) {
                        item.audio_cache = None;
                    }
                }
                let count = to_regen.len();
                for (id, path) in to_regen {
                    self.spawn_audio_conform(id, path);
                }
                if count > 0 {
                    self.state.toast.show(format!("Regenerating audio cache for {count} media item(s)…"));
                }
                true
            }
            AppCommand::RegenerateFilmstripsForAll => {
                // Wipe in-memory filmstrip textures + in-progress map and
                // re-spawn filmstrip generation for every pool item.
                let to_regen: Vec<(klipp_core::MediaId, std::path::PathBuf, klipp_core::RationalTime)> = self
                    .state
                    .media_pool
                    .all()
                    .filter(|m| !m.is_still)
                    .map(|m| (m.id, m.path.clone(), m.duration))
                    .collect();
                let mut dropped_textures: Vec<egui::TextureHandle> = Vec::new();
                for id in to_regen.iter().map(|(id, _, _)| *id) {
                    if let Some(strip) = self.state.filmstrip_textures.remove(&id) {
                        dropped_textures.extend(strip.into_iter().map(|(_, t)| t));
                    }
                    self.state.filmstrip_in_progress.remove(&id);
                    self.state.thumbnail_cache.set_filmstrip(id, Vec::new());
                }
                self.defer_drop_textures(dropped_textures);
                let count = to_regen.len();
                if count > 0 {
                    self.state.filmstrip_generating = true;
                    self.state.filmstrip_generate_start = std::time::Instant::now();
                }
                for (id, path, duration) in to_regen {
                    self.spawn_filmstrip(id, path, duration);
                }
                if count > 0 {
                    self.state.toast.show(format!("Regenerating filmstrips for {count} media item(s)…"));
                }
                true
            }
            AppCommand::RegenerateSkimTranscodesForAll => {
                // Clear stale skim_file references and queue a fresh skim
                // transcode for each media item. The pump in update() picks
                // these up the same way it does on first import.
                let mut count = 0usize;
                let ids: Vec<klipp_core::MediaId> = self
                    .state
                    .media_pool
                    .all()
                    .filter(|m| !m.is_still)
                    .map(|m| m.id)
                    .collect();
                for id in &ids {
                    if let Some(item) = self.state.media_pool.get_mut(id) {
                        item.skim_file = None;
                    }
                    self.state.pending_skim_transcodes.push(*id);
                    count += 1;
                }
                if count > 0 {
                    self.state.toast.show(format!("Regenerating skim transcodes for {count} media item(s)…"));
                }
                true
            }
            AppCommand::OpenSequenceSettings { sequence_idx } => {
                if let Some(seq) = self.state.sequences.get(*sequence_idx) {
                    self.sequence_settings_dialog = Some(
                        crate::panels::sequence_settings_dialog::SequenceSettingsDialog::open(
                            *sequence_idx,
                            seq.resolution.width,
                            seq.resolution.height,
                            seq.fps.num,
                            seq.fps.den,
                            (seq.camera.shutter_deg, seq.camera.shutter_samples),
                            seq.title_nits,
                        ),
                    );
                }
                true
            }
            AppCommand::SetSequenceResolution { sequence_idx, width, height } => {
                if *sequence_idx < self.state.sequences.len() && *width > 0 && *height > 0 {
                    let res = klipp_core::Resolution { width: *width, height: *height };
                    let from = self.state.sequences[*sequence_idx].resolution;
                    self.state.sequences[*sequence_idx].resolution = res;
                    // Each reframe stays on its picture in the new frame.
                    klipp_document::ops::keep_reframes(&mut self.state.sequences[*sequence_idx], &self.state.media_pool, from);
                    self.state.sequences[*sequence_idx].first_clip_autoset.resolution = false;
                    self.state.dirty = true;
                }
                true
            }
            AppCommand::SetSequenceFps { sequence_idx, fps_num, fps_den } => {
                if *sequence_idx < self.state.sequences.len() && *fps_num > 0 && *fps_den > 0 {
                    self.state.sequences[*sequence_idx].fps = klipp_core::Fps::new(*fps_num, *fps_den);
                    self.state.sequences[*sequence_idx].first_clip_autoset.fps = false;
                    if *sequence_idx == self.state.active_sequence_idx {
                        // Engine reads `state.fps` for the active sequence.
                        self.state.fps = self.state.sequences[*sequence_idx].fps;
                    }
                    self.state.dirty = true;
                }
                true
            }
            AppCommand::ReorderSequences { from_idx, to_idx } => {
                let len = self.state.sequences.len();
                if *from_idx < len && *to_idx < len && from_idx != to_idx {
                    let seq = self.state.sequences.remove(*from_idx);
                    self.state.sequences.insert(*to_idx, seq);
                    // Adjust active index
                    if self.state.active_sequence_idx == *from_idx {
                        self.state.active_sequence_idx = *to_idx;
                    } else if *from_idx < self.state.active_sequence_idx
                        && *to_idx >= self.state.active_sequence_idx
                    {
                        self.state.active_sequence_idx -= 1;
                    } else if *from_idx > self.state.active_sequence_idx
                        && *to_idx <= self.state.active_sequence_idx
                    {
                        self.state.active_sequence_idx += 1;
                    }
                    self.state.dirty = true;
                }
                true
            }
            AppCommand::CreateWorkspace { path, name } => {
                match klipp_project::workspace::create_workspace(path, name) {
                    Ok(()) => {
                        tracing::info!("Created workspace at {:?}", path);
                        self.state.toast.show(format!("Workspace \"{name}\" created"));
                    }
                    Err(e) => {
                        tracing::error!("Failed to create workspace: {e}");
                    }
                }
                true
            }
            // RefineFilmstrip — handled by handle_browser_commands
            // Dynamic tracks
            AppCommand::SyncAutoCreateTracks(enabled) => {
                self.state.auto_create_tracks = *enabled;
                true
            }
            AppCommand::AddVideoTrack => {
                let name = self.state.sequences[self.state.active_sequence_idx].next_track_name(klipp_timeline::TrackKind::Video);
                self.state.sequences[self.state.active_sequence_idx].add_video_track(name);
                self.sync_sequence_to_engine();
                self.state.dirty = true;
                true
            }
            AppCommand::AddAudioTrack => {
                let name = self.state.sequences[self.state.active_sequence_idx].next_track_name(klipp_timeline::TrackKind::Audio);
                self.state.sequences[self.state.active_sequence_idx].add_audio_track(name);
                self.sync_sequence_to_engine();
                self.state.dirty = true;
                true
            }
            AppCommand::AddSubtitleTrack { name } => {
                let n = name.clone();
                self.state.sequences[self.state.active_sequence_idx].add_subtitle_track(n);
                self.sync_sequence_to_engine();
                self.state.dirty = true;
                true
            }
            AppCommand::AddTrackAbove { track_index } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                let ti = *track_index;
                if let Some(kind) = seq.tracks.get(ti).map(|t| t.kind) {
                    let name = seq.next_track_name(kind);
                    let track = klipp_timeline::Track::new(&name, kind);
                    seq.tracks.insert(ti, track);
                    seq.renumber_tracks();
                }
                self.sync_sequence_to_engine();
                self.state.dirty = true;
                true
            }
            AppCommand::AddTrackBelow { track_index } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                let ti = *track_index;
                if let Some(kind) = seq.tracks.get(ti).map(|t| t.kind) {
                    let name = seq.next_track_name(kind);
                    let track = klipp_timeline::Track::new(&name, kind);
                    seq.tracks.insert((ti + 1).min(seq.tracks.len()), track);
                    seq.renumber_tracks();
                }
                self.sync_sequence_to_engine();
                self.state.dirty = true;
                true
            }
            AppCommand::RemoveTrack { track_id } => {
                let tid = *track_id;
                let has_clips = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                    .find(|t| t.id == tid)
                    .map(|t| !t.clips.is_empty())
                    .unwrap_or(false);
                if has_clips {
                    // Show confirmation dialog
                    self.state.confirm_remove_track = Some(tid);
                } else {
                    let _ = self.state.sequences[self.state.active_sequence_idx].remove_track(tid);
                    self.state.sequences[self.state.active_sequence_idx].renumber_tracks();
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                }
                true
            }
            AppCommand::SetTrackLanguage { track_id, language } => {
                if let Some(t) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut().find(|t| t.id == *track_id) {
                    t.language = language.clone();
                    self.state.dirty = true;
                }
                true
            }
            AppCommand::SetTrackAutomationKeys { track_index, param, keys } => {
                if let Some(t) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                    t.automation.lanes.insert(*param, keys.clone());
                    self.state.dirty = true;
                }
                true
            }
            AppCommand::SetTrackAutomationMode { track_index, mode } => {
                if let Some(t) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                    t.automation.mode = *mode;
                    self.state.dirty = true;
                }
                true
            }
            AppCommand::SetTrackSyncLock { track_id, on } => {
                if let Some(t) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut().find(|t| t.id == *track_id) {
                    t.sync_lock = *on;
                    self.state.dirty = true;
                }
                true
            }
            AppCommand::SetTrackTargeted { track_id, on } => {
                if let Some(t) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut().find(|t| t.id == *track_id) {
                    t.targeted = *on;
                    self.state.dirty = true;
                }
                true
            }
            AppCommand::SetTrackSourcePatch { track_id, on } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                if let Some(i) = seq.tracks.iter().position(|t| t.id == *track_id) {
                    if *on {
                        seq.patch_source_to(i);
                    } else {
                        seq.tracks[i].source_patch = false;
                    }
                    self.state.dirty = true;
                }
                true
            }
            AppCommand::RenameTrack { track_id, ref name } => {
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut().find(|t| t.id == *track_id) {
                    track.name = name.clone();
                }
                self.state.dirty = true;
                true
            }
            AppCommand::ResetWaveformZoom { track_id } => {
                // Remove the entry entirely so the default (1.0) applies.
                self.state.waveform_zoom.remove(track_id);
                true
            }
            AppCommand::SetAudioReference { clip_id } => {
                self.state.audio_reference_clip_id = Some(*clip_id);
                self.state.toast.show("Audio reference set.".to_string());
                true
            }
            AppCommand::AlignToAudioReference { clip_id } => {
                self.spawn_audio_sync(*clip_id);
                true
            }
            // Auto Align Audio (#615).
            AppCommand::OpenAutoAlignAudio { reference, recording } => {
                let pair = crate::auto_align::AlignPair { reference: *reference, recording: *recording };
                self.state.auto_align = Some((pair, crate::auto_align::AlignMethod::Waveform));
                true
            }
            AppCommand::AutoAlignAudio { reference, recording, by_timecode } => {
                let pair = crate::auto_align::AlignPair { reference: *reference, recording: *recording };
                if *by_timecode {
                    match crate::auto_align::timecode_position(self.state.sequence(), &self.state.media_pool, pair) {
                        Ok(new_timeline_in) => {
                            self.send_commands(vec![AppCommand::MoveClip { clip_id: *recording, new_timeline_in }]);
                            self.state.toast.show(format!("Aligned by timecode: the recording starts at {:.2} s", new_timeline_in.to_secs_f64()));
                        }
                        Err(why) => self.state.toast.show(format!("Not aligned: {why}")),
                    }
                } else if let Some(why) = crate::auto_align::waveform_unavailable(self.state.sequence(), &self.state.media_pool, pair) {
                    self.state.toast.show(format!("Not aligned: {why}"));
                } else {
                    self.spawn_audio_sync_between(*reference, *recording, true);
                }
                true
            }
            AppCommand::SetAudioReferenceByMedia { media_id } => {
                // Resolve to the first timeline clip using this media.
                // The by-media variants (F-160) let the bin context menu
                // offer the same audio-sync commands without the user
                // having to hunt for a specific clip on the timeline.
                let mid = *media_id;
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let found = seq.tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.media_id() == Some(mid))
                    .map(|c| c.id);
                match found {
                    Some(clip_id) => {
                        self.state.audio_reference_clip_id = Some(clip_id);
                        self.state.toast.show("Audio reference set.".to_string());
                    }
                    None => {
                        self.state.toast.show(
                            "No timeline clip uses this media — add it to the timeline first.".to_string()
                        );
                    }
                }
                true
            }
            AppCommand::AlignToAudioReferenceByMedia { media_id } => {
                let mid = *media_id;
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let found = seq.tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.media_id() == Some(mid))
                    .map(|c| c.id);
                match found {
                    Some(clip_id) => self.spawn_audio_sync(clip_id),
                    None => self.state.toast.show(
                        "No timeline clip uses this media — add it to the timeline first.".to_string()
                    ),
                }
                true
            }
            // Markers — handled by handle_marker_commands
            // Linked clips
            AppCommand::UnlinkClip { clip_id } => {
                let cid = *clip_id;
                // Find the linked partner
                let partner_id = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == cid)
                    .and_then(|c| c.linked_clip_id);
                // Clear linked_clip_id on both
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(c) = track.find_clip_mut(cid) {
                        c.linked_clip_id = None;
                    }
                    if let Some(pid) = partner_id {
                        if let Some(c) = track.find_clip_mut(pid) {
                            c.linked_clip_id = None;
                        }
                    }
                }
                self.state.dirty = true;
                true
            }
            AppCommand::LinkClips { clip_a, clip_b } => {
                let a = *clip_a;
                let b = *clip_b;
                // Set bidirectional link
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(c) = track.find_clip_mut(a) {
                        c.linked_clip_id = Some(b);
                    }
                    if let Some(c) = track.find_clip_mut(b) {
                        c.linked_clip_id = Some(a);
                    }
                }
                self.state.dirty = true;
                true
            }
            // Track locking
            AppCommand::SetTrackLocked { track_id, locked } => {
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut().find(|t| t.id == *track_id) {
                    track.locked = *locked;
                }
                self.state.dirty = true;
                true
            }
            // A track's colour (#922), shown on its header.
            AppCommand::SetTrackColourLabel { track_index, label } => {
                if let Some(track) = self.state.sequence_mut().tracks.get_mut(*track_index) {
                    track.colour_label = *label;
                    self.state.dirty = true;
                }
                true
            }
            // A track moved up or down among tracks of its kind (#922):
            // its order is the composite's, so the engine is told.
            AppCommand::MoveTrack { from, to } => {
                let seq = self.state.sequence_mut();
                if move_track(&mut seq.tracks, *from, *to) {
                    seq.renumber_tracks();
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                }
                true
            }
            AppCommand::SetTrackChannelWaveforms { track_index, on } => {
                if let Some(track) = self.state.sequence_mut().tracks.get_mut(*track_index) {
                    track.channel_waveforms = *on;
                    self.state.dirty = true;
                }
                true
            }
            AppCommand::SplitToMono { clip_id } => {
                match split_to_mono(self.state.sequence_mut(), *clip_id) {
                    Ok(()) => {
                        self.state.dirty = true;
                        self.sync_sequence_to_engine();
                        self.state.toast.show("Split to mono: the left channel stays, the right is on the track below");
                    }
                    Err(why) => self.state.toast.show(why),
                }
                true
            }
            AppCommand::SetTrackCustomHeight { track_id, height } => {
                self.state.timeline_track_scale = 1.0;
                let cap = crate::panels::timeline::track_height_cap(&self.state.timeline_prefs);
                let seq = self.state.sequence_mut();
                if let Some(track) = seq.tracks.iter_mut().find(|t| t.id == *track_id) {
                    track.custom_height = height.map(|h| h.clamp(32.0, cap));
                }
                self.state.dirty = true;
                true
            }
            AppCommand::SetTrackAutomationLaneHeight { track_id, height } => {
                self.state.timeline_track_scale = 1.0;
                let seq = self.state.sequence_mut();
                if let Some(track) = seq.tracks.iter_mut().find(|t| t.id == *track_id) {
                    track.automation_lane_height = height.map(|h| {
                        h.clamp(
                            crate::panels::timeline::MIN_AUTOMATION_LANE_HEIGHT,
                            crate::panels::timeline::MAX_AUTOMATION_LANE_HEIGHT,
                        )
                    });
                }
                self.state.dirty = true;
                true
            }
            AppCommand::GroupClips { clip_ids } => {
                let gid = GroupId::new();
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    for clip in &mut track.clips {
                        if clip_ids.contains(&clip.id) {
                            clip.group_id = Some(gid);
                        }
                    }
                }
                self.state.dirty = true;
                true
            }
            AppCommand::UngroupClips { group_id } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    for clip in &mut track.clips {
                        if clip.group_id == Some(*group_id) {
                            clip.group_id = None;
                        }
                    }
                }
                self.state.dirty = true;
                true
            }
            AppCommand::UngroupClip { clip_id } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    for clip in &mut track.clips {
                        if clip.id == *clip_id {
                            clip.group_id = None;
                        }
                    }
                }
                self.state.dirty = true;
                true
            }
            _ => false,
        }
    }
}

/// Move the track at `from` to `to` among `tracks`, when both are tracks
/// of the same kind and differ (#922). Returns whether it moved.
pub(super) fn move_track(tracks: &mut Vec<klipp_timeline::Track>, from: usize, to: usize) -> bool {
    if from == to || from >= tracks.len() || to >= tracks.len() || tracks[from].kind != tracks[to].kind {
        return false;
    }
    let track = tracks.remove(from);
    tracks.insert(to, track);
    true
}

/// Split a stereo clip on an audio track into its two channels (#922):
/// the clip plays its left channel, and a copy of it playing the right one
/// goes on the first audio track below with room for it, or on a new one.
pub(super) fn split_to_mono(seq: &mut klipp_timeline::Sequence, clip_id: klipp_core::ClipId) -> Result<(), String> {
    use klipp_timeline::{ChannelConfig, TrackKind};
    let (ti, ci) = seq.find_clip(clip_id).ok_or("the clip is not on the timeline")?;
    if seq.tracks[ti].kind != TrackKind::Audio {
        return Err("Split to Mono is for a clip on an audio track".into());
    }
    if seq.tracks[ti].locked {
        return Err(format!("{} is locked", seq.tracks[ti].name));
    }
    let left = &mut seq.tracks[ti].clips[ci];
    left.channel_config = ChannelConfig::LeftOnly;
    let mut right = left.clone();
    right.id = klipp_core::ClipId::new();
    right.channel_config = ChannelConfig::RightOnly;
    right.linked_clip_id = None;
    let (start, end) = (right.timeline_in, right.timeline_out);
    let free = |t: &klipp_timeline::Track| t.kind == TrackKind::Audio && !t.locked && t.clips.iter().all(|c| c.timeline_out <= start || c.timeline_in >= end);
    let below = (ti + 1..seq.tracks.len()).find(|&i| free(&seq.tracks[i]));
    let target = match below {
        Some(i) => i,
        None => {
            let name = seq.next_track_name(TrackKind::Audio);
            seq.tracks.insert(ti + 1, klipp_timeline::Track::new(&name, TrackKind::Audio));
            ti + 1
        }
    };
    let track = &mut seq.tracks[target];
    let at = track.clips.partition_point(|c| c.timeline_in < start);
    track.clips.insert(at, right);
    seq.renumber_tracks();
    Ok(())
}

#[cfg(test)]
mod track_tests {
    use super::{move_track, split_to_mono};
    use klipp_timeline::{ChannelConfig, Clip, Sequence, Track, TrackKind};

    fn t(secs: i64) -> klipp_core::Timecode {
        klipp_core::Timecode { value: secs * 25, rate: 25 }
    }

    fn seq_with_audio(clips: usize) -> (Sequence, Vec<klipp_core::ClipId>) {
        let mut seq = Sequence::new_default();
        seq.tracks.clear();
        seq.tracks.push(Track::new("Video 1", TrackKind::Video));
        let mut a = Track::new("Audio 1", TrackKind::Audio);
        let mut ids = Vec::new();
        for i in 0..clips {
            let c = Clip::media(klipp_core::MediaId::new(), t(i as i64 * 5), t(i as i64 * 5 + 4), t(0), t(4));
            ids.push(c.id);
            a.clips.push(c);
        }
        seq.tracks.push(a);
        (seq, ids)
    }

    #[test]
    fn a_track_moves_among_its_kind_only() {
        let (mut seq, _) = seq_with_audio(0);
        seq.tracks.push(Track::new("Audio 2", TrackKind::Audio));
        let a2 = seq.tracks[2].id;
        assert!(move_track(&mut seq.tracks, 2, 1));
        assert_eq!(seq.tracks[1].id, a2, "Audio 2 is now above Audio 1");
        assert!(!move_track(&mut seq.tracks, 0, 1), "a video track does not go among the audio tracks");
        assert!(!move_track(&mut seq.tracks, 1, 1));
        assert!(!move_track(&mut seq.tracks, 1, 9));
    }

    #[test]
    fn split_to_mono_keeps_the_left_and_puts_the_right_below() {
        let (mut seq, ids) = seq_with_audio(1);
        split_to_mono(&mut seq, ids[0]).unwrap();
        assert_eq!(seq.tracks.len(), 3, "a new audio track for the right channel");
        let left = &seq.tracks[1].clips[0];
        let right = &seq.tracks[2].clips[0];
        assert_eq!((left.id, left.channel_config), (ids[0], ChannelConfig::LeftOnly));
        assert_eq!(right.channel_config, ChannelConfig::RightOnly);
        assert_ne!(right.id, left.id);
        assert_eq!((right.timeline_in, right.timeline_out), (left.timeline_in, left.timeline_out), "at the same time");
    }

    #[test]
    fn split_to_mono_uses_room_on_a_track_below() {
        let (mut seq, ids) = seq_with_audio(1);
        let mut below = Track::new("Audio 2", TrackKind::Audio);
        below.clips.push(Clip::media(klipp_core::MediaId::new(), t(10), t(12), t(0), t(2)));
        seq.tracks.push(below);
        split_to_mono(&mut seq, ids[0]).unwrap();
        assert_eq!(seq.tracks.len(), 3, "no new track: Audio 2 is free at 0..4");
        assert_eq!(seq.tracks[2].clips.len(), 2);
        assert_eq!(seq.tracks[2].clips[0].channel_config, ChannelConfig::RightOnly, "in time order, before the clip at 10 s");
    }

    #[test]
    fn split_to_mono_refuses_a_picture_clip() {
        let (mut seq, _) = seq_with_audio(0);
        let pic = Clip::media(klipp_core::MediaId::new(), t(0), t(2), t(0), t(2));
        let id = pic.id;
        seq.tracks[0].clips.push(pic);
        assert!(split_to_mono(&mut seq, id).is_err());
    }
}

