//! The viewer's frames, rendered by the shared renderer.
//!
//! Every frame the viewer shows goes through `klipp_compose::Renderer` —
//! the same rules export renders by — except the two fast paths in
//! `update_playback`, which only engage when the renderer would do nothing
//! but show (or grade) one clip, and are pinned to it by tests. This
//! module is what surrounds a render in the viewer: which decoded frames
//! it reads, what the grade tools ask of it, the split views, the caches,
//! and handing the result to the UI.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use klipp_compose::{FrameSource, Inspect, Inspection, RenderOptions, RenderStats, SourceFrame};
use klipp_core::{AppEvent, ClipId};
use klipp_media::MediaPool;
use klipp_timeline::{Clip, GradeAccess, Sequence};

use super::streams::{ClipStreams, HeldFrame, Want};
use super::{PlaybackEngine, PlaybackState};
use crate::types::SharedFrameQueue;

/// Decoded frames for one render: the primary decoder's frame, and the
/// clip streams' for everything else.
struct ViewerSource<'a> {
    primary: Option<(ClipId, &'a HeldFrame)>,
    /// The primary decoder's queue: its front is the frame after
    /// `primary`, for frame blending.
    primary_queue: Option<&'a SharedFrameQueue>,
    streams: &'a mut ClipStreams,
    media_pool: &'a RwLock<MediaPool>,
    /// The neighbour frames kept while parked (#1863); `None` while
    /// playing, when every frame is a new time.
    hold: Option<&'a std::sync::Arc<std::sync::Mutex<NeighbourHold>>>,
    /// Whether the viewer decodes proxies where a media item has one.
    use_proxies: bool,
    /// The primary frame the render before drew, of the same clip: a
    /// temporal denoise's neighbour before, on the GPU (#1988).
    primary_previous: Option<&'a HeldFrame>,
}

/// The source frames a temporal node read beside the parked frame, kept
/// between renders of it (#1863): a slider step on a clip with temporal
/// noise reduction decoded the frame either side again on every step,
/// from a reader opened afresh, about 290 ms at 4K.
///
/// A frame is the file's picture at a time, decoded into a working space
/// at a size, so that is its key: the same key is the same pixels, after
/// any number of seeks. The file is named by its path, length and
/// modification time, so a file replaced on disk, or a relink to another
/// file, is never shown through frames held from the one before. What
/// keeps the hold to the parked frame is its size and its clearing: a
/// temporal denoiser's pair at each of two render sizes, and emptied when
/// playback starts, where every frame is a new time. Two sizes because a
/// drag renders smaller than the frame it lets go on: with one pair, the
/// release decoded the full-size pair again and the next drag the small
/// one. At 4K the four are 266 MB and 66 MB.
#[derive(Default)]
pub(crate) struct NeighbourHold {
    frames: std::collections::VecDeque<(NeighbourKey, std::sync::Arc<Vec<f32>>, u32, u32)>,
}

/// What a held neighbour is the picture of.
#[derive(Clone, PartialEq, Debug)]
struct NeighbourKey {
    path: std::path::PathBuf,
    /// The file's length and modification time as the render found them.
    file: (u64, Option<std::time::SystemTime>),
    working: klipp_core::WorkingColorspace,
    log_format: Option<klipp_core::LogFormat>,
    size: (u32, u32),
    /// The file's time, in microseconds.
    micros: i64,
}

impl NeighbourHold {
    /// Frames held at each size: a temporal denoiser's two neighbours.
    const FRAMES: usize = 2;
    /// Render sizes held at once: a drag's and its release's.
    const SIZES: usize = 2;

    fn get(&self, key: &NeighbourKey) -> Option<(std::sync::Arc<Vec<f32>>, u32, u32)> {
        self.frames.iter().find(|(k, ..)| k == key).map(|(_, px, w, h)| (std::sync::Arc::clone(px), *w, *h))
    }

    fn put(&mut self, key: NeighbourKey, px: std::sync::Arc<Vec<f32>>, w: u32, h: u32) {
        self.frames.retain(|(k, ..)| *k != key);
        // The oldest of this size past its pair.
        if self.frames.iter().filter(|(k, ..)| k.size == key.size).count() >= Self::FRAMES {
            let oldest = self.frames.iter().position(|(k, ..)| k.size == key.size).expect("counted above");
            self.frames.remove(oldest);
        }
        // A third size: the least recently stored size goes.
        let mut sizes: Vec<(u32, u32)> = Vec::new();
        for (k, ..) in self.frames.iter().rev() {
            if !sizes.contains(&k.size) {
                sizes.push(k.size);
            }
        }
        if !sizes.contains(&key.size) && sizes.len() >= Self::SIZES {
            let gone = *sizes.last().expect("at least SIZES");
            self.frames.retain(|(k, ..)| k.size != gone);
        }
        self.frames.push_back((key, px, w, h));
    }

    pub(crate) fn clear(&mut self) {
        self.frames.clear();
    }
}

#[cfg(test)]
mod neighbour_hold_tests {
    use super::{NeighbourHold, NeighbourKey};
    use std::sync::Arc;

    fn key(micros: i64) -> NeighbourKey {
        NeighbourKey { path: "/m.mov".into(), file: (10, None), working: klipp_core::WorkingColorspace::Rec709Linear, log_format: None, size: (4, 2), micros }
    }

    /// #1863: a frame is found by its key and only by it — another time,
    /// size or working space is another frame — and the hold keeps the
    /// two latest.
    #[test]
    fn the_hold_finds_a_frame_by_its_whole_key_and_keeps_two() {
        let mut hold = NeighbourHold::default();
        hold.put(key(1), Arc::new(vec![1.0]), 4, 2);
        hold.put(key(2), Arc::new(vec![2.0]), 4, 2);
        assert_eq!(hold.get(&key(1)).map(|f| f.0[0]), Some(1.0));
        assert_eq!(hold.get(&key(2)).map(|f| f.0[0]), Some(2.0));
        assert!(hold.get(&NeighbourKey { size: (8, 4), ..key(1) }).is_none(), "another size");
        assert!(hold.get(&NeighbourKey { file: (11, None), ..key(1) }).is_none(), "the file rewritten");
        assert!(hold.get(&NeighbourKey { working: klipp_core::WorkingColorspace::AcesCG, ..key(1) }).is_none(), "another working space");
        hold.put(key(3), Arc::new(vec![3.0]), 4, 2);
        assert!(hold.get(&key(1)).is_none(), "the oldest of three is dropped");
        assert_eq!(hold.get(&key(3)).map(|f| f.0[0]), Some(3.0));
        hold.clear();
        assert!(hold.get(&key(3)).is_none());
    }

    /// A drag's pair and its release's pair, at two sizes, are both held:
    /// letting go and dragging again decode nothing. A third size drops
    /// the size stored longest ago, so the hold stays at four frames.
    #[test]
    fn a_pair_is_held_at_each_of_two_sizes() {
        let at = |micros, size| NeighbourKey { size, ..key(micros) };
        let (small, full, other) = ((2, 1), (4, 2), (8, 4));
        let mut hold = NeighbourHold::default();
        for size in [small, full] {
            hold.put(at(1, size), Arc::new(vec![1.0]), size.0, size.1);
            hold.put(at(3, size), Arc::new(vec![3.0]), size.0, size.1);
        }
        for size in [small, full] {
            assert!(hold.get(&at(1, size)).is_some() && hold.get(&at(3, size)).is_some(), "the pair at {size:?}");
        }
        hold.put(at(5, full), Arc::new(vec![5.0]), 4, 2);
        assert!(hold.get(&at(1, full)).is_none() && hold.get(&at(1, small)).is_some(), "a third frame at a size drops that size's oldest");
        hold.put(at(1, other), Arc::new(vec![1.0]), 8, 4);
        assert!(hold.get(&at(1, small)).is_none() && hold.get(&at(3, small)).is_none(), "a third size drops the size stored longest ago");
        assert!(hold.get(&at(5, full)).is_some() && hold.get(&at(1, other)).is_some());
        assert!(hold.frames.len() <= NeighbourHold::FRAMES * NeighbourHold::SIZES);
    }
}

impl ViewerSource<'_> {
    fn held(&self, clip: ClipId) -> Option<&HeldFrame> {
        match self.primary {
            Some((id, f)) if id == clip => Some(f),
            _ => self.streams.frame(clip),
        }
    }

    /// The clip's original media file: temporal plug-ins read it at
    /// arbitrary times, as they always have.
    fn original_path(&self, clip: &Clip) -> Option<std::path::PathBuf> {
        let media = clip.media_id()?;
        let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
        pool.get(&media).map(|item| item.path.clone())
    }
}

/// `clip`'s files as the viewer decodes them, stamped now (#1863): the
/// original, and the proxy when the decoder would pick it (ready, and
/// there). The renderer's held outputs and the GPU path's lookups both
/// read them here.
pub(super) fn held_file_of(pool: &MediaPool, clip: &Clip, use_proxies: bool) -> Option<klipp_compose::HeldFile> {
    let media = clip.media_id()?;
    let item = pool.get(&media)?;
    let proxy = item.proxy_path.clone().filter(|p| use_proxies && item.proxy_ready && p.exists());
    let path = item.path.clone();
    let log_format = crate::video_decoder::log_format_for_path(&path);
    klipp_compose::HeldFile::stamp(media, path, log_format, proxy)
}

/// Whether two sizes are the same shape, to within a pixel of rounding.
pub(super) fn same_shape((aw, ah): (u32, u32), (bw, bh): (u32, u32)) -> bool {
    if aw == 0 || ah == 0 || bw == 0 || bh == 0 {
        return true;
    }
    let (aw, ah, bw, bh) = (aw as u64, ah as u64, bw as u64, bh as u64);
    // `a`'s width at `b`'s height, and the other way round.
    ((aw * bh + ah / 2) / ah).abs_diff(bw) <= 1 || ((ah * bw + aw / 2) / aw).abs_diff(bh) <= 1
}

/// The frame the viewer renders a primary picture of `picture` size in.
///
/// The picture's own size when it is the sequence's shape, so a clip is
/// shown at the resolution it was decoded at. Otherwise a frame of the
/// sequence's shape around it, which the renderer bars — as export does.
/// Rendered at the picture's size, a portrait phone clip in a landscape
/// sequence made a portrait frame: titles over it were laid out to its
/// width, and nothing said the export would differ.
pub(super) fn viewer_canvas(picture: (u32, u32), sequence: (u32, u32)) -> (u32, u32) {
    let ((pw, ph), (sw, sh)) = (picture, sequence);
    if same_shape(picture, sequence) {
        return picture;
    }
    let even = |v: u64| ((v as u32) & !1).max(2);
    let (w, h) = if (pw as u64) * (sh as u64) < (ph as u64) * (sw as u64) {
        // Narrower: the picture's height, bars at the sides.
        (even((ph as u64 * sw as u64 + sh as u64 / 2) / sh as u64), ph)
    } else {
        (pw, even((pw as u64 * sh as u64 + sw as u64 / 2) / sw as u64))
    };
    // Never beyond the sequence itself: a 4K portrait picture would
    // otherwise want a frame 6827 pixels wide.
    if w > sw || h > sh { sequence } else { (w, h) }
}

