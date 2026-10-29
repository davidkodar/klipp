//! Cuts: selects, the pacing a style asks for, cut points, cuts to a
//! budget, the music bed's beats and what to do next (#826).

#[allow(unused_imports)]
use super::*;

/// A keep-range cut out of a long take: where it starts and ends in the
/// file, and what is said in it.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct Select {
    pub in_secs: f64,
    pub out_secs: f64,
    pub text: String,
    /// True when the range is only filler (um, uh) or too short to use —
    /// a candidate to drop, not to keep.
    pub filler: bool,
}


/// Whether a take's words are too thin to cut it by (#2489): a take of
/// half a minute or more with speech under a tenth of it, or words whose
/// times collapsed (most under a twentieth of a second, the transcriber
/// unsure where they fell). A select made of them is a word for a whole
/// take, or seven words in a quarter second.
pub fn speech_too_thin(words: &[(f64, f64, String)], duration_secs: f64) -> bool {
    if words.is_empty() {
        return true;
    }
    let spoken: f64 = words.iter().map(|(a, b, _)| (b - a).max(0.0)).sum();
    let collapsed = words.iter().filter(|(a, b, _)| b - a < 0.05).count();
    (duration_secs >= 30.0 && spoken < duration_secs * 0.1) || collapsed * 2 > words.len()
}

/// A take's selects from its picture: one per shot, at the shot changes
/// the analysis found (`boundaries`, after the first shot's start), the
/// whole take as one when it found none.
pub fn picture_selects(boundaries: &[f64], duration_secs: f64) -> Vec<Select> {
    let mut edges: Vec<f64> = std::iter::once(0.0).chain(boundaries.iter().copied().filter(|b| *b > 0.0 && *b < duration_secs)).collect();
    edges.push(duration_secs);
    edges
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[1] - w[0] > 0.0)
        .map(|(k, w)| Select { in_secs: w[0], out_secs: w[1], text: format!("shot {} (picture)", k + 1), filler: false })
        .collect()
}

/// Group a long take's words into selects — the runs of speech between
/// the real pauses — so an agent can keep the good takes and drop the
/// dead air and the filler, building a tight cut from a loose one. A gap
/// of `min_gap` seconds or more starts a new select; a run that is only
/// filler, or shorter than `min_len`, is flagged to drop.
pub fn suggest_selects(words: &[(f64, f64, String)], min_gap: f64, min_len: f64, lists: Option<&klipp_core::lang::Words>) -> Vec<Select> {
    let mut selects: Vec<Select> = Vec::new();
    let mut cur: Vec<&(f64, f64, String)> = Vec::new();
    let flush = |cur: &mut Vec<&(f64, f64, String)>, out: &mut Vec<Select>| {
        if cur.is_empty() {
            return;
        }
        let in_secs = cur.first().unwrap().0;
        let out_secs = cur.last().unwrap().1;
        let text = cur.iter().map(|w| w.2.trim()).collect::<Vec<_>>().join(" ");
        // Only filler in the transcript's language (#2889); none with no
        // list for it.
        let all_filler = lists.is_some_and(|l| cur.iter().all(|w| l.is_filler(&w.2)));
        let too_short = out_secs - in_secs < min_len;
        out.push(Select { in_secs, out_secs, text, filler: all_filler || too_short });
        cur.clear();
    };
    for w in words {
        if let Some(prev) = cur.last() {
            if w.0 - prev.1 >= min_gap {
                flush(&mut cur, &mut selects);
            }
        }
        cur.push(w);
    }
    flush(&mut cur, &mut selects);
    selects
}

