//! What is in the picture (#2212): frames of a file sampled through it,
//! each a CLIP embedding, so a description ("a dog on a beach",
//! "close-up of hands") finds the stretches that look like it. Made and
//! searched on this machine.

use serde::{Deserialize, Serialize};

pub const ID: &str = "visual";
pub const VERSION: u32 = 1;
pub use klipp_ml_ort::clip::{CLIP_TEXT_ID as TEXT_ID, CLIP_TOKENIZER_ID as TOKENIZER_ID, CLIP_VISION_ID as VISION_ID};

/// The files the model needs, in the order they are fetched.
pub const MODEL_IDS: [&str; 3] = [TOKENIZER_ID, TEXT_ID, VISION_ID];

/// The model as one candle file, before the ONNX encoders (#2245): its
/// size and hash as the manifest gave them, so only that file is removed.
const CANDLE_WEIGHTS: (&str, u64, &str) = (klipp_ml_ort::clip::CLIP_CANDLE_ID, 605_157_884, "99d28a652e6ec46629ab7047a0ac82c69b1fe11e0ce672c43af65d3a9a3fc05d");

/// Seconds between samples.
const EVERY_SECS: f64 = 2.0;

/// The most samples a file gets; a longer file has them spread wider.
const MAX_SAMPLES: usize = 600;

/// Pictures embedded at once.
const BATCH: usize = 16;

/// A file's samples: when, and the embedding, unit length, each value
/// times 127 rounded to a byte.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Visual {
    pub every_secs: f64,
    pub samples: Vec<Sample>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub secs: f64,
    pub e: Vec<i8>,
}

/// A stretch of a file that looks like what was asked: from the first
/// to past the last matching sample, and its best score (cosine).
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub start_secs: f64,
    pub end_secs: f64,
    pub score: f32,
    /// The best-matching sample's time: where its thumbnail is.
    pub best_secs: f64,
}

/// An embedding as stored.
pub fn quantise(e: &[f32]) -> Vec<i8> {
    e.iter().map(|v| (v * 127.0).round().clamp(-127.0, 127.0) as i8).collect()
}

impl Visual {
    /// Stretches whose samples score `min` or more against `query` (a
    /// unit embedding), neighbouring samples joined, best first.
    pub fn search(&self, query: &[f32], min: f32) -> Vec<Hit> {
        let score = |s: &Sample| s.e.iter().zip(query).map(|(a, b)| *a as f32 / 127.0 * b).sum::<f32>();
        let mut hits: Vec<Hit> = Vec::new();
        for s in &self.samples {
            let v = score(s);
            if v < min {
                continue;
            }
            let end = s.secs + self.every_secs;
            match hits.last_mut() {
                Some(h) if s.secs <= h.end_secs + 1e-6 => {
                    h.end_secs = end;
                    if v > h.score {
                        h.score = v;
                        h.best_secs = s.secs;
                    }
                }
                _ => hits.push(Hit { start_secs: s.secs, end_secs: end, score: v, best_secs: s.secs }),
            }
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits
    }
}

/// What the store holds for the file at `path`, at this version.
pub fn load(path: &std::path::Path) -> Option<Visual> {
    let key = crate::store::key_for(path)?;
    crate::store::load(&key, ID, VERSION).and_then(|v| serde_json::from_value(v).ok())
}

/// [`load`], kept: a file's samples are read from the store once and
/// kept while the file is unchanged (its store key holds its size and
/// time). A file not looked at yet is asked again next time.
pub fn load_kept(path: &std::path::Path) -> Option<std::sync::Arc<Visual>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static KEPT: OnceLock<Mutex<HashMap<String, Arc<Visual>>>> = OnceLock::new();
    let key = crate::store::key_for(path)?;
    let kept = KEPT.get_or_init(Default::default);
    if let Some(v) = kept.lock().ok().and_then(|k| k.get(&key).cloned()) {
        return Some(v);
    }
    let v = Arc::new(load(path)?);
    if let Ok(mut k) = kept.lock() {
        k.insert(key, v.clone());
    }
    Some(v)
}

