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
}
