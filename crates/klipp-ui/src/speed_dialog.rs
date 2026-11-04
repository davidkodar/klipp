//! The Speed dialog's arithmetic (#3115): a clip's speed and the length it
//! plays for are one setting, the clip's source span over the length, so
//! the dialog keeps the two in step; and the lengths it offers to fill.

use klipp_core::{RationalTime, TimecodeDisplay};
use klipp_core::ClipId;
use klipp_timeline::Sequence;

/// The speeds the dialog offers, as a percentage: those a clip can have. It
/// offered 1% to 1000%, so 5% silently applied as 10%, and a clip at 1600%
/// could not be shown at its speed.
pub const PERCENT_MIN: f64 = klipp_timeline::SPEED_MIN * 100.0;
pub const PERCENT_MAX: f64 = klipp_timeline::SPEED_MAX * 100.0;

/// Seconds of source the clip shows, whatever its speed.
pub fn content_secs(seq: &Sequence, clip_id: ClipId) -> Option<f64> {
    let c = seq.all_clips().find(|c| c.id == clip_id)?;
    let s = (c.source_out - c.source_in).to_secs_f64();
    (s > 0.0).then_some(s)
}

/// How long the clip plays at `percent`.
pub fn length_at(content_secs: f64, percent: f64) -> f64 {
    content_secs / (percent / 100.0)
}

/// The percentage that makes the clip `length` seconds long.
pub fn percent_for(content_secs: f64, length: f64) -> f64 {
    content_secs / length * 100.0
}

/// The lengths the dialog offers to fit the clip to: the room up to the
/// next shot on its track (a gap clip is room, not a shot), and the
/// sequence's marked range.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FitTargets {
    pub to_next: Option<f64>,
    pub range: Option<f64>,
}

pub fn fit_targets(seq: &Sequence, clip_id: ClipId) -> FitTargets {
    let Some((ti, ci)) = seq.find_clip(clip_id) else { return FitTargets::default() };
    let clip = &seq.tracks[ti].clips[ci];
    let to_next = seq.tracks[ti].clips.iter()
        .filter(|c| c.id != clip.id && !c.kind.is_gap() && c.timeline_in >= clip.timeline_out)
        .map(|c| (c.timeline_in - clip.timeline_in).to_secs_f64())
        .min_by(f64::total_cmp)
        .filter(|l| *l > 0.0);
    let range = match (seq.in_point, seq.out_point) {
        (Some(a), Some(b)) => Some((b - a).to_secs_f64()).filter(|l| *l > 0.0),
        _ => None,
    };
    FitTargets { to_next, range }
}

/// Fit to the marked range: the document's, which the `retime`
/// op's `fit_range` uses too, so the dialog and an assistant agree.
pub use klipp_document::clipboard::{on_marks as fit_on_marks, OnMarks};

/// Whether Apply moves the clips after this one: not while the speed is
/// still the one Fill to the next clip set. Filled, the clip ends where the
/// next starts and that clip stays; moved along by the time it grew by, it
/// left a gap the same size and the fill undid itself.
pub fn moves_the_clips_after(ripple: bool, fit: FitTargets, content_secs: f64, percent: f64) -> bool {
    ripple && !fit.to_next.is_some_and(|l| (percent_for(content_secs, l).clamp(PERCENT_MIN, PERCENT_MAX) - percent).abs() < 1e-6)
}

/// The commands the dialog's Apply sends for `clip`: only what changed,
/// and the speed and Keep Pitch together as one undo step, "Clip Speed".
/// It sent the speed unchanged too, an empty step, and Keep Pitch
/// as a step of its own.
pub fn apply_commands(clip: &klipp_timeline::Clip, speed: f64, keep_pitch: bool, ripple: bool) -> Vec<klipp_core::AppCommand> {
    use klipp_core::AppCommand;
    let clip_id = clip.id;
    // The dialog holds a percentage: the clip's own speed comes back
    // from it to within rounding.
    let speed_changes = (speed - clip.speed).abs() > 1e-6;
    let pitch_changes = keep_pitch != clip.keep_pitch;
    match (speed_changes, pitch_changes) {
        (true, true) => vec![
            AppCommand::BeginHistoryBatch,
            AppCommand::SetClipSpeed { clip_id, speed, ripple },
            AppCommand::SetClipKeepPitch { clip_id, keep_pitch },
            AppCommand::EndHistoryBatchAs { label: "Clip Speed".into() },
        ],
        (true, false) => vec![AppCommand::SetClipSpeed { clip_id, speed, ripple }],
        (false, true) => vec![AppCommand::SetClipKeepPitch { clip_id, keep_pitch }],
        (false, false) => Vec::new(),
    }
}

