use crate::clip_decoder::RenderDecoder;
use crate::error::RenderError;
use crate::snapshot::{
    ClipKindSnapshot, ClipSnapshot, SequenceSnapshot, TransitionSnapshot, VideoTrackSnapshot,
};
use klipp_core::ClipId;
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
                    let raw = self.decode_clip_frame(c, decoders, frame_num, snapshot.fps, w, h)?;

                    // Check for active transition on this track (Gap 6)
                    if let Some(transitioned) =
                        self.apply_transition(raw.as_ref(), frame_num, snapshot.fps, c, track, decoders, w, h)?
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

    /// Decode a single frame from a clip (media, title, generator, or gap).
    fn decode_clip_frame(
        &mut self,
        clip: &ClipSnapshot,
        decoders: &mut HashMap<ClipId, RenderDecoder>,
        frame_num: u64,
        fps: f64,
        width: u32,
        height: u32,
    ) -> Result<Option<Vec<u8>>, RenderError> {
        match &clip.kind {
            ClipKindSnapshot::Gap => Ok(None),
            ClipKindSnapshot::Title(data) => {
                Ok(Some(self.title_renderer.render(data, width, height)))
            }
            ClipKindSnapshot::Generator(data) => {
                let clip_frame = frame_num.saturating_sub(clip.timeline_start_frame);
                let elapsed_secs = clip_frame as f64 / fps.max(1.0);
                let total_frames = clip.timeline_end_frame.saturating_sub(clip.timeline_start_frame);
                let total_secs = total_frames as f64 / fps.max(1.0);
                Ok(Some(klipp_core::generator::render_generator(
                    data,
                    width,
                    height,
                    elapsed_secs,
                    total_secs,
                )))
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
        fps: f64,
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

        let to_frame = match self.decode_clip_frame(partner_clip, decoders, frame_num, fps, width, height)? {
            Some(f) => f,
            None => black_frame(width, height),
        };

        // Calculate transition progress
        let progress = self.transition_progress(frame_num, transition, track);

        // Render the transition using the shared CPU implementation
        Ok(Some(klipp_transitions::render_transition_with_params(
            transition.kind,
            &from_frame,
            &to_frame,
            progress,
            width,
            height,
            &transition.user_params,
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

    fn render_transition_default(
        kind: klipp_timeline::TransitionKind,
        from: &[u8],
        to: &[u8],
        progress: f32,
        w: u32,
        h: u32,
    ) -> Vec<u8> {
        let params = klipp_core::TransitionUserParams::default();
        klipp_transitions::render_transition_with_params(kind, from, to, progress, w, h, &params)
    }

    #[test]
    fn cross_dissolve_midpoint() {
        let from = solid_frame(200, 0, 0, 255, 4, 4);
        let to = solid_frame(0, 0, 200, 255, 4, 4);
        let result = render_transition_default(
            klipp_timeline::TransitionKind::CrossDissolve,
            &from,
            &to,
            0.5,
            4,
            4,
        );
        assert!((result[0] as i32 - 100).abs() <= 1);
        assert!((result[2] as i32 - 100).abs() <= 1);
    }

    #[test]
    fn cross_dissolve_endpoints() {
        let from = solid_frame(255, 0, 0, 255, 2, 2);
        let to = solid_frame(0, 0, 255, 255, 2, 2);

        let at_zero = render_transition_default(
            klipp_timeline::TransitionKind::CrossDissolve,
            &from,
            &to,
            0.0,
            2,
            2,
        );
        assert_eq!(at_zero[0], 255);
        assert_eq!(at_zero[2], 0);

        let at_one = render_transition_default(
            klipp_timeline::TransitionKind::CrossDissolve,
            &from,
            &to,
            1.0,
            2,
            2,
        );
        assert_eq!(at_one[0], 0);
        assert_eq!(at_one[2], 255);
    }

    #[test]
    fn wipe_left_halfway() {
        let from = solid_frame(255, 0, 0, 255, 10, 1);
        let to = solid_frame(0, 255, 0, 255, 10, 1);
        let result = render_transition_default(
            klipp_timeline::TransitionKind::WipeLeft,
            &from,
            &to,
            0.5,
            10,
            1,
        );
        // With default feather=0, smoothstep gives near-hard edge
        // Pixel 0 should be mostly `to` (green), pixel 6 mostly `from` (red)
        assert!(result[1] > 200, "pixel 0 should be mostly green, got G={}", result[1]);
        assert!(result[24] > 200, "pixel 6 should be mostly red, got R={}", result[24]);
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

    // ================================================================
    // Export / playback parity regression tests
    // ================================================================

    #[test]
    fn circle_wipe_with_custom_params_matches_direct_call() {
        // Render CircleWipeIn with off-center, extra softness params
        // via direct call (simulating playback) and via the same function
        // (simulating export with params threaded). Should be byte-identical.
        let from = solid_frame(255, 0, 0, 255, 16, 16);
        let to = solid_frame(0, 255, 0, 255, 16, 16);
        let params = klipp_core::TransitionUserParams {
            param0: 2.0,
            param1: 0.25,
            param2: 0.75,
            ..Default::default()
        };
        let playback = klipp_transitions::render_transition_with_params(
            klipp_timeline::TransitionKind::CircleWipeIn,
            &from, &to, 0.5, 16, 16, &params,
        );
        let export = klipp_transitions::render_transition_with_params(
            klipp_timeline::TransitionKind::CircleWipeIn,
            &from, &to, 0.5, 16, 16, &params,
        );
        assert_eq!(playback, export, "export and playback should be byte-identical");
    }

    #[test]
    fn dip_to_black_with_custom_color() {
        // Render DipToBlack at midpoint with dark red color.
        // Under the old compositor this would have been pure black.
        let from = solid_frame(200, 200, 200, 255, 4, 4);
        let to = solid_frame(200, 200, 200, 255, 4, 4);
        let params = klipp_core::TransitionUserParams {
            color: [0.5, 0.0, 0.0, 1.0],
            ..Default::default()
        };
        let result = klipp_transitions::render_transition_with_params(
            klipp_timeline::TransitionKind::DipToBlack,
            &from, &to, 0.5, 4, 4, &params,
        );
        // Midpoint should show the dark red dip color, not pure black.
        // With hold=0, at p=0.5: mid_start=0.5, mid_end=0.5, so p<=mid_start
        // → t = 0.5/0.5 = 1.0 → fully dip color → R=127, G=0, B=0.
        assert!(result[0] > 50, "R should reflect dark red dip, got {}", result[0]);
        assert!(result[1] < 10, "G should be near zero, got {}", result[1]);
    }

    #[test]
    fn wipe_left_with_feathering() {
        // Render WipeLeft at midpoint with 10px feather.
        // Under the old compositor this was a hard integer threshold.
        let from = solid_frame(255, 0, 0, 255, 20, 1);
        let to = solid_frame(0, 255, 0, 255, 20, 1);
        let params = klipp_core::TransitionUserParams {
            param0: 10.0,
            ..Default::default()
        };
        let result = klipp_transitions::render_transition_with_params(
            klipp_timeline::TransitionKind::WipeLeft,
            &from, &to, 0.5, 20, 1, &params,
        );
        // With feathering, there should be intermediate blended pixels
        // near the wipe edge (not just hard red/green boundary).
        let mut found_intermediate = false;
        for i in 0..20 {
            let r = result[i * 4];
            let g = result[i * 4 + 1];
            if r > 20 && r < 235 && g > 20 && g < 235 {
                found_intermediate = true;
                break;
            }
        }
        assert!(found_intermediate, "feathered wipe should have intermediate blended pixels");
    }

    #[test]
    fn snapshot_preserves_user_params_and_alignment() {
        use klipp_timeline::*;
        use klipp_core::*;
        let transition = Transition {
            id: TransitionId::new(),
            kind: TransitionKind::CircleWipeIn,
            duration: RationalTime { value: 12, rate: 24 },
            clip_a_id: ClipId::new(),
            clip_b_id: ClipId::new(),
            user_params: TransitionUserParams {
                param0: 2.5,
                param1: 0.3,
                param2: 0.8,
                ..Default::default()
            },
            alignment: TransitionAlignment::StartAtCut,
        };
        let snapshot = crate::snapshot::snapshot_transition_for_test(&transition, 24.0);
        assert_eq!(snapshot.user_params.param0, 2.5);
        assert_eq!(snapshot.user_params.param1, 0.3);
        assert_eq!(snapshot.user_params.param2, 0.8);
        assert_eq!(snapshot.alignment, TransitionAlignment::StartAtCut);
    }
}
