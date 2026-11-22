//! The Veylvik page: a clip's composition in Veylvik's own editor, drawn
//! in Klipp's window on Klipp's GPU device. The clip is the selected one,
//! else the one under the playhead on the top picture track; one without
//! a composition gets one made of its media the first time it opens here.
//! Every change in the editor goes into the project as it is made, and the
//! clip shows it in Edit from the next frame.

use std::sync::Arc;

use klipp_core::ClipId;
use klipp_timeline::CompositionId;
use klipp_veylvik::editor_footage::{EditorFootage, TabHost};
use klipp_veylvik::veylvik_editor as editor;

/// The page's editor and what it has open. The editor is made the first
/// time the page is shown, never for a session that doesn't.
pub(crate) struct VeylvikTab {
    render_state: Option<eframe::egui_wgpu::RenderState>,
    editor: Option<editor::Editor>,
    host: Arc<TabHost>,
    open: Option<Open>,
    /// Why the editor could not start, said on the page.
    failed: Option<String>,
    /// The page has just come into view: the keyboard goes to the editor.
    just_shown: bool,
    /// The page has just come into view, or opened another clip: the
    /// editor goes to the frame under Klipp's playhead.
    place: bool,
    /// The composition time the editor is to go to, until it can (not
    /// mid-gesture).
    going_to: Option<klipp_veylvik::Time>,
    /// Where the artist took the editor's playhead, as Klipp's playhead:
    /// Klipp's goes there when the page is left.
    carried: Option<klipp_core::Timecode>,
    /// Klipp's Veylvik settings as last given to the editor: given again
    /// when Klipp's differ (Preferences → Veylvik changed them).
    applied: Option<serde_json::Map<String, serde_json::Value>>,
    /// The editor's shortcut window opens on the next frame it is drawn.
    shortcuts: bool,
    /// What Klipp's viewer was showing with when the editor's viewer was
    /// last given it: given again when the project's working space, the
    /// monitor's transfer or the viewer's LUT change.
    view: Option<ViewKey>,
    /// The sounds of the timeline's compositions, for Edit and the export.
    sounds: Option<klipp_veylvik::composition_sound::Sounds>,
    /// What they were last worked out from, and when.
    sounds_from: Option<(klipp_timeline::SequenceRevision, usize, usize)>,
    sounds_checked: Option<std::time::Instant>,
}

/// What makes Klipp's view of a picture.
#[derive(Clone, PartialEq)]
struct ViewKey {
    working: klipp_core::WorkingColorspace,
    transfer: klipp_core::SdrTransfer,
    luts: (Option<std::path::PathBuf>, Option<std::path::PathBuf>),
    lut_loaded: bool,
}

/// The clip open in the editor.
struct Open {
    clip: ClipId,
    composition: CompositionId,
    /// The editor's revision last written into the project.
    revision: u64,
}

impl VeylvikTab {
    pub(crate) fn new(render_state: Option<eframe::egui_wgpu::RenderState>) -> Self {
        Self { render_state, editor: None, host: Arc::new(TabHost::default()), open: None, failed: None, just_shown: true, place: true, going_to: None, carried: None, applied: None, shortcuts: false, view: None, sounds: None, sounds_from: None, sounds_checked: None }
    }

    /// Whether the editor has the keyboard: the page shows and it was
    /// the last thing pressed on.
    pub(crate) fn has_keyboard(&self) -> bool {
        self.editor.as_ref().is_some_and(|e| e.has_keyboard())
    }

    /// The page has come into view.
    pub(crate) fn shown(&mut self) {
        self.just_shown = true;
        self.place = true;
        self.carried = None;
    }

    /// Open the editor's own shortcut window when the page next shows.
    pub(crate) fn open_shortcuts(&mut self) {
        self.shortcuts = true;
    }

    /// Where Klipp's playhead goes as the page is left: the frame of the
    /// clip the artist took the editor's to, if they did.
    pub(crate) fn take_carried(&mut self) -> Option<klipp_core::Timecode> {
        self.carried.take()
    }

    /// Open the clip again on the next frame: its composition's folder
    /// moved (a save somewhere else), or the project changed under it.
    pub(crate) fn reopen(&mut self) {
        self.open = None;
    }

    /// Let the editor finish its work and the GPU go, at quit.
    pub(crate) fn close(&mut self) {
        if let Some(editor) = self.editor.as_mut() {
            editor.close();
        }
    }
}

/// What the page has to show: a clip and its media, or why not.
enum Target {
    Clip(ClipId),
    Nothing(&'static str),
}

fn target(state: &crate::state::AppState) -> Target {
    let seq = state.sequence();
    let has_media = |id: ClipId| seq.find_clip(id).and_then(|(t, c)| seq.tracks[t].clips[c].media_id()).is_some();
    if let Some(id) = state.selected_clip {
        return if has_media(id) { Target::Clip(id) } else { Target::Nothing("The selected clip has no media to compose with. Select a clip of a video or a still.") };
    }
    let under = seq.tracks.iter().rev().filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible).find_map(|t| t.clip_at_timecode(state.playhead).map(|(_, c)| c.id).filter(|id| has_media(*id)));
    match under {
        Some(id) => Target::Clip(id),
        None => Target::Nothing("Select a clip on the timeline, or move the playhead over one, to open it in Veylvik."),
    }
}

