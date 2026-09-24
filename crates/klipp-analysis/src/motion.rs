//! How much the picture moves, second by second: the mean absolute
//! difference between consecutive decoded frames' luma (ffmpeg's
//! `signalstats` YDIF) on a small native-rate decode. A gap between decoded
//! samples is treated as a held picture; leading and trailing gaps remain
//! unknown. Not where the camera went — a static shot
//! with a subject walking through it moves too — but enough to tell a
//! locked-off shot from a pan or a handheld one, and to find the
//! stretches where nothing happens.

use serde::{Deserialize, Serialize};

pub const ID: &str = "motion";
pub const VERSION: u32 = 7;
/// The reference cadence used to make YDIF comparable across source rates.
/// A 30 fps stream with six units of YDIF per frame therefore measures 6.0.
const REFERENCE_FPS: f64 = 30.0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Motion {
    pub duration_secs: f64,
    /// Mean YDIF over each whole second of the file, in order.
    /// None / JSON null means no decoded measurement; Some(0) is stillness.
    pub per_second: Vec<Option<f32>>,
    pub mean: Option<f32>,
    pub p95: Option<f32>,
}

impl Motion {
    /// Build the cached summary and bins together; callers must not invent
    /// a mean inconsistent with the samples returned by MediaAnalysis.
    pub fn from_samples(duration_secs: f64, samples: Vec<Option<f32>>) -> Self {
        let per_second: Vec<_> = samples.into_iter().map(|v| v.filter(|v| valid_sample(*v))).collect();
        let mean = mean_of(per_second.iter().flatten().copied());
        let mut sorted: Vec<_> = per_second.iter().flatten().copied().collect();
        sorted.sort_by(f32::total_cmp);
        let p95 = sorted.get(((sorted.len() as f32 * 0.95) as usize).min(sorted.len().saturating_sub(1))).copied();
        Self { duration_secs, per_second, mean, p95 }
    }

    /// The authoritative whole-file mean, absent when nothing was measured.
    pub fn overall_mean(&self) -> Option<f32> {
        self.mean
    }

    /// The seconds `from..to` of the file.
    fn slice(&self, from: f64, to: f64) -> &[Option<f32>] {
        let a = (from.max(0.0).floor() as usize).min(self.per_second.len());
        let b = (to.max(0.0).ceil() as usize).clamp(a, self.per_second.len());
        &self.per_second[a..b]
    }

    pub fn mean_within(&self, from: f64, to: f64) -> Option<f32> {
        mean_of(self.slice(from, to).iter().flatten().copied())
    }

    /// A word for the amount of movement in `from..to`.
    pub fn class_within(&self, from: f64, to: f64) -> Option<&'static str> {
        self.mean_within(from, to).map(class_for)
    }
}

/// Mean of measured samples, accumulated in f64.
pub fn mean_of(values: impl Iterator<Item = f32>) -> Option<f32> {
    let (sum, count) = values.fold((0.0f64, 0usize), |(sum, count), v| (sum + v as f64, count + 1));
    (count > 0).then(|| (sum / count as f64) as f32)
}

fn valid_sample(value: f32) -> bool {
    value.is_finite() && (0.0..=255.0).contains(&value)
}

/// YDIF is on a 0 to 255 scale; these bands come from looking at
/// footage, not from a standard, and are here to be revised.
pub fn class_for(mean_ydif: f32) -> &'static str {
    if mean_ydif < 1.5 {
        "static"
    } else if mean_ydif < 4.0 {
        "gentle"
    } else if mean_ydif < 10.0 {
        "moving"
    } else {
        "busy"
    }
}

