use crate::clip_decoder::RenderDecoder;
use crate::error::RenderError;
use crate::snapshot::{
    ClipKindSnapshot, ClipSnapshot, SequenceSnapshot, TransitionSnapshot, VideoTrackSnapshot,
};
use klipp_core::ClipId;
use klipp_timeline::TransitionKind;
use klipp_title::TitleRenderer;
use std::collections::HashMap;

/// Composites all video tracks into a single RGBA frame for each timeline frame.
///
/// Handles:
/// - Multi-track compositing (bottom track first, top track composited over)
/// - Transition rendering between clips (Gap 6 fix)
/// - Title clip rendering via TitleRenderer
/// - Gap/invisible tracks → black/skip
pub struct FrameCompositor {
    title_renderer: TitleRenderer,
}

impl FrameCompositor {
    pub fn new() -> Self {
        Self {
            title_renderer: TitleRenderer::new(),
        }
    }

    /// Composite all video tracks at the given timeline frame into one RGBA frame.
    /// Returns `None` if there is nothing to render (all tracks empty/invisible).
    pub fn composite_frame(
        &mut self,
        frame_num: u64,
        snapshot: &SequenceSnapshot,
        decoders: &mut HashMap<ClipId, RenderDecoder>,
    ) -> Result<Option<Vec<u8>>, RenderError> {
        let w = snapshot.width;
        let h = snapshot.height;
        let mut base: Option<Vec<u8>> = None;

        // Process tracks bottom to top (index 0 = bottom, composited first)
        for track in &snapshot.video_tracks {
            if !track.visible {
                continue;
            }

            // Find which clip is active at this frame
            let clip = track
                .clips
                .iter()
                .find(|c| frame_num >= c.timeline_start_frame && frame_num < c.timeline_end_frame);

            let layer_frame = match clip {
                None => continue, // No clip at this frame on this track
                Some(c) => {
                    let raw = self.decode_clip_frame(c, decoders, w, h)?;

                    // Check for active transition on this track (Gap 6)
                    if let Some(transitioned) =
                        self.apply_transition(raw.as_ref(), frame_num, c, track, decoders, w, h)?
                    {
                        transitioned
                    } else {
                        match raw {
                            Some(f) => f,
                            None => continue, // Gap clip or exhausted decoder
                        }
                    }
                }
            };

            // Composite onto base using alpha-over blending with track opacity
            base = Some(match base {
                None => {
                    if (track.opacity - 1.0).abs() < 0.001 {
                        layer_frame
                    } else {
                        apply_opacity(layer_frame, track.opacity)
                    }
                }
                Some(b) => blend_over(b, &layer_frame, track.opacity),
            });
        }

        Ok(base)
    }