/// The viewer's frame reduced for Playback Quality: `size` divided by
/// `divisor`, kept even and the same shape, and never below [`MIN_REDUCED_HEIGHT`]
/// lines — a quarter of a 480p sequence is not a picture anyone can judge
/// motion on. Clip positions and pixel-sized effects follow the render
/// size (`klipp_compose::render::render_scale`), so the reduced frame is
/// the same picture, only coarser.
pub(super) fn reduced_for_quality(size: (u32, u32), divisor: u8) -> (u32, u32) {
    let (w, h) = size;
    if divisor <= 1 || h <= MIN_REDUCED_HEIGHT {
        return size;
    }
    let d = u32::from(divisor);
    let rh = (h / d).max(MIN_REDUCED_HEIGHT).min(h);
    let rw = ((w as u64 * rh as u64 + h as u64 / 2) / h as u64) as u32;
    ((rw & !1).max(2), (rh & !1).max(2))
}

/// Playback Quality's divisor for a frame rendered in `state`: the
/// reduction while playing, full size otherwise. A parked frame is for
/// judging and renders full size, which is why the render cache's scan
/// warms that size as well as the reduced one.
pub(super) fn quality_divisor_for(state: PlaybackState, divisor: u8) -> u8 {
    if state == PlaybackState::Playing { divisor } else { 1 }
}

/// The sizes the render cache's scan warms a frame at, in order: full,
/// which a parked frame is shown at, then Playback Quality's reduced size
/// when it differs, which playback asks for.
pub(super) fn prerender_sizes(full: (u32, u32), divisor: u8) -> Vec<(u32, u32)> {
    let reduced = reduced_for_quality(full, divisor);
    if reduced == full { vec![full] } else { vec![full, reduced] }
}

/// The fewest lines Playback Quality reduces the viewer to.
pub(super) const MIN_REDUCED_HEIGHT: u32 = 180;

fn source_frame(f: &HeldFrame) -> SourceFrame {
    SourceFrame { pixels: f.source_pixels(), width: f.width, height: f.height, pts: f.pts }
}

/// Media clips that start within [`seek_lead::MAX_LEAD_SECS`] of `time`
/// while playing, at their first frame, each with when it starts: their
/// streams open and start decoding before the playhead reaches them, each
/// as early as its own seek needs (`sync_streams`), so the cut into one
/// does not wait for a file to open and seek. A reversed clip's reader
/// too, which also decodes a window (#617). Not on `primary_track`, whose
/// next shot the primary decoder prerolls; a shot on another track coming
/// up as the primary's track runs out, behind a held still, was opened
/// cold at the cut, the picture freezing half a second (#2097).
fn upcoming(seq: &Sequence, time: klipp_core::Timecode, playing: bool, primary_track: Option<usize>) -> Vec<(Want, f64)> {
    if !playing {
        return Vec::new();
    }
    let (from, to) = (time.to_secs_f64(), time.to_secs_f64() + super::seek_lead::MAX_LEAD_SECS);
    let fps = seq.fps.as_f64();
    let mut out: Vec<(Want, f64)> = seq.tracks
        .iter()
        .enumerate()
        .filter(|(i, t)| t.kind == klipp_timeline::TrackKind::Video && t.visible && Some(*i) != primary_track)
        .flat_map(|(_, t)| t.clips.iter())
        .filter(|c| c.enabled && c.timeline_in.to_secs_f64() > from && c.timeline_in.to_secs_f64() <= to)
        .filter_map(|c| Some((Want { clip: c.id, media: c.media_id()?, rotation: c.source_rotation, source_secs: c.source_secs_at(0.0, fps), reversed: c.is_reversed(), ahead: true }, c.timeline_in.to_secs_f64() - from)))
        .collect();
    // The soonest first: with the decoders all open, the one due next.
    out.sort_by(|a, b| a.1.total_cmp(&b.1));
    out
}

/// The source times the viewer can read `clip`'s media at: from its start
/// to its duration in the pool, as export's snapshot says, so a temporal
/// node's neighbour past the media's end is out of range on both routes,
/// never a frame the viewer merely couldn't read.
pub fn viewer_media_range(pool: &MediaPool, clip: &Clip) -> Option<(f64, f64)> {
    Some((0.0, pool.get(&clip.media_id()?)?.duration.to_secs_f64()))
}

/// The viewer's key for `kind`'s analysis of `clip`'s media (M4 step 9):
/// keyed by the original file even when the viewer decodes a proxy, so the
/// viewer and export share one asset.
pub fn viewer_analysis_key(pool: &MediaPool, clip: &Clip, kind: klipp_analysis::assets::AssetKind) -> Option<klipp_analysis::assets::AssetKey> {
    let klipp_timeline::ClipKind::Media { media_id } = &clip.kind else { return None };
    klipp_analysis::assets::asset_key(kind, pool.get(media_id)?)
}

impl FrameSource for ViewerSource<'_> {
    fn media_fps(&mut self, clip: &Clip) -> Option<f64> {
        let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
        Some(pool.get(&clip.media_id()?)?.fps.as_f64())
    }

    /// The files the parked frame is drawn from, stamped now, for the
    /// renderer to hold the clip's heavy nodes' outputs by (#1863): only
    /// while the neighbour hold is on, which is parked. A clip shown from
    /// a proxy names both the proxy (its frame) and the original (the
    /// neighbours a denoiser reads), so a change to either is a new key.
    fn held_file(&mut self, clip: &Clip) -> Option<klipp_compose::HeldFile> {
        self.hold?;
        held_file_of(&self.media_pool.read().unwrap_or_else(|e| e.into_inner()), clip, self.use_proxies)
    }

    /// A still image's file, stamped now, unless the clip plays reversed
    /// (#2092): the stack keeps its picture while this stands.
    fn still_file(&mut self, clip: &Clip) -> Option<klipp_compose::HeldFile> {
        if clip.is_reversed() {
            return None;
        }
        let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
        let item = pool.get(&clip.media_id()?)?;
        if !item.is_still {
            return None;
        }
        held_file_of(&pool, clip, self.use_proxies)
    }

    fn media_range(&mut self, clip: &Clip) -> Option<(f64, f64)> {
        viewer_media_range(&self.media_pool.read().unwrap_or_else(|e| e.into_inner()), clip)
    }

    /// By the original file, proxy or not (#985 phase 3).
    fn subject_matte_dir(&mut self, clip: &Clip, model: u32) -> Option<std::path::PathBuf> {
        let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
        let item = pool.get(&clip.media_id()?)?;
        klipp_compose::subject_matte::media_dir(&item.path, klipp_compose::subject_matte::decode_rotation(clip, item.source_rotation), model)
    }

    /// By the original file, proxy or not (`viewer_analysis_key`).
    fn analysis_key(&mut self, clip: &Clip, kind: klipp_analysis::assets::AssetKind) -> Option<klipp_analysis::assets::AssetKey> {
        let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
        viewer_analysis_key(&pool, clip, kind)
    }

    fn frame(&mut self, clip: &Clip, source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        // A reversed clip is never the primary decoder's, and its frames
        // are read at the time asked for (#617).
        if clip.is_reversed() {
            return self.streams.reversed_frame(clip, source_secs, width, height);
        }
        self.held(clip.id).map(source_frame)
    }

    fn frame_after(&mut self, clip: &Clip, _source_secs: f64, _width: u32, _height: u32) -> Option<SourceFrame> {
        let next = match self.primary {
            Some((id, f)) if id == clip.id => {
                let q = self.primary_queue?.lock().unwrap_or_else(|e| e.into_inner());
                q.front()
                    .filter(|n| !n.is_eof && f.pts.is_some_and(|p| n.pts_secs() > p))
                    .map(HeldFrame::copy_of)?
            }
            _ => self.streams.next_frame(clip.id)?,
        };
        Some(source_frame(&next))
    }

    /// From the file the viewer decodes the clip from, the proxy when it
    /// shows one, read as export reads its neighbours, by a reader kept
    /// across frames (#2013).
    fn temporal_frames(&mut self, clip: &Clip) -> Option<klipp_plugin::FrameProviderFn> {
        let held = held_file_of(&self.media_pool.read().unwrap_or_else(|e| e.into_inner()), clip, self.use_proxies)?;
        let decoded = held.proxy.as_ref().map_or(&held.path, |(p, ..)| p);
        let reader = self.streams.neighbour_reader(decoded, held.log_format)?;
        let source_in = clip.source_in.to_secs_f64();
        Some(Box::new(move |t: f64| reader.lock().ok()?.decode_at((source_in + t).max(0.0))))
    }

    /// The file's frames at the render size, opened on the first frame
    /// not held (#1863): while parked, a frame read is kept, and a step
    /// that asks for it again gets it without a decode.
    fn temporal_frames_linear_at_size(&mut self, clip: &Clip, working: klipp_core::WorkingColorspace, width: u32, height: u32) -> Option<klipp_compose::LinearFrameProviderFn> {
        let Some(hold) = self.hold.cloned() else {
            return self.temporal_frames_linear(clip, working).map(|reader| klipp_compose::render::fit_linear_reader(reader, width, height));
        };
        let path = self.original_path(clip)?;
        let source_in = clip.source_in.to_secs_f64();
        let log_format = crate::video_decoder::log_format_for_path(&path);
        let file = std::fs::metadata(&path).map(|m| (m.len(), m.modified().ok())).unwrap_or((0, None));
        let mut reader: Option<Option<klipp_compose::LinearFrameProviderFn>> = None;
        Some(Box::new(move |t: f64| {
            let key = NeighbourKey { path: path.clone(), file, working, log_format, size: (width, height), micros: ((source_in + t).max(0.0) * 1e6).round() as i64 };
            if let Some(held) = hold.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
                return Some(held);
            }
            let read = reader.get_or_insert_with(|| {
                let mut file = klipp_media::frame_reader::SeekableFrameReader::open_linear(&path, log_format, working)?;
                let read: klipp_compose::LinearFrameProviderFn = Box::new(move |t: f64| file.decode_at((source_in + t).max(0.0)).map(|(px, w, h)| (std::sync::Arc::new(px), w, h)));
                Some(klipp_compose::render::fit_linear_reader(read, width, height))
            });
            let (px, w, h) = read.as_mut()?(t)?;
            hold.lock().unwrap_or_else(|e| e.into_inner()).put(key, std::sync::Arc::clone(&px), w, h);
            Some((px, w, h))
        }))
    }

    /// The frames either side that the viewer's own streams hold on the
    /// GPU (#1988): the primary's frame before (the one the last render
    /// drew) and after (its queue's front), or a clip stream's. Bound by a
    /// temporal denoise on the chain, decoded from their sRGB bytes there,
    /// instead of read from the file on the CPU and uploaded: 14 to 20 ms
    /// a 4K neighbour, two a frame. Only in the Rec.709 working space, in
    /// which a decoded frame's linear light is the working space's, and at
    /// the render size; any other time or frame is read the CPU way.
    fn temporal_frames_on_gpu(&mut self, clip: &Clip, working: klipp_core::WorkingColorspace, width: u32, height: u32) -> Option<klipp_compose::GpuFrameProviderFn> {
        if working != klipp_core::WorkingColorspace::Rec709Linear || clip.is_reversed() {
            return None;
        }
        let half_frame = 0.5 / self.media_fps(clip).filter(|f| *f > 0.0)?;
        let held: Vec<HeldFrame> = match self.primary {
            Some((id, f)) if id == clip.id => {
                let next = self.primary_queue.and_then(|q| q.lock().unwrap_or_else(|e| e.into_inner()).front().filter(|n| !n.is_eof).map(HeldFrame::copy_of));
                self.primary_previous.cloned().into_iter().chain([f.clone()]).chain(next).collect()
            }
            _ => self.streams.previous_frame(clip.id).cloned().into_iter().chain(self.streams.frame(clip.id).cloned()).chain(self.streams.next_frame(clip.id)).collect(),
        };
        let held: Vec<(f64, std::sync::Arc<dyn klipp_nodes::frame_context::GpuFramePixels>)> =
            held.iter().filter(|f| (f.width, f.height) == (width, height)).filter_map(|f| Some((f.pts?, f.gpu_neighbour()?))).collect();
        if held.is_empty() {
            return None;
        }
        let source_in = clip.source_in.to_secs_f64();
        Some(Box::new(move |t: f64| {
            let secs = (source_in + t).max(0.0);
            held.iter().find(|(pts, _)| (pts - secs).abs() < half_frame).map(|(_, g)| std::sync::Arc::clone(g))
        }))
    }

    /// The transfer the pool says the item's frames decode through, as
    /// export's snapshot takes it.
    fn media_transfer(&mut self, media: &str) -> Option<klipp_color::hdr::HdrTransfer> {
        let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
        klipp_compose::resolve::media_id(media).and_then(|id| pool.get(&id))?.scene_transfer()
    }

    /// The item's original file, as export reads it: a proxy is the
    /// clip's stand-in, and a media source is no clip.
    fn media_frame(&mut self, media: &str, secs: f64, working: klipp_core::WorkingColorspace, width: u32, height: u32) -> Result<Vec<f32>, klipp_nodes::side_picture::SideMissing> {
        use klipp_nodes::side_picture::SideMissing;
        let (path, log_format) = {
            let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
            let item = klipp_compose::resolve::media_id(media).and_then(|id| pool.get(&id)).ok_or_else(|| SideMissing::File(media.to_owned()))?;
            (item.path.clone(), item.camera_log_format())
        };
        if !path.exists() {
            return Err(SideMissing::File(path.display().to_string()));
        }
        let (pixels, w, h) = self.streams.media.read(&path, log_format, working, secs).ok_or(SideMissing::Unavailable)?;
        klipp_compose::render::fit_linear(pixels, w, h, width, height).ok_or(SideMissing::Unavailable)
    }

    /// The same frames in linear light, for the renderer's own use.
    ///
    /// The log format comes from the same registry the viewer's own
    /// decode consults, so a neighbour is rendered exactly as the
    /// frame beside it was.
    fn temporal_frames_linear(
        &mut self,
        clip: &Clip,
        working: klipp_core::WorkingColorspace,
    ) -> Option<klipp_compose::LinearFrameProviderFn> {
        let path = self.original_path(clip)?;
        let source_in = clip.source_in.to_secs_f64();
        let log_format = crate::video_decoder::log_format_for_path(&path);
        let mut reader = klipp_media::frame_reader::SeekableFrameReader::open_linear(
            &path, log_format, working,
        )?;
        Some(Box::new(move |t: f64| reader.decode_at((source_in + t).max(0.0)).map(|(px, w, h)| (std::sync::Arc::new(px), w, h))))
    }
}

