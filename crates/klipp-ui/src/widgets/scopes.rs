use crate::state::{ScopeMode, ScopeScale};
use crate::theme::KlippTheme;
use klipp_engine::SharedFrameBuffer;
use std::time::Instant;

// `scope_ffmpeg_enabled` removed during Phase 8 — the engine always
// renders via libavfilter now and the legacy in-tree CPU fallback is
// gone. See RECOVERY_SCOPES_INHOUSE.md for the legacy recovery path.

/// Scope render result — either from GPU engine or CPU direct rendering.
enum ScopeImage {
    Ready(egui::ColorImage),
}

/// Persistent state for scope rendering (texture cache + throttling).
pub struct ScopeState {
    /// One cached texture per scope mode — switching is instant.
    textures: [Option<egui::TextureHandle>; 4], // Waveform, Parade, Vectorscope, Histogram
    /// Frame generation counter used for each cached texture (from FrameBuffer.generation).
    frame_generations: [u64; 4],
    /// Render sizes used for each cached texture.
    render_sizes: [(usize, usize); 4],
    /// Current scale mode (synced from AppState each frame).
    pub scale: ScopeScale,
    /// Enable horizontal low pass filter on scope output.
    pub low_pass: bool,
    /// Enable bloom on dense regions.
    pub bloom: bool,
    /// R/G/B channel pill states (synced from AppState each frame).
    pub show_r: bool,
    pub show_g: bool,
    pub show_b: bool,
    /// Quality setting (synced from AppState each frame).
    pub quality: klipp_core::ScopeQuality,
    /// Incremented when any scope setting changes (triggers re-render).
    pub settings_generation: u64,
    last_settings_generation: u64,
    /// Throttle: last time a scope render completed.
    last_render_time: Option<Instant>,
}

fn mode_index(mode: ScopeMode) -> usize {
    match mode {
        ScopeMode::Waveform => 0,
        ScopeMode::Parade => 1,
        ScopeMode::Vectorscope => 2,
        ScopeMode::Histogram => 3,
    }
}

impl ScopeState {
    /// Bump the settings generation counter to trigger a re-render.
    pub fn increment_settings_generation(&mut self) {
        self.settings_generation = self.settings_generation.wrapping_add(1);
    }
}

impl Default for ScopeState {
    fn default() -> Self {
        Self {
            textures: [None, None, None, None],
            frame_generations: [u64::MAX; 4], // MAX ensures first frame always triggers render
            render_sizes: [(0, 0); 4],
            scale: ScopeScale::TenBit,
            low_pass: false,
            bloom: true,
            show_r: true,
            show_g: true,
            show_b: true,
            quality: klipp_core::ScopeQuality::Medium,
            settings_generation: 0,
            last_settings_generation: u64::MAX,
            last_render_time: None,
        }
    }
}

// ── Scale system constants ─────────────────────────────────────────────
pub(crate) const AXIS_W: f32 = 40.0;
pub(crate) const PAD_TOP: f32 = 14.0;
pub(crate) const PAD_BOTTOM: f32 = 16.0;
pub(crate) const PAD_RIGHT: f32 = 8.0;


fn val_to_y(val: f32, rect: &egui::Rect, scale: ScopeScale) -> f32 {
    let max = scale.max_val();
    let draw_h = rect.height() - PAD_TOP - PAD_BOTTOM;
    rect.min.y + PAD_TOP + (1.0 - val / max) * draw_h
}

/// Return major tick values for the given scale mode.
fn major_ticks(scale: ScopeScale) -> Vec<u32> {
    match scale {
        ScopeScale::TenBit => vec![0, 128, 256, 384, 512, 640, 768, 896, 1023],
        _ => (0..=10).map(|i| i * 10).collect(),
    }
}

pub(crate) fn draw_scale(ui: &egui::Ui, rect: egui::Rect, scale: ScopeScale, is_parade: bool, section_count: usize, theme: &KlippTheme) {
    let painter = ui.painter_at(rect);
    // Graticule colour. The theme's `scope_grid` is blended 50%
    // toward classic CRT-graticule gold (#e8a820) so guides land
    // squarely in the saturated yellow-amber family DaVinci /
    // Resolve / FCP X all use, regardless of the theme's polarity.
    // Bumped from the post-Phase-2 30% / #c89060 (which still read
    // too brown / not gold enough against dense traces) to 50% /
    // #e8a820 per the 2026-05-09 user reference. Alpha + stroke are
    // also bumped (see major/minor sites below) so the lines are
    // visibly the right colour rather than the right colour but
    // too faint to perceive.
    let grid = theme.scope_grid;
    let label_color = theme.scope_label;
    let warm_amber = |a: u8| -> egui::Color32 {
        let t = 0.50_f32;
        let r = ((grid.r() as f32) * (1.0 - t) + 232.0 * t) as u8;
        let g = ((grid.g() as f32) * (1.0 - t) + 168.0 * t) as u8;
        let b = ((grid.b() as f32) * (1.0 - t) + 32.0 * t) as u8;
        egui::Color32::from_rgba_unmultiplied(r, g, b, a)
    };
    let font = egui::FontId::monospace(9.0);

    let major_vals: Vec<f32> = major_ticks(scale).into_iter().map(|v| v as f32).collect();
    let minor_step: f32 = match scale {
        ScopeScale::TenBit => 32.0,
        _ => 5.0,
    };

    // Minor grid lines — pushed subtler (alpha 10 → 8, tick 64 → 48) so
    // major lines dominate and the hierarchy reads cleanly.
    let max = scale.max_val();
    let mut v = 0.0;
    while v <= max {
        let y = val_to_y(v, &rect, scale);
        if !major_vals.iter().any(|&mv| (mv - v).abs() < 0.1) {
            painter.line_segment(
                [egui::pos2(rect.min.x + AXIS_W, y), egui::pos2(rect.max.x, y)],
                egui::Stroke::new(0.5_f32, warm_amber(8)),
            );
            // 3px tick on axis
            painter.line_segment(
                [egui::pos2(rect.min.x + AXIS_W - 3.0, y), egui::pos2(rect.min.x + AXIS_W, y)],
                egui::Stroke::new(0.5_f32, warm_amber(48)),
            );
        }
        v += minor_step;
    }

    // Major grid lines + labels — pushed thicker (stroke 0.5 → 1.5)
    // and brighter (alpha 110 → 170, tick 215 → 235) per the
    // 2026-05-09 user reference so the IRE 0/50/100 / 10-bit
    // 256/512/768 / dB anchors read at a glance even through dense
    // traces, in the saturated gold colour real CRT graticules use.
    for &val in &major_vals {
        let y = val_to_y(val, &rect, scale);
        painter.line_segment(
            [egui::pos2(rect.min.x + AXIS_W, y), egui::pos2(rect.max.x, y)],
            egui::Stroke::new(1.5_f32, warm_amber(170)),
        );
        // 6px tick on axis
        painter.line_segment(
            [egui::pos2(rect.min.x + AXIS_W - 6.0, y), egui::pos2(rect.min.x + AXIS_W, y)],
            egui::Stroke::new(1.5_f32, warm_amber(235)),
        );
        // Label — right-aligned to axis, 0 sits BELOW (TOP baseline), others sit ABOVE (BOTTOM baseline)
        let label = format!("{}", val as u32);
        let align = if val < 0.1 { egui::Align2::RIGHT_TOP } else { egui::Align2::RIGHT_BOTTOM };
        painter.text(egui::pos2(rect.min.x + AXIS_W - 8.0, y), align, &label, font.clone(), label_color);
    }

    // Axis separator — 1px vertical at x=AXIS_W, rgba(255,255,255,0.08)
    painter.line_segment(
        [egui::pos2(rect.min.x + AXIS_W, rect.min.y), egui::pos2(rect.min.x + AXIS_W, rect.max.y - PAD_BOTTOM)],
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20)),
    );

    // Bottom baseline — horizontal at y=(height-PAD_BOTTOM), rgba(255,255,255,0.08)
    let bottom_y = rect.max.y - PAD_BOTTOM;
    painter.line_segment(
        [egui::pos2(rect.min.x + AXIS_W, bottom_y), egui::pos2(rect.max.x, bottom_y)],
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20)),
    );

    // Bottom ticks — DaVinci-style dense ruler row sitting INSIDE
    // the trace area, drawn upward from the bottom baseline so they
    // read as graticule marks etched into the instrument glass
    // rather than as marks dangling below the trace. For parade,
    // tick within each of the 3 sections separately so the
    // section-divider gaps stay clean.
    //
    // Density bumped 2.5× per the 2026-05-09 user reference: 20
    // ticks/section → 50 ticks/section (every 2 % of section width
    // instead of every 5 %). Major every 10 % (every 5th tick),
    // minor between. Brightness bumped to alpha 230/110 — the
    // ruler row should be clearly visible at typical dock sizes,
    // matching DaVinci's bottom-edge tick density.
    let trace_x0 = rect.min.x + AXIS_W;
    let trace_x1 = rect.max.x - PAD_RIGHT;
    let trace_w = trace_x1 - trace_x0;
    let n_sections: usize = if is_parade { section_count.max(1) } else { 1 };
    let gap = if is_parade { 2.0_f32 } else { 0.0_f32 };
    let section_w = (trace_w - gap * (n_sections.saturating_sub(1)) as f32)
        / n_sections.max(1) as f32;
    let ticks_per_section: usize = 50;
    for s in 0..n_sections {
        let sx0 = trace_x0 + s as f32 * (section_w + gap);
        for i in 0..=ticks_per_section {
            let frac = i as f32 / ticks_per_section as f32;
            let x = sx0 + frac * section_w;
            let is_major = i % 5 == 0;
            let tick_h = if is_major { 6.0 } else { 3.0 };
            let color = if is_major { warm_amber(230) } else { warm_amber(110) };
            painter.line_segment(
                [egui::pos2(x, bottom_y), egui::pos2(x, bottom_y - tick_h)],
                egui::Stroke::new(1.0_f32, color),
            );
        }
    }

    // Parade section separators — rgba(255,255,255,0.05) within trace area
    if is_parade && section_count == 3 {
        let third = trace_w / 3.0;
        for i in 1..3 {
            let x = rect.min.x + AXIS_W + third * i as f32;
            painter.line_segment(
                [egui::pos2(x, rect.min.y + PAD_TOP), egui::pos2(x, bottom_y)],
                egui::Stroke::new(0.5_f32, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 13)),
            );
        }
    }
}