/// `metadata=print` output: `frame:N pts:.. pts_time:T` lines followed
/// by `lavfi.signalstats.YDIF=V`, into per-second means.
pub fn parse_metadata(text: &str, duration_secs: f64) -> Motion {
    // Native-rate YDIF avoids making the motion class depend on source FPS.
    // Empty bins between two decoded frames are held pictures and therefore
    // measured zero; leading/trailing gaps remain unknown.
    // Accept the existing final bin even when video outlasts container duration;
    // the allowance is measured from the decoded timestamps, never assumed.
    let seconds = duration_secs.ceil().max(1.0) as usize;
    let mut sums = vec![0.0f64; seconds];
    let mut measured = vec![false; seconds];
    let mut decoded_times = Vec::new();
    let mut samples = Vec::new();
    let mut t: Option<f64> = None;
    for line in text.lines() {
        if let Some(i) = line.find("pts_time:") {
            t = line[i + "pts_time:".len()..].trim().parse().ok();
            if let Some(frame_time) = t.filter(|value: &f64| value.is_finite() && *value >= 0.0) {
                decoded_times.push(frame_time);
            }
        } else if let Some(v) = line.strip_prefix("lavfi.signalstats.YDIF=") {
            if let (Some(t), Ok(v)) = (t, v.trim().parse::<f32>()) {
                if !t.is_finite() || t < 0.0 || !valid_sample(v) {
                    continue;
                }
                samples.push((t, v));
            }
        }
    }
    decoded_times.sort_by(f64::total_cmp);
    decoded_times.dedup_by(|a, b| a == b);
    let mut intervals: Vec<f64> = decoded_times
        .windows(2)
        .filter_map(|pair| {
            let d = pair[1] - pair[0];
            (d.is_finite() && d > 0.0).then_some(d)
        })
        .collect();
    intervals.sort_by(f64::total_cmp);
    let observed_interval = intervals
        .iter()
        .rev()
        .take(16)
        .copied()
        .collect::<Vec<_>>()
        .get(8.min(intervals.len().saturating_sub(1)))
        .copied()
        .unwrap_or(0.1)
        .clamp(1e-6, 0.1);
    let tail_limit = duration_secs.ceil() + observed_interval + 1e-9;
    for (time, value) in samples {
        if time > tail_limit {
            continue;
        }
        let tail_sample = time >= seconds as f64;
        let second = if tail_sample {
            seconds - 1
        } else {
            (time.floor() as usize).min(seconds - 1)
        };
        // YDIF is a per-frame difference. Count each decoded transition once,
        // then normalize by time at a fixed reference cadence. Weighting by
        // dwell would make a held VFR picture look busier than its CFR twin.
        sums[second] += value as f64;
        measured[second] = true;
    }
    for second in 0..seconds {
        if measured[second] {
            continue;
        }
        let start = second as f64;
        let end = (second + 1) as f64;
        let bracketed = decoded_times.first().is_some_and(|time| *time <= start)
            && decoded_times.last().is_some_and(|time| *time >= end);
        if bracketed {
            measured[second] = true;
        }
    }
    let per_second = sums
        .iter()
        .zip(&measured)
        .enumerate()
        .map(|(second, (sum, has_measurement))| {
            if !has_measurement {
                return None;
            }
            let coverage = if second + 1 == seconds {
                (duration_secs - second as f64).clamp(0.0, 1.0).max(1.0_f64.min(observed_interval))
            } else {
                1.0
            };
            Some((*sum / (coverage * REFERENCE_FPS)) as f32)
        })
        .collect();
    Motion::from_samples(duration_secs, per_second)
}

pub struct MotionAnalyser;