impl super::KlippApp {
    /// The Veylvik page in `area`, the whole window under the toolbar.
    pub(super) fn render_veylvik_page(&mut self, ui: &mut egui::Ui, area: egui::Rect) {
        let theme = self.theme.clone();
        crate::ui_util::panel_ui(ui, area, "veylvik_page", |ui| {
            ui.set_min_size(area.size());
            ui.painter().rect_filled(area, 0.0, theme.bg_panel);
            let say = |ui: &mut egui::Ui, text: &str| {
                ui.centered_and_justified(|ui| ui.label(egui::RichText::new(text).color(theme.text_secondary)));
            };
            let clip = match target(&self.state) {
                Target::Clip(id) => id,
                Target::Nothing(why) => return say(ui, why),
            };
            let had = self.veylvik_tab.open.as_ref().map(|o| o.clip);
            if let Err(why) = self.veylvik_open(ui.ctx(), clip) {
                return say(ui, &why);
            }
            if had != Some(clip) || std::mem::take(&mut self.veylvik_tab.place) {
                self.veylvik_place(clip);
                // The clip's sound as Edit has it now: on each visit, so
                // a change made on Edit is heard here.
                self.veylvik_tab.host.set_sound(self.veylvik_sound(clip));
            }
            if let (Some(time), Some(editor)) = (self.veylvik_tab.going_to, self.veylvik_tab.editor.as_mut()) {
                // Not while an inspector drag is under way: next frame.
                if editor.set_time(time) {
                    self.veylvik_tab.going_to = None;
                }
            }
            self.veylvik_settings_in(ui.ctx());
            self.veylvik_view_in();
            let mut relink_asked = false;
            if let Some(editor) = self.veylvik_tab.editor.as_mut() {
                if std::mem::take(&mut self.veylvik_tab.shortcuts) {
                    editor.open_shortcuts();
                }
                // The keys are the editor's when the page opens: space plays
                // its composition, not Klipp's timeline.
                if std::mem::take(&mut self.veylvik_tab.just_shown) {
                    editor.set_keyboard(true);
                }
                // What was picked on Klipp's bar, then the editor's menus as
                // they are now, for the bar's next frame: Klipp's one menu
                // bar shows them, the editor draws none of its own.
                if let Some(id) = self.state.veylvik_menu_pick.take() {
                    editor.run_menu_item(ui.ctx(), &id);
                }
                self.state.veylvik_menus = editor.menus(ui.ctx());
                let missing = editor.unavailable_media().len();
                relink_asked = veylvik_header(ui, editor, missing, &theme);
                editor.ui(ui);
            }
            if relink_asked || self.veylvik_tab.editor.as_mut().is_some_and(|e| e.take_relink_request()) {
                self.veylvik_relink();
            }
            self.veylvik_follow(clip);
            self.veylvik_settings_out();
            // The editor's own Preferences… (its File menu, Ctrl+,): Klipp's,
            // on its Veylvik tab.
            if self.veylvik_tab.editor.as_mut().is_some_and(|e| e.take_preferences_request()) {
                self.preferences_panel.open_page("veylvik");
                self.state.show_preferences = true;
            }
            self.veylvik_take_edits();
        });
        self.state.veylvik_keys = self.veylvik_tab.has_keyboard();
    }

    /// Have `clip` open in the editor, its composition made first if it has
    /// none, and the editor made first if there is none yet.
    fn veylvik_open(&mut self, ctx: &egui::Context, clip: ClipId) -> Result<(), String> {
        if let Some(why) = &self.veylvik_tab.failed {
            return Err(why.clone());
        }
        if self.veylvik_tab.open.as_ref().is_some_and(|o| o.clip == clip && self.state.compositions.contains_key(&o.composition)) {
            return Ok(());
        }
        let (composition, id) = self.veylvik_composition_for(clip)?;
        let assets = self.composition_assets_dir(id);
        let _ = std::fs::create_dir_all(&assets);
        self.veylvik_tab.host.set(self.veylvik_footage(clip));
        let made = match self.veylvik_tab.editor.as_mut() {
            Some(editor) => editor.set_composition(ctx, composition, assets).map_err(|e| e.to_string()),
            None => self.veylvik_make_editor(ctx, composition, assets),
        };
        if let Err(why) = made {
            return Err(format!("Veylvik could not open this clip's composition: {why}"));
        }
        let name = clip_name(&self.state, clip);
        if let Some(editor) = self.veylvik_tab.editor.as_mut() {
            // Its title, and the start of the files it renders: the clip's
            // name, not "Untitled" (a hosted composition has no file).
            editor.set_name(name);
        }
        let revision = self.veylvik_tab.editor.as_ref().map_or(0, |e| e.revision());
        self.veylvik_tab.open = Some(Open { clip, composition: id, revision });
        Ok(())
    }

    /// Klipp's viewer for the editor's: its picture shown as Klipp's viewer
    /// shows a frame, given again when what makes that view changes.
    fn veylvik_view_in(&mut self) {
        let key = ViewKey {
            working: klipp_engine::video_decoder::working_colorspace(),
            transfer: self.prefs.color.sdr_display_transfer,
            luts: (self.state.project_viewing_lut.clone(), self.prefs.color.viewer_lut_path.clone()),
            lut_loaded: self.viewer.lut.lut.is_some(),
        };
        if self.veylvik_tab.view.as_ref() == Some(&key) {
            return;
        }
        let Some(editor) = self.veylvik_tab.editor.as_mut() else { return };
        let lut = self.viewer.lut.lut.clone().map(Arc::new);
        let mut display = klipp_veylvik::display::KlippDisplay::new(key.working, key.transfer, lut);
        // The editor draws on the UI's device; when that is Klipp's shared
        // one, Klipp's GPU encode can take the page's picture there.
        if klipp_effects::gpu_context::GpuContext::shared_is_ui_device() {
            display = display.on_shared_device();
        }
        editor.set_host_display(Some(Arc::new(display)));
        self.veylvik_tab.view = Some(key);
    }

