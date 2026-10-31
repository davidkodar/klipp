//! What the camera recorded about a shot beyond the picture: the start
//! timecode and reel, the lens and exposure, the gamma and gamut it was
//! shot in, and every tag the file carried.
//!
//! Filled at import from three places, in order of trust: the tags in
//! the container and its streams, a still's EXIF, and the sidecar XML a
//! Sony or Canon camera writes beside a clip. A field the first source
//! has is kept; a later source fills only what is still missing.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShotMetadata {
    /// The clip's first frame, as the camera counted it: "hh:mm:ss:ff",
    /// or "hh:mm:ss;ff" for drop-frame.
    pub start_timecode: Option<String>,
    /// The reel or card the clip was recorded on.
    pub reel_name: Option<String>,
    /// The lens, as the camera names it.
    pub lens: Option<String>,
    pub iso: Option<u32>,
    /// The exposure time as written, "1/50" or "0.02".
    pub shutter: Option<String>,
    /// The f-number.
    pub aperture: Option<f32>,
    pub focal_length_mm: Option<f32>,
    /// A colour temperature ("5600 K") or a mode ("Auto").
    pub white_balance: Option<String>,
    /// The gamma the camera recorded in, in its own words: "s-log3-cine".
    pub gamma: Option<String>,
    /// The colour gamut likewise: "s-gamut3-cine".
    pub colour_gamut: Option<String>,
    /// Where the lens and exposure came from: "EXIF" or "Sony XML".
    pub exposure_source: Option<String>,
    /// Every tag as written, keyed by where it came from: "format:",
    /// "video:", "audio:", "data:", "exif:", "sidecar:".
    pub tags: Vec<(String, String)>,
}

impl ShotMetadata {
    /// Nothing was recorded.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The exposure on one line: "ISO 800 · 1/50 · f/2.8 · 24 mm", with
    /// whatever parts are known. `None` when none are.
    pub fn exposure_summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(iso) = self.iso {
            parts.push(format!("ISO {iso}"));
        }
        if let Some(s) = &self.shutter {
            parts.push(s.clone());
        }
        if let Some(f) = self.aperture {
            parts.push(format!("f/{}", trim_float(f)));
        }
        if let Some(mm) = self.focal_length_mm {
            parts.push(format!("{} mm", trim_float(mm)));
        }
        if parts.is_empty() { None } else { Some(parts.join(" \u{00B7} ")) }
    }

    /// Take from `other` every field this one lacks, and all its tags.
    pub fn fill_missing_from(&mut self, other: ShotMetadata) {
        fn fill<T>(slot: &mut Option<T>, from: Option<T>) {
            if slot.is_none() {
                *slot = from;
            }
        }
        fill(&mut self.start_timecode, other.start_timecode);
        fill(&mut self.reel_name, other.reel_name);
        fill(&mut self.lens, other.lens);
        fill(&mut self.iso, other.iso);
        fill(&mut self.shutter, other.shutter);
        fill(&mut self.aperture, other.aperture);
        fill(&mut self.focal_length_mm, other.focal_length_mm);
        fill(&mut self.white_balance, other.white_balance);
        fill(&mut self.gamma, other.gamma);
        fill(&mut self.colour_gamut, other.colour_gamut);
        fill(&mut self.exposure_source, other.exposure_source);
        self.tags.extend(other.tags);
    }
}

/// A float with the digits it needs: "2.8", "24", "1.4".
fn trim_float(v: f32) -> String {
    let s = format!("{v:.1}");
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

/// Unix seconds of a civil date and time, taken as UTC. Camera clocks
/// carry no zone, so this is the time the camera showed.
pub fn civil_to_epoch(y: i64, m: u32, d: u32, h: u32, min: u32, s: u32) -> Option<u64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || min > 59 || s > 60 {
        return None;
    }
    // Days from civil, after Howard Hinnant.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h as i64 * 3600 + min as i64 * 60 + s as i64;
    u64::try_from(secs).ok()
}

/// The civil date and time of Unix seconds, in UTC: (year, month, day,
/// hour, minute). The inverse of [`civil_to_epoch`].
pub fn epoch_to_civil(secs: u64) -> (i64, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil from days, after Howard Hinnant.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, (rem / 3600) as u32, ((rem % 3600) / 60) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_seconds_round_trip_through_the_civil_date() {
        assert_eq!(epoch_to_civil(0), (1970, 1, 1, 0, 0));
        assert_eq!(epoch_to_civil(1_710_498_600), (2024, 3, 15, 10, 30));
        for secs in [951_782_400u64, 1_000_000_000, 1_704_067_199, 4_102_444_800] {
            let (y, m, d, h, mi) = epoch_to_civil(secs);
            assert_eq!(civil_to_epoch(y, m, d, h, mi, (secs % 60) as u32), Some(secs));
        }
    }

    #[test]
    fn the_exposure_reads_on_one_line_from_what_is_known() {
        let mut shot = ShotMetadata { iso: Some(800), shutter: Some("1/50".into()), aperture: Some(2.8), focal_length_mm: Some(24.0), ..Default::default() };
        assert_eq!(shot.exposure_summary().as_deref(), Some("ISO 800 \u{00B7} 1/50 \u{00B7} f/2.8 \u{00B7} 24 mm"));
        shot.iso = None;
        shot.focal_length_mm = None;
        assert_eq!(shot.exposure_summary().as_deref(), Some("1/50 \u{00B7} f/2.8"));
        assert_eq!(ShotMetadata::default().exposure_summary(), None);
        assert!(ShotMetadata::default().is_empty());
    }

    #[test]
    fn a_later_source_fills_only_what_is_missing() {
        let mut shot = ShotMetadata { iso: Some(100), tags: vec![("format:a".into(), "1".into())], ..Default::default() };
        shot.fill_missing_from(ShotMetadata { iso: Some(800), lens: Some("50mm".into()), tags: vec![("exif:b".into(), "2".into())], ..Default::default() });
        assert_eq!(shot.iso, Some(100));
        assert_eq!(shot.lens.as_deref(), Some("50mm"));
        assert_eq!(shot.tags.len(), 2);
    }

    #[test]
    fn a_camera_clock_reading_becomes_unix_seconds() {
        assert_eq!(civil_to_epoch(1970, 1, 1, 0, 0, 0), Some(0));
        assert_eq!(civil_to_epoch(2024, 3, 15, 10, 30, 0), Some(1_710_498_600));
        assert_eq!(civil_to_epoch(2024, 13, 1, 0, 0, 0), None);
    }
}

/// Where a file's picture and its sound end (#3230), in seconds from the
/// first of its streams to start: either can stop short of the container,
/// which is as long as its longest stream. `None` where the probe couldn't
/// tell; the container's length stands in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamEnds {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sound: Option<f64>,
}

impl StreamEnds {
    pub fn is_unknown(&self) -> bool {
        *self == Self::default()
    }
}