/// Where the model's files are.
pub struct ModelPaths {
    pub vision: std::path::PathBuf,
    pub text: std::path::PathBuf,
    pub tokenizer: std::path::PathBuf,
}

/// The model's files in the app's model cache, when all are there.
pub fn model_paths() -> Option<ModelPaths> {
    let manifest = klipp_ml::manifest::load_effective().ok()?;
    let cache = klipp_ml::cache::ModelCache::open().ok()?;
    let path = |id: &str| manifest.get(id).map(|e| cache.path_for(e)).filter(|p| p.is_file());
    Some(ModelPaths { vision: path(VISION_ID)?, text: path(TEXT_ID)?, tokenizer: path(TOKENIZER_ID)? })
}

/// The model's files, downloaded and verified when `download` and not
/// there.
pub fn ensure_models(download: bool) -> Result<ModelPaths, String> {
    if let Some(p) = model_paths() {
        remove_candle_weights();
        return Ok(p);
    }
    if !download {
        return Err(format!("the picture search model {VISION_ID} is not downloaded; ask with download_models"));
    }
    let manifest = klipp_ml::manifest::load_effective().map_err(|e| format!("model manifest: {e:?}"))?;
    let cache = klipp_ml::cache::ModelCache::open().map_err(|e| format!("model cache: {e:?}"))?;
    for id in MODEL_IDS {
        let entry = manifest.get(id).ok_or_else(|| format!("no manifest entry {id}"))?;
        if cache.path_for(entry).is_file() {
            continue;
        }
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let mut progress = klipp_ml::download::NoOpProgress;
        klipp_ml::download::download_to_cache(&cache, entry, &mut progress, &cancel).map_err(|e| format!("download of {id}: {e:?}"))?;
    }
    let paths = model_paths().ok_or_else(|| "the picture search model did not land in the cache".to_string())?;
    remove_candle_weights();
    Ok(paths)
}

/// The candle weights the ONNX encoders replaced, removed from the model
/// cache once those are there: checked once a process, and only the file
/// the old manifest entry named.
fn remove_candle_weights() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Ok(cache) = klipp_ml::cache::ModelCache::open() else { return };
        let (id, size, sha) = CANDLE_WEIGHTS;
        match cache.remove_replaced(id, size, sha) {
            Ok(Some(bytes)) => tracing::info!("removed the old picture search weights ({id}, {} MB): the ONNX encoders replace them", bytes / 1_000_000),
            Ok(None) => {}
            Err(e) => tracing::warn!("the old picture search weights ({id}) could not be removed: {e}"),
        }
    });
}

/// The model, loaded once for the process: a search should not wait the
/// second or two a load takes.
fn model() -> Result<std::sync::Arc<klipp_ml_ort::clip::Clip>, String> {
    static LOADED: std::sync::Mutex<Option<std::sync::Arc<klipp_ml_ort::clip::Clip>>> = std::sync::Mutex::new(None);
    let mut loaded = LOADED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = loaded.as_ref() {
        return Ok(std::sync::Arc::clone(m));
    }
    let p = ensure_models(false)?;
    let m = std::sync::Arc::new(klipp_ml_ort::clip::Clip::load(&p.vision, &p.text, &p.tokenizer, klipp_ml::DevicePreference::GpuPreferCpu)?);
    *loaded = Some(std::sync::Arc::clone(&m));
    Ok(m)
}

/// `query` as an embedding to search with.
pub fn query(query: &str) -> Result<Vec<f32>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Err("describe what to find".into());
    }
    model()?.embed_text(q)
}

/// The sample times for a file `duration` seconds long.
pub fn sample_times(duration: f64, still: bool) -> Vec<f64> {
    if still || duration <= 0.0 {
        return vec![0.0];
    }
    let step = EVERY_SECS.max(duration / MAX_SAMPLES as f64);
    let n = ((duration / step).floor() as usize).max(1);
    (0..n).map(|i| i as f64 * step + step.min(duration) * 0.5).filter(|t| *t < duration).collect()
}