/// Summarise a reference's editing from its shots and beats: the cut
/// cadence, and how tightly the cuts fall on the beat. Pure, so it is
/// unit-tested without ffmpeg. `shots` are `(start, end)` in seconds,
/// `boundaries` the cut times, `beats` the tempo grid.
pub fn style_cut_summary(shots: &[(f64, f64)], boundaries: &[f64], duration: f64, beats: &klipp_analysis::beats::BeatData) -> serde_json::Value {
    let mut lengths: Vec<f64> = shots.iter().map(|(a, b)| b - a).filter(|d| *d > 0.0).collect();
    lengths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = lengths.len();
    let mean = if n > 0 { lengths.iter().sum::<f64>() / n as f64 } else { 0.0 };
    let median = if n > 0 { lengths[n / 2] } else { 0.0 };
    let cuts_per_min = if duration > 0.0 { boundaries.len() as f64 / (duration / 60.0) } else { 0.0 };
    let pace = if mean == 0.0 { "unknown" } else if mean < 1.5 { "fast" } else if mean < 4.0 { "medium" } else { "slow" };
    // How many cuts land on a beat (within a 12th of a second).
    let on_beat = if beats.beat_times.len() >= 2 && beats.confidence > 0.3 && !boundaries.is_empty() {
        let hits = boundaries.iter().filter(|&&c| {
            beats.beat_times.iter().any(|&b| (b - c).abs() < 0.12)
        }).count();
        Some(hits as f64 / boundaries.len() as f64)
    } else {
        None
    };
    serde_json::json!({
        "shot_count": n,
        "mean_shot_secs": (mean * 100.0).round() / 100.0,
        "median_shot_secs": (median * 100.0).round() / 100.0,
        "shortest_shot_secs": lengths.first().copied().unwrap_or(0.0),
        "longest_shot_secs": lengths.last().copied().unwrap_or(0.0),
        "cuts_per_min": (cuts_per_min * 10.0).round() / 10.0,
        "pace": pace,
        "cut_on_beat_ratio": on_beat,
    })
}

/// Evidence about development inside detected shots, separate from cut cadence.
/// Luma differences and freeze detection measure change, not the meaning of an
/// animation, and must not be converted into a universal shot-length recipe.
#[derive(serde::Serialize)]
pub(crate) struct PacingEvidence {
    pub(crate) within_shot_mean_ydif: Option<f32>,
    pub(crate) within_shot_sample_seconds: usize,
    pub(crate) valid_sample_seconds: usize,
    pub(crate) cut_exclusion: &'static str,
    pub(crate) within_shot_motion_class: Option<&'static str>,
    pub(crate) motion_evidence: &'static str,
    pub(crate) detected_holds: Option<Vec<serde_json::Value>>,
    pub(crate) hold_evidence: &'static str,
    pub(crate) guidance: String,
}

pub fn style_pacing_evidence(
    shots: &klipp_analysis::shots::Shots,
    motion: Option<&klipp_analysis::motion::Motion>,
    sheet: Option<&klipp_analysis::beatsheet::BeatSheet>,
) -> serde_json::Value {
    serde_json::to_value(pacing_evidence(shots, motion, sheet)).expect("finite pacing evidence")
}

pub(crate) fn pacing_evidence(
    shots: &klipp_analysis::shots::Shots,
    motion: Option<&klipp_analysis::motion::Motion>,
    sheet: Option<&klipp_analysis::beatsheet::BeatSheet>,
) -> PacingEvidence {
    // Preserve actual detector provenance. An exact beat-aligned fast cut
    // excludes only its own bin. Widen only known slow-only detections.
    let mut excluded = std::collections::HashSet::new();
    for &b in &shots.boundaries { excluded.insert(b.floor() as usize); }
    for &b in &shots.slow_boundaries { excluded.insert((b - 0.5).max(0.0).floor() as usize); }
    let mut valid = 0;
    let mut samples = Vec::new();
    if let Some(m) = motion {
        for (i, &v) in m.per_second.iter().enumerate() {
            if let Some(v) = v {
                valid += 1;
                if !excluded.contains(&i) { samples.push(v); }
            }
        }
    }
    let within_mean = klipp_analysis::motion::mean_of(samples.iter().copied());
    let holds = sheet.map(|s| s.segments.iter()
        .filter(|s| s.kind == "idle")
        .map(|s| serde_json::json!({"start_secs": s.start_secs, "end_secs": s.end_secs}))
        .collect::<Vec<_>>());
    let motion_class = within_mean.map(klipp_analysis::motion::class_for);
    let motion_evidence = match (motion.is_some(), valid, samples.len()) {
        (false, _, _) => "unavailable",
        (true, 0, _) => "no_valid_samples",
        (true, _, 0) => "excluded_by_cuts",
        _ => "measured",
    };
    let mut guidance = String::from("Detected-shot durations describe cut cadence only; they are not target hold lengths or animation timings. ");
    match motion_class {
        Some("static") => guidance.push_str("Measured within-shot change is low, but small animated elements can be missed by a whole-frame average. Inspect the reference before choosing holds. "),
        Some(_) => guidance.push_str("Measured change continues inside detected shots. Inspect entrances, reveals, transformations and exits before choosing edit timings. "),
        None if motion_evidence == "excluded_by_cuts" => guidance.push_str("Motion was analysed, but every valid one-second sample overlaps a cut or its timing uncertainty; within-shot motion cannot be isolated at this sampling rate. "),
        None if motion_evidence == "no_valid_samples" => guidance.push_str("No decoded motion samples are available. Re-analyse the reference and check that its video stream decodes. "),
        None => guidance.push_str("Within-shot motion evidence is unavailable; cut statistics alone cannot establish the reference's visual pace. Re-analyse the reference to populate missing or outdated analysis. "),
    }
    if holds.as_ref().is_some_and(|h| !h.is_empty()) {
        guidance.push_str("Detected settled intervals are inspection candidates, not prescribed hold lengths. ");
    }
    guidance.push_str("Luma differences can include gradual transitions beyond the detector timing window. Luma differences and freeze detection do not identify semantic events or distinguish motion graphics from camera or subject movement. ");
    PacingEvidence {
        within_shot_mean_ydif: within_mean,
        within_shot_sample_seconds: samples.len(),
        valid_sample_seconds: valid,
        cut_exclusion: "boundary bins plus up to 0.5 s before known slow-only detections",
        within_shot_motion_class: motion_class,
        motion_evidence,
        detected_holds: holds,
        hold_evidence: match sheet { None => "unavailable", Some(s) if s.has_freeze => "detected", Some(_) => "no_settled_intervals_detected" },
        guidance,
    }
}

