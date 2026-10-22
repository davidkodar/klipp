//! What a file's sound is (#2216): speech, music, effects or ambience,
//! over each stretch of it and over the whole, from AudioSet's classes as
//! the PANNs Cnn10 tagger hears them. It proposes the role of the file's
//! clips; a confident proposal is applied, an unsure one is offered.

use serde::{Deserialize, Serialize};

pub const ID: &str = "audio_role";
pub const VERSION: u32 = 1;
pub use klipp_ml_candle::panns::PANNS_CNN10_MODEL_ID as MODEL_ID;

/// The stretch scored at a time, in seconds.
const WINDOW_SECS: f64 = 4.0;

/// The most of a file read, from its start: an hour.
const MAX_SECS: f64 = 3600.0;

/// Windows scored at once.
const BATCH: usize = 8;

/// A window quieter than this (RMS, dBFS) has no role: room tone between
/// takes says nothing about what the file is.
const QUIET_DB: f32 = -55.0;

/// A label is applied without asking at this score or more, and when it
/// is at least this many times the runner-up's.
const CONFIDENT: f32 = 0.5;
const MARGIN: f32 = 2.0;

/// The roles a sound can propose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoundKind {
    Speech,
    Music,
    Effects,
    Ambience,
}

impl SoundKind {
    pub const ALL: [SoundKind; 4] = [SoundKind::Speech, SoundKind::Music, SoundKind::Effects, SoundKind::Ambience];

    /// The clip role the sound proposes.
    pub fn role(self) -> klipp_core::ClipRole {
        match self {
            SoundKind::Speech => klipp_core::ClipRole::Dialogue,
            SoundKind::Music => klipp_core::ClipRole::Music,
            SoundKind::Effects => klipp_core::ClipRole::SFX,
            SoundKind::Ambience => klipp_core::ClipRole::Ambience,
        }
    }

    /// Which AudioSet classes (by index) count toward it. Speech is
    /// talking; music is singing, instruments and genres; ambience is
    /// weather, water, fire, crowds, traffic and the sound of places;
    /// effects are every other source that makes a sound.
    fn classes(self) -> &'static [std::ops::RangeInclusive<usize>] {
        match self {
            SoundKind::Speech => &[0..=5, 7..=7, 15..=15],
            SoundKind::Music => &[27..=37, 137..=282],
            SoundKind::Ambience => &[68..=71, 283..=299, 327..=327, 412..=413, 506..=510, 514..=514, 526..=526],
            SoundKind::Effects => &[8..=14, 16..=26, 38..=67, 72..=136, 300..=326, 328..=411, 414..=499, 504..=504],
        }
    }

    /// Its score in a window: the most likely of its classes.
    fn score(self, classes: &[f32]) -> f32 {
        self.classes().iter().flat_map(|r| r.clone()).filter_map(|i| classes.get(i)).fold(0.0, |a, b| a.max(*b))
    }
}

/// One stretch's scores, in [`SoundKind::ALL`]'s order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stretch {
    pub start_secs: f64,
    pub end_secs: f64,
    pub scores: [f32; 4],
}

/// The file's sound, stretch by stretch; quiet stretches are left out.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioRoles {
    pub stretches: Vec<Stretch>,
}

/// What a span of sound proposes: its kind, its mean score, and whether
/// that is sure enough to apply.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Label {
    pub kind: SoundKind,
    pub confidence: f32,
    pub confident: bool,
}

impl AudioRoles {
    /// The label of the stretches that overlap `from..to` (file seconds),
    /// or the whole file's with `None`: the kind whose mean score is
    /// highest. `None` when nothing in it was loud enough to hear.
    pub fn label(&self, span: Option<(f64, f64)>) -> Option<Label> {
        let inside: Vec<&Stretch> = self.stretches.iter().filter(|s| span.is_none_or(|(a, b)| s.end_secs > a && s.start_secs < b)).collect();
        if inside.is_empty() {
            return None;
        }
        let mean = |k: usize| inside.iter().map(|s| s.scores[k]).sum::<f32>() / inside.len() as f32;
        let mut ranked: Vec<(SoundKind, f32)> = SoundKind::ALL.iter().enumerate().map(|(k, kind)| (*kind, mean(k))).collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        let (kind, top) = ranked[0];
        let second = ranked[1].1;
        Some(Label { kind, confidence: top, confident: top >= CONFIDENT && top >= MARGIN * second })
    }

