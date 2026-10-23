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
// Invalidate results derived from the previous shot-boundary selection.
// v8 records whether the face detector ran, so a person with no face in
// view (walking away) is not read for skin by the back of their head.
// v9 counts only what the detectors are sure of: a cat was a person, its
// head a face graded to skin, and a street held a toilet (#462, #501).
pub const VERSION: u32 = 10;

/// The face detector, when downloaded: a face box with landmarks per
/// face on every sampled frame.
pub const FACE_MODEL_ID: &str = "yunet_face_2023mar";
/// The detectors in the registry, best first: the medium tier where it
/// is downloaded, else the preview tier. `MODEL_ID` names what
/// `download_models` fetches.
pub const MODEL_IDS: [&str; 2] = ["rf_detr_seg_medium_v1", "rf_detr_seg_v1"];
pub const MODEL_ID: &str = "rf_detr_seg_medium_v1";

/// Frames looked at per shot, and shots looked at per file at most;
/// beyond that, shots are sampled evenly.
const SAMPLES_PER_SHOT: usize = 3;
const MAX_SHOTS: usize = 40;
/// The detector's confidence below which it reports nothing.
const MIN_SCORE: f32 = 0.5;
/// A person counts from here. People the detector is sure of score 0.96
/// and up; below 0.7 were a cat on a sofa, a cushion's edge.
const PERSON_MIN: f32 = 0.7;
/// An object is named for a shot when seen in two of its frames from
/// here, or once from `OBJECT_SURE`. What was wrong on real footage — a
/// dog on a cat, a refrigerator, a surfboard on a street, a bird —
/// scored low or turned up once; what was right, a plant, a chair, the
/// cat, came back frame after frame.
const OBJECT_MIN: f32 = 0.7;
const OBJECT_SURE: f32 = 0.9;
/// A face counts from here, and only inside a person. Faces inside a
/// person scored 0.9 and up 97 times in 100; the ones outside
/// every person, a cat's head among them, scored 0.6 to 0.8.
const FACE_MIN: f32 = 0.8;
/// Frames are decoded to fit this square, letterboxed so nothing is
/// squashed; the detector resizes again to its own input. Boxes come
/// back against the padded square and are mapped to the picture.
const FRAME_SIZE: u32 = 640;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShotFraming {
    pub start_secs: f64,
    pub end_secs: f64,
    /// close-up, medium close-up, medium, wide, very wide, inset (a small
    /// figure in a corner: a face cam over a screen), no people.
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
    /// Every frame looked at, with where the people were: what a face
    /// measure reads skin in, and what a title keeps clear of.
    #[serde(default)]
    pub samples: Vec<FrameSample>,
    /// The face detector that looked at the samples, when one did. With
    /// one, a sample with people and no faces has no face in view.
    #[serde(default)]
    pub face_model: Option<String>,
}

/// The people in one sampled frame, as normalised boxes (x, y, w, h),
/// and the faces when the face model is there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameSample {
    pub secs: f64,
    pub people: Vec<(f32, f32, f32, f32)>,
    #[serde(default)]
    pub faces: Vec<FaceBox>,
}

/// A face: box and landmarks (right eye, left eye, nose, mouth corners)
/// as fractions of the frame, and the detector's confidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
    pub landmarks: Vec<(f32, f32)>,
}

/// The face model in the app's model cache, if downloaded.
pub fn face_model_path() -> Option<std::path::PathBuf> {
    let manifest = klipp_ml::manifest::load_effective().ok()?;
    let cache = klipp_ml::cache::ModelCache::open().ok()?;
    let entry = manifest.get(FACE_MODEL_ID)?;
    let p = cache.path_for(entry);
    p.is_file().then_some(p)
}

impl Framing {
    /// Whether a face detector looked at the samples. When one did, a
    /// sample with people and no faces has no face in view — someone
    /// walking away, a figure too small — and a person's head must not
    /// stand in for one.
    pub fn detects_faces(&self) -> bool {
        self.face_model.is_some()
    }

