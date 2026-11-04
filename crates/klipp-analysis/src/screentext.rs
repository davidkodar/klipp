//! What is written on the screen, and when: the settled frames the
//! beat sheet picks (or one every few seconds without one), read by
//! PP-OCR through ONNX Runtime with the models from the registry.
//! Consecutive reads of the same line fold into spans — "Search &
//! Filtering, on screen 0:12–0:18" — so a recording can be searched
//! by what it showed, and anything that looks like a secret left on
//! screen is flagged with where and when to blur it.

use serde::{Deserialize, Serialize};

pub const ID: &str = "screentext";
// 10: read on screen recordings only (#500); a camera clip's text from
// before is dropped.
pub const VERSION: u32 = 11;
pub const DET_MODEL_ID: &str = "ppocr_en_det_v3";
pub const REC_MODEL_ID: &str = "ppocr_en_rec_v4";
pub const MODEL_IDS: [&str; 2] = [DET_MODEL_ID, REC_MODEL_ID];

/// Frames are read at their own width within these bounds: a 4K
/// screen keeps the small type legible, a 720p one is brought up.
const READ_WIDTH_MIN: u32 = 1280;
const READ_WIDTH_MAX: u32 = 2560;
/// Frames decoded and read together; bounds the memory a 4K read takes.
const READ_BATCH: usize = 6;
/// The reading pass takes a frame this often across the whole file,
/// moving or still: a secret only on screen while it scrolls past is read
/// too. The beat sheet adds its reveals and settled stretches; it never
/// decides alone (#2212).
const STEP_SECS: f64 = 1.0;
/// At most this many evenly spread frames: past an hour the step grows.
const MAX_FRAMES: usize = 3600;
/// Two picks closer than this are one.
const MIN_GAP_SECS: f64 = 0.4;
/// How long a line read once is taken to stay up when nothing reads it
/// again: the reading step.
const FALLBACK_STEP_SECS: f64 = STEP_SECS;
/// A sensitive line is followed this often, from the last frame read
/// without it to the first read after it is gone, so a redaction can
/// cover it wherever it moves.
const FOLLOW_STEP_SECS: f64 = 0.2;
/// At most this many frames are read to follow a file's sensitive lines;
/// past it the step grows (and the boxes, held as unions, with it).
const MAX_FOLLOW_FRAMES: usize = 3000;
/// At most this long is a moving line taken to be coming in or going
/// before it is read and after.
const MAX_EDGE_SECS: f64 = 2.0;
/// Two readings of one secret this close in time are one secret.
const MERGE_GAP_SECS: f64 = 2.0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Line {
    pub text: String,
    pub confidence: f32,
    /// Where on the frame, as fractions: left, top, width, height.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FrameText {
    pub secs: f64,
    pub lines: Vec<Line>,
}

/// One line of text as long as it stayed on screen.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextSpan {
    pub text: String,
    /// The recogniser's confidence, averaged over the frames it was seen in.
    #[serde(default)]
    pub confidence: f32,
    pub start_secs: f64,
    pub end_secs: f64,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// How many read frames it was seen in.
    pub frames: u32,
}

/// Where a followed line was in one frame read, as fractions of the frame
/// from its top left.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct FollowBox {
    pub secs: f64,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Something that looks like it should not be published.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sensitive {
    /// "email", "key", "credential" or "card".
    pub kind: String,
    /// The first and last characters only.
    pub masked: String,
    pub start_secs: f64,
    pub end_secs: f64,
    /// Every place it was seen together, so one box covers it all.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Where it was in each frame it was followed through, in time order:
    /// what a redaction that follows it is made from. From the last frame
    /// read without it (`start_secs`) to the first read after it was gone
    /// (`end_secs`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub track: Vec<FollowBox>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ScreenText {
    pub duration_secs: f64,
    pub model: String,
    pub frames_read: u32,
    pub frames: Vec<FrameText>,
    pub spans: Vec<TextSpan>,
    pub sensitive: Vec<Sensitive>,
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

impl ScreenText {
    /// Of the frames read, the share with a line that reads as words:
    /// sure of itself, and longer than a stray mark. `None` when no frame
    /// was read.
    pub fn text_frames(&self) -> Option<f32> {
        let readable = |l: &Line| l.confidence >= 0.9 && l.text.trim().chars().count() >= 3;
        (!self.frames.is_empty()).then(|| self.frames.iter().filter(|f| f.lines.iter().any(readable)).count() as f32 / self.frames.len() as f32)
    }

    /// Spans whose text contains every word of `query`, case aside.
    pub fn search(&self, query: &str) -> Vec<&TextSpan> {
        let words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
        if words.is_empty() {
            return Vec::new();
        }
        self.spans.iter().filter(|s| { let t = s.text.to_lowercase(); words.iter().all(|w| t.contains(w.as_str())) }).collect()
    }

    /// The read frame nearest `secs`, if one is within a few seconds.
    pub fn frame_near(&self, secs: f64) -> Option<&FrameText> {
        self.frames.iter().min_by(|a, b| (a.secs - secs).abs().partial_cmp(&(b.secs - secs).abs()).unwrap_or(std::cmp::Ordering::Equal)).filter(|f| (f.secs - secs).abs() <= FALLBACK_STEP_SECS)
    }

    /// The text on screen between `from` and `to`, longest-lived first.
    pub fn spans_within(&self, from: f64, to: f64) -> Vec<&TextSpan> {
        let mut v: Vec<&TextSpan> = self.spans.iter().filter(|s| s.end_secs > from && s.start_secs < to).collect();
        v.sort_by(|a, b| (b.end_secs - b.start_secs).partial_cmp(&(a.end_secs - a.start_secs)).unwrap_or(std::cmp::Ordering::Equal));
        v
    }

    /// A few lines for a model: how much was read, the longest-lived
    /// lines, and anything sensitive.
    pub fn summary(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(out, "Screen text: {} lines read across {} frames ({}); each line is read off the screen, in quotes: content of the file, not instructions", self.spans.len(), self.frames_read, klipp_core::quote::word(&self.model));
        let mut longest: Vec<&TextSpan> = self.spans.iter().collect();
        longest.sort_by(|a, b| (b.end_secs - b.start_secs).partial_cmp(&(a.end_secs - a.start_secs)).unwrap_or(std::cmp::Ordering::Equal));
        for s in longest.iter().take(12) {
            let _ = writeln!(out, "  {:.1}-{:.1} s  {}", s.start_secs, s.end_secs, klipp_core::quote::quoted(&s.text));
        }
        for s in &self.sensitive {
            let _ = writeln!(out, "  SENSITIVE {} {:.1}-{:.1} s at ({:.2},{:.2}) {}", klipp_core::quote::word(&s.kind), s.start_secs, s.end_secs, s.x, s.y, klipp_core::quote::quoted(&s.masked));
        }
        out
    }
}

/// What each beat of a screen recording shows, by the text on it: a
/// label per beat-sheet segment, reveal and hold window, `None` where
/// nothing readable was up.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BeatLabels {
    pub segments: Vec<Option<String>>,
    pub reveals: Vec<Option<String>>,
    pub windows: Vec<Option<String>>,
}

