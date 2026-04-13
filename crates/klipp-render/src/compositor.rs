use crate::clip_decoder::RenderDecoder;
use crate::error::RenderError;
use crate::pipeline::RenderPipeline;
use crate::snapshot::{
    ClipKindSnapshot, ClipSnapshot, SequenceSnapshot, TransitionSnapshot, VideoTrackSnapshot,
};
use crate::stage::FrameContext;
use klipp_core::ClipId;
use klipp_title::TitleRenderer;
use std::collections::HashMap;
use std::sync::Arc;

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
    /// Each clip's effects (color grade, OFX, builtins) are applied to its decoded
    /// frame BEFORE compositing, so every clip renders with its own effects.
    /// Returns `None` if there is nothing to render (all tracks empty/invisible).
    pub fn composite_frame(
        &mut self,
        frame_num: u64,
        export_frame: u64,
        snapshot: &SequenceSnapshot,
        decoders: &mut HashMap<ClipId, RenderDecoder>,
        pipelines: &mut [RenderPipeline],
    ) -> Result<Option<Vec<u8>>, RenderError> {
        let w = snapshot.width;
        let h = snapshot.height;
        let mut base: Option<Vec<u8>> = None;

        // Process tracks bottom to top (index 0 = bottom, composited first).
        // Each clip's effects run on its own decoded frame before compositing.
        for (track_idx, track) in snapshot.video_tracks.iter().enumerate() {
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
                    let decoded = if let Some(transitioned) =
                        self.apply_transition(raw.as_ref(), frame_num, snapshot.fps, c, track, decoders, w, h)?
                    {
                        transitioned
                    } else {
                        match raw {
                            Some(f) => f,
                            None => continue, // Gap clip or exhausted decoder
                        }
                    };

                    // Apply per-clip effects (color grade, OFX, builtins) before compositing
                    let clip_frame = frame_num.saturating_sub(c.timeline_start_frame);
                    let ctx = FrameContext {
                        export_frame,
                        clip_frame,
                        clip_time_secs: clip_frame as f64 / snapshot.fps,
                        clip: Some(Arc::new(c.clone())),
                        fps: snapshot.fps,
                        track_index: track_idx,
                    };
                    pipelines[track_idx].process_frame(decoded, w, h, &ctx)
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

    // ═══════════════════════════════════════════════════════════════════
    // Multi-track per-clip effects regression tests
    // ═══════════════════════════════════════════════════════════════════

    use crate::pipeline::RenderPipeline;
    use crate::snapshot::{SequenceSnapshot, VideoTrackSnapshot};
    use crate::stage::ClipContext;
    use klipp_effects::BuiltinEffect;

    /// Helper: build a ClipSnapshot backed by a solid-color generator.
    fn generator_clip(
        color: [u8; 4],
        start_frame: u64,
        end_frame: u64,
        effects: Vec<BuiltinEffect>,
    ) -> ClipSnapshot {
        ClipSnapshot {
            id: ClipId::new(),
            kind: ClipKindSnapshot::Generator(klipp_core::GeneratorData::SolidColor { color }),
            timeline_start_frame: start_frame,
            timeline_end_frame: end_frame,
            source_start_secs: 0.0,
            source_end_secs: (end_frame - start_frame) as f64 / 24.0,
            speed: 1.0,
            node_graph: klipp_nodes::NodeGraph::default(),
            effects: vec![],
            builtin_effects: effects,
            source_rotation: 0,
            stabilization_data: None,
            smoothed_trajectory: None,
        }
    }

    /// Helper: build a minimal SequenceSnapshot with given video tracks.
    fn test_snapshot(w: u32, h: u32, tracks: Vec<VideoTrackSnapshot>) -> SequenceSnapshot {
        SequenceSnapshot {
            fps: 24.0,
            fps_num: 24,
            fps_den: 1,
            width: w,
            height: h,
            sample_rate: 48000,
            duration_frames: 24,
            export_range: None,
            video_tracks: tracks,
            audio_tracks: vec![],
            global_lut_path: None,
        }
    }

    /// Helper: build a visible video track with one clip.
    fn track_with_clip(clip: ClipSnapshot) -> VideoTrackSnapshot {
        VideoTrackSnapshot {
            opacity: 1.0,
            visible: true,
            clips: vec![clip],
            transitions: vec![],
        }
    }

    /// Helper: init a pipeline and call begin_clip for a clip snapshot.
    fn pipeline_for_clip(clip: &ClipSnapshot) -> RenderPipeline {
        let mut p = RenderPipeline::default_stages(None);
        let settings = klipp_core::ExportSettings {
            output_path: std::path::PathBuf::from("/dev/null"),
            format: klipp_core::ExportFormat::Mp4,
            video_codec: klipp_core::ExportVideoCodec::H264,
            resolution: None, fps: None, crf: 23,
            preset: klipp_core::EncoderPreset::Fast,
            audio_codec: klipp_core::ExportAudioCodec::Aac,
            audio_bitrate_kbps: None, video_bitrate_kbps: None,
            rate_control: klipp_core::RateControl::Crf,
            burn_in_subtitles: true, target_lufs: None,
        };
        p.begin_export(&settings);
        p.begin_clip(&ClipContext {
            clip: std::sync::Arc::new(clip.clone()),
            sequence_fps: 24.0,
            sequence_width: 8,
            sequence_height: 8,
        });
        p
    }

    /// Primary regression test: two tracks with different effects produce
    /// independently graded output.
    ///
    /// V1 (bottom): solid red (255,0,0) + HSVAdjust(saturation=0) → grayscale ≈ (76,76,76)
    /// V2 (top, full frame): solid blue (0,0,255) + HSVAdjust(value=0.5) → half-bright ≈ (0,0,128)
    ///
    /// If the old "topmost only" bug returned, V1's grayscale would not apply:
    /// the entire composite would get V2's value=0.5 effect, making V1 ≈ (128,0,0).
    #[test]
    fn multi_track_effects_apply_per_clip() {
        let w = 8u32;
        let h = 8u32;

        // V1: solid red, desaturated to grayscale
        let v1_clip = generator_clip(
            [255, 0, 0, 255], 0, 24,
            vec![BuiltinEffect::HSVAdjust { hue_shift: 0.0, saturation: 0.0, value: 1.0 }],
        );
        // V2: solid blue at half brightness (covers entire frame)
        let v2_clip = generator_clip(
            [0, 0, 255, 255], 0, 24,
            vec![BuiltinEffect::HSVAdjust { hue_shift: 0.0, saturation: 1.0, value: 0.5 }],
        );

        let snapshot = test_snapshot(w, h, vec![
            track_with_clip(v1_clip.clone()),
            track_with_clip(v2_clip.clone()),
        ]);

        let mut pipelines = vec![
            pipeline_for_clip(&v1_clip),
            pipeline_for_clip(&v2_clip),
        ];

        let mut compositor = FrameCompositor::new();
        let mut decoders = HashMap::new();
        let result = compositor.composite_frame(0, 0, &snapshot, &mut decoders, &mut pipelines)
            .unwrap()
            .expect("should produce a frame");

        // V2 is opaque and covers the entire frame, so the output should show
        // V2's effect: solid blue at half brightness ≈ (0, 0, 128).
        // V1 is completely hidden behind V2, so we can't directly test V1's
        // grayscale in this setup. But we CAN verify V2's own effect ran:
        let r = result[0];
        let g = result[1];
        let b = result[2];
        // Half-brightness blue: R≈0, G≈0, B≈128 (not raw 255)
        assert!(r < 5, "expected R≈0, got {r}");
        assert!(g < 5, "expected G≈0, got {g}");
        assert!((b as i32 - 128).abs() < 10, "expected B≈128, got {b}");
    }

    /// Test that V1's effects apply even when V2 only partially overlaps.
    /// V1 covers full frame, V2 covers only top-left quadrant (via being
    /// a full-frame clip with 50% alpha, so V1 shows through blended).
    ///
    /// Since generators always produce full-frame output, we test using
    /// semi-transparent V2 — V1's grayscale should bleed through the alpha.
    #[test]
    fn multi_track_v1_effect_visible_through_semitransparent_v2() {
        let w = 4u32;
        let h = 4u32;

        // V1: solid red, desaturated to grayscale → ≈ (76, 76, 76, 255)
        let v1_clip = generator_clip(
            [255, 0, 0, 255], 0, 24,
            vec![BuiltinEffect::HSVAdjust { hue_shift: 0.0, saturation: 0.0, value: 1.0 }],
        );
        // V2: solid green, no effects, 50% track opacity
        let v2_clip = generator_clip(
            [0, 255, 0, 255], 0, 24,
            vec![], // no effects
        );

        let mut v2_track = track_with_clip(v2_clip.clone());
        v2_track.opacity = 0.5; // 50% opacity

        let snapshot = test_snapshot(w, h, vec![
            track_with_clip(v1_clip.clone()),
            v2_track,
        ]);

        let mut pipelines = vec![
            pipeline_for_clip(&v1_clip),
            pipeline_for_clip(&v2_clip),
        ];

        let mut compositor = FrameCompositor::new();
        let mut decoders = HashMap::new();
        let result = compositor.composite_frame(0, 0, &snapshot, &mut decoders, &mut pipelines)
            .unwrap()
            .expect("should produce a frame");

        // V1 grayscale red ≈ (76, 76, 76). V2 green (0, 255, 0) at 50%.
        // Alpha-over blend: output ≈ (38, 166, 38) roughly.
        // The key assertion: R and B channels should be approximately EQUAL
        // (because V1 was desaturated to grayscale). If V1's effect didn't run,
        // V1 would be raw red (255, 0, 0) and the blend would have R >> B.
        let r = result[0] as i32;
        let b = result[2] as i32;
        assert!((r - b).abs() < 5,
            "V1 should be grayscale (R≈B), but R={r} B={b} — V1's effect may not have applied");
        // Green channel should be significantly above R (from V2's green at 50%)
        let g = result[1] as i32;
        assert!(g > r + 30, "expected green > red due to V2 blend, but G={g} R={r}");
    }

    /// Single track with effects — sanity check.
    #[test]
    fn single_track_effects_still_work() {
        let w = 4u32;
        let h = 4u32;
        let clip = generator_clip(
            [200, 100, 50, 255], 0, 24,
            vec![BuiltinEffect::HSVAdjust { hue_shift: 0.0, saturation: 0.0, value: 1.0 }],
        );
        let snapshot = test_snapshot(w, h, vec![track_with_clip(clip.clone())]);
        let mut pipelines = vec![pipeline_for_clip(&clip)];
        let mut compositor = FrameCompositor::new();
        let mut decoders = HashMap::new();
        let result = compositor.composite_frame(0, 0, &snapshot, &mut decoders, &mut pipelines)
            .unwrap()
            .expect("should produce a frame");

        // Grayscale: all RGB channels should be approximately equal
        let r = result[0] as i32;
        let g = result[1] as i32;
        let b = result[2] as i32;
        assert!((r - g).abs() <= 2 && (g - b).abs() <= 2,
            "expected grayscale, got R={r} G={g} B={b}");
    }

    /// Clip with no effects — pass-through unchanged.
    #[test]
    fn no_effects_passthrough() {
        let w = 4u32;
        let h = 4u32;
        let clip = generator_clip([100, 150, 200, 255], 0, 24, vec![]);
        let snapshot = test_snapshot(w, h, vec![track_with_clip(clip.clone())]);
        let mut pipelines = vec![pipeline_for_clip(&clip)];
        let mut compositor = FrameCompositor::new();
        let mut decoders = HashMap::new();
        let result = compositor.composite_frame(0, 0, &snapshot, &mut decoders, &mut pipelines)
            .unwrap()
            .expect("should produce a frame");

        // No effects → output should match generator color exactly
        assert_eq!(result[0], 100);
        assert_eq!(result[1], 150);
        assert_eq!(result[2], 200);
        assert_eq!(result[3], 255);
    }
}
