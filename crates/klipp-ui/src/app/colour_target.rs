//! Which clip the Colour page grades (#2952): the one at the playhead, the
//! selected one when clips are stacked there, and a selection elsewhere
//! named in the page's header with "Go to it". The page never moves the
//! playhead by itself; only that click does.

use crate::state::AppState;
use klipp_core::{ClipId, Timecode};

impl AppState {
    /// The clips on the visible video tracks at `at`, the top track's
    /// first: what the viewer shows there.
    pub(crate) fn picture_clips_at(&self, at: Timecode) -> Vec<ClipId> {
        self.sequence().tracks.iter()
            .filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Video) && t.visible)
            .filter_map(|t| t.clip_at_timecode(at).map(|(_, c)| c.id))
            .collect()
    }

    /// The clip the Colour page grades at the playhead, selected: the
    /// selected one when it is among the clips stacked there, else the top
    /// one, a selection elsewhere kept in `colour_elsewhere`. With no clip
    /// there the selection stays as it is (`None`). The playhead is left
    /// where it is.
    pub(crate) fn grade_at_playhead(&mut self) -> Option<ClipId> {
        let here = self.picture_clips_at(self.playhead);
        let graded = self.selected_clip.filter(|c| here.contains(c)).or_else(|| here.first().copied())?;
        if self.selected_clip != Some(graded) {
            // The person's own selection, the first one passed over.
            if self.colour_elsewhere.is_none() {
                self.colour_elsewhere = self.selected_clip.filter(|c| self.picture_clip(*c).is_some());
            }
            self.selected_clip = Some(graded);
            self.selected_clips.clear();
        }
        if self.colour_elsewhere == Some(graded) {
            self.colour_elsewhere = None;
        }
        Some(graded)
    }

    /// Whether the page should look again for the clip to grade: the
    /// playhead has moved since `last`, or nothing is selected any more, as
    /// after Escape (#3451), which would otherwise leave the page with no
    /// clip until the next move, or the one selected is a picture away from
    /// the playhead while one is there: a click on another clip made the
    /// header say it was graded, the viewer still on the clip at the
    /// playhead. The page grades that one and offers the pick with "Go to
    /// it". A selection of several is left alone.
    pub(crate) fn grade_needs_following(&self, last: Option<Timecode>) -> bool {
        let picked_away = self.selected_clip.is_some_and(|c| {
            let here = self.picture_clips_at(self.playhead);
            self.picture_clip(c).is_some() && !here.is_empty() && !here.contains(&c)
        });
        last != Some(self.playhead) || (self.selected_clip.is_none() && self.selected_clips.is_empty()) || picked_away
    }

    /// `clip` on a video track of the sequence, with that track's index.
    fn picture_clip(&self, clip: ClipId) -> Option<(usize, &klipp_timeline::Clip)> {
        self.sequence().tracks.iter().enumerate()
            .filter(|(_, t)| matches!(t.kind, klipp_timeline::TrackKind::Video))
            .find_map(|(i, t)| t.clips.iter().find(|c| c.id == clip).map(|c| (i, c)))
    }

    /// The Colour page's header for `clip`: "Grading: <file> · V1".
    pub(crate) fn grading_title(&self, clip: ClipId) -> String {
        let Some((track, c)) = self.picture_clip(clip) else { return "Grading".into() };
        let name = c.media_id().and_then(|m| self.media_pool.get(&m)).map(|m| m.filename.clone()).unwrap_or_else(|| "Clip".into());
        let n = crate::timeline::track_headers::badge_number(&self.sequence().tracks, track);
        format!("Grading: {name} · V{n}")
    }

    /// Where selecting `clip` takes the playhead: a title the playhead is
    /// outside of brings it to its rest pose, as the viewer shows a title
    /// only where the engine draws it; nothing else moves it. On the
    /// Colour page a clip picked elsewhere is named in the header with
    /// "Go to it", which moves it when clicked: the pick took the playhead
    /// there itself, which the page must never do unasked.
    pub(crate) fn seek_on_select(&self, clip: ClipId) -> Option<Timecode> {
        let c = self.sequence().all_clips().find(|c| c.id == clip)?;
        c.title_data()?;
        super::titles::title_seek_on_select(c, self.playhead, self.sequence().fps.rate())
    }

    /// A clip picked on the Colour page is the one graded (#2952), so no
    /// earlier selection is waiting elsewhere: also when it was the
    /// selected clip already, as the one the page had chosen (#3426).
    pub(crate) fn colour_clip_picked(&mut self) {
        if self.active_workspace == crate::state::Workspace::Color {
            self.colour_elsewhere = None;
        }
    }

    /// The clip the header's "Go to it" goes to: the selection the page
    /// passed over, or the graded clip itself when the playhead has left
    /// it (a gap keeps the last one graded).
    pub(crate) fn colour_elsewhere_hint(&self) -> Option<ClipId> {
        let selected = self.selected_clip.filter(|c| self.picture_clip(*c).is_some());
        self.colour_elsewhere.filter(|c| Some(*c) != selected && self.picture_clip(*c).is_some())
            .or_else(|| selected.filter(|c| !self.picture_clips_at(self.playhead).contains(c)))
    }

    /// "Go to it": `clip` selected and the playhead on it, where it was
    /// last graded (`remembered`) or its first frame. Where the playhead
    /// went, when it moved.
    pub(crate) fn go_to_for_grading(&mut self, clip: ClipId, remembered: Option<Timecode>) -> Option<Timecode> {
        self.colour_elsewhere = None;
        let at = self.picture_clip(clip).and_then(|(_, c)| super::colour::colour_seek_on_select(c, self.playhead, remembered));
        if self.picture_clip(clip).is_some() {
            self.selected_clip = Some(clip);
            self.selected_clips.clear();
        }
        if let Some(t) = at {
            self.playhead = t;
            self.playhead_smooth_secs = t.to_secs_f64();
            self.playhead_anchor = t;
            self.playhead_anchor_wall = std::time::Instant::now();
        }
        at
    }
}