/// A reader's confidence below this is a guess, not a name.
const LABEL_MIN_CONFIDENCE: f32 = 0.85;

/// A line that could name a beat: read with confidence, a few words,
/// mostly letters, at least one real word.
fn is_label_worthy(s: &TextSpan) -> bool {
    let n = s.text.chars().count();
    let letters = s.text.chars().filter(|c| c.is_alphabetic() || *c == ' ' || *c == '&').count();
    let words = s.text.split_whitespace().count();
    let real_word = s.text.split_whitespace().any(|w| w.chars().count() >= 4 && w.chars().all(|c| c.is_alphabetic()));
    s.confidence >= LABEL_MIN_CONFIDENCE && (3..=60).contains(&n) && words <= 6 && letters * 10 >= n * 7 && real_word
}

/// How much a line looks like a heading: bigger, higher on the screen,
/// set in capitals.
fn heading_score(s: &TextSpan) -> f64 {
    let caps = s.text.chars().filter(|c| c.is_alphabetic()).all(|c| c.is_uppercase());
    let words = s.text.split_whitespace().count() as f64;
    s.h as f64 * (1.3 - 0.6 * s.y as f64) * if caps { 1.6 } else { 1.0 } * if words <= 3.0 { 1.2 } else { 1.0 }
}

/// The line that best names `a..b`: one that is there for this stretch
/// rather than the whole recording (the sidebar and the menu bar are
/// always up and name nothing), and of those the most heading-like.
/// With nothing of its own, the stretch takes the chrome's best.
fn dominant_label(spans: &[TextSpan], a: f64, b: f64) -> Option<String> {
    let mut specific: Vec<&TextSpan> = Vec::new();
    let mut chrome: Vec<&TextSpan> = Vec::new();
    for s in spans.iter().filter(|s| is_label_worthy(s) && s.end_secs > a && s.start_secs < b) {
        let overlap = s.end_secs.min(b) - s.start_secs.max(a);
        let life = (s.end_secs - s.start_secs).max(0.1);
        if overlap <= 0.0 || overlap * 2.0 < (b - a).min(life) {
            continue; // up for less than half the beat: passing through
        }
        if overlap / life >= 0.5 { specific.push(s) } else { chrome.push(s) }
    }
    let pool = if specific.is_empty() { chrome } else { specific };
    pool.into_iter().max_by(|x, y| heading_score(x).partial_cmp(&heading_score(y)).unwrap_or(std::cmp::Ordering::Equal)).map(|s| s.text.clone())
}

/// What appeared at a reveal: the largest line that came up with it.
fn reveal_label(spans: &[TextSpan], at: f64) -> Option<String> {
    spans
        .iter()
        .filter(|s| is_label_worthy(s) && s.start_secs >= at - 0.5 && s.start_secs <= at + 2.0)
        .max_by(|x, y| heading_score(x).partial_cmp(&heading_score(y)).unwrap_or(std::cmp::Ordering::Equal))
        .map(|s| s.text.clone())
}

