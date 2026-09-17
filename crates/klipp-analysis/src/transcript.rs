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
pub const VERSION: u32 = 3;
/// The manifest entry the analyser runs with.
pub const MODEL_ID: &str = "whisper_base_ggml";

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
    let out = std::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-nostats", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-vn", "-sn", "-ac", "1", "-ar", "16000", "-f", "f32le", "-"])
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("could not run {}: {e}", ffmpeg.display()))?;
    if !out.status.success() {
        return Err(format!("ffmpeg could not decode the sound: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(out.stdout.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
}

/// The model file in the app's model cache, if it is there.
pub fn model_path() -> Option<std::path::PathBuf> {
    let manifest = klipp_ml::manifest::load_effective().ok()?;
    let entry = manifest.get(MODEL_ID)?;
    let cache = klipp_ml::cache::ModelCache::open().ok()?;
    // Presence only: the download verified the bytes, and hashing
    // 148 MB on every import would not be worth what it proves.
    let path = cache.path_for(entry);
    path.is_file().then_some(path)
}

/// The model file, downloaded and verified against the manifest when
/// `download` and it is not there yet.
pub fn ensure_model(download: bool) -> Result<std::path::PathBuf, String> {
    if let Some(p) = model_path() {
        return Ok(p);
    }
    if !download {
        return Err(format!("the transcription model {MODEL_ID} is not downloaded; ask with download_models"));
    }
    let manifest = klipp_ml::manifest::load_effective().map_err(|e| format!("model manifest: {e:?}"))?;
    let entry = manifest.get(MODEL_ID).ok_or_else(|| format!("no manifest entry {MODEL_ID}"))?;
    let cache = klipp_ml::cache::ModelCache::open().map_err(|e| format!("model cache: {e:?}"))?;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut progress = klipp_ml::download::NoOpProgress;
    klipp_ml::download::download_to_cache(&cache, entry, &mut progress, &cancel).map_err(|e| format!("download of {MODEL_ID}: {e:?}"))
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
        item.has_audio && item.duration.to_secs_f64() > 0.0 && model_path().is_some()
    }
    fn run(&self, item: &klipp_media::MediaItem, ctx: &crate::AnalysisCtx) -> Result<serde_json::Value, String> {
        let model = ensure_model(false)?;
        let audio = decode_audio(&ctx.ffmpeg, &item.path)?;
        if audio.len() < 16000 / 2 {
            let t = Transcript { duration_secs: item.duration.to_secs_f64(), model: MODEL_ID.into(), ..Default::default() };
            return serde_json::to_value(t).map_err(|e| e.to_string());
        }
        let mut transcript = transcribe(&model, &audio, item.duration.to_secs_f64())?;
        // The sound analyser has already found where the file is silent;
        // words whisper places there are its invention, whatever it
        // thinks of them.
        if let Some(env) = crate::store::key_for(&item.path)
            .and_then(|key| crate::store::load(&key, crate::audio::ID, crate::audio::VERSION))
            .and_then(|v| serde_json::from_value::<crate::audio::AudioEnvelope>(v).ok())
        {
            drop_words_in_silence(&mut transcript, &env);
        }
        serde_json::to_value(transcript).map_err(|e| e.to_string())
    }
}

fn transcribe(model: &std::path::Path, audio: &[f32], duration_secs: f64) -> Result<Transcript, String> {
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};
    whisper_rs::install_logging_hooks();
    let mut cparams = WhisperContextParameters::default();
    cparams.use_gpu(false);
    let ctx = WhisperContext::new_with_params(model, cparams).map_err(|e| format!("whisper model: {e}"))?;
    let mut state = ctx.create_state().map_err(|e| format!("whisper state: {e}"))?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8) as i32;
    params.set_n_threads(threads);
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
    state.full(params, audio).map_err(|e| format!("whisper run: {e}"))?;

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
            tokens.push(Token { text, t0: d.t0 as f64 / 100.0, t1: d.t1 as f64 / 100.0, p: d.p });
        }
    }
    let words = group_words(&tokens);
    // A language detected over silence is a guess; say nothing instead.
    let language = if segments.is_empty() { None } else { lang };
    Ok(Transcript { duration_secs, language, model: MODEL_ID.into(), segments, words })
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
}