    /// Klipp's Veylvik settings into the editor, when they aren't what it
    /// was last given: at its start, and after Preferences → Veylvik. It
    /// never reads Veylvik Studio's own.
    fn veylvik_settings_in(&mut self, ctx: &egui::Context) {
        let Some(editor) = self.veylvik_tab.editor.as_mut() else { return };
        if self.veylvik_tab.applied.as_ref() == Some(&self.prefs.veylvik) {
            return;
        }
        let values: editor::preferences::Values = self.prefs.veylvik_settings();
        if let Err(why) = editor.set_preferences(ctx, values) {
            tracing::warn!("Veylvik's editor refused Klipp's Veylvik settings, its own kept: {why}");
        }
        self.veylvik_tab.applied = Some(self.prefs.veylvik.clone());
    }

    /// What the artist changed in the editor itself (a layout saved, a
    /// viewer aid, a shortcut), into Klipp's preferences and saved.
    fn veylvik_settings_out(&mut self) {
        let Some(values) = self.veylvik_tab.editor.as_mut().and_then(|e| e.take_preferences_change()) else { return };
        if self.prefs.set_veylvik_settings(&values) {
            self.prefs_dirty = true;
        }
        // The editor has these already: not given back.
        self.veylvik_tab.applied = Some(self.prefs.veylvik.clone());
    }

    /// `clip`'s place in its composition: its used part as the editor's
    /// range, the rest dimmed as handles, and the editor's playhead on the
    /// frame under Klipp's (the clip's nearest when it is elsewhere).
    fn veylvik_place(&mut self, clip: ClipId) {
        let Some((range, time)) = clip_time(&self.state, clip, |t| (t.used_range(), t.composition_time(self.state.playhead))) else { return };
        if let Some(editor) = self.veylvik_tab.editor.as_mut() {
            editor.set_range(Some(range));
        }
        self.veylvik_tab.going_to = Some(time);
    }

    /// Where the artist took the editor's playhead, carried to Klipp's
    /// when it is a frame the clip shows; in the handles, Klipp's stays.
    fn veylvik_follow(&mut self, clip: ClipId) {
        let Some(moved) = self.veylvik_tab.editor.as_mut().and_then(|e| e.take_time_change()) else { return };
        let fps = self.state.sequence().fps;
        if let Some(frame) = clip_time(&self.state, clip, |t| t.timeline_frame_at(moved)).flatten() {
            self.veylvik_tab.carried = Some(playhead_at(frame, fps));
        }
    }

    /// The editor's "unavailable" button: media is Klipp's to relink.
    fn veylvik_relink(&mut self) {
        let clip = self.veylvik_tab.open.as_ref().map(|o| o.clip);
        let missing = self.veylvik_tab.editor.as_ref().map(|e| e.unavailable_media()).unwrap_or_default();
        relink(&mut self.state, clip, &missing);
    }

    fn veylvik_make_editor(&mut self, ctx: &egui::Context, composition: klipp_veylvik::Composition, assets: std::path::PathBuf) -> Result<(), String> {
        let Some(rs) = self.veylvik_tab.render_state.clone() else {
            let why = "Veylvik needs Klipp's GPU device, and this session has none".to_string();
            self.veylvik_tab.failed = Some(why.clone());
            return Err(why);
        };
        let setup = editor::Setup {
            interface: editor::HOST_INTERFACE,
            context: ctx.clone(),
            device: rs.device.clone(),
            queue: rs.queue.clone(),
            adapter: rs.adapter.get_info(),
            target_format: rs.target_format,
            opens: editor::Opens::Composition { composition: Box::new(composition), assets },
            live_control: editor::LiveControl::Off,
            capabilities: editor::Capabilities::embedded(),
            host: Some(Arc::clone(&self.veylvik_tab.host) as Arc<dyn klipp_veylvik::Host>),
            // The compositor's: one budget and set of caches for the tab
            // and every render of Klipp's.
            renderer: klipp_veylvik::Compositor::shared().map(|c| c.renderer()),
        };
        match editor::Editor::new(setup) {
            Ok(mut e) => {
                // The pictures it prepares to play, in Klipp's cache folder:
                // Klipp's clear-cache clears them with the rest.
                e.set_playback_folder(dirs::cache_dir().map(|d| d.join("klipp").join("veylvik-playback")));
                self.veylvik_tab.editor = Some(e);
                // A new editor starts at Veylvik's defaults: Klipp's settings
                // go in before it is first drawn.
                self.veylvik_tab.applied = None;
                self.veylvik_tab.view = None;
                Ok(())
            }
            Err(e) => {
                let why = format!("Veylvik's editor could not start: {e}");
                tracing::warn!("{why}");
                self.veylvik_tab.failed = Some(why.clone());
                Err(why)
            }
        }
    }

