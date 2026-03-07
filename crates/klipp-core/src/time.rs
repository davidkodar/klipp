use serde::{Deserialize, Serialize};
use std::ops::{Add, AddAssign, Sub, SubAssign};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fps {
    pub num: u32,
    pub den: u32,
}

impl Fps {
    pub fn new(num: u32, den: u32) -> Self {
        Self { num, den }
    }

    pub fn as_f64(&self) -> f64 {
        self.num as f64 / self.den as f64
    }

    pub fn frame_duration_secs(&self) -> f64 {
        1.0 / self.as_f64()
    }

    pub fn rate(&self) -> u32 {
        // For integer fps (den=1), rate is just num.
        // For fractional fps like 24000/1001, use num as the rate
        // to preserve frame-accurate timing.
        self.num
    }

    pub fn ntsc_23976() -> Self {
        Self { num: 24000, den: 1001 }
    }

    pub fn ntsc_29970() -> Self {
        Self { num: 30000, den: 1001 }
    }

    pub fn film_24() -> Self {
        Self { num: 24, den: 1 }
    }

    pub fn pal_25() -> Self {
        Self { num: 25, den: 1 }
    }
}

/// A time value stored as an exact rational number: value/rate seconds.
/// When rate=24 and value=48, that represents exactly 2.0 seconds.
/// When rate=16 and value=81, that represents 5.0625 seconds.
/// Times at different rates can be compared and combined via rescaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RationalTime {
    pub value: i64,
    pub rate: u32,
}

impl RationalTime {
    pub fn zero() -> Self {
        Self { value: 0, rate: 1 }
    }

    pub fn from_frames(frames: i64, rate: u32) -> Self {
        Self { value: frames, rate }
    }

    pub fn from_secs_f64(secs: f64, rate: u32) -> Self {
        Self {
            value: (secs * rate as f64).round() as i64,
            rate,
        }
    }

    pub fn to_secs_f64(&self) -> f64 {
        self.value as f64 / self.rate as f64
    }

    /// Rescale to a different rate, preserving the time value
    pub fn rescale_to(&self, new_rate: u32) -> Self {
        if self.rate == new_rate {
            return *self;
        }
        let new_value = (self.value as f64 * new_rate as f64 / self.rate as f64).round() as i64;
        Self {
            value: new_value,
            rate: new_rate,
        }
    }

    /// Format as HH:MM:SS:FF at the stored rate
    pub fn to_timecode_string(&self) -> String {
        let total_frames = self.value.abs();
        let rate = self.rate.max(1) as i64;
        let ff = total_frames % rate;
        let total_secs = total_frames / rate;
        let ss = total_secs % 60;
        let mm = (total_secs / 60) % 60;
        let hh = total_secs / 3600;
        let sign = if self.value < 0 { "-" } else { "" };
        format!("{sign}{hh:02}:{mm:02}:{ss:02}:{ff:02}")
    }
}

impl Add for RationalTime {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        let rhs_rescaled = rhs.rescale_to(self.rate);
        Self {
            value: self.value + rhs_rescaled.value,
            rate: self.rate,
        }
    }
}

impl Sub for RationalTime {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        let rhs_rescaled = rhs.rescale_to(self.rate);
        Self {
            value: self.value - rhs_rescaled.value,
            rate: self.rate,
        }
    }
}

impl AddAssign for RationalTime {
    fn add_assign(&mut self, rhs: Self) {
        let rhs_rescaled = rhs.rescale_to(self.rate);
        self.value += rhs_rescaled.value;
    }
}

impl SubAssign for RationalTime {
    fn sub_assign(&mut self, rhs: Self) {
        let rhs_rescaled = rhs.rescale_to(self.rate);
        self.value -= rhs_rescaled.value;
    }
}

impl PartialOrd for RationalTime {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RationalTime {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Compare by cross-multiplying to avoid floating point
        let lhs = self.value as i128 * other.rate as i128;
        let rhs = other.value as i128 * self.rate as i128;
        lhs.cmp(&rhs)
    }
}

/// Type alias for backward compatibility during migration.
pub type Timecode = RationalTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: RationalTime,
    pub end: RationalTime,
}

impl TimeRange {
    pub fn new(start: RationalTime, end: RationalTime) -> Self {
        Self { start, end }
    }

    pub fn duration(&self) -> RationalTime {
        self.end - self.start
    }

    pub fn contains(&self, t: RationalTime) -> bool {
        t >= self.start && t < self.end
    }

    pub fn overlaps(&self, other: &TimeRange) -> bool {
        self.start < other.end && self.end > other.start
    }

    pub fn intersection(&self, other: &TimeRange) -> Option<TimeRange> {
        let start = if self.start >= other.start {
            self.start
        } else {
            other.start
        };
        let end = if self.end <= other.end {
            self.end
        } else {
            other.end
        };
        if end > start {
            Some(Self::new(start, end))
        } else {
            None
        }
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}
