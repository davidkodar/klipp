//! Transition cards rendered by the real transition code, on a worker
//! thread, between two sample pictures: the card at rest shows a moment
//! that says what the transition does, hovering plays it.
//!
//! The pictures are the same in every project. They were the first two
//! thumbnails the media pool happened to hand out, in no order: a face
//! from imported artwork turned up over street footage on every card, and
//! changed between projects and reloads (#547).

use crossbeam_channel::{Receiver, Sender};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const PREVIEW_W: u32 = 320;
pub const PREVIEW_H: u32 = 180;
pub const REST_PROGRESS: f32 = 0.5;
/// Hover frames are quantised to this step so they are shared and cached.
pub const PROGRESS_STEP: f32 = 1.0 / 24.0;

/// The two pictures a transition runs between, linear light, at
/// `PREVIEW_W` × `PREVIEW_H`.
#[derive(Clone)]
pub struct Frames {
    pub from: Arc<Vec<f32>>,
    pub to: Arc<Vec<f32>>,
    /// Which media the frames came from, to notice a change.
    pub source: String,
}

/// Every job and result carries the generation of the frames it is for,
/// so a picture rendered from frames since replaced can never satisfy,
/// or take the place of, a request for the frames now there (#550).
enum Job {
    Frames(Frames, u64),
    Render { key: String, generation: u64, kind_id: u32, progress: f32, params: klipp_core::TransitionUserParams },
}

struct Done {
    key: String,
    generation: u64,
    rgba: Vec<u8>,
}

pub struct TransitionPreviews {
    textures: HashMap<String, egui::TextureHandle>,
    pending: HashSet<String>,
    wanted: HashSet<String>,
    /// Prefixes (one per card in use this frame) whose every cached
    /// step stays resident, so a hover loop never re-renders.
    wanted_prefixes: HashSet<String>,
    frames_source: Option<String>,
    /// The frames' generation, moved on by every change of frames.
    generation: u64,
    jobs: Sender<Job>,
    done: Receiver<Done>,
}

const CAP: usize = 160;

impl Default for TransitionPreviews {
    fn default() -> Self {
        let (jobs, job_rx) = crossbeam_channel::unbounded::<Job>();
        let (done_tx, done) = crossbeam_channel::unbounded::<Done>();
        std::thread::Builder::new()
            .name("transition-previews".into())
            .spawn(move || {
                let mut frames: Option<(Frames, u64)> = None;
                while let Ok(job) = job_rx.recv() {
                    match job {
                        Job::Frames(f, g) => frames = Some((f, g)),
                        Job::Render { key, generation, kind_id, progress, params } => {
                            // Rendered from the frames asked for or not at
                            // all: frames since replaced make the wrong
                            // picture.
                            let Some((f, _)) = frames.as_ref().filter(|(_, g)| *g == generation) else { continue };
                            let out = klipp_effects::render_transition_linear(
                                &f.from, &f.to, kind_id, progress, PREVIEW_W, PREVIEW_H,
                                params.param0, params.param1, params.param2, params.param3, params.color,
                            );
                            let rgba = klipp_effects::linear_f32_rgba_to_srgb_u8(&out);
                            if done_tx.send(Done { key, generation, rgba }).is_err() {
                                break;
                            }
                        }
                    }
                }
            })
            .expect("transition preview thread");
        Self { textures: HashMap::new(), pending: HashSet::new(), wanted: HashSet::new(), wanted_prefixes: HashSet::new(), frames_source: None, generation: 0, jobs, done }
    }
}

impl TransitionPreviews {
    pub fn key(name: &str, progress: f32) -> String {
        format!("{name}\u{1}{:.4}", progress)
    }

    /// The hover moment quantised to the frame step.
    pub fn hover_progress(progress: f32) -> f32 {
        (progress / PROGRESS_STEP).round() * PROGRESS_STEP
    }

