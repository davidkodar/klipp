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

/// How frames are drawn past the working space: the grade's encoding,
/// and whether LUTs are looked up tetrahedrally or trilinearly (the
/// viewer's choice; export always tetrahedral). A renderer drawing frames
/// to keep draws with the viewer's, and the key takes them (#3265): a
/// frame kept under the other encoding or lookup is another picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DrawSettings {
    pub encoding: klipp_core::GradeEncoding,
    pub lut_tetrahedral: bool,
}

impl DrawSettings {
    /// The project's grade encoding, with the viewer's LUT lookup.
    pub fn current(lut_tetrahedral: bool) -> Self {
        Self { encoding: klipp_core::grade_encoding(), lut_tetrahedral }
    }

    /// `renderer` set to draw so.
    pub fn apply(&self, renderer: &mut klipp_compose::Renderer) {
        renderer.set_grade_encoding(self.encoding);
        renderer.set_lut_tetrahedral(self.lut_tetrahedral);
    }
}

/// The render environment a frozen or kept frame's key takes (#3207):
/// the working space, whether proxies are drawn, and how the frame is
/// drawn past that ([`DrawSettings`]). The freeze, drawing ahead and
/// playback all key by it, so a frame drawn in one is never shown in
/// another.
pub fn environment(working: klipp_core::WorkingColorspace, use_proxies: bool, draw: DrawSettings) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (format!("{working:?}"), use_proxies, draw).hash(&mut h);
    h.finish()
}

/// The size playback draws a sequence of `sequence` size at while playing,
/// with no other picture setting the frame: the size a track is frozen at.
/// A frame the viewer draws at another size (a shot of another size below
/// sets it) lays the track live.
pub fn playback_size(sequence: (u32, u32)) -> (u32, u32) {
    let full = crate::engine::view::playback_canvas(None, sequence);
    crate::engine::view::reduced_for_quality(full, crate::video_decoder::playback_quality_divisor())
}

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
        return Some(frei0r_identity(&info.path, (info.major_version, info.minor_version)));
    }
    id.hash(&mut h);
    Some(h.finish())
}

/// A frei0r plug-in's identity: its file (path and modification time)
/// and the version it reports.
fn frei0r_identity(path: &std::path::Path, version: (i32, i32)) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).hash(&mut h);
    version.hash(&mut h);
    h.finish()
}

/// Frozen frames read ahead of playback (#3207): a thread reads and
/// decompresses the frames the engine asks for next, so the frame being
/// drawn finds its frozen layers read. Holds the last few it read.
pub struct FrozenReader {
    ask: std::sync::mpsc::Sender<(std::path::PathBuf, String, FrameAt)>,
    ready: Arc<Mutex<std::collections::VecDeque<((String, FrameAt), Arc<klipp_compose::freeze::FrozenLayer>)>>>,
}

/// Frames a [`FrozenReader`] keeps: a few frames ahead of each frozen track.
const READ_AHEAD_KEPT: usize = 24;

impl FrozenReader {
    pub fn new() -> Self {
        let (ask, asked) = std::sync::mpsc::channel::<(std::path::PathBuf, String, FrameAt)>();
        let ready: Arc<Mutex<std::collections::VecDeque<((String, FrameAt), Arc<klipp_compose::freeze::FrozenLayer>)>>> = Arc::default();
        let held = Arc::clone(&ready);
        let _ = std::thread::Builder::new().name("klipp-frozen-read".into()).spawn(move || {
            while let Ok((folder, ns, at)) = asked.recv() {
                let have = held.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|((n, a), _)| *n == ns && *a == at);
                if have {
                    continue;
                }
                if let Some(packed) = crate::cache_store::read_stored_packed(&folder, &ns, at) {
                    let mut held = held.lock().unwrap_or_else(|e| e.into_inner());
                    held.push_back(((ns, at), Arc::new(klipp_compose::freeze::FrozenLayer::from_packed(packed))));
                    while held.len() > READ_AHEAD_KEPT {
                        held.pop_front();
                    }
                }
            }
        });
        FrozenReader { ask, ready }
    }

    /// Read `namespace`'s frame `at` ahead.
    pub fn ask(&self, folder: &std::path::Path, namespace: &str, at: FrameAt) {
        let _ = self.ask.send((folder.to_path_buf(), namespace.to_owned(), at));
    }

    /// `namespace`'s frame `at`: read ahead, or read now.
    pub fn take(&self, folder: &std::path::Path, namespace: &str, at: FrameAt) -> Option<Arc<klipp_compose::freeze::FrozenLayer>> {
        let found = self.ready.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|((n, a), _)| n == namespace && *a == at).map(|(_, l)| Arc::clone(l));
        found.or_else(|| crate::cache_store::read_stored_packed(folder, namespace, at).map(|p| Arc::new(klipp_compose::freeze::FrozenLayer::from_packed(p))))
    }
}

