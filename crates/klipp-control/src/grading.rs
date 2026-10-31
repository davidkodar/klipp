//! Measuring and grading: faces and skin, shot spans, a clip's measures,
//! looks and their targets, the mix's loudness, grade limits (#826).

use crate::poison::Recover;
#[allow(unused_imports)]
use super::*;

/// A media item's file and duration, by id.
pub(crate) fn media_path_and_duration(ctx: &ControlContext, media_id: &str) -> Result<(std::path::PathBuf, f64), String> {
    let id = klipp_core::MediaId(media_id.parse().map_err(|e| format!("media_id is not a uuid: {e}"))?);
    let pool = ctx.media_pool.read().recovered();
    let item = pool.get(&id).ok_or_else(|| format!("no media item {}", id.0))?;
    if item.offline || item.kind != klipp_media::MediaItemKind::File {
        return Err(format!("{} has no file to read", item.filename));
    }
    Ok((item.path.clone(), item.duration.to_secs_f64()))
}

/// An absolute output path the caller gave, or one in the temp dir.
/// A sample's faces as boxes. Its people's heads stand in only when no
/// face detector looked; when one did and found no face, there is none
/// to read — the back of a head measured as a face failed every skin
/// check on hair (#326).
pub(crate) fn face_or_head_regions(framing: &klipp_analysis::framing::Framing, s: &klipp_analysis::framing::FrameSample) -> Vec<[f32; 4]> {
    if !s.faces.is_empty() {
        s.faces.iter().map(|f| [f.x, f.y, f.w, f.h]).collect()
    } else if framing.detects_faces() {
        Vec::new()
    } else {
        s.people.iter().map(|p| klipp_color::measure::head_region([p.0, p.1, p.2, p.3])).collect()
    }
}


/// The skin regions to read in a frame: each detected face, its box
/// drawn in a little so hair and background stay out; with no face
/// model, each person's skin cluster, else the head's place in the box.
pub(crate) fn skin_regions(ctx: &ControlContext, secs: f64, data: &[u8], w: u32, h: u32) -> Vec<[f32; 4]> {
    let seq = match ctx.sequence.read() { Ok(s) => s, Err(_) => return Vec::new() };
    let pool = ctx.media_pool.read().recovered();
    let tc = klipp_core::Timecode::from_secs_f64(secs.max(0.0), seq.fps.rate());
    let Some((clip, item)) = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible).find_map(|t| {
        let (_, c) = t.clip_at_timecode(tc)?;
        let item = pool.get(&c.media_id()?)?;
        c.enabled.then_some((c.clone(), item.clone()))
    }) else { return Vec::new() };
    let source = clip.source_secs_at((secs - clip.timeline_in.to_secs_f64()).max(0.0), seq.fps.as_f64());
    let Some(framing) = klipp_analysis::report::MediaAnalysis::load(&item.path).framing else { return Vec::new() };
    let Some(sample) = framing.people_at(source, 4.0) else { return Vec::new() };
    let source_h = if item.source_rotation.rem_euclid(180) == 90 { item.resolution.width } else { item.resolution.height };
    skin_regions_in(sample, framing.detects_faces(), source_h, data, w, h)
}

/// [`skin_regions`] for one framing `sample` of a file `source_h` pixels
/// tall, with `detects_faces` whether a face model looked.
pub(crate) fn skin_regions_in(sample: &klipp_analysis::framing::FrameSample, detects_faces: bool, source_h: u32, data: &[u8], w: u32, h: u32) -> Vec<[f32; 4]> {
    // Too few of the file's own pixels to read skin from (#2493): on a
    // low-resolution clip a face a dozen pixels tall read as background,
    // and failed the look.
    let readable = |fh: f32| source_h == 0 || fh * source_h as f32 >= MIN_SKIN_PX;
    if !sample.faces.is_empty() {
        // The cheek band between the eyes and the mouth, which is skin
        // and nothing else; the box drawn in, when the landmarks are no
        // good.
        return sample
            .faces
            .iter()
            .filter(|f| readable(f.h))
            .map(|f| klipp_color::measure::skin_band(&f.landmarks).unwrap_or([f.x + f.w * 0.12, f.y + f.h * 0.15, f.w * 0.76, f.h * 0.72]))
            .collect();
    }
    // A face detector looked and saw no face: someone walking away, or
    // too small. There is no skin to read, and the back of a head read
    // as one failed every skin check on hair.
    if detects_faces {
        return Vec::new();
    }
    // A person's skin, where their box has any: a head's place guessed
    // in a box with none is the background behind them (#2493).
    sample.people.iter().filter_map(|p| klipp_color::measure::skin_region(data, w, h, [p.0, p.1, p.2, p.3])).filter(|r| readable(r[3])).collect()
}

/// The fewest of the file's own pixel rows a face or skin region needs to
/// be read as skin (#2493).
const MIN_SKIN_PX: f32 = 24.0;

/// The face's path across a clip, as window keys in clip time.
///
/// A skin key is a band of hue, and a red jumper, a brick wall, a
/// wooden table and a tanned hand all sit inside it — a hue key alone
/// turns every one of them when it turns the face. A window says
/// *where*, and tracking it says where *now*: without the tracking a
/// window is only good for a locked-off camera, and the face walks out
/// of it half way through the shot, taking the correction with it.
///
/// Read from the framing analysis, which already holds face boxes over
/// time, so this costs no inference. Empty when the clip has no
/// analysis or no face in it, and the secondary is then keyed on hue
/// alone as before.
pub(crate) fn face_track(ctx: &ControlContext, clip_id: &str) -> Vec<klipp_document::ops::WindowKey> {
    let Ok(seq) = ctx.sequence.read() else { return Vec::new() };
    let pool = ctx.media_pool.read().recovered();
    let Ok(uuid) = clip_id.parse() else { return Vec::new() };
    let id = klipp_core::ClipId(uuid);
    let Some(clip) = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == id) else { return Vec::new() };
    let Some(item) = clip.media_id().and_then(|m| pool.get(&m)) else { return Vec::new() };
    let Some(framing) = klipp_analysis::report::MediaAnalysis::load(&item.path).framing else { return Vec::new() };
    let (s_in, s_out) = (clip.source_in.to_secs_f64(), clip.source_out.to_secs_f64());
    let fps = seq.fps.as_f64();
    let mut keys: Vec<klipp_document::ops::WindowKey> = framing
        .samples
        .iter()
        .filter(|s| s.secs >= s_in && s.secs < s_out)
        .filter_map(|s| {
            // The biggest face in the sample: the subject. A window on
            // a face in the background would leave the subject's own
            // face outside the correction.
            let b = face_or_head_regions(&framing, s).into_iter().max_by(|a, b| (a[2] * a[3]).total_cmp(&(b[2] * b[3])))?;
            Some(klipp_document::ops::WindowKey { at: clip.elapsed_for_source_secs(s.secs, fps), x: b[0], y: b[1], w: b[2], h: b[3] })
        })
        .collect();
    keys.sort_by(|a, b| a.at.total_cmp(&b.at));
    // A window keyframed every few frames is neither more accurate nor
    // cheaper to read; thirty samples across a shot is a tracker's
    // worth of path.
    if keys.len() > 30 {
        let step = keys.len() as f64 / 30.0;
        keys = (0..30).map(|i| keys[((i as f64 * step) as usize).min(keys.len() - 1)]).collect();
    }
    keys
}

/// A clip's span on the timeline, by its id.
pub(crate) fn shot_span(ctx: &ControlContext, clip_id: &str) -> Option<(f64, f64)> {
    let id = klipp_core::ClipId(clip_id.parse().ok()?);
    let seq = ctx.sequence.read().ok()?;
    seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == id).map(|c| (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64()))
}

/// A clip read the way a colourist checks a shot: three frames across
/// it, not one. One frame lies — a flash, a passer-by, a cloud — and
/// the drift between them is itself a finding.
/// A clip's detected shots in timeline seconds: the clip cut at the shot
/// boundaries its media's analysis found. Slivers under half a second
/// are dropped; when there are more than `max`, the longest `max` are
/// kept, longest first. A clip with no analysis is one span, itself.
pub(crate) fn clip_shot_spans(clip: &klipp_timeline::Clip, pool: &klipp_media::MediaPool, fps: f64, max: usize) -> Vec<(f64, f64)> {
    let item = clip.media_id().and_then(|m| pool.get(&m));
    let media_fps = item.map(|item| item.fps.as_f64()).filter(|f| *f > 0.0).unwrap_or(fps);
    let boundaries = item
        .and_then(|item| klipp_analysis::report::MediaAnalysis::load(&item.path).shots)
        .map(|s| s.boundaries)
        .unwrap_or_default();
    // The boundaries on the timeline, through the clip's speed and in its
    // direction (#617), each on the first frame that shows the new shot
    // (#691); then cut the span there.
    let from = clip.timeline_in.to_secs_f64();
    let at: Vec<f64> = boundaries.iter().filter_map(|b| clip.first_frame_showing(*b, media_fps, fps)).map(|f| f as f64 / fps).collect();
    shot_spans(from, clip.timeline_out.to_secs_f64(), from, 1.0, &at, max)
}

