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

/// How wide a clip's frame for a wipe is drawn (#899): enough for the
/// viewer at its usual size.
pub const WIPE_WIDTH: u32 = 1280;

/// How often the clips' pictures are looked at again for a change, in
/// seconds: a slider drag redraws once it settles, not every frame.
const LOOK_EVERY: f64 = 0.4;

struct Job {
    width: u32,
    clip: Clip,
    item: klipp_media::MediaItem,
    key: u64,
    working: klipp_core::WorkingColorspace,
}

struct Done {
    width: u32,
    clip: ClipId,
    key: u64,
    still: Result<klipp_render::still::Still, String>,
}

/// The strip's graded thumbnails, by clip.
#[derive(Default)]
pub struct GradedThumbs {
    /// By clip and width: the thumbnail, or a wipe's frame.
    shown: HashMap<(ClipId, u32), (u64, egui::TextureHandle)>,
    asked: HashSet<(ClipId, u32, u64)>,
    /// Clips whose picture couldn't be drawn, by the key that failed.
    failed: HashMap<(ClipId, u32), u64>,
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

/// The picture clips in timeline order: each video track's media clips,
/// by where they start, the upper track first at a tie.
fn picture_clips(seq: &Sequence) -> Vec<&Clip> {
    let mut clips: Vec<&Clip> = seq.tracks.iter().filter(|t| t.kind == TrackKind::Video).flat_map(|t| t.clips.iter()).filter(|c| c.enabled && matches!(c.kind, ClipKind::Media { .. })).collect();
    clips.sort_by_key(|c| c.timeline_in.value as i128 * 1_000_000 / c.timeline_in.rate.max(1) as i128);
    clips
}

/// The picture clip before (`forward` false) or after `clip` in timeline
/// order: what a wipe against the previous or next shot compares with.
pub fn neighbour(seq: &Sequence, clip: ClipId, forward: bool) -> Option<ClipId> {
    let clips = picture_clips(seq);
    let at = clips.iter().position(|c| c.id == clip)?;
    let to = if forward { at.checked_add(1)? } else { at.checked_sub(1)? };
    clips.get(to).map(|c| c.id)
}

/// The picture clip on top at `at`: what is graded when none is selected.
pub fn clip_at(seq: &Sequence, at: klipp_core::Timecode) -> Option<ClipId> {
    seq.tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .flat_map(|t| t.clips.iter())
        .find(|c| c.enabled && matches!(c.kind, ClipKind::Media { .. }) && c.timeline_in <= at && at < c.timeline_out)
        .map(|c| c.id)
}

/// The clip the viewer shows graded, what a wipe compares with (#899):
/// the picture clip on top at `playhead`, else `selected`.
pub fn viewed_clip(seq: &Sequence, selected: Option<ClipId>, playhead: klipp_core::Timecode) -> Option<ClipId> {
    clip_at(seq, playhead).or_else(|| selected.filter(|c| seq.all_clips().any(|x| x.id == *c)))
}

/// The clip a wipe `target` names against the clip the viewer shows
/// ([`viewed_clip`]) (#899): the shot before or after it, or the clip
/// named, never the shown clip itself. `None` when there is none.
pub fn wipe_target(seq: &Sequence, selected: Option<ClipId>, playhead: klipp_core::Timecode, target: klipp_core::WipeClip) -> Option<ClipId> {
    let graded = viewed_clip(seq, selected, playhead);
    match target {
        klipp_core::WipeClip::Clip(id) => Some(id).filter(|id| Some(*id) != graded && seq.all_clips().any(|c| c.id == *id)),
        klipp_core::WipeClip::Previous => graded.and_then(|g| neighbour(seq, g, false)),
        klipp_core::WipeClip::Next => graded.and_then(|g| neighbour(seq, g, true)),
    }
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
        self.shown.get(&(clip, THUMB_WIDTH)).map(|(_, t)| t)
    }

    /// `clip`'s graded frame for a wipe (#899): its latest, while a newer
    /// one is drawn.
    pub fn wipe_frame(&self, clip: ClipId) -> Option<&egui::TextureHandle> {
        self.shown.get(&(clip, WIPE_WIDTH)).map(|(_, t)| t)
    }

    /// Whether `clip`'s thumbnail shows its picture as it is now.
    pub fn is_current(&self, clip: &Clip, working: klipp_core::WorkingColorspace) -> bool {
        key_of(clip, working).is_some_and(|k| self.shown.get(&(clip.id, THUMB_WIDTH)).is_some_and(|(s, _)| *s == k))
    }

    /// Once a frame while the Colour page shows: take the thumbnails drawn
    /// since, and every `LOOK_EVERY` ask for those whose picture changed.
    /// `now` is the UI's clock.
    ///
    /// `wipe` is the clip the viewer wipes against, whose frame is drawn
    /// at the viewer's size too.
    pub fn update(&mut self, ctx: &egui::Context, now: f64, seq: &Sequence, pool: &klipp_media::MediaPool, working: klipp_core::WorkingColorspace, wipe: Option<ClipId>) {
        self.take(ctx);
        if self.looked_at.is_some_and(|t| now - t < LOOK_EVERY) {
            return;
        }
        self.looked_at = Some(now);
        let wanted = wanted(seq, working);
        if let Some((clip, key)) = wipe.and_then(|w| wanted.iter().find(|(c, _)| c.id == w)) {
            self.ask(ctx, clip, *key, WIPE_WIDTH, pool, working);
        }
        for (clip, key) in &wanted {
            self.ask(ctx, clip, *key, THUMB_WIDTH, pool, working);
        }
    }