    /// `clip`'s composition, read; one made of its media and given to it
    /// when it has none.
    pub(super) fn veylvik_composition_for(&mut self, clip: ClipId) -> Result<(klipp_veylvik::Composition, CompositionId), String> {
        let seq = self.state.sequence();
        let (t, c) = seq.find_clip(clip).ok_or("the clip is no longer on the timeline")?;
        let it = &seq.tracks[t].clips[c];
        if let Some(id) = it.composition {
            if let Some(bytes) = self.state.compositions.get(&id) {
                let composition: klipp_veylvik::Composition = serde_json::from_slice(bytes).map_err(|e| format!("its composition can't be read: {e}"))?;
                return Ok((composition, id));
            }
        }
        let (composition, bytes) = fresh_composition(&self.state, it)?;
        let id = CompositionId::new();
        let old = it.clone();
        let mut new = it.clone();
        new.composition = Some(id);
        // One step: the composition, and the clip naming it.
        let edits = [
            crate::history::EditCommand::SetComposition { id, before: None, after: Some(bytes), label: "Veylvik: Make composition".into() },
            crate::history::EditCommand::ReplaceClip { description: "Veylvik composition", old: Box::new(old), new: Box::new(new) },
        ];
        self.apply_edits_as_one(&edits, "Veylvik: Make Composition");
        Ok((composition, id))
    }

