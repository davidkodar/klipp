//! A sound-only clip placed under the picture ends with it (#2983): a
//! three-minute song put under a twenty-second cut no longer makes the
//! timeline three minutes long. The trim is a step of its own after the
//! placement, with a fade-out, and the note it shows undoes just that
//! step, so the full length is one click away. With no picture, or a
//! sound placed past the picture's end, nothing is trimmed.

use crate::history::EditCommand;
use klipp_core::{ClipId, Timecode};
use klipp_timeline::{Sequence, TrackKind};

/// The trim's step in the history.
pub(crate) const LABEL: &str = "Trim to the end of the picture";

/// The fade the trimmed end gets, seconds: at most half the clip.
pub(crate) const FADE_OUT_SECS: f32 = 1.0;

/// Where the picture ends: the latest end of an enabled clip on a video
/// track. `None` with no picture.
pub(crate) fn picture_end(seq: &Sequence) -> Option<Timecode> {
    seq.tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .flat_map(|t| t.clips.iter())
        .filter(|c| c.enabled)
        .map(|c| c.timeline_out)
        .max_by(|a, b| a.to_secs_f64().total_cmp(&b.to_secs_f64()))
}

/// The edits that end the sound clip `clip` with the picture, fading out:
/// `None` unless it is on an audio track, begins before the picture ends
/// and runs past it.
pub(crate) fn trim_edits(seq: &Sequence, clip: ClipId) -> Option<Vec<EditCommand>> {
    let end = picture_end(seq)?;
    let c = seq.tracks.iter().filter(|t| t.kind == TrackKind::Audio).find_map(|t| t.find_clip(clip))?;
    let (start, out, end_secs) = (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64(), end.to_secs_f64());
    if !(start < end_secs && end_secs < out) {
        return None;
    }
    let kept = end_secs - start;
    let new_source_out = Timecode::from_secs_f64(c.source_in.to_secs_f64() + kept * c.speed, c.source_out.rate);
    let fade = FADE_OUT_SECS.min((kept / 2.0) as f32);
    Some(vec![
        EditCommand::LegacyTrimClip {
            clip_id: clip,
            old_source_in: c.source_in,
            old_source_out: c.source_out,
            old_timeline_in: c.timeline_in,
            old_timeline_out: c.timeline_out,
            new_source_in: c.source_in,
            new_source_out,
            new_timeline_in: c.timeline_in,
            new_timeline_out: end,
        },
        EditCommand::EditClipAudio {
            clip_id: clip,
            old_gain: c.gain,
            old_volume: c.volume,
            old_pan: c.pan,
            old_fade_in: c.fade_in_secs,
            old_fade_out: c.fade_out_secs,
            old_channel_config: c.channel_config,
            new_gain: c.gain,
            new_volume: c.volume,
            new_pan: c.pan,
            new_fade_in: c.fade_in_secs,
            new_fade_out: c.fade_out_secs.max(fade),
            new_channel_config: c.channel_config,
        },
    ])
}

/// The note the trim shows.
pub(crate) fn note(kind: &str) -> String {
    format!("{kind} trimmed to the end of the picture (full length available)")
}

impl super::KlippApp {
    /// After a sound-only clip was placed: end it with the picture, as a
    /// step of its own, and say so with "Undo trim" (#2983).
    pub(super) fn trim_sound_to_picture(&mut self, clip: ClipId) {
        let Some(edits) = trim_edits(self.state.sequence(), clip) else { return };
        self.apply_edits_as_one(&edits, LABEL);
        let Some(stamp) = self.state.active_history().undo_stamped().last().map(|(s, _)| s) else { return };
        let kind = match self.state.sequence().all_clips().find(|c| c.id == clip).and_then(|c| c.role.clone()) {
            Some(r) if r.is("music") => "Music",
            _ => "Sound",
        };
        self.state.toast.show_with_action(note(kind), "Undo trim", crate::widgets::toast::ToastAction::UndoStep(stamp));
    }
}

impl super::KlippApp {
    /// A toast's button, clicked.
    pub(super) fn toast_action(&mut self, action: crate::widgets::toast::ToastAction) {
        match action {
            crate::widgets::toast::ToastAction::UndoStep(stamp) => {
                // Only that step: another made since would be undone instead.
                if undo_step_is_last(&self.state, stamp) {
                    self.send_commands(vec![klipp_core::AppCommand::Undo]);
                    self.state.toast.show("Full length back");
                } else {
                    self.state.toast.show("Something was changed since; use Undo in the Edit menu");
                }
            }
            crate::widgets::toast::ToastAction::Stretch { clip_id, at_end, to } => {
                self.send_commands(vec![klipp_core::AppCommand::StretchClip { clip_ids: vec![clip_id], at_end, to, ripple: false, keep_pitch: true }]);
            }
            crate::widgets::toast::ToastAction::SmoothMotion(clip_id) => {
                match klipp_document::ops::interpolation_at(self.state.sequence(), &self.state.media_pool, clip_id, "flow") {
                    Ok(Some((_, edit))) => {
                        self.apply_edits_as_one(&[edit], "Smooth Motion");
                        self.state.toast.show("Smooth motion: its in-between frames follow the movement");
                    }
                    Ok(None) => {}
                    Err(r) => self.state.toast.show(format!("Smooth motion not set: {}", r.why)),
                }
            }
        }
    }
}

/// Whether the last step in the history is the one stamped `stamp`.
pub(crate) fn undo_step_is_last(state: &crate::state::AppState, stamp: u64) -> bool {
    state.active_history().undo_stamped().last().is_some_and(|(s, _)| s == stamp)
}