/// The header's quiet hint under the title: "Selected clip is elsewhere —
/// Go to it". `true` when "Go to it" is clicked.
pub(crate) fn elsewhere_hint(ui: &mut egui::Ui, theme: &crate::theme::KlippTheme) -> bool {
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(egui::RichText::new("Selected clip is elsewhere —").size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
        ui.link(egui::RichText::new("Go to it").size(crate::theme::type_scale::CAPTION))
            .on_hover_text("Move the playhead to the selected clip and grade it")
            .clicked()
    })
    .inner
}

/// The header's offer for a graded piece of a split shot: "Apply to the
/// other N pieces of this shot". `true` when it is clicked.
pub(crate) fn shot_pieces_hint(ui: &mut egui::Ui, theme: &crate::theme::KlippTheme, n: usize) -> bool {
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(egui::RichText::new("Split shot —").size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
        let text = if n == 1 { "Apply to the other piece".to_string() } else { format!("Apply to the other {n} pieces") };
        ui.link(egui::RichText::new(text).size(crate::theme::type_scale::CAPTION))
            .on_hover_text("Give this grade to the ungraded pieces of the same shot, as one undo step")
            .clicked()
    })
    .inner
}

impl super::KlippApp {
    /// "Go to it" from the Colour page's header: the playhead moved onto
    /// the clip, which the page then grades.
    pub(super) fn go_to_colour_elsewhere(&mut self, clip: ClipId) {
        let remembered = self.graded_positions.get(&clip).copied();
        if let Some(t) = self.state.go_to_for_grading(clip, remembered) {
            // The follow must not re-pick from here: the clip may sit
            // under a higher track.
            self.last_graded_playhead = Some(t);
            let _ = self.cmd_tx.send(klipp_core::AppCommand::SeekTo(t));
        }
    }
}
