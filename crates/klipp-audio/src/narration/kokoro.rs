//! Kokoro voices: Kokoro-82M (Apache-2.0) run through ONNX Runtime, with
//! espeak-ng turning the words into the phonemes it reads.
//!
//! espeak-ng is run as a program, as piper is, and its IPA is mapped onto
//! Kokoro's phoneme set the way upstream's own espeak fallback does it
//! (hexgrad/misaki `espeak.py`, `EspeakFallback`). The text is spoken a
//! sentence or a few at a time: Kokoro reads at most 510 phonemes at once
//! and sounds best at a hundred or two.

use std::path::{Path, PathBuf};

/// The model every Kokoro voice is spoken by.
pub const MODEL: &str = "kokoro_v1_0_q8f16";
/// The id prefix of a Kokoro voice in the manifest.
pub const VOICE_PREFIX: &str = "kokoro_voice_";

/// Phonemes Kokoro takes in one go, not counting the two pads.
const MAX_PHONEMES: usize = 510;

/// Whether `voice` is a Kokoro voice rather than a Piper one.
pub fn is_kokoro(voice: &str) -> bool {
    voice.starts_with(VOICE_PREFIX)
}

/// Kokoro's UK voices are the `b` ones (`bf_emma`, `bm_george`).
fn british(voice: &str) -> bool {
    voice.strip_prefix(VOICE_PREFIX).is_some_and(|v| v.starts_with('b'))
}

/// The espeak-ng program: `KLIPP_ESPEAK`, else `espeak-ng` on the path.
pub fn espeak() -> Result<PathBuf, String> {
    std::env::var("KLIPP_ESPEAK")
        .ok()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| super::which("espeak-ng"))
        .ok_or_else(|| "Kokoro voices need the espeak-ng program for their phonemes; on Arch: pacman -S espeak-ng, or set KLIPP_ESPEAK to it".to_string())
}

/// Kokoro's phoneme vocabulary (hexgrad/Kokoro-82M `config.json`).
const VOCAB: &[(char, i64)] = &[(';', 1), (':', 2), (',', 3), ('.', 4), ('!', 5), ('?', 6), ('—', 9), ('…', 10), ('"', 11), ('(', 12), (')', 13), ('“', 14), ('”', 15), (' ', 16), ('\u{0303}', 17), ('ʣ', 18), ('ʥ', 19), ('ʦ', 20), ('ʨ', 21), ('ᵝ', 22), ('ꭧ', 23), ('A', 24), ('I', 25), ('O', 31), ('Q', 33), ('S', 35), ('T', 36), ('W', 39), ('Y', 41), ('ᵊ', 42), ('a', 43), ('b', 44), ('c', 45), ('d', 46), ('e', 47), ('f', 48), ('h', 50), ('i', 51), ('j', 52), ('k', 53), ('l', 54), ('m', 55), ('n', 56), ('o', 57), ('p', 58), ('q', 59), ('r', 60), ('s', 61), ('t', 62), ('u', 63), ('v', 64), ('w', 65), ('x', 66), ('y', 67), ('z', 68), ('ɑ', 69), ('ɐ', 70), ('ɒ', 71), ('æ', 72), ('β', 75), ('ɔ', 76), ('ɕ', 77), ('ç', 78), ('ɖ', 80), ('ð', 81), ('ʤ', 82), ('ə', 83), ('ɚ', 85), ('ɛ', 86), ('ɜ', 87), ('ɟ', 90), ('ɡ', 92), ('ɥ', 99), ('ɨ', 101), ('ɪ', 102), ('ʝ', 103), ('ɯ', 110), ('ɰ', 111), ('ŋ', 112), ('ɳ', 113), ('ɲ', 114), ('ɴ', 115), ('ø', 116), ('ɸ', 118), ('θ', 119), ('œ', 120), ('ɹ', 123), ('ɾ', 125), ('ɻ', 126), ('ʁ', 128), ('ɽ', 129), ('ʂ', 130), ('ʃ', 131), ('ʈ', 132), ('ʧ', 133), ('ʊ', 135), ('ʋ', 136), ('ʌ', 138), ('ɣ', 139), ('ɤ', 140), ('χ', 142), ('ʎ', 143), ('ʒ', 147), ('ʔ', 148), ('ˈ', 156), ('ˌ', 157), ('ː', 158), ('ʰ', 162), ('ʲ', 164), ('↓', 169), ('→', 171), ('↗', 172), ('↘', 173), ('ᵻ', 177)];