    fn ask(&mut self, ctx: &egui::Context, clip: &Clip, key: u64, width: u32, pool: &klipp_media::MediaPool, working: klipp_core::WorkingColorspace) {
        let current = self.shown.get(&(clip.id, width)).is_some_and(|(k, _)| *k == key);
        if current || self.asked.contains(&(clip.id, width, key)) || self.failed.get(&(clip.id, width)) == Some(&key) {
            return;
        }
        let ClipKind::Media { media_id } = &clip.kind else { return };
        let Some(item) = pool.get(media_id).filter(|m| !m.offline) else { return };
        self.asked.insert((clip.id, width, key));
        let job = Job { width, clip: clip.clone(), item: item.clone(), key, working };
        if self.sender(ctx).send(job).is_err() {
            self.worker = None;
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
            self.asked.remove(&(d.clip, d.width, d.key));
            match d.still {
                Ok(still) => {
                    // A still of the wrong size is a failed render, kept
                    // as failed like any other (#1913).
                    let Some(image) = crate::ui_util::checked_image([still.width as usize, still.height as usize], &still.rgba, "graded thumbnail") else {
                        self.failed.insert((d.clip, d.width), d.key);
                        continue;
                    };
                    let texture = ctx.load_texture(format!("graded_thumb_{}_{}", d.clip.0, d.width), image, egui::TextureOptions::LINEAR);
                    self.shown.insert((d.clip, d.width), (d.key, texture));
                    self.failed.remove(&(d.clip, d.width));
                }
                Err(why) => {
                    tracing::debug!(clip = %d.clip.0, %why, "no graded thumbnail");
                    self.failed.insert((d.clip, d.width), d.key);
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
                    let still = klipp_render::still::clip_still(&job.clip, &pool, at, job.width, job.working);
                    if done_tx.send(Done { width: job.width, clip: job.clip.id, key: job.key, still }).is_err() {
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

#[cfg(test)]
mod tests {

    /// #1913: a still whose bytes are not its size (the crash's: an empty
    /// one for 192x108) is a failed thumbnail, not a panic, and is not
    /// asked for again at that key.
    #[test]
    fn a_still_of_the_wrong_size_is_a_failed_thumbnail() {
        let ctx = egui::Context::default();
        let mut thumbs = GradedThumbs::default();
        let (job_tx, _jobs) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        thumbs.worker = Some((job_tx, done_rx));
        let clip = ClipId::new();
        thumbs.asked.insert((clip, 192, 7));
        done_tx.send(Done { width: 192, clip, key: 7, still: Ok(klipp_render::still::Still { rgba: Vec::new(), width: 192, height: 108 }) }).unwrap();
        thumbs.take(&ctx);
        assert!(thumbs.shown.is_empty(), "nothing shown");
        assert_eq!(thumbs.failed.get(&(clip, 192)), Some(&7), "failed at its key");
        assert!(thumbs.asked.is_empty());
    }
    use super::*;
    use klipp_core::{MediaId, RationalTime, WipeClip};

    /// Three shots across two tracks: the wipe's previous and next follow
    /// timeline order, whichever track; the graded clip is the selected
    /// one, else the one on top at the playhead; a clip never wipes
    /// against itself, and past either end there is none.
    #[test]
    fn a_wipe_names_the_shot_before_or_after() {
        let t = |s: i64| RationalTime { value: 24 * s, rate: 24 };
        let mut seq = Sequence::new_default();
        let media = MediaId::new();
        let video: Vec<usize> = seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Video).map(|(i, _)| i).collect();
        let (a, b, c) = (Clip::media(media, t(0), t(2), t(0), t(2)), Clip::media(media, t(2), t(4), t(0), t(2)), Clip::media(media, t(4), t(6), t(0), t(2)));
        let (ia, ib, ic) = (a.id, b.id, c.id);
        seq.tracks[video[0]].clips = vec![a, c];
        if video.len() > 1 {
            seq.tracks[video[1]].clips = vec![b];
        } else {
            seq.tracks[video[0]].clips.insert(1, b);
        }
        let at = |s| t(s);
        assert_eq!(wipe_target(&seq, None, at(3), WipeClip::Previous), Some(ia));
        assert_eq!(wipe_target(&seq, None, at(3), WipeClip::Next), Some(ic));
        assert_eq!(wipe_target(&seq, Some(ia), at(5), WipeClip::Previous), Some(ib), "the clip the viewer shows, at the playhead, before the selection");
        assert_eq!(wipe_target(&seq, Some(ib), at(60), WipeClip::Next), Some(ic), "past every clip: the selected one");
        assert_eq!(wipe_target(&seq, None, at(0), WipeClip::Previous), None, "nothing before the first");
        assert_eq!(wipe_target(&seq, None, at(5), WipeClip::Next), None, "nothing after the last");
        assert_eq!(wipe_target(&seq, None, at(0), WipeClip::Clip(ic)), Some(ic));
        assert_eq!(wipe_target(&seq, None, at(0), WipeClip::Clip(ia)), None, "not against itself");
    }
}