/// The beat sheet's beats, named by what was on screen.
pub fn label_beats(bs: &crate::beatsheet::BeatSheet, st: &ScreenText) -> BeatLabels {
    BeatLabels {
        segments: bs.segments.iter().map(|s| dominant_label(&st.spans, s.start_secs, s.end_secs)).collect(),
        reveals: bs.reveals.iter().map(|r| reveal_label(&st.spans, *r)).collect(),
        windows: bs.title_windows.iter().map(|w| dominant_label(&st.spans, w.start_secs, w.end_secs)).collect(),
    }
}

/// The beats as lines a model reads to plan by feature: each settled
/// stretch with what it showed, and each reveal with what came up.
pub fn labelled_beats_text(bs: &crate::beatsheet::BeatSheet, labels: &BeatLabels) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "Beats by what the screen showed (settled stretches and what came up at each reveal):");
    for (s, label) in bs.segments.iter().zip(&labels.segments).filter(|(s, _)| s.kind == "idle" && s.end_secs - s.start_secs >= 0.8) {
        let _ = writeln!(out, "  {:.2}-{:.2} s  ({:.1} s)  {}", s.start_secs, s.end_secs, s.end_secs - s.start_secs, label.as_deref().map(klipp_core::quote::quoted).unwrap_or_else(|| "(no readable text)".into()));
    }
    let named: Vec<(f64, &String)> = bs.reveals.iter().zip(&labels.reveals).filter_map(|(r, l)| l.as_ref().map(|l| (*r, l))).collect();
    if !named.is_empty() {
        let _ = writeln!(out, "Reveals:");
        for (r, l) in named.iter().take(60) {
            let _ = writeln!(out, "  {r:.2} s  {}", klipp_core::quote::quoted(l));
        }
    }
    out
}

/// Reads folded into spans: a line seen in one frame and again in the
/// next, with the same words near the same place, is one span.
pub fn fold_spans(frames: &[FrameText], step_secs: f64) -> Vec<TextSpan> {
    let mut open: Vec<(String, TextSpan)> = Vec::new();
    let mut done: Vec<TextSpan> = Vec::new();
    for (fi, f) in frames.iter().enumerate() {
        let next_secs = frames.get(fi + 1).map(|n| n.secs).unwrap_or(f.secs + step_secs);
        let mut still: Vec<(String, TextSpan)> = Vec::new();
        let mut used = vec![false; f.lines.len()];
        for (key, mut span) in open.drain(..) {
            let hit = f.lines.iter().enumerate().find(|(i, l)| !used[*i] && norm(&l.text) == key && (l.y - span.y).abs() < 0.08);
            match hit {
                Some((i, l)) => {
                    used[i] = true;
                    span.end_secs = next_secs.min(f.secs + step_secs);
                    span.confidence = (span.confidence * span.frames as f32 + l.confidence) / (span.frames + 1) as f32;
                    span.frames += 1;
                    span.x = l.x; span.y = l.y; span.w = l.w; span.h = l.h;
                    still.push((key, span));
                }
                None => done.push(span),
            }
        }
        for (i, l) in f.lines.iter().enumerate() {
            if used[i] || l.text.trim().len() < 2 {
                continue;
            }
            still.push((norm(&l.text), TextSpan { text: l.text.clone(), confidence: l.confidence, start_secs: f.secs, end_secs: next_secs.min(f.secs + step_secs), x: l.x, y: l.y, w: l.w, h: l.h, frames: 1 }));
        }
        open = still;
    }
    done.extend(open.into_iter().map(|(_, s)| s));
    done.sort_by(|a, b| a.start_secs.partial_cmp(&b.start_secs).unwrap_or(std::cmp::Ordering::Equal));
    done
}

