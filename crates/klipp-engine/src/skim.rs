//! Skim frames — the picture under the mouse while hovering a clip in
//! the media browser or on the timeline — from decoders that stay open.
//!
//! Every hover position used to open the file, build a *software* H.264
//! decoder with a thread per core, seek to the keyframe before the
//! target, decode up to it, convert the frame to RGBA with swscale on
//! the CPU, and throw the decoder away. A skim is a stream of positions,
//! so that burned every core at mouse-move rate while NVDEC sat idle.
//!
//! [`SkimDecoders`] keeps one playback decoder ([`VideoDecoder`]: NVDEC
//! where the file allows, with the NV12→RGBA conversion on the GPU) open
//! per skimmed file, a few most recently used. A position is a seek on
//! that decoder — a step forward decodes on and keeps its read-ahead —
//! and one read of the frame's bytes. [`SkimWorker`] runs one of these
//! on its own thread, answering the newest request first.

use crate::types::realise_srgb_u8_from_gpu;
use crate::video_decoder::VideoDecoder;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A skim frame: sRGB RGBA8 bytes.
pub struct SkimFrame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Whether the decoder that made it was already open.
    pub reused_decoder: bool,
    /// Whether it is the frame this decoder last delivered, answered again
    /// without a seek because the time asked for is still inside it.
    pub repeated: bool,
}

/// The frame of a `fps` stream shown at `secs`: the one whose display
/// interval contains it. A pointer resting between two frames' start times
/// is on the earlier one.
pub fn frame_index(secs: f64, fps: f64) -> i64 {
    (secs * fps + 1e-6).floor() as i64
}

/// Slack for comparing a time with a frame's timestamp, which the
/// container rounds.
const PTS_SLACK: f64 = 1e-4;

/// The frame a decoder last delivered, and the span of time it is shown
/// for.
struct Delivered {
    from: f64,
    to: f64,
    max_height: u32,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

struct Entry {
    path: PathBuf,
    rotation: i32,
    decoder: VideoDecoder,
    last_used: Instant,
    last: Option<Delivered>,
}

/// The open skim decoders, most recently used kept.
pub struct SkimDecoders {
    open: Vec<Entry>,
    max_open: usize,
}

impl SkimDecoders {
    pub fn new(max_open: usize) -> Self {
        Self { open: Vec::new(), max_open: max_open.max(1) }
    }

    pub fn open_count(&self) -> usize {
        self.open.len()
    }

    /// Close decoders not used for `idle`.
    pub fn sweep(&mut self, idle: Duration) {
        self.open.retain(|e| e.last_used.elapsed() < idle);
    }