    /// Hand the worker new frames. Every cached picture was made from
    /// the old ones and is dropped.
    pub fn set_frames(&mut self, frames: Frames, displaced: &mut Vec<egui::TextureHandle>) {
        if self.frames_source.as_deref() == Some(frames.source.as_str()) {
            return;
        }
        self.frames_source = Some(frames.source.clone());
        self.generation += 1;
        displaced.extend(self.textures.drain().map(|(_, t)| t));
        self.pending.clear();
        let _ = self.jobs.send(Job::Frames(frames, self.generation));
    }

    /// No frames to render from: every picture made from the old ones
    /// goes, and so does every request still out for them.
    pub fn clear_frames(&mut self, displaced: &mut Vec<egui::TextureHandle>) {
        if self.frames_source.take().is_some() {
            self.generation += 1;
            displaced.extend(self.textures.drain().map(|(_, t)| t));
            self.pending.clear();
        }
    }

    pub fn has_frames(&self) -> bool {
        self.frames_source.is_some()
    }

    /// Which frames the cards are rendered from: `"sample"`, or a
    /// selected cut's (#547).
    pub fn source(&self) -> Option<&str> {
        self.frames_source.as_deref()
    }

    pub fn get_or_request(
        &mut self,
        key: &str,
        kind_id: u32,
        progress: f32,
        params: &klipp_core::TransitionUserParams,
    ) -> Option<&egui::TextureHandle> {
        self.wanted.insert(key.to_string());
        if let Some(i) = key.rfind('\u{1}') {
            self.wanted_prefixes.insert(key[..=i].to_string());
        }
        if self.frames_source.is_some() && !self.textures.contains_key(key) && !self.pending.contains(key) {
            self.pending.insert(key.to_string());
            let _ = self.jobs.send(Job::Render { key: key.to_string(), generation: self.generation, kind_id, progress, params: *params });
        }
        self.textures.get(key)
    }

    /// While the requested step is still rendering: the cached step
    /// nearest to it, so the picture never jumps to a distant frame.
    pub fn nearest_for(&self, name: &str, progress: f32) -> Option<&egui::TextureHandle> {
        let prefix = format!("{name}\u{1}");
        self.textures
            .iter()
            .filter_map(|(k, t)| {
                let p: f32 = k.strip_prefix(&prefix)?.parse().ok()?;
                Some(((p - progress).abs(), t))
            })
            .min_by(|(a, _), (b, _)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(_, t)| t)
    }

