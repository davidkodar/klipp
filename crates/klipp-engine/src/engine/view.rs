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
use klipp_timeline::{Clip, Sequence};

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
    streams: &'a ClipStreams,
    media_pool: &'a RwLock<MediaPool>,
}

impl ViewerSource<'_> {
    fn held(&self, clip: ClipId) -> Option<&HeldFrame> {
        match self.primary {
            Some((id, f)) if id == clip => Some(f),
            _ => self.streams.frame(clip),
        }
    }

    /// The clip's original media file: motion estimation and temporal
    /// plugins read it at arbitrary times, as they always have.
    fn original_path(&self, clip: &Clip) -> Option<std::path::PathBuf> {
        let media = clip.media_id()?;
        let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
        pool.get(&media).map(|item| item.path.clone())
    }
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

/// The fewest lines Playback Quality reduces the viewer to.
pub(super) const MIN_REDUCED_HEIGHT: u32 = 180;

fn source_frame(f: &HeldFrame) -> SourceFrame {
    SourceFrame { pixels: f.source_pixels(), width: f.width, height: f.height, pts: f.pts }
}

impl FrameSource for ViewerSource<'_> {
    fn frame(&mut self, clip: &Clip, _source_secs: f64, _width: u32, _height: u32) -> Option<SourceFrame> {
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

    fn motion_frame(&mut self, clip: &Clip, source_secs: f64, max_width: u32) -> Option<(Vec<u8>, u32, u32)> {
        let path = self.original_path(clip)?;
        super::generate_thumbnail_at_size(&path, source_secs.max(0.0), max_width).ok()
    }

    fn temporal_frames(&mut self, clip: &Clip) -> Option<klipp_plugin::FrameProviderFn> {
        let path = self.original_path(clip)?;
        let source_in = clip.source_in.to_secs_f64();
        let decoder = std::cell::RefCell::new(super::CachedFrameDecoder::open(&path)?);
        Some(Box::new(move |t: f64| decoder.borrow_mut().decode_at((source_in + t).max(0.0))))
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
        Some(Box::new(move |t: f64| reader.decode_at((source_in + t).max(0.0))))
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
    /// The clip the primary decoder plays.
    pub(super) fn v1_clip_id(&self, seq: &Sequence) -> Option<ClipId> {
        seq.tracks.get(self.current_track_index)?.clips.get(self.current_clip_index).map(|c| c.id)
    }

    /// Bring the clip streams in step with the playhead: one decoder for
    /// every media clip the frame needs besides the primary decoder's.
    pub(super) fn sync_streams(&mut self) {
        let time = self.playhead;
        let wants: Vec<Want> = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            let primary = if self.v1_decoder.is_some() { self.v1_clip_id(&seq) } else { None };
            // A layer's clips are read at the layer's own time and rate:
            // inside a precomposition those are the scene's, not ours.
            klipp_compose::active_layers(&seq, time)
                .iter()
                .flat_map(|l| l.clips.iter().map(move |c| (c, l.at, l.fps)))
                .filter(|(c, _, _)| Some(c.id) != primary)
                .filter_map(|(c, at, fps)| {
                    Some(Want {
                        clip: c.id,
                        media: c.media_id()?,
                        rotation: c.source_rotation,
                        source_secs: klipp_compose::render::source_secs(c, at, fps),
                    })
                })
                .collect()
        };
        // Files are resolved here, for new streams only: resolution logs.
        let paths: HashMap<ClipId, (std::path::PathBuf, i32)> = wants
            .iter()
            .filter(|w| !self.streams.has(w.clip))
            .filter_map(|w| {
                let path = self.path_for_media(w.media)?;
                let rot = self.effective_decode_rotation(w.media, &path, w.rotation);
                Some((w.clip, (path, rot)))
            })
            .collect();
        let frame_secs = self.fps.frame_duration_secs();
        self.streams.sync(&wants, frame_secs, &|w: &Want| paths.get(&w.clip).cloned());
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
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
            self.streams.pump(frame_secs);
        }
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
            self.regrade_cached_frame();
        }
    }

    /// What the grade tools want from this frame. They follow the clip
    /// the grade panel is on when it is on screen, else the primary clip.
    fn inspect_request(&self, seq: &Sequence, primary: Option<ClipId>) -> Inspect {
        let find = |id: ClipId| seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == id);
        let clip = self.inspect_clip.filter(|id| find(*id).is_some()).or(primary);
        let matte_view = if self.matte_preview {
            self.matte_preview_node.or_else(|| {
                let graph = &find(clip?)?.node_graph;
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
        Inspect {
            clip,
            thumbs,
            scope_tap: self.scope_tap_node,
            matte_view,
            solo: self.solo_node,
            ungraded: self.bypass_color_grade,
            matte_of: self.inspector_matte_preview_target,
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
        let lut = match (&p.global_lut, p.enabled) {
            (Some(lut), true) => Some(klipp_compose::GlobalLut { lut: lut.clone(), opacity: p.global_lut_opacity }),
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
    /// being rendered ahead.
    fn render_cache_key(&mut self, seq: &Sequence, primary: Option<ClipId>, size: (u32, u32)) -> Option<crate::render_cache::RenderCacheKey> {
        if self.in_node_param_edit
            || self.solo_node.is_some()
            || self.matte_preview
            // The cache holds display bytes of the SDR picture; HDR
            // monitoring shows another picture entirely.
            || self.hdr_monitor.renders_hdr()
            || self.bypass_color_grade
            || self.compare_stabilization
            || self.compare_effect_active
            || self.scope_tap_node.is_some()
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
        if clip.node_graph.is_identity() && clip.builtin_effects.is_empty() {
            return None;
        }
        let track_opacity = seq.tracks.get(layer.track).map(|t| t.opacity).unwrap_or(1.0);
        let grade_hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (self.render_key_hash(clip, track_opacity), size).hash(&mut h);
            h.finish()
        };
        Some(crate::render_cache::RenderCacheKey { clip_id: clip.id, frame_number: self.playhead.value, grade_hash })
    }

    /// The render cache's hash for a clip's frames: the clip, the global
    /// LUT, its track's opacity, and the render environment (working
    /// space, LUT interpolation, proxy decode, the media's log format).
    pub(super) fn render_key_hash(&mut self, clip: &Clip, track_opacity: f32) -> u64 {
        let environment = crate::render_cache::render_environment_hash(
            self.color_pipeline.use_tetrahedral,
            crate::video_decoder::working_colorspace(),
            self.use_proxies,
            self.current_file.as_deref().and_then(crate::video_decoder::registered_log_format),
        );
        self.clip_render_hash(clip) ^ self.lut_hash() ^ (track_opacity.to_bits() as u64).rotate_left(17) ^ environment.rotate_left(29)
    }

    /// A hash of everything about a clip that its rendered frames depend
    /// on, kept until the sequence changes.
    fn clip_render_hash(&mut self, clip: &Clip) -> u64 {
        if self.current_grade_hash == 0 {
            // The sequence or a grade changed since the hashes were made.
            self.render_hashes.clear();
            self.current_grade_hash = 1;
        }
        *self.render_hashes.entry(clip.id).or_insert_with(|| {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            serde_json::to_vec(clip).unwrap_or_default().hash(&mut h);
            h.finish()
        })
    }

    fn lut_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let p = &self.color_pipeline;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (p.enabled && p.global_lut.is_some()).hash(&mut h);
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
        self.renderer.set_lut_tetrahedral(self.color_pipeline.use_tetrahedral);
        self.push_global_lut();
        let time = self.playhead;
        let mut src = ViewerSource {
            primary,
            primary_queue: self.v1_decoder.as_ref().map(|d| d.frame_queue()),
            streams: &self.streams,
            media_pool: &self.media_pool,
        };
        let opts = RenderOptions {
            hdr_peak_nits: self
                .hdr_monitor
                .renders_hdr()
                .then_some(klipp_color::hdr::DEFAULT_MASTERING_MAX_LUMA_CD_M2),
            ..Default::default()
        };
        self.renderer.render_display_on_gpu(seq, time, width, height, &mut src, opts, out)
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
        let time = self.playhead;
        let mut src = ViewerSource {
            primary,
            primary_queue: self.v1_decoder.as_ref().map(|d| d.frame_queue()),
            streams: &self.streams,
            media_pool: &self.media_pool,
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
        let t_frame = Instant::now();
        self.sync_streams();
        if self.state != PlaybackState::Playing {
            // While scrubbing, show the frame at once and fill in the other
            // clips when they arrive; waiting on each would drag the scrub.
            let wait = if self.is_scrubbing { Duration::ZERO } else { Duration::from_millis(150) };
            self.wait_for_streams(wait);
        }
        let (seq, primary_clip) = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            let id = primary.as_ref().and(self.v1_clip_id(&seq));
            (klipp_compose::frame_snapshot(&seq, self.playhead), id)
        };
        let seq_size = (seq.resolution.width, seq.resolution.height);
        let (w, h) = match &primary {
            Some(p) => viewer_canvas((p.width, p.height), seq_size),
            None => seq_size,
        };
        // Playback Quality applies while playing: a parked frame is for
        // judging, and renders full size.
        let (w, h) = if self.state == PlaybackState::Playing {
            reduced_for_quality((w, h), crate::video_decoder::playback_quality_divisor())
        } else {
            (w, h)
        };
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
            self.frame_cache.insert(self.playhead.value, pregrade, pw, ph);
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

        let cache_key = if progress || stack_only { None } else { self.render_cache_key(&seq, primary_clip, (w, h)) };
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
        if let Some(p) = primary.as_ref().filter(|_| !stack_only) {
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
                && super::PlaybackEngine::only_the_graded_path_handles(&blockers)
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
        // consumers (reference frame, eyedroppers, histogram), a scope
        // pass that is due, or a viewer without the texture. The stack
        // declines what it cannot do yet and the full path below takes
        // the frame.
        if !progress && inspect_is_plain(&inspect) && self.gpu_composite_allowed() {
            let target = self.gpu_viewer_target(w, h);
            let want_bytes = target.is_none() || self.state != PlaybackState::Playing || self.should_render_scope();
            let out = klipp_compose::DisplayDelivery { texture: target.as_deref(), bytes: want_bytes };
            if let Some((bytes, stats)) = self.render_composite_on_gpu(&seq, primary_ref, w, h, out) {
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
                        self.show_on_gpu(w, h, stats, t_frame);
                    }
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

        let (linear, found) = self.render_timeline(&seq, primary_ref, w, h, inspect);
        let cached = cache_key
            .filter(|_| self.state != PlaybackState::Playing)
            .map(|key| (key, super::linear_rgba_to_u8(&linear)));

        self.show_split_views(&seq, primary_ref, w, h, inspected, found.ungraded.as_deref());
        self.show_inspection(&found, w, h);
        self.show_rendered(
            Rendered { picture: Picture::Linear(linear), width: w, height: h, stats: Some(found.stats) },
            t_frame,
        );

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
            alt_render(self, &|c: &mut Clip| {
                let stab: Vec<usize> = c
                    .builtin_effects
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| matches!(e, klipp_effects::BuiltinEffect::VideoStabilization { .. }))
                    .map(|(i, _)| i)
                    .collect();
                for i in &stab {
                    set_effect_enabled(c, *i, false);
                }
                !stab.is_empty()
            }, inspected)
        } else {
            Vec::new()
        };

        let compare = if self.compare_effect_active {
            let (index, is_ofx) = (self.compare_effect_index, self.compare_effect_is_ofx);
            alt_render(self, &|c: &mut Clip| {
                if is_ofx {
                    match c.effects.get_mut(index) {
                        Some(e) => {
                            e.enabled = !e.enabled;
                            true
                        }
                        None => false,
                    }
                } else if index < c.builtin_effects.len() {
                    let on = c.builtin_effects_enabled.get(index).copied().unwrap_or(true);
                    set_effect_enabled(c, index, !on);
                    true
                } else {
                    false
                }
            }, self.compare_effect_clip_id)
        } else {
            Vec::new()
        };

        let original = ungraded.map(|u| self.display_encode(u)).unwrap_or_default();
        let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
        fb.unstab_data = unstab;
        fb.compare_effect_data = compare;
        fb.original_data = original;
    }

    /// Hand the grade tools what the render found.
    fn show_inspection(&mut self, found: &Inspection, w: u32, h: u32) {
        if found.scope_tap.is_some() || !found.thumbs.is_empty() {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(tap) = &found.scope_tap {
                fb.scope_data = klipp_effects::linear_f32_rgba_to_srgb_u8(tap);
            }
            if !found.thumbs.is_empty() {
                fb.node_thumbs = found.thumbs.clone();
                fb.node_thumbs_gen = fb.node_thumbs_gen.wrapping_add(1);
            }
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
            _ => klipp_color::hdr_view::preview_frame(
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
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.write_frame_swap(&mut bytes, w, h);
            fb.set_presented(self.playhead, !self.pending_stream_refresh);
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
            fb.set_presented(self.playhead, !self.pending_stream_refresh);
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
        let target_fps = self.target_fps_for_overlay();
        let decode_ms = self.v1_decoder.as_ref().map(|d| d.last_decode_ms()).unwrap_or(0.0);
        let dropped = match target_fps {
            Some(fps) if fps > 0.0 => (total_ms as f32).max(decode_ms) > 1000.0 / fps,
            _ => false,
        };
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
            });
        }
        if total_ms > 16.0 || self.in_node_param_edit {
            tracing::debug!(
                "PERF view_frame: {total_ms:.1}ms {w}x{h} | render={:.1} (grade={:.1} ofx={:.1} fx={:.1} lut={:.1} composite={:.1}) \
                 encode={encode_ms:.1} scope={scope_ms:.1} streams={} cached={}",
                s.total_ms, s.grade_ms, s.ofx_ms, s.effects_ms, s.lut_ms, s.composite_ms,
                self.streams.len(), stats.is_none(),
            );
        }

        let _ = self.event_tx.send(AppEvent::FrameReady);
        (self.repaint_cb)();
    }

    /// Render the frame at `frame_number` from the pre-grade cache into the
    /// render cache, without showing it. Only frames the cache can hold.
    pub(super) fn prerender_frame(&mut self, frame_number: i64, pixels: HeldFrame) -> bool {
        let saved = self.playhead;
        self.playhead = klipp_core::RationalTime { value: frame_number, rate: saved.rate };
        let seq = {
            let seq = self.sequence.read().unwrap_or_else(|e| e.into_inner());
            klipp_compose::frame_snapshot(&seq, self.playhead)
        };
        let primary = self.sequence.read().ok().and_then(|s| self.v1_clip_id(&s));
        // Ahead of playback, so at the size playback will ask for.
        let (w, h) = reduced_for_quality(
            viewer_canvas((pixels.width, pixels.height), (seq.resolution.width, seq.resolution.height)),
            crate::video_decoder::playback_quality_divisor(),
        );
        let rendered = match self.render_cache_key(&seq, primary, (w, h)) {
            // Already cached at this size, in memory or on disk: nothing to
            // render. Without this the scan rendered its nearest candidate
            // again on every idle tick, forever — a full render, a readback,
            // compression and a disk write, every 40 to 80 ms while paused.
            Some(key)
                if self
                    .render_cache_hot
                    .get(&key)
                    .is_some_and(|(_, cw, ch)| (cw, ch) == (w, h))
                    || self.render_cache_disk.contains(&key) =>
            {
                false
            }
            Some(key) => {
                // No per-node thumbnails: this is not the user's frame.
                self.thumb_capture_suspended = true;
                let (linear, _) = self.render_timeline(&seq, primary.map(|id| (id, &pixels)), w, h, Inspect::default());
                self.thumb_capture_suspended = false;
                let bytes = super::linear_rgba_to_u8(&linear);
                self.render_cache_hot.insert(key.clone(), bytes.clone(), w, h);
                self.render_cache_disk.put(&key, &bytes, w, h);
                true
            }
            None => false,
        };
        self.playhead = saved;
        rendered
    }
}

/// Turn effect `index` on or off, growing the enabled list as needed.
fn set_effect_enabled(c: &mut Clip, index: usize, on: bool) {
    if c.builtin_effects_enabled.len() <= index {
        c.builtin_effects_enabled.resize(c.builtin_effects.len().max(index + 1), true);
    }
    c.builtin_effects_enabled[index] = on;
}

/// The inspector's matte preview: the mask as a small grey tile.
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
    !i.ungraded && i.thumbs.is_none() && i.scope_tap.is_none() && i.matte_view.is_none() && i.solo.is_none() && i.matte_of.is_none()
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
        // Odd shapes stay even, for the encoder-shaped buffers downstream.
        let (w, h) = reduced_for_quality((2048, 858), 2);
        assert!(w % 2 == 0 && h % 2 == 0 && (w as f64 / h as f64 - 2048.0 / 858.0).abs() < 0.01, "{w}x{h}");
    }
}
