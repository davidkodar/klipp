//! A camera clip paired with the sound a separate recorder made of it:
//! which audio file, and how far into it the picture's first frame
//! falls. Found from the two files' timecodes when both carry one, and
//! from the sound itself otherwise.

use crate::{Fps, MediaId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncMethod {
    Timecode,
    Waveform,
}

impl SyncMethod {
    pub fn label(self) -> &'static str {
        match self {
            SyncMethod::Timecode => "timecode",
            SyncMethod::Waveform => "waveform",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncedAudio {
    pub audio_media_id: MediaId,
    /// Audio time = video time + this: how far into the recording the
    /// picture's first frame falls (negative when the recorder started
    /// after the camera).
    pub offset_secs: f64,
    pub method: SyncMethod,
}

impl SyncedAudio {
    /// "+1.234 s by timecode".
    pub fn describe(&self) -> String {
        format!("{:+.3} s by {}", self.offset_secs, self.method.label())
    }
}

/// The seconds a timecode "hh:mm:ss:ff" stands for at `fps`. A
/// semicolon before the frames marks drop-frame, where two frames (four
/// at 59.94) are skipped each minute but the tenth, so the count stays
/// on the clock; the seconds come out of the true frame rate either way.
pub fn timecode_to_secs(tc: &str, fps: Fps) -> Option<f64> {
    let tc = tc.trim();
    if fps.den == 0 || fps.num == 0 {
        return None;
    }
    let drop = tc.contains(';');
    let parts: Vec<u64> = tc.split([':', ';', '.']).map(|p| p.trim().parse::<u64>()).collect::<Result<_, _>>().ok()?;
    let [h, m, s, f] = parts[..] else { return None };
    let rate = fps.as_f64();
    let nominal = rate.round() as u64;
    if nominal == 0 || f >= nominal.max(1) || s > 59 || m > 59 {
        return None;
    }
    let minutes = h * 60 + m;
    let mut frames = (minutes * 60 + s) * nominal + f;
    if drop {
        let per_minute = match nominal {
            30 => 2,
            60 => 4,
            _ => 0,
        };
        frames = frames.checked_sub(per_minute * (minutes - minutes / 10))?;
    }
    Some(frames as f64 / rate)
}

/// How many frames a minute drops at `fps`: two at 29.97, four at
/// 59.94, none at any rate where drop-frame timecode does not exist.
///
/// Drop-frame is a counting trick for the NTSC rates, whose true rate
/// is 1000/1001 of the nominal one. Skipping frame *numbers* — never
/// frames — keeps the count on the wall clock, which is what a
/// broadcast running order is measured against.
pub fn drop_frames_per_minute(fps: Fps) -> u64 {
    match (fps.as_f64() * 100.0).round() as u64 {
        2997 => 2,
        5994 => 4,
        _ => 0,
    }
}

/// Whether drop-frame timecode exists at `fps` at all.
pub fn is_drop_frame_rate(fps: Fps) -> bool {
    drop_frames_per_minute(fps) > 0
}

/// The timecode of frame `frames` at `fps`, as (hours, minutes,
/// seconds, frames). The inverse of [`timecode_to_secs`], and the one
/// place the forward conversion lives — a second copy of this
/// arithmetic is how a file comes to declare drop-frame and then
/// contain non-drop numbers.
///
/// With `drop`, frame numbers are skipped at the top of every minute
/// but every tenth, so the count tracks the clock. Without it the count
/// runs slow against the clock by about 3.6 s an hour at 29.97, which
/// is correct and is what 29.97 non-drop delivery means.
///
/// `drop` is honoured only at a rate that has drop-frame; asking for it
/// at 25 or 24 gives a non-drop count, because there is nothing to drop.
pub fn frames_to_timecode(frames: i64, fps: Fps, drop: bool) -> (i64, i64, i64, i64) {
    let nominal = fps.as_f64().round().max(1.0) as i64;
    let mut n = frames.max(0);
    let per_minute = drop_frames_per_minute(fps) as i64;

    if drop && per_minute > 0 {
        // Walk the count forward over the frame numbers that do not
        // exist, so the remaining arithmetic can treat it as a plain
        // nominal-rate count. Ten minutes of drop-frame is nine
        // dropped minutes plus one whole one.
        let per_10min = nominal * 60 * 10 - per_minute * 9;
        let per_min = nominal * 60 - per_minute;
        let tens = n / per_10min;
        let within = n % per_10min;
        // The first minute of each ten drops nothing, so only what is
        // past it contributes further skips.
        let extra = if within >= nominal * 60 {
            1 + (within - nominal * 60) / per_min
        } else {
            0
        };
        n += per_minute * (9 * tens + extra);
    }

    let f = n % nominal;
    let total_secs = n / nominal;
    (total_secs / 3600, (total_secs / 60) % 60, total_secs % 60, f)
}

/// How the angles of a multicam are put on one clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MulticamSync {
    /// By each clip's start timecode only.
    Timecode,
    /// By sound only, against one reference angle.
    Audio,
    /// Timecode where a clip has it, sound for the rest.
    TimecodeThenAudio,
}

impl MulticamSync {
    pub fn label(self) -> &'static str {
        match self {
            MulticamSync::Timecode => "Sync by Timecode",
            MulticamSync::Audio => "Sync by Audio",
            MulticamSync::TimecodeThenAudio => "Sync by Timecode, Else Audio",
        }
    }
}

/// Whose sound a multicam group plays, set on its primary. Every angle
/// brings its own sound, and heard together they are the same moment
/// several times over, a room of echoes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MulticamSound {
    /// One angle's sound throughout, whatever the picture cuts to (counted
    /// from 0; an angle the group lacks is the primary's). The default,
    /// angle 0, is the sound a synced group's angles were matched to.
    Angle(u8),
    /// The sound of the angle on screen, cut where the picture cuts.
    FollowPicture,
    /// No angle's sound: the group plays under sound from elsewhere, a
    /// recorder's track outside the group.
    Off,
}

