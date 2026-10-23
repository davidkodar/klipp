//! Narration: a scratch voice from text, through the `piper` program and
//! a voice from the model manifest.
//!
//! The control socket's `narrate` and the Narrate window both come here,
//! so the same words, voice and delivery make the same file, and a take
//! made once is found again rather than made twice.

use std::path::{Path, PathBuf};

pub mod kokoro;

/// The voice a take is spoken in when none is named: Kokoro's Heart,
/// natural-sounding and free for any use. Where Kokoro cannot run (a build
/// without ONNX Runtime, or no espeak-ng) [`default_voice`] falls back to
/// [`FALLBACK_VOICE`].
pub const DEFAULT_VOICE: &str = "kokoro_voice_af_heart";

/// The default where Kokoro cannot run: a public-domain Piper voice.
pub const FALLBACK_VOICE: &str = "piper_en_us_ljspeech_high";

/// The voice a take is spoken in when none is named, on this machine.
pub fn default_voice() -> &'static str {
    if cfg!(feature = "kokoro") && kokoro::espeak().is_ok() {
        DEFAULT_VOICE
    } else {
        FALLBACK_VOICE
    }
}

/// The voice takes were named without: the default before the voices
/// were checked for their licences. Its takes keep the names they have,
/// and every other voice is in its take's name.
const UNNAMED_VOICE: &str = "piper_en_us_lessac_medium";

/// Which engine speaks a voice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// Kokoro-82M: natural, one shared model, needs espeak-ng.
    Kokoro,
    /// Piper: lighter and more robotic, a model per voice, needs piper.
    Piper,
}

/// A voice the manifest offers, as a person chooses it.
#[derive(Debug, Clone, PartialEq)]
pub struct Voice {
    /// The manifest id, which `narrate` takes as `voice`.
    pub id: String,
    /// As the manifest names it.
    pub name: String,
    /// Its own name: "Heart", "George", "LJSpeech".
    pub label: String,
    /// Who it is, in a few words: "female, US English".
    pub about: String,
    pub engine: Engine,
    /// What is still to download before it speaks, in bytes.
    pub size_bytes: u64,
    /// Everything it needs is in the model cache.
    pub installed: bool,
    /// The manifest ids still to download before it speaks: for a Kokoro
    /// voice the model and every Kokoro voice, fetched as one.
    pub missing: Vec<String>,
    /// What the voice may be used for, as the manifest says.
    pub license: String,
    /// Its terms allow a take in a piece that is sold or paid for.
    pub commercial_use: bool,
    /// A few seconds of it, bundled, to hear before anything downloads.
    pub sample: Option<&'static [u8]>,
}

/// What to say, and how.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    pub text: &'a str,
    /// A manifest voice id; `None` is [`default_voice`].
    pub voice: Option<&'a str>,
    /// Above 1 quicker, below 1 slower; `None` is as the voice speaks.
    pub speed: Option<f32>,
    /// The silence between sentences; `None` is the voice's own.
    pub pause_secs: Option<f32>,
    /// How long the take must last, seconds (#112): spoken again at
    /// another speed until it lands within a frame of it, from `speed`
    /// (or as the voice speaks). `None` is as long as it comes out.
    pub fit_secs: Option<f32>,
    /// How much a Piper voice varies its delivery, 0 (flat, the same line
    /// said the same way every time) to 2, 1 as the voice does (#112).
    /// Kokoro has no such control.
    pub variation: Option<f32>,
    /// Fetch a voice that is not downloaded yet.
    pub download: bool,
    /// Where to write the take; `None` names it in `dir` after its words.
    pub path: Option<PathBuf>,
    /// The narration folder a take is named into.
    pub dir: &'a Path,
}

/// A bundled sample of a Kokoro voice ("Hi, I'm Heart. This is how your
/// narration will sound."), made with the voice itself.
pub fn sample(voice: &str) -> Option<&'static [u8]> {
    Some(match voice.strip_prefix(kokoro::VOICE_PREFIX)? {
        "af_heart" => include_bytes!("../../../../assets/narration-samples/af_heart.mp3"),
        "af_bella" => include_bytes!("../../../../assets/narration-samples/af_bella.mp3"),
        "af_nicole" => include_bytes!("../../../../assets/narration-samples/af_nicole.mp3"),
        "am_michael" => include_bytes!("../../../../assets/narration-samples/am_michael.mp3"),
        "am_fenrir" => include_bytes!("../../../../assets/narration-samples/am_fenrir.mp3"),
        "am_puck" => include_bytes!("../../../../assets/narration-samples/am_puck.mp3"),
        "bf_emma" => include_bytes!("../../../../assets/narration-samples/bf_emma.mp3"),
        "bf_isabella" => include_bytes!("../../../../assets/narration-samples/bf_isabella.mp3"),
        "bm_george" => include_bytes!("../../../../assets/narration-samples/bm_george.mp3"),
        "bm_fable" => include_bytes!("../../../../assets/narration-samples/bm_fable.mp3"),
        _ => return None,
    })
}