/// [`clip_shot_spans`] on numbers: the timeline span `from..to` of a
/// clip starting `source_in` into its media at `speed`, cut at the
/// media's shot `boundaries` (source seconds).
pub(crate) fn shot_spans(from: f64, to: f64, source_in: f64, speed: f64, boundaries: &[f64], max: usize) -> Vec<(f64, f64)> {
    let mut cuts: Vec<f64> = boundaries
        .iter()
        .map(|b| from + (b - source_in) / speed)
        .filter(|t| *t > from && *t < to)
        .collect();
    cuts.sort_by(|a, b| a.total_cmp(b));
    let mut edges = vec![from];
    edges.extend(cuts);
    edges.push(to);
    let mut spans: Vec<(f64, f64)> = edges.windows(2).map(|w| (w[0], w[1])).filter(|(a, b)| b - a >= 0.5).collect();
    if spans.is_empty() {
        spans.push((from, to));
    }
    spans.sort_by(|a, b| (b.1 - b.0).total_cmp(&(a.1 - a.0)));
    spans.truncate(max.max(1));
    spans
}

#[cfg(test)]
mod status_text_tests {
    use super::*;

    /// #626: what a person is told when they ask whether Klipp is there —
    /// the version, the project by name and whether it is saved, the
    /// timeline, the playhead and the media, in words.
    #[test]
    fn status_reads_as_a_person_would_say_it() {
        let build = klipp_core::build_info::BuildInfo { version: "0.1.0", number: 2711, commit: "f5e7b0ee", dirty: false, profile: "release", exe: None, built_at: None };
        let mut seq = klipp_timeline::Sequence::new_default();
        seq.name = "Main Sequence".into();
        let facts = |seq: &klipp_timeline::Sequence, project: Option<&std::path::Path>, unsaved: bool, media: usize| status_text(&StatusFacts { build: &build, project, unsaved, sequence: seq, media, playhead_secs: 65.2, playing: false, sequences: &[], active_sequence: 0 });
        let empty = facts(&seq, Some(std::path::Path::new("/home/u/Videos/test it again.klipp")), false, 0);
        assert!(empty.starts_with("Klipp 0.1.0 is running (build f5e7b0ee)."), "{empty}");
        assert!(empty.contains("Project: \"test it again\", all changes saved."), "{empty}");
        assert!(empty.contains("no clips yet") && empty.contains("1920\u{00d7}1080 at 24 fps"), "{empty}");
        assert!(empty.contains("Playhead: paused at 1:05.") && empty.contains("Media: none imported yet."), "{empty}");
        assert!(!empty.contains('/'), "no paths: {empty}");

        seq.tracks[0].clips.push(klipp_timeline::Clip::media(klipp_core::MediaId::new(), klipp_core::Timecode::zero(), klipp_core::Timecode { value: 24 * 90, rate: 24 }, klipp_core::Timecode::zero(), klipp_core::Timecode { value: 24 * 90, rate: 24 }));
        let cut = facts(&seq, None, true, 3);
        assert!(cut.contains("1 clip, 1:30 long") && cut.contains("a new project with changes, not saved yet") && cut.contains("Media: 3 files imported."), "{cut}");
    }
}

#[cfg(test)]
mod shot_span_tests {
    use super::shot_spans;

    /// #329: a clip with a cut in it is two shots, and the longest is
    /// read first — not the clip's middle, which here is past the cut.
    #[test]
    fn a_clip_with_a_cut_is_read_inside_its_longest_shot() {
        // Clip at 23.6..84.1 s of the timeline, from the media's start; the
        // media cuts at 44 s.
        let spans = shot_spans(23.6, 84.1, 0.0, 1.0, &[44.0], 6);
        assert_eq!(spans.len(), 2);
        let (a, b) = spans[0];
        assert!((a - 23.6).abs() < 1e-9 && (b - 67.6).abs() < 1e-9, "the longest shot first: {spans:?}");
        let mid = (a + b) / 2.0;
        assert!(mid < 67.6, "its middle is inside the first shot, not past the cut");
    }

    #[test]
    fn boundaries_outside_the_clip_and_slivers_are_ignored() {
        // Trimmed to source 10..20 s; cuts at 5 (before), 10.2 (a sliver), 15 and 30 (after).
        let spans = shot_spans(0.0, 10.0, 10.0, 1.0, &[5.0, 10.2, 15.0, 30.0], 6);
        let want = [(5.0, 10.0), (0.2, 5.0)];
        assert_eq!(spans.len(), want.len(), "{spans:?}");
        for ((a, b), (x, y)) in spans.iter().zip(want) {
            assert!((a - x).abs() < 1e-9 && (b - y).abs() < 1e-9, "{spans:?}");
        }
    }

    #[test]
    fn many_shots_keep_the_longest_and_none_is_the_whole_clip() {
        let cuts: Vec<f64> = (1..30).map(|i| i as f64 * 10.0 + (i % 3) as f64).collect();
        assert_eq!(shot_spans(0.0, 300.0, 0.0, 1.0, &cuts, 6).len(), 6);
        assert_eq!(shot_spans(5.0, 9.0, 0.0, 1.0, &[], 6), vec![(5.0, 9.0)]);
    }

    #[test]
    fn a_sped_up_clip_maps_its_cuts_onto_the_timeline() {
        // Twice speed from source 0: the cut at source 10 s is 5 s in.
        assert_eq!(shot_spans(0.0, 10.0, 0.0, 2.0, &[10.0], 6), vec![(0.0, 5.0), (5.0, 10.0)]);
    }
}

pub(crate) fn measure_clip(ctx: &ControlContext, from: f64, to: f64, only: Option<klipp_core::ClipId>, timeout_ms: Option<u64>) -> Result<(Vec<(f64, klipp_color::measure::FrameMeasure)>, Option<String>), String> {
    let span = (to - from).max(0.05);
    let times: Vec<f64> = if span < 0.6 {
        vec![from + span / 2.0]
    } else {
        [0.2, 0.5, 0.8].iter().map(|f| from + span * f).collect()
    };
    let mut out = Vec::new();
    for t in times {
        match measure_at(ctx, t, only, timeout_ms) {
            Ok(m) => out.push((t, m)),
            Err(e) if out.is_empty() => return Err(e),
            Err(_) => {}
        }
    }
    if out.is_empty() {
        return Err("no frame of the clip could be read".into());
    }
    // What moved across the shot: the light, the exposure, the face.
    let spread = |f: &dyn Fn(&klipp_color::measure::FrameMeasure) -> f32| {
        let vs: Vec<f32> = out.iter().map(|(_, m)| f(m)).collect();
        vs.iter().copied().fold(f32::MIN, f32::max) - vs.iter().copied().fold(f32::MAX, f32::min)
    };
    let (d_l, d_a, d_b) = (spread(&|m| m.l_p50), spread(&|m| m.cast_a), spread(&|m| m.cast_b));
    let d_cast = (d_a * d_a + d_b * d_b).sqrt();
    let faces: Vec<f32> = out.iter().filter_map(|(_, m)| m.faces.iter().find(|f| f.readable()).map(|f| f.hue)).collect();
    let d_face = if faces.len() > 1 {
        let (mut lo, mut hi) = (0.0f32, 0.0f32);
        for w in faces.windows(2) {
            let d = klipp_color::measure::hue_diff(w[0], w[1]);
            lo = lo.min(d);
            hi = hi.max(d);
        }
        hi - lo
    } else {
        0.0
    };
    let mut said = Vec::new();
    if d_l > 6.0 {
        said.push(format!("the exposure moves {d_l:.0} L* across the shot"));
    }
    if d_cast > 4.0 {
        said.push(format!("the light moves {d_cast:.1} in colour across the shot"));
    }
    if d_face > 6.0 {
        said.push(format!("the face's hue moves {d_face:.0}° across the shot"));
    }
    let drift = (!said.is_empty()).then(|| format!("{} — one grade for the whole shot will be wrong at one end; keyframe it or cut it", said.join("; ")));
    Ok((out, drift))
}

/// The middle reading of a clip's frames: what a single grade is set by.
pub(crate) fn median_measure(reads: &[(f64, klipp_color::measure::FrameMeasure)]) -> (f64, klipp_color::measure::FrameMeasure) {
    let mut by_light: Vec<&(f64, klipp_color::measure::FrameMeasure)> = reads.iter().collect();
    by_light.sort_by(|a, b| a.1.l_p50.total_cmp(&b.1.l_p50));
    let mid = by_light[by_light.len() / 2];
    (mid.0, mid.1.clone())
}

/// The frame at a timeline second, measured: Lab readings and the faces.
/// A clip id given as text, for the colour tools' clip-alone reads.
pub(crate) fn clip_of(id: &str) -> Option<klipp_core::ClipId> {
    id.parse().ok().map(klipp_core::ClipId)
}

