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
    /// Zero time with rate=1. Arithmetic with it takes the other side's
    /// rate (#2450): `ZERO - t` and `ZERO + t` are `t`'s rate and exact,
    /// where they used to be rounded to whole seconds.
    /// A clip's timeline_in still takes its rate from its media.
    pub const ZERO: Self = Self { value: 0, rate: 1 };

    /// Returns zero time with rate=1.
    /// **Prefer `ZERO` const for clarity. Never use as clip timeline_in.**
    pub fn zero() -> Self {
        Self::ZERO
    }

    /// Absolute value — preserves rate, negates negative values.
    pub fn abs(self) -> Self {
        Self { value: self.value.abs(), rate: self.rate }
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

impl RationalTime {
    /// The rate a sum or difference with `other` is reckoned at: this
    /// one's, unless this is a zero at rate 1 ([`Self::ZERO`]), which has
    /// no rate of its own to keep and takes `other`'s. Reckoned at rate 1,
    /// `other` was rounded to whole seconds.
    fn rate_with(self, other: Self) -> u32 {
        if self.value == 0 && self.rate == 1 { other.rate } else { self.rate }
    }
}

impl Add for RationalTime {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        let rate = self.rate_with(rhs);
        Self {
            value: self.rescale_to(rate).value + rhs.rescale_to(rate).value,
            rate,
        }
    }
}

impl Sub for RationalTime {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        let rate = self.rate_with(rhs);
        Self {
            value: self.rescale_to(rate).value - rhs.rescale_to(rate).value,
            rate,
        }
    }
}

impl AddAssign for RationalTime {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl SubAssign for RationalTime {
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
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

/// How a sequence shows a time on its clock, and reads one back.
///
/// # Positions and durations are not the same thing
///
/// Drop-frame skips frame *numbers* so a running count keeps up with the
/// wall clock. That applies to a **position** — where the playhead is,
/// where a clip starts, where a marker sits — because those are read
/// against a clock.
///
/// A **duration** is a count of frames, and counting frames is not
/// something drop-frame does. Two seconds of 29.97 is 60 frames whether
/// the sequence is drop-frame or not, and showing `00:00:02;00` for it
/// would imply a convention that does not apply. So durations are always
/// shown plainly, at the nominal rate, which is what Avid and Resolve do
/// and what [`Self::duration`] does here.
///
/// Getting this backwards is subtle and wrong in a way nobody notices:
/// the numbers stay plausible.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimecodeDisplay {
    pub fps: Fps,
    /// Honoured only at a rate that has drop-frame at all.
    pub drop: bool,
}

impl TimecodeDisplay {
    /// Non-drop at `fps` — for anything with no sequence behind it.
    pub fn plain(fps: Fps) -> Self {
        Self { fps, drop: false }
    }

    /// Whether this display actually counts drop-frame: the flag, and a
    /// rate that has it.
    pub fn is_drop(&self) -> bool {
        self.drop && crate::sync::is_drop_frame_rate(self.fps)
    }

    /// A position on the clock, `hh:mm:ss:ff` — or `hh:mm:ss;ff` when
    /// drop-frame, which is how every application marks it.
    pub fn position(&self, t: RationalTime) -> String {
        self.position_secs(t.to_secs_f64())
    }

    /// A position given in seconds.
    pub fn position_secs(&self, secs: f64) -> String {
        let drop = self.is_drop();
        let frames = (secs.abs() * self.fps.as_f64()).round() as i64;
        let (h, m, s, f) = crate::sync::frames_to_timecode(frames, self.fps, drop);
        let sep = if drop { ';' } else { ':' };
        let sign = if secs < 0.0 { "-" } else { "" };
        format!("{sign}{h:02}:{m:02}:{s:02}{sep}{f:02}")
    }

    /// A length, counted plainly — never drop-frame. See the type's docs.
    pub fn duration(&self, t: RationalTime) -> String {
        self.duration_secs(t.to_secs_f64())
    }

    /// A length given in seconds.
    pub fn duration_secs(&self, secs: f64) -> String {
        let frames = (secs.abs() * self.fps.as_f64()).round() as i64;
        let (h, m, s, f) = crate::sync::frames_to_timecode(frames, self.fps, false);
        let sign = if secs < 0.0 { "-" } else { "" };
        format!("{sign}{h:02}:{m:02}:{s:02}:{f:02}")
    }

    /// Read a typed position back, in either spelling.
    ///
    /// A semicolon means drop-frame whatever the sequence is set to, so
    /// a timecode copied off a camera report or a spotting list can be
    /// pasted in as written. Colons are read in the sequence's own mode,
    /// because that is what the app itself displays.
    pub fn parse(&self, text: &str) -> Option<RationalTime> {
        let text = text.trim();
        let marked_drop = text.contains(';');
        let secs = if marked_drop || !self.is_drop() {
            crate::sync::timecode_to_secs(text, self.fps)?
        } else {
            // Colons, on a drop-frame sequence: re-spell it so the
            // parser counts it the way the display wrote it.
            let mut owned = text.to_string();
            if let Some(i) = owned.rfind(':') {
                owned.replace_range(i..i + 1, ";");
            }
            crate::sync::timecode_to_secs(&owned, self.fps)?
        };
        Some(RationalTime::from_secs_f64(secs, self.fps.rate()))
    }
}

#[cfg(test)]
mod timecode_display_tests {
    use super::*;

