//! Moving the playhead by what is on the timeline: edit points, markers,
//! and the speeds J and L step through (#921).

use klipp_core::Timecode;
use klipp_timeline::{ClipKind, Sequence};

/// The places Up and Down stop at: every clip's start and end (gaps
/// aside), every marker, and the sequence's In and Out, in order, each
/// once.
pub fn edit_points(seq: &Sequence) -> Vec<Timecode> {
    // The targeted tracks' cuts only (#915).
    let mut points: Vec<Timecode> = seq
        .tracks
        .iter()
        .enumerate()
        .filter(|(i, _)| seq.is_targeted(*i))
        .flat_map(|(_, t)| t.clips.iter())
        .filter(|c| !matches!(c.kind, ClipKind::Gap))
        .flat_map(|c| [c.timeline_in, c.timeline_out])
        .chain(seq.markers.iter().map(|m| m.position))
        .chain(seq.in_point)
        .chain(seq.out_point)
        .collect();
    points.sort();
    points.dedup();
    points
}

/// The first of `points` after `at` going forward, or the last before
/// it going back.
pub fn step_to(points: &[Timecode], at: Timecode, forward: bool) -> Option<Timecode> {
    if forward {
        points.iter().copied().filter(|&p| p > at).min()
    } else {
        points.iter().copied().filter(|&p| p < at).max()
    }
}

/// The next marker after `at`, or the previous one before it.
pub fn marker_from(seq: &Sequence, at: Timecode, forward: bool) -> Option<Timecode> {
    let positions: Vec<Timecode> = seq.markers.iter().map(|m| m.position).collect();
    step_to(&positions, at, forward)
}

/// The speed J or L goes to. From a stop, from the other direction, or
/// from slow play it starts at normal speed; each press after that
/// doubles it, up to 8x.
pub fn shuttle_speed(current: f64, playing: bool, forward: bool) -> f64 {
    let dir = if forward { 1.0 } else { -1.0 };
    let along = current * dir;
    if !playing || along < 1.0 {
        return dir;
    }
    dir * if along < 2.0 {
        2.0
    } else if along < 4.0 {
        4.0
    } else {
        8.0
    }
}

/// The speed J and L play at while K is held.
pub const SLOW_SPEED: f64 = 0.5;

/// How many frames Shift+Left and Shift+Right move: the preference, or
/// one second's worth when it is 0.
pub fn shift_step_frames(preference: u32, fps: f64) -> i64 {
    if preference == 0 {
        fps.round().max(1.0) as i64
    } else {
        i64::from(preference)
    }
}

/// What Play In to Out does this frame.
#[derive(Debug, PartialEq, Eq)]
pub enum PlayToOutStep {
    /// Still waiting for playback to start.
    Wait,
    /// Playback has started.
    Started,
    /// It is over without stopping anything: the user paused, stopped or
    /// sought, or playback never started.
    Done,
    /// The Out is reached: pause there.
    StopAtOut,
}

/// How long Play In to Out waits for playback to start.
pub const PLAY_TO_OUT_START: std::time::Duration = std::time::Duration::from_secs(3);

