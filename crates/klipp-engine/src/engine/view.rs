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
}

/// A rendered viewer frame, before the display encode.
struct Rendered {
    linear: Vec<f32>,
    width: u32,
    height: u32,
    stats: Option<RenderStats>,
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
            let fps = seq.fps.as_f64();
            klipp_compose::active_layers(&seq, time)
                .iter()
                .flat_map(|l| l.clips.iter())
                .filter(|c| Some(c.id) != primary)
                .filter_map(|c| {
                    Some(Want {
                        clip: c.id,
                        media: c.media_id()?,
                        rotation: c.source_rotation,
                        source_secs: klipp_compose::render::source_secs(c, time, fps),
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
    fn render_cache_key(&mut self, seq: &Sequence, primary: Option<ClipId>) -> Option<crate::render_cache::RenderCacheKey> {
        if self.in_node_param_edit
            || self.solo_node.is_some()
            || self.matte_preview
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
        let grade_hash = self.render_key_hash(clip, track_opacity);
        Some(crate::render_cache::RenderCacheKey { clip_id: clip.id, frame_number: self.playhead.value, grade_hash })
    }

    /// The render cache's hash for a clip's frames: the clip, the global
    /// LUT, its track's opacity.
    pub(super) fn render_key_hash(&mut self, clip: &Clip, track_opacity: f32) -> u64 {
        self.clip_render_hash(clip) ^ self.lut_hash() ^ (track_opacity.to_bits() as u64).rotate_left(17)
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
    fn render_timeline(
        &mut self,
        seq: &Sequence,
        primary: Option<(ClipId, &HeldFrame)>,
        width: u32,
        height: u32,
        inspect: Inspect,
    ) -> (Vec<f32>, Inspection) {
        self.renderer.set_working_colorspace(crate::video_decoder::working_colorspace());
        self.push_global_lut();
        let downsample = self.in_node_param_edit
            && super::drag_downsample_enabled()
            && !self.force_full_res_regrade
            && !self.bypass_color_grade
            && !self.compare_stabilization
            && !self.compare_effect_active
            && width >= 480
            && height >= 480
            && width % 2 == 0
            && height % 2 == 0;
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
        let (out, found) = self.renderer.render_inspected(seq, time, w, h, &mut src, RenderOptions::default(), inspect);
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
        let (w, h) = match &primary {
            Some(p) => (p.width, p.height),
            None => (seq.resolution.width, seq.resolution.height),
        };
        if w == 0 || h == 0 {
            return;
        }

        // The ungraded frame, for compare modes, scrubbing back and the UI's
        // scope preview during a slider drag. A regrade of the parked frame
        // has it already: remaking it cost an HDR frame a CPU decode, tone
        // map and encode — 20 ms at 4K — per slider step.
        let parked = primary
            .as_ref()
            .zip(self.cached_pregrade_frame.as_ref())
            .is_some_and(|(p, (c, cw, ch))| (*cw, *ch) == (p.width, p.height) && p.is_pregrade(c));
        if let Some(p) = primary.as_ref().filter(|_| !parked && self.pregrade_encode_needed()) {
            let pregrade = p.pregrade();
            let display = pregrade.display();
            self.cached_pregrade_frame = Some((pregrade.clone(), w, h));
            self.frame_cache.insert(self.playhead.value, pregrade, w, h);
            self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner()).pregrade_data = display;
        }

        let cache_key = self.render_cache_key(&seq, primary_clip);
        if let Some(key) = &cache_key {
            let hit = self
                .render_cache_hot
                .get(key)
                .filter(|(_, cw, ch)| (*cw, *ch) == (w, h))
                .map(|(data, _, _)| data.to_vec())
                .or_else(|| {
                    self.render_cache_disk.get(key).filter(|(_, cw, ch)| (*cw, *ch) == (w, h)).map(|(data, _, _)| data)
                });
            if let Some(bytes) = hit {
                self.render_cache_hot.insert(key.clone(), bytes.clone(), w, h);
                let linear = super::u8_rgba_to_linear(&bytes);
                self.show_rendered(Rendered { linear, width: w, height: h, stats: None }, t_frame);
                return;
            }
        }

        let inspect = self.inspect_request(&seq, primary_clip);
        let inspected = inspect.clip;
        let primary_ref = primary_clip.zip(primary.as_ref());
        let (linear, found) = self.render_timeline(&seq, primary_ref, w, h, inspect);
        let cached = cache_key
            .filter(|_| self.state != PlaybackState::Playing)
            .map(|key| (key, super::linear_rgba_to_u8(&linear)));

        self.show_split_views(&seq, primary_ref, w, h, inspected, found.ungraded.as_deref());
        self.show_inspection(&found, w, h);
        self.show_rendered(Rendered { linear, width: w, height: h, stats: Some(found.stats) }, t_frame);

        // Cached once the frame is on screen: compressing and writing a 4K
        // frame (~15 ms) is not something the viewer should wait for.
        if let Some((key, bytes)) = cached {
            self.render_cache_disk.put(&key, &bytes, w, h);
            self.render_cache_hot.insert(key, bytes, w, h);
        }
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
            let changed = alt.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).filter(|c| c.id == id).any(|c| edit(c));
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

    /// Show a rendered frame: scopes, the perf overlay, the GPU viewer
    /// texture, then the display encode into the frame buffer.
    fn show_rendered(&mut self, r: Rendered, t_frame: Instant) {
        let Rendered { linear, width: w, height: h, stats } = r;
        let t_scope = Instant::now();
        if self.should_render_scope() {
            self.render_active_scopes_via_ffmpeg_linear(&linear, w, h, 1024, 512);
            self.last_scope_render = Instant::now();
        }
        let scope_ms = t_scope.elapsed().as_secs_f64() * 1000.0;

        // Mirror the frame into the UI's GPU texture when that path is on.
        if super::should_publish_gpu_viewer() && (w * 4) % 256 == 0 {
            if let Some(gpu) = self.renderer.executor().gpu_pipeline_mut() {
                let device = gpu.device();
                let tex = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner()).ensure_gpu_output(&device, w, h);
                if let Some(tex) = tex {
                    let _ = gpu.linear_rgba_to_texture_gpu(&linear, w, h, &tex);
                }
            }
        }

        let t_encode = Instant::now();
        let mut bytes = self.display_encode(&linear);
        let encode_ms = t_encode.elapsed().as_secs_f64() * 1000.0;
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.write_frame_swap(&mut bytes, w, h);
            fb.set_presented(self.playhead);
        }
        self.composite_scratch_u8 = bytes;

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
        let rendered = match self.render_cache_key(&seq, primary) {
            Some(key) => {
                let (w, h) = (pixels.width, pixels.height);
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

