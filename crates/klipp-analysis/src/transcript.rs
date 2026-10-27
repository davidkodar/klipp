//! The words: speech to text with timestamps, through whisper.cpp
//! (`whisper-rs`) on the CPU, with the model from the app's model cache.
//! Segments are whisper's sentences; words come from its token
//! timestamps, grouped at the spaces whisper's tokeniser puts before a
//! word. The language is detected from the first thirty seconds.
//!
//! The model is not fetched behind the user's back: the analyser wants
//! an item only when the model is in the cache, and
//! [`ensure_model`] downloads it when asked to.

use serde::{Deserialize, Serialize};

pub const ID: &str = "transcript";
/// Bumped whenever what a transcript says can change for the same
/// sound, so a stored one made before is made again (#731): 6 drops a
/// segment whose words all lie in measured silence, and takes in the
/// confidence filtering of #593, which changed transcripts without a bump;
/// 7 drops a phrase whisper repeats back to back over noise (#779).
pub const VERSION: u32 = 7;

/// The models the analyser can run with, best first; it uses the best
/// one installed. The small one is what came first; on real speech it
/// mishears names ("a matchy" for Omarchy), drops words and places
/// them loosely, and every caption made from it had to be rewritten.
pub const MODELS: [&str; 2] = ["whisper_large_v3_turbo_q5", "whisper_base_ggml"];

