//! The subject matte pass (#239): a segmentation model run over every
//! frame of a range of a media file, its mattes kept as a matte sequence
//! (`klipp_nodes::matte_sequence`) that an `ExternalMatte` reads frame by
//! frame.
//!
//! - **Models:** the app's own, downloaded only when the person asks
//!   (BiRefNet Lite for the whole foreground, RF-DETR-Seg for one detected
//!   subject). The pass never fetches one.
//! - **Frames:** read in order by the frame reader the render's own reads
//!   use (`SeekableFrameReader`), each named by its time as that reader
//!   names it, so a stream that starts late or a variable frame rate is
//!   named as the render asks for it; turned upright by the media's
//!   rotation before the model sees it.
//! - **Where it runs:** on the CPU, always, and the folder says so: a pass
//!   made elsewhere with another provider is another folder, never frames
//!   of two mixed.
//! - **Writing:** the model's matte goes to the raw folder, written whole
//!   and moved into place; a frame is published for the render, steadied,
//!   once the frames two after it are made (or the pass ends), so the
//!   render never sees a raw matte and then a steadied one at one frame.
//!   A cancelled pass publishes what it finished and nothing else.
//! - **A longer range** makes only the frames missing.

use std::path::{Path, PathBuf};

use klipp_media::MediaItem;
use klipp_nodes::matte_sequence::{self, Manifest};

/// The job's name where the analyses are reported.
pub const ID: &str = "subject_matte";
/// The pass's own version: a change of method is a new sequence.
pub const ALGORITHM: u32 = 1;
/// How tall the kept mattes are; their width keeps the picture's aspect.
pub const MATTE_HEIGHT: u32 = 270;
/// How tall the frames the model is shown are (upright).
pub const MODEL_HEIGHT: u32 = 576;
/// Where the models run: the CPU, so a sequence made on any machine is
/// made the same way.
pub const PROVIDER: &str = "cpu";

/// Which model makes the mattes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatteModel {
    /// The whole foreground (BiRefNet Lite).
    Foreground,
    /// One detected subject, by its label, followed from frame to frame
    /// (RF-DETR-Seg).
    Subject,
}

impl MatteModel {
    /// The model's ids in the manifest, best first.
    pub fn ids(self) -> &'static [&'static str] {
        match self {
            MatteModel::Foreground => &["birefnet_lite_v1"],
            MatteModel::Subject => &["rf_detr_seg_medium_v1", "rf_detr_seg_v1"],
        }
    }
}

/// The model's file in the app's model cache, with its id, when it is
/// downloaded. Never downloads.
pub fn model_path(model: MatteModel) -> Option<(&'static str, PathBuf)> {
    let manifest = klipp_ml::manifest::load_effective().ok()?;
    let cache = klipp_ml::cache::ModelCache::open().ok()?;
    model.ids().iter().find_map(|id| {
        let path = cache.path_for(manifest.get(id)?);
        path.is_file().then_some((*id, path))
    })
}

/// The model, downloaded and verified first when `download` (the person
/// asked) and it isn't there; refused otherwise.
pub fn ensure_model(model: MatteModel, download: bool) -> Result<(&'static str, PathBuf), String> {
    if let Some(found) = model_path(model) {
        return Ok(found);
    }
    let id = *model.ids().last().expect("a model id");
    if !download {
        return Err(format!("the model {id} is not downloaded; download it first"));
    }
    let manifest = klipp_ml::manifest::load_effective().map_err(|e| format!("model manifest: {e:?}"))?;
    let cache = klipp_ml::cache::ModelCache::open().map_err(|e| format!("model cache: {e:?}"))?;
    let entry = manifest.get(id).ok_or_else(|| format!("no manifest entry {id}"))?;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut progress = klipp_ml::download::NoOpProgress;
    let path = klipp_ml::download::download_to_cache(&cache, entry, &mut progress, &cancel).map_err(|e| format!("download of {id}: {e:?}"))?;
    Ok((id, path))
}

