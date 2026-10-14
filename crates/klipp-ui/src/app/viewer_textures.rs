//! The viewer's textures: frames uploaded to egui, the LUT and the GPU
//! texture, the filmstrip preview, and textures let go a few frames late (#890).

use super::*;

impl KlippApp {
    /// Show nearest filmstrip thumbnail as instant viewer preview while full-res decodes.
    pub(super) fn show_filmstrip_preview(&mut self, ctx: &egui::Context) {
        let playhead_secs = self.state.playhead.to_secs_f64();
        let seq = &self.state.sequences[self.state.active_sequence_idx];

        // Find the clip at the playhead
        let media_id = seq.tracks.iter()
            .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
            .find_map(|t| t.clip_at_timecode(self.state.playhead))
            .and_then(|(_, clip)| {
                clip.media_id().map(|mid| {
                    let source_secs = clip.source_secs_at(playhead_secs - clip.timeline_in.to_secs_f64(), seq.fps.as_f64());
                    (mid, source_secs)
                })
            });

        if let Some((mid, source_secs)) = media_id {
            if let Some(filmstrip) = self.state.filmstrip_textures.get(&mid) {
                if !filmstrip.is_empty() {
                    // Binary search for nearest filmstrip frame
                    let idx = match filmstrip.binary_search_by(|f| {
                        f.0.partial_cmp(&source_secs).unwrap_or(std::cmp::Ordering::Equal)
                    }) {
                        Ok(i) => i,
                        Err(i) => {
                            if i == 0 { 0 }
                            else if i >= filmstrip.len() { filmstrip.len() - 1 }
                            else {
                                let d_left = (filmstrip[i - 1].0 - source_secs).abs();
                                let d_right = (filmstrip[i].0 - source_secs).abs();
                                if d_left <= d_right { i - 1 } else { i }
                            }
                        }
                    };

                    // Get the filmstrip texture and create a clone for the viewer
                    let (_, ref thumb_tex) = filmstrip[idx];
                    let size = thumb_tex.size_vec2();
                    if size.x > 0.0 && size.y > 0.0 {
                        // Read the filmstrip frame pixels from thumbnail cache
                        if let Some(frames) = self.state.thumbnail_cache.filmstrip(&mid) {
                            if let Some(frame) = frames.get(idx) {
                                let fw = frame.width as usize;
                                let fh = frame.height as usize;
                                if frame.rgba.len() == fw * fh * 4 {
                                    let pixels: Vec<egui::Color32> = frame.rgba.chunks_exact(4)
                                        .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
                                        .collect();
                                    let image = egui::ColorImage { size: [fw, fh], pixels };
                                    // Upload as viewer texture — bilinear upscale handles the rest.
                                    // Must go through upload_texture_deferred: a size change on
                                    // viewer.video_texture (e.g. 720p thumbnail → 1080p frame)
                                    // would otherwise trip egui-wgpu's destroy-mid-submit race.
                                    let drops_slot = self.state.deferred_texture_drops_slot;
                                    upload_texture_deferred(
                                        ctx,
                                        &mut self.viewer.video_texture,
                                        &mut self.state.deferred_texture_drops[drops_slot],
                                        "klipp-video-preview",
                                        image,
                                        egui::TextureOptions::LINEAR,
                                    );
                                    self.state.showing_thumbnail_preview = true;
                                    ctx.request_repaint();
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    pub(super) fn upload_frame_to_egui(&mut self, ctx: &egui::Context) {
        let t_total = std::time::Instant::now();

        // ── Phase 1: minimal lock — take data, release immediately (~5µs) ──
        let frame_data = {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.swap_pending();
            self.state.render_cache_ranges = fb.render_cache_ranges.clone();
            if self.state.render_status != fb.render_status {
                self.state.render_status = fb.render_status.clone();
            }
            if self.state.frame_declines.as_slice() != fb.declines() {
                self.state.frame_declines = fb.declines().to_vec();
                crate::panels::viewer_declines::publish(ctx, &self.state.frame_declines);
            }
            // A frame the GPU stack wrote straight into the viewer's
            // texture: nothing to upload, the viewer samples it.
            self.viewer.gpu_frame_live = fb.gpu_output_live && fb.gpu_output.is_some();
            self.viewer.gpu_frame_size = (fb.gpu_output_width, fb.gpu_output_height);
            if !fb.dirty {
                return;
            }
            let w = fb.width as usize;
            let h = fb.height as usize;
            if fb.data.len() != w * h * 4 {
                fb.dirty = false;
                return;
            }
            fb.scope_preview.clear();
            fb.scope_preview_w = 0;
            fb.scope_preview_h = 0;
            fb.dirty = false;
            let fw = fb.width;
            let fh = fb.height;
            // Take ownership — zero copy, ~0µs
            let rgba = std::mem::take(&mut fb.data);
            let original = std::mem::take(&mut fb.original_data);
            let unstab = std::mem::take(&mut fb.unstab_data);
            let compare_effect = std::mem::take(&mut fb.compare_effect_data);
            (rgba, original, unstab, compare_effect, fw, fh, w, h)
            // lock released immediately — fb.data is now empty
        };
        let (mut rgba_bytes, original_bytes, unstab_bytes, compare_effect_bytes, fb_width, fb_height, w, h) = frame_data;

        // Full-res frame arrived — clear preview flags
        self.state.showing_thumbnail_preview = false;
        self.state.awaiting_seek_frame = false;

        // ── Phase 2: all expensive work outside lock ──

        // Capture reference frame for effect thumbnails
        let has_content = rgba_bytes.len() >= 64 && {
            let mid = rgba_bytes.len() / 2;
            let q1 = rgba_bytes.len() / 4;
            let q3 = 3 * rgba_bytes.len() / 4;
            rgba_bytes[q1] > 0 || rgba_bytes[mid] > 0 || rgba_bytes[q3] > 0
        };
        if has_content {
            self.state.effect_registry.set_reference_frame(&rgba_bytes, fb_width, fb_height);
        }

        // Compute curve histograms for the color panel
        if self.state.show_color_panel {
            let hist = klipp_color::scopes::compute_histogram(&rgba_bytes, fb_width, fb_height);
            self.state.curve_histograms = Some(hist);
        }

        // Before/After split compositing: left = original, right = graded
        if self.state.before_after_active && !original_bytes.is_empty()
            && original_bytes.len() == rgba_bytes.len()
        {
            let split_x = (self.state.split_position * w as f32) as usize;
            for y in 0..h {
                let row_start = y * w * 4;
                let copy_end = (split_x * 4).min(w * 4);
                rgba_bytes[row_start..row_start + copy_end]
                    .copy_from_slice(&original_bytes[row_start..row_start + copy_end]);
            }
        }

        // Stabilization compare split: left = raw (unstabilized), right = stabilized
        if self.state.stabilization_compare_active && !unstab_bytes.is_empty()
            && unstab_bytes.len() == rgba_bytes.len()
        {
            let split_x = (self.state.split_position * w as f32) as usize;
            for y in 0..h {
                let row_start = y * w * 4;
                let copy_end = (split_x * 4).min(w * 4);
                rgba_bytes[row_start..row_start + copy_end]
                    .copy_from_slice(&unstab_bytes[row_start..row_start + copy_end]);
            }
        }

        // Per-effect compare split: left = without the selected effect,
        // right = with it. Same byte-row copy as the other compare modes;
        // the engine has already produced compare_effect_data at the same
        // dimensions as the main frame.
        if self.state.split_compare_node.is_some() && !compare_effect_bytes.is_empty()
            && compare_effect_bytes.len() == rgba_bytes.len()
        {
            let split_x = (self.state.split_position * w as f32) as usize;
            for y in 0..h {
                let row_start = y * w * 4;
                let copy_end = (split_x * 4).min(w * 4);
                rgba_bytes[row_start..row_start + copy_end]
                    .copy_from_slice(&compare_effect_bytes[row_start..row_start + copy_end]);
            }
        }

        // Apply viewer overlays (multiple can be active simultaneously).
        // Order: false color first (replaces image), then zebras on top (stripes
        // over the color-mapped image give useful combined feedback).
        if self.state.viewer_overlays.false_color {
            let lut = &self.state.false_color_lut;
            for pixel in rgba_bytes.chunks_exact_mut(4) {
                let luma = (0.2126 * pixel[0] as f32 + 0.7152 * pixel[1] as f32 + 0.0722 * pixel[2] as f32) as usize;
                let idx = luma * 3;
                let mapped = &lut[idx..idx + 3];
                // Only replace if stop covers this luma (non-zero = covered)
                if mapped != [0, 0, 0] {
                    pixel[0] = mapped[0];
                    pixel[1] = mapped[1];
                    pixel[2] = mapped[2];
                }
            }
        }
        if self.state.viewer_overlays.zebras {
            let hi = self.prefs.viewer.zebra_high_threshold;
            let lo = self.prefs.viewer.zebra_low_threshold;
            let fc = self.state.frame_count as usize;
            for y in 0..h {
                for x in 0..w {
                    let idx = (y * w + x) * 4;
                    let r = rgba_bytes[idx];
                    let g = rgba_bytes[idx + 1];
                    let b = rgba_bytes[idx + 2];
                    let is_over = r > hi || g > hi || b > hi;
                    let is_under = r < lo || g < lo || b < lo;
                    if is_over || is_under {
                        // Animated diagonal stripes — different angles so high/low are
                        // visually distinct: high = 45° (x+y), low = 135° (x-y)
                        if is_over {
                            let stripe = (x + y + fc * 2) % 8 < 4;
                            if stripe {
                                rgba_bytes[idx] = 255;
                                rgba_bytes[idx + 1] = 0;
                                rgba_bytes[idx + 2] = 0;
                            }
                        } else {
                            let stripe = (x.wrapping_add(h).wrapping_sub(y) + fc * 2) % 8 < 4;
                            if stripe {
                                rgba_bytes[idx] = 0;
                                rgba_bytes[idx + 1] = 0;
                                rgba_bytes[idx + 2] = 255;
                            }
                        }
                    }
                }
            }
            self.state.frame_count = self.state.frame_count.wrapping_add(1);
        }
        // Focus peaking last — edge highlights on top of everything else.
        // Adapts the Sobel algorithm from apply_edge_detect() in klipp-effects/src/lib.rs:581-621.
        if self.state.viewer_overlays.focus_peaking {
            let sensitivity = self.prefs.viewer.focus_peaking_sensitivity;
            let [pr, pg, pb] = self.prefs.viewer.focus_peaking_color;
            // Threshold: sensitivity 0.0 → show all edges (threshold=0), 1.0 → only strongest (threshold≈255)
            let threshold = sensitivity * 255.0;
            // Compute luma into scratch buffer (Sobel needs neighbor access, can't modify in-place)
            // TODO: rayon parallelize y-loop if profiling shows stalls at 4K
            let mut luma = vec![0u8; w * h];
            for i in 0..w * h {
                let idx = i * 4;
                let r = rgba_bytes[idx] as f32;
                let g = rgba_bytes[idx + 1] as f32;
                let b = rgba_bytes[idx + 2] as f32;
                luma[i] = (0.2126 * r + 0.7152 * g + 0.0722 * b).clamp(0.0, 255.0) as u8;
            }
            // Sobel edge detection on luma, overlay peaking color where magnitude exceeds threshold
            let sobel_x: [[f32; 3]; 3] = [[-1.0, 0.0, 1.0], [-2.0, 0.0, 2.0], [-1.0, 0.0, 1.0]];
            let sobel_y: [[f32; 3]; 3] = [[-1.0, -2.0, -1.0], [0.0, 0.0, 0.0], [1.0, 2.0, 1.0]];
            for y in 0..h {
                for x in 0..w {
                    let mut gx = 0.0f32;
                    let mut gy = 0.0f32;
                    for ky in 0..3i32 {
                        for kx in 0..3i32 {
                            let sx = (x as i32 + kx - 1).clamp(0, w as i32 - 1) as usize;
                            let sy = (y as i32 + ky - 1).clamp(0, h as i32 - 1) as usize;
                            let l = luma[sy * w + sx] as f32;
                            gx += l * sobel_x[ky as usize][kx as usize];
                            gy += l * sobel_y[ky as usize][kx as usize];
                        }
                    }
                    let mag = (gx * gx + gy * gy).sqrt();
                    if mag > threshold {
                        // Blend peaking color by edge strength — strong edges glow
                        // brightly, weak edges fade to nothing
                        let alpha = ((mag - threshold) / (255.0 - threshold)).min(1.0);
                        let inv = 1.0 - alpha;
                        let idx = (y * w + x) * 4;
                        rgba_bytes[idx] = (rgba_bytes[idx] as f32 * inv + pr as f32 * alpha) as u8;
                        rgba_bytes[idx + 1] = (rgba_bytes[idx + 1] as f32 * inv + pg as f32 * alpha) as u8;
                        rgba_bytes[idx + 2] = (rgba_bytes[idx + 2] as f32 * inv + pb as f32 * alpha) as u8;
                    }
                }
            }
        }

        let t_convert = std::time::Instant::now();
        // Safety: Color32 is repr(C) [u8; 4], same as our RGBA layout.
        // Vec reinterpret: same alignment (1), element size (4), length and capacity divide evenly.
        let pixels = {
            let mut bytes = std::mem::ManuallyDrop::new(rgba_bytes.clone());
            let ptr = bytes.as_mut_ptr() as *mut egui::Color32;
            let len = bytes.len() / 4;
            let cap = bytes.capacity() / 4;
            unsafe { Vec::from_raw_parts(ptr, len, cap) }
        };
        let convert_ms = t_convert.elapsed().as_secs_f64() * 1000.0;

        // The monitor LUT's pass reads the same picture (#465).
        self.viewer.lut_source_bytes = self.viewer.lut_pass.is_some().then(|| (rgba_bytes.clone(), w as u32, h as u32));

        let t_texture = std::time::Instant::now();
        let image = egui::ColorImage {
            size: [w, h],
            pixels,
        };

        // Reuse existing texture handle when dimensions match; fall
        // through to a fresh alloc + deferred-drop when the frame
        // size changes (clip switch, skim → playback transitions).
        // Must go through upload_texture_deferred — a raw `set()`
        // with a size change triggers egui-wgpu's destroy-mid-submit
        // validation crash.
        let drops_slot = self.state.deferred_texture_drops_slot;
        upload_texture_deferred(
            ctx,
            &mut self.viewer.video_texture,
            &mut self.state.deferred_texture_drops[drops_slot],
            "klipp-video-frame",
            image,
            egui::TextureOptions::LINEAR,
        );
        let texture_ms = t_texture.elapsed().as_secs_f64() * 1000.0;

        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;
        if total_ms > 2.0 {
            tracing::info!(
                "PERF upload_frame: {w}x{h} total={total_ms:.1}ms (convert={convert_ms:.1}ms texture={texture_ms:.1}ms)"
            );
        }

        // Put frame data back so scopes can read it (~5µs pointer move)
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            // Only put back if engine hasn't written a newer frame
            if fb.data.is_empty() {
                fb.data = rgba_bytes;
                fb.original_data = original_bytes;
                fb.unstab_data = unstab_bytes;
                fb.compare_effect_data = compare_effect_bytes;
            }
        }
    }

    /// Lift any newly-decoded source frame from `source_frame_buffer`
    /// into an egui texture for the source viewer panel. Mirrors the
    /// Program viewer's `upload_frame_to_egui` but stripped to the
    /// essentials: no scopes, no compare buffers, no defer-drop ring
    /// (we re-use existing handles via `set` on size match, defer-
    /// drop only on size change). Steady-state cost: one mutex check
    /// + one generation compare per frame.
    pub(super) fn upload_source_frame_to_egui(&mut self, ctx: &egui::Context) {
        let (rgba, w, h, gen) = {
            let mut fb = self.source_frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            // Promote any pending engine write into `data`. `dirty`
            // gates whether this method actually does work below.
            fb.swap_pending();
            if !fb.dirty {
                return;
            }
            let w = fb.width as usize;
            let h = fb.height as usize;
            if fb.data.len() != w * h * 4 {
                fb.dirty = false;
                return;
            }
            fb.dirty = false;
            (std::mem::take(&mut fb.data), w, h, fb.generation)
        };
        // Skip uploads when this generation already landed — guards
        // against double-pumping when both source and program viewers
        // dirty in the same frame.
        if gen == self.state.source_live_texture_gen
            && self.state.source_live_texture.is_some()
        {
            return;
        }
        let Some(color_image) = crate::ui_util::checked_image([w, h], &rgba, "source viewer frame") else { return };
        let new_size = egui::vec2(w as f32, h as f32);
        let needs_alloc = match &self.state.source_live_texture {
            Some(tex) => tex.size_vec2() != new_size,
            None => true,
        };
        if needs_alloc {
            // Different dimensions: defer-drop the old handle (egui-wgpu
            // may still have a submit in flight), then allocate fresh.
            if let Some(old) = self.state.source_live_texture.take() {
                self.defer_drop_texture(old);
            }
            let tex = ctx.load_texture(
                "source_live", color_image, egui::TextureOptions::LINEAR,
            );
            self.state.source_live_texture = Some(tex);
        } else if let Some(tex) = self.state.source_live_texture.as_mut() {
            // Same dimensions: in-place data update reuses the GPU texture.
            tex.set(color_image, egui::TextureOptions::LINEAR);
        }
        self.state.source_live_texture_gen = gen;
    }

    /// Stash a `TextureHandle` so its underlying wgpu::Texture is held
    /// across the current frame's submit+present cycle. Actual drop
    /// happens two `update()`s later, at which point the GPU has
    /// finished every command buffer that could have referenced it.
    /// Call whenever a churn-site replaces an existing handle with a
    /// freshly-allocated one; dropping the old handle inline on
    /// eframe-wgpu trips `Texture has been destroyed` validation.
    pub(super) fn defer_drop_texture(&mut self, h: egui::TextureHandle) {
        let slot = self.state.deferred_texture_drops_slot;
        self.state.deferred_texture_drops[slot].push(h);
    }

    /// Refresh the letter-picker preview texture if the selected
    /// clip's resolved title-context has changed since the last paint.
    /// Runs from `render_inspector_panel` while the App still has
    /// `&mut self` access — texture lifecycle (creation + the
    /// `defer_drop_texture` call on the prior handle) stays in this
    /// layer. The inspector then reads `state.letter_picker_texture`
    /// without ever touching wgpu.
    ///
    /// Resolution flow:
    /// 1. No clip selected → drop the texture (defer-drop the old).
    /// 2. Selected clip resolves a Title via `picker_target_for_clip`
    ///    AND `(clip_id, layout_hash, w, h)` differs from the cache
    ///    key → render the title bitmap, upload, swap (defer-drop).
    /// 3. Same target + hash → no-op; texture is reused.
    /// 4. Selected clip with no title context → drop the texture.
    ///
    /// Frame dimensions are pinned at the inspector preview's fixed
    /// size (210×118 px) — no need to thread the rect down here.
    pub(super) fn refresh_letter_picker_texture(&mut self, ctx: &egui::Context) {
        const PREVIEW_W: u32 = 210;
        const PREVIEW_H: u32 = 118;

        let Some(selected_clip_id) = self.state.selected_clip else {
            if let Some(stale) = self.state.letter_picker_texture.take() {
                self.defer_drop_texture(stale);
            }
            self.state.letter_picker_texture_for = None;
            return;
        };

        // Resolve target + compute layout hash under one read of the
        // sequence; the rest of the function works off snapshots so
        // we don't hold the borrow across the texture upload.
        let target_info: Option<(klipp_core::ClipId, u64, klipp_timeline::TitleData)> = {
            let seq = self.state.sequence();
            let target_clip = crate::inspector::letter_picker::picker_target_for_clip(
                seq, selected_clip_id, self.state.playhead,
            );
            target_clip.and_then(|c| {
                c.title_data().map(|td| {
                    let hash = klipp_title::glyph_layout_hash(td, PREVIEW_W, PREVIEW_H);
                    (c.id, hash, td.clone())
                })
            })
        };

        let Some((title_clip_id, layout_hash, td)) = target_info else {
            // No title context for this selection — drop any leftover
            // texture so the inspector falls back to the spinbox flow.
            if let Some(stale) = self.state.letter_picker_texture.take() {
                self.defer_drop_texture(stale);
            }
            self.state.letter_picker_texture_for = None;
            return;
        };

        let new_key = (title_clip_id, layout_hash, PREVIEW_W, PREVIEW_H);
        if self.state.letter_picker_texture_for == Some(new_key)
            && self.state.letter_picker_texture.is_some()
        {
            return; // cache hit — texture stays.
        }

        // Cache miss: render bitmap, upload, defer-drop the old.
        // Per-paint TitleRenderer is wasteful but contained here —
        // refresh fires only when the layout hash changes.
        let mut tr = klipp_title::TitleRenderer::new();
        let bitmap = tr.render(&td, PREVIEW_W, PREVIEW_H);
        let img = egui::ColorImage::from_rgba_unmultiplied(
            [PREVIEW_W as usize, PREVIEW_H as usize],
            &bitmap,
        );
        let tex = ctx.load_texture(
            format!("hyperzoom_picker_{}", title_clip_id.0.as_simple()),
            img,
            egui::TextureOptions::LINEAR,
        );

        if let Some(stale) = self.state.letter_picker_texture.take() {
            self.defer_drop_texture(stale);
        }
        self.state.letter_picker_texture = Some(tex);
        self.state.letter_picker_texture_for = Some(new_key);
    }

    /// Extend the current deferred-drop bucket from an iterator of
    /// handles. Use for `HashMap::drain` / `Vec::drain` sites where
    /// many textures retire at once (clip switch, thumbnail cache
    /// invalidation, etc.).
    pub(super) fn defer_drop_textures(&mut self, it: impl IntoIterator<Item = egui::TextureHandle>) {
        let slot = self.state.deferred_texture_drops_slot;
        self.state.deferred_texture_drops[slot].extend(it);
    }

    /// Run the picture on screen through the monitor LUT set in
    /// Preferences (#465), into a texture the viewer shows in its place.
    /// On screen only: the frame buffer, scopes, measurements and
    /// exports keep the graded frame. Not while the viewer shows HDR: the
    /// LUT reads a display-encoded SDR signal.
    pub(super) fn refresh_viewer_lut(&mut self, frame: &mut eframe::Frame) {
        let changed = self.viewer.lut.follow(self.prefs.color.viewer_lut_path.as_deref());
        let Some(render_state) = frame.wgpu_render_state() else {
            self.viewer.lut_live = false;
            return;
        };
        if changed {
            if let Some(why) = &self.viewer.lut.error {
                self.state.toast.show(format!("Viewer LUT not applied: {why}"));
            }
            self.viewer.lut_pass = self.viewer.lut.lut.as_ref().map(|lut| {
                crate::viewer_lut::ViewerLutPass::new(&render_state.device, &render_state.queue, lut)
            });
            self.viewer.lut_live = false;
            // The parked frame is not uploaded again until it changes:
            // take it from the frame buffer for the new LUT.
            if self.viewer.lut_pass.is_some() {
                let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                let (bytes, w, h, _) = fb.latest_frame();
                self.viewer.lut_source_bytes = Some((bytes.to_vec(), w, h));
            }
        }
        let hdr = self.state.hdr_monitor.renders_hdr();
        let Some(pass) = self.viewer.lut_pass.as_mut().filter(|_| !hdr) else {
            self.viewer.lut_live = false;
            return;
        };
        let remade = if self.viewer.gpu_frame_live {
            // The GPU stack writes each frame into the same texture: run
            // the pass on every repaint while it is live.
            let source = {
                let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                fb.gpu_output.as_ref().map(|t| {
                    let view = t.create_view(&wgpu::TextureViewDescriptor {
                        format: Some(klipp_engine::texture::GPU_VIEWER_SAMPLE_FORMAT),
                        ..Default::default()
                    });
                    (view, (t.width(), t.height()))
                })
            };
            let Some((view, size)) = source else {
                self.viewer.lut_live = false;
                return;
            };
            pass.apply(&render_state.device, &render_state.queue, &view, size)
        } else if let Some((bytes, w, h)) = self.viewer.lut_source_bytes.take() {
            pass.apply_bytes(&render_state.device, &render_state.queue, &bytes, (w, h))
        } else if self.viewer.lut_live {
            // The parked frame has not changed: the output still holds it.
            false
        } else {
            // Back from the playing or HDR view with nothing new uploaded:
            // the parked frame is in the frame buffer.
            let (bytes, w, h) = {
                let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                let (bytes, w, h, _) = fb.latest_frame();
                (bytes.to_vec(), w, h)
            };
            if !pass.apply_bytes(&render_state.device, &render_state.queue, &bytes, (w, h)) && pass.output_view().is_none() {
                return;
            }
            true
        };
        if remade || self.viewer.lut_texture_id.is_none() {
            let Some(view) = pass.output_view() else { return };
            let mut renderer = render_state.renderer.write();
            // Free the old id before its view drops, as for
            // `gpu_texture_id` (see `refresh_gpu_viewer_texture`).
            if let Some(old) = self.viewer.lut_texture_id.take() {
                renderer.free_texture(&old);
                self.viewer.lut_texture_view = None;
            }
            self.viewer.lut_texture_id = Some(renderer.register_native_texture(&render_state.device, &view, wgpu::FilterMode::Linear));
            self.viewer.lut_texture_view = Some(view);
        }
        self.viewer.lut_live = true;
    }

    pub(super) fn refresh_gpu_viewer_texture(&mut self, frame: &mut eframe::Frame) {
        // Snapshot the current gpu_output handle + generation.
        let snapshot = {
            let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.gpu_output.as_ref().map(|t| (
                std::sync::Arc::clone(t),
                fb.gpu_output_generation,
            ))
        };
        let Some((texture, gen)) = snapshot else { return; };
        if gen == self.viewer.gpu_texture_gen && self.viewer.gpu_texture_id.is_some() {
            return; // already registered this exact texture
        }
        let Some(render_state) = frame.wgpu_render_state() else { return; };

        // Build a view from the Arc'd texture. wgpu::Texture::create_view
        // takes &self, so the Arc holder keeps the underlying texture
        // alive — but the view itself is a separate resource.
        // Through the sRGB view: the texture holds display-encoded bytes,
        // and egui's shader takes whatever it samples as linear light and
        // encodes it for the screen. A plain view encoded the picture a
        // second time, so playback looked washed out beside the paused
        // frame and the export (#455).
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(klipp_engine::texture::GPU_VIEWER_SAMPLE_FORMAT),
            ..Default::default()
        });

        let mut renderer = render_state.renderer.write();
        // Order matters here. egui-wgpu's `register_native_texture`
        // creates an internal BindGroup that records the new view's
        // wgpu storage `Id` BUT does NOT take a refcounted clone —
        // wgpu 22's BindGroup tracks resources by Id only. Free the
        // previous TextureId FIRST so egui-wgpu drops the old
        // BindGroup; only after that is it safe to drop the previous
        // `gpu_texture_view` (handle drop tells wgpu_core to mark
        // the storage slot Vacant). If we dropped the view before
        // the BindGroup, the next paint that samples the old id
        // panics with "TextureView[Id(N,vk)] does not exist" — this
        // was the recurring "first-frame post compound / first-grade-
        // tweak" crash documented as GPU-COMPOUND-PANIC.
        if let Some(old_id) = self.viewer.gpu_texture_id.take() {
            renderer.free_texture(&old_id);
            self.viewer.gpu_texture_view = None;
        }
        let new_id = renderer.register_native_texture(
            &render_state.device,
            &view,
            wgpu::FilterMode::Linear,
        );
        // Keep the new view alive for as long as the registered id
        // is used. Replaces the previous view (already dropped above
        // after the corresponding `free_texture` call).
        self.viewer.gpu_texture_view = Some(view);
        self.viewer.gpu_texture_id = Some(new_id);
        self.viewer.gpu_texture_gen = gen;
        tracing::debug!("GPU_VIEWER registered id={:?} gen={}", new_id, gen);
    }
}

/// Upload `image` into `slot`, reusing the existing handle only when
/// dimensions match exactly. On a size change, allocate a fresh
/// handle and push the old one into `deferred_drops`. Mandatory on
/// eframe-wgpu: `TextureHandle::set` with a different-sized image
/// causes egui to replace (and destroy) the underlying wgpu::Texture
/// internally, which races the current frame's submit.
pub(super) fn upload_texture_deferred(
    ctx: &egui::Context,
    slot: &mut Option<egui::TextureHandle>,
    deferred_drops: &mut Vec<egui::TextureHandle>,
    name: &str,
    image: egui::ColorImage,
    options: egui::TextureOptions,
) {
    // Nothing to show — the engine reset with no project, a frame of no
    // size: the old picture goes, and no texture is made. A zero-size
    // one fails in wgpu, and every redraw that binds it after is another
    // validation error, until GPU effects are switched off for the
    // session (#645).
    if image.size[0] == 0 || image.size[1] == 0 {
        if let Some(old) = slot.take() {
            tracing::debug!("TEX_DEFER_DROP id={:?} name={} reason=empty", old.id(), name);
            deferred_drops.push(old);
        }
        return;
    }
    let new_size = egui::vec2(image.size[0] as f32, image.size[1] as f32);
    match slot {
        Some(tex) if tex.size_vec2() == new_size => {
            tex.set(image, options);
        }
        _ => {
            if let Some(old) = slot.take() {
                tracing::debug!(
                    "TEX_DEFER_DROP id={:?} name={} reason=size_mismatch",
                    old.id(), name,
                );
                deferred_drops.push(old);
            }
            let h = alloc_texture_logged(ctx, name, image, options);
            *slot = Some(h);
        }
    }
}

/// Wrapper around `ctx.load_texture` that emits a `TEX_ALLOC` trace.
/// Used to correlate `egui_texid_Managed(N)` panic IDs with the call
/// site that allocated them — the panic only gives the numeric ID, no
/// context. When a crash names ID=N, grep `TEX_ALLOC id=...(N)` in the
/// log to find which name/site was responsible.
pub(crate) fn alloc_texture_logged(
    ctx: &egui::Context,
    name: &str,
    image: egui::ColorImage,
    options: egui::TextureOptions,
) -> egui::TextureHandle {
    let h = ctx.load_texture(name, image, options);
    tracing::debug!("TEX_ALLOC id={:?} name={}", h.id(), name);
    h
}

#[cfg(test)]
mod empty_texture_tests {
    use super::upload_texture_deferred;

    /// #645: an empty frame — the engine reset with no project — takes the
    /// old picture away and makes no texture. A zero-size one failed in
    /// wgpu and every redraw after was a validation error, until GPU
    /// effects were switched off for the session.
    #[test]
    fn an_empty_frame_clears_the_picture_and_makes_no_texture() {
        let ctx = egui::Context::default();
        let mut slot = None;
        let mut drops = Vec::new();
        let image = |w: usize, h: usize| egui::ColorImage { size: [w, h], pixels: vec![egui::Color32::BLACK; w * h] };
        upload_texture_deferred(&ctx, &mut slot, &mut drops, "t", image(4, 2), egui::TextureOptions::LINEAR);
        assert!(slot.is_some());
        let before = ctx.tex_manager().read().allocated().count();
        upload_texture_deferred(&ctx, &mut slot, &mut drops, "t", image(0, 0), egui::TextureOptions::LINEAR);
        assert!(slot.is_none(), "the old picture is gone");
        assert_eq!(drops.len(), 1, "dropped after the frame that may still bind it");
        assert_eq!(ctx.tex_manager().read().allocated().count(), before, "no texture made for nothing");
        upload_texture_deferred(&ctx, &mut slot, &mut drops, "t", image(4, 2), egui::TextureOptions::LINEAR);
        assert!(slot.is_some(), "the next real frame shows");
    }
}