/// Analysis inputs used to derive both the motion summary and pacing evidence.
pub struct StylePacingInput<'a> {
    pub shots: &'a klipp_analysis::shots::Shots,
    pub motion: Option<&'a klipp_analysis::motion::Motion>,
    pub sheet: Option<&'a klipp_analysis::beatsheet::BeatSheet>,
}

/// Assemble the complete StyleProfile response after media analysis and scope
/// measurements. Keeping the returned text, guidance and next step together
/// lets regressions exercise the same response the control handler sends.
pub fn style_profile_response(
    filename: &str,
    media_id: &str,
    cuts: serde_json::Value,
    beats: &klipp_analysis::beats::BeatData,
    look: serde_json::Value,
    analysis: StylePacingInput<'_>,
) -> serde_json::Value {
    let pacing = pacing_evidence(analysis.shots, analysis.motion, analysis.sheet);
    let motion = analysis.motion.and_then(klipp_analysis::motion::Motion::overall_mean).map(|mean| json!({
        "class": klipp_analysis::motion::class_for(mean), "mean": mean,
        "scope": "whole_file_including_cuts",
    }));
    // Guidance: turn the numbers into how to build the same feel.
    let pace = cuts["pace"].as_str().unwrap_or("medium");
    let mean_shot = cuts["mean_shot_secs"].as_f64().unwrap_or(0.0);
    let mut guide = pacing.guidance.clone();
    if beats.confidence > 0.3 {
        let onb = cuts["cut_on_beat_ratio"].as_f64().unwrap_or(0.0);
        if onb > 0.4 {
            guide.push_str(&format!("It cuts on the beat ({:.0}% of cuts) at {:.0} BPM — place your cuts on beat times (klipp_beats on your music). ", onb * 100.0, beats.bpm));
        } else {
            guide.push_str(&format!("The music runs at {:.0} BPM; compare cuts and internal animation accents against the beat. ", beats.bpm));
        }
    }
    if let Some(l) = look.as_object() {
        guide.push_str(&format!("Grade toward the look ({}): blacks ~{} IRE, whites ~{} IRE, saturation ~{}%. Use match_grade against a frame of the reference, or look for a LUT. ", l.get("summary").and_then(|v| v.as_str()).unwrap_or(""), l.get("black_ire").and_then(|v| v.as_f64()).unwrap_or(0.0), l.get("white_ire").and_then(|v| v.as_f64()).unwrap_or(0.0), l.get("saturation_pct").and_then(|v| v.as_f64()).unwrap_or(0.0)));
    }
    guide.push_str("Match the energy with your own or licensed music, not the reference's track.");

    let text = format!(
        "Style of {}:\ndetected cuts — {} shots, mean {:.1} s, {:.0} per minute ({pace} cut cadence){}\nlook — {}\nwhole-file luma change (includes cuts) — {}\nwithin-shot luma change — {}\n\n{guide}",
        quoted(filename),
        cuts["shot_count"].as_i64().unwrap_or(0),
        mean_shot,
        cuts["cuts_per_min"].as_f64().unwrap_or(0.0),
        if beats.confidence > 0.3 { format!("; {:.0} BPM", beats.bpm) } else { String::new() },
        look.get("summary").and_then(|v| v.as_str()).unwrap_or("not measured"),
        motion.as_ref().and_then(|m| m.get("class")).and_then(|v| v.as_str()).unwrap_or("unknown"),
        pacing.within_shot_motion_class.unwrap_or(match pacing.motion_evidence {
            "no_valid_samples" => "no decoded samples",
            "excluded_by_cuts" => "excluded by cuts",
            _ => "unavailable",
        }),
    );
    json!({
        "text": text, "media_id": media_id, "filename": filename,
        "cuts": cuts,
        "tempo": { "bpm": beats.bpm, "confidence": beats.confidence },
        "look": look,
        "motion": motion,
        "guidance": guide,
        "pacing_evidence": pacing,
        "next": if matches!(pacing.motion_evidence, "unavailable" | "no_valid_samples") { "Re-analyse the reference to populate missing or outdated motion analysis, then request style_profile again." } else { "Inspect changes inside the detected shots, including reveals, movement and settled intervals. Choose cut and animation timings from that review; compare accents with the beat and grade toward the measured look using your own footage and music." },
    })
}

