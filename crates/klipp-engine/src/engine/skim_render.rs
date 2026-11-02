//! Timeline skim frames as the picture (#2945): titles, effects, the
//! grade, transitions and scenes in scenes, not the raw clip under the
//! cursor.
//!
//! A skim position is drawn by the GPU stack the parked frame uses, its
//! sources read from the media's skim files (fast to seek, 1080 lines at
//! most) by decoders of its own, a few kept open. A decoded frame stays
//! on the GPU and is handed to the stack as it is; the stack's picture
//! goes to the viewer's texture, read back only where the viewer has
//! none. What the stack can't draw, or a source with no skim file yet,
//! is not drawn here: the UI shows the raw skim frame while moving, and
//! the parked frame once the cursor rests.

use super::streams::HeldFrame;
use crate::video_decoder::VideoDecoder;
use klipp_compose::{FrameSource, SourceFrame};
use klipp_core::{ClipId, MediaId, Timecode};
use klipp_timeline::Clip;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Skim decoders kept open at most: each holds its frames on the GPU,
/// hundreds of megabytes (#2935), so the least recently used is closed
/// past this.
pub(crate) const SKIM_DECODERS: usize = 4;

/// How long a skim position waits for a source's frame before it is left
/// to the raw skim frame; a decoder just opened has longer, for its first.
const SKIM_BUDGET: Duration = Duration::from_millis(60);
const SKIM_FIRST_BUDGET: Duration = Duration::from_millis(400);

/// Slack for comparing a time with a frame's timestamp, which the
/// container rounds.
const PTS_SLACK: f64 = 1e-4;

/// Sources handed to a skim render that were not on the GPU (a file the
/// GPU decoder refused, decoded in software): counted for the test that
/// a skim reads nothing back.
pub static SKIM_SOURCES_OFF_GPU: AtomicU64 = AtomicU64::new(0);
/// Skim positions drawn by the stack, and those left to the raw frame.
pub static SKIMS_COMPOSITED: AtomicU64 = AtomicU64::new(0);
pub static SKIMS_DECLINED: AtomicU64 = AtomicU64::new(0);

struct Open {
    path: PathBuf,
    decoder: VideoDecoder,
    /// The frame last delivered and the time it is shown until: a
    /// position inside it is that frame again, without a seek.
    last: Option<(f64, f64, HeldFrame)>,
}

/// The skim decoders, most recently used last, at most
/// [`SKIM_DECODERS`].
#[derive(Default)]
pub(crate) struct SkimPool {
    open: Vec<Open>,
    /// Why the last position's source wasn't there (no skim file, or its
    /// decoder didn't open), for the skim's note.
    failed_open: Option<String>,
}

impl SkimPool {
    /// Whether a skim decoder is open: a skim is under way.
    pub fn is_open(&self) -> bool {
        !self.open.is_empty()
    }

    /// Close every decoder: the skim is over.
    pub fn clear(&mut self) {
        self.open.clear();
    }

    /// `path`'s decoder, made the most recent, and whether it was opened
    /// now.
    fn entry(&mut self, path: &Path) -> Option<(&mut Open, bool)> {
        let opened = if let Some(i) = self.open.iter().position(|o| o.path == path) {
            let o = self.open.remove(i);
            self.open.push(o);
            false
        } else {
            while self.open.len() >= SKIM_DECODERS {
                self.open.remove(0);
            }
            let decoder = VideoDecoder::open(path, 0)
                .map_err(|e| {
                    tracing::debug!("skim: cannot open {}: {e}", path.display());
                    self.failed_open = Some(format!("{} didn't open: {e}", path.file_name().unwrap_or_default().to_string_lossy()));
                })
                .ok()?;
            self.open.push(Open { path: path.to_path_buf(), decoder, last: None });
            true
        };
        self.open.last_mut().map(|o| (o, opened))
    }

