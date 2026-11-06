//! Frames close together, read in one pass of one decoder instead of a
//! seek each.
//!
//! A seek decodes from the keyframe before its time, so frames that sit
//! closer together than half the keyframe spacing decode the same
//! pictures again and again, each with a decoder of its own. One pass
//! from the first time to the last decodes each picture once. Which
//! frame each time gets doesn't change: the first one at or after it,
//! the one a seek gives. Anything the pass can't account for exactly
//! is left to the seeks, so the worst a pass can do is cost the time it
//! took.

use std::path::Path;

/// The longest stretch of the file one pass decodes: a few seconds of
/// decoding even for 8K, well inside the time a GPU grab is given.
const MAX_PASS_SECS: i64 = 20;

/// The most frames one pass picks out.
const MAX_PASS_FRAMES: usize = 48;

/// How far past its last time a pass reads, in milliseconds: the frame
/// at or after that time is inside it for any video of one frame a
/// second or more. A later one is left to a seek.
const PASS_TAIL_MS: i64 = 1000;

/// How much of the file, from the first time, the keyframe spacing is
/// read from.
const KEYFRAME_WINDOW_SECS: u32 = 20;

/// `t` in whole milliseconds, rounded as a seek's `-ss {t:.3}` rounds
/// it, so a pass and a seek ask for the same time. Nothing for a time
/// before the start or not a number.
pub(crate) fn millis(t: f64) -> Option<i64> {
    if !t.is_finite() || t < 0.0 {
        return None;
    }
    format!("{t:.3}").replace('.', "").parse().ok()
}

fn secs_text(ms: i64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

/// The times to read in one pass each: runs of indices into `times`,
/// in time order, each of two or more frames closer together than half
/// of `keyframe_ms`. A time left out of every run is for a seek.
pub(crate) fn runs(ms: &[i64], keyframe_ms: i64) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..ms.len()).collect();
    order.sort_by_key(|&i| ms[i]);
    let mut runs: Vec<Vec<usize>> = Vec::new();
    let mut run: Vec<usize> = Vec::new();
    for i in order {
        let joins = run.last().is_some_and(|&last| {
            // Strictly closer than half the spacing, so a file of all
            // keyframes (spacing one frame) is never read in a pass.
            2 * (ms[i] - ms[last]) < keyframe_ms && ms[i] - ms[run[0]] <= MAX_PASS_SECS * 1000 && run.len() < MAX_PASS_FRAMES
        });
        if !joins && !run.is_empty() {
            runs.push(std::mem::take(&mut run));
        }
        run.push(i);
    }
    runs.push(run);
    runs.retain(|r| r.len() >= 2);
    runs
}

/// The spacing of the keyframes from `from_ms` on, in milliseconds,
/// from the packets alone (nothing decoded): the median gap. A window
/// with fewer than two keyframes spaces them at least the window apart.
/// Nothing when it can't be read.
pub(crate) fn keyframe_spacing(ffmpeg: &Path, item: &klipp_media::MediaItem, from_ms: i64) -> Option<i64> {
    let mut cmd = std::process::Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-nostats", "-loglevel", "error", "-ss", &secs_text(from_ms), "-t", &KEYFRAME_WINDOW_SECS.to_string()]);
    cmd.args(klipp_media::input_args(&item.path).ok()?);
    cmd.args(["-map", "0:v:0", "-c", "copy", "-f", "framecrc", "-"]);
    let out = crate::hold::output_within(&mut cmd, Some(std::time::Duration::from_secs(10))).ok()??;
    if !out.status.success() {
        return None;
    }
    spacing_from_framecrc(&String::from_utf8_lossy(&out.stdout))
}