/// Where a cut is clean, and where it is not. Folds the measured
/// silences, the transcript's sentence boundaries, and the shot changes
/// into ranked cut opportunities, and — when `near` is given —
/// classifies that moment as clean, mid-sentence, or mid-word, with the
/// nearest clean point. So an agent trims and places on a pause, a
/// full stop or a shot change, and never through the middle of a word
/// or a sentence.
///
/// `words` and `segments` are `(start, end, text)` in file seconds,
/// `silences` are `(start, end)` gaps, `shots` are boundary times.
pub fn cut_points(
    words: &[(f64, f64, String)],
    segments: &[(f64, f64, String)],
    silences: &[(f64, f64)],
    shots: &[f64],
    near: Option<f64>,
    range: (f64, f64),
) -> serde_json::Value {
    let (a, b) = range;
    let in_range = |t: f64| t >= a - 1e-6 && t <= b + 1e-6;
    let ends_sentence = |s: &str| s.trim_end().chars().last().is_some_and(|c| matches!(c, '.' | '?' | '!' | '…'));
    // strength: 3 a silent gap (cleanest), 2 a full stop or a shot cut, 1 a phrase break.
    let mut pts: Vec<(f64, u8, &'static str, String)> = Vec::new();
    for &(s, e) in silences {
        let mid = (s + e) / 2.0;
        if in_range(mid) {
            pts.push((mid, 3, "silence", format!("a {:.2} s pause ({s:.2}-{e:.2})", e - s)));
        }
    }
    for (s, e, text) in segments {
        if ends_sentence(text) {
            if in_range(*e) {
                pts.push((*e, 2, "sentence_end", "a sentence ends".into()));
            }
            if in_range(*s) {
                pts.push((*s, 2, "sentence_start", "a sentence begins".into()));
            }
        } else {
            if in_range(*e) {
                pts.push((*e, 1, "phrase", "a phrase break".into()));
            }
            if in_range(*s) {
                pts.push((*s, 1, "phrase", "a phrase begins".into()));
            }
        }
    }
    for &t in shots {
        if in_range(t) {
            pts.push((t, 2, "shot_change", "the picture cuts".into()));
        }
    }
    // Merge points within a frame of each other, keeping the strongest.
    pts.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut merged: Vec<(f64, u8, &'static str, String)> = Vec::new();
    for p in pts {
        match merged.last_mut() {
            Some(last) if (p.0 - last.0).abs() < 0.05 => {
                if p.1 > last.1 {
                    *last = p;
                }
            }
            _ => merged.push(p),
        }
    }

    // Classify the target moment, if one was asked about.
    let target = near.map(|t| {
        let in_word = words.iter().find(|(s, e, _)| t > *s + 1e-3 && t < *e - 1e-3);
        let in_silence = silences.iter().any(|(s, e)| t >= *s && t <= *e);
        let (verdict, why) = if let Some((_, _, w)) = in_word {
            ("mid-word", format!("the cut lands inside the word {} — it would clip the sound", quoted(w)))
        } else if in_silence {
            ("clean", "the cut lands in a pause".into())
        } else if segments.iter().any(|(s, e, _)| t > *s && t < *e) {
            ("mid-sentence", "the cut lands inside a spoken sentence".into())
        } else {
            ("clean", "the cut lands between segments".into())
        };
        // The nearest clean point (silence or sentence end/start) to move to.
        let nearest = merged.iter().filter(|p| p.1 >= 2).min_by(|x, y| (x.0 - t).abs().partial_cmp(&(y.0 - t).abs()).unwrap_or(std::cmp::Ordering::Equal));
        json!({
            "secs": t,
            "verdict": verdict,
            "why": why,
            "nearest_clean_secs": nearest.map(|p| p.0),
            "nearest_clean_delta_secs": nearest.map(|p| p.0 - t),
        })
    });

    // The list the agent reads: nearest first when a target was given.
    let mut ordered: Vec<&(f64, u8, &'static str, String)> = merged.iter().collect();
    if let Some(t) = near {
        ordered.sort_by(|x, y| (x.0 - t).abs().partial_cmp(&(y.0 - t).abs()).unwrap_or(std::cmp::Ordering::Equal));
    }
    let points: Vec<serde_json::Value> = ordered.iter().take(24).map(|(s, strength, kind, why)| json!({
        "secs": s, "kind": kind, "why": why, "strength": strength,
    })).collect();

    let mut text = String::new();
    if let Some(tg) = &target {
        text.push_str(&format!("At {:.2} s: {} — {}\n", near.unwrap_or(0.0), tg["verdict"].as_str().unwrap_or(""), tg["why"].as_str().unwrap_or("")));
        if let Some(n) = tg["nearest_clean_secs"].as_f64() {
            text.push_str(&format!("Nearest clean cut: {:.2} s ({:+.2} s away)\n", n, n - near.unwrap_or(0.0)));
        }
    }
    text.push_str("Clean cut points (strength 3 a pause, 2 a full stop or shot cut, 1 a phrase break):\n");
    for p in &points {
        text.push_str(&format!("  {:.2} s  {}  {}\n", p["secs"].as_f64().unwrap_or(0.0), p["kind"].as_str().unwrap_or(""), p["why"].as_str().unwrap_or("")));
    }
    json!({ "text": text, "target": target, "points": points })
}

pub(crate) fn cut_to_budget(text: &str, max_chars: usize) -> (String, bool) {
    cut_to_budget_with(text, max_chars, "ask again with a larger max_chars, or list_clips for the rest")
}

pub(crate) fn cut_to_budget_with(text: &str, max_chars: usize, rest: &str) -> (String, bool) {
    if text.chars().count() <= max_chars {
        return (text.to_string(), false);
    }
    let mut kept = String::new();
    for line in text.lines() {
        if kept.chars().count() + line.chars().count() + 1 > max_chars.saturating_sub(120) {
            break;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    kept.push_str(&format!("… cut at {max_chars} chars of {}; {rest}.\n", text.chars().count()));
    (kept, true)
}

/// What the timeline holds that the next step would otherwise advise
/// again (#785).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Trail {
    /// The ops placed pictures, not only sound: selects, not effects.
    pub(crate) pictures_placed: bool,
    /// A title is on a text track.
    pub(crate) titled: bool,
    /// A picture clip is graded, alone or in a grade group.
    pub(crate) graded: bool,
    /// Music is on a track: a clip playing as music, or a track from the
    /// music library.
    pub(crate) music_on_track: bool,
    /// Most cuts between pictures fall on that music's beats: the cut is
    /// to it already, and the beat is not advised again (#785). False
    /// until the music's beats have been read once.
    pub(crate) cut_to_beat: bool,
}

/// The music bed: the longest audio clip that plays as music, or comes
/// from the music library, with its file.
pub(crate) fn music_bed<'a>(seq: &'a klipp_timeline::Sequence, pool: &klipp_media::MediaPool) -> Option<(&'a klipp_timeline::Clip, std::path::PathBuf)> {
    seq.tracks
        .iter()
        .filter(|t| t.kind == klipp_timeline::TrackKind::Audio)
        .flat_map(|t| t.clips.iter())
        .filter_map(|c| {
            let item = pool.get(&c.media_id()?)?;
            let music = sound_role(seq, c) == klipp_core::ClipRole::Music
                || klipp_audio_search::library_source(&item.path) == Some(klipp_audio_search::Source::JamendoMusic);
            music.then(|| (c, item.path.clone()))
        })
        .max_by_key(|(c, _)| (c.timeline_out - c.timeline_in).value)
}

/// Whether most cuts between pictures fall within 0.12 s of one of the
/// bed's beats (the rule style_cut_summary uses), the bed placed as it is:
/// its beats are the file's, carried to the timeline through the clip's
/// source range and speed. Three cuts at least, so two that happen to
/// land on a beat do not count as a cut to the music.
pub(crate) fn cuts_on_beat(seq: &klipp_timeline::Sequence, bed: &klipp_timeline::Clip, beats: &klipp_analysis::beats::BeatData) -> bool {
    if beats.confidence <= 0.3 || beats.beat_times.len() < 2 {
        return false;
    }
    let (s_in, s_out) = (bed.source_in.to_secs_f64(), bed.source_out.to_secs_f64());
    let speed = bed.speed.abs().max(1e-3);
    let on_timeline: Vec<f64> = beats
        .beat_times
        .iter()
        .filter(|&&b| b >= s_in && b < s_out)
        .map(|&b| bed.timeline_in.to_secs_f64() + (b - s_in) / speed)
        .collect();
    let mut cuts = Vec::new();
    for t in seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video) {
        let mut clips: Vec<&klipp_timeline::Clip> = t.clips.iter().filter(|c| is_picture_clip(c)).collect();
        clips.sort_by_key(|c| c.timeline_in.value);
        for w in clips.windows(2) {
            if (w[1].timeline_in.to_secs_f64() - w[0].timeline_out.to_secs_f64()).abs() < 0.02 {
                cuts.push(w[1].timeline_in.to_secs_f64());
            }
        }
    }
    if cuts.len() < 3 {
        return false;
    }
    let hits = cuts.iter().filter(|&&c| on_timeline.iter().any(|&b| (b - c).abs() < 0.12)).count();
    hits * 2 > cuts.len()
}

/// The bed's beats if they have been read; if not, read in the
/// background, once per file, for the next apply. Reading takes a decode
/// of the music, too slow to wait for in a reply.
pub(crate) fn bed_beats(path: &std::path::Path) -> Option<klipp_analysis::beats::BeatData> {
    if let Some(b) = klipp_analysis::beats::cached(path) {
        return Some(b);
    }
    static READING: std::sync::OnceLock<Mutex<std::collections::HashSet<std::path::PathBuf>>> = std::sync::OnceLock::new();
    let reading = READING.get_or_init(Default::default);
    if reading.lock().map(|mut r| r.insert(path.to_path_buf())).unwrap_or(false) {
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            if let Err(e) = klipp_analysis::beats::read_and_cache(std::path::Path::new("ffmpeg"), &path) {
                tracing::debug!("beats of {} not read: {e}", path.display());
            }
            if let Ok(mut r) = reading.lock() {
                r.remove(&path);
            }
        });
    }
    None
}