/// Force transcription onto the CPU. Set from the preference; when it has
/// never been set, the `KLIPP_TRANSCRIBE_ON_CPU=1` environment variable
/// is honoured instead, so scripts and older callers keep working.
static FORCE_CPU: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static FORCE_CPU_SET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set whether transcription runs on the CPU (slower, no GPU needed).
pub fn set_transcribe_on_cpu(on: bool) {
    FORCE_CPU.store(on, std::sync::atomic::Ordering::Relaxed);
    FORCE_CPU_SET.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether transcription should run on the CPU: the preference if set,
/// else the `KLIPP_TRANSCRIBE_ON_CPU` environment variable.
pub fn transcribe_on_cpu() -> bool {
    if FORCE_CPU_SET.load(std::sync::atomic::Ordering::Relaxed) {
        FORCE_CPU.load(std::sync::atomic::Ordering::Relaxed)
    } else {
        std::env::var("KLIPP_TRANSCRIBE_ON_CPU").map(|v| v == "1").unwrap_or(false)
    }
}
/// The model a download fetches.
pub const MODEL_ID: &str = MODELS[0];

/// Voice activity detection, fetched with the model: whisper runs only
/// where someone speaks. Without it, a street walk came back as "I'm
/// going to go to the next place." twice and wind as "The wind is blowing
/// up." five times, each stretched over thirty seconds; whisper.cpp's
/// no-speech probability read 0.00 on all of them (#499).
pub const VAD_MODEL_ID: &str = "silero_vad_v5_ggml";

/// Download the voice activity model; a failure is logged, and the
/// transcript is then made without it, as before.
fn fetch_vad_model() {
    let (Ok(manifest), Ok(cache)) = (klipp_ml::manifest::load_effective(), klipp_ml::cache::ModelCache::open()) else { return };
    let Some(entry) = manifest.get(VAD_MODEL_ID) else { return };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut progress = klipp_ml::download::NoOpProgress;
    if let Err(e) = klipp_ml::download::download_to_cache(&cache, entry, &mut progress, &cancel) {
        tracing::warn!("voice activity model download: {e:?}");
    }
}

/// The voice activity model in the app's model cache, if downloaded.
pub fn vad_model_path() -> Option<std::path::PathBuf> {
    let manifest = klipp_ml::manifest::load_effective().ok()?;
    let cache = klipp_ml::cache::ModelCache::open().ok()?;
    let path = cache.path_for(manifest.get(VAD_MODEL_ID)?);
    path.is_file().then_some(path)
}

/// How things are spelt: names and terms given to whisper as its
/// prompt, so "Omarchy" is not "a matchy". Set by whoever asks for
/// analysis, from the brief; a transcript made with another list is
/// stale.
static VOCABULARY: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Replace the list. Order and case are kept as given; duplicates and
/// blanks go.
pub fn set_vocabulary(words: Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    let words: Vec<String> = words.into_iter().map(|w| w.trim().to_string()).filter(|w| !w.is_empty() && seen.insert(w.to_lowercase())).collect();
    *VOCABULARY.lock().unwrap_or_else(|e| e.into_inner()) = words;
}

pub fn vocabulary() -> Vec<String> {
    VOCABULARY.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The language the project's speech is in (#531), as an ISO 639-1 code
/// ("en", "sv"); `None` detects it per file, from the first thirty
/// seconds, as before. Set from the project.
static LANGUAGE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn set_language(language: Option<String>) {
    *LANGUAGE.lock().unwrap_or_else(|e| e.into_inner()) = language.map(|l| l.trim().to_lowercase()).filter(|l| !l.is_empty() && l != "auto");
}

pub fn language() -> Option<String> {
    LANGUAGE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// A file to transcribe again however fresh its transcript (#531): with
/// another model, or in another language than the project's.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Redo {
    /// A model id from [`MODELS`]; `None` takes the best installed.
    pub model: Option<String>,
    /// `None` takes the project's language, or detects.
    pub language: Option<String>,
}

static REDO: std::sync::Mutex<Vec<(std::path::PathBuf, Redo)>> = std::sync::Mutex::new(Vec::new());

/// Ask for `path` to be transcribed again as `redo` says, on its next
/// analysis run.
pub fn redo(path: &std::path::Path, redo: Redo) {
    let mut all = REDO.lock().unwrap_or_else(|e| e.into_inner());
    all.retain(|(p, _)| p != path);
    all.push((path.to_path_buf(), redo));
}

/// Whether `path` waits to be transcribed again.
pub fn wants_redo(path: &std::path::Path) -> bool {
    REDO.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|(p, _)| p == path)
}

fn take_redo(path: &std::path::Path) -> Option<Redo> {
    let mut all = REDO.lock().unwrap_or_else(|e| e.into_inner());
    let at = all.iter().position(|(p, _)| p == path)?;
    Some(all.remove(at).1)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Segment {
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Word {
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
    /// The model's probability for the word's tokens, 0 to 1.
    pub confidence: f32,
    /// Corrected by hand (#531): kept when the file is transcribed again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub corrected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Transcript {
    pub duration_secs: f64,
    /// ISO 639-1 where whisper knows it, e.g. "en", "sv".
    pub language: Option<String>,
    pub model: String,
    /// The names and terms whisper was given.
    #[serde(default)]
    pub vocabulary: Vec<String>,
    /// Whisper ran only where voice activity detection heard speech.
    #[serde(default)]
    pub vad: bool,
    /// The language whisper was told (#531); `None` when it detected one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_language: Option<String>,
    /// The model was chosen for this file, not the best installed (#531),
    /// so a better one installed later does not redo it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub chosen_model: bool,
    pub segments: Vec<Segment>,
    pub words: Vec<Word>,
}

impl Transcript {
    /// The words spoken within `from..to`.
    pub fn words_within(&self, from: f64, to: f64) -> Vec<&Word> {
        self.words.iter().filter(|w| w.end_secs > from && w.start_secs < to).collect()
    }

    /// The segments overlapping `from..to`.
    pub fn segments_within(&self, from: f64, to: f64) -> Vec<&Segment> {
        self.segments.iter().filter(|s| s.end_secs > from && s.start_secs < to).collect()
    }

    /// Segments whose text contains `query`, case-insensitively.
    pub fn search(&self, query: &str) -> Vec<&Segment> {
        let q = query.to_lowercase();
        if q.trim().is_empty() {
            return Vec::new();
        }
        self.segments.iter().filter(|s| s.text.to_lowercase().contains(&q)).collect()
    }

    pub fn text(&self) -> String {
        self.segments.iter().map(|s| s.text.trim()).collect::<Vec<_>>().join(" ")
    }

    /// The word spoken at `at_secs` set to `text`, marked corrected
    /// (#531), and the text of the segment holding it made again from its
    /// words, so captions and search read the correction. `false` when no
    /// word is there or `text` is blank.
    pub fn correct(&mut self, at_secs: f64, text: &str) -> bool {
        let text = text.trim();
        if text.is_empty() {
            return false;
        }
        let Some(i) = self.words.iter().position(|w| w.start_secs <= at_secs && at_secs < w.end_secs).or_else(|| {
            self.words.iter().enumerate().min_by(|a, b| {
                let d = |w: &Word| ((w.start_secs + w.end_secs) / 2.0 - at_secs).abs();
                d(a.1).total_cmp(&d(b.1))
            }).filter(|(_, w)| ((w.start_secs + w.end_secs) / 2.0 - at_secs).abs() < 0.5).map(|(i, _)| i)
        }) else { return false };
        self.words[i].text = text.to_string();
        self.words[i].corrected = true;
        let mid = (self.words[i].start_secs + self.words[i].end_secs) / 2.0;
        if let Some(seg) = self.segments.iter_mut().find(|s| s.start_secs <= mid && mid <= s.end_secs) {
            let (from, to) = (seg.start_secs, seg.end_secs);
            seg.text = self.words.iter().filter(|w| (w.start_secs + w.end_secs) / 2.0 >= from && (w.start_secs + w.end_secs) / 2.0 <= to).map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        }
        true
    }

    /// The corrections in `before`, the transcript this one replaces,
    /// made again on the words here that cover the same moments (#531).
    pub fn keep_corrections(&mut self, before: &Transcript) {
        for w in before.words.iter().filter(|w| w.corrected) {
            self.correct((w.start_secs + w.end_secs) / 2.0, &w.text);
        }
    }
}

/// Correct the word spoken at `at_secs` of `path` to `text` (#531), in
/// the stored transcript, so captions, search and word cuts read it.
pub fn correct_word(path: &std::path::Path, at_secs: f64, text: &str) -> Result<(), String> {
    let key = crate::store::key_for(path).ok_or_else(|| format!("{} is not readable", path.display()))?;
    let stored = crate::store::load(&key, ID, VERSION).ok_or("the clip has no transcript")?;
    let mut t: Transcript = serde_json::from_value(stored).map_err(|e| e.to_string())?;
    if !t.correct(at_secs, text) {
        return Err("no word there to correct".into());
    }
    crate::store::save(&key, ID, VERSION, &serde_json::to_value(&t).map_err(|e| e.to_string())?)
}

/// One token as whisper reports it, for grouping into words.
#[derive(Debug, Clone)]
pub struct Token {
    pub text: String,
    pub t0: f64,
    pub t1: f64,
    pub p: f32,
}

/// Whisper's tokens are word pieces; a piece that begins with a space
/// begins a word. Special tokens (`[_BEG_]`, `<|en|>`) are not words.
pub fn group_words(tokens: &[Token]) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();
    let mut ps: Vec<f32> = Vec::new();
    for t in tokens {
        if t.text.starts_with("[_") || t.text.starts_with("<|") || t.text.trim().is_empty() {
            continue;
        }
        let starts_word = t.text.starts_with(' ') || words.is_empty();
        if starts_word {
            if let Some(last) = words.last_mut() {
                last.confidence = ps.iter().copied().sum::<f32>() / ps.len().max(1) as f32;
            }
            ps.clear();
            words.push(Word { start_secs: t.t0, end_secs: t.t1, text: t.text.trim().to_string(), confidence: t.p, corrected: false });
        } else if let Some(last) = words.last_mut() {
            last.text.push_str(t.text.trim_end());
            last.end_secs = t.t1;
        }
        ps.push(t.p);
    }
    if let Some(last) = words.last_mut() {
        last.confidence = ps.iter().copied().sum::<f32>() / ps.len().max(1) as f32;
    }
    words.retain(|w| !w.text.is_empty());
    words
}

/// Decode the file's sound to what whisper eats: 16 kHz mono f32.
fn decode_audio(ffmpeg: &std::path::Path, path: &std::path::Path) -> Result<Vec<f32>, String> {
    let mut cmd = std::process::Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-nostats", "-loglevel", "error"]).args(klipp_media::input_args(path)?).args(["-vn", "-sn", "-ac", "1", "-ar", "16000", "-f", "f32le", "-"]);
    let out = crate::hold::output(&mut cmd).map_err(|e| format!("could not run {}: {e}", ffmpeg.display()))?;
    if !out.status.success() {
        return Err(format!("ffmpeg could not decode the sound: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(out.stdout.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
}

/// The model file in the app's model cache, if it is there.
/// The best model installed: its manifest id and its file.
pub fn installed_model() -> Option<(&'static str, std::path::PathBuf)> {
    let manifest = klipp_ml::manifest::load_effective().ok()?;
    let cache = klipp_ml::cache::ModelCache::open().ok()?;
    MODELS.iter().find_map(|id| {
        let entry = manifest.get(id)?;
        // Presence only: the download verified the bytes, and hashing
        // half a gigabyte on every import would not be worth what it proves.
        let path = cache.path_for(entry);
        path.is_file().then_some((*id, path))
    })
}

/// Every model installed, best first (#531): what a re-transcription can
/// choose from.
pub fn installed_models() -> Vec<&'static str> {
    MODELS.iter().copied().filter(|id| installed_model_named(id).is_some()).collect()
}

/// The model `id` and its file, when it is installed.
pub fn installed_model_named(id: &str) -> Option<(&'static str, std::path::PathBuf)> {
    let manifest = klipp_ml::manifest::load_effective().ok()?;
    let cache = klipp_ml::cache::ModelCache::open().ok()?;
    let id = *MODELS.iter().find(|m| **m == id)?;
    let path = cache.path_for(manifest.get(id)?);
    path.is_file().then_some((id, path))
}

/// A model's name for people: "Large v3 Turbo", "Base".
pub fn model_label(id: &str) -> &'static str {
    match id {
        "whisper_large_v3_turbo_q5" => "Large v3 Turbo",
        "whisper_base_ggml" => "Base",
        _ => "another model",
    }
}

pub fn model_path() -> Option<std::path::PathBuf> {
    installed_model().map(|(_, p)| p)
}

/// The model to run with. With `download`, the best one, fetched and
/// verified against the manifest if it is not there yet; without, the
/// best one installed.
pub fn ensure_model(download: bool) -> Result<(&'static str, std::path::PathBuf), String> {
    // Voice activity detection rides along, under a megabyte and what
    // keeps whisper from writing sentences into noise: fetched on any
    // ask for transcription, with the transcriber there already or not.
    if download && vad_model_path().is_none() {
        fetch_vad_model();
    }
    match installed_model() {
        Some(found) if !download || found.0 == MODEL_ID => return Ok(found),
        None if !download => return Err(format!("no transcription model is downloaded; ask with download_models ({MODEL_ID})")),
        _ => {}
    }
    let manifest = klipp_ml::manifest::load_effective().map_err(|e| format!("model manifest: {e:?}"))?;
    let entry = manifest.get(MODEL_ID).ok_or_else(|| format!("no manifest entry {MODEL_ID}"))?;
    let cache = klipp_ml::cache::ModelCache::open().map_err(|e| format!("model cache: {e:?}"))?;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut progress = klipp_ml::download::NoOpProgress;
    let path = klipp_ml::download::download_to_cache(&cache, entry, &mut progress, &cancel).map_err(|e| format!("download of {MODEL_ID}: {e:?}"))?;
    Ok((MODEL_ID, path))
}

/// The spans of segments that are a model's hallucination: one text
/// (letters only, case aside) said by more than a third of the
/// segments when there are at least four, or three or more in a row.
/// Real speech does not repeat a sentence like that; music under a
/// speech model does.
pub fn hallucinated_spans(segments: &[Segment]) -> Vec<(f64, f64)> {
    let key = |s: &Segment| plain(&s.text);
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for s in segments {
        *counts.entry(key(s)).or_default() += 1;
    }
    let mut out = Vec::new();
    for (i, s) in segments.iter().enumerate() {
        let k = key(s);
        if k.is_empty() {
            continue;
        }
        let dominant = counts[&k] >= 4 && counts[&k] * 2 >= segments.len();
        let run = (i >= 2 && key(&segments[i - 1]) == k && key(&segments[i - 2]) == k)
            || (i + 2 < segments.len() && key(&segments[i + 1]) == k && key(&segments[i + 2]) == k)
            || (i >= 1 && i + 1 < segments.len() && key(&segments[i - 1]) == k && key(&segments[i + 1]) == k);
        if dominant || run {
            out.push((s.start_secs, s.end_secs));
        }
    }
    out
}

/// A phrase of at least this many words, said this many times back to
/// back, is whisper looping over noise: wind on a beach came out as "The
/// wind is blowing up." three times in one segment, whose text gave it
/// once (#779). People repeat a word or two ("no, no, no"); not a
/// sentence three times over.
const LOOP_PHRASE_WORDS: usize = 4;
const LOOP_REPEATS: usize = 3;
/// The longest phrase looked for: whisper loops a sentence, not a
/// paragraph, and the search is linear in the transcript only with a cap
/// (an hour's interview is nine thousand words, #803 review).
const LOOP_PHRASE_MAX_WORDS: usize = 24;

/// Drops the words of every phrase repeated back to back as above, and
/// the segments left without words by it. Returns how many words went.
pub fn drop_repeated_phrases(t: &mut Transcript) -> usize {
    let keys: Vec<String> = t.words.iter().map(|w| plain(&w.text)).collect();
    let n = keys.len();
    let mut looped = vec![false; n];
    let mut i = 0;
    while i < n {
        // The longest stretch from here that is one phrase over and over.
        let mut best = 0;
        for k in LOOP_PHRASE_WORDS..=((n - i) / LOOP_REPEATS).min(LOOP_PHRASE_MAX_WORDS) {
            if keys[i..i + k].iter().all(|w| w.is_empty()) {
                continue;
            }
            let mut reps = 1;
            while i + (reps + 1) * k <= n && keys[i + reps * k..i + (reps + 1) * k] == keys[i..i + k] {
                reps += 1;
            }
            if reps >= LOOP_REPEATS {
                best = best.max(k * reps);
            }
        }
        if best > 0 {
            looped[i..i + best].iter_mut().for_each(|l| *l = true);
            i += best;
        } else {
            i += 1;
        }
    }
    let dropped = looped.iter().filter(|l| **l).count();
    if dropped == 0 {
        return 0;
    }
    let words_in = |s: &Segment, words: &[Word]| words.iter().any(|w| w.start_secs >= s.start_secs - 0.05 && w.start_secs < s.end_secs + 0.05);
    let had: Vec<bool> = t.segments.iter().map(|s| words_in(s, &t.words)).collect();
    let mut k = 0;
    t.words.retain(|_| {
        k += 1;
        !looped[k - 1]
    });
    let mut j = 0;
    let words = t.words.clone();
    t.segments.retain(|s| {
        j += 1;
        !had[j - 1] || words_in(s, &words)
    });
    if t.segments.is_empty() {
        t.language = None;
    }
    tracing::info!("transcript: {dropped} words of a phrase repeated back to back dropped as hallucination");
    dropped
}

/// Letters and digits, lower case: what two spellings of a name share.
fn plain(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

/// A word's consonants: how it sounds to a model that spells by ear.
/// "clawed" and "Claude" are both `cld`.
fn skeleton(s: &str) -> String {
    plain(s).chars().filter(|c| !"aeiouywh".contains(*c)).collect()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut prev = row[0];
        row[0] = i;
        for j in 1..=b.len() {
            let cur = row[j];
            row[j] = if a[i - 1] == b[j - 1] { prev } else { 1 + prev.min(row[j]).min(row[j - 1]) };
            prev = cur;
        }
    }
    row[b.len()]
}

fn similarity(a: &str, b: &str) -> f32 {
    let n = a.chars().count().max(b.chars().count()).max(1);
    1.0 - edit_distance(a, b) as f32 / n as f32
}

/// Whether what was heard, `heard` (one word of the transcript), is
/// `term` (one word of a spelling) written by ear.
fn sounds_like(heard: &str, term: &str) -> bool {
    let (h, t) = (plain(heard), plain(term));
    if h == t {
        return true;
    }
    if t.chars().count() < 5 || h.chars().count().abs_diff(t.chars().count()) > 1 {
        return false;
    }
    let (sh, st) = (skeleton(heard), skeleton(term));
    let by_ear = similarity(&sh, &st);
    0.5 * similarity(&h, &t) + 0.5 * by_ear >= 0.6 || (sh == st && sh.chars().count() >= 3)
}

/// Write the brief's spellings over what whisper wrote by ear. Its
/// prompt only reaches the first half minute, and past that a name it
/// does not know comes out as "Amachi", "Umachi" and "Omachi" in turn,
/// each with full confidence. A run of words is a spelling when it is
/// the same letters (whatever the case and spacing: "quick shell" is
/// Quickshell), or — one word for one word — sounds like it, provided
/// the heard word is capitalised or another word of the term is there
/// letter for letter ("clawed code" is Claude Code; "march" is not
/// Omarchy).
pub fn respell(t: &mut Transcript, spellings: &[String]) {
    let mut terms: Vec<Vec<&str>> = spellings.iter().map(|s| s.split_whitespace().collect::<Vec<_>>()).filter(|w| !w.is_empty()).collect();
    terms.sort_by_key(|w| std::cmp::Reverse(w.len()));
    let mut fixes: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    while i < t.words.len() {
        // A word put right by hand stays as it was put.
        if t.words[i].corrected {
            i += 1;
            continue;
        }
        let mut took = 0;
        for term in &terms {
            let whole: String = plain(&term.join(""));
            // The same letters, over up to three words of the transcript.
            for n in (1..=3usize.min(t.words.len() - i)).rev() {
                if t.words[i..i + n].iter().any(|w| w.corrected) {
                    continue;
                }
                let heard: String = t.words[i..i + n].iter().map(|w| plain(&w.text)).collect();
                if heard == whole && whole.chars().count() >= 3 {
                    took = n;
                    break;
                }
            }
            // Word for word, by ear.
            if took == 0 && i + term.len() <= t.words.len() && !t.words[i..i + term.len()].iter().any(|w| w.corrected) {
                let heard = &t.words[i..i + term.len()];
                let exact = heard.iter().zip(term.iter()).filter(|(h, w)| plain(&h.text) == plain(w)).count();
                let all_sound = heard.iter().zip(term.iter()).all(|(h, w)| sounds_like(&h.text, w));
                let named = heard.iter().all(|h| h.text.chars().next().is_some_and(|c| c.is_uppercase()));
                if all_sound && exact < term.len() && (named || exact >= 1) {
                    took = term.len();
                }
            }
            if took > 0 {
                let was: Vec<String> = t.words[i..i + took].iter().map(|w| w.text.clone()).collect();
                let last = was.last().cloned().unwrap_or_default();
                let tail: String = last.chars().rev().take_while(|c| !c.is_alphanumeric()).collect::<Vec<_>>().into_iter().rev().collect();
                let now = format!("{}{tail}", term.join(" "));
                if was.join(" ") != now {
                    fixes.push((was.join(" "), now.clone()));
                    let (start, end) = (t.words[i].start_secs, t.words[i + took - 1].end_secs);
                    let confidence = t.words[i..i + took].iter().map(|w| w.confidence).fold(1.0, f32::min);
                    t.words.splice(i..i + took, [Word { start_secs: start, end_secs: end, text: now, confidence, corrected: false }]);
                    took = 1;
                }
                break;
            }
        }
        i += took.max(1);
    }
    // The segments read the same way.
    fixes.sort();
    fixes.dedup();
    for seg in &mut t.segments {
        for (was, now) in &fixes {
            if seg.text.contains(was.as_str()) {
                seg.text = seg.text.replace(was.as_str(), now);
            }
        }
    }
}

/// Whether a transcript made with the spellings `given` lacks any of
/// the ones `wanted` now, whatever their case.
pub fn lacks_spellings(given: &[String], wanted: &[String]) -> bool {
    let given: std::collections::HashSet<String> = given.iter().map(|w| w.to_lowercase()).collect();
    wanted.iter().any(|w| !given.contains(&w.to_lowercase()))
}

/// Whether a stored transcript is in the wrong language for a project
/// in `project` (#531): one whisper detected, other than the project's.
/// One asked for, the project's or the file's own, stands.
fn language_is_stale(stored: &serde_json::Value, project: Option<&str>) -> bool {
    let asked = stored.get("asked_language").and_then(|v| v.as_str()).is_some();
    let detected = stored.get("language").and_then(|v| v.as_str());
    !asked && project.is_some_and(|want| detected.is_some_and(|d| d != want))
}

pub struct TranscriptAnalyser;

impl crate::Analyser for TranscriptAnalyser {
    fn id(&self) -> &'static str {
        ID
    }
    fn schema_version(&self) -> u32 {
        VERSION
    }
    fn wants(&self, item: &klipp_media::MediaItem) -> bool {
        // A library track or effect has no speech; a model given one
        // hallucinates a sentence and repeats it for the whole file.
        item.has_audio && item.duration.to_secs_f64() > 0.0 && model_path().is_some() && !crate::is_licensed_audio(&item.path)
    }
    /// Made by a lesser model than the one installed now, with another
    /// list of how things are spelt, or over the whole sound when voice
    /// activity detection is there now to say where the speech is.
    fn stale(&self, stored: &serde_json::Value) -> bool {
        let model = stored.get("model").and_then(|m| m.as_str()).unwrap_or("");
        let vad = stored.get("vad").and_then(|v| v.as_bool()).unwrap_or(false);
        if !vad && vad_model_path().is_some() {
            return true;
        }
        let words: Vec<String> = stored.get("vocabulary").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
        // A language detected where the project now names another (#531);
        // one asked for, the project's or the file's own, stands.
        let wrong_language = language_is_stale(stored, language().as_deref());
        // A model chosen for the file (#531) is not redone for a better one.
        let chosen = stored.get("chosen_model").and_then(|v| v.as_bool()).unwrap_or(false);
        // Spellings asked for now that the stored one was not given. One
        // made with more is as good; with none asked for, any will do.
        (!chosen && installed_model().is_some_and(|(best, _)| best != model)) || lacks_spellings(&words, &vocabulary()) || wrong_language
    }
    fn run(&self, item: &klipp_media::MediaItem, ctx: &crate::AnalysisCtx) -> Result<serde_json::Value, String> {
        // A re-transcription asked for this file (#531): its model and
        // language; otherwise the best model and the project's language.
        let redo = take_redo(&item.path).unwrap_or_default();
        let chosen = redo.model.as_deref().and_then(|id| installed_model_named(id));
        let chosen_model = chosen.is_some();
        let (model_id, model) = match chosen {
            Some(m) => m,
            None => ensure_model(false)?,
        };
        let asked_language = redo.language.clone().or_else(language);
        let words = vocabulary();
        let audio = decode_audio(&ctx.ffmpeg, &item.path)?;
        if audio.len() < 16000 / 2 {
            let t = Transcript { duration_secs: item.duration.to_secs_f64(), model: model_id.into(), vocabulary: words, vad: vad_model_path().is_some(), asked_language, chosen_model, ..Default::default() };
            return serde_json::to_value(t).map_err(|e| e.to_string());
        }
        let mut transcript = transcribe_safely(model_id, &model, &audio, item.duration.to_secs_f64(), &words, asked_language.as_deref())?;
        transcript.asked_language = asked_language;
        transcript.chosen_model = chosen_model;
        drop_repeated_phrases(&mut transcript);
        // The sound analyser has already found where the file is silent;
        // words whisper places there are its invention, whatever it
        // thinks of them.
        if let Some(env) = crate::store::key_for(&item.path)
            .and_then(|key| crate::store::load(&key, crate::audio::ID, crate::audio::VERSION))
            .and_then(|v| serde_json::from_value::<crate::audio::AudioEnvelope>(v).ok())
        {
            // Trimmed first: an aligned word runs to the next word's
            // start, so the last word before a pause was mostly pause,
            // and dropping by share of silence took it for an invention
            // — "developer.", "that.", "it." went missing from captions.
            trim_words_to_sound(&mut transcript, &env);
            drop_words_in_silence(&mut transcript, &env);
        }
        respell(&mut transcript, &words);
        // Hand corrections outlive the transcript they were made on.
        if let Some(before) = crate::store::key_for(&item.path)
            .and_then(|key| crate::store::load(&key, ID, VERSION))
            .and_then(|v| serde_json::from_value::<Transcript>(v).ok())
        {
            transcript.keep_corrections(&before);
        }
        serde_json::to_value(transcript).map_err(|e| e.to_string())
    }
}

/// Models already loaded, by the id and the device they were built for.
///
/// A context is the 574 MB of weights plus whatever the backend put on
/// the GPU, and it is read-only once built — whisper keeps the mutable
/// part in a state, which every transcription still makes for itself.
/// So one load serves a whole queue instead of one file.
///
/// The lock doubles as the one that made the load safe: ggml brings its
/// Vulkan backend up lazily and that initialisation is not thread-safe,
/// so two analysis threads building a context at once raced in it and
/// called through a half-written vtable. Whoever gets here second now
/// either waits for the first load and then finds it, or finds it
/// already there.
type Cached = (String, bool, bool, std::sync::Arc<whisper_rs::WhisperContext>);
static CONTEXTS: std::sync::Mutex<Vec<Cached>> = std::sync::Mutex::new(Vec::new());

/// Let the weights go. Called when the analysis queue drains: holding
/// half a gigabyte of model, and its share of the GPU, against the
/// chance of another file arriving is not a trade an editor wants
/// while it is trying to play video.
pub fn release_models() {
    let mut cache = CONTEXTS.lock().unwrap_or_else(|e| e.into_inner());
    if !cache.is_empty() {
        tracing::info!("released {} cached whisper model(s)", cache.len());
        cache.clear();
    }
}

/// The argument that makes the app's executable a transcription worker.
pub const WORKER_FLAG: &str = "--transcribe-worker";

/// What a worker is asked to do; the sound comes on its standard input,
/// 16 kHz mono f32, little-endian.
#[derive(Serialize, Deserialize)]
struct WorkerJob {
    model_id: String,
    model: std::path::PathBuf,
    duration_secs: f64,
    words: Vec<String>,
    /// Word times by alignment (DTW), or the segments' own.
    aligned: bool,
    on_cpu: bool,
    /// The language to transcribe in; `None` detects (#531).
    #[serde(default)]
    language: Option<String>,
}

/// One worker at a time: each loads the model itself, half a gigabyte
/// of it onto the GPU, where the app's own cache shared one.
static ONE_WORKER: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// [`transcribe`] where a crash in whisper.cpp cannot take the app with
/// it (#640): in a [`crate::worker`] when the app has set them up, and
/// again without word alignment when the worker dies, its segments'
/// times standing in for the words'. In this process otherwise — a
/// test, a tool.
///
/// whisper.cpp's word alignment (DTW) aborts on a decode pass of 150 ms
/// or less, which quiet sound makes, and took the editor down with it —
/// on every open of a project holding such a clip, since analysis runs
/// again then.
fn transcribe_safely(model_id: &str, model: &std::path::Path, audio: &[f32], duration_secs: f64, words: &[String], language: Option<&str>) -> Result<Transcript, String> {
    match crate::worker::exe() {
        Some(exe) => transcribe_in(exe, model_id, model, audio, duration_secs, words, language),
        None => transcribe(model_id, model, audio, duration_secs, words, true, language),
    }
}

/// [`transcribe_safely`] in the worker `exe`.
fn transcribe_in(exe: &std::path::Path, model_id: &str, model: &std::path::Path, audio: &[f32], duration_secs: f64, words: &[String], language: Option<&str>) -> Result<Transcript, String> {
    let _one = ONE_WORKER.lock().unwrap_or_else(|e| e.into_inner());
    let job = |aligned: bool| WorkerJob { model_id: model_id.into(), model: model.into(), duration_secs, words: words.to_vec(), aligned, on_cpu: transcribe_on_cpu(), language: language.map(str::to_string) };
    match run_worker(exe, &job(true), audio) {
        Ok(reply) => reply,
        Err(died) => {
            tracing::warn!("transcription stopped in its worker ({died}); running it again without word alignment");
            match run_worker(exe, &job(false), audio) {
                Ok(reply) => reply,
                Err(died) => Err(format!("transcription stopped in its worker twice ({died})")),
            }
        }
    }
}

/// How long a worker may take on `audio_secs` of sound before it is
/// taken for hung and stopped: the CPU transcribes in about a third of
/// the sound's length, so this is generous.
fn worker_deadline(audio_secs: f64) -> std::time::Duration {
    std::time::Duration::from_secs_f64((audio_secs * 2.0).max(0.0) + 120.0)
}

/// Run one job in a worker: its reply, or why the worker died.
fn run_worker(exe: &std::path::Path, job: &WorkerJob, audio: &[f32]) -> Result<Result<Transcript, String>, String> {
    let spec = serde_json::to_string(job).map_err(|e| e.to_string())?;
    let bytes: Vec<u8> = audio.iter().flat_map(|v| v.to_le_bytes()).collect();
    let job = crate::worker::Job { flag: WORKER_FLAG, spec, input: crate::worker::Input::Data(bytes), deadline: worker_deadline(audio.len() as f64 / 16000.0), what: "transcription" };
    crate::worker::run::<Box<Transcript>>(exe, job).map(|reply| reply.map(|t| *t))
}

/// A transcription worker's whole life (#640): the job named in `spec`,
/// on the sound on standard input, its reply on standard output. Returns
/// the exit code; a crash in whisper.cpp ends it before that.
pub(crate) fn worker_main(spec: &str) -> i32 {
    use std::io::Read;
    let reply = (|| -> Result<Box<Transcript>, String> {
        let job: WorkerJob = serde_json::from_str(spec).map_err(|e| format!("the job: {e}"))?;
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes).map_err(|e| format!("the sound: {e}"))?;
        let audio: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        set_transcribe_on_cpu(job.on_cpu);
        transcribe(&job.model_id, &job.model, &audio, job.duration_secs, &job.words, job.aligned, job.language.as_deref()).map(Box::new)
    })();
    crate::worker::answer(reply)
}

/// Transcribe `audio`. `aligned`: word times from the model's alignment
/// heads (DTW); without, the timestamp tokens' — rougher, and what a
/// worker falls back to when alignment brought whisper.cpp down (#640).
fn transcribe(model_id: &str, model: &std::path::Path, audio: &[f32], duration_secs: f64, words: &[String], aligned: bool, language: Option<&str>) -> Result<Transcript, String> {
    use whisper_rs::{DtwMode, DtwModelPreset, DtwParameters, FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};
    whisper_rs::install_logging_hooks();
    let mut cparams = WhisperContextParameters::default();
    // On the GPU, through Vulkan, whichever vendor's it is: the app
    // needs one to run at all. A preference override wins; otherwise
    // `KLIPP_TRANSCRIBE_ON_CPU=1` keeps it off.
    let on_gpu = !transcribe_on_cpu();
    cparams.use_gpu(on_gpu);
    // Word times from the model's alignment heads (DTW over the
    // cross-attention), not from its timestamp tokens: those put a
    // word anywhere inside its half-second bucket, which is no use for
    // cutting on.
    let preset = || match model_id {
        "whisper_large_v3_turbo_q5" => DtwModelPreset::LargeV3Turbo,
        _ => DtwModelPreset::Base,
    };
    if aligned {
        cparams.dtw_parameters(DtwParameters { mode: DtwMode::ModelPreset { model_preset: preset() }, ..Default::default() });
    }
    let ctx = {
        let mut cache = CONTEXTS.lock().unwrap_or_else(|e| e.into_inner());
        match cache.iter().find(|(id, gpu, dtw, _)| id == model_id && *gpu == on_gpu && *dtw == aligned) {
            Some((_, _, _, ctx)) => ctx.clone(),
            None => {
                let t = std::time::Instant::now();
                let built = match WhisperContext::new_with_params(model, cparams) {
                    Ok(ctx) => ctx,
                    // A GPU that will not have it (no memory left, a
                    // driver without what the shaders need) is not the
                    // end: the CPU does the same work in about a third
                    // of the file's length.
                    Err(e) if on_gpu => {
                        tracing::warn!("whisper could not start on the GPU ({e}); running on the CPU");
                        let mut cpu = WhisperContextParameters::default();
                        cpu.use_gpu(false);
                        if aligned {
                            cpu.dtw_parameters(DtwParameters { mode: DtwMode::ModelPreset { model_preset: preset() }, ..Default::default() });
                        }
                        WhisperContext::new_with_params(model, cpu).map_err(|e| format!("whisper model: {e}"))?
                    }
                    Err(e) => return Err(format!("whisper model: {e}")),
                };
                tracing::info!("loaded whisper model {model_id} in {:.1} s", t.elapsed().as_secs_f64());
                let ctx = std::sync::Arc::new(built);
                cache.push((model_id.to_string(), on_gpu, aligned, ctx.clone()));
                ctx
            }
        }
    };
    // The state is per transcription; the weights behind it are shared.
    let mut state = ctx.create_state().map_err(|e| format!("whisper state: {e}"))?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    // Half the machine, up to sixteen: whisper stops gaining past that,
    // and the app is still being used while this runs.
    let threads = std::env::var("KLIPP_TRANSCRIBE_THREADS")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or((std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) / 2).clamp(4, 16) as i32);
    params.set_n_threads(threads);
    let prompt = if words.is_empty() { String::new() } else { format!("Names and terms: {}.", words.join(", ")) };
    if !prompt.is_empty() {
        params.set_initial_prompt(&prompt);
    }
    params.set_language(Some(language.unwrap_or("auto")));
    params.set_translate(false);
    params.set_token_timestamps(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);
    // Whisper writes captions for sound that is not speech — "[Music]",
    // "(coughing)", a date it has seen too often — and does so most of
    // all over silence. Keep its non-speech tokens out, and drop what it
    // says where it thinks nobody is speaking.
    params.set_suppress_nst(true);
    params.set_no_speech_thold(NO_SPEECH_THRESHOLD);
    // Where someone speaks, when the voice activity model is here.
    // Whisper still runs on the whole sound, for its timings and for a
    // line said under loud music, which the detector can miss even in a
    // file where it hears nothing else; what it writes outside the
    // speech is judged below. (whisper.cpp's own VAD switch left a
    // street walk's invented sentences as they were.)
    let speech = vad_model_path().and_then(|p| speech_spans(&p, audio, threads));
    let t_run = std::time::Instant::now();
    state.full(params, audio).map_err(|e| format!("whisper run: {e}"))?;
    tracing::info!(
        "whisper: {:.1} s of audio in {:.1} s on the {} with {threads} threads, model {model_id}",
        audio.len() as f64 / 16000.0,
        t_run.elapsed().as_secs_f64(),
        if on_gpu { "GPU" } else { "CPU" },
    );
    if std::env::var("KLIPP_WHISPER_TIMINGS").is_ok() {
        ctx.print_timings();
    }

    let lang = whisper_rs::get_lang_str(state.full_lang_id_from_state()).map(|s| s.to_string());
    let n = state.full_n_segments();
    let mut segments = Vec::with_capacity(n as usize);
    let mut tokens = Vec::new();
    for i in 0..n {
        let Some(seg) = state.get_segment(i) else { continue };
        let text = seg.to_str_lossy().map(|c| c.into_owned()).unwrap_or_default();
        let (t0, t1) = (seg.start_timestamp() as f64 / 100.0, seg.end_timestamp() as f64 / 100.0);
        if std::env::var_os("KLIPP_TRANSCRIPT_DEBUG").is_some() {
            let ps: Vec<f32> = (0..seg.n_tokens()).filter_map(|j| seg.get_token(j)).map(|t| t.token_data().p).collect();
            let mean = ps.iter().sum::<f32>() / ps.len().max(1) as f32;
            let mean_log = ps.iter().map(|p| p.max(1e-6).ln()).sum::<f32>() / ps.len().max(1) as f32;
            eprintln!("segment {t0:.2}-{t1:.2} no_speech {:.2} mean_p {mean:.2} mean_logp {mean_log:.2} tokens {} {text:?}", seg.no_speech_probability(), ps.len());
        }
        if seg.no_speech_probability() > NO_SPEECH_THRESHOLD || !is_speech(&text) {
            continue;
        }
        segments.push(Segment { start_secs: t0, end_secs: t1, text: text.trim().to_string() });
        for j in 0..seg.n_tokens() {
            let Some(tok) = seg.get_token(j) else { continue };
            let d = tok.token_data();
            let text = tok.to_str_lossy().map(|c| c.into_owned()).unwrap_or_default();
            // The aligned time when there is one: where the token is
            // said. Its end is the next token's start, settled below.
            let (t0, t1) = if d.t_dtw >= 0 { (d.t_dtw as f64 / 100.0, f64::NAN) } else { (d.t0 as f64 / 100.0, d.t1 as f64 / 100.0) };
            tokens.push(Token { text, t0, t1, p: d.p });
        }
    }
    // What whisper wrote where nobody speaks is its invention.
    if let Some(spans) = &speech {
        let (kept, heard) = keep_spoken(&segments, &tokens, spans);
        if kept.len() < segments.len() {
            tracing::info!("transcript: {} segments outside the speech dropped", segments.len() - kept.len());
        }
        segments = kept;
        tokens = heard;
    }
    // Music or noise makes the model repeat one sentence for the whole
    // file; those segments are not speech and are dropped with their
    // tokens.
    let dropped = hallucinated_spans(&segments);
    if !dropped.is_empty() {
        tracing::info!("transcript: {} repeated segments dropped as hallucination", dropped.len());
        tokens.retain(|t| !dropped.iter().any(|(a, b)| t.t0 >= *a - 0.01 && t.t0 < *b + 0.01));
        segments.retain(|s| !dropped.iter().any(|(a, b)| (s.start_secs - *a).abs() < 1e-6 && (s.end_secs - *b).abs() < 1e-6));
    }
    settle_token_ends(&mut tokens, &segments);
    let spoken = group_words(&tokens);
    // A language detected over silence is a guess; say nothing instead.
    let language = if segments.is_empty() { None } else { lang };
    Ok(Transcript { duration_secs, language, model: model_id.into(), vocabulary: words.to_vec(), vad: speech.is_some(), asked_language: None, chosen_model: false, segments, words: spoken })
}