    /// The frame of `path` shown at `secs`, within the budget.
    fn frame(&mut self, path: &Path, secs: f64) -> Option<HeldFrame> {
        let secs = secs.max(0.0);
        let (o, opened) = self.entry(path)?;
        let deadline = Instant::now() + if opened { SKIM_FIRST_BUDGET } else { SKIM_BUDGET };
        if let Some((_, _, f)) = o.last.as_ref().filter(|(from, to, _)| *from <= secs + PTS_SLACK && secs + PTS_SLACK < *to) {
            return Some(f.clone());
        }
        let dec = &o.decoder;
        let epoch = dec.seek(secs);
        dec.resume();
        // The latest frame at or before `secs`; the first after it ends
        // the search, and says how long the chosen one is shown.
        let mut chosen = None;
        let mut until = f64::INFINITY;
        'wait: loop {
            {
                let mut q = dec.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
                while let Some(f) = q.front() {
                    if f.seek_epoch < epoch {
                        q.pop();
                        continue;
                    }
                    if f.is_eof {
                        if chosen.is_none() {
                            q.pop();
                        }
                        break 'wait;
                    }
                    let pts = f.pts_secs();
                    if pts <= secs + PTS_SLACK || chosen.is_none() {
                        let early = pts <= secs + PTS_SLACK;
                        chosen = q.pop();
                        if early {
                            continue;
                        }
                        break 'wait;
                    }
                    until = pts;
                    break 'wait;
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        dec.pause();
        let frame = chosen?;
        let from = frame.pts_secs().min(secs);
        let held = HeldFrame::from_frame(frame);
        if !until.is_finite() {
            until = from + 1.0 / 30.0;
        }
        o.last = Some((from, until, held.clone()));
        Some(held)
    }
}

/// Where each media's skim frames come from: its skim file, or a still
/// image's own file. `None` for media with neither, still transcoding.
pub(crate) fn skim_file(item: &klipp_media::MediaItem) -> Option<PathBuf> {
    if item.is_still {
        return Some(item.path.clone());
    }
    item.skim_file.clone().filter(|p| p.exists())
}

/// The frames of one skim position, by clip: decoded before the render,
/// so a position missing one is left to the raw frame whole, never drawn
/// with a layer gone.
pub(crate) struct SkimSource {
    frames: HashMap<ClipId, HeldFrame>,
}

impl SkimSource {
    /// Every media clip `seq` shows at `time`, nested scenes' too, at its
    /// own source time, from `pool`; `None` when one has no skim file or
    /// its frame didn't come in time.
    pub fn gather(seq: &klipp_timeline::Sequence, time: Timecode, files: &HashMap<MediaId, PathBuf>, pool: &mut SkimPool) -> Option<Self> {
        let mut frames = HashMap::new();
        // One decode per frame of a file, however many clips show it.
        let mut decoded: Vec<(PathBuf, f64, HeldFrame)> = Vec::new();
        for layer in klipp_compose::active_layers(seq, time) {
            for c in &layer.clips {
                let Some(media) = c.media_id() else { continue };
                let Some(path) = files.get(&media) else {
                    pool.failed_open = Some(format!("{} has no skim file", c.id.0));
                    return None;
                };
                let secs = klipp_compose::render::source_secs(c, layer.at, layer.fps);
                let frame = match decoded.iter().find(|(p, s, _)| p == path && (s - secs).abs() < PTS_SLACK) {
                    Some((_, _, f)) => f.clone(),
                    None => {
                        let f = pool.frame(path, secs)?;
                        decoded.push((path.clone(), secs, f.clone()));
                        f
                    }
                };
                if frame.gpu_neighbour().is_none() {
                    SKIM_SOURCES_OFF_GPU.fetch_add(1, Ordering::Relaxed);
                }
                frames.insert(c.id, frame);
            }
        }
        Some(Self { frames })
    }
}

impl FrameSource for SkimSource {
    fn frame(&mut self, clip: &Clip, _source_secs: f64, _width: u32, _height: u32) -> Option<SourceFrame> {
        let f = self.frames.get(&clip.id)?;
        Some(SourceFrame { pixels: f.source_pixels(), width: f.width, height: f.height, pts: f.pts })
    }
}

/// The skim picture's size for a `w` × `h` sequence: its shape, 1080
/// lines at most, as the skim files are.
pub(crate) fn skim_size(w: u32, h: u32) -> (u32, u32) {
    let (w, h) = (w.max(2), h.max(2));
    if h <= 1080 {
        return (w, h);
    }
    let sw = ((w as f64 * 1080.0 / h as f64).round() as u32).max(2) & !1;
    (sw, 1080)
}

impl super::PlaybackEngine {
    /// The picture at `time` drawn for a skim (#2945): composited by the
    /// GPU stack from skim-file frames and shown, the playhead left where
    /// it is. `false` when it isn't drawn: playing, a display path the
    /// stack doesn't take, a source with no skim file or no frame in
    /// time, or a frame the stack declines. The UI shows the raw skim
    /// frame then.
    pub(super) fn skim_timeline(&mut self, time: Timecode) -> bool {
        let shown = self.skim_timeline_inner(time);
        let counter = if shown { &SKIMS_COMPOSITED } else { &SKIMS_DECLINED };
        counter.fetch_add(1, Ordering::Relaxed);
        shown
    }

    fn skim_timeline_inner(&mut self, time: Timecode) -> bool {
        let refused = |e: &mut Self, why: &str| {
            e.frame_buffer.lock().unwrap_or_else(|p| p.into_inner()).note(format!("skim at {:?} left to the raw frame: {why}", time.value));
            false
        };
        if self.state == super::PlaybackState::Playing {
            return refused(self, "playing");
        }
        if !self.gpu_composite_allowed() {
            return refused(self, "the display path is not the GPU stack's");
        }
        // A preview up (#3210) is what the skim shows, as the parked
        // frame does.
        let (snapshot, _, preview) = self.sequence.view_with_revision();
        let snapshot = preview.unwrap_or(snapshot);
        let seq = klipp_compose::frame_snapshot(&snapshot, time);
        let files: HashMap<MediaId, PathBuf> = self.media_pool.read().unwrap_or_else(|e| e.into_inner()).all().filter_map(|m| Some((m.id, skim_file(m)?))).collect();
        self.skim_pool.failed_open = None;
        let Some(mut src) = SkimSource::gather(&seq, time, &files, &mut self.skim_pool) else {
            // Which it was, when a decoder didn't open (the 0.3.0 run saw
            // this skim refused at once, five times, with no cause).
            let why = match self.skim_pool.failed_open.take() {
                Some(e) => format!("a source with no skim file, or no frame in time: {e}"),
                None => "a source with no skim file, or no frame in time".to_string(),
            };
            return refused(self, &why);
        };
        let (w, h) = skim_size(seq.resolution.width, seq.resolution.height);
        self.renderer.set_working_colorspace(crate::video_decoder::working_colorspace());
        self.renderer.set_grade_encoding(klipp_core::grade_encoding());
        self.renderer.set_lut_tetrahedral(self.color_pipeline.use_tetrahedral);
        self.push_global_lut();
        // Into the viewer's texture where the UI shares the device, as a
        // playing frame is shown: bytes only where it has none.
        let target = self.gpu_viewer_target(w, h);
        let out = klipp_compose::DisplayDelivery { texture: target.as_deref(), bytes: target.is_none() };
        let opts = klipp_compose::RenderOptions {
            hdr_peak_nits: self.hdr_monitor.renders_hdr().then_some(klipp_color::hdr::DEFAULT_MASTERING_MAX_LUMA_CD_M2),
            view_channel: self.view_channel,
            ..Default::default()
        };
        let Some((bytes, _)) = self.renderer.render_display_on_gpu(&seq, time, w, h, &mut src, opts, out) else { return refused(self, "the GPU stack declined the frame") };
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            match bytes {
                Some(bytes) => fb.write_skim_frame(bytes, w, h),
                None => fb.note_gpu_frame(),
            }
            fb.note(format!("skim at {:?}: composited from skim files", time.value));
        }
        (self.repaint_cb)();
        true
    }

    /// The skim is over: its decoders are closed, their memory let go,
    /// and the node outputs its frames held for a parked frame's slider
    /// drag, which no skim position is.
    pub(super) fn end_skim(&mut self) {
        if self.skim_pool.is_open() {
            self.renderer.executor().clear_held();
        }
        self.skim_pool.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_skim_is_drawn_at_most_1080_lines_in_the_sequences_shape() {
        assert_eq!(skim_size(3840, 2160), (1920, 1080));
        assert_eq!(skim_size(2160, 3840), (608, 1080));
        assert_eq!(skim_size(1280, 720), (1280, 720));
    }
}