/// The frame at `secs` measured in Lab — with `only`, that clip alone
/// (#719).
pub(crate) fn measure_at(ctx: &ControlContext, secs: f64, only: Option<klipp_core::ClipId>, timeout_ms: Option<u64>) -> Result<klipp_color::measure::FrameMeasure, String> {
    let (data, w, h) = grab_frame_rgba_of(ctx, secs, only, timeout_ms)?;
    let faces = skin_regions(ctx, secs, &data, w, h);
    let mut m = klipp_color::measure::measure_frame(&data, w, h, &faces);
    m.log_prior = log_prior_at(ctx, secs, only);
    Ok(m)
}

/// The clip's own file at timeline second `secs`, decoded as the file
/// holds it — no grade, no input transform — for the rules that tell
/// what the grade did from what the scene is (#1896). `None` for a clip
/// not from a file, a file that is not there or will not decode, and a
/// file that reads as camera log, whose floor is the encoding's and says
/// nothing about the scene. Only a display-referred SDR file is read:
/// an HDR or wide-gamut file decoded as it is holds its lightness on
/// another scale from the graded frame's, which goes through the output
/// transform (#1896 review).
pub(crate) fn source_reading(ctx: &ControlContext, clip: klipp_core::ClipId, secs: f64) -> Option<klipp_color::measure::SourceReading> {
    let (path, at, prior, size) = {
        let seq = ctx.sequence.read().ok()?;
        let pool = ctx.media_pool.read().recovered();
        let c = seq.all_clips().find(|c| c.id == clip && is_picture_clip(c))?;
        let item = pool.get(&c.media_id()?)?;
        if item.offline || !item.path.exists() || !display_sdr(item) {
            return None;
        }
        (item.path.clone(), c.source_secs_at(secs - c.timeline_in.to_secs_f64(), seq.fps.as_f64()), item.log_prior(), shown_size(item))
    };
    let (rgba, w, h) = decode_frame_rgba_sized(&path, at.max(0.0), size.0, size.1).ok()?;
    let mut m = klipp_color::measure::measure_frame(&rgba, w, h, &[]);
    m.log_prior = prior;
    let (range_stated, range_contradicted) = range_reading(&path, at.max(0.0));
    (!klipp_color::measure::looks_like_log(&m)).then(|| klipp_color::measure::SourceReading { range_stated, range_contradicted, ..klipp_color::measure::SourceReading::of(&m) })
}

/// Whether `item` is display-referred SDR as it is decoded: not HDR, not
/// BT.2020, not log by its tags or the user's choice. Only such a file's
/// own frame reads on the graded frame's scale.
pub(crate) fn display_sdr(item: &klipp_media::MediaItem) -> bool {
    let tagged = |v: &Option<String>, bad: &[&str]| v.as_deref().is_some_and(|s| { let s = s.to_ascii_lowercase(); bad.iter().any(|b| s.contains(b)) });
    !matches!(item.colour_space, klipp_media::ColourSpace::Hdr10 | klipp_media::ColourSpace::Hlg)
        && !tagged(&item.color_primaries, &["2020", "p3", "dci"])
        && !tagged(&item.color_transfer, &["2084", "pq", "hlg", "arib", "log", "2020"])
        && item.effective_log_format() == klipp_core::LogFormat::Srgb
}

/// The size a file's frame is shown at, its rotation applied, at most
/// 3840 wide: the size its own frame is read at.
fn shown_size(item: &klipp_media::MediaItem) -> (u32, u32) {
    let (w, h) = (item.resolution.width.max(2), item.resolution.height.max(2));
    let (w, h) = if item.source_rotation.rem_euclid(180) == 90 { (h, w) } else { (w, h) };
    if w <= 3840 { (w, h) } else { (3840, ((h as u64 * 3840 / w as u64) as u32).max(2) & !1) }
}

/// The top visible picture clip at timeline second `secs`, the one a
/// frame read there shows.
pub(crate) fn picture_clip_at(ctx: &ControlContext, secs: f64) -> Option<klipp_core::ClipId> {
    let seq = ctx.sequence.read().ok()?;
    let tc = klipp_core::Timecode::from_secs_f64(secs.max(0.0), seq.fps.rate());
    seq.tracks.iter()
        .filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible)
        .find_map(|t| t.clip_at_timecode(tc).map(|(_, c)| c).filter(|c| is_picture_clip(c)))
        .map(|c| c.id)
}

