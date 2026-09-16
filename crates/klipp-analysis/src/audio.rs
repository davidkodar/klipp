//! The sound as numbers: integrated loudness, loudness range and true
//! peak per EBU R128, and the spans of silence — the facts most quiet
//! suggestions and every loudness decision start from.

use serde::{Deserialize, Serialize};

pub const ID: &str = "audio";
pub const VERSION: u32 = 1;

/// Below this the signal is silence, and it has to stay there this long.
const SILENCE_DB: f32 = -35.0;
const SILENCE_MIN_SECS: f32 = 0.4;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Span {
    pub start_secs: f64,
    pub end_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AudioEnvelope {
    pub duration_secs: f64,
    /// EBU R128 integrated loudness, LUFS.
    pub integrated_lufs: Option<f32>,
    /// Loudness range, LU.
    pub loudness_range_lu: Option<f32>,
    /// True peak, dBTP.
    pub true_peak_dbtp: Option<f32>,
    pub silence_threshold_db: f32,
    pub silences: Vec<Span>,
}

impl AudioEnvelope {
    /// Seconds of silence within `from..to`.
    pub fn silence_within(&self, from: f64, to: f64) -> f64 {
        self.silences
            .iter()
            .map(|s| (s.end_secs.min(to) - s.start_secs.max(from)).max(0.0))
            .sum()
    }

    /// The silences overlapping `from..to`.
    pub fn silences_within(&self, from: f64, to: f64) -> Vec<&Span> {
        self.silences.iter().filter(|s| s.end_secs > from && s.start_secs < to).collect()
    }
}

fn number_after(line: &str, key: &str) -> Option<f64> {
    let i = line.find(key)?;
    let rest = line[i + key.len()..].trim_start();
    let token: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
    token.parse().ok()
}

/// What `silencedetect` and `ebur128` printed.
pub fn parse(stderr: &str, duration_secs: f64) -> AudioEnvelope {
    let mut env = AudioEnvelope { duration_secs, silence_threshold_db: SILENCE_DB, ..Default::default() };
    let mut open: Option<f64> = None;
    let mut in_summary = false;
    for line in stderr.lines() {
        if line.contains("silence_start:") {
            open = number_after(line, "silence_start:");
        } else if line.contains("silence_end:") {
            if let (Some(start), Some(end)) = (open.take(), number_after(line, "silence_end:")) {
                env.silences.push(Span { start_secs: start, end_secs: end });
            }
        } else if line.contains("Summary:") {
            in_summary = true;
        } else if in_summary {
            let t = line.trim_start();
            if let Some(v) = t.strip_prefix("I:") {
                env.integrated_lufs = number_after(v, "").map(|x| x as f32);
            } else if let Some(v) = t.strip_prefix("LRA:") {
                env.loudness_range_lu = number_after(v, "").map(|x| x as f32);
            } else if let Some(v) = t.strip_prefix("Peak:") {
                env.true_peak_dbtp = number_after(v, "").map(|x| x as f32);
            }
        }
    }
    // A silence still open at the end runs to the end.
    if let Some(start) = open {
        if duration_secs > start {
            env.silences.push(Span { start_secs: start, end_secs: duration_secs });
        }
    }
    env
}

pub struct AudioAnalyser;

impl crate::Analyser for AudioAnalyser {
    fn id(&self) -> &'static str {
        ID
    }
    fn schema_version(&self) -> u32 {
        VERSION
    }
    fn wants(&self, item: &klipp_media::MediaItem) -> bool {
        item.has_audio && item.duration.to_secs_f64() > 0.0
    }
    fn run(&self, item: &klipp_media::MediaItem, ctx: &crate::AnalysisCtx) -> Result<serde_json::Value, String> {
        let args = vec![
            "-hide_banner".to_string(),
            "-nostats".to_string(),
            "-i".to_string(),
            item.path.to_string_lossy().into_owned(),
            "-vn".to_string(),
            "-sn".to_string(),
            "-af".to_string(),
            format!("silencedetect=noise={SILENCE_DB}dB:d={SILENCE_MIN_SECS},ebur128=peak=true"),
            "-f".to_string(),
            "null".to_string(),
            "-".to_string(),
        ];
        let stderr = crate::ffmpeg_stderr(&ctx.ffmpeg, &args)?;
        let env = parse(&stderr, item.duration.to_secs_f64());
        serde_json::to_value(env).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silences_and_the_r128_summary_are_read() {
        let text = "\
[silencedetect @ 0x1] silence_start: 1.02\n\
[silencedetect @ 0x1] silence_end: 2.5 | silence_duration: 1.48\n\
[silencedetect @ 0x1] silence_start: 9.0\n\
[Parsed_ebur128_1 @ 0x2] Summary:\n\
\n\
  Integrated loudness:\n\
    I:         -23.4 LUFS\n\
    Threshold: -33.5 LUFS\n\
\n\
  Loudness range:\n\
    LRA:         6.2 LU\n\
\n\
  True peak:\n\
    Peak:       -1.3 dBFS\n";
        let e = parse(text, 10.0);
        assert_eq!(e.silences, vec![Span { start_secs: 1.02, end_secs: 2.5 }, Span { start_secs: 9.0, end_secs: 10.0 }]);
        assert_eq!(e.integrated_lufs, Some(-23.4));
        assert_eq!(e.loudness_range_lu, Some(6.2));
        assert_eq!(e.true_peak_dbtp, Some(-1.3));
        assert!((e.silence_within(0.0, 2.0) - 0.98).abs() < 1e-6);
    }
}
