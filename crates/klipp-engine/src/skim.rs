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
}

struct Entry {
    path: PathBuf,
    rotation: i32,
    decoder: VideoDecoder,
    last_used: Instant,
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
        let dec = &entry.decoder;

        let epoch = dec.seek(secs.max(0.0));
        dec.resume();
        let deadline = Instant::now() + timeout;
        let frame = loop {
            {
                let mut q = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
                let mut taken = None;
                while let Some(f) = q.front() {
                    if f.seek_epoch < epoch {
                        q.pop();
                        continue;
                    }
                    if f.is_eof {
                        q.pop();
                        dec.pause();
                        return Err(format!("no frame at {secs:.3}s: the file ends before it"));
                    }
                    taken = q.pop();
                    break;
                }
                if let Some(f) = taken {
                    break f;
                }
            }
            if Instant::now() >= deadline {
                dec.pause();
                return Err(format!("no frame at {secs:.3}s within {timeout:?}"));
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        // Read ahead stays queued for the next position; the thread need
        // not run meanwhile.
        dec.pause();

        if frame.hdr.is_some() {
            return Err("an HDR frame; skim HDR media from its tone-mapped transcode".into());
        }
        let (mut rgba, mut w, mut h) = match &frame.gpu_source {
            Some(gpu) if frame.data.is_empty() => (realise_srgb_u8_from_gpu(gpu), frame.width, frame.height),
            _ => (frame.to_rgba(), frame.width, frame.height),
        };
        if rgba.len() != (w as usize) * (h as usize) * 4 {
            return Err(format!("frame bytes do not match {w}x{h}"));
        }
        while h > max_height.max(2) && w >= 2 && h >= 2 {
            (rgba, w, h) = halve_rgba(&rgba, w, h);
        }
        Ok(SkimFrame { rgba, width: w, height: h, reused_decoder: reused })
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
        self.open.push(Entry { path: path.to_path_buf(), rotation, decoder, last_used: Instant::now() });
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
                    .or_else(|e| {
                        tracing::debug!("skim: decoder path failed for {:?} @ {:.3}s ({e}); software fallback", job.path, job.secs);
                        crate::generate_thumbnail_at_size_with_rotation(&job.path, job.secs, job.max_height.max(240), job.rotation)
                            .map(|(rgba, width, height)| SkimFrame { rgba, width, height, reused_decoder: false })
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

#[cfg(test)]
mod tests {
    use super::halve_rgba;

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