/// The matte sequence for `item` made by `model_id` (its file `model_bytes`
/// long) with this pass, upright by `rotation`: the media's analysis
/// folder, by model, provider, method and turn. `None` when the media
/// file isn't there.
pub fn sequence_dir(item: &MediaItem, model_id: &str, model_bytes: u64, rotation: i32) -> Option<PathBuf> {
    let key = crate::store::key_for(&item.path)?;
    Some(crate::store::analysis_dir().join(key).join(format!("subject-matte-{model_id}-{model_bytes}-{PROVIDER}-a{ALGORITHM}-r{}", rotation.rem_euclid(360))))
}

/// The kept mattes' size for a `w` × `h` (upright) picture: `MATTE_HEIGHT`
/// tall (or less, never upscaled), even width, the aspect kept.
pub fn matte_size(w: u32, h: u32) -> (u32, u32) {
    let mh = MATTE_HEIGHT.min(h.max(2));
    let mw = (((w as f64 * mh as f64 / h.max(1) as f64) / 2.0).round() as u32 * 2).max(2);
    (mw, mh)
}

/// The size the model is shown a `w` × `h` (upright) picture at.
fn model_size(w: u32, h: u32) -> (u32, u32) {
    let mh = MODEL_HEIGHT.min(h.max(2)) & !1;
    let mw = (((w as f64 * mh as f64 / h.max(1) as f64) / 2.0).round() as u32 * 2).max(2);
    (mw, mh)
}

/// One decoded frame, upright: RGBA8, `width` × `height`, at `secs` in the
/// media as the frame readers name it.
pub struct Frame {
    pub secs: f64,
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// The mattes made: their keys, of about how many asked for, how many
/// with the subject lost (an empty matte), and whether the pass stopped
/// before the end because it was told to.
#[derive(Debug, Clone, PartialEq)]
pub struct Made {
    pub frames: Vec<i64>,
    pub asked: usize,
    pub lost: usize,
    pub cancelled: bool,
}

/// How a candidate is picked from a model's answer.
pub struct Pick<'a> {
    /// A detector's label to follow ("person"); `None` takes any.
    pub label: Option<&'a str>,
}

/// The matte of `resp` to keep, `mw` × `mh`, and whether the subject was
/// lost. Following one (`previous` not empty): the candidate with `pick`'s
/// label that overlaps it most, then the largest, then the first; none
/// overlapping, the subject is lost, and the matte is empty (a gap shows
/// as a gap). With none followed yet: the most confident, then the
/// largest, then the first.
fn choose(resp: &klipp_ml::MaskResponse, pick: &Pick, previous: Option<&[u8]>, mw: u32, mh: u32) -> (Vec<u8>, bool) {
    let fits = |c: &klipp_ml::MaskCandidate| pick.label.is_none_or(|l| c.label.as_deref().is_none_or(|have| have.eq_ignore_ascii_case(l)));
    let small: Vec<(usize, f32, Vec<u8>)> = resp.candidates.iter().enumerate().filter(|(_, c)| fits(c)).map(|(i, c)| (i, c.confidence, downscale(&c.mask.alpha, c.mask.width.max(1), c.mask.height.max(1), mw, mh))).collect();
    let area = |m: &[u8]| m.iter().map(|v| *v as u64).sum::<u64>();
    let empty = || vec![0u8; (mw * mh) as usize];
    match previous.filter(|p| area(p) > 0) {
        Some(prev) => {
            let overlap = |m: &[u8]| m.iter().zip(prev).map(|(x, y)| (*x).min(*y) as u64).sum::<u64>();
            // Largest overlap, then largest area, then the first.
            let best = small.iter().filter(|c| overlap(&c.2) > 0).max_by(|a, b| overlap(&a.2).cmp(&overlap(&b.2)).then(area(&a.2).cmp(&area(&b.2))).then(b.0.cmp(&a.0)));
            match best {
                Some(c) => (c.2.clone(), false),
                None => (empty(), true),
            }
        }
        None => {
            let best = small.iter().max_by(|a, b| a.1.total_cmp(&b.1).then(area(&a.2).cmp(&area(&b.2))).then(b.0.cmp(&a.0)));
            match best {
                Some(c) => (c.2.clone(), false),
                None => (empty(), true),
            }
        }
    }
}