impl Trail {
    pub(crate) fn read(ctx: &ControlContext, ops: &[klipp_document::ops::Op]) -> Trail {
        let Ok(seq) = ctx.sequence.read() else { return Trail::default() };
        let pictures_placed = ctx.media_pool.read().is_ok_and(|pool| {
            ops.iter().any(|o| match o {
                klipp_document::ops::Op::Place { media_id, .. } => pool.get(media_id).is_some_and(|m| m.is_still || !m.video_codec.is_empty()),
                _ => false,
            })
        });
        let clips = || seq.tracks.iter().flat_map(|t| t.clips.iter().map(move |c| (t, c)));
        let pool = ctx.media_pool.read().ok();
        let bed = pool.as_ref().and_then(|p| music_bed(&seq, p));
        Trail {
            pictures_placed,
            titled: clips().any(|(t, c)| t.kind == klipp_timeline::TrackKind::Text && matches!(c.kind, klipp_timeline::ClipKind::Title(_))),
            graded: clips().any(|(_, c)| is_picture_clip(c) && (!c.own_grade().is_identity() || c.grade_group.is_some())),
            music_on_track: bed.is_some(),
            cut_to_beat: bed.is_some_and(|(clip, path)| bed_beats(&path).is_some_and(|b| cuts_on_beat(&seq, clip, &b))),
        }
    }
}

