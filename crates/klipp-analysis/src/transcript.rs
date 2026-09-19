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
pub const VERSION: u32 = 5;

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
            words.push(Word { start_secs: t.t0, end_secs: t.t1, text: t.text.trim().to_string(), confidence: t.p });
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
    cmd.args(["-hide_banner", "-nostats", "-loglevel", "error", "-i"]).arg(path).args(["-vn", "-sn", "-ac", "1", "-ar", "16000", "-f", "f32le", "-"]);
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

pub fn model_path() -> Option<std::path::PathBuf> {
    installed_model().map(|(_, p)| p)
}

/// The model to run with. With `download`, the best one, fetched and
/// verified against the manifest if it is not there yet; without, the
/// best one installed.
pub fn ensure_model(download: bool) -> Result<(&'static str, std::path::PathBuf), String> {
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
        let mut took = 0;
        for term in &terms {
            let whole: String = plain(&term.join(""));
            // The same letters, over up to three words of the transcript.
            for n in (1..=3usize.min(t.words.len() - i)).rev() {
                let heard: String = t.words[i..i + n].iter().map(|w| plain(&w.text)).collect();
                if heard == whole && whole.chars().count() >= 3 {
                    took = n;
                    break;
                }
            }
            // Word for word, by ear.
            if took == 0 && i + term.len() <= t.words.len() {
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
                    t.words.splice(i..i + took, [Word { start_secs: start, end_secs: end, text: now, confidence }]);
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
    /// Made by a lesser model than the one installed now, or with
    /// another list of how things are spelt.
    fn stale(&self, stored: &serde_json::Value) -> bool {
        let model = stored.get("model").and_then(|m| m.as_str()).unwrap_or("");
        let words: Vec<String> = stored.get("vocabulary").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
        // Spellings asked for now that the stored one was not given. One
        // made with more is as good; with none asked for, any will do.
        installed_model().is_some_and(|(best, _)| best != model) || lacks_spellings(&words, &vocabulary())
    }
    fn run(&self, item: &klipp_media::MediaItem, ctx: &crate::AnalysisCtx) -> Result<serde_json::Value, String> {
        let (model_id, model) = ensure_model(false)?;
        let words = vocabulary();
        let audio = decode_audio(&ctx.ffmpeg, &item.path)?;
        if audio.len() < 16000 / 2 {
            let t = Transcript { duration_secs: item.duration.to_secs_f64(), model: model_id.into(), vocabulary: words, ..Default::default() };
            return serde_json::to_value(t).map_err(|e| e.to_string());
        }
        let mut transcript = transcribe(model_id, &model, &audio, item.duration.to_secs_f64(), &words)?;
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
        serde_json::to_value(transcript).map_err(|e| e.to_string())
    }
}

fn transcribe(model_id: &str, model: &std::path::Path, audio: &[f32], duration_secs: f64, words: &[String]) -> Result<Transcript, String> {
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
    cparams.dtw_parameters(DtwParameters { mode: DtwMode::ModelPreset { model_preset: preset() }, ..Default::default() });
    let ctx = match WhisperContext::new_with_params(model, cparams) {
        Ok(ctx) => ctx,
        // A GPU that will not have it (no memory left, a driver without
        // what the shaders need) is not the end: the CPU does the same
        // work in about a third of the file's length.
        Err(e) if on_gpu => {
            tracing::warn!("whisper could not start on the GPU ({e}); running on the CPU");
            let mut cpu = WhisperContextParameters::default();
            cpu.use_gpu(false);
            cpu.dtw_parameters(DtwParameters { mode: DtwMode::ModelPreset { model_preset: preset() }, ..Default::default() });
            WhisperContext::new_with_params(model, cpu).map_err(|e| format!("whisper model: {e}"))?
        }
        Err(e) => return Err(format!("whisper model: {e}")),
    };
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
    params.set_language(Some("auto"));
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
    Ok(Transcript { duration_secs, language, model: model_id.into(), vocabulary: words.to_vec(), segments, words: spoken })
}

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
    t.segments.retain(|s| !silent(s.start_secs, s.end_secs));
    t.words.retain(|w| !silent(w.start_secs, w.end_secs));
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
            let words = text.split(' ').enumerate().map(|(k, w)| Word { start_secs: k as f64, end_secs: k as f64 + 0.5, text: w.into(), confidence: 1.0 }).collect();
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
                Word { start_secs: 1.0, end_secs: 2.0, text: "encounters".into(), confidence: 0.5 },
                Word { start_secs: 5.0, end_secs: 5.5, text: "hello".into(), confidence: 0.9 },
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