/// `src` (`sw` × `sh`) to `w` × `h` by area: each kept value the mean of
/// the source values under it.
fn downscale(src: &[u8], sw: u32, sh: u32, w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        let (y0, y1) = ((y * sh) / h, (((y + 1) * sh).div_ceil(h)).min(sh).max((y * sh) / h + 1));
        for x in 0..w {
            let (x0, x1) = ((x * sw) / w, (((x + 1) * sw).div_ceil(w)).min(sw).max((x * sw) / w + 1));
            let (mut sum, mut n) = (0u32, 0u32);
            for yy in y0..y1 {
                for xx in x0..x1 {
                    sum += src[(yy * sw + xx) as usize] as u32;
                    n += 1;
                }
            }
            out.push(((sum + n / 2) / n.max(1)) as u8);
        }
    }
    out
}

/// An RGBA8 picture turned clockwise by `degrees` (0, 90, 180, 270), as
/// the decoders turn a phone's frames upright.
fn rotate(rgba: &[u8], w: u32, h: u32, degrees: i32) -> (Vec<u8>, u32, u32) {
    let (nw, nh) = match degrees.rem_euclid(360) {
        90 | 270 => (h, w),
        180 => (w, h),
        _ => return (rgba.to_vec(), w, h),
    };
    let (wi, hi, nwi) = (w as usize, h as usize, nw as usize);
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for dy in 0..nh as usize {
        for dx in 0..nwi {
            let (sx, sy) = match degrees.rem_euclid(360) {
                90 => (dy, hi - 1 - dx),
                270 => (wi - 1 - dy, dx),
                _ => (wi - 1 - dx, hi - 1 - dy),
            };
            let (s, d) = ((sy * wi + sx) * 4, (dy * nwi + dx) * 4);
            out[d..d + 4].copy_from_slice(&rgba[s..s + 4]);
        }
    }
    (out, nw, nh)
}