/// [`keyframe_spacing`] from ffmpeg's `framecrc` listing: a `#tb 0: N/D`
/// line, then a line a packet, `stream, dts, pts, duration, size, crc`
/// with `, F=0x..` after when it is not a keyframe.
pub(crate) fn spacing_from_framecrc(text: &str) -> Option<i64> {
    let (mut num, mut den) = (0i64, 0i64);
    let mut keys: Vec<i64> = Vec::new();
    let mut packets = 0usize;
    for line in text.lines() {
        if let Some(tb) = line.strip_prefix("#tb 0:") {
            let (n, d) = tb.trim().split_once('/')?;
            (num, den) = (n.trim().parse().ok()?, d.trim().parse().ok()?);
            continue;
        }
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        if fields.len() < 6 || fields[0] != "0" {
            continue;
        }
        packets += 1;
        if !fields.iter().any(|f| f.starts_with("F=")) {
            keys.push(fields[2].parse().ok()?);
        }
    }
    if num <= 0 || den <= 0 || packets < 2 {
        return None;
    }
    if keys.len() < 2 {
        return Some(i64::from(KEYFRAME_WINDOW_SECS) * 1000);
    }
    keys.sort_unstable();
    let mut gaps: Vec<i64> = keys.windows(2).map(|w| w[1] - w[0]).collect();
    gaps.sort_unstable();
    let gap = gaps[gaps.len() / 2];
    Some(((gap as i128 * num as i128 * 1000) / den as i128) as i64)
}