    /// Decode a single frame from a clip (media, title, or gap).
    fn decode_clip_frame(
        &mut self,
        clip: &ClipSnapshot,
        decoders: &mut HashMap<ClipId, RenderDecoder>,
        width: u32,
        height: u32,
    ) -> Result<Option<Vec<u8>>, RenderError> {
        match &clip.kind {
            ClipKindSnapshot::Gap => Ok(None),
            ClipKindSnapshot::Title(data) => {
                Ok(Some(self.title_renderer.render(data, width, height)))
            }
            ClipKindSnapshot::Media { .. } => {
                if let Some(decoder) = decoders.get_mut(&clip.id) {
                    decoder.next_frame()
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// Check if there's an active transition at this frame and render it.
    /// Returns `Some(blended_frame)` if a transition is active, `None` otherwise.
    #[allow(clippy::too_many_arguments)]
    fn apply_transition(
        &mut self,
        current_frame: Option<&Vec<u8>>,
        frame_num: u64,
        current_clip: &ClipSnapshot,
        track: &VideoTrackSnapshot,
        decoders: &mut HashMap<ClipId, RenderDecoder>,
        width: u32,
        height: u32,
    ) -> Result<Option<Vec<u8>>, RenderError> {
        // Find a transition where this clip is clip_a (outgoing)
        let transition = track.transitions.iter().find(|t| {
            t.clip_a_id == current_clip.id && self.is_in_transition_region(frame_num, t, track)
        });

        let transition = match transition {
            Some(t) => t,
            None => return Ok(None),
        };

        // Find the partner clip (clip_b = incoming)
        let partner_clip = track
            .clips
            .iter()
            .find(|c| c.id == transition.clip_b_id);

        let partner_clip = match partner_clip {
            Some(c) => c,
            None => return Ok(None),
        };

        // Get frames from both clips
        let from_frame = match current_frame {
            Some(f) => f.clone(),
            None => black_frame(width, height),
        };

        let to_frame = match self.decode_clip_frame(partner_clip, decoders, width, height)? {
            Some(f) => f,
            None => black_frame(width, height),
        };

        // Calculate transition progress
        let progress = self.transition_progress(frame_num, transition, track);

        // Render the transition
        Ok(Some(render_transition(
            transition.kind,
            &from_frame,
            &to_frame,
            progress,
            width,
            height,
        )))
    }

    /// Check if frame_num falls within the transition region.
    /// The transition region is at the overlap between clip_a's end and clip_b's start.
    fn is_in_transition_region(
        &self,
        frame_num: u64,
        transition: &TransitionSnapshot,
        track: &VideoTrackSnapshot,
    ) -> bool {
        if let Some(clip_b) = track.clips.iter().find(|c| c.id == transition.clip_b_id) {
            let trans_start = clip_b.timeline_start_frame;
            let trans_end = trans_start + transition.duration_frames;
            frame_num >= trans_start && frame_num < trans_end
        } else {
            false
        }
    }

    /// Calculate transition progress (0.0 to 1.0) at the given frame.
    fn transition_progress(
        &self,
        frame_num: u64,
        transition: &TransitionSnapshot,
        track: &VideoTrackSnapshot,
    ) -> f32 {
        if let Some(clip_b) = track.clips.iter().find(|c| c.id == transition.clip_b_id) {
            let trans_start = clip_b.timeline_start_frame;
            let elapsed = frame_num.saturating_sub(trans_start);
            if transition.duration_frames > 0 {
                (elapsed as f32 / transition.duration_frames as f32).clamp(0.0, 1.0)
            } else {
                1.0
            }
        } else {
            0.0
        }
    }
}

impl Default for FrameCompositor {
    fn default() -> Self {
        Self::new()
    }
}

/// Produce a fully transparent black frame.
fn black_frame(width: u32, height: u32) -> Vec<u8> {
    vec![0u8; (width * height * 4) as usize]
}

/// Alpha-over blend: composite `src` over `dst` with additional opacity.
fn blend_over(mut dst: Vec<u8>, src: &[u8], opacity: f32) -> Vec<u8> {
    debug_assert_eq!(dst.len(), src.len());
    for i in (0..dst.len()).step_by(4) {
        let sa = (src[i + 3] as f32 / 255.0) * opacity;
        let da = dst[i + 3] as f32 / 255.0;
        let out_a = sa + da * (1.0 - sa);
        if out_a > 0.0 {
            dst[i] = ((src[i] as f32 * sa + dst[i] as f32 * da * (1.0 - sa)) / out_a) as u8;
            dst[i + 1] =
                ((src[i + 1] as f32 * sa + dst[i + 1] as f32 * da * (1.0 - sa)) / out_a) as u8;
            dst[i + 2] =
                ((src[i + 2] as f32 * sa + dst[i + 2] as f32 * da * (1.0 - sa)) / out_a) as u8;
            dst[i + 3] = (out_a * 255.0) as u8;
        }
    }
    dst
}

/// Multiply all alpha values by opacity (pre-multiply style).
fn apply_opacity(mut frame: Vec<u8>, opacity: f32) -> Vec<u8> {
    if opacity >= 0.999 {
        return frame;
    }
    for chunk in frame.chunks_exact_mut(4) {
        chunk[3] = (chunk[3] as f32 * opacity).clamp(0.0, 255.0) as u8;
    }
    frame
}

/// CPU transition renderer — ported from klipp-engine/src/transition.rs.
/// Renders a transition between two RGBA frames.
fn render_transition(
    kind: TransitionKind,
    from: &[u8],
    to: &[u8],
    progress: f32,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let byte_count = (width * height * 4) as usize;
    let mut output = vec![0u8; byte_count];
    let p = progress.clamp(0.0, 1.0);

    match kind {
        TransitionKind::CrossDissolve => {
            let inv = 1.0 - p;
            for i in 0..byte_count {
                output[i] = (from[i] as f32 * inv + to[i] as f32 * p + 0.5) as u8;
            }
        }
        TransitionKind::WipeLeft => {
            let threshold = (p * width as f32) as u32;
            wipe_horizontal(&mut output, from, to, width, height, threshold, true);
        }
        TransitionKind::WipeRight => {
            let threshold = ((1.0 - p) * width as f32) as u32;
            wipe_horizontal(&mut output, from, to, width, height, threshold, false);
        }
        TransitionKind::WipeUp => {
            let threshold = (p * height as f32) as u32;
            wipe_vertical(&mut output, from, to, width, height, threshold, true);
        }
        TransitionKind::WipeDown => {
            let threshold = ((1.0 - p) * height as f32) as u32;
            wipe_vertical(&mut output, from, to, width, height, threshold, false);
        }
        TransitionKind::DipToBlack => {
            dip_transition(&mut output, from, to, p, 0);
        }
        TransitionKind::DipToWhite => {
            dip_transition(&mut output, from, to, p, 255);
        }
        // Newer transitions: fallback to cross dissolve in export compositor
        // (GPU pipeline handles these during playback; for export, the engine's
        // transition renderer is used when available)
        TransitionKind::PushLeft => {
            let offset = (p * width as f32) as i32;
            for y in 0..height {
                for x in 0..width {
                    let idx = (y * width + x) as usize * 4;
                    let src_x = x as i32 + offset;
                    if src_x < width as i32 {
                        let si = (y as i32 * width as i32 + src_x) as usize * 4;
                        output[idx..idx + 4].copy_from_slice(&from[si..si + 4]);
                    } else {
                        let si = (y as i32 * width as i32 + (src_x - width as i32)) as usize * 4;
                        output[idx..idx + 4].copy_from_slice(&to[si..si + 4]);
                    }
                }
            }
        }
        TransitionKind::FilmDissolve => {
            for i in (0..byte_count).step_by(4) {
                for c in 0..3 {
                    let f = from[i + c] as f32 / 255.0;
                    let t = to[i + c] as f32 / 255.0;
                    let blended = (f * f * (1.0 - p) + t * t * p).sqrt();
                    output[i + c] = (blended * 255.0 + 0.5).min(255.0) as u8;
                }
                output[i + 3] = (from[i + 3] as f32 * (1.0 - p).sqrt() + to[i + 3] as f32 * p.sqrt() + 0.5) as u8;
            }
        }
        TransitionKind::Flash => {
            let white_i = if p < 0.3 { p / 0.3 } else if p < 0.5 { 1.0 } else if p < 0.7 { 1.0 - (p - 0.5) / 0.2 } else { 0.0 };
            let blend_p = if p < 0.4 { 0.0 } else { ((p - 0.4) / 0.6).min(1.0) };
            for i in (0..byte_count).step_by(4) {
                for c in 0..3 {
                    let base = from[i + c] as f32 * (1.0 - blend_p) + to[i + c] as f32 * blend_p;
                    output[i + c] = (base * (1.0 - white_i) + 255.0 * white_i + 0.5).min(255.0) as u8;
                }
                output[i + 3] = if p < 0.4 { from[i + 3] } else { to[i + 3] };
            }
        }
        TransitionKind::LightLeak => {
            let leak = (p * (1.0 - p) * 4.0).powf(0.7);
            let inv = 1.0 - p;
            for i in (0..byte_count).step_by(4) {
                let r = from[i] as f32 * inv + to[i] as f32 * p;
                let g = from[i + 1] as f32 * inv + to[i + 1] as f32 * p;
                let b = from[i + 2] as f32 * inv + to[i + 2] as f32 * p;
                output[i] = (r + (255.0 - r) * leak * 0.7 + 0.5).min(255.0) as u8;
                output[i + 1] = (g + (180.0 - g).max(0.0) * leak * 0.5 + 0.5).min(255.0) as u8;
                output[i + 2] = (b + (60.0 - b).max(0.0) * leak * 0.3 + 0.5).min(255.0) as u8;
                output[i + 3] = (from[i + 3] as f32 * inv + to[i + 3] as f32 * p + 0.5) as u8;
            }
        }
        TransitionKind::LuminanceDissolve => {
            // Bright pixels of `from` dissolve to `to` first, dark pixels last.
            let softness = 0.15_f32;
            for i in (0..byte_count).step_by(4) {
                let r = from[i] as f32 / 255.0;
                let g = from[i + 1] as f32 / 255.0;
                let b = from[i + 2] as f32 / 255.0;
                let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                let lo = p - softness;
                let hi = p + softness;
                let t = ((luma - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0);
                let t = t * t * (3.0 - 2.0 * t);
                output[i] = (from[i] as f32 * (1.0 - t) + to[i] as f32 * t + 0.5) as u8;
                output[i + 1] = (from[i + 1] as f32 * (1.0 - t) + to[i + 1] as f32 * t + 0.5) as u8;
                output[i + 2] = (from[i + 2] as f32 * (1.0 - t) + to[i + 2] as f32 * t + 0.5) as u8;
                output[i + 3] = 255;
            }
        }
        TransitionKind::CircleWipeIn | TransitionKind::CircleWipeOut => {
            // Circle expanding/contracting from frame center (export uses default center).
            let cx = 0.5 * width as f32;
            let cy = 0.5 * height as f32;
            let max_r = ((cx.max(width as f32 - cx)).powi(2)
                + (cy.max(height as f32 - cy)).powi(2))
                .sqrt()
                .max(1.0);
            let softness = 0.04_f32;
            let is_in = matches!(kind, TransitionKind::CircleWipeIn);
            let radius = if is_in {
                p * (1.0 + 2.0 * softness) - softness
            } else {
                (1.0 - p) * (1.0 + 2.0 * softness) - softness
            };
            for y in 0..height {
                for x in 0..width {
                    let idx = (y * width + x) as usize * 4;
                    let dx = x as f32 - cx;
                    let dy = y as f32 - cy;
                    let dist = (dx * dx + dy * dy).sqrt() / max_r;
                    let lo = radius - softness;
                    let hi = radius + softness;
                    let t_raw = ((dist - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0);
                    let s_val = t_raw * t_raw * (3.0 - 2.0 * t_raw);
                    let t = if is_in { 1.0 - s_val } else { s_val };
                    for c in 0..3 {
                        output[idx + c] =
                            (from[idx + c] as f32 * (1.0 - t) + to[idx + c] as f32 * t + 0.5)
                                as u8;
                    }
                    output[idx + 3] = 255;
                }
            }
        }
        TransitionKind::ClockWipe => {
            // Clockwise sweep from 12 o'clock around frame center (export uses defaults).
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let softness = 0.02_f32;
            let two_pi = std::f32::consts::TAU;
            let threshold = p * (1.0 + 2.0 * softness) - softness;
            for y in 0..height {
                for x in 0..width {
                    let idx = (y * width + x) as usize * 4;
                    let dx = x as f32 - cx;
                    let dy = y as f32 - cy;
                    let mut angle = dx.atan2(-dy);
                    angle = ((angle % two_pi) + two_pi) % two_pi;
                    let norm_angle = angle / two_pi;
                    let lo = threshold - softness;
                    let hi = threshold + softness;
                    let t_raw = ((norm_angle - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0);
                    let s_val = t_raw * t_raw * (3.0 - 2.0 * t_raw);
                    let t = 1.0 - s_val;
                    for c in 0..3 {
                        output[idx + c] =
                            (from[idx + c] as f32 * (1.0 - t) + to[idx + c] as f32 * t + 0.5)
                                as u8;
                    }
                    output[idx + 3] = 255;
                }
            }
        }
        TransitionKind::BarnDoorOpen | TransitionKind::BarnDoorClose => {
            // Horizontal barn door split from frame center (export uses defaults).
            let softness = 0.03_f32;
            let cx = width as f32 * 0.5;
            let is_open = matches!(kind, TransitionKind::BarnDoorOpen);
            for y in 0..height {
                for x in 0..width {
                    let idx = (y * width + x) as usize * 4;
                    let dist = (x as f32 - cx).abs() / cx.max(1.0);
                    let t = if is_open {
                        let threshold = p * (1.0 + 2.0 * softness) - softness;
                        let lo = threshold - softness;
                        let hi = threshold + softness;
                        let tr = ((dist - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0);
                        1.0 - tr * tr * (3.0 - 2.0 * tr)
                    } else {
                        let threshold = (1.0 - p) * (1.0 + 2.0 * softness) - softness;
                        let lo = threshold - softness;
                        let hi = threshold + softness;
                        let tr = ((dist - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0);
                        tr * tr * (3.0 - 2.0 * tr)
                    };
                    for c in 0..3 {
                        output[idx + c] =
                            (from[idx + c] as f32 * (1.0 - t) + to[idx + c] as f32 * t + 0.5)
                                as u8;
                    }
                    output[idx + 3] = 255;
                }
            }
        }
        TransitionKind::Pixelate => {
            // Block size grows to 32 at midpoint (export uses default max block + from-only mode).
            let max_block = 32_usize;
            let triangle = 1.0 - (p * 2.0 - 1.0).abs();
            let block = ((max_block as f32 * triangle) as usize).max(1);
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let bx = ((x / block) * block + block / 2).min(w - 1);
                    let by = ((y / block) * block + block / 2).min(h - 1);
                    let bi = (by * w + bx) * 4;
                    if p < 0.5 {
                        output[fi..fi + 4].copy_from_slice(&from[bi..bi + 4]);
                    } else {
                        let t = (p - 0.5) * 2.0;
                        for c in 0..3 {
                            output[fi + c] = (from[bi + c] as f32 * (1.0 - t)
                                + to[fi + c] as f32 * t
                                + 0.5) as u8;
                        }
                        output[fi + 3] = 255;
                    }
                }
            }
        }
        TransitionKind::Ripple => {
            // Water ripple export fallback (uses default amplitude/frequency/speed).
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let amplitude = 20.0_f32;
            let frequency = 8.0_f32;
            let speed = 1.0_f32;
            let envelope = (p * std::f32::consts::PI).sin() * amplitude;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let dx = x as f32 - cx;
                    let dy = y as f32 - cy;
                    let dist = (dx * dx + dy * dy).sqrt();
                    let phase = dist * frequency * 0.1 - p * speed * 10.0;
                    let ripple = phase.sin() * envelope;
                    let (nx, ny) = if dist > 0.1 {
                        (dx / dist, dy / dist)
                    } else {
                        (0.0, 0.0)
                    };
                    let sx = (x as f32 + nx * ripple).clamp(0.0, w as f32 - 1.0) as usize;
                    let sy = (y as f32 + ny * ripple).clamp(0.0, h as f32 - 1.0) as usize;
                    let si = (sy * w + sx) * 4;
                    for c in 0..3 {
                        output[fi + c] = (from[si + c] as f32 * (1.0 - p)
                            + to[fi + c] as f32 * p
                            + 0.5) as u8;
                    }
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::PageTurn | TransitionKind::PageTurnReverse => {
            // Page peel export (uses default shadow=0.6, right-to-left; Reverse flips).
            let shadow_intensity = 0.6_f32;
            let flip = matches!(kind, TransitionKind::PageTurnReverse);
            let fold_x = 1.0 - p;
            let w = width as usize;
            let h = height as usize;
            let denom = (p * 0.3).max(0.01);
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let fx = if flip { w - 1 - x } else { x };
                    let norm_x = fx as f32 / w as f32;
                    if norm_x > fold_x {
                        let shadow_dist = (norm_x - fold_x) / denom;
                        let shadow = shadow_intensity * (-shadow_dist * 3.0).exp();
                        let t = (1.0 - shadow).clamp(0.0, 1.0);
                        for c in 0..3 {
                            output[fi + c] = (to[fi + c] as f32 * t) as u8;
                        }
                    } else {
                        let shadow_dist = (fold_x - norm_x) / denom;
                        let shadow = shadow_intensity * 0.5 * (-shadow_dist * 2.0).exp();
                        let t = (1.0 - shadow).clamp(0.0, 1.0);
                        for c in 0..3 {
                            output[fi + c] = (from[fi + c] as f32 * t) as u8;
                        }
                    }
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::ChromaticAberration => {
            // RGB channel split export (uses default 20px horizontal offset).
            let max_offset = 20.0_f32;
            let envelope = (p * std::f32::consts::PI).sin() * max_offset;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let base_g = (from[fi + 1] as f32 * (1.0 - p)
                        + to[fi + 1] as f32 * p
                        + 0.5) as u8;
                    let rx = (x as f32 - envelope).clamp(0.0, w as f32 - 1.0) as usize;
                    let ri = (y * w + rx) * 4;
                    let r_val = (from[ri] as f32 * (1.0 - p)
                        + to[ri] as f32 * p
                        + 0.5) as u8;
                    let bx = (x as f32 + envelope).clamp(0.0, w as f32 - 1.0) as usize;
                    let bi = (y * w + bx) * 4;
                    let b_val = (from[bi + 2] as f32 * (1.0 - p)
                        + to[bi + 2] as f32 * p
                        + 0.5) as u8;
                    output[fi] = r_val;
                    output[fi + 1] = base_g;
                    output[fi + 2] = b_val;
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::FilmBurn => {
            // Film Burn export (defaults: temp 0.8, speed 1.0, grain 0.4).
            let temp = 0.8_f32;
            let grain = 0.4_f32;
            let burn_r: f32 = 1.0;
            let burn_g: f32 = 1.0 - temp * 0.35;
            let burn_b: f32 = 1.0 - temp * 0.75;
            let shaped_p = p.powf(0.6).min(1.0);
            let envelope = (shaped_p * std::f32::consts::PI).sin();
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let hash = ((x as f32 * 127.1 + y as f32 * 311.7).sin() * 43758.545)
                        .fract()
                        .abs();
                    let hash2 = ((x as f32 * 269.5 + y as f32 * 183.3).sin() * 12345.678)
                        .fract()
                        .abs();
                    let pixel_thresh = hash * 0.6 + 0.2;
                    let burn_t = ((envelope - pixel_thresh) / 0.3).clamp(0.0, 1.0);
                    let grain_val = (hash2 - 0.5) * grain * envelope;
                    let base_r = (from[fi] as f32 * (1.0 - p) + to[fi] as f32 * p) / 255.0;
                    let base_g =
                        (from[fi + 1] as f32 * (1.0 - p) + to[fi + 1] as f32 * p) / 255.0;
                    let base_b =
                        (from[fi + 2] as f32 * (1.0 - p) + to[fi + 2] as f32 * p) / 255.0;
                    let r = (base_r + (burn_r - base_r) * burn_t + grain_val).clamp(0.0, 1.0);
                    let g = (base_g + (burn_g - base_g) * burn_t + grain_val).clamp(0.0, 1.0);
                    let b = (base_b + (burn_b - base_b) * burn_t + grain_val).clamp(0.0, 1.0);
                    output[fi] = (r * 255.0 + 0.5) as u8;
                    output[fi + 1] = (g * 255.0 + 0.5) as u8;
                    output[fi + 2] = (b * 255.0 + 0.5) as u8;
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::StarWipe => {
            // 5-point star export (defaults).
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let points = 5.0_f32;
            let inner_ratio = 0.4_f32;
            let softness = 0.03_f32;
            let max_r = (cx * cx + cy * cy).sqrt().max(1.0);
            let sector = std::f32::consts::PI / points;
            let two_sector = 2.0 * sector;
            let max_norm = 1.0 / inner_ratio;
            let threshold = p * (max_norm + 2.0 * softness) - softness;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let idx = (y * w + x) * 4;
                    let dx = x as f32 - cx;
                    let dy = y as f32 - cy;
                    let dist = (dx * dx + dy * dy).sqrt();
                    let angle = dy.atan2(dx);
                    let sector_angle = ((angle % two_sector) + two_sector) % two_sector;
                    let offset = ((sector_angle - sector).abs() / sector).clamp(0.0, 1.0);
                    let s_off = offset * offset * (3.0 - 2.0 * offset);
                    let star_r_norm = inner_ratio + (1.0 - inner_ratio) * s_off;
                    let norm_dist = dist / (max_r * star_r_norm.max(0.01));
                    let lo = threshold - softness;
                    let hi = threshold + softness;
                    let tr = ((norm_dist - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0);
                    let t = 1.0 - tr * tr * (3.0 - 2.0 * tr);
                    for c in 0..3 {
                        output[idx + c] = (from[idx + c] as f32 * (1.0 - t)
                            + to[idx + c] as f32 * t
                            + 0.5) as u8;
                    }
                    output[idx + 3] = 255;
                }
            }
        }
        TransitionKind::SplitWipeHorizontal | TransitionKind::SplitWipeVertical => {
            // Center-outward reveal.
            let softness = 0.03_f32;
            let horizontal = matches!(kind, TransitionKind::SplitWipeHorizontal);
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let threshold = p * (1.0 + 2.0 * softness) - softness;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let idx = (y * w + x) * 4;
                    let dist = if horizontal {
                        (x as f32 - cx).abs() / cx.max(1.0)
                    } else {
                        (y as f32 - cy).abs() / cy.max(1.0)
                    };
                    let lo = threshold - softness;
                    let hi = threshold + softness;
                    let tr = ((dist - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0);
                    let s_val = tr * tr * (3.0 - 2.0 * tr);
                    let t = 1.0 - s_val;
                    for c in 0..3 {
                        output[idx + c] = (from[idx + c] as f32 * (1.0 - t)
                            + to[idx + c] as f32 * t
                            + 0.5) as u8;
                    }
                    output[idx + 3] = 255;
                }
            }
        }
        TransitionKind::VenetianBlindHorizontal | TransitionKind::VenetianBlindVertical => {
            // 8 parallel strips wipe simultaneously (default blind count).
            let softness = 0.04_f32;
            let blinds = 8.0_f32;
            let horizontal = matches!(kind, TransitionKind::VenetianBlindHorizontal);
            let threshold = p * (1.0 + 2.0 * softness) - softness;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let idx = (y * w + x) * 4;
                    let strip_pos = if horizontal {
                        (y as f32 / height as f32 * blinds).fract()
                    } else {
                        (x as f32 / width as f32 * blinds).fract()
                    };
                    let lo = threshold - softness;
                    let hi = threshold + softness;
                    let tr = ((strip_pos - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0);
                    let s_val = tr * tr * (3.0 - 2.0 * tr);
                    let t = 1.0 - s_val;
                    for c in 0..3 {
                        output[idx + c] = (from[idx + c] as f32 * (1.0 - t)
                            + to[idx + c] as f32 * t
                            + 0.5) as u8;
                    }
                    output[idx + 3] = 255;
                }
            }
        }
        TransitionKind::CheckerboardWipe => {
            // Alternating squares wipe in opposite directions (default 8x8 grid).
            let softness = 0.04_f32;
            let grid = 8.0_f32;
            let threshold = p * (1.0 + 2.0 * softness) - softness;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let idx = (y * w + x) * 4;
                    let fx = x as f32 / width as f32 * grid;
                    let fy = y as f32 / height as f32 * grid;
                    let col = fx as usize;
                    let row = fy as usize;
                    let local_x = fx.fract();
                    let is_even = (col + row) % 2 == 0;
                    let local_pos = if is_even { local_x } else { 1.0 - local_x };
                    let lo = threshold - softness;
                    let hi = threshold + softness;
                    let tr = ((local_pos - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0);
                    let s_val = tr * tr * (3.0 - 2.0 * tr);
                    let t = 1.0 - s_val;
                    for c in 0..3 {
                        output[idx + c] = (from[idx + c] as f32 * (1.0 - t)
                            + to[idx + c] as f32 * t
                            + 0.5) as u8;
                    }
                    output[idx + 3] = 255;
                }
            }
        }
        TransitionKind::DiagonalWipeTLBR | TransitionKind::DiagonalWipeTRBL => {
            let softness = 0.03_f32;
            let tlbr = matches!(kind, TransitionKind::DiagonalWipeTLBR);
            let threshold = p * (1.0 + 2.0 * softness) - softness;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let idx = (y * w + x) * 4;
                    let nx = x as f32 / width as f32;
                    let ny = y as f32 / height as f32;
                    let diag = if tlbr {
                        (nx + ny) * 0.5
                    } else {
                        ((1.0 - nx) + ny) * 0.5
                    };
                    let lo = threshold - softness;
                    let hi = threshold + softness;
                    let tr = ((diag - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0);
                    let s_val = tr * tr * (3.0 - 2.0 * tr);
                    let t = 1.0 - s_val;
                    for c in 0..3 {
                        output[idx + c] = (from[idx + c] as f32 * (1.0 - t)
                            + to[idx + c] as f32 * t
                            + 0.5) as u8;
                    }
                    output[idx + 3] = 255;
                }
            }
        }
        TransitionKind::BoxWipeIn | TransitionKind::BoxWipeOut => {
            let softness = 0.03_f32;
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let is_in = matches!(kind, TransitionKind::BoxWipeIn);
            let threshold = if is_in {
                p * (1.0 + 2.0 * softness) - softness
            } else {
                (1.0 - p) * (1.0 + 2.0 * softness) - softness
            };
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let idx = (y * w + x) * 4;
                    let dx = (x as f32 - cx).abs() / cx.max(1.0);
                    let dy = (y as f32 - cy).abs() / cy.max(1.0);
                    let dist = dx.max(dy);
                    let lo = threshold - softness;
                    let hi = threshold + softness;
                    let tr = ((dist - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0);
                    let s_val = tr * tr * (3.0 - 2.0 * tr);
                    let t = if is_in { 1.0 - s_val } else { s_val };
                    for c in 0..3 {
                        output[idx + c] = (from[idx + c] as f32 * (1.0 - t)
                            + to[idx + c] as f32 * t
                            + 0.5) as u8;
                    }
                    output[idx + 3] = 255;
                }
            }
        }
        TransitionKind::SlideLeft
        | TransitionKind::SlideRight
        | TransitionKind::SlideUp
        | TransitionKind::SlideDown
        | TransitionKind::CoverLeft
        | TransitionKind::CoverRight
        | TransitionKind::UncoverLeft
        | TransitionKind::UncoverRight
        | TransitionKind::SqueezeH
        | TransitionKind::SqueezeV => {
            // Squeeze handled separately below; slide/cover/uncover inline here.
            if matches!(kind, TransitionKind::SqueezeH | TransitionKind::SqueezeV) {
                let w = width as usize;
                let h = height as usize;
                let is_h = matches!(kind, TransitionKind::SqueezeH);
                let dim = if is_h { w } else { h } as f32;
                let half = dim * 0.5;
                let one_px = 1.0 / dim;
                for y in 0..h {
                    for x in 0..w {
                        let fi = (y * w + x) * 4;
                        let pos = if is_h { x as f32 } else { y as f32 };
                        if p <= 0.5 {
                            let scale = 1.0 - 2.0 * p;
                            if scale < one_px {
                                output[fi..fi + 4].copy_from_slice(&[0, 0, 0, 255]);
                            } else {
                                let vis_lo = half - half * scale;
                                let vis_hi = half + half * scale;
                                if pos < vis_lo || pos >= vis_hi {
                                    output[fi..fi + 4].copy_from_slice(&[0, 0, 0, 255]);
                                } else {
                                    let src = ((pos - half) / scale + half)
                                        .clamp(0.0, dim - 1.0)
                                        as usize;
                                    let si = if is_h {
                                        (y * w + src) * 4
                                    } else {
                                        (src * w + x) * 4
                                    };
                                    output[fi..fi + 4].copy_from_slice(&from[si..si + 4]);
                                }
                            }
                        } else {
                            let scale = 2.0 * p - 1.0;
                            if scale < one_px {
                                output[fi..fi + 4].copy_from_slice(&[0, 0, 0, 255]);
                            } else {
                                let vis_lo = half - half * scale;
                                let vis_hi = half + half * scale;
                                if pos < vis_lo || pos >= vis_hi {
                                    output[fi..fi + 4].copy_from_slice(&[0, 0, 0, 255]);
                                } else {
                                    let src = ((pos - half) / scale + half)
                                        .clamp(0.0, dim - 1.0)
                                        as usize;
                                    let si = if is_h {
                                        (y * w + src) * 4
                                    } else {
                                        (src * w + x) * 4
                                    };
                                    output[fi..fi + 4].copy_from_slice(&to[si..si + 4]);
                                }
                            }
                        }
                    }
                }
            } else {
            let w = width as usize;
            let h = height as usize;
            let offset_w = ((p * width as f32) as usize).min(w);
            let offset_h = ((p * height as f32) as usize).min(h);
            let wi = w as isize;
            let hi = h as isize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let (use_to, sx, sy): (bool, usize, usize) = match kind {
                        TransitionKind::SlideLeft => {
                            let to_x = x as isize + wi - offset_w as isize;
                            if to_x < wi {
                                (true, to_x.max(0) as usize, y)
                            } else {
                                let fx = (x as isize - offset_w as isize).clamp(0, wi - 1)
                                    as usize;
                                (false, fx, y)
                            }
                        }
                        TransitionKind::SlideRight => {
                            let to_x = x as isize - wi + offset_w as isize;
                            if to_x >= 0 {
                                (true, (to_x as usize).min(w - 1), y)
                            } else {
                                (false, (x + offset_w).min(w - 1), y)
                            }
                        }
                        TransitionKind::SlideUp => {
                            let to_y = y as isize + hi - offset_h as isize;
                            if to_y < hi {
                                (true, x, to_y.max(0) as usize)
                            } else {
                                let fy = (y as isize - offset_h as isize).clamp(0, hi - 1)
                                    as usize;
                                (false, x, fy)
                            }
                        }
                        TransitionKind::SlideDown => {
                            let to_y = y as isize - hi + offset_h as isize;
                            if to_y >= 0 {
                                (true, x, (to_y as usize).min(h - 1))
                            } else {
                                (false, x, (y + offset_h).min(h - 1))
                            }
                        }
                        TransitionKind::CoverLeft => {
                            let to_x = x as isize + wi - offset_w as isize;
                            if to_x < wi {
                                (true, to_x.max(0) as usize, y)
                            } else {
                                (false, x, y)
                            }
                        }
                        TransitionKind::CoverRight => {
                            let to_x = x as isize - wi + offset_w as isize;
                            if to_x >= 0 {
                                (true, (to_x as usize).min(w - 1), y)
                            } else {
                                (false, x, y)
                            }
                        }
                        TransitionKind::UncoverLeft => {
                            let from_x = x + offset_w;
                            if from_x < w {
                                (false, from_x, y)
                            } else {
                                (true, x, y)
                            }
                        }
                        TransitionKind::UncoverRight => {
                            let from_x = x as isize - offset_w as isize;
                            if from_x >= 0 {
                                (false, from_x as usize, y)
                            } else {
                                (true, x, y)
                            }
                        }
                        _ => unreachable!(),
                    };
                    let si = (sy * w + sx) * 4;
                    let src = if use_to { to } else { from };
                    output[fi..fi + 4].copy_from_slice(&src[si..si + 4]);
                }
            }
            } // end else (slide/cover/uncover)
        }
        TransitionKind::ColorizeFlash => {
            // Colorize flash export (default cyan, intensity=1, peak=0.5).
            let flash_r = 0.0_f32;
            let flash_g = 0.8 * 255.0;
            let flash_b = 1.0 * 255.0;
            let peak = 0.5_f32;
            let ramp_up = peak * 0.6;
            let ramp_down = peak + (1.0 - peak) * 0.4;
            let flash_t = if p < ramp_up {
                p / ramp_up.max(0.001)
            } else if p < peak {
                1.0
            } else if p < ramp_down {
                1.0 - (p - peak) / (ramp_down - peak).max(0.001)
            } else {
                0.0
            };
            let dissolve_start = peak * 0.8;
            let dissolve_t =
                ((p - dissolve_start) / (1.0 - dissolve_start).max(0.001)).clamp(0.0, 1.0);
            let inv_d = 1.0 - dissolve_t;
            for i in (0..byte_count).step_by(4) {
                let base_r = from[i] as f32 * inv_d + to[i] as f32 * dissolve_t;
                let base_g = from[i + 1] as f32 * inv_d + to[i + 1] as f32 * dissolve_t;
                let base_b = from[i + 2] as f32 * inv_d + to[i + 2] as f32 * dissolve_t;
                output[i] =
                    (base_r + (flash_r - base_r) * flash_t).clamp(0.0, 255.0) as u8;
                output[i + 1] =
                    (base_g + (flash_g - base_g) * flash_t).clamp(0.0, 255.0) as u8;
                output[i + 2] =
                    (base_b + (flash_b - base_b) * flash_t).clamp(0.0, 255.0) as u8;
                output[i + 3] = 255;
            }
        }
        TransitionKind::HueRotate => {
            // Hue rotate export (default 1 rotation).
            let hue_shift = (p * std::f32::consts::PI).sin() * 360.0;
            let inv = 1.0 - p;
            for i in (0..byte_count).step_by(4) {
                let base_r = (from[i] as f32 * inv + to[i] as f32 * p) / 255.0;
                let base_g = (from[i + 1] as f32 * inv + to[i + 1] as f32 * p) / 255.0;
                let base_b = (from[i + 2] as f32 * inv + to[i + 2] as f32 * p) / 255.0;
                let cmax = base_r.max(base_g).max(base_b);
                let cmin = base_r.min(base_g).min(base_b);
                let delta = cmax - cmin;
                let h = if delta < 0.001 {
                    0.0
                } else if (cmax - base_r).abs() < 0.001 {
                    60.0 * (((base_g - base_b) / delta) % 6.0)
                } else if (cmax - base_g).abs() < 0.001 {
                    60.0 * ((base_b - base_r) / delta + 2.0)
                } else {
                    60.0 * ((base_r - base_g) / delta + 4.0)
                };
                let s = if cmax < 0.001 { 0.0 } else { delta / cmax };
                let v = cmax;
                let h2 = ((h + hue_shift) % 360.0 + 360.0) % 360.0;
                let c = v * s;
                let x2 = c * (1.0 - ((h2 / 60.0) % 2.0 - 1.0).abs());
                let m = v - c;
                let hi = (h2 / 60.0) as u32 % 6;
                let (rr, gg, bb) = match hi {
                    0 => (c, x2, 0.0),
                    1 => (x2, c, 0.0),
                    2 => (0.0, c, x2),
                    3 => (0.0, x2, c),
                    4 => (x2, 0.0, c),
                    _ => (c, 0.0, x2),
                };
                output[i] = ((rr + m) * 255.0).clamp(0.0, 255.0) as u8;
                output[i + 1] = ((gg + m) * 255.0).clamp(0.0, 255.0) as u8;
                output[i + 2] = ((bb + m) * 255.0).clamp(0.0, 255.0) as u8;
                output[i + 3] = 255;
            }
        }
        TransitionKind::NegativeFlash => {
            // Negative flash export with defaults (intensity=1, peak=0.5).
            let peak = 0.5_f32;
            let ramp_up = peak * 0.6;
            let ramp_down = peak + (1.0 - peak) * 0.4;
            let flash_t = if p < ramp_up {
                p / ramp_up.max(0.001)
            } else if p < peak {
                1.0
            } else if p < ramp_down {
                1.0 - (p - peak) / (ramp_down - peak).max(0.001)
            } else {
                0.0
            };
            let dissolve_start = peak * 0.8;
            let dissolve_t =
                ((p - dissolve_start) / (1.0 - dissolve_start).max(0.001)).clamp(0.0, 1.0);
            let inv_d = 1.0 - dissolve_t;
            for i in (0..byte_count).step_by(4) {
                let base_r = from[i] as f32 * inv_d + to[i] as f32 * dissolve_t;
                let base_g = from[i + 1] as f32 * inv_d + to[i + 1] as f32 * dissolve_t;
                let base_b = from[i + 2] as f32 * inv_d + to[i + 2] as f32 * dissolve_t;
                output[i] =
                    (base_r + (255.0 - 2.0 * base_r) * flash_t).clamp(0.0, 255.0) as u8;
                output[i + 1] =
                    (base_g + (255.0 - 2.0 * base_g) * flash_t).clamp(0.0, 255.0) as u8;
                output[i + 2] =
                    (base_b + (255.0 - 2.0 * base_b) * flash_t).clamp(0.0, 255.0) as u8;
                output[i + 3] = 255;
            }
        }
        TransitionKind::Sepia => {
            let envelope = (p * std::f32::consts::PI).sin();
            let inv = 1.0 - p;
            for i in (0..byte_count).step_by(4) {
                let base_r = from[i] as f32 * inv + to[i] as f32 * p;
                let base_g = from[i + 1] as f32 * inv + to[i + 1] as f32 * p;
                let base_b = from[i + 2] as f32 * inv + to[i + 2] as f32 * p;
                let luma = 0.2126 * base_r + 0.7152 * base_g + 0.0722 * base_b;
                let sepia_r = (luma * 1.2).clamp(0.0, 255.0);
                let sepia_g = luma.clamp(0.0, 255.0);
                let sepia_b = (luma * 0.75).clamp(0.0, 255.0);
                output[i] = (base_r + (sepia_r - base_r) * envelope + 0.5) as u8;
                output[i + 1] = (base_g + (sepia_g - base_g) * envelope + 0.5) as u8;
                output[i + 2] = (base_b + (sepia_b - base_b) * envelope + 0.5) as u8;
                output[i + 3] = 255;
            }
        }
        TransitionKind::GlitchDissolve => {
            // Glitch export: simplified block-shift + noise with default params.
            let intensity = 1.0_f32;
            let block_size = 16_usize;
            let envelope = (p * std::f32::consts::PI).sin() * intensity;
            let w = width as usize;
            let h = height as usize;
            let inv = 1.0 - p;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let block_row = y / block_size.max(1);
                    let hash = ((block_row as f32 * 127.1 + p * 311.7).sin() * 43758.545)
                        .fract()
                        .abs();
                    let hash2 = ((block_row as f32 * 269.5 + p * 183.3).sin() * 12345.678)
                        .fract()
                        .abs();
                    let shift = ((hash - 0.5) * envelope * 40.0) as isize;
                    let sx = (x as isize + shift).clamp(0, w as isize - 1) as usize;
                    let r_sx = (x as isize + shift / 2).clamp(0, w as isize - 1) as usize;
                    let b_sx = (x as isize - shift / 2).clamp(0, w as isize - 1) as usize;
                    let si = (y * w + sx) * 4;
                    let r_si = (y * w + r_sx) * 4;
                    let b_si = (y * w + b_sx) * 4;
                    let noise = ((hash2 - 0.5) * envelope * 60.0) as i16;
                    output[fi] = ((from[r_si] as f32 * inv + to[r_si] as f32 * p) as i16
                        + noise)
                        .clamp(0, 255) as u8;
                    output[fi + 1] = ((from[si + 1] as f32 * inv + to[si + 1] as f32 * p)
                        as i16
                        + noise / 2)
                        .clamp(0, 255) as u8;
                    output[fi + 2] = ((from[b_si + 2] as f32 * inv + to[b_si + 2] as f32 * p)
                        as i16
                        + noise)
                        .clamp(0, 255) as u8;
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::Desaturate => {
            let depth = 1.0_f32;
            let envelope = (p * std::f32::consts::PI).sin() * depth;
            let inv = 1.0 - p;
            for i in (0..byte_count).step_by(4) {
                let base_r = from[i] as f32 * inv + to[i] as f32 * p;
                let base_g = from[i + 1] as f32 * inv + to[i + 1] as f32 * p;
                let base_b = from[i + 2] as f32 * inv + to[i + 2] as f32 * p;
                let luma = 0.2126 * base_r + 0.7152 * base_g + 0.0722 * base_b;
                output[i] = (base_r + (luma - base_r) * envelope + 0.5) as u8;
                output[i + 1] = (base_g + (luma - base_g) * envelope + 0.5) as u8;
                output[i + 2] = (base_b + (luma - base_b) * envelope + 0.5) as u8;
                output[i + 3] = 255;
            }
        }
        TransitionKind::AdditiveDissolve => {
            let intensity = 1.0_f32;
            let envelope = (p * std::f32::consts::PI).sin() * intensity;
            let add = envelope * 0.5;
            for i in (0..byte_count).step_by(4) {
                for c in 0..3 {
                    let base = from[i + c] as f32 * (1.0 - p) + to[i + c] as f32 * p;
                    output[i + c] =
                        ((base / 255.0 + add).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
                output[i + 3] = 255;
            }
        }
        TransitionKind::FadeToColor => {
            // Default: fade to black (color=[0,0,0]).
            let mid_start = 0.5_f32;
            let mid_end = 0.5_f32;
            for i in (0..byte_count).step_by(4) {
                if p <= mid_start {
                    let t = p / mid_start.max(0.001);
                    for c in 0..3 {
                        output[i + c] = (from[i + c] as f32 * (1.0 - t) + 0.5) as u8;
                    }
                } else if p >= mid_end {
                    let t = (p - mid_end) / (1.0 - mid_end).max(0.001);
                    for c in 0..3 {
                        output[i + c] = (to[i + c] as f32 * t + 0.5) as u8;
                    }
                } else {
                    output[i] = 0;
                    output[i + 1] = 0;
                    output[i + 2] = 0;
                }
                output[i + 3] = 255;
            }
        }
        // BlurDissolve and Spin are complex — fallback to cross dissolve in export
        TransitionKind::BlurDissolve | TransitionKind::Spin => {
            let inv = 1.0 - p;
            for i in 0..byte_count {
                output[i] = (from[i] as f32 * inv + to[i] as f32 * p + 0.5) as u8;
            }
        }
    }

    output
}

fn wipe_horizontal(
    output: &mut [u8],
    from: &[u8],
    to: &[u8],
    width: u32,
    height: u32,
    threshold: u32,
    to_is_before: bool,
) {
    for y in 0..height {
        for x in 0..width {
            let idx = (y * width + x) as usize * 4;
            let use_to = if to_is_before {
                x < threshold
            } else {
                x >= threshold
            };
            let src = if use_to { to } else { from };
            output[idx..idx + 4].copy_from_slice(&src[idx..idx + 4]);
        }
    }
}

fn wipe_vertical(
    output: &mut [u8],
    from: &[u8],
    to: &[u8],
    width: u32,
    height: u32,
    threshold: u32,
    to_is_before: bool,
) {
    for y in 0..height {
        for x in 0..width {
            let idx = (y * width + x) as usize * 4;
            let use_to = if to_is_before {
                y < threshold
            } else {
                y >= threshold
            };
            let src = if use_to { to } else { from };
            output[idx..idx + 4].copy_from_slice(&src[idx..idx + 4]);
        }
    }
}

fn dip_transition(output: &mut [u8], from: &[u8], to: &[u8], p: f32, dip_value: u8) {
    let dv = dip_value as f32;
    if p < 0.5 {
        let t = p * 2.0;
        let inv = 1.0 - t;
        for i in (0..output.len()).step_by(4) {
            output[i] = (from[i] as f32 * inv + dv * t + 0.5) as u8;
            output[i + 1] = (from[i + 1] as f32 * inv + dv * t + 0.5) as u8;
            output[i + 2] = (from[i + 2] as f32 * inv + dv * t + 0.5) as u8;
            output[i + 3] = from[i + 3];
        }
    } else {
        let t = (p - 0.5) * 2.0;
        let inv = 1.0 - t;
        for i in (0..output.len()).step_by(4) {
            output[i] = (dv * inv + to[i] as f32 * t + 0.5) as u8;
            output[i + 1] = (dv * inv + to[i + 1] as f32 * t + 0.5) as u8;
            output[i + 2] = (dv * inv + to[i + 2] as f32 * t + 0.5) as u8;
            output[i + 3] = to[i + 3];
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_frame(r: u8, g: u8, b: u8, a: u8, w: u32, h: u32) -> Vec<u8> {
        vec![r, g, b, a].repeat((w * h) as usize)
    }

    #[test]
    fn blend_over_fully_opaque() {
        let dst = solid_frame(255, 0, 0, 255, 2, 2); // red
        let src = solid_frame(0, 255, 0, 255, 2, 2); // green
        let result = blend_over(dst, &src, 1.0);
        // Fully opaque green over red = green
        assert_eq!(result[0], 0);
        assert_eq!(result[1], 255);
        assert_eq!(result[2], 0);
        assert_eq!(result[3], 255);
    }

    #[test]
    fn blend_over_half_opacity() {
        let dst = solid_frame(200, 0, 0, 255, 2, 2); // red
        let src = solid_frame(0, 200, 0, 255, 2, 2); // green
        let result = blend_over(dst, &src, 0.5);
        // 50% green over red should be roughly 50/50 mix
        let r = result[0] as i32;
        let g = result[1] as i32;
        assert!(r > 50 && r < 150, "expected ~100 red, got {r}");
        assert!(g > 50 && g < 150, "expected ~100 green, got {g}");
    }

    #[test]
    fn blend_over_transparent_src() {
        let dst = solid_frame(255, 0, 0, 255, 2, 2); // red
        let src = solid_frame(0, 255, 0, 0, 2, 2); // green but transparent
        let result = blend_over(dst, &src, 1.0);
        // Transparent source should not affect destination
        assert_eq!(result[0], 255); // still red
        assert_eq!(result[1], 0);
    }

    #[test]
    fn cross_dissolve_midpoint() {
        let from = solid_frame(200, 0, 0, 255, 4, 4);
        let to = solid_frame(0, 0, 200, 255, 4, 4);
        let result = render_transition(TransitionKind::CrossDissolve, &from, &to, 0.5, 4, 4);
        assert!((result[0] as i32 - 100).abs() <= 1);
        assert!((result[2] as i32 - 100).abs() <= 1);
    }

    #[test]
    fn cross_dissolve_endpoints() {
        let from = solid_frame(255, 0, 0, 255, 2, 2);
        let to = solid_frame(0, 0, 255, 255, 2, 2);

        let at_zero = render_transition(TransitionKind::CrossDissolve, &from, &to, 0.0, 2, 2);
        assert_eq!(at_zero[0], 255);
        assert_eq!(at_zero[2], 0);

        let at_one = render_transition(TransitionKind::CrossDissolve, &from, &to, 1.0, 2, 2);
        assert_eq!(at_one[0], 0);
        assert_eq!(at_one[2], 255);
    }

    #[test]
    fn wipe_left_halfway() {
        let from = solid_frame(255, 0, 0, 255, 10, 1);
        let to = solid_frame(0, 255, 0, 255, 10, 1);
        let result = render_transition(TransitionKind::WipeLeft, &from, &to, 0.5, 10, 1);
        // First 5 pixels: to (green), last 5: from (red)
        assert_eq!(result[0], 0); // pixel 0 green
        assert_eq!(result[1], 255);
        assert_eq!(result[24], 255); // pixel 6 red
        assert_eq!(result[25], 0);
    }

    #[test]
    fn apply_opacity_reduces_alpha() {
        let frame = solid_frame(255, 0, 0, 200, 2, 2);
        let result = apply_opacity(frame, 0.5);
        assert_eq!(result[3], 100);
    }

    #[test]
    fn apply_opacity_full_passthrough() {
        let frame = solid_frame(255, 0, 0, 200, 2, 2);
        let result = apply_opacity(frame.clone(), 1.0);
        assert_eq!(result, frame);
    }

    #[test]
    fn black_frame_is_all_zeros() {
        let f = black_frame(4, 4);
        assert!(f.iter().all(|&b| b == 0));
        assert_eq!(f.len(), 4 * 4 * 4);
    }
}