/// Apply a CRT scan-texture overlay — every other row darkens by a
/// small fraction. The goal is "very subtle row structure" — not
/// obvious stripes, just enough to break the sterile mathematically-
/// clean look without distracting from the trace. 4 % per-row
/// darkening (×0.96) reads as texture, not as banding.
///
/// Cheap: one multiply per pixel on alternating rows. No tier-gating
/// today — the effect is subtle enough that the perf cost (~1 µs at
/// typical scope sizes) doesn't justify a quality knob.
///
/// Part of the "energetic imperfection" direction (see
/// feedback_scope_realism_through_imperfection.md): realism comes
/// from non-uniformity, not from more blur.
pub(crate) fn apply_scanline_texture(image: &mut egui::ColorImage) {
    let w = image.size[0];
    let h = image.size[1];
    let mul: f32 = 0.96;
    for y in (1..h).step_by(2) {
        let row_start = y * w;
        for x in 0..w {
            let p = image.pixels[row_start + x];
            image.pixels[row_start + x] = egui::Color32::from_rgb(
                ((p.r() as f32) * mul) as u8,
                ((p.g() as f32) * mul) as u8,
                ((p.b() as f32) * mul) as u8,
            );
        }
    }
}

/// Apply 5-tap horizontal Gaussian low pass filter to a ColorImage.
fn apply_low_pass_filter(image: &mut egui::ColorImage) {
    let w = image.size[0];
    let h = image.size[1];
    let weights = [0.05_f32, 0.20, 0.50, 0.20, 0.05];
    let src = image.pixels.clone();
    for y in 0..h {
        for x in 2..w.saturating_sub(2) {
            let idx = x + y * w;
            let mut r = 0.0_f32;
            let mut g = 0.0_f32;
            let mut b = 0.0_f32;
            for (di, &wt) in weights.iter().enumerate() {
                let si = idx - 2 + di;
                r += src[si].r() as f32 * wt;
                g += src[si].g() as f32 * wt;
                b += src[si].b() as f32 * wt;
            }
            image.pixels[idx] = egui::Color32::from_rgb(r as u8, g as u8, b as u8);
        }
    }
}

