//! The check before a piece goes out: what an editor would catch on a
//! last watch, read off the timeline and the brief. Nothing here
//! changes anything; every finding says where, what, and what would
//! put it right.

use klipp_media::MediaPool;
use klipp_timeline::{Clip, ClipKind, Sequence, TitleObjectContent, TrackKind};
use serde::Serialize;

use crate::ops::{is_heard, linear_to_db, not_speech, sound_role, sound_settings_clip, spoken_spans, ScreenBeats, SensitiveText, SpokenWord, Transcripts};

/// A cut this close to a settle point is a cut to the settle.
const SETTLE_NEAR_SECS: f64 = 0.6;
/// A screen needs about this long to be read, until OCR can say how much is on it.
const READ_MIN_SECS: f64 = 2.0;
/// A feature of the brief needs at least this long on screen to register.
const FEATURE_MIN_SECS: f64 = 3.0;
/// An edge this close to one of the music's beats is on it, at the
/// least: a frame is the finer bound (#1908 review). A wider window
/// against beats half a second apart catches half the timeline.
const BEAT_NEAR_SECS: f64 = 0.04;
/// How far either side of a title the picture cuts around it are looked
/// for, to see the stretch is cut to the music too.
const BEAT_CONTEXT_SECS: f64 = 4.0;
/// Under this the beat analysis is not sure there is a pulse to cut to.
const BEAT_SURE: f32 = 0.3;
/// A detected body shorter than this share of the frame is too small for
/// a title to hide (#1590).
const PERSON_MIN_HEIGHT: f32 = 0.1;
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

/// How many new pictures a second a clip of a `media_fps` file at `speed`
/// shows on a `timeline_fps` timeline, when a picture is held about every
/// ten frames or more often: a judder (#2479). A file a frame a second
/// short of the timeline's rate, 24 on 25, is the usual conform and not
/// said. `None` when it is smooth, or a rate is unknown.
pub fn judders(media_fps: f64, speed: f64, timeline_fps: f64) -> Option<f64> {
    if !(media_fps > 0.0 && timeline_fps > 0.0 && speed.abs() > 1e-6) {
        return None;
    }
    let new = media_fps * speed.abs();
    (new < timeline_fps * 0.9).then_some(new)
}

fn secs(t: klipp_core::Timecode) -> f64 {
    t.to_secs_f64()
}

// Graphics share the title container, but only visible nonblank text is read.
fn visible_title_text(td: &klipp_timeline::TitleData) -> impl Iterator<Item = &str> {
    td.objects.iter().filter_map(|o| match &o.content {
        TitleObjectContent::TextBox(tb) if o.visible && !tb.text.trim().is_empty() => Some(tb.text.trim()),
        _ => None,
    })
}

fn clip_label(pool: &MediaPool, c: &Clip) -> String {
    if let Some(name) = c.name.as_deref().filter(|n| !n.trim().is_empty()) {
        return format!("\"{name}\"");
    }
    match &c.kind {
        ClipKind::Media { media_id } => pool.get(media_id).map(|m| m.filename.clone()).unwrap_or_else(|| "a missing file".into()),
        ClipKind::Title(td) => visible_title_text(td).next().map(|text| format!("title \"{}\"", text.lines().next().unwrap_or(""))).unwrap_or_else(|| "graphics".into()),
        ClipKind::Generator(_) => "a generator".into(),
        _ => "a clip".into(),
    }
}

/// Whether a clip puts a picture on screen.
fn is_picture(c: &Clip) -> bool {
    c.enabled && !matches!(c.kind, ClipKind::Gap | ClipKind::AdjustmentLayer)
}

/// Whether a clip on a text track fills the frame: a title on a full-frame
/// card, like an end card, or a generator. Text over nothing does not.
fn fills_frame(c: &Clip) -> bool {
    c.enabled
        && match &c.kind {
            ClipKind::Title(td) => matches!(td.background, klipp_timeline::TitleBackground::FullFrame { color } if color[3] > 0),
            ClipKind::Generator(_) => true,
            _ => false,
        }
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

/// A strobe as the strobe op leaves it (#2357): three or more pieces of
/// one shot, each shorter than a flash (8 frames), in the shot's order,
/// with a gap under a flash between them, black on purpose, or none when
/// rippled. A longer piece of the same shot at either end belongs to it
/// when the gap to it is no wider than the run's own: the clip the strobe
/// left whole outside its stretch. A gap of another width, or one before
/// or after it, is not the strobe's.
struct Strobe {
    /// What it covers, with the pieces of the shot either side.
    covers: (f64, f64),
    /// From its first piece to its last.
    pieces_span: (f64, f64),
    pieces: usize,
    piece_frames: i64,
}

fn strobe_spans(seq: &Sequence) -> Vec<Strobe> {
    let fps = seq.fps.as_f64().max(1.0);
    let tolerance = 0.5 / fps;
    let frames = |c: &Clip| ((secs(c.timeline_out) - secs(c.timeline_in)) * fps).round() as i64;
    let mut spans = Vec::new();
    for track in seq.tracks.iter().filter(|t| t.visible && t.kind == TrackKind::Video) {
        let mut clips: Vec<&Clip> = track.clips.iter().filter(|c| is_picture(c)).collect();
        clips.sort_by_key(|c| c.timeline_in);
        let gap = |k: usize| secs(clips[k + 1].timeline_in) - secs(clips[k].timeline_out);
        // Clip k and the next: one shot, in its order, a gap under a flash.
        let linked = |k: usize| {
            let (a, b) = (clips[k], clips[k + 1]);
            a.media_id().is_some() && a.media_id() == b.media_id() && (-tolerance..8.0 / fps).contains(&gap(k)) && secs(b.source_in) >= secs(a.source_out) - tolerance
        };
        let mut k = 0;
        while k < clips.len() {
            let mut end = k;
            while end + 1 < clips.len() && frames(clips[end]) < 8 && frames(clips[end + 1]) < 8 && linked(end) {
                end += 1;
            }
            if end - k + 1 >= 3 && frames(clips[k]) < 8 {
                let widest = (k..end).map(gap).fold(0.0, f64::max) + tolerance;
                let first = if k > 0 && linked(k - 1) && gap(k - 1) <= widest { k - 1 } else { k };
                let last = if end + 1 < clips.len() && linked(end) && gap(end) <= widest { end + 1 } else { end };
                let piece = (k..=end).map(|i| frames(clips[i])).min().unwrap_or(1);
                spans.push(Strobe {
                    covers: (secs(clips[first].timeline_in), secs(clips[last].timeline_out)),
                    pieces_span: (secs(clips[k].timeline_in), secs(clips[end].timeline_out)),
                    pieces: end - k + 1,
                    piece_frames: piece,
                });
                k = end + 1;
            } else {
                k += 1;
            }
        }
    }
    spans
}

/// How much of a title's drawn box must lie over a changing screen for
/// the title to be over it: a twentieth, so a graze of its glow along
/// the screen's edge is not, and a caption with a third of it on the
/// screen is (#1909 review).
const TITLE_OVER_SHARE: f32 = 0.05;
/// A margin round a title's drawn box for its anti-aliased edges and
/// the renderer's rounding, as a fraction of the frame.
const TITLE_EDGE_MARGIN: f32 = 0.01;

/// `rect` of a clip's own picture (fractions of a picture of size
/// `shown`) where it lands on the frame at timeline second `t`: fitted as
/// the renderer fits it and moved by the clip's placement then, as the
/// box round the quad.
fn placed_box(seq: &Sequence, transcripts: &dyn Transcripts, c: &Clip, shown: [u32; 2], rect: [f32; 4], t: f64) -> [f32; 4] {
    let frame = [seq.resolution.width, seq.resolution.height];
    let [x, y, w, h] = rect;
    let quad = [[x, y], [x + w, y], [x + w, y + h], [x, y + h]];
    let placed = transcripts.placement(c, t - secs(c.timeline_in), seq.fps.as_f64());
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for [qx, qy] in klipp_timeline::surface::source_quad_in_frame(&quad, shown, frame, placed) {
        (x0, y0, x1, y1) = (x0.min(qx), y0.min(qy), x1.max(qx), y1.max(qy));
    }
    [x0, y0, x1 - x0, y1 - y0]
}

/// The box a title draws in, as fractions of the frame: its text and
/// shapes ([`crate::ops::title_box`]) grown by the widest outline, glow
/// and shadow (offset and blur) its layers carry, its backing's padding,
/// and an edge margin. `None` for a title with nothing visible.
fn drawn_box(td: &klipp_timeline::TitleData, frame: klipp_core::Resolution) -> Option<[f32; 4]> {
    let tb = crate::ops::title_box(td, frame)?;
    let mut reach: f32 = 0.0;
    for o in td.objects.iter().filter(|o| o.visible) {
        let fx = o.fx();
        let outline = fx.outlines.iter().filter(|x| x.enabled).map(|x| x.width).fold(0.0, f32::max);
        let glow = fx.glows.iter().filter(|x| x.enabled).map(|x| x.radius).fold(0.0, f32::max);
        let shadow = fx.shadows.iter().filter(|x| x.enabled).map(|x| x.offset_x.abs().max(x.offset_y.abs()) + x.blur).fold(0.0, f32::max);
        reach = reach.max(outline).max(glow).max(shadow);
    }
    if let klipp_timeline::TitleBackground::Color { padding, .. } = td.background {
        reach = reach.max(padding);
    }
    // Pixels at 1080 on the frame's short side, as the renderer scales a
    // title's sizes.
    let (w, h) = (frame.width.max(1) as f32, frame.height.max(1) as f32);
    let px = reach * w.min(h) / 1080.0;
    let (mx, my) = (px / w + TITLE_EDGE_MARGIN, px / h + TITLE_EDGE_MARGIN);
    Some([tb[0] - mx, tb[1] - my, tb[2] + 2.0 * mx, tb[3] + 2.0 * my])
}

/// Whether a title's layers move on their own (an entrance or exit, a
/// loop): where they are at a moment is not their resting box.
fn title_moves(td: &klipp_timeline::TitleData) -> bool {
    td.objects.iter().filter(|o| o.visible).any(|o| o.fx().motion.is_active() || !o.fx().loops.is_empty())
}

/// The share of box `a` that lies inside box `b`.
fn share_over(a: [f32; 4], b: [f32; 4]) -> f32 {
    let w = ((a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0])).max(0.0);
    let h = ((a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1])).max(0.0);
    w * h / (a[2] * a[3]).max(1e-6)
}

/// Whether title clip `title` is over picture clip `screen` anywhere in
/// `from..to`: both placed as the renderer places them, sampled across
/// the stretch, the title's drawn box against the screen's (#1909). A
/// title whose layers move on their own is taken to be over it.
#[allow(clippy::too_many_arguments)]
fn title_over_screen(seq: &Sequence, pool: &MediaPool, transcripts: &dyn Transcripts, title: &Clip, td: &klipp_timeline::TitleData, screen: &Clip, from: f64, to: f64) -> bool {
    let Some(drawn) = drawn_box(td, seq.resolution) else { return false };
    if title_moves(td) {
        return true;
    }
    let frame = [seq.resolution.width, seq.resolution.height];
    let shown = screen.media_id().and_then(|id| pool.get(&id)).map_or(frame, |m| {
        let (w, h) = (m.resolution.width, m.resolution.height);
        if m.source_rotation.rem_euclid(180) == 90 { [h, w] } else { [w, h] }
    });
    let steps = ((to - from) / 0.25).ceil().clamp(1.0, 16.0) as usize;
    (0..=steps).any(|k| {
        let t = from + (to - from) * k as f64 / steps as f64;
        let words = placed_box(seq, transcripts, title, frame, drawn, t);
        let picture = placed_box(seq, transcripts, screen, shown, [0.0, 0.0, 1.0, 1.0], t);
        share_over(words, picture) > TITLE_OVER_SHARE
    })
}