/// Where in 16 kHz mono `audio` someone speaks, as (start, end) seconds,
/// from the voice activity model at `model`. `None` when the model would
/// not run: the transcript is then made as before, over everything.
fn speech_spans(model: &std::path::Path, audio: &[f32], threads: i32) -> Option<Vec<(f64, f64)>> {
    use whisper_rs::{WhisperVadContext, WhisperVadContextParams, WhisperVadParams};
    let mut cparams = WhisperVadContextParams::new();
    cparams.set_n_threads(threads.min(4));
    cparams.set_use_gpu(false);
    let mut ctx = WhisperVadContext::new(model.to_str()?, cparams).map_err(|e| tracing::warn!("voice activity model: {e}")).ok()?;
    let segments = ctx.segments_from_samples(WhisperVadParams::new(), audio).map_err(|e| tracing::warn!("voice activity: {e}")).ok()?;
    // Centiseconds.
    Some(segments.map(|s| (s.start as f64 / 100.0, s.end as f64 / 100.0)).collect())
}

/// Slack around a stretch of speech: a word's aligned time can sit a
/// little before the voice is loud enough to be heard as one.
const SPEECH_PAD_SECS: f64 = 0.4;

/// The segments that overlap speech, and those outside it that read as
/// speech all the same, with their tokens.
///
/// Voice activity misses a line shouted under a film's score ("Captain,
/// we have to abort!"), which whisper still hears at a speaking pace and
/// sure of it. What it invents over noise is nothing like that: one
/// sentence stretched over a thirty-second window, a third of a token a
/// second. So outside the speech a segment stays only at a speaking pace
/// (`SPOKEN_TOKENS_PER_SEC`) and sure of its words (`SPOKEN_MEAN_LOG_P`).
pub fn keep_spoken(segments: &[Segment], tokens: &[Token], speech: &[(f64, f64)]) -> (Vec<Segment>, Vec<Token>) {
    let overlap = |a: f64, b: f64| speech.iter().map(|(s, e)| (b.min(*e + SPEECH_PAD_SECS) - a.max(*s - SPEECH_PAD_SECS)).max(0.0)).sum::<f64>();
    let within = |t: &Token, s: &Segment| t.t0 >= s.start_secs - 0.01 && t.t0 < s.end_secs + 0.01;
    let reads_as_speech = |s: &Segment| {
        let ps: Vec<f32> = tokens.iter().filter(|t| within(t, s)).map(|t| t.p).collect();
        let secs = (s.end_secs - s.start_secs).max(0.1);
        let mean_log_p = ps.iter().map(|p| p.max(1e-6).ln()).sum::<f32>() / ps.len().max(1) as f32;
        !ps.is_empty() && ps.len() as f64 / secs >= SPOKEN_TOKENS_PER_SEC && mean_log_p >= SPOKEN_MEAN_LOG_P
    };
    let kept: Vec<Segment> = segments.iter().filter(|s| overlap(s.start_secs, s.end_secs) > 0.2 || reads_as_speech(s)).cloned().collect();
    let heard: Vec<Token> = tokens.iter().filter(|t| kept.iter().any(|s| within(t, s))).cloned().collect();
    (kept, heard)
}

