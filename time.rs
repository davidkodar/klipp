use serde::{Deserialize, Serialize};

/// Frames per second — stored as a rational number to handle
/// 23.976, 29.97, 59.94 etc without floating point imprecision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fps {
    pub numerator: u32,
    pub denominator: u32,
}

impl Fps {
    pub const F23_976: Self = Self { numerator: 24000, denominator: 1001 };
    pub const F24:     Self = Self { numerator: 24,    denominator: 1 };
    pub const F25:     Self = Self { numerator: 25,    denominator: 1 };
    pub const F29_97:  Self = Self { numerator: 30000, denominator: 1001 };
    pub const F30:     Self = Self { numerator: 30,    denominator: 1 };
    pub const F50:     Self = Self { numerator: 50,    denominator: 1 };
    pub const F59_94:  Self = Self { numerator: 60000, denominator: 1001 };
    pub const F60:     Self = Self { numerator: 60,    denominator: 1 };

    pub fn as_f64(&self) -> f64 {
        self.numerator as f64 / self.denominator as f64
    }

    /// Convert a frame number to seconds
    pub fn frames_to_seconds(&self, frames: i64) -> f64 {
        frames as f64 * self.denominator as f64 / self.numerator as f64
    }

    /// Convert seconds to the nearest frame number
    pub fn seconds_to_frames(&self, seconds: f64) -> i64 {
        (seconds * self.numerator as f64 / self.denominator as f64).round() as i64
    }
}

/// A position on the timeline expressed in frames.
/// Using i64 to support negative values (e.g. pre-roll before zero).
/// At 60fps, i64 gives us ~9.7 billion years of timeline — sufficient.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Timecode(pub i64);

impl Timecode {
    pub const ZERO: Self = Self(0);

    pub fn from_frames(frames: i64) -> Self {
        Self(frames)
    }

    pub fn from_seconds(seconds: f64, fps: Fps) -> Self {
        Self(fps.seconds_to_frames(seconds))
    }

    pub fn as_frames(&self) -> i64 {
        self.0
    }

    pub fn as_seconds(&self, fps: Fps) -> f64 {
        fps.frames_to_seconds(self.0)
    }

    pub fn to_hms_string(&self, fps: Fps) -> String {
        let total_frames = self.0.abs();
        let fps_round = fps.as_f64().round() as i64;
        let frames = total_frames % fps_round;
        let total_secs = total_frames / fps_round;
        let secs = total_secs % 60;
        let mins = (total_secs / 60) % 60;
        let hours = total_secs / 3600;
        let sign = if self.0 < 0 { "-" } else { "" };
        format!("{sign}{hours:02}:{mins:02}:{secs:02}:{frames:02}")
    }
}

impl std::ops::Add for Timecode {
    type Output = Self;
    fn add(self, rhs: Self) -> Self { Self(self.0 + rhs.0) }
}

impl std::ops::Sub for Timecode {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self { Self(self.0 - rhs.0) }
}

impl std::ops::Neg for Timecode {
    type Output = Self;
    fn neg(self) -> Self { Self(-self.0) }
}

/// An inclusive range on the timeline [start, end).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: Timecode,
    pub end: Timecode,
}

impl TimeRange {
    pub fn new(start: Timecode, end: Timecode) -> Self {
        debug_assert!(end >= start, "TimeRange end must be >= start");
        Self { start, end }
    }

    pub fn duration(&self) -> Timecode {
        self.end - self.start
    }

    pub fn contains(&self, tc: Timecode) -> bool {
        tc >= self.start && tc < self.end
    }

    pub fn overlaps(&self, other: &Self) -> bool {
        self.start < other.end && self.end > other.start
    }

    pub fn intersection(&self, other: &Self) -> Option<Self> {
        let start = self.start.max(other.start);
        let end = self.end.min(other.end);
        if end > start { Some(Self::new(start, end)) } else { None }
    }
}