impl Default for MulticamSound {
    fn default() -> Self {
        Self::Angle(0)
    }
}

impl MulticamSound {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// As a menu and a reply name it, angles counted from 1.
    pub fn label(self) -> String {
        match self {
            MulticamSound::Angle(a) => format!("Angle {}", a as usize + 1),
            MulticamSound::FollowPicture => "Follow the Picture".into(),
            MulticamSound::Off => "No Angle (Sound From Elsewhere)".into(),
        }
    }
}

/// One clip to become an angle, as far as syncing is concerned.
#[derive(Debug, Clone, PartialEq)]
pub struct MulticamAngle {
    pub media_id: MediaId,
    pub name: String,
    pub start_timecode: Option<String>,
    pub fps: Fps,
    pub duration_secs: f64,
    pub has_audio: bool,
}

/// Where one angle goes, and how that was decided.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnglePlacement {
    pub media_id: MediaId,
    /// Seconds from the group's first frame.
    pub start_secs: f64,
    /// `None` when the angle could not be synced and was put at the
    /// group's start for the editor to line up.
    pub method: Option<SyncMethod>,
    /// For an audio sync, how well the sound matched (0..1).
    pub confidence: Option<f64>,
    /// The sound the others were matched against.
    pub is_reference: bool,
}

/// What syncing a multicam found: a place for every angle, and what the
/// editor should know about it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MulticamPlan {
    pub placements: Vec<AnglePlacement>,
    pub notes: Vec<String>,
}

impl MulticamPlan {
    /// Angles actually put on the shared clock.
    pub fn synced(&self) -> usize {
        self.placements.iter().filter(|p| p.method.is_some()).count()
    }
}