/// Whether the file's video stream states its range, limited ("tv") or
/// full ("pc"), as ffprobe reads it (#1906 review), and whether its frame
/// at `secs` agrees (#1975): its luma read as stored, with no range
/// conversion, against [`klipp_color::measure::range_tag_holds`]. A tag
/// the codes contradict is not taken: (stated, contradicted).
fn range_reading(path: &std::path::Path, secs: f64) -> (bool, bool) {
    // Media only: a list of other inputs has no range to read.
    let Ok(input) = klipp_media::input_args(path) else { return (false, false) };
    let tag = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=color_range", "-of", "csv=p=0"])
        .args(&input)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if tag != "tv" && tag != "pc" {
        return (false, false);
    }
    // Full to full: the scale leaves the codes as they are stored.
    let codes = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-nostats", "-ss", &format!("{secs}")])
        .args(&input)
        .args(["-frames:v", "1", "-vf", "scale=320:180:in_range=pc:out_range=pc", "-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .map(|o| o.stdout)
        .unwrap_or_default();
    if codes.len() < 320 * 180 {
        return (false, false);
    }
    match klipp_color::measure::range_tag_holds(&tag, &codes) {
        true => (true, false),
        false => (false, true),
    }
}

/// Whether one of `found`, judged against `look`, waits on the file's
/// own reading before it can be called the grade's: a black point
/// (#1896), and a screen recording's share at either rail (#1906).
pub(crate) fn needs_source(found: &[klipp_color::measure::Finding], look: &klipp_color::measure::LookTarget) -> bool {
    found.iter().any(|f| {
        f.verdict != klipp_color::measure::Verdict::Pass
            && (f.rule == "black_point" || (look.screencast && matches!(f.rule, "crushed_black" | "clipped_white" | "banding")))
    })
}

/// The measured clip's metadata: the isolated clip when requested, otherwise
/// the visible picture at `secs`. An overlay must not supply a solo read's prior.
pub(crate) fn log_prior_at(ctx: &ControlContext, secs: f64, only: Option<klipp_core::ClipId>) -> klipp_color::measure::LogPrior {
    let Ok(seq) = ctx.sequence.read() else { return Default::default() };
    let pool = ctx.media_pool.read().recovered();
    let tc = klipp_core::Timecode::from_secs_f64(secs.max(0.0), seq.fps.rate());
    let mut tracks = seq.tracks.iter()
        .filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible);
    let clip = match only {
        Some(id) => tracks.flat_map(|t| &t.clips).find(|c| c.id == id && is_picture_clip(c)),
        None => tracks.find_map(|t| t.clip_at_timecode(tc).map(|(_, c)| c).filter(|c| is_picture_clip(c))),
    };
    clip.and_then(|c| pool.get(&c.media_id()?).map(|item| item.log_prior()))
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "grading_log_prior_tests.rs"]
mod log_prior_tests;

#[cfg(test)]
mod look_tests {
    use super::*;

    /// #1906: an app recording marked graphics in a piece whose brief
    /// names a look is judged as a screen, not against the look; the
    /// camera footage beside it is judged against the look, and a grade
    /// group's own look still comes first.
    #[test]
    fn a_screen_or_graphics_clip_is_judged_as_a_screen_under_a_briefed_look() {
        let mut pool = klipp_media::MediaPool::new();
        let camera = pool.add(klipp_media::MediaItem::synthetic_for_test("/tmp/klipp-1906/camera.mov".into(), None));
        let app = pool.add(klipp_media::MediaItem::synthetic_for_test("/tmp/klipp-1906/app.mp4".into(), None));
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 25);
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut v = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        v.clips.push(klipp_timeline::Clip::media(camera, tc(0.0), tc(4.0), tc(0.0), tc(4.0)));
        let mut ui = klipp_timeline::Clip::media(camera, tc(4.0), tc(8.0), tc(0.0), tc(4.0));
        ui.role = klipp_core::ClipRole::Graphics;
        v.clips.push(ui);
        v.clips.push(klipp_timeline::Clip::media(app, tc(8.0), tc(12.0), tc(0.0), tc(4.0)));
        seq.tracks = vec![v];
        seq.brief = Some(klipp_timeline::Brief { look: Some(klipp_color::measure::LookTarget::preset("web_punchy").unwrap()), ..Default::default() });
        let is_app = |item: &klipp_media::MediaItem| item.id == app;
        let name = |s: f64| look_in(&seq, &pool, tc(s), &is_app).name;
        assert_eq!(name(1.0), "web_punchy", "camera footage takes the brief's look");
        assert_eq!(name(5.0), "screencast", "a clip marked graphics is a screen");
        assert_eq!(name(9.0), "screencast", "a screen recording is a screen");
        let group = klipp_timeline::GradeGroup { look: Some(klipp_color::measure::LookTarget::preset("film_soft").unwrap()), ..klipp_timeline::GradeGroup::new("night") };
        let id = group.id;
        seq.grade_groups.push(group);
        seq.tracks[0].clips[1].grade_group = Some(id);
        assert_eq!(look_in(&seq, &pool, tc(5.0), &is_app).name, "film_soft", "a group's own look comes first");
    }

    /// #1974: the check's judging of a generated layer on rendered frames,
    /// read as `check` reads them — the composite, and the layer alone —
    /// through the compositor, encoded as the viewer shows them.
    mod rendered {
        use super::super::*;
        use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
        use klipp_core::{GeneratorData, RationalTime};
        use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};
        use std::sync::Arc;

        const W: u32 = 64;
        const H: u32 = 36;

        fn tc(s: f64) -> RationalTime {
            RationalTime::from_secs_f64(s, 24)
        }

        /// A bright, flat plate: the footage under the graphics.
        struct Plate;
        impl FrameSource for Plate {
            fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
                Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new([0.75f32, 0.75, 0.75, 1.0].repeat((w * h) as usize))), width: w, height: h, pts: None })
            }
        }

        /// A gradient generator, mid to light grey, in `blend` at `opacity`.
        fn gradient(blend: klipp_core::BlendMode, opacity: f32) -> Clip {
            let kind = ClipKind::Generator(GeneratorData::Gradient { top: [120, 120, 120, 255], bottom: [200, 200, 200, 255], angle_deg: 0.0, four_point: None, grain: klipp_core::DEFAULT_GENERATOR_GRAIN });
            let mut c = Clip { kind, ..Clip::gap(tc(0.0), tc(2.0)) };
            c.blend_mode = blend;
            if opacity < 1.0 {
                c = klipp_timeline::test_clip::TestClip::of(c).effect(klipp_effects::BuiltinEffect::Opacity { value: opacity }).build();
            }
            c
        }

        /// Tracks top first, each a list of clips.
        fn sequence(tracks: Vec<Vec<Clip>>) -> Sequence {
            let mut seq = Sequence::new_default();
            seq.resolution = klipp_core::Resolution { width: W, height: H };
            seq.tracks = tracks.into_iter().enumerate().map(|(i, clips)| {
                let mut t = Track::new(&format!("V{i}"), TrackKind::Video);
                t.clips = clips;
                t
            }).collect();
            seq
        }

        /// `seq` at 0.5 s as the viewer shows it, measured and judged as
        /// graphics: the colour findings `check` would report.
        fn judged(seq: &Sequence) -> Vec<(String, f64, String, String)> {
            let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            r.set_gpu_enabled(false);
            let linear = r.render(seq, tc(0.5), W, H, &mut Plate, RenderOptions::default());
            let rgba: Vec<u8> = linear.chunks(4).flat_map(|p| {
                let enc = |v: f32| (klipp_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8;
                [enc(p[0]), enc(p[1]), enc(p[2]), 255]
            }).collect();
            let m = klipp_color::measure::measure_frame(&rgba, W, H, &[]);
            colour_report(&m, &klipp_color::measure::LookTarget::graphics(), 0.5)
        }

        fn plate() -> Clip {
            Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) }
        }

        fn clips_white(found: &[(String, f64, String, String)]) -> bool {
            found.iter().any(|f| colour_rule(&f.2) == "clipped_white")
        }

        /// In Add over a bright plate the layer clips only in the composite;
        /// alone it is clean, and the finding says so.
        #[test]
        fn an_added_gradient_clips_only_in_the_composite() {
            let layer = gradient(klipp_core::BlendMode::Add, 1.0);
            let composite = judged(&sequence(vec![vec![layer.clone()], vec![plate()]]));
            let alone = judged(&sequence(vec![vec![layer]]));
            assert!(clips_white(&composite), "{composite:?}");
            assert!(!clips_white(&alone), "{alone:?}");
            let said = attribute_to_layer(composite, &alone);
            let f = said.iter().find(|f| colour_rule(&f.2) == "clipped_white").unwrap();
            assert!(f.3.contains("only in the composite"), "{}", f.3);
        }

        /// The same layer in Normal at partial opacity over the same plate
        /// is clean: the clip came from the blend, not the plate.
        #[test]
        fn the_same_gradient_at_partial_opacity_is_clean() {
            let composite = judged(&sequence(vec![vec![gradient(klipp_core::BlendMode::Normal, 0.5)], vec![plate()]]));
            assert!(!clips_white(&composite), "{composite:?}");
        }

        /// Under an adjustment layer whose levels push it past white, the
        /// layer clips in the composite and not alone.
        #[test]
        fn a_gradient_under_a_levels_layer_clips_in_the_composite() {
            let push = klipp_effects::BuiltinEffect::Levels { in_black: 0.0, in_white: 0.3, gamma: 1.0, out_black: 0.0, out_white: 1.0 };
            let adjust = klipp_timeline::test_clip::TestClip::of(Clip { kind: ClipKind::AdjustmentLayer, ..Clip::gap(tc(0.0), tc(2.0)) }).effect(push).build();
            let layer = gradient(klipp_core::BlendMode::Normal, 1.0);
            let composite = judged(&sequence(vec![vec![adjust], vec![layer.clone()]]));
            let alone = judged(&sequence(vec![vec![layer]]));
            assert!(clips_white(&composite), "{composite:?}");
            assert!(attribute_to_layer(composite, &alone).iter().any(|f| f.3.contains("only in the composite")));
        }
    }

    /// #1919 review: a generated picture is judged on the composite, and
    /// each finding says whether the layer alone has it.
    #[test]
    fn a_composite_finding_says_whose_it_is() {
        let f = |rule: &str| ("error".to_string(), 3.0, format!("12% of the frame is at white [{rule}]"), "highlights down".to_string());
        let said = attribute_to_layer(vec![f("clipped_white"), f("gamut")], &[f("gamut")]);
        assert!(said[0].3.contains("only in the composite"), "{said:?}");
        assert!(said[1].3.contains("the layer's own"), "{said:?}");
        assert_eq!(colour_rule("x [grade order]"), "grade_order");
    }

    /// #1976: what the viewer's frame goes through after the grade is part
    /// of the colour management a colour finding's id holds.
    #[test]
    fn the_after_grade_transform_is_in_the_colour_management() {
        let text = |after: &str| colour_management_text(Default::default(), Default::default(), None, after);
        assert_ne!(text("lut /a.cube on 100%|sdr Bt1886|hdr Off"), text("lut /a.cube on 50%|sdr Bt1886|hdr Off"));
        assert_ne!(text("lut /a.cube on 100%|sdr Bt1886|hdr Off"), text("lut /a.cube off 100%|sdr Bt1886|hdr Off"));
    }

    /// #1919: zero shots read is "not measured", never a clean result;
    /// graphics are counted apart from footage and say what they were
    /// judged for.
    #[test]
    fn the_colour_section_says_what_it_covered() {
        assert!(colour_coverage_line(0, 0, 0, 0).starts_with("Colour: not measured"));
        assert!(colour_coverage_line(3, 0, 0, 0).contains("none of the 3 shots could be read"));
        assert_eq!(colour_coverage_line(5, 5, 0, 4), "Colour (measured in Lab on 5 shots, 4 on target):");
        let g = colour_coverage_line(6, 0, 6, 5);
        assert!(g.contains("6 shots, all generated graphics") && g.contains("clipping, gamut and banding only; 5 on target"), "{g}");
        let mixed = colour_coverage_line(4, 3, 1, 2);
        assert!(mixed.contains("4 shots: 3 of footage, and 1 of generated graphics") && mixed.ends_with("2 on target):"), "{mixed}");
    }
}

/// A picture the timeline makes rather than a file: an enabled
/// generator, or a title on a full-frame card. The colour check reads it
/// too, judged as graphics (#1919).
pub(crate) fn is_generated_picture(c: &klipp_timeline::Clip) -> bool {
    c.enabled
        && match &c.kind {
            klipp_timeline::ClipKind::Generator(_) => true,
            klipp_timeline::ClipKind::Title(td) => matches!(td.background, klipp_timeline::TitleBackground::FullFrame { color } if color[3] > 0),
            _ => false,
        }
}

/// The project's colour management as it changes a rendered picture,
/// for a colour finding's basis (#1932): the working space, the grade
/// encoding, and what the viewer's frame goes through after the grade —
/// the global LUT with its switch and opacity, the viewer's output
/// (#1976).
pub(crate) fn colour_management(ctx: &ControlContext) -> String {
    let (lut, after) = {
        let p = ctx.shared.project.lock().recovered();
        (p.global_lut.clone(), p.picture_transform.clone())
    };
    colour_management_text(klipp_engine::video_decoder::working_colorspace(), klipp_core::grade_encoding(), lut.as_deref(), &after)
}

