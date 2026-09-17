//! How each shot is framed: who is in it and how large, from an object
//! detector run on a few frames per shot. A person's box against the
//! frame gives the shot size — close-up to very wide — the way an
//! editor names it; the count gives one-shot or two-shot; the largest
//! person's position gives left, centre, right; the other things the
//! detector names (a dog, a car) become the shot's objects.
//!
//! The detector is RF-DETR through ONNX Runtime, the model from the
//! registry. The analyser wants an item only when the model is in the
//! cache and the runtime loads; nothing is fetched behind the user's
//! back.

use serde::{Deserialize, Serialize};

pub const ID: &str = "framing";
pub const VERSION: u32 = 1;
/// The detectors in the registry, best first: the medium tier where it
/// is downloaded, else the preview tier. `MODEL_ID` names what
/// `download_models` fetches.
pub const MODEL_IDS: [&str; 2] = ["rf_detr_seg_medium_v1", "rf_detr_seg_v1"];
pub const MODEL_ID: &str = "rf_detr_seg_medium_v1";

/// Frames looked at per shot, and shots looked at per file at most;
/// beyond that, shots are sampled evenly.
const SAMPLES_PER_SHOT: usize = 3;
const MAX_SHOTS: usize = 40;
/// The detector's confidence below which a detection is not counted.
const MIN_SCORE: f32 = 0.5;
/// Frames are decoded to fit this square, letterboxed so nothing is
/// squashed; the detector resizes again to its own input. Boxes come
/// back against the padded square and are mapped to the picture.
const FRAME_SIZE: u32 = 640;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShotFraming {
    pub start_secs: f64,
    pub end_secs: f64,
    /// close-up, medium close-up, medium, wide, very wide, no people.
    pub framing: String,
    /// The most people seen in one frame of the shot.
    pub people: u32,
    /// Where the largest person stands: left, centre, right; empty
    /// with no people.
    pub position: String,
    /// What else the detector named, most confident first.
    pub objects: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Framing {
    pub duration_secs: f64,
    pub model: String,
    pub frames_looked_at: u32,
    pub shots: Vec<ShotFraming>,
}

impl Framing {
    pub fn within(&self, from: f64, to: f64) -> Vec<&ShotFraming> {
        self.shots.iter().filter(|s| s.end_secs > from && s.start_secs < to).collect()
    }
}

/// One frame's detections, as the classifier needs them: a person's
/// normalised box, and the labels of everything else.
#[derive(Debug, Clone, Default)]
pub struct FrameFacts {
    /// (x, y, w, h) in 0..1 for each person.
    pub people: Vec<(f32, f32, f32, f32)>,
    /// (label, confidence) for everything that is not a person.
    pub objects: Vec<(String, f32)>,
}

/// The shot size from the largest person's box height as a share of
/// the frame. The bands follow how a person fills the frame at each
/// conventional size; they are a starting point to revise on footage.
pub fn framing_for(people: &[(f32, f32, f32, f32)]) -> &'static str {
    let Some(largest) = people.iter().max_by(|a, b| (a.2 * a.3).partial_cmp(&(b.2 * b.3)).unwrap()) else {
        return "no people";
    };
    let h = largest.3;
    let w = largest.2;
    if h >= 0.9 && w >= 0.55 {
        "close-up"
    } else if h >= 0.75 {
        "medium close-up"
    } else if h >= 0.45 {
        "medium"
    } else if h >= 0.2 {
        "wide"
    } else {
        "very wide"
    }
}

pub fn position_for(people: &[(f32, f32, f32, f32)]) -> &'static str {
    let Some(largest) = people.iter().max_by(|a, b| (a.2 * a.3).partial_cmp(&(b.2 * b.3)).unwrap()) else {
        return "";
    };
    let cx = largest.0 + largest.2 / 2.0;
    if cx < 0.38 {
        "left"
    } else if cx > 0.62 {
        "right"
    } else {
        "centre"
    }
}

