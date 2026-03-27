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