    /// Queue a step without reading it, so the frames a hover is about
    /// to need are rendering before they are asked for.
    pub fn request(&mut self, key: &str, kind_id: u32, progress: f32, params: &klipp_core::TransitionUserParams) {
        self.wanted.insert(key.to_string());
        if let Some(i) = key.rfind('\u{1}') {
            self.wanted_prefixes.insert(key[..=i].to_string());
        }
        if self.frames_source.is_some() && !self.textures.contains_key(key) && !self.pending.contains(key) {
            self.pending.insert(key.to_string());
            let _ = self.jobs.send(Job::Render { key: key.to_string(), generation: self.generation, kind_id, progress, params: *params });
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context, displaced: &mut Vec<egui::TextureHandle>) {
        let mut any = false;
        while let Ok(done) = self.done.try_recv() {
            // From frames since replaced: it neither shows nor clears the
            // request for the same key made for the frames now there.
            if done.generation != self.generation || !self.pending.remove(&done.key) {
                continue;
            }
            let pixels: Vec<egui::Color32> = done
                .rgba
                .chunks_exact(4)
                .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
                .collect();
            let image = egui::ColorImage { size: [PREVIEW_W as usize, PREVIEW_H as usize], pixels };
            let tex = crate::app::alloc_texture_logged(ctx, "transition_preview", image, egui::TextureOptions::LINEAR);
            if let Some(old) = self.textures.insert(done.key, tex) {
                displaced.push(old);
            }
            any = true;
        }
        if self.textures.len() > CAP {
            let wanted = std::mem::take(&mut self.wanted);
            let prefixes = std::mem::take(&mut self.wanted_prefixes);
            let keys: Vec<String> = self
                .textures
                .keys()
                .filter(|k| !wanted.contains(*k) && !prefixes.iter().any(|p| k.starts_with(p.as_str())))
                .cloned()
                .collect();
            for k in keys {
                if let Some(old) = self.textures.remove(&k) {
                    displaced.push(old);
                }
            }
        } else {
            self.wanted.clear();
            self.wanted_prefixes.clear();
        }
        if any {
            ctx.request_repaint();
        }
    }

    pub fn drain(&mut self) -> Vec<egui::TextureHandle> {
        self.textures.drain().map(|(_, t)| t).collect()
    }
}

/// An sRGB RGBA picture resampled into the preview size whole, black
/// either side of a portrait one or above and below a wide one, so a
/// clip's framing is kept (#547), in linear light.
pub fn frame_fit_from_rgba(rgba: &[u8], w: u32, h: u32) -> Arc<Vec<f32>> {
    let (pw, ph) = (PREVIEW_W as usize, PREVIEW_H as usize);
    let (w, h) = (w.max(1) as usize, h.max(1) as usize);
    let scale = (pw as f32 / w as f32).min(ph as f32 / h as f32);
    let (dw, dh) = ((w as f32 * scale).round() as usize, (h as f32 * scale).round() as usize);
    let (ox, oy) = ((pw - dw.min(pw)) / 2, (ph - dh.min(ph)) / 2);
    let mut out = vec![0u8; pw * ph * 4];
    for px in out.chunks_exact_mut(4) {
        px[3] = 255;
    }
    for y in 0..dh.min(ph) {
        let sy = ((y as f32 + 0.5) / scale) as usize;
        for x in 0..dw.min(pw) {
            let sx = ((x as f32 + 0.5) / scale) as usize;
            let si = (sy.min(h - 1) * w + sx.min(w - 1)) * 4;
            let di = ((oy + y) * pw + ox + x) * 4;
            if si + 4 <= rgba.len() {
                out[di..di + 3].copy_from_slice(&rgba[si..si + 3]);
            }
        }
    }
    Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&out))
}

/// The frames either side of the selected edit point, for "Preview
/// selected cut" (#547): the outgoing clip's last frame and the incoming
/// clip's first, from their filmstrips, framing kept; `Ok(None)` when
/// they are the frames `current` already names. `Err` says why there is
/// nothing to preview. The project is only read.
pub fn cut_frames(state: &crate::state::AppState, current: Option<&str>) -> Result<Option<Frames>, String> {
    let Some(edit) = state.selected_edits.first() else {
        return Err("select an edit point between two clips (U, or click a cut)".into());
    };
    let (out, inc) = crate::trim_view::sides(state.sequence(), edit, |m| state.media_pool.get(&m).map(|i| (i.duration.to_secs_f64(), i.filename.clone())));
    let (Some(out), Some(inc)) = (out, inc) else {
        return Err("the selected edit has a gap on one side".into());
    };
    // Keyed by the clips and the frames, so another cut, or a trim of
    // this one, renders again.
    let at = |s: &crate::trim_view::Side| s.frame.map(|(_, secs)| (secs * 1000.0).round() as i64);
    let source = format!("cut:{:?}@{:?}|{:?}@{:?}", edit.outgoing, at(&out), edit.incoming, at(&inc));
    // The filmstrip frame nearest each side's frame at the cut.
    let nearest = |side: &crate::trim_view::Side| -> Result<&klipp_media::FilmstripFrame, String> {
        let (media, secs) = side.frame.ok_or_else(|| format!("\"{}\" is not a media clip", side.name))?;
        let strip = state.thumbnail_cache.filmstrip(&media).filter(|s| !s.is_empty()).ok_or_else(|| format!("no thumbnails for \"{}\" yet", side.name))?;
        Ok(strip.iter().min_by(|a, b| (a.time_secs - secs).abs().total_cmp(&(b.time_secs - secs).abs())).expect("not empty"))
    };
    let (a, b) = (nearest(&out)?, nearest(&inc)?);
    if current == Some(source.as_str()) {
        return Ok(None);
    }
    Ok(Some(Frames { from: frame_fit_from_rgba(&a.rgba, a.width, a.height), to: frame_fit_from_rgba(&b.rgba, b.width, b.height), source }))
}