/// A voice's own name and a few words about it, from its id and its
/// manifest name: `kokoro_voice_bm_george` is "George", "male, UK English";
/// "Piper voice — English (US), Kristin, medium" is "Kristin", "English (US)".
fn label_of(id: &str, display_name: &str) -> (String, String) {
    if let Some(v) = id.strip_prefix(kokoro::VOICE_PREFIX) {
        let mut chars = v.chars();
        let accent = match chars.next() { Some('b') => "UK English", _ => "US English" };
        let gender = match chars.next() { Some('m') => "male", _ => "female" };
        let name = v.split('_').nth(1).unwrap_or(v);
        let name = name[..1].to_uppercase() + &name[1..];
        return (name, format!("{gender}, {accent}"));
    }
    let parts: Vec<&str> = display_name.split(" — ").nth(1).unwrap_or(display_name).split(", ").collect();
    let label = parts.get(1).copied().unwrap_or(display_name).to_string();
    (label, parts.first().copied().unwrap_or("").to_string())
}

/// Whether the AI model manager lists this entry as a row of its own. A
/// Piper voice's settings file and each small Kokoro voice come with the
/// voice or the Kokoro download they belong to, so they are not rows.
pub fn shown_in_model_manager(id: &str) -> bool {
    !(kokoro::is_kokoro(id) || (id.starts_with("piper_") && id.ends_with("_config")))
}

/// The files that come with a model manager row and go when it is
/// uninstalled: with the Kokoro download, every Kokoro voice; with a
/// Piper voice, its settings and the copy of them made beside the model.
pub fn extra_files(cache: &klipp_ml::cache::ModelCache, manifest: &klipp_ml::manifest::ModelManifest, id: &str) -> Vec<PathBuf> {
    if id == kokoro::MODEL {
        return manifest.models.iter().filter(|e| kokoro::is_kokoro(&e.id)).map(|e| cache.path_for(e)).collect();
    }
    if id.starts_with("piper_") && !id.ends_with("_config") {
        let mut beside = cache.path_for_id(id).into_os_string();
        beside.push(".json");
        return vec![cache.path_for_id(&format!("{id}_config")), PathBuf::from(beside)];
    }
    Vec::new()
}

/// Every voice in the manifest: the Kokoro ones first, the default at
/// the top, then Piper's.
pub fn voices() -> Result<Vec<Voice>, String> {
    let manifest = klipp_ml::manifest::load_effective().map_err(|e| format!("model manifest: {e:?}"))?;
    let cache = klipp_ml::cache::ModelCache::open().map_err(|e| format!("model cache: {e:?}"))?;
    let absent = |id: &str| manifest.get(id).filter(|e| !cache.path_for(e).is_file());
    // Every Kokoro voice needs the model, and they are fetched as one.
    let kokoro_bundle: Vec<&klipp_ml::manifest::ModelEntry> = manifest
        .models
        .iter()
        .filter(|e| e.id == kokoro::MODEL || kokoro::is_kokoro(&e.id))
        .filter(|e| !cache.path_for(e).is_file())
        .collect();
    let mut voices: Vec<Voice> = manifest
        .models
        .iter()
        .filter(|e| e.kind == klipp_ml::manifest::ModelKind::Voice && !e.id.ends_with("_config") && e.id != kokoro::MODEL)
        .map(|e| {
            let (label, about) = label_of(&e.id, &e.display_name);
            let (engine, missing): (Engine, Vec<&klipp_ml::manifest::ModelEntry>) = if kokoro::is_kokoro(&e.id) {
                (Engine::Kokoro, kokoro_bundle.clone())
            } else {
                (Engine::Piper, [absent(&e.id), absent(&format!("{}_config", e.id))].into_iter().flatten().collect())
            };
            Voice {
                id: e.id.clone(),
                name: e.display_name.clone(),
                label,
                about,
                engine,
                size_bytes: missing.iter().map(|m| m.size_bytes).sum(),
                installed: missing.is_empty(),
                missing: missing.iter().map(|m| m.id.clone()).collect(),
                license: e.license.clone(),
                commercial_use: commercial_use(&e.license),
                sample: sample(&e.id),
            }
        })
        .collect();
    voices.sort_by_key(|v| (v.id != DEFAULT_VOICE, v.engine != Engine::Kokoro, v.id.clone()));
    Ok(voices)
}

/// Download `ids` through the verified cache, counting the bytes into
/// `done` as they come and stopping when `cancel` is set. What finished
/// before a cancel stays; the next download fetches only the rest.
pub fn download(ids: &[String], done: &std::sync::atomic::AtomicU64, cancel: &std::sync::atomic::AtomicBool) -> Result<(), String> {
    struct Count<'a>(&'a std::sync::atomic::AtomicU64);
    impl klipp_ml::download::ProgressSink for Count<'_> {
        fn started(&mut self, _: u64) {}
        fn advance(&mut self, bytes: u64) {
            self.0.fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
        }
        fn finished(&mut self) {}
    }
    let manifest = klipp_ml::manifest::load_effective().map_err(|e| format!("model manifest: {e:?}"))?;
    let cache = klipp_ml::cache::ModelCache::open().map_err(|e| format!("model cache: {e:?}"))?;
    for id in ids {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let entry = manifest.get(id).ok_or_else(|| format!("no {id} in the model manifest"))?;
        if cache.path_for(entry).is_file() {
            continue;
        }
        klipp_ml::download::download_to_cache(&cache, entry, &mut Count(done), cancel).map_err(|e| format!("{}: {e:?}", entry.display_name))?;
    }
    Ok(())
}

/// Why `engine` cannot speak on this machine, or `None` when it can.
pub fn engine_missing(engine: Engine) -> Option<String> {
    match engine {
        Engine::Kokoro if !cfg!(feature = "kokoro") => Some("this build of Klipp has no ONNX Runtime, which Kokoro voices need".into()),
        Engine::Kokoro => kokoro::espeak().err(),
        Engine::Piper => piper().err(),
    }
}