/// A skip within one take shorter than this is a jump whatever the
/// camera did: the travel films' real jump cuts skipped 0.6 to 1.7 s, the
/// montage's cuts between places 4 s and more (#461).
const JUMP_PLACE_SECS: f64 = 2.0;
/// The motion analyser's mean YDIF from which a camera is moving.
const MOVING_YDIF: f32 = 4.0;

/// Add to `report` that the project's global LUT is missing, when it
/// names one that is not on disk: the timeline renders without it until
/// it is relinked (#1019).
pub fn note_missing_global_lut(report: &mut Report, lut: Option<&std::path::Path>) {
    let Some(lut) = lut.filter(|p| !p.exists()) else { return };
    report.findings.insert(0, Finding {
        severity: Severity::Error,
        rule: "missing_lut",
        at_secs: None,
        what: format!("the project's LUT {} is missing", lut.display()),
        hint: "Relink it in Project Doctor, or clear it, before delivering".into(),
    });
    report.errors += 1;
}

/// Below this contrast between a title's words and the picture under
/// them (the ratio of their relative luminances, the WCAG measure), most
/// of the words are hard to read: 3:1 is the floor for large text.
const LEGIBLE_CONTRAST: f32 = 3.0;

/// A title read against the picture under it (#1581): its words' fill and
/// box, the moment it is read at (its middle) and the picture clip under
/// it then, which the caller renders alone.
#[derive(Debug, Clone, PartialEq)]
pub struct TitleToRead {
    pub label: String,
    pub at_secs: f64,
    pub under: klipp_core::ClipId,
    /// The words' box as fractions of the frame: x, y, width, height.
    pub rect: [f32; 4],
    pub fill: [u8; 4],
    /// A shadow or an outline behind the words, which a contrast of the
    /// fill alone doesn't see.
    pub helped: bool,
}

/// The titles whose words are read straight off the picture, each with
/// the picture under it: on a visible text track, over a picture clip,
/// with no backing and no card behind them. At most `limit`, in time
/// order.
pub fn titles_to_read(seq: &Sequence, pool: &MediaPool, limit: usize) -> Vec<TitleToRead> {
    let pictures: Vec<&Clip> = seq.tracks.iter().filter(|t| t.kind == TrackKind::Video && t.visible).flat_map(|t| t.clips.iter()).filter(|c| is_picture(c)).collect();
    let mut out = Vec::new();
    for t in seq.tracks.iter().filter(|t| t.kind == TrackKind::Text && t.visible) {
        for c in t.clips.iter().filter(|c| c.enabled) {
            let ClipKind::Title(td) = &c.kind else { continue };
            if !matches!(td.background, klipp_timeline::TitleBackground::None) {
                continue;
            }
            let Some(tb) = td.primary_text() else { continue };
            let Some(rect) = crate::ops::title_box(td, seq.resolution) else { continue };
            let at = (secs(c.timeline_in) + secs(c.timeline_out)) / 2.0;
            // The top picture then: the first track in the list is the top.
            let Some(under) = pictures.iter().find(|p| secs(p.timeline_in) <= at && at < secs(p.timeline_out)) else { continue };
            let helped = td.objects.iter().filter(|o| o.visible).map(|o| o.fx()).any(|fx| fx.shadows.iter().any(|s| s.enabled && s.color[3] >= 96) || fx.outlines.iter().any(|o| o.enabled));
            out.push(TitleToRead { label: clip_label(pool, c), at_secs: at, under: under.id, rect, fill: tb.color, helped });
        }
    }
    out.sort_by(|a, b| a.at_secs.total_cmp(&b.at_secs));
    out.truncate(limit);
    out
}

