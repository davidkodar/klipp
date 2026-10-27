//! One decode of a file for everything that reads its whole picture:
//! the shot scores at two rates, the per-frame motion, the settling
//! (freezedetect) and the activity grid. A long file was decoded once
//! per analyser, and each decode was the cost — the filters are cheap.
//! The result is kept in memory for the analysers that run next on the
//! same file, so shots, motion and the beat sheet share it.

use std::sync::{Arc, Mutex, OnceLock};

/// The whole-picture reads of one file.
#[derive(Debug, Default)]
pub(crate) struct VideoPass {
    /// Times of frames whose scene score passed the shot threshold, at
    /// the file's own rate.
    pub scene_fast: Vec<f64>,
    /// The same at two frames a second and the slower threshold: a
    /// layout change that takes a moment.
    pub scene_slow: Vec<f64>,
    /// ffmpeg's `metadata=print` text of `lavfi.signalstats.YDIF` per
    /// frame, as the motion analyser parses it.
    pub ydif_text: String,
    /// Frozen spans from freezedetect, `(start, end)` seconds.
    pub frozen: Vec<(f64, f64)>,
    /// The activity grid, `GRID_W`×`GRID_H` bytes per sampled frame.
    pub grid: Vec<u8>,
}

/// The last few passes, by the file's content key.
fn cache() -> &'static Mutex<Vec<(String, Arc<VideoPass>)>> {
    static C: OnceLock<Mutex<Vec<(String, Arc<VideoPass>)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Vec::new()))
}
const KEEP: usize = 4;

/// The pass for `item`, run now if no analyser has asked for it yet.
pub(crate) fn get(ctx: &crate::AnalysisCtx, item: &klipp_media::MediaItem) -> Result<Arc<VideoPass>, String> {
    let key = crate::store::key_for(&item.path).ok_or("file not readable")?;
    if let Some((_, p)) = cache().lock().map_err(|_| "video pass cache poisoned")?.iter().find(|(k, _)| *k == key) {
        return Ok(Arc::clone(p));
    }
    let pass = Arc::new(run(ctx, item)?);
    let mut c = cache().lock().map_err(|_| "video pass cache poisoned")?;
    c.retain(|(k, _)| *k != key);
    c.push((key, Arc::clone(&pass)));
    if c.len() > KEEP {
        c.remove(0);
    }
    Ok(pass)
}

/// Frame times from ffmpeg's `metadata=print` text: one per `pts_time:`.
pub fn parse_pts_times(text: &str) -> Vec<f64> {
    text.lines()
        .filter_map(|l| l.find("pts_time:").map(|i| &l[i + "pts_time:".len()..]))
        .filter_map(|rest| rest.split_whitespace().next()?.parse::<f64>().ok())
        .collect()
}

/// The pass, on the GPU where it can be, on the CPU as the last resort.
fn run(ctx: &crate::AnalysisCtx, item: &klipp_media::MediaItem) -> Result<VideoPass, String> {
    let mut last = String::new();
    for plan in crate::decode_plans(&ctx.ffmpeg) {
        match run_with(ctx, item, plan) {
            Ok(p) => {
                tracing::info!("video pass of {} decoded with {plan:?}", item.filename);
                return Ok(p);
            }
            Err(e) => {
                tracing::warn!("video pass of {} with {plan:?} failed ({e}); trying the next", item.filename);
                last = e;
            }
        }
    }
    Err(last)
}

fn run_with(ctx: &crate::AnalysisCtx, item: &klipp_media::MediaItem, plan: crate::Decode) -> Result<VideoPass, String> {
    let duration = item.duration.to_secs_f64();
    let stamp = format!("{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    let dir = std::env::temp_dir();
    let fast_path = dir.join(format!("klipp-vp-{stamp}-fast.txt"));
    let slow_path = dir.join(format!("klipp-vp-{stamp}-slow.txt"));
    let ydif_path = dir.join(format!("klipp-vp-{stamp}-ydif.txt"));
    let (fast, slow, ydif) = (fast_path.to_string_lossy().into_owned(), slow_path.to_string_lossy().into_owned(), ydif_path.to_string_lossy().into_owned());
    let sc = |w: u32| crate::scale_filter(&ctx.ffmpeg, plan, w, -2);
    // The branches that only print end in a null sink: an output that
    // never gets a frame (a screen recording with no scene change) would
    // fail the whole run at the end of the file.
    let graph = format!(
        "[0:v:0]split=4[s1][s2][m][b];\
         [s1]{s320},select='gt(scene,{})',metadata=print:key=lavfi.scene_score:file={fast},nullsink;\
         [s2]fps=2,{s320},select='gt(scene,{})',metadata=print:key=lavfi.scene_score:file={slow},nullsink;\
         [m]{s160},signalstats,metadata=print:key=lavfi.signalstats.YDIF:file={ydif},nullsink;\
         [b]fps={},{s480},split[fz][act];\
         [fz]freezedetect=n={}:d={},nullsink;\
         [act]fps={},format=gray,tblend=all_mode=difference,scale={}:{}:flags=area[acto]",
        crate::shots::THRESHOLD, crate::shots::SLOW_THRESHOLD,
        crate::beatsheet::SAMPLE_FPS, crate::beatsheet::FREEZE_NOISE, crate::beatsheet::FREEZE_MIN_SECS,
        crate::beatsheet::GRID_FPS, crate::beatsheet::GRID_W, crate::beatsheet::GRID_H,
        s320 = sc(320), s160 = sc(160), s480 = sc(crate::beatsheet::SCALE_W),
    );
    let mut cmd = std::process::Command::new(&ctx.ffmpeg);
    cmd.args(["-hide_banner", "-nostats", "-loglevel", "info"]);
    cmd.args(crate::decode_input_args(&ctx.ffmpeg, plan, &item.video_codec));
    cmd.args(klipp_media::input_args(&item.path)?).args(["-an", "-sn", "-filter_complex", &graph]);
    cmd.args(["-map", "[acto]", "-f", "rawvideo", "-pix_fmt", "gray", "-"]);
    let out = crate::hold::output(&mut cmd).map_err(|e| format!("could not run {}: {e}", ctx.ffmpeg.display()))?;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let read = |p: &std::path::Path| std::fs::read_to_string(p).unwrap_or_default();
    let (fast_text, slow_text, ydif_text) = (read(&fast_path), read(&slow_path), read(&ydif_path));
    for p in [&fast_path, &slow_path, &ydif_path] {
        let _ = std::fs::remove_file(p);
    }
    if !out.status.success() {
        let tail: String = stderr.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" | ");
        return Err(format!("ffmpeg failed ({}): {tail}", out.status));
    }
    Ok(VideoPass {
        scene_fast: parse_pts_times(&fast_text),
        scene_slow: parse_pts_times(&slow_text),
        ydif_text,
        frozen: crate::beatsheet::parse_freezedetect(&stderr, duration),
        grid: out.stdout,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_print_times_are_read_one_per_frame() {
        let text = "frame:0    pts:0      pts_time:0\nlavfi.scene_score=0.911\nframe:57   pts:57057  pts_time:2.336\nlavfi.scene_score=0.401\n";
        assert_eq!(parse_pts_times(text), vec![0.0, 2.336]);
        assert!(parse_pts_times("").is_empty());
    }
}
