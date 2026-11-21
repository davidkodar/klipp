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
        Self { render_state, editor: None, host: Arc::new(TabHost::default()), open: None, failed: None }
    }

    /// Whether the editor has the keyboard: the page shows and it was
    /// the last thing pressed on.
    pub(crate) fn has_keyboard(&self) -> bool {
        self.editor.as_ref().is_some_and(|e| e.has_keyboard())
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
            if let Err(why) = self.veylvik_open(ui.ctx(), clip) {
                return say(ui, &why);
            }
            if let Some(editor) = self.veylvik_tab.editor.as_mut() {
                editor.ui(ui);
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
        let revision = self.veylvik_tab.editor.as_ref().map_or(0, |e| e.revision());
        self.veylvik_tab.open = Some(Open { clip, composition: id, revision });
        Ok(())
    }

    fn veylvik_make_editor(&mut self, ctx: &egui::Context, composition: klipp_veylvik::Composition, assets: std::path::PathBuf) -> Result<(), String> {
        let Some(rs) = self.veylvik_tab.render_state.clone() else {
            let why = "Veylvik needs Klipp's GPU device, and this session has none".to_string();
            self.veylvik_tab.failed = Some(why.clone());
            return Err(why);
        };
        let (again, assets_again) = (composition.clone(), assets.clone());
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
                // Opened once more now the editor knows its host: the
                // first opening read the clip's media before it did.
                if let Err(err) = e.set_composition(ctx, again, assets_again) {
                    tracing::warn!("the Veylvik tab's composition could not be opened again: {err}");
                }
                self.veylvik_tab.editor = Some(e);
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
    fn veylvik_composition_for(&mut self, clip: ClipId) -> Result<(klipp_veylvik::Composition, CompositionId), String> {
        let seq = self.state.sequence();
        let (t, c) = seq.find_clip(clip).ok_or("the clip is no longer on the timeline")?;
        let it = &seq.tracks[t].clips[c];
        if let Some(id) = it.composition {
            if let Some(bytes) = self.state.compositions.get(&id) {
                let composition: klipp_veylvik::Composition = serde_json::from_slice(bytes).map_err(|e| format!("its composition can't be read: {e}"))?;
                return Ok((composition, id));
            }
        }
        let media = it.media_id().ok_or("the clip has no media")?;
        let item = self.state.media_pool.get(&media).ok_or("the clip's media is not in the project")?;
        let fps = item.fps.as_f64();
        let rate = klipp_veylvik::Time { ticks: (fps * 1000.0).round() as i64, rate: 1000 };
        let rate = if (fps - fps.round()).abs() < 1e-6 { klipp_veylvik::Time { ticks: fps.round() as i64, rate: 1 } } else if ((fps * 1001.0 / 1000.0) - (fps * 1001.0 / 1000.0).round()).abs() < 1e-3 { klipp_veylvik::Time { ticks: (fps * 1001.0 / 1000.0).round() as i64 * 1000, rate: 1001 } } else { rate };
        let frames = ((item.duration.to_secs_f64() * fps).floor() as u32).max(1);
        let label = item.filename.clone();
        let size = [item.resolution.width.max(1), item.resolution.height.max(1)];
        let composition = klipp_veylvik::Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, &label, None, size, rate, frames).map_err(|e| e.to_string())?;
        let bytes: Arc<[u8]> = serde_json::to_vec(&composition).map_err(|e| e.to_string())?.into();
        let id = CompositionId::new();
        self.state.compositions.insert(id, bytes);
        let seq = self.state.sequence_mut();
        if let Some(c) = seq.tracks[t].clips.get_mut(c) {
            c.composition = Some(id);
        }
        self.state.dirty = true;
        self.sync_compositions_to_engine();
        self.sync_sequence_to_engine();
        Ok((composition, id))
    }

    /// The open clip's media as the editor reads it.
    fn veylvik_footage(&self, clip: ClipId) -> Option<EditorFootage> {
        let seq = self.state.sequence();
        let (t, c) = seq.find_clip(clip)?;
        let item = self.state.media_pool.get(&seq.tracks[t].clips[c].media_id()?)?;
        let device = klipp_effects::gpu_context::GpuContext::shared()?.device();
        let fps = item.fps.as_f64();
        let frames = (item.duration.to_secs_f64() * fps).floor() as u32;
        let log = klipp_engine::video_decoder::log_format_for_path(&item.path);
        Some(EditorFootage::new(item.path.clone(), log, klipp_engine::video_decoder::working_colorspace(), fps, [item.resolution.width, item.resolution.height], Some(frames), super::engine_sync::media_state(item), device))
    }

    /// The editor's changes into the project: its composition as it is now
    /// whenever its revision moved on.
    fn veylvik_take_edits(&mut self) {
        let (Some(editor), Some(open)) = (self.veylvik_tab.editor.as_ref(), self.veylvik_tab.open.as_mut()) else { return };
        let revision = editor.revision();
        if revision == open.revision {
            return;
        }
        open.revision = revision;
        match serde_json::to_vec(editor.composition()) {
            Ok(bytes) => {
                self.state.compositions.insert(open.composition, bytes.into());
                self.state.dirty = true;
                self.sync_compositions_to_engine();
            }
            Err(e) => tracing::warn!("a Veylvik edit could not be written into the project: {e}"),
        }
    }
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
}