/// Whether a voice's licence allows commercial work: anything but one
/// marked research only or non-commercial.
pub fn commercial_use(license: &str) -> bool {
    let l = license.to_ascii_lowercase();
    !(l.contains("research only") || l.contains("non-commercial") || l.contains("noncommercial") || l.contains("-nc"))
}

/// The piper program: `KLIPP_PIPER`, else `piper` or `piper-tts` on the
/// path.
pub fn piper() -> Result<PathBuf, String> {
    std::env::var("KLIPP_PIPER")
        .ok()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| which("piper"))
        .or_else(|| which("piper-tts"))
        .ok_or_else(|| "the piper program is not installed; on Arch: pacman -S piper-tts, elsewhere pip install piper-tts, or set KLIPP_PIPER to the program".to_string())
}

/// A manifest entry's file in the model cache, fetched first with
/// `download`.
fn cached(id: &str, download: bool) -> Result<PathBuf, String> {
    let manifest = klipp_ml::manifest::load_effective().map_err(|e| format!("model manifest: {e:?}"))?;
    let cache = klipp_ml::cache::ModelCache::open().map_err(|e| format!("model cache: {e:?}"))?;
    let entry = manifest.get(id).ok_or_else(|| format!("no {id} in the model manifest"))?;
    let p = cache.path_for(entry);
    if !p.is_file() {
        if !download {
            return Err(format!("{} is not downloaded ({} MB); ask with download_models true, or download it in Window > Narrate", entry.display_name, entry.size_bytes.div_ceil(1_000_000)));
        }
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let mut progress = klipp_ml::download::NoOpProgress;
        klipp_ml::download::download_to_cache(&cache, entry, &mut progress, &cancel).map_err(|e| format!("download of {id}: {e:?}"))?;
    }
    Ok(cache.path_for(entry))
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|d| d.join(name)).find(|p| p.is_file()))
}

/// Piper's arguments for a delivery: `speed` above 1 quicker and below
/// 1 slower (Piper's length scale is its inverse), `pause_secs` the
/// silence it leaves between sentences. Neither given: Piper's own, and
/// no arguments, so earlier takes keep their names (#112).
pub fn delivery_args(speed: Option<f32>, pause_secs: Option<f32>) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    if let Some(s) = speed {
        if !(0.5..=2.0).contains(&s) {
            return Err(format!("a speed of {s} is outside 0.5 to 2 (1 is as the voice speaks)"));
        }
        args.push("--length_scale".to_string());
        args.push(format!("{:.3}", 1.0 / s));
    }
    if let Some(p) = pause_secs {
        if !(0.0..=3.0).contains(&p) {
            return Err(format!("a pause of {p} s between sentences is outside 0 to 3"));
        }
        args.push("--sentence_silence".to_string());
        args.push(format!("{p:.3}"));
    }
    Ok(args)
}

/// Piper's arguments for `variation` (#112): its noise scale and noise
/// width (0.667 and 0.8 as a voice comes) scaled together. None given:
/// no arguments, and the take keeps its name.
pub fn variation_args(variation: Option<f32>) -> Result<Vec<String>, String> {
    let Some(v) = variation else { return Ok(Vec::new()) };
    if !(0.0..=2.0).contains(&v) {
        return Err(format!("a variation of {v} is outside 0 to 2 (1 is as the voice speaks)"));
    }
    Ok(vec!["--noise_scale".into(), format!("{:.3}", 0.667 * v), "--noise_w".into(), format!("{:.3}", 0.8 * v)])
}

/// How far from its target a fitted take may land: a frame at 30 fps.
pub const FIT_TOLERANCE_SECS: f64 = 1.0 / 30.0;

/// The speed a take lasts `target` seconds at (#112): `speak(speed)` says
/// it at a speed and gives its length; speech scales close to inversely
/// with speed, so each pass corrects by the ratio of what it got to the
/// target, from `start`, held to 0.5 to 2, until within
/// [`FIT_TOLERANCE_SECS`], six passes at most. The speed of the last pass,
/// and how long it was.
pub fn fit_speed(target: f64, start: f32, mut speak: impl FnMut(f32) -> Result<f64, String>) -> Result<(f32, f64), String> {
    if !(0.2..=600.0).contains(&target) {
        return Err(format!("a take of {target} s is outside 0.2 to 600"));
    }
    let mut speed = start.clamp(0.5, 2.0);
    let mut got = speak(speed)?;
    for _ in 0..5 {
        if (got - target).abs() <= FIT_TOLERANCE_SECS {
            break;
        }
        let next = (speed as f64 * got / target).clamp(0.5, 2.0) as f32;
        if (next - speed).abs() < 1e-4 {
            // Held at a limit: as close as the voice can be made.
            break;
        }
        speed = next;
        got = speak(speed)?;
    }
    Ok((speed, got))
}

/// How long a WAV file lasts, seconds: its data chunk over its byte rate.
pub fn wav_secs(path: &Path) -> Result<f64, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(format!("{} is not a WAV file", path.display()));
    }
    let (mut at, mut byte_rate, mut data) = (12usize, 0u32, None);
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]]) as usize;
        if id == b"fmt " && at + 20 <= bytes.len() {
            byte_rate = u32::from_le_bytes([bytes[at + 16], bytes[at + 17], bytes[at + 18], bytes[at + 19]]);
        }
        if id == b"data" {
            // A streamed WAV may say 0 or more than it holds: what is there.
            data = Some(if size == 0 || at + 8 + size > bytes.len() { bytes.len() - at - 8 } else { size });
            break;
        }
        at += 8 + size + (size & 1);
    }
    match (byte_rate, data) {
        (r, Some(d)) if r > 0 => Ok(d as f64 / r as f64),
        _ => Err(format!("{} has no audio to time", path.display())),
    }
}

