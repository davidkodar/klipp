//! Narration: a scratch voice from text, through the `piper` program and
//! a voice from the model manifest.
//!
//! The control socket's `narrate` and the Narrate window both come here,
//! so the same words, voice and delivery make the same file, and a take
//! made once is found again rather than made twice.

use std::path::{Path, PathBuf};

/// The voice a take is spoken in when none is named: public domain, so a
/// take made in it can go into any piece, paid work included.
pub const DEFAULT_VOICE: &str = "piper_en_us_ljspeech_high";

/// The voice takes were named without: the default before the voices
/// were checked for their licences. Its takes keep the names they have,
/// and every other voice is in its take's name.
const UNNAMED_VOICE: &str = "piper_en_us_lessac_medium";

/// A voice the manifest offers.
#[derive(Debug, Clone, PartialEq)]
pub struct Voice {
    /// The manifest id, which `narrate` takes as `voice`.
    pub id: String,
    /// As the manifest names it.
    pub name: String,
    /// The download, model and settings together.
    pub size_bytes: u64,
    /// Both its files are in the model cache.
    pub installed: bool,
    /// What the voice may be used for, as the manifest says.
    pub license: String,
    /// Its terms allow a take in a piece that is sold or paid for.
    pub commercial_use: bool,
}

/// What to say, and how.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    pub text: &'a str,
    /// A manifest voice id; `None` is [`DEFAULT_VOICE`].
    pub voice: Option<&'a str>,
    /// Above 1 quicker, below 1 slower; `None` is as the voice speaks.
    pub speed: Option<f32>,
    /// The silence between sentences; `None` is the voice's own.
    pub pause_secs: Option<f32>,
    /// Fetch a voice that is not downloaded yet.
    pub download: bool,
    /// Where to write the take; `None` names it in `dir` after its words.
    pub path: Option<PathBuf>,
    /// The narration folder a take is named into.
    pub dir: &'a Path,
}

/// Every voice in the manifest, the default first.
pub fn voices() -> Result<Vec<Voice>, String> {
    let manifest = klipp_ml::manifest::load_effective().map_err(|e| format!("model manifest: {e:?}"))?;
    let cache = klipp_ml::cache::ModelCache::open().map_err(|e| format!("model cache: {e:?}"))?;
    let mut voices: Vec<Voice> = manifest
        .models
        .iter()
        .filter(|e| e.kind == klipp_ml::manifest::ModelKind::Voice && !e.id.ends_with("_config"))
        .map(|e| {
            let config = manifest.get(&format!("{}_config", e.id));
            Voice {
                id: e.id.clone(),
                name: e.display_name.clone(),
                size_bytes: e.size_bytes + config.map_or(0, |c| c.size_bytes),
                installed: cache.path_for(e).is_file() && config.is_some_and(|c| cache.path_for(c).is_file()),
                license: e.license.clone(),
                commercial_use: commercial_use(&e.license),
            }
        })
        .collect();
    voices.sort_by_key(|v| v.id != DEFAULT_VOICE);
    Ok(voices)
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
    let piper = piper()?;
    let delivery = delivery_args(req.speed, req.pause_secs)?;
    let voice = req.voice.unwrap_or(DEFAULT_VOICE);
    let (model, config) = voice_files(voice, req.download)?;
    let out = match &req.path {
        Some(p) => p.clone(),
        None => {
            std::fs::create_dir_all(req.dir).map_err(|e| format!("{}: {e}", req.dir.display()))?;
            take_path(req.dir, text, voice, &delivery)
        }
    };
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
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{commercial_use, config_beside, delivery_args, take_key, text_hash, DEFAULT_VOICE, UNNAMED_VOICE};

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
        let voices: Vec<_> = manifest.models.iter().filter(|e| e.kind == klipp_ml::manifest::ModelKind::Voice && !e.id.ends_with("_config")).collect();
        assert!(voices.len() >= 8, "{}", voices.len());
        for v in &voices {
            assert!(manifest.get(&format!("{}_config", v.id)).is_some(), "{} has no settings entry", v.id);
        }
        let default = voices.iter().find(|v| v.id == DEFAULT_VOICE).expect("the default voice is in the manifest");
        assert!(commercial_use(&default.license), "{}", default.license);
        let lessac = voices.iter().find(|v| v.id == UNNAMED_VOICE).expect("Lessac stays, labelled");
        assert!(!commercial_use(&lessac.license), "{}", lessac.license);
        assert_eq!(voices.iter().filter(|v| !commercial_use(&v.license)).count(), 1, "only Lessac is restricted");
        assert!(!commercial_use("CC BY-NC-SA 4.0"));
        assert!(commercial_use("CC BY 4.0 (LibriTTS): credit LibriTTS"));
    }

    #[test]
    fn speed_and_pause_become_piper_arguments() {
        assert!(delivery_args(None, None).unwrap().is_empty(), "nothing asked, nothing passed");
        assert_eq!(delivery_args(Some(0.8), Some(0.6)).unwrap(), ["--length_scale", "1.250", "--sentence_silence", "0.600"]);
        assert_eq!(delivery_args(Some(2.0), None).unwrap(), ["--length_scale", "0.500"]);
        assert!(delivery_args(Some(0.0), None).is_err());
        assert!(delivery_args(Some(f32::NAN), None).is_err());
        assert!(delivery_args(None, Some(-0.1)).is_err());
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
            super::synthesize(&super::Request { text, voice, speed, pause_secs: None, download: true, path: None, dir: &dir })
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
