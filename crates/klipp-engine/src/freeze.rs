//! Freezing a track (#3207): its frames drawn in the background into the
//! cache store, each under its key, so playback can take them while they
//! are current.
//!
//! What a frozen frame is and what keys it are the compositor's
//! (`klipp_compose::freeze`). Here: the identities a key reads (a media
//! item's file, a plug-in's version), and the job that freezes a range — a
//! frame at a time, waiting while the person works (the scheduler pauses a
//! background job then), stopping between frames when cancelled, skipping
//! frames already stored under their current key.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use klipp_compose::freeze::{live_reason, slot_key, FreezeEnv};
use klipp_compose::{FrameSource, Renderer};
use klipp_core::{Timecode, TrackId};
use klipp_jobs::{JobPriority, ResourceClass, ThreadJob};
use klipp_timeline::Sequence;

use crate::cache_store::{CacheStore, FrameAt, PutRefused};

/// A frozen track's namespace in the cache store.
pub fn namespace(track: TrackId) -> String {
    format!("freeze/{}", track)
}

/// A media item's identity for a freeze key: its file (path, length and
/// modification time, and the content fingerprint taken at import), its
/// proxy and its log format. A file changed on disk keys anew.
pub fn media_identity(item: &klipp_media::MediaItem) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    item.path.hash(&mut h);
    if let Ok(meta) = std::fs::metadata(&item.path) {
        meta.len().hash(&mut h);
        meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).hash(&mut h);
    }
    item.fingerprint.hash(&mut h);
    item.proxy_path.hash(&mut h);
    format!("{:?}", item.log_format).hash(&mut h);
    h.finish()
}

/// Every pool item's identity, by its id as a key reads it.
pub fn media_identities(pool: &klipp_media::MediaPool) -> HashMap<String, u64> {
    pool.all().map(|item| (item.id.to_string(), media_identity(item))).collect()
}

/// A plug-in's identity for a freeze key: a frei0r plug-in's file (path and
/// modification time) and reported version, from the scanned registry;
/// `None` when it isn't installed. An OFX plug-in by name only, for now.
pub fn plugin_identity(id: &str) -> Option<u64> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if id.starts_with("frei0r:") {
        let info = klipp_effects::frei0r_effect::lookup(id)?;
        info.path.hash(&mut h);
        std::fs::metadata(&info.path).ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).hash(&mut h);
        (info.major_version, info.minor_version).hash(&mut h);
    } else {
        id.hash(&mut h);
    }
    Some(h.finish())
}

/// What freezing a range reads: the sequence as the renderer draws it, the
/// project's sequences, the track, its frames, the size they're drawn at,
/// and the identities and environment its keys take.
pub struct FreezeRequest {
    /// The render copy (`flatten_sequence`).
    pub seq: Arc<Sequence>,
    pub sequences: Arc<Vec<Sequence>>,
    pub track: usize,
    pub track_id: TrackId,
    pub frames: Vec<Timecode>,
    pub size: (u32, u32),
    pub working: klipp_core::WorkingColorspace,
    pub environment: u64,
    pub media: Arc<HashMap<String, u64>>,
}

/// How one frame went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameOutcome {
    /// Drawn and stored.
    Stored,
    /// Already stored under its current key.
    Current,
    /// Stays live: the reason.
    Live(&'static str),
    /// Nothing on the track there.
    Empty,
    /// The cache is full of frozen frames.
    Full,
    /// Couldn't be drawn or stored.
    Failed(String),
}

/// What a freeze did, counted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FreezeTally {
    pub stored: usize,
    pub current: usize,
    pub live: usize,
    pub empty: usize,
    pub failed: usize,
    /// Drawn by the CPU where the GPU stack declined the layer.
    pub on_cpu: usize,
    pub full: bool,
    pub cancelled: bool,
}

impl FreezeRequest {
    fn key(&self, at: Timecode) -> Option<u64> {
        let media = |id: &str| self.media.get(id).copied();
        let env = FreezeEnv { sequences: &self.sequences, media: &media, plugin: &plugin_identity, environment: self.environment };
        slot_key(&self.seq, self.track, at, &env)
    }