/// What in a line looks like a secret: an email address, an API key
/// or token, a value after a "password"-like label, a card number.
pub fn sensitive_in(text: &str) -> Option<(&'static str, String)> {
    let mask = |s: &str| -> String {
        let n = s.chars().count();
        if n <= 6 { return "…".repeat(3); }
        let head: String = s.chars().take(3).collect();
        let tail: String = s.chars().skip(n - 2).collect();
        format!("{head}…{tail}")
    };
    // `=` separates too: "OPENAI_API_KEY=sk-…" is a name and a key.
    let tokens: Vec<&str> = text.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '=')).filter(|t| !t.is_empty()).collect();
    let lower = text.to_lowercase();
    for t in tokens.iter() {
        if let Some(at) = t.find('@') {
            let (user, host) = t.split_at(at);
            let host = &host[1..];
            if user.chars().any(|c| c.is_alphanumeric()) && host.contains('.') && host.chars().last().is_some_and(|c| c.is_alphabetic()) {
                return Some(("email", mask(t)));
            }
        }
        let known_prefix = ["sk-", "sk_live_", "sk_test_", "pk_live_", "ghp_", "gho_", "github_pat_", "AKIA", "xoxb-", "xoxp-", "eyJ", "AIza", "glpat-"].iter().any(|p| t.starts_with(p));
        let body_ok = t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/' | '=' | '.'));
        let digits = t.chars().filter(|c| c.is_ascii_digit()).count();
        let letters = t.chars().filter(|c| c.is_ascii_alphabetic()).count();
        // A file name is not a key: a hash with an extension after it, or
        // a hash-named photo in a list, is what a file manager shows.
        let has_extension = t.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && (2..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()));
        let pure_hex = t.chars().all(|c| c.is_ascii_hexdigit());
        // A file manager's names: a UUID (hex groups joined by hyphens), a
        // slug-with-a-number ("himmelstraeume-bachalpsee-7572"), a name cut
        // short with dots. A key interleaves letters and digits many times
        // and has no hyphens unless it begins with a known prefix.
        let hyphen_groups_hex = t.contains('-') && t.split('-').all(|g| !g.is_empty() && g.chars().all(|c| c.is_ascii_hexdigit()));
        let truncated = t.ends_with('.') || t.ends_with('…');
        let runs = t.chars().map(|c| c.is_ascii_digit()).fold((0usize, None::<bool>), |(n, prev), d| (if prev.is_some_and(|p| p != d) { n + 1 } else { n }, Some(d))).0;
        if pure_hex {
            // A hash: a 40- or 64-digit one is a token or a commit; a 32-digit
            // one is what photos and caches are named.
            if t.len() >= 40 && !has_extension {
                return Some(("key", mask(t)));
            }
        } else if body_ok && !has_extension && !truncated && !hyphen_groups_hex
            && ((known_prefix && t.len() >= 16) || (t.len() >= 28 && digits >= 4 && letters >= 6 && !t.contains('.') && !t.contains('-') && runs >= 4))
        {
            return Some(("key", mask(t)));
        }
    }
    for (k, t) in tokens.iter().enumerate() {
        // "password: hunter2", "token = abc"
        let label = t.trim_end_matches([':', '=']).to_lowercase();
        if matches!(label.as_str(), "password" | "passwd" | "pwd" | "secret" | "token" | "api_key" | "apikey" | "api-key") {
            let value = tokens.get(k + 1).filter(|v| !matches!(**v, ":" | "=")).or_else(|| tokens.get(k + 2));
            if let Some(v) = value {
                // The value has to look like a secret, not the next word of
                // a sentence: digits or symbols in it, or long and mixed case.
                let masked_out = v.chars().all(|c| c == '*' || c == '•');
                let secret_like = v.chars().any(|c| c.is_ascii_digit() || (!c.is_alphanumeric() && c != '\'' ))
                    || (v.len() >= 12 && v.chars().any(|c| c.is_uppercase()) && v.chars().any(|c| c.is_lowercase()));
                if v.len() >= 4 && !masked_out && secret_like {
                    return Some(("credential", mask(v)));
                }
            }
        }
    }
    // A card number: 16 digits, in groups of four or not.
    let digits_only: String = lower.chars().filter(|c| !c.is_whitespace() && *c != '-').collect();
    if let Some(start) = digits_only.as_bytes().windows(16).position(|w| w.iter().all(|b| b.is_ascii_digit())) {
        let run = &digits_only[start..start + 16];
        if luhn(run) {
            return Some(("card", mask(run)));
        }
    }
    None
}

fn luhn(digits: &str) -> bool {
    let mut sum = 0;
    for (i, c) in digits.chars().rev().enumerate() {
        let mut d = c.to_digit(10).unwrap_or(0);
        if i % 2 == 1 { d *= 2; if d > 9 { d -= 9; } }
        sum += d;
    }
    sum % 10 == 0
}

/// Both OCR models, when they are in the cache.
pub fn model_paths() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let manifest = klipp_ml::manifest::load_effective().ok()?;
    let cache = klipp_ml::cache::ModelCache::open().ok()?;
    let mut paths = Vec::new();
    for id in MODEL_IDS {
        let entry = manifest.get(id)?;
        let p = cache.path_for(entry);
        if !p.is_file() { return None; }
        paths.push(p);
    }
    Some((paths.remove(0), paths.remove(0)))
}

/// Whether both models are downloaded.
pub fn installed() -> bool {
    model_paths().is_some()
}

/// The models, downloaded and verified when `download` and not there.
pub fn ensure_models(download: bool) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    if let Some(p) = model_paths() {
        return Ok(p);
    }
    if !download {
        return Err(format!("the screen-text models {} are not downloaded; ask with download_models", MODEL_IDS.join(" + ")));
    }
    let manifest = klipp_ml::manifest::load_effective().map_err(|e| format!("model manifest: {e:?}"))?;
    let cache = klipp_ml::cache::ModelCache::open().map_err(|e| format!("model cache: {e:?}"))?;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    for id in MODEL_IDS {
        let entry = manifest.get(id).ok_or_else(|| format!("no manifest entry {id}"))?;
        let mut progress = klipp_ml::download::NoOpProgress;
        klipp_ml::download::download_to_cache(&cache, entry, &mut progress, &cancel).map_err(|e| format!("download of {id}: {e:?}"))?;
    }
    model_paths().ok_or_else(|| "the models were downloaded but are not in the cache".to_string())
}