/// A length typed into the dialog: seconds ("4", "4 s", "4.5") or a
/// timecode as the sequence counts it ("00:00:04:00").
pub fn parse_length(input: &str, tc: TimecodeDisplay) -> Option<f64> {
    let s = input.trim();
    let bare = s.strip_suffix('s').unwrap_or(s).trim();
    if let Ok(secs) = bare.parse::<f64>() {
        return (secs.is_finite() && secs > 0.0).then_some(secs);
    }
    tc.parse(s).map(|t: RationalTime| t.to_secs_f64()).filter(|l| *l > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::MediaId;
    use klipp_timeline::Clip;

    fn t(secs: f64) -> RationalTime {
        RationalTime::from_secs_f64(secs, 24)
    }

    /// A clip of 4 s of source at 0-4 s, a gap clip to 10 s, a shot at
    /// 10-14 s.
    fn seq() -> (Sequence, ClipId) {
        let mut seq = Sequence::new_default();
        let p = seq.primary_track_index().unwrap();
        let boats = Clip::media(MediaId::new(), t(0.0), t(4.0), t(14.8), t(18.8));
        let id = boats.id;
        seq.tracks[p].clips = vec![boats, Clip::gap(t(4.0), t(10.0)), Clip::media(MediaId::new(), t(10.0), t(14.0), t(0.0), t(4.0))];
        (seq, id)
    }

    /// #3115: the speed and the length stay in step either way round.
    #[test]
    fn the_speed_and_the_length_stay_in_step() {
        let (seq, id) = seq();
        let c = content_secs(&seq, id).unwrap();
        assert!((c - 4.0).abs() < 1e-6);
        assert!((length_at(c, 50.0) - 8.0).abs() < 1e-9);
        assert!((percent_for(c, 9.8) - 40.816_326_530_612_24).abs() < 1e-9);
        assert!((length_at(c, percent_for(c, 9.8)) - 9.8).abs() < 1e-9, "round trip");
    }

    /// The dialog's range is the speeds a clip can have, and the lengths that
    /// go with them.
    #[test]
    fn the_dialog_offers_the_speeds_a_clip_can_have() {
        assert_eq!((PERCENT_MIN, PERCENT_MAX), (10.0, 1600.0));
        assert!((length_at(8.0, PERCENT_MAX) - 0.5).abs() < 1e-9 && (length_at(8.0, PERCENT_MIN) - 80.0).abs() < 1e-9);
    }

    /// Fill to the next shot runs over the gap clip; the marked range
    /// offers its length; neither is offered without one.
    #[test]
    fn it_offers_the_room_to_the_next_shot_and_the_marked_range() {
        let (mut seq, id) = seq();
        assert_eq!(fit_targets(&seq, id), FitTargets { to_next: Some(10.0), range: None });
        seq.in_point = Some(t(2.0));
        seq.out_point = Some(t(11.5));
        assert_eq!(fit_targets(&seq, id).range, Some(9.5));
        let last = seq.tracks[seq.primary_track_index().unwrap()].clips[2].id;
        assert_eq!(fit_targets(&seq, last).to_next, None, "nothing follows the last shot");
    }

    /// The range offered is the marks as they stand: after an edit that
    /// shortened the timeline, what `Sequence::marks_within_end` left.
    #[test]
    fn the_range_offered_after_a_shortening_is_the_clamped_one() {
        let (mut seq, id) = seq();
        seq.in_point = Some(t(2.0));
        seq.out_point = Some(t(40.0));
        let (a, b) = seq.marks_within_end();
        (seq.in_point, seq.out_point) = (a, b);
        assert_eq!(fit_targets(&seq, id).range, Some(12.0), "2 s to the end at 14 s");
    }

    /// Fill to the next clip leaves that clip where it is, whatever Move
    /// the clips after it says, for as long as the speed is the fill's;
    /// another speed moves them as the box asks. It went from 50.90 to
    /// 53.80 s in the walk behind this, a 2.9 s gap after the filled clip.
    #[test]
    fn fill_to_the_next_clip_leaves_the_next_clip_where_it_is() {
        use klipp_core::AppCommand as C;
        let (seq, id) = seq();
        let fit = fit_targets(&seq, id);
        let content = content_secs(&seq, id).unwrap();
        let filled = percent_for(content, fit.to_next.unwrap());
        assert!(!moves_the_clips_after(true, fit, content, filled), "filled");
        assert!(moves_the_clips_after(true, fit, content, 50.0), "another speed");
        assert!(!moves_the_clips_after(false, fit, content, 50.0), "the box off");
        assert!(moves_the_clips_after(true, FitTargets::default(), content, filled), "nothing to fill to");
        let clip = seq.all_clips().find(|c| c.id == id).unwrap().clone();
        let cmds = apply_commands(&clip, filled / 100.0, false, moves_the_clips_after(true, fit, content, filled));
        assert!(matches!(cmds.as_slice(), [C::SetClipSpeed { ripple: false, .. }]), "{cmds:?}");
    }

    /// Fitted to the marked range, the clip goes on the marks: it
    /// starts at In and ends at Out at the speed that takes, its sound
    /// with it. Over another clip, Overwrite lays it over; Insert pushes
    /// what is there along by the range.
    #[test]
    fn fit_to_the_marked_range_puts_the_clip_on_the_marks() {
        use klipp_document::clipboard::PasteMode;
        use klipp_timeline::{Track, TrackKind};
        let (mut seq, id) = seq();
        let p = seq.primary_track_index().unwrap();
        // Its sound linked under it.
        let mut sound = Clip::media(MediaId::new(), t(0.0), t(4.0), t(14.8), t(18.8));
        sound.linked_clip_id = Some(id);
        seq.tracks[p].clips[0].linked_clip_id = Some(sound.id);
        let mut a1 = Track::new("A1", TrackKind::Audio);
        a1.clips.push(sound);
        seq.tracks.push(a1);
        let ai = seq.tracks.len() - 1;
        // The range 10 to 18 s: 8 s, over the shot at 10-14 s.
        seq.in_point = Some(t(10.0));
        seq.out_point = Some(t(18.0));
        let span = |s: &Sequence, ti: usize| -> Vec<(f64, f64)> { s.tracks[ti].clips.iter().filter(|c| !c.kind.is_gap()).map(|c| (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64())).collect() };

        let over = fit_on_marks(&seq, id, PasteMode::Overwrite, false).expect("fitted");
        let placed = over.sequence.all_clips().find(|c| c.id == over.clip).unwrap();
        assert_eq!((placed.timeline_in, placed.timeline_out), (t(10.0), t(18.0)), "on the marks");
        assert!((placed.speed - 0.5).abs() < 1e-9, "4 s of source over 8 s: half speed, {}", placed.speed);
        assert_eq!(span(&over.sequence, p), vec![(10.0, 18.0)], "laid over the shot at 10 s, its old place empty");
        assert_eq!(span(&over.sequence, ai), vec![(10.0, 18.0)], "its sound with it");

        let pushed = fit_on_marks(&seq, id, PasteMode::Insert, false).expect("fitted");
        assert_eq!(span(&pushed.sequence, p), vec![(10.0, 18.0), (18.0, 22.0)], "the shot pushed along by 8 s");

        seq.out_point = None;
        assert!(fit_on_marks(&seq, id, PasteMode::Overwrite, false).is_none(), "no marked range");
    }

    /// Apply sends what changed: nothing for nothing, Keep Pitch
    /// alone with no speed step, both in one "Clip Speed" step.
    #[test]
    fn apply_sends_only_what_changed() {
        use klipp_core::AppCommand as C;
        let (seq, id) = seq();
        let clip = seq.all_clips().find(|c| c.id == id).unwrap().clone();
        assert!(apply_commands(&clip, 1.0, false, false).is_empty(), "nothing changed");
        assert!(matches!(apply_commands(&clip, 1.0, true, false).as_slice(), [C::SetClipKeepPitch { keep_pitch: true, .. }]), "the pitch alone");
        assert!(matches!(apply_commands(&clip, 0.5, false, true).as_slice(), [C::SetClipSpeed { ripple: true, .. }]));
        let both = apply_commands(&clip, 0.5, true, false);
        assert!(matches!(both.as_slice(), [C::BeginHistoryBatch, C::SetClipSpeed { .. }, C::SetClipKeepPitch { .. }, C::EndHistoryBatchAs { label }] if label == "Clip Speed"), "{both:?}");
    }

    #[test]
    fn a_length_is_typed_as_seconds_or_a_timecode() {
        let tc = TimecodeDisplay::plain(klipp_core::Fps::film_24());
        assert_eq!(parse_length("4", tc), Some(4.0));
        assert_eq!(parse_length(" 4.5 s ", tc), Some(4.5));
        assert_eq!(parse_length("00:00:04:12", tc), Some(4.5));
        assert_eq!(parse_length("0", tc), None);
        assert_eq!(parse_length("soon", tc), None);
    }
}
