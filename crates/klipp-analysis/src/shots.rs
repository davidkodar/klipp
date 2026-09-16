//! Shot boundaries: where the picture changes enough to be a cut, from
//! ffmpeg's scene-change score on a downscaled decode. One record per
//! shot, so everything else can be bucketed by shot.

use serde::{Deserialize, Serialize};

pub const ID: &str = "shots";
pub const VERSION: u32 = 1;

/// Scene-change score above which a frame starts a new shot; ffmpeg's
/// `scene` is 0 to 1, and 0.3 is the customary threshold.
const THRESHOLD: f32 = 0.3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Shot {
    pub start_secs: f64,
    pub end_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Shots {
    pub duration_secs: f64,
    /// Where each new shot starts, seconds from the file's start; the
    /// first shot starts at 0 and is not listed.
    pub boundaries: Vec<f64>,
    pub shots: Vec<Shot>,
}

impl Shots {
    pub fn from_boundaries(duration_secs: f64, mut boundaries: Vec<f64>) -> Self {
        boundaries.retain(|b| *b > 0.0 && *b < duration_secs);
        boundaries.sort_by(|a, b| a.partial_cmp(b).unwrap());
        boundaries.dedup_by(|a, b| (*a - *b).abs() < 0.04);
        let mut shots = Vec::with_capacity(boundaries.len() + 1);
        let mut start = 0.0;
        for b in &boundaries {
            shots.push(Shot { start_secs: start, end_secs: *b });
            start = *b;
        }
        shots.push(Shot { start_secs: start, end_secs: duration_secs });
        Self { duration_secs, boundaries, shots }
    }

    /// The shots overlapping `from..to` of the file.
    pub fn within(&self, from: f64, to: f64) -> Vec<&Shot> {
        self.shots.iter().filter(|s| s.end_secs > from && s.start_secs < to).collect()
    }
}

/// The `pts_time:` of every frame `showinfo` printed: with the select
/// filter ahead of it, the frames that start a shot.
pub fn parse_showinfo(stderr: &str) -> Vec<f64> {
    let mut out = Vec::new();
    for line in stderr.lines() {
        if !line.contains("showinfo") {
            continue;
        }
        if let Some(i) = line.find("pts_time:") {
            let rest = &line[i + "pts_time:".len()..];
            let token: String = rest.trim_start().chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
            if let Ok(t) = token.parse::<f64>() {
                out.push(t);
            }
        }
    }
    out
}

pub struct ShotsAnalyser;

impl crate::Analyser for ShotsAnalyser {
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
        let args = vec![
            "-hide_banner".to_string(),
            "-nostats".to_string(),
            "-i".to_string(),
            item.path.to_string_lossy().into_owned(),
            "-an".to_string(),
            "-sn".to_string(),
            "-vf".to_string(),
            format!("scale=320:-2,select='gt(scene,{THRESHOLD})',showinfo"),
            "-f".to_string(),
            "null".to_string(),
            "-".to_string(),
        ];
        let stderr = crate::ffmpeg_stderr(&ctx.ffmpeg, &args)?;
        let boundaries = parse_showinfo(&stderr);
        let shots = Shots::from_boundaries(item.duration.to_secs_f64(), boundaries);
        serde_json::to_value(shots).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn showinfo_lines_give_the_boundaries_and_shots_follow() {
        let text = "\
[Parsed_showinfo_2 @ 0x1] n:   0 pts:  50 pts_time:2.04    pos: 1 fmt:yuv420p\n\
[Parsed_showinfo_2 @ 0x1] n:   1 pts: 125 pts_time:5.0 pos: 2\n\
some other line pts_time:9.9\n";
        let b = parse_showinfo(text);
        assert_eq!(b, vec![2.04, 5.0]);
        let s = Shots::from_boundaries(8.0, b);
        assert_eq!(s.shots.len(), 3);
        assert_eq!(s.shots[1], Shot { start_secs: 2.04, end_secs: 5.0 });
        assert_eq!(s.within(4.0, 6.0).len(), 2);
    }

    #[test]
    fn boundaries_outside_the_file_and_duplicates_are_dropped() {
        let s = Shots::from_boundaries(10.0, vec![12.0, 3.0, 3.01, 0.0, 7.0]);
        assert_eq!(s.boundaries, vec![3.0, 7.0]);
        assert_eq!(s.shots.len(), 3);
    }
}
