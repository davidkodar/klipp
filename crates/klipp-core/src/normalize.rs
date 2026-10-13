//! Normalizing clips to a loudness or peak target (#1843): the settings
//! the dialog, the socket and the preferences share, and the gain each
//! clip takes from what was measured of it. Measuring is
//! `klipp_render::normalize`'s; applying is a clip's `gain`, so the
//! source media is never rewritten.

use serde::{Deserialize, Serialize};

/// What a clip is brought to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NormalizeMode {
    /// Integrated loudness, ITU-R BS.1770, in LUFS.
    #[default]
    Loudness,
    /// The highest sample, in dBFS.
    Peak,
}

/// The loudness targets with a name: web, podcast, EBU R128 broadcast and
/// ATSC A/85.
pub const LOUDNESS_PRESETS: [(&str, f64); 4] = [("Web", -14.0), ("Podcast", -16.0), ("EBU R128 broadcast", -23.0), ("ATSC A/85", -24.0)];

/// The most a normalize raises a clip's gain: the most a clip's gain
/// holds (+24 dB).
pub const MAX_GAIN_DB: f64 = 24.0;

/// How to normalize.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NormalizeSettings {
    pub mode: NormalizeMode,
    /// LUFS for [`NormalizeMode::Loudness`], dBFS for [`NormalizeMode::Peak`].
    pub target: f64,
    /// The true peak, in dBTP, no clip is raised past.
    pub ceiling_dbtp: f64,
    /// One gain for all the clips, from their loudness together, so their
    /// balance is kept; otherwise each clip on its own.
    pub group: bool,
}

impl Default for NormalizeSettings {
    fn default() -> Self {
        Self { mode: NormalizeMode::Loudness, target: -14.0, ceiling_dbtp: crate::events::DEFAULT_TRUE_PEAK_DBTP as f64, group: false }
    }
}

impl NormalizeSettings {
    /// Whether the target and ceiling are values a clip can be brought to.
    pub fn check(&self) -> Result<(), String> {
        let (lo, hi) = match self.mode {
            NormalizeMode::Loudness => (-70.0, 0.0),
            NormalizeMode::Peak => (-60.0, 0.0),
        };
        if !(lo..=hi).contains(&self.target) {
            return Err(format!("a target of {} is outside {lo} to {hi}", self.target));
        }
        if !(-20.0..=0.0).contains(&self.ceiling_dbtp) {
            return Err(format!("a true-peak ceiling of {} dBTP is outside -20 to 0", self.ceiling_dbtp));
        }
        Ok(())
    }
}

/// What was measured of a clip's sound, or of several together. Silence
/// reads `-inf` throughout.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Level {
    pub integrated_lufs: f64,
    pub sample_peak_dbfs: f64,
    pub true_peak_dbtp: f64,
}

/// A clip's normalized gain, and what held it back.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Gain {
    pub db: f64,
    /// The ceiling held it below what the target asked for.
    pub ceiling_limited: bool,
    /// [`MAX_GAIN_DB`] held it.
    pub max_limited: bool,
}

/// The gain that brings `level` to `settings`' target, held under its
/// ceiling: `None` for silence, which no gain brings anywhere.
pub fn gain_for(level: &Level, settings: &NormalizeSettings) -> Option<Gain> {
    let measured = match settings.mode {
        NormalizeMode::Loudness => level.integrated_lufs,
        NormalizeMode::Peak => level.sample_peak_dbfs,
    };
    if !measured.is_finite() {
        return None;
    }
    let asked = settings.target - measured;
    let room = settings.ceiling_dbtp - level.true_peak_dbtp;
    let (db, ceiling_limited) = if room.is_finite() && asked > room { (room, true) } else { (asked, false) };
    let max_limited = db > MAX_GAIN_DB;
    Some(Gain { db: db.min(MAX_GAIN_DB), ceiling_limited, max_limited })
}

/// Each clip's gain: on its own from `levels`, or in a group one gain for
/// all from `together` (their loudness measured as one), held under the
/// ceiling by the loudest true peak among them.
pub fn gains(levels: &[Level], together: &Level, settings: &NormalizeSettings) -> Vec<Option<Gain>> {
    if !settings.group {
        return levels.iter().map(|l| gain_for(l, settings)).collect();
    }
    let peak = levels.iter().map(|l| l.true_peak_dbtp).fold(f64::NEG_INFINITY, f64::max);
    let one = gain_for(&Level { true_peak_dbtp: peak, ..*together }, settings);
    levels.iter().map(|l| if l.integrated_lufs.is_finite() || l.sample_peak_dbfs.is_finite() { one } else { None }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(lufs: f64, peak: f64, tp: f64) -> Level {
        Level { integrated_lufs: lufs, sample_peak_dbfs: peak, true_peak_dbtp: tp }
    }

    #[test]
    fn a_clip_is_brought_to_its_loudness_target() {
        let s = NormalizeSettings { target: -16.0, ..Default::default() };
        let g = gain_for(&level(-22.0, -9.0, -8.5), &s).unwrap();
        assert!((g.db - 6.0).abs() < 1e-9 && !g.ceiling_limited, "{g:?}");
    }

    #[test]
    fn the_ceiling_holds_the_gain() {
        // -14 asks for +10 dB, but the true peak at -4 dBTP has 3 dB of
        // room under a -1 dBTP ceiling.
        let s = NormalizeSettings { target: -14.0, ceiling_dbtp: -1.0, ..Default::default() };
        let g = gain_for(&level(-24.0, -4.5, -4.0), &s).unwrap();
        assert!((g.db - 3.0).abs() < 1e-9 && g.ceiling_limited, "{g:?}");
    }

    #[test]
    fn peak_mode_reads_the_sample_peak() {
        let s = NormalizeSettings { mode: NormalizeMode::Peak, target: -3.0, ceiling_dbtp: 0.0, group: false };
        let g = gain_for(&level(-30.0, -12.0, -11.8), &s).unwrap();
        assert!((g.db - 9.0).abs() < 1e-9, "{g:?}");
    }

    #[test]
    fn silence_is_left_alone() {
        let silent = level(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        assert_eq!(gain_for(&silent, &NormalizeSettings::default()), None);
    }

    #[test]
    fn a_group_takes_one_gain_and_keeps_its_balance() {
        let s = NormalizeSettings { target: -16.0, ceiling_dbtp: -1.0, group: true, ..Default::default() };
        let levels = [level(-20.0, -6.0, -5.5), level(-26.0, -14.0, -13.0)];
        let got = gains(&levels, &level(-22.0, -6.0, -5.5), &s);
        let (a, b) = (got[0].unwrap(), got[1].unwrap());
        assert_eq!(a, b, "one gain for the group");
        assert!((a.db - 4.5).abs() < 1e-9 && a.ceiling_limited, "+6 dB asked, the louder clip's true peak allows 4.5: {a:?}");
    }

    #[test]
    fn the_gain_stops_at_the_most_a_clip_holds() {
        let s = NormalizeSettings { target: -14.0, ceiling_dbtp: 0.0, ..Default::default() };
        let g = gain_for(&level(-60.0, -45.0, -44.0), &s).unwrap();
        assert!(g.db == MAX_GAIN_DB && g.max_limited, "{g:?}");
    }

    #[test]
    fn settings_out_of_range_are_refused() {
        assert!(NormalizeSettings { target: 3.0, ..Default::default() }.check().is_err());
        assert!(NormalizeSettings { ceiling_dbtp: 2.0, ..Default::default() }.check().is_err());
        assert!(NormalizeSettings::default().check().is_ok());
    }
}