    /// The people nearest `secs` of the file, when a frame was looked at
    /// within `within` seconds of it.
    pub fn people_at(&self, secs: f64, within: f64) -> Option<&FrameSample> {
        let near = |s: &&FrameSample| (s.secs - secs).abs() <= within;
        let by_distance = |a: &&FrameSample, b: &&FrameSample| (a.secs - secs).abs().total_cmp(&(b.secs - secs).abs());
        // The nearest frame that saw someone, within the window; else the
        // nearest at all.
        self.samples.iter().filter(near).filter(|s| !s.people.is_empty() || !s.faces.is_empty()).min_by(by_distance)
            .or_else(|| self.samples.iter().filter(near).min_by(by_distance))
    }
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
    // A small figure in a corner is a face cam inset over something
    // else, not a wide shot of a person: B-roll with one is not clean.
    let (cx, cy) = (largest.0 + w / 2.0, largest.1 + h / 2.0);
    let in_a_corner = (cx < 0.28 || cx > 0.72) && (cy < 0.32 || cy > 0.68);
    if h < 0.3 && w < 0.25 && in_a_corner {
        return "inset";
    }
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
    let people = frames.iter().map(|f| f.people.len() as u32).max().unwrap_or(0);
    // A small figure the detector loses in half the frames still makes
    // the shot one with a person in it: the framing is the commonest of
    // the frames that saw someone, when any did. "no people, 1 person"
    // was what a shot read as before.
    let framing = if people == 0 {
        "no people".to_string()
    } else {
        commonest(frames.iter().filter(|f| !f.people.is_empty()).map(|f| framing_for(&f.people)))
    };
    let position = if people == 0 { String::new() } else { commonest(frames.iter().map(|f| position_for(&f.people)).filter(|p| !p.is_empty())) };
    // (label, best confidence, frames seen in)
    let mut objects: Vec<(String, f32, usize)> = Vec::new();
    for f in frames {
        let mut seen_here: Vec<&str> = Vec::new();
        for (label, c) in f.objects.iter().filter(|(_, c)| *c >= OBJECT_MIN) {
            let once = !seen_here.contains(&label.as_str());
            seen_here.push(label);
            match objects.iter_mut().find(|(l, _, _)| l == label) {
                Some(o) => {
                    o.1 = o.1.max(*c);
                    o.2 += usize::from(once);
                }
                None => objects.push((label.clone(), *c, 1)),
            }
        }
    }
    objects.retain(|(_, best, frames)| *frames >= 2 || *best >= OBJECT_SURE);
    objects.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    ShotFraming { start_secs, end_secs, framing, people, position, objects: objects.into_iter().take(5).map(|(l, _, _)| l).collect() }
}

/// What is only ever outdoors, on or by a street.
const STREET: &[&str] = &["car", "bus", "truck", "train", "motorcycle", "bicycle", "traffic light", "fire hydrant", "stop sign", "parking meter", "boat", "airplane"];
/// Fixtures of a room: the detector's indoor classes that do not leave
/// the house the way a chair or a cup can.
const FIXTURES: &[&str] = &["bed", "toilet", "microwave", "oven", "sink", "refrigerator", "toaster", "couch", "tv", "hair drier"];
/// What a real room shows besides its fixtures.
const ROOM: &[&str] = &["chair", "dining table", "potted plant", "book", "clock", "vase", "laptop", "keyboard", "mouse", "remote", "cup", "bottle", "wine glass"];

/// Room fixtures named where they cannot be: a street walk read as a
/// toilet, a microwave, a bed and a couch beside its car and traffic
/// light (#762), each seen twice at a confidence the real objects of
/// other footage also score. In footage that shows the street anywhere,
/// a fixture is named only in a shot that is plainly a room, one with two
/// fixtures or a fixture and two other things of a room: the walk named
/// the street in three of its eleven shots, and a lone toilet, couch or
/// bed in six others. The walk that goes into a café keeps its café; a
/// house filmed with no street in view keeps everything.
pub fn drop_out_of_place(shots: &mut [ShotFraming]) {
    let named = |s: &ShotFraming, set: &[&str]| s.objects.iter().filter(|o| set.contains(&o.as_str())).count();
    let outdoors = shots.iter().any(|s| named(s, STREET) > 0);
    for s in shots.iter_mut() {
        let room = named(s, FIXTURES) >= 2 || (named(s, FIXTURES) >= 1 && named(s, ROOM) >= 2);
        if outdoors && !room {
            s.objects.retain(|o| !FIXTURES.contains(&o.as_str()));
        }
    }
}