/// [`colour_management`] from its parts.
pub(crate) fn colour_management_text(working: klipp_core::WorkingColorspace, encoding: klipp_core::GradeEncoding, lut: Option<&std::path::Path>, after: &str) -> String {
    let lut = lut.map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "-".into());
    format!("{working:?}|{encoding:?}|{lut}|{after}")
}

/// The rule a colour finding's words end with, "[black_point]", as a
/// name ("black_point").
pub(crate) fn colour_rule(what: &str) -> String {
    what.rsplit_once(" [").map_or("colour".to_string(), |(_, r)| r.trim_end_matches(']').replace(' ', "_"))
}

/// A generated picture's findings, read on the composite, each said as
/// the layer's own when the layer read alone has it too, and else as the
/// composite's: its blend, opacity or what is over or under it (#1919).
pub(crate) fn attribute_to_layer(composite: Vec<(String, f64, String, String)>, alone: &[(String, f64, String, String)]) -> Vec<(String, f64, String, String)> {
    composite.into_iter().map(|(severity, at, what, hint)| {
        let rule = colour_rule(&what);
        let own = alone.iter().any(|a| colour_rule(&a.2) == rule);
        let whose = if own { "the layer's own: it is there read alone" } else { "only in the composite: the layer alone is clean, so look at its blend, its opacity, or what is over or under it" };
        (severity, at, what, format!("{hint} ({whose})"))
    }).collect()
}

/// The head of the check's colour section: how many shots were read, of
/// footage and of generated graphics, and how many were on target. Zero
/// read is said as not measured, never as a clean result (#1919):
/// `listed` is how many shots there were to read.
pub(crate) fn colour_coverage_line(listed: usize, footage: usize, graphics: usize, on_target: usize) -> String {
    let shots = |n: usize| format!("{n} shot{}", if n == 1 { "" } else { "s" });
    if footage + graphics == 0 {
        return if listed == 0 {
            "Colour: not measured — there is no footage, generator or title card on a visible track to read.".to_string()
        } else {
            format!("Colour: not measured — none of the {} could be read.", shots(listed))
        };
    }
    match (footage, graphics) {
        (f, 0) => format!("Colour (measured in Lab on {}, {on_target} on target):", shots(f)),
        (0, g) => format!("Colour (measured on {}, all generated graphics judged for clipping, gamut and banding only; {on_target} on target):", shots(g)),
        (f, g) => format!("Colour (measured in Lab on {}: {f} of footage, and {g} of generated graphics judged for clipping, gamut and banding only; {on_target} on target):", shots(f + g)),
    }
}
pub(crate) fn is_picture_clip(c: &klipp_timeline::Clip) -> bool {
    c.enabled && matches!(c.kind, klipp_timeline::ClipKind::Media { .. })
}

/// The track the colour tools work on: the top visible video track
/// with a picture clip on it. A track above holding only titles, or
/// nothing, is passed over; taking the first video track as it came
/// left every tool with nothing to grade (#513).
pub(crate) fn picture_track(seq: &klipp_timeline::Sequence) -> Option<&klipp_timeline::Track> {
    seq.tracks.iter().find(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible && t.clips.iter().any(is_picture_clip))
}

/// `current` with `fields` laid over it by name: what `update_brief`
/// sets. A field given as null goes back to its empty value; a name
/// the brief does not have is refused, where a whole `set_brief` would
/// drop it without a word (#498).
pub(crate) fn merge_brief(current: &klipp_timeline::Brief, fields: &serde_json::Value) -> Result<klipp_timeline::Brief, String> {
    let object = |b: &klipp_timeline::Brief| match serde_json::to_value(b) {
        Ok(serde_json::Value::Object(m)) => Ok(m),
        _ => Err("the brief does not serialise as an object".to_string()),
    };
    let serde_json::Value::Object(fields) = fields else {
        return Err("fields is an object: the brief's fields to change, by name".into());
    };
    let empty = object(&klipp_timeline::Brief::default())?;
    // The same names a whole brief is held to: `language`, left out of
    // an empty brief's JSON, is one (#1915).
    let known = protocol::brief_fields();
    let mut merged = object(current)?;
    for (name, value) in fields {
        if !known.contains(name) {
            return Err(protocol::unknown_brief_field(name, &known));
        }
        let cleared = empty.get(name).cloned().unwrap_or(serde_json::Value::Null);
        merged.insert(name.clone(), if value.is_null() { cleared } else { value.clone() });
    }
    protocol::brief_from_value(serde_json::Value::Object(merged))
}

/// The look a frame is judged against: its grade group's when the group
/// has one, or none past faults when the group leaves the look on
/// purpose (#793); else, for a screen recording or a clip marked
/// graphics, the screen recording's; else the brief's, else neutral. See
/// [`look_in`].
pub(crate) fn look_for(ctx: &ControlContext, secs: f64) -> klipp_color::measure::LookTarget {
    let Ok(seq) = ctx.sequence.read() else { return klipp_color::measure::LookTarget::unbriefed() };
    let pool = ctx.media_pool.read().recovered();
    let tc = klipp_core::Timecode::from_secs_f64(secs.max(0.0), seq.fps.rate());
    look_in(&seq, &pool, tc, &|item| {
        // A still picture held on screen also "never changes", and a
        // locked-off camera shot holds too, so the beat sheet alone is
        // not enough: a file with a person in it is footage.
        let a = klipp_analysis::report::MediaAnalysis::load(&item.path);
        let holds = a.screen_beatsheet(item).is_some_and(|b| b.has_freeze);
        let people = a.framing.as_ref().is_some_and(|f| f.samples.iter().any(|s| !s.people.is_empty() || !s.faces.is_empty()));
        holds && !people
    })
}

/// [`look_for`] on the timeline itself, `screen_recording` saying
/// whether a file is one. A recording of an app, or a clip marked
/// graphics, is judged as a screen whatever the brief's look: the
/// brief's look is for camera footage, and judged by it a dark UI's
/// panels and scopes read as crushed blacks (#1906). A grade group's
/// own look still comes first: that is a choice made for those clips.
pub(crate) fn look_in(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool, tc: klipp_core::Timecode, screen_recording: &dyn Fn(&klipp_media::MediaItem) -> bool) -> klipp_color::measure::LookTarget {
    use klipp_color::measure::LookTarget;
    if let Some(look) = group_look(seq, tc) {
        return look;
    }
    let top = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video).find_map(|t| t.clip_at_timecode(tc)).map(|(_, c)| c);
    if let Some(c) = top {
        let screen = c.role == klipp_core::ClipRole::Graphics || c.media_id().and_then(|id| pool.get(&id)).is_some_and(screen_recording);
        if screen {
            return LookTarget::preset("screencast").expect("the screencast preset exists");
        }
    }
    if let Some(look) = seq.brief.as_ref().and_then(|b| b.look.clone()) {
        return resolve_look_target(look);
    }
    LookTarget::unbriefed()
}

/// The look the grade group of the picture on top at `tc` holds it to:
/// the group's own, or faults only for a group that leaves the look on
/// purpose; `None` when the picture is in no group or its group names
/// neither, and the brief decides (#793).
pub(crate) fn group_look(seq: &klipp_timeline::Sequence, tc: klipp_core::Timecode) -> Option<klipp_color::measure::LookTarget> {
    let clip = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video).find_map(|t| t.clip_at_timecode(tc).map(|(_, c)| c))?;
    let g = seq.grade_groups.iter().find(|g| Some(g.id) == clip.grade_group)?;
    if g.departs {
        return Some(departing_look());
    }
    g.look.clone().map(resolve_look_target)
}

/// What a group that leaves the look on purpose is judged against:
/// faults only. Clipping, crushed blacks, log left ungraded, noise and
/// banding are judged whatever the target, and so is a face lost or
/// blown (skin_lightness). No cast, black point, face exposure or taste
/// numbers: the unbriefed target's neutral cast told a group warm on
/// purpose that its neutrals sat toward orange shot after shot, and its
/// face exposure would fail a section dark on purpose (#811 review).
pub(crate) fn departing_look() -> klipp_color::measure::LookTarget {
    klipp_color::measure::LookTarget { name: "off the look on purpose".into(), ..Default::default() }
}

/// A look given by name alone takes the preset's numbers, and keeps what
/// it says it protects when it says (#1579): `warm_interview` with
/// `protect: ["skin"]` may warm the sky.
pub(crate) fn resolve_look_target(look: klipp_color::measure::LookTarget) -> klipp_color::measure::LookTarget {
    use klipp_color::measure::LookTarget;
    let bare = look.black_l.is_none() && look.white_l.is_none() && look.mid_l.is_none() && look.contrast_range.is_none() && look.chroma_mean.is_none() && look.cast_a.is_none() && look.skin_hue.is_none();
    if bare {
        if let Some(p) = LookTarget::preset(&look.name) {
            return LookTarget { protect: look.protect.or(p.protect.clone()), ..p };
        }
    }
    look
}