/// A rendered viewer frame.
struct Rendered {
    picture: Picture,
    width: u32,
    height: u32,
    stats: Option<RenderStats>,
}

/// What there is to show.
enum Picture {
    /// The renderer's own output: linear light, working space. Everything
    /// that measures the picture — the scopes, the chromaticity samples, the
    /// HDR nit map — reads this, and the display transform runs over it.
    Linear(Vec<f32>),
    /// Display bytes, finished. A frame from the render cache, which holds
    /// frames 8-bit, taken through the display transform as a per-channel
    /// table (see [`PlaybackEngine::cached_frame_display_lut`]) instead of
    /// expanding to 132 MB of float at 4K to arrive at the same bytes.
    Display(Vec<u8>),
}

impl PlaybackEngine {
    /// The neighbour hold a render reads (#1863): the engine's while
    /// parked; none while playing, and emptied, so a long play does not
    /// keep a parked frame's neighbours.
    fn neighbour_hold_for(hold: &std::sync::Arc<std::sync::Mutex<NeighbourHold>>, state: PlaybackState, on: bool) -> Option<&std::sync::Arc<std::sync::Mutex<NeighbourHold>>> {
        if state == PlaybackState::Playing || !on {
            hold.lock().unwrap_or_else(|e| e.into_inner()).clear();
            None
        } else {
            Some(hold)
        }
    }

    /// Empty the renderer's held outputs (#1863) when what they were held
    /// for has gone: playback started or stopped, or the parked frame is
    /// now another clip's (another clip, another sequence). The key keeps
    /// a stale entry from being used; this keeps it from taking memory.
    fn scope_held_outputs(&mut self, primary: Option<ClipId>) {
        // Held outputs' GPU copies are GPU memory (#2266): a quarter of
        // what is free there, no more than the default, when it can be read.
        if let Some(free) = crate::decode_budget::free_vram() {
            self.renderer.executor().set_held_gpu_budget(((free * 0.25) as usize).min(klipp_nodes::held::DEFAULT_BUDGET));
        }
        let scope = (self.state, primary);
        if self.held_scope != Some(scope) || self.state == PlaybackState::Playing || !self.hold_neighbours {
            self.renderer.executor().clear_held();
            self.held_scope = Some(scope);
        }
    }

    /// The clip the primary decoder plays.
    pub(super) fn v1_clip_id(&self, seq: &Sequence) -> Option<ClipId> {
        seq.tracks.get(self.current_track_index)?.clips.get(self.current_clip_index).map(|c| c.id)
    }