    /// The sounds of the timeline's compositions kept as they are, for
    /// Edit's playback and the export: worked out again when the timeline
    /// or a composition changes, and while one is being made.
    pub(super) fn keep_composition_sounds(&mut self) {
        if self.state.compositions.is_empty() && self.veylvik_tab.sounds.is_none() {
            return;
        }
        let from = (self.sequence.revision(), self.state.compositions.len(), self.state.compositions.values().map(|b| b.as_ptr() as usize).fold(0, usize::wrapping_add));
        let busy = self.veylvik_tab.sounds.as_ref().is_some_and(|s| s.busy());
        let recent = self.veylvik_tab.sounds_checked.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(250));
        if recent || (!busy && self.veylvik_tab.sounds_from == Some(from)) {
            return;
        }
        self.veylvik_tab.sounds_from = Some(from);
        self.veylvik_tab.sounds_checked = Some(std::time::Instant::now());
        // Every composed clip of the sequence Edit plays, at its own speed
        // (the sound plays at 1x, as the page plays the media).
        let wants: Vec<(ClipId, Arc<[u8]>, std::path::PathBuf, f64)> = self
            .state
            .sequence()
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .filter(|c| c.enabled && c.speed == 1.0 && c.speed_keyframes.is_empty() && !c.is_reversed())
            .filter_map(|c| {
                let id = c.composition?;
                let bytes = self.state.compositions.get(&id)?;
                Some((c.id, Arc::clone(bytes), self.composition_assets_dir(id), c.source_out.to_secs_f64()))
            })
            .collect();
        let folder = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("klipp").join("veylvik-sound");
        let sounds = self.veylvik_tab.sounds.get_or_insert_with(|| klipp_veylvik::composition_sound::Sounds::new(folder));
        let given = sounds.update(wants.iter().map(|(clip, bytes, assets, until)| klipp_veylvik::composition_sound::Want { clip: *clip, bytes, assets: assets.clone(), until_secs: *until }));
        if let Some(list) = given {
            // The export reads the WAV beside each: its decoder's format.
            klipp_render::snapshot::set_composition_sounds(list.iter().map(|(c, p)| (*c, p.with_extension("wav"))).collect());
            let _ = self.cmd_tx.send(klipp_core::AppCommand::SetCompositionSounds(list));
        }
    }

    /// Undo (or with `redo`, redo) a step of the composition open on the
    /// Veylvik page, while it is the page in view: false when it has none.
    pub(super) fn veylvik_page_step(&mut self, redo: bool) -> bool {
        if self.state.active_workspace != crate::state::Workspace::Veylvik {
            return false;
        }
        let Some(editor) = self.veylvik_tab.editor.as_mut() else { return false };
        if redo { editor.redo() } else { editor.undo() }
    }

    /// The sound linked to the open clip, as Edit plays it.
    fn veylvik_sound(&self, clip: ClipId) -> Option<klipp_veylvik::clip_sound::ClipSound> {
        klipp_veylvik::clip_sound::ClipSound::new(self.state.sequence(), &self.state.sequences, &self.state.media_pool, clip)
    }

    /// The open clip's media as the editor reads it.
    fn veylvik_footage(&self, clip: ClipId) -> Option<EditorFootage> {
        let seq = self.state.sequence();
        let (t, c) = seq.find_clip(clip)?;
        let item = self.state.media_pool.get(&seq.tracks[t].clips[c].media_id()?)?;
        let ctx = klipp_effects::gpu_context::GpuContext::shared()?;
        Some(EditorFootage::new(item, klipp_engine::video_decoder::working_colorspace(), super::engine_sync::media_state(item), ctx.device(), ctx.queue()))
    }

    /// The editor's changes into the project, each as a step of Klipp's
    /// one list (an edit) or a move over one (its own undo and redo).
    fn veylvik_take_edits(&mut self) {
        let (Some(editor), Some(open)) = (self.veylvik_tab.editor.as_mut(), self.veylvik_tab.open.as_mut()) else { return };
        let changes = editor.take_changes();
        let Some(last) = changes.last() else { return };
        open.revision = editor.revision();
        let bytes: Arc<[u8]> = match serde_json::to_vec(editor.composition()) {
            Ok(b) => b.into(),
            Err(e) => return tracing::warn!("a Veylvik edit could not be written into the project: {e}"),
        };
        // Several in one frame: one step, the last one's kind and name.
        let kind = match last.kind {
            klipp_veylvik::veylvik_document::ChangeKind::Edit => crate::state::CompositionChange::Edit,
            klipp_veylvik::veylvik_document::ChangeKind::Undo => crate::state::CompositionChange::Undo,
            klipp_veylvik::veylvik_document::ChangeKind::Redo => crate::state::CompositionChange::Redo,
        };
        let id = open.composition;
        self.state.take_composition_change(id, kind, &last.label, bytes);
        self.sync_compositions_to_engine();
    }

    /// A change to composition `id` made on Edit, by its settings in the
    /// inspector or over the control socket: taken as a step (one a
    /// drag), shown in the viewer at once, and the page, holding that
    /// composition, opens it again as it is now.
    pub(super) fn take_control_change(&mut self, id: CompositionId, bytes: Arc<[u8]>, label: &str, dragging: bool) {
        self.state.take_control_change(id, bytes, label, dragging);
        if self.veylvik_tab.open.as_ref().is_some_and(|o| o.composition == id) {
            self.veylvik_tab.reopen();
        }
        self.sync_compositions_to_engine();
    }

    /// `action` on `clip`'s composition versions, as one step; what was
    /// done, or why nothing was.
    pub(super) fn composition_version(&mut self, clip: ClipId, action: &klipp_core::VersionAction) -> Result<String, String> {
        use klipp_core::VersionAction as A;
        use klipp_timeline::veylvik::{next_version_name, versions};
        let seq = self.state.sequence();
        let (t, c) = seq.find_clip(clip).ok_or("the clip is no longer on the timeline")?;
        let old = seq.tracks[t].clips[c].clone();
        let mut list = versions(&old);
        let mut new = old.clone();
        let mut made: Option<(CompositionId, Arc<[u8]>)> = None;
        let shown = list.iter().position(|v| Some(v.id) == old.composition);
        let at = |i: usize, list: &Vec<klipp_timeline::CompositionVersion>| list.get(i).cloned().ok_or_else(|| format!("the clip has no version {}", i + 1));
        let (label, said) = match action {
            A::New => {
                let (_, bytes) = fresh_composition(&self.state, &old)?;
                let id = CompositionId::new();
                let name = next_version_name(&old);
                list.push(klipp_timeline::CompositionVersion { name: name.clone(), id });
                new.composition = Some(id);
                made = Some((id, bytes));
                ("Veylvik: New version", format!("Made {name} of the clip's media and showed it"))
            }
            A::Duplicate => {
                let from = old.composition.ok_or("the clip has no composition to copy; make one first")?;
                let bytes = self.state.compositions.get(&from).cloned().ok_or("the project doesn't hold the clip's composition")?;
                let id = CompositionId::new();
                let home = self.compositions_home();
                match klipp_project::compositions::copy_composition_assets(&home, from, id) {
                    Ok(said) => said.into_iter().take(1).for_each(|s| self.state.toast.show(s)),
                    Err(e) => return Err(format!("its files could not be copied: {e}")),
                }
                let name = next_version_name(&old);
                let of = shown.and_then(|i| list.get(i)).map(|v| v.name.clone()).unwrap_or_default();
                list.push(klipp_timeline::CompositionVersion { name: name.clone(), id });
                new.composition = Some(id);
                made = Some((id, bytes));
                ("Veylvik: Duplicate version", format!("Made {name}, a copy of {of}, and showed it"))
            }
            A::Switch(i) => {
                let v = at(*i, &list)?;
                if Some(v.id) == old.composition {
                    return Ok(format!("{} is already shown", v.name));
                }
                if !self.state.compositions.contains_key(&v.id) {
                    return Err(format!("the project doesn't hold {}", v.name));
                }
                new.composition = Some(v.id);
                ("Veylvik: Switch version", format!("Showing {}", v.name))
            }
            A::Rename(i, name) => {
                let name = name.trim();
                if name.is_empty() {
                    return Err("a version needs a name".into());
                }
                let v = at(*i, &list)?;
                if list.iter().any(|o| o.name == name && o.id != v.id) {
                    return Err(format!("another version is named {name}"));
                }
                list[*i].name = name.to_owned();
                ("Veylvik: Rename version", format!("Named {} {name}", v.name))
            }
            A::Delete(i) => {
                let v = at(*i, &list)?;
                if list.len() < 2 {
                    return Err("a clip's last version can't be removed".into());
                }
                list.remove(*i);
                if Some(v.id) == old.composition {
                    new.composition = list.get(i.saturating_sub(1)).map(|o| o.id);
                }
                ("Veylvik: Remove version", format!("Removed {}", v.name))
            }
        };
        // One version under the first name is the clip's one composition.
        new.composition_versions = if list.len() == 1 && list[0].name == klipp_timeline::veylvik::version_name(1) { Vec::new() } else { list };
        let mut edits = Vec::new();
        if let Some((id, bytes)) = made {
            edits.push(crate::history::EditCommand::SetComposition { id, before: None, after: Some(bytes), label: label.into() });
        }
        edits.push(crate::history::EditCommand::ReplaceClip { description: "Veylvik composition version", old: Box::new(old), new: Box::new(new) });
        self.apply_edits_as_one(&edits, label);
        if self.veylvik_tab.open.as_ref().is_some_and(|o| o.clip == clip) {
            self.veylvik_tab.reopen();
        }
        self.sync_compositions_to_engine();
        Ok(said)
    }

    /// Compositions an undo or redo of Klipp's put back: the tab opens
    /// its clip's again if it was one of them.
    pub(super) fn veylvik_compositions_put_back(&mut self, changed: &[(CompositionId, Option<Arc<[u8]>>)]) {
        for (id, bytes) in changed {
            match bytes {
                Some(b) => {
                    self.state.compositions.insert(*id, b.clone());
                }
                None => {
                    self.state.compositions.remove(id);
                }
            }
            if self.veylvik_tab.open.as_ref().is_some_and(|o| o.composition == *id) {
                self.veylvik_tab.reopen();
            }
        }
        self.sync_compositions_to_engine();
    }
}