    /// What the sound is over the whole file and where that changes, for
    /// a reader: "Sound: music (82%); 0–8 s speech, 8–40 s music".
    pub fn summary(&self) -> String {
        let Some(whole) = self.label(None) else { return String::new() };
        let name = |k: SoundKind| match k {
            SoundKind::Speech => "speech",
            SoundKind::Music => "music",
            SoundKind::Effects => "effects",
            SoundKind::Ambience => "ambience",
        };
        let mut runs: Vec<(f64, f64, SoundKind)> = Vec::new();
        for s in &self.stretches {
            let k = SoundKind::ALL[(0..4).max_by(|a, b| s.scores[*a].total_cmp(&s.scores[*b])).unwrap_or(0)];
            match runs.last_mut() {
                Some(r) if r.2 == k && (r.1 - s.start_secs).abs() < 1e-6 => r.1 = s.end_secs,
                _ => runs.push((s.start_secs, s.end_secs, k)),
            }
        }
        let shown: Vec<String> = runs.iter().take(24).map(|(a, b, k)| format!("{a:.0}–{b:.0} s {}", name(*k))).collect();
        format!("Sound: {} ({:.0}%{}); {}\n", name(whole.kind), whole.confidence * 100.0, if whole.confident { "" } else { ", unsure" }, shown.join(", "))
    }

    /// [`Self::label`] of the whole file, as the role it proposes.
    pub fn proposal(&self) -> Option<klipp_core::RoleProposal> {
        self.label(None).map(|l| klipp_core::RoleProposal { role: l.kind.role(), confidence: l.confidence, confident: l.confident })
    }
}

/// The tagger in the app's model cache.
pub fn model_path() -> Option<std::path::PathBuf> {
    let manifest = klipp_ml::manifest::load_effective().ok()?;
    let cache = klipp_ml::cache::ModelCache::open().ok()?;
    let path = cache.path_for(manifest.get(MODEL_ID)?);
    path.is_file().then_some(path)
}

/// The tagger, downloaded and verified when `download` and not there.
pub fn ensure_model(download: bool) -> Result<std::path::PathBuf, String> {
    if let Some(p) = model_path() {
        return Ok(p);
    }
    if !download {
        return Err(format!("the sound tagger {MODEL_ID} is not downloaded; ask with download_models"));
    }
    let manifest = klipp_ml::manifest::load_effective().map_err(|e| format!("model manifest: {e:?}"))?;
    let cache = klipp_ml::cache::ModelCache::open().map_err(|e| format!("model cache: {e:?}"))?;
    let entry = manifest.get(MODEL_ID).ok_or_else(|| format!("no manifest entry {MODEL_ID}"))?;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut progress = klipp_ml::download::NoOpProgress;
    klipp_ml::download::download_to_cache(&cache, entry, &mut progress, &cancel).map_err(|e| format!("download of {MODEL_ID}: {e:?}"))
}

/// Score `pcm` (32 kHz mono) window by window with `classify`, which
/// takes a batch of equal windows and gives each AudioSet's scores.
pub fn score(pcm: &[f32], classify: &mut dyn FnMut(&[Vec<f32>]) -> Result<Vec<Vec<f32>>, String>) -> Result<AudioRoles, String> {
    let rate = klipp_ml_candle::panns::SAMPLE_RATE as f64;
    let n = (WINDOW_SECS * rate) as usize;
    let mut windows: Vec<(f64, Vec<f32>)> = Vec::new();
    let mut stretches = Vec::new();
    let mut flush = |windows: &mut Vec<(f64, Vec<f32>)>, stretches: &mut Vec<Stretch>| -> Result<(), String> {
        if windows.is_empty() {
            return Ok(());
        }
        let batch: Vec<Vec<f32>> = windows.iter().map(|(_, w)| w.clone()).collect();
        for ((start, _), classes) in windows.drain(..).zip(classify(&batch)?) {
            let scores = SoundKind::ALL.map(|k| k.score(&classes));
            stretches.push(Stretch { start_secs: start, end_secs: start + WINDOW_SECS, scores });
        }
        Ok(())
    };
    for (i, chunk) in pcm.chunks(n).enumerate() {
        // A short last stretch is scored padded with silence, as long as
        // there is a second of it.
        if chunk.len() < (rate as usize) {
            continue;
        }
        let rms = (chunk.iter().map(|s| s * s).sum::<f32>() / chunk.len() as f32).sqrt();
        if 20.0 * rms.max(1e-9).log10() < QUIET_DB {
            continue;
        }
        let mut w = chunk.to_vec();
        w.resize(n, 0.0);
        windows.push((i as f64 * WINDOW_SECS, w));
        if windows.len() == BATCH {
            flush(&mut windows, &mut stretches)?;
        }
    }
    flush(&mut windows, &mut stretches)?;
    Ok(AudioRoles { stretches })
}

