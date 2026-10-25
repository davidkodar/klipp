//! Whether the project has unsaved changes, derived from what a save
//! would write (#2626). Edits raise `state.dirty` as a hint, from any of
//! the places that change the project; the hint is settled by comparing
//! the project's digest with the one taken when it was last loaded or
//! saved, so a path that raises it without changing anything that is
//! saved — a re-read file, playback, a layout — cannot mark the project
//! unsaved, and an undo back to the saved state leaves it clean.

use super::*;

/// The longest a raised hint waits to be settled while nothing asks.
const SETTLE_EVERY: std::time::Duration = std::time::Duration::from_millis(100);

impl crate::state::AppState {
    /// The colour and media settings a save writes with the project.
    pub(crate) fn project_color_settings(&self) -> klipp_project::ColorSettings {
        klipp_project::ColorSettings {
            global_lut_path: self.global_lut_path.clone(),
            global_lut_opacity: self.global_lut_opacity,
            global_lut_enabled: self.global_lut_enabled,
            track_header_width: self.track_header_width,
            auto_generate_filmstrips: self.project_auto_filmstrips,
            auto_generate_skim: self.project_auto_skim,
            working_colorspace: self.project_working_colorspace,
            grade_encoding: self.project_grade_encoding,
            default_odt: self.project_default_odt,
            default_input_format: self.project_default_input_format,
            default_input_lut: self.project_default_input_lut.clone(),
            viewing_lut: self.project_viewing_lut.clone(),
            scopes_after_viewing_lut: self.project_scopes_after_viewing_lut,
        }
    }

    /// The digest of the project as it now is; `None` when it cannot be
    /// built, which counts as unsaved.
    fn project_digest_now(&self) -> Option<u64> {
        let dir = self.project_path.clone().unwrap_or_default();
        klipp_project::project_digest(
            &dir,
            &self.sequences,
            &self.media_pool,
            &self.project_color_settings(),
            self.created_at.as_deref(),
            &self.project_notes,
            &self.title_assets,
            &self.looks,
        )
        .map_err(|e| tracing::warn!("The project's digest could not be made: {e}"))
        .ok()
    }

    /// The project as it now is is the saved one: after a load (before
    /// anything that arrives later), a save, a new project, or changes
    /// let go.
    pub(crate) fn mark_saved(&mut self) {
        self.saved_digest = self.project_digest_now();
        self.dirty = false;
        self.unsaved = false;
    }

    /// Settle a raised hint now: unsaved exactly when the project differs
    /// from the one last loaded or saved. Whether there was one to settle.
    pub(crate) fn settle_unsaved(&mut self) -> bool {
        if !self.dirty {
            return false;
        }
        self.dirty = false;
        let now = self.project_digest_now();
        self.unsaved = now.is_none() || now != self.saved_digest;
        true
    }
}

impl KlippApp {
    /// The project as it now is is the saved one.
    pub(super) fn mark_saved(&mut self) {
        self.state.mark_saved();
        self.unsaved_settled_at = None;
    }

    /// Once a frame: a raised hint is settled at most every
    /// [`SETTLE_EVERY`], and a repaint is asked for so it is settled
    /// even when nothing else is drawn.
    pub(super) fn tick_unsaved(&mut self, ctx: &egui::Context) {
        if !self.state.dirty {
            return;
        }
        match self.unsaved_settled_at.map(|t| t.elapsed()) {
            Some(since) if since < SETTLE_EVERY => ctx.request_repaint_after(SETTLE_EVERY - since),
            _ => {
                self.state.settle_unsaved();
                self.unsaved_settled_at = Some(std::time::Instant::now());
            }
        }
    }

    /// Whether the project has unsaved changes, settled now: for a
    /// decision (asking to save, auto-saving, refusing to open over it).
    pub(super) fn is_unsaved(&mut self) -> bool {
        if self.state.settle_unsaved() {
            self.unsaved_settled_at = Some(std::time::Instant::now());
        }
        self.state.unsaved
    }
}