/// One shot's framing from the facts of its sampled frames: the
/// commonest size, the most people, the commonest position, the
/// objects by their best confidence.
pub fn shot_framing(start_secs: f64, end_secs: f64, frames: &[FrameFacts]) -> ShotFraming {
    fn commonest<'a>(it: impl Iterator<Item = &'a str>) -> String {
        let mut counts: Vec<(&str, usize)> = Vec::new();
        for v in it {
            match counts.iter_mut().find(|(k, _)| *k == v) {
                Some(c) => c.1 += 1,
                None => counts.push((v, 1)),
            }
        }
        counts.sort_by_key(|c| std::cmp::Reverse(c.1));
        counts.first().map(|(k, _)| k.to_string()).unwrap_or_default()
    }
    let framing = commonest(frames.iter().map(|f| framing_for(&f.people)));
    let people = frames.iter().map(|f| f.people.len() as u32).max().unwrap_or(0);
    let position = if people == 0 { String::new() } else { commonest(frames.iter().map(|f| position_for(&f.people)).filter(|p| !p.is_empty())) };
    let mut objects: Vec<(String, f32)> = Vec::new();
    for f in frames {
        for (label, c) in &f.objects {
            match objects.iter_mut().find(|(l, _)| l == label) {
                Some(o) => o.1 = o.1.max(*c),
                None => objects.push((label.clone(), *c)),
            }
        }
    }
    objects.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    ShotFraming { start_secs, end_secs, framing, people, position, objects: objects.into_iter().take(5).map(|(l, _)| l).collect() }
}

/// The best detector in the app's model cache, with its id.
pub fn model_path() -> Option<(&'static str, std::path::PathBuf)> {
    let manifest = klipp_ml::manifest::load_effective().ok()?;
    let cache = klipp_ml::cache::ModelCache::open().ok()?;
    MODEL_IDS.iter().find_map(|id| {
        let entry = manifest.get(id)?;
        let path = cache.path_for(entry);
        path.is_file().then_some((*id, path))
    })
}

/// The model, downloaded and verified when `download` and not there.
pub fn ensure_model(download: bool) -> Result<std::path::PathBuf, String> {
    if let Some((_, p)) = model_path() {
        return Ok(p);
    }
    if !download {
        return Err(format!("the detection model {MODEL_ID} is not downloaded; ask with download_models"));
    }
    let manifest = klipp_ml::manifest::load_effective().map_err(|e| format!("model manifest: {e:?}"))?;
    let entry = manifest.get(MODEL_ID).ok_or_else(|| format!("no manifest entry {MODEL_ID}"))?;
    let cache = klipp_ml::cache::ModelCache::open().map_err(|e| format!("model cache: {e:?}"))?;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut progress = klipp_ml::download::NoOpProgress;
    klipp_ml::download::download_to_cache(&cache, entry, &mut progress, &cancel).map_err(|e| format!("download of {MODEL_ID}: {e:?}"))
}