/// The ids of a phoneme string; a symbol Kokoro does not know is dropped.
pub fn tokens(phonemes: &str) -> Vec<i64> {
    phonemes.chars().filter_map(|c| VOCAB.iter().find(|(v, _)| *v == c).map(|(_, id)| *id)).collect()
}

/// espeak-ng's IPA, written with `^` ties, as Kokoro's phonemes: upstream's
/// `EspeakFallback` step for step, in its order (longest first, then as
/// listed), since a replacement made earlier changes what a later one sees.
pub fn from_espeak(ipa: &str, british: bool) -> String {
    const E2M: &[(&str, &str)] = &[
        ("ʔˌn\u{0329}", "ʔn"),
        ("ʔn\u{0329}", "ʔn"),
        ("a^ɪ", "I"),
        ("a^ʊ", "W"),
        ("d^ʒ", "ʤ"),
        ("e^ɪ", "A"),
        ("t^ʃ", "ʧ"),
        ("ɔ^ɪ", "Y"),
        ("ə^l", "ᵊl"),
        ("ʲo", "jo"),
        ("ʲə", "jə"),
        ("e", "A"),
        ("ʲ", ""),
        ("ɚ", "əɹ"),
        ("r", "ɹ"),
        ("x", "k"),
        ("ç", "k"),
        ("ɐ", "ə"),
        ("ɬ", "l"),
        ("\u{0303}", ""),
    ];
    let mut ps = ipa.trim().to_string();
    for (old, new) in E2M {
        ps = ps.replace(old, new);
    }
    // A syllabic consonant: `n̩` is `ᵊn`.
    let mut out: Vec<char> = Vec::with_capacity(ps.len());
    for c in ps.chars() {
        if c == '\u{0329}' {
            if let Some(prev) = out.pop() {
                if prev.is_whitespace() {
                    out.push(prev);
                } else {
                    out.push('ᵊ');
                    out.push(prev);
                }
            }
        } else {
            out.push(c);
        }
    }
    let mut ps: String = out.into_iter().collect();
    if british {
        ps = ps.replace("e^ə", "ɛː").replace("iə", "ɪə").replace("ə^ʊ", "Q");
    } else {
        ps = ps.replace("o^ʊ", "O").replace("ɜːɹ", "ɜɹ").replace("ɜː", "ɜɹ").replace("ɪə", "iə").replace('ː', "");
    }
    ps.replace('o', "ɔ").replace('ɾ', "T").replace('ʔ', "t").replace('^', "")
}

/// What ends a clause, and so what espeak-ng is never shown: it would
/// drop it, and Kokoro reads the pause from it.
const CLAUSE_ENDS: &[char] = &['.', ',', '!', '?', ';', ':', '—', '…'];
const SENTENCE_ENDS: &[char] = &['.', '!', '?', '…'];

/// The text as clauses, each with the mark that ends it. A point or comma
/// between digits is part of the number; a line break ends a sentence.
fn clauses(text: &str) -> Vec<(String, Option<char>)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let in_number = matches!(c, '.' | ',') && i > 0 && chars[i - 1].is_ascii_digit() && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit());
        if c == '\n' || (CLAUSE_ENDS.contains(&c) && !in_number) {
            let mark = if c == '\n' { Some('.') } else { Some(c) };
            if !cur.trim().is_empty() {
                out.push((cur.trim().to_string(), mark));
            } else if let Some(last) = out.last_mut() {
                // "Wait...!" or a blank line: the mark joins the clause before.
                if last.1.is_none() {
                    last.1 = mark;
                }
            }
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    if !cur.trim().is_empty() {
        out.push((cur.trim().to_string(), None));
    }
    out
}