/// Show the complete scope panel with header, scale, and traces.
pub fn show_scopes(
    ui: &mut egui::Ui,
    state: &mut ScopeState,
    mode: ScopeMode,
    frame_buffer: &SharedFrameBuffer,
    quality: klipp_core::ScopeQuality,
    skin_tone_angle: f32,
    is_playing: bool,
    scope_cursor: Option<crate::state::ScopeCursorData>,
    show_cursor: bool,
    theme: &KlippTheme,
) {
    let render_h: usize = 512;

    let mi = mode_index(mode);

    // ── Check for GPU-rendered scope texture from engine ──────────────────
    // Minimize mutex hold time: snapshot data, drop lock, then render CPU scopes outside lock
    let scope_result = {
        // Read the engine-published per-mode scope texture from the
        // frame buffer. The legacy CPU fallback (ScopeAction::CpuRender)
        // was removed in Phase 8 — the FFmpeg path is the sole renderer.
        // On a never-rendered-yet mode (first tab visit) we return None
        // so the dock displays its previously cached texture (or nothing)
        // until the engine catches up on its next tick.
        let _ = (skin_tone_angle, quality); // legacy CPU params; kept on the signature for API stability
        let fb = frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
        if fb.width == 0 || fb.height == 0 || fb.data.is_empty() {
            None
        } else {
            let gen = fb.generation;
            let tex_none = state.textures[mi].is_none();
            let gen_diff = state.frame_generations[mi] != gen;
            let size_diff = state.render_sizes[mi] != (fb.width as usize, render_h);
            let settings_diff = state.last_settings_generation != state.settings_generation;
            let stale = tex_none || gen_diff || size_diff || settings_diff;
            let throttled = is_playing && state.last_render_time
                .map_or(false, |t| t.elapsed().as_millis() < 80);
            if stale && !throttled {
                let map_entry = fb.scope_textures.get(&(mi as u8));
                let mode_tex = map_entry.filter(|e| !e.data.is_empty() && e.w > 0);
                if let Some(entry) = mode_tex {
                    let pixels: Vec<egui::Color32> = entry.data.chunks_exact(4)
                        .map(|c| egui::Color32::from_rgba_premultiplied(c[0], c[1], c[2], c[3]))
                        .collect();
                    let image = egui::ColorImage {
                        size: [entry.w as usize, entry.h as usize],
                        pixels,
                    };
                    Some((ScopeImage::Ready(image), gen))
                } else if !fb.scope_texture.is_empty()
                    && fb.scope_texture_w > 0
                    && fb.scope_texture_mode == mi as u8
                {
                    // Back-compat singular fields (still populated by
                    // `write_scope_tex_to_fb` for the main-dock back-
                    // compat path during the multi-mode rollout).
                    let pixels: Vec<egui::Color32> = fb.scope_texture.chunks_exact(4)
                        .map(|c| egui::Color32::from_rgba_premultiplied(c[0], c[1], c[2], c[3]))
                        .collect();
                    let image = egui::ColorImage {
                        size: [fb.scope_texture_w as usize, fb.scope_texture_h as usize],
                        pixels,
                    };
                    Some((ScopeImage::Ready(image), gen))
                } else {
                    None
                }
            } else {
                None
            }
        }
    };

    if let Some((scope_img, gen)) = scope_result {
        state.frame_generations[mi] = gen;
        state.last_settings_generation = state.settings_generation;
        state.last_render_time = Some(Instant::now());
        for idx in 0..4 {
            if idx != mi { state.frame_generations[idx] = u64::MAX; }
        }

        let mut image = match scope_img {
            ScopeImage::Ready(img) => img,
        };
        state.render_sizes[mi] = (image.size[0], image.size[1]);

        // Post-processing: threshold bloom (extract bright → gaussian blur → additive blend).
        // Now applied to vectorscope as well — chroma vector clouds
        // gain MORE from bloom than waveforms do (per the phosphor
        // audit). Tuning is more aggressive than the legacy values:
        // lower threshold catches mid-density energy, larger radius
        // softens it across more pixels, higher strength makes the
        // glow visible without needing to crank monitor brightness.
        if state.bloom {
            let w = image.size[0];
            let h = image.size[1];
            // Tightened to a thin inner-glow pass (was a broad
            // front-lit halo). Three changes from the previous
            // 0.20 / 0.65 / 18 tuning, all driven by the same
            // perceptual goal: bloom should look EMITTED FROM
            // INSIDE the densest filament cores, not painted on
            // top of the trace.
            //
            //   * Threshold 0.20 → 0.55 — only fires on the very
            //     brightest pixels (sparse hot intersections).
            //     Below that, the trace's own intensity ramp is
            //     doing the work; bloom would just smudge it.
            //   * Strength 0.65 → 0.18 — even where bloom fires,
            //     the contribution is a quiet additive glow, not
            //     a wash. Combined with the smaller radius this
            //     reads as core-light leaking 2-3 px outward,
            //     not as a halo painted around the trace.
            //   * Radius 18 → 5 — keeps glow LOCAL to the hot
            //     cores. The previous 18-px radius was the main
            //     contributor to the "broadly luminous" feel —
            //     it spread bright-pixel energy 36 px across the
            //     scope, painting whole regions with light.
            let threshold = 0.55_f32;
            let bloom_strength = 0.18_f32;
            let radius = 5usize;

            // 1. Extract bright pass
            let mut bright: Vec<[f32; 3]> = image.pixels.iter().map(|p| {
                let r = p.r() as f32 / 255.0;
                let g = p.g() as f32 / 255.0;
                let b = p.b() as f32 / 255.0;
                let lum = r * 0.3 + g * 0.6 + b * 0.1;
                if lum > threshold {
                    [r, g, b]
                } else {
                    [0.0, 0.0, 0.0]
                }
            }).collect();

            // 2. Separable Gaussian blur (horizontal then vertical)
            for _pass in 0..1 {
                // Horizontal
                let src = bright.clone();
                for y in 0..h {
                    for x in radius..w.saturating_sub(radius) {
                        let mut sum = [0.0_f32; 3];
                        let mut wt_sum = 0.0_f32;
                        for dx in 0..=radius * 2 {
                            let sx = x + dx - radius;
                            let d = (dx as f32 - radius as f32).abs();
                            let wt = (-d * d / (2.0 * (radius as f32 * 0.5).powi(2))).exp();
                            sum[0] += src[y * w + sx][0] * wt;
                            sum[1] += src[y * w + sx][1] * wt;
                            sum[2] += src[y * w + sx][2] * wt;
                            wt_sum += wt;
                        }
                        bright[y * w + x] = [sum[0] / wt_sum, sum[1] / wt_sum, sum[2] / wt_sum];
                    }
                }
                // Vertical
                let src = bright.clone();
                for x in 0..w {
                    for y in radius..h.saturating_sub(radius) {
                        let mut sum = [0.0_f32; 3];
                        let mut wt_sum = 0.0_f32;
                        for dy in 0..=radius * 2 {
                            let sy = y + dy - radius;
                            let d = (dy as f32 - radius as f32).abs();
                            let wt = (-d * d / (2.0 * (radius as f32 * 0.5).powi(2))).exp();
                            sum[0] += src[sy * w + x][0] * wt;
                            sum[1] += src[sy * w + x][1] * wt;
                            sum[2] += src[sy * w + x][2] * wt;
                            wt_sum += wt;
                        }
                        bright[y * w + x] = [sum[0] / wt_sum, sum[1] / wt_sum, sum[2] / wt_sum];
                    }
                }
            }

            // 3. Additive blend back
            for (pixel, bloom) in image.pixels.iter_mut().zip(bright.iter()) {
                let r = (pixel.r() as f32 / 255.0 + bloom[0] * bloom_strength).min(1.0);
                let g = (pixel.g() as f32 / 255.0 + bloom[1] * bloom_strength).min(1.0);
                let b = (pixel.b() as f32 / 255.0 + bloom[2] * bloom_strength).min(1.0);
                *pixel = egui::Color32::from_rgb(
                    (r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8,
                );
            }
        }

        // Post-processing: horizontal low pass filter (5-tap gaussian)
        if state.low_pass && mode != ScopeMode::Vectorscope {
            apply_low_pass_filter(&mut image);
        }

        // Post-processing: subtle CRT scanline overlay. Always on —
        // the effect is small enough (4% per-row darkening on alternating
        // rows) that it reads as texture rather than as stripes. Adds
        // analog character to all four scope modes.
        apply_scanline_texture(&mut image);

        let new_size = egui::vec2(image.size[0] as f32, image.size[1] as f32);
        match state.textures[mi] {
            Some(ref mut tex) if tex.size_vec2() == new_size => {
                tex.set(image, egui::TextureOptions::LINEAR);
            }
            _ => {
                // Size change forces a fresh allocation; the old handle
                // is dropped inline (no access to AppState's deferred
                // ring from here). If this site starts producing
                // texture-destroyed panics, plumb the ring through.
                let name = format!("scope_{mi}");
                let h = crate::app::alloc_texture_logged(ui.ctx(), &name, image, egui::TextureOptions::LINEAR);
                state.textures[mi] = Some(h);
            }
        }
    }

    // Display the cached texture with scale overlay
    let available = ui.available_size();
    if let Some(tex) = &state.textures[mi] {
        // Only drive a continuous repaint while playback is producing
        // new frames. When paused/idle, scope data only changes on
        // grade/param updates — those emit their own repaint via
        // SequenceUpdated, so we don't need a 60 fps idle loop.
        if is_playing {
            ui.ctx().request_repaint();
        }

        let is_vector = mode == ScopeMode::Vectorscope;
        let (full_rect, _) = ui.allocate_exact_size(available, egui::Sense::hover());

        // Pure #000000 (NOT `theme.bg_void` which is `#0a0a0c`). The
        // FFmpeg scope texture renders its interior in pure black, and
        // a near-black-but-not-quite-black surround reads as a visible
        // grey halo against the texture's true black — the axis-label
        // column, the trace padding, and the strip above/below all
        // pop out as "grey border" otherwise.
        ui.painter().rect_filled(full_rect, 0.0, egui::Color32::BLACK);

        if is_vector {
            // Vectorscope: centered square, no scale
            let side = full_rect.width().min(full_rect.height());
            let offset_x = (full_rect.width() - side) / 2.0;
            let offset_y = (full_rect.height() - side) / 2.0;
            let img_rect = egui::Rect::from_min_size(
                egui::pos2(full_rect.min.x + offset_x, full_rect.min.y + offset_y),
                egui::vec2(side, side),
            );
            ui.painter().image(tex.id(), img_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE);

            // Vectorscope graticule — rainbow hue ring + range circles +
            // crosshair + skin-tone I-line + target boxes + labels.
            // Drawn entirely on the UI side so the same broadcast-style
            // look works regardless of whether the underlying scope
            // texture came from the in-tree GPU/CPU renderer or
            // libavfilter (which paints `graticule=none` here).
            //
            // Match the in-tree CPU `render_vectorscope` baked graticule:
            // hue gradient at 100% radius, subtle inner rings, center
            // crosshair, amber skin-tone line with end label, and target
            // boxes at the six broadcast chroma positions.
            let painter = ui.painter();
            let label_font = egui::FontId::monospace(8.0);
            let label_color = theme.scope_label;
            let cx = img_rect.center().x;
            let cy = img_rect.center().y;
            let r = side / 2.0 - 4.0;

            // 1. Inner range rings at 25 / 50 / 75 % — subtle so they
            // don't compete with the trace.
            let inner_ring_color = egui::Color32::from_rgba_unmultiplied(
                label_color.r(), label_color.g(), label_color.b(), 40,
            );
            for frac in [0.25_f32, 0.50, 0.75] {
                painter.circle_stroke(
                    egui::pos2(cx, cy),
                    r * frac,
                    egui::Stroke::new(0.6_f32, inner_ring_color),
                );
            }

            // 2. Rainbow hue ring at 100% — paints the outer circle as
            // 120 short arc segments, each tinted by its angular hue.
            // This is the signature broadcast/Resolve look that's
            // missing when libavfilter doesn't paint a graticule.
            let segments = 120;
            for i in 0..segments {
                let a0 = (i as f32) / (segments as f32) * std::f32::consts::TAU;
                let a1 = ((i + 1) as f32) / (segments as f32) * std::f32::consts::TAU;
                // Hue at angle 0 = red, increasing counter-clockwise.
                let hue = (a0 / std::f32::consts::TAU).fract();
                let (rh, gh, bh) = hue_to_rgb(hue);
                let seg_color = egui::Color32::from_rgba_unmultiplied(
                    (rh * 255.0) as u8,
                    (gh * 255.0) as u8,
                    (bh * 255.0) as u8,
                    140,
                );
                let p0 = egui::pos2(cx + a0.cos() * r, cy + a0.sin() * r);
                let p1 = egui::pos2(cx + a1.cos() * r, cy + a1.sin() * r);
                painter.line_segment([p0, p1], egui::Stroke::new(1.4_f32, seg_color));
            }

            // 3. Center crosshair — very subtle reference axes across
            // the full diameter so the eye can find chroma null.
            let cross_color = egui::Color32::from_rgba_unmultiplied(
                label_color.r(), label_color.g(), label_color.b(), 30,
            );
            painter.line_segment(
                [egui::pos2(cx - r, cy), egui::pos2(cx + r, cy)],
                egui::Stroke::new(0.5_f32, cross_color),
            );
            painter.line_segment(
                [egui::pos2(cx, cy - r), egui::pos2(cx, cy + r)],
                egui::Stroke::new(0.5_f32, cross_color),
            );

            // 4. Skin-tone I-line — amber tinted, from centre at the
            // broadcast-standard 123° angle (centre → upper-left between
            // Yl and R). Stops short of the hue ring so the "I" label
            // sits inside the ring.
            let skin_angle_deg = 123.0_f32;
            let skin_rad = (skin_angle_deg - 90.0).to_radians();
            let dx = skin_rad.sin();
            let dy = -skin_rad.cos();
            let i_line_len = r * 0.92;
            let i_color = egui::Color32::from_rgb(220, 170, 80);
            painter.line_segment(
                [
                    egui::pos2(cx, cy),
                    egui::pos2(cx + dx * i_line_len, cy + dy * i_line_len),
                ],
                egui::Stroke::new(1.0_f32, i_color),
            );
            painter.text(
                egui::pos2(cx + dx * (i_line_len + 8.0), cy + dy * (i_line_len + 8.0)),
                egui::Align2::CENTER_CENTER,
                "I",
                label_font.clone(),
                i_color,
            );

            // 5. Target boxes at the six broadcast chroma positions.
            // Small unfilled squares — same shape colorists see on
            // hardware vectorscopes and in DaVinci / FCPX.
            let target_color = egui::Color32::from_rgba_unmultiplied(
                label_color.r(), label_color.g(), label_color.b(), 180,
            );
            let box_half = 4.0;
            for (name, cb, cr) in &[
                ("R", 0.17_f32, 0.29_f32),
                ("Mg", 0.28, -0.05),
                ("B", 0.12, -0.34),
                ("Cy", -0.17, -0.29),
                ("G", -0.28, 0.05),
                ("Yl", -0.12, 0.34),
            ] {
                let tx = cx + cb * r * 2.0;
                let ty = cy - cr * r * 2.0;
                let rect = egui::Rect::from_center_size(
                    egui::pos2(tx, ty),
                    egui::vec2(box_half * 2.0, box_half * 2.0),
                );
                painter.rect_stroke(
                    rect,
                    0.0,
                    egui::Stroke::new(1.0_f32, target_color),
                );
                // Label offset radially outward — never overlaps the
                // target square or the scatter cluster.
                let mag = (cb * cb + cr * cr).sqrt().max(0.001);
                let ux = cb / mag;
                let uy = -cr / mag;
                let label_offset = 10.0;
                painter.text(
                    egui::pos2(tx + ux * label_offset, ty + uy * label_offset),
                    egui::Align2::CENTER_CENTER,
                    name,
                    label_font.clone(),
                    label_color,
                );
            }

        } else {
            // Waveform/Parade/Histogram: scope image + scale overlay
            let img_rect = egui::Rect::from_min_max(
                egui::pos2(full_rect.min.x + AXIS_W, full_rect.min.y + PAD_TOP),
                egui::pos2(full_rect.max.x - PAD_RIGHT, full_rect.max.y - PAD_BOTTOM),
            );
            ui.painter().image(tex.id(), img_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE);

            // Draw scale system on top
            // Read scale from the scope_state (passed via the ScopeState struct)
            let scale = state.scale;
            let is_parade = mode == ScopeMode::Parade;
            draw_scale(ui, full_rect, scale, is_parade, 3, theme);

        }

        // ── Scope cursor indicator ──
        if show_cursor {
            if let Some(ref cur) = scope_cursor {
                let scope_type = crate::state::ScopeType::from(mode);
                super::scope_window::draw_scope_cursor_indicator(
                    &ui.painter_at(full_rect), scope_type, cur, full_rect,
                );
            }
        }
    } else {
        ui.label(egui::RichText::new("No frame data").color(theme.text_dim).size(10.0));
    }
}

// ── Scope rendering pipeline ──────────────────────────────────────────────
//
// Technique (matching FCP/DaVinci quality):
// 1. Accumulate into f32 RGB buffer (no clamping — allows values > 1.0)
// 2. Apply bloom: extract bright areas, gaussian blur, blend back
// 3. Tone-map from HDR accumulation to display range
// 4. Apply gamma correction for perceptual brightness
// 5. Convert to u8 ColorImage

/// Pre-computed anisotropic Gaussian splat kernel. `weights` is
/// row-major over `width × height`, already normalised so the
/// kernel integrates to 1.0 — one input event always deposits one
/// unit of density spread across the kernel footprint.
///
/// **Why anisotropic.** Real CRT phosphor traces stretch vertically
/// along the electron-beam path, not in a circle. A horizontally-
/// short, vertically-tall Gaussian (e.g. width=3 height=5 with
/// σx=0.6 σy=1.8) makes traces feel like accumulating phosphor
/// filaments rather than soft round blobs. A 1×1 kernel collapses
/// to the legacy single-pixel point-write.
struct AccumBuffer {
    data: Vec<[f32; 4]>,
    w: usize,
    h: usize,
}

impl AccumBuffer {
    fn new(w: usize, h: usize) -> Self {
        Self { data: vec![[0.0; 4]; w * h], w, h }
    }

    /// Single-pixel point write — back-compat with the legacy `add()`
    /// shape. Density increments by 1.0 per call.
    #[inline]
    fn add(&mut self, x: usize, y: usize, r: f32, g: f32, b: f32) {
        if x < self.w && y < self.h {
            let idx = y * self.w + x;
            self.data[idx][0] += r;
            self.data[idx][1] += g;
            self.data[idx][2] += b;
            self.data[idx][3] += 1.0;
        }
    }

    /// Tone-map the density buffer to an RGBA8 image with the given
    /// background colour (each channel 0–255) and gamma. Hue is
    /// preserved by dividing accumulated colour by density first, then
    /// brightness is the density-saturating curve `d / (1 + d)`.
    /// Used by [`render_cie_chromaticity`]; the legacy phosphor variant
    /// was tied to the in-tree scope renderer and deleted in Phase 8.
    fn to_image(&self, bg: [f32; 3], gamma: f32) -> egui::ColorImage {
        let mut pixels = Vec::with_capacity(self.w * self.h);
        let bg_color = egui::Color32::from_rgb(
            bg[0].clamp(0.0, 255.0) as u8,
            bg[1].clamp(0.0, 255.0) as u8,
            bg[2].clamp(0.0, 255.0) as u8,
        );
        for cell in &self.data {
            let density = cell[3];
            if density <= 1e-6 {
                pixels.push(bg_color);
                continue;
            }
            let mean_r = cell[0] / density;
            let mean_g = cell[1] / density;
            let mean_b = cell[2] / density;
            let bright = (density / (1.0 + density)).powf(1.0 / gamma);
            let lr = bg[0] + (mean_r * 255.0 - bg[0]) * bright;
            let lg = bg[1] + (mean_g * 255.0 - bg[1]) * bright;
            let lb = bg[2] + (mean_b * 255.0 - bg[2]) * bright;
            pixels.push(egui::Color32::from_rgb(
                lr.clamp(0.0, 255.0) as u8,
                lg.clamp(0.0, 255.0) as u8,
                lb.clamp(0.0, 255.0) as u8,
            ));
        }
        egui::ColorImage { size: [self.w, self.h], pixels }
    }
}

/// Render CIE 1931 xy chromaticity diagram.
/// Plots image pixels in CIE xy space with spectral locus outline and sRGB gamut triangle.
pub(crate) fn render_cie_chromaticity(rgba: &[u8], width: u32, height: u32, step: u32, rw: usize, rh: usize) -> egui::ColorImage {
    let size = rw.min(rh);
    let mut buf = AccumBuffer::new(size, size);

    // The CIE xy diagram spans roughly x=[0, 0.8], y=[0, 0.9].
    // Map to pixel coords with some margin.
    let margin = 12.0;
    let plot_size = size as f32 - margin * 2.0;
    let xy_to_px = |x: f32, y: f32| -> (usize, usize) {
        let px = (margin + x / 0.8 * plot_size) as usize;
        // y=0 at bottom, y=0.9 at top → invert
        let py = (margin + (1.0 - y / 0.9) * plot_size) as usize;
        (px.min(size - 1), py.min(size - 1))
    };

    // ── 1. Spectral locus outline (CIE 1931 standard observer, 380–700nm) ──
    // Subset of xy coordinates along the spectral locus boundary
    let locus: &[(f32, f32)] = &[
        (0.1741, 0.0050), (0.1740, 0.0050), (0.1738, 0.0049), // 380-400nm
        (0.1736, 0.0049), (0.1733, 0.0048), (0.1726, 0.0048),
        (0.1714, 0.0051), (0.1689, 0.0069), (0.1644, 0.0109),
        (0.1566, 0.0177), (0.1440, 0.0297), (0.1241, 0.0578), // 440-460nm
        (0.0913, 0.1327), (0.0687, 0.2007), (0.0454, 0.2950),
        (0.0235, 0.4127), (0.0082, 0.5384), (0.0039, 0.6548),
        (0.0139, 0.7502), (0.0389, 0.8120), (0.0743, 0.8338), // 520-540nm
        (0.1142, 0.8262), (0.1547, 0.8059), (0.1929, 0.7816),
        (0.2296, 0.7543), (0.2658, 0.7243), (0.3016, 0.6923),
        (0.3373, 0.6589), (0.3731, 0.6245), (0.4087, 0.5896), // 580-600nm
        (0.4441, 0.5547), (0.4788, 0.5202), (0.5125, 0.4866),
        (0.5448, 0.4544), (0.5752, 0.4242), (0.6029, 0.3965),
        (0.6270, 0.3725), (0.6482, 0.3514), (0.6658, 0.3340), // 640-660nm
        (0.6801, 0.3197), (0.6915, 0.3083), (0.7006, 0.2993),
        (0.7079, 0.2920), (0.7140, 0.2859), (0.7190, 0.2809),
        (0.7230, 0.2770), (0.7260, 0.2740), (0.7283, 0.2717), // 700-780nm
    ];

    // Draw locus as a connected line
    let locus_color = 0.12_f32;
    for pair in locus.windows(2) {
        let (x0, y0) = xy_to_px(pair[0].0, pair[0].1);
        let (x1, y1) = xy_to_px(pair[1].0, pair[1].1);
        // Bresenham-ish: just draw both endpoints; AccumBuffer handles it
        if x0 < size && y0 < size { buf.add(x0, y0, locus_color, locus_color, locus_color * 1.2); }
        if x1 < size && y1 < size { buf.add(x1, y1, locus_color, locus_color, locus_color * 1.2); }
        // Interpolate some intermediate points for smoother line
        for t in 1..8 {
            let f = t as f32 / 8.0;
            let ix = (pair[0].0 + (pair[1].0 - pair[0].0) * f, pair[0].1 + (pair[1].1 - pair[0].1) * f);
            let (px, py) = xy_to_px(ix.0, ix.1);
            if px < size && py < size { buf.add(px, py, locus_color * 0.6, locus_color * 0.6, locus_color * 0.8); }
        }
    }
    // Close the locus: line from last spectral point to first (purple line)
    if let (Some(first), Some(last)) = (locus.first(), locus.last()) {
        for t in 0..20 {
            let f = t as f32 / 20.0;
            let ix = (last.0 + (first.0 - last.0) * f, last.1 + (first.1 - last.1) * f);
            let (px, py) = xy_to_px(ix.0, ix.1);
            if px < size && py < size { buf.add(px, py, locus_color * 0.8, 0.02, locus_color * 0.8); }
        }
    }

    // ── 2. sRGB / Rec.709 gamut triangle ──
    let srgb_r = (0.6400_f32, 0.3300_f32);
    let srgb_g = (0.3000, 0.6000);
    let srgb_b = (0.1500, 0.0600);
    let gamut_verts = [(srgb_r, srgb_g), (srgb_g, srgb_b), (srgb_b, srgb_r)];
    let gamut_colors = [
        (0.15_f32, 0.06, 0.06), // R-G edge: reddish
        (0.06, 0.12, 0.06),     // G-B edge: greenish
        (0.06, 0.06, 0.15),     // B-R edge: bluish
    ];
    for (edge, color) in gamut_verts.iter().zip(gamut_colors.iter()) {
        for t in 0..60 {
            let f = t as f32 / 60.0;
            let x = edge.0.0 + (edge.1.0 - edge.0.0) * f;
            let y = edge.0.1 + (edge.1.1 - edge.0.1) * f;
            let (px, py) = xy_to_px(x, y);
            if px < size && py < size {
                buf.add(px, py, color.0, color.1, color.2);
                // Thicken
                if px + 1 < size { buf.add(px + 1, py, color.0 * 0.5, color.1 * 0.5, color.2 * 0.5); }
            }
        }
    }

    // ── 3. D65 white point marker ──
    let (wpx, wpy) = xy_to_px(0.3127, 0.3290);
    for dx in 0..3_usize {
        for dy in 0..3_usize {
            let gx = wpx.saturating_sub(1) + dx;
            let gy = wpy.saturating_sub(1) + dy;
            if gx < size && gy < size { buf.add(gx, gy, 0.2, 0.2, 0.2); }
        }
    }

    // ── 4. Plot image data ──
    let sample_step = (step * 3).max(3) as usize;
    for y in (0..height).step_by(sample_step) {
        for x in (0..width).step_by(sample_step) {
            let idx = ((y * width + x) * 4) as usize;
            if idx + 2 >= rgba.len() { continue; }
            // sRGB → linear
            let sr = rgba[idx] as f32 / 255.0;
            let sg = rgba[idx + 1] as f32 / 255.0;
            let sb = rgba[idx + 2] as f32 / 255.0;
            let to_linear = |v: f32| -> f32 {
                if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            let lr = to_linear(sr);
            let lg = to_linear(sg);
            let lb = to_linear(sb);

            // Linear sRGB → CIE XYZ (D65)
            let cap_x = 0.4124564 * lr + 0.3575761 * lg + 0.1804375 * lb;
            let cap_y = 0.2126729 * lr + 0.7151522 * lg + 0.0721750 * lb;
            let z = 0.0193339 * lr + 0.1191920 * lg + 0.9503041 * lb;

            let sum = cap_x + cap_y + z;
            if sum < 0.001 { continue; } // skip near-black

            // XYZ → xy chromaticity
            let cx = cap_x / sum;
            let cy = cap_y / sum;

            let (px, py) = xy_to_px(cx, cy);
            if px < size && py < size {
                // Plot with actual pixel color, brightness-weighted
                let brightness = cap_y.sqrt().min(1.0);
                let boost = 0.15 + brightness * 0.6;
                buf.add(px, py, sr * boost, sg * boost, sb * boost);
            }
        }
    }

    buf.to_image([4.0, 4.0, 7.0], 2.0)
}

/// Convert hue (0..1) to RGB for the rainbow ring
pub fn hue_to_rgb(h: f32) -> (f32, f32, f32) {
    let h = h * 6.0;
    let c = 1.0_f32;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    }
}

#[allow(dead_code)]
fn draw_ire_lines(pixels: &mut [egui::Color32], w: usize, h: usize) {
    // Very subtle dashed lines — should not compete with the trace data
    let line_color = egui::Color32::from_rgb(0x24, 0x22, 0x14);
    for &ire in &[0.0_f32, 0.25, 0.50, 0.75, 1.0] {
        let y = h - 1 - ((ire * (h - 1) as f32) as usize).min(h - 1);
        for x in (0..w).step_by(8) {
            // 2-on 6-off — very sparse dashes
            for dx in 0..2 {
                let px = x + dx;
                if px < w {
                    let idx = y * w + px;
                    if idx < pixels.len() {
                        // Blend with existing — don't overwrite trace data
                        let existing = pixels[idx];
                        let r = (existing.r() as u16 + line_color.r() as u16).min(255) as u8;
                        let g = (existing.g() as u16 + line_color.g() as u16).min(255) as u8;
                        let b = (existing.b() as u16 + line_color.b() as u16).min(255) as u8;
                        pixels[idx] = egui::Color32::from_rgb(r, g, b);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ScopeScale;

    #[test]
    fn test_settings_gen_increments_on_bloom_toggle() {
        let mut state = ScopeState::default();
        let gen_before = state.settings_generation;
        state.bloom = !state.bloom;
        state.increment_settings_generation();
        assert!(state.settings_generation > gen_before);
    }

    #[test]
    fn test_settings_gen_increments_on_low_pass_toggle() {
        let mut state = ScopeState::default();
        let gen_before = state.settings_generation;
        state.low_pass = !state.low_pass;
        state.increment_settings_generation();
        assert!(state.settings_generation > gen_before);
    }

    #[test]
    fn test_val_to_y_10bit_range() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
        // 1023 should map to PAD_TOP
        let y_top = val_to_y(1023.0, &rect, ScopeScale::TenBit);
        assert!((y_top - PAD_TOP).abs() < 1.0, "1023 at y={y_top}, expected ~{PAD_TOP}");
        // 0 should map to height - PAD_BOTTOM
        let y_bot = val_to_y(0.0, &rect, ScopeScale::TenBit);
        assert!((y_bot - (300.0 - PAD_BOTTOM)).abs() < 1.0, "0 at y={y_bot}, expected ~{}", 300.0 - PAD_BOTTOM);
    }

    #[test]
    fn test_val_to_y_ire_range() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
        let y_top = val_to_y(100.0, &rect, ScopeScale::Ire);
        assert!((y_top - PAD_TOP).abs() < 1.0, "100 IRE at y={y_top}, expected ~{PAD_TOP}");
        let y_bot = val_to_y(0.0, &rect, ScopeScale::Ire);
        assert!((y_bot - (300.0 - PAD_BOTTOM)).abs() < 1.0, "0 IRE at y={y_bot}, expected ~{}", 300.0 - PAD_BOTTOM);
    }

    #[test]
    fn test_low_pass_filter_changes_pixels() {
        // Create a 10x1 image with a spike at position 5
        let mut pixels = vec![egui::Color32::BLACK; 10];
        pixels[5] = egui::Color32::from_rgb(255, 0, 0);
        let mut image = egui::ColorImage { size: [10, 1], pixels };
        apply_low_pass_filter(&mut image);
        // After filtering, neighbors of the spike should be non-zero
        assert!(image.pixels[4].r() > 0, "Left neighbor should pick up red from spike");
        assert!(image.pixels[6].r() > 0, "Right neighbor should pick up red from spike");
        // Center should be reduced from 255
        assert!(image.pixels[5].r() < 255, "Center spike should be reduced by filter");
    }

    // ── Phase 2 tests ──────────────────────────────────────────────────

    #[test]
    fn test_pad_top_prevents_label_clipping() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 400.0));
        let y = val_to_y(1023.0, &rect, ScopeScale::TenBit);
        assert!(y >= PAD_TOP - 1.0, "1023 label at y={} is too close to top edge", y);
    }

    #[test]
    fn test_major_ticks_10bit() {
        let ticks = major_ticks(ScopeScale::TenBit);
        assert_eq!(ticks, vec![0, 128, 256, 384, 512, 640, 768, 896, 1023]);
    }

    #[test]
    fn test_major_ticks_ire() {
        let ticks = major_ticks(ScopeScale::Ire);
        assert_eq!(ticks, vec![0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100]);
    }

    #[test]
    fn test_major_ticks_percent() {
        let ticks = major_ticks(ScopeScale::Percent);
        assert_eq!(ticks, vec![0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100]);
    }

    #[test]
    fn test_axis_w_is_40() {
        assert_eq!(AXIS_W, 40.0);
    }

    // ── Phase 3 tests ──────────────────────────────────────────────────

    #[test]
    fn test_scope_layout_pane_count() {
        use crate::state::ScopeLayout;
        assert_eq!(ScopeLayout::OnePane.pane_count(), 1);
        assert_eq!(ScopeLayout::TwoPane.pane_count(), 2);
        assert_eq!(ScopeLayout::FourPane.pane_count(), 4);
        assert_eq!(ScopeLayout::NinePane.pane_count(), 9);
    }

    #[test]
    fn test_pane_config_default() {
        use crate::state::{PaneConfig, ScopeLayout, ScopeType};
        let config = PaneConfig::default_for_layout(ScopeLayout::FourPane);
        assert_eq!(config.len(), 4);
        assert_eq!(config[0].scope_type, ScopeType::Parade);
        assert_eq!(config[1].scope_type, ScopeType::Waveform);
        assert_eq!(config[2].scope_type, ScopeType::Vectorscope);
        assert_eq!(config[3].scope_type, ScopeType::Histogram);
    }

    #[test]
    fn test_waveform_channel_luma_formula() {
        let r = 1.0_f32;
        let g = 0.0_f32;
        let b = 0.0_f32;
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        assert!((luma - 0.2126).abs() < 0.0001);
    }

    #[test]
    fn test_scope_window_state_persistence() {
        use crate::state::{ScopeLayout, ScopeWindowState};
        let state = ScopeWindowState {
            open: true,
            layout: ScopeLayout::FourPane,
            ..Default::default()
        };
        let json = serde_json::to_string(&state).unwrap();
        let restored: ScopeWindowState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.layout, ScopeLayout::FourPane);
        assert!(restored.open);
    }

    // ── Phase 4 tests ──────────────────────────────────────────────────

    #[test]
    fn test_waveform_brightness_affects_gain() {
        use crate::state::{PaneScopeSettings, compute_tone_map_gain};
        let settings = PaneScopeSettings { waveform_brightness: 0.5, ..Default::default() };
        let gain = compute_tone_map_gain(&settings);
        let settings_bright = PaneScopeSettings { waveform_brightness: 1.0, ..Default::default() };
        let gain_bright = compute_tone_map_gain(&settings_bright);
        assert!(gain_bright > gain, "Brighter setting should produce higher gain");
    }

    #[test]
    fn test_graticule_brightness_affects_alpha() {
        use crate::state::graticule_alpha;
        let alpha_dim = graticule_alpha(0.25);
        let alpha_bright = graticule_alpha(1.0);
        assert!(alpha_bright > alpha_dim);
        assert!(alpha_dim > 0.0, "Even dim graticule should be visible");
    }

    #[test]
    fn test_reference_levels_render_when_enabled() {
        use crate::state::{PaneScopeSettings, compute_reference_lines};
        let settings = PaneScopeSettings {
            show_reference_levels: true,
            reference_low: 64.0,
            reference_high: 940.0,
            ..Default::default()
        };
        let lines = compute_reference_lines(&settings, 300.0);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].y < lines[1].y, "High value should be above low value on screen");
    }

    #[test]
    fn test_skin_tone_angle_constant() {
        use crate::state::SKIN_TONE_ANGLE_DEGREES;
        assert_eq!(SKIN_TONE_ANGLE_DEGREES, 123.0);
    }

    #[test]
    fn test_vectorscope_range_rings() {
        use crate::state::{PaneScopeSettings, compute_saturation_rings};
        let settings = PaneScopeSettings { low_range: 0.30, high_range: 0.70, ..Default::default() };
        let rings = compute_saturation_rings(&settings, 200.0);
        assert_eq!(rings.len(), 2);
        assert!(rings[0].radius < rings[1].radius, "Low range ring should have smaller radius");
    }

    // ── Audio correlation + loudness-history scope tests ──

    #[test]
    fn render_audio_correlation_produces_correct_size() {
        let samples = vec![0.0f32; 4096]; // silence
        let img = render_audio_correlation(&samples, 256, 256);
        assert_eq!(img.size, [256, 256]);
        assert_eq!(img.pixels.len(), 256 * 256);
    }

    #[test]
    fn correlation_of_identical_channels_is_positive() {
        // L == R → correlation ≈ +1. We don't inspect the exact r value
        // here (it's rendered into pixels) but we do confirm the render
        // doesn't panic and produces a correctly-sized image.
        let mut samples = vec![0.0f32; 2048];
        for i in 0..1024 {
            let v = (i as f32 * 0.01).sin() * 0.5;
            samples[i * 2] = v;      // L
            samples[i * 2 + 1] = v;  // R — identical
        }
        let img = render_audio_correlation(&samples, 128, 128);
        assert_eq!(img.size, [128, 128]);
    }

    #[test]
    fn render_audio_correlation_empty_input_doesnt_panic() {
        let img = render_audio_correlation(&[], 128, 128);
        assert_eq!(img.size, [128, 128]);
        assert_eq!(img.pixels.len(), 128 * 128);
    }

    #[test]
    fn render_lufs_history_produces_correct_size() {
        let history = vec![-14.0f32; 100];
        let img = render_lufs_history(&history, -14.0, 300, 100);
        assert_eq!(img.size, [300, 100]);
        assert_eq!(img.pixels.len(), 300 * 100);
    }

    #[test]
    fn lufs_history_empty_doesnt_panic() {
        let img = render_lufs_history(&[], -14.0, 300, 100);
        assert_eq!(img.size, [300, 100]);
        assert_eq!(img.pixels.len(), 300 * 100);
    }

    #[test]
    fn lufs_history_appends_and_caps_at_600() {
        // Mirrors the app.rs LufsReadings handler: push_back + pop_front
        // when the ring exceeds 600 entries (60s at 10Hz).
        let mut history: std::collections::VecDeque<f32> =
            std::collections::VecDeque::new();
        for i in 0..700 {
            history.push_back(-14.0 - (i as f32 * 0.01));
            if history.len() > 600 {
                history.pop_front();
            }
        }
        assert_eq!(history.len(), 600);
        // Oldest retained sample should be i=100 (700 pushes - 600 cap).
        assert!((history[0] - (-14.0 - 1.0)).abs() < 0.001);
    }

    #[test]
    fn lufs_history_handles_infinity_values() {
        // The LUFS meter publishes f32::NEG_INFINITY before the first
        // 400ms block completes. The renderer must skip these rather
        // than clamp them to the bottom of the display range and draw
        // a spurious flat line.
        let history = [f32::NEG_INFINITY, -18.0, -16.0, -14.0, f32::NEG_INFINITY];
        let img = render_lufs_history(&history, -14.0, 200, 80);
        assert_eq!(img.size, [200, 80]);
    }

    // ── Scope enhancements (#75/#76/#77) ─────────────────────────────────

    #[test]
    fn histogram_scale_sqrt_greater_than_linear() {
        // Sqrt scaling lifts mid-range counts above linear scaling, which is
        // the whole reason sqrt is the default — it keeps the tails visible.
        let count = 1000u32;
        let max = 4000u32;
        let h = 256usize;
        let sqrt_h = (count as f32 / max as f32).sqrt() * (h as f32 - 1.0);
        let lin_h = (count as f32 / max as f32) * (h as f32 - 1.0);
        assert!(sqrt_h > lin_h);
        assert!(sqrt_h < h as f32);
    }

    #[test]
    fn histogram_log_zero_count_is_zero() {
        // The log branch must short-circuit on count==0 — ln(0) is -inf.
        let count = 0u32;
        let result = if count > 0 {
            (count as f32).ln() / (1000f32).ln() * 255.0
        } else {
            0.0
        };
        assert_eq!(result, 0.0);
    }

    #[test]
    fn waveform_classic_channels_distinct() {
        // Classic paint mode must produce visibly different RGB colours
        // for the R/G/B channels (a user-facing invariant of #77).
        let r = egui::Color32::from_rgb(200, 50, 50);
        let g = egui::Color32::from_rgb(50, 200, 50);
        let b = egui::Color32::from_rgb(50, 50, 200);
        assert_ne!(r, g);
        assert_ne!(g, b);
        assert_ne!(r, b);
    }

    // ── Spectrogram scope tests (#74) ────────────────────────────────────

    #[test]
    fn render_spectrogram_correct_size() {
        // Slices now carry dB values (F-156). A mid-band -36 dBFS
        // signal with the default -72 floor maps to ~0.5 magnitude.
        let history = vec![vec![-36.0f32; 256]; 10];
        let img = render_audio_spectrogram(&history, 300, 200, -72.0);
        assert_eq!(img.size, [300, 200]);
        assert_eq!(img.pixels.len(), 300 * 200);
    }

    #[test]
    fn render_spectrogram_empty_doesnt_panic() {
        let img = render_audio_spectrogram(&[], 300, 200, -72.0);
        assert_eq!(img.size, [300, 200]);
    }

    #[test]
    fn render_spectrogram_honours_db_floor() {
        // A signal at -50 dBFS: with floor -72 it's clearly audible
        // (≥ mid-range magnitude); with floor -24 it's BELOW the
        // floor and renders at the silence colour. Spot-check the
        // magnitude_to_color output at the middle-newest pixel.
        let history = vec![vec![-50.0f32; 256]; 1];
        let wide_floor_img = render_audio_spectrogram(&history, 4, 256, -72.0);
        let narrow_floor_img = render_audio_spectrogram(&history, 4, 256, -24.0);
        // Sample an arbitrary non-bottom pixel on the rightmost
        // column; both histories contain a single slice so x = w-1.
        let idx = 128 * 4 + 3;
        assert!(
            wide_floor_img.pixels[idx].r() > narrow_floor_img.pixels[idx].r()
                || wide_floor_img.pixels[idx].g() > narrow_floor_img.pixels[idx].g()
                || wide_floor_img.pixels[idx].b() > narrow_floor_img.pixels[idx].b(),
            "wider floor should light up more pixel magnitude",
        );
    }

    #[test]
    fn magnitude_to_color_extremes() {
        let black = magnitude_to_color(0.0);
        let white = magnitude_to_color(1.0);
        assert!(black.r() < 50);
        assert!(black.b() < 60);
        assert!(white.r() > 200);
        assert!(white.g() > 200);
        assert!(white.b() > 200);
    }

    #[test]
    fn spectrum_history_caps_at_300() {
        let mut history: std::collections::VecDeque<Vec<f32>> =
            std::collections::VecDeque::new();
        for _ in 0..400 {
            history.push_back(vec![0.5; 256]);
            if history.len() > 300 { history.pop_front(); }
        }
        assert_eq!(history.len(), 300);
    }

    #[test]
    fn lerp_rgb_midpoint() {
        let mid = lerp_rgb((0,0,0), (200,200,200), 0.5);
        assert_eq!(mid, (100, 100, 100));
    }

    #[test]
    fn pane_settings_new_fields_default_correctly() {
        // Defaults must preserve the pre-enhancement behaviour: I/Q lines
        // off, histogram sqrt, waveform green.
        let settings = crate::state::PaneScopeSettings::default();
        assert!(!settings.show_iq_lines);
        assert_eq!(settings.histogram_scale, crate::state::HistogramScale::Sqrt);
        assert_eq!(settings.waveform_paint_mode, crate::state::WaveformPaintMode::Green);
    }

    // ── Splat kernel + density-aware tonemap tests ─────────────────────

    /// Pin: a normalised Gaussian kernel sums to 1.0 — splatting one
    /// data point deposits exactly one unit of density (away from
    /// edges). Catches a regression in the normaliser.
    #[test]
    fn scanline_texture_darkens_odd_rows_only() {
        // 4×4 image filled with mid-grey (128, 128, 128).
        let mut image = egui::ColorImage {
            size: [4, 4],
            pixels: vec![egui::Color32::from_rgb(128, 128, 128); 16],
        };
        apply_scanline_texture(&mut image);

        // Even rows (y=0, 2) untouched.
        for y in [0, 2] {
            for x in 0..4 {
                let p = image.pixels[y * 4 + x];
                assert_eq!(p.r(), 128, "even row y={y} darkened");
                assert_eq!(p.g(), 128);
                assert_eq!(p.b(), 128);
            }
        }
        // Odd rows (y=1, 3) darkened by ×0.96.
        let expected = (128.0_f32 * 0.96) as u8;
        for y in [1, 3] {
            for x in 0..4 {
                let p = image.pixels[y * 4 + x];
                assert_eq!(p.r(), expected, "odd row y={y} not darkened");
                assert_eq!(p.g(), expected);
                assert_eq!(p.b(), expected);
            }
        }
    }

    /// Pin: scanline overlay is subtle — at most a few percent total
    /// brightness loss across the whole image. If a regression cranked
    /// the multiplier down to 0.5 the test would catch it before
    /// users do.
    #[test]
    fn scanline_texture_is_subtle() {
        let mut image = egui::ColorImage {
            size: [10, 10],
            pixels: vec![egui::Color32::from_rgb(200, 200, 200); 100],
        };
        apply_scanline_texture(&mut image);

        // Average brightness across all pixels — even rows untouched,
        // odd rows ×0.96. Five even + five odd rows → expected
        // ≈ 200 × (0.5 + 0.5 × 0.96) = 200 × 0.98 = 196.
        let total: u32 = image.pixels.iter().map(|p| p.r() as u32).sum();
        let avg = total as f32 / 100.0;
        assert!(avg > 195.0 && avg < 197.0,
            "scanline should drop avg brightness by ~2%, got avg={avg}");
    }

    /// Pin: scanline overlay handles non-multiple-of-2 heights. Odd
    /// rows past the last even row still darken; bottom-most row
    /// being either even or odd doesn't break the loop.
    #[test]
    fn scanline_texture_handles_odd_height() {
        let mut image = egui::ColorImage {
            size: [4, 5],
            pixels: vec![egui::Color32::from_rgb(100, 100, 100); 20],
        };
        apply_scanline_texture(&mut image);

        // y=0, 2, 4 are even (untouched at 100); y=1, 3 are odd (96).
        let expected_dark = (100.0_f32 * 0.96) as u8;
        assert_eq!(image.pixels[0].r(), 100);     // y=0
        assert_eq!(image.pixels[4].r(), expected_dark);   // y=1
        assert_eq!(image.pixels[8].r(), 100);     // y=2
        assert_eq!(image.pixels[12].r(), expected_dark);  // y=3
        assert_eq!(image.pixels[16].r(), 100);    // y=4
    }

}