/// A composition of `clip`'s media alone, at the sequence's size and the
/// media's rate, and its file's bytes: what a clip's first composition,
/// and a new version, start as.
fn fresh_composition(state: &crate::state::AppState, it: &klipp_timeline::Clip) -> Result<(klipp_veylvik::Composition, Arc<[u8]>), String> {
    let seq = state.sequence();
        let media = it.media_id().ok_or("the clip has no media")?;
        let item = state.media_pool.get(&media).ok_or("the clip's media is not in the project")?;
        let fps = item.fps.as_f64();
        let rate = klipp_veylvik::Time { ticks: (fps * 1000.0).round() as i64, rate: 1000 };
        let rate = if (fps - fps.round()).abs() < 1e-6 { klipp_veylvik::Time { ticks: fps.round() as i64, rate: 1 } } else if ((fps * 1001.0 / 1000.0) - (fps * 1001.0 / 1000.0).round()).abs() < 1e-3 { klipp_veylvik::Time { ticks: (fps * 1001.0 / 1000.0).round() as i64 * 1000, rate: 1001 } } else { rate };
        let frames = ((item.duration.to_secs_f64() * fps).floor() as u32).max(1);
        let label = item.filename.clone();
        // The sequence's size, the footage fitted inside it, as Resolve's
        // Fusion compositions are the timeline's: a title or an effect is
        // drawn at the frame's own resolution and may reach past the clip's
        // picture (a portrait clip's sides). Its time stays the media's.
        let size = [seq.resolution.width.max(1), seq.resolution.height.max(1)];
        let composition = klipp_veylvik::Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, &label, None, size, rate, frames).map_err(|e| e.to_string())?;
        let bytes: Arc<[u8]> = serde_json::to_vec(&composition).map_err(|e| e.to_string())?.into();
    Ok((composition, bytes))
}

/// The page's strip above the editor, in place of the bar the editor drew
/// itself: its workspaces (Compose, Animate, 3D) on the left, the clip's
/// name in the middle, and what the composition can't find. Its menus are
/// on Klipp's own bar. True when relinking was asked for.
fn veylvik_header(ui: &mut egui::Ui, editor: &mut editor::Editor, missing: usize, theme: &crate::theme::KlippTheme) -> bool {
    let mut relink = false;
    let height = crate::theme::control::REGULAR + 8.0;
    let (strip, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::hover());
    ui.painter().hline(strip.x_range(), strip.max.y, egui::Stroke::new(1.0_f32, theme.border));
    let font = egui::FontId::proportional(crate::theme::type_scale::CONTROL);
    let workspaces = editor.workspaces();
    let labels: Vec<&str> = workspaces.iter().map(|(_, l)| l.as_str()).collect();
    let current = editor.workspace();
    let active = workspaces.iter().position(|(id, _)| *id == current).unwrap_or(0);
    let size = crate::widgets::segmented::segmented_size(ui, &labels, &font, crate::theme::control::REGULAR);
    let at = egui::Rect::from_min_size(egui::pos2(strip.min.x + 8.0, strip.center().y - size.y / 2.0), size);
    if !labels.is_empty() {
        if let Some(i) = crate::widgets::segmented::segmented_in(ui, at, &labels, active, theme, font.clone()) {
            editor.set_workspace(&workspaces[i].0);
        }
    }
    let title = if editor.unsaved() { format!("{} \u{2022}", editor.title()) } else { editor.title() };
    ui.painter().text(strip.center(), egui::Align2::CENTER_CENTER, title, font.clone(), theme.text_secondary);
    if missing > 0 {
        let text = if missing == 1 { "1 file missing".to_string() } else { format!("{missing} files missing") };
        let width = ui.fonts_mut(|f| f.layout_no_wrap(text.clone(), font.clone(), egui::Color32::WHITE).size().x) + 16.0;
        let rect = egui::Rect::from_min_size(egui::pos2(strip.max.x - width - 8.0, strip.center().y - crate::theme::control::REGULAR / 2.0), egui::vec2(width, crate::theme::control::REGULAR));
        let resp = ui.put(rect, egui::Button::new(egui::RichText::new(text).color(theme.marker_warning)));
        if resp.on_hover_text("Find the files this composition uses that aren't where it says").clicked() {
            relink = true;
        }
    }
    relink
}

/// Klipp's playhead on timeline frame `frame` of a sequence at `fps`: a
/// frame is `den` ticks of a rate of `num` (1001 of 24000 at 23.976).
fn playhead_at(frame: i64, fps: klipp_core::Fps) -> klipp_core::Timecode {
    klipp_core::Timecode::from_frames(frame * i64::from(fps.den), fps.num)
}

/// `f` of `clip`'s place in time: on the sequence's timeline, from its
/// media's frames.
fn clip_time<T>(state: &crate::state::AppState, clip: ClipId, f: impl FnOnce(&klipp_veylvik::clip_time::ClipTime) -> T) -> Option<T> {
    let seq = state.sequence();
    let (t, c) = seq.find_clip(clip)?;
    let it = &seq.tracks[t].clips[c];
    let media_fps = state.media_pool.get(&it.media_id()?)?.fps.as_f64();
    let fps = seq.fps.as_f64();
    (fps > 0.0 && media_fps > 0.0).then(|| f(&klipp_veylvik::clip_time::ClipTime { clip: it, fps, media_fps }))
}