    /// The frame of `path` at `secs`, from its open decoder — opened now
    /// if it is not — within `timeout`. Scaled down by halving while
    /// taller than `max_height`. An HDR frame is refused: the skim of
    /// HDR media comes from its tone-mapped transcode, and this decoder
    /// would hand back scene light.
    pub fn frame_at(
        &mut self,
        path: &Path,
        rotation: i32,
        secs: f64,
        max_height: u32,
        timeout: Duration,
    ) -> Result<SkimFrame, String> {
        let reused = self.ensure_open(path, rotation)?;
        let entry = self.open.last_mut().expect("just ensured");
        entry.last_used = Instant::now();
        let secs = secs.max(0.0);

        // Still inside the frame last delivered: that is the frame shown
        // now. Every repeat of a position used to seek again, and a seek
        // lands on whichever neighbour the read-ahead left, so a pointer
        // resting on a clip flickered between frames.
        if let Some(d) = entry.last.as_ref().filter(|d| d.max_height == max_height && d.from <= secs + PTS_SLACK && secs + PTS_SLACK < d.to) {
            return Ok(SkimFrame { rgba: d.rgba.clone(), width: d.width, height: d.height, reused_decoder: true, repeated: true });
        }
        let dec = &entry.decoder;

        let epoch = dec.seek(secs);
        dec.resume();
        let deadline = Instant::now() + timeout;
        // The frame shown at `secs` is the latest at or before it, and it
        // is shown until the next one starts. The decode thread begins a
        // seek up to 50 ms before its target, so the first frame to arrive
        // is not necessarily it: frames are taken while they start at or
        // before `secs`, and the first one after ends the search. A target
        // before the file's first frame gets that frame.
        let mut chosen = None;
        let mut shown_until = f64::INFINITY;
        loop {
            let mut found = false;
            {
                let mut q = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
                while let Some(f) = q.front() {
                    let (frame_epoch, eof, pts) = (f.seek_epoch, f.is_eof, f.pts_secs());
                    if frame_epoch < epoch {
                        q.pop();
                        continue;
                    }
                    if eof {
                        if chosen.is_some() {
                            found = true;
                            break;
                        }
                        q.pop();
                        dec.pause();
                        return Err(format!("no frame at {secs:.3}s: the file ends before it"));
                    }
                    if pts <= secs + PTS_SLACK {
                        chosen = q.pop();
                        continue;
                    }
                    if chosen.is_none() {
                        chosen = q.pop();
                    } else {
                        // Left queued: the next position may want it.
                        shown_until = pts;
                    }
                    found = true;
                    break;
                }
            }
            if found {
                break;
            }
            if Instant::now() >= deadline {
                if chosen.is_some() {
                    break;
                }
                dec.pause();
                return Err(format!("no frame at {secs:.3}s within {timeout:?}"));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        // Read ahead stays queued for the next position; the thread need
        // not run meanwhile.
        dec.pause();
        let frame = chosen.expect("a frame was chosen");
        let shown_from = frame.pts_secs().min(secs);
        if !shown_until.is_finite() {
            shown_until = shown_from.max(frame.pts_secs()) + 1.0 / 30.0;
        }

        if frame.hdr.is_some() {
            return Err("an HDR frame; skim HDR media from its tone-mapped transcode".into());
        }
        let (mut rgba, mut w, mut h) = match &frame.gpu_source {
            // The readback is the GPU buffer's own size: labelled with the
            // frame's, a rotated clip's swapped sides sheared the tile.
            Some(gpu) if frame.data.is_empty() => (realise_srgb_u8_from_gpu(gpu), gpu.width, gpu.height),
            _ => (frame.to_rgba(), frame.width, frame.height),
        };
        if rgba.len() != (w as usize) * (h as usize) * 4 {
            return Err(format!("frame bytes do not match {w}x{h}"));
        }
        while h > max_height.max(2) && w >= 2 && h >= 2 {
            (rgba, w, h) = halve_rgba(&rgba, w, h);
        }
        if let Some(e) = self.open.last_mut() {
            e.last = Some(Delivered { from: shown_from, to: shown_until, max_height, rgba: rgba.clone(), width: w, height: h });
        }
        Ok(SkimFrame { rgba, width: w, height: h, reused_decoder: reused, repeated: false })
    }

    /// Make `path`'s decoder the last entry, opening it if needed.
    /// Returns whether it was already open.
    fn ensure_open(&mut self, path: &Path, rotation: i32) -> Result<bool, String> {
        if let Some(i) = self.open.iter().position(|e| e.path == path && e.rotation == rotation) {
            let e = self.open.remove(i);
            self.open.push(e);
            return Ok(true);
        }
        while self.open.len() >= self.max_open {
            // Least recently used first: entries are kept in use order.
            self.open.remove(0);
        }
        let decoder = VideoDecoder::open(path, rotation).map_err(|e| format!("opening {}: {e}", path.display()))?;
        self.open.push(Entry { path: path.to_path_buf(), rotation, decoder, last_used: Instant::now(), last: None });
        Ok(false)
    }
}

/// `rgba` at half size, each output pixel the mean of a 2×2 block.
fn halve_rgba(rgba: &[u8], w: u32, h: u32) -> (Vec<u8>, u32, u32) {
    let (nw, nh) = (w / 2, h / 2);
    let mut out = Vec::with_capacity((nw * nh * 4) as usize);
    let row = (w * 4) as usize;
    for y in 0..nh as usize {
        let (r0, r1) = (y * 2 * row, (y * 2 + 1) * row);
        for x in 0..nw as usize {
            let (a, b) = (r0 + x * 8, r1 + x * 8);
            for c in 0..4 {
                let sum = rgba[a + c] as u32 + rgba[a + 4 + c] as u32 + rgba[b + c] as u32 + rgba[b + 4 + c] as u32;
                out.push(((sum + 2) / 4) as u8);
            }
        }
    }
    (out, nw, nh)
}

/// One position to skim.
#[derive(Clone, Debug)]
pub struct SkimJob<T: Clone + Send + 'static> {
    pub path: PathBuf,
    /// The original media, when `path` is its skim transcode: tried when
    /// the transcode has no frame at the time (it ends early, or is
    /// damaged), so the skim shows the picture rather than nothing.
    pub fallback_path: Option<PathBuf>,
    pub rotation: i32,
    pub secs: f64,
    pub max_height: u32,
    /// Anything the requester needs back with the frame.
    pub tag: T,
}

/// A thread owning a [`SkimDecoders`], answering the newest request:
/// positions that arrived while one was being made are skipped, as a
/// scrub only wants the latest.
pub struct SkimWorker<T: Clone + Send + 'static> {
    tx: crossbeam_channel::Sender<SkimJob<T>>,
}

impl<T: Clone + Send + 'static> SkimWorker<T> {
    /// Start the thread. `deliver` is called with every answered job.
    /// A frame the decoders cannot make (an HDR original, a file NVDEC
    /// refuses) is made the old way, by a one-off software decode, so
    /// the skim never goes blank.
    pub fn spawn(max_open: usize, deliver: impl Fn(SkimJob<T>, Result<SkimFrame, String>) + Send + 'static) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<SkimJob<T>>();
        let spawned = std::thread::Builder::new().name("klipp-skim".into()).spawn(move || {
            let mut decoders = SkimDecoders::new(max_open);
            loop {
                let mut job = match rx.recv_timeout(Duration::from_secs(5)) {
                    Ok(job) => job,
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        decoders.sweep(Duration::from_secs(30));
                        continue;
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
                };
                while let Ok(newer) = rx.try_recv() {
                    job = newer;
                }
                let t0 = Instant::now();
                let result = decoders
                    .frame_at(&job.path, job.rotation, job.secs, job.max_height, Duration::from_millis(400))
                    .or_else(|e| match &job.fallback_path {
                        Some(original) => {
                            tracing::debug!("skim: {:?} @ {:.3}s failed ({e}); from the original", job.path, job.secs);
                            decoders.frame_at(original, job.rotation, job.secs, job.max_height, Duration::from_millis(600))
                        }
                        None => Err(e),
                    })
                    .or_else(|e| {
                        let source = job.fallback_path.as_deref().unwrap_or(&job.path);
                        tracing::debug!("skim: decoder path failed for {:?} @ {:.3}s ({e}); software fallback", source, job.secs);
                        crate::generate_thumbnail_at_size_with_rotation(source, job.secs, job.max_height.max(240), job.rotation)
                            .map(|(rgba, width, height)| SkimFrame { rgba, width, height, reused_decoder: false, repeated: false })
                    });
                tracing::trace!("skim: {:?} @ {:.3}s in {:.1} ms", job.path.file_name().unwrap_or_default(), job.secs, t0.elapsed().as_secs_f64() * 1000.0);
                deliver(job, result);
            }
        });
        if let Err(e) = spawned {
            tracing::error!("skim worker thread could not start: {e}");
        }
        Self { tx }
    }