/// A short hash of a text, for a file name that follows its words.
fn text_hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// The hash in a take's file name: the text, the delivery when one was
/// asked for, and the voice. A slower take or another voice is a file of
/// its own, while a Lessac take, the voice takes were first named
/// without, keeps the name it always had.
pub fn take_key(text: &str, voice: &str, delivery: &[String]) -> u64 {
    let mut extra = delivery.join(" ");
    if voice != UNNAMED_VOICE {
        extra = format!("{voice}\u{0}{extra}");
    }
    if extra.is_empty() {
        text_hash(text)
    } else {
        text_hash(&format!("{text}\u{0}{extra}"))
    }
}

/// Where a take is written in `dir`: its first words, and [`take_key`].
pub fn take_path(dir: &Path, text: &str, voice: &str, delivery: &[String]) -> PathBuf {
    let stem: String = text.chars().filter(|c| c.is_alphanumeric() || *c == ' ').take(40).collect::<String>().trim().replace(' ', "-").to_lowercase();
    dir.join(format!("{}-{:x}.wav", if stem.is_empty() { "narration".into() } else { stem }, take_key(text, voice, delivery)))
}

/// A voice's model and settings in the model cache, downloaded first
/// with `download`.
pub fn voice_files(voice: &str, download: bool) -> Result<(PathBuf, PathBuf), String> {
    let manifest = klipp_ml::manifest::load_effective().map_err(|e| format!("model manifest: {e:?}"))?;
    let cache = klipp_ml::cache::ModelCache::open().map_err(|e| format!("model cache: {e:?}"))?;
    let ids = [voice.to_string(), format!("{voice}_config")];
    let mut paths = Vec::new();
    for id in &ids {
        let entry = manifest.get(id).ok_or_else(|| {
            let voices: Vec<&str> = manifest.models.iter().filter(|e| e.kind == klipp_ml::manifest::ModelKind::Voice && !e.id.ends_with("_config")).map(|e| e.id.as_str()).collect();
            format!("no voice {id} in the manifest; the voices are {}", voices.join(", "))
        })?;
        let p = cache.path_for(entry);
        if !p.is_file() {
            if !download {
                return Err(format!("the voice {voice} is not downloaded ({} MB); ask with download_models true, or download it in Preferences", entry.size_bytes / 1_000_000));
            }
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let mut progress = klipp_ml::download::NoOpProgress;
            klipp_ml::download::download_to_cache(&cache, entry, &mut progress, &cancel).map_err(|e| format!("download of {id}: {e:?}"))?;
        }
        paths.push(cache.path_for(entry));
    }
    let (model, config) = (paths.remove(0), paths.remove(0));
    config_beside(&model, &config)?;
    Ok((model, config))
}

/// Put the voice's config where piper also looks for it: `<model>.json`
/// beside the model. piper-tts 1.8 takes `--config` and then loads the
/// voice from the model path alone, which reads `<model>.json`; the cache
/// keeps the config under its own id, so narration failed with the file
/// not found even with everything installed (#113). `--config` is still
/// passed, for a piper that honours it.
fn config_beside(model: &Path, config: &Path) -> Result<(), String> {
    let mut beside = model.as_os_str().to_owned();
    beside.push(".json");
    let beside = PathBuf::from(beside);
    if beside.is_file() {
        return Ok(());
    }
    std::fs::copy(config, &beside).map(|_| ()).map_err(|e| format!("the voice config for piper at {}: {e}", beside.display()))
}

