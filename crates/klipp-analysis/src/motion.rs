//! How much the picture moves, second by second: the mean absolute
//! difference between consecutive frames' luma (ffmpeg's `signalstats`
//! YDIF) on a small decode. Not where the camera went — a static shot
//! with a subject walking through it moves too — but enough to tell a
//! locked-off shot from a pan or a handheld one, and to find the
//! stretches where nothing happens.

use serde::{Deserialize, Serialize};

pub const ID: &str = "motion";
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Motion {
    pub duration_secs: f64,
    /// Mean YDIF over each whole second of the file, in order.
    pub per_second: Vec<f32>,
    pub mean: f32,
    pub p95: f32,
}

impl Motion {
    /// The seconds `from..to` of the file.
    fn slice(&self, from: f64, to: f64) -> &[f32] {
        let a = (from.max(0.0).floor() as usize).min(self.per_second.len());
        let b = (to.max(0.0).ceil() as usize).clamp(a, self.per_second.len());
        &self.per_second[a..b]
    }

    pub fn mean_within(&self, from: f64, to: f64) -> Option<f32> {
        let s = self.slice(from, to);
        (!s.is_empty()).then(|| s.iter().sum::<f32>() / s.len() as f32)
    }

    /// A word for the amount of movement in `from..to`.
    pub fn class_within(&self, from: f64, to: f64) -> Option<&'static str> {
        self.mean_within(from, to).map(class_for)
    }
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
    let seconds = duration_secs.ceil().max(1.0) as usize;
    let mut sums = vec![0.0f32; seconds];
    let mut counts = vec![0u32; seconds];
    let mut t: Option<f64> = None;
    for line in text.lines() {
        if let Some(i) = line.find("pts_time:") {
            t = line[i + "pts_time:".len()..].trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("lavfi.signalstats.YDIF=") {
            if let (Some(t), Ok(v)) = (t, v.trim().parse::<f32>()) {
                let s = (t.floor() as usize).min(seconds - 1);
                sums[s] += v;
                counts[s] += 1;
            }
        }
    }
    let per_second: Vec<f32> = sums.iter().zip(&counts).map(|(s, c)| if *c > 0 { s / *c as f32 } else { 0.0 }).collect();
    let mean = if per_second.is_empty() { 0.0 } else { per_second.iter().sum::<f32>() / per_second.len() as f32 };
    let mut sorted = per_second.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p95 = sorted.get(((sorted.len() as f32 * 0.95) as usize).min(sorted.len().saturating_sub(1))).copied().unwrap_or(0.0);
    Motion { duration_secs, per_second, mean, p95 }
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
        let out = std::process::Command::new(&ctx.ffmpeg)
            .args(["-hide_banner", "-nostats", "-loglevel", "error", "-i"])
            .arg(&item.path)
            .args(["-an", "-sn", "-vf", "scale=160:-2,signalstats,metadata=print:key=lavfi.signalstats.YDIF:file=-", "-f", "null", "-"])
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|e| format!("could not run {}: {e}", ctx.ffmpeg.display()))?;
        if !out.status.success() {
            return Err(format!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        let motion = parse_metadata(&String::from_utf8_lossy(&out.stdout), item.duration.to_secs_f64());
        serde_json::to_value(motion).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_second_means_and_classes() {
        let text = "frame:0 pts:0 pts_time:0\nlavfi.signalstats.YDIF=0\n\
frame:1 pts:20 pts_time:0.5\nlavfi.signalstats.YDIF=2\n\
frame:2 pts:40 pts_time:1.2\nlavfi.signalstats.YDIF=12\n\
frame:3 pts:60 pts_time:1.8\nlavfi.signalstats.YDIF=8\n";
        let m = parse_metadata(text, 2.0);
        assert_eq!(m.per_second, vec![1.0, 10.0]);
        assert_eq!(m.class_within(0.0, 1.0), Some("static"));
        assert_eq!(m.class_within(1.0, 2.0), Some("busy"));
        assert_eq!(class_for(5.0), "moving");
    }
}