/// Render an audio spectrum (frequency-magnitude) scope from pre-computed
/// log-frequency dBFS bins (`bins[i]` clamped to −96..0). Produces the same
/// kind of amber-on-dark ColorImage as the video scopes so it drops into the
/// existing texture pipeline with no special casing.
///
/// Each column maps to one output bin (nearest-neighbour); bars rise from
/// the bottom with a gradient from the grid amber at the base to near-white
/// at the peak. Horizontal guide lines mark −12, −24, −48 and −72 dBFS.
pub(crate) fn render_audio_spectrum(bins: &[f32], rw: usize, rh: usize) -> egui::ColorImage {
    use egui::Color32;
    let w = rw.max(1);
    let h = rh.max(1);
    let mut pixels = vec![Color32::from_rgb(4, 4, 7); w * h];

    // Helper to index into the pixels buffer.
    let idx = |x: usize, y: usize| y * w + x;

    // Background gridlines at fixed dB thresholds, faint amber.
    let grid_color = Color32::from_rgba_unmultiplied(245, 180, 60, 32);
    let label_db = [-12.0f32, -24.0, -48.0, -72.0];
    for &db in &label_db {
        let t = ((db + 96.0) / 96.0).clamp(0.0, 1.0);
        let y = (h - 1).saturating_sub((t * (h - 1) as f32) as usize);
        if y < h {
            for x in 0..w {
                pixels[idx(x, y)] = grid_color;
            }
        }
    }

    // Bars.
    let num_bins = bins.len().max(1);
    for x in 0..w {
        let bin_i = (x * num_bins / w).min(num_bins - 1);
        let db = bins[bin_i].clamp(-96.0, 0.0);
        let t = (db + 96.0) / 96.0;
        let bar_h = (t * h as f32) as usize;
        if bar_h == 0 { continue; }
        for y_from_bottom in 0..bar_h {
            let y = h - 1 - y_from_bottom;
            // Gradient: amber near the base → warm white at the peak. `k` is
            // distance from the clip column base to the pixel, normalised by
            // bar height.
            let k = (y_from_bottom as f32 / bar_h as f32).clamp(0.0, 1.0);
            let r = (180.0 * (1.0 - k) + 245.0 * k) as u8;
            let g = (140.0 * (1.0 - k) + 220.0 * k) as u8;
            let b = (40.0 * (1.0 - k) + 180.0 * k) as u8;
            pixels[idx(x, y)] = Color32::from_rgb(r, g, b);
        }
    }

    egui::ColorImage { size: [w, h], pixels }
}