/// Outside detected speech, a segment is kept at this pace or faster:
/// speech runs at three to five tokens a second.
const SPOKEN_TOKENS_PER_SEC: f64 = 1.0;
/// And this sure of its tokens, as the mean of their log probability.
const SPOKEN_MEAN_LOG_P: f32 = -0.7;

/// An aligned token has a time and no end: it ends where the next one
/// starts, or a little after its own start at the end of a segment —
/// never past the segment it is in.
fn settle_token_ends(tokens: &mut [Token], segments: &[Segment]) {
    const LAST_TOKEN_SECS: f64 = 0.45;
    for i in 0..tokens.len() {
        if !tokens[i].t1.is_nan() {
            continue;
        }
        let start = tokens[i].t0;
        let seg_end = segments.iter().find(|s| start >= s.start_secs - 0.05 && start <= s.end_secs + 0.05).map(|s| s.end_secs);
        let next = tokens[i + 1..].iter().map(|t| t.t0).find(|t| *t > start);
        let mut end = next.unwrap_or(start + LAST_TOKEN_SECS).min(start + 1.5);
        if let Some(e) = seg_end {
            end = end.min(e.max(start + 0.05));
        }
        tokens[i].t1 = end.max(start + 0.02);
    }
}

/// A word does not reach into measured silence: its edges come in to
/// where the sound is. Whisper let "Oh," run across 1.2 s of nothing,
/// and a cut in that nothing read as a cut inside the word.
pub fn trim_words_to_sound(t: &mut Transcript, env: &crate::audio::AudioEnvelope) {
    for w in &mut t.words {
        for s in &env.silences {
            if s.end_secs <= w.start_secs || s.start_secs >= w.end_secs {
                continue;
            }
            let covers_head = s.start_secs <= w.start_secs;
            let covers_tail = s.end_secs >= w.end_secs;
            if covers_head && !covers_tail {
                w.start_secs = s.end_secs.min(w.end_secs - 0.02);
            } else if !covers_head {
                // Silence over the word's tail, or inside it: the word's
                // time is its onset, so it is the sound before the
                // silence, and what follows the silence is the next word's.
                w.end_secs = s.start_secs.max(w.start_secs + 0.02);
            }
        }
    }
}