/// What to do after a batch of ops was applied, from the ops in it: the
/// reply points at the next step so a session follows a trail rather
/// than recalling a recipe.
/// The brief shapes it (#743): a piece that is not captioned is not told
/// to caption, nor a montage cut to music to cover its joins like an
/// interview's. And the timeline does (#785): sound effects placed are
/// not selects, and titles or a grade already there are not advised.
pub(crate) fn next_after_ops(ops: &[klipp_document::ops::Op], brief: Option<&klipp_timeline::Brief>, trail: Trail) -> Option<String> {
    use klipp_document::ops::Op;
    let has = |f: &dyn Fn(&Op) -> bool| ops.iter().any(f);
    let uncaptioned = brief.is_some_and(|b| !b.captions);
    let selects = trail.pictures_placed && has(&|o| matches!(o, Op::Place { with_audio: true, .. })) && ops.len() >= 2;
    // What is left after the cut, in order, ending in the check.
    let then = || {
        let mut left: Vec<&str> = Vec::new();
        if !trail.titled {
            left.push("titles in the brief's type styles");
        }
        if !trail.graded {
            left.push("the grade");
        }
        match left.as_slice() {
            [] => ", then check.".to_string(),
            [one] => format!(", then {one} and check."),
            many => format!(", then {}, and check.", many.join(", ")),
        }
    };
    let said: &str = if has(&|o| matches!(o, Op::Redact { .. })) {
        "Redacted. Look at a frame inside the stretch (frame_at) to see the box covers what it should — the copy on the track above hides it — then check again."
    } else if selects && uncaptioned && brief.is_some_and(|b| b.music) {
        let beat = if trail.music_on_track && trail.cut_to_beat { "" } else { ", cut them to the music's beat (klipp_beats)" };
        return Some(format!("The selects are down. Look at them together (frame_at across the cut){beat}{}", then()));
    } else if selects && uncaptioned {
        return Some(format!("The selects are down. Look at them together (frame_at across the cut) and tighten each cut{}", then()));
    } else if selects {
        "The selects are down. Where two from the same set-up meet is a jump cut: cover the joins with B-roll on a track above (place with_audio false), or j_cut the sound. Then captions, then check."
    } else if has(&|o| matches!(o, Op::Captions { .. })) {
        "Captions are down. Read the cues in the reply and correct any the transcript misheard with set_text. Then check, and export with the brief's loudness."
    } else if has(&|o| matches!(o, Op::LowerThird { .. })) {
        "The lower third is placed. Look at a frame with frame_at to see it sits clear of anything burnt into the picture, then move it with style if not."
    } else if has(&|o| matches!(o, Op::Duck { .. } | Op::Audio { .. })) {
        "The sound is set. Check holds the mix to the craft; then export at the brief's loudness."
    } else if has(&|o| matches!(o, Op::Place { .. })) && !trail.pictures_placed {
        "Placed. Hear it against the picture (audio_excerpt across where it lands), and run check when the cut is together."
    } else if has(&|o| matches!(o, Op::Place { .. })) {
        "Placed. Look with frame_at or contact_sheet, and run check when the cut is together."
    } else {
        "Run check to hold the cut to the brief and the craft before you export."
    };
    Some(said.to_string())
}