/// Render an audio-correlation (Lissajous / goniometer) scope plus a
/// correlation-coefficient strip meter along the bottom edge.
///
/// Input is interleaved stereo PCM (`LRLRLR…`). Each sample pair is
/// plotted at `(x = (L+R)/2, y = (L-R)/2)` in a 45°-rotated coordinate
/// system — the classic broadcast goniometer convention, where a mono
/// signal collapses to a vertical line and a hard-panned signal splays
/// to a diagonal.
///
/// The bottom 8 rows paint a Pearson correlation strip:
/// `r = Σ(L·R) / sqrt(Σ L² · Σ R²)`, mapped to [-1, +1]. The filled
/// region is green (>+0.5), yellow (0..+0.5), or red (negative — phase
/// issues that will cancel on mono downmix).
pub(crate) fn render_audio_correlation(
    interleaved: &[f32],
    width: usize,
    height: usize,
) -> egui::ColorImage {
    use egui::Color32;
    let w = width.max(1);
    let h = height.max(1);
    let mut pixels = vec![Color32::from_rgb(14, 14, 18); w * h];

    // Discard the optional trailing odd sample so we always work with
    // complete stereo pairs.
    let samples: Vec<(f32, f32)> = interleaved
        .chunks_exact(2)
        .map(|c| (c[0], c[1]))
        .collect();

    // ── Lissajous plot ──
    // Goniometer convention: x = (L+R)/√2, y = (L-R)/√2. We use 0.5 as
    // the scaling factor (slightly under √2/2) so loud-but-not-clipping
    // signals still fit inside the canvas.
    for (l, r) in &samples {
        let x_norm = (l + r) * 0.5 + 0.5; // [-1,1] → [0,1]
        let y_norm = (l - r) * 0.5 + 0.5;
        let px = (x_norm.clamp(0.0, 1.0) * (w - 1) as f32) as usize;
        // Y axis flipped so +(L-R) plots upward.
        let py = ((1.0 - y_norm.clamp(0.0, 1.0)) * (h - 1) as f32) as usize;
        if px < w && py < h {
            pixels[py * w + px] = Color32::from_rgb(60, 220, 100);
        }
    }

    // ── Correlation-coefficient strip (bottom 8px) ──
    let (sum_lr, sum_l2, sum_r2) = samples.iter().fold(
        (0f32, 0f32, 0f32),
        |(lr, l2, r2), (l, r)| (lr + l * r, l2 + l * l, r2 + r * r),
    );
    let corr = if sum_l2 > 0.0 && sum_r2 > 0.0 {
        (sum_lr / (sum_l2.sqrt() * sum_r2.sqrt())).clamp(-1.0, 1.0)
    } else {
        0.0
    };

    let bar_y_start = h.saturating_sub(8);
    let corr_x = (((corr + 1.0) * 0.5) * (w - 1) as f32) as usize;
    let center_x = w / 2;
    let fill_color = if corr > 0.5 {
        Color32::from_rgb(60, 200, 80)
    } else if corr > 0.0 {
        Color32::from_rgb(200, 180, 40)
    } else {
        Color32::from_rgb(200, 60, 60)
    };
    for y in bar_y_start..h {
        for x in 0..w {
            let color = if x == center_x {
                Color32::from_gray(180)
            } else if (corr >= 0.0 && x >= center_x && x <= corr_x)
                || (corr < 0.0 && x <= center_x && x >= corr_x)
            {
                fill_color
            } else {
                Color32::from_rgb(30, 30, 36)
            };
            pixels[y * w + x] = color;
        }
    }

    egui::ColorImage { size: [w, h], pixels }
}