/// Above this probability of no speech, whisper's words for a segment
/// are dropped.
const NO_SPEECH_THRESHOLD: f32 = 0.6;

/// Segments and words lying almost wholly inside measured silence are
/// dropped: nobody spoke there. A segment is kept unless silence covers
/// at least this share of it.
const SILENT_SHARE: f64 = 0.8;

pub fn drop_words_in_silence(t: &mut Transcript, env: &crate::audio::AudioEnvelope) {
    let silent = |a: f64, b: f64| -> bool {
        let span = (b - a).max(0.01);
        env.silence_within(a, b) / span >= SILENT_SHARE
    };
    // A segment whose words all lie in silence was not heard either, even
    // when the window it spans is not silent enough to drop as a whole:
    // whisper stretched "So, let's go." over thirty seconds of a street
    // walk where the voice detector had caught a passer-by (#731). Only
    // for segments that had words; one from a transcript without word
    // times is judged by its span alone.
    let words_in = |s: &Segment, words: &[Word]| words.iter().filter(|w| w.start_secs >= s.start_secs - 0.05 && w.start_secs < s.end_secs + 0.05).count();
    let before: Vec<usize> = t.segments.iter().map(|s| words_in(s, &t.words)).collect();
    t.words.retain(|w| !silent(w.start_secs, w.end_secs));
    let mut i = 0;
    let words = t.words.clone();
    t.segments.retain(|s| {
        let had = before[i];
        i += 1;
        !silent(s.start_secs, s.end_secs) && !(had > 0 && words_in(s, &words) == 0)
    });
    if t.segments.is_empty() {
        t.language = None;
    }
}

