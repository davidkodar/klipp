//! The timeline fits itself after an assistant's edit: when an edit over
//! the control socket changes the sequence's length or puts a clip outside
//! the view, the view eases to the whole sequence, as the Fit button sets
//! it. Not when the person moved the view in the last few seconds, and not
//! when they turned it off (Preferences → Timeline).

use std::collections::HashSet;
use std::time::{Duration, Instant};

use klipp_core::{AppCommand, ClipId};

use crate::state::AppState;

/// How long after the person zooms or scrolls the timeline an assistant's
/// edit leaves the view where they put it.
pub const QUIET_AFTER_TOUCH: Duration = Duration::from_secs(10);
/// How long an assistant's edit is waited for: an import lands on the
/// timeline some frames after the request that asked for it.
pub const WAIT_FOR_EDIT: Duration = Duration::from_secs(3);
/// How long the view takes to ease to the fit.
pub const EASE: Duration = Duration::from_millis(250);

/// The view's state between frames: an assistant's edit waiting to be
/// looked at, when the person last moved the view, and a fit under way.
#[derive(Debug, Default)]
pub struct AutoFit {
    pending: Option<Before>,
    seen_view: Option<(f32, f32)>,
    touched_at: Option<Instant>,
    easing: Option<Easing>,
}

/// The sequence as it was before an assistant's edit: which, its length,
/// and the clips the view didn't show whole.
#[derive(Debug, Clone)]
pub struct Before {
    at: Instant,
    sequence: usize,
    duration: f64,
    outside: HashSet<ClipId>,
}

/// A fit under way: zoom, horizontal scroll and track scale, from and to.
#[derive(Debug, Clone, Copy)]
struct Easing {
    start: Instant,
    from: (f32, f32, f32),
    to: (f32, f32, f32),
}

/// Whether control commands can change the document: anything but the
/// transport, which moves the playhead and nothing else.
pub fn edits_document(commands: &[AppCommand]) -> bool {
    commands.iter().any(|c| !matches!(c, AppCommand::Play | AppCommand::Pause | AppCommand::Stop | AppCommand::SeekTo(_) | AppCommand::SeekRelative(_)))
}

impl AppState {
    /// The zoom that shows the whole sequence in the timeline's width, as
    /// the Fit button sets it; `None` with no length or no width yet.
    pub(crate) fn fit_zoom(&self) -> Option<f32> {
        let dur = self.sequence_duration().to_secs_f64() as f32;
        let w = self.last_timeline_content_width;
        (dur > 0.0 && w > 0.0).then(|| (w / dur).clamp(0.1, 2000.0))
    }

    /// The track scale that shows every track in the timeline's height.
    pub(crate) fn fit_track_scale(&self) -> f32 {
        let total: f32 = self.sequence().tracks.iter().map(crate::panels::timeline::track_row_height).sum();
        crate::panels::timeline::fit_track_scale(total, self.last_timeline_tracks_height)
    }

    /// The clips of the active sequence the view doesn't show whole.
    fn clips_outside_view(&self) -> HashSet<ClipId> {
        let zoom = self.timeline_zoom.max(1e-3) as f64;
        let from = self.timeline_scroll_x as f64 / zoom;
        let to = (self.timeline_scroll_x + self.last_timeline_content_width) as f64 / zoom;
        // Half a pixel either way is still in view.
        let slack = 0.5 / zoom;
        self.sequence()
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .filter(|c| c.timeline_in.to_secs_f64() < from - slack || c.timeline_out.to_secs_f64() > to + slack)
            .map(|c| c.id)
            .collect()
    }

    /// The sequence as it is now, for [`AppState::after_assistant_edit`].
    pub fn before_assistant_edit(&self, now: Instant) -> Before {
        Before { at: now, sequence: self.active_sequence_idx, duration: self.sequence_duration().to_secs_f64(), outside: self.clips_outside_view() }
    }

    /// An assistant's edit was applied over `before`: look at it on the
    /// coming frames.
    pub fn after_assistant_edit(&mut self, before: Before) {
        self.auto_fit.pending = Some(before);
    }

