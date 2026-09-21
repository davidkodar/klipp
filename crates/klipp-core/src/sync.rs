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
}
