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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Timecode(pub i64);

impl Timecode {
    pub fn zero() -> Self {
        Self(0)
    }

    pub fn from_secs(secs: f64, fps: &Fps) -> Self {
        Self((secs * fps.as_f64()).round() as i64)
    }

    pub fn to_secs(&self, fps: &Fps) -> f64 {
        self.0 as f64 / fps.as_f64()
    }

    pub fn to_hmsf(&self, fps: &Fps) -> (u32, u32, u32, u32) {
        let total_frames = self.0.unsigned_abs();
        let fps_round = fps.as_f64().round() as u64;
        let frames = (total_frames % fps_round) as u32;
        let total_secs = total_frames / fps_round;
        let secs = (total_secs % 60) as u32;
        let mins = ((total_secs / 60) % 60) as u32;
        let hours = (total_secs / 3600) as u32;
        (hours, mins, secs, frames)
    }

    pub fn format_hmsf(&self, fps: &Fps) -> String {
        let (h, m, s, f) = self.to_hmsf(fps);
        let sign = if self.0 < 0 { "-" } else { "" };
        format!("{sign}{h:02}:{m:02}:{s:02}:{f:02}")
    }
}

impl Add for Timecode {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl Sub for Timecode {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}

impl AddAssign for Timecode {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl SubAssign for Timecode {
    fn sub_assign(&mut self, rhs: Self) {
        self.0 -= rhs.0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: Timecode,
    pub end: Timecode,
}

impl TimeRange {
    pub fn new(start: Timecode, end: Timecode) -> Self {
        Self { start, end }
    }

    pub fn duration(&self) -> Timecode {
        self.end - self.start
    }

    pub fn contains(&self, t: Timecode) -> bool {
        t >= self.start && t < self.end
    }

    pub fn overlaps(&self, other: &TimeRange) -> bool {
        self.start < other.end && self.end > other.start
    }

    pub fn intersection(&self, other: &TimeRange) -> Option<TimeRange> {
        let start = self.start.max(other.start);
        let end = self.end.min(other.end);
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
