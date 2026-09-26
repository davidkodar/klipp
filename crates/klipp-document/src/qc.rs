//! The check before a piece goes out: what an editor would catch on a
//! last watch, read off the timeline and the brief. Nothing here
//! changes anything; every finding says where, what, and what would
//! put it right.

use klipp_media::MediaPool;
use klipp_timeline::{Clip, ClipKind, Sequence, TitleObjectContent, TrackKind};
use serde::Serialize;

use crate::ops::{is_heard, linear_to_db, sound_settings_clip, ScreenBeats, SensitiveText, SpokenWord, Transcripts};

/// A cut this close to a settle point is a cut to the settle.
const SETTLE_NEAR_SECS: f64 = 0.6;
/// A screen needs about this long to be read, until OCR can say how much is on it.
const READ_MIN_SECS: f64 = 2.0;
/// A feature of the brief needs at least this long on screen to register.
const FEATURE_MIN_SECS: f64 = 3.0;
/// A title over a screen that is changing for longer than this fights it.
const BUSY_UNDER_TITLE_SECS: f64 = 0.5;
/// Shots all within this much of their mean length are a flat cadence.
const FLAT_CADENCE: f64 = 0.15;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The piece should not go out like this.
    Error,
    /// An editor would look at this and decide.
    Warning,
    /// Worth knowing; nothing wrong.
    Note,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
    pub severity: Severity,
    /// The rule that found it, for a caller that wants to filter.
    pub rule: &'static str,
    /// Where on the timeline, when it is somewhere.
    pub at_secs: Option<f64>,
    pub what: String,
    pub hint: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub duration_secs: f64,
    pub errors: usize,
    pub warnings: usize,
    pub findings: Vec<Finding>,
}

impl Report {
    /// The report as lines of text, the worst first.
    pub fn text(&self) -> String {
        let mut out = vec![format!(
            "Checked {:.2} s: {} error{}, {} warning{}, {} note{}.",
            self.duration_secs,
            self.errors,
            if self.errors == 1 { "" } else { "s" },
            self.warnings,
            if self.warnings == 1 { "" } else { "s" },
            self.findings.len() - self.errors - self.warnings,
            if self.findings.len() - self.errors - self.warnings == 1 { "" } else { "s" },
        )];
        for f in &self.findings {
            let tag = match f.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Note => "note",
            };
            let at = f.at_secs.map(|t| format!(" {t:.2} s")).unwrap_or_default();
            out.push(format!("[{tag}]{at}  {}. {}", f.what, f.hint));
        }
        out.join("\n")
    }
}

fn secs(t: klipp_core::Timecode) -> f64 {
    t.to_secs_f64()
}

fn clip_label(pool: &MediaPool, c: &Clip) -> String {
    match &c.kind {
        ClipKind::Media { media_id } => pool.get(media_id).map(|m| m.filename.clone()).unwrap_or_else(|| "a missing file".into()),
        ClipKind::Title(td) => format!("title \"{}\"", td.text().lines().next().unwrap_or("")),
        ClipKind::Generator(_) => "a generator".into(),
        _ => "a clip".into(),
    }
}

/// Whether a clip puts a picture on screen.
fn is_picture(c: &Clip) -> bool {
    c.enabled && !matches!(c.kind, ClipKind::Gap | ClipKind::AdjustmentLayer)
}

/// The stretches of `0..end` that none of `spans` covers, each at least `min` long.
fn uncovered(mut spans: Vec<(f64, f64)>, end: f64, min: f64) -> Vec<(f64, f64)> {
    spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = Vec::new();
    let mut at = 0.0;
    for (a, b) in spans {
        if a - at >= min {
            out.push((at, a));
        }
        at = at.max(b);
    }
    if end - at >= min {
        out.push((at, end));
    }
    out
}

/// Locate complete copies of the approved plan, including plans appended
/// after existing footage. A stale or partial plan must not relax checks
/// on unrelated clips merely because it has a montage label.
fn montage_spans(seq: &Sequence) -> Vec<(f64, f64)> {
    let Some(plan) = seq.edit_plan.as_ref().filter(|p| p.approved && !p.segments.is_empty()) else { return Vec::new() };
    let tolerance = 1.0 / seq.fps.as_f64().max(1.0) + 1e-6;
    let mut spans: Vec<(f64, f64)> = Vec::new();
    for track in seq.tracks.iter().filter(|t| t.visible && t.kind == TrackKind::Video) {
        for clips in track.clips.windows(plan.segments.len()) {
            let matches = clips.iter().zip(&plan.segments).all(|(c, s)| {
                c.enabled && c.media_id() == Some(s.media_id)
                    && (secs(c.source_in) - s.source_in_secs).abs() <= tolerance
                    && (secs(c.source_out) - s.source_out_secs).abs() <= tolerance
                    && (secs(c.timeline_out) - secs(c.timeline_in) - s.duration_secs()).abs() <= tolerance
            }) && clips.windows(2).all(|pair| (secs(pair[1].timeline_in) - secs(pair[0].timeline_out)).abs() <= tolerance);
            if !matches { continue; }
            for (clip, segment) in clips.iter().zip(&plan.segments) {
                if crate::edit_plan::is_montage(segment) {
                    spans.push((secs(clip.timeline_in), secs(clip.timeline_out)));
                }
            }
        }
    }
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (a, b) in spans {
        if let Some(last) = merged.last_mut().filter(|last| a <= last.1 + 1e-6) {
            last.1 = last.1.max(b);
        } else { merged.push((a, b)); }
    }
    merged
}