    pub fn request(&self, job: SkimJob<T>) {
        let _ = self.tx.send(job);
    }
}

/// A thread owning a [`SkimDecoders`] of its own, answering the newest
/// batch of requests: one frame per job, each delivered as it is made.
/// For a picture of several sources at once (a multicam grid's angles),
/// where [`SkimWorker`] keeps only the last request and would starve all
/// but one. A batch that arrives while one is being made replaces what
/// is left of it.
pub struct SkimBatchWorker<T: Clone + Send + 'static> {
    tx: crossbeam_channel::Sender<Vec<SkimJob<T>>>,
}

impl<T: Clone + Send + 'static> SkimBatchWorker<T> {
    /// Start the thread with room for `max_open` decoders, one per source
    /// the batches name. `deliver` is called with every answered job.
    pub fn spawn(max_open: usize, deliver: impl Fn(SkimJob<T>, Result<SkimFrame, String>) + Send + 'static) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<Vec<SkimJob<T>>>();
        let spawned = std::thread::Builder::new().name("klipp-skim-batch".into()).spawn(move || {
            let mut decoders = SkimDecoders::new(max_open);
            let mut pending: std::collections::VecDeque<(usize, SkimJob<T>)> = std::collections::VecDeque::new();
            // Where the next batch starts: after the source served last.
            let mut next_start = 0;
            loop {
                if pending.is_empty() {
                    match rx.recv_timeout(Duration::from_secs(5)) {
                        Ok(batch) => pending = rotated(batch, next_start),
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                            decoders.sweep(Duration::from_secs(30));
                            continue;
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
                    }
                }
                while let Ok(newer) = rx.try_recv() {
                    pending = rotated(newer, next_start);
                }
                let Some((index, job)) = pending.pop_front() else { continue };
                next_start = index + 1;
                let result = decoders
                    .frame_at(&job.path, job.rotation, job.secs, job.max_height, Duration::from_millis(300))
                    .or_else(|e| match &job.fallback_path {
                        Some(original) => decoders.frame_at(original, job.rotation, job.secs, job.max_height, Duration::from_millis(400)),
                        None => Err(e),
                    })
                    // As the single skim does: a frame no decoder here will
                    // make (a file NVDEC refuses) is made by a one-off
                    // software decode, so a tile is never left blank (#84).
                    .or_else(|e| {
                        let source = job.fallback_path.as_deref().unwrap_or(&job.path);
                        tracing::debug!("skim batch: decoders failed for {:?} @ {:.3}s ({e}); software fallback", source, job.secs);
                        crate::generate_thumbnail_at_size_with_rotation(source, job.secs, job.max_height.max(240), job.rotation)
                            .map(|(rgba, width, height)| SkimFrame { rgba, width, height, reused_decoder: false, repeated: false })
                            .map_err(|soft| format!("{e}; software decode: {soft}"))
                    });
                deliver(job, result);
            }
        });
        if let Err(e) = spawned {
            tracing::error!("skim batch worker thread could not start: {e}");
        }
        Self { tx }
    }

    /// Replace whatever is waiting with `batch`.
    pub fn request(&self, batch: Vec<SkimJob<T>>) {
        let _ = self.tx.send(batch);
    }
}