impl crate::Analyser for MotionAnalyser {
    fn id(&self) -> &'static str {
        ID
    }
    fn schema_version(&self) -> u32 {
        VERSION
    }
    fn wants(&self, item: &klipp_media::MediaItem) -> bool {
        !item.is_still && !item.video_codec.is_empty() && item.duration.to_secs_f64() > 0.0
    }
    fn run(&self, item: &klipp_media::MediaItem, ctx: &crate::AnalysisCtx) -> Result<serde_json::Value, String> {
        // From the one decode every whole-picture analyser shares.
        let pass = crate::videopass::get(ctx, item)?;
        let motion = parse_metadata(&pass.ydif_text, item.duration.to_secs_f64());
        serde_json::to_value(motion).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_tail_inside_existing_bin_is_retained() {
        let m=parse_metadata("frame:0 pts_time:4.6\nlavfi.signalstats.YDIF=2\nframe:1 pts_time:4.67\nlavfi.signalstats.YDIF=4\n",4.5);
        assert_eq!(m.per_second[4],Some(0.4));
    }
    #[test]
    fn tail_samples_allow_one_sampling_interval_but_not_arbitrary_late_data() {
        let text = "frame:0 pts_time:4.94\nlavfi.signalstats.YDIF=2\nframe:1 pts_time:4.96\nlavfi.signalstats.YDIF=2\nframe:2 pts_time:4.98\nlavfi.signalstats.YDIF=2\nframe:3 pts_time:5.00\nlavfi.signalstats.YDIF=10\nframe:4 pts_time:5.02\nlavfi.signalstats.YDIF=14\nframe:5 pts_time:5.50\nlavfi.signalstats.YDIF=255\n";
        let m = parse_metadata(text, 5.0);
        assert_eq!(m.per_second[4], Some(1.0));
        assert!(m.per_second[..4].iter().all(Option::is_none));
        let variable = "frame:0 pts_time:4.85\nlavfi.signalstats.YDIF=2\nframe:1 pts_time:4.9\nlavfi.signalstats.YDIF=2\nframe:2 pts_time:4.97\nlavfi.signalstats.YDIF=2\nframe:3 pts_time:5.03\nlavfi.signalstats.YDIF=10\n";
        assert_eq!(parse_metadata(variable,5.0).per_second[4], Some(0.53333336));
    }

    #[test]
    fn gaps_between_decoded_frames_are_held_stillness() {
        let text = "frame:0 pts_time:0\nlavfi.signalstats.YDIF=2\n\
frame:1 pts_time:3.0\nlavfi.signalstats.YDIF=4\n";
        let m = parse_metadata(text, 4.0);
        assert_eq!(m.per_second, vec![Some(0.06666667), Some(0.0), Some(0.0), Some(0.13333334)]);
    }

    #[test]
    fn motion_scale_does_not_depend_on_source_frame_rate() {
        let text = "frame:0 pts_time:0\nlavfi.signalstats.YDIF=2\n\
frame:1 pts_time:0.5\nlavfi.signalstats.YDIF=2\n";
        let m = parse_metadata(text, 1.0);
        assert_eq!(m.mean, Some(0.13333334));
        assert_eq!(m.class_within(0.0, 1.0), Some("static"));
    }

    #[test]
    fn equivalent_cfr_vfr_and_high_rate_inputs_have_one_class() {
        let cfr30 = (0..30)
            .map(|i| format!("frame:{i} pts_time:{:.6}\nlavfi.signalstats.YDIF=3\n", i as f64 / 30.0))
            .collect::<String>();
        let cfr60 = (0..60)
            .map(|i| format!("frame:{i} pts_time:{:.6}\nlavfi.signalstats.YDIF=1.5\n", i as f64 / 60.0))
            .collect::<String>();
        let vfr = "frame:0 pts_time:0\nlavfi.signalstats.YDIF=45\nframe:1 pts_time:0.5\nlavfi.signalstats.YDIF=45\n";
        let classes = [cfr30, cfr60, vfr.to_string()]
            .into_iter()
            .map(|text| parse_metadata(&text, 1.0).class_within(0.0, 1.0))
            .collect::<Vec<_>>();
        assert_eq!(classes, vec![Some("gentle"), Some("gentle"), Some("gentle")]);
    }

    #[test]
    fn subsecond_holds_do_not_change_the_motion_class_at_a_boundary() {
        let held_short = "frame:0 pts_time:0\nlavfi.signalstats.YDIF=8\nframe:1 pts_time:0.9\nlavfi.signalstats.YDIF=8\nframe:2 pts_time:2.0\nlavfi.signalstats.YDIF=8\n";
        let held_long = "frame:0 pts_time:0\nlavfi.signalstats.YDIF=8\nframe:1 pts_time:1.1\nlavfi.signalstats.YDIF=8\nframe:2 pts_time:2.0\nlavfi.signalstats.YDIF=8\n";
        assert_eq!(parse_metadata(held_short, 2.0).class_within(0.0, 2.0), Some("static"));
        assert_eq!(parse_metadata(held_long, 2.0).class_within(0.0, 2.0), Some("static"));
    }

    #[test]
    fn per_second_means_and_classes() {
        let text = "frame:0 pts:0 pts_time:0\nlavfi.signalstats.YDIF=0\n\
frame:1 pts:20 pts_time:0.5\nlavfi.signalstats.YDIF=2\n\
frame:2 pts:40 pts_time:1.2\nlavfi.signalstats.YDIF=12\n\
frame:3 pts:60 pts_time:1.8\nlavfi.signalstats.YDIF=8\n";
        let m = parse_metadata(text, 2.0);
        assert_eq!(m.per_second, vec![Some(0.06666667), Some(0.6666667)]);
        assert_eq!(m.class_within(0.0, 1.0), Some("static"));
        assert_eq!(m.class_within(1.0, 2.0), Some("static"));
        assert_eq!(class_for(5.0), "moving");
    }
    #[test]
    fn undecoded_seconds_survive_cache_round_trip_as_missing() {
        let m = parse_metadata("frame:1 pts:1 pts_time:0.5\nlavfi.signalstats.YDIF=12\n", 21.0);
        assert_eq!(m.mean, Some(0.4));
        assert_eq!(m.p95, Some(0.4));
        assert_eq!(m.mean_within(1.0, 21.0), None);
        assert_eq!(m.class_within(0.0, 21.0), Some("static"));
        let cached = serde_json::to_value(&m).unwrap();
        assert!(cached["per_second"][1].is_null());
        assert_eq!(serde_json::from_value::<Motion>(cached).unwrap(), m);
        let invalid = parse_metadata("frame:0 pts_time:NaN\nlavfi.signalstats.YDIF=3\nframe:1 pts_time:0.5\nlavfi.signalstats.YDIF=NaN\n", 2.0);
        assert_eq!(invalid.mean_within(0.0, 2.0), None);
        assert_eq!(invalid.mean, None);
        assert_eq!(invalid.p95, None);
    }

}