    /// Freeze one frame (see [`FrameOutcome`]), drawn on the GPU stack where
    /// it takes the layer and on the CPU otherwise.
    pub fn freeze_frame(&self, renderer: &mut Renderer, store: &Mutex<CacheStore>, at: Timecode, src: &mut dyn FrameSource, on_cpu: &mut usize) -> FrameOutcome {
        if let Some(why) = live_reason(&self.seq, self.track, at) {
            return FrameOutcome::Live(why);
        }
        let Some(key) = self.key(at) else { return FrameOutcome::Empty };
        let ns = namespace(self.track_id);
        let place = FrameAt { tick: at.value, key, size: self.size };
        if store.lock().unwrap_or_else(|e| e.into_inner()).contains(&ns, place) {
            return FrameOutcome::Current;
        }
        let (w, h) = self.size;
        let layer = match renderer.render_track_layer_on_gpu(&self.seq, self.track, at, w, h, src) {
            Some(layer) => layer,
            None => {
                *on_cpu += 1;
                match renderer.render_track_layer(&self.seq, self.track, at, w, h, src) {
                    Some(layer) => layer,
                    None => return FrameOutcome::Empty,
                }
            }
        };
        match store.lock().unwrap_or_else(|e| e.into_inner()).put(&ns, place, &layer) {
            Ok(()) => FrameOutcome::Stored,
            Err(PutRefused::Full) => FrameOutcome::Full,
            Err(PutRefused::Io(why)) => FrameOutcome::Failed(why),
        }
    }
}

