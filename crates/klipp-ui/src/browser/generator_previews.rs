//! Generator cards rendered by the generator renderer itself, on a
//! worker thread: a countdown card counts, a grain plate grains, a
//! slate is the slate you get.

use crossbeam_channel::{Receiver, Sender};
use std::collections::{HashMap, HashSet};

pub const PREVIEW_W: u32 = 320;
pub const PREVIEW_H: u32 = 180;
/// The clip length a preview pretends to have and the moment a card
/// shows at rest.
pub const PREVIEW_WINDOW: f64 = 5.0;
pub const REST_TIME: f32 = 0.6;
pub const LOOP_SECS: f32 = 5.0;
pub const FRAME_STEP: f32 = 1.0 / 15.0;

struct Job {
    key: String,
    data: klipp_core::GeneratorData,
    time: f32,
}

struct Done {
    key: String,
    rgba: Vec<u8>,
}

pub struct GeneratorPreviews {
    textures: HashMap<String, egui::TextureHandle>,
    pending: HashSet<String>,
    wanted: HashSet<String>,
    wanted_prefixes: HashSet<String>,
    jobs: Sender<Job>,
    done: Receiver<Done>,
}

const CAP: usize = 120;

impl Default for GeneratorPreviews {
    fn default() -> Self {
        let (jobs, job_rx) = crossbeam_channel::unbounded::<Job>();
        let (done_tx, done) = crossbeam_channel::unbounded::<Done>();
        std::thread::Builder::new()
            .name("generator-previews".into())
            .spawn(move || {
                let mut titles = klipp_title::TitleRenderer::new();
                while let Ok(job) = job_rx.recv() {
                    let rgba = klipp_compose::synth::render_generator_with_slate(
                        &mut titles, &job.data, PREVIEW_W, PREVIEW_H, job.time as f64, PREVIEW_WINDOW,
                    );
                    if done_tx.send(Done { key: job.key, rgba }).is_err() {
                        break;
                    }
                }
            })
            .expect("generator preview thread");
        Self {
            textures: HashMap::new(),
            pending: HashSet::new(),
            wanted: HashSet::new(),
            wanted_prefixes: HashSet::new(),
            jobs,
            done,
        }
    }
}

impl GeneratorPreviews {
    pub fn key(name: &str, time: f32) -> String {
        format!("{name}\u{1}{:.3}", time)
    }

    pub fn hover_time(elapsed: f32) -> f32 {
        let t = elapsed % LOOP_SECS;
        (t / FRAME_STEP).floor() * FRAME_STEP
    }

    fn want(&mut self, key: &str) {
        self.wanted.insert(key.to_string());
        if let Some(i) = key.rfind('\u{1}') {
            self.wanted_prefixes.insert(key[..=i].to_string());
        }
    }

    pub fn request(&mut self, key: &str, time: f32, make: impl FnOnce() -> klipp_core::GeneratorData) {
        self.want(key);
        if !self.textures.contains_key(key) && !self.pending.contains(key) {
            self.pending.insert(key.to_string());
            let _ = self.jobs.send(Job { key: key.to_string(), data: make(), time });
        }
    }

    pub fn get_or_request(
        &mut self,
        key: &str,
        time: f32,
        make: impl FnOnce() -> klipp_core::GeneratorData,
    ) -> Option<&egui::TextureHandle> {
        self.request(key, time, make);
        self.textures.get(key)
    }

    /// While the requested moment renders: the cached moment nearest it.
    pub fn nearest_for(&self, name: &str, time: f32) -> Option<&egui::TextureHandle> {
        let prefix = format!("{name}\u{1}");
        self.textures
            .iter()
            .filter_map(|(k, t)| {
                let moment: f32 = k.strip_prefix(&prefix)?.parse().ok()?;
                Some(((moment - time).abs(), t))
            })
            .min_by(|(a, _), (b, _)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(_, t)| t)
    }

    pub fn poll(&mut self, ctx: &egui::Context, displaced: &mut Vec<egui::TextureHandle>) {
        let mut any = false;
        while let Ok(done) = self.done.try_recv() {
            self.pending.remove(&done.key);
            let pixels: Vec<egui::Color32> = done
                .rgba
                .chunks_exact(4)
                .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
                .collect();
            let image = egui::ColorImage { size: [PREVIEW_W as usize, PREVIEW_H as usize], pixels };
            let tex = crate::app::alloc_texture_logged(ctx, "generator_preview", image, egui::TextureOptions::LINEAR);
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