fn srgb_to_linear(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Make the mattes of `frames` (in order) with `generator` into the
/// sequence in `dir`, skipping a frame whose raw matte is there already.
/// Each raw matte is written whole before the next; a frame is published
/// (steadied, for the render) once the frames two after it are made, and
/// the rest when the pass ends. `same_shot` keeps steadying inside a shot.
/// `progress(done, of)` is told after each frame, and stops the pass by
/// answering `false`: what is finished is published, nothing else.
#[allow(clippy::too_many_arguments)]
pub fn make_mattes(
    dir: &Path,
    manifest: &Manifest,
    frames: &mut dyn Iterator<Item = Result<Frame, String>>,
    asked: usize,
    generator: &mut dyn klipp_ml::MaskGenerator,
    pick: &Pick,
    same_shot: &dyn Fn(i64, i64) -> bool,
    progress: &mut dyn FnMut(usize, usize) -> bool,
) -> Result<Made, String> {
    matte_sequence::create(dir, manifest)?;
    let (mw, mh) = (manifest.width, manifest.height);
    let have: std::collections::HashSet<i64> = matte_sequence::frames(&matte_sequence::raw_dir(dir)).into_iter().collect();
    let mut made = Vec::new();
    let mut lost = 0;
    let mut order: Vec<i64> = Vec::new();
    let mut published = 0usize;
    let mut previous: Option<Vec<u8>> = None;
    let mut cancelled = false;
    let mut linear = Vec::new();
    for frame in frames {
        let frame = frame?;
        let key = matte_sequence::key_of(frame.secs);
        order.push(key);
        if have.contains(&key) {
            previous = matte_sequence::get(&matte_sequence::raw_dir(dir), manifest, key);
        } else {
            crate::hold::wait();
            linear.clear();
            linear.extend(frame.rgba.chunks_exact(4).flat_map(|p| [srgb_to_linear(p[0]), srgb_to_linear(p[1]), srgb_to_linear(p[2]), 1.0]));
            let req = klipp_ml::MaskRequest {
                frame: klipp_ml::InputFrame { width: frame.width, height: frame.height, linear_rgba: &linear },
                prompts: klipp_ml::MaskPrompts::default(),
                temporal: None,
                tunables: klipp_ml::MaskTunables::default(),
            };
            let resp = generator.generate(req).map_err(|e| format!("the frame at {:.3} s: {e}", frame.secs))?;
            let (matte, gone) = choose(&resp, pick, previous.as_deref(), mw, mh);
            lost += gone as usize;
            matte_sequence::put_raw(dir, manifest, key, &matte)?;
            made.push(key);
            previous = Some(matte);
        }
        // Frames whose window (two after) is complete: published now.
        while published + matte_sequence::STEADY_RADIUS < order.len() {
            let k = order[published];
            if made.contains(&k) {
                matte_sequence::publish(dir, &[k], same_shot)?;
            }
            published += 1;
        }
        if !made.is_empty() && made.last() == Some(&key) && !progress(made.len(), asked) {
            cancelled = true;
            break;
        }
    }
    let tail: Vec<i64> = order[published..].iter().copied().filter(|k| made.contains(k)).collect();
    if !tail.is_empty() {
        matte_sequence::publish(dir, &tail, same_shot)?;
    }
    Ok(Made { frames: made, asked, lost, cancelled })
}

/// The frames of `item` from `from_secs` to `to_secs`, upright, in order,
/// read as the render's frame reader reads them and named by its times,
/// scaled for the model.
pub fn decode_frames(item: &MediaItem, from_secs: f64, to_secs: f64) -> Result<impl Iterator<Item = Result<Frame, String>>, String> {
    let rotation = item.source_rotation.rem_euclid(360);
    let (cw, ch) = (item.resolution.width.max(2), item.resolution.height.max(2));
    // The coded picture's size for an upright model size.
    let (uw, uh) = if rotation == 90 || rotation == 270 { (ch, cw) } else { (cw, ch) };
    let (mw, mh) = model_size(uw, uh);
    let (dw, dh) = if rotation == 90 || rotation == 270 { (mh, mw) } else { (mw, mh) };
    let log = Some(item.effective_log_format()).filter(|f| *f != klipp_core::LogFormat::default());
    let reader = klipp_media::frame_reader::SeekableFrameReader::open_colour_managed_sized(&item.path, log, dw, dh)
        .ok_or_else(|| format!("{} could not be opened", item.path.display()))?;
    let frames = reader.into_frames_from(from_secs);
    let half = frames.frame_duration() * 0.5;
    Ok(frames
        .take_while(move |f| f.as_ref().map_or(true, |(secs, ..)| *secs <= to_secs - half))
        .map(move |f| {
            f.map(|(secs, rgba, _, _)| {
                let (rgba, width, height) = rotate(&rgba, dw, dh, rotation);
                Frame { secs, rgba, width, height }
            })
        }))
}

/// The shot boundaries of `item` the shots analysis found, when it has.
fn cuts_of(item: &MediaItem) -> Vec<f64> {
    let Some(key) = crate::store::key_for(&item.path) else { return Vec::new() };
    crate::store::load(&key, crate::shots::ID, crate::shots::VERSION)
        .and_then(|v| serde_json::from_value::<crate::shots::Shots>(v).ok())
        .map(|s| s.boundaries)
        .unwrap_or_default()
}

/// A whole pass over `from_secs`..`to_secs` of `item`: the model must be
/// downloaded already. The sequence's folder when done (or cancelled,
/// with the frames finished).
pub fn run(item: &MediaItem, model: MatteModel, label: Option<&str>, from_secs: f64, to_secs: f64, progress: &mut dyn FnMut(usize, usize) -> bool) -> Result<(PathBuf, Made), String> {
    let (model_id, weights) = ensure_model(model, false)?;
    let model_bytes = std::fs::metadata(&weights).map(|m| m.len()).unwrap_or(0);
    let rotation = item.source_rotation.rem_euclid(360);
    let dir = sequence_dir(item, model_id, model_bytes, rotation).ok_or_else(|| format!("{} is not readable", item.path.display()))?;
    let fps = item.fps.as_f64();
    if fps <= 0.0 {
        return Err("the media has no frame rate".into());
    }
    let (cw, ch) = (item.resolution.width.max(2), item.resolution.height.max(2));
    let (uw, uh) = if rotation == 90 || rotation == 270 { (ch, cw) } else { (cw, ch) };
    let (mw, mh) = matte_size(uw, uh);
    let manifest = Manifest { format: matte_sequence::FORMAT, fps, width: mw, height: mh, rotation, model: model_id.into(), model_bytes, provider: PROVIDER.into(), algorithm: ALGORITHM, steadied: false };
    let mut generator: Box<dyn klipp_ml::MaskGenerator> = match model {
        MatteModel::Foreground => Box::new(klipp_ml_ort::BiRefNetLiteBuilder::new().weights(weights).device(klipp_ml::DevicePreference::Cpu).build().map_err(|e| format!("model: {e}"))?),
        MatteModel::Subject => Box::new(
            klipp_ml_ort::RfDetrSegBuilder::new()
                .weights(weights)
                .model_id(model_id, "RF-DETR-Seg")
                .device(klipp_ml::DevicePreference::Cpu)
                .score_threshold(0.4)
                .build()
                .map_err(|e| format!("model: {e}"))?,
        ),
    };
    let cuts = cuts_of(item);
    let same_shot = move |a: i64, b: i64| {
        let (a, b) = (a.min(b) as f64 / 1e6, a.max(b) as f64 / 1e6);
        !cuts.iter().any(|c| *c > a && *c <= b)
    };
    let asked = ((to_secs - from_secs).max(0.0) * fps).round() as usize;
    let mut frames = decode_frames(item, from_secs, to_secs)?;
    let label = label.or(match model {
        MatteModel::Subject => Some("person"),
        MatteModel::Foreground => None,
    });
    let made = make_mattes(&dir, &manifest, &mut frames, asked, generator.as_mut(), &Pick { label }, &same_shot, progress)?;
    Ok((dir, made))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in model: a "person" disc where the frame's red is
    /// brightest, and a more confident "car" in the corner.
    struct Spot;

    impl klipp_ml::MaskGenerator for Spot {
        fn id(&self) -> &str {
            "spot"
        }
        fn display_name(&self) -> &str {
            "Spot"
        }
        fn capabilities(&self) -> klipp_ml::MaskCapabilities {
            klipp_ml::MaskCapabilities::AUTO_DETECT
        }
        fn preferred_device(&self) -> klipp_ml::DevicePreference {
            klipp_ml::DevicePreference::Cpu
        }
        fn generate(&mut self, req: klipp_ml::MaskRequest<'_>) -> Result<klipp_ml::MaskResponse, klipp_ml::MaskError> {
            let (w, h) = (req.frame.width, req.frame.height);
            let px = req.frame.linear_rgba;
            let best = (0..(w * h) as usize).max_by(|a, b| px[a * 4].total_cmp(&px[b * 4])).unwrap_or(0);
            let (bx, by) = ((best as u32 % w) as f32, (best as u32 / w) as f32);
            let lit = px[best * 4] > 0.2;
            let disc = |cx: f32, cy: f32| -> Vec<u8> {
                (0..w * h).map(|i| if (((i % w) as f32 - cx).powi(2) + ((i / w) as f32 - cy).powi(2)).sqrt() < h as f32 * 0.2 { 255 } else { 0 }).collect()
            };
            let cand = |mask: Vec<u8>, confidence: f32, label: &str| klipp_ml::MaskCandidate { mask: klipp_ml::MaskBuffer { width: w, height: h, alpha: mask }, confidence, bbox: None, label: Some(label.into()) };
            let mut candidates = vec![cand(disc(w as f32 * 0.1, h as f32 * 0.1), 0.9, "car")];
            if lit {
                candidates.push(cand(disc(bx, by), 0.6, "person"));
            }
            Ok(klipp_ml::MaskResponse { candidates, inference_ms: 0 })
        }
    }

    /// A frame at `secs` with a bright spot at `sx`.
    fn frame(secs: f64, sx: u32, w: u32, h: u32, lit: bool) -> Frame {
        let rgba = (0..w * h).flat_map(|i| {
            let (x, y) = (i % w, i / w);
            let d = ((x as i32 - sx as i32).pow(2) + (y as i32 - (h / 2) as i32).pow(2)) as f32;
            [if lit { (255.0 * (-d / 20.0).exp()) as u8 } else { 0 }, 20, 20, 255]
        });
        Frame { secs, rgba: rgba.collect(), width: w, height: h }
    }

    fn temp() -> PathBuf {
        std::env::temp_dir().join(format!("klipp-subject-matte-{}-{}", std::process::id(), klipp_core::MediaId::new().0))
    }

    fn manifest() -> Manifest {
        Manifest { format: matte_sequence::FORMAT, fps: 25.0, width: 32, height: 18, rotation: 0, model: "spot".into(), model_bytes: 1, provider: PROVIDER.into(), algorithm: ALGORITHM, steadied: false }
    }

    /// The pass keeps a matte a frame named by its time, following the
    /// labelled subject (not the more confident car), and says how far it
    /// got as it goes. A frame is published for the render only once the
    /// two after it are made; told to stop, the pass publishes the frames
    /// it finished and nothing else, nothing half-written; run again, it
    /// makes only the frames missing. A frame where the subject is gone
    /// is an empty matte, counted lost.
    #[test]
    fn the_pass_publishes_steadied_frames_follows_its_subject_and_resumes() {
        let (w, h) = (64u32, 36u32);
        let dir = temp();
        let m = manifest();
        let pick = Pick { label: Some("person") };
        let all = |_: i64, _: i64| true;
        let secs = |i: i64| i as f64 / 25.0;
        let mut told = Vec::new();
        let mut published_while = Vec::new();
        let mut frames = (0..10).map(|i| Ok(frame(secs(i), 10 + 2 * i as u32, w, h, i != 2)));
        let made = make_mattes(&dir, &m, &mut frames, 10, &mut Spot, &pick, &all, &mut |done, of| {
            told.push((done, of));
            published_while.push(matte_sequence::frames(&dir).len());
            done < 5
        })
        .unwrap();
        let keys: Vec<i64> = (0..5).map(|i| matte_sequence::key_of(secs(i))).collect();
        assert_eq!(made, Made { frames: keys.clone(), asked: 10, lost: 1, cancelled: true });
        assert_eq!(told, (1..=5).map(|d| (d, 10)).collect::<Vec<_>>());
        assert_eq!(published_while, vec![0, 0, 1, 2, 3], "a frame published once the two after it are made");
        assert_eq!(matte_sequence::frames(&dir), keys, "the finished frames, all published at the stop");
        assert!(std::fs::read_dir(&dir).unwrap().chain(std::fs::read_dir(matte_sequence::raw_dir(&dir)).unwrap()).flatten().all(|e| !e.file_name().to_string_lossy().contains("partial")), "nothing half-written");

        let mm = matte_sequence::read_manifest(&dir).unwrap();
        let raw = |k: i64| matte_sequence::get(&matte_sequence::raw_dir(&dir), &mm, k).unwrap();
        let mass_x = |a: &[u8]| {
            let (mut s, mut n) = (0.0, 0.0);
            for (i, v) in a.iter().enumerate() {
                s += *v as f64 * (i as u32 % 32) as f64;
                n += *v as f64;
            }
            s / n.max(1.0)
        };
        assert_eq!(raw(keys[0])[0], 0, "the car's corner is out");
        assert!(raw(keys[2]).iter().all(|v| *v == 0), "the subject gone: an empty matte, not the last one kept");
        assert!(mass_x(&raw(keys[4])) > mass_x(&raw(keys[0])) + 1.5, "found again, and followed as it moves");

        // Resumed over the whole range: only frames 5..9 are made.
        let mut frames = (0..10).map(|i| Ok(frame(secs(i), 10 + 2 * i as u32, w, h, i != 2)));
        let made = make_mattes(&dir, &m, &mut frames, 10, &mut Spot, &pick, &all, &mut |_, _| true).unwrap();
        assert_eq!(made.frames, (5..10).map(|i| matte_sequence::key_of(secs(i))).collect::<Vec<_>>());
        assert_eq!(matte_sequence::frames(&dir).len(), 10);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The same answer picks the same subject every time: the larger of
    /// two equal overlaps, the first of two equal ones.
    #[test]
    fn the_pick_is_the_same_every_time() {
        let mask = |cells: &[usize]| klipp_ml::MaskBuffer { width: 4, height: 1, alpha: (0..4).map(|i| if cells.contains(&i) { 255 } else { 0 }).collect() };
        let cand = |m, c| klipp_ml::MaskCandidate { mask: m, confidence: c, bbox: None, label: Some("person".into()) };
        let resp = klipp_ml::MaskResponse { candidates: vec![cand(mask(&[1]), 0.5), cand(mask(&[1, 2]), 0.5), cand(mask(&[1, 3]), 0.5)], inference_ms: 0 };
        let prev = [0u8, 255, 0, 0];
        let pick = Pick { label: None };
        let (m, lost) = choose(&resp, &pick, Some(&prev), 4, 1);
        assert_eq!((m, lost), (vec![0, 255, 255, 0], false), "equal overlap: the larger, then the first of the larger");
        let (m, _) = choose(&resp, &pick, None, 4, 1);
        assert_eq!(m, vec![0, 255, 255, 0], "equal confidence: the larger, the first");
    }

    #[test]
    fn sizes_and_turns() {
        assert_eq!(matte_size(1920, 1080), (480, 270));
        assert_eq!(matte_size(1080, 1920), (152, 270));
        assert_eq!(matte_size(320, 180), (320, 180), "never upscaled");
        assert_eq!(downscale(&[0, 255, 255, 0], 2, 2, 1, 1), vec![128]);
        // A 2×1 picture, red then blue, turned a quarter clockwise: red on top.
        let (r, w, h) = rotate(&[255, 0, 0, 255, 0, 0, 255, 255], 2, 1, 90);
        assert_eq!((w, h, r[0], r[6]), (1, 2, 255, 255));
    }

    /// Made by ffmpeg: a bar moving right a column a frame over a black
    /// picture, its frame number in red (`8 × n`), and the file's own
    /// timing given by `extra`.
    fn test_file(dir: &Path, name: &str, extra: &[&str]) -> Option<PathBuf> {
        let ctx = crate::AnalysisCtx::default();
        if !ctx.ffmpeg_available() {
            return None;
        }
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        let mut cmd = std::process::Command::new(&ctx.ffmpeg);
        cmd.args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=black:s=64x36:r=25:d=1.6", "-vf", "format=gbrp,geq=r='if(between(X,4+N,6+N),255,8*N)':g='if(between(X,4+N,6+N),255,0)':b=0"])
            .args(extra)
            .args(["-pix_fmt", "gbrp", "-c:v", "ffv1", "-y"])
            .arg(&path);
        // ffmpeg is here: a file it won't make is the test's fault, said.
        assert!(cmd.status().is_ok_and(|s| s.success()), "ffmpeg made no {name}");
        Some(path)
    }

    fn item_for(path: &Path, rotation: i32) -> MediaItem {
        let mut item = MediaItem::synthetic_for_test(path.to_path_buf(), None);
        item.resolution = klipp_core::Resolution { width: 64, height: 36 };
        item.fps = klipp_core::Fps::new(25, 1);
        item.source_rotation = rotation;
        item
    }

    /// The pass's frames are the render reader's: a file whose stream
    /// starts two frames late, and one with uneven frame times, each frame
    /// named by the time `SeekableFrameReader::decode_at` gives that very
    /// frame for, found by the bar's place (not a colour).
    #[test]
    fn the_pass_names_frames_as_the_render_reads_them() {
        let dir = temp();
        let cases: [(&str, Vec<&str>); 3] = [
            ("plain.mkv", vec![]),
            ("late.mkv", vec!["-output_ts_offset", "0.08"]),
            // 25 frames a second, then 40 after the tenth.
            ("uneven.mkv", vec!["-vf", "format=gbrp,geq=r='if(between(X,4+N,6+N),255,8*N)':g='if(between(X,4+N,6+N),255,0)':b=0,settb=1/1000,setpts='if(gt(N,10),(10/25+(N-10)/40)/TB,N/25/TB)'", "-fps_mode", "passthrough", "-enc_time_base", "1/1000"]),
        ];
        let bar = |rgba: &[u8], w: usize| (0..w).find(|x| rgba[(18 * w + x) * 4 + 1] > 128);
        for (name, extra) in cases {
            let Some(path) = test_file(&dir, name, &extra) else {
                eprintln!("skipping: no ffmpeg");
                return;
            };
            let item = item_for(&path, 0);
            let frames: Vec<Frame> = decode_frames(&item, 0.0, 1.7).unwrap().collect::<Result<_, _>>().unwrap();
            assert!(frames.len() >= 30, "{name}: {} frames", frames.len());
            // The files are what they say: late from the first frame, or
            // uneven past the tenth.
            match name {
                "late.mkv" => assert!((frames[0].secs - 0.08).abs() < 1e-3, "{name}: starts at {}", frames[0].secs),
                "uneven.mkv" => assert!((frames[12].secs - frames[11].secs - 0.025).abs() < 1e-3, "{name}: {} then {}", frames[11].secs, frames[12].secs),
                _ => {}
            }
            let mut reader = klipp_media::frame_reader::SeekableFrameReader::open_rgba(&path).unwrap();
            for f in frames.iter().step_by(3) {
                let (rgba, w, _) = reader.decode_at(f.secs).unwrap();
                let (got, want) = (bar(&f.rgba, f.width as usize).map(|x| x as f32 / f.width as f32), bar(&rgba, w as usize).map(|x| x as f32 / w as f32));
                assert!(got.is_some() && want.is_some() && (got.unwrap() - want.unwrap()).abs() < 0.007, "{name}: the frame named {:.3} s is the reader's frame there ({got:?} against {want:?})", f.secs);
            }
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A phone's frames are turned upright before the model sees them,
    /// and the mattes are upright: a bar that moves down the turned
    /// picture's rows.
    #[test]
    fn a_turned_media_is_seen_upright() {
        let dir = temp();
        let Some(path) = test_file(&dir, "turned.mkv", &[]) else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let item = item_for(&path, 90);
        let f: Frame = decode_frames(&item, 0.4, 0.44).unwrap().next().unwrap().unwrap();
        assert!(f.height > f.width, "upright: {}×{}", f.width, f.height);
        // Turned a quarter clockwise, the coded picture's columns are the
        // upright one's rows: the bar is a horizontal band.
        let row_lit = |y: u32| (0..f.width).all(|x| f.rgba[((y * f.width + x) * 4 + 1) as usize] > 128);
        assert!((0..f.height).any(row_lit), "the bar lies across the upright picture");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The real model over a real shot, by hand: `KLIPP_MATTE_SAMPLE` a
    /// video with a person in its first second, RF-DETR-Seg downloaded.
    #[test]
    #[ignore]
    fn the_subject_model_over_a_real_shot() {
        let Ok(sample) = std::env::var("KLIPP_MATTE_SAMPLE") else { return };
        let (id, weights) = ensure_model(MatteModel::Subject, false).expect("RF-DETR-Seg downloaded");
        let probe = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0"])
            .arg(&sample)
            .output()
            .unwrap();
        let wh: Vec<u32> = String::from_utf8_lossy(&probe.stdout).trim().split(',').map(|v| v.parse().unwrap()).collect();
        let mut item = MediaItem::synthetic_for_test(PathBuf::from(&sample), None);
        item.resolution = klipp_core::Resolution { width: wh[0], height: wh[1] };
        let (mw, mh) = matte_size(wh[0], wh[1]);
        let dir = temp();
        let manifest = Manifest { width: mw, height: mh, model: id.into(), ..manifest() };
        let mut generator = klipp_ml_ort::RfDetrSegBuilder::new().weights(weights).model_id(id, "RF-DETR-Seg").device(klipp_ml::DevicePreference::Cpu).score_threshold(0.4).build().unwrap();
        let mut frames = decode_frames(&item, 0.0, 1.0).unwrap();
        let t = std::time::Instant::now();
        let made = make_mattes(&dir, &manifest, &mut frames, 25, &mut generator, &Pick { label: Some("person") }, &|_, _| true, &mut |_, _| true).unwrap();
        eprintln!("{} frames in {:.1} s, the subject lost in {}", made.frames.len(), t.elapsed().as_secs_f64(), made.lost);
        for k in made.frames.iter().step_by(6) {
            let m = matte_sequence::get(&dir, &manifest, *k).unwrap();
            let cover = m.iter().filter(|v| **v > 127).count() as f64 / m.len() as f64;
            eprintln!("{:.3} s: {:.1}% covered", *k as f64 / 1e6, cover * 100.0);
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