pub struct VisualAnalyser;

impl crate::Analyser for VisualAnalyser {
    fn id(&self) -> &'static str {
        ID
    }

    fn schema_version(&self) -> u32 {
        VERSION
    }

    /// Files with a picture, once the model is downloaded.
    fn wants(&self, item: &klipp_media::MediaItem) -> bool {
        item.resolution.width > 0 && item.resolution.height > 0 && model_paths().is_some()
    }

    fn run(&self, item: &klipp_media::MediaItem, ctx: &crate::AnalysisCtx) -> Result<serde_json::Value, String> {
        serde_json::to_value(look(&*model()?, item, ctx)?).map_err(|e| e.to_string())
    }

    fn native(&self) -> bool {
        // Decodes and runs a 600 MB model: a worker keeps a crash in it
        // from taking the app down.
        true
    }
}

/// `item`'s picture samples, embedded by `clip`.
pub fn look(clip: &klipp_ml_ort::clip::Clip, item: &klipp_media::MediaItem, ctx: &crate::AnalysisCtx) -> Result<Visual, String> {
    {
        let times = sample_times(item.duration.to_secs_f64(), item.is_still);
        let side = klipp_ml_ort::clip::IMAGE_SIZE;
        // The short side to the model's, then the middle square, as CLIP
        // was trained to see.
        let crop = format!("scale={side}:{side}:force_original_aspect_ratio=increase:flags=bicubic,crop={side}:{side}");
        let mut samples = Vec::with_capacity(times.len());
        for chunk in times.chunks(BATCH) {
            let frames = crate::grab_frames(&ctx.ffmpeg, item, chunk, side * 2, -2, &crop, (side * side * 4) as usize)?;
            let rgb: Vec<Vec<u8>> = frames.iter().map(|f| f.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect()).collect();
            for (secs, e) in chunk.iter().zip(clip.embed_images(&rgb)?) {
                samples.push(Sample { secs: *secs, e: quantise(&e) });
            }
        }
        let every_secs = times.windows(2).next().map_or(EVERY_SECS, |w| w[1] - w[0]);
        Ok(Visual { every_secs, samples })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: &[f32]) -> Vec<f32> {
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / n).collect()
    }

    /// Neighbouring samples that match join into one stretch, scored by
    /// the best of them; a stretch that matches better comes first; a
    /// sample under the bar is left out.
    #[test]
    fn matching_samples_join_into_ranked_stretches() {
        let beach = unit(&[1.0, 0.0, 0.1]);
        let hands = unit(&[0.0, 1.0, 0.1]);
        let at = |secs: f64, e: &[f32]| Sample { secs, e: quantise(e) };
        let v = Visual { every_secs: 2.0, samples: vec![at(1.0, &beach), at(3.0, &beach), at(5.0, &hands), at(7.0, &unit(&[0.9, 0.3, 0.0])), at(9.0, &beach)] };
        let hits = v.search(&beach, 0.8);
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!((hits[0].start_secs, hits[0].end_secs), (1.0, 5.0));
        assert!(hits[0].score > 0.99);
        assert_eq!((hits[1].start_secs, hits[1].end_secs), (7.0, 11.0), "7 and 9 meet");
        assert!(v.search(&hands, 0.8).iter().all(|h| h.start_secs == 5.0));
    }

    #[test]
    fn samples_spread_through_the_file() {
        assert_eq!(sample_times(5.0, false), vec![1.0, 3.0]);
        assert_eq!(sample_times(30.0, true), vec![0.0]);
        let long = sample_times(4.0 * 3600.0, false);
        assert!(long.len() <= MAX_SAMPLES && long.len() >= MAX_SAMPLES - 1, "{}", long.len());
        assert!(long.windows(2).all(|w| w[1] > w[0]));
    }
}
