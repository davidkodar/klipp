//! What the cut uses, for consolidating to an archive (#77): the media the
//! sequences reference, the stretches of each file they use with handles,
//! and the timeline moved onto files trimmed to those stretches.
//!
//! The planning here is pure and tested. The cutting runs ffmpeg as a
//! stream copy, never a re-encode of a video: the cut starts on the
//! keyframe at or before the stretch's start, found with ffprobe, so the
//! copy begins on a whole picture and no quality is lost. Every cut is
//! checked by decoding a frame inside it and the same frame from the
//! source; a cut whose frame differs is thrown away and the whole file is
//! copied instead, so a wrong remap cannot pass quietly.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use klipp_core::{MediaId, Timecode};
use klipp_media::{MediaItem, MediaItemKind, MediaPool};
use klipp_timeline::{Clip, Sequence};

/// The media the sequences use: every clip's, an audition's other takes'
/// too (#3209), the parents of subclips used, and the separate recordings
/// synced to used media.
pub fn used_media(pool: &MediaPool, sequences: &[Sequence]) -> HashSet<MediaId> {
    let mut used: HashSet<MediaId> = sequences
        .iter()
        .flat_map(|s| s.tracks.iter())
        .flat_map(|t| t.clips.iter())
        .flat_map(Clip::with_takes)
        .filter_map(|c| c.media_id())
        .collect();
    let direct: Vec<MediaId> = used.iter().copied().collect();
    for id in direct {
        if let Some(item) = pool.get(&id) {
            used.extend(item.parent_media_id);
            used.extend(item.synced_audio.as_ref().map(|s| s.audio_media_id));
        }
    }
    used
}

/// One stretch of a file kept, in seconds of the file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range {
    pub start: f64,
    pub end: f64,
}