/// The faces that are people's: sure enough, and centred inside a
/// person the detector counted. The face detector fires on a cat's head
/// too, and a cat's head graded to skin took a warm evening shot up 1.5
/// stops (#501).
pub fn human_faces(faces: Vec<FaceBox>, people: &[(f32, f32, f32, f32)]) -> Vec<FaceBox> {
    faces
        .into_iter()
        .filter(|f| f.score >= FACE_MIN)
        .filter(|f| {
            let (cx, cy) = (f.x + f.w / 2.0, f.y + f.h / 2.0);
            people.iter().any(|p| cx >= p.0 && cx <= p.0 + p.2 && cy >= p.1 && cy <= p.1 + p.3)
        })
        .collect()
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
    let cache = klipp_ml::cache::ModelCache::open().map_err(|e| format!("model cache: {e:?}"))?;
    // The face model rides along: small, and what a skin read needs.
    if let Some(face) = manifest.get(FACE_MODEL_ID) {
        if !cache.path_for(face).is_file() {
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let mut progress = klipp_ml::download::NoOpProgress;
            if let Err(e) = klipp_ml::download::download_to_cache(&cache, face, &mut progress, &cancel) {
                tracing::warn!("face model download: {e:?}");
            }
        }
    }
    let entry = manifest.get(MODEL_ID).ok_or_else(|| format!("no manifest entry {MODEL_ID}"))?;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut progress = klipp_ml::download::NoOpProgress;
    klipp_ml::download::download_to_cache(&cache, entry, &mut progress, &cancel).map_err(|e| format!("download of {MODEL_ID}: {e:?}"))
}