    fn ntsc() -> TimecodeDisplay {
        TimecodeDisplay { fps: Fps::new(30000, 1001), drop: true }
    }

    #[test]
    fn a_position_counts_drop_frame_and_says_so_with_a_semicolon() {
        let d = ntsc();
        // Frame 107_892 is the canonical hour in drop-frame; counted
        // non-drop the same frame reads 00:59:56:12.
        let hour = RationalTime { value: 107_892 * 1001, rate: 30_000 };
        assert_eq!(d.position(hour), "01:00:00;00");
        let ndf = TimecodeDisplay { drop: false, ..d };
        assert_eq!(ndf.position(hour), "00:59:56:12");
    }

    #[test]
    fn a_duration_is_never_drop_frame() {
        // The distinction this type exists for. Two seconds is 60 frames
        // at 29.97 whichever way the sequence counts, and drop-frame has
        // nothing to say about a length.
        let d = ntsc();
        let two_secs = RationalTime { value: 60 * 1001, rate: 30_000 };
        assert_eq!(d.duration(two_secs), "00:00:02:00");
        // An hour of frames is an hour of frames, not the clock's hour.
        let hour_of_frames = RationalTime { value: 107_892 * 1001, rate: 30_000 };
        assert_eq!(d.duration(hour_of_frames), "00:59:56:12");
    }

    #[test]
    fn drop_frame_is_ignored_at_a_rate_that_has_none() {
        let d = TimecodeDisplay { fps: Fps::new(25, 1), drop: true };
        assert!(!d.is_drop(), "25 has nothing to drop");
        let t = RationalTime { value: 25 * 60, rate: 25 };
        assert_eq!(d.position(t), "00:01:00:00");
    }

    #[test]
    fn what_is_shown_can_be_typed_back() {
        let d = ntsc();
        for frames in [0i64, 1799, 1800, 17_982, 107_892] {
            let t = RationalTime { value: frames * 1001, rate: 30_000 };
            let shown = d.position(t);
            let back = d.parse(&shown).unwrap_or_else(|| panic!("{shown} should parse"));
            let back_frames = (back.to_secs_f64() * d.fps.as_f64()).round() as i64;
            assert_eq!(back_frames, frames, "{shown} came back as frame {back_frames}");
        }
    }

    #[test]
    fn a_colon_timecode_typed_into_a_drop_frame_sequence_means_what_is_displayed() {
        // Someone reading the viewer and typing it back without the
        // semicolon means the position they can see.
        let d = ntsc();
        let with = d.parse("01:00:00;00").expect("semicolon");
        let without = d.parse("01:00:00:00").expect("colon");
        assert_eq!(with, without);
    }

    #[test]
    fn a_semicolon_is_honoured_on_a_non_drop_sequence() {
        // Pasted off a camera report: it says what it is, and is read
        // that way rather than being silently recounted.
        let ndf = TimecodeDisplay { fps: Fps::new(30000, 1001), drop: false };
        let df = ndf.parse("01:00:00;00").expect("semicolon");
        let plain = ndf.parse("01:00:00:00").expect("colon");
        assert_ne!(df, plain);
    }
}

#[cfg(test)]
mod zero_tests {
    use super::*;

    /// #2450: a sum or difference with `ZERO` is exact, at the other
    /// side's rate, whichever side the zero is on; a zero at another rate
    /// keeps its own, as any time does.
    #[test]
    fn arithmetic_with_zero_keeps_the_other_sides_rate_and_value() {
        let t = RationalTime { value: 1_234_567, rate: 48_000 };
        let neg = RationalTime { value: -1_234_567, rate: 48_000 };
        assert_eq!(RationalTime::ZERO - t, neg);
        assert_eq!(t - RationalTime::ZERO, t);
        assert_eq!(RationalTime::ZERO + t, t);
        assert_eq!(t + RationalTime::ZERO, t);
        let mut sum = RationalTime::ZERO;
        sum += t;
        sum += t;
        assert_eq!(sum, RationalTime { value: 2_469_134, rate: 48_000 }, "an accumulator from zero");
        let mut back = RationalTime::zero();
        back -= t;
        assert_eq!(back, neg);
        assert_eq!(RationalTime::ZERO + RationalTime::ZERO, RationalTime::ZERO);
        // A zero at a frame rate is a time on that grid, and stays on it.
        let frame_zero = RationalTime { value: 0, rate: 24 };
        assert_eq!((frame_zero + RationalTime { value: 48, rate: 24 }).rate, 24);
        // Comparisons were exact already.
        assert!(RationalTime::ZERO < t && neg < RationalTime::ZERO);
        assert_eq!(RationalTime::ZERO.cmp(&frame_zero), std::cmp::Ordering::Equal);
        assert_eq!(RationalTime::ZERO.max(t), t);
    }
}