    /// Bring the clip streams in step with the playhead: one decoder for
    /// every media clip the frame needs besides the primary decoder's.
    pub(super) fn sync_streams(&mut self) {
        let time = self.playhead;
        let playing = self.state == PlaybackState::Playing;
        let (mut wants, ahead): (Vec<Want>, Vec<(Want, f64)>) = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            let primary = if self.v1_decoder.is_some() { self.v1_clip_id(&seq) } else { None };
            // A layer's clips are read at the layer's own time and rate:
            // inside a precomposition those are the scene's, not ours.
            let on_screen = klipp_compose::active_layers(&seq, time)
                .iter()
                .flat_map(|l| l.clips.iter().map(move |c| (c, l.at, l.fps)))
                .filter(|(c, _, _)| Some(c.id) != primary)
                .filter_map(|(c, at, fps)| {
                    Some(Want {
                        clip: c.id,
                        media: c.media_id()?,
                        rotation: c.source_rotation,
                        source_secs: klipp_compose::render::source_secs(c, at, fps),
                        reversed: c.is_reversed(),
                        ahead: false,
                    })
                })
                .collect();
            let primary_track = self.v1_decoder.is_some().then_some(self.current_track_index);
            (on_screen, upcoming(&seq, time, playing, primary_track))
        };
        // Clips on screen with no stream yet: opened late, if playing.
        let late: Vec<ClipId> = if playing { wants.iter().filter(|w| !self.streams.has(w.clip)).map(|w| w.clip).collect() } else { Vec::new() };
        // Each coming clip as early as its own seek needs (#2108): one
        // already open is kept; one due within its lead is opened.
        for (w, due_in) in ahead {
            let open = self.streams.has(w.clip);
            let lead = match (open, self.path_for_media(w.media)) {
                (true, _) | (false, None) => super::seek_lead::MAX_LEAD_SECS,
                (false, Some(path)) => {
                    let fps = self.media_pool.read().unwrap_or_else(|e| e.into_inner()).get(&w.media).map_or(30.0, |m| m.fps.as_f64());
                    // Earlier on a slow drive, so its first frames are read
                    // in time (#2128).
                    let slow = crate::disk_budget::drive_of(&path).kind.extra_lead_secs();
                    (self.seek_leads.lead(&path, w.source_secs, fps) + slow).min(super::seek_lead::MAX_LEAD_SECS)
                }
            };
            if due_in <= lead {
                wants.push(w);
            }
        }
        // Files are resolved here, for new streams only: resolution logs.
        let paths: HashMap<ClipId, (std::path::PathBuf, i32, super::streams::Cost)> = wants
            .iter()
            .filter(|w| !self.streams.has(w.clip))
            .filter_map(|w| {
                let path = self.path_for_media(w.media)?;
                let cost = self.media_cost(w.media, &path);
                let rot = self.effective_decode_rotation(w.media, &path, w.rotation);
                Some((w.clip, (path, rot, cost)))
            })
            .collect();
        let frame_secs = self.fps.frame_duration_secs();
        self.streams.sync(&wants, frame_secs, &|w: &Want| paths.get(&w.clip).cloned());
        let opened_late = late.iter().filter(|c| self.streams.has(**c)).count() as u64;
        super::STREAMS_OPENED_ON_SCREEN.fetch_add(opened_late, std::sync::atomic::Ordering::Relaxed);
    }

    /// Wait, up to `timeout`, for every clip stream to hold its frame for
    /// the playhead: a paused frame must show what is there, not what was
    /// there before a seek. Left waiting, the paused tick re-renders once
    /// they arrive.
    fn wait_for_streams(&mut self, timeout: Duration) {
        let frame_secs = self.fps.frame_duration_secs();
        let deadline = Instant::now() + timeout;
        while !self.streams.ready(frame_secs) {
            if Instant::now() >= deadline {
                self.pending_stream_refresh = true;
                self.note_view(format!("streams not ready in time at {:?}: refresh pending", self.playhead.value));
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
            self.streams.pump(frame_secs);
        }
    }

    /// Whether the frame being published shows every clip at the playhead,
    /// which is what a reader of the viewer waits for (the control
    /// socket's frame reads, the parity tests):
    ///
    /// - not a paused frame still waiting on the other clips' streams;
    /// - not a progress frame, the one short of the playhead shown while a
    ///   slow seek decodes, published under the playhead only for want of
    ///   its own time;
    /// - while playing, only when every stream held its frame for this
    ///   time. Playback does not wait for them, so a slow stream's clip is
    ///   drawn from the frame before. That frame was published as
    ///   complete, and after a pause it stayed on screen until the parked
    ///   refresh replaced it, so a read taken in between got a dissolve
    ///   with its other side a frame or more behind.
    pub(super) fn published_complete(&self) -> bool {
        !self.pending_stream_refresh
            && !self.presenting_progress
            && (self.state != PlaybackState::Playing || self.streams.ready(self.fps.frame_duration_secs()))
    }

    /// Re-render once the clip streams a paused frame was waiting on have
    /// their frames. Called every paused tick; nothing to do otherwise.
    pub(super) fn tick_stream_refresh(&mut self) {
        if !self.pending_stream_refresh {
            return;
        }
        let frame_secs = self.fps.frame_duration_secs();
        self.streams.pump(frame_secs);
        if self.streams.ready(frame_secs) {
            self.pending_stream_refresh = false;
            self.note_view(format!("streams ready: regrade {:?} (paused refresh pending {})", self.playhead.value, self.pending_paused_refresh));
            self.regrade_cached_frame();
        }
    }

    /// What the grade tools want from this frame. They follow the clip
    /// the grade panel is on when it is on screen, else the primary clip.
    fn inspect_request(&self, seq: &Sequence, primary: Option<ClipId>) -> Inspect {
        let find = |id: ClipId| seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == id);
        // The timeline grade is inspected by its stand-in's id, which no
        // track holds: its render grades the composite under that id.
        let timeline_grade = |id: ClipId| id == seq.timeline_grade_clip_id() && seq.timeline_grade().is_some();
        let clip = self.inspect_clip.filter(|id| find(*id).is_some() || timeline_grade(*id)).or(primary);
        let matte_view = if self.matte_preview {
            self.matte_preview_node.or_else(|| {
                let graph = find(clip?)?.grade();
                graph
                    .nodes
                    .iter()
                    .find(|n| matches!(n.kind, klipp_nodes::NodeKind::Qualifier | klipp_nodes::NodeKind::Mask))
                    .map(|n| n.id)
            })
        } else {
            None
        };
        // Thumbnails cost a readback per node and keep every node they
        // show off the GPU chain — twice the frame's cost at 4K. Paused
        // frames only, never the speculative pre-render, and not while a
        // slider is held: the drag's frames are for the viewer, and the
        // regrade on release brings the thumbnails up to date.
        let thumbs = (self.state != PlaybackState::Playing
            && !self.visible_node_thumbs.is_empty()
            && !self.thumb_capture_suspended
            && !self.in_node_param_edit)
            .then(|| self.visible_node_thumbs.clone());
        // The matte view's node is the clip's own grade's (M3 step 16a).
        let at = |node: klipp_nodes::NodeId| clip.map(|c| klipp_compose::inspect::grade_node(c, node));
        Inspect {
            clip,
            thumbs,
            // An eyedropper read borrows the tap for one render.
            scope_tap: self.node_sample.as_ref().map(|s| s.tap.clone()),
            matte_view: matte_view.and_then(at),
            solo: self.solo.clone(),
            ungraded: self.bypass_color_grade,
            matte_of: self.inspector_matte_preview_target,
            only: self.measure_only_clip,
        }
    }

    /// Give the renderer the timeline's global LUT when it has changed.
    fn push_global_lut(&mut self) {
        let p = &self.color_pipeline;
        let sig = (
            p.enabled,
            p.global_lut_path.clone(),
            p.global_lut_opacity.to_bits(),
            p.global_lut.as_ref().map(|l| (l.size, l.data.len())),
        );
        if self.pushed_lut_sig.as_ref() == Some(&sig) {
            return;
        }
        let lut = match &p.global_lut {
            Some(lut) if p.applies() => Some(klipp_compose::GlobalLut { lut: lut.clone(), opacity: p.global_lut_opacity }),
            _ => None,
        };
        self.renderer.set_global_lut(lut);
        self.pushed_lut_sig = Some(sig);
    }

    /// The render cache's key for this frame, when the frame is one it can
    /// hold: the primary clip alone on screen, graded, and nothing on that
    /// the cache does not key (a grade tool's view, a split, a drag).
    /// `size` is the frame's rendered size, part of the key: a half-size
    /// frame and a full-size one of the same clip and grade are different
    /// pictures, and the disk cache is looked up by name alone, so without
    /// it a frame cached at one Playback Quality stopped the other ever
    /// being rendered ahead. It is its own field, not folded into the
    /// grade hash, so the timeline's cache bar still counts a clip's
    /// frames by grade.
    fn render_cache_key(&mut self, seq: &Sequence, primary: Option<ClipId>, size: (u32, u32)) -> Option<crate::render_cache::RenderCacheKey> {
        if self.in_node_param_edit
            || self.solo.is_some()
            || self.matte_preview
            // The cache holds display bytes of the SDR picture; HDR
            // monitoring shows another picture entirely.
            || self.hdr_monitor.renders_hdr()
            || self.bypass_color_grade
            || self.compare_stabilization
            || self.compare_effect_active
            || self.node_sample.is_some()
            || (self.state != PlaybackState::Playing && !self.visible_node_thumbs.is_empty())
        {
            return None;
        }
        let layers = klipp_compose::active_layers(seq, self.playhead);
        let [layer] = layers.as_slice() else { return None };
        let [clip] = layer.clips.as_slice() else { return None };
        if layer.transition || Some(clip.id) != primary {
            return None;
        }
        if !clip.frames_cacheable() {
            return None;
        }
        let track_opacity = seq.tracks.get(layer.track).map(|t| t.opacity).unwrap_or(1.0);
        let grade_hash = self.render_key_hash(seq, clip, track_opacity);
        Some(crate::render_cache::RenderCacheKey { clip_id: clip.id, frame_number: self.playhead.value, grade_hash, size })
    }

    /// The render cache's hash for a clip's frames: the clip, the global
    /// LUT, its track's opacity, and the render environment (working
    /// space, LUT interpolation, proxy decode, the media's log format).
    pub(super) fn render_key_hash(&mut self, seq: &klipp_timeline::Sequence, clip: &Clip, track_opacity: f32) -> u64 {
        let environment = crate::render_cache::render_environment_hash(
            self.color_pipeline.use_tetrahedral,
            crate::video_decoder::working_colorspace(),
            self.use_proxies,
            self.current_file.as_deref().and_then(crate::video_decoder::registered_log_format),
        );
        self.clip_render_hash(clip) ^ self.lut_hash() ^ self.timeline_grade_hash(seq) ^ (track_opacity.to_bits() as u64).rotate_left(17) ^ environment.rotate_left(29)
    }

    /// The timeline grade, for the cache key: it runs over every frame,
    /// so a change to it is a change to every cached frame. Hashed as the
    /// clip it renders as, so it is cached and cleared with the others.
    fn timeline_grade_hash(&mut self, seq: &klipp_timeline::Sequence) -> u64 {
        match Clip::timeline_grade_of(seq) {
            Some(tg) => self.clip_render_hash(&tg).rotate_left(41),
            None => 0,
        }
    }

    /// A hash of everything about a clip that its rendered frames depend
    /// on, kept until the sequence changes. The same in every session for
    /// the same clip, whatever order its maps were built in, so the disk
    /// cache is found again after a restart (#1132).
    fn clip_render_hash(&mut self, clip: &Clip) -> u64 {
        if self.current_grade_hash == 0 {
            // The sequence or a grade changed since the hashes were made.
            self.render_hashes.clear();
            self.current_grade_hash = 1;
        }
        *self.render_hashes.entry(clip.id).or_insert_with(|| clip_render_hash_of(clip))
    }

    fn lut_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let p = &self.color_pipeline;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        p.applies().hash(&mut h);
        p.global_lut_path.hash(&mut h);
        p.global_lut_opacity.to_bits().hash(&mut h);
        h.finish()
    }

    /// Render the timeline at the playhead. `primary` is the primary
    /// decoder's frame, when there is one; `width` × `height` the size to
    /// render at. A colour-grade drag renders at half size when enabled.
    /// One line in the log when the composite path changes, so a slow
    /// frame says which path it took.
    fn note_composite_path(&mut self, path: &'static str) {
        if self.last_composite_path != Some(path) {
            tracing::debug!("composite path: {path}");
            self.last_composite_path = Some(path);
        }
    }

    /// `render_timeline`'s GPU twin: display bytes straight off the GPU
    /// stack, or `None` when the stack cannot take this frame.
    fn render_composite_on_gpu(
        &mut self,
        seq: &Sequence,
        primary: Option<(ClipId, &HeldFrame)>,
        width: u32,
        height: u32,
        out: klipp_compose::DisplayDelivery<'_>,
    ) -> Option<(Option<Vec<u8>>, klipp_compose::RenderStats)> {
        self.renderer.set_working_colorspace(crate::video_decoder::working_colorspace());
        self.renderer.set_grade_encoding(klipp_core::grade_encoding());
        self.renderer.set_lut_tetrahedral(self.color_pipeline.use_tetrahedral);
        self.push_global_lut();
        self.scope_held_outputs(primary.map(|(id, _)| id));
        let time = self.playhead;
        let previous = self.previous_primary.take();
        let mut src = ViewerSource {
            primary,
            primary_queue: self.v1_decoder.as_ref().map(|d| d.frame_queue()),
            streams: &mut self.streams,
            media_pool: &self.media_pool,
            hold: Self::neighbour_hold_for(&self.neighbour_hold, self.state, self.hold_neighbours),
            use_proxies: self.use_proxies,
            primary_previous: previous.as_ref().filter(|(id, _)| primary.is_some_and(|(p, _)| p == *id)).map(|(_, f)| f),
        };
        let opts = RenderOptions {
            hdr_peak_nits: self
                .hdr_monitor
                .renders_hdr()
                .then_some(klipp_color::hdr::DEFAULT_MASTERING_MAX_LUMA_CD_M2),
            ..Default::default()
        };
        let shown = self.renderer.render_display_on_gpu(seq, time, width, height, &mut src, opts, out);
        // Declined, the frame is drawn by `render_timeline` next, which
        // needs the frame before still.
        self.previous_primary = if shown.is_some() { primary.map(|(id, f)| (id, f.clone())) } else { previous };
        shown
    }

    fn render_timeline(
        &mut self,
        seq: &Sequence,
        primary: Option<(ClipId, &HeldFrame)>,
        width: u32,
        height: u32,
        inspect: Inspect,
    ) -> (Vec<f32>, Inspection) {
        self.renderer.set_working_colorspace(crate::video_decoder::working_colorspace());
        self.renderer.set_grade_encoding(klipp_core::grade_encoding());
        // The user's LUT interpolation preference, for the global LUT and
        // every clip's LUT node. An export always asks for tetrahedral.
        self.renderer.set_lut_tetrahedral(self.color_pipeline.use_tetrahedral);
        self.push_global_lut();
        let downsample = self.in_node_param_edit
            && super::drag_downsample_enabled()
            && !self.force_full_res_regrade
            && !self.bypass_color_grade
            && !self.compare_stabilization
            && !self.compare_effect_active
            && width >= 480
            && height >= 480
            && width.is_multiple_of(2)
            && height.is_multiple_of(2);
        if self.in_node_param_edit {
            self.drag_grade_count = self.drag_grade_count.saturating_add(1);
        }
        let (w, h) = if downsample { (width / 2, height / 2) } else { (width, height) };
        self.scope_held_outputs(primary.map(|(id, _)| id));
        let time = self.playhead;
        let previous = self.previous_primary.take();
        let mut src = ViewerSource {
            primary,
            primary_queue: self.v1_decoder.as_ref().map(|d| d.frame_queue()),
            streams: &mut self.streams,
            media_pool: &self.media_pool,
            hold: Self::neighbour_hold_for(&self.neighbour_hold, self.state, self.hold_neighbours),
            use_proxies: self.use_proxies,
            primary_previous: previous.as_ref().filter(|(id, _)| primary.is_some_and(|(p, _)| p == *id)).map(|(_, f)| f),
        };
        // HDR monitoring renders what the export writes: display light,
        // 1.0 = 203 cd/m², mastered to the delivery's peak.
        let opts = RenderOptions {
            hdr_peak_nits: self
                .hdr_monitor
                .renders_hdr()
                .then_some(klipp_color::hdr::DEFAULT_MASTERING_MAX_LUMA_CD_M2),
            ..Default::default()
        };
        let (out, found) = self.renderer.render_inspected(seq, time, w, h, &mut src, opts, inspect);
        self.previous_primary = primary.map(|(id, f)| (id, f.clone()));
        if downsample {
            self.drag_downsampled_count = self.drag_downsampled_count.saturating_add(1);
            (crate::grade_resample::upsample_linear_2x(&out, w, h), found)
        } else {
            (out, found)
        }
    }

    /// Render the timeline at the playhead and show it. `primary` is the
    /// primary decoder's frame (live or from the pre-grade cache), `None`
    /// when no media clip is under the primary decoder.
    pub(super) fn present_frame(&mut self, primary: Option<HeldFrame>) {
        self.present_frame_with(primary, false);
    }

    /// [`Self::present_frame`], and with `stack_only` the frame is shown
    /// by the GPU stack or not at all: `false` means the stack declined
    /// and nothing was published, so the caller's fast paths take the
    /// frame. The playing loop asks this first for a frame the fast
    /// paths could show, because the stack presents it without the
    /// readback and the UI upload they pay.
    pub(super) fn present_frame_with(&mut self, primary: Option<HeldFrame>, stack_only: bool) -> bool {
        if let Some(p) = primary.as_ref() {
            self.retain_primary(p);
        }
        let _residency = klipp_effects::residency::frame("viewer", self.playhead.to_secs_f64());
        let t_frame = Instant::now();
        // Each phase's time, for the frame's perf sample (#1850).
        let ms = |t: Instant| (t.elapsed().as_secs_f64() * 1000.0) as f32;
        self.frame_phases = crate::perf::FramePhases::default();
        self.sync_streams();
        if self.state != PlaybackState::Playing {
            // While scrubbing, show the frame at once and fill in the other
            // clips when they arrive; waiting on each would drag the scrub.
            let wait = if self.is_scrubbing { Duration::ZERO } else { Duration::from_millis(150) };
            self.wait_for_streams(wait);
        }
        self.frame_phases.sync_ms = ms(t_frame);
        let t = Instant::now();
        let (snapshot, revision) = self.sequence.snapshot_with_revision();
        self.frame_phases.snapshot_ms = ms(t);
        let primary_clip = primary.as_ref().and(self.v1_clip_id(&snapshot));
        // A still's one frame, shown wherever it is first (a parked seek
        // takes it before playback does), is kept to show again while its
        // clip plays on (#2097).
        if let Some((id, p)) = primary_clip.zip(primary.as_ref()).filter(|_| self.primary_is_still()) {
            self.held_tail = Some((id, p.clone(), self.playhead.value));
        }
        let t = Instant::now();
        let seq = klipp_compose::frame_snapshot(&snapshot, self.playhead);
        self.frame_phases.frame_snapshot_ms = ms(t);
        let seq_size = (seq.resolution.width, seq.resolution.height);
        let (w, h) = match &primary {
            Some(p) => viewer_canvas((p.width, p.height), seq_size),
            None => seq_size,
        };
        // Playback Quality applies while playing: a parked frame is for
        // judging, and renders full size.
        let (w, h) = reduced_for_quality((w, h), quality_divisor_for(self.state, crate::video_decoder::playback_quality_divisor()));
        if w == 0 || h == 0 {
            return false;
        }

        // The ungraded frame, for compare modes, scrubbing back and the UI's
        // scope preview during a slider drag. A regrade of the parked frame
        // has it already: remaking it cost an HDR frame a CPU decode, tone
        // map and encode — 20 ms at 4K — per slider step.
        let parked = primary
            .as_ref()
            .zip(self.cached_pregrade_frame.as_ref())
            .is_some_and(|(p, (c, cw, ch))| (*cw, *ch) == (p.width, p.height) && p.is_pregrade(c));
        // A progress frame — one short of the playhead, shown while a
        // slow seek is still decoding — is not this playhead's frame and
        // is kept out of both caches, or a slider drag would regrade it
        // and the render cache would serve it as the playhead's.
        let progress = self.presenting_progress;
        if let Some(p) = primary.as_ref().filter(|_| !parked && !progress && self.pregrade_encode_needed()) {
            let pregrade = p.pregrade();
            // At the picture's own size, which the frame around it may
            // not be (see `viewer_canvas`).
            let (pw, ph) = (p.width, p.height);
            self.cached_pregrade_frame = Some((pregrade.clone(), pw, ph));
            self.cached_pregrade_at = Some(self.playhead);
            let published = std::sync::Arc::new(pregrade.clone());
            self.frame_cache.insert(self.playhead.value, primary_clip, pregrade, pw, ph);
            // The frame, not its picture: only the UI's scope preview reads
            // this, and only while a grade is being dragged. Encoding it here
            // cost an HDR 4K frame 20 to 30 ms on every paused frame, for
            // bytes that were usually dropped unread.
            //
            // Read at the frame buffer's size, so only when the picture
            // fills the frame.
            self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner()).pregrade =
                ((pw, ph) == (w, h)).then_some(published);
        }

        let cache_key = if progress || stack_only || self.measure_only_clip.is_some() { None } else { self.render_cache_key(&seq, primary_clip, (w, h)) };
        if let Some(key) = &cache_key {
            // The cached frame is 8-bit, so the display transform over it is
            // a table: no expansion to float on the way to the same bytes.
            let lut = self.cached_frame_display_lut();
            // Read it where it lies. A 4K frame is 33 MB, and this path used
            // to copy it twice — once out of the cache, once back in to say
            // it had been used — before doing any work on it.
            let from_hot = {
                let hot = self
                    .render_cache_hot
                    .get(key)
                    .filter(|(_, cw, ch)| (*cw, *ch) == (w, h))
                    .map(|(data, _, _)| data);
                hot.map(|data| match lut {
                    Some(lut) => Picture::Display(super::map_u8_rgb(data, &lut)),
                    None => Picture::Linear(super::u8_rgba_to_linear(data)),
                })
            };
            if let Some(picture) = from_hot {
                self.render_cache_hot.touch(key);
                self.show_rendered(Rendered { picture, width: w, height: h, stats: None }, t_frame);
                return true;
            }
            let from_disk =
                self.render_cache_disk.get(key).filter(|(_, cw, ch)| (*cw, *ch) == (w, h));
            if let Some((bytes, _, _)) = from_disk {
                // Straight into the hot cache, then read back out of it, so
                // the frame is moved rather than copied.
                self.render_cache_hot.insert(key.clone(), bytes, w, h);
                let picture = self
                    .render_cache_hot
                    .get(key)
                    .map(|(data, _, _)| match lut {
                        Some(lut) => Picture::Display(super::map_u8_rgb(data, &lut)),
                        None => Picture::Linear(super::u8_rgba_to_linear(data)),
                    })
                    .expect("just inserted");
                self.show_rendered(Rendered { picture, width: w, height: h, stats: None }, t_frame);
                return true;
            }
        }

        // A frame the renderer would only pass through — one ungraded clip,
        // no effect, no overlay, SDR in Rec.709 — is shown as the sRGB bytes
        // the decoder made, which the pre-grade capture above already
        // read back. Rendering it expanded those bytes to linear floats,
        // composited them and encoded them back: 17 ms of a 24 ms 1080p
        // present, for the same bytes. Everything the blockers name —
        // a grade, an effect, a LUT, a tone map, a scope tap, node
        // thumbnails — still takes the full path.
        if let Some(p) = primary.as_ref().filter(|_| !stack_only && self.measure_only_clip.is_none()) {
            let mut blockers = self.fast_path_blockers_for(super::FastPath::PausedPresent);
            blockers.extend(self.picture_shape_blocker((p.width, p.height)));
            if !blockers.is_empty() {
                tracing::debug!("paused present: full path because {}", blockers.join(", "));
            }
            if blockers.is_empty() {
                if let super::PregradePixels::Srgb8(bytes) = &p.pregrade().pixels {
                    if bytes.len() == (w as usize) * (h as usize) * 4 {
                        let mut display = bytes.as_ref().clone();
                        self.overlay_titles_onto(&mut display, w, h);
                        let picture = Picture::Display(display);
                        self.show_rendered(Rendered { picture, width: w, height: h, stats: None }, t_frame);
                        return true;
                    }
                }
            } else if !progress
                && super::PlaybackEngine::only_the_graded_path_handles(&super::PlaybackEngine::held_may_lift(&blockers))
                && self.regrade_parked_on_gpu_for(super::FastPath::PausedPresent)
            {
                // Only the clip's own graph stands between the decoded
                // frame and the screen: graded on the GPU from the
                // pre-grade captured above, the way a slider step is.
                return true;
            }
        }

        let inspect = self.inspect_request(&seq, primary_clip);
        let inspected = inspect.clip;
        let primary_ref = primary_clip.zip(primary.as_ref());

        // The GPU stack: the composite and the display encode on the
        // GPU, into the viewer's texture when the UI shares the device.
        // Bytes come back only for a reader: the UI's paused-frame
        // consumers (reference frame, eyedroppers, histogram) or a viewer
        // without the texture. A scope pass that is due measures the
        // frame where the encode left it instead of reading 33 MB back
        // (#333). The stack declines what it cannot do yet and the full
        // path below takes the frame.
        if !progress && inspect_is_plain(&inspect) && self.gpu_composite_allowed() {
            let target = self.gpu_viewer_target(w, h);
            let want_bytes = target.is_none() || self.state != PlaybackState::Playing;
            let out = klipp_compose::DisplayDelivery { texture: target.as_deref(), bytes: want_bytes };
            let t = Instant::now();
            let stacked = self.render_composite_on_gpu(&seq, primary_ref, w, h, out);
            self.frame_phases.render_ms = ms(t);
            if let Some((bytes, stats)) = stacked {
                // The nodes the stack didn't draw as asked, named against
                // this frame as the timeline path names its own (step 10).
                let declines: Vec<klipp_nodes::Decline> = self.renderer.frame_declines().iter().filter(|d| d.shown()).cloned().collect();
                match bytes {
                    Some(bytes) => {
                        self.note_composite_path(if target.is_some() { "GPU stack, bytes for a reader" } else { "GPU stack" });
                        self.show_rendered(
                            Rendered { picture: Picture::Display(bytes), width: w, height: h, stats: Some(stats) },
                            t_frame,
                        );
                    }
                    None => {
                        self.note_composite_path("GPU stack, zero-copy");
                        self.draw_due_scopes_from_gpu_display(w, h);
                        self.show_on_gpu(w, h, stats, t_frame);
                    }
                }
                if !declines.is_empty() {
                    self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner()).set_declines(declines);
                }
                return true;
            }
            if stack_only {
                return false;
            }
            self.note_composite_path("CPU (the stack declined, see the renderer's reason)");
        } else if stack_only {
            return false;
        } else if !progress {
            self.note_composite_path(if inspect_is_plain(&inspect) { "CPU (display blockers)" } else { "CPU (inspection)" });
        }

        let thumbs_requested = inspect.thumbs.is_some() && !progress;
        let t = Instant::now();
        let (linear, mut found) = self.render_timeline(&seq, primary_ref, w, h, inspect);
        // After a declined stack, the timeline render's time with it.
        self.frame_phases.render_ms += ms(t);
        if thumbs_requested {
            let batch = crate::node_thumbnails::ThumbnailBatch::new(
                self.thumbnail_request, crate::node_thumbnails::frame_index(self.playhead, snapshot.fps), crate::node_thumbnails::PreviewQuality::Viewer,
                revision, &self.thumbnail_priority, std::mem::take(&mut found.thumbs),
            )
            .with_unavailable(&self.thumbnail_priority, std::mem::take(&mut found.unavailable));
            self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner()).publish_node_thumbnails(batch);
        }
        // A frame a node didn't draw as asked isn't kept: shown again from
        // the cache it would carry no decline, and no chip (M4 step 10).
        let cached = cache_key
            .filter(|_| self.state != PlaybackState::Playing)
            .filter(|_| !found.declines.iter().any(|d| d.fails()))
            .map(|key| (key, super::rendered_frame_to_cache_u8(&linear)));

        self.show_split_views(&seq, primary_ref, w, h, inspected, found.ungraded.as_deref());
        self.show_inspection(&found, w, h, progress);
        self.show_rendered(
            Rendered { picture: Picture::Linear(linear), width: w, height: h, stats: Some(found.stats) },
            t_frame,
        );
        // Named against this frame: the next one, from any path, has none.
        // A node switched off is as asked, and not named.
        let declines: Vec<klipp_nodes::Decline> = found.declines.iter().filter(|d| d.shown()).cloned().collect();
        if !declines.is_empty() {
            self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner()).set_declines(declines);
        }

        // Cached once the frame is on screen: compressing and writing a 4K
        // frame (~15 ms) is not something the viewer should wait for.
        if let Some((key, bytes)) = cached {
            self.render_cache_disk.put(&key, &bytes, w, h);
            self.render_cache_hot.insert(key, bytes, w, h);
        }
        true
    }

    /// The split views' left halves: this frame with stabilisation off, or
    /// with the compared effect toggled, and the ungraded "before".
    fn show_split_views(
        &mut self,
        seq: &Sequence,
        primary: Option<(ClipId, &HeldFrame)>,
        w: u32,
        h: u32,
        inspected: Option<ClipId>,
        ungraded: Option<&[f32]>,
    ) {
        let alt_render = |this: &mut Self, edit: &dyn Fn(&mut Clip) -> bool, clip: Option<ClipId>| -> Vec<u8> {
            let Some(id) = clip else { return Vec::new() };
            let mut alt = seq.clone();
            let changed = alt.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).filter(|c| c.id == id).any(edit);
            if !changed {
                return Vec::new();
            }
            let (linear, _) = this.render_timeline(&alt, primary, w, h, Inspect::default());
            this.display_encode(&linear)
        };

        let unstab = if self.compare_stabilization {
            alt_render(self, &disable_stabilization, inspected)
        } else {
            Vec::new()
        };

        let compare = if self.compare_effect_active {
            let node = self.compare_effect_node;
            alt_render(self, &|c: &mut Clip| node.is_some_and(|n| c.toggle_effect_node(n)), self.compare_effect_clip_id)
        } else {
            Vec::new()
        };

        let original = ungraded.map(|u| self.display_encode(u)).unwrap_or_default();
        let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
        fb.unstab_data = unstab;
        fb.compare_effect_data = compare;
        fb.original_data = original;
    }

    /// Hand the grade tools what the render found. A `progress` frame,
    /// one short of the playhead while a seek still decodes, is not the
    /// parked frame: a pick waits past it for the frame itself, as node
    /// thumbnails do.
    fn show_inspection(&mut self, found: &Inspection, w: u32, h: u32, progress: bool) {
        // An eyedropper read: answered here, from the tap it set.
        let borrowed = self.node_sample.is_some();
        match (self.node_sample.clone(), &found.scope_tap) {
            (Some(s), Some(tap)) if !progress && tap.len() == (w as usize) * (h as usize) * 4 => {
                self.node_sample = None;
                let rgb = patch_mean(tap, w, h, s.nx, s.ny);
                let _ = self.event_tx.send(AppEvent::NodeInputSampled { clip_id: s.clip, node_id: s.node, nx: s.nx, ny: s.ny, sample: Ok(rgb) });
            }
            _ => {}
        }
        // A tap borrowed for the read is not the scopes' to show,
        // answered or not.
        let scope_tap = found.scope_tap.as_ref().filter(|_| !borrowed);
        if let Some(tap) = scope_tap {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.scope_data = klipp_effects::linear_f32_rgba_to_srgb_u8(tap);
        }
        if let (Some(mask), Some(consumer)) = (&found.matte_mask, self.inspector_matte_preview_target) {
            let _ = self.event_tx.send(matte_preview_event(consumer, mask, w, h));
        }
    }

    /// The picture for the screen, and the light it carries.
    ///
    /// Off HDR monitoring this is the display encode of a display-referred
    /// frame, as it has always been. On, the frame is HDR display light:
    /// the screen gets either its SDR preview or the false colours, and
    /// the light itself goes out as a map for the readout
    /// (`klipp_color::hdr_view`).
    /// The display transform for a frame that is already 8-bit, as a
    /// per-channel table — or `None` when this tick cannot use one.
    ///
    /// The render cache holds frames sRGB-encoded, so a hit used to expand
    /// them back to linear light (132 MB at 4K, and ~34 ms of it) purely to
    /// run a transform that then quantises straight back to 8 bit. With 256
    /// possible values per channel the whole chain is a table, and
    /// `display::the_u8_table_is_the_float_path` pins it byte for byte.
    ///
    /// Three things have to hold, and monitoring can change between ticks:
    ///
    /// * the transform is per-channel — a BT.2020 or ACEScg project mixes
    ///   channels on the way to the display and cannot be a table;
    /// * HDR monitoring is off, since its readout needs the picture's light
    ///   and a cached frame has been clamped to `[0, 1]` besides.
    ///
    /// The scopes are no reason to leave the table: they measure the bytes
    /// it makes, which are the bytes the float path makes.
    fn cached_frame_display_lut(&self) -> Option<[u8; 256]> {
        if self.hdr_monitor.renders_hdr() {
            return None;
        }
        klipp_color::display::display_transform_u8_lut_to(
            self.tone_map_op,
            crate::video_decoder::working_colorspace(),
            self.sdr_transfer,
        )
    }

    /// Hand the HDR picture to `klipp-hdr-out`, which shows it on a
    /// surface that can carry it.
    ///
    /// HDR display light, exactly as the export writes it — 1.0 = 203
    /// cd/m² — so the reference output and the delivered file are the same
    /// numbers. No conversion happens here and none should: the presenter
    /// puts these values on a linear surface unchanged.
    ///
    /// Only when the render is *producing* display light, which is what
    /// HDR monitoring switches on. Publishing the SDR picture instead
    /// would give a reference monitor something that looks like a picture
    /// and is not the one being delivered — worse than publishing nothing.
    ///
    /// The writer is created on the first frame and re-created when the
    /// geometry changes, because a resize means a different mapping and a
    /// presenter is better off re-attaching than reading a stale one.
    fn publish_hdr_reference(&mut self, linear: &[f32], w: u32, h: u32) {
        if !self.hdr_reference_output || !self.hdr_monitor.renders_hdr() || w == 0 || h == 0 {
            return;
        }
        let stale = self.hdr_writer.as_ref().is_none_or(|x| (x.width(), x.height()) != (w, h));
        if stale {
            let path = klipp_hdr_out::default_path();
            match klipp_hdr_out::FrameWriter::create(&path, w, h) {
                Ok(writer) => {
                    tracing::info!(
                        "HDR reference output: publishing {w}x{h} to {}",
                        path.display(),
                    );
                    self.hdr_writer = Some(writer);
                }
                Err(e) => {
                    // A broken output is not a broken render: report it
                    // once, turn it off, and keep showing the viewer.
                    tracing::warn!("HDR reference output unavailable ({}): {e}", path.display());
                    self.hdr_reference_output = false;
                    return;
                }
            }
        }
        if let Some(writer) = self.hdr_writer.as_mut() {
            writer.set_peak_nits(klipp_color::hdr::DEFAULT_MASTERING_MAX_LUMA_CD_M2);
            if !writer.publish(linear) {
                tracing::warn!(
                    "HDR reference output: a {}-sample frame does not fit {w}x{h}",
                    linear.len(),
                );
            }
        }
    }

    fn encode_for_screen(&self, linear: &[f32], w: u32, h: u32) -> (Vec<u8>, Option<(Vec<f32>, u32, u32, u32)>) {
        use klipp_core::HdrMonitor;
        if !self.hdr_monitor.renders_hdr() {
            return (self.display_encode(linear), None);
        }
        // Measured where the encoder measures: BT.2020, whatever the
        // project works in.
        let measure = klipp_color::hdr_view::HdrMeasure::for_working_space(crate::video_decoder::working_colorspace());
        let map = measure.nit_map(linear, w, h, super::NIT_MAP_STEP);
        let bytes = match self.hdr_monitor {
            HdrMonitor::FalseColour => measure.false_colour_frame(linear),
            // One pass, reading the frame once. This used to copy it
            // (`linear.to_vec()`), roll the copy off in place and encode
            // that; at 4K the copy alone measured 30 ms — more than the
            // encode it fed — so the roll-off is a hook on the transform.
            // Dithered as the GPU's preview encode is (#2072).
            _ => klipp_color::hdr_view::preview_frame_dithered(
                linear,
                self.tone_map_op,
                crate::video_decoder::working_colorspace(),
            ),
        };
        (bytes, Some((map.0, map.1, map.2, super::NIT_MAP_STEP)))
    }

    /// Show a rendered frame: scopes, the perf overlay, the GPU viewer
    /// texture, then the display encode into the frame buffer.
    fn show_rendered(&mut self, r: Rendered, t_frame: Instant) {
        let Rendered { picture, width: w, height: h, stats } = r;
        let mut scope_ms = 0.0;
        let t_encode = Instant::now();
        let (mut bytes, nits) = match picture {
            // Already the bytes the screen wants: a cached frame through its
            // per-channel display transform, or a decoder's own picture,
            // Rec.709 either way. The scopes measure them when due; nothing
            // here mirrors the picture to the GPU, which is a precondition
            // of taking this path, checked in `cached_frame_display_lut`.
            Picture::Display(bytes) => {
                scope_ms = self.draw_due_scopes(super::DisplayScopeFrame::rec709(&bytes), w, h);
                (bytes, None)
            }
            Picture::Linear(linear) => {
                // ACES has no GPU display transform, so its waveform reads
                // the bytes on screen after the encode below; every other
                // space is measured from the linear frame here.
                let scope_due = self.should_render_scope();
                let scopes_after_encode = crate::video_decoder::working_colorspace()
                    == klipp_core::WorkingColorspace::AcesCG;
                if scope_due && !scopes_after_encode {
                    scope_ms = self.draw_scopes(Some(&linear), None, w, h);
                }

                // The reference output carries this same picture. Done
                // here rather than in `encode_for_screen` because creating
                // the shared frame needs `&mut self`, and because this is
                // the one place the HDR frame exists on the CPU as a whole
                // — which is what another process needs.
                self.publish_hdr_reference(&linear, w, h);
                let encoded = self.encode_for_screen(&linear, w, h);
                if scope_due && scopes_after_encode {
                    scope_ms += self.draw_scopes(Some(&linear), Some(super::DisplayScopeFrame::rec709(&encoded.0)), w, h);
                }
                // Scopes not due this frame still show the one before it.
                self.scope_render_owed = if scope_due { false } else { self.scopes_visible };
                encoded
            }
        };
        let encode_ms = t_encode.elapsed().as_secs_f64() * 1000.0;
        let held = self.renderer.executor().held_counts();
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.write_frame_swap(&mut bytes, w, h);
            fb.set_presented(self.playhead, self.published_complete());
            fb.note(format!("shown as bytes: {:?} {:?} complete {} (streams pending {}, progress {}) held outputs {} hit {} missed", self.state, self.playhead.value, self.published_complete(), self.pending_stream_refresh, self.presenting_progress, held.hits, held.misses));
            match nits {
                Some((map, mw, mh, step)) => {
                    fb.nit_map = map;
                    (fb.nit_map_w, fb.nit_map_h, fb.nit_map_step) = (mw, mh, step);
                }
                None if !fb.nit_map.is_empty() => {
                    fb.nit_map = Vec::new();
                    (fb.nit_map_w, fb.nit_map_h) = (0, 0);
                }
                None => {}
            }
        }
        self.composite_scratch_u8 = bytes;
        self.finish_view_frame(w, h, stats, encode_ms, scope_ms, t_frame);
    }

    /// The frame is in the viewer's texture already, written there by
    /// the GPU stack with nothing read back: say so, and account for it
    /// as [`Self::show_rendered`] would.
    fn show_on_gpu(&mut self, w: u32, h: u32, stats: klipp_compose::RenderStats, t_frame: Instant) {
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.note_gpu_frame();
            fb.set_presented(self.playhead, self.published_complete());
            fb.note(format!("shown zero-copy, the bytes not renamed: {:?} {:?} complete {} (streams pending {}, progress {})", self.state, self.playhead.value, self.published_complete(), self.pending_stream_refresh, self.presenting_progress));
            if !fb.nit_map.is_empty() {
                fb.nit_map = Vec::new();
                (fb.nit_map_w, fb.nit_map_h) = (0, 0);
            }
        }
        self.finish_view_frame(w, h, Some(stats), 0.0, 0.0, t_frame);
    }

    /// The frame's accounting once it is published: the perf ring, the
    /// slow-frame log line, and the UI's wake-up.
    fn finish_view_frame(
        &mut self,
        w: u32,
        h: u32,
        stats: Option<klipp_compose::RenderStats>,
        encode_ms: f64,
        scope_ms: f64,
        t_frame: Instant,
    ) {
        let total_ms = t_frame.elapsed().as_secs_f64() * 1000.0;
        let s = stats.unwrap_or_default();
        // What the frame did after its render: the rest of its time.
        let mut phases = std::mem::take(&mut self.frame_phases);
        let before = phases.sync_ms + phases.snapshot_ms + phases.frame_snapshot_ms + phases.render_ms;
        phases.present_ms = (total_ms as f32 - before).max(0.0);
        let target_fps = self.target_fps_for_overlay();
        let decode_ms = self.v1_decoder.as_ref().map(|d| d.last_decode_ms()).unwrap_or(0.0);
        let dropped = match target_fps {
            Some(fps) if fps > 0.0 => (total_ms as f32).max(decode_ms) > 1000.0 / fps,
            _ => false,
        };
        let (streams_gpu, streams_software) = crate::decode_budget::counts();
        if let Ok(mut ring) = self.perf_ring.lock() {
            ring.push(crate::perf::PerfSample {
                timestamp: Instant::now(),
                width: w,
                height: h,
                target_fps: target_fps.unwrap_or(0.0),
                decode_ms,
                seq_read_ms: 0.0,
                grade_ms: s.grade_ms as f32,
                ofx_ms: s.ofx_ms as f32,
                builtin_ms: s.effects_ms as f32,
                lut_ms: s.lut_ms as f32,
                total_ms: total_ms as f32,
                dropped,
                bridge_u8_ms: 0.0,
                reencode_ms: encode_ms as f32,
                scope_ms: scope_ms as f32,
                composite_ms: s.composite_ms as f32,
                update_playback_ms: 0.0,
                in_param_edit: self.in_node_param_edit,
                phases,
                stills_kept: s.stills_kept,
                stills_uploaded: s.stills_uploaded,
                streams_gpu: streams_gpu as u32,
                streams_software: streams_software as u32,
            });
        }
        if total_ms > 16.0 || self.in_node_param_edit {
            tracing::debug!(
                "PERF view_frame: {total_ms:.1}ms {w}x{h} | render={:.1} (grade={:.1} ofx={:.1} fx={:.1} lut={:.1} composite={:.1}) \
                 encode={encode_ms:.1} scope={scope_ms:.1} streams={} cached={} stills kept={} uploaded={} decoders gpu={streams_gpu} software={streams_software} shortened={} disk_short={} \
                 grade[splice={:.1} analyses={:.1} prepare={:.1} seed={:.1} graph={:.1}]",
                s.total_ms, s.grade_ms, s.ofx_ms, s.effects_ms, s.lut_ms, s.composite_ms,
                self.streams.len(), stats.is_none(), s.stills_kept, s.stills_uploaded, super::streams_shortened(), super::streams_disk_short(),
                s.grade_split.splice_ms, s.grade_split.analyses_ms, s.grade_split.prepare_ms, s.grade_split.seed_ms, s.grade_split.graph_ms,
            );
        }

        let _ = self.event_tx.send(AppEvent::FrameReady);
        (self.repaint_cb)();
    }

    /// Render the frame at `frame_number` from the pre-grade cache into the
    /// render cache, without showing it. Only frames the cache can hold.
    pub(super) fn prerender_frame(&mut self, frame_number: i64, pixels: HeldFrame) -> bool {
        let primary = self.sequence.read().ok().and_then(|s| self.v1_clip_id(&s));
        self.prerender_frame_as(frame_number, primary, &pixels)
    }

    /// Render `frame_number` into the render cache with `pixels` as
    /// `primary`'s picture: the clip the frame shows, named by the caller.
    /// The idle pre-render names the primary decoder's clip; Render In to
    /// Out (#924) the clip at the frame, wherever the playhead is.
    pub(super) fn prerender_frame_as(&mut self, frame_number: i64, primary: Option<ClipId>, pixels: &HeldFrame) -> bool {
        let saved = self.playhead;
        self.playhead = klipp_core::RationalTime { value: frame_number, rate: saved.rate };
        let seq = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            klipp_compose::frame_snapshot(&seq, self.playhead)
        };
        // Both sizes a frame is shown at: full, for a parked scrub, and
        // under Half or Quarter the reduced one playback asks for. Full
        // size first, since the scan runs while parked. One render per
        // call, as before: a size already cached is passed over.
        let full = viewer_canvas((pixels.width, pixels.height), (seq.resolution.width, seq.resolution.height));
        let mut rendered = false;
        for (w, h) in prerender_sizes(full, crate::video_decoder::playback_quality_divisor()) {
            let Some(key) = self.render_cache_key(&seq, primary, (w, h)) else { break };
            // Already cached at this size, in memory or on disk: nothing to
            // render. Without this the scan rendered its nearest candidate
            // again on every idle tick, forever — a full render, a readback,
            // compression and a disk write, every 40 to 80 ms while paused.
            let cached = self.render_cache_hot.get(&key).is_some_and(|(_, cw, ch)| (cw, ch) == (w, h))
                || self.render_cache_disk.contains(&key);
            if cached {
                continue;
            }
            // No per-node thumbnails: this is not the user's frame.
            self.thumb_capture_suspended = true;
            let (linear, _) = self.render_timeline(&seq, primary.map(|id| (id, pixels)), w, h, Inspect::default());
            self.thumb_capture_suspended = false;
            let bytes = super::rendered_frame_to_cache_u8(&linear);
            self.render_cache_hot.insert(key.clone(), bytes.clone(), w, h);
            self.render_cache_disk.put(&key, &bytes, w, h);
            rendered = true;
            break;
        }
        self.playhead = saved;
        rendered
    }
}