/// The rule `KlippApp::stop_at_out` follows each frame. It has started
/// only once it plays with the playhead between the In and the Out
/// (`from_the_in`): until the seek to the In lands, a playhead still past
/// the Out would otherwise stop it at once (#940 review).
pub fn play_to_out_step(started: bool, playing: bool, since_asked: std::time::Duration, from_the_in: bool, at_out: bool) -> PlayToOutStep {
    match (started, playing) {
        (false, true) if from_the_in => PlayToOutStep::Started,
        (false, _) if since_asked > PLAY_TO_OUT_START => PlayToOutStep::Done,
        (false, _) => PlayToOutStep::Wait,
        (true, false) => PlayToOutStep::Done,
        (true, true) if at_out => PlayToOutStep::StopAtOut,
        (true, true) => PlayToOutStep::Wait,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_timeline::{Clip, Marker, Track, TrackKind};

    fn t(frames: i64) -> Timecode {
        Timecode { value: frames, rate: 25 }
    }

    fn marker(at: i64) -> Marker {
        Marker {
            id: klipp_core::MarkerId::new(),
            position: t(at),
            label: String::new(),
            color: klipp_core::MarkerColor::Blue,
            duration: None,
            comment: String::new(),
            kind: klipp_core::MarkerKind::None,
        }
    }

    fn sequence() -> Sequence {
        let mut seq = Sequence::new_default();
        seq.tracks.clear();
        let mut v = Track::new("V1", TrackKind::Video);
        v.clips.push(Clip::media(klipp_core::MediaId::new(), t(0), t(50), t(0), t(50)));
        v.clips.push(Clip::media(klipp_core::MediaId::new(), t(50), t(120), t(0), t(70)));
        seq.tracks.push(v);
        let mut a = Track::new("A1", TrackKind::Audio);
        a.clips.push(Clip::media(klipp_core::MediaId::new(), t(10), t(90), t(0), t(80)));
        seq.tracks.push(a);
        seq.markers = vec![marker(30), marker(100)];
        seq.in_point = Some(t(20));
        seq.out_point = Some(t(110));
        seq
    }

    #[test]
    fn edit_points_take_in_clips_markers_and_in_and_out() {
        let points: Vec<i64> = edit_points(&sequence()).iter().map(|p| p.value).collect();
        assert_eq!(points, vec![0, 10, 20, 30, 50, 90, 100, 110, 120]);
    }

    /// #915: an untargeted track's cuts are passed over; markers, In and
    /// Out still stop.
    #[test]
    fn edit_points_skip_untargeted_tracks() {
        let mut seq = sequence();
        seq.tracks[1].targeted = false;
        let points: Vec<i64> = edit_points(&seq).iter().map(|p| p.value).collect();
        assert_eq!(points, vec![0, 20, 30, 50, 100, 110, 120]);
    }

    #[test]
    fn up_and_down_stop_at_the_next_point_each_way() {
        let points = edit_points(&sequence());
        assert_eq!(step_to(&points, t(25), true), Some(t(30)), "a marker is a stop");
        assert_eq!(step_to(&points, t(105), true), Some(t(110)), "so is the Out");
        assert_eq!(step_to(&points, t(25), false), Some(t(20)), "and the In");
        assert_eq!(step_to(&points, t(30), true), Some(t(50)), "never the point it is on");
        assert_eq!(step_to(&points, t(120), true), None);
        assert_eq!(step_to(&points, t(0), false), None);
    }

    #[test]
    fn markers_alone_for_the_marker_keys() {
        let seq = sequence();
        assert_eq!(marker_from(&seq, t(0), true), Some(t(30)));
        assert_eq!(marker_from(&seq, t(30), true), Some(t(100)));
        assert_eq!(marker_from(&seq, t(100), false), Some(t(30)));
        assert_eq!(marker_from(&seq, t(20), false), None);
    }

    #[test]
    fn l_starts_at_normal_speed_and_doubles() {
        assert_eq!(shuttle_speed(1.0, false, true), 1.0, "from a stop: normal speed, not 2x");
        assert_eq!(shuttle_speed(1.0, true, true), 2.0);
        assert_eq!(shuttle_speed(2.0, true, true), 4.0);
        assert_eq!(shuttle_speed(4.0, true, true), 8.0);
        assert_eq!(shuttle_speed(8.0, true, true), 8.0, "8x is the top");
        assert_eq!(shuttle_speed(-4.0, true, true), 1.0, "from reverse: forward at normal speed");
        assert_eq!(shuttle_speed(SLOW_SPEED, true, true), 1.0, "from slow play: normal speed");
    }

    #[test]
    fn j_mirrors_l() {
        assert_eq!(shuttle_speed(1.0, false, false), -1.0);
        assert_eq!(shuttle_speed(-1.0, true, false), -2.0);
        assert_eq!(shuttle_speed(-8.0, true, false), -8.0);
        assert_eq!(shuttle_speed(2.0, true, false), -1.0);
    }

    #[test]
    fn shift_steps_a_second_unless_set() {
        assert_eq!(shift_step_frames(0, 25.0), 25);
        assert_eq!(shift_step_frames(0, 29.97), 30);
        assert_eq!(shift_step_frames(10, 25.0), 10);
    }

    #[test]
    fn play_in_to_out_stops_at_the_out_and_nowhere_else() {
        use std::time::Duration;
        let soon = Duration::from_millis(100);
        assert_eq!(play_to_out_step(false, false, soon, false, true), PlayToOutStep::Wait, "the seek and play are on their way");
        assert_eq!(play_to_out_step(false, true, soon, false, true), PlayToOutStep::Wait, "playing, but the playhead is still past the Out: not started, not stopped");
        assert_eq!(play_to_out_step(false, true, soon, true, false), PlayToOutStep::Started, "playing from the In");
        assert_eq!(play_to_out_step(true, true, soon, true, false), PlayToOutStep::Wait, "playing towards the Out");
        assert_eq!(play_to_out_step(true, true, soon, false, true), PlayToOutStep::StopAtOut);
        assert_eq!(play_to_out_step(true, false, soon, true, false), PlayToOutStep::Done, "the user paused: nothing to stop");
        assert_eq!(play_to_out_step(false, false, Duration::from_secs(5), false, false), PlayToOutStep::Done, "playback never started");
        assert_eq!(play_to_out_step(false, true, Duration::from_secs(5), false, true), PlayToOutStep::Done, "the seek never landed");
    }
}
