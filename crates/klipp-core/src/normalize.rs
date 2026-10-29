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
/// ATSC A/85. Built in, as the standards are: a person's own come after
/// them ([`loudness_targets`]) and never stand in for one.
pub const LOUDNESS_PRESETS: [(&str, f64); 4] = [("Web", -14.0), ("Podcast", -16.0), ("EBU R128 broadcast", -23.0), ("ATSC A/85", -24.0)];

/// A loudness target of a person's own (#2887), as their presets folder
/// holds it (`klipp_presets::user_dir("loudness")`, `[[target]]` with a
/// `name` and `lufs`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoudnessTarget {
    pub name: String,
    pub lufs: f64,
}

/// The longest name a person's loudness target takes, in characters.
const MAX_TARGET_NAME: usize = 64;

/// Every loudness target with a name: the built-in ones, then a person's
/// own, from their presets folder, read as someone else's. One with no
/// name or a name past its length or with control characters, a loudness
/// outside -70 to 0 LUFS, or the name of one before it (a standard's, any
/// case) is skipped, as is a file that doesn't read, each said in the log.
pub fn loudness_targets() -> Vec<(String, f64)> {
    let (targets, skipped) = loudness_targets_in(klipp_presets::user_dir("loudness").as_deref());
    for why in skipped {
        tracing::warn!("a loudness target was skipped: {why}");
    }
    targets
}

/// [`loudness_targets`] with a person's own from `dir`, and what was
/// skipped.
pub fn loudness_targets_in(dir: Option<&std::path::Path>) -> (Vec<(String, f64)>, Vec<String>) {
    let mut targets: Vec<(String, f64)> = LOUDNESS_PRESETS.iter().map(|(n, v)| (n.to_string(), *v)).collect();
    let (theirs, mut skipped) = dir.map_or_else(|| (Vec::new(), Vec::new()), |d| klipp_presets::read_dir::<LoudnessTarget>(d, "loudness_target", 1, "target"));
    for t in theirs {
        let name = t.name.trim();
        let why = if name.is_empty() || name.chars().count() > MAX_TARGET_NAME || name.chars().any(char::is_control) {
            Some(format!("a name of {} characters, or with control characters", name.chars().count()))
        } else if !(t.lufs.is_finite() && (-70.0..=0.0).contains(&t.lufs)) {
            Some(format!("\"{name}\": {} LUFS is outside -70 to 0", t.lufs))
        } else if targets.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)) {
            Some(format!("\"{name}\" is the name of one before it"))
        } else {
            None
        };
        match why {
            Some(why) => skipped.push(why),
            None => targets.push((name.to_string(), t.lufs)),
        }
    }
    (targets, skipped)
}

/// The loudness of the target named `name` (any case), built in or a
/// person's own.
pub fn named_target(name: &str) -> Option<f64> {
    loudness_targets().into_iter().find(|(n, _)| n.eq_ignore_ascii_case(name.trim())).map(|(_, v)| v)
}

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

    /// A person's own loudness targets come after the four standards
    /// (#2887), read from their folder as someone else's: one that would
    /// take a standard's name (any case) or one before it, a loudness
    /// outside -70 to 0, a name with control characters, a field a target
    /// hasn't and a file that doesn't read are each skipped and said.
    #[test]
    fn a_person_s_loudness_targets_follow_the_standards() {
        let dir = std::env::temp_dir().join(format!("klipp-loudness-targets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |n: &str, entries: &str| std::fs::write(dir.join(n), format!("kind = \"loudness_target\"\nformat_version = 1\n{entries}")).unwrap();
        file("mine.toml", "[[target]]\nname = \"Cinema trailer\"\nlufs = -27.0\n[[target]]\nname = \"WEB\"\nlufs = -10.0\n[[target]]\nname = \"Too loud\"\nlufs = 6.0\n[[target]]\nname = \"Bell\\u0007\"\nlufs = -20.0\n[[target]]\nname = \"cinema TRAILER\"\nlufs = -26.0\n");
        file("odd.toml", "[[target]]\nname = \"x\"\nlufs = -20.0\npeak = 1.0\n");
        let (targets, skipped) = loudness_targets_in(Some(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
        let names: Vec<&str> = targets.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Web", "Podcast", "EBU R128 broadcast", "ATSC A/85", "Cinema trailer"]);
        assert_eq!(targets[0].1, -14.0, "a standard keeps its loudness");
        assert_eq!(targets[4].1, -27.0);
        assert_eq!(skipped.len(), 5, "{skipped:?}");
        assert!(skipped.iter().any(|s| s.contains("\"WEB\" is the name of one before it")));
        assert!(skipped.iter().any(|s| s.contains("outside -70 to 0")));
        assert!(skipped.iter().any(|s| s.contains("control characters")));
        assert!(skipped.iter().any(|s| s.starts_with("odd.toml: ") && s.contains("unknown field")));
        assert_eq!(loudness_targets_in(None).0.len(), 4, "no folder, the standards");
        assert_eq!(named_target(" ebu r128 BROADCAST "), Some(-23.0), "a standard by name, any case");
    }
}