/// Disable stabilization in the comparison's render copy.
/// Its stabilization nodes are found in, and switched off on, the clip's
/// picture (M4 step 12d).
fn disable_stabilization(c: &mut Clip) -> bool {
    use klipp_graph_format::{op::GraphOp, path::{NodePath, Root}};
    let Ok(fx) = c.picture_effects() else { return false };
    let clip = c.id.0;
    let ops: Vec<_> = fx
        .iter()
        .filter(|e| matches!(e.effect, klipp_effects::BuiltinEffect::VideoStabilization { .. }))
        .map(|e| GraphOp::SetEnabled { path: NodePath::new(Root::Clip { clip }, [e.node]), enabled: false })
        .collect();
    !ops.is_empty() && c.apply_picture_ops(&ops).is_ok()
}

/// The inspector's matte preview: the mask as a small grey tile.
/// The mean of the 5×5 patch of an RGBA frame around `(nx, ny)` (0..=1
/// over it), clipped to the frame: one noisy pixel does not set a key.
pub(super) fn patch_mean(rgba: &[f32], width: u32, height: u32, nx: f32, ny: f32) -> [f32; 3] {
    let (w, h) = (width as i64, height as i64);
    let cx = (nx.clamp(0.0, 1.0) * (w - 1) as f32).round() as i64;
    let cy = (ny.clamp(0.0, 1.0) * (h - 1) as f32).round() as i64;
    let (mut sum, mut n) = ([0.0f32; 3], 0.0f32);
    for y in (cy - 2).max(0)..=(cy + 2).min(h - 1) {
        for x in (cx - 2).max(0)..=(cx + 2).min(w - 1) {
            let i = ((y * w + x) * 4) as usize;
            for c in 0..3 {
                sum[c] += rgba[i + c];
            }
            n += 1.0;
        }
    }
    sum.map(|v| v / n.max(1.0))
}