/// The colour findings of a frame as report entries: (severity, secs, what, hint).
pub(crate) fn colour_report(m: &klipp_color::measure::FrameMeasure, look: &klipp_color::measure::LookTarget, secs: f64) -> Vec<(String, f64, String, String)> {
    use klipp_color::measure::Verdict;
    klipp_color::measure::judge(m, look)
        .into_iter()
        .filter(|f| f.verdict != Verdict::Pass)
        .map(|f| ((if f.verdict == Verdict::Fail { "error" } else { "warning" }).to_string(), secs, format!("{} [{}]", f.what, f.rule), f.fix.to_string()))
        .collect()
}

/// The mix metered from `from` to `to` in `step`s: (step start, short-term
/// LUFS at its end, loudest momentary LUFS inside it). Metering starts
/// three seconds early, where there are three, so the first short-term
/// reading has its window (#792).
pub(crate) fn mix_loudness(snapshot: &klipp_render::SequenceSnapshot, from: f64, to: f64, step: f64) -> Result<Vec<(f64, f32, f32)>, String> {
    let fps = snapshot.fps.max(1.0);
    let mut mix = klipp_render::AudioMixdown::new(snapshot).map_err(|e| format!("the mix could not be read: {e}"))?;
    let mut meter = klipp_audio::LufsMeter::new(snapshot.sample_rate as f32, false);
    let first = ((from - 3.0).max(0.0) * fps).floor() as u64;
    let last = (to * fps).ceil() as u64;
    let floor_db = |v: f32| if v.is_finite() { v.max(-70.0) } else { -70.0 };
    let mut rows = Vec::new();
    let mut step_start = from;
    let mut peak = f32::MIN;
    for frame in first..last {
        // Unclamped, as the export's loudness pass meters it (R-3): a hit
        // summed over music is where the mix runs past full scale, and a
        // clamp would read the step that should stand out as quieter (#812).
        let audio = mix.get_frame_audio_unclamped(frame);
        meter.process(&audio);
        let t = (frame + 1) as f64 / fps;
        if t <= from {
            continue;
        }
        let r = meter.readings();
        peak = peak.max(floor_db(r.momentary));
        if t >= step_start + step - 1e-6 || frame + 1 == last {
            rows.push(((step_start * 100.0).round() / 100.0, floor_db(r.short_term), peak));
            step_start += step;
            peak = f32::MIN;
        }
    }
    Ok(rows)
}

/// The whole mix as an export without a target of its own delivers it:
/// its integrated loudness, LUFS, and its true peak, dBTP (4x
/// oversampled), unclamped as the export's loudness pass reads it (#2481).
pub(crate) fn mix_delivery(snapshot: &klipp_render::SequenceSnapshot) -> Result<(f32, f32), String> {
    let mut mix = klipp_render::AudioMixdown::new(snapshot).map_err(|e| format!("the mix could not be read: {e}"))?;
    let mut meter = klipp_audio::LufsMeter::new(snapshot.sample_rate as f32, true);
    for frame in 0..snapshot.end_frame() {
        meter.process(&mix.get_frame_audio_unclamped(frame));
    }
    let r = meter.readings();
    Ok((r.integrated, r.true_peak_dbfs))
}

/// How far a mix may sit from its target before `check` says the export
/// has far to take it, LU.
const TARGET_FAR_LU: f32 = 6.0;

/// What `check` says of a mix measured at `integrated` LUFS with a true
/// peak of `peak` dBTP (#2481): with the brief's `target`, the export
/// brings it there, so the question is whether it can under `ceiling`;
/// without one the mix is what is delivered, and an over clips. Each as
/// (severity, rule, what, hint).
pub(crate) fn delivery_findings(integrated: f32, peak: f32, target: Option<f32>, ceiling: f32) -> Vec<(klipp_document::qc::Severity, &'static str, String, String)> {
    use klipp_document::qc::Severity;
    let mut out = vec![(Severity::Note, "loudness_measured", format!("the mix measures {integrated:.1} LUFS integrated, true peak {peak:+.1} dBTP"), "mix_loudness shows it second by second".to_string())];
    if !integrated.is_finite() || integrated < -60.0 {
        return out;
    }
    match target {
        Some(t) => {
            let gain = t - integrated;
            let after = peak + gain;
            if after > ceiling + 0.1 {
                out.push((Severity::Error, "loudness_target", format!("reaching the brief's {t:.0} LUFS takes {gain:+.1} dB, which puts the true peak at {after:+.1} dBTP, over the {ceiling:.1} ceiling: the export will fall short of it"), "Bring the peaks down on the track that has them (a compressor or limiter, or its level), then check again".to_string()));
            } else if gain.abs() >= TARGET_FAR_LU {
                out.push((Severity::Warning, "loudness_target", format!("the mix is {integrated:.1} LUFS, {:.1} LU from the brief's {t:.0}: an export to the brief's target takes it there, one with another target or none does not", gain.abs()), "Export with the brief's target (the default), or bring the mix nearer it".to_string()));
            }
        }
        None if peak > 0.0 => out.push((Severity::Error, "loudness_overs", format!("the mix runs past full scale, true peak {peak:+.1} dBTP, and with no loudness target nothing brings it down: the delivery clips"), "Lower the loud tracks, or give the brief a loudness (or the export a target_lufs) so it is normalised under the ceiling".to_string())),
        None if peak > ceiling => out.push((Severity::Warning, "loudness_overs", format!("the mix's true peak is {peak:+.1} dBTP, over the {ceiling:.1} ceiling: a lossy encode may clip it"), "Lower the loud tracks, or export with a loudness target".to_string())),
        None => {}
    }
    out
}

/// How far under the piece's median short-term loudness a stretch of the
/// mix sits before `check` says so (#1584), and how long it must last: a
/// spliced bed left 10 LU down for a section went unremarked.
const DROP_UNDER_LU: f32 = 8.0;
const DROP_MIN_SECS: f64 = 3.0;

/// The stretches of a mix, metered as [`mix_loudness`] rows `step` apart,
/// whose short-term loudness sits more than [`DROP_UNDER_LU`] under the
/// piece's median for at least [`DROP_MIN_SECS`]: `(from, to, lufs,
/// median)`, `lufs` the stretch's median. `from` is where the first quiet
/// row's three-second window starts, where the sound went quiet, not the
/// row's own time (#1598). A row whose three-second
/// window `excused` (a fade, speech: a quiet moment meant as one), that
/// is silence (-60 LUFS or under, a deliberate pause), or whose window
/// starts before the piece does, is never part of one.
pub(crate) fn loudness_drops(rows: &[(f64, f32, f32)], step: f64, excused: &dyn Fn(f64, f64) -> bool) -> Vec<(f64, f64, f32, f32)> {
    let median = |mut v: Vec<f32>| -> Option<f32> {
        v.sort_by(f32::total_cmp);
        v.get(v.len() / 2).copied()
    };
    let Some(piece) = median(rows.iter().map(|r| r.1).filter(|v| v.is_finite() && *v > -70.0).collect()) else { return Vec::new() };
    let quiet = |&(t, short, _): &(f64, f32, f32)| {
        let (a, b) = (t + step - 3.0, t + step);
        a >= 0.0 && short > -60.0 && short < piece - DROP_UNDER_LU && !excused(a, b)
    };
    let mut drops = Vec::new();
    let mut run: Vec<&(f64, f32, f32)> = Vec::new();
    for row in rows.iter().chain(std::iter::once(&(f64::INFINITY, f32::NAN, f32::NAN))) {
        if row.0.is_finite() && quiet(row) {
            run.push(row);
            continue;
        }
        if let (Some(first), Some(last)) = (run.first(), run.last()) {
            let to = last.0 + step;
            if to - first.0 >= DROP_MIN_SECS - 1e-6 {
                if let Some(lufs) = median(run.iter().map(|r| r.1).collect()) {
                    drops.push(((first.0 + step - 3.0).max(0.0), to, lufs, piece));
                }
            }
        }
        run.clear();
    }
    drops
}

/// The sequence's mix from `from` to `to` seconds of the timeline, as the
/// export mixes it (the same mixdown, before loudness normalisation),
/// written to `out` as 16 kHz mono 16-bit WAV (#1577): what an agent can
/// listen to, or transcribe, before rendering.
pub(crate) fn mix_excerpt(snapshot: &klipp_render::SequenceSnapshot, from: f64, to: f64, out: &std::path::Path) -> Result<std::path::PathBuf, String> {
    mix_excerpt_as(snapshot, from, to, out, Some(16_000), 1)
}

