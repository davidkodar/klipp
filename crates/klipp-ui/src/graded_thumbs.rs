//! Graded thumbnails for the Colour page's clip strip (#899): each picture
//! clip's frame at its middle with its grade, so a shot's state reads at a
//! glance. Drawn off the UI thread by export's renderer
//! (`klipp_render::still::clip_still`) and redrawn when the clip's picture
//! changes; until then the last one stays up.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::mpsc;

use klipp_core::ClipId;
use klipp_timeline::{Clip, ClipKind, Sequence, TrackKind};

/// How wide a graded thumbnail is drawn.
pub const THUMB_WIDTH: u32 = 192;

/// How often the clips' pictures are looked at again for a change, in
/// seconds: a slider drag redraws once it settles, not every frame.
const LOOK_EVERY: f64 = 0.4;

struct Job {
    clip: Clip,
    item: klipp_media::MediaItem,
    key: u64,
    working: klipp_core::WorkingColorspace,
}

struct Done {
    clip: ClipId,
    key: u64,
    still: Result<klipp_render::still::Still, String>,
}

/// The strip's graded thumbnails, by clip.
#[derive(Default)]
pub struct GradedThumbs {
    shown: HashMap<ClipId, (u64, egui::TextureHandle)>,
    asked: HashSet<(ClipId, u64)>,
    /// Clips whose picture couldn't be drawn, by the key that failed.
    failed: HashMap<ClipId, u64>,
    worker: Option<(mpsc::Sender<Job>, mpsc::Receiver<Done>)>,
    looked_at: Option<f64>,
}

/// What a clip's thumbnail shows: its picture (grade, effects, placement),
/// its source and its media, and the working space; a change to any is a
/// new thumbnail.
pub fn key_of(clip: &Clip, working: klipp_core::WorkingColorspace) -> Option<u64> {
    let ClipKind::Media { media_id } = &clip.kind else { return None };
    let picture = clip.picture_v2().ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(&picture.picture).ok()?.hash(&mut h);
    media_id.0.hash(&mut h);
    (clip.source_in.value, clip.source_in.rate, clip.source_out.value, clip.speed.to_bits()).hash(&mut h);
    (working as u8).hash(&mut h);
    Some(h.finish())
}

/// The picture clips the strip shows, as (clip, key).
fn wanted(seq: &Sequence, working: klipp_core::WorkingColorspace) -> Vec<(&Clip, u64)> {
    seq.tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .flat_map(|t| t.clips.iter())
        .filter_map(|c| key_of(c, working).map(|k| (c, k)))
        .collect()
}

impl GradedThumbs {
    /// The thumbnail to draw for `clip`: its latest, while a newer one is
    /// drawn.
    pub fn texture(&self, clip: ClipId) -> Option<&egui::TextureHandle> {
        self.shown.get(&clip).map(|(_, t)| t)
    }

    /// Whether `clip`'s thumbnail shows its picture as it is now.
    pub fn is_current(&self, clip: &Clip, working: klipp_core::WorkingColorspace) -> bool {
        key_of(clip, working).is_some_and(|k| self.shown.get(&clip.id).is_some_and(|(s, _)| *s == k))
    }

    /// Once a frame while the Colour page shows: take the thumbnails drawn
    /// since, and every `LOOK_EVERY` ask for those whose picture changed.
    /// `now` is the UI's clock.
    pub fn update(&mut self, ctx: &egui::Context, now: f64, seq: &Sequence, pool: &klipp_media::MediaPool, working: klipp_core::WorkingColorspace) {
        self.take(ctx);
        if self.looked_at.is_some_and(|t| now - t < LOOK_EVERY) {
            return;
        }
        self.looked_at = Some(now);
        for (clip, key) in wanted(seq, working) {
            let current = self.shown.get(&clip.id).is_some_and(|(k, _)| *k == key);
            if current || self.asked.contains(&(clip.id, key)) || self.failed.get(&clip.id) == Some(&key) {
                continue;
            }
            let ClipKind::Media { media_id } = &clip.kind else { continue };
            let Some(item) = pool.get(media_id).filter(|m| !m.offline) else { continue };
            self.asked.insert((clip.id, key));
            let job = Job { clip: clip.clone(), item: item.clone(), key, working };
            if self.sender(ctx).send(job).is_err() {
                self.worker = None;
            }
        }
    }

    /// Wait for every asked thumbnail, for a test.
    #[cfg(test)]
    pub fn settle(&mut self, ctx: &egui::Context) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !self.asked.is_empty() && std::time::Instant::now() < deadline {
            self.take(ctx);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn take(&mut self, ctx: &egui::Context) {
        let Some((_, rx)) = &self.worker else { return };
        let done: Vec<Done> = rx.try_iter().collect();
        for d in done {
            self.asked.remove(&(d.clip, d.key));
            match d.still {
                Ok(still) => {
                    let image = egui::ColorImage::from_rgba_unmultiplied([still.width as usize, still.height as usize], &still.rgba);
                    let texture = ctx.load_texture(format!("graded_thumb_{}", d.clip.0), image, egui::TextureOptions::LINEAR);
                    self.shown.insert(d.clip, (d.key, texture));
                    self.failed.remove(&d.clip);
                }
                Err(why) => {
                    tracing::debug!(clip = %d.clip.0, %why, "no graded thumbnail");
                    self.failed.insert(d.clip, d.key);
                }
            }
        }
    }

    fn sender(&mut self, ctx: &egui::Context) -> &mpsc::Sender<Job> {
        let ctx = ctx.clone();
        &self.worker.get_or_insert_with(|| {
            let (job_tx, job_rx) = mpsc::channel::<Job>();
            let (done_tx, done_rx) = mpsc::channel::<Done>();
            let spawned = std::thread::Builder::new().name("graded-thumbs".into()).spawn(move || {
                for job in job_rx {
                    let mut pool = klipp_media::MediaPool::new();
                    pool.add(job.item);
                    let at = job.clip.duration().to_secs_f64() / 2.0;
                    let still = klipp_render::still::clip_still(&job.clip, &pool, at, THUMB_WIDTH, job.working);
                    if done_tx.send(Done { clip: job.clip.id, key: job.key, still }).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            });
            if let Err(e) = spawned {
                tracing::warn!(%e, "could not start the graded thumbnail worker");
            }
            (job_tx, done_rx)
        })
        .0
    }
}