fn matte_preview_event(consumer: ClipId, mask: &[f32], width: u32, height: u32) -> AppEvent {
    const PREVIEW_W: u32 = 96;
    const PREVIEW_H: u32 = 54;
    let mut rgba = Vec::with_capacity((PREVIEW_W * PREVIEW_H * 4) as usize);
    for ty in 0..PREVIEW_H {
        for tx in 0..PREVIEW_W {
            let sx = (tx as u64 * width as u64 / PREVIEW_W as u64) as usize;
            let sy = (ty as u64 * height as u64 / PREVIEW_H as u64) as usize;
            let v = mask.get(sy * width as usize + sx).copied().unwrap_or(0.0);
            let g = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            rgba.extend_from_slice(&[g, g, g, 255]);
        }
    }
    AppEvent::MatteMaskReady { consumer_clip_id: consumer, rgba, width: PREVIEW_W, height: PREVIEW_H }
}

/// An inspection that asks for nothing: the plain picture. `clip` alone
/// names the primary and changes nothing until something else is asked
/// of it.
fn inspect_is_plain(i: &Inspect) -> bool {
    !i.ungraded && i.thumbs.is_none() && i.scope_tap.is_none() && i.matte_view.is_none() && i.solo.is_none() && i.matte_of.is_none() && i.only.is_none()
}