/// [`mix_excerpt`] at `rate` (the sequence's when `None`) with `channels`:
/// 16 kHz mono to be read, the sequence's rate in stereo to be heard
/// (#1891).
pub(crate) fn mix_excerpt_as(snapshot: &klipp_render::SequenceSnapshot, from: f64, to: f64, out: &std::path::Path, out_rate: Option<u32>, channels: u8) -> Result<std::path::PathBuf, String> {
    use std::io::Write;
    let fps = snapshot.fps.max(1.0);
    let rate = snapshot.sample_rate;
    let mut mix = klipp_render::AudioMixdown::new(snapshot).map_err(|e| format!("the mix could not be read: {e}"))?;
    let (first, last) = ((from * fps).floor() as u64, (to * fps).ceil() as u64);
    let mut pcm: Vec<f32> = Vec::new();
    for frame in first..last {
        pcm.extend(mix.get_frame_audio(frame));
    }
    // Interleaved stereo from the first frame's first sample: cut to the
    // range's own samples.
    let start = mix.frame_sample_range(first).0;
    let a = (((from * rate as f64).round() as u64).saturating_sub(start) * 2) as usize;
    let b = (((to * rate as f64).round() as u64).saturating_sub(start) * 2) as usize;
    let pcm = &pcm[a.min(pcm.len())..b.min(pcm.len())];
    let mut child = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-nostats", "-loglevel", "error", "-y", "-f", "f32le", "-ar"])
        .arg(rate.to_string())
        .args(["-ac", "2", "-i", "pipe:0", "-ac", &channels.to_string(), "-ar", &out_rate.unwrap_or(rate).to_string(), "-c:a", "pcm_s16le"])
        .arg(out)
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run ffmpeg: {e}"))?;
    let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    child.stdin.take().ok_or("ffmpeg took no input")?.write_all(&bytes).map_err(|e| format!("could not hand ffmpeg the mix: {e}"))?;
    let done = child.wait_with_output().map_err(|e| format!("ffmpeg stopped: {e}"))?;
    if !done.status.success() || !out.is_file() {
        return Err(format!("ffmpeg could not write the excerpt: {}", String::from_utf8_lossy(&done.stderr).trim()));
    }
    Ok(out.to_path_buf())
}

/// The level an effect is set to so it sits `over_db` above the loudest
/// sound already heard under it (#792): files a dozen decibels apart, all
/// set to the same volume, left one whoosh shouting over the intro and
/// another lost under the music. Each heard clip under it counts by its
/// file's integrated loudness and its own volume and gain; effects on the
/// SFX track do not count. `None` when either side has not been measured.
pub(crate) fn level_over_bed(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool, effect: klipp_core::MediaId, at: f64, secs: f64, over_db: f32) -> Option<f32> {
    let loudness = |id: klipp_core::MediaId| -> Option<f32> {
        let item = pool.get(&id)?;
        let audio = klipp_analysis::report::MediaAnalysis::load(&item.path).audio?;
        // A clip shorter than the meter's gate measures nothing; its
        // true peak, twelve under, stands in for a short hit.
        audio.integrated_lufs.filter(|l| *l > -60.0).or(audio.true_peak_dbtp.map(|p| p - 12.0))
    };
    let bed = seq
        .tracks
        .iter()
        .filter(|t| t.kind == klipp_timeline::TrackKind::Audio && !t.name.eq_ignore_ascii_case("sfx") && !t.muted)
        .flat_map(|t| t.clips.iter())
        .filter(|c| c.enabled && c.timeline_in.to_secs_f64() < at + secs && c.timeline_out.to_secs_f64() > at)
        .filter_map(|c| {
            let id = c.media_id()?;
            let owner = sound_owner(seq, c);
            let db = 20.0 * (owner.volume.max(1e-4) * owner.gain.max(1e-4)).log10();
            Some(loudness(id)? + db)
        })
        .fold(None, |m: Option<f32>, v| Some(m.map_or(v, |m| m.max(v))))?;
    let own = loudness(effect)?;
    Some((bed + over_db - own).clamp(-30.0, 12.0))
}

/// How far grade_to_target may move each knob from where a shot
/// started (#742). Gentle by default: chasing a look's chroma on vivid
/// phone footage took saturation to 1.3x and turned a sky olive.
#[derive(Clone)]
pub(crate) struct GradeLimits {
    pub(crate) name: &'static str,
    pub(crate) exposure: f32,
    pub(crate) lift: f32,
    pub(crate) gain: f32,
    pub(crate) temperature: f32,
    pub(crate) tint: f32,
    /// Saturation as a factor of the start's: down to 1/x, up to x.
    pub(crate) saturation: f32,
}

impl GradeLimits {
    pub(crate) fn named(name: &str) -> Result<Self, String> {
        let (name, k) = match name.to_lowercase().as_str() {
            "gentle" => ("gentle", 1.0),
            "normal" => ("normal", 2.0),
            "bold" => ("bold", 3.0),
            other => return Err(format!("strength {other:?} is not one of gentle, normal or bold")),
        };
        // Temperature in mireds, about 0.2 b* on mid grey a unit: gentle
        // moves a grey about 3 b*, normal 6, bold 9 (#1580); tint alike.
        Ok(Self { name, exposure: 0.5 * k, lift: 0.05 * k, gain: 0.1 * k, temperature: 15.0 * k, tint: 11.0 * k, saturation: 1.0 + 0.12 * k })
    }

    /// The knobs after `step` from `now`, held within the limits around
    /// `start`, and the names of the knobs the limits held.
    pub(crate) fn clamp(&self, start: &klipp_color::measure::PrimaryStep, now: &klipp_color::measure::PrimaryStep, step: &klipp_color::measure::PrimaryStep) -> (klipp_color::measure::PrimaryStep, Vec<&'static str>) {
        let mut held = Vec::new();
        let mut within = |name: &'static str, want: f32, lo: f32, hi: f32| {
            let got = want.clamp(lo, hi);
            if (got - want).abs() > 1e-4 {
                held.push(name);
            }
            got
        };
        let out = klipp_color::measure::PrimaryStep {
            exposure: within("exposure", now.exposure + step.exposure, start.exposure - self.exposure, start.exposure + self.exposure).clamp(-6.0, 6.0),
            lift: within("lift", now.lift + step.lift, start.lift - self.lift, start.lift + self.lift).clamp(-1.0, 1.0),
            gain: within("gain", now.gain + step.gain, start.gain - self.gain, start.gain + self.gain).clamp(-1.0, 1.0),
            temperature: within("temperature", now.temperature + step.temperature, start.temperature - self.temperature, start.temperature + self.temperature).clamp(-50.0, 50.0),
            tint: within("tint", now.tint + step.tint, start.tint - self.tint, start.tint + self.tint).clamp(-50.0, 50.0),
            saturation: within("saturation", now.saturation * (1.0 + step.saturation), start.saturation / self.saturation, start.saturation * self.saturation).clamp(0.0, 4.0),
            held_for_highlights: step.held_for_highlights,
            held_for_blacks: step.held_for_blacks,
        };
        (out, held)
    }
}

/// A step as the knobs it moves, from `from` to `to`: "exposure +0.30,
/// temperature -6.0, saturation x1.10"; "nothing" when none moves.
pub(crate) fn describe_step(from: &klipp_color::measure::PrimaryStep, to: &klipp_color::measure::PrimaryStep) -> String {
    let mut parts = Vec::new();
    for (name, d, min) in [
        ("exposure", to.exposure - from.exposure, 0.01),
        ("lift", to.lift - from.lift, 0.002),
        ("gain", to.gain - from.gain, 0.002),
        ("temperature", to.temperature - from.temperature, 0.1),
        ("tint", to.tint - from.tint, 0.1),
    ] {
        if d.abs() >= min {
            parts.push(format!("{name} {d:+.2}"));
        }
    }
    if from.saturation > 0.0 && (to.saturation / from.saturation - 1.0).abs() >= 0.005 {
        parts.push(format!("saturation x{:.2}", to.saturation / from.saturation));
    }
    if parts.is_empty() { "nothing".into() } else { parts.join(", ") }
}

/// Shots of one piece of media, two or more, as scenes to grade
/// together: `{file, media_id, clips}` each. Keyed by the media, not the
/// file name, so two cards' IMG_0001.mov are two scenes (#770 review).
pub(crate) fn scene_groups(shots: &[(String, f64, String)], media_of: &std::collections::HashMap<String, klipp_core::MediaId>) -> Vec<serde_json::Value> {
    let mut by_media: Vec<(klipp_core::MediaId, &str, Vec<&str>)> = Vec::new();
    for (id, _, name) in shots {
        let Some(media) = media_of.get(id).copied() else { continue };
        match by_media.iter_mut().find(|(m, _, _)| *m == media) {
            Some(g) => g.2.push(id.as_str()),
            None => by_media.push((media, name.as_str(), vec![id.as_str()])),
        }
    }
    by_media.iter().filter(|(_, _, ids)| ids.len() >= 2).map(|(media, name, ids)| json!({ "file": name, "media_id": media.0.to_string(), "clips": ids })).collect()
}