pub struct AudioRoleAnalyser;

impl crate::Analyser for AudioRoleAnalyser {
    fn id(&self) -> &'static str {
        ID
    }

    fn schema_version(&self) -> u32 {
        VERSION
    }

    /// Files with sound, once the tagger is downloaded.
    fn wants(&self, item: &klipp_media::MediaItem) -> bool {
        item.has_audio && model_path().is_some()
    }

    fn run(&self, item: &klipp_media::MediaItem, ctx: &crate::AnalysisCtx) -> Result<serde_json::Value, String> {
        let path = ensure_model(false)?;
        let model = klipp_ml_candle::panns::Cnn10::load(&path)?;
        let secs = item.duration.to_secs_f64().clamp(1.0, MAX_SECS);
        let pcm = crate::audio_sync::decode_mono_pcm(&ctx.ffmpeg, &item.path, klipp_ml_candle::panns::SAMPLE_RATE, 0.0, secs)?;
        let roles = score(&pcm, &mut |batch| model.classify(batch))?;
        serde_json::to_value(roles).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes_with(hot: &[(usize, f32)]) -> Vec<f32> {
        let mut v = vec![0.01; klipp_ml_candle::panns::CLASSES];
        for (i, p) in hot {
            v[*i] = *p;
        }
        v
    }

    #[test]
    fn every_class_counts_for_at_most_one_kind() {
        let mut seen = vec![None; klipp_ml_candle::panns::CLASSES];
        for kind in SoundKind::ALL {
            for i in kind.classes().iter().flat_map(|r| r.clone()) {
                assert!(seen[i].is_none(), "class {i} in {kind:?} and {:?}", seen[i]);
                seen[i] = Some(kind);
            }
        }
    }

    /// Talking reads as speech, a song as music, rain as ambience and a
    /// door slam as effects; quiet stretches are left out, and a span
    /// reads its own stretches.
    #[test]
    fn stretches_read_as_their_kind() {
        let rate = klipp_ml_candle::panns::SAMPLE_RATE as usize;
        let n = 4 * rate;
        // 16 s: talking, a song, near silence, rain.
        let mut pcm = vec![0.1f32; 16 * rate];
        for s in &mut pcm[2 * n..3 * n] {
            *s = 1e-5;
        }
        let mut calls = 0;
        let roles = score(&pcm, &mut |batch| {
            calls += 1;
            Ok(batch.iter().enumerate().map(|(i, _)| match i {
                0 => classes_with(&[(0, 0.9)]),
                1 => classes_with(&[(137, 0.8), (27, 0.6)]),
                _ => classes_with(&[(289, 0.7)]),
            }).collect())
        }).unwrap();
        assert_eq!(calls, 1);
        assert_eq!(roles.stretches.len(), 3, "the quiet stretch is left out");
        assert_eq!(roles.label(Some((0.0, 3.0))).unwrap().kind, SoundKind::Speech);
        assert_eq!(roles.label(Some((4.5, 7.0))).unwrap().kind, SoundKind::Music);
        let rain = roles.label(Some((12.0, 16.0))).unwrap();
        assert_eq!(rain.kind, SoundKind::Ambience);
        assert!(rain.confident);
        assert!(roles.label(Some((8.5, 11.5))).is_none(), "nothing heard");
        // Talking, a song and rain in one file: no kind is sure.
        assert!(!roles.label(None).unwrap().confident);
        assert_eq!(SoundKind::Effects.score(&classes_with(&[(358, 0.6)])), 0.6, "a slam");
    }
}