#[cfg(test)]
mod effect_comparison_tests {
    use super::*;
    use klipp_core::Timecode;
    use klipp_effects::BuiltinEffect as E;

    fn fixture(flags: Vec<bool>) -> Clip {
        let mut clip = Clip::gap(Timecode::from_frames(0, 24), Timecode::from_frames(48, 24));
        let stabilization = E::VideoStabilization { smoothing: 0.5, crop_ratio: 0.1, zoom: true };
        clip.set_effect_list(vec![stabilization.clone(), E::GaussianBlur { radius: 2.0 }, stabilization], flags);
        let ids: Vec<_> = clip.effect_entries().map(|(id, _, _)| id).collect();
        for (i, id) in ids.into_iter().enumerate() {
            let param = if i == 1 { "radius" } else { "crop_ratio" };
            clip.edit_effect_keyframes_of(id, param, |k| klipp_core::keyframe::upsert_keyframe(k, 0.5, i as f32)).unwrap();
            clip.edit_effect_modifiers(id, param, |v| v.modifiers.push(klipp_core::Modifier::Clamp { min: 0.0, max: 5.0, enabled: true })).unwrap();
        }
        clip
    }

    fn assert_only_flags_changed(before: &mut Clip, after: &mut Clip) {
        assert_eq!(before.effect_entries().map(|(id, _, _)| id).collect::<Vec<_>>(), after.effect_entries().map(|(id, _, _)| id).collect::<Vec<_>>());
        let mut a = state(before);
        let mut b = state(after);
        a.as_object_mut().unwrap().remove("enabled");
        b.as_object_mut().unwrap().remove("enabled");
        assert_eq!(a, b, "comparison must preserve values, keyframes and modifiers");
    }

    fn state(clip: &Clip) -> serde_json::Value {
        // ParamId is an enum key; the project writer supplies its string
        // encoding, so inspect the stacks separately from Clip's serde.
        let modifiers: std::collections::BTreeMap<_, _> = clip.param_modifiers().iter()
            .map(|(id, stack)| (format!("{id:?}"), serde_json::to_value(stack).unwrap()))
            .collect();
        serde_json::json!({ "effects": clip.builtin_effects(), "enabled": clip.builtin_effects_enabled(),
            "keyframes": clip.effect_keyframes(), "modifiers": modifiers })
    }