/// Render a rolling short-term-LUFS history graph with the user's
/// loudness target drawn as a horizontal reference line plus a ±1 LU
/// tolerance band.
///
/// `history` is oldest-first (natural `VecDeque::iter()` order), so the
/// newest sample lands on the right edge. Values outside the
/// [`lufs_min`, `lufs_max`] display range are clamped.
pub(crate) fn render_lufs_history(
    history: &[f32],
    target_lufs: f32,
    width: usize,
    height: usize,
) -> egui::ColorImage {
    use egui::Color32;
    let w = width.max(1);
    let h = height.max(1);
    let mut pixels = vec![Color32::from_rgb(14, 14, 18); w * h];

    // Display range chosen to cover the practical band: streaming targets
    // cluster around -14..-23, broadcast targets at -23..-27, and
    // near-silence bottoms out around -36.
    let lufs_max = -9.0f32;
    let lufs_min = -36.0f32;
    let lufs_range = lufs_max - lufs_min;

    // Map a LUFS value to a Y pixel (top = loud).
    let lufs_to_y = |lufs: f32| -> usize {
        let clamped = lufs.clamp(lufs_min, lufs_max);
        let norm = (lufs_max - clamped) / lufs_range;
        (norm * (h - 1) as f32) as usize
    };

    // ── Target line + ±1 LU tolerance band ──
    let target_y = lufs_to_y(target_lufs);
    let band_top = lufs_to_y((target_lufs + 1.0).min(lufs_max));
    let band_bot = lufs_to_y((target_lufs - 1.0).max(lufs_min));
    for x in 0..w {
        for y in band_top..=band_bot.min(h - 1) {
            pixels[y * w + x] = Color32::from_rgba_unmultiplied(40, 120, 40, 60);
        }
        if target_y < h {
            pixels[target_y * w + x] = Color32::from_rgb(60, 200, 60);
        }
    }

    if history.is_empty() {
        return egui::ColorImage { size: [w, h], pixels };
    }

    // ── History line ──
    // Finite-only filter: the LUFS meter publishes f32::NEG_INFINITY for
    // ungated early samples. Plotting them would anchor the line at the
    // bottom edge and swamp the visible band.
    let n = history.len();
    for (i, &lufs) in history.iter().enumerate() {
        if !lufs.is_finite() {
            continue;
        }
        let x = if n > 1 {
            (i as f32 / (n - 1) as f32 * (w - 1) as f32) as usize
        } else {
            w - 1
        };
        let y = lufs_to_y(lufs);

        let delta = (lufs - target_lufs).abs();
        let color = if delta <= 0.5 {
            Color32::from_rgb(60, 220, 80)
        } else if delta <= 1.0 {
            Color32::from_rgb(200, 180, 40)
        } else {
            Color32::from_rgb(200, 80, 60)
        };

        if x < w && y < h {
            pixels[y * w + x] = color;
            if y + 1 < h {
                pixels[(y + 1) * w + x] = color;
            }
        }
    }

    egui::ColorImage { size: [w, h], pixels }
}

