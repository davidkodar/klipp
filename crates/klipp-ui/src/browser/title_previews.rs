//! Title cards rendered by the real title engine, on a worker thread,
//! so a card shows exactly what the template will look like: the
//! bundled face, the shadows, the motion at the resting pose, and,
//! while hovered, the motion playing.

use crossbeam_channel::{Receiver, Sender};
use std::collections::{HashMap, HashSet};

/// Preview frame size: sharp on a 2× display at the card's width, cheap
/// to render (glyphs are a third of their 1080p size).
pub const PREVIEW_W: u32 = 640;
pub const PREVIEW_H: u32 = 360;
/// The clip length the preview pretends to have, and the moment in it
/// that a card shows when it is not being hovered: past the entrance,
/// before the exit.
pub const PREVIEW_WINDOW: f32 = 5.0;
pub const REST_TIME: f32 = 2.0;
/// Hover animation: the loop and the frame step.
pub const LOOP_SECS: f32 = 4.0;
pub const FRAME_STEP: f32 = 1.0 / 15.0;

struct Job {
    key: String,
    td: klipp_timeline::TitleData,
    time: f32,
}

struct Done {
    key: String,
    rgba: Vec<u8>,
}

pub struct TitlePreviews {
    textures: HashMap<String, egui::TextureHandle>,
    pending: HashSet<String>,
    /// Keys touched this frame; anything else is dropped once the map
    /// grows past `CAP`, so hover frames never pile up.
    wanted: HashSet<String>,
    jobs: Sender<Job>,
    done: Receiver<Done>,
}

const CAP: usize = 96;

impl Default for TitlePreviews {
    fn default() -> Self {
        let (jobs, job_rx) = crossbeam_channel::unbounded::<Job>();
        let (done_tx, done) = crossbeam_channel::unbounded::<Done>();
        std::thread::Builder::new()
            .name("title-previews".into())
            .spawn(move || {
                let mut renderer = klipp_title::TitleRenderer::new();
                while let Ok(job) = job_rx.recv() {
                    let rgba = renderer.render_with_window(&job.td, PREVIEW_W, PREVIEW_H, None, job.time, Some(PREVIEW_WINDOW));
                    if done_tx.send(Done { key: job.key, rgba }).is_err() {
                        break;
                    }
                }
            })
            .expect("title preview thread");
        Self { textures: HashMap::new(), pending: HashSet::new(), wanted: HashSet::new(), jobs, done }
    }
}

impl TitlePreviews {
    /// The cache key of one template at one text and one moment.
    pub fn key(template: &str, text: &str, time: f32) -> String {
        format!("{template}\u{1}{text}\u{1}{:.3}", time)
    }

    /// The hover moment for a card hovered `elapsed` seconds ago,
    /// quantised to the frame step so frames are shared and cached.
    pub fn hover_time(elapsed: f32) -> f32 {
        let t = elapsed % LOOP_SECS;
        (t / FRAME_STEP).floor() * FRAME_STEP
    }

    /// The texture for `key`, requesting a render when there is none.
    /// `make` builds the title to render only when it is needed.
    pub fn get_or_request(
        &mut self,
        key: &str,
        time: f32,
        make: impl FnOnce() -> klipp_timeline::TitleData,
    ) -> Option<&egui::TextureHandle> {
        self.wanted.insert(key.to_string());
        if !self.textures.contains_key(key) && !self.pending.contains(key) {
            self.pending.insert(key.to_string());
            let _ = self.jobs.send(Job { key: key.to_string(), td: make(), time });
        }
        self.textures.get(key)
    }

    /// The most recent frame for a template while its hover frames are
    /// still arriving: the latest cached moment of the same template
    /// and text.
    pub fn latest_for(&self, template: &str, text: &str) -> Option<&egui::TextureHandle> {
        let prefix = format!("{template}\u{1}{text}\u{1}");
        self.textures
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .max_by(|(a, _), (b, _)| a.cmp(b))
            .map(|(_, t)| t)
    }

    /// Upload finished renders. Displaced handles go to `displaced`
    /// for the caller's deferred drop.
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
            let tex = crate::app::alloc_texture_logged(ctx, "title_preview", image, egui::TextureOptions::LINEAR);
            if let Some(old) = self.textures.insert(done.key, tex) {
                displaced.push(old);
            }
            any = true;
        }
        if self.textures.len() > CAP {
            let wanted = std::mem::take(&mut self.wanted);
            let keys: Vec<String> = self.textures.keys().filter(|k| !wanted.contains(*k)).cloned().collect();
            for k in keys {
                if let Some(old) = self.textures.remove(&k) {
                    displaced.push(old);
                }
            }
        } else {
            self.wanted.clear();
        }
        if any {
            ctx.request_repaint();
        }
    }

    /// Every cached handle, for shutdown.
    pub fn drain(&mut self) -> Vec<egui::TextureHandle> {
        self.textures.drain().map(|(_, t)| t).collect()
    }
}