impl Default for FrozenReader {
    fn default() -> Self {
        Self::new()
    }
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
    /// How its frames are drawn, as `environment` takes it.
    pub draw: DrawSettings,
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
    /// The frames gone through, in the request's order: those after are
    /// still to draw (a full cache or a cancel stopped it).
    pub reached: usize,
    /// The frames that play live, and those that couldn't be drawn.
    pub live_frames: Vec<Timecode>,
    pub failed_frames: Vec<Timecode>,
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
            Err(why @ (PutRefused::Io(_) | PutRefused::Namespace(_))) => FrameOutcome::Failed(why.to_string()),
        }
    }
}

/// The frames of track `track` that have a clip on them, at the
/// sequence's rate as playback times them, those from `from` on first and
/// then those before it: what is played next is frozen first.
pub fn track_frames(seq: &Sequence, track: usize, from: Timecode) -> Vec<Timecode> {
    let Some(t) = seq.tracks.get(track) else { return Vec::new() };
    let (num, den) = (i128::from(seq.fps.num.max(1)), i128::from(seq.fps.den.max(1)));
    // Frame f is at f·den/num s: the first frame at or after a time, and the
    // first at or after its end.
    let first_at = |at: Timecode| -> i64 {
        let (v, r) = (i128::from(at.value), i128::from(at.rate.max(1)));
        let (n, d) = (v * num, r * den);
        (n.div_euclid(d) + i128::from(n.rem_euclid(d) != 0)) as i64
    };
    let mut frames = std::collections::BTreeSet::new();
    for c in &t.clips {
        frames.extend(first_at(c.timeline_in).max(0)..first_at(c.timeline_out));
    }
    let start = first_at(from);
    let (before, after): (Vec<i64>, Vec<i64>) = frames.into_iter().partition(|f| *f < start);
    let tc = |f: i64| Timecode { value: f * i64::from(seq.fps.den), rate: seq.fps.num };
    after.into_iter().chain(before).map(tc).collect()
}

/// `frames` (at the sequence's rate, as [`track_frames`] gives them) as
/// spans of frames one after another: each from its first frame's start
/// to its last frame's end.
pub fn spans(frames: &[Timecode], fps: klipp_core::Fps) -> Vec<(Timecode, Timecode)> {
    let step = i64::from(fps.den.max(1));
    let mut v: Vec<i64> = frames.iter().map(|t| t.value).collect();
    v.sort_unstable();
    v.dedup();
    let rate = frames.first().map_or(fps.num, |t| t.rate);
    let mut out: Vec<(Timecode, Timecode)> = Vec::new();
    for f in v {
        match out.last_mut() {
            Some((_, end)) if end.value == f => end.value = f + step,
            _ => out.push((Timecode { value: f, rate }, Timecode { value: f + step, rate })),
        }
    }
    out
}