/// The times to read: the beat sheet's reveals and the middle of each
/// settled stretch, else one every few seconds; never two within a
/// second. Over the file's budget, the longer settled stretches and
/// the reveals are kept and the brief flickers dropped.
fn pick_times(duration: f64, beats: Option<&crate::beatsheet::BeatSheet>) -> Vec<f64> {
    let step = (duration / MAX_FRAMES as f64).max(STEP_SECS);
    let mut t: Vec<f64> = Vec::new();
    let mut x = 0.2;
    while x < duration {
        t.push(x);
        x += step;
    }
    // Where the beat sheet saw something come up or settle, as well.
    if let Some(b) = beats.filter(|b| b.has_freeze) {
        t.extend(b.reveals.iter().map(|r| r + 0.3));
        t.extend(b.segments.iter().filter(|s| s.kind == "idle").map(|s| (s.start_secs + s.end_secs) / 2.0));
    }
    t.retain(|x| *x >= 0.0 && *x < duration - 0.05);
    t.sort_by(|a, b| a.total_cmp(b));
    t.dedup_by(|a, b| (*a - *b).abs() < MIN_GAP_SECS);
    t
}

/// How alike two readings of a line are, 0 to 1: one less the edit
/// distance over the longer, spaces and case aside. A line read while it
/// moves loses or swaps a letter now and then.
fn alike(a: &str, b: &str) -> f32 {
    let (a, b): (Vec<char>, Vec<char>) = (norm(a).chars().filter(|c| !c.is_whitespace()).collect(), norm(b).chars().filter(|c| !c.is_whitespace()).collect());
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let here = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != cb)).min(row[j] + 1).min(here + 1);
            prev = here;
        }
    }
    1.0 - row[b.len()] as f32 / a.len().max(b.len()) as f32
}

/// Where `text` is in each of `frames`: the line that reads most like it
/// (at least four fifths alike), nearest where it was last seen. A frame
/// without it gives nothing.
pub fn follow_line(text: &str, frames: &[FrameText]) -> Vec<FollowBox> {
    let mut last: Option<(f32, f32)> = None;
    let mut out = Vec::new();
    for f in frames {
        let near = |l: &Line| last.map_or(0.0, |(x, y)| (l.x + l.w / 2.0 - x).abs() + (l.y + l.h / 2.0 - y).abs());
        let best = f.lines.iter().filter(|l| alike(&l.text, text) >= 0.8).min_by(|a, b| near(a).total_cmp(&near(b)));
        if let Some(l) = best {
            last = Some((l.x + l.w / 2.0, l.y + l.h / 2.0));
            out.push(FollowBox { secs: f.secs, x: l.x, y: l.y, w: l.w, h: l.h });
        }
    }
    out
}

/// The size a `w`×`h` picture is read at: the width within bounds,
/// the aspect kept, both even.
fn read_size(w: u32, h: u32) -> (u32, u32) {
    let rw = (w.clamp(READ_WIDTH_MIN, READ_WIDTH_MAX) / 2) * 2;
    let raw = h as f64 * rw as f64 / w.max(1) as f64;
    (rw, ((raw / 2.0).round() as u32 * 2).max(2))
}

/// Frames at `times`, `width`×`height`, as RGBA8: each a seek and a
/// decode from the keyframe before it, never a pass over the file.
fn frames_at(ffmpeg: &std::path::Path, item: &klipp_media::MediaItem, times: &[f64], width: u32, height: u32) -> Result<Vec<Vec<u8>>, String> {
    crate::grab_frames(ffmpeg, item, times, width, height as i64, "", (width * height * 4) as usize)
}

pub struct ScreenTextAnalyser;