    /// One frame of the fit: the person's own moves of the view noted, a
    /// waiting edit looked at, a fit under way eased on. True while the
    /// view is easing, so a frame is asked for.
    pub fn step_auto_fit(&mut self, now: Instant) -> bool {
        if let Some(e) = self.auto_fit.easing {
            let t = (now.saturating_duration_since(e.start).as_secs_f32() / EASE.as_secs_f32()).min(1.0);
            let k = 1.0 - (1.0 - t).powi(3);
            let lerp = |a: f32, b: f32| a + (b - a) * k;
            // Zoom eases by ratio, so each step looks the same size.
            self.timeline_zoom = (e.from.0.ln() + (e.to.0.ln() - e.from.0.ln()) * k).exp();
            self.timeline_scroll_x = lerp(e.from.1, e.to.1);
            self.timeline_track_scale = lerp(e.from.2, e.to.2);
            if t >= 1.0 {
                (self.timeline_zoom, self.timeline_scroll_x, self.timeline_track_scale) = e.to;
                self.auto_fit.easing = None;
            }
            self.auto_fit.seen_view = Some((self.timeline_zoom, self.timeline_scroll_x));
            return self.auto_fit.easing.is_some();
        }
        let view = (self.timeline_zoom, self.timeline_scroll_x);
        if self.auto_fit.seen_view.is_some_and(|seen| seen != view) {
            self.auto_fit.touched_at = Some(now);
        }
        self.auto_fit.seen_view = Some(view);

        let Some(before) = self.auto_fit.pending.take() else { return false };
        if before.sequence != self.active_sequence_idx || now.saturating_duration_since(before.at) > WAIT_FOR_EDIT {
            return false;
        }
        let lengthened = (self.sequence_duration().to_secs_f64() - before.duration).abs() > 1e-6;
        let left_view = !lengthened && self.clips_outside_view().difference(&before.outside).next().is_some();
        if !lengthened && !left_view {
            // Nothing yet: an import may still land.
            self.auto_fit.pending = Some(before);
            return false;
        }
        let touched = self.auto_fit.touched_at.is_some_and(|at| now.saturating_duration_since(at) < QUIET_AFTER_TOUCH);
        if !self.timeline_prefs.fit_after_assistant_edits || touched {
            return false;
        }
        let Some(zoom) = self.fit_zoom() else { return false };
        self.timeline_scroll_y = 0.0;
        self.auto_fit.easing = Some(Easing { start: now, from: (self.timeline_zoom.max(0.1), self.timeline_scroll_x, self.timeline_track_scale), to: (zoom, 0.0, self.fit_track_scale()) });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::Clip;

    fn t(s: f64) -> RationalTime {
        RationalTime::from_secs_f64(s, 24)
    }

    /// A 20 s sequence on V1, the view 1000 px wide showing 0–20 s, the
    /// view settled (a frame seen).
    fn state(now: Instant) -> AppState {
        let mut state = AppState::default();
        let v = state.sequence().tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        state.sequence_mut().tracks[v].clips = vec![Clip::media(MediaId::new(), t(0.0), t(20.0), t(0.0), t(20.0))];
        state.last_timeline_content_width = 1000.0;
        state.last_timeline_tracks_height = 400.0;
        state.timeline_zoom = 50.0;
        state.timeline_scroll_x = 0.0;
        state.step_auto_fit(now);
        state
    }

    /// An edit as the socket makes it: the clip appended at the end.
    fn append(state: &mut AppState, secs: f64) {
        let v = state.sequence().tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        let end = state.sequence().tracks[v].clips.last().map_or(0.0, |c| c.timeline_out.to_secs_f64());
        state.sequence_mut().tracks[v].clips.push(Clip::media(MediaId::new(), t(end), t(end + secs), t(0.0), t(secs)));
    }

    fn settle(state: &mut AppState, from: Instant) -> Instant {
        let end = from + EASE + Duration::from_millis(16);
        let mut now = from;
        while now < end {
            now += Duration::from_millis(16);
            state.step_auto_fit(now);
        }
        now
    }

    /// An assistant's edit that lengthens the sequence past the view eases
    /// it to the whole sequence: the Fit button's zoom, scrolled to 0.
    #[test]
    fn a_socket_edit_that_lengthens_the_sequence_fits_the_view() {
        let now = Instant::now();
        let mut state = state(now);
        let before = state.before_assistant_edit(now);
        append(&mut state, 20.0);
        state.after_assistant_edit(before);
        assert!(state.step_auto_fit(now), "a fit starts, a frame asked for");
        settle(&mut state, now);
        assert!((state.timeline_zoom - 25.0).abs() < 1e-3, "40 s in 1000 px: {}", state.timeline_zoom);
        assert_eq!(state.timeline_scroll_x, 0.0);
        assert_eq!(Some(state.timeline_zoom), state.fit_zoom(), "as the Fit button sets it");
    }

    /// The view eases there: halfway through, between where it was and
    /// the fit.
    #[test]
    fn the_fit_eases() {
        let now = Instant::now();
        let mut state = state(now);
        let before = state.before_assistant_edit(now);
        append(&mut state, 20.0);
        state.after_assistant_edit(before);
        state.step_auto_fit(now);
        assert!(state.step_auto_fit(now + EASE / 2), "still easing");
        assert!(state.timeline_zoom < 50.0 && state.timeline_zoom > 25.0, "between: {}", state.timeline_zoom);
    }

    /// The person zoomed a moment before: the edit leaves the view alone.
    #[test]
    fn a_zoom_just_before_holds_the_view() {
        let now = Instant::now();
        let mut state = state(now);
        state.timeline_zoom = 80.0;
        let later = now + Duration::from_secs(2);
        state.step_auto_fit(later);
        let before = state.before_assistant_edit(later);
        append(&mut state, 20.0);
        state.after_assistant_edit(before);
        assert!(!state.step_auto_fit(later));
        settle(&mut state, later);
        assert_eq!(state.timeline_zoom, 80.0, "the person's zoom kept");
        // Once the quiet is over, the next edit fits again.
        let after_quiet = later + QUIET_AFTER_TOUCH + Duration::from_secs(1);
        let before = state.before_assistant_edit(after_quiet);
        append(&mut state, 5.0);
        state.after_assistant_edit(before);
        assert!(state.step_auto_fit(after_quiet), "fits after the quiet");
    }

    /// An edit inside the view that keeps the length leaves the zoom alone.
    #[test]
    fn a_small_edit_inside_the_view_keeps_the_zoom() {
        let now = Instant::now();
        let mut state = state(now);
        state.timeline_zoom = 40.0;
        state.step_auto_fit(now);
        let later = now + QUIET_AFTER_TOUCH + Duration::from_secs(1);
        state.step_auto_fit(later);
        let before = state.before_assistant_edit(later);
        let v = state.sequence().tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        state.sequence_mut().tracks[v].clips[0].volume = 0.5;
        state.after_assistant_edit(before);
        assert!(!state.step_auto_fit(later));
        settle(&mut state, later + WAIT_FOR_EDIT + Duration::from_millis(1));
        assert_eq!(state.timeline_zoom, 40.0, "zoom kept");
        assert!(state.auto_fit.pending.is_none(), "given up after the wait");
    }

    /// A clip moved out of the view, the length the same, fits too.
    #[test]
    fn a_clip_moved_out_of_view_fits() {
        let now = Instant::now();
        let mut state = state(now);
        append(&mut state, 20.0);
        // Zoomed on the first 20 s, settled long ago.
        let later = now + QUIET_AFTER_TOUCH + Duration::from_secs(1);
        state.step_auto_fit(later);
        let v = state.sequence().tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        let before = state.before_assistant_edit(later);
        // The first clip and the second swap places: the first now 20–40 s.
        let clips = &mut state.sequence_mut().tracks[v].clips;
        clips.swap(0, 1);
        (clips[0].timeline_in, clips[0].timeline_out) = (t(0.0), t(20.0));
        (clips[1].timeline_in, clips[1].timeline_out) = (t(20.0), t(40.0));
        state.after_assistant_edit(before);
        assert!(state.step_auto_fit(later), "a clip left the view");
    }

    /// Turned off in Preferences, an edit leaves the view alone.
    #[test]
    fn turned_off_it_leaves_the_view() {
        let now = Instant::now();
        let mut state = state(now);
        state.timeline_prefs.fit_after_assistant_edits = false;
        let before = state.before_assistant_edit(now);
        append(&mut state, 20.0);
        state.after_assistant_edit(before);
        assert!(!state.step_auto_fit(now));
        assert_eq!(state.timeline_zoom, 50.0);
    }

    /// An import lands frames after its request: the wait catches it.
    #[test]
    fn an_edit_landing_a_moment_later_fits() {
        let now = Instant::now();
        let mut state = state(now);
        let before = state.before_assistant_edit(now);
        state.after_assistant_edit(before);
        assert!(!state.step_auto_fit(now), "nothing yet");
        append(&mut state, 20.0);
        assert!(state.step_auto_fit(now + Duration::from_millis(500)), "the import landed");
    }

    /// The transport alone doesn't count as an edit.
    #[test]
    fn only_document_commands_are_edits() {
        assert!(!edits_document(&[AppCommand::Play, AppCommand::SeekRelative(3), AppCommand::Pause]));
        assert!(edits_document(&[AppCommand::Play, AppCommand::Undo]));
    }
}