/// The frames at `times` of `media`, decoded once in one pass, each
/// letterboxed into a `FRAME_SIZE` square of RGBA8.
fn frames_at(ffmpeg: &std::path::Path, media: &std::path::Path, times: &[f64]) -> Result<Vec<Vec<u8>>, String> {
    if times.is_empty() {
        return Ok(Vec::new());
    }
    // ffmpeg's expression parser has a limit a long file's sample list
    // runs past; a long list is decoded in slices instead, each a pass.
    const MAX_PICKS: usize = 24;
    if times.len() > MAX_PICKS {
        let mut out = Vec::with_capacity(times.len());
        for chunk in times.chunks(MAX_PICKS) {
            out.extend(frames_at(ffmpeg, media, chunk)?);
        }
        return Ok(out);
    }
    // One frame per time: the first frame within a fortieth of a second
    // of it, which at any frame rate is at most one frame.
    let picks: Vec<String> = times.iter().map(|t| format!("lt(abs(t-{t:.3}),0.02)")).collect();
    let vf = format!(
        "select='{}',scale={FRAME_SIZE}:{FRAME_SIZE}:force_original_aspect_ratio=decrease,pad={FRAME_SIZE}:{FRAME_SIZE}:(ow-iw)/2:(oh-ih)/2:color=black",
        picks.join("+")
    );
    let out = std::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-nostats", "-loglevel", "error", "-i"])
        .arg(media)
        .args(["-an", "-sn", "-vf", &vf, "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("could not run {}: {e}", ffmpeg.display()))?;
    if !out.status.success() {
        return Err(format!("ffmpeg could not decode frames: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let frame_bytes = (FRAME_SIZE * FRAME_SIZE * 4) as usize;
    Ok(out.stdout.chunks_exact(frame_bytes).map(|c| c.to_vec()).collect())
}

/// Where the picture sits in the letterboxed square: (x, y, w, h) of
/// the content in 0..1, found from the black bars in the frame itself
/// so no metadata about rotation has to be trusted.
fn content_rect(rgba: &[u8]) -> (f32, f32, f32, f32) {
    let n = FRAME_SIZE as usize;
    let dark_row = |y: usize| rgba[y * n * 4..(y + 1) * n * 4].chunks_exact(4).all(|p| p[0] < 8 && p[1] < 8 && p[2] < 8);
    let dark_col = |x: usize| (0..n).all(|y| { let p = &rgba[(y * n + x) * 4..(y * n + x) * 4 + 3]; p[0] < 8 && p[1] < 8 && p[2] < 8 });
    let top = (0..n).find(|y| !dark_row(*y)).unwrap_or(0);
    let bottom = (0..n).rev().find(|y| !dark_row(*y)).unwrap_or(n - 1);
    let left = (0..n).find(|x| !dark_col(*x)).unwrap_or(0);
    let right = (0..n).rev().find(|x| !dark_col(*x)).unwrap_or(n - 1);
    let f = n as f32;
    (left as f32 / f, top as f32 / f, (right + 1 - left) as f32 / f, (bottom + 1 - top) as f32 / f)
}

/// A box against the square, mapped onto the picture inside it.
fn to_content(b: klipp_ml::BBox, rect: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    let (rx, ry, rw, rh) = rect;
    if rw <= 0.0 || rh <= 0.0 {
        return (b.x, b.y, b.w, b.h);
    }
    (((b.x - rx) / rw).clamp(0.0, 1.0), ((b.y - ry) / rh).clamp(0.0, 1.0), (b.w / rw).min(1.0), (b.h / rh).min(1.0))
}

fn srgb_to_linear(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub struct FramingAnalyser;

impl crate::Analyser for FramingAnalyser {
    fn id(&self) -> &'static str {
        ID
    }
    fn schema_version(&self) -> u32 {
        VERSION
    }
    fn wants(&self, item: &klipp_media::MediaItem) -> bool {
        !item.is_still
            && !item.video_codec.is_empty()
            && item.duration.to_secs_f64() > 0.0
            && model_path().is_some()
            && klipp_ml_ort::runtime_resolve::ensure_runtime_loadable().is_ok()
    }
    fn run(&self, item: &klipp_media::MediaItem, ctx: &crate::AnalysisCtx) -> Result<serde_json::Value, String> {
        use klipp_ml::MaskGenerator;
        let (model_id, model) = model_path().ok_or_else(|| format!("no detection model downloaded; ask with download_models ({MODEL_ID})"))?;
        let duration = item.duration.to_secs_f64();
        // The shots, as stored; a file with no shot analysis is cut
        // into stretches of five seconds instead.
        let key = crate::store::key_for(&item.path).ok_or("file not readable")?;
        let shots: Vec<(f64, f64)> = match crate::store::load(&key, crate::shots::ID, crate::shots::VERSION)
            .and_then(|v| serde_json::from_value::<crate::shots::Shots>(v).ok())
        {
            Some(s) => s.shots.iter().map(|sh| (sh.start_secs, sh.end_secs)).collect(),
            None => {
                let mut v = Vec::new();
                let mut t = 0.0;
                while t < duration {
                    v.push((t, (t + 5.0).min(duration)));
                    t += 5.0;
                }
                v
            }
        };
        let chosen: Vec<(f64, f64)> = if shots.len() > MAX_SHOTS {
            (0..MAX_SHOTS).map(|i| shots[i * shots.len() / MAX_SHOTS]).collect()
        } else {
            shots
        };
        let mut times: Vec<(usize, f64)> = Vec::new();
        for (si, (a, b)) in chosen.iter().enumerate() {
            let len = b - a;
            let n = if len < 1.0 { 1 } else { SAMPLES_PER_SHOT };
            for k in 0..n {
                let t = a + len * (k as f64 + 0.5) / n as f64;
                times.push((si, t.min(duration - 0.05).max(0.0)));
            }
        }
        times.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        let t0 = std::time::Instant::now();
        let frames = frames_at(&ctx.ffmpeg, &item.path, &times.iter().map(|(_, t)| *t).collect::<Vec<_>>())?;
        let decode_ms = t0.elapsed().as_millis();

        let mut detector = klipp_ml_ort::RfDetrSegBuilder::new()
            .weights(model)
            .model_id(model_id, "RF-DETR-Seg")
            .device(klipp_ml::DevicePreference::Cpu)
            .score_threshold(MIN_SCORE)
            .build()
            .map_err(|e| format!("detector: {e}"))?;
        let mut facts: Vec<Vec<FrameFacts>> = vec![Vec::new(); chosen.len()];
        let mut linear = vec![0.0f32; (FRAME_SIZE * FRAME_SIZE * 4) as usize];
        let t1 = std::time::Instant::now();
        for ((si, _), rgba) in times.iter().zip(frames.iter()) {
            let rect = content_rect(rgba);
            for (o, px) in linear.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
                o[0] = srgb_to_linear(px[0]);
                o[1] = srgb_to_linear(px[1]);
                o[2] = srgb_to_linear(px[2]);
                o[3] = 1.0;
            }
            let req = klipp_ml::MaskRequest {
                frame: klipp_ml::InputFrame { width: FRAME_SIZE, height: FRAME_SIZE, linear_rgba: &linear },
                prompts: klipp_ml::MaskPrompts::default(),
                temporal: None,
                tunables: klipp_ml::MaskTunables::default(),
            };
            let resp = detector.generate(req).map_err(|e| format!("detection: {e}"))?;
            let mut f = FrameFacts::default();
            let debug = std::env::var_os("KLIPP_FRAMING_DEBUG").is_some();
            for c in resp.candidates {
                if debug {
                    eprintln!("detection {:?} {:.2} {:?}", c.label, c.confidence, c.bbox.map(|b| to_content(b, rect)));
                }
                if c.confidence < MIN_SCORE {
                    continue;
                }
                match (c.label.as_deref(), c.bbox) {
                    (Some("person"), Some(b)) => f.people.push(to_content(b, rect)),
                    (Some(label), _) => f.objects.push((label.to_string(), c.confidence)),
                    _ => {}
                }
            }
            facts[*si].push(f);
        }
        tracing::info!(
            "framing of {}: {} frames, decode {decode_ms} ms, detect {} ms",
            item.filename,
            times.len(),
            t1.elapsed().as_millis()
        );
        let shots = chosen.iter().zip(&facts).map(|((a, b), f)| shot_framing(*a, *b, f)).collect();
        let out = Framing { duration_secs: duration, model: model_id.into(), frames_looked_at: times.len() as u32, shots };
        serde_json::to_value(out).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_persons_box_names_the_shot_size_and_position() {
        assert_eq!(framing_for(&[]), "no people");
        assert_eq!(framing_for(&[(0.2, 0.0, 0.6, 0.95)]), "close-up");
        assert_eq!(framing_for(&[(0.3, 0.1, 0.3, 0.8)]), "medium close-up");
        assert_eq!(framing_for(&[(0.3, 0.2, 0.2, 0.5)]), "medium");
        assert_eq!(framing_for(&[(0.1, 0.3, 0.1, 0.3)]), "wide");
        assert_eq!(framing_for(&[(0.1, 0.3, 0.05, 0.1)]), "very wide");
        assert_eq!(position_for(&[(0.05, 0.2, 0.2, 0.5)]), "left");
        assert_eq!(position_for(&[(0.7, 0.2, 0.2, 0.5)]), "right");
        // Two people: the largest decides the position.
        assert_eq!(position_for(&[(0.7, 0.2, 0.1, 0.3), (0.35, 0.1, 0.3, 0.8)]), "centre");
    }

    #[test]
    fn a_shot_takes_the_commonest_framing_the_most_people_and_the_objects() {
        let frames = vec![
            FrameFacts { people: vec![(0.3, 0.2, 0.2, 0.5)], objects: vec![("cat".into(), 0.7)] },
            FrameFacts { people: vec![(0.3, 0.2, 0.2, 0.5), (0.6, 0.3, 0.1, 0.3)], objects: vec![("cat".into(), 0.9), ("couch".into(), 0.6)] },
            FrameFacts { people: vec![(0.3, 0.1, 0.3, 0.8)], objects: vec![] },
        ];
        let s = shot_framing(1.0, 4.0, &frames);
        assert_eq!(s.framing, "medium");
        assert_eq!(s.people, 2);
        assert_eq!(s.position, "centre");
        assert_eq!(s.objects, vec!["cat".to_string(), "couch".to_string()]);
    }
}