/// Speak `req` into a WAV file and say where it is. Blocks for as long
/// as piper takes (and a download, with `download`), so a caller on the
/// UI thread runs it on a thread of its own.
pub fn synthesize(req: &Request) -> Result<PathBuf, String> {
    let text = req.text.trim();
    if text.is_empty() {
        return Err("narrate needs the words to say".into());
    }
    let mut delivery = delivery_args(req.speed, req.pause_secs)?;
    let voice = match req.voice {
        Some(v) => v,
        None => default_voice(),
    };
    if req.variation.is_some() && kokoro::is_kokoro(voice) {
        return Err(format!("{voice} is a Kokoro voice, which has no variation control; a Piper voice has"));
    }
    delivery.extend(variation_args(req.variation)?);
    // A fitted take is its own file, named for its target.
    if let Some(t) = req.fit_secs {
        delivery.push(format!("fit={t:.3}"));
    }
    let out = match &req.path {
        Some(p) => p.clone(),
        None => {
            std::fs::create_dir_all(req.dir).map_err(|e| format!("{}: {e}", req.dir.display()))?;
            take_path(req.dir, text, voice, &delivery)
        }
    };
    // Fitted to a length: said at a speed until it lasts it (#112).
    if let Some(target) = req.fit_secs {
        let mut pass = 0u32;
        let trial = |pass: u32| out.with_extension(format!("fit{pass}.wav"));
        let (_, _) = fit_speed(target as f64, req.speed.unwrap_or(1.0), |speed| {
            pass += 1;
            let take = trial(pass);
            synthesize(&Request { text: req.text, voice: req.voice, speed: Some(speed), pause_secs: req.pause_secs, fit_secs: None, variation: req.variation, download: req.download, path: Some(take.clone()), dir: req.dir })?;
            wav_secs(&take)
        })?;
        std::fs::rename(trial(pass), &out).map_err(|e| format!("{}: {e}", out.display()))?;
        for p in 1..pass {
            let _ = std::fs::remove_file(trial(p));
        }
        return Ok(out);
    }
    if kokoro::is_kokoro(voice) {
        kokoro::speak(text, voice, req.speed, req.pause_secs, req.download, &out)?;
        finish_take(&out)?;
        return Ok(out);
    }
    let piper = piper()?;
    let (model, config) = voice_files(voice, req.download)?;
    let mut child = std::process::Command::new(&piper)
        .arg("--model")
        .arg(&model)
        .arg("--config")
        .arg(&config)
        .args(&delivery)
        .arg("--output_file")
        .arg(&out)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {}: {e}", piper.display()))?;
    {
        use std::io::Write as _;
        let mut stdin = child.stdin.take().ok_or("piper's stdin")?;
        stdin.write_all(text.as_bytes()).and_then(|_| stdin.write_all(b"\n")).map_err(|e| format!("writing to piper: {e}"))?;
    }
    let done = child.wait_with_output().map_err(|e| format!("piper: {e}"))?;
    if !done.status.success() || !out.is_file() {
        return Err(format!("piper failed ({}): {}", done.status, String::from_utf8_lossy(&done.stderr).trim()));
    }
    finish_take(&out)?;
    Ok(out)
}

/// A RIFF WAVE file's channels, rate and data byte range when it is 16-bit
/// PCM; `None` for anything else.
fn pcm16_layout(bytes: &[u8]) -> Option<(u16, u32, std::ops::Range<usize>)> {
    if bytes.get(0..4)? != b"RIFF" || bytes.get(8..12)? != b"WAVE" {
        return None;
    }
    let (mut at, mut fmt) = (12usize, None);
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        let body = at + 8;
        if id == b"fmt " {
            let f = bytes.get(body..body + 16)?;
            let (format, channels, rate, bits) = (u16::from_le_bytes([f[0], f[1]]), u16::from_le_bytes([f[2], f[3]]), u32::from_le_bytes([f[4], f[5], f[6], f[7]]), u16::from_le_bytes([f[14], f[15]]));
            fmt = (format == 1 && bits == 16 && channels > 0).then_some((channels, rate));
        } else if id == b"data" {
            let (channels, rate) = fmt?;
            let end = (body + size).min(bytes.len());
            return Some((channels, rate, body..body + (end - body) / 2 * 2));
        }
        at = body + size + size % 2;
    }
    None
}

/// A 2-pole Butterworth high-pass (RBJ's cookbook, Q = 1/sqrt 2).
#[derive(Clone, Copy)]
struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    x: [f32; 2],
    y: [f32; 2],
}

impl Biquad {
    fn high_pass(hz: f32, rate: f32) -> Self {
        let w = std::f32::consts::TAU * hz / rate;
        let alpha = w.sin() / std::f32::consts::SQRT_2;
        let (cos, a0) = (w.cos(), 1.0 + alpha);
        Biquad { b: [(1.0 + cos) / 2.0 / a0, -(1.0 + cos) / a0, (1.0 + cos) / 2.0 / a0], a: [-2.0 * cos / a0, (1.0 - alpha) / a0], x: [0.0; 2], y: [0.0; 2] }
    }