/// The frames at `times` of `media`, each a seek and a decode from the
/// keyframe before it, letterboxed into a `FRAME_SIZE` square of RGBA8.
fn frames_at(ffmpeg: &std::path::Path, item: &klipp_media::MediaItem, times: &[f64]) -> Result<Vec<Vec<u8>>, String> {
    // The GPU scales the longest side to the square's; the CPU pads the
    // small result into it.
    let after = format!("scale={FRAME_SIZE}:{FRAME_SIZE}:force_original_aspect_ratio=decrease,pad={FRAME_SIZE}:{FRAME_SIZE}:(ow-iw)/2:(oh-ih)/2:color=black");
    crate::grab_frames(ffmpeg, item, times, FRAME_SIZE, -2, &after, (FRAME_SIZE * FRAME_SIZE * 4) as usize)
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
        // A still too (#2490): who is in a photo is what a refit centres
        // and a grade reads skin by, as in a shot.
        !item.video_codec.is_empty()
            && (item.is_still || item.duration.to_secs_f64() > 0.0)
            && model_path().is_some()
            && klipp_ml_ort::runtime_resolve::ensure_runtime_loadable().is_ok()
    }
    /// ONNX Runtime, in a worker (#689).
    fn native(&self) -> bool {
        true
    }
    fn run(&self, item: &klipp_media::MediaItem, ctx: &crate::AnalysisCtx) -> Result<serde_json::Value, String> {
        use klipp_ml::MaskGenerator;
        let (model_id, model) = model_path().ok_or_else(|| format!("no detection model downloaded; ask with download_models ({MODEL_ID})"))?;
        let duration = item.duration.to_secs_f64();
        // The shots, as stored; a file with no shot analysis is cut
        // into stretches of five seconds instead. A still is one shot,
        // its one picture read once, at its start (#2490).
        let key = crate::store::key_for(&item.path).ok_or("file not readable")?;
        let shots: Vec<(f64, f64)> = if item.is_still { vec![(0.0, duration.max(0.1))] } else { match crate::store::load(&key, crate::shots::ID, crate::shots::VERSION)
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
        } };
        let chosen: Vec<(f64, f64)> = if shots.len() > MAX_SHOTS {
            (0..MAX_SHOTS).map(|i| shots[i * shots.len() / MAX_SHOTS]).collect()
        } else {
            shots
        };
        let mut times: Vec<(usize, f64)> = Vec::new();
        for (si, (a, b)) in chosen.iter().enumerate() {
            let len = b - a;
            let n = if len < 1.0 || item.is_still { 1 } else { SAMPLES_PER_SHOT };
            for k in 0..n {
                let t = a + len * (k as f64 + 0.5) / n as f64;
                times.push((si, if item.is_still { 0.0 } else { t.min(duration - 0.05).max(0.0) }));
            }
        }
        // And a time grid, so a face is never more than a couple of
        // seconds from a sample whatever the shots: what a skin read and
        // a title's keep-out need. Capped, so a long file thins the grid
        // rather than looking at hundreds of frames.
        const GRID_SECS: f64 = 2.0;
        const MAX_FRAMES: usize = 360;
        let step = GRID_SECS.max(duration / MAX_FRAMES as f64);
        let mut t = step / 2.0;
        while t < duration && !item.is_still {
            if !times.iter().any(|(_, x)| (x - t).abs() < step * 0.5) {
                let si = chosen.iter().position(|(a, b)| t >= *a && t < *b).unwrap_or(chosen.len().saturating_sub(1));
                times.push((si, t.min(duration - 0.05).max(0.0)));
            }
            t += step;
        }
        times.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        let t0 = std::time::Instant::now();
        let frames = frames_at(&ctx.ffmpeg, item, &times.iter().map(|(_, t)| *t).collect::<Vec<_>>())?;
        let decode_ms = t0.elapsed().as_millis();

        let mut detector = klipp_ml_ort::RfDetrSegBuilder::new()
            .weights(model)
            .model_id(model_id, "RF-DETR-Seg")
            .device(klipp_ml::DevicePreference::GpuPreferCpu)
            .score_threshold(MIN_SCORE)
            .build()
            .map_err(|e| format!("detector: {e}"))?;
        let mut facts: Vec<Vec<FrameFacts>> = vec![Vec::new(); chosen.len()];
        let mut samples: Vec<FrameSample> = Vec::new();
        let mut faces_model = face_model_path().and_then(|p| klipp_ml_ort::yunet::FaceDetector::open(&p).map_err(|e| tracing::warn!("face detector: {e}")).ok());
        let mut linear = vec![0.0f32; (FRAME_SIZE * FRAME_SIZE * 4) as usize];
        let t1 = std::time::Instant::now();
        for ((si, t), rgba) in times.iter().zip(frames.iter()) {
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
                match (c.label.as_deref(), c.bbox) {
                    (Some("person"), Some(b)) if c.confidence >= PERSON_MIN => f.people.push(to_content(b, rect)),
                    (Some("person"), _) => {}
                    (Some(label), _) if c.confidence >= OBJECT_MIN => f.objects.push((label.to_string(), c.confidence)),
                    _ => {}
                }
            }
            let faces = match faces_model.as_mut() {
                Some(fd) => fd.detect(rgba, FRAME_SIZE, FRAME_SIZE).unwrap_or_default().into_iter().map(|face| {
                    let (x, y, w, h) = to_content(klipp_ml::BBox { x: face.x, y: face.y, w: face.w, h: face.h }, rect);
                    let (rx, ry, rw, rh) = rect;
                    let map = |p: (f32, f32)| if rw > 0.0 && rh > 0.0 { (((p.0 - rx) / rw).clamp(0.0, 1.0), ((p.1 - ry) / rh).clamp(0.0, 1.0)) } else { p };
                    FaceBox { x, y, w, h, score: face.score, landmarks: face.landmarks.iter().map(|p| map(*p)).collect() }
                }).collect(),
                None => Vec::new(),
            };
            let faces = human_faces(faces, &f.people);
            samples.push(FrameSample { secs: *t, people: f.people.clone(), faces });
            facts[*si].push(f);
        }
        tracing::info!(
            "framing of {}: {} frames, decode {decode_ms} ms, detect {} ms",
            item.filename,
            times.len(),
            t1.elapsed().as_millis()
        );
        let mut shots: Vec<ShotFraming> = chosen.iter().zip(&facts).map(|((a, b), f)| shot_framing(*a, *b, f)).collect();
        drop_out_of_place(&mut shots);
        let face_model = faces_model.as_ref().map(|_| FACE_MODEL_ID.to_string());
        let out = Framing { duration_secs: duration, model: model_id.into(), frames_looked_at: times.len() as u32, shots, samples, face_model };
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
        // A small figure in a corner is a face cam over a screen.
        assert_eq!(framing_for(&[(0.85, 0.75, 0.12, 0.2)]), "inset");
        assert_eq!(framing_for(&[(0.45, 0.75, 0.12, 0.15)]), "very wide", "small, but mid-frame");
        assert_eq!(position_for(&[(0.05, 0.2, 0.2, 0.5)]), "left");
        assert_eq!(position_for(&[(0.7, 0.2, 0.2, 0.5)]), "right");
        // Two people: the largest decides the position.
        assert_eq!(position_for(&[(0.7, 0.2, 0.1, 0.3), (0.35, 0.1, 0.3, 0.8)]), "centre");
    }

    #[test]
    fn a_shot_takes_the_commonest_framing_the_most_people_and_the_objects() {
        let frames = vec![
            FrameFacts { people: vec![(0.3, 0.2, 0.2, 0.5)], objects: vec![("cat".into(), 0.7), ("chair".into(), 0.95)] },
            FrameFacts { people: vec![(0.3, 0.2, 0.2, 0.5), (0.6, 0.3, 0.1, 0.3)], objects: vec![("cat".into(), 0.9), ("couch".into(), 0.6)] },
            FrameFacts { people: vec![(0.3, 0.1, 0.3, 0.8)], objects: vec![("surfboard".into(), 0.85)] },
        ];
        let s = shot_framing(1.0, 4.0, &frames);
        assert_eq!(s.framing, "medium");
        assert_eq!(s.people, 2);
        assert_eq!(s.position, "centre");
        // The cat twice, the chair once but sure; the couch too unsure,
        // the surfboard seen once.
        assert_eq!(s.objects, vec!["chair".to_string(), "cat".to_string()]);
    }

    /// #762: the street walk's toilet, microwave, bed and couch go; its
    /// car, traffic light and plant stay; a room in the same walk keeps
    /// its own; and indoor footage keeps its fixtures.
    #[test]
    fn room_fixtures_are_not_named_in_the_street() {
        let shot = |objects: &[&str]| ShotFraming { start_secs: 0.0, end_secs: 1.0, framing: "no people".into(), people: 0, position: String::new(), objects: objects.iter().map(|o| o.to_string()).collect() };
        let mut walk = vec![
            shot(&["car", "toilet", "traffic light"]),
            shot(&["potted plant", "microwave"]),
            shot(&["car", "bed"]),
            shot(&["couch", "bicycle"]),
            shot(&["chair", "dining table", "couch", "cup"]),
        ];
        drop_out_of_place(&mut walk);
        let objects: Vec<Vec<&str>> = walk.iter().map(|s| s.objects.iter().map(|o| o.as_str()).collect()).collect();
        assert_eq!(objects, vec![
            vec!["car", "traffic light"],
            vec!["potted plant"],
            vec!["car"],
            vec!["bicycle"],
            vec!["chair", "dining table", "couch", "cup"],
        ]);
        let mut home = vec![shot(&["couch", "tv"]), shot(&["microwave", "potted plant"]), shot(&["bed"])];
        let before = home.clone();
        drop_out_of_place(&mut home);
        assert_eq!(home, before, "indoors, nothing is out of place");
    }

    #[test]
    fn a_face_counts_when_sure_and_inside_a_person() {
        let face = |x: f32, score: f32| FaceBox { x, y: 0.1, w: 0.1, h: 0.15, score, landmarks: vec![] };
        let person = [(0.4, 0.0, 0.3, 1.0)];
        let kept = human_faces(vec![face(0.5, 0.92), face(0.5, 0.7), face(0.1, 0.95)], &person);
        assert_eq!(kept.len(), 1, "the sure face in the person; not the unsure one, not the one outside");
        assert_eq!(kept[0].score, 0.92);
        // The cat on the sofa: faces at 0.64 to 0.72, no person counted.
        assert!(human_faces(vec![face(0.2, 0.72)], &[]).is_empty());
    }
}