    #[test]
    fn toggling_the_comparison_keeps_animation_on_its_effect() {
        for flags in [vec![], vec![false], vec![true, false, true, false]] {
            let mut original = fixture(flags);
            for index in 0..3 {
                let mut compared = original.clone();
                let mut expected: Vec<_> = original.effect_entries().map(|(_, _, on)| on).collect();
                expected[index] = !expected[index];
                let node = original.effect_node_at(false, index).unwrap();
                assert!(compared.toggle_effect_node(node));
                assert_eq!(compared.effect_entries().map(|(_, _, on)| on).collect::<Vec<_>>(), expected);
                assert_only_flags_changed(&mut original, &mut compared);
            }
            let before = state(&original);
            assert!(!original.toggle_effect_node(uuid::Uuid::new_v4()));
            assert_eq!(state(&original), before, "a node the clip doesn't hold is a no-op");
        }
    }

    #[test]
    fn stabilization_comparison_disables_only_stabilizers() {
        for flags in [vec![], vec![false], vec![true, false, true, false]] {
            let mut original = fixture(flags);
            let blur_on = original.effect_entries().nth(1).unwrap().2;
            let mut compared = original.clone();
            assert!(disable_stabilization(&mut compared));
            assert_eq!(compared.effect_entries().map(|(_, _, on)| on).collect::<Vec<_>>(), vec![false, blur_on, false]);
            assert_only_flags_changed(&mut original, &mut compared);
        }
        let mut plain = klipp_timeline::test_clip::TestClip::of(Clip::gap(Timecode::from_frames(0, 24), Timecode::from_frames(48, 24))).effect(E::GaussianBlur { radius: 2.0 }).build();
        let before = state(&plain);
        assert!(!disable_stabilization(&mut plain));
        assert_eq!(state(&plain), before);
    }
}

#[cfg(test)]
mod playback_quality_tests {
    use super::*;

    #[test]
    fn half_and_quarter_keep_the_shape_and_stop_at_a_watchable_size() {
        assert_eq!(reduced_for_quality((3840, 2160), 1), (3840, 2160));
        assert_eq!(reduced_for_quality((3840, 2160), 2), (1920, 1080));
        assert_eq!(reduced_for_quality((3840, 2160), 4), (960, 540));
        // A 1080p frame at a quarter would be 270 lines: fine. A 480p one
        // stops at the floor, and one already below it is left alone.
        assert_eq!(reduced_for_quality((1920, 1080), 4), (480, 270));
        assert_eq!(reduced_for_quality((854, 480), 4), (320, 180));
        assert_eq!(reduced_for_quality((320, 180), 2), (320, 180));
        // Parked, whatever the quality, frames are full size; the
        // reduction is for playing.
        assert_eq!(quality_divisor_for(PlaybackState::Playing, 2), 2);
        assert_eq!(quality_divisor_for(PlaybackState::Paused, 4), 1);
        assert_eq!(quality_divisor_for(PlaybackState::Stopped, 2), 1);
        // The scan warms the parked size first, then playback's.
        assert_eq!(prerender_sizes((1920, 1080), 1), vec![(1920, 1080)]);
        assert_eq!(prerender_sizes((1920, 1080), 2), vec![(1920, 1080), (960, 540)]);
        assert_eq!(prerender_sizes((320, 180), 4), vec![(320, 180)], "already at the floor");
        // Odd shapes stay even, for the encoder-shaped buffers downstream.
        let (w, h) = reduced_for_quality((2048, 858), 2);
        assert!(w % 2 == 0 && h % 2 == 0 && (w as f64 / h as f64 - 2048.0 / 858.0).abs() < 0.01, "{w}x{h}");
    }
}

/// [`clip_render_hash`]'s value for `clip`. A clip that won't serialise
/// gets a key no other has had, made afresh after each change, so a frame
/// of one state of it is never taken for another: salted per process, so
/// it doesn't meet a file the disk cache kept from another run either.
fn clip_render_hash_of(clip: &Clip) -> u64 {
    crate::canonical_hash::canonical_hash(clip).unwrap_or_else(|e| {
        tracing::warn!("render cache: clip {} can't be hashed ({e}); its frames are kept for this state only", clip.id.0);
        static SALT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        static UNHASHABLE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let salt = *SALT.get_or_init(|| {
            use std::hash::BuildHasher;
            std::hash::RandomState::new().hash_one(std::process::id())
        });
        salt ^ UNHASHABLE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    })
}

#[cfg(test)]
mod media_range_tests {
    use super::*;

    /// The viewer bounds a clip's neighbours by its media's duration, as
    /// export does; a clip without media has no range.
    #[test]
    fn the_viewers_range_is_the_medias_duration() {
        let mut pool = MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/known/ten.mov".into(), None);
        item.duration = klipp_core::RationalTime { value: 250, rate: 25 };
        let media = item.id;
        pool.add(item);
        let tc = |f: i64| klipp_core::RationalTime { value: f, rate: 25 };
        let clip = Clip::media(media, tc(0), tc(50), tc(200), tc(250));
        assert_eq!(viewer_media_range(&pool, &clip), Some((0.0, 10.0)), "the whole media, handles included");
        assert_eq!(viewer_media_range(&pool, &Clip::gap(tc(0), tc(50))), None, "no media, no range");
    }
}

#[cfg(test)]
mod analysis_key_tests {
    use super::viewer_analysis_key;
    use klipp_analysis::assets::AssetKind;
    use klipp_compose::FrameSource;

    /// A media item with a proxy: the viewer, which decodes the proxy, and
    /// export, which decodes the original, give one key, the original's,
    /// so they read one asset.
    #[test]
    fn the_viewer_and_export_key_a_proxied_clip_by_the_original() {
        let dir = std::env::temp_dir().join(format!("klipp-viewer-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (original, proxy) = (dir.join("clip.mov"), dir.join("clip.proxy.mov"));
        std::fs::write(&original, b"original").unwrap();
        std::fs::write(&proxy, b"proxy").unwrap();
        let mut item = klipp_media::MediaItem::synthetic_for_test(original.clone(), Some(proxy.to_str().unwrap()));
        item.resolution = klipp_core::Resolution { width: 1920, height: 1080 };
        item.fps = klipp_core::Fps::new(25, 1);
        let id = item.id;
        let mut pool = klipp_media::MediaPool::new();
        pool.add(item);
        let tc = |f| klipp_core::RationalTime::from_frames(f, 25);
        let clip = klipp_timeline::Clip::media(id, tc(0), tc(50), tc(0), tc(50));
        let viewer = viewer_analysis_key(&pool, &clip, AssetKind::Stabilisation).expect("the viewer's key");
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut track = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
        track.clips = vec![clip.clone()];
        seq.tracks = vec![track];
        let settings = {
            use klipp_core::*;
            ExportSettings {
                pan_law: Default::default(),
                output_path: dir.join("out.mp4"), format: ExportFormat::Mp4, video_codec: ExportVideoCodec::H264,
                resolution: None, fps: None, crf: 18, preset: EncoderPreset::Fast, audio_codec: ExportAudioCodec::Copy,
                audio_bitrate_kbps: None, video_bitrate_kbps: None, rate_control: RateControl::Crf,
                burn_in_subtitles: false, embed_subtitles: false, target_lufs: None, true_peak_dbtp: None, embed_chapters: false, captions_608: false, hdr10plus: false,
                bit_depth: 8, hdr_mode: HdrMode::Sdr, tone_map_op: ToneMapOp::Hable, odt: OdtChoice::MatchViewer,
                mastering_peak_nits: DEFAULT_MASTERING_PEAK_NITS,
            }
        };
        let snap = klipp_render::SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
        let mut export = klipp_render::ExportFrameSource::new(&snap.video.media, 25.0, klipp_core::WorkingColorspace::Rec709Linear);
        let exported = export.analysis_key(&clip, AssetKind::Stabilisation).expect("export's key");
        assert_eq!(viewer, exported, "one asset for both");
        assert_eq!(Some(viewer.media), klipp_analysis::store::key_for(&original), "the original's file, not the proxy's");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod neighbour_tests {
    use super::ViewerSource;
    use klipp_compose::FrameSource;

    /// A flat BT.709 red written 4:4:4 at `w × h`; `None` without ffmpeg.
    fn red(path: &std::path::Path, w: u32, h: u32) -> Option<()> {
        std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("color=c=0xC82828:s={w}x{h}:r=24:d=1"), "-vf",
                "format=rgb24,scale=out_color_matrix=bt709:out_range=limited,format=yuv444p,setparams=color_primaries=bt709:color_trc=bt709",
                "-colorspace", "bt709", "-color_range", "tv", "-c:v", "ffv1"])
            .arg(path)
            .status()
            .ok()?
            .success()
            .then_some(())
    }

    /// #2013: the viewer hands an OFX plug-in export's neighbours, byte
    /// for byte, from the file it decodes: the proxy when it shows one
    /// (its own size), the original when proxies are off. One reader per
    /// file is kept across frames, not opened for each. Control: plain
    /// swscale, the old route, is many levels off the file's red.
    #[test]
    fn the_viewers_neighbours_are_exports_from_the_file_it_shows() {
        let dir = std::env::temp_dir().join(format!("klipp-viewer-neighbours-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (original, proxy) = (dir.join("clip.mkv"), dir.join("clip.proxy.mkv"));
        if red(&original, 64, 36).is_none() || red(&proxy, 32, 18).is_none() {
            eprintln!("skipped: ffmpeg could not write the test clips");
            return;
        }
        let mut item = klipp_media::MediaItem::synthetic_for_test(original.clone(), Some(proxy.to_str().unwrap()));
        item.proxy_ready = true;
        item.fps = klipp_core::Fps::new(24, 1);
        let id = item.id;
        let mut pool = klipp_media::MediaPool::new();
        pool.add(item);
        let pool = std::sync::RwLock::new(pool);
        let tc = |f| klipp_core::RationalTime::from_frames(f, 24);
        let clip = klipp_timeline::Clip::media(id, tc(0), tc(24), tc(0), tc(24));
        let mut streams = super::super::streams::ClipStreams::new();
        for (use_proxies, file, size) in [(true, &proxy, (32, 18)), (false, &original, (64, 36))] {
            let mut view = ViewerSource { primary: None, primary_queue: None, streams: &mut streams, media_pool: &pool, hold: None, use_proxies, primary_previous: None };
            let read = view.temporal_frames(&clip).expect("a reader");
            let (viewer, w, h) = read(0.5).expect("a frame");
            assert_eq!((w, h), size, "proxies {use_proxies}: read from {}", file.display());
            let (export, ..) = klipp_media::frame_reader::SeekableFrameReader::open_colour_managed(file, None).unwrap().decode_at(0.5).unwrap();
            assert_eq!(viewer, export, "proxies {use_proxies}: the viewer's neighbour is export's");
            let mid = ((h / 2 * w + w / 2) * 4) as usize;
            let (plain, ..) = klipp_media::frame_reader::SeekableFrameReader::open_rgba(file).unwrap().decode_at(0.5).unwrap();
            let off = plain[mid..mid + 3].iter().zip(&viewer[mid..mid + 3]).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            assert!(off > 5, "the control: swscale is {off} off");
        }
        // Three frames of the proxy: one reader, kept.
        for _ in 0..3 {
            let mut view = ViewerSource { primary: None, primary_queue: None, streams: &mut streams, media_pool: &pool, hold: None, use_proxies: true, primary_previous: None };
            view.temporal_frames(&clip).expect("a reader")(0.25).expect("a frame");
        }
        assert_eq!(streams.neighbour_readers(), 2, "the proxy's reader and the original's, each opened once");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