/// Render a scrolling FFT waterfall (spectrogram): x = time, y = frequency
/// (bin 0 = lowest at the bottom, bin N-1 = highest at the top), colour =
/// magnitude via a 5-stop heat-map gradient.
///
/// `history` is oldest-first — the most recent slice lands on the right
/// edge. Each slice is a `Vec<f32>` of per-bin dBFS values (clamped to
/// `[-96.0, 0.0]` by the app-side FFT). `db_floor` sets the lower bound
/// of the displayed dynamic range; anything softer renders as the
/// darkest colour. Passing `db_floor = -72.0` reproduces the old
/// hardcoded behaviour.
///
/// Empty `history` returns a background-only image at the requested size.
pub fn render_audio_spectrogram(
    history: &[Vec<f32>],
    width: usize,
    height: usize,
    db_floor: f32,
) -> egui::ColorImage {
    use egui::Color32;
    let w = width.max(1);
    let h = height.max(1);
    let bg = Color32::from_rgb(10, 10, 14);
    let mut pixels = vec![bg; w * h];

    if history.is_empty() {
        return egui::ColorImage { size: [w, h], pixels };
    }

    // Clamp the floor to a usable range so a pref set to 0 or positive
    // doesn't produce a divide-by-zero. Upper bound always 0 dBFS.
    let floor = db_floor.min(-1.0).max(-96.0);
    let db_range = -floor;

    let n_slices = history.len();
    for (slice_idx, slice) in history.iter().enumerate() {
        if slice.is_empty() {
            continue;
        }
        // Map oldest→newest onto left→right. Naive mapping; see function
        // docstring for the n_slices-vs-width aliasing caveat.
        let x = if n_slices > 1 {
            (slice_idx as f32 / (n_slices - 1) as f32 * (w - 1) as f32) as usize
        } else {
            w - 1
        };
        let n_bins = slice.len();
        for (bin_idx, &db) in slice.iter().enumerate() {
            // Normalise dB → [0, 1] per the configured floor.
            let mag = ((db - floor) / db_range).clamp(0.0, 1.0);
            // bin 0 = lowest freq, render at the bottom → invert Y.
            let y_frac = if n_bins > 1 {
                bin_idx as f32 / (n_bins - 1) as f32
            } else {
                0.0
            };
            let y = (h - 1).saturating_sub((y_frac * (h - 1) as f32) as usize);
            if x < w && y < h {
                pixels[y * w + x] = magnitude_to_color(mag);
            }
        }
    }

    egui::ColorImage { size: [w, h], pixels }
}