/// The frames of `item` at `ms` (in time order, two or more), read in
/// one pass: one each, or nothing for a time the pass didn't reach.
/// Nothing at all when the pass failed or its result can't be matched
/// to the times exactly; the seeks are then the way.
#[allow(clippy::too_many_arguments)]
pub(crate) fn read(ffmpeg: &Path, item: &klipp_media::MediaItem, ms: &[i64], width: u32, height: i64, after: &str, frame_bytes: usize) -> Option<Vec<Option<Vec<u8>>>> {
    let (first, last) = (*ms.first()?, *ms.last()?);
    let mut distinct = ms.to_vec();
    distinct.dedup();
    // A frame is picked when one of the times falls after the frame
    // before it and at or before it: the first frame at or after that
    // time. In the stream's own ticks (`pts`, shifted back to the file's
    // by the seek's), each time rounded to a tick as the seek rounds it.
    let start = format!("round({}/TB)", secs_text(first));
    let expr = distinct
        .iter()
        .map(|&t| {
            let at = format!("round({}/TB)", secs_text(t));
            format!("gte(pts+{start},{at})*(isnan(prev_pts)+lt(prev_pts+{start},{at}))")
        })
        .collect::<Vec<_>>()
        .join("+");
    let mut last_err = String::new();
    for plan in crate::decode_plans(ffmpeg) {
        let mut cmd = std::process::Command::new(ffmpeg);
        // `info`: the frames' times come back through showinfo.
        cmd.args(["-hide_banner", "-nostats", "-loglevel", "info"]);
        cmd.args(crate::decode_input_args(ffmpeg, plan, &item.video_codec));
        cmd.args(["-ss", &secs_text(first), "-t", &secs_text(last - first + PASS_TAIL_MS)]);
        cmd.args(klipp_media::input_args(&item.path).ok()?);
        let mut graph = format!("[0:v:0]select='{expr}',{}", crate::scale_filter(ffmpeg, plan, width, height));
        if !after.is_empty() {
            graph.push(',');
            graph.push_str(after);
        }
        graph.push_str(",format=rgba,showinfo[out]");
        cmd.args(["-an", "-sn", "-filter_complex", &graph, "-map", "[out]", "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "rgba", "-"]);
        let limit = (!matches!(plan, crate::Decode::Cpu)).then_some(crate::GPU_GRAB_LIMIT);
        let out = match crate::hold::output_within(&mut cmd, limit) {
            Ok(Some(out)) => out,
            Ok(None) => {
                tracing::warn!("one pass for frames at {ms:?} ms: the {plan:?} decode did not finish within {} s; trying the next", crate::GPU_GRAB_LIMIT.as_secs());
                continue;
            }
            Err(e) => {
                tracing::warn!("one pass for frames at {ms:?} ms: could not run {}: {e}", ffmpeg.display());
                return None;
            }
        };
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() {
            last_err = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
            continue;
        }
        let matched = match_frames(&stderr, &out.stdout, frame_bytes, first, ms);
        if matched.is_none() {
            tracing::warn!("one pass for frames at {ms:?} ms: its {} bytes didn't match the times; seeking instead", out.stdout.len());
        }
        return matched;
    }
    tracing::debug!("one pass for frames at {ms:?} ms failed on every decode ({last_err}); seeking instead");
    None
}

/// The pass's frames given to the times `ms`, from the times showinfo
/// printed for them (`stderr`) and the pictures (`stdout`). Every frame
/// has to be the one for some time, and every time's frame has to be
/// there or past the end of what was read, or nothing is matched.
pub(crate) fn match_frames(stderr: &str, stdout: &[u8], frame_bytes: usize, first: i64, ms: &[i64]) -> Option<Vec<Option<Vec<u8>>>> {
    let mut tb: Option<(i64, i64)> = None;
    let mut pts: Vec<i64> = Vec::new();
    for line in stderr.lines() {
        if !line.contains("Parsed_showinfo") {
            continue;
        }
        if let Some(rest) = line.split("config in time_base:").nth(1) {
            let text = rest.split(',').next()?.trim();
            let (n, d) = text.split_once('/')?;
            tb = Some((n.trim().parse().ok()?, d.trim().parse().ok()?));
        } else if line.contains("] n:") {
            let mut words = line.split_whitespace();
            let value = loop {
                let w = words.next()?;
                if w == "pts:" {
                    break words.next()?;
                }
                if let Some(v) = w.strip_prefix("pts:") {
                    break v;
                }
            };
            pts.push(value.parse().ok()?);
        }
    }
    let (num, den) = tb?;
    if num <= 0 || den <= 0 || frame_bytes == 0 || stdout.len() != pts.len() * frame_bytes {
        return None;
    }
    // As ffmpeg reckons it in the select: seconds over the time base, a
    // double, rounded half away from zero.
    let tick = num as f64 / den as f64;
    let ticks = |t: i64| ((t as f64 / 1000.0) / tick).round() as i64;
    let start = ticks(first);
    let mut used = vec![false; pts.len()];
    let mut out = Vec::with_capacity(ms.len());
    let mut missing = false;
    for &t in ms {
        let at = ticks(t);
        match pts.iter().position(|p| p + start >= at) {
            Some(k) if !missing => {
                used[k] = true;
                out.push(Some(stdout[k * frame_bytes..(k + 1) * frame_bytes].to_vec()));
            }
            Some(_) => return None,
            None => {
                missing = true;
                out.push(None);
            }
        }
    }
    used.iter().all(|u| *u).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_round_to_the_millisecond_as_a_seek_does() {
        assert_eq!(millis(0.0), Some(0));
        assert_eq!(millis(2.004), Some(2004));
        assert_eq!(millis(1.0005), Some(format!("{:.3}", 1.0005f64).replace('.', "").parse().unwrap()));
        assert_eq!(millis(359.25), Some(359_250));
        assert_eq!(millis(-0.1), None);
        assert_eq!(millis(f64::NAN), None);
        assert_eq!(secs_text(2004), "2.004");
        assert_eq!(secs_text(40), "0.040");
    }

    #[test]
    fn only_frames_closer_than_half_the_keyframe_spacing_share_a_pass() {
        // Keyframes 4 s apart: a second apart joins, three apart doesn't.
        let ms = [5000, 1000, 2000, 3000, 9000, 12000, 12000];
        assert_eq!(runs(&ms, 4000), vec![vec![1, 2, 3], vec![5, 6]]);
        // All keyframes: never a pass, whatever the spacing of the times.
        assert!(runs(&[0, 40, 80, 120], 40).is_empty());
        // Even a long run is cut, by span and by count.
        let dense: Vec<i64> = (0..100).map(|i| i * 500).collect();
        let cut = runs(&dense, 4000);
        assert!(cut.iter().all(|r| r.len() <= MAX_PASS_FRAMES && dense[*r.last().unwrap()] - dense[r[0]] <= MAX_PASS_SECS * 1000), "{cut:?}");
        assert_eq!(cut.iter().map(Vec::len).sum::<usize>(), 100);
    }

    #[test]
    fn the_keyframe_spacing_is_read_from_the_packets() {
        let listing = "#tb 0: 1/1000\n0,  -1265,  -1233,  16,  855449, 0xfe2647c7\n0,  -1249,  -1167,  16,  295, 0x94632747, F=0x0\n0,  2900,  2933,  16,  870614, 0x8817ad3f\n0,  2916,  2950,  16,  295, 0x94632747, F=0x0\n0,  7067,  7100,  16,  819198, 0x0589927f\n";
        // Gaps 4166 and 4167 ms: the median, the larger of two.
        assert_eq!(spacing_from_framecrc(listing), Some(4167));
        let intra = "#tb 0: 1/12288\n0, 0, 0, 512, 9, 0x1\n0, 512, 512, 512, 9, 0x1\n0, 1024, 1024, 512, 9, 0x1\n";
        assert_eq!(spacing_from_framecrc(intra), Some(41));
        let one_key = "#tb 0: 1/90000\n0, 0, 0, 3750, 9, 0x1\n0, 3750, 3750, 3750, 9, 0x1, F=0x0\n";
        assert_eq!(spacing_from_framecrc(one_key), Some(20_000));
        assert_eq!(spacing_from_framecrc("#tb 0: 1/1000\n"), None);
    }

    fn showinfo(pts: &[i64]) -> String {
        let mut s = String::from("[Parsed_showinfo_4 @ 0x1] config in time_base: 1/1000, frame_rate: 60/1\n");
        for (n, p) in pts.iter().enumerate() {
            s.push_str(&format!("[Parsed_showinfo_4 @ 0x1] n:{n:4} pts:{p:7} pts_time:{} duration: 16\n", *p as f64 / 1000.0));
        }
        s
    }

    #[test]
    fn each_time_gets_the_first_frame_at_or_after_it() {
        // Frames picked at 1500, 2000, 2017 ms (shifted back by the seek
        // to 1500); 2004 and 2010 both land on 2017's frame.
        let frames: Vec<u8> = (1..=3u8).flat_map(|k| [k; 4]).collect();
        let got = match_frames(&showinfo(&[0, 500, 517]), &frames, 4, 1500, &[1500, 2000, 2004, 2010]).unwrap();
        assert_eq!(got, vec![Some(vec![1; 4]), Some(vec![2; 4]), Some(vec![3; 4]), Some(vec![3; 4])]);
        // A time past what was read is left for a seek.
        let tail = match_frames(&showinfo(&[0, 500]), &frames[..8], 4, 1500, &[1500, 2000, 9000]).unwrap();
        assert_eq!(tail, vec![Some(vec![1; 4]), Some(vec![2; 4]), None]);
        // Seven-digit times print with no space after `pts:`.
        let long = match_frames(&showinfo(&[1_234_567]), &frames[..4], 4, 0, &[1_234_567]).unwrap();
        assert_eq!(long, vec![Some(vec![1; 4])]);
    }

    #[test]
    fn a_pass_that_does_not_add_up_is_not_used() {
        let frames: Vec<u8> = (1..=3u8).flat_map(|k| [k; 4]).collect();
        // A frame no time asked for.
        assert!(match_frames(&showinfo(&[0, 300, 500]), &frames, 4, 1500, &[1500, 2000]).is_none());
        // Pictures and times of different counts.
        assert!(match_frames(&showinfo(&[0, 500]), &frames, 4, 1500, &[1500, 2000]).is_none());
        // No time base.
        assert!(match_frames("[Parsed_showinfo_4 @ 0x1] n:   0 pts:      0\n", &frames[..4], 4, 0, &[0]).is_none());
    }
}