/// The placed angles in groups whose spans overlap, directly or through
/// one another.
fn overlap_groups(start: &[Option<f64>], angles: &[MulticamAngle]) -> Vec<Vec<usize>> {
    let placed: Vec<usize> = (0..start.len()).filter(|&i| start[i].is_some()).collect();
    let span = |i: usize| {
        let s = start[i].unwrap_or(0.0);
        (s, s + angles[i].duration_secs)
    };
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for &i in &placed {
        let (a0, a1) = span(i);
        let touching: Vec<usize> = (0..groups.len())
            .filter(|&g| groups[g].iter().any(|&j| {
                let (b0, b1) = span(j);
                a0 < b1 && b0 < a1
            }))
            .collect();
        let mut merged = vec![i];
        for &g in touching.iter().rev() {
            merged.extend(groups.remove(g));
        }
        merged.sort_unstable();
        groups.push(merged);
    }
    groups
}

/// A match weaker than this is two recordings that share no sound.
pub const MIN_AUDIO_CONFIDENCE: f64 = 0.3;

/// Sound and timecode disagreeing by more than this means the timecode
/// is not on the event's clock.
pub const TIMECODE_DISAGREES_SECS: f64 = 0.5;

/// How far outside the synced angles a timecode may fall back to and
/// still be believed: a long day's pause between setups, not a
/// different clock.
pub const MAX_TIMECODE_FALLBACK_GAP_SECS: f64 = 2.0 * 3600.0;

/// How sure the sound must be to move an angle its timecode placed.
/// Jam-synced timecode is right far more often than a sound match on
/// repeated music or a PA loop, so a match that would place an angle
/// with no timecode is only reported against one that has it.
pub const OVERRULE_TIMECODE_CONFIDENCE: f64 = 0.7;

/// What matching one angle's sound against the reference found.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioMatch {
    /// A moment `t` into the reference is `t + offset_secs` into the angle.
    Matched { offset_secs: f64, confidence: f64 },
    /// They share less sound than a match needs.
    TooLittleOverlap,
    /// The sound could not be read.
    Unavailable(String),
}