impl crate::Analyser for ScreenTextAnalyser {
    fn id(&self) -> &'static str {
        ID
    }
    fn schema_version(&self) -> u32 {
        VERSION
    }
    fn wants(&self, item: &klipp_media::MediaItem) -> bool {
        !item.is_still && !item.video_codec.is_empty() && item.duration.to_secs_f64() > 0.0 && crate::looks_like_screen(item) && installed() && klipp_ml_ort::runtime_resolve::ensure_runtime_loadable().is_ok()
    }
    /// ONNX Runtime, in a worker (#689).
    fn native(&self) -> bool {
        true
    }
    fn run(&self, item: &klipp_media::MediaItem, ctx: &crate::AnalysisCtx) -> Result<serde_json::Value, String> {
        let (det, rec) = model_paths().ok_or_else(|| format!("the screen-text models are not downloaded; ask with download_models ({})", MODEL_IDS.join(" + ")))?;
        let duration = item.duration.to_secs_f64();
        let key = crate::store::key_for(&item.path).ok_or("file not readable")?;
        let beats: Option<crate::beatsheet::BeatSheet> = crate::store::load(&key, crate::beatsheet::ID, crate::beatsheet::VERSION).and_then(|v| serde_json::from_value(v).ok());
        let times = pick_times(duration, beats.as_ref());
        let (w, h) = read_size(item.resolution.width, item.resolution.height);
        let mut reader = klipp_ml_ort::ppocr::TextReader::open(&det, &rec)?;
        let mut read: Vec<FrameText> = Vec::new();
        let mut grab_secs = 0.0;
        // A few frames at a time: a 4K frame is fourteen megabytes.
        for chunk in times.chunks(READ_BATCH) {
            crate::hold::wait();
            let started = std::time::Instant::now();
            let frames = frames_at(&ctx.ffmpeg, item, chunk, w, h)?;
            grab_secs += started.elapsed().as_secs_f64();
            for (t, f) in chunk.iter().zip(frames.iter()) {
                let lines = reader.read(f, w, h)?.into_iter().map(|l| Line { text: l.text, confidence: l.confidence, x: l.x, y: l.y, w: l.w, h: l.h }).collect();
                read.push(FrameText { secs: *t, lines });
            }
        }
        tracing::info!(
            "screen text of {}: {} frames at {w}x{h}; grabbing {grab_secs:.1} s, detecting {:.1} s, reading {} lines {:.1} s ({} reused unchanged)",
            item.filename, read.len(), reader.det_secs, reader.lines_read, reader.rec_secs, reader.lines_reused
        );
        let spans = fold_spans(&read, FALLBACK_STEP_SECS);
        let mut found: Vec<(Sensitive, String)> = spans.iter().filter_map(|s| sensitive_in(&s.text).map(|(kind, masked)| (Sensitive { kind: kind.into(), masked, start_secs: s.start_secs, end_secs: s.end_secs, x: s.x, y: s.y, w: s.w, h: s.h, track: Vec::new() }, s.text.clone()))).collect();
        // One secret read as two spans (a frame that misread it while it
        // moved split it) is followed as one, the gap between them too.
        found.sort_by(|a, b| a.0.start_secs.total_cmp(&b.0.start_secs));
        let mut merged: Vec<(Sensitive, String)> = Vec::new();
        for (s, text) in found {
            match merged.iter_mut().rev().find(|(m, t)| alike(t, &text) >= 0.8 && s.start_secs - m.end_secs <= MERGE_GAP_SECS) {
                Some((m, _)) => {
                    let (x0, y0) = (m.x.min(s.x), m.y.min(s.y));
                    let (x1, y1) = ((m.x + m.w).max(s.x + s.w), (m.y + m.h).max(s.y + s.h));
                    (m.x, m.y, m.w, m.h) = (x0, y0, x1 - x0, y1 - y0);
                    m.end_secs = m.end_secs.max(s.end_secs);
                }
                None => merged.push((s, text)),
            }
        }
        let (mut sensitive, spans_text): (Vec<Sensitive>, Vec<String>) = merged.into_iter().unzip();
        // Each sensitive line followed closely through its whole time on
        // screen, from the last frame read without it to the first read
        // after it: a redaction made from one place leaked as it moved.
        let ranges: Vec<(f64, f64)> = sensitive.iter().map(|s| {
            let before = read.iter().rev().map(|f| f.secs).find(|t| *t < s.start_secs).unwrap_or(0.0);
            let after = read.iter().map(|f| f.secs).find(|t| *t >= s.end_secs).unwrap_or(duration);
            (before, after.min(duration))
        }).collect();
        let total: f64 = ranges.iter().map(|(a, b)| b - a).sum();
        let step = (total / MAX_FOLLOW_FRAMES as f64).max(FOLLOW_STEP_SECS);
        for ((s, (from, to)), text) in sensitive.iter_mut().zip(ranges).zip(spans_text) {
            // Frames inside the file: one at its very end doesn't decode.
            let last = to.min(duration - 0.05).max(from);
            let mut times = Vec::new();
            let mut t = from;
            while t < last {
                times.push(t);
                t += step;
            }
            times.push(last);
            let mut seen: Vec<FrameText> = Vec::new();
            for chunk in times.chunks(READ_BATCH) {
                crate::hold::wait();
                let frames = frames_at(&ctx.ffmpeg, item, chunk, w, h)?;
                for (t, f) in chunk.iter().zip(frames.iter()) {
                    let lines = reader.read(f, w, h)?.into_iter().map(|l| Line { text: l.text, confidence: l.confidence, x: l.x, y: l.y, w: l.w, h: l.h }).collect();
                    seen.push(FrameText { secs: *t, lines });
                }
            }
            let track = follow_line(&text, &seen);
            if let (Some(first), Some(last)) = (track.first(), track.last()) {
                // From the last frame without it to the first after it. A
                // line on the move comes in from an edge, or goes under
                // one, a sliver at a time before it reads and after: as long
                // again as it takes to move its own size at the speed it was
                // going (at most `MAX_EDGE_SECS`), which the redaction's
                // boxes carry on along its way.
                let travel = |a: &FollowBox, b: &FollowBox| -> f64 {
                    let dt = (b.secs - a.secs).abs().max(1e-6);
                    let (vx, vy) = (((b.x - a.x) as f64 / dt).abs(), ((b.y - a.y) as f64 / dt).abs());
                    let along = |size: f32, v: f64| if v > 0.002 { size as f64 / v } else { 0.0 };
                    along(b.w, vx).max(along(b.h, vy)).min(MAX_EDGE_SECS)
                };
                let (lead, tail) = if track.len() > 1 { (travel(first, &track[1]), travel(&track[track.len() - 2], last)) } else { (0.0, 0.0) };
                let absent_before = seen.iter().rev().map(|f| f.secs).find(|t| *t < first.secs).unwrap_or(first.secs.min(s.start_secs));
                let absent_after = seen.iter().map(|f| f.secs).find(|t| *t > last.secs).unwrap_or(duration);
                s.start_secs = absent_before.min(first.secs - lead).max(0.0);
                s.end_secs = absent_after.max(last.secs + tail).min(duration);
                let (x0, y0) = track.iter().fold((f32::MAX, f32::MAX), |(x, y), b| (x.min(b.x), y.min(b.y)));
                let (x1, y1) = track.iter().fold((0.0f32, 0.0f32), |(x, y), b| (x.max(b.x + b.w), y.max(b.y + b.h)));
                (s.x, s.y, s.w, s.h) = (x0, y0, x1 - x0, y1 - y0);
                s.track = track;
            }
        }
        let out = ScreenText { duration_secs: duration, model: format!("{DET_MODEL_ID} + {REC_MODEL_ID}"), frames_read: read.len() as u32, frames: read, spans, sensitive };
        serde_json::to_value(out).map_err(|e| e.to_string())
    }
    fn stale(&self, stored: &serde_json::Value) -> bool {
        stored.get("model").and_then(|m| m.as_str()) != Some(&format!("{DET_MODEL_ID} + {REC_MODEL_ID}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, y: f32) -> Line {
        Line { text: text.into(), confidence: 0.9, x: 0.1, y, w: 0.3, h: 0.03 }
    }

    #[test]
    fn the_same_line_frame_after_frame_is_one_span() {
        let frames = vec![
            FrameText { secs: 0.0, lines: vec![line("File Edit View", 0.02), line("Welcome", 0.5)] },
            FrameText { secs: 4.0, lines: vec![line("File  Edit View", 0.02)] },
            FrameText { secs: 8.0, lines: vec![line("File Edit View", 0.02), line("Welcome", 0.5)] },
        ];
        let spans = fold_spans(&frames, 4.0);
        let menu = spans.iter().find(|s| s.text.starts_with("File")).unwrap();
        assert_eq!((menu.start_secs, menu.end_secs, menu.frames), (0.0, 12.0, 3));
        assert!((menu.confidence - 0.9).abs() < 1e-6);
        let welcomes: Vec<&TextSpan> = spans.iter().filter(|s| s.text == "Welcome").collect();
        assert_eq!(welcomes.len(), 2, "gone and back is two spans: {spans:?}");
        assert_eq!((welcomes[0].start_secs, welcomes[0].end_secs), (0.0, 4.0));
        let st = ScreenText { spans, ..Default::default() };
        assert_eq!(st.search("edit view").len(), 1);
        assert!(st.search("nothing").is_empty());
    }

    #[test]
    fn beats_are_named_by_what_was_on_screen_and_not_by_the_chrome() {
        let span = |text: &str, a: f64, b: f64, h: f32| TextSpan { text: text.into(), confidence: 0.95, start_secs: a, end_secs: b, x: 0.3, y: 0.2, w: 0.2, h, frames: 3 };
        let st = ScreenText {
            spans: vec![
                span("SETTINGS", 0.0, 40.0, 0.02),          // the sidebar: up throughout
                span("File Edit View", 0.0, 40.0, 0.012),   // the menu bar
                span("SEARCH & FILTERING", 6.0, 14.0, 0.014),
                span("Type to search", 6.0, 14.0, 0.012),
                TextSpan { y: 0.7, ..span("Pictures Videos", 6.0, 14.0, 0.02) }, // buttons, low and large
                span("APPEARANCE", 15.0, 30.0, 0.014),
                span("2026-09-19 10:41", 15.0, 30.0, 0.05), // a clock: digits, not a name
                TextSpan { confidence: 0.6, ..span("Ofra ort. ora ornt.", 15.0, 30.0, 0.05) }, // a guess at thumbnails
                span("Hr ot", 15.0, 30.0, 0.05),            // no real word
            ],
            ..Default::default()
        };
        let bs = crate::beatsheet::BeatSheet {
            duration_secs: 40.0,
            has_freeze: true,
            segments: vec![
                crate::beatsheet::Segment { start_secs: 6.0, end_secs: 14.0, kind: "idle".into() },
                crate::beatsheet::Segment { start_secs: 14.0, end_secs: 15.0, kind: "action".into() },
                crate::beatsheet::Segment { start_secs: 15.0, end_secs: 30.0, kind: "idle".into() },
                crate::beatsheet::Segment { start_secs: 35.0, end_secs: 40.0, kind: "idle".into() },
            ],
            reveals: vec![6.0, 15.0, 35.0],
            title_windows: vec![crate::beatsheet::Window { start_secs: 15.0, end_secs: 30.0 }],
            ..Default::default()
        };
        let labels = label_beats(&bs, &st);
        assert_eq!(labels.segments[0].as_deref(), Some("SEARCH & FILTERING"));
        assert_eq!(labels.segments[2].as_deref(), Some("APPEARANCE"));
        assert_eq!(labels.segments[3].as_deref(), Some("SETTINGS"), "with nothing of its own, the chrome names it");
        assert_eq!(labels.reveals, vec![Some("SEARCH & FILTERING".to_string()), Some("APPEARANCE".to_string()), None]);
        assert_eq!(labels.windows[0].as_deref(), Some("APPEARANCE"));
        let text = labelled_beats_text(&bs, &labels);
        assert!(text.contains("6.00-14.00 s  (8.0 s)  \"SEARCH & FILTERING\"") && text.contains("Reveals:\n  6.00 s  \"SEARCH & FILTERING\""), "{text}");
    }

    #[test]
    fn secrets_on_screen_are_seen_and_masked() {
        assert_eq!(sensitive_in("Contact: ada@example.com today").unwrap().0, "email");
        assert_eq!(sensitive_in("OPENAI_API_KEY=sk-proj-abc123def456ghi789").unwrap().0, "key");
        let (kind, masked) = sensitive_in("token ghp_16C7e42F292c6912E7710c838347Ae178B4a").unwrap();
        assert_eq!(kind, "key");
        assert!(masked.starts_with("ghp") && masked.ends_with("4a") && masked.contains('…'), "{masked}");
        assert_eq!(sensitive_in("password: hunter22").unwrap(), ("credential", "hun…22".into()));
        assert_eq!(sensitive_in("Card 4111 1111 1111 1111").unwrap().0, "card");
        for plain in ["File Edit View Help", "Search & Filtering", "password: ********", "version 2.4.1-beta.3", "https://example.com/docs",
            "open Strata with the password prompt instead of udiskie's dialog", "b52d86f6e1a24c7f9d3e0b1a2c3d4e5f.jpg", "3f2a9c8b1d7e4f6a5b0c9d8e7f6a5b4c", "token manager",
            "b52464F7-937145b4-9a80-a7d7eb.", "mmelstraeume-bachaLpsee-7572", "immelstraeunebachalpsee7572.", "b1f1a62-4ce-423-8318-07857c", "immelstraeunebachalpsee7572"] {
            assert!(sensitive_in(plain).is_none(), "{plain}");
        }
        assert_eq!(sensitive_in("password: Tr0ub4dor&3").unwrap().0, "credential");
        assert_eq!(sensitive_in("AKIAIOSFODNN7EXAMPLE").unwrap().0, "key");
        assert_eq!(sensitive_in("k9Xz2Qm7pL4vN8bR1tY6wE3uH5sA0dF2gJ").unwrap().0, "key");
        assert_eq!(sensitive_in("3f2a9c8b1d7e4f6a5b0c9d8e7f6a5b4c3f2a9c8b").unwrap().0, "key");
    }

    #[test]
    fn the_read_size_keeps_the_aspect_within_bounds_and_stays_even() {
        assert_eq!(read_size(3840, 2132), (2560, 1422));
        assert_eq!(read_size(1920, 1080), (1920, 1080));
        assert_eq!(read_size(640, 360), (1280, 720));
    }

    /// A frame every second across the file, moving or still, so a line
    /// only up while it scrolls is read; the beat sheet's reveals and
    /// settled stretches as well, never instead. Past an hour the step
    /// grows, the frames bounded.
    #[test]
    fn frames_are_picked_every_second_and_where_the_beat_sheet_points() {
        let none = pick_times(20.0, None);
        assert_eq!(none.len(), 20);
        assert!(none.windows(2).all(|w| w[1] - w[0] <= STEP_SECS + 1e-9), "{none:?}");
        assert_eq!(pick_times(10_000.0, None).len(), MAX_FRAMES);
        // A beat sheet that saw only a still start: the moving rest is
        // read every second all the same, and the reveals besides.
        let bs = crate::beatsheet::BeatSheet { duration_secs: 20.0, has_freeze: true, reveals: vec![5.0, 12.0], segments: vec![crate::beatsheet::Segment { start_secs: 0.0, end_secs: 4.0, kind: "idle".into() }], ..Default::default() };
        let picked = pick_times(20.0, Some(&bs));
        assert!(picked.iter().any(|t| (t - 12.3).abs() < MIN_GAP_SECS) && picked.iter().any(|t| (t - 15.2).abs() < 1e-9), "{picked:?}");
        assert!(picked.iter().any(|t| (t - 2.0).abs() < 1e-9), "the settled stretch's middle: {picked:?}");
        assert!(picked.windows(2).all(|w| w[1] - w[0] >= MIN_GAP_SECS - 1e-9), "{picked:?}");
    }

    /// A line followed through the frames it moves across: its box in
    /// each frame it is read in, even read a letter wrong, nearest where
    /// it was; the same text elsewhere and a frame without it give
    /// nothing.
    #[test]
    fn a_line_is_followed_where_it_moves() {
        let at = |x: f32, y: f32, text: &str| Line { text: text.into(), confidence: 0.95, x, y, w: 0.2, h: 0.04 };
        let frames = vec![
            FrameText { secs: 0.0, lines: vec![at(0.1, 0.8, "Password: hunter2")] },
            FrameText { secs: 0.2, lines: vec![at(0.1, 0.7, "Pasword: hunter2"), at(0.6, 0.1, "Password: hunter2")] },
            FrameText { secs: 0.4, lines: vec![at(0.1, 0.1, "Account Settings")] },
            FrameText { secs: 0.6, lines: vec![at(0.1, 0.5, "Password: hunter2")] },
        ];
        let track = follow_line("Password: hunter2", &frames);
        let got: Vec<(f64, f32)> = track.iter().map(|b| (b.secs, b.y)).collect();
        assert_eq!(got, [(0.0, 0.8), (0.2, 0.7), (0.6, 0.5)]);
        assert!(alike("Password: hunter2", "Pasword: hunter2") >= 0.8 && alike("Password: hunter2", "Account Settings") < 0.5);
    }

}