/// Whether a segment's text is words rather than one of whisper's
/// sound captions: `[Music]`, `(coughing)`, `*släpp*`, `♪`.
pub fn is_speech(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    let wrapped = |a: char, b: char| t.starts_with(a) && t.ends_with(b);
    if wrapped('[', ']') || wrapped('(', ')') || wrapped('*', '*') || wrapped('♪', '♪') {
        return false;
    }
    t.chars().any(|c| c.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(text: &str, t0: f64, t1: f64, p: f32) -> Token {
        Token { text: text.into(), t0, t1, p }
    }

    /// #779: the beach take's transcript as stored, the wind heard as one
    /// sentence three times over, is dropped whole; a line that repeats a
    /// short phrase, or says a sentence twice, is kept.
    fn said(words: &[(&str, f64)]) -> Transcript {
        let words: Vec<Word> = words.iter().map(|&(t, at)| Word { start_secs: at, end_secs: at + 0.4, text: t.into(), confidence: 0.9, corrected: false }).collect();
        let text = words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        Transcript { segments: vec![Segment { start_secs: 0.0, end_secs: 10.0, text }], words, ..Default::default() }
    }

    #[test]
    fn a_corrected_word_reaches_its_segment_and_outlives_a_new_transcript() {
        let mut t = said(&[("Welcome", 0.0), ("to", 0.5), ("a", 1.0), ("matchy", 1.5)]);
        assert!(t.correct(1.6, "Omarchy"));
        assert_eq!(t.segments[0].text, "Welcome to a Omarchy");
        assert!(t.words[3].corrected);
        assert!(!t.correct(1.6, "  "), "a blank correction is not taken");
        assert!(!t.correct(30.0, "x"), "nothing is said there");
        // Transcribed again, a little differently: the correction lands
        // on the word covering the same moment.
        let mut again = said(&[("Welcome", 0.05), ("to", 0.55), ("a", 1.05), ("much", 1.55)]);
        again.keep_corrections(&t);
        assert_eq!(again.words[3].text, "Omarchy");
        assert_eq!(again.segments[0].text, "Welcome to a Omarchy");
    }

    #[test]
    fn a_detected_language_other_than_the_projects_is_stale_and_an_asked_one_is_not() {
        let stored = |asked: Option<&str>, detected: &str| serde_json::json!({ "language": detected, "asked_language": asked });
        assert!(language_is_stale(&stored(None, "en"), Some("sv")), "detected English in a Swedish project: made again in Swedish");
        assert!(!language_is_stale(&stored(None, "sv"), Some("sv")));
        assert!(!language_is_stale(&stored(Some("en"), "en"), Some("sv")), "English asked for this file stands");
        assert!(!language_is_stale(&stored(None, "en"), None), "no project language: anything detected stands");
    }

    #[test]
    fn a_sentence_looped_over_wind_is_dropped() {
        let word = |text: &str, at: f64| Word { start_secs: at, end_secs: at + 0.2, text: text.into(), confidence: 1.0, corrected: false };
        let said = |text: &str, from: f64| text.split(' ').enumerate().map(|(k, w)| word(w, from + k as f64 * 0.3)).collect::<Vec<_>>();
        let mut beach = Transcript {
            duration_secs: 23.6,
            language: Some("en".into()),
            segments: vec![Segment { start_secs: 7.58, end_secs: 13.58, text: "The wind is blowing up.".into() }],
            words: [said("The wind is blowing up.", 8.9), said("The wind is blowing up.", 10.5), said("The wind is blowing up.", 12.1)].concat(),
            ..Default::default()
        };
        assert_eq!(drop_repeated_phrases(&mut beach), 15);
        assert!(beach.words.is_empty() && beach.segments.is_empty() && beach.language.is_none(), "{beach:?}");

        let line = "No, no, no, we have to go back. We have to go back.";
        let mut speech = Transcript {
            segments: vec![Segment { start_secs: 1.0, end_secs: 6.0, text: line.into() }],
            words: said(line, 1.0),
            ..Default::default()
        };
        assert_eq!(drop_repeated_phrases(&mut speech), 0);
        assert_eq!(speech.words.len(), 13);

        // A loop after real speech takes only the loop.
        let mut mixed = Transcript {
            segments: vec![Segment { start_secs: 0.0, end_secs: 3.0, text: "Look at that.".into() }, Segment { start_secs: 4.0, end_secs: 9.0, text: "Thank you for watching.".into() }],
            words: [said("Look at that.", 0.5), said("Thank you for watching.", 4.0), said("Thank you for watching.", 5.5), said("Thank you for watching.", 7.0)].concat(),
            ..Default::default()
        };
        assert_eq!(drop_repeated_phrases(&mut mixed), 12);
        assert_eq!(mixed.segments.len(), 1);
        assert_eq!(mixed.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), ["Look", "at", "that."]);
    }

    /// #803 review: the search is bounded by the phrase cap, not the
    /// transcript's length: a hundred thousand words of talk (some eight
    /// hours), and as many of one word over and over, each well under a
    /// second. Uncapped, the talk took four.
    #[test]
    fn looking_for_loops_in_a_long_transcript_is_quick() {
        let talk: Vec<Word> = (0..100_000).map(|k| Word { start_secs: k as f64 * 0.3, end_secs: k as f64 * 0.3 + 0.2, text: format!("w{}", (k * 7919) % 5003), confidence: 1.0, corrected: false }).collect();
        let same: Vec<Word> = (0..100_000).map(|k| Word { start_secs: k as f64 * 0.3, end_secs: k as f64 * 0.3 + 0.2, text: "so".into(), confidence: 1.0, corrected: false }).collect();
        for (what, words) in [("talk", talk), ("one word", same)] {
            let mut t = Transcript { segments: vec![Segment { start_secs: 0.0, end_secs: 30_000.0, text: String::new() }], words, ..Default::default() };
            let start = std::time::Instant::now();
            drop_repeated_phrases(&mut t);
            assert!(start.elapsed() < std::time::Duration::from_secs(1), "{what}: {:?}", start.elapsed());
        }
    }

    #[test]
    fn aligned_tokens_end_where_the_next_begins_and_words_stay_out_of_silence() {
        // Aligned tokens come with a time and no end.
        let mut tokens = vec![tok(" Oh", 10.2, f64::NAN, 0.9), tok(",", 10.5, f64::NAN, 0.9), tok(" I", 12.0, f64::NAN, 0.9), tok(" hope", 12.2, f64::NAN, 0.9)];
        let segments = vec![Segment { start_secs: 10.0, end_secs: 12.5, text: "Oh, I hope".into() }];
        settle_token_ends(&mut tokens, &segments);
        assert_eq!(tokens[0].t1, 10.5, "to the next token");
        assert_eq!(tokens[1].t1, 12.0);
        assert_eq!(tokens[3].t1, 12.5, "the last one to a little after it, inside its segment");
        let mut t = Transcript { words: group_words(&tokens), ..Default::default() };
        assert_eq!(t.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), ["Oh,", "I", "hope"]);
        assert_eq!((t.words[0].start_secs, t.words[0].end_secs), (10.2, 12.0), "the comma drags it across the pause");

        // The sound analyser measured nothing from 10.6 to 11.9 s: the word ends where the sound does.
        let env = crate::audio::AudioEnvelope { duration_secs: 20.0, silences: vec![crate::audio::Span { start_secs: 10.6, end_secs: 11.9 }, crate::audio::Span { start_secs: 0.0, end_secs: 10.25 }], ..Default::default() };
        trim_words_to_sound(&mut t, &env);
        assert_eq!((t.words[0].start_secs, t.words[0].end_secs), (10.25, 10.6));
        assert_eq!((t.words[1].start_secs, t.words[1].end_secs), (12.0, 12.2), "a word in the sound is left alone");
    }

    #[test]
    fn names_written_by_ear_are_respelt_and_ordinary_words_are_not() {
        let say = |text: &str| -> Transcript {
            let words = text.split(' ').enumerate().map(|(k, w)| Word { start_secs: k as f64, end_secs: k as f64 + 0.5, text: w.into(), confidence: 1.0, corrected: false }).collect();
            Transcript { segments: vec![Segment { start_secs: 0.0, end_secs: 99.0, text: text.into() }], words, ..Default::default() }
        };
        let spell: Vec<String> = ["Omarchy", "Quickshell", "Hyprland", "Claude Code", "Ruby on Rails", "QML", "DHH"].iter().map(|s| s.to_string()).collect();
        let mut t = say("I install Amachi, then Umachi and Omachi. QuickShell plus Hyperland, the quick shell, was an clawed code harness on ruby on rails.");
        respell(&mut t, &spell);
        let text: Vec<&str> = t.words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(text.join(" "), "I install Omarchy, then Omarchy and Omarchy. Quickshell plus Hyprland, the Quickshell, was an Claude Code harness on Ruby on Rails.");
        assert_eq!(t.segments[0].text, text.join(" "), "the segment reads the same");
        // Three words became one: it runs from the first to the last of them.
        let rails = t.words.last().unwrap();
        assert_eq!((rails.start_secs, rails.end_secs), (19.0, 21.5));

        // Ordinary words that happen to be near a spelling are left alone.
        let mut t = say("In March the monarchy moved very quickly to the code and the rails of a hyper land.");
        let before = t.clone();
        respell(&mut t, &spell);
        assert_eq!(t, before);
    }

    #[test]
    fn a_transcript_is_stale_only_for_spellings_it_was_not_given() {
        let given = vec!["Omarchy".to_string(), "DHH".to_string()];
        assert!(!lacks_spellings(&given, &[]), "nothing asked for");
        assert!(!lacks_spellings(&given, &["omarchy".into()]), "a subset, whatever the case");
        assert!(lacks_spellings(&given, &["Omarchy".into(), "QML".into()]));
        assert!(lacks_spellings(&[], &["QML".into()]));
    }

    #[test]
    fn words_inside_measured_silence_are_dropped() {
        let env = crate::audio::AudioEnvelope {
            duration_secs: 10.0,
            silences: vec![crate::audio::Span { start_secs: 0.0, end_secs: 4.0 }],
            ..Default::default()
        };
        let mut t = Transcript {
            language: Some("nn".into()),
            segments: vec![
                Segment { start_secs: 1.0, end_secs: 3.0, text: "encounters n".into() },
                Segment { start_secs: 5.0, end_secs: 7.0, text: "hello".into() },
            ],
            words: vec![
                Word { start_secs: 1.0, end_secs: 2.0, text: "encounters".into(), confidence: 0.5, corrected: false },
                Word { start_secs: 5.0, end_secs: 5.5, text: "hello".into(), confidence: 0.9, corrected: false },
            ],
            ..Default::default()
        };
        drop_words_in_silence(&mut t, &env);
        assert_eq!(t.segments.len(), 1);
        assert_eq!(t.words.len(), 1);
        assert_eq!(t.language.as_deref(), Some("nn"), "speech remains, so the language stays");
        t.segments.clear();
        drop_words_in_silence(&mut t, &env);
        assert!(t.language.is_none());
    }

    /// #731: a sentence stretched over a long window whose words all lie
    /// in measured silence goes, though the window itself is mostly
    /// sound; one with a word in the sound stays; and a transcript with
    /// no word times keeps its segments, judged by span alone.
    #[test]
    fn a_segment_whose_words_were_all_silent_goes_too() {
        let env = crate::audio::AudioEnvelope {
            duration_secs: 40.0,
            silences: vec![crate::audio::Span { start_secs: 10.0, end_secs: 12.0 }],
            ..Default::default()
        };
        let stretched = Segment { start_secs: 0.0, end_secs: 30.0, text: "So, let's go.".into() };
        let mut t = Transcript {
            language: Some("en".into()),
            segments: vec![stretched.clone(), Segment { start_secs: 31.0, end_secs: 33.0, text: "Look at that.".into() }],
            words: vec![
                Word { start_secs: 10.2, end_secs: 10.6, text: "So,".into(), confidence: 0.4, corrected: false },
                Word { start_secs: 10.7, end_secs: 11.2, text: "let's".into(), confidence: 0.4, corrected: false },
                Word { start_secs: 11.3, end_secs: 11.8, text: "go.".into(), confidence: 0.4, corrected: false },
                Word { start_secs: 31.0, end_secs: 31.4, text: "Look".into(), confidence: 0.9, corrected: false },
            ],
            ..Default::default()
        };
        drop_words_in_silence(&mut t, &env);
        assert_eq!(t.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), vec!["Look at that."], "the stretched sentence was never heard");
        // Without word times there is nothing to judge it by but its span.
        let mut bare = Transcript { segments: vec![stretched], ..Default::default() };
        drop_words_in_silence(&mut bare, &env);
        assert_eq!(bare.segments.len(), 1);
    }

    #[test]
    fn sound_captions_are_not_speech() {
        assert!(!is_speech("[Music]"));
        assert!(!is_speech("(coughing)"));
        assert!(!is_speech("*släpp*"));
        assert!(!is_speech(" ♪ "));
        assert!(is_speech("- Grab the gun when he was nine."));
        assert!(is_speech("Hej."));
    }

    #[test]
    fn tokens_group_into_words_at_the_spaces_and_specials_are_dropped() {
        let tokens = vec![
            tok("[_BEG_]", 0.0, 0.0, 1.0),
            tok(" Hel", 0.10, 0.20, 0.9),
            tok("lo", 0.20, 0.30, 0.7),
            tok(" world", 0.35, 0.60, 0.8),
            tok(".", 0.60, 0.62, 0.99),
            tok("<|en|>", 0.0, 0.0, 1.0),
        ];
        let w = group_words(&tokens);
        assert_eq!(w.len(), 2, "{w:?}");
        assert_eq!(w[0].text, "Hello");
        assert!((w[0].start_secs - 0.10).abs() < 1e-9 && (w[0].end_secs - 0.30).abs() < 1e-9);
        assert!((w[0].confidence - 0.8).abs() < 1e-6);
        assert_eq!(w[1].text, "world.");
        let t = Transcript { segments: vec![Segment { start_secs: 0.0, end_secs: 1.0, text: "Hello world.".into() }], words: w, ..Default::default() };
        assert_eq!(t.search("WORLD").len(), 1);
        assert_eq!(t.words_within(0.0, 0.25).len(), 1);
    }

    /// #499: what whisper writes where voice activity heard nobody is
    /// dropped, segment and words, unless it reads as speech; what
    /// overlaps speech stays.
    #[test]
    fn segments_and_words_outside_the_speech_are_dropped() {
        let seg = |a: f64, b: f64, t: &str| Segment { start_secs: a, end_secs: b, text: t.into() };
        let segments = vec![seg(0.0, 29.98, "I'm going to go to the next place."), seg(31.0, 33.0, "Over here."), seg(40.0, 42.0, "Thank you."), seg(50.0, 51.0, "Abort!")];
        // Three tokens in thirty seconds; one unsure token in two; two
        // sure ones in a second, under a score the detector missed.
        let tokens = vec![
            tok(" I'm", 3.0, 3.2, 0.7), tok(" going", 12.0, 12.2, 0.8), tok(" place", 25.0, 25.2, 0.8),
            tok(" Over", 31.1, 31.4, 0.9), tok(" here", 31.9, 32.2, 0.9),
            tok(" Thank", 40.2, 40.5, 0.3),
            tok(" Ab", 50.1, 50.3, 0.9), tok("ort", 50.3, 50.6, 0.85),
        ];
        let speech = [(31.2, 32.8)];
        let (kept, heard) = keep_spoken(&segments, &tokens, &speech);
        assert_eq!(kept.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), vec!["Over here.", "Abort!"]);
        assert_eq!(heard.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), vec![" Over", " here", " Ab", "ort"]);
    }

    #[test]
    fn a_sentence_repeated_across_a_file_is_a_hallucination() {
        let seg = |a: f64, t: &str| Segment { start_secs: a, end_secs: a + 2.0, text: t.into() };
        let music: Vec<Segment> = (0..10).map(|i| seg(i as f64 * 2.0, if i == 3 { "Inc. Rep seb" } else { "I'm not sure if you're a fan" })).collect();
        let dropped = hallucinated_spans(&music);
        assert_eq!(dropped.len(), 9, "{dropped:?}");
        let talk = vec![seg(0.0, "Hello"), seg(2.0, "This is a talk"), seg(4.0, "about files"), seg(6.0, "Hello"), seg(8.0, "and folders")];
        assert!(hallucinated_spans(&talk).is_empty());
        let stuck = vec![seg(0.0, "One"), seg(2.0, "Two"), seg(4.0, "Two"), seg(6.0, "Two"), seg(8.0, "Three"), seg(10.0, "Four"), seg(12.0, "Five"), seg(14.0, "Six")];
        assert_eq!(hallucinated_spans(&stuck).len(), 3);
    }
}