/// Resample an sRGB RGBA picture to the preview size, covering it
/// (cropping the longer side), and convert to linear light.
pub fn frame_from_rgba(rgba: &[u8], w: u32, h: u32) -> Arc<Vec<f32>> {
    let (pw, ph) = (PREVIEW_W as usize, PREVIEW_H as usize);
    let (w, h) = (w as usize, h as usize);
    let src_aspect = w as f32 / h as f32;
    let dst_aspect = pw as f32 / ph as f32;
    // The source window that maps onto the preview.
    let (sx0, sy0, sw, sh) = if src_aspect > dst_aspect {
        let sw = h as f32 * dst_aspect;
        ((w as f32 - sw) / 2.0, 0.0, sw, h as f32)
    } else {
        let sh = w as f32 / dst_aspect;
        (0.0, (h as f32 - sh) / 2.0, w as f32, sh)
    };
    let mut out = vec![0u8; pw * ph * 4];
    for y in 0..ph {
        let sy = ((sy0 + (y as f32 + 0.5) * sh / ph as f32) as usize).min(h.saturating_sub(1));
        for x in 0..pw {
            let sx = ((sx0 + (x as f32 + 0.5) * sw / pw as f32) as usize).min(w.saturating_sub(1));
            let si = (sy * w + sx) * 4;
            let di = (y * pw + x) * 4;
            if si + 4 <= rgba.len() {
                out[di..di + 4].copy_from_slice(&rgba[si..si + 4]);
                out[di + 3] = 255;
            }
        }
    }
    Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&out))
}

/// The two sample pictures every card runs between: a warm dusk with a
/// sun over dark hills, then a cool day over a green treeline. Unlike in
/// colour and in shape, so a wipe's edge, a slide's travel and a
/// dissolve's blend all read at a glance, and drawn here, so they are the
/// same everywhere and cost nothing to fetch.
pub fn sample_frames() -> Frames {
    let (w, h) = (PREVIEW_W as usize, PREVIEW_H as usize);
    let paint = |f: &dyn Fn(f32, f32) -> [u8; 3]| -> Arc<Vec<f32>> {
        let mut rgba = vec![255u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let c = f(x as f32 / w as f32, y as f32 / h as f32);
                rgba[(y * w + x) * 4..(y * w + x) * 4 + 3].copy_from_slice(&c);
            }
        }
        Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&rgba))
    };
    let mix = |a: [f32; 3], b: [f32; 3], t: f32| -> [u8; 3] { std::array::from_fn(|i| (a[i] + (b[i] - a[i]) * t.clamp(0.0, 1.0)).round() as u8) };
    let from = paint(&|u, v| {
        let hill = 0.68 + 0.07 * (u * 7.0).sin() + 0.04 * (u * 17.0 + 1.0).sin();
        let sun = ((u - 0.7).powi(2) * 3.2 + (v - 0.42).powi(2)).sqrt();
        if v > hill {
            mix([62.0, 36.0, 46.0], [30.0, 18.0, 26.0], (v - hill) * 3.0)
        } else if sun < 0.09 {
            [255, 236, 190]
        } else {
            mix([236.0, 112.0, 70.0], [252.0, 196.0, 120.0], v / hill)
        }
    });
    let to = paint(&|u, v| {
        let ground = 0.62;
        // A row of trees: triangles standing on the ground line.
        let (cell, x) = ((u * 9.0).floor(), (u * 9.0).fract());
        let tree_h = 0.22 + 0.08 * ((cell * 2.3).sin() * 0.5 + 0.5);
        let tree = v > ground - tree_h * (1.0 - (x - 0.5).abs() * 2.0) && v <= ground;
        if v > ground {
            mix([96.0, 138.0, 64.0], [58.0, 92.0, 40.0], (v - ground) * 2.5)
        } else if tree {
            [34, 84, 58]
        } else {
            mix([70.0, 128.0, 206.0], [176.0, 214.0, 238.0], v / ground)
        }
    });
    Frames { from, to, source: "sample".into() }
}