/// A clause's IPA from espeak-ng.
fn espeak_ipa(program: &Path, clause: &str, british: bool) -> Result<String, String> {
    use std::io::Write as _;
    let mut child = std::process::Command::new(program)
        .args(["-q", "--ipa", "--tie=^", "-v", if british { "en-gb" } else { "en-us" }, "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {}: {e}", program.display()))?;
    child
        .stdin
        .take()
        .ok_or("espeak-ng's stdin")?
        // The line ended: without its newline espeak-ng cuts the last word
        // short ("thunderstorm" as "thunderst…"), and a word before every
        // comma and full stop came out clipped.
        .write_all(format!("{clause}\n").as_bytes())
        .map_err(|e| format!("writing to espeak-ng: {e}"))?;
    let done = child.wait_with_output().map_err(|e| format!("espeak-ng: {e}"))?;
    if !done.status.success() {
        return Err(format!("espeak-ng failed ({}): {}", done.status, String::from_utf8_lossy(&done.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&done.stdout).split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Phonemes Kokoro speaks best around, when it may choose how much to
/// take at once.
const COMFORTABLE: usize = 250;

/// The phonemes of each piece to speak: whole sentences, several short
/// ones together unless each is to be followed by a pause, and a sentence
/// too long for the model cut at its clauses (or, a clause too long, at
/// the limit).
fn pieces(clauses: &[(String, String)], one_sentence_each: bool) -> Vec<String> {
    let limit = MAX_PHONEMES;
    // Sentences first: the clauses up to one that ends one.
    let mut sentences: Vec<Vec<String>> = vec![Vec::new()];
    for (ps, mark) in clauses {
        sentences.last_mut().expect("never empty").push(format!("{ps}{mark}"));
        if mark.chars().next().is_some_and(|m| SENTENCE_ENDS.contains(&m)) {
            sentences.push(Vec::new());
        }
    }
    let count = |s: &str| tokens(s).len();
    let mut out: Vec<String> = Vec::new();
    for sentence in sentences.into_iter().filter(|s| !s.is_empty()) {
        // A sentence too long is cut at clauses, and a clause at the limit.
        let mut parts: Vec<String> = Vec::new();
        for clause in sentence {
            let mut clause = clause;
            while count(&clause) > limit {
                let cut: String = clause.chars().take(limit).collect();
                clause = clause.chars().skip(limit).collect();
                parts.push(cut);
            }
            match parts.last_mut() {
                Some(last) if count(last) + 1 + count(&clause) <= limit => {
                    last.push(' ');
                    last.push_str(&clause);
                }
                _ => parts.push(clause),
            }
        }
        for part in parts {
            match out.last_mut() {
                Some(last) if !one_sentence_each && count(last) + 1 + count(&part) <= COMFORTABLE => {
                    last.push(' ');
                    last.push_str(&part);
                }
                _ => out.push(part),
            }
        }
    }
    out
}

/// A Kokoro voice's model and style file in the model cache.
pub fn files(voice: &str, download: bool) -> Result<(PathBuf, PathBuf), String> {
    Ok((super::cached(MODEL, download)?, super::cached(voice, download)?))
}

/// Speak `text` in the Kokoro `voice` into a WAV file at `out`.
pub(super) fn speak(text: &str, voice: &str, speed: Option<f32>, pause_secs: Option<f32>, download: bool, out: &Path) -> Result<(), String> {
    let program = espeak()?;
    let (model, style_file) = files(voice, download)?;
    let style_bytes = std::fs::read(&style_file).map_err(|e| format!("{}: {e}", style_file.display()))?;
    let uk = british(voice);
    let mut phonemised = Vec::new();
    for (clause, mark) in clauses(text) {
        let ps = from_espeak(&espeak_ipa(&program, &clause, uk)?, uk);
        phonemised.push((ps, mark.map(String::from).unwrap_or_default()));
    }
    let pieces = pieces(&phonemised, pause_secs.is_some());
    if pieces.is_empty() {
        return Err("there is nothing in the words to say".into());
    }
    let samples = run(&model, &style_bytes, &pieces, speed.unwrap_or(1.0), pause_secs.unwrap_or(0.0))?;
    write_wav(out, &samples, SAMPLE_RATE)
}

/// The rate Kokoro speaks at.
const SAMPLE_RATE: u32 = 24_000;

/// Each piece spoken, `pause_secs` of silence between them.
#[cfg(feature = "kokoro")]
fn run(model: &Path, style: &[u8], pieces: &[String], speed: f32, pause_secs: f32) -> Result<Vec<f32>, String> {
    debug_assert_eq!(SAMPLE_RATE, klipp_ml_ort::kokoro::SAMPLE_RATE);
    let mut model = klipp_ml_ort::kokoro::Kokoro::open(model)?;
    let gap = vec![0f32; (pause_secs * SAMPLE_RATE as f32) as usize];
    let mut samples = Vec::new();
    for (i, piece) in pieces.iter().enumerate() {
        if i > 0 {
            samples.extend_from_slice(&gap);
        }
        let ids = tokens(piece);
        let row = klipp_ml_ort::kokoro::style_row(style, ids.len())?;
        samples.extend(model.speak(&ids, &row, speed)?);
    }
    Ok(samples)
}

#[cfg(not(feature = "kokoro"))]
fn run(_model: &Path, _style: &[u8], _pieces: &[String], _speed: f32, _pause_secs: f32) -> Result<Vec<f32>, String> {
    Err("this build of Klipp has no ONNX Runtime, which Kokoro voices need; a Piper voice works without it".into())
}

/// Mono samples as a 16-bit PCM WAV file.
fn write_wav(path: &Path, samples: &[f32], rate: u32) -> Result<(), String> {
    let data_len = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        bytes.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
    }
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// espeak-ng 1.52's en-us IPA for "Some walks you plan. Others plan
    /// you." (tie `^`), and what upstream's fallback makes of it: `ɚ` is
    /// `əɹ`, and length marks go in US English.
    #[test]
    fn espeak_ipa_becomes_kokoro_phonemes_as_upstream_maps_it() {
        let us = from_espeak("sˌʌm wˈɔːks juː plˈæn", false);
        assert_eq!(us, "sˌʌm wˈɔks ju plˈæn");
        assert_eq!(from_espeak("ˈʌðɚz", false), "ˈʌðəɹz");
        // Ties become Kokoro's own letters.
        assert_eq!(from_espeak("mˈa^ɪ ɡˈo^ʊ tʃˈe^ɪndʒ", false), "mˈI ɡˈO tʃˈAndʒ");
        assert_eq!(from_espeak("d^ʒˈʌst t^ʃˈiːz", false), "ʤˈʌst ʧˈiz");
        // UK keeps length and has its own diphthong.
        assert_eq!(from_espeak("ɡˈə^ʊ hˈɑːf", true), "ɡˈQ hˈɑːf");
        // A syllabic n, and the flap and glottal stop.
        assert_eq!(from_espeak("bˈʌʔn\u{0329}", false), "bˈʌtn");
        assert_eq!(from_espeak("bˈʌtn\u{0329}", false), "bˈʌtᵊn");
        assert_eq!(from_espeak("wˈɔːɾɚ", false), "wˈɔTəɹ");
    }

    #[test]
    fn phonemes_become_the_ids_upstream_gives() {
        // The ids kokoro-onnx gives for espeak's en-us "Some walks you
        // plan. Others plan you." (research, 2026-09-28): the vocabulary
        // is the same one character to one id.
        let ps = "sˌʌm wˈɔːks juː plˈæn. ˈʌðɚz plˈæn juː.";
        assert_eq!(
            tokens(ps),
            [61, 157, 138, 55, 16, 65, 156, 76, 158, 53, 61, 16, 52, 63, 158, 16, 58, 54, 156, 72, 56, 4, 16, 156, 138, 81, 85, 68, 16, 58, 54, 156, 72, 56, 16, 52, 63, 158, 4]
        );
        assert!(tokens("g").is_empty(), "ascii g is not Kokoro's ɡ");
    }

    #[test]
    fn text_splits_into_clauses_at_their_marks_but_not_inside_numbers() {
        let c = clauses("Wait... it costs 3.50, or 1,200 more!\nNext line");
        let marks: Vec<_> = c.iter().map(|(t, m)| (t.as_str(), *m)).collect();
        assert_eq!(marks, [("Wait", Some('.')), ("it costs 3.50", Some(',')), ("or 1,200 more", Some('!')), ("Next line", None)]);
    }

    #[test]
    fn short_sentences_are_spoken_together_unless_each_gets_a_pause() {
        let c = vec![("hˈaɪ".to_string(), ".".to_string()), ("jˈɛs".to_string(), "!".to_string()), ("nˈoʊ".to_string(), "?".to_string())];
        assert_eq!(pieces(&c, false).len(), 1);
        assert_eq!(pieces(&c, true).len(), 3);
        // A sentence longer than the model takes is cut, and no piece is over.
        let long = vec![("a".repeat(700), ".".to_string())];
        let p = pieces(&long, false);
        assert!(p.len() >= 2 && p.iter().all(|s| tokens(s).len() <= MAX_PHONEMES), "{:?}", p.iter().map(|s| tokens(s).len()).collect::<Vec<_>>());
    }

    /// The real thing, where espeak-ng is installed: the model and the
    /// Heart and George voices fetched through the verified cache, each
    /// speaking a take, and a pause between sentences making it longer.
    /// `cargo test --features kokoro -- --ignored`; about 87 MB the first time.
    #[test]
    #[ignore = "needs espeak-ng, and downloads the Kokoro model"]
    #[cfg(feature = "kokoro")]
    fn kokoro_speaks_a_take_into_a_wav_file() {
        // espeak-ng given a clause without its newline cut the last word
        // short; every word before a comma or a full stop came out clipped.
        let ipa = espeak_ipa(&espeak().unwrap(), "Nobody expected the thunderstorm", false).unwrap();
        assert!(ipa.ends_with('m'), "the last word whole: {ipa}");
        let dir = std::env::temp_dir().join(format!("klipp-kokoro-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let say = |voice: &str, pause: Option<f32>, name: &str| {
            let out = dir.join(name);
            speak("Some walks you plan. Others plan you.", voice, None, pause, true, &out).expect("espeak-ng is installed and the model downloads");
            let b = std::fs::read(&out).unwrap();
            assert_eq!(&b[..4], b"RIFF");
            let samples = (b.len() - 44) / 2;
            let peak = b[44..].chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]]).unsigned_abs()).max().unwrap();
            assert!(peak > 3000, "{voice}: speech, not silence (peak {peak})");
            samples
        };
        let heart = say("kokoro_voice_af_heart", None, "heart.wav");
        let secs = heart as f32 / 24_000.0;
        assert!((1.5..6.0).contains(&secs), "two short sentences take a couple of seconds, not {secs}");
        say("kokoro_voice_bm_george", None, "george.wav");
        let paused = say("kokoro_voice_af_heart", Some(1.0), "paused.wav");
        assert!(paused as f32 > heart as f32 + 0.8 * 24_000.0, "a one-second pause between the sentences: {paused} vs {heart}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_uk_voice_is_known_by_its_letter() {
        assert!(british("kokoro_voice_bf_emma"));
        assert!(!british("kokoro_voice_af_heart"));
        assert!(is_kokoro("kokoro_voice_am_michael") && !is_kokoro("piper_en_us_ljspeech_high"));
    }

    #[test]
    fn a_wav_file_is_written_as_16_bit_mono() {
        let path = std::env::temp_dir().join(format!("klipp-kokoro-wav-{}.wav", std::process::id()));
        write_wav(&path, &[0.0, 0.5, -1.0, 2.0], 24_000).unwrap();
        let b = std::fs::read(&path).unwrap();
        assert_eq!(&b[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes([b[24], b[25], b[26], b[27]]), 24_000);
        assert_eq!(b.len(), 44 + 8);
        assert_eq!(i16::from_le_bytes([b[48], b[49]]), -32767);
        assert_eq!(i16::from_le_bytes([b[50], b[51]]), 32767, "clipped, not wrapped");
        let _ = std::fs::remove_file(&path);
    }
}