#[cfg(all(test, unix))]
mod worker_tests {
    use super::*;

    /// A stand-in worker: it reads the sound, then fails as whisper.cpp's
    /// DTW assertion does when asked for aligned word times, and answers
    /// with a transcript when not (#640). It exits with abort's status
    /// rather than raising the signal: a real abort leaves a core dump in
    /// the system's crash log for every run of the tests, and the worker's
    /// parent treats any failed exit alike.
    fn fake_worker(dir: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let reply = serde_json::to_string(&crate::worker::Reply::Done(Box::new(Transcript { duration_secs: 2.0, model: "fake".into(), ..Default::default() }))).unwrap();
        let script = dir.join("worker.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ncat > /dev/null\ncase \"$2\" in *'\"aligned\":true'*) echo 'GGML_ASSERT(filter_width < a->ne[2]) failed' >&2; exit 134;; esac\nprintf '%s' '{reply}'\n"),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    /// #640: a worker that dies on the aligned pass is run again without
    /// word alignment, and the editor gets its transcript; one that dies
    /// both times is an error, not a crash.
    #[test]
    fn a_worker_that_aborts_is_run_again_without_alignment() {
        let dir = std::env::temp_dir().join(format!("klipp-worker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let worker = fake_worker(&dir);
        let audio = vec![0.0f32; 32000];
        let t = transcribe_in(&worker, "fake", std::path::Path::new("/nowhere"), &audio, 2.0, &[], None).expect("a transcript after the second run");
        assert_eq!(t.model, "fake");

        let dead = dir.join("dead.sh");
        std::fs::write(&dead, "#!/bin/sh\ncat > /dev/null\nexit 134\n").unwrap();
        std::fs::set_permissions(&dead, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let err = transcribe_in(&dead, "fake", std::path::Path::new("/nowhere"), &audio, 2.0, &[], None).expect_err("dead twice");
        assert!(err.contains("twice"), "{err}");

        // An app upgraded while it ran starts the new file at its path.
        let deleted = std::path::PathBuf::from(format!("{} (deleted)", worker.display()));
        assert_eq!(crate::worker::worker_exe(&deleted), worker);
        assert_eq!(crate::worker::worker_exe(&worker), worker);
        assert!(worker_deadline(60.0) >= std::time::Duration::from_secs(240));
        std::fs::remove_dir_all(&dir).ok();
    }
}