/// How a frozen track stands (#3207), as its freeze job last told it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FreezeStatus {
    /// The frames the track has something on.
    pub frames: usize,
    /// The share of them gone through, while the job runs.
    pub progress: Option<f32>,
    /// What the last finished job found.
    pub tally: Option<FreezeTally>,
    /// Why nothing is frozen: no saved project to keep the frames in.
    pub unsaved: bool,
    /// Where it plays live though frozen: frames that read other tracks.
    pub live: Vec<(Timecode, Timecode)>,
    /// Where its frames aren't drawn yet, or couldn't be: played live.
    pub stale: Vec<(Timecode, Timecode)>,
    /// A sound track: frozen as one stem, `frames` 1 once it is drawn.
    pub sound: bool,
    /// Why it plays live though frozen, when nothing of it can be frozen
    /// (a sound track with a plug-in from outside Klipp).
    pub live_reason: Option<&'static str>,
}

/// How a frozen track stands, in one word: what its header shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FreezeState {
    /// Its frames being drawn: the share done.
    Drawing(f32),
    /// Every frame it can play frozen is drawn.
    Current,
    /// Frames waiting, not drawn or not kept: they play live.
    Stale,
}

impl FreezeStatus {
    pub fn state(&self) -> FreezeState {
        if let Some(p) = self.progress {
            return FreezeState::Drawing(p);
        }
        match &self.tally {
            Some(_) if self.live_reason.is_some() => FreezeState::Stale,
            Some(t) if !self.unsaved && !t.full && t.failed == 0 && t.stored + t.current + t.live + t.empty >= self.frames => FreezeState::Current,
            _ => FreezeState::Stale,
        }
    }

    /// The status in a few words, for the timeline text and the header.
    pub fn describe(&self) -> String {
        if self.unsaved {
            return "frozen, but played live until the project is saved: its frames are kept in the project".into();
        }
        if let Some(why) = self.live_reason {
            return format!("frozen, but played live: {why}");
        }
        if self.sound {
            return match (self.progress, &self.tally) {
                (Some(p), _) => format!("frozen, drawing its sound: {:.0} %", p * 100.0),
                (None, Some(t)) if t.stored + t.current > 0 => "frozen, its sound current".into(),
                (None, Some(_)) => "frozen, but its sound couldn't be drawn: played live".into(),
                (None, None) => "frozen, its sound waiting to be drawn".into(),
            };
        }
        let mut out = match (self.progress, &self.tally) {
            (Some(p), _) => format!("frozen, drawing its frames: {:.0} % of {}", p * 100.0, self.frames),
            (None, Some(t)) => format!("frozen, {} of {} frames current", t.stored + t.current, self.frames),
            (None, None) => format!("frozen, {} frames waiting to be drawn", self.frames),
        };
        if let Some(t) = &self.tally {
            if t.live > 0 {
                out.push_str(&format!(", {} played live (they read other tracks)", t.live));
            }
            if t.failed > 0 {
                out.push_str(&format!(", {} couldn't be drawn", t.failed));
            }
            if t.full {
                out.push_str(", the cache is full");
            }
        }
        out
    }
}

static STATUS: std::sync::LazyLock<Mutex<HashMap<TrackId, FreezeStatus>>> = std::sync::LazyLock::new(Default::default);

/// Set every frozen track's status at once: a track left out isn't frozen.
pub fn set_statuses(statuses: HashMap<TrackId, FreezeStatus>) {
    *STATUS.lock().unwrap_or_else(|e| e.into_inner()) = statuses;
}

/// Whether a frozen track's frames are being drawn now.
pub fn any_drawing() -> bool {
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).values().any(|s| s.progress.is_some())
}

/// A frozen track's status; `None` for a track that isn't frozen.
pub fn status(track: TrackId) -> Option<FreezeStatus> {
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).get(&track).cloned()
}