impl Range {
    pub fn len(&self) -> f64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// The stretches of each file the clips use, each widened by `handle`
/// seconds both ways, clamped to the file, overlapping ones merged. Keyed
/// by path: a subclip shares its parent's file. A file a recording is
/// synced to, or one of a spanned card clip, is not listed: it is kept
/// whole.
pub fn used_ranges(pool: &MediaPool, sequences: &[Sequence], handle: f64) -> HashMap<PathBuf, Vec<Range>> {
    let mut whole: HashSet<PathBuf> = HashSet::new();
    for item in pool.all() {
        if let Some(sync) = &item.synced_audio {
            if let Some(audio) = pool.get(&sync.audio_media_id) {
                whole.insert(audio.path.clone());
            }
        }
        if item.span.is_some() {
            whole.insert(item.path.clone());
        }
    }
    let mut raw: HashMap<PathBuf, (f64, Vec<Range>)> = HashMap::new();
    for track in sequences.iter().flat_map(|s| s.tracks.iter()) {
        // An audition's other takes keep their stretches too (#3209).
        for clip in track.clips.iter().flat_map(Clip::with_takes) {
            let Some(item) = clip.media_id().and_then(|id| pool.get(&id)) else { continue };
            if item.kind != MediaItemKind::File || whole.contains(&item.path) {
                continue;
            }
            let duration = item.duration.to_secs_f64();
            let (a, b) = (clip.source_in.to_secs_f64(), clip.source_out.to_secs_f64());
            // A transition reads the outgoing clip's file past its out and
            // the incoming's before its in, by up to its length at the
            // clip's speed: kept whatever the handle.
            let speed = clip.speed.abs().max(1e-6);
            let into = track.transitions.iter().filter(|t| t.clip_b_id == clip.id).map(|t| t.duration.to_secs_f64() * speed).fold(0.0, f64::max);
            let out_of = track.transitions.iter().filter(|t| t.clip_a_id == clip.id).map(|t| t.duration.to_secs_f64() * speed).fold(0.0, f64::max);
            let (before, after) = if clip.speed < 0.0 { (out_of.max(into), out_of.max(into)) } else { (into, out_of) };
            let entry = raw.entry(item.path.clone()).or_insert((duration, Vec::new()));
            entry.0 = entry.0.max(duration);
            entry.1.push(Range { start: a.min(b) - before, end: a.max(b) + after });
        }
    }
    raw.into_iter().map(|(path, (duration, ranges))| (path, merge(ranges, handle, duration))).collect()
}

/// Widen by `handle`, clamp to `0..duration` (when it is known), merge.
fn merge(mut ranges: Vec<Range>, handle: f64, duration: f64) -> Vec<Range> {
    let top = if duration > 0.0 { duration } else { f64::INFINITY };
    for r in &mut ranges {
        r.start = (r.start - handle).max(0.0);
        r.end = (r.end + handle).min(top);
    }
    ranges.retain(|r| !r.is_empty());
    ranges.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out: Vec<Range> = Vec::new();
    for r in ranges {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

/// Whether trimming a file to `ranges` is worth it: they leave out at
/// least a fifth of it.
pub fn worth_trimming(ranges: &[Range], duration: f64) -> bool {
    duration > 0.0 && ranges.iter().map(Range::len).sum::<f64>() < duration * 0.8
}

/// A file cut from a source: where it starts in the source (its first
/// frame, the keyframe the copy began on), how long it is, and where it
/// was written.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub duration: f64,
    pub path: PathBuf,
}

/// How far past a cut's end a clip may reach and still be held by it: a
/// frame or two of container-versus-stream rounding (#963 review).
const END_SLACK_SECS: f64 = 0.1;

impl Segment {
    fn holds(&self, a: f64, b: f64) -> bool {
        a >= self.start - 1e-6 && b <= self.start + self.duration + END_SLACK_SECS
    }
}

/// The cut files with a clip no segment of theirs holds (#963 review):
/// remapped, such a clip would keep its whole-file times on a cut file
/// and play the wrong frames, so the file is copied whole instead.
pub fn unheld(pool: &MediaPool, sequences: &[Sequence], cuts: &HashMap<PathBuf, Vec<Segment>>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for clip in sequences.iter().flat_map(|s| s.tracks.iter()).flat_map(|t| t.clips.iter()).flat_map(Clip::with_takes) {
        let Some(item) = clip.media_id().and_then(|id| pool.get(&id)) else { continue };
        let Some(segments) = cuts.get(&item.path) else { continue };
        let (a, b) = (clip.source_in.to_secs_f64(), clip.source_out.to_secs_f64());
        if !segments.iter().any(|s| s.holds(a.min(b), a.max(b))) && !out.contains(&item.path) {
            out.push(item.path.clone());
        }
    }
    out
}

/// The pool and sequences moved onto the cut files: each clip on the
/// segment that holds its stretch, its source times counted from the
/// segment's start; each item that has clips on more than one segment
/// split into one item per segment, the first keeping its id. Items of a
/// file not in `cuts` are left as they are.
pub fn remap(pool: &MediaPool, sequences: &[Sequence], cuts: &HashMap<PathBuf, Vec<Segment>>) -> (MediaPool, Vec<Sequence>) {
    let mut seqs = sequences.to_vec();
    // (item, segment index) → the id of the item on that segment.
    let mut ids: HashMap<(MediaId, usize), MediaId> = HashMap::new();
    let mut first_segment: HashMap<MediaId, usize> = HashMap::new();
    let mut onto_segment = |clip: &mut Clip| {
        let Some(item) = clip.media_id().and_then(|id| pool.get(&id)) else { return };
        let Some(segments) = cuts.get(&item.path) else { return };
        let (a, b) = (clip.source_in.to_secs_f64(), clip.source_out.to_secs_f64());
        let Some(j) = segments.iter().position(|s| s.holds(a.min(b), a.max(b))) else { return };
        let first = *first_segment.entry(item.id).or_insert(j);
        let id = *ids.entry((item.id, j)).or_insert_with(|| if j == first { item.id } else { MediaId::new() });
        let shift = segments[j].start;
        let moved = |t: Timecode| Timecode::from_secs_f64((t.to_secs_f64() - shift).max(0.0), t.rate);
        clip.source_in = moved(clip.source_in);
        clip.source_out = moved(clip.source_out);
        clip.kind = klipp_timeline::ClipKind::Media { media_id: id };
    };
    for clip in seqs.iter_mut().flat_map(|s| s.tracks.iter_mut()).flat_map(|t| t.clips.iter_mut()) {
        onto_segment(clip);
        // An audition's other takes moved with it (#3209).
        clip.takes_mut().for_each(&mut onto_segment);
    }
    let mut out = MediaPool::new();
    out.bins = pool.bins.clone();
    for item in pool.all() {
        let Some(segments) = cuts.get(&item.path) else {
            out.add(item.clone());
            continue;
        };
        // An item with no clips of its own (a subclip's parent) goes on
        // the first segment.
        let mut on: Vec<(usize, MediaId)> = ids.iter().filter(|((m, _), _)| *m == item.id).map(|((_, j), id)| (*j, *id)).collect();
        if on.is_empty() {
            on.push((0, item.id));
        }
        on.sort_by_key(|(j, _)| *j);
        let parts = on.len();
        for (n, (j, id)) in on.into_iter().enumerate() {
            let mut moved = on_segment(item, &segments[j]);
            moved.id = id;
            if parts > 1 && n > 0 {
                moved.filename = format!("{} ({})", item.filename, n + 1);
            }
            out.add(moved);
        }
    }
    // Synced recordings: the picture's times moved by its cut's start.
    let shifts: HashMap<MediaId, f64> = out
        .all()
        .filter_map(|m| cuts.values().flatten().find(|s| s.path == m.path).map(|s| (m.id, s.start)))
        .collect();
    let ids: Vec<MediaId> = out.all().map(|m| m.id).collect();
    for id in ids {
        let shift = shifts.get(&id).copied().unwrap_or(0.0);
        if let Some(sync) = out.get_mut(&id).and_then(|m| m.synced_audio.as_mut()) {
            sync.offset_secs += shift;
        }
    }
    (out, seqs)
}

/// `item` on `segment`: its path, length and source-timed fields moved.
fn on_segment(item: &MediaItem, segment: &Segment) -> MediaItem {
    let shift = segment.start;
    let mut m = item.clone();
    m.path = segment.path.clone();
    m.duration = Timecode::from_secs_f64(segment.duration, item.duration.rate.max(1));
    let inside = |t: Timecode| {
        let s = t.to_secs_f64() - shift;
        (0.0..=segment.duration).contains(&s).then(|| Timecode::from_secs_f64(s, t.rate))
    };
    m.default_source_in = item.default_source_in.and_then(inside);
    m.default_source_out = item.default_source_out.and_then(inside);
    if m.default_source_in.is_none() || m.default_source_out.is_none() {
        m.default_source_in = None;
        m.default_source_out = None;
    }
    m.markers.retain(|mk| mk.secs >= shift && mk.secs <= shift + segment.duration);
    for mk in &mut m.markers {
        mk.secs -= shift;
    }
    m.shot.start_timecode = item.shot.start_timecode.as_deref().and_then(|tc| shifted_timecode(tc, item.fps, shift));
    m.file_size_bytes = std::fs::metadata(&segment.path).map(|md| md.len()).unwrap_or(0);
    // Made from the whole file: made again from the cut one.
    m.audio_cache = None;
    m.waveform_data = None;
    m.skim_file = None;
    m.proxy_path = None;
    m.proxy_ready = false;
    m.fingerprint = None;
    m.truncated_from_secs = None;
    m
}

/// "hh:mm:ss:ff" moved on by `secs`, at `fps`.
fn shifted_timecode(tc: &str, fps: klipp_core::Fps, secs: f64) -> Option<String> {
    let rate = fps.as_f64();
    if rate <= 0.0 {
        return None;
    }
    let start = klipp_core::sync::timecode_to_secs(tc, fps)?;
    let nominal = rate.round() as i64;
    let frames = ((start + secs) * rate).round() as i64;
    let (f, s) = (frames % nominal, frames / nominal);
    Some(format!("{:02}:{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60, f))
}

/// How a file is cut: video as a stream copy from a keyframe; sound alone
/// to the sample when it is PCM (re-written in its own PCM format, which
/// loses nothing).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Cutting {
    VideoCopy,
    Pcm,
}

fn cutting(item: &MediaItem) -> Option<Cutting> {
    if item.is_still || item.kind != MediaItemKind::File {
        return None;
    }
    if !item.video_codec.is_empty() {
        return Some(Cutting::VideoCopy);
    }
    item.audio_codec.starts_with("pcm_").then_some(Cutting::Pcm)
}

/// Whether `item`'s file can be cut at all.
pub fn can_cut(item: &MediaItem) -> bool {
    cutting(item).is_some()
}

/// Seconds from ffprobe's text.
fn probe_secs(path: &Path, args: &[&str]) -> Vec<f64> {
    let Ok(input) = klipp_media::input_args(path) else { return Vec::new() };
    let out = Command::new("ffprobe").args(["-v", "error"]).args(args).args(&input).output();
    let Ok(out) = out else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().trim_end_matches(',').parse::<f64>().ok()).collect()
}

/// The container's start time and the first video stream's, in seconds.
fn starts(path: &Path) -> (f64, f64) {
    let format = probe_secs(path, &["-show_entries", "format=start_time", "-of", "csv=p=0"]).first().copied().unwrap_or(0.0);
    let video = probe_secs(path, &["-select_streams", "v:0", "-show_entries", "stream=start_time", "-of", "csv=p=0"]).first().copied().unwrap_or(format);
    (format, video)
}

/// The last keyframe at or before `at` (seconds from the video's start).
fn keyframe_before(path: &Path, video_start: f64, at: f64) -> Option<f64> {
    let from = (video_start + at - 30.0).max(0.0);
    let interval = format!("{from}%{}", video_start + at + 0.001);
    probe_secs(path, &["-select_streams", "v:0", "-skip_frame", "nokey", "-show_entries", "frame=pts_time", "-of", "csv=p=0", "-read_intervals", &interval])
        .into_iter()
        .map(|t| t - video_start)
        .filter(|t| *t <= at + 1e-6)
        .reduce(f64::max)
}

/// The first frame shown at or after `at` seconds into the file (from
/// the container's start), small and grey, for comparing two files.
fn frame_at(path: &Path, at: f64) -> Option<Vec<u8>> {
    let out = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-ss", &format!("{at:.6}")])
        .args(klipp_media::input_args(path).ok()?)
        .args(["-frames:v", "1", "-vf", "scale=64:36", "-pix_fmt", "gray", "-f", "rawvideo", "-"])
        .output()
        .ok()?;
    (out.status.success() && !out.stdout.is_empty()).then_some(out.stdout)
}

/// Cut `range` of `item`'s file into `dst`. Returns the segment, or why
/// the whole file should be copied instead.
pub fn cut(item: &MediaItem, range: Range, dst: &Path) -> Result<Segment, String> {
    let how = cutting(item).ok_or("not a file that can be cut")?;
    let src = &item.path;
    let (format_start, video_start) = starts(src);
    let start = match how {
        Cutting::VideoCopy => keyframe_before(src, video_start, range.start).ok_or("no keyframe found before the stretch")?,
        Cutting::Pcm => range.start,
    };
    // A stretch that reaches the file's end is cut to the end of the
    // file, not to its probed length: a clip may run a frame past that.
    let to_end = range.end >= item.duration.to_secs_f64() - 1e-3;
    let mut duration = range.end - start;
    // ffmpeg's -ss counts from the container's start; a hair past the
    // keyframe so the seek lands on it, not the one before.
    let seek = video_start - format_start + start + 0.0001;
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-nostdin", "-v", "error", "-y", "-ss", &format!("{seek:.6}")]).args(klipp_media::input_args(src)?);
    if !to_end {
        cmd.args(["-t", &format!("{duration:.6}")]);
    }
    cmd.args(["-map", "0"]);
    match how {
        Cutting::VideoCopy => cmd.args(["-c", "copy", "-avoid_negative_ts", "make_zero"]),
        Cutting::Pcm => cmd.args(["-c:a", &item.audio_codec]),
    };
    let out = cmd.arg(dst).output().map_err(|e| format!("ffmpeg did not run: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(dst);
        return Err(format!("ffmpeg could not cut it: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or_default()));
    }
    if to_end {
        duration = duration.max(probe_secs(dst, &["-show_entries", "format=duration", "-of", "csv=p=0"]).first().copied().unwrap_or(0.0));
    }
    let segment = Segment { start, duration, path: dst.to_path_buf() };
    if how == Cutting::VideoCopy {
        if let Err(why) = check(src, (format_start, video_start), &segment) {
            let _ = std::fs::remove_file(dst);
            return Err(why);
        }
    }
    Ok(segment)
}

/// The cut's frame half a second in (or halfway, when shorter) is the
/// source's frame at the same moment, counted as the app counts it: from
/// each file's first video frame.
fn check(src: &Path, (src_format, src_video): (f64, f64), segment: &Segment) -> Result<(), String> {
    let (cut_format, cut_video) = starts(&segment.path);
    let into = (segment.duration / 2.0).min(0.5);
    let theirs = frame_at(src, src_video - src_format + segment.start + into);
    let ours = frame_at(&segment.path, cut_video - cut_format + into);
    match (theirs, ours) {
        (Some(a), Some(b)) if a == b => Ok(()),
        (Some(_), Some(_)) => Err("the cut's frames do not line up with the source's".into()),
        _ => Err("a frame of the cut could not be read back".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_timeline::{Clip, Track, TrackKind};

    fn t(secs: f64) -> Timecode {
        Timecode::from_secs_f64(secs, 24)
    }

    fn item(path: &str, secs: f64) -> MediaItem {
        let mut m = MediaItem::synthetic_for_test(path.into(), None);
        m.duration = t(secs);
        m.video_codec = "h264".into();
        m
    }

    fn seq_with(clips: Vec<Clip>) -> Sequence {
        let mut seq = Sequence::new_default();
        seq.tracks.clear();
        let mut v = Track::new("Video 1", TrackKind::Video);
        v.clips = clips;
        seq.tracks.push(v);
        seq
    }

    #[test]
    fn only_media_on_a_timeline_is_used_with_subclip_parents_and_synced_sound() {
        let mut pool = MediaPool::new();
        let parent = pool.add(item("/m/a.mov", 60.0));
        let mut sub = item("/m/a.mov", 60.0);
        sub.parent_media_id = Some(parent);
        let recorder = pool.add(item("/m/rec.wav", 60.0));
        sub.synced_audio = Some(klipp_core::SyncedAudio { audio_media_id: recorder, offset_secs: 1.0, method: klipp_core::SyncMethod::Timecode });
        let sub = pool.add(sub);
        let unused = pool.add(item("/m/b.mov", 60.0));
        let seq = seq_with(vec![Clip::media(sub, t(0.0), t(2.0), t(10.0), t(12.0))]);
        let used = used_media(&pool, &[seq]);
        assert!(used.contains(&sub) && used.contains(&parent) && used.contains(&recorder));
        assert!(!used.contains(&unused));
    }

    #[test]
    fn ranges_take_handles_merge_and_stay_in_the_file() {
        let mut pool = MediaPool::new();
        let a = pool.add(item("/m/a.mov", 100.0));
        let seq = seq_with(vec![
            Clip::media(a, t(0.0), t(2.0), t(0.5), t(2.5)),
            Clip::media(a, t(2.0), t(4.0), t(3.0), t(5.0)),
            Clip::media(a, t(4.0), t(6.0), t(50.0), t(52.0)),
            Clip::media(a, t(6.0), t(8.0), t(98.0), t(99.5)),
        ]);
        let ranges = &used_ranges(&pool, &[seq], 1.0)[Path::new("/m/a.mov")];
        let r = |a: f64, b: f64| Range { start: a, end: b };
        assert_eq!(ranges.len(), 3, "{ranges:?}");
        assert!((ranges[0].start - 0.0).abs() < 1e-6 && (ranges[0].end - 6.0).abs() < 1e-6, "clamped at 0, merged: {ranges:?}");
        assert!((ranges[1].start - r(49.0, 53.0).start).abs() < 1e-6 && (ranges[1].end - 53.0).abs() < 1e-6);
        assert!((ranges[2].end - 100.0).abs() < 1e-6, "clamped at the end: {ranges:?}");
        assert!(worth_trimming(ranges, 100.0));
        assert!(!worth_trimming(&[r(0.0, 90.0)], 100.0));
    }

    #[test]
    fn clips_move_onto_their_segments_and_split_items() {
        let mut pool = MediaPool::new();
        let mut a = item("/m/a.mov", 100.0);
        a.markers.push(klipp_core::MediaMarker { secs: 50.5, duration_secs: 0.0, name: "here".into(), kind: klipp_core::MediaMarkerKind::Note });
        a.shot.start_timecode = Some("01:00:00:00".into());
        let a = pool.add(a);
        let b = pool.add(item("/m/b.mov", 10.0));
        let seq = seq_with(vec![
            Clip::media(a, t(0.0), t(2.0), t(3.0), t(5.0)),
            Clip::media(a, t(2.0), t(4.0), t(50.0), t(52.0)),
            Clip::media(b, t(4.0), t(6.0), t(1.0), t(3.0)),
        ]);
        let cuts = HashMap::from([(
            PathBuf::from("/m/a.mov"),
            vec![
                Segment { start: 2.0, duration: 4.0, path: "/x/a_1.mov".into() },
                Segment { start: 48.0, duration: 6.0, path: "/x/a_2.mov".into() },
            ],
        )]);
        let (pool2, seqs) = remap(&pool, &[seq], &cuts);
        let clips = &seqs[0].tracks[0].clips;
        assert_eq!(clips[0].media_id(), Some(a), "the first segment keeps the id");
        assert!((clips[0].source_in.to_secs_f64() - 1.0).abs() < 1e-3);
        let second = clips[1].media_id().unwrap();
        assert_ne!(second, a, "the second segment is its own item");
        assert!((clips[1].source_in.to_secs_f64() - 2.0).abs() < 1e-3 && (clips[1].source_out.to_secs_f64() - 4.0).abs() < 1e-3);
        assert_eq!(clips[2].media_id(), Some(b), "another file's clip is untouched");
        assert!((clips[2].source_in.to_secs_f64() - 1.0).abs() < 1e-3);
        let on_second = pool2.get(&second).unwrap();
        assert_eq!(on_second.path, PathBuf::from("/x/a_2.mov"));
        assert!((on_second.duration.to_secs_f64() - 6.0).abs() < 0.05);
        assert_eq!(on_second.markers.len(), 1);
        assert!((on_second.markers[0].secs - 2.5).abs() < 1e-6);
        assert_eq!(on_second.shot.start_timecode.as_deref(), Some("01:00:48:00"));
        assert!(pool2.get(&a).unwrap().markers.is_empty(), "the marker is not on the first segment");
        assert_eq!(pool2.get(&b).unwrap().path, PathBuf::from("/m/b.mov"));
    }

    /// #963 review: a clip a frame past the file's probed end is still
    /// held by the cut that reaches the end; one well past any cut is
    /// reported, so its file is copied whole rather than remapped wrong.
    #[test]
    fn a_clip_no_cut_holds_sends_its_file_back_to_a_whole_copy() {
        let mut pool = MediaPool::new();
        let a = pool.add(item("/m/a.mov", 100.0));
        let cuts = HashMap::from([(PathBuf::from("/m/a.mov"), vec![Segment { start: 80.0, duration: 20.0, path: "/x/a_1.mov".into() }])]);
        let seq = seq_with(vec![Clip::media(a, t(0.0), t(5.0), t(95.0), t(100.04))]);
        assert!(unheld(&pool, std::slice::from_ref(&seq), &cuts).is_empty(), "a frame past the end is held");
        let seq = seq_with(vec![Clip::media(a, t(0.0), t(5.0), t(10.0), t(15.0))]);
        assert_eq!(unheld(&pool, &[seq], &cuts), vec![PathBuf::from("/m/a.mov")]);
    }

    /// #963 review: a transition reads past the outgoing clip's out and
    /// before the incoming's in; those frames are kept even with no handle.
    #[test]
    fn ranges_keep_what_transitions_read() {
        let mut pool = MediaPool::new();
        let a = pool.add(item("/m/a.mov", 100.0));
        let out_clip = Clip::media(a, t(0.0), t(5.0), t(10.0), t(15.0));
        let in_clip = Clip::media(a, t(5.0), t(10.0), t(50.0), t(55.0));
        let mut seq = seq_with(vec![out_clip.clone(), in_clip.clone()]);
        seq.tracks[0].transitions.push(klipp_timeline::Transition { id: klipp_core::TransitionId::new(), kind: klipp_timeline::TransitionKind::CrossDissolve, clip_a_id: out_clip.id, clip_b_id: in_clip.id, duration: t(2.0), user_params: Default::default(), alignment: Default::default() });
        let ranges = &used_ranges(&pool, &[seq], 0.0)[Path::new("/m/a.mov")];
        assert!((ranges[0].end - 17.0).abs() < 1e-6, "the outgoing clip reads 2 s past its out: {ranges:?}");
        assert!((ranges[1].start - 48.0).abs() < 1e-6, "the incoming reads 2 s before its in: {ranges:?}");
    }

    /// A real file cut and checked: needs ffmpeg, and skips without it.
    #[test]
    fn a_cut_starts_on_a_keyframe_and_its_frames_are_the_sources() {
        let have = |bin: &str| Command::new(bin).arg("-version").output().is_ok_and(|o| o.status.success());
        if !have("ffmpeg") || !have("ffprobe") {
            eprintln!("skipped: no ffmpeg");
            return;
        }
        let dir = std::env::temp_dir().join(format!("klipp_cut_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("src.mp4");
        // Ten seconds, a keyframe every two, a frame counter drawn by testsrc.
        let made = Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=160x90:rate=25:duration=10", "-c:v", "libx264", "-g", "50", "-keyint_min", "50", "-sc_threshold", "0", "-pix_fmt", "yuv420p"])
            .arg(&src)
            .status();
        if !made.is_ok_and(|s| s.success()) {
            eprintln!("skipped: ffmpeg could not make the test file");
            return;
        }
        let mut m = item(src.to_str().unwrap(), 10.0);
        m.fps = klipp_core::Fps::new(25, 1);
        let segment = cut(&m, Range { start: 5.0, end: 7.5 }, &dir.join("cut.mp4")).expect("the cut is made and checked");
        assert!((segment.start - 4.0).abs() < 1e-3, "starts on the keyframe at 4 s: {}", segment.start);
        assert!((segment.duration - 3.5).abs() < 1e-3);
        // A frame well into the cut is the source's at the same moment.
        let (sf, sv) = starts(&src);
        let (cf, cv) = starts(&segment.path);
        assert_eq!(frame_at(&src, sv - sf + 6.2), frame_at(&segment.path, cv - cf + 2.2));
        assert_ne!(frame_at(&src, sv - sf + 6.2), frame_at(&segment.path, cv - cf + 1.0), "and not any frame");
        // A stretch to the file's end is cut to the end of the file.
        let tail = cut(&m, Range { start: 9.0, end: 10.0 }, &dir.join("tail.mp4")).expect("the tail is cut");
        assert!((tail.start - 8.0).abs() < 1e-3 && tail.duration >= 1.95, "{tail:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