/// The clip's Corrector knobs as they are: what a grade step adds to.
pub(crate) fn corrector_now(clip: &klipp_timeline::Clip) -> klipp_color::measure::PrimaryStep {
    let mut s = klipp_color::measure::PrimaryStep { saturation: 1.0, ..Default::default() };
    if let Some(n) = clip.own_grade().nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector) {
        s.exposure = n.params.exposure.unwrap_or(0.0);
        s.lift = n.params.lift_master.unwrap_or(0.0);
        s.gain = n.params.gain_master.unwrap_or(0.0);
        s.temperature = n.params.temperature.unwrap_or(0.0);
        s.tint = n.params.tint.unwrap_or(0.0);
        s.saturation = n.params.saturation.unwrap_or(1.0);
    }
    s
}

#[cfg(test)]
mod loudness_drop_tests {
    use super::loudness_drops;

    /// #1584: a spliced bed left a section about 10 LU under the rest,
    /// and nothing said so. Metered a second apart: loud, then six
    /// seconds 11 LU down, then loud again, is one stretch; a dip of two
    /// seconds, 6 LU down, silence, a faded stretch and the meter's first
    /// seconds are none.
    #[test]
    fn a_section_well_under_the_rest_is_a_drop() {
        let rows = |levels: &[f32]| -> Vec<(f64, f32, f32)> { levels.iter().enumerate().map(|(i, &l)| (i as f64, l, l + 2.0)).collect() };
        let none = |_: f64, _: f64| false;
        let mut levels = vec![-11.0f32; 30];
        levels[12..18].fill(-22.0);
        let drops = loudness_drops(&rows(&levels), 1.0, &none);
        assert_eq!(drops.len(), 1, "{drops:?}");
        let (from, to, lufs, median) = drops[0];
        // The rows from 12 s read quiet; the first one's window, and the
        // quiet, starts 3 s before it ends, at 10 s (#1598).
        assert_eq!((from, to, lufs, median), (10.0, 18.0, -22.0, -11.0));

        // Two seconds down is a moment, not a section.
        let mut short = vec![-11.0f32; 30];
        short[12..14].fill(-22.0);
        assert!(loudness_drops(&rows(&short), 1.0, &none).is_empty());
        // Six under is a change of level, not a hole.
        let mut mild = vec![-11.0f32; 30];
        mild[12..18].fill(-17.0);
        assert!(loudness_drops(&rows(&mild), 1.0, &none).is_empty());
        // Silence is a pause.
        let mut pause = vec![-11.0f32; 30];
        pause[12..18].fill(-70.0);
        assert!(loudness_drops(&rows(&pause), 1.0, &none).is_empty());
        // A fade there is meant.
        let faded = |a: f64, b: f64| a < 18.0 && b > 12.0;
        assert!(loudness_drops(&rows(&levels), 1.0, &faded).is_empty());
        // The meter's first seconds hold less than its window.
        let mut start = vec![-11.0f32; 30];
        start[0..2].fill(-30.0);
        assert!(loudness_drops(&rows(&start), 1.0, &none).is_empty());
    }
}

#[cfg(test)]
mod limit_tests {
    use super::GradeLimits;

    /// How far a temperature of `t` moves a mid grey, in Lab b*.
    fn grey_b(t: f32) -> f32 {
        let m = klipp_color::white_balance::white_balance_matrix(t, 0.0, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
        let g = 0.18;
        let c = [0, 1, 2].map(|i| m[i][0] * g + m[i][1] * g + m[i][2] * g);
        klipp_color::measure::linear_to_lab(c[0], c[1], c[2]).b
    }

    /// Each strength's temperature limit is a step a colourist reads
    /// (#1580): gentle moves a mid grey about 3 b*, normal 6, bold 9, the
    /// knob's own scale. Control: the old normal limit, 16, moved it 4.
    #[test]
    fn the_strengths_move_a_grey_as_far_as_they_say() {
        for (name, want) in [("gentle", 3.0), ("normal", 6.0), ("bold", 9.0)] {
            let got = grey_b(GradeLimits::named(name).unwrap().temperature);
            assert!((got - want).abs() < 1.0, "{name}: b* {got}, not about {want}");
        }
        assert!(grey_b(16.0) < 4.5, "the old normal limit moved a grey {}", grey_b(16.0));
    }
}

#[cfg(test)]
mod look_resolve_tests {
    use super::resolve_look_target;
    use klipp_color::measure::LookTarget;

    /// #1579: a preset named alone takes its numbers and what it protects;
    /// named with a list of its own, keeps that list. Control: a look of
    /// numbers is left as given.
    #[test]
    fn a_named_look_keeps_what_it_says_it_protects() {
        let golden = resolve_look_target(LookTarget { name: "golden".into(), ..Default::default() });
        assert_eq!(golden, LookTarget::preset("golden").unwrap());
        let warm = resolve_look_target(LookTarget { name: "warm_interview".into(), protect: Some(vec!["skin".into()]), ..Default::default() });
        assert_eq!(warm.cast_b, LookTarget::preset("warm_interview").unwrap().cast_b);
        assert!(warm.protects("skin") && !warm.protects("sky"));
        let numbers = LookTarget { name: "mine".into(), cast_b: Some(7.0), mid_l: Some(50.0), ..Default::default() };
        assert_eq!(resolve_look_target(numbers.clone()), numbers);
    }
}

#[cfg(test)]
mod delivery_tests {
    use super::delivery_findings;
    use klipp_document::qc::Severity;

    fn rules(found: &[(Severity, &'static str, String, String)]) -> Vec<(Severity, &'static str)> {
        found.iter().map(|f| (f.0, f.1)).collect()
    }

    /// #2481: with no target, a mix past full scale is an error (it is what
    /// is delivered) and one over the ceiling a warning; with the brief's
    /// target, a mix the export can bring there under the ceiling is fine,
    /// one far from it a warning, and one whose peaks the gain would push
    /// over the ceiling an error. The measurement is always said.
    #[test]
    fn the_mix_is_judged_as_its_delivery() {
        use Severity::{Error, Note, Warning};
        // The film's master: -7.1 LUFS, +0.4 dBTP.
        assert_eq!(rules(&delivery_findings(-7.1, 0.4, None, -1.0)), vec![(Note, "loudness_measured"), (Error, "loudness_overs")]);
        assert_eq!(rules(&delivery_findings(-14.0, -0.5, None, -1.0)), vec![(Note, "loudness_measured"), (Warning, "loudness_overs")]);
        assert_eq!(rules(&delivery_findings(-14.0, -3.0, None, -1.0)), vec![(Note, "loudness_measured")]);
        // With the brief's -14: brought down 6.9 dB, its peak -6.5 dBTP.
        assert_eq!(rules(&delivery_findings(-7.1, 0.4, Some(-14.0), -1.0)), vec![(Note, "loudness_measured"), (Warning, "loudness_target")]);
        assert_eq!(rules(&delivery_findings(-15.0, -4.0, Some(-14.0), -1.0)), vec![(Note, "loudness_measured")]);
        // A quiet mix with a loud peak: +10 dB would put it at +2 dBTP.
        let short = delivery_findings(-24.0, -8.0, Some(-14.0), -1.0);
        assert_eq!(rules(&short), vec![(Note, "loudness_measured"), (Error, "loudness_target")]);
        assert!(short[1].2.contains("+10.0 dB") && short[1].2.contains("fall short"), "{}", short[1].2);
        assert!(delivery_findings(-7.1, 0.4, None, -1.0)[0].2.contains("-7.1 LUFS") , "the measurement said");
        // Silence: measured, nothing judged.
        assert_eq!(rules(&delivery_findings(-70.0, -90.0, Some(-14.0), -1.0)), vec![(Note, "loudness_measured")]);
    }
}

#[cfg(test)]
mod skin_region_tests {
    use super::skin_regions_in;
    use klipp_analysis::framing::{FaceBox, FrameSample};

    /// #2493: a face too few of the file's pixels tall to read skin
    /// from is not read, and with no face model a person whose box holds
    /// no skin gives none: a head's place guessed there was background.
    #[test]
    fn a_face_too_small_or_a_person_with_no_skin_is_not_read() {
        let face = |h: f32| FaceBox { x: 0.4, y: 0.2, w: h * 0.75, h, score: 0.95, landmarks: vec![] };
        let grey = vec![128u8; 64 * 36 * 4];
        let sample = FrameSample { secs: 0.0, people: vec![(0.3, 0.1, 0.4, 0.8)], faces: vec![face(0.03), face(0.3)] };
        // At 360 rows the small face is 11 rows; at 2160, 65.
        assert_eq!(skin_regions_in(&sample, true, 360, &grey, 64, 36).len(), 1, "only the large face at 360p");
        assert_eq!(skin_regions_in(&sample, true, 2160, &grey, 64, 36).len(), 2, "both at 2160p");
        // No face model, a grey person: nothing to read.
        let person = FrameSample { secs: 0.0, people: vec![(0.3, 0.1, 0.4, 0.8)], faces: vec![] };
        assert!(skin_regions_in(&person, false, 360, &grey, 64, 36).is_empty());
    }
}