/// Check `seq` against its brief and the craft's rules.
pub fn check(seq: &Sequence, pool: &MediaPool, transcripts: &dyn Transcripts) -> Report {
    let mut f: Vec<Finding> = Vec::new();
    let mut add = |severity, rule, at_secs: Option<f64>, what: String, hint: &str| {
        f.push(Finding { severity, rule, at_secs, what, hint: hint.to_string() });
    };
    let fps = seq.fps.as_f64().max(1.0);
    let frame = 1.0 / fps;
    let duration = secs(seq.total_duration());
    let montages = montage_spans(seq);
    let in_montage = |a: f64, b: f64| montages.iter().any(|(start, end)| a + 1e-6 >= *start && b <= *end + 1e-6);
    let video_tracks: Vec<(usize, &klipp_timeline::Track)> = seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Video && t.visible).collect();
    let pictures: Vec<(usize, &Clip)> = video_tracks.iter().flat_map(|(ti, t)| t.clips.iter().filter(|c| is_picture(c)).map(move |c| (*ti, c))).collect();

    if pictures.is_empty() {
        add(Severity::Error, "empty", None, "there is no picture on the timeline".into(), "Place the piece first");
    }

    // Media that is not there.
    for t in &seq.tracks {
        for c in t.clips.iter().filter(|c| c.enabled) {
            if let ClipKind::Media { media_id } = &c.kind {
                match pool.get(media_id) {
                    None => add(Severity::Error, "offline", Some(secs(c.timeline_in)), "a clip's media is not in the pool".into(), "Remove the clip or import its file"),
                    Some(m) if m.offline => add(Severity::Error, "offline", Some(secs(c.timeline_in)), format!("{} is offline", m.filename), "Relink the file before delivering"),
                    _ => {}
                }
            }
        }
    }

    // Black: time with no picture under it.
    if !pictures.is_empty() {
        let picture_end = pictures.iter().map(|(_, c)| secs(c.timeline_out)).fold(0.0, f64::max);
        for (a, b) in uncovered(pictures.iter().map(|(_, c)| (secs(c.timeline_in), secs(c.timeline_out))).collect(), picture_end, frame * 0.9) {
            add(Severity::Error, "black", Some(a), format!("no picture from {a:.2} to {b:.2} s: {:.0} frame{} of black", (b - a) * fps, if (b - a) * fps >= 1.5 { "s" } else { "" }), "Close the gap, or cover it");
        }
        // Picture and sound ending apart.
        let sound_end = seq
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter().map(move |c| (t, c)))
            .filter(|(t, c)| is_heard(seq, pool, t, c))
            .map(|(_, c)| secs(c.timeline_out))
            .fold(0.0, f64::max);
        if sound_end > 0.0 && (sound_end - picture_end).abs() > frame * 1.5 {
            let (first, then) = if sound_end > picture_end { ("picture", "sound") } else { ("sound", "picture") };
            add(Severity::Warning, "ends_apart", Some(picture_end.min(sound_end)), format!("the {first} ends at {:.2} s and the {then} runs on to {:.2} s", picture_end.min(sound_end), picture_end.max(sound_end)), "End them together, or fade the one that runs on");
        }
    }

    // What the beat sheet says about each screen recording, read once.
    let mut beats_cache: std::collections::HashMap<klipp_core::MediaId, Option<ScreenBeats>> = std::collections::HashMap::new();
    let mut beats_of = |media_id: klipp_core::MediaId| -> Option<ScreenBeats> {
        beats_cache.entry(media_id).or_insert_with(|| pool.get(&media_id).and_then(|item| transcripts.screen_beats(item))).clone()
    };

    // Flash frames and jump cuts, track by track.
    for (ti, t) in &video_tracks {
        let clips: Vec<&Clip> = t.clips.iter().filter(|c| is_picture(c)).collect();
        for c in &clips {
            let frames = ((secs(c.timeline_out) - secs(c.timeline_in)) * fps).round() as i64;
            if frames < 8 {
                add(Severity::Warning, "flash", Some(secs(c.timeline_in)), format!("{} is on screen for {frames} frame{}", clip_label(pool, c), if frames == 1 { "" } else { "s" }), "A shot that short reads as a flash: lengthen it or take it out");
            }
        }
        for pair in clips.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let cut = secs(b.timeline_in);
            let same_media = matches!((&a.kind, &b.kind), (ClipKind::Media { media_id: x }, ClipKind::Media { media_id: y }) if x == y);
            let meets = (secs(a.timeline_out) - cut).abs() < frame * 0.5;
            let jumps = (secs(b.source_in) - secs(a.source_out)).abs() > 0.2;
            if !(same_media && meets && jumps) {
                continue;
            }
            // The picture changes shot in the file between the two: a
            // split screen becoming a stacked one is a cut, not a jump.
            let source_cut_between = match (&a.kind, a.media_id().and_then(|id| pool.get(&id))) {
                (ClipKind::Media { .. }, Some(item)) => {
                    let (lo, hi) = (secs(a.source_out).min(secs(b.source_in)), secs(a.source_out).max(secs(b.source_in)));
                    transcripts.shot_changes(item).iter().any(|t| *t > lo && *t < hi)
                }
                _ => false,
            };
            // On a screen recording, a cut to where a change has just
            // settled is the cut the beat sheet asked for, not a jump.
            let to_settle = a.media_id().and_then(&mut beats_of).is_some_and(|bt| bt.settles.iter().any(|s| (s - secs(b.source_in)).abs() <= SETTLE_NEAR_SECS));
            if source_cut_between || to_settle {
                continue;
            }
            // Covered when a picture on a track above runs across the cut,
            // or a title does: a card hides the join, a heading carries the
            // eye across it.
            let title_over = seq.tracks.iter().filter(|t| t.kind == TrackKind::Text && t.visible).flat_map(|t| t.clips.iter()).any(|c| c.enabled && matches!(c.kind, ClipKind::Title(_)) && secs(c.timeline_in) <= cut - 0.2 && secs(c.timeline_out) >= cut + 0.2);
            let covered = title_over || pictures.iter().any(|(tj, c)| tj < ti && secs(c.timeline_in) <= cut - 0.2 && secs(c.timeline_out) >= cut + 0.2);
            if !covered {
                add(Severity::Warning, "jump_cut", Some(cut), format!("a jump cut: {} cuts to itself, {:.1} s later in the file, with nothing over the join", clip_label(pool, a), secs(b.source_in) - secs(a.source_out)), "Cover the join with B-roll or a reaction, or keep it if jump cuts are the piece's style");
            }
        }
    }

    // Screen recordings: a cut in or out while the screen is still
    // changing lands mid-action; a screen needs time to be read.
    for (_, c) in &pictures {
        let Some(beats) = c.media_id().and_then(&mut beats_of) else { continue };
        let (s_in, s_out, t_in, t_out) = (secs(c.source_in), secs(c.source_out), secs(c.timeline_in), secs(c.timeline_out));
        let label = clip_label(pool, c);
        let inside = |t: f64| beats.action.iter().find(|(a, b)| t > *a + 0.15 && t < *b - 0.05).copied();
        if let Some((a, b)) = inside(s_in) {
            let settle = beats.settles.iter().copied().filter(|s| *s >= s_in).fold(None, |m: Option<f64>, s| Some(m.map_or(s, |m| m.min(s))));
            add(Severity::Warning, "cut_mid_action", Some(t_in), format!("{label} cuts in at {s_in:.2} s of the file while the screen is still changing ({a:.2}-{b:.2} s)"), &match settle {
                Some(s) => format!("Cut in at {s:.2} s of the file, where the change has settled, or at {a:.2} s before it starts"),
                None => format!("Cut in before the change starts at {a:.2} s, or after it settles at {b:.2} s"),
            });
        }
        if let Some((a, b)) = inside(s_out) {
            add(Severity::Warning, "cut_mid_action", Some(t_out), format!("{label} cuts away at {s_out:.2} s of the file in the middle of a change ({a:.2}-{b:.2} s)"), &format!("Hold until it settles at {b:.2} s of the file, or cut before the change starts at {a:.2} s"));
        }
        let len = t_out - t_in;
        if len + 1e-6 < READ_MIN_SECS && (len * fps).round() as i64 >= 8 && !in_montage(t_in, t_out) {
            add(Severity::Warning, "too_short_to_read", Some(t_in), format!("{label} shows a screen for {len:.1} s"), "A screen needs about two seconds to be read: hold it, or take it out");
        }
    }

    // Secrets on screen: an email, a key, a password, a card number the
    // recording caught, within the stretch of the file that is used.
    let mut sensitive_cache: std::collections::HashMap<klipp_core::MediaId, Vec<SensitiveText>> = std::collections::HashMap::new();
    for (_, c) in &pictures {
        let Some(media_id) = c.media_id() else { continue };
        let Some(item) = pool.get(&media_id) else { continue };
        let found = sensitive_cache.entry(media_id).or_insert_with(|| transcripts.screen_sensitive(item));
        let (s_in, s_out, t_in) = (secs(c.source_in), secs(c.source_out), secs(c.timeline_in));
        for s in found.iter().filter(|s| s.end_secs > s_in && s.start_secs < s_out) {
            let from = t_in + (s.start_secs - s_in).max(0.0);
            let to = t_in + (s.end_secs.min(s_out) - s_in);
            add(Severity::Error, "sensitive_on_screen", Some(from), format!("{} shows what looks like a {} ({}) from {from:.2} to {to:.2} s, at {:.0}% across and {:.0}% down the frame", clip_label(pool, c), s.kind, s.masked, s.x * 100.0, s.y * 100.0), "Redact it: the redact op with this clip, from, to and the box (auto_redact does every finding at once), or cut it out");
        }
    }

    // Titles: across a cut, too short to read, sharing the bottom of the frame with captions.
    let cuts: Vec<f64> = pictures.iter().flat_map(|(_, c)| [secs(c.timeline_in), secs(c.timeline_out)]).collect();
    let titles: Vec<&Clip> = seq.tracks.iter().filter(|t| t.kind == TrackKind::Text && t.visible).flat_map(|t| t.clips.iter()).filter(|c| c.enabled && matches!(c.kind, ClipKind::Title(_))).collect();
    let captions: Vec<&Clip> = seq.tracks.iter().filter(|t| t.kind == TrackKind::Subtitle && t.visible).flat_map(|t| t.clips.iter()).filter(|c| c.enabled).collect();
    for c in &titles {
        let ClipKind::Title(td) = &c.kind else { continue };
        let (a, b) = (secs(c.timeline_in), secs(c.timeline_out));
        if let Some(cut) = cuts.iter().copied().filter(|e| *e > a + frame && *e < b - frame).reduce(f64::min) {
            add(Severity::Warning, "title_across_cut", Some(cut), format!("{} is up from {a:.2} to {b:.2} s and the picture changes at {cut:.2} s", clip_label(pool, c)), "A title does not run across a shot change: move it inside one shot");
        }
        let words: usize = td.objects.iter().filter_map(|o| match &o.content { TitleObjectContent::TextBox(tb) if o.visible => Some(tb.text.split_whitespace().count()), _ => None }).sum();
        let montage = in_montage(a, b);
        let needs = (words as f64 * 0.3).max(if montage { 0.3 } else { 1.5 });
        if b - a + 1e-6 < needs {
            add(Severity::Warning, "title_short", Some(a), format!("{} has {words} word{} and {:.1} s to be read in", clip_label(pool, c), if words == 1 { "" } else { "s" }, b - a), &format!("Hold it at least {needs:.1} s: about a third of a second a word{}", if montage { " in a montage" } else { ", never under a second and a half" }));
        }
        // Over a screen that is changing, a title fights the picture.
        for (_, p) in &pictures {
            let Some(beats) = p.media_id().and_then(&mut beats_of) else { continue };
            let (t_in, t_out) = (secs(p.timeline_in), secs(p.timeline_out));
            let (lo, hi) = (a.max(t_in), b.min(t_out));
            if hi <= lo {
                continue;
            }
            let file_at = |t: f64| secs(p.source_in) + (t - t_in);
            let (f_lo, f_hi) = (file_at(lo), file_at(hi));
            if let Some((x, _)) = beats.action.iter().find(|(x, y)| f_hi.min(*y) - f_lo.max(*x) > BUSY_UNDER_TITLE_SECS) {
                let from = lo + (x - f_lo).max(0.0);
                let holds: Vec<String> = beats.holds.iter().filter(|(h0, h1)| h1 - h0 >= b - a).take(3).map(|(h0, h1)| format!("{h0:.1}-{h1:.1} s")).collect();
                add(Severity::Warning, "title_over_busy", Some(from), format!("{} is up over a changing screen from {from:.2} s", clip_label(pool, c)), &if holds.is_empty() { "Move it to where the screen holds still".to_string() } else { format!("Move it to where the screen holds still: {} of the file", holds.join(", ")) });
                break;
            }
        }
        // Over a face: the first thing the eye goes to, under words.
        if let Some(tb) = crate::ops::title_box(td) {
            let faces = crate::ops::faces_under(seq, pool, transcripts, a, b);
            if let Some(face) = faces.iter().find(|f| crate::ops::boxes_overlap(tb, **f, 0.04)) {
                let zone = crate::ops::clear_zone((tb[0] + tb[2] / 2.0, tb[1] + tb[3] / 2.0), (tb[2], tb[3]), &faces).and_then(|(_, z)| z);
                add(Severity::Error, "title_over_face", Some(a), format!("{} sits over a face (at {:.0}% across, {:.0}% down) while it is up from {a:.2} to {b:.2} s", clip_label(pool, c), (face[0] + face[2] / 2.0) * 100.0, (face[1] + face[3] / 2.0) * 100.0), &match zone { Some(z) => format!("Move it to the {z}, which is clear of every face for its span (a title op with x and y, or leave x and y out and it is placed clear)"), None => "No zone is clear of a face for its whole span: hold it where the shot has no face, or make it a card".to_string() });
            }
        }
        let low = td.objects.iter().any(|o| o.visible && o.transform.center.y > 0.72);
        if low {
            if let Some(cap) = captions.iter().find(|k| secs(k.timeline_in) < b && secs(k.timeline_out) > a) {
                let from = a.max(secs(cap.timeline_in));
                // A title set to one side and captions in the middle share
                // the band but not the space: worth a look, not a change.
                let title_x = td.objects.iter().filter(|o| o.visible).map(|o| o.transform.center.x).fold(0.5f32, |acc, x| if (x - 0.5).abs() > (acc - 0.5).abs() { x } else { acc });
                let caption_x = cap.title_data().and_then(|t| t.objects.first()).map(|o| o.transform.center.x).unwrap_or(0.5);
                if (title_x - caption_x).abs() > 0.3 {
                    add(Severity::Note, "captions_beside_title", Some(from), format!("{} and the captions share the bottom of the frame from {from:.2} s, at different sides", clip_label(pool, c)), "Look at one frame to see they do not touch");
                } else {
                    add(Severity::Warning, "captions_under_title", Some(from), format!("{} and the captions share the bottom of the frame from {from:.2} s", clip_label(pool, c)), "Lift the captions for that stretch (style the subtitle track with from, to and a y of about 0.7), or put the title up where nobody is speaking");
                }
            }
        }
    }

    // Sound: cuts inside a word, music left at full level under speech.
    let mut speech_spans: Vec<(f64, f64)> = Vec::new();
    let mut musics: Vec<&Clip> = Vec::new();
    let mut cache: std::collections::HashMap<klipp_core::MediaId, Option<Vec<SpokenWord>>> = std::collections::HashMap::new();
    let mut silence_cache: std::collections::HashMap<klipp_core::MediaId, Vec<(f64, f64)>> = std::collections::HashMap::new();
    for t in &seq.tracks {
        for c in t.clips.iter().filter(|c| is_heard(seq, pool, t, c)) {
            let Some(media_id) = c.media_id() else { continue };
            let Some(item) = pool.get(&media_id) else { continue };
            // A clip's role settles what it is before any transcript does:
            // a bed is music, an effect is neither speech nor music.
            match c.role {
                klipp_core::ClipRole::Music => {
                    musics.push(c);
                    continue;
                }
                klipp_core::ClipRole::SFX | klipp_core::ClipRole::Ambience | klipp_core::ClipRole::Foley => continue,
                _ => {}
            }
            let words = cache.entry(media_id).or_insert_with(|| transcripts.words(item));
            let (s_in, s_out) = (secs(c.source_in), secs(c.source_out));
            let inside: Vec<&SpokenWord> = words.as_deref().unwrap_or(&[]).iter().filter(|w| w.end_secs > s_in && w.start_secs < s_out).collect();
            if inside.is_empty() {
                if words.is_some() || item.video_codec.is_empty() {
                    musics.push(c);
                }
                continue;
            }
            speech_spans.push((secs(c.timeline_in), secs(c.timeline_out)));
            const INTO: f64 = 0.06;
            let silences = silence_cache.entry(media_id).or_insert_with(|| transcripts.silences(item));
            for (edge, at, name) in [(s_in, secs(c.timeline_in), "starts"), (s_out, secs(c.timeline_out), "ends")] {
                // In measured silence the cut is clean, whatever the words' times say.
                if silences.iter().any(|(a, b)| edge >= *a - 0.01 && edge <= *b + 0.01) {
                    continue;
                }
                if let Some(w) = inside.iter().find(|w| w.start_secs + INTO < edge && w.end_secs - INTO > edge) {
                    let to = if name == "starts" { w.start_secs } else { w.end_secs };
                    add(Severity::Warning, "word_cut", Some(at), format!("the sound {name} inside the word \"{}\"", w.text.trim()), &format!("Trim that edge to {to:.2} s of the file, the word's own edge, plus a beat"));
                }
            }
        }
    }
    let has_music = !musics.is_empty();
    for m in musics {
        let (a, b) = (secs(m.timeline_in), secs(m.timeline_out));
        let settings = sound_settings_clip(seq, m);
        let under_speech = speech_spans.iter().any(|(x, y)| *x < b && *y > a);
        if under_speech && settings.volume_keyframes.is_empty() && settings.volume > 0.26 {
            add(Severity::Warning, "music_level", Some(a), format!("{} plays at {:.0} dB under speech and never comes down", clip_label(pool, m), linear_to_db(settings.volume)), "Duck it under the speech, six to twelve decibels, with a fade");
        }
    }

    // The brief.
    match &seq.brief {
        None => add(Severity::Note, "no_brief", None, "there is no brief, so length, size, loudness and names are not checked, and colour is judged only for clipping, cast, black point and faces — not for how bright or saturated a scene should be".into(), "Set one with the piece's length, platform, loudness and cast"),
        Some(brief) => {
            if let (Some(target), Some(tol)) = (brief.target_secs, brief.length_tolerance_secs()) {
                if (duration - target).abs() > tol {
                    add(Severity::Error, "length", None, format!("the piece runs {duration:.1} s and the brief asks for {target:.0} s (within {tol:.0})"), if duration > target { "Take time out" } else { "It runs short: add to it, or change the brief" });
                }
            }
            if let (Some(w), Some(h)) = (brief.width, brief.height) {
                if (seq.resolution.width, seq.resolution.height) != (w, h) {
                    add(Severity::Warning, "size", None, format!("the sequence is {}x{} and the brief asks for {w}x{h}", seq.resolution.width, seq.resolution.height), "Export at the brief's size (the picture is fitted, not stretched), or reframe for it");
                }
            }
            if brief.captions && captions.is_empty() {
                add(Severity::Warning, "captions_missing", None, "the brief asks for captions and there are none".into(), "Add them with the captions op once the speech is where it will stay");
            }
            for member in &brief.cast {
                let named = titles.iter().any(|c| matches!(&c.kind, ClipKind::Title(td) if td.objects.iter().any(|o| matches!(&o.content, TitleObjectContent::TextBox(tb) if tb.text.trim().eq_ignore_ascii_case(member.name.trim())))));
                if !named {
                    add(Severity::Note, "cast_unnamed", None, format!("{} is in the cast and is never named on screen", member.name), "A lower third on their first appearance, if they appear");
                }
            }
            if brief.music && !has_music {
                add(Severity::Note, "music_missing", None, "the brief asks for music and none is heard".into(), "Place a music bed on an audio track of its own and duck it");
            }
            if let Some(l) = brief.loudness_lufs {
                add(Severity::Note, "loudness", None, format!("the delivery's loudness is {l:.0} LUFS"), "An export with no target of its own takes this one; the export's status reports what the file measures");
            }
            // The features the brief ranks, held to by the edit plan.
            let ranked = brief.ranked_features();
            if !ranked.is_empty() {
                match &seq.edit_plan {
                    None => add(Severity::Note, "no_edit_plan", None, format!("the brief ranks {} feature{} and there is no edit plan to hold the cut to them", ranked.len(), if ranked.len() == 1 { "" } else { "s" }), "Propose one with set_edit_plan, a segment for each"),
                    Some(plan) => {
                        for fe in ranked {
                            let total: f64 = plan.segments.iter().filter(|s| crate::edit_plan::covers_feature(s, &fe.title)).map(|s| s.duration_secs()).sum();
                            let montage = plan.segments.iter().any(|s| crate::edit_plan::is_montage(s) && crate::edit_plan::covers_feature(s, &fe.title));
                            if total <= 0.0 {
                                add(Severity::Warning, "feature_unplanned", None, format!("the brief's feature \"{}\"{} is in no segment of the edit plan", fe.title, if fe.rank > 0 { format!(" (rank {})", fe.rank) } else { String::new() }), "Give it a segment, or take it out of the brief");
                            } else if total + 1e-6 < if montage { 0.3 } else { FEATURE_MIN_SECS } {
                                add(Severity::Warning, "feature_short", None, format!("the brief's feature \"{}\" gets {total:.1} s in the edit plan", fe.title), if montage { "Hold a montage feature for at least a third of a second" } else { "Three seconds at least for a feature to register: hold its reveal" });
                            }
                        }
                    }
                }
            }
        }
    }

    // The approved edit plan against what is down.
    if let Some(plan) = seq.edit_plan.as_ref().filter(|p| p.approved && !p.segments.is_empty()) {
        let on_timeline = |s: &klipp_timeline::PlanSegment| pictures.iter().any(|(_, c)| c.media_id() == Some(s.media_id) && (secs(c.source_in) - s.source_in_secs).abs() < 1.0);
        let missing: Vec<String> = plan.segments.iter().enumerate().filter(|(_, s)| !on_timeline(s)).map(|(i, _)| (i + 1).to_string()).collect();
        if missing.len() == plan.segments.len() {
            add(Severity::Note, "plan_not_built", None, "the approved edit plan is not on the timeline".into(), "build_edit_plan with apply true builds it as one undo step");
        } else if !missing.is_empty() {
            add(Severity::Warning, "plan_drift", None, format!("segment{} {} of the approved edit plan {} not on the timeline", if missing.len() == 1 { "" } else { "s" }, missing.join(", "), if missing.len() == 1 { "is" } else { "are" }), "The cut has moved on from the plan: bring the plan up to date with set_edit_plan, or put the segments back");
        }
    }

    // Pacing: shots all of a length read as a flat cadence.
    if let Some((_, t)) = video_tracks.first() {
        let lens: Vec<f64> = t.clips.iter().filter(|c| is_picture(c)).map(|c| secs(c.timeline_out) - secs(c.timeline_in)).collect();
        if lens.len() >= 4 {
            let mean = lens.iter().sum::<f64>() / lens.len() as f64;
            let spread = lens.iter().map(|l| (l - mean).abs()).fold(0.0, f64::max) / mean.max(1e-9);
            if spread < FLAT_CADENCE {
                add(Severity::Note, "pacing_flat", None, format!("all {} shots run about {mean:.1} s: the cadence is flat", lens.len()), "Vary the lengths: hold the reveals, move through the rest");
            }
        }
    }

    f.sort_by(|a, b| a.severity.cmp(&b.severity).then(a.at_secs.unwrap_or(-1.0).partial_cmp(&b.at_secs.unwrap_or(-1.0)).unwrap_or(std::cmp::Ordering::Equal)));
    let errors = f.iter().filter(|x| x.severity == Severity::Error).count();
    let warnings = f.iter().filter(|x| x.severity == Severity::Warning).count();
    Report { duration_secs: duration, errors, warnings, findings: f }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{Fps, RationalTime, Timecode};
    use klipp_timeline::{Brief, CastMember, Track};

    fn tc(s: f64) -> Timecode {
        RationalTime::from_secs_f64(s, 25)
    }

    /// The same interview, measured silent around 5.3 and 9.3 s, where the second select is cut,
    /// and with a change of shot in the file at 12 s.
    struct Gated(klipp_core::MediaId);

    impl Transcripts for Gated {
        fn words(&self, item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
            Spoken(self.0).words(item)
        }
        fn silences(&self, item: &klipp_media::MediaItem) -> Vec<(f64, f64)> {
            if item.id == self.0 { vec![(5.2, 5.4), (9.2, 9.4)] } else { Vec::new() }
        }
        fn shot_changes(&self, item: &klipp_media::MediaItem) -> Vec<f64> {
            if item.id == self.0 { vec![12.0] } else { Vec::new() }
        }
    }

    /// An interview file that says a sentence, and a music file that says nothing.
    struct Spoken(klipp_core::MediaId);

    impl Transcripts for Spoken {
        fn words(&self, item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
            if item.id != self.0 {
                return Some(Vec::new());
            }
            Some("one two three four five six seven eight nine ten".split(' ').enumerate().map(|(k, w)| SpokenWord { start_secs: k as f64, end_secs: k as f64 + 0.6, text: w.into() }).collect())
        }
    }

    /// Two selects of one interview meeting at 4 s, their sound linked under them.
    fn piece() -> (Sequence, MediaPool, klipp_core::MediaId, klipp_core::MediaId) {
        let mut pool = MediaPool::new();
        let mut add = |path: &str, video: bool| {
            let mut item = klipp_media::MediaItem::synthetic_for_test(path.into(), None);
            item.duration = tc(60.0);
            item.has_audio = true;
            item.video_codec = if video { "hevc".into() } else { String::new() };
            pool.add(item)
        };
        let talk = add("/tmp/talk.mov", true);
        let music = add("/tmp/music.wav", false);
        let mut seq = Sequence::new_default();
        seq.fps = Fps::new(25, 1);
        let mut v = Track::new("V1", TrackKind::Video);
        let mut a = Track::new("A1", TrackKind::Audio);
        for (t0, t1, s0) in [(0.0, 4.0, 0.0), (4.0, 8.0, 4.0)] {
            let mut pic = Clip::media(talk, tc(t0), tc(t1), tc(s0), tc(s0 + t1 - t0));
            let mut snd = Clip::media(talk, tc(t0), tc(t1), tc(s0), tc(s0 + t1 - t0));
            pic.linked_clip_id = Some(snd.id);
            snd.linked_clip_id = Some(pic.id);
            v.clips.push(pic);
            a.clips.push(snd);
        }
        seq.tracks = vec![v, a];
        (seq, pool, talk, music)
    }

    fn rules(r: &Report) -> Vec<&'static str> {
        r.findings.iter().map(|f| f.rule).collect()
    }

    /// A screen recording whose screen changes at 10-12 s and 30-33 s of
    /// the file, settling at 12 and 33; still from 0-10, 12-30 and 33-60.
    struct Screen(klipp_core::MediaId);

    impl Transcripts for Screen {
        fn words(&self, _item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
            None
        }
        fn screen_beats(&self, item: &klipp_media::MediaItem) -> Option<ScreenBeats> {
            (item.id == self.0).then(|| ScreenBeats { action: vec![(10.0, 12.0), (30.0, 33.0)], settles: vec![12.0, 33.0], holds: vec![(0.0, 10.0), (12.0, 30.0), (33.0, 60.0)], spots: vec![(10.5, 0.3, 0.4), (31.0, 0.7, 0.6)] })
        }
        fn faces(&self, item: &klipp_media::MediaItem) -> Vec<(f64, [f32; 4])> {
            // A face top-centre from 40 to 44 s of the file.
            if item.id == self.0 { vec![(40.5, [0.4, 0.06, 0.2, 0.28]), (42.5, [0.4, 0.06, 0.2, 0.28])] } else { vec![] }
        }
    }

    /// A title over the face at 40-44 s of the file is an error naming a
    /// clear zone; the same title low in the frame is not.
    #[test]
    fn a_title_over_a_face_is_an_error_with_a_clear_zone() {
        // Timeline 0-10 s shows 40-50 s of the file, where the faces are.
        let (mut seq, pool, screen) = screencast(&[(0.0, 10.0, 40.0)]);
        let mut over = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Over".into(), font_size: 72.0, ..Default::default() });
        over.objects[0].transform.center = klipp_timeline::TitlePosition { x: 0.5, y: 0.15 };
        let mut low = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Low".into(), font_size: 40.0, ..Default::default() });
        low.objects[0].transform.center = klipp_timeline::TitlePosition { x: 0.5, y: 0.85 };
        let mut text = Track::new("T", TrackKind::Text);
        text.clips.push(Clip { id: klipp_core::ClipId::new(), kind: ClipKind::Title(over), source_in: tc(0.0), source_out: tc(3.0), ..Clip::gap(tc(1.0), tc(4.0)) });
        text.clips.push(Clip { id: klipp_core::ClipId::new(), kind: ClipKind::Title(low), source_in: tc(0.0), source_out: tc(3.0), ..Clip::gap(tc(5.0), tc(8.0)) });
        seq.tracks.insert(0, text);
        let r = check(&seq, &pool, &Screen(screen));
        let f: Vec<_> = r.findings.iter().filter(|f| f.rule == "title_over_face").collect();
        assert_eq!(f.len(), 1, "{}", r.text());
        assert!(f[0].hint.contains("Move it to the"), "{}", f[0].hint);
        assert!((f[0].at_secs.unwrap() - 1.0).abs() < 1e-6);
    }

    /// One screen recording cut into shots, no sound.
    fn screencast(shots: &[(f64, f64, f64)]) -> (Sequence, MediaPool, klipp_core::MediaId) {
        let mut pool = MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/screen.mp4".into(), None);
        item.duration = tc(60.0);
        item.video_codec = "h264".into();
        let screen = pool.add(item);
        let mut seq = Sequence::new_default();
        seq.fps = Fps::new(25, 1);
        let mut v = Track::new("V1", TrackKind::Video);
        for (t0, t1, s0) in shots {
            v.clips.push(Clip::media(screen, tc(*t0), tc(*t1), tc(*s0), tc(s0 + t1 - t0)));
        }
        seq.tracks = vec![v];
        (seq, pool, screen)
    }

    #[test]
    fn only_the_built_montage_relaxes_read_time_and_counts_title_features() {
        use klipp_timeline::{Brief, EditPlan, Feature, PlanSegment, PlanTitle};
        let (mut seq, pool, screen) = screencast(&[(5.0, 5.85, 0.0), (5.85, 6.7, 20.0)]);
        let segment = |source, section: &str, title: &str| PlanSegment { media_id: screen, source_in_secs: source, source_out_secs: source + 0.85, section: section.into(), title: Some(PlanTitle { text: title.into(), ..Default::default() }), feature: String::new(), note: String::new(), with_audio: false, transition: None, sfx: None, zoom: None, placement: None, background: None };
        seq.edit_plan = Some(EditPlan { segments: vec![segment(0.0, "montage", "Search"), segment(20.0, "body", "Body")], approved: true, ..Default::default() });
        seq.brief = Some(Brief { features: vec![Feature { title: "Search".into(), rank: 1, ..Default::default() }], ..Default::default() });
        let mut text = Track::new("T", TrackKind::Text);
        for (a, b, word) in [(5.0, 5.85, "Search"), (5.85, 6.7, "Body")] {
            text.clips.push(Clip { kind: ClipKind::Title(klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: word.into(), ..Default::default() })), ..Clip::gap(tc(a), tc(b)) });
        }
        seq.tracks.push(text);
        let report = check(&seq, &pool, &Screen(screen));
        for rule in ["too_short_to_read", "title_short"] {
            let findings: Vec<_> = report.findings.iter().filter(|f| f.rule == rule).collect();
            assert_eq!(findings.len(), 1, "{}", report.text());
            assert!(findings[0].at_secs.unwrap() > 5.8);
        }
        assert!(!rules(&report).contains(&"feature_unplanned"));
        assert!(!rules(&report).contains(&"feature_short"));
        seq.edit_plan.as_mut().unwrap().segments[0].source_in_secs = 10.0;
        assert_eq!(check(&seq, &pool, &Screen(screen)).findings.iter().filter(|f| f.rule == "too_short_to_read").count(), 2, "a stale plan must not suppress warnings");
    }

    #[test]
    fn a_screencast_is_held_to_its_beat_sheet() {
        // Four four-second shots: the second cuts in mid-change (11 s of the
        // file), the fourth cuts away mid-change (at 31 s).
        let (seq, pool, screen) = screencast(&[(0.0, 4.0, 0.0), (4.0, 8.0, 11.0), (8.0, 12.0, 20.0), (12.0, 16.0, 27.0)]);
        let r = check(&seq, &pool, &Screen(screen));
        let got = rules(&r);
        let mid: Vec<&Finding> = r.findings.iter().filter(|f| f.rule == "cut_mid_action").collect();
        assert_eq!(mid.len(), 2, "{}", r.text());
        assert!(mid[0].what.contains("cuts in at 11.00 s") && mid[0].hint.contains("12.00 s"), "{:?}", mid[0]);
        assert!(mid[1].what.contains("cuts away at 31.00 s") && mid[1].hint.contains("33.00 s"), "{:?}", mid[1]);
        // All of a length: flat. Every join skips ahead in the file to no
        // settle: three jump cuts.
        assert!(got.contains(&"pacing_flat"), "{}", r.text());
        assert_eq!(got.iter().filter(|r| **r == "jump_cut").count(), 3, "{}", r.text());

        // Cut to the settles instead (12 and 33 s), the third shot running on
        // from the second: nothing mid-action, and no jump cut either.
        let (seq, pool, screen) = screencast(&[(0.0, 4.0, 0.0), (4.0, 9.0, 12.0), (9.0, 12.0, 17.0), (12.0, 13.5, 33.0)]);
        let r = check(&seq, &pool, &Screen(screen));
        let got = rules(&r);
        assert!(!got.contains(&"cut_mid_action") && !got.contains(&"jump_cut") && !got.contains(&"pacing_flat"), "{}", r.text());
        // The last shot is a second and a half of screen: too short to read.
        let short = r.findings.iter().find(|f| f.rule == "too_short_to_read").expect("too short");
        assert_eq!(short.at_secs, Some(12.0));
    }

    #[test]
    fn a_secret_on_screen_is_an_error_where_the_cut_shows_it() {
        struct Leaky(klipp_core::MediaId);
        impl Transcripts for Leaky {
            fn words(&self, _item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
                None
            }
            fn screen_sensitive(&self, item: &klipp_media::MediaItem) -> Vec<SensitiveText> {
                if item.id != self.0 { return Vec::new(); }
                vec![SensitiveText { kind: "key".into(), masked: "sk-…9z".into(), start_secs: 10.0, end_secs: 14.0, x: 0.2, y: 0.6, w: 0.3, h: 0.03 }]
            }
        }
        // The first shot uses 0-4 s of the file: clean. The second uses 12-17 s: the key is up.
        let (seq, pool, screen) = screencast(&[(0.0, 4.0, 0.0), (4.0, 9.0, 12.0)]);
        let r = check(&seq, &pool, &Leaky(screen));
        let f = r.findings.iter().find(|f| f.rule == "sensitive_on_screen").unwrap_or_else(|| panic!("{}", r.text()));
        assert_eq!((f.severity, f.at_secs), (Severity::Error, Some(4.0)));
        assert!(f.what.contains("key (sk-…9z) from 4.00 to 6.00 s") && f.what.contains("20% across"), "{}", f.what);
        let (seq, pool, screen) = screencast(&[(0.0, 4.0, 0.0), (4.0, 9.0, 20.0)]);
        assert!(!rules(&check(&seq, &pool, &Leaky(screen))).contains(&"sensitive_on_screen"));
    }

    #[test]
    fn a_title_over_a_changing_screen_is_sent_to_a_hold() {
        let (mut seq, pool, screen) = screencast(&[(0.0, 8.0, 6.0)]);
        // The screen changes at 10-12 s of the file: 4-6 s of the timeline.
        let mut text = Track::new("T1", TrackKind::Text);
        let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Search".into(), ..Default::default() });
        text.clips.push(Clip { id: klipp_core::ClipId::new(), kind: ClipKind::Title(td), source_in: tc(0.0), source_out: tc(4.0), ..Clip::gap(tc(3.0), tc(7.0)) });
        seq.tracks.insert(0, text);
        let r = check(&seq, &pool, &Screen(screen));
        let busy = r.findings.iter().find(|f| f.rule == "title_over_busy").unwrap_or_else(|| panic!("{}", r.text()));
        assert_eq!(busy.at_secs, Some(4.0));
        assert!(busy.hint.contains("0.0-10.0 s") && busy.hint.contains("12.0-30.0 s"), "{}", busy.hint);
        // Inside a hold, nothing to say.
        seq.tracks[0].clips[0].timeline_in = tc(0.0);
        seq.tracks[0].clips[0].timeline_out = tc(3.5);
        assert!(!rules(&check(&seq, &pool, &Screen(screen))).contains(&"title_over_busy"));
    }

    #[test]
    fn the_brief_s_features_are_held_to_by_the_edit_plan() {
        use klipp_timeline::{EditPlan, Feature, PlanSegment};
        let (mut seq, pool, screen) = screencast(&[(0.0, 4.0, 0.0), (4.0, 9.0, 12.0)]);
        seq.brief = Some(Brief { features: vec![Feature { title: "Timeline".into(), rank: 1, ..Default::default() }, Feature { title: "Grade".into(), rank: 2, ..Default::default() }], ..Default::default() });
        let got = rules(&check(&seq, &pool, &Screen(screen)));
        assert!(got.contains(&"no_edit_plan"), "{got:?}");
        let seg = |feature: &str, a: f64, b: f64| PlanSegment { feature: feature.into(), media_id: screen, source_in_secs: a, source_out_secs: b, note: String::new(), with_audio: false, title: None, transition: None, sfx: None, zoom: None, placement: None, background: None, section: String::new() };
        // Timeline gets a second, Grade nothing; the plan is approved and not built.
        seq.edit_plan = Some(EditPlan { segments: vec![seg("Timeline", 40.0, 41.0)], approved: true, ..Default::default() });
        let r = check(&seq, &pool, &Screen(screen));
        let got = rules(&r);
        for rule in ["feature_short", "feature_unplanned", "plan_not_built"] {
            assert!(got.contains(&rule), "{rule} missing from {}", r.text());
        }
        // Built in part: one segment is down, one is not.
        seq.edit_plan = Some(EditPlan { segments: vec![seg("Timeline", 0.0, 4.0), seg("Grade", 12.0, 17.0), seg("", 50.0, 53.0)], approved: true, ..Default::default() });
        let r = check(&seq, &pool, &Screen(screen));
        let drift = r.findings.iter().find(|f| f.rule == "plan_drift").unwrap_or_else(|| panic!("{}", r.text()));
        assert!(drift.what.contains("segment 3 "), "{}", drift.what);
        assert!(!rules(&r).contains(&"feature_unplanned") && !rules(&r).contains(&"feature_short"));
        // Unapproved, the plan is a proposal and the timeline is not held to it.
        seq.edit_plan.as_mut().unwrap().approved = false;
        assert!(!rules(&check(&seq, &pool, &Screen(screen))).contains(&"plan_drift"));
    }

    #[test]
    fn a_clean_piece_has_nothing_to_say_but_that_it_has_no_brief() {
        let (seq, pool, talk, _) = piece();
        // The two selects run on from each other in the file: a cut, not a jump. The
        // words sit inside the seconds, so no edge is inside one.
        let r = check(&seq, &pool, &Spoken(talk));
        assert_eq!(rules(&r), vec!["no_brief"], "{}", r.text());
        assert_eq!((r.errors, r.warnings), (0, 0));
        assert!(r.text().starts_with("Checked 8.00 s: 0 errors, 0 warnings, 1 note."), "{}", r.text());
    }

    #[test]
    fn what_an_editor_would_catch_is_caught() {
        let (mut seq, mut pool, talk, music) = piece();
        let _ = &mut pool;
        // The second select comes from later in the file and starts a second late: a
        // jump cut after a second of black, its sound starting inside the word "six".
        for t in seq.tracks.iter_mut() {
            let c = &mut t.clips[1];
            (c.timeline_in, c.timeline_out, c.source_in, c.source_out) = (tc(5.0), tc(9.0), tc(5.3), tc(9.3));
        }
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(rules(&r).contains(&"black") && r.errors == 1, "{}", r.text());
        let word = r.findings.iter().find(|f| f.rule == "word_cut").expect("a cut inside a word");
        assert!(word.what.contains("\"six\"") && word.hint.contains("5.00 s"), "{word:?}");
        // Measured silent at that edge, the cut is clean: the words' times are a
        // model's estimate, and whisper lets a word run across a pause.
        assert!(!rules(&check(&seq, &pool, &Gated(talk))).contains(&"word_cut"));

        // Closed up, it is a jump cut; B-roll across the join covers it.
        for t in seq.tracks.iter_mut() {
            let c = &mut t.clips[1];
            (c.timeline_in, c.timeline_out, c.source_in, c.source_out) = (tc(4.0), tc(8.0), tc(20.0), tc(24.0));
        }
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(rules(&r).contains(&"jump_cut") && !rules(&r).contains(&"black"), "{}", r.text());
        // The file changes shot at 12 s, between the two selects' source times
        // (4 s and 20 s): that is a cut in the picture, not a jump.
        assert!(!rules(&check(&seq, &pool, &Gated(talk))).contains(&"jump_cut"));
        let mut broll = Track::new("V2", TrackKind::Video);
        let mut cover = Clip::media(music, tc(3.0), tc(5.0), tc(0.0), tc(2.0));
        cover.volume = 0.0;
        broll.clips = vec![cover];
        seq.tracks.insert(0, broll);
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(!rules(&r).contains(&"jump_cut"), "{}", r.text());

        // A five-frame shot is a flash.
        seq.tracks[0].clips.push(Clip::media(music, tc(6.0), tc(6.2), tc(0.0), tc(0.2)));
        seq.tracks[0].clips[1].volume = 0.0;
        assert!(rules(&check(&seq, &pool, &Spoken(talk))).contains(&"flash"));
        seq.tracks[0].clips.pop();

        // Music under the speech at full level is said; ducked, it is not.
        let mut bed = Track::new("A2", TrackKind::Audio);
        bed.clips = vec![Clip::media(music, tc(0.0), tc(8.0), tc(0.0), tc(8.0))];
        seq.tracks.push(bed);
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(rules(&r).contains(&"music_level"), "{}", r.text());
        klipp_core::keyframe::upsert_keyframe(&mut seq.tracks.last_mut().unwrap().clips[0].volume_keyframes, 0.0, 0.3);
        assert!(!rules(&check(&seq, &pool, &Spoken(talk))).contains(&"music_level"));
    }

    #[test]
    fn the_brief_is_what_the_piece_is_held_to() {
        let (mut seq, pool, talk, _) = piece();
        seq.brief = Some(Brief {
            target_secs: Some(60.0),
            width: Some(1080),
            height: Some(1920),
            loudness_lufs: Some(-14.0),
            captions: true,
            music: true,
            cast: vec![CastMember { name: "Ada".into(), role: "Engineer".into(), described: String::new() }],
            ..Default::default()
        });
        let r = check(&seq, &pool, &Spoken(talk));
        let got = rules(&r);
        for rule in ["length", "size", "captions_missing", "cast_unnamed", "music_missing", "loudness"] {
            assert!(got.contains(&rule), "{rule} missing from {}", r.text());
        }
        assert!(!got.contains(&"no_brief"));
        assert_eq!(r.findings[0].rule, "length", "errors come first: {}", r.text());
        // As JSON for a caller that wants to filter.
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["findings"][0]["severity"], "error");
    }
}