/// Freeze `req`'s frames in a background job: a frame at a time, waiting
/// between frames while the scheduler has the job paused (the person
/// playing, scrubbing or editing), stopping when cancelled with every
/// stored frame whole. `source` makes, on the job's thread, what gives the
/// track's pictures (a decoder's state stays on the thread that made it);
/// `done` hears the tally when it ends. The job's progress is the share of
/// frames done.
pub fn spawn_freeze<S: FrameSource + 'static>(req: FreezeRequest, store: Arc<Mutex<CacheStore>>, source: impl FnOnce() -> S + Send + 'static, done: impl FnOnce(FreezeTally) + Send + 'static) -> Arc<ThreadJob> {
    let name = format!("Freeze track {}", req.track + 1);
    ThreadJob::spawn(name, JobPriority::Background, ResourceClass::GpuMixed, move |pause, progress| {
        // Told once its decoders and renderer are let go: a caller that
        // waits on it and exits doesn't race them closing down (#3287).
        let tally = {
            let mut src = source();
            let mut renderer = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            renderer.set_gpu_enabled(true);
            renderer.set_working_colorspace(req.working);
            req.draw.apply(&mut renderer);
            let mut tally = FreezeTally::default();
            let n = req.frames.len().max(1);
            for (i, at) in req.frames.iter().enumerate() {
                pause.wait_while_paused();
                if pause.is_cancelled() {
                    tally.cancelled = true;
                    break;
                }
                match req.freeze_frame(&mut renderer, &store, *at, &mut src, &mut tally.on_cpu) {
                    FrameOutcome::Stored => tally.stored += 1,
                    FrameOutcome::Current => tally.current += 1,
                    FrameOutcome::Live(_) => {
                        tally.live += 1;
                        tally.live_frames.push(*at);
                    }
                    FrameOutcome::Empty => tally.empty += 1,
                    FrameOutcome::Failed(why) => {
                        tracing::warn!("freeze: track {} at {:.3} s: {why}", req.track + 1, at.to_secs_f64());
                        tally.failed += 1;
                        tally.failed_frames.push(*at);
                    }
                    FrameOutcome::Full => {
                        tally.full = true;
                        break;
                    }
                }
                tally.reached = i + 1;
                progress.set((i + 1) as f32 / n as f32);
            }
            tally
        };
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
        FreezeRequest { seq: Arc::new(flat.clone()), sequences: Arc::new(vec![]), track: 0, track_id: flat.tracks[0].id, frames, size: (W, H), working: klipp_core::WorkingColorspace::Rec709Linear, draw: DrawSettings::current(true), environment: 1, media: Arc::new(HashMap::new()) }
    }

    fn store(name: &str) -> (Arc<Mutex<CacheStore>>, std::path::PathBuf) {
        let d = std::env::temp_dir().join(format!("klipp-freeze-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        (Arc::new(Mutex::new(CacheStore::open(&d, 1 << 30))), d)
    }

    fn run(req: FreezeRequest, store: &Arc<Mutex<CacheStore>>) -> FreezeTally {
        let (tx, rx) = std::sync::mpsc::channel();
        let job = spawn_freeze(req, Arc::clone(store), || Pictures, move |t| tx.send(t).unwrap());
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
        r.set_frozen_layers(HashMap::from([(0, Arc::new(klipp_compose::freeze::FrozenLayer::from_linear(&layer)))]));
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
        let job = spawn_freeze(request_for(&flat, frames.clone()), Arc::clone(&s), || Pictures, move |t| tx.send(t).unwrap());
        while job.progress() <= 0.0 && !job.is_done() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        job.cancel();
        job.join();
        let tally = rx.recv().unwrap();
        assert!(tally.cancelled && tally.stored < frames.len(), "{tally:?}");
        assert_eq!(tally.reached, tally.stored, "gone through as far as it stored: the rest is still to draw");
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
        let job = spawn_freeze(request_for(&flat, frames.clone()), Arc::clone(&s), || Pictures, move |t| tx.send(t).unwrap());
        while job.progress() <= 0.0 {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        job.pause();
        // The frame in hand is finished first, however long the machine
        // takes over it: under load that was past the 100 ms this once
        // waited (#3289). So the progress is read once it has held still,
        // and then must stay so.
        let mut held = job.progress();
        let t0 = std::time::Instant::now();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(150));
            let now = job.progress();
            if now == held || t0.elapsed() > std::time::Duration::from_secs(10) {
                break;
            }
            held = now;
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(job.progress(), held, "no frame drawn while paused");
        assert!(held < 1.0, "not done while paused");
        job.resume();
        job.join();
        let tally = rx.recv().unwrap();
        assert_eq!((tally.stored, tally.reached), (frames.len(), frames.len()));
        assert!(tally.live_frames.is_empty() && tally.failed_frames.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod track_frames_tests {
    use super::*;
    use klipp_core::{Fps, MediaId};
    use klipp_timeline::{Clip, Track, TrackKind};

    fn seq(fps: Fps, clips: Vec<Clip>) -> Sequence {
        let mut s = Sequence::new_default();
        s.fps = fps;
        let mut t = Track::new("V1", TrackKind::Video);
        t.clips = clips;
        s.tracks = vec![t];
        s
    }

    fn at(secs: f64, rate: u32) -> Timecode {
        Timecode::from_secs_f64(secs, rate)
    }

    /// The frames under the clips, none in the gap between them, each
    /// once, as playback times them; from the playhead on first, then
    /// the ones before it.
    #[test]
    fn a_track_s_frames_are_those_under_its_clips_from_the_playhead_on_first() {
        let m = MediaId::new();
        let s = seq(Fps::new(25, 1), vec![Clip::media(m, at(0.0, 25), at(0.2, 25), at(0.0, 25), at(0.2, 25)), Clip::media(m, at(0.4, 25), at(0.6, 25), at(0.0, 25), at(0.2, 25))]);
        let frames = track_frames(&s, 0, at(0.48, 25));
        let index: Vec<i64> = frames.iter().map(|t| t.value).collect();
        assert_eq!(index, vec![12, 13, 14, 0, 1, 2, 3, 4, 10, 11], "from frame 12 on, then before");
        assert!(frames.iter().all(|t| t.rate == 25));
        assert!(track_frames(&s, 3, at(0.0, 25)).is_empty(), "no such track");
    }

    /// At 29.97 a frame is 1001 ticks of 30000, as the engine's playhead
    /// counts; a clip whose edges fall between frames covers the frames
    /// that start inside it.
    #[test]
    fn ntsc_frames_are_timed_as_playback_times_them() {
        let m = MediaId::new();
        let s = seq(Fps::ntsc_29970(), vec![Clip::media(m, at(0.05, 30_000), at(0.15, 30_000), at(0.0, 30_000), at(0.1, 30_000))]);
        let frames = track_frames(&s, 0, at(0.0, 30_000));
        assert_eq!(frames, vec![Timecode { value: 2 * 1001, rate: 30_000 }, Timecode { value: 3 * 1001, rate: 30_000 }, Timecode { value: 4 * 1001, rate: 30_000 }]);
    }

    #[test]
    fn a_status_says_how_far_its_frames_are_drawn() {
        let mut s = FreezeStatus { frames: 240, progress: Some(0.25), ..Default::default() };
        assert_eq!(s.describe(), "frozen, drawing its frames: 25 % of 240");
        s.progress = None;
        s.tally = Some(FreezeTally { stored: 200, current: 30, live: 10, ..Default::default() });
        assert_eq!(s.describe(), "frozen, 230 of 240 frames current, 10 played live (they read other tracks)");
        s.tally = Some(FreezeTally { stored: 5, full: true, ..Default::default() });
        assert!(s.describe().ends_with("the cache is full"), "{}", s.describe());
        assert!(FreezeStatus { unsaved: true, ..Default::default() }.describe().contains("until the project is saved"));
    }

    /// Frames one after another make one span, from the first frame's
    /// start to the last one's end, in whatever order they come.
    #[test]
    fn frames_make_spans() {
        let fps = Fps::ntsc_29970();
        let f = |n: i64| Timecode { value: n * 1001, rate: 30_000 };
        let s = spans(&[f(5), f(3), f(4), f(9), f(4)], fps);
        assert_eq!(s, vec![(f(3), f(6)), (f(9), f(10))]);
        assert!(spans(&[], fps).is_empty());
    }

    #[test]
    fn a_status_is_drawing_current_or_stale() {
        let drawing = FreezeStatus { frames: 10, progress: Some(0.3), ..Default::default() };
        assert_eq!(drawing.state(), FreezeState::Drawing(0.3));
        let done = FreezeStatus { frames: 10, tally: Some(FreezeTally { stored: 6, current: 2, live: 1, empty: 1, ..Default::default() }), ..Default::default() };
        assert_eq!(done.state(), FreezeState::Current);
        for stale in [
            FreezeStatus { frames: 10, ..Default::default() },
            FreezeStatus { frames: 10, tally: Some(FreezeTally { stored: 9, ..Default::default() }), ..Default::default() },
            FreezeStatus { frames: 10, tally: Some(FreezeTally { stored: 9, failed: 1, ..Default::default() }), ..Default::default() },
            FreezeStatus { frames: 10, tally: Some(FreezeTally { stored: 10, full: true, ..Default::default() }), ..Default::default() },
            FreezeStatus { frames: 10, unsaved: true, ..Default::default() },
        ] {
            assert_eq!(stale.state(), FreezeState::Stale, "{stale:?}");
        }
    }
}

#[cfg(test)]
mod key_input_tests {
    use super::*;
    use klipp_core::{GradeEncoding, WorkingColorspace};

    /// #3265: the environment a frozen or kept frame's key takes moves
    /// with each thing that changes how a frame is drawn: the working
    /// space, proxies, the grade's encoding and the LUT lookup.
    #[test]
    fn the_environment_moves_with_each_way_a_frame_is_drawn() {
        let draw = DrawSettings { encoding: GradeEncoding::Bt1886, lut_tetrahedral: true };
        let base = environment(WorkingColorspace::Rec709Linear, true, draw);
        assert_eq!(base, environment(WorkingColorspace::Rec709Linear, true, draw), "the same inputs, the same key");
        let others = [
            ("working space", environment(WorkingColorspace::AcesCG, true, draw)),
            ("proxies", environment(WorkingColorspace::Rec709Linear, false, draw)),
            ("grade encoding", environment(WorkingColorspace::Rec709Linear, true, DrawSettings { encoding: GradeEncoding::AcesCct, ..draw })),
            ("LUT lookup", environment(WorkingColorspace::Rec709Linear, true, DrawSettings { lut_tetrahedral: false, ..draw })),
        ];
        for (what, key) in others {
            assert_ne!(key, base, "{what} leaves the environment as it was");
        }
    }

    /// #3265: a media item's identity moves with its file rewritten in
    /// place (same path, new content), its proxy and its log format; a
    /// frei0r plug-in's with its version and its file.
    #[test]
    fn a_file_or_plugin_changed_in_place_keys_anew() {
        let dir = std::env::temp_dir().join(format!("klipp-key-inputs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shot.mov");
        std::fs::write(&path, b"first take").unwrap();
        let mut item = klipp_media::MediaItem::synthetic_for_test(path.clone(), None);
        let first = media_identity(&item);
        assert_eq!(first, media_identity(&item));
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, b"a second, longer take").unwrap();
        let rewritten = media_identity(&item);
        assert_ne!(rewritten, first, "the file rewritten at its path");
        item.proxy_path = Some(dir.join("shot.proxy.mov"));
        let proxied = media_identity(&item);
        assert_ne!(proxied, rewritten, "a proxy");
        item.log_format = klipp_core::LogFormat::Linear;
        assert_ne!(media_identity(&item), proxied, "a log format");

        let plugin = dir.join("glow.so");
        std::fs::write(&plugin, b"v1").unwrap();
        let v1 = frei0r_identity(&plugin, (1, 0));
        assert_ne!(frei0r_identity(&plugin, (1, 1)), v1, "a new version");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&plugin, b"v1, rebuilt").unwrap();
        assert_ne!(frei0r_identity(&plugin, (1, 0)), v1, "the file replaced");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