/// Put `angles` on one clock.
///
/// Timecode first where `method` allows it and the clip carries one.
/// Timecode is only trusted where the clips agree: the angles whose
/// timecode spans overlap form groups, and those outside the largest are
/// on another clock — a DSLR counting from zero beside cameras on time of
/// day. Then sound: the reference is the longest angle with sound,
/// preferring one placed by trusted timecode, and `audio_offset(reference,
/// i)` matches angle `i` against it. With [`MulticamSync::TimecodeThenAudio`]
/// the sound also checks every timecode placement, and moves an angle
/// whose timecode it contradicts (two record-run cameras both starting at
/// zero overlap each other, so only the sound can tell). An angle neither
/// method can place goes at the group's start and is named in the notes.
pub fn plan_multicam(
    angles: &[MulticamAngle],
    method: MulticamSync,
    mut audio_offset: impl FnMut(usize, usize) -> AudioMatch,
) -> MulticamPlan {
    let n = angles.len();
    let mut start: Vec<Option<f64>> = vec![None; n];
    // Where timecode alone put each angle, for one set aside as another
    // clock that the sound then cannot place either.
    let mut tc_start: Vec<Option<f64>> = vec![None; n];
    let mut how: Vec<Option<SyncMethod>> = vec![None; n];
    let mut confidence: Vec<Option<f64>> = vec![None; n];
    let mut notes = Vec::new();

    if method != MulticamSync::Audio {
        for (i, a) in angles.iter().enumerate() {
            if let Some(t) = a.start_timecode.as_deref().and_then(|tc| timecode_to_secs(tc, a.fps)) {
                start[i] = Some(t);
                how[i] = Some(SyncMethod::Timecode);
            }
        }
        let missing: Vec<&str> = angles.iter().zip(&start).filter(|(_, s)| s.is_none()).map(|(a, _)| a.name.as_str()).collect();
        if method == MulticamSync::Timecode && !missing.is_empty() {
            notes.push(format!("No timecode on {}", missing.join(", ")));
        }
        // Trust the largest group of angles whose timecodes agree.
        tc_start = start.clone();
        let groups = overlap_groups(&start, angles);
        if groups.len() > 1 {
            let main = groups
                .iter()
                .max_by(|a, b| {
                    a.len().cmp(&b.len()).then_with(|| {
                        let total = |g: &Vec<usize>| g.iter().map(|&i| angles[i].duration_secs).sum::<f64>();
                        total(a).total_cmp(&total(b))
                    })
                })
                .cloned()
                .unwrap_or_default();
            for g in groups.iter().filter(|g| **g != main) {
                for &i in g {
                    if method == MulticamSync::TimecodeThenAudio {
                        notes.push(format!(
                            "{}'s timecode is on another clock from the other angles, so it was synced by sound",
                            angles[i].name
                        ));
                        start[i] = None;
                        how[i] = None;
                    } else {
                        notes.push(format!(
                            "{} shares no time with the other angles; its timecode may be on a different clock",
                            angles[i].name
                        ));
                    }
                }
            }
        }
    }

    let mut reference = None;
    let wants_sound = match method {
        MulticamSync::Timecode => false,
        MulticamSync::Audio => true,
        // Also to check the timecode placements.
        MulticamSync::TimecodeThenAudio => true,
    };
    if wants_sound {
        let longest = |only_placed: bool| {
            (0..n)
                .filter(|&i| angles[i].has_audio && (!only_placed || start[i].is_some()))
                .max_by(|&x, &y| angles[x].duration_secs.total_cmp(&angles[y].duration_secs))
        };
        reference = longest(true).or_else(|| longest(false));
        match reference {
            None if start.iter().any(Option::is_none) => notes.push("None of the clips has sound to sync by".to_string()),
            None => {}
            Some(r) => {
                if start[r].is_none() {
                    start[r] = Some(0.0);
                    how[r] = Some(SyncMethod::Waveform);
                }
                let r_start = start[r].unwrap_or(0.0);
                for i in (0..n).filter(|&i| i != r) {
                    let name = &angles[i].name;
                    let checking = start[i].is_some();
                    if checking && method != MulticamSync::TimecodeThenAudio {
                        continue;
                    }
                    if !angles[i].has_audio {
                        if !checking {
                            notes.push(format!("{name} has no timecode and no sound to sync by"));
                        }
                        continue;
                    }
                    match audio_offset(r, i) {
                        AudioMatch::Matched { offset_secs, confidence: c } if c >= MIN_AUDIO_CONFIDENCE => {
                            let by_sound = r_start - offset_secs;
                            if let Some(by_tc) = start[i].filter(|_| checking) {
                                if (by_tc - by_sound).abs() <= TIMECODE_DISAGREES_SECS {
                                    continue;
                                }
                                if c < OVERRULE_TIMECODE_CONFIDENCE {
                                    notes.push(format!(
                                        "{name}'s sound suggests {:.1} s from its timecode (match {:.0}%); kept its timecode — check it",
                                        by_sound - by_tc,
                                        c * 100.0
                                    ));
                                    continue;
                                }
                                notes.push(format!(
                                    "{name}'s timecode disagreed with its sound by {:.1} s, so it was placed by sound",
                                    by_tc - by_sound
                                ));
                            }
                            start[i] = Some(by_sound);
                            how[i] = Some(SyncMethod::Waveform);
                            confidence[i] = Some(c);
                        }
                        // A timecode placement the sound cannot confirm stands.
                        _ if checking => {}
                        AudioMatch::Matched { confidence: c, .. } => notes.push(format!(
                            "{name} shares no clear sound with {} (match {:.0}%)",
                            angles[r].name,
                            c * 100.0
                        )),
                        AudioMatch::TooLittleOverlap => {
                            notes.push(format!("{name} overlaps {} by too little to sync by sound", angles[r].name))
                        }
                        AudioMatch::Unavailable(why) => notes.push(format!("{name}'s sound could not be read: {why}")),
                    }
                }
            }
        }
    }

    // An angle set aside as another clock that the sound could not place
    // either keeps its timecode when that is near the others: same-clock
    // angles can simply not overlap, and then the timecode beats a guess.
    // Hours away it is a different clock after all — a record-run camera
    // at zero beside time-of-day ones — and would stretch the timeline
    // across the gap, so it goes to the start for the editor instead.
    let placed_span = (0..n)
        .filter_map(|i| start[i].map(|s| (s, s + angles[i].duration_secs)))
        .fold(None, |acc: Option<(f64, f64)>, (a, b)| Some(acc.map_or((a, b), |(x, y)| (x.min(a), y.max(b)))));
    for i in 0..n {
        if start[i].is_some() {
            continue;
        }
        let Some(t) = tc_start[i] else { continue };
        let gap = placed_span.map_or(0.0, |(from, to)| (from - (t + angles[i].duration_secs)).max(t - to).max(0.0));
        if gap <= MAX_TIMECODE_FALLBACK_GAP_SECS {
            start[i] = Some(t);
            how[i] = Some(SyncMethod::Timecode);
            notes.push(format!("{} kept its timecode, which the sound could not confirm", angles[i].name));
        } else {
            notes.push(format!(
                "{}'s timecode is {:.1} hours from the other angles, too far to trust",
                angles[i].name,
                gap / 3600.0
            ));
        }
    }

    let origin = start.iter().flatten().copied().fold(f64::INFINITY, f64::min);
    let origin = if origin.is_finite() { origin } else { 0.0 };
    let unsynced: Vec<&str> = angles.iter().zip(&start).filter(|(_, s)| s.is_none()).map(|(a, _)| a.name.as_str()).collect();
    if !unsynced.is_empty() {
        notes.push(format!("Placed at the start for you to line up: {}", unsynced.join(", ")));
    }

    let placements = angles
        .iter()
        .enumerate()
        .map(|(i, a)| AnglePlacement {
            media_id: a.media_id,
            start_secs: start[i].map_or(0.0, |s| s - origin),
            method: how[i],
            confidence: confidence[i],
            is_reference: reference == Some(i),
        })
        .collect();
    MulticamPlan { placements, notes }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timecode_reads_as_seconds_at_its_rate_and_drop_frame_stays_on_the_clock() {
        assert_eq!(timecode_to_secs("00:00:01:00", Fps::new(25, 1)), Some(1.0));
        assert_eq!(timecode_to_secs("01:02:03:04", Fps::new(25, 1)), Some(3723.0 + 4.0 / 25.0));
        assert_eq!(timecode_to_secs("00:00:10:05", Fps::new(24, 1)), Some(10.0 + 5.0 / 24.0));
        // Non-drop 29.97 runs slow against the clock; drop-frame keeps up.
        let ndf = timecode_to_secs("01:00:00:00", Fps::new(30000, 1001)).unwrap();
        assert!((ndf - 3603.6).abs() < 0.01, "{ndf}");
        let df = timecode_to_secs("01:00:00;00", Fps::new(30000, 1001)).unwrap();
        assert!((df - 3600.0).abs() < 0.05, "{df}");
        assert_eq!(timecode_to_secs("00:00:00:30", Fps::new(25, 1)), None, "frame 30 does not exist at 25");
        assert_eq!(timecode_to_secs("garbage", Fps::new(25, 1)), None);
        let s = SyncedAudio { audio_media_id: MediaId::new(), offset_secs: 1.2345, method: SyncMethod::Timecode };
        assert_eq!(s.describe(), "+1.234 s by timecode");
    }

    #[test]
    fn drop_frame_only_exists_at_the_ntsc_rates() {
        assert_eq!(drop_frames_per_minute(Fps::new(30000, 1001)), 2);
        assert_eq!(drop_frames_per_minute(Fps::new(60000, 1001)), 4);
        for r in [Fps::new(24, 1), Fps::new(25, 1), Fps::new(30, 1), Fps::new(50, 1), Fps::new(24000, 1001)] {
            assert_eq!(drop_frames_per_minute(r), 0, "{r:?} has no drop-frame");
            assert!(!is_drop_frame_rate(r));
        }
        assert!(is_drop_frame_rate(Fps::new(30000, 1001)));
    }

    #[test]
    fn the_canonical_drop_frame_boundaries() {
        let ntsc = Fps::new(30000, 1001);
        // The three values every naive implementation gets wrong. The
        // first minute drops ;00 and ;01, so frame 1800 is ;02 — and
        // the tenth minute drops nothing, so 17982 is exactly 10:00;00.
        assert_eq!(frames_to_timecode(1799, ntsc, true), (0, 0, 59, 29));
        assert_eq!(frames_to_timecode(1800, ntsc, true), (0, 1, 0, 2));
        assert_eq!(frames_to_timecode(17982, ntsc, true), (0, 10, 0, 0));
        assert_eq!(frames_to_timecode(107892, ntsc, true), (1, 0, 0, 0));
        // Non-drop counts every number, so the same frame reads earlier.
        assert_eq!(frames_to_timecode(107892, ntsc, false), (0, 59, 56, 12));
        // 59.94 drops four a minute.
        let ntsc60 = Fps::new(60000, 1001);
        assert_eq!(frames_to_timecode(3600, ntsc60, true), (0, 1, 0, 4));
        assert_eq!(frames_to_timecode(35964, ntsc60, true), (0, 10, 0, 0));
        // A rate with nothing to drop ignores the request.
        assert_eq!(frames_to_timecode(1500, Fps::new(25, 1), true), (0, 1, 0, 0));
    }

    #[test]
    fn forward_and_back_agree_at_every_minute_of_an_hour() {
        // The round trip is the real guard: `timecode_to_secs` was here
        // first and is the reference. Walking the minute boundaries
        // covers every skip and the tenth-minute exception nine times.
        for (fps, nominal) in [(Fps::new(30000, 1001), 30i64), (Fps::new(60000, 1001), 60i64)] {
            for minute in 0..60 {
                for offset in [0, 1, 2, 3] {
                    let frames = minute * nominal * 60 + offset;
                    let (h, m, s, f) = frames_to_timecode(frames, fps, true);
                    let tc = format!("{h:02}:{m:02}:{s:02};{f:02}");
                    let secs = timecode_to_secs(&tc, fps)
                        .unwrap_or_else(|| panic!("{tc} at {fps:?} should parse"));
                    let back = (secs * fps.as_f64()).round() as i64;
                    assert_eq!(back, frames, "{tc} at {fps:?} came back as frame {back}, not {frames}");
                }
            }
        }
    }

    fn angle(name: &str, tc: Option<&str>, secs: f64, audio: bool) -> MulticamAngle {
        MulticamAngle {
            media_id: MediaId::new(),
            name: name.into(),
            start_timecode: tc.map(Into::into),
            fps: Fps::new(25, 1),
            duration_secs: secs,
            has_audio: audio,
        }
    }

    #[test]
    fn jam_synced_cameras_are_placed_by_timecode() {
        let angles = [
            angle("A", Some("10:00:05:00"), 60.0, true),
            angle("B", Some("10:00:00:00"), 60.0, true),
            angle("C", Some("10:00:12:12"), 30.0, false),
        ];
        let plan = plan_multicam(&angles, MulticamSync::Timecode, |_, _| panic!("no sound needed"));
        let starts: Vec<f64> = plan.placements.iter().map(|p| p.start_secs).collect();
        for (got, want) in starts.iter().zip([5.0, 0.0, 12.48]) {
            assert!((got - want).abs() < 1e-6, "{starts:?}");
        }
        assert!(plan.placements.iter().all(|p| p.method == Some(SyncMethod::Timecode)));
        assert!(plan.notes.is_empty(), "{:?}", plan.notes);
    }

    #[test]
    fn half_jam_synced_falls_back_to_sound_on_the_same_clock() {
        let angles = [
            angle("A", Some("10:00:05:00"), 60.0, true),
            angle("B", Some("10:00:00:00"), 90.0, true),
            angle("phone", None, 40.0, true),
        ];
        // B is the reference (longest with timecode); the phone's sound
        // matches 20 s into B.
        // A's sound agrees with its timecode (5 s after B).
        let plan = plan_multicam(&angles, MulticamSync::TimecodeThenAudio, |r, i| match (r, i) {
            (1, 2) => matched(-20.0, 0.8),
            (1, 0) => matched(-5.0, 0.9),
            other => panic!("unexpected pair {other:?}"),
        });
        assert_eq!(plan.placements[0].method, Some(SyncMethod::Timecode));
        assert!(plan.notes.is_empty(), "{:?}", plan.notes);
        let phone = &plan.placements[2];
        assert_eq!(phone.method, Some(SyncMethod::Waveform));
        assert!((phone.start_secs - 20.0).abs() < 1e-9);
        assert!(plan.placements[1].is_reference);
        assert_eq!(plan.synced(), 3);
    }

    #[test]
    fn an_angle_with_no_shared_sound_is_named_not_guessed() {
        let angles = [angle("A", None, 60.0, true), angle("B", None, 30.0, true)];
        let plan = plan_multicam(&angles, MulticamSync::Audio, |_, _| matched(5.0, 0.1));
        assert_eq!(plan.placements[1].method, None);
        assert!(plan.notes.iter().any(|n| n.contains("B shares no clear sound")), "{:?}", plan.notes);
        assert!(plan.notes.iter().any(|n| n.contains("line up: B")), "{:?}", plan.notes);
        assert_eq!(plan.synced(), 1);
    }

    #[test]
    fn a_camera_on_another_clock_is_flagged() {
        // Record-run timecode on one camera, time of day on the other.
        let angles = [angle("A", Some("00:00:10:00"), 60.0, false), angle("B", Some("14:22:00:00"), 60.0, false)];
        let plan = plan_multicam(&angles, MulticamSync::Timecode, |_, _| AudioMatch::TooLittleOverlap);
        assert!(plan.notes.iter().any(|n| n.contains("different clock")), "{:?}", plan.notes);
    }

    fn matched(offset_secs: f64, confidence: f64) -> AudioMatch {
        AudioMatch::Matched { offset_secs, confidence }
    }

    #[test]
    fn a_record_run_camera_beside_time_of_day_ones_is_synced_by_sound() {
        let angles = [
            angle("A", Some("14:00:00:00"), 600.0, true),
            angle("B", Some("14:00:30:00"), 500.0, true),
            angle("dslr", Some("00:00:00:00"), 120.0, true),
        ];
        // The DSLR's sound starts 100 s into A.
        let plan = plan_multicam(&angles, MulticamSync::TimecodeThenAudio, |r, i| match (r, i) {
            (0, 1) => matched(-30.0, 0.9),
            (0, 2) => matched(-100.0, 0.9),
            other => panic!("unexpected pair {other:?}"),
        });
        let dslr = &plan.placements[2];
        assert_eq!(dslr.method, Some(SyncMethod::Waveform));
        assert!((dslr.start_secs - 100.0).abs() < 1e-6, "{plan:?}");
        assert!(plan.notes.iter().any(|n| n.contains("dslr's timecode is on another clock")), "{:?}", plan.notes);
    }

    #[test]
    fn two_record_run_cameras_both_at_zero_are_corrected_by_sound() {
        // They overlap each other, so timecode alone looks consistent.
        let angles = [angle("A", Some("00:00:00:00"), 300.0, true), angle("B", Some("00:00:00:00"), 200.0, true)];
        let plan = plan_multicam(&angles, MulticamSync::TimecodeThenAudio, |_, _| matched(-42.0, 0.8));
        assert!((plan.placements[1].start_secs - 42.0).abs() < 1e-6, "{plan:?}");
        assert!(plan.notes.iter().any(|n| n.contains("disagreed with its sound by -42.0 s")), "{:?}", plan.notes);
        // Timecode only has nothing to check against, and says nothing.
        let tc_only = plan_multicam(&angles, MulticamSync::Timecode, |_, _| panic!("no sound"));
        assert_eq!(tc_only.placements[1].start_secs, 0.0);
    }

    #[test]
    fn unreadable_sound_is_reported_as_such() {
        let angles = [angle("A", None, 60.0, true), angle("B", None, 60.0, true)];
        let plan = plan_multicam(&angles, MulticamSync::Audio, |_, _| AudioMatch::Unavailable("ffmpeg failed".into()));
        assert!(plan.notes.iter().any(|n| n.contains("'s sound could not be read: ffmpeg failed")), "{:?}", plan.notes);
    }

    #[test]
    fn a_middling_sound_match_does_not_move_jammed_timecode() {
        let angles = [angle("A", Some("10:00:00:00"), 300.0, true), angle("B", Some("10:00:10:00"), 200.0, true)];
        // A PA loop: a match, but not a sure one, 30 s from the timecode.
        let plan = plan_multicam(&angles, MulticamSync::TimecodeThenAudio, |_, _| matched(-40.0, 0.5));
        assert_eq!(plan.placements[1].method, Some(SyncMethod::Timecode));
        assert!((plan.placements[1].start_secs - 10.0).abs() < 1e-6);
        assert!(plan.notes.iter().any(|n| n.contains("kept its timecode")), "{:?}", plan.notes);
    }

    #[test]
    fn a_same_clock_angle_that_overlaps_nothing_keeps_its_timecode_when_sound_cannot_help() {
        let angles = [
            angle("A", Some("10:00:00:00"), 90.0, true),
            angle("B", Some("10:00:30:00"), 60.0, true),
            angle("C", Some("11:00:00:00"), 60.0, true),
        ];
        let plan = plan_multicam(&angles, MulticamSync::TimecodeThenAudio, |r, i| match (r, i) {
            (_, 2) | (2, _) => AudioMatch::TooLittleOverlap,
            _ => matched(-30.0, 0.9),
        });
        let c = &plan.placements[2];
        assert_eq!(c.method, Some(SyncMethod::Timecode));
        assert!((c.start_secs - 3600.0).abs() < 1e-6, "{plan:?}");
    }

    #[test]
    fn a_record_run_angle_the_sound_cannot_place_goes_to_the_start_not_hours_away() {
        let angles = [
            angle("A", Some("14:00:00:00"), 600.0, true),
            angle("B", Some("14:00:30:00"), 500.0, true),
            angle("dslr", Some("00:00:00:00"), 120.0, true),
        ];
        let plan = plan_multicam(&angles, MulticamSync::TimecodeThenAudio, |_, i| match i {
            2 => AudioMatch::Unavailable("no audio stream".into()),
            _ => matched(-30.0, 0.9),
        });
        let dslr = &plan.placements[2];
        assert_eq!(dslr.method, None);
        assert_eq!(dslr.start_secs, 0.0);
        assert!(plan.notes.iter().any(|n| n.contains("hours from the other angles")), "{:?}", plan.notes);
        // And the group itself was not stretched across fourteen hours.
        assert!(plan.placements.iter().all(|p| p.start_secs < 3600.0), "{plan:?}");
    }
}