    fn run(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1] - self.a[0] * self.y[0] - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// Where a take's low end is cut: under a voice's fundamental. A synthetic
/// voice (Piper's in particular) carries energy under 60 Hz that nothing
/// speaks there, and that the sound check reads as wind or handling (#2482).
const TAKE_HIGH_PASS_HZ: f32 = 90.0;

/// The true peak a take is brought down to, dBTP: Piper writes its takes
/// at full scale, and a lossy encode of a take at 0 dBTP clips (#2482).
const TAKE_CEILING_DBTP: f32 = -1.0;

/// A take as it is kept: its low end under [`TAKE_HIGH_PASS_HZ`] taken
/// off (three 2-pole high-passes, 36 dB an octave), then brought down,
/// never up, to a true peak of [`TAKE_CEILING_DBTP`]. A 16-bit PCM WAV,
/// rewritten in place; another format is left as it is.
fn finish_take(path: &Path) -> Result<(), String> {
    let mut bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let Some((channels, rate, data)) = pcm16_layout(&bytes) else { return Ok(()) };
    let mut samples: Vec<f32> = bytes[data.clone()].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
    let ch = channels as usize;
    for c in 0..ch {
        let mut stages = [Biquad::high_pass(TAKE_HIGH_PASS_HZ, rate as f32); 3];
        for frame in samples.chunks_exact_mut(ch) {
            frame[c] = stages.iter_mut().fold(frame[c], |x, st| st.run(x));
        }
    }
    let mut meter = crate::TruePeakDetector::new(rate);
    let mut peak = 0.0f32;
    for frame in samples.chunks_exact(ch) {
        let (l, r) = (frame[0], frame[ch.min(2) - 1]);
        peak = peak.max(meter.process(l, r));
    }
    peak = peak.max(meter.flush());
    let ceiling = 10f32.powf(TAKE_CEILING_DBTP / 20.0);
    let gain = if peak > ceiling { ceiling / peak } else { 1.0 };
    for (dst, s) in bytes[data].chunks_exact_mut(2).zip(&samples) {
        dst.copy_from_slice(&(((s * gain).clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
    }
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}


#[cfg(test)]
mod tests {
    use super::{commercial_use, config_beside, delivery_args, take_key, text_hash, DEFAULT_VOICE, UNNAMED_VOICE};

    /// A 16-bit mono WAV at `rate` of `samples`.
    fn wav(rate: u32, samples: &[f32]) -> Vec<u8> {
        let data = samples.len() as u32 * 2;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data.to_le_bytes());
        for s in samples {
            b.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
        }
        b
    }

    /// The level of `hz` in `samples`, dB (a Goertzel bin).
    fn level_db(samples: &[f32], rate: f32, hz: f32) -> f32 {
        let k = 2.0 * (std::f32::consts::TAU * hz / rate).cos();
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for &x in samples {
            let s0 = x + k * s1 - s2;
            (s2, s1) = (s1, s0);
        }
        10.0 * (s1 * s1 + s2 * s2 - k * s1 * s2).max(1e-20).log10()
    }

    fn read(path: &std::path::Path) -> Vec<f32> {
        let b = std::fs::read(path).unwrap();
        let (_, _, data) = super::pcm16_layout(&b).unwrap();
        b[data].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).collect()
    }

    /// #2482: a take at full scale, as Piper writes one, with energy under
    /// 60 Hz, comes back with its true peak at -1 dBTP or under and its low
    /// end down by 25 dB against the voice; a quiet take is not raised.
    #[test]
    fn a_take_gets_headroom_and_loses_its_low_end() {
        let rate = 22_050u32;
        let tone = |hz: f32, i: usize| (std::f32::consts::TAU * hz * i as f32 / rate as f32).sin();
        let loud: Vec<f32> = (0..rate as usize * 2).map(|i| 0.7 * tone(300.0, i) + 0.3 * tone(40.0, i)).collect();
        let dir = std::env::temp_dir().join(format!("klipp_take_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("take.wav");
        std::fs::write(&path, wav(rate, &loud)).unwrap();
        super::finish_take(&path).unwrap();
        let after = read(&path);
        let mut meter = crate::TruePeakDetector::new(rate);
        let peak = after.iter().map(|s| meter.process(*s, *s)).fold(0.0f32, f32::max).max(meter.flush());
        assert!(20.0 * peak.log10() <= -0.95, "true peak {:.2} dBTP", 20.0 * peak.log10());
        let tail = &after[rate as usize..];
        let (before_tail, r) = (&loud[rate as usize..], rate as f32);
        let low_drop = (level_db(before_tail, r, 40.0) - level_db(before_tail, r, 300.0)) - (level_db(tail, r, 40.0) - level_db(tail, r, 300.0));
        assert!(low_drop >= 25.0, "the 40 Hz band came down {low_drop:.1} dB against the voice");

        // A quiet take keeps its level: nothing is raised.
        let quiet: Vec<f32> = (0..rate as usize).map(|i| 0.1 * tone(300.0, i)).collect();
        std::fs::write(&path, wav(rate, &quiet)).unwrap();
        super::finish_take(&path).unwrap();
        let back = read(&path);
        let rms = |s: &[f32]| (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt();
        assert!((20.0 * (rms(&back[rate as usize / 2..]) / rms(&quiet[rate as usize / 2..])).log10()).abs() < 0.5, "not raised");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #112: a take is fitted to a length within a frame in a few passes,
    /// though a voice is not exactly inverse in its speed; held at the
    /// speed limits it says what it got; a length out of range is refused.
    #[test]
    fn a_take_is_fitted_to_its_length() {
        // A voice whose 3 s at speed 1 slows less than in proportion,
        // with pauses that don't scale.
        let voice = |speed: f32| 0.6 + 2.4 / (speed as f64).powf(0.9);
        let mut passes = 0;
        let (speed, got) = super::fit_speed(2.0, 1.0, |s| {
            passes += 1;
            Ok(voice(s))
        })
        .unwrap();
        assert!((got - 2.0).abs() <= super::FIT_TOLERANCE_SECS, "{got} s at speed {speed}");
        assert!(passes <= 6, "{passes} passes");
        // Too long to reach: held at twice the speed, and said.
        let (speed, got) = super::fit_speed(0.5, 1.0, |s| Ok(voice(s))).unwrap();
        assert_eq!(speed, 2.0);
        assert!(got > 0.5);
        assert!(super::fit_speed(0.1, 1.0, |s| Ok(voice(s))).is_err());
    }

    /// A WAV's length is its data over its byte rate, a streamed one's
    /// unknown size read as what is there.
    #[test]
    fn a_wav_files_length_is_read_from_its_header() {
        let wav = |data_size: u32, samples: usize| {
            let mut b = Vec::new();
            b.extend_from_slice(b"RIFF");
            b.extend_from_slice(&0u32.to_le_bytes());
            b.extend_from_slice(b"WAVEfmt ");
            b.extend_from_slice(&16u32.to_le_bytes());
            b.extend_from_slice(&1u16.to_le_bytes());
            b.extend_from_slice(&1u16.to_le_bytes());
            b.extend_from_slice(&22050u32.to_le_bytes());
            b.extend_from_slice(&44100u32.to_le_bytes());
            b.extend_from_slice(&2u16.to_le_bytes());
            b.extend_from_slice(&16u16.to_le_bytes());
            b.extend_from_slice(b"data");
            b.extend_from_slice(&data_size.to_le_bytes());
            b.extend(std::iter::repeat_n(0u8, samples * 2));
            b
        };
        let dir = std::env::temp_dir().join(format!("klipp-wav-secs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.wav");
        std::fs::write(&p, wav(44100, 22050)).unwrap();
        assert!((super::wav_secs(&p).unwrap() - 1.0).abs() < 1e-9);
        std::fs::write(&p, wav(0, 11025)).unwrap();
        assert!((super::wav_secs(&p).unwrap() - 0.5).abs() < 1e-9, "a streamed WAV's size read from the file");
        std::fs::write(&p, b"not a wav").unwrap();
        assert!(super::wav_secs(&p).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_take_keeps_its_name_unless_a_delivery_or_another_voice_is_asked_for() {
        let line = "Some walks you plan.";
        assert_eq!(take_key(line, UNNAMED_VOICE, &[]), text_hash(line), "a Lessac take with no delivery: the name it always had");
        let slower = delivery_args(Some(0.8), None).unwrap();
        let paused = delivery_args(None, Some(0.6)).unwrap();
        assert_ne!(take_key(line, UNNAMED_VOICE, &slower), text_hash(line));
        assert_ne!(take_key(line, UNNAMED_VOICE, &slower), take_key(line, UNNAMED_VOICE, &paused));
        // #523's names are kept: a delivery in Lessac hashes as it did
        // before voices were in the key.
        assert_eq!(take_key(line, UNNAMED_VOICE, &slower), text_hash(&format!("{line}\u{0}{}", slower.join(" "))));
        // Any other voice, the default included, is another file.
        assert_ne!(take_key(line, DEFAULT_VOICE, &[]), text_hash(line));
        assert_ne!(take_key(line, DEFAULT_VOICE, &slower), take_key(line, UNNAMED_VOICE, &slower));
        assert_ne!(take_key(line, "piper_en_gb_cori_high", &[]), take_key(line, DEFAULT_VOICE, &[]));
    }

    /// Every voice the manifest offers is there with both its files, the
    /// default is free for any use, and only a voice labelled so is not.
    #[test]
    fn the_manifest_voices_and_their_licences() {
        let manifest = klipp_ml::manifest::ModelManifest::parse(klipp_ml::manifest::BUILTIN_MANIFEST).unwrap();
        let voices: Vec<_> = manifest.models.iter().filter(|e| e.kind == klipp_ml::manifest::ModelKind::Voice && !e.id.ends_with("_config") && e.id != super::kokoro::MODEL).collect();
        assert!(voices.len() >= 18, "{}", voices.len());
        assert!(manifest.get(super::kokoro::MODEL).is_some(), "the Kokoro model every Kokoro voice needs");
        for v in &voices {
            if super::kokoro::is_kokoro(&v.id) {
                assert_eq!(v.size_bytes, 522_240, "{}: a Kokoro voice is 510 style rows of 256 floats", v.id);
            } else {
                assert!(manifest.get(&format!("{}_config", v.id)).is_some(), "{} has no settings entry", v.id);
            }
        }
        let default = voices.iter().find(|v| v.id == DEFAULT_VOICE).expect("the default voice is in the manifest");
        assert!(commercial_use(&default.license), "{}", default.license);
        let lessac = voices.iter().find(|v| v.id == UNNAMED_VOICE).expect("Lessac stays, labelled");
        assert!(!commercial_use(&lessac.license), "{}", lessac.license);
        assert_eq!(voices.iter().filter(|v| !commercial_use(&v.license)).count(), 1, "only Lessac is restricted");
        assert!(!commercial_use("CC BY-NC-SA 4.0"));
        assert!(commercial_use("CC BY 4.0 (LibriTTS): credit LibriTTS"));
    }

    /// A person picks "Heart, female, US English", not an id; each
    /// Kokoro voice has a sample to hear before downloading; and the model
    /// manager shows voices, not their parts.
    #[test]
    fn voices_have_plain_names_samples_and_one_row_each() {
        assert_eq!(super::label_of("kokoro_voice_af_heart", "Kokoro voice — English (US), Heart, grade A"), ("Heart".to_string(), "female, US English".to_string()));
        assert_eq!(super::label_of("kokoro_voice_bm_george", ""), ("George".to_string(), "male, UK English".to_string()));
        assert_eq!(super::label_of("piper_en_us_kristin_medium", "Piper voice — English (US), Kristin, medium"), ("Kristin".to_string(), "English (US)".to_string()));
        let manifest = klipp_ml::manifest::ModelManifest::parse(klipp_ml::manifest::BUILTIN_MANIFEST).unwrap();
        for e in manifest.models.iter().filter(|e| super::kokoro::is_kokoro(&e.id)) {
            let s = super::sample(&e.id).unwrap_or_else(|| panic!("{} has no bundled sample", e.id));
            assert!(s.len() > 5_000 && s.len() < 60_000, "{}: {} bytes", e.id, s.len());
        }
        assert!(super::shown_in_model_manager(super::kokoro::MODEL), "the Kokoro download is a row");
        assert!(!super::shown_in_model_manager("kokoro_voice_af_heart"));
        assert!(!super::shown_in_model_manager("piper_en_us_kristin_medium_config"));
        assert!(super::shown_in_model_manager("piper_en_us_kristin_medium"));
        assert!(super::shown_in_model_manager("whisper_base_ggml"));
        assert!(manifest.get(super::FALLBACK_VOICE).is_some());
    }

    /// Uninstalling the Kokoro row takes its ten voices with it, and a
    /// Piper voice its settings; nothing is left with no row to remove it.
    #[test]
    fn a_row_uninstalls_the_files_that_came_with_it() {
        let manifest = klipp_ml::manifest::ModelManifest::parse(klipp_ml::manifest::BUILTIN_MANIFEST).unwrap();
        let cache = klipp_ml::cache::ModelCache::open_at("/nowhere");
        let kokoro = super::extra_files(&cache, &manifest, super::kokoro::MODEL);
        assert_eq!(kokoro.len(), 10);
        assert!(kokoro.iter().all(|p| p.file_name().unwrap().to_string_lossy().starts_with("kokoro_voice_")));
        let piper = super::extra_files(&cache, &manifest, "piper_en_us_kristin_medium");
        assert_eq!(piper, [std::path::PathBuf::from("/nowhere/piper_en_us_kristin_medium_config.bin"), std::path::PathBuf::from("/nowhere/piper_en_us_kristin_medium.bin.json")]);
        assert!(super::extra_files(&cache, &manifest, "whisper_base_ggml").is_empty());
        // Every file a row hides comes with some row.
        for e in manifest.models.iter().filter(|e| !super::shown_in_model_manager(&e.id)) {
            let owner = if super::kokoro::is_kokoro(&e.id) { super::kokoro::MODEL.to_string() } else { e.id.trim_end_matches("_config").to_string() };
            assert!(super::extra_files(&cache, &manifest, &owner).contains(&cache.path_for(e)), "{} is hidden and would be left behind", e.id);
        }
    }

    /// Without Kokoro in the build, a take with no voice named is spoken
    /// by the public-domain Piper fallback.
    #[test]
    #[cfg(not(feature = "kokoro"))]
    fn without_kokoro_the_default_is_the_piper_fallback() {
        assert_eq!(super::default_voice(), super::FALLBACK_VOICE);
    }

    #[test]
    fn speed_and_pause_become_piper_arguments() {
        assert!(delivery_args(None, None).unwrap().is_empty(), "nothing asked, nothing passed");
        assert_eq!(delivery_args(Some(0.8), Some(0.6)).unwrap(), ["--length_scale", "1.250", "--sentence_silence", "0.600"]);
        assert_eq!(delivery_args(Some(2.0), None).unwrap(), ["--length_scale", "0.500"]);
        assert!(delivery_args(Some(0.0), None).is_err());
        assert!(delivery_args(Some(f32::NAN), None).is_err());
        assert!(delivery_args(None, Some(-0.1)).is_err());
        // Variation, #112: both noise terms scaled together, none by default.
        assert!(super::variation_args(None).unwrap().is_empty());
        assert_eq!(super::variation_args(Some(2.0)).unwrap(), ["--noise_scale", "1.334", "--noise_w", "1.600"]);
        assert!(super::variation_args(Some(3.0)).is_err());
    }

    /// The real thing, where piper is installed: the default voice and
    /// the Swedish one fetched through the verified cache (their hashes
    /// checked against the manifest), each speaking a WAV file named
    /// after its words, and a slower take a file of its own and longer.
    /// `cargo test -- --ignored`; downloads about 180 MB the first time.
    #[test]
    #[ignore = "needs the piper program, and downloads two voices"]
    fn piper_speaks_a_take_into_a_wav_file() {
        let dir = std::env::temp_dir().join(format!("klipp-narration-{}", std::process::id()));
        let say = |text: &str, voice: Option<&str>, speed| {
            super::synthesize(&super::Request { text, voice, speed, pause_secs: None, fit_secs: None, variation: None, download: true, path: None, dir: &dir })
                .expect("piper is installed and the voice downloads")
        };
        let plain = say("Some walks you plan.", None, None);
        assert!(plain.file_name().unwrap().to_string_lossy().starts_with("some-walks-you-plan-"), "{}", plain.display());
        assert_eq!(&std::fs::read(&plain).unwrap()[..4], b"RIFF", "a wav file");
        let slower = say("Some walks you plan.", None, Some(0.7));
        assert_ne!(slower, plain, "a slower take is a file of its own");
        let len = |p: &std::path::Path| std::fs::metadata(p).unwrap().len();
        assert!(len(&slower) > len(&plain) * 11 / 10, "slower is longer: {} vs {}", len(&slower), len(&plain));
        let swedish = say("Vissa promenader planerar man.", Some("piper_sv_se_nst_medium"), None);
        assert_eq!(&std::fs::read(&swedish).unwrap()[..4], b"RIFF");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_config_is_copied_beside_the_model_once() {
        let dir = std::env::temp_dir().join(format!("klipp-piper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let model = dir.join("voice.bin");
        let config = dir.join("voice_config.bin");
        std::fs::write(&model, b"onnx").unwrap();
        std::fs::write(&config, b"{\"audio\":{}}").unwrap();
        config_beside(&model, &config).unwrap();
        assert_eq!(std::fs::read(dir.join("voice.bin.json")).unwrap(), b"{\"audio\":{}}");
        // Already there: left alone.
        std::fs::write(dir.join("voice.bin.json"), b"kept").unwrap();
        config_beside(&model, &config).unwrap();
        assert_eq!(std::fs::read(dir.join("voice.bin.json")).unwrap(), b"kept");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