/// The moment a card shows at rest: one that says what the transition
/// does (#547). Half way is a wipe's edge, a slide's travel, a zoom's
/// push; a dip through a colour is solid colour half way, an empty card,
/// so it rests on the first picture going down into it.
pub fn rest_progress(kind: klipp_core::TransitionKind) -> f32 {
    let name = format!("{kind:?}");
    if name.starts_with("Dip") || name.contains("Flash") {
        0.3
    } else {
        REST_PROGRESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_cover_the_preview() {
        // A 4:3 picture, solid mid grey.
        let (w, h) = (40u32, 30u32);
        let rgba = vec![128u8; (w * h * 4) as usize];
        let frame = frame_from_rgba(&rgba, w, h);
        assert_eq!(frame.len(), (PREVIEW_W * PREVIEW_H * 4) as usize);
        assert!(frame.iter().all(|v| v.is_finite()));
        assert!(frame[3] > 0.99, "alpha is opaque");
    }

    /// #547: the samples are the preview's size, opaque, the same every
    /// time, and unlike each other: warm against cool.
    /// #547: a portrait clip keeps its framing: black either side, the
    /// picture whole in the middle.
    #[test]
    fn a_fitted_frame_keeps_the_pictures_framing() {
        let (w, h) = (90u32, 160u32);
        let f = frame_fit_from_rgba(&vec![255u8; (w * h * 4) as usize], w, h);
        let at = |x: usize, y: usize| f[(y * PREVIEW_W as usize + x) * 4];
        assert_eq!(at(5, 90), 0.0, "black at the side");
        assert!(at(160, 90) > 0.9, "the picture in the middle");
    }

    /// #547: the selected cut's own frames, or the reason there are none;
    /// asked again for the same cut, nothing new is made.
    #[test]
    fn the_selected_cut_previews_its_own_frames_or_says_why_not() {
        use klipp_core::{MediaId, RationalTime};
        use klipp_timeline::{Clip, Track, TrackKind};
        let tc = |s: f64| RationalTime::from_secs_f64(s, 25);
        let mut state = crate::state::AppState::default();
        let (m1, m2) = (MediaId::new(), MediaId::new());
        let seq = state.sequence_mut();
        seq.fps = klipp_core::Fps::new(25, 1);
        let mut v = Track::new("V1", TrackKind::Video);
        v.clips = vec![Clip::media(m1, tc(0.0), tc(4.0), tc(0.0), tc(4.0)), Clip::media(m2, tc(4.0), tc(8.0), tc(0.0), tc(4.0))];
        seq.tracks = vec![v];
        let why = |state: &crate::state::AppState| match cut_frames(state, None) {
            Err(why) => why,
            Ok(_) => panic!("frames where there should be none"),
        };
        assert!(why(&state).contains("select an edit point"));
        state.selected_edits = vec![crate::edit_point::nearest(state.sequence(), tc(4.0), |_| true).unwrap()];
        assert!(why(&state).contains("no thumbnails"), "the filmstrips are not there yet");
        let strip = |shade: u8| vec![klipp_media::FilmstripFrame { time_secs: 0.0, rgba: vec![shade; 16 * 9 * 4], width: 16, height: 9 }];
        state.thumbnail_cache.set_filmstrip(m1, strip(40));
        state.thumbnail_cache.set_filmstrip(m2, strip(220));
        let Ok(Some(frames)) = cut_frames(&state, None) else { panic!("new frames") };
        assert!(frames.source.starts_with("cut:"));
        assert!(frames.from[0] < frames.to[0], "the outgoing clip's frame, then the incoming one's");
        assert!(matches!(cut_frames(&state, Some(&frames.source)), Ok(None)), "the same cut again: nothing new");
    }

    #[test]
    fn the_sample_pair_is_fixed_and_the_two_differ() {
        let (a, b) = (sample_frames(), sample_frames());
        assert!(a.from == b.from && a.to == b.to, "the same every time");
        assert_eq!(a.from.len(), (PREVIEW_W * PREVIEW_H * 4) as usize);
        let mean = |f: &[f32], c: usize| f.chunks_exact(4).map(|p| p[c]).sum::<f32>() / (f.len() / 4) as f32;
        assert!(mean(&a.from, 0) > mean(&a.from, 2), "the first is warm");
        assert!(mean(&a.to, 2) > mean(&a.to, 0), "the second is cool");
        assert!(a.from.chunks_exact(4).all(|p| p[3] > 0.99));
    }

    /// #550: a result rendered from frames since replaced neither shows
    /// nor clears the request for the same key made for the frames now
    /// there, whatever order the two arrive in.
    #[test]
    fn a_result_from_older_frames_never_stands_for_the_current_ones() {
        let ctx = egui::Context::default();
        let (jobs, _job_rx) = crossbeam_channel::unbounded::<Job>();
        let (done_tx, done) = crossbeam_channel::unbounded::<Done>();
        let mut p = TransitionPreviews { textures: HashMap::new(), pending: HashSet::new(), wanted: HashSet::new(), wanted_prefixes: HashSet::new(), frames_source: None, generation: 0, jobs, done };
        let mut displaced = Vec::new();
        let frames = |source: &str| Frames { from: Arc::new(vec![0.0; 4]), to: Arc::new(vec![0.0; 4]), source: source.into() };
        let params = klipp_core::TransitionUserParams::default();
        let key = TransitionPreviews::key("Wipe Left", 0.5);
        let rgba = vec![0u8; (PREVIEW_W * PREVIEW_H * 4) as usize];
        p.set_frames(frames("old"), &mut displaced);
        let old = p.generation;
        p.request(&key, 1, 0.5, &params);
        p.set_frames(frames("new"), &mut displaced);
        p.request(&key, 1, 0.5, &params);
        // The old frames' picture arrives first.
        done_tx.send(Done { key: key.clone(), generation: old, rgba: rgba.clone() }).unwrap();
        p.poll(&ctx, &mut displaced);
        assert!(!p.textures.contains_key(&key), "the old picture is not shown");
        assert!(p.pending.contains(&key), "and the current request is still out");
        done_tx.send(Done { key: key.clone(), generation: p.generation, rgba }).unwrap();
        p.poll(&ctx, &mut displaced);
        assert!(p.textures.contains_key(&key) && !p.pending.contains(&key), "the current one is");
        // No frames at all: everything made from the last ones goes.
        p.clear_frames(&mut displaced);
        assert!(p.textures.is_empty() && !p.has_frames());
    }

    #[test]
    fn a_pending_step_shows_its_nearest_neighbour_not_the_rest_frame() {
        // While step 0.08 renders, a hover at 0.08 must show 0.04, never
        // jump back to the resting midpoint at 0.5.
        let ctx = egui::Context::default();
        let mut p = TransitionPreviews::default();
        let img = egui::ColorImage::new([2, 2], egui::Color32::BLACK);
        let mut ids = Vec::new();
        for step in [0.5f32, 0.0, 0.04] {
            let t = ctx.load_texture(format!("t{step}"), img.clone(), egui::TextureOptions::LINEAR);
            ids.push((step, t.id()));
            p.textures.insert(TransitionPreviews::key("Wipe Left", step), t);
        }
        let got = p.nearest_for("Wipe Left", 0.08).map(|t| t.id());
        assert_eq!(got, ids.iter().find(|(s, _)| *s == 0.04).map(|(_, id)| *id));
        assert!(p.nearest_for("Other", 0.08).is_none());
    }

    #[test]
    fn hover_progress_is_quantised_and_keys_are_stable() {
        let a = TransitionPreviews::hover_progress(0.4999);
        let b = TransitionPreviews::hover_progress(0.5001);
        assert_eq!(a, b);
        assert_eq!(TransitionPreviews::key("Cross Dissolve", a), TransitionPreviews::key("Cross Dissolve", b));
    }
}
