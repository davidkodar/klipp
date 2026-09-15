//! Transition cards rendered by the real transition code between two
//! frames of the project's own footage, on a worker thread: the card at
//! rest shows the midpoint, hovering plays the transition.

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

enum Job {
    Frames(Frames),
    Render { key: String, kind_id: u32, progress: f32, params: klipp_core::TransitionUserParams },
}

struct Done {
    key: String,
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
                let mut frames: Option<Frames> = None;
                while let Ok(job) = job_rx.recv() {
                    match job {
                        Job::Frames(f) => frames = Some(f),
                        Job::Render { key, kind_id, progress, params } => {
                            let Some(f) = frames.as_ref() else { continue };
                            let out = klipp_effects::render_transition_linear(
                                &f.from, &f.to, kind_id, progress, PREVIEW_W, PREVIEW_H,
                                params.param0, params.param1, params.param2, params.param3, params.color,
                            );
                            let rgba = klipp_effects::linear_f32_rgba_to_srgb_u8(&out);
                            if done_tx.send(Done { key, rgba }).is_err() {
                                break;
                            }
                        }
                    }
                }
            })
            .expect("transition preview thread");
        Self { textures: HashMap::new(), pending: HashSet::new(), wanted: HashSet::new(), wanted_prefixes: HashSet::new(), frames_source: None, jobs, done }
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
        displaced.extend(self.textures.drain().map(|(_, t)| t));
        self.pending.clear();
        let _ = self.jobs.send(Job::Frames(frames));
    }

    pub fn has_frames(&self) -> bool {
        self.frames_source.is_some()
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
            let _ = self.jobs.send(Job::Render { key: key.to_string(), kind_id, progress, params: *params });
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
            let _ = self.jobs.send(Job::Render { key: key.to_string(), kind_id, progress, params: *params });
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context, displaced: &mut Vec<egui::TextureHandle>) {
        let mut any = false;
        while let Ok(done) = self.done.try_recv() {
            if !self.pending.remove(&done.key) {
                continue; // from frames since replaced
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

/// A darker, slightly shifted copy of a frame, for a project with only
/// one picture to transition between.
pub fn counterpart(frame: &[f32]) -> Arc<Vec<f32>> {
    Arc::new(frame.iter().enumerate().map(|(i, v)| if i % 4 == 3 { *v } else { v * 0.45 }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_cover_the_preview_and_the_counterpart_is_darker() {
        // A 4:3 picture, solid mid grey.
        let (w, h) = (40u32, 30u32);
        let rgba = vec![128u8; (w * h * 4) as usize];
        let frame = frame_from_rgba(&rgba, w, h);
        assert_eq!(frame.len(), (PREVIEW_W * PREVIEW_H * 4) as usize);
        assert!(frame.iter().all(|v| v.is_finite()));
        assert!(frame[3] > 0.99, "alpha is opaque");
        let dark = counterpart(&frame);
        assert!(dark[0] < frame[0] && (dark[3] - frame[3]).abs() < 1e-6);
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