/// Freeze `req`'s frames in a background job: a frame at a time, waiting
/// between frames while the scheduler has the job paused (the person
/// playing, scrubbing or editing), stopping when cancelled with every
/// stored frame whole. `src` gives the track's pictures; `done` hears the
/// tally when it ends. The job's progress is the share of frames done.
pub fn spawn_freeze(req: FreezeRequest, store: Arc<Mutex<CacheStore>>, mut src: Box<dyn FrameSource + Send>, done: impl FnOnce(FreezeTally) + Send + 'static) -> Arc<ThreadJob> {
    let name = format!("Freeze track {}", req.track + 1);
    ThreadJob::spawn(name, JobPriority::Background, ResourceClass::GpuMixed, move |pause, progress| {
        let mut renderer = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        renderer.set_gpu_enabled(true);
        renderer.set_working_colorspace(req.working);
        let mut tally = FreezeTally::default();
        let n = req.frames.len().max(1);
        for (i, at) in req.frames.iter().enumerate() {
            pause.wait_while_paused();
            if pause.is_cancelled() {
                tally.cancelled = true;
                break;
            }
            match req.freeze_frame(&mut renderer, &store, *at, src.as_mut(), &mut tally.on_cpu) {
                FrameOutcome::Stored => tally.stored += 1,
                FrameOutcome::Current => tally.current += 1,
                FrameOutcome::Live(_) => tally.live += 1,
                FrameOutcome::Empty => tally.empty += 1,
                FrameOutcome::Failed(why) => {
                    tracing::warn!("freeze: track {} at {:.3} s: {why}", req.track + 1, at.to_secs_f64());
                    tally.failed += 1;
                }
                FrameOutcome::Full => {
                    tally.full = true;
                    break;
                }
            }
            progress.set((i + 1) as f32 / n as f32);
        }
        done(tally);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_compose::{RenderOptions, SourceFrame, SourcePixels};
    use klipp_core::RationalTime;
    use klipp_jobs::BackgroundJob;
    use klipp_timeline::{Clip, ClipKind, Track, TrackKind};

    const W: u32 = 32;
    const H: u32 = 18;

    fn tc(frame: i64) -> Timecode {
        RationalTime { value: frame, rate: 24 }
    }

    /// Each clip's own ramp.
    struct Pictures;

    impl FrameSource for Pictures {
        fn frame(&mut self, c: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
            let seed = (c.id.0.as_u128() % 89) as f32 / 89.0;
            let px: Vec<f32> = (0..h).flat_map(|y| (0..w).flat_map(move |x| [x as f32 / w as f32 * 0.8 + seed * 0.2, y as f32 / h as f32, (secs as f32 * 0.25).fract(), 1.0])).collect();
            Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(px)), width: w, height: h, pts: None })
        }
    }

    /// The render copy of a frozen track (V2, index 0) of two clips, a
    /// second each, the first blurred, over V1.
    fn project() -> Sequence {
        let media = |from: i64, to: i64| Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(from), tc(to)) };
        let top = klipp_timeline::test_clip::TestClip::of(media(0, 24)).effects(vec![klipp_effects::BuiltinEffect::GaussianBlur { radius: 1.5 }]).build();
        let mut v2 = Track::new("V2", TrackKind::Video);
        v2.clips = vec![top, media(24, 48)];
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![media(0, 48)];
        let mut seq = Sequence::new_default();
        seq.resolution = klipp_core::Resolution { width: W, height: H };
        seq.tracks = vec![v2, v1];
        klipp_timeline::flatten::flatten_sequence(&seq, std::slice::from_ref(&seq))
    }

    fn request_for(flat: &Sequence, frames: Vec<Timecode>) -> FreezeRequest {
        FreezeRequest { seq: Arc::new(flat.clone()), sequences: Arc::new(vec![]), track: 0, track_id: flat.tracks[0].id, frames, size: (W, H), working: klipp_core::WorkingColorspace::Rec709Linear, environment: 1, media: Arc::new(HashMap::new()) }
    }

    fn store(name: &str) -> (Arc<Mutex<CacheStore>>, std::path::PathBuf) {
        let d = std::env::temp_dir().join(format!("klipp-freeze-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        (Arc::new(Mutex::new(CacheStore::open(&d, 1 << 30))), d)
    }

    fn run(req: FreezeRequest, store: &Arc<Mutex<CacheStore>>) -> FreezeTally {
        let (tx, rx) = std::sync::mpsc::channel();
        let job = spawn_freeze(req, Arc::clone(store), Box::new(Pictures), move |t| tx.send(t).unwrap());
        job.join();
        rx.recv().unwrap()
    }

    /// A freeze stores every frame of the range under its key, and a second
    /// one finds them all current and draws nothing. A stored frame, laid
    /// in its track's place, draws the live frame.
    #[test]
    fn a_freeze_stores_each_frame_once_under_its_key() {
        let frames: Vec<Timecode> = (0..48).step_by(6).map(tc).collect();
        let (s, d) = store("once");
        let flat = project();
        let first = run(request_for(&flat, frames.clone()), &s);
        assert_eq!((first.stored, first.failed, first.cancelled), (frames.len(), 0, false), "{first:?}");
        let second = run(request_for(&flat, frames.clone()), &s);
        assert_eq!((second.stored, second.current), (0, frames.len()), "{second:?}");
        // Laid back from the store: the frame the live track draws.
        let at = tc(30);
        let key = request_for(&flat, vec![]).key(at).unwrap();
        let layer = s.lock().unwrap().get(&namespace(flat.tracks[0].id), FrameAt { tick: at.value, key, size: (W, H) }).expect("stored");
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(false);
        let live = r.render(&flat, at, W, H, &mut Pictures, RenderOptions::default());
        r.set_frozen_layers(HashMap::from([(0, Arc::new(layer))]));
        let laid = r.render(&flat, at, W, H, &mut Pictures, RenderOptions::default());
        let code = |c: f32| (c.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0).round() as i32;
        let worst = live.iter().zip(&laid).map(|(a, b)| (code(*a) - code(*b)).abs()).max().unwrap();
        assert!(worst <= 1, "laid from the store {worst} levels from live");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Cancelled, a freeze stops between frames: fewer frames stored, each
    /// whole, nothing half-written left.
    #[test]
    fn a_cancelled_freeze_stops_between_frames_and_keeps_nothing_half_made() {
        let frames: Vec<Timecode> = (0..48).map(tc).collect();
        let (s, d) = store("cancel");
        let flat = project();
        let (tx, rx) = std::sync::mpsc::channel();
        let job = spawn_freeze(request_for(&flat, frames.clone()), Arc::clone(&s), Box::new(Pictures), move |t| tx.send(t).unwrap());
        while job.progress() <= 0.0 && !job.is_done() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        job.cancel();
        job.join();
        let tally = rx.recv().unwrap();
        assert!(tally.cancelled && tally.stored < frames.len(), "{tally:?}");
        let ns = namespace(flat.tracks[0].id);
        let kept = s.lock().unwrap().frames(&ns);
        assert_eq!(kept.len(), tally.stored);
        let stray: Vec<_> = walkdir::WalkDir::new(&d).into_iter().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).collect();
        assert!(stray.is_empty(), "nothing half-written: {stray:?}");
        for at in kept {
            assert!(s.lock().unwrap().get(&ns, at).is_some(), "each stored frame whole");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Paused (the person working), a freeze draws nothing until resumed:
    /// the frame in hand when the pause comes is finished, then it waits.
    #[test]
    fn a_paused_freeze_waits_until_resumed() {
        let frames: Vec<Timecode> = (0..48).map(tc).collect();
        let (s, d) = store("pause");
        let flat = project();
        let (tx, rx) = std::sync::mpsc::channel();
        let job = spawn_freeze(request_for(&flat, frames.clone()), Arc::clone(&s), Box::new(Pictures), move |t| tx.send(t).unwrap());
        while job.progress() <= 0.0 {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        job.pause();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let held = job.progress();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(job.progress(), held, "no frame drawn while paused");
        assert!(held < 1.0, "not done while paused");
        job.resume();
        job.join();
        assert_eq!(rx.recv().unwrap().stored, frames.len());
        let _ = std::fs::remove_dir_all(&d);
    }
}