#[cfg(test)]
mod tests {
    use crate::state::AppState;
    use klipp_core::{GroupId, Keyframe, MediaId, RationalTime};
    use klipp_effects::BuiltinEffect as E;
    use klipp_media::{MediaItem, MediaPool};
    use klipp_timeline::test_clip::TestClip;
    use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};
    use std::path::{Path, PathBuf};

    fn t(s: f64) -> RationalTime {
        RationalTime::from_secs_f64(s, 24)
    }

    fn key(time_secs: f64, value: f32) -> Keyframe {
        Keyframe { time_secs, value, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
    }

    fn title(text: &str, from: f64, to: f64) -> Clip {
        let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: text.into(), font_size: 40.0, ..Default::default() });
        Clip { kind: ClipKind::Title(td), source_in: t(0.0), source_out: t(to - from), ..Clip::gap(t(from), t(to)) }
    }

    fn track(name: &str, kind: TrackKind, clips: Vec<Clip>) -> Track {
        let mut tr = Track::new(name, kind);
        tr.clips = clips;
        tr
    }

    /// The pool for `ids`: a file each, with what the analysis said.
    fn pool(ids: &[MediaId]) -> MediaPool {
        let mut pool = MediaPool::new();
        for (i, id) in ids.iter().enumerate() {
            let mut item = MediaItem::synthetic_for_test(PathBuf::from(format!("/media/take-{i}.mp4")), None);
            item.id = *id;
            item.screen_text = format!("Take {i} · File · Edit");
            item.audio_role = Some(klipp_core::RoleProposal { role: klipp_core::ClipRole::Dialogue, confidence: 0.7, confident: true });
            pool.add(item);
        }
        pool
    }

    /// The kinds of project the issue names: several sequences with
    /// titles, a multicam group, a graded clip, captions, sound keys.
    fn projects() -> Vec<(&'static str, Vec<Sequence>, MediaPool)> {
        let (a, b) = (MediaId::new(), MediaId::new());
        let mut sequences = Vec::new();

        let mut first = Sequence::new_default();
        first.name = "First".into();
        first.tracks = vec![track("T1", TrackKind::Text, vec![title("Hello", 0.0, 3.0)]), track("V1", TrackKind::Video, vec![Clip::media(a, t(0.0), t(4.0), t(0.0), t(4.0))])];
        let mut second = Sequence::new_default();
        second.name = "Second".into();
        second.tracks = vec![track("V1", TrackKind::Video, vec![Clip::media(b, t(0.0), t(2.0), t(1.0), t(3.0))])];
        sequences.push(("several sequences and a title", vec![first, second], pool(&[a, b])));

        let group = GroupId::new();
        let mut primary = Clip::media(a, t(0.0), t(8.0), t(0.0), t(8.0));
        (primary.multicam_group, primary.multicam_angle, primary.multicam_cuts) = (Some(group), 0, vec![(3.0, 1), (6.0, 0)]);
        let mut angle = Clip::media(b, t(0.0), t(8.0), t(0.5), t(8.5));
        (angle.multicam_group, angle.multicam_angle) = (Some(group), 1);
        let mut multicam = Sequence::new_default();
        multicam.tracks = vec![track("V2", TrackKind::Video, vec![angle]), track("V1", TrackKind::Video, vec![primary])];
        sequences.push(("multicam", vec![multicam], pool(&[a, b])));

        let graded = TestClip::of(Clip::media(a, t(0.0), t(4.0), t(0.0), t(4.0)))
            .effects([E::GaussianBlur { radius: 4.0 }, E::Vignette { amount: 0.5, size: 0.5, softness: 0.5, roundness: 0.5 }])
            .key(0, "radius", 0.0, 1.0)
            .key(0, "radius", 3.0, 8.0)
            .lane("opacity", 2.0, 0.5)
            .build();
        let mut grade = Sequence::new_default();
        grade.tracks = vec![track("V1", TrackKind::Video, vec![graded])];
        sequences.push(("graded", vec![grade], pool(&[a])));

        let mut subs = track("Subtitle 1", TrackKind::Subtitle, vec![title("Hello there.", 0.5, 2.0), title("And welcome.", 2.2, 4.8)]);
        subs.language = Some("en".into());
        subs.caption_style = Some(klipp_timeline::CaptionStyle { size: 52.0, y: 0.8, shadow: true, ..Default::default() });
        let mut captions = Sequence::new_default();
        captions.tracks = vec![track("V1", TrackKind::Video, vec![Clip::media(a, t(0.0), t(5.0), t(0.0), t(5.0))]), subs];
        sequences.push(("captions", vec![captions], pool(&[a])));

        let mut sound = Clip::media(b, t(0.0), t(6.0), t(0.0), t(6.0));
        sound.volume_keyframes = vec![key(0.0, 1.0), key(4.0, 0.25)];
        sound.pan_keyframes = vec![key(1.0, -0.5), key(5.0, 0.5)];
        (sound.volume, sound.gain) = (0.8, -3.0);
        let mut audio = Sequence::new_default();
        audio.tracks = vec![track("A1", TrackKind::Audio, vec![sound])];
        sequences.push(("sound keys", vec![audio], pool(&[b])));
        sequences
    }

    /// A folder of its own for `test`'s `kind`: tests run side by side.
    fn scratch(test: &str, kind: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klipp_unsaved_{test}_{}_{}", kind.replace(' ', "_"), std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// The project saved to `dir`, then opened into a state as the open
    /// does it: the same fields from the same load, then the baseline.
    fn opened(dir: &Path, sequences: &[Sequence], pool: &MediaPool) -> AppState {
        klipp_project::save_project_full(dir, sequences, pool, &Default::default(), None, None, "notes", 0, Some(80.0), Some(0.0), Some(1.0), &Default::default()).unwrap();
        let (sequences, media_pool, color, _active, created_at, notes, zoom, scroll, scale, title_assets) = klipp_project::load_project(dir).unwrap();
        let mut s = AppState { sequences, media_pool, created_at, project_notes: notes, title_assets, ..Default::default() };
        s.looks = klipp_project::load_looks(dir);
        (s.timeline_zoom, s.timeline_scroll_x, s.timeline_track_scale) = (zoom.unwrap_or(1.0), scroll.unwrap_or(0.0), scale.unwrap_or(1.0));
        (s.global_lut_path, s.global_lut_opacity, s.global_lut_enabled) = (color.global_lut_path, color.global_lut_opacity, color.global_lut_enabled);
        s.track_header_width = color.track_header_width;
        (s.project_auto_filmstrips, s.project_auto_skim) = (color.auto_generate_filmstrips, color.auto_generate_skim);
        (s.project_working_colorspace, s.project_grade_encoding, s.project_default_odt) = (color.working_colorspace, color.grade_encoding, color.default_odt);
        (s.project_default_input_format, s.project_default_input_lut) = (color.default_input_format, color.default_input_lut);
        (s.project_viewing_lut, s.project_scopes_after_viewing_lut) = (color.viewing_lut, color.scopes_after_viewing_lut);
        s.project_path = Some(dir.to_path_buf());
        s.mark_saved();
        s
    }

    /// Settled now, as a decision asks.
    fn unsaved(s: &mut AppState) -> bool {
        s.settle_unsaved();
        s.unsaved
    }

    /// #2626: a project opened and left alone is saved, of every kind,
    /// even with the hint raised by something that changed nothing.
    #[test]
    fn an_opened_project_is_saved() {
        for (kind, sequences, pool) in projects() {
            let dir = scratch("an_opened_pr", kind);
            let mut s = opened(&dir, &sequences, &pool);
            assert!(!unsaved(&mut s), "{kind}: opened");
            s.dirty = true;
            assert!(!unsaved(&mut s), "{kind}: a hint with nothing changed");
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Every file re-read as each open does, play, a seek and a layout
    /// change (zoom, scroll, track height, header width, panels, another
    /// sequence open) leave it saved: none is a change to the project.
    #[test]
    fn rereading_playing_seeking_and_a_layout_leave_it_saved() {
        for (kind, sequences, pool) in projects() {
            let dir = scratch("rereading_pl", kind);
            let mut s = opened(&dir, &sequences, &pool);
            let ids: Vec<MediaId> = s.media_pool.all().map(|m| m.id).collect();
            for id in ids {
                let stored = s.media_pool.remove(&id).unwrap();
                let mut fresh = MediaItem { rating: 0, tags: Vec::new(), user_notes: String::new(), markers: Vec::new(), screen_text: String::new(), audio_role: None, metadata_read: true, ..stored.clone() };
                klipp_media::merge_reprobed(&stored, &mut fresh);
                s.media_pool.add(fresh);
            }
            s.playback = klipp_engine::PlaybackState::Playing;
            s.playhead = t(2.5);
            (s.timeline_zoom, s.timeline_scroll_x, s.timeline_track_scale) = (240.0, 90.0, 0.6);
            s.track_header_width += 60.0;
            (s.show_curve_editor, s.show_scopes) = (true, true);
            s.active_sequence_idx = s.sequences.len() - 1;
            s.dirty = true;
            assert!(!unsaved(&mut s), "{kind}: still as saved");
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// A real edit is unsaved, and an undo back to what was saved is not.
    #[test]
    fn an_edit_is_unsaved_and_undone_is_not() {
        for (kind, sequences, pool) in projects() {
            let dir = scratch("an_edit_is_u", kind);
            let mut s = opened(&dir, &sequences, &pool);
            let (ti, ci) = s.sequences[0].tracks.iter().enumerate().find_map(|(ti, tr)| (!tr.clips.is_empty()).then_some((ti, 0))).unwrap();
            let before = s.sequences[0].tracks[ti].clips[ci].clone();
            s.sequences[0].tracks[ti].clips[ci].timeline_out = before.timeline_out - t(0.5);
            s.dirty = true;
            assert!(unsaved(&mut s), "{kind}: trimmed");
            s.sequences[0].tracks[ti].clips[ci] = before;
            s.dirty = true;
            assert!(!unsaved(&mut s), "{kind}: undone back to as saved");
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Saved, the project is what was saved; with no saved state (an
    /// auto-save restored over it), any change is unsaved until it is.
    #[test]
    fn a_save_is_the_new_saved_state_and_a_restore_is_unsaved() {
        let (kind, sequences, pool) = projects().remove(0);
        let dir = scratch("a_save_is_th", kind);
        let mut s = opened(&dir, &sequences, &pool);
        s.sequences[0].name.push_str(" (cut)");
        s.dirty = true;
        assert!(unsaved(&mut s));
        s.mark_saved();
        assert!(!s.unsaved);
        s.saved_digest = None;
        s.dirty = true;
        assert!(unsaved(&mut s), "restored from an auto-save: unsaved until saved");
        let _ = std::fs::remove_dir_all(dir);
    }
}