#[cfg(test)]
mod language_tests {
    use super::suggest_selects;

    /// #2889: a Swedish take's runs of only hesitation are dropped by the
    /// Swedish list, and kept by the English one, which does not know them;
    /// with no list, nothing is dropped as filler.
    #[test]
    fn a_second_languages_fillers_are_dropped() {
        let w = |a: f64, b: f64, t: &str| (a, b, t.to_string());
        let words = vec![
            w(0.0, 0.5, "Vi"), w(0.5, 1.0, "åkte"), w(1.0, 1.6, "till"), w(1.6, 2.2, "havet."),
            w(3.5, 4.0, "Öh,"), w(4.0, 4.6, "ähm..."),
            w(6.0, 6.5, "Det"), w(6.5, 7.0, "var"), w(7.0, 7.8, "fint."),
        ];
        let sv = klipp_core::lang::words(Some("sv")).unwrap();
        let dropped = |l: Option<&klipp_core::lang::Words>| suggest_selects(&words, 0.6, 0.35, l).into_iter().filter(|s| s.filler).map(|s| s.text).collect::<Vec<_>>();
        assert_eq!(dropped(Some(&sv)), ["Öh, ähm..."]);
        assert!(dropped(Some(&klipp_core::lang::english())).is_empty(), "English does not know them");
        assert!(dropped(None).is_empty());
    }
}