/// sRGB8 to relative luminance (0 black, 1 white).
fn luminance(rgb: [u8; 3]) -> f32 {
    let lin = |v: u8| {
        let c = v as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(rgb[0]) + 0.7152 * lin(rgb[1]) + 0.0722 * lin(rgb[2])
}

/// The contrast of two relative luminances, 1 (none) to 21.
fn contrast(a: f32, b: f32) -> f32 {
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// `title` against `picture` (the clip under it alone, RGBA8 at
/// `width × height`): a finding when the words' fill has too little
/// contrast with the picture across the middle of their box. A warning,
/// or a note when a shadow or an outline may carry the words.
pub fn legibility(title: &TitleToRead, picture: &[u8], width: usize, height: usize) -> Option<Finding> {
    let [x, y, w, h] = title.rect;
    let px = |f: f32, n: usize| ((f.clamp(0.0, 1.0) * n as f32) as usize).min(n);
    let (x0, x1, y0, y1) = (px(x, width), px(x + w, width), px(y, height), px(y + h, height));
    let fill = luminance([title.fill[0], title.fill[1], title.fill[2]]);
    let mut ratios: Vec<f32> = Vec::new();
    for row in y0..y1 {
        for col in x0..x1 {
            let i = (row * width + col) * 4;
            let Some(p) = picture.get(i..i + 3) else { continue };
            ratios.push(contrast(fill, luminance([p[0], p[1], p[2]])));
        }
    }
    if ratios.is_empty() {
        return None;
    }
    ratios.sort_by(f32::total_cmp);
    let median = ratios[ratios.len() / 2];
    if median >= LEGIBLE_CONTRAST {
        return None;
    }
    let [r, g, b, _] = title.fill;
    let darker = fill > 0.18;
    Some(Finding {
        severity: if title.helped { Severity::Note } else { Severity::Warning },
        rule: "title_hard_to_read",
        at_secs: Some(title.at_secs),
        what: format!(
            "{}'s words (#{r:02x}{g:02x}{b:02x}) have a contrast of {median:.1}:1 with the picture under them at {:.2} s{}",
            title.label,
            title.at_secs,
            if title.helped { ", which its shadow or outline may carry" } else { "" },
        ),
        hint: format!(
            "Aim for {LEGIBLE_CONTRAST:.0}:1 or more: give it a shadow or a backing (the style op's shadow or backing), make the words {}, or move it over a {} part of the picture",
            if darker { "darker" } else { "lighter" },
            if darker { "darker" } else { "lighter" },
        ),
    })
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
    // A strobe's gaps, pieces and cuts are its effect: said once (#2357).
    let strobes = strobe_spans(seq);
    let in_strobe = |a: f64, b: f64| strobes.iter().any(|s| a + 1e-6 >= s.covers.0 && b <= s.covers.1 + 1e-6);
    for s in &strobes {
        let ((a, b), n) = (s.pieces_span, s.piece_frames);
        add(Severity::Note, "strobe", Some(a), format!("a strobe from {a:.2} to {b:.2} s: {} pieces of {n} frame{}", s.pieces, if n == 1 { "" } else { "s" }), "Its gaps and flashes are its own; nothing to do");
    }
    let video_tracks: Vec<(usize, &klipp_timeline::Track)> = seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Video && t.visible).collect();
    let pictures: Vec<(usize, &Clip)> = video_tracks.iter().flat_map(|(ti, t)| t.clips.iter().filter(|c| is_picture(c)).map(move |c| (*ti, c))).collect();
    // Where there is something on screen: the pictures, and the cards on
    // the text tracks that fill the frame, so an end card is not "black"
    // and the picture does not end before it (#517).
    let on_screen: Vec<(f64, f64)> = pictures
        .iter()
        .map(|(_, c)| *c)
        .chain(seq.tracks.iter().filter(|t| t.kind == TrackKind::Text && t.visible).flat_map(|t| t.clips.iter()).filter(|c| fills_frame(c)))
        .map(|c| (secs(c.timeline_in), secs(c.timeline_out)))
        .collect();

    if on_screen.is_empty() {
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

    // A clip that shows fewer new pictures a second than the timeline
    // plays, each held for more than a frame: a slowed or lower-rate clip
    // judders unless its in-between frames are blended (#2479).
    for (_, c) in pictures.iter().filter(|(_, c)| c.enabled && c.speed_keyframes.is_empty() && !c.has_frame_blend_entry()) {
        let Some(m) = c.media_id().and_then(|id| pool.get(&id)).filter(|m| !m.is_still && !m.video_codec.is_empty()) else { continue };
        if let Some(new) = judders(m.fps.as_f64(), c.speed, seq.fps.as_f64()) {
            add(Severity::Note, "judder", Some(secs(c.timeline_in)), format!("{} shows {new:.1} new pictures a second on a {:.3} fps timeline ({:.3} fps at {:.2}x), each held for more than a frame", clip_label(pool, c), seq.fps.as_f64(), m.fps.as_f64(), c.speed.abs()), "Retime it with interpolation blend, or flow to follow the motion, if the steps show");
        }
    }

    // A picture clip trimmed past the end of its file (#978): from an in
    // point at or past the end it shows nothing, in the viewer and the
    // export alike, which now fails on it; running on past the end, it
    // holds the file's last frame. A still image is one frame for any
    // length, so it is neither.
    for (_, c) in pictures.iter().filter(|(_, c)| c.enabled) {
        let Some(m) = c.media_id().and_then(|id| pool.get(&id)) else { continue };
        let file_end = secs(m.duration);
        if m.is_still || m.offline || file_end <= 0.0 {
            continue;
        }
        let (src_in, src_out) = (secs(c.source_in), secs(c.source_out));
        if src_in >= file_end - frame * 0.5 {
            add(
                Severity::Error,
                "past_end",
                Some(secs(c.timeline_in)),
                format!("{} starts at {src_in:.2} s of a {file_end:.2} s file: it has no picture", m.filename),
                "Slip the clip back inside the file, or remove it",
            );
        } else if src_out > file_end + frame * 1.5 {
            let held_from = secs(c.timeline_in) + (file_end - src_in) / c.speed.max(0.001);
            add(
                Severity::Warning,
                "held_end",
                Some(held_from),
                format!("{} runs {:.2} s past the end of its file: its last frame is held from {held_from:.2} s", m.filename, src_out - file_end),
                "Trim the clip to the file's end, or cover the hold",
            );
        }
    }

    // A second Transform effect on a clip does nothing (#995): the clip's
    // transform is applied once, at the first, and the render drops the
    // rest, which used to move, scale and turn the clip again.
    for (_, c) in pictures.iter().filter(|(_, c)| c.enabled) {
        let on = |i: usize| c.builtin_effects_enabled().get(i).copied().unwrap_or(true);
        let transforms = c.builtin_effects().iter().enumerate().filter(|(i, e)| on(*i) && matches!(e, klipp_effects::BuiltinEffect::Transform { .. })).count();
        if transforms > 1 {
            add(
                Severity::Warning,
                "extra_transform",
                Some(secs(c.timeline_in)),
                format!("{} has {transforms} Transform effects: only the first places it, the rest do nothing", clip_label(pool, c)),
                "Remove the extra Transform effects, or move what they did into the first",
            );
        }
    }

    // Clips laid over each other on one track: only one of them shows
    // there, in the viewer and the export alike, and the other is hidden
    // with its grade (#639). Nothing makes this on purpose; a retime that
    // grew a clip into the next one did.
    for (ti, t) in seq.tracks.iter().enumerate() {
        let mut clips: Vec<&Clip> = t.clips.iter().filter(|c| c.enabled && !matches!(c.kind, ClipKind::Gap)).collect();
        clips.sort_by_key(|c| c.timeline_in);
        for pair in clips.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (from, to) = (secs(b.timeline_in), secs(a.timeline_out.min(b.timeline_out)));
            if to - from > frame * 0.5 {
                add(
                    Severity::Error,
                    "overlap",
                    Some(from),
                    format!("{} and {} overlap on track {ti} from {from:.2} to {to:.2} s: one hides the other there", clip_label(pool, a), clip_label(pool, b)),
                    "Trim or move one of them, or ripple the clips after the first",
                );
            }
        }
    }

    // Black: time with no picture under it.
    if !on_screen.is_empty() {
        let picture_end = on_screen.iter().map(|(_, b)| *b).fold(0.0, f64::max);
        for (a, b) in uncovered(on_screen.clone(), picture_end, frame * 0.9).into_iter().filter(|(a, b)| !in_strobe(*a, *b)) {
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
        // What runs on fades out at its own end, as the hint asks: the
        // sound by its fade, a picture by its opacity keyed down to
        // nothing (#1572). The fade covers the run-on's last second, or
        // half of it when it runs on under two: a blink of a fade still ends the piece
        // on its run-on at full level (#1598).
        let covers = |run_on: f64| run_on.min(2.0) * 0.5;
        let sound_fades = || {
            seq.tracks
                .iter()
                .flat_map(|t| t.clips.iter().map(move |c| (t, c)))
                .filter(|(t, c)| is_heard(seq, pool, t, c) && secs(c.timeline_out) > picture_end + frame * 1.5)
                .all(|(_, c)| f64::from(sound_settings_clip(seq, c).fade_out_secs) >= covers(secs(c.timeline_out) - picture_end) - 1e-6)
        };
        let picture_fades = || {
            pictures
                .iter()
                .map(|(_, c)| *c)
                .chain(seq.tracks.iter().filter(|t| t.kind == TrackKind::Text && t.visible).flat_map(|t| t.clips.iter()).filter(|c| fills_frame(c)))
                .filter(|c| secs(c.timeline_out) > sound_end + frame * 1.5)
                .all(|c| {
                    let end = secs(c.timeline_out) - secs(c.timeline_in) - frame * 0.5;
                    // Down to nothing at the end, and already on its way
                    // down `covers` before it.
                    let begun = end - covers(secs(c.timeline_out) - sound_end) * 0.9;
                    c.truth_lane(&klipp_core::ParamId::Opacity)
                        .map(|(keys, _)| keys)
                        .filter(|keys| !keys.is_empty())
                        .is_some_and(|keys| {
                            let at = |t: f64| klipp_core::keyframe::evaluate(&keys, t, 1.0);
                            at(end) < 0.05 && at(begun) < 0.95 * at(0.0).max(0.05)
                        })
                })
        };
        let runs_on_fades = if sound_end > picture_end { sound_fades() } else { picture_fades() };
        if sound_end > 0.0 && (sound_end - picture_end).abs() > frame * 1.5 && !runs_on_fades {
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
            if frames < 8 && !in_strobe(secs(c.timeline_in), secs(c.timeline_out)) {
                add(Severity::Warning, "flash", Some(secs(c.timeline_in)), format!("{} is on screen for {frames} frame{}", clip_label(pool, c), if frames == 1 { "" } else { "s" }), "A shot that short reads as a flash: lengthen it or take it out");
            }
        }
        for pair in clips.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let cut = secs(b.timeline_in);
            let same_media = matches!((&a.kind, &b.kind), (ClipKind::Media { media_id: x }, ClipKind::Media { media_id: y }) if x == y);
            let meets = (secs(a.timeline_out) - cut).abs() < frame * 0.5;
            let jumps = (secs(b.source_in) - secs(a.source_out)).abs() > 0.2;
            if !(same_media && meets && jumps) || in_strobe(secs(a.timeline_in), secs(b.timeline_out)) {
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
            // A camera that kept moving over the stretch skipped has gone
            // somewhere else: in a walk cut to a montage, a cut between
            // places in one take, not a jump cut (#461). A short skip, or
            // a camera standing still (an interview), is still a jump.
            let skipped = (secs(b.source_in) - secs(a.source_out)).abs();
            let moved_on = skipped >= JUMP_PLACE_SECS
                && a.media_id().and_then(|id| pool.get(&id)).and_then(|item| {
                    let (lo, hi) = (secs(a.source_out).min(secs(b.source_in)), secs(a.source_out).max(secs(b.source_in)));
                    transcripts.motion_between(item, lo, hi)
                }).is_some_and(|m| m >= MOVING_YDIF);
            if source_cut_between || to_settle || moved_on {
                continue;
            }
            // Covered when a picture on a track above runs across the cut,
            // or a title does: a card hides the join, a heading carries the
            // eye across it.
            let title_over = seq.tracks.iter().filter(|t| t.kind == TrackKind::Text && t.visible).flat_map(|t| t.clips.iter()).any(|c| c.enabled && matches!(c.kind, ClipKind::Title(_)) && secs(c.timeline_in) <= cut - 0.2 && secs(c.timeline_out) >= cut + 0.2);
            // A transition on the cut carries the eye across it too: a
            // dissolve, a zoom, a whip is what covers a jump (#778).
            let transition_on_cut = t.transitions.iter().any(|tr| tr.clip_a_id == a.id && tr.clip_b_id == b.id);
            let covered = title_over || transition_on_cut || pictures.iter().any(|(tj, c)| tj < ti && secs(c.timeline_in) <= cut - 0.2 && secs(c.timeline_out) >= cut + 0.2);
            if !covered {
                add(Severity::Warning, "jump_cut", Some(cut), format!("a jump cut: {} cuts to itself, {:.1} s later in the file, with nothing over the join", clip_label(pool, a), secs(b.source_in) - secs(a.source_out)), "Cover the join with B-roll or a reaction, or keep it if jump cuts are the piece's style");
            }
        }
    }

    // Screen recordings: a cut in or out while the screen is still
    // changing lands mid-action; a screen needs time to be read.
    for (_, c) in &pictures {
        let Some(beats) = c.media_id().and_then(&mut beats_of) else { continue };
        let label = clip_label(pool, c);
        // Each stretch the program shows of it: a multicam angle only where
        // its group cuts to it, so a hidden angle's edges are no cut and a
        // switch to it is one (#2417).
        for (t_in, t_out, s_in, s_out) in shown_pieces(seq, c) {
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
    }

    // Secrets on screen: an email, a key, a password, a card number the
    // recording caught, within the stretch of the file that is used.
    let mut sensitive_cache: std::collections::HashMap<klipp_core::MediaId, Vec<SensitiveText>> = std::collections::HashMap::new();
    for (_, c) in &pictures {
        let Some(media_id) = c.media_id() else { continue };
        let Some(item) = pool.get(&media_id) else { continue };
        let found = sensitive_cache.entry(media_id).or_insert_with(|| transcripts.screen_sensitive(item));
        for (t_in, _, s_in, s_out) in shown_pieces(seq, c) {
            for s in found.iter().filter(|s| s.end_secs > s_in && s.start_secs < s_out) {
                let from = t_in + (s.start_secs - s_in).max(0.0);
                let to = t_in + (s.end_secs.min(s_out) - s_in);
                add(Severity::Error, "sensitive_on_screen", Some(from), format!("{} shows what looks like a {} ({}) from {from:.2} to {to:.2} s, at {:.0}% across and {:.0}% down the frame", clip_label(pool, c), s.kind, s.masked, s.x * 100.0, s.y * 100.0), "Redact it: the redact op with this clip, from, to and the box (auto_redact does every finding at once), or cut it out");
            }
        }
    }

    // Titles: across a cut, too short to read, sharing the bottom of the frame with captions.
    let cuts: Vec<f64> = pictures.iter().flat_map(|(ti, c)| {
        [secs(c.timeline_in), secs(c.timeline_out)].into_iter().filter(|edge| {
            // A small generated card entering over an uninterrupted shot
            // is an overlay, not a new shot. Keep full-frame generators
            // and cuts in the underlying picture in the cut list.
            if c.generator_data().is_none() { return true; }
            let sample = edge.clamp(secs(c.timeline_in), (secs(c.timeline_out) - frame).max(secs(c.timeline_in)));
            let rect = placed_box(seq, transcripts, c, [seq.resolution.width, seq.resolution.height], [0.0, 0.0, 1.0, 1.0], sample);
            let inset = rect[2] < 0.95 && rect[3] < 0.95;
            let continuous_below = pictures.iter().any(|(below, shot)| below > ti
                && secs(shot.timeline_in) < *edge - frame
                && secs(shot.timeline_out) > *edge + frame);
            !(inset && continuous_below)
        })
    }).collect();
    let titles: Vec<&Clip> = seq.tracks.iter().filter(|t| t.kind == TrackKind::Text && t.visible).flat_map(|t| t.clips.iter()).filter(|c| c.enabled && matches!(c.kind, ClipKind::Title(_))).collect();
    let captions: Vec<&Clip> = seq.tracks.iter().filter(|t| t.kind == TrackKind::Subtitle && t.visible).flat_map(|t| t.clips.iter()).filter(|c| c.enabled).collect();
    // The music's beats on the timeline, when the analysis is sure of
    // them: text cut to the music is timed by them, not by reading
    // speed alone (#1908).
    let mut beats_at: Vec<f64> = Vec::new();
    // The beds whose beats are not read yet: titles under them are timed
    // without the music, and the report says so (#1977).
    let mut unread_beds: Vec<String> = Vec::new();
    for t in &seq.tracks {
        for m in t.clips.iter().filter(|m| is_heard(seq, pool, t, m)) {
            let Some(item) = m.media_id().and_then(|id| pool.get(&id)) else { continue };
            if sound_role(seq, m) != klipp_core::ClipRole::Music && transcripts.library_role(item) != Some(klipp_core::ClipRole::Music) {
                continue;
            }
            let Some(read) = transcripts.beats(item) else {
                let name = clip_label(pool, m);
                if !unread_beds.contains(&name) {
                    unread_beds.push(name);
                }
                continue;
            };
            let Some((_, beats)) = Some(read).filter(|(sure, _)| *sure > BEAT_SURE) else { continue };
            let (s_in, s_out, speed) = (secs(m.source_in), secs(m.source_out), m.speed.abs().max(1e-3));
            beats_at.extend(beats.iter().filter(|b| **b >= s_in && **b < s_out).map(|b| secs(m.timeline_in) + (b - s_in) / speed));
        }
    }
    let near = BEAT_NEAR_SECS.min(frame.max(1e-3));
    let on_beat = |t: f64| beats_at.iter().any(|b| (b - t).abs() <= near + 1e-6);
    // The stretch round a title is cut to the music when the picture
    // cuts on either side of it land on beats too: one title's edges on
    // beats can be chance, a run of cuts on them is not.
    let beat_cut_round = |a: f64, b: f64| {
        let before = cuts.iter().copied().filter(|e| *e <= a + near && *e >= a - BEAT_CONTEXT_SECS).reduce(f64::max);
        let after = cuts.iter().copied().filter(|e| *e >= b - near && *e <= b + BEAT_CONTEXT_SECS).reduce(f64::min);
        matches!((before, after), (Some(x), Some(y)) if on_beat(x) && on_beat(y))
    };
    let mut timing_flagged = false;
    for c in &titles {
        let ClipKind::Title(td) = &c.kind else { continue };
        let (a, b) = (secs(c.timeline_in), secs(c.timeline_out));
        let words: usize = visible_title_text(td).map(|text| text.split_whitespace().count()).sum();
        if words > 0 {
            // In and out on the music's beats: text cut to the music.
            let rhythmic = on_beat(a) && on_beat(b) && beat_cut_round(a, b);
            let mut crossed: Vec<f64> = cuts.iter().copied().filter(|e| *e > a + frame && *e < b - frame).collect();
            crossed.sort_by(f64::total_cmp);
            crossed.dedup_by(|x, y| (*x - *y).abs() < 1e-6);
            if let Some(&cut) = crossed.first() {
                if rhythmic && crossed.iter().all(|e| on_beat(*e)) {
                    add(Severity::Note, "title_over_beat_cuts", Some(a), format!("{} is held from {a:.2} to {b:.2} s over {} cut{} on the music's beats: a heading over a cut-to-music run", clip_label(pool, c), crossed.len(), if crossed.len() == 1 { "" } else { "s" }), "Nothing to change if it is meant as one");
                } else {
                    timing_flagged = true;
                    add(Severity::Warning, "title_across_cut", Some(cut), format!("{} is up from {a:.2} to {b:.2} s and the picture changes at {cut:.2} s", clip_label(pool, c)), "A title does not run across a shot change: move it inside one shot");
                }
            }
            let montage = in_montage(a, b);
            let needs = (words as f64 * 0.3).max(if montage { 0.3 } else { 1.5 });
            let on_the_music = (words as f64 * 0.3).max(0.3);
            if b - a + 1e-6 < needs {
                if rhythmic && b - a + 1e-6 >= on_the_music {
                    add(Severity::Note, "title_on_beat", Some(a), format!("{} has {words} word{} and {:.1} s, in and out on the music's beats: rhythmic text, held to a third of a second a word", clip_label(pool, c), if words == 1 { "" } else { "s" }, b - a), "Keep it if it is cut to the music; hold it longer if it has to be read");
                } else {
                    timing_flagged = true;
                    add(Severity::Warning, "title_short", Some(a), format!("{} has {words} word{} and {:.1} s to be read in", clip_label(pool, c), if words == 1 { "" } else { "s" }, b - a), &format!("Hold it at least {needs:.1} s: about a third of a second a word{}", if montage { " in a montage" } else { ", never under a second and a half" }));
                }
            }
        }
        // Over a screen that is changing, a title fights the picture —
        // when the change is under it: captions in a footer beneath an
        // inset recording are beside the screen, not over it (#1909).
        for (_, p) in &pictures {
            let Some(beats) = p.media_id().and_then(&mut beats_of) else { continue };
            let (t_in, t_out) = (secs(p.timeline_in), secs(p.timeline_out));
            let (lo, hi) = (a.max(t_in), b.min(t_out));
            if hi <= lo {
                continue;
            }
            let file_at = |t: f64| secs(p.source_in) + (t - t_in);
            let (f_lo, f_hi) = (file_at(lo), file_at(hi));
            let busy = beats.action.iter().filter(|(x, y)| f_hi.min(*y) - f_lo.max(*x) > BUSY_UNDER_TITLE_SECS).find_map(|(x, y)| {
                let (from, to) = (lo + (x - f_lo).max(0.0), lo + (y.min(f_hi) - f_lo));
                let under = title_over_screen(seq, pool, transcripts, c, td, p, from, to);
                under.then_some(from)
            });
            if let Some(from) = busy {
                let holds: Vec<String> = beats.holds.iter().filter(|(h0, h1)| h1 - h0 >= b - a).take(3).map(|(h0, h1)| format!("{h0:.1}-{h1:.1} s")).collect();
                add(Severity::Warning, "title_over_busy", Some(from), format!("{} is up over a changing screen from {from:.2} s", clip_label(pool, c)), &if holds.is_empty() { "Move it to where the screen holds still".to_string() } else { format!("Move it to where the screen holds still: {} of the file", holds.join(", ")) });
                break;
            }
        }
        // Compare detections and title pixels at the same timeline time,
        // after both pictures' placement and known foreground occlusion.
        let faces = crate::qc_geometry::detections(seq, pool, transcripts, a, b, true);
        let mut incomplete = faces.incomplete;
        let mut overlaps = |sample| match crate::qc_geometry::title_overlap(seq, transcripts, c, sample, 0.0) {
            Some(hit) => hit,
            None => { incomplete = true; false }
        };
        if let Some(face) = faces.samples.into_iter().find(|sample| overlaps(*sample)) {
            add(Severity::Error, "title_over_face", Some(face.at), format!("{} overlaps a visible face near {:.2} s (at {:.0}% across, {:.0}% down in the composed frame)", clip_label(pool, c), face.at, (face.rect[0]+face.rect[2]/2.0)*100.0, (face.rect[1]+face.rect[3]/2.0)*100.0), "Move it to the side of the composed face or change its timing; inspect this sampled frame before changing the title");
        } else {
            let people = crate::qc_geometry::detections(seq, pool, transcripts, a, b, false);
            if let Some(person) = people.samples.into_iter().filter(|p| p.rect[3] >= PERSON_MIN_HEIGHT).find(|sample| overlaps(*sample)) {
                add(Severity::Warning, "title_over_person", Some(person.at), format!("{} crosses a visible person near {:.2} s in the composed frame", clip_label(pool, c), person.at), "Move it to the side of the composed person or change its timing; inspect this sampled frame first");
            }
            incomplete |= people.incomplete;
        }
        if incomplete {
            add(Severity::Note, "title_overlap_unchecked", Some(a), format!("Some face/person overlap samples for {} could not be checked in composed coordinates", clip_label(pool, c)), "Inspect the composed frames: crops, masks, warps, transitions and unsupported placement need a visual check");
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

    if timing_flagged && !unread_beds.is_empty() {
        add(Severity::Note, "beats_unread", None, format!("the beats of {} are not read yet, so the titles above were timed without the music: a title cut to it may be flagged that is not", unread_beds.join(", ")), "Check again once the beats are read; they are being read now");
    }

    // Sound: cuts inside a word, music left at full level under speech.
    let mut speech_spans: Vec<(f64, f64)> = Vec::new();
    let mut musics: Vec<&Clip> = Vec::new();
    let mut cache: std::collections::HashMap<klipp_core::MediaId, Option<Vec<SpokenWord>>> = std::collections::HashMap::new();
    let mut silence_cache: std::collections::HashMap<klipp_core::MediaId, Vec<(f64, f64)>> = std::collections::HashMap::new();
    let mut unspoken: std::collections::HashSet<klipp_core::MediaId> = std::collections::HashSet::new();
    for t in &seq.tracks {
        for c in t.clips.iter().filter(|c| is_heard(seq, pool, t, c)) {
            let Some(media_id) = c.media_id() else { continue };
            let Some(item) = pool.get(&media_id) else { continue };
            // A clip's role settles what it is before any transcript does:
            // a bed is music, an effect or a room's air is neither speech
            // nor music, whichever of a pair carries the role (#777).
            let role = sound_role(seq, c);
            if role == klipp_core::ClipRole::Music {
                musics.push(c);
                continue;
            }
            let words = cache.entry(media_id).or_insert_with(|| transcripts.words(item));
            let (s_in, s_out) = (secs(c.source_in), secs(c.source_out));
            let inside: Vec<&SpokenWord> = words.as_deref().unwrap_or(&[]).iter().filter(|w| w.end_secs > s_in && w.start_secs < s_out).collect();
            if not_speech(role) {
                // Its transcript is what the recogniser made of the noise:
                // said once, so the words in the clip summaries are read
                // for what they are.
                if !inside.is_empty() && unspoken.insert(media_id) {
                    add(Severity::Note, "words_not_speech", Some(secs(c.timeline_in)), format!("{} is marked {}, so the words its transcript hears are not taken as speech", clip_label(pool, c), format!("{role:?}").to_lowercase()), "Mark it dialogue if someone does speak there");
                }
                continue;
            }
            if inside.is_empty() {
                if words.is_some() || item.video_codec.is_empty() {
                    musics.push(c);
                }
                continue;
            }
            // Speech where its words are, not along the whole clip, and
            // not one word whisper was unsure of (#457).
            speech_spans.extend(spoken_spans(c, words.as_deref(), seq.fps.as_f64()).unwrap_or_default());
            const INTO: f64 = 0.06;
            let silences = silence_cache.entry(media_id).or_insert_with(|| transcripts.silences(item));
            for (edge, at, name) in [(s_in, secs(c.timeline_in), "starts"), (s_out, secs(c.timeline_out), "ends")] {
                // In measured silence the cut is clean, whatever the words' times say.
                if silences.iter().any(|(a, b)| edge >= *a - 0.01 && edge <= *b + 0.01) {
                    continue;
                }
                // A word is no longer than the file: a last word timed past
                // its end is not cut by a clip that plays to the end (#2493).
                let file_end = Some(secs(item.duration)).filter(|d| *d > 0.0).unwrap_or(f64::INFINITY);
                if let Some(w) = inside.iter().find(|w| w.start_secs + INTO < edge && w.end_secs.min(file_end) - INTO > edge) {
                    let to = if name == "starts" { w.start_secs } else { w.end_secs.min(file_end) };
                    add(Severity::Warning, "word_cut", Some(at), format!("the sound {name} inside the word \"{}\"", w.text.trim()), &format!("Trim that edge to {to:.2} s of the file, the word's own edge, plus a beat"));
                }
            }
        }
    }
    let has_music = !musics.is_empty();
    let under_speech = |m: &Clip| {
        let (a, b) = (secs(m.timeline_in), secs(m.timeline_out));
        speech_spans.iter().any(|(x, y)| *x < b && *y > a)
    };
    for m in &musics {
        let a = secs(m.timeline_in);
        let settings = sound_settings_clip(seq, m);
        // Split around the speech and the pieces under it set lower: a
        // duck by hand, as good as keyframes (#457). Three dB under a
        // piece of the same music that plays clear of the speech.
        let lowered = musics.iter().any(|o| {
            o.id != m.id && o.media_id() == m.media_id() && !under_speech(o) && sound_settings_clip(seq, o).volume >= settings.volume * crate::ops::db_to_linear(3.0)
        });
        if under_speech(m) && settings.volume_keyframes.is_empty() && settings.volume > 0.26 && !lowered {
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

/// The stretches of `c` the program shows, each as (timeline in, out,
/// file in, out): the whole clip, or for a multicam angle the stretches its
/// group cuts to it (#2417).
fn shown_pieces(seq: &Sequence, c: &Clip) -> Vec<(f64, f64, f64, f64)> {
    let (t_in, t_out, s_in, s_out) = (secs(c.timeline_in), secs(c.timeline_out), secs(c.source_in), secs(c.source_out));
    let rate = if t_out > t_in { (s_out - s_in) / (t_out - t_in) } else { 1.0 };
    match seq.multicam_shown_spans(c) {
        None => vec![(t_in, t_out, s_in, s_out)],
        Some(spans) => spans.into_iter().map(|(a, b)| (a, b, s_in + (a - t_in) * rate, s_in + (b - t_in) * rate)).collect(),
    }
}

#[cfg(test)]
mod tests {

    /// A global LUT that is not on disk is an error `check` names; one
    /// that is there, or none at all, is nothing (#1019).
    #[test]
    fn a_missing_global_lut_is_an_error() {
        let empty = || Report { duration_secs: 0.0, errors: 0, warnings: 0, findings: Vec::new() };
        let mut r = empty();
        note_missing_global_lut(&mut r, Some(std::path::Path::new("/nonexistent/klipp-1019/look.cube")));
        assert_eq!(r.errors, 1);
        assert_eq!(r.findings[0].rule, "missing_lut");
        let mut r = empty();
        note_missing_global_lut(&mut r, Some(&std::env::current_exe().unwrap()));
        note_missing_global_lut(&mut r, None);
        assert_eq!(r.errors, 0);
        assert!(r.findings.is_empty());
    }
    use super::*;
    use klipp_core::{Fps, RationalTime, Timecode};
    use klipp_timeline::{Brief, CastMember, Track};
    use klipp_timeline::test_clip::TestClip;

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
            Some("one two three four five six seven eight nine ten".split(' ').enumerate().map(|(k, w)| SpokenWord { start_secs: k as f64, end_secs: k as f64 + 0.6, text: w.into(), confidence: 1.0 }).collect())
        }
    }

    /// A take whose camera moves (YDIF `self.1`) all through it.
    struct Moving(klipp_core::MediaId, f32);

    impl Transcripts for Moving {
        fn words(&self, _item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
            Some(Vec::new())
        }
        fn motion_between(&self, item: &klipp_media::MediaItem, _from: f64, _to: f64) -> Option<f32> {
            (item.id == self.0).then_some(self.1)
        }
    }

    /// #461: in a walk cut to a montage, a cut far along one moving take
    /// goes somewhere else and is not a jump cut; a short skip still is,
    /// and so is a long one on a camera standing still.
    #[test]
    fn a_cut_between_places_in_one_walking_take_is_not_a_jump_cut() {
        let (mut seq, pool, talk, _) = piece();
        let skip_to = |seq: &mut Sequence, src: f64| {
            for t in seq.tracks.iter_mut() {
                let c = &mut t.clips[1];
                (c.timeline_in, c.timeline_out, c.source_in, c.source_out) = (tc(4.0), tc(8.0), tc(src), tc(src + 4.0));
            }
        };
        skip_to(&mut seq, 24.0);
        assert!(!rules(&check(&seq, &pool, &Moving(talk, 6.5))).contains(&"jump_cut"), "20 s along a walk: another place");
        assert!(rules(&check(&seq, &pool, &Moving(talk, 1.0))).contains(&"jump_cut"), "20 s on a still camera: a jump");
        skip_to(&mut seq, 5.0);
        assert!(rules(&check(&seq, &pool, &Moving(talk, 6.5))).contains(&"jump_cut"), "a second along a walk: still a jump");
    }

    /// #778: a transition on the cut covers the jump as a picture over it
    /// does: a dissolve between two selects of one take is no jump cut.
    #[test]
    fn a_transition_on_the_cut_covers_a_jump() {
        let (mut seq, pool, talk, _) = piece();
        for t in seq.tracks.iter_mut() {
            let c = &mut t.clips[1];
            (c.timeline_in, c.timeline_out, c.source_in, c.source_out) = (tc(4.0), tc(8.0), tc(20.0), tc(24.0));
        }
        assert!(rules(&check(&seq, &pool, &Spoken(talk))).contains(&"jump_cut"), "bare, it jumps");
        let v = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
        let (a, b) = (seq.tracks[v].clips[0].id, seq.tracks[v].clips[1].id);
        seq.tracks[v].transitions.push(klipp_timeline::Transition {
            id: klipp_core::TransitionId::new(),
            kind: klipp_core::TransitionKind::CrossDissolve,
            duration: tc(0.5),
            clip_a_id: a,
            clip_b_id: b,
            user_params: Default::default(),
            alignment: klipp_core::TransitionAlignment::Center,
        });
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(!rules(&r).contains(&"jump_cut"), "{}", r.text());
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

    /// `seq` with `op` resolved and applied, as the app does.
    fn applied(seq: &Sequence, pool: &MediaPool, op: crate::ops::Op) -> Sequence {
        let plan = crate::ops::resolve(seq, pool, &[op]).expect("the op plans");
        let (mut seqs, mut pool, mut fps) = (vec![seq.clone()], pool.clone(), seq.fps);
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        for step in &plan.steps {
            view.apply_forward(&step.edit);
        }
        seqs.remove(0)
    }

    /// #2357: a strobe's black gaps and two-frame pieces are its effect,
    /// said once as a note, not 13 errors and 13 flashes; rippled, its
    /// cuts aren't jump cuts. A gap of another width after it is still
    /// black.
    #[test]
    fn a_strobe_is_one_note_and_a_real_gap_beside_it_still_an_error() {
        use crate::ops::{Anchor, ClipRef, Op};
        let (seq, pool, talk, _) = piece();
        let strobe = |ripple| Op::Strobe { clip_id: ClipRef::Id(seq.tracks[0].clips[0].id), piece_frames: 2, from: Some(Anchor::Frame(25)), to: Some(Anchor::Frame(77)), on: 1, off: 1, ripple };
        let left = applied(&seq, &pool, strobe(false));
        let r = check(&left, &pool, &Spoken(talk));
        let notes: Vec<_> = r.findings.iter().filter(|f| f.rule == "strobe").collect();
        assert_eq!(notes.len(), 1, "{}", r.text());
        assert!(notes[0].what == "a strobe from 1.00 to 3.00 s: 13 pieces of 2 frames" && notes[0].severity == Severity::Note, "{:?}", notes[0]);
        assert!(!rules(&r).iter().any(|x| matches!(*x, "black" | "flash")), "{}", r.text());
        let rippled = applied(&seq, &pool, strobe(true));
        let r = check(&rippled, &pool, &Spoken(talk));
        assert!(rules(&r).contains(&"strobe") && !rules(&r).iter().any(|x| matches!(*x, "flash" | "jump_cut")), "{}", r.text());
        // The second select moved 5 frames later: a gap of its own.
        let mut gapped = left.clone();
        for t in gapped.tracks.iter_mut() {
            let c = t.clips.last_mut().unwrap();
            (c.timeline_in, c.timeline_out) = (tc(4.2), tc(8.2));
        }
        let r = check(&gapped, &pool, &Spoken(talk));
        let black: Vec<_> = r.findings.iter().filter(|f| f.rule == "black").collect();
        assert!(black.len() == 1 && black[0].what.contains("from 4.00 to 4.20"), "{}", r.text());
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

    /// #2417: a multicam group's edges are read where the program shows
    /// an angle, once a shot. Two angles of the screen end together at 31
    /// s, inside the change at 30-33 s; the group cuts to angle 1 at 5 s,
    /// so the cut away is angle 1's alone. The switch itself, at 5 s of
    /// the file, lands on a still screen.
    #[test]
    fn a_multicam_cut_is_read_once_from_the_angle_on_screen() {
        let (mut seq, pool, screen) = screencast(&[(0.0, 31.0, 0.0)]);
        let group = klipp_core::GroupId::new();
        let mut second = seq.tracks[0].clips[0].clone();
        second.id = klipp_core::ClipId::new();
        (second.multicam_group, second.multicam_angle) = (Some(group), 1);
        let primary = &mut seq.tracks[0].clips[0];
        (primary.multicam_group, primary.multicam_angle, primary.multicam_cuts) = (Some(group), 0, vec![(5.0, 1)]);
        let mut v2 = Track::new("V2", TrackKind::Video);
        v2.clips.push(second.clone());
        seq.tracks.push(v2);
        let r = check(&seq, &pool, &Screen(screen));
        let mid: Vec<&Finding> = r.findings.iter().filter(|f| f.rule == "cut_mid_action").collect();
        assert_eq!(mid.len(), 1, "once, not once an angle:\n{}", r.text());
        assert!(mid[0].what.contains("cuts away at 31.00 s") && (mid[0].at_secs.unwrap() - 31.0).abs() < 1e-6, "{}", r.text());
    }

    /// A title over the face at 40-44 s reports the actual sample time;
    /// the same title low in the frame is not an overlap.
    #[test]
    fn a_title_over_a_face_reports_the_sample_time() {
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
        assert!((f[0].at_secs.unwrap() - 2.5).abs() < 1e-6);
    }

    #[test]
    fn qc_unsupported_geometry_requests_a_visual_check() {
        struct Unknown(Screen);
        impl Transcripts for Unknown {
            fn words(&self, _: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> { None }
            fn faces(&self, item: &klipp_media::MediaItem) -> Vec<(f64, [f32;4])> { self.0.faces(item) }
            fn picture_geometry(&self, _: &Sequence, _: &Clip, _: f64) -> Option<crate::qc_geometry::Geometry> { None }
        }
        let (mut seq, pool, media) = screencast(&[(0.0,10.0,40.0)]);
        let mut title = Track::new("T", TrackKind::Text);
        title.clips.push(Clip { kind: ClipKind::Title(klipp_timeline::TitleData::single_text(Default::default())), ..Clip::gap(tc(0.0),tc(4.0)) });
        seq.tracks.insert(0,title);
        let report = check(&seq,&pool,&Unknown(Screen(media)));
        assert!(rules(&report).contains(&"title_overlap_unchecked"));
        assert!(!rules(&report).contains(&"title_over_face"));
    }

    /// #2168: a persistent or brief graphic has no words to time, while
    /// a mixed text/shape title still needs both editorial warnings.
    #[test]
    fn graphics_only_title_timing_requires_visible_words() {
        use klipp_timeline::{Shape, ShapeKind, TextBox, TitleData};
        let (mut seq, pool, _) = screencast(&[(0.0, 2.0, 0.0), (2.0, 5.0, 20.0)]);
        let mut graphic = TitleData::single_text(TextBox::default());
        graphic.objects[0].content = TitleObjectContent::Shape(Shape {
            kind: ShapeKind::Rectangle { corner_radius: 0.0 },
            fill: Some([255, 255, 255, 255]), gradient: None, stroke: None, fit: None,
        });
        let mut text = Track::new("Graphics", TrackKind::Text);
        text.clips.push(Clip { kind: ClipKind::Title(graphic.clone()), ..Clip::gap(tc(1.5), tc(2.5)) });
        seq.tracks.insert(0, text);
        for (words, visible, expected) in [("", true, false), (" \n\t", true, false), ("Hidden", false, false), ("Readable", true, true)] {
            let mut td = graphic.clone();
            let mut object = TitleData::single_text(TextBox { text: words.into(), ..Default::default() }).objects.remove(0);
            object.id = klipp_timeline::TitleObjectId(1);
            object.visible = visible;
            td.objects.push(object);
            seq.tracks[0].clips[0].kind = ClipKind::Title(td);
            let report = check(&seq, &pool, &crate::ops::NoTranscripts);
            for rule in ["title_short", "title_across_cut"] {
                assert_eq!(rules(&report).contains(&rule), expected, "{words:?}, visible={visible}: {}", report.text());
            }
            let label = clip_label(&pool, &seq.tracks[0].clips[0]);
            assert_eq!(label, if expected { "title \"Readable\"" } else { "graphics" });
        }
    }

    #[test]
    fn graphics_only_title_keeps_overlap_checks_and_its_own_label() {
        use klipp_timeline::{Shape, ShapeKind, TextBox, TitleData, TitlePosition};
        let (mut seq, pool, screen) = screencast(&[(0.0, 10.0, 40.0)]);
        let mut td = TitleData::single_text(TextBox::default());
        td.objects[0].content = TitleObjectContent::Shape(Shape {
            kind: ShapeKind::Rectangle { corner_radius: 0.0 },
            fill: Some([255, 255, 255, 255]), gradient: None, stroke: None, fit: None,
        });
        td.objects[0].transform.center = TitlePosition { x: 0.5, y: 0.15 };
        td.objects[0].transform.size = TitlePosition { x: 0.2, y: 0.2 };
        let mut track = Track::new("Graphics", TrackKind::Text);
        track.clips.push(Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(4.0)) });
        seq.tracks.insert(0, track);
        let report = check(&seq, &pool, &Screen(screen));
        let finding = report.findings.iter().find(|f| f.rule == "title_over_face").expect("graphics can still obscure faces");
        assert!(finding.what.starts_with("graphics "), "{}", finding.what);
        seq.tracks[0].clips[0].name = Some("Corner hexagon".into());
        assert_eq!(clip_label(&pool, &seq.tracks[0].clips[0]), "\"Corner hexagon\"");
    }

    /// #510: a title across someone's body, not their face, is a warning
    /// with a zone that stays clear of them; one clear of them is not.
    #[test]
    fn a_title_across_a_person_is_a_warning() {
        struct Walker(klipp_core::MediaId);
        impl Transcripts for Walker {
            fn words(&self, _item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
                None
            }
            fn people(&self, item: &klipp_media::MediaItem) -> Vec<(f64, [f32; 4])> {
                if item.id == self.0 { vec![(41.0, [0.4, 0.35, 0.2, 0.6]), (43.0, [0.45, 0.35, 0.2, 0.6])] } else { vec![] }
            }
        }
        let (mut seq, pool, media) = screencast(&[(0.0, 10.0, 40.0)]);
        let title = |text: &str, y: f32| {
            let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: text.into(), font_size: 60.0, ..Default::default() });
            td.objects[0].transform.center = klipp_timeline::TitlePosition { x: 0.5, y };
            td
        };
        let mut text = Track::new("T", TrackKind::Text);
        text.clips.push(Clip { id: klipp_core::ClipId::new(), kind: ClipKind::Title(title("Across", 0.6)), source_in: tc(0.0), source_out: tc(3.0), ..Clip::gap(tc(1.0), tc(4.0)) });
        text.clips.push(Clip { id: klipp_core::ClipId::new(), kind: ClipKind::Title(title("Above", 0.12)), source_in: tc(0.0), source_out: tc(3.0), ..Clip::gap(tc(5.0), tc(8.0)) });
        seq.tracks.insert(0, text);
        let r = check(&seq, &pool, &Walker(media));
        let f: Vec<_> = r.findings.iter().filter(|f| f.rule == "title_over_person").collect();
        assert_eq!(f.len(), 1, "{}", r.text());
        assert!(f[0].hint.contains("Move it to the"), "{}", f[0].hint);
        assert!((f[0].at_secs.unwrap() - 1.0).abs() < 1e-6);
        // #1590: a figure a few pixels tall on a wide beach is not crossed.
        struct Tiny(klipp_core::MediaId);
        impl Transcripts for Tiny {
            fn words(&self, _item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
                None
            }
            fn people(&self, item: &klipp_media::MediaItem) -> Vec<(f64, [f32; 4])> {
                if item.id == self.0 { vec![(41.0, [0.49, 0.56, 0.01, 0.04])] } else { vec![] }
            }
        }
        assert!(!rules(&check(&seq, &pool, &Tiny(media))).contains(&"title_over_person"));
    }

    /// One screen recording cut into shots, no sound.
    #[test]
    fn inset_generator_edges_are_not_shot_cuts_but_underlying_edits_are() {
        struct Inset(bool);
        impl Transcripts for Inset {
            fn words(&self, _: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> { None }
            fn placement(&self, c: &Clip, _: f64, _: f64) -> Option<klipp_timeline::surface::Placement> {
                (self.0 && c.generator_data().is_some()).then_some((0.0, 0.0, 0.4, 0.4, 0.0, 0.5, 0.5))
            }
        }
        let (mut seq, pool, _) = screencast(&[(0.0, 8.0, 0.0)]);
        let mut overlay = Track::new("Card", TrackKind::Video);
        overlay.clips.push(Clip { kind: ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color: [30, 60, 90, 255] }), ..Clip::gap(tc(2.0), tc(6.0)) });
        seq.tracks.insert(0, overlay);
        let mut title = Track::new("Heading", TrackKind::Text);
        title.clips.push(Clip { kind: ClipKind::Title(klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Keep your flow".into(), ..Default::default() })), ..Clip::gap(tc(1.0), tc(7.0)) });
        seq.tracks.insert(0, title);
        assert!(!rules(&check(&seq, &pool, &Inset(true))).contains(&"title_across_cut"));
        assert!(rules(&check(&seq, &pool, &Inset(false))).contains(&"title_across_cut"));
        let mut second = seq.tracks[2].clips[0].clone();
        second.id = klipp_core::ClipId::new();
        second.timeline_in = tc(4.0);
        seq.tracks[2].clips[0].timeline_out = tc(4.0);
        seq.tracks[2].clips.push(second);
        assert!(rules(&check(&seq, &pool, &Inset(true))).contains(&"title_across_cut"));
    }

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
        let segment = |source, section: &str, title: &str| PlanSegment { media_id: screen, source_in_secs: source, source_out_secs: source + 0.85, section: section.into(), title: Some(PlanTitle { text: title.into(), ..Default::default() }), feature: String::new(), note: String::new(), with_audio: false, transition: None, sfx: None, zoom: None, placement: None, background: None, layers: Vec::new() };
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

    /// #1908: a promo's text cut to the music. A one-word title in and
    /// out on the bed's beats is rhythmic, a note not a reading fault; a
    /// heading held over cuts that land on the beats is one too. Off the
    /// beat, or with no music under it, both are judged as before.
    #[test]
    fn text_cut_to_the_music_is_read_as_rhythm() {
        struct Bed(klipp_core::MediaId, f64);
        impl Transcripts for Bed {
            fn words(&self, _item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
                None
            }
            fn beats(&self, item: &klipp_media::MediaItem) -> Option<(f32, Vec<f64>)> {
                (item.id == self.0).then(|| (0.9, (0..200).map(|i| f64::from(i) * self.1).collect()))
            }
        }
        // Shots cut at 2, 4 and 6 s, on the beats; a music bed under them.
        let (mut seq, mut pool, _) = screencast(&[(0.0, 2.0, 0.0), (2.0, 4.0, 20.0), (4.0, 6.0, 40.0), (6.0, 8.0, 50.0)]);
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/bed.wav".into(), None);
        item.duration = tc(60.0);
        item.has_audio = true;
        let bed = pool.add(item);
        let mut a = Track::new("A1", TrackKind::Audio);
        let mut music = Clip::media(bed, tc(0.0), tc(8.0), tc(0.0), tc(8.0));
        music.role = klipp_core::ClipRole::Music;
        a.clips.push(music);
        seq.tracks.push(a);
        let title = |text: &str, from: f64, to: f64| Clip { id: klipp_core::ClipId::new(), kind: ClipKind::Title(klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: text.into(), ..Default::default() })), source_in: tc(0.0), source_out: tc(to - from), ..Clip::gap(tc(from), tc(to)) };
        let mut text = Track::new("T1", TrackKind::Text);
        text.clips.push(title("Colour.", 1.0, 2.0));
        seq.tracks.insert(0, text);
        let found = |seq: &Sequence, t: &dyn Transcripts| rules(&check(seq, &pool, t));
        let got = found(&seq, &Bed(bed, 0.5));
        assert!(got.contains(&"title_on_beat") && !got.contains(&"title_short"), "{got:?}");
        let unread = found(&seq, &crate::ops::NoTranscripts);
        assert!(unread.contains(&"title_short"), "no beats read: judged as before");
        assert!(unread.contains(&"beats_unread"), "and said to be judged without the music (#1977): {unread:?}");
        assert!(!got.contains(&"beats_unread"), "read beats: nothing to say");
        seq.tracks[0].clips[0] = title("Colour.", 0.2, 1.9);
        assert!(!found(&seq, &crate::ops::NoTranscripts).contains(&"beats_unread"), "no title judged on its timing: nothing to say");
        seq.tracks[0].clips[0] = title("Colour.", 1.0, 2.0);
        seq.tracks[0].clips[0] = title("Colour.", 1.15, 2.15);
        assert!(found(&seq, &Bed(bed, 0.5)).contains(&"title_short"), "off the beat: judged as before");
        // At 140 BPM, on the beats, but the cuts round it are not: chance,
        // not rhythm.
        let beat = 60.0 / 140.0;
        seq.tracks[0].clips[0] = title("Create.", 6.0 * beat, 8.0 * beat);
        assert!(found(&seq, &Bed(bed, beat)).contains(&"title_short"), "cuts at 2, 4, 6 s are off a 140 BPM grid");
        // Cut to that grid, a 0.8 s title whose edges sit 0.1 s off two
        // beats is still not on them.
        {
            let v = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
            for (k, c) in seq.tracks[v].clips.iter_mut().enumerate() {
                (c.timeline_in, c.timeline_out) = (tc(k as f64 * 5.0 * beat), tc((k + 1) as f64 * 5.0 * beat));
            }
        }
        seq.tracks[0].clips[0] = title("Create.", 3.0 * beat + 0.1, 3.0 * beat + 0.9);
        assert!(found(&seq, &Bed(bed, beat)).contains(&"title_short"), "a tenth of a second off is off");
        seq.tracks[0].clips[0] = title("Create.", 3.0 * beat, 5.0 * beat);
        assert!(found(&seq, &Bed(bed, beat)).contains(&"title_on_beat"), "on that grid it is rhythm");
        {
            let v = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
            for (k, c) in seq.tracks[v].clips.iter_mut().enumerate() {
                (c.timeline_in, c.timeline_out) = (tc(k as f64 * 2.0), tc((k + 1) as f64 * 2.0));
            }
        }
        // A heading held over the cuts at 2 and 4 s.
        seq.tracks[0].clips[0] = title("Keep your flow.", 1.5, 5.0);
        let got = found(&seq, &Bed(bed, 0.5));
        assert!(got.contains(&"title_over_beat_cuts") && !got.contains(&"title_across_cut"), "{got:?}");
        // A cut off the beat under it.
        let v = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
        seq.tracks[v].clips[1].timeline_out = tc(4.3);
        seq.tracks[v].clips[2].timeline_in = tc(4.3);
        assert!(found(&seq, &Bed(bed, 0.5)).contains(&"title_across_cut"));
    }

    /// #1909: feature captions in a footer beneath an inset recording
    /// were flagged as over the changing screen though they never touch
    /// it. Placed inset, the footer is clear; the same footer over a
    /// full-frame screen, or the title moved onto the inset, is not.
    #[test]
    fn a_title_beside_an_inset_screen_is_not_over_it() {
        struct Inset(klipp_core::MediaId, bool, bool);
        impl Transcripts for Inset {
            fn words(&self, _item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
                None
            }
            fn screen_beats(&self, item: &klipp_media::MediaItem) -> Option<ScreenBeats> {
                Screen(self.0).screen_beats(item)
            }
            // Three quarters of the frame, lifted 100 px: the screen
            // spans 3% to 78% down the frame.
            // With the third flag, the title clip rises 300 px two seconds in.
            fn placement(&self, clip: &Clip, clip_secs: f64, _fps: f64) -> Option<klipp_timeline::surface::Placement> {
                match clip.media_id() {
                    Some(_) => self.1.then_some((0.0, -100.0, 0.75, 0.75, 0.0, 0.5, 0.5)),
                    None => self.2.then_some((0.0, if clip_secs > 2.0 { -300.0 } else { 0.0 }, 1.0, 1.0, 0.0, 0.5, 0.5)),
                }
            }
        }
        // The screen changes at 10-12 s of the file: 4-6 s of the timeline.
        let (mut seq, pool, screen) = screencast(&[(0.0, 8.0, 6.0)]);
        let caption = |y: f32| {
            let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Connected nodes".into(), font_size: 40.0, ..Default::default() });
            td.objects[0].transform.center = klipp_timeline::TitlePosition { x: 0.5, y };
            Clip { id: klipp_core::ClipId::new(), kind: ClipKind::Title(td), source_in: tc(0.0), source_out: tc(4.0), ..Clip::gap(tc(3.0), tc(7.0)) }
        };
        let mut text = Track::new("T1", TrackKind::Text);
        text.clips.push(caption(0.92));
        seq.tracks.insert(0, text);
        let busy = |seq: &Sequence, t: &dyn Transcripts| {
            let r = check(seq, &pool, t);
            (rules(&r).contains(&"title_over_busy"), r.text())
        };
        let (flagged, text) = busy(&seq, &Inset(screen, true, false));
        assert!(!flagged, "the footer is clear of the inset: {text}");
        assert!(busy(&seq, &Inset(screen, false, false)).0, "over a full-frame screen it is flagged");
        assert!(busy(&seq, &Inset(screen, true, true)).0, "carried up over the inset by its own placement, it is flagged");
        // Its bottom third over the inset's bottom edge: over it.
        seq.tracks[0].clips[0] = caption(0.79);
        assert!(busy(&seq, &Inset(screen, true, false)).0, "a third of the caption over the screen is over it");
        // Clear of it by its text, but not by its drawn shadow.
        seq.tracks[0].clips[0] = caption(0.84);
        assert!(!busy(&seq, &Inset(screen, true, false)).0, "just clear");
        let ClipKind::Title(td) = &mut seq.tracks[0].clips[0].kind else { unreachable!() };
        td.objects[0].fx_mut().shadows.push(klipp_timeline::TitleShadow { enabled: true, color: [0, 0, 0, 200], offset_x: 0.0, offset_x_keyframes: Vec::new(), offset_y: -40.0, offset_y_keyframes: Vec::new(), blur: 0.0, blur_keyframes: Vec::new() });
        assert!(busy(&seq, &Inset(screen, true, false)).0, "its shadow falls on the screen");
        seq.tracks[0].clips[0] = caption(0.5);
        assert!(busy(&seq, &Inset(screen, true, false)).0, "moved onto the inset it is flagged");
    }

    #[test]
    fn the_brief_s_features_are_held_to_by_the_edit_plan() {
        use klipp_timeline::{EditPlan, Feature, PlanSegment};
        let (mut seq, pool, screen) = screencast(&[(0.0, 4.0, 0.0), (4.0, 9.0, 12.0)]);
        seq.brief = Some(Brief { features: vec![Feature { title: "Timeline".into(), rank: 1, ..Default::default() }, Feature { title: "Grade".into(), rank: 2, ..Default::default() }], ..Default::default() });
        let got = rules(&check(&seq, &pool, &Screen(screen)));
        assert!(got.contains(&"no_edit_plan"), "{got:?}");
        let seg = |feature: &str, a: f64, b: f64| PlanSegment { feature: feature.into(), media_id: screen, source_in_secs: a, source_out_secs: b, note: String::new(), with_audio: false, title: None, transition: None, sfx: None, zoom: None, placement: None, background: None, section: String::new(), layers: Vec::new() };
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

    /// #639: two clips laid over each other on one track are an error,
    /// on whichever track; clips that only meet are not.
    #[test]
    fn clips_laid_over_each_other_on_a_track_are_an_error() {
        let (mut seq, pool, talk, _) = piece();
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(!rules(&r).contains(&"overlap"), "{}", r.text());
        // The first picture grown two seconds into the second.
        seq.tracks[0].clips[0].timeline_out = tc(6.0);
        let r = check(&seq, &pool, &Spoken(talk));
        let f = r.findings.iter().find(|f| f.rule == "overlap").unwrap_or_else(|| panic!("{}", r.text()));
        assert_eq!((f.severity, f.at_secs), (Severity::Error, Some(4.0)));
        assert!(f.what.contains("from 4.00 to 6.00 s"), "{}", f.what);
    }

    /// #978: a clip trimmed past its file's end is caught before export:
    /// starting there it has no picture, an error; running on past it, the
    /// last frame is held, a warning. A still of any length is neither.
    #[test]
    fn a_clip_past_the_end_of_its_file_is_caught() {
        let (mut seq, mut pool, talk, _) = piece();
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(!rules(&r).contains(&"past_end") && !rules(&r).contains(&"held_end"), "{}", r.text());
        // The talk file is 60 s: the second picture slipped to start at 61 s.
        let second = &mut seq.tracks[0].clips[1];
        (second.source_in, second.source_out) = (tc(61.0), tc(65.0));
        let r = check(&seq, &pool, &Spoken(talk));
        let f = r.findings.iter().find(|f| f.rule == "past_end").unwrap_or_else(|| panic!("{}", r.text()));
        assert_eq!((f.severity, f.at_secs), (Severity::Error, Some(4.0)));
        // Starting at 58 s, it runs 2 s past the end: held from 6 s.
        let second = &mut seq.tracks[0].clips[1];
        (second.source_in, second.source_out) = (tc(58.0), tc(62.0));
        let r = check(&seq, &pool, &Spoken(talk));
        let f = r.findings.iter().find(|f| f.rule == "held_end").unwrap_or_else(|| panic!("{}", r.text()));
        assert_eq!((f.severity, f.at_secs), (Severity::Warning, Some(6.0)));
        // A still: its 5 s pool duration is a default, not an end.
        let mut still = klipp_media::MediaItem::synthetic_for_test("/tmp/card.png".into(), None);
        still.duration = tc(5.0);
        still.video_codec = "png".into();
        still.is_still = true;
        let card = pool.add(still);
        seq.tracks[0].clips[1] = Clip::media(card, tc(4.0), tc(8.0), tc(3.0), tc(7.0));
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(!rules(&r).contains(&"past_end") && !rules(&r).contains(&"held_end"), "{}", r.text());
    }

    /// #995: a clip with two Transform effects is told that only the
    /// first places it.
    #[test]
    fn a_second_transform_effect_is_reported() {
        let (mut seq, pool, talk, _) = piece();
        let t = || klipp_effects::BuiltinEffect::Transform { position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
        seq.tracks[0].clips[0] = TestClip::of(seq.tracks[0].clips[0].clone()).effect(t()).build();
        assert!(!rules(&check(&seq, &pool, &Spoken(talk))).contains(&"extra_transform"));
        seq.tracks[0].clips[0] = TestClip::of(seq.tracks[0].clips[0].clone()).effect(t()).build();
        let r = check(&seq, &pool, &Spoken(talk));
        let f = r.findings.iter().find(|f| f.rule == "extra_transform").unwrap_or_else(|| panic!("{}", r.text()));
        assert_eq!((f.severity, f.at_secs), (Severity::Warning, Some(0.0)));
        // Switched off, the second is no finding.
        {
            let effect_clip = &mut seq.tracks[0].clips[0];
            effect_clip.set_effect_list(effect_clip.builtin_effects().to_vec(), vec![true, false]);
        }
        assert!(!rules(&check(&seq, &pool, &Spoken(talk))).contains(&"extra_transform"));
    }

    /// #517: a full-frame title is picture. The film ended on a 7.6 s end
    /// card over the music's fade, and the check said the picture had
    /// ended while the sound ran on. A card over a gap is not black either;
    /// a plain title, text over nothing, still is neither.
    #[test]
    fn a_full_frame_card_is_picture() {
        let (mut seq, pool, talk, music) = piece();
        let card = |a: f64, b: f64, background| {
            let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Wherever We Walk".into(), ..Default::default() });
            td.background = background;
            Clip { kind: ClipKind::Title(td), source_in: tc(0.0), source_out: tc(b - a), ..Clip::gap(tc(a), tc(b)) }
        };
        let full = || klipp_timeline::TitleBackground::FullFrame { color: [20, 17, 14, 255] };
        let mut bed = Track::new("A2", TrackKind::Audio);
        bed.clips = vec![Clip::media(music, tc(0.0), tc(12.0), tc(0.0), tc(12.0))];
        seq.tracks.push(bed);
        let mut text = Track::new("T1", TrackKind::Text);
        text.clips = vec![card(8.0, 12.0, klipp_timeline::TitleBackground::None)];
        seq.tracks.insert(0, text);
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(rules(&r).contains(&"ends_apart"), "text over nothing is not picture: {}", r.text());

        seq.tracks[0].clips = vec![card(8.0, 12.0, full())];
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(!rules(&r).contains(&"ends_apart") && !rules(&r).contains(&"black"), "{}", r.text());

        // The second shot a second later: black, unless a card covers it.
        let v = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
        let c = &mut seq.tracks[v].clips[1];
        (c.timeline_in, c.timeline_out) = (tc(5.0), tc(8.0));
        assert!(rules(&check(&seq, &pool, &Spoken(talk))).contains(&"black"));
        seq.tracks[0].clips.insert(0, card(4.0, 5.0, full()));
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(!rules(&r).contains(&"black"), "{}", r.text());
    }

    /// #1572: what runs on past the other fades out at its end, as the
    /// hint asks, and the check stays quiet: music past the picture with
    /// its fade, a last shot past the sound keyed down to nothing (by the
    /// compose op, as a film fades its picture out). A fade of a blink is
    /// still a finding: it must cover the run-on's last second (#1598).
    #[test]
    fn a_run_on_that_fades_out_is_no_finding() {
        let (mut seq, pool, talk, music) = piece();
        let mut bed = Track::new("A2", TrackKind::Audio);
        bed.clips = vec![Clip::media(music, tc(0.0), tc(10.0), tc(0.0), tc(10.0))];
        seq.tracks.push(bed);
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(rules(&r).contains(&"ends_apart"), "music runs on 2 s past the picture: {}", r.text());
        seq.tracks.last_mut().unwrap().clips[0].fade_out_secs = 0.2;
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(rules(&r).contains(&"ends_apart"), "a 0.2 s fade leaves 1.8 s of music at full level over black (#1598): {}", r.text());
        seq.tracks.last_mut().unwrap().clips[0].fade_out_secs = 3.5;
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(!rules(&r).contains(&"ends_apart"), "the music fades out: {}", r.text());

        // The picture runs on past the sound: a warning until the last shot
        // fades to nothing at its end.
        seq.tracks.pop();
        let v = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
        let a = seq.tracks.iter().position(|t| t.kind == TrackKind::Audio).unwrap();
        seq.tracks[a].clips[1].timeline_out = tc(6.0);
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(rules(&r).contains(&"ends_apart"), "the picture runs on 2 s past the sound: {}", r.text());
        let last = seq.tracks[v].clips[1].id;
        let fade_from = |from: f64, to: f64| -> Sequence {
            let ops: Vec<crate::ops::Op> = serde_json::from_str(&format!(r#"[{{"op":"compose","clip_id":"{}","poses":[{{"secs":{from},"opacity":1}},{{"secs":4.0,"opacity":{to}}}]}}]"#, last.0)).unwrap();
            let plan = crate::ops::resolve(&seq, &pool, &ops).unwrap();
            let (mut seqs, mut p, mut fps) = (vec![seq.clone()], pool.clone(), seq.fps);
            let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
            view.apply_forward(&plan.steps[0].edit);
            seqs.remove(0)
        };
        let fade = |to: f64| fade_from(2.0, to);
        let r = check(&fade_from(3.8, 0.0), &pool, &Spoken(talk));
        assert!(rules(&r).contains(&"ends_apart"), "a 0.2 s fade to nothing is a cut to black after 1.8 s of picture (#1598): {}", r.text());
        let r = check(&fade(0.5), &pool, &Spoken(talk));
        assert!(rules(&r).contains(&"ends_apart"), "a fade to half is still picture: {}", r.text());
        let r = check(&fade(0.0), &pool, &Spoken(talk));
        assert!(!rules(&r).contains(&"ends_apart"), "the last shot fades out: {}", r.text());
    }

    /// #2493: a clip that plays to the end of its file does not cut its
    /// last word, whose transcript time runs past the end; a cut short of
    /// the end inside it still does.
    #[test]
    fn a_word_timed_past_the_files_end_is_not_cut_there() {
        let (mut seq, mut pool, talk, _) = piece();
        // "eight" is timed 7.0 to 7.6 s; the file ends at 7.3.
        pool.get_mut(&talk).unwrap().duration = tc(7.3);
        let to_end = |seq: &mut Sequence, out: f64| for t in seq.tracks.iter_mut() {
            let c = &mut t.clips[1];
            (c.timeline_out, c.source_out) = (tc(out), tc(out));
        };
        to_end(&mut seq, 7.3);
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(!rules(&r).contains(&"word_cut"), "played to the file's end: {}", r.text());
        to_end(&mut seq, 7.2);
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(rules(&r).contains(&"word_cut"), "cut inside the word before the end: {}", r.text());
    }

    /// #2479: a clip that holds its pictures for more than a frame is
    /// noted, unless its in-between frames are blended.
    #[test]
    fn a_juddering_retime_is_noted() {
        assert_eq!(judders(16.0, 0.6, 30.0).map(|n| (n * 10.0).round()), Some(96.0));
        assert_eq!(judders(30.0, 1.0, 30.0), None);
        assert_eq!(judders(29.97, 1.0, 30.0), None, "a rounding of the rate is no judder");
        assert_eq!(judders(24.0, 1.0, 25.0), None, "the usual conform");
        assert!(judders(24.0, 1.0, 30.0).is_some());
        assert_eq!(judders(60.0, 0.5, 30.0), None, "high-rate slow motion is smooth");
        let (mut seq, pool, talk, _) = piece();
        let v = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
        seq.tracks[v].clips[0].speed = 0.5;
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(rules(&r).contains(&"judder"), "{}", r.text());
        seq.tracks[v].clips[0] = klipp_timeline::test_clip::TestClip::of(seq.tracks[v].clips[0].clone()).effect(klipp_effects::BuiltinEffect::SpeedRamp { speed: 1.0, blend_mode: 2 }).build();
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(!rules(&r).contains(&"judder"), "blended, it does not: {}", r.text());
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

    /// #457: speech under the music is where words are said, not a whole
    /// clip with one word whisper was unsure of in it; and music split
    /// around the speech, the piece under it set lower, is ducked.
    #[test]
    fn music_under_speech_is_judged_by_the_words() {
        struct Said(klipp_core::MediaId, Vec<(f64, f64, f32)>);
        impl Transcripts for Said {
            fn words(&self, item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
                Some(if item.id == self.0 { self.1.iter().map(|&(a, b, confidence)| SpokenWord { start_secs: a, end_secs: b, text: " w".into(), confidence }).collect() } else { Vec::new() })
            }
        }
        let (mut seq, pool, talk, music) = piece();
        let bed = |pieces: &[(f64, f64, f32)]| {
            let mut t = Track::new("A2", TrackKind::Audio);
            t.clips = pieces.iter().map(|&(a, b, db)| {
                let mut c = Clip::media(music, tc(a), tc(b), tc(a), tc(b));
                c.volume = crate::ops::db_to_linear(db);
                c
            }).collect();
            t
        };
        let level = |seq: &Sequence, said: &Said| rules(&check(seq, &pool, said)).contains(&"music_level");
        // One word, unsure, in street sound: no speech, nothing to duck.
        seq.tracks.push(bed(&[(0.0, 8.0, 0.0)]));
        assert!(!level(&seq, &Said(talk, vec![(6.0, 6.2, 0.4)])));
        // A phrase at 1-2 s under the full bed is said.
        let phrase = Said(talk, vec![(1.0, 1.5, 0.9), (1.6, 2.0, 0.9), (6.0, 6.2, 0.4)]);
        assert!(level(&seq, &phrase));
        // Split at 3 s and the piece under the phrase set 7 dB down: ducked.
        seq.tracks.pop();
        seq.tracks.push(bed(&[(0.0, 3.0, -7.0), (3.0, 8.0, 0.0)]));
        assert!(!level(&seq, &phrase), "{}", check(&seq, &pool, &phrase).text());
        // Split but left level: still said.
        seq.tracks.pop();
        seq.tracks.push(bed(&[(0.0, 3.0, 0.0), (3.0, 8.0, 0.0)]));
        assert!(level(&seq, &phrase));
    }

    /// #777: a take whose wind was transcribed as words, marked ambience
    /// on its picture or on its sound, is not speech: no duck asked of
    /// the bed over it, and one note saying its words were set aside.
    #[test]
    fn a_take_marked_ambience_is_not_speech_whichever_side_carries_the_role() {
        let (mut seq, pool, talk, music) = piece();
        let mut bed = Track::new("A2", TrackKind::Audio);
        bed.clips = vec![Clip::media(music, tc(0.0), tc(8.0), tc(0.0), tc(8.0))];
        seq.tracks.push(bed);
        assert!(rules(&check(&seq, &pool, &Spoken(talk))).contains(&"music_level"), "unmarked, the take is speech");
        for side in [0, 1] {
            let mut marked = seq.clone();
            for c in marked.tracks[side].clips.iter_mut() {
                c.role = klipp_core::ClipRole::Ambience;
            }
            let found = rules(&check(&marked, &pool, &Spoken(talk)));
            assert!(!found.contains(&"music_level"), "role on track {side}: {found:?}");
            assert_eq!(found.iter().filter(|r| **r == "words_not_speech").count(), 1, "role on track {side}: {found:?}");
        }
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

    /// #1581: a cream title is hard to read over white surf and easy over
    /// a dark sea; a shadow makes it a note; a backing or a card takes it
    /// out of the reading.
    #[test]
    fn a_title_hard_to_read_against_the_picture_is_a_warning() {
        let (mut seq, pool, _) = screencast(&[(0.0, 10.0, 40.0)]);
        let cream = [0xf2, 0xe8, 0xd0, 255];
        let title = |shadow: bool, background: klipp_timeline::TitleBackground| {
            let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Chapter".into(), font_size: 140.0, color: cream, ..Default::default() });
            td.background = background;
            if shadow {
                td.objects[0].fx_mut().shadows = vec![klipp_timeline::TitleShadow { enabled: true, color: [0, 0, 0, 160], offset_x: 0.0, offset_x_keyframes: Vec::new(), offset_y: 4.0, offset_y_keyframes: Vec::new(), blur: 8.0, blur_keyframes: Vec::new() }];
            }
            td
        };
        let mut text = Track::new("T", TrackKind::Text);
        let at = |a: f64, td| Clip { id: klipp_core::ClipId::new(), kind: ClipKind::Title(td), source_in: tc(0.0), source_out: tc(2.0), ..Clip::gap(tc(a), tc(a + 2.0)) };
        text.clips.push(at(0.0, title(false, klipp_timeline::TitleBackground::None)));
        text.clips.push(at(2.0, title(true, klipp_timeline::TitleBackground::None)));
        text.clips.push(at(4.0, title(false, klipp_timeline::TitleBackground::Color { color: [0, 0, 0, 200], padding: 8.0 })));
        text.clips.push(at(6.0, title(false, klipp_timeline::TitleBackground::FullFrame { color: [16, 20, 24, 255] })));
        seq.tracks.insert(0, text);
        let to_read = titles_to_read(&seq, &pool, 16);
        assert_eq!(to_read.iter().map(|t| (t.at_secs, t.helped)).collect::<Vec<_>>(), vec![(1.0, false), (3.0, true)], "the backed title and the card aren't read off the picture");
        let picture = |rgb: [u8; 3]| (0..64 * 36).flat_map(|_| [rgb[0], rgb[1], rgb[2], 255]).collect::<Vec<u8>>();
        let surf = picture([235, 240, 242]);
        let sea = picture([20, 40, 60]);
        let plain = legibility(&to_read[0], &surf, 64, 36).expect("cream over surf is hard to read");
        assert_eq!((plain.severity, plain.rule), (Severity::Warning, "title_hard_to_read"));
        assert!(plain.hint.contains("darker"), "{}", plain.hint);
        assert_eq!(legibility(&to_read[1], &surf, 64, 36).map(|f| f.severity), Some(Severity::Note), "a shadow may carry it");
        assert_eq!(legibility(&to_read[0], &sea, 64, 36), None, "cream over a dark sea reads");
    }
}
