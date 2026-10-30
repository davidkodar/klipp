//! The Speed dialog's arithmetic (#3115): a clip's speed and the length it
//! plays for are one setting, the clip's source span over the length, so
//! the dialog keeps the two in step; and the lengths it offers to fill.

use klipp_core::{RationalTime, TimecodeDisplay};
use klipp_core::ClipId;
use klipp_timeline::Sequence;

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