/// Map a normalized magnitude `[0.0, 1.0]` to an RGB heat-map colour via
/// a 5-stop gradient: dark-blue → blue → cyan → yellow-green → white.
/// Values outside the range clamp to the endpoints.
pub(crate) fn magnitude_to_color(mag: f32) -> egui::Color32 {
    // Five evenly-spaced stops (t = 0.0, 0.25, 0.5, 0.75, 1.0).
    let stops: [(u8, u8, u8); 5] = [
        (10, 10, 40),     // dark blue  — near-silence
        (20, 80, 180),    // blue       — quiet
        (20, 180, 180),   // cyan       — mid
        (180, 200, 20),   // yellow-green — loud
        (255, 255, 255),  // white      — peak
    ];
    let m = mag.clamp(0.0, 1.0);
    // Figure out which of the four segments we're in.
    let seg_f = m * 4.0;
    let seg = (seg_f as usize).min(3);
    let t = seg_f - seg as f32;
    let (r, g, b) = lerp_rgb(stops[seg], stops[seg + 1], t);
    egui::Color32::from_rgb(r, g, b)
}

/// Linear interpolation between two RGB triples. `t` is clamped to
/// `[0.0, 1.0]` by the callers above, but rounding uses `as u8` so the
/// result is tolerant of slight overshoot.
pub(crate) fn lerp_rgb(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let lerp = |x: u8, y: u8, t: f32| -> u8 {
        (x as f32 + (y as f32 - x as f32) * t).round() as u8
    };
    (lerp(a.0, b.0, t), lerp(a.1, b.1, t), lerp(a.2, b.2, t))
}