/// `batch` with each job's place in it, starting at `start` and wrapping.
/// A batch that replaces another starts after the source served last, so
/// sources take turns: started at the first each time, a stream of
/// batches (a scrub while paused) kept serving the first and starved the
/// rest.
fn rotated<J>(batch: Vec<J>, start: usize) -> std::collections::VecDeque<(usize, J)> {
    let n = batch.len();
    let mut out: std::collections::VecDeque<(usize, J)> = batch.into_iter().enumerate().collect();
    if n > 0 {
        out.rotate_left(start % n);
    }
    out
}

#[cfg(test)]
mod batch_tests {
    use super::rotated;

    /// A batch replacing another carries on from where the last left off,
    /// wrapping, so every source is served in turn.
    #[test]
    fn a_new_batch_starts_after_the_source_served_last() {
        let order = |start| rotated(vec!['a', 'b', 'c', 'd'], start).into_iter().collect::<Vec<_>>();
        assert_eq!(order(0), vec![(0, 'a'), (1, 'b'), (2, 'c'), (3, 'd')]);
        assert_eq!(order(2), vec![(2, 'c'), (3, 'd'), (0, 'a'), (1, 'b')]);
        assert_eq!(order(5), vec![(1, 'b'), (2, 'c'), (3, 'd'), (0, 'a')], "wraps");
        assert!(rotated(Vec::<char>::new(), 3).is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::{frame_index, halve_rgba};

    #[test]
    fn a_time_belongs_to_the_frame_shown_at_it() {
        assert_eq!(frame_index(18.122, 30.0), 543, "18.122 s is inside the frame shown from 18.100 s");
        assert_eq!(frame_index(18.1334, 30.0), 544);
        assert_eq!(frame_index(0.40, 25.0), 10, "a frame's own start time is that frame");
        assert_eq!(frame_index(0.4399, 25.0), 10);
        assert_eq!(frame_index(0.44, 25.0), 11);
    }

    #[test]
    fn halving_averages_each_block() {
        let px = |v: u8| [v, v, v, 255];
        let rgba: Vec<u8> = [px(0), px(100), px(200), px(0), px(10), px(30), px(50), px(70)].concat();
        let (out, w, h) = halve_rgba(&rgba, 4, 2);
        assert_eq!((w, h), (2, 1));
        assert_eq!(&out[..4], &[35, 35, 35, 255], "mean of 0,100,10,30");
        assert_eq!(&out[4..8], &[80, 80, 80, 255], "mean of 200,0,50,70");
    }
}