/// `clip` as the timeline names it: its own name, else its media's file
/// name.
fn clip_name(state: &crate::state::AppState, clip: ClipId) -> Option<String> {
    let seq = state.sequence();
    let (t, c) = seq.find_clip(clip)?;
    let it = &seq.tracks[t].clips[c];
    let file = it.media_id().and_then(|m| state.media_pool.get(&m)).map(|m| m.filename.clone()).unwrap_or_default();
    Some(it.label(&file).to_owned()).filter(|n| !n.trim().is_empty())
}

/// The editor's "unavailable" pressed for `clip`, `missing` what it can't
/// have: the clip's media offline, Klipp's Relink dialog; else what can't
/// be had is the composition's own (a file in its folder), said by name.
fn relink(state: &mut crate::state::AppState, clip: Option<ClipId>, missing: &std::collections::BTreeMap<String, String>) {
    let offline = clip.and_then(|clip| {
        let seq = state.sequence();
        let (t, c) = seq.find_clip(clip)?;
        state.media_pool.get(&seq.tracks[t].clips[c].media_id()?).map(|m| m.offline)
    });
    if offline == Some(true) {
        state.show_relink_dialog = true;
        return;
    }
    let said: Vec<String> = missing.iter().take(3).map(|(name, why)| format!("{name}: {why}")).collect();
    let more = missing.len().saturating_sub(said.len());
    state.toast.show(match (said.is_empty(), more) {
        (true, _) => "The clip's media is online: nothing for Klipp to relink".to_owned(),
        (false, 0) => format!("Not found in the composition's folder: {}", said.join("; ")),
        (false, n) => format!("Not found in the composition's folder: {} and {n} more", said.join("; ")),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::{Clip, Sequence, Track, TrackKind};

    fn t(s: f64) -> RationalTime {
        RationalTime::from_secs_f64(s, 24)
    }

    /// The page opens the selected clip; else the one under the playhead
    /// on the top picture track that shows; never a clip without media.
    #[test]
    fn the_page_opens_the_selected_clip_else_the_top_one_under_the_playhead() {
        let (low, high) = (Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0)), Clip::media(MediaId::new(), t(1.0), t(3.0), t(0.0), t(2.0)));
        let gap = Clip::gap(t(4.0), t(6.0));
        let (low_id, high_id, gap_id) = (low.id, high.id, gap.id);
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![low, gap];
        let mut v2 = Track::new("V2", TrackKind::Video);
        v2.clips = vec![high];
        let mut seq = Sequence::new_default();
        seq.tracks = vec![v1, v2];
        let mut s = crate::state::AppState { sequences: vec![seq], ..Default::default() };
        let opened = |s: &crate::state::AppState| match target(s) {
            Target::Clip(id) => Some(id),
            Target::Nothing(_) => None,
        };
        s.playhead = t(2.0);
        assert_eq!(opened(&s), Some(high_id), "the top one under the playhead");
        s.playhead = t(0.5);
        assert_eq!(opened(&s), Some(low_id));
        s.sequences[0].tracks[1].visible = false;
        s.playhead = t(2.0);
        assert_eq!(opened(&s), Some(low_id), "a hidden track's clip is not shown");
        s.selected_clip = Some(high_id);
        assert_eq!(opened(&s), Some(high_id), "the selected one, whatever is under the playhead");
        s.selected_clip = Some(gap_id);
        assert_eq!(opened(&s), None, "a gap has no media");
        s.selected_clip = None;
        s.playhead = t(5.0);
        assert_eq!(opened(&s), None, "nothing under the playhead");
    }

    fn bytes(text: &str) -> Arc<[u8]> {
        text.as_bytes().into()
    }

    fn steps(s: &crate::state::AppState) -> (usize, usize) {
        (s.project_history.undo_entries().len(), s.project_history.redo_entries().len())
    }

    /// An edit in the tab is one step of Klipp's, named for the editor's;
    /// the editor's own undo and redo move over it rather than adding
    /// steps; with an edit of Klipp's after it, the editor's undo is a
    /// step of its own; Klipp's undo of a step puts the bytes before back.
    #[test]
    fn the_tabs_edits_are_klipps_steps_and_its_undo_moves_over_them() {
        use crate::state::CompositionChange as C;
        let id = CompositionId::new();
        let mut s = crate::state::AppState::default();
        s.compositions.insert(id, bytes("v0"));
        s.take_composition_change(id, C::Edit, "Move node", bytes("v1"));
        s.take_composition_change(id, C::Edit, "Blur", bytes("v2"));
        assert_eq!(steps(&s), (2, 0));
        assert_eq!(s.project_history.undo_label(), Some("Veylvik: Blur"));

        // The editor's undo, then its redo: Klipp's list moves, no more.
        s.take_composition_change(id, C::Undo, "Blur", bytes("v1"));
        assert_eq!(steps(&s), (1, 1), "moved over, not added");
        assert_eq!(s.compositions[&id], bytes("v1"));
        s.take_composition_change(id, C::Redo, "Blur", bytes("v2"));
        assert_eq!(steps(&s), (2, 0));
        assert_eq!(s.compositions[&id], bytes("v2"));

        // Nothing changed: nothing recorded.
        s.take_composition_change(id, C::Edit, "Nothing", bytes("v2"));
        assert_eq!(steps(&s), (2, 0));

        // An edit of Klipp's after the tab's: the editor's undo can't move
        // over a step that isn't next, and is one of its own.
        s.push_history(crate::history::EditCommand::SetComposition { id: CompositionId::new(), before: None, after: Some(bytes("other")), label: "Other".into() });
        s.take_composition_change(id, C::Undo, "Blur", bytes("v1"));
        assert_eq!(steps(&s).0, 4);
        assert_eq!(s.project_history.undo_label(), Some("Veylvik: Undo Blur"));

        // Klipp's undo of that step: the bytes before it.
        let entry = s.take_undo().expect("a step");
        let crate::history::EditCommand::SetComposition { before, .. } = &entry.command else { panic!("{:?}", entry.command) };
        assert_eq!(before.as_deref(), Some(&b"v2"[..]));
    }

    /// The editor is told the clip's name as the timeline gives it, and its
    /// "unavailable" opens Klipp's Relink for the clip's offline media, or
    /// names the composition's own missing files.
    #[test]
    fn the_editor_has_the_clips_name_and_klipps_relink() {
        let path = std::path::PathBuf::from("/footage/Handheld.mp4");
        let mut item = klipp_media::MediaItem::synthetic_for_test(path, None);
        item.filename = "Handheld.mp4".into();
        let clip = Clip::media(item.id, t(0.0), t(4.0), t(0.0), t(4.0));
        let id = clip.id;
        let mut seq = Sequence::new_default();
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![clip];
        seq.tracks = vec![v1];
        let mut s = crate::state::AppState { sequences: vec![seq], ..Default::default() };
        s.media_pool.add(item.clone());
        assert_eq!(clip_name(&s, id).as_deref(), Some("Handheld.mp4"), "its media's file name");
        s.sequences[0].tracks[0].clips[0].name = Some("Opening shot".into());
        assert_eq!(clip_name(&s, id).as_deref(), Some("Opening shot"), "its own name first");

        let none = std::collections::BTreeMap::new();
        relink(&mut s, Some(id), &none);
        assert!(!s.show_relink_dialog, "online media: nothing to relink");
        let missing = std::collections::BTreeMap::from([("grain.png".to_owned(), "not found".to_owned())]);
        relink(&mut s, Some(id), &missing);
        assert!(!s.show_relink_dialog, "the composition's own file is not Klipp's media");
        s.media_pool.get_mut(&item.id).unwrap().offline = true;
        relink(&mut s, Some(id), &missing);
        assert!(s.show_relink_dialog, "offline media: Klipp's Relink");
    }

    /// The page's place in a clip comes from the sequence's and the media's
    /// rates, and a frame the artist goes to comes back to Klipp's playhead
    /// exactly, NTSC rates too.
    #[test]
    fn the_page_carries_the_clips_place_at_the_sequences_rate() {
        let fps = klipp_core::Fps::new(24000, 1001);
        let mut item = klipp_media::MediaItem::synthetic_for_test(std::path::PathBuf::from("/footage/a.mp4"), None);
        item.fps = fps;
        // From timeline frame 100, media frame 30, 48 frames long.
        let at = |f: i64| playhead_at(f, fps);
        let clip = Clip::media(item.id, at(100), at(148), at(30), at(78));
        let id = clip.id;
        let mut seq = Sequence::new_default();
        seq.fps = fps;
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![clip];
        seq.tracks = vec![v1];
        let mut s = crate::state::AppState { sequences: vec![seq], ..Default::default() };
        s.media_pool.add(item);
        for f in [100, 117, 147] {
            let time = clip_time(&s, id, |t| t.composition_time(at(f))).expect("a clip with media");
            let back = clip_time(&s, id, |t| t.timeline_frame_at(time)).flatten().expect("a frame the clip shows");
            assert_eq!(playhead_at(back, fps), at(f), "timeline frame {f}");
            assert_eq!(at(f).value, f * 1001, "{f} frames of 23.976 in ticks");
        }
        let (start, end) = clip_time(&s, id, |t| t.used_range()).unwrap();
        assert!((start.seconds().unwrap() - 30.0 * 1001.0 / 24000.0).abs() < 1e-9 && (end.seconds().unwrap() - 78.0 * 1001.0 / 24000.0).abs() < 1e-9, "the trim");
        let mut tab = VeylvikTab::new(None);
        tab.carried = Some(at(120));
        tab.shown();
        assert_eq!(tab.take_carried(), None, "a new visit starts from Klipp's playhead");
    }

    /// On the Veylvik page space, J, K and L never play Klipp's timeline:
    /// played behind the page, it rendered the hidden viewer at 4K on the
    /// card the editor plays on, and both stuttered. Off the page, they do.
    #[test]
    fn the_veylvik_page_keeps_klipps_transport_keys_to_itself() {
        let mut seq = Sequence::new_default();
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0))];
        seq.tracks = vec![v1];
        let mut state = crate::state::AppState { sequences: vec![seq], project_path: Some("/projects/a.klipp".into()), active_panel: Some(crate::state::PanelKind::Timeline), ..Default::default() };
        let kb = crate::keybindings::KeyBindings::default();
        let pressed = |state: &crate::state::AppState, key: egui::Key| {
            let ctx = egui::Context::default();
            let event = egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
            let mut out = Vec::new();
            let _ = ctx.run(egui::RawInput { events: vec![event], ..Default::default() }, |ctx| out = crate::panels::transport::TransportBar::default().handle_keyboard(ctx, state, &kb));
            out
        };
        state.active_workspace = crate::state::Workspace::Veylvik;
        for key in [egui::Key::Space, egui::Key::J, egui::Key::K, egui::Key::L] {
            assert!(pressed(&state, key).is_empty(), "{key:?} reached Klipp's transport on the Veylvik page");
        }
        state.active_workspace = crate::state::Workspace::Edit;
        assert!(pressed(&state, egui::Key::Space).iter().any(|c| matches!(c, klipp_core::AppCommand::Play)), "the control: space plays on Edit");
    }
}
