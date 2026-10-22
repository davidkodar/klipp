/// CPU fallback transition rendering.
///
/// The GPU path in gpu.rs is preferred for pixel math; this module is the
/// fallback when no adapter is available, and the reference the GPU path
/// is checked against.
use klipp_timeline::TransitionKind;

/// Map TransitionKind to the GPU shader's kind_id.
pub fn transition_kind_to_id(kind: TransitionKind) -> u32 {
    klipp_core::spec_for_kind(kind).kind_id as u32
}

/// Map a u8 kind_id back to TransitionKind.
pub fn transition_kind_from_id(id: u8) -> TransitionKind {
    klipp_core::spec_for_kind_id(id)
        .map(|s| s.kind)
        .unwrap_or(TransitionKind::CrossDissolve)
}

/// All transition kinds with their display labels.
/// Note: the display names here use the full names from the registry, not the
/// short ribbon labels used on the timeline.
pub fn all_transition_kinds() -> &'static [(u8, &'static str)] {
    // This is used by UI combo boxes. The labels here are the full display_name
    // from the registry. Built once at compile time via a const array.
    // Sized from the registry, so a new kind is never left out of the list.
    const ALL: [(u8, &str); klipp_core::TRANSITION_SPECS.len()] = {
        let specs = &klipp_core::TRANSITION_SPECS;
        let mut arr = [(0u8, ""); klipp_core::TRANSITION_SPECS.len()];
        let mut i = 0;
        while i < specs.len() {
            arr[i] = (specs[i].kind_id, specs[i].display_name);
            i += 1;
        }
        arr
    };
    &ALL
}

/// Information about an active transition at the current playhead position.
pub struct ActiveTransition {
    pub kind: TransitionKind,
    pub progress: f32,
    /// The partner clip (the clip on the other side of the transition from the main decoder).
    /// First half of the centered window: main=clip_a → partner=clip_b.
    /// Second half: main=clip_b → partner=clip_a.
    pub partner_clip: klipp_timeline::Clip,
    pub track_index: usize,
    /// True when playhead is at or past the cut (i.e. on clip_b's side of the window).
    /// Callers use this to order blend arguments as clip_a → clip_b regardless of
    /// which clip the main decoder is currently on.
    pub past_cut: bool,
    /// Per-kind tunable parameters copied from the source `Transition`.
    pub user_params: klipp_core::TransitionUserParams,
    /// Where the transition window sits relative to the cut.
    pub alignment: klipp_core::TransitionAlignment,
}

/// Check if the playhead is within a transition region on any video track.
/// Returns the transition info including the partner clip and progress.
pub fn find_active_transition(
    seq: &klipp_timeline::Sequence,
    playhead: klipp_core::Timecode,
) -> Option<ActiveTransition> {
    for (track_idx, track) in seq.tracks.iter().enumerate() {
        if track.kind != klipp_timeline::TrackKind::Video {
            continue;
        }

        for transition in &track.transitions {
            // Find clip_a and clip_b
            let clip_a = track.find_clip(transition.clip_a_id);
            let clip_b = track.find_clip(transition.clip_b_id);
            let (clip_a, clip_b) = match (clip_a, clip_b) {
                (Some(a), Some(b)) => (a, b),
                _ => continue,
            };

            // Window placement depends on the transition's alignment.
            //   Center      → [cut - D/2, cut + D/2)  — needs handles on both clips
            //   StartAtCut  → [cut, cut + D)          — entire window on clip_b
            //   EndAtCut    → [cut - D, cut)          — entire window on clip_a
            let cut = clip_b.timeline_in;
            let (trans_start, trans_end_tc) = match transition.alignment {
                klipp_core::TransitionAlignment::Center => {
                    let half = klipp_core::RationalTime {
                        value: transition.duration.value / 2,
                        rate: transition.duration.rate,
                    };
                    (cut - half, cut + half)
                }
                klipp_core::TransitionAlignment::StartAtCut => (cut, cut + transition.duration),
                klipp_core::TransitionAlignment::EndAtCut => (cut - transition.duration, cut),
            };

            if playhead >= trans_start && playhead < trans_end_tc {
                let elapsed = (playhead - trans_start).to_secs_f64();
                let total = transition.duration.to_secs_f64();
                let progress = if total > 0.0 {
                    (elapsed / total).clamp(0.0, 1.0) as f32
                } else {
                    0.0
                };

                // Partner-decoder selection depends on alignment:
                //   Center      → clip_b (pre-roll for second half; first half caches clip_a)
                //   StartAtCut  → clip_a (whole window is past_cut, partner supplies clip_a tail)
                //   EndAtCut    → clip_b (whole window is pre-cut, partner supplies clip_b head)
                let partner_clip = match transition.alignment {
                    klipp_core::TransitionAlignment::StartAtCut => clip_a.clone(),
                    klipp_core::TransitionAlignment::Center
                    | klipp_core::TransitionAlignment::EndAtCut => clip_b.clone(),
                };
                let past_cut = playhead >= clip_b.timeline_in;
                return Some(ActiveTransition {
                    kind: transition.kind,
                    progress,
                    partner_clip,
                    track_index: track_idx,
                    past_cut,
                    user_params: transition.user_params,
                    alignment: transition.alignment,
                });
            }
        }
    }
    None
}

/// Render a transition between two RGBA frames on the CPU.
///
/// This is a thin wrapper over [`render_transition_with_params`] that passes
/// default (kind-neutral) user parameters. Callers that want per-kind tunable
/// parameters (feather, colour, rotations, …) should use the `_with_params`
/// variant directly.
pub fn render_transition(
    kind: TransitionKind,
    from: &[u8],
    to: &[u8],
    progress: f32,
    width: u32,
    height: u32,
) -> Vec<u8> {
    // Use kind-specific defaults so callers that don't supply params still get
    // sensible behaviour for kinds whose look depends on the colour or other
    // tunables (e.g. DipToBlack vs DipToWhite distinguishable by colour).
    let user_params =
        klipp_core::TransitionUserParams::default_for_kind(transition_kind_to_id(kind) as u8);
    render_transition_with_params(kind, from, to, progress, width, height, &user_params)
}


// CPU pixel math lives in the klipp-transitions crate.
// Re-export for backward compatibility with existing callers.
pub use klipp_transitions::apply_easing;
pub use klipp_transitions::render_transition_with_params;

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 16;
    const H: u32 = 16;
    const TOLERANCE: i32 = 2; // rounding tolerance for blending

    fn make_solid(r: u8, g: u8, b: u8, a: u8, w: u32, h: u32) -> Vec<u8> {
        let mut buf = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            buf.extend_from_slice(&[r, g, b, a]);
        }
        buf
    }

    /// Check that output length matches expected dimensions.
    fn assert_dimensions(output: &[u8], w: u32, h: u32) {
        assert_eq!(output.len(), (w * h * 4) as usize);
    }

    /// Check that every pixel's RGB channels are approximately equal to expected values.
    /// Alpha is checked separately since some transitions handle it differently.
    fn assert_approx_rgb(output: &[u8], r: u8, g: u8, b: u8, tolerance: i32) {
        for i in (0..output.len()).step_by(4) {
            assert!(
                (output[i] as i32 - r as i32).abs() <= tolerance,
                "pixel {}: R expected ~{}, got {} (delta {})",
                i / 4, r, output[i], (output[i] as i32 - r as i32).abs()
            );
            assert!(
                (output[i + 1] as i32 - g as i32).abs() <= tolerance,
                "pixel {}: G expected ~{}, got {} (delta {})",
                i / 4, g, output[i + 1], (output[i + 1] as i32 - g as i32).abs()
            );
            assert!(
                (output[i + 2] as i32 - b as i32).abs() <= tolerance,
                "pixel {}: B expected ~{}, got {} (delta {})",
                i / 4, b, output[i + 2], (output[i + 2] as i32 - b as i32).abs()
            );
        }
    }

    /// Check that the output is NOT all-black (all RGB=0).
    fn assert_not_all_black(output: &[u8]) {
        let has_color = (0..output.len()).step_by(4).any(|i| {
            output[i] > 0 || output[i + 1] > 0 || output[i + 2] > 0
        });
        assert!(has_color, "output is unexpectedly all black");
    }

    /// Check that the output is NOT all-white (all RGB=255).
    fn assert_not_all_white(output: &[u8]) {
        let has_non_white = (0..output.len()).step_by(4).any(|i| {
            output[i] < 255 || output[i + 1] < 255 || output[i + 2] < 255
        });
        assert!(has_non_white, "output is unexpectedly all white");
    }

    /// Get pixel RGBA at (x, y).
    fn pixel_at(buf: &[u8], x: u32, y: u32, w: u32) -> (u8, u8, u8, u8) {
        let idx = (y * w + x) as usize * 4;
        (buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3])
    }

    // ========================================================================
    // CrossDissolve
    // ========================================================================

    #[test]
    fn cross_dissolve_at_zero() {
        let from = make_solid(255, 0, 0, 255, W, H);
        let to = make_solid(0, 0, 255, 255, W, H);
        let result = render_transition(TransitionKind::CrossDissolve, &from, &to, 0.0, W, H);
        assert_dimensions(&result, W, H);
        assert_approx_rgb(&result, 255, 0, 0, TOLERANCE);
    }

    #[test]
    fn cross_dissolve_at_one() {
        let from = make_solid(255, 0, 0, 255, W, H);
        let to = make_solid(0, 0, 255, 255, W, H);
        let result = render_transition(TransitionKind::CrossDissolve, &from, &to, 1.0, W, H);
        assert_dimensions(&result, W, H);
        assert_approx_rgb(&result, 0, 0, 255, TOLERANCE);
    }

    #[test]
    fn cross_dissolve_midpoint() {
        let from = make_solid(200, 0, 0, 255, W, H);
        let to = make_solid(0, 0, 200, 255, W, H);
        let result = render_transition(TransitionKind::CrossDissolve, &from, &to, 0.5, W, H);
        assert_dimensions(&result, W, H);
        assert_approx_rgb(&result, 100, 0, 100, TOLERANCE);
    }

    // ========================================================================
    // WipeLeft
    // ========================================================================

    #[test]
    fn wipe_left_at_zero() {
        let from = make_solid(255, 0, 0, 255, W, H);
        let to = make_solid(0, 255, 0, 255, W, H);
        let result = render_transition(TransitionKind::WipeLeft, &from, &to, 0.0, W, H);
        assert_dimensions(&result, W, H);
        assert_approx_rgb(&result, 255, 0, 0, 0); // all from
    }

    #[test]
    fn wipe_left_at_one() {
        let from = make_solid(255, 0, 0, 255, W, H);
        let to = make_solid(0, 255, 0, 255, W, H);
        let result = render_transition(TransitionKind::WipeLeft, &from, &to, 1.0, W, H);
        assert_dimensions(&result, W, H);
        assert_approx_rgb(&result, 0, 255, 0, 0); // all to
    }

    #[test]
    fn wipe_left_midpoint() {
        let from = make_solid(255, 0, 0, 255, 10, 1);
        let to = make_solid(0, 255, 0, 255, 10, 1);
        let result = render_transition(TransitionKind::WipeLeft, &from, &to, 0.5, 10, 1);
        assert_dimensions(&result, 10, 1);
        // Left half should be 'to', right half 'from'
        assert_eq!(pixel_at(&result, 0, 0, 10), (0, 255, 0, 255));
        assert_eq!(pixel_at(&result, 4, 0, 10), (0, 255, 0, 255));
        assert_eq!(pixel_at(&result, 6, 0, 10), (255, 0, 0, 255));
        assert_eq!(pixel_at(&result, 9, 0, 10), (255, 0, 0, 255));
    }

    // ========================================================================
    // WipeRight
    // ========================================================================

    #[test]
    fn wipe_right_at_zero() {
        let from = make_solid(255, 0, 0, 255, W, H);
        let to = make_solid(0, 255, 0, 255, W, H);
        let result = render_transition(TransitionKind::WipeRight, &from, &to, 0.0, W, H);
        assert_dimensions(&result, W, H);
        assert_approx_rgb(&result, 255, 0, 0, 0);
    }

    #[test]
    fn wipe_right_at_one() {
        let from = make_solid(255, 0, 0, 255, W, H);
        let to = make_solid(0, 255, 0, 255, W, H);
        let result = render_transition(TransitionKind::WipeRight, &from, &to, 1.0, W, H);
        assert_dimensions(&result, W, H);
        assert_approx_rgb(&result, 0, 255, 0, 0);
    }

    #[test]
    fn wipe_right_midpoint() {
        let from = make_solid(255, 0, 0, 255, 10, 1);
        let to = make_solid(0, 255, 0, 255, 10, 1);
        let result = render_transition(TransitionKind::WipeRight, &from, &to, 0.5, 10, 1);
        assert_dimensions(&result, 10, 1);
        // Right half should be 'to', left half 'from' (opposite of WipeLeft)
        assert_eq!(pixel_at(&result, 0, 0, 10), (255, 0, 0, 255));
        assert_eq!(pixel_at(&result, 4, 0, 10), (255, 0, 0, 255));
        assert_eq!(pixel_at(&result, 6, 0, 10), (0, 255, 0, 255));
        assert_eq!(pixel_at(&result, 9, 0, 10), (0, 255, 0, 255));
    }

    // ========================================================================
    // WipeUp
    // ========================================================================

    #[test]
    fn wipe_up_at_zero() {
        let from = make_solid(255, 0, 0, 255, W, H);
        let to = make_solid(0, 255, 0, 255, W, H);
        let result = render_transition(TransitionKind::WipeUp, &from, &to, 0.0, W, H);
        assert_dimensions(&result, W, H);
        assert_approx_rgb(&result, 255, 0, 0, 0);
    }

    #[test]
    fn wipe_up_at_one() {
        let from = make_solid(255, 0, 0, 255, W, H);
        let to = make_solid(0, 255, 0, 255, W, H);
        let result = render_transition(TransitionKind::WipeUp, &from, &to, 1.0, W, H);
        assert_dimensions(&result, W, H);
        assert_approx_rgb(&result, 0, 255, 0, 0);
    }

    #[test]
    fn wipe_up_midpoint() {
        let from = make_solid(255, 0, 0, 255, 1, 10);
        let to = make_solid(0, 255, 0, 255, 1, 10);
        let result = render_transition(TransitionKind::WipeUp, &from, &to, 0.5, 1, 10);
        assert_dimensions(&result, 1, 10);
        // Top half should be 'to', bottom half 'from'
        assert_eq!(pixel_at(&result, 0, 0, 1), (0, 255, 0, 255));
        assert_eq!(pixel_at(&result, 0, 4, 1), (0, 255, 0, 255));
        assert_eq!(pixel_at(&result, 0, 6, 1), (255, 0, 0, 255));
        assert_eq!(pixel_at(&result, 0, 9, 1), (255, 0, 0, 255));
    }

    // ========================================================================
    // WipeDown
    // ========================================================================

    #[test]
    fn wipe_down_at_zero() {
        let from = make_solid(255, 0, 0, 255, W, H);
        let to = make_solid(0, 255, 0, 255, W, H);
        let result = render_transition(TransitionKind::WipeDown, &from, &to, 0.0, W, H);
        assert_dimensions(&result, W, H);
        assert_approx_rgb(&result, 255, 0, 0, 0);
    }

    #[test]
    fn wipe_down_at_one() {
        let from = make_solid(255, 0, 0, 255, W, H);
        let to = make_solid(0, 255, 0, 255, W, H);
        let result = render_transition(TransitionKind::WipeDown, &from, &to, 1.0, W, H);
        assert_dimensions(&result, W, H);
        assert_approx_rgb(&result, 0, 255, 0, 0);
    }

    #[test]
    fn wipe_down_midpoint() {
        let from = make_solid(255, 0, 0, 255, 1, 10);
        let to = make_solid(0, 255, 0, 255, 1, 10);
        let result = render_transition(TransitionKind::WipeDown, &from, &to, 0.5, 1, 10);
        assert_dimensions(&result, 1, 10);
        // Bottom half should be 'to', top half 'from' (opposite of WipeUp)
        assert_eq!(pixel_at(&result, 0, 0, 1), (255, 0, 0, 255));
        assert_eq!(pixel_at(&result, 0, 4, 1), (255, 0, 0, 255));
        assert_eq!(pixel_at(&result, 0, 6, 1), (0, 255, 0, 255));
        assert_eq!(pixel_at(&result, 0, 9, 1), (0, 255, 0, 255));
    }

    // ========================================================================
    // DipToBlack
    // ========================================================================

    #[test]
    fn dip_to_black_at_zero() {
        let from = make_solid(200, 100, 50, 255, W, H);
        let to = make_solid(50, 100, 200, 255, W, H);
        let result = render_transition(TransitionKind::DipToBlack, &from, &to, 0.0, W, H);
        assert_dimensions(&result, W, H);
        // At p=0, t=0*2=0, inv=1.0 so RGB = from * 1.0
        assert_approx_rgb(&result, 200, 100, 50, TOLERANCE);
    }

    #[test]
    fn dip_to_black_at_one() {
        let from = make_solid(200, 100, 50, 255, W, H);
        let to = make_solid(50, 100, 200, 255, W, H);
        let result = render_transition(TransitionKind::DipToBlack, &from, &to, 1.0, W, H);
        assert_dimensions(&result, W, H);
        // At p=1.0, second branch: t=(1.0-0.5)*2=1.0, RGB = to * 1.0
        assert_approx_rgb(&result, 50, 100, 200, TOLERANCE);
    }

    #[test]
    fn dip_to_black_midpoint_is_black() {
        let from = make_solid(200, 100, 50, 255, W, H);
        let to = make_solid(50, 100, 200, 255, W, H);
        let result = render_transition(TransitionKind::DipToBlack, &from, &to, 0.5, W, H);
        assert_dimensions(&result, W, H);
        // At p=0.5, first branch: t=0.5*2=1.0, inv=0.0, so RGB = from * 0 = 0 (black)
        assert_approx_rgb(&result, 0, 0, 0, TOLERANCE);
    }

    // ========================================================================
    // DipToWhite
    // ========================================================================

    #[test]
    fn dip_to_white_at_zero() {
        let from = make_solid(200, 100, 50, 255, W, H);
        let to = make_solid(50, 100, 200, 255, W, H);
        let result = render_transition(TransitionKind::DipToWhite, &from, &to, 0.0, W, H);
        assert_dimensions(&result, W, H);
        // At p=0, t=0, inv=1.0, RGB = from * 1.0 + 255 * 0
        assert_approx_rgb(&result, 200, 100, 50, TOLERANCE);
    }

    #[test]
    fn dip_to_white_at_one() {
        let from = make_solid(200, 100, 50, 255, W, H);
        let to = make_solid(50, 100, 200, 255, W, H);
        let result = render_transition(TransitionKind::DipToWhite, &from, &to, 1.0, W, H);
        assert_dimensions(&result, W, H);
        // At p=1.0, second branch: t=1.0, inv=0, RGB = 255*0 + to*1.0
        assert_approx_rgb(&result, 50, 100, 200, TOLERANCE);
    }

    #[test]
    fn dip_to_white_midpoint_is_white() {
        let from = make_solid(200, 100, 50, 255, W, H);
        let to = make_solid(50, 100, 200, 255, W, H);
        let result = render_transition(TransitionKind::DipToWhite, &from, &to, 0.5, W, H);
        assert_dimensions(&result, W, H);
        // At p=0.5 (first branch edge): t=1.0, RGB = from*0 + 255*1 = 255
        assert_approx_rgb(&result, 255, 255, 255, TOLERANCE);
    }

    // ========================================================================
    // PushLeft
    // ========================================================================

    #[test]
    fn push_left_at_zero() {
        let from = make_solid(255, 0, 0, 255, W, H);
        let to = make_solid(0, 255, 0, 255, W, H);
        let result = render_transition(TransitionKind::PushLeft, &from, &to, 0.0, W, H);
        assert_dimensions(&result, W, H);
        // offset=0, all pixels src_x = x + 0 < width, so all from
        assert_approx_rgb(&result, 255, 0, 0, 0);
    }

    #[test]
    fn push_left_at_one() {
        let from = make_solid(255, 0, 0, 255, W, H);
        let to = make_solid(0, 255, 0, 255, W, H);
        let result = render_transition(TransitionKind::PushLeft, &from, &to, 1.0, W, H);
        assert_dimensions(&result, W, H);
        // offset=W, all pixels src_x = x + W >= W, so all to
        assert_approx_rgb(&result, 0, 255, 0, 0);
    }

    #[test]
    fn push_left_midpoint() {
        let from = make_solid(255, 0, 0, 255, 10, 1);
        let to = make_solid(0, 255, 0, 255, 10, 1);
        let result = render_transition(TransitionKind::PushLeft, &from, &to, 0.5, 10, 1);
        assert_dimensions(&result, 10, 1);
        // offset=5: pixels 0-4 show from[5..9], pixels 5-9 show to[0..4]
        assert_eq!(pixel_at(&result, 0, 0, 10), (255, 0, 0, 255)); // from shifted
        assert_eq!(pixel_at(&result, 4, 0, 10), (255, 0, 0, 255));
        assert_eq!(pixel_at(&result, 5, 0, 10), (0, 255, 0, 255)); // to entering
        assert_eq!(pixel_at(&result, 9, 0, 10), (0, 255, 0, 255));
    }

    // ========================================================================
    // FilmDissolve
    // ========================================================================

    #[test]
    fn film_dissolve_at_zero() {
        let from = make_solid(200, 100, 50, 255, W, H);
        let to = make_solid(50, 100, 200, 255, W, H);
        let result = render_transition(TransitionKind::FilmDissolve, &from, &to, 0.0, W, H);
        assert_dimensions(&result, W, H);
        // At p=0: blended = sqrt(f^2 * 1 + t^2 * 0) = sqrt(f^2) = f
        assert_approx_rgb(&result, 200, 100, 50, TOLERANCE);
    }

    #[test]
    fn film_dissolve_at_one() {
        let from = make_solid(200, 100, 50, 255, W, H);
        let to = make_solid(50, 100, 200, 255, W, H);
        let result = render_transition(TransitionKind::FilmDissolve, &from, &to, 1.0, W, H);
        assert_dimensions(&result, W, H);
        // At p=1: blended = sqrt(f^2 * 0 + t^2 * 1) = t
        assert_approx_rgb(&result, 50, 100, 200, TOLERANCE);
    }

    #[test]
    fn film_dissolve_midpoint_is_blend() {
        let from = make_solid(200, 0, 0, 255, W, H);
        let to = make_solid(0, 0, 200, 255, W, H);
        let result = render_transition(TransitionKind::FilmDissolve, &from, &to, 0.5, W, H);
        assert_dimensions(&result, W, H);
        assert_not_all_black(&result);
        assert_not_all_white(&result);
        // Should be a mix: R > 0 (from contributes) and B > 0 (to contributes)
        let (r, _, b, _) = pixel_at(&result, 0, 0, W);
        assert!(r > 0 && r < 200, "R should be partial, got {}", r);
        assert!(b > 0 && b < 200, "B should be partial, got {}", b);
    }

    // ========================================================================
    // BlurDissolve
    // ========================================================================

    #[test]
    fn blur_dissolve_at_zero() {
        let from = make_solid(200, 100, 50, 255, W, H);
        let to = make_solid(50, 100, 200, 255, W, H);
        let result = render_transition(TransitionKind::BlurDissolve, &from, &to, 0.0, W, H);
        assert_dimensions(&result, W, H);
        // At p=0, standard dissolve = from, blur radius = 0 (no blur)
        assert_approx_rgb(&result, 200, 100, 50, TOLERANCE);
    }

    #[test]
    fn blur_dissolve_at_one() {
        let from = make_solid(200, 100, 50, 255, W, H);
        let to = make_solid(50, 100, 200, 255, W, H);
        let result = render_transition(TransitionKind::BlurDissolve, &from, &to, 1.0, W, H);
        assert_dimensions(&result, W, H);
        // At p=1, dissolve = to, blur radius = 0 (no blur)
        assert_approx_rgb(&result, 50, 100, 200, TOLERANCE);
    }

    #[test]
    fn blur_dissolve_midpoint_is_blend() {
        // With solid colors, blur won't change values (all neighbors same)
        let from = make_solid(200, 0, 0, 255, W, H);
        let to = make_solid(0, 0, 200, 255, W, H);
        let result = render_transition(TransitionKind::BlurDissolve, &from, &to, 0.5, W, H);
        assert_dimensions(&result, W, H);
        assert_not_all_black(&result);
        assert_not_all_white(&result);
        // Solid input means blur doesn't change anything: just dissolve at 0.5
        assert_approx_rgb(&result, 100, 0, 100, TOLERANCE);
    }

    // ========================================================================
    // Flash
    // ========================================================================

    #[test]
    fn flash_at_zero() {
        let from = make_solid(200, 100, 50, 255, W, H);
        let to = make_solid(50, 100, 200, 255, W, H);
        let result = render_transition(TransitionKind::Flash, &from, &to, 0.0, W, H);
        assert_dimensions(&result, W, H);
        // At p=0: white_intensity=0, src=from, base=from
        assert_approx_rgb(&result, 200, 100, 50, TOLERANCE);
    }

    #[test]
    fn flash_at_one() {
        let from = make_solid(200, 100, 50, 255, W, H);
        let to = make_solid(50, 100, 200, 255, W, H);
        let result = render_transition(TransitionKind::Flash, &from, &to, 1.0, W, H);
        assert_dimensions(&result, W, H);
        // At p=1.0: white_intensity=0, blend_p=1.0, base = to
        assert_approx_rgb(&result, 50, 100, 200, TOLERANCE);
    }

    #[test]
    fn flash_midpoint_is_white() {
        let from = make_solid(200, 100, 50, 255, W, H);
        let to = make_solid(50, 100, 200, 255, W, H);
        let result = render_transition(TransitionKind::Flash, &from, &to, 0.4, W, H);
        assert_dimensions(&result, W, H);
        // At p=0.4: white_intensity = 1.0 (p>0.3 and p<0.5 => hold white)
        // base = from (p<0.4 is false at 0.4 exactly, so blend_p=(0.4-0.4)/0.6=0)
        // output = base * 0 + 255 * 1.0 = 255
        assert_approx_rgb(&result, 255, 255, 255, TOLERANCE);
    }

    #[test]
    fn flash_midpoint_not_black() {
        let from = make_solid(200, 100, 50, 255, W, H);
        let to = make_solid(50, 100, 200, 255, W, H);
        let result = render_transition(TransitionKind::Flash, &from, &to, 0.5, W, H);
        assert_dimensions(&result, W, H);
        assert_not_all_black(&result);
    }

    // ========================================================================
    // Spin
    // ========================================================================

    #[test]
    fn spin_at_zero() {
        let from = make_solid(200, 100, 50, 255, W, H);
        let to = make_solid(50, 100, 200, 255, W, H);
        let result = render_transition(TransitionKind::Spin, &from, &to, 0.0, W, H);
        assert_dimensions(&result, W, H);
        // At p=0: angle=0, scale=1, no rotation. inv=1.0 so output = from*1 + to*0
        assert_approx_rgb(&result, 200, 100, 50, TOLERANCE);
    }

    #[test]
    fn spin_at_one() {
        let from = make_solid(200, 100, 50, 255, W, H);
        let to = make_solid(50, 100, 200, 255, W, H);
        let result = render_transition(TransitionKind::Spin, &from, &to, 1.0, W, H);
        assert_dimensions(&result, W, H);
        // At p=1: angle=2*PI (full rotation back to identity), scale=1, inv=0
        // So output = from*0 + to*1 = to
        assert_approx_rgb(&result, 50, 100, 200, TOLERANCE);
    }

    #[test]
    fn spin_midpoint_is_blend() {
        let from = make_solid(200, 0, 0, 255, W, H);
        let to = make_solid(0, 0, 200, 255, W, H);
        let result = render_transition(TransitionKind::Spin, &from, &to, 0.5, W, H);
        assert_dimensions(&result, W, H);
        assert_not_all_black(&result);
        assert_not_all_white(&result);
        // With solid colors, rotation doesn't matter; it's just 50/50 blend
        assert_approx_rgb(&result, 100, 0, 100, TOLERANCE);
    }

    // ========================================================================
    // LightLeak
    // ========================================================================

    #[test]
    fn light_leak_at_zero() {
        let from = make_solid(200, 100, 50, 255, W, H);
        let to = make_solid(50, 100, 200, 255, W, H);
        let result = render_transition(TransitionKind::LightLeak, &from, &to, 0.0, W, H);
        assert_dimensions(&result, W, H);
        // At p=0: leak_intensity = (0*1*4)^0.7 = 0, inv=1, so output = from
        assert_approx_rgb(&result, 200, 100, 50, TOLERANCE);
    }

    #[test]
    fn light_leak_at_one() {
        let from = make_solid(200, 100, 50, 255, W, H);
        let to = make_solid(50, 100, 200, 255, W, H);
        let result = render_transition(TransitionKind::LightLeak, &from, &to, 1.0, W, H);
        assert_dimensions(&result, W, H);
        // At p=1: leak_intensity = 0, inv=0, so output = to
        assert_approx_rgb(&result, 50, 100, 200, TOLERANCE);
    }

    #[test]
    fn light_leak_midpoint_has_warm_tint() {
        let from = make_solid(0, 0, 0, 255, W, H);
        let to = make_solid(0, 0, 0, 255, W, H);
        let result = render_transition(TransitionKind::LightLeak, &from, &to, 0.5, W, H);
        assert_dimensions(&result, W, H);
        // With black input, the warm tint should add color: R > G > B (amber)
        let (r, g, b, _) = pixel_at(&result, 0, 0, W);
        assert!(r > 0, "light leak should add red, got R={}", r);
        assert!(g > 0, "light leak should add green, got G={}", g);
        assert!(r > g, "R should be > G for warm tint, R={} G={}", r, g);
        assert!(g > b, "G should be > B for warm tint, G={} B={}", g, b);
    }

    #[test]
    fn light_leak_midpoint_not_black() {
        let from = make_solid(100, 80, 60, 255, W, H);
        let to = make_solid(60, 80, 100, 255, W, H);
        let result = render_transition(TransitionKind::LightLeak, &from, &to, 0.5, W, H);
        assert_dimensions(&result, W, H);
        assert_not_all_black(&result);
        assert_not_all_white(&result);
    }

    // ========================================================================
    // Wipe symmetry: WipeLeft/WipeRight and WipeUp/WipeDown are inverses
    // ========================================================================

    #[test]
    fn wipe_left_right_symmetry() {
        let from = make_solid(255, 0, 0, 255, 20, 1);
        let to = make_solid(0, 255, 0, 255, 20, 1);
        let left = render_transition(TransitionKind::WipeLeft, &from, &to, 0.5, 20, 1);
        let right = render_transition(TransitionKind::WipeRight, &from, &to, 0.5, 20, 1);
        // WipeLeft reveals to from left; WipeRight reveals to from right
        // They should cover complementary regions
        // Pixel 0: WipeLeft=to, WipeRight=from
        assert_eq!(pixel_at(&left, 0, 0, 20), (0, 255, 0, 255));
        assert_eq!(pixel_at(&right, 0, 0, 20), (255, 0, 0, 255));
        // Pixel 19: WipeLeft=from, WipeRight=to
        assert_eq!(pixel_at(&left, 19, 0, 20), (255, 0, 0, 255));
        assert_eq!(pixel_at(&right, 19, 0, 20), (0, 255, 0, 255));
    }

    #[test]
    fn wipe_up_down_symmetry() {
        let from = make_solid(255, 0, 0, 255, 1, 20);
        let to = make_solid(0, 255, 0, 255, 1, 20);
        let up = render_transition(TransitionKind::WipeUp, &from, &to, 0.5, 1, 20);
        let down = render_transition(TransitionKind::WipeDown, &from, &to, 0.5, 1, 20);
        // WipeUp reveals to from top; WipeDown reveals to from bottom
        assert_eq!(pixel_at(&up, 0, 0, 1), (0, 255, 0, 255));
        assert_eq!(pixel_at(&down, 0, 0, 1), (255, 0, 0, 255));
        assert_eq!(pixel_at(&up, 0, 19, 1), (255, 0, 0, 255));
        assert_eq!(pixel_at(&down, 0, 19, 1), (0, 255, 0, 255));
    }

    // ========================================================================
    // Kind ID round-trip
    // ========================================================================

    #[test]
    fn kind_to_id_mapping() {
        assert_eq!(transition_kind_to_id(TransitionKind::CrossDissolve), 0);
        assert_eq!(transition_kind_to_id(TransitionKind::WipeLeft), 1);
        assert_eq!(transition_kind_to_id(TransitionKind::WipeRight), 2);
        assert_eq!(transition_kind_to_id(TransitionKind::WipeUp), 3);
        assert_eq!(transition_kind_to_id(TransitionKind::WipeDown), 4);
        assert_eq!(transition_kind_to_id(TransitionKind::DipToBlack), 5);
        assert_eq!(transition_kind_to_id(TransitionKind::DipToWhite), 6);
        assert_eq!(transition_kind_to_id(TransitionKind::PushLeft), 7);
        assert_eq!(transition_kind_to_id(TransitionKind::FilmDissolve), 8);
        assert_eq!(transition_kind_to_id(TransitionKind::BlurDissolve), 9);
        assert_eq!(transition_kind_to_id(TransitionKind::Flash), 10);
        assert_eq!(transition_kind_to_id(TransitionKind::Spin), 11);
        assert_eq!(transition_kind_to_id(TransitionKind::LightLeak), 12);
        assert_eq!(transition_kind_to_id(TransitionKind::LuminanceDissolve), 13);
        assert_eq!(transition_kind_to_id(TransitionKind::CircleWipeIn), 14);
        assert_eq!(transition_kind_to_id(TransitionKind::CircleWipeOut), 15);
        assert_eq!(transition_kind_to_id(TransitionKind::ClockWipe), 16);
        assert_eq!(transition_kind_to_id(TransitionKind::BarnDoorOpen), 17);
        assert_eq!(transition_kind_to_id(TransitionKind::BarnDoorClose), 18);
        assert_eq!(transition_kind_to_id(TransitionKind::Pixelate), 19);
        assert_eq!(transition_kind_to_id(TransitionKind::Ripple), 20);
        assert_eq!(transition_kind_to_id(TransitionKind::PageTurn), 21);
        assert_eq!(transition_kind_to_id(TransitionKind::PageTurnReverse), 22);
        assert_eq!(transition_kind_to_id(TransitionKind::ChromaticAberration), 23);
        assert_eq!(transition_kind_to_id(TransitionKind::FilmBurn), 24);
        assert_eq!(transition_kind_to_id(TransitionKind::StarWipe), 25);
        assert_eq!(transition_kind_to_id(TransitionKind::SplitWipeHorizontal), 26);
        assert_eq!(transition_kind_to_id(TransitionKind::SplitWipeVertical), 27);
        assert_eq!(transition_kind_to_id(TransitionKind::VenetianBlindHorizontal), 28);
        assert_eq!(transition_kind_to_id(TransitionKind::VenetianBlindVertical), 29);
        assert_eq!(transition_kind_to_id(TransitionKind::CheckerboardWipe), 30);
        assert_eq!(transition_kind_to_id(TransitionKind::DiagonalWipeTLBR), 31);
        assert_eq!(transition_kind_to_id(TransitionKind::DiagonalWipeTRBL), 32);
        assert_eq!(transition_kind_to_id(TransitionKind::BoxWipeIn), 33);
        assert_eq!(transition_kind_to_id(TransitionKind::BoxWipeOut), 34);
        assert_eq!(transition_kind_to_id(TransitionKind::SlideLeft), 35);
        assert_eq!(transition_kind_to_id(TransitionKind::SlideRight), 36);
        assert_eq!(transition_kind_to_id(TransitionKind::SlideUp), 37);
        assert_eq!(transition_kind_to_id(TransitionKind::SlideDown), 38);
        assert_eq!(transition_kind_to_id(TransitionKind::CoverLeft), 39);
        assert_eq!(transition_kind_to_id(TransitionKind::CoverRight), 40);
        assert_eq!(transition_kind_to_id(TransitionKind::UncoverLeft), 41);
        assert_eq!(transition_kind_to_id(TransitionKind::UncoverRight), 42);
        assert_eq!(transition_kind_to_id(TransitionKind::SqueezeH), 43);
        assert_eq!(transition_kind_to_id(TransitionKind::SqueezeV), 44);
        assert_eq!(transition_kind_to_id(TransitionKind::AdditiveDissolve), 45);
        assert_eq!(transition_kind_to_id(TransitionKind::FadeToColor), 46);
        assert_eq!(transition_kind_to_id(TransitionKind::GlitchDissolve), 47);
        assert_eq!(transition_kind_to_id(TransitionKind::Desaturate), 48);
        assert_eq!(transition_kind_to_id(TransitionKind::NegativeFlash), 49);
        assert_eq!(transition_kind_to_id(TransitionKind::Sepia), 50);
        assert_eq!(transition_kind_to_id(TransitionKind::ColorizeFlash), 51);
        assert_eq!(transition_kind_to_id(TransitionKind::HueRotate), 52);
        assert_eq!(transition_kind_to_id(TransitionKind::Swirl), 53);
        assert_eq!(transition_kind_to_id(TransitionKind::WaveDistortion), 54);
        assert_eq!(transition_kind_to_id(TransitionKind::StaticTV), 55);
        assert_eq!(transition_kind_to_id(TransitionKind::VHSTracking), 56);
        assert_eq!(transition_kind_to_id(TransitionKind::GlitchRGBSplit), 57);
        assert_eq!(transition_kind_to_id(TransitionKind::ProjectorFlicker), 58);
        assert_eq!(transition_kind_to_id(TransitionKind::Halation), 59);
        assert_eq!(transition_kind_to_id(TransitionKind::AnamorphicFlare), 60);
        assert_eq!(transition_kind_to_id(TransitionKind::DreamSequence), 61);
        assert_eq!(transition_kind_to_id(TransitionKind::BokehDissolve), 62);
        assert_eq!(transition_kind_to_id(TransitionKind::ZoomInDissolve), 63);
        assert_eq!(transition_kind_to_id(TransitionKind::ZoomOutDissolve), 64);
        assert_eq!(transition_kind_to_id(TransitionKind::CrossZoom), 65);
        assert_eq!(transition_kind_to_id(TransitionKind::WaterDrop), 66);
    }

    #[test]
    fn kind_id_round_trip() {
        for id in 0..=65u8 {
            let kind = transition_kind_from_id(id);
            assert_eq!(transition_kind_to_id(kind), id as u32);
        }
    }

    #[test]
    fn unknown_kind_id_defaults_to_cross_dissolve() {
        assert_eq!(transition_kind_from_id(255), TransitionKind::CrossDissolve);
        assert_eq!(transition_kind_from_id(73), TransitionKind::CrossDissolve);
    }

    #[test]
    fn all_transition_kinds_returns_every_registered_kind() {
        assert_eq!(all_transition_kinds().len(), klipp_core::TRANSITION_SPECS.len());
        assert!(all_transition_kinds().iter().all(|(_, name)| !name.is_empty()), "every kind is named, the last included");
    }

    // ========================================================================
    // LuminanceDissolve
    // ========================================================================

    #[test]
    fn luminance_dissolve_bright_transitions_first() {
        // White pixel (luma=1.0) at progress=0.9 should be mostly `to` (black).
        let from = make_solid(255, 255, 255, 255, 1, 1);
        let to = make_solid(0, 0, 0, 255, 1, 1);
        let result = render_transition(TransitionKind::LuminanceDissolve, &from, &to, 0.9, 1, 1);
        assert!(
            result[0] < 50,
            "bright pixel should be mostly `to` at p=0.9, got {}",
            result[0]
        );
    }

    #[test]
    fn luminance_dissolve_dark_transitions_last() {
        // Near-black pixel (luma≈0.04) at progress=0.1 should still be mostly `from`.
        let from = make_solid(10, 10, 10, 255, 1, 1);
        let to = make_solid(200, 200, 200, 255, 1, 1);
        let result = render_transition(TransitionKind::LuminanceDissolve, &from, &to, 0.1, 1, 1);
        assert!(
            result[0] < 70,
            "dark pixel should still be mostly `from` at p=0.1, got {}",
            result[0]
        );
    }

    // ========================================================================
    // CircleWipe In/Out
    // ========================================================================

    #[test]
    fn circle_wipe_in_full_progress_is_to() {
        let from = make_solid(255, 0, 0, 255, W, H);
        let to = make_solid(0, 255, 0, 255, W, H);
        let result = render_transition(TransitionKind::CircleWipeIn, &from, &to, 1.0, W, H);
        assert_approx_rgb(&result, 0, 255, 0, TOLERANCE);
    }

    // ========================================================================
    // Zoom In / Zoom Out / Cross Zoom
    // ========================================================================

    #[test]
    fn zoom_in_dissolve_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::ZoomInDissolve, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn zoom_in_dissolve_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::ZoomInDissolve, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    #[test]
    fn zoom_out_dissolve_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::ZoomOutDissolve, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn zoom_out_dissolve_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::ZoomOutDissolve, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    #[test]
    fn cross_zoom_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::CrossZoom, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn cross_zoom_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::CrossZoom, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    // ========================================================================
    // Bokeh Dissolve
    // ========================================================================

    #[test]
    fn bokeh_dissolve_at_zero_is_from() {
        let from = make_solid(100, 100, 100, 255, 8, 8);
        let to = make_solid(150, 150, 150, 255, 8, 8);
        let result =
            render_transition(TransitionKind::BokehDissolve, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 100, 100, 100, 5);
    }

    #[test]
    fn bokeh_dissolve_at_one_is_to() {
        let from = make_solid(100, 100, 100, 255, 8, 8);
        let to = make_solid(150, 150, 150, 255, 8, 8);
        let result =
            render_transition(TransitionKind::BokehDissolve, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 150, 150, 150, 5);
    }

    #[test]
    fn bokeh_dissolve_midpoint_brightens() {
        let from = make_solid(120, 120, 120, 255, 8, 8);
        let to = make_solid(120, 120, 120, 255, 8, 8);
        let result =
            render_transition(TransitionKind::BokehDissolve, &from, &to, 0.5, 8, 8);
        let avg: f32 = result.chunks(4).map(|px| px[0] as f32).sum::<f32>() / 64.0;
        assert!(
            avg > 120.0,
            "bokeh should brighten at midpoint, got {}",
            avg
        );
    }

    // ========================================================================
    // Anamorphic Flare / Dream Sequence
    // ========================================================================

    #[test]
    fn anamorphic_flare_at_zero_is_from() {
        let from = make_solid(100, 100, 100, 255, 8, 8);
        let to = make_solid(150, 150, 150, 255, 8, 8);
        let result =
            render_transition(TransitionKind::AnamorphicFlare, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 100, 100, 100, 5);
    }

    #[test]
    fn anamorphic_flare_at_one_is_to() {
        let from = make_solid(100, 100, 100, 255, 8, 8);
        let to = make_solid(150, 150, 150, 255, 8, 8);
        let result =
            render_transition(TransitionKind::AnamorphicFlare, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 150, 150, 150, 5);
    }

    #[test]
    fn dream_sequence_at_zero_is_from() {
        let from = make_solid(100, 80, 60, 255, 8, 8);
        let to = make_solid(60, 80, 100, 255, 8, 8);
        let result =
            render_transition(TransitionKind::DreamSequence, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 100, 80, 60, 5);
    }

    #[test]
    fn dream_sequence_at_one_is_to() {
        let from = make_solid(100, 80, 60, 255, 8, 8);
        let to = make_solid(60, 80, 100, 255, 8, 8);
        let result =
            render_transition(TransitionKind::DreamSequence, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 60, 80, 100, 5);
    }

    #[test]
    fn dream_sequence_midpoint_is_brighter() {
        let from = make_solid(100, 100, 100, 255, 8, 8);
        let to = make_solid(100, 100, 100, 255, 8, 8);
        let result =
            render_transition(TransitionKind::DreamSequence, &from, &to, 0.5, 8, 8);
        let avg: f32 = result.chunks(4).map(|px| px[0] as f32).sum::<f32>() / 64.0;
        assert!(
            avg > 100.0,
            "dream should brighten at midpoint, got {}",
            avg
        );
    }

    // ========================================================================
    // Projector Flicker / Halation
    // ========================================================================

    #[test]
    fn projector_flicker_at_zero_is_from() {
        let from = make_solid(200, 150, 100, 255, 8, 8);
        let to = make_solid(100, 150, 200, 255, 8, 8);
        let result =
            render_transition(TransitionKind::ProjectorFlicker, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 200, 150, 100, 10);
    }

    #[test]
    fn projector_flicker_at_one_is_to() {
        let from = make_solid(200, 150, 100, 255, 8, 8);
        let to = make_solid(100, 150, 200, 255, 8, 8);
        let result =
            render_transition(TransitionKind::ProjectorFlicker, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 100, 150, 200, 10);
    }

    #[test]
    fn halation_at_zero_is_from() {
        let from = make_solid(100, 80, 60, 255, 8, 8);
        let to = make_solid(60, 80, 100, 255, 8, 8);
        let result = render_transition(TransitionKind::Halation, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 100, 80, 60, 5);
    }

    #[test]
    fn halation_at_one_is_to() {
        let from = make_solid(100, 80, 60, 255, 8, 8);
        let to = make_solid(60, 80, 100, 255, 8, 8);
        let result = render_transition(TransitionKind::Halation, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 60, 80, 100, 5);
    }

    // ========================================================================
    // Glitch RGB Split
    // ========================================================================

    #[test]
    fn glitch_rgb_split_at_zero_is_from() {
        let from = make_solid(200, 100, 50, 255, 8, 8);
        let to = make_solid(50, 100, 200, 255, 8, 8);
        let result =
            render_transition(TransitionKind::GlitchRGBSplit, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 200, 100, 50, 10);
    }

    #[test]
    fn glitch_rgb_split_at_one_is_to() {
        let from = make_solid(200, 100, 50, 255, 8, 8);
        let to = make_solid(50, 100, 200, 255, 8, 8);
        let result =
            render_transition(TransitionKind::GlitchRGBSplit, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 50, 100, 200, 10);
    }

    // ========================================================================
    // Static TV / VHS Tracking
    // ========================================================================

    #[test]
    fn static_tv_at_zero_is_from() {
        let from = make_solid(200, 100, 50, 255, 8, 8);
        let to = make_solid(50, 100, 200, 255, 8, 8);
        let result = render_transition(TransitionKind::StaticTV, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 200, 100, 50, 10);
    }

    #[test]
    fn static_tv_at_one_is_to() {
        let from = make_solid(200, 100, 50, 255, 8, 8);
        let to = make_solid(50, 100, 200, 255, 8, 8);
        let result = render_transition(TransitionKind::StaticTV, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 50, 100, 200, 10);
    }

    #[test]
    fn vhs_tracking_at_zero_is_from() {
        let from = make_solid(200, 100, 50, 255, 8, 8);
        let to = make_solid(50, 100, 200, 255, 8, 8);
        let result = render_transition(TransitionKind::VHSTracking, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 200, 100, 50, 10);
    }

    #[test]
    fn vhs_tracking_at_one_is_to() {
        let from = make_solid(200, 100, 50, 255, 8, 8);
        let to = make_solid(50, 100, 200, 255, 8, 8);
        let result = render_transition(TransitionKind::VHSTracking, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 50, 100, 200, 10);
    }

    // ========================================================================
    // Swirl / Wave Distortion
    // ========================================================================

    #[test]
    fn swirl_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::Swirl, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn swirl_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::Swirl, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    #[test]
    fn wave_distortion_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::WaveDistortion, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn wave_distortion_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::WaveDistortion, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    // ========================================================================
    // Colorize Flash / Hue Rotate
    // ========================================================================

    #[test]
    fn colorize_flash_at_zero_is_from() {
        let from = make_solid(200, 100, 50, 255, 8, 8);
        let to = make_solid(50, 100, 200, 255, 8, 8);
        let result =
            render_transition(TransitionKind::ColorizeFlash, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 200, 100, 50, 10);
    }

    #[test]
    fn colorize_flash_at_one_is_to() {
        let from = make_solid(200, 100, 50, 255, 8, 8);
        let to = make_solid(50, 100, 200, 255, 8, 8);
        let result =
            render_transition(TransitionKind::ColorizeFlash, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 50, 100, 200, 10);
    }

    #[test]
    fn hue_rotate_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(255, 0, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::HueRotate, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn hue_rotate_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(255, 0, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::HueRotate, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn hue_rotate_midpoint_produces_valid_output() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(255, 0, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::HueRotate, &from, &to, 0.5, 8, 8);
        assert_eq!(result.len(), 8 * 8 * 4);
    }

    // ========================================================================
    // Negative Flash / Sepia
    // ========================================================================

    #[test]
    fn negative_flash_at_zero_is_from() {
        let from = make_solid(200, 100, 50, 255, 8, 8);
        let to = make_solid(50, 100, 200, 255, 8, 8);
        let result =
            render_transition(TransitionKind::NegativeFlash, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 200, 100, 50, 10);
    }

    #[test]
    fn negative_flash_at_one_is_to() {
        let from = make_solid(200, 100, 50, 255, 8, 8);
        let to = make_solid(50, 100, 200, 255, 8, 8);
        let result =
            render_transition(TransitionKind::NegativeFlash, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 50, 100, 200, 10);
    }

    #[test]
    fn negative_flash_peak_inverts() {
        let from = make_solid(200, 200, 200, 255, 8, 8);
        let to = make_solid(200, 200, 200, 255, 8, 8);
        let result =
            render_transition(TransitionKind::NegativeFlash, &from, &to, 0.3, 8, 8);
        assert!(
            result[0] < 150,
            "bright pixel should invert at peak, got {}",
            result[0]
        );
    }

    #[test]
    fn sepia_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::Sepia, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn sepia_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::Sepia, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    #[test]
    fn sepia_midpoint_is_warm() {
        let from = make_solid(128, 128, 128, 255, 8, 8);
        let to = make_solid(128, 128, 128, 255, 8, 8);
        let result = render_transition(TransitionKind::Sepia, &from, &to, 0.5, 8, 8);
        assert!(
            result[0] > result[2],
            "sepia should be warmer (R > B) at midpoint, R={} B={}",
            result[0],
            result[2]
        );
    }

    // ========================================================================
    // Glitch Dissolve / Desaturate
    // ========================================================================

    #[test]
    fn glitch_dissolve_at_zero_is_from() {
        let from = make_solid(200, 100, 50, 255, 8, 8);
        let to = make_solid(50, 100, 200, 255, 8, 8);
        let result =
            render_transition(TransitionKind::GlitchDissolve, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 200, 100, 50, 10);
    }

    #[test]
    fn glitch_dissolve_at_one_is_to() {
        let from = make_solid(200, 100, 50, 255, 8, 8);
        let to = make_solid(50, 100, 200, 255, 8, 8);
        let result =
            render_transition(TransitionKind::GlitchDissolve, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 50, 100, 200, 10);
    }

    #[test]
    fn desaturate_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::Desaturate, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn desaturate_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::Desaturate, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    #[test]
    fn desaturate_midpoint_is_grey() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(255, 0, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::Desaturate, &from, &to, 0.5, 8, 8);
        let r = result[0] as i16;
        let g = result[1] as i16;
        let b = result[2] as i16;
        assert!((r - g).abs() < 20, "should be near grey at midpoint, R={r} G={g}");
        assert!((r - b).abs() < 20, "should be near grey at midpoint, R={r} B={b}");
    }

    // ========================================================================
    // Additive Dissolve / Fade to Color
    // ========================================================================

    #[test]
    fn additive_dissolve_at_zero_is_from() {
        let from = make_solid(100, 100, 100, 255, 8, 8);
        let to = make_solid(100, 100, 100, 255, 8, 8);
        let result =
            render_transition(TransitionKind::AdditiveDissolve, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 100, 100, 100, 5);
    }

    #[test]
    fn additive_dissolve_midpoint_is_brighter() {
        let from = make_solid(100, 100, 100, 255, 8, 8);
        let to = make_solid(100, 100, 100, 255, 8, 8);
        let result =
            render_transition(TransitionKind::AdditiveDissolve, &from, &to, 0.5, 8, 8);
        let avg: f32 = result.chunks(4).map(|px| px[0] as f32).sum::<f32>() / 64.0;
        assert!(
            avg > 100.0,
            "additive should brighten at midpoint, got {}",
            avg
        );
    }

    #[test]
    fn fade_to_color_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::FadeToColor, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn fade_to_color_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::FadeToColor, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    // ========================================================================
    // Slide / Cover / Uncover / Squeeze family endpoint tests
    // ========================================================================
    macro_rules! slide_family_endpoint_test {
        ($test_name:ident, $kind:ident) => {
            #[test]
            fn $test_name() {
                let from = make_solid(255, 0, 0, 255, 8, 8);
                let to = make_solid(0, 255, 0, 255, 8, 8);
                assert_approx_rgb(
                    &render_transition(TransitionKind::$kind, &from, &to, 0.0, 8, 8),
                    255, 0, 0, 5,
                );
                assert_approx_rgb(
                    &render_transition(TransitionKind::$kind, &from, &to, 1.0, 8, 8),
                    0, 255, 0, 5,
                );
            }
        };
    }
    slide_family_endpoint_test!(slide_left_endpoints, SlideLeft);
    slide_family_endpoint_test!(slide_right_endpoints, SlideRight);
    slide_family_endpoint_test!(slide_up_endpoints, SlideUp);
    slide_family_endpoint_test!(slide_down_endpoints, SlideDown);
    slide_family_endpoint_test!(cover_left_endpoints, CoverLeft);
    slide_family_endpoint_test!(cover_right_endpoints, CoverRight);
    slide_family_endpoint_test!(uncover_left_endpoints, UncoverLeft);
    slide_family_endpoint_test!(uncover_right_endpoints, UncoverRight);
    slide_family_endpoint_test!(squeeze_h_endpoints, SqueezeH);
    slide_family_endpoint_test!(squeeze_v_endpoints, SqueezeV);

    #[test]
    fn squeeze_h_midpoint_is_black_at_edges() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::SqueezeH, &from, &to, 0.5, 8, 8);
        // At p=0.5, visible region collapses to ~0 width — all pixels should be black.
        assert_approx_rgb(&result, 0, 0, 0, 10);
    }

    #[test]
    fn box_wipe_in_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::BoxWipeIn, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn box_wipe_in_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::BoxWipeIn, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    #[test]
    fn box_wipe_out_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::BoxWipeOut, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn box_wipe_out_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::BoxWipeOut, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    #[test]
    fn diagonal_tlbr_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::DiagonalWipeTLBR, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn diagonal_tlbr_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::DiagonalWipeTLBR, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    #[test]
    fn diagonal_trbl_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::DiagonalWipeTRBL, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn diagonal_trbl_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::DiagonalWipeTRBL, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    #[test]
    fn checkerboard_wipe_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::CheckerboardWipe, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn checkerboard_wipe_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::CheckerboardWipe, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    #[test]
    fn venetian_blind_h_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(
            TransitionKind::VenetianBlindHorizontal,
            &from,
            &to,
            0.0,
            8,
            8,
        );
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn venetian_blind_h_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(
            TransitionKind::VenetianBlindHorizontal,
            &from,
            &to,
            1.0,
            8,
            8,
        );
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    #[test]
    fn venetian_blind_v_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::VenetianBlindVertical, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn venetian_blind_v_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::VenetianBlindVertical, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    #[test]
    fn split_wipe_h_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::SplitWipeHorizontal, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn split_wipe_h_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::SplitWipeHorizontal, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    #[test]
    fn split_wipe_v_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::SplitWipeVertical, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn split_wipe_v_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::SplitWipeVertical, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    #[test]
    fn star_wipe_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::StarWipe, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn star_wipe_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::StarWipe, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    #[test]
    fn film_burn_at_zero_is_from() {
        let from = make_solid(100, 100, 100, 255, 8, 8);
        let to = make_solid(200, 200, 200, 255, 8, 8);
        let result = render_transition(TransitionKind::FilmBurn, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 100, 100, 100, 10);
    }

    #[test]
    fn film_burn_at_one_is_to() {
        let from = make_solid(100, 100, 100, 255, 8, 8);
        let to = make_solid(200, 200, 200, 255, 8, 8);
        let result = render_transition(TransitionKind::FilmBurn, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 200, 200, 200, 10);
    }

    #[test]
    fn film_burn_midpoint_is_brighter() {
        let from = make_solid(50, 50, 50, 255, 8, 8);
        let to = make_solid(50, 50, 50, 255, 8, 8);
        let result = render_transition(TransitionKind::FilmBurn, &from, &to, 0.5, 8, 8);
        let avg_r: f32 = result.chunks(4).map(|px| px[0] as f32).sum::<f32>() / 64.0;
        assert!(
            avg_r > 70.0,
            "burn should brighten at midpoint, got avg {}",
            avg_r
        );
    }

    #[test]
    fn chromatic_aberration_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::ChromaticAberration, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn chromatic_aberration_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::ChromaticAberration, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    #[test]
    fn chromatic_aberration_midpoint_produces_output() {
        let from = make_solid(200, 100, 50, 255, 16, 8);
        let to = make_solid(50, 100, 200, 255, 16, 8);
        let result =
            render_transition(TransitionKind::ChromaticAberration, &from, &to, 0.5, 16, 8);
        assert_eq!(result.len(), 16 * 8 * 4);
    }

    #[test]
    fn page_turn_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::PageTurn, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn page_turn_at_one_right_side_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::PageTurn, &from, &to, 1.0, 8, 8);
        // Pixel at x=1 should be mostly `to` (some shadow darkening allowed).
        assert!(
            result[1 * 4 + 1] > 150,
            "right-side pixel should show `to` at p=1.0, got G={}",
            result[1 * 4 + 1]
        );
    }

    #[test]
    fn page_turn_reverse_mirrors_direction() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result =
            render_transition(TransitionKind::PageTurnReverse, &from, &to, 0.5, 8, 8);
        // Left pixel (x=0) should be `to` (green), right pixel (x=7) should be `from` (red).
        let left_g = result[0 * 4 + 1];
        let right_g = result[7 * 4 + 1];
        assert!(
            left_g > right_g,
            "left G ({}) should exceed right G ({}) in reverse at p=0.5",
            left_g,
            right_g
        );
    }

    #[test]
    fn ripple_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::Ripple, &from, &to, 0.0, 8, 8);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn ripple_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 8, 8);
        let to = make_solid(0, 255, 0, 255, 8, 8);
        let result = render_transition(TransitionKind::Ripple, &from, &to, 1.0, 8, 8);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    #[test]
    fn pixelate_at_zero_is_from() {
        let from = make_solid(255, 0, 0, 255, 4, 4);
        let to = make_solid(0, 255, 0, 255, 4, 4);
        let result = render_transition(TransitionKind::Pixelate, &from, &to, 0.0, 4, 4);
        assert_approx_rgb(&result, 255, 0, 0, 5);
    }

    #[test]
    fn pixelate_at_one_is_to() {
        let from = make_solid(255, 0, 0, 255, 4, 4);
        let to = make_solid(0, 255, 0, 255, 4, 4);
        let result = render_transition(TransitionKind::Pixelate, &from, &to, 1.0, 4, 4);
        assert_approx_rgb(&result, 0, 255, 0, 5);
    }

    #[test]
    fn pixelate_midpoint_from_only_mode_shows_from() {
        let from = make_solid(200, 100, 50, 255, 8, 8);
        let to = make_solid(50, 100, 200, 255, 8, 8);
        let result = render_transition(TransitionKind::Pixelate, &from, &to, 0.5, 8, 8);
        assert!(
            result[0] > 150,
            "at midpoint in from-only mode R should still be ~200, got {}",
            result[0]
        );
    }

    #[test]
    fn barn_door_open_center_reveals_first() {
        // At p=0.5 with a 5-wide frame, center pixel (x=2) should be more `to`
        // than edge pixel (x=0).
        let from = make_solid(255, 0, 0, 255, 5, 1);
        let to = make_solid(0, 255, 0, 255, 5, 1);
        let result = render_transition(TransitionKind::BarnDoorOpen, &from, &to, 0.5, 5, 1);
        let center_g = result[2 * 4 + 1];
        let edge_g = result[0 * 4 + 1];
        assert!(
            center_g > edge_g,
            "center G ({}) should exceed edge G ({}) at p=0.5",
            center_g,
            edge_g
        );
    }

    #[test]
    fn barn_door_close_full_progress_is_to() {
        let from = make_solid(255, 0, 0, 255, 4, 4);
        let to = make_solid(0, 255, 0, 255, 4, 4);
        let result = render_transition(TransitionKind::BarnDoorClose, &from, &to, 1.0, 4, 4);
        assert_approx_rgb(&result, 0, 255, 0, TOLERANCE);
    }

    #[test]
    fn clock_wipe_full_progress_is_to() {
        let from = make_solid(255, 0, 0, 255, 4, 4);
        let to = make_solid(0, 255, 0, 255, 4, 4);
        let result = render_transition(TransitionKind::ClockWipe, &from, &to, 1.0, 4, 4);
        assert_approx_rgb(&result, 0, 255, 0, TOLERANCE);
    }

    #[test]
    fn clock_wipe_zero_progress_is_from() {
        let from = make_solid(255, 0, 0, 255, 4, 4);
        let to = make_solid(0, 255, 0, 255, 4, 4);
        let result = render_transition(TransitionKind::ClockWipe, &from, &to, 0.0, 4, 4);
        assert_approx_rgb(&result, 255, 0, 0, TOLERANCE);
    }

    #[test]
    fn circle_wipe_out_zero_progress_is_from() {
        let from = make_solid(255, 0, 0, 255, W, H);
        let to = make_solid(0, 255, 0, 255, W, H);
        let result = render_transition(TransitionKind::CircleWipeOut, &from, &to, 0.0, W, H);
        assert_approx_rgb(&result, 255, 0, 0, TOLERANCE);
    }

    // ========================================================================
    // Edge cases
    // ========================================================================

    #[test]
    fn progress_clamped_below_zero() {
        let from = make_solid(255, 0, 0, 255, W, H);
        let to = make_solid(0, 0, 255, 255, W, H);
        // Negative progress should be treated as 0
        let result = render_transition(TransitionKind::CrossDissolve, &from, &to, -0.5, W, H);
        assert_approx_rgb(&result, 255, 0, 0, TOLERANCE);
    }

    #[test]
    fn progress_clamped_above_one() {
        let from = make_solid(255, 0, 0, 255, W, H);
        let to = make_solid(0, 0, 255, 255, W, H);
        // Progress > 1.0 should be treated as 1.0
        let result = render_transition(TransitionKind::CrossDissolve, &from, &to, 1.5, W, H);
        assert_approx_rgb(&result, 0, 0, 255, TOLERANCE);
    }

    #[test]
    fn single_pixel_frame() {
        let from = make_solid(255, 128, 64, 255, 1, 1);
        let to = make_solid(64, 128, 255, 255, 1, 1);
        // Every transition should work on a 1x1 frame without panicking
        let kinds = [
            TransitionKind::CrossDissolve, TransitionKind::WipeLeft,
            TransitionKind::WipeRight, TransitionKind::WipeUp,
            TransitionKind::WipeDown, TransitionKind::DipToBlack,
            TransitionKind::DipToWhite, TransitionKind::PushLeft,
            TransitionKind::FilmDissolve, TransitionKind::BlurDissolve,
            TransitionKind::Flash, TransitionKind::Spin,
            TransitionKind::LightLeak,
            TransitionKind::LuminanceDissolve,
            TransitionKind::CircleWipeIn,
            TransitionKind::CircleWipeOut,
            TransitionKind::ClockWipe,
            TransitionKind::BarnDoorOpen,
            TransitionKind::BarnDoorClose,
            TransitionKind::Pixelate,
            TransitionKind::Ripple,
            TransitionKind::PageTurn,
            TransitionKind::PageTurnReverse,
            TransitionKind::ChromaticAberration,
            TransitionKind::FilmBurn,
            TransitionKind::StarWipe,
            TransitionKind::SplitWipeHorizontal,
            TransitionKind::SplitWipeVertical,
            TransitionKind::VenetianBlindHorizontal,
            TransitionKind::VenetianBlindVertical,
            TransitionKind::CheckerboardWipe,
            TransitionKind::DiagonalWipeTLBR,
            TransitionKind::DiagonalWipeTRBL,
            TransitionKind::BoxWipeIn,
            TransitionKind::BoxWipeOut,
            TransitionKind::SlideLeft,
            TransitionKind::SlideRight,
            TransitionKind::SlideUp,
            TransitionKind::SlideDown,
            TransitionKind::CoverLeft,
            TransitionKind::CoverRight,
            TransitionKind::UncoverLeft,
            TransitionKind::UncoverRight,
            TransitionKind::SqueezeH,
            TransitionKind::SqueezeV,
            TransitionKind::AdditiveDissolve,
            TransitionKind::FadeToColor,
            TransitionKind::GlitchDissolve,
            TransitionKind::Desaturate,
            TransitionKind::NegativeFlash,
            TransitionKind::Sepia,
            TransitionKind::ColorizeFlash,
            TransitionKind::HueRotate,
            TransitionKind::Swirl,
            TransitionKind::WaveDistortion,
            TransitionKind::StaticTV,
            TransitionKind::VHSTracking,
            TransitionKind::GlitchRGBSplit,
            TransitionKind::ProjectorFlicker,
            TransitionKind::Halation,
            TransitionKind::AnamorphicFlare,
            TransitionKind::DreamSequence,
            TransitionKind::BokehDissolve,
            TransitionKind::ZoomInDissolve,
            TransitionKind::ZoomOutDissolve,
            TransitionKind::CrossZoom,
            TransitionKind::WaterDrop,
            TransitionKind::WhipPan,
        ];
        for kind in &kinds {
            let result = render_transition(*kind, &from, &to, 0.5, 1, 1);
            assert_eq!(result.len(), 4, "1x1 frame should produce 4 bytes for {:?}", kind);
        }
    }

    #[test]
    fn all_transitions_preserve_dimensions() {
        let from = make_solid(128, 64, 32, 255, 32, 24);
        let to = make_solid(32, 64, 128, 255, 32, 24);
        let kinds = [
            TransitionKind::CrossDissolve, TransitionKind::WipeLeft,
            TransitionKind::WipeRight, TransitionKind::WipeUp,
            TransitionKind::WipeDown, TransitionKind::DipToBlack,
            TransitionKind::DipToWhite, TransitionKind::PushLeft,
            TransitionKind::FilmDissolve, TransitionKind::BlurDissolve,
            TransitionKind::Flash, TransitionKind::Spin,
            TransitionKind::LightLeak,
            TransitionKind::LuminanceDissolve,
            TransitionKind::CircleWipeIn,
            TransitionKind::CircleWipeOut,
            TransitionKind::ClockWipe,
            TransitionKind::BarnDoorOpen,
            TransitionKind::BarnDoorClose,
            TransitionKind::Pixelate,
            TransitionKind::Ripple,
            TransitionKind::PageTurn,
            TransitionKind::PageTurnReverse,
            TransitionKind::ChromaticAberration,
            TransitionKind::FilmBurn,
            TransitionKind::StarWipe,
            TransitionKind::SplitWipeHorizontal,
            TransitionKind::SplitWipeVertical,
            TransitionKind::VenetianBlindHorizontal,
            TransitionKind::VenetianBlindVertical,
            TransitionKind::CheckerboardWipe,
            TransitionKind::DiagonalWipeTLBR,
            TransitionKind::DiagonalWipeTRBL,
            TransitionKind::BoxWipeIn,
            TransitionKind::BoxWipeOut,
            TransitionKind::SlideLeft,
            TransitionKind::SlideRight,
            TransitionKind::SlideUp,
            TransitionKind::SlideDown,
            TransitionKind::CoverLeft,
            TransitionKind::CoverRight,
            TransitionKind::UncoverLeft,
            TransitionKind::UncoverRight,
            TransitionKind::SqueezeH,
            TransitionKind::SqueezeV,
            TransitionKind::AdditiveDissolve,
            TransitionKind::FadeToColor,
            TransitionKind::GlitchDissolve,
            TransitionKind::Desaturate,
            TransitionKind::NegativeFlash,
            TransitionKind::Sepia,
            TransitionKind::ColorizeFlash,
            TransitionKind::HueRotate,
            TransitionKind::Swirl,
            TransitionKind::WaveDistortion,
            TransitionKind::StaticTV,
            TransitionKind::VHSTracking,
            TransitionKind::GlitchRGBSplit,
            TransitionKind::ProjectorFlicker,
            TransitionKind::Halation,
            TransitionKind::AnamorphicFlare,
            TransitionKind::DreamSequence,
            TransitionKind::BokehDissolve,
            TransitionKind::ZoomInDissolve,
            TransitionKind::ZoomOutDissolve,
            TransitionKind::CrossZoom,
            TransitionKind::WaterDrop,
            TransitionKind::WhipPan,
        ];
        for kind in &kinds {
            for &p in &[0.0f32, 0.25, 0.5, 0.75, 1.0] {
                let result = render_transition(*kind, &from, &to, p, 32, 24);
                assert_dimensions(&result, 32, 24);
            }
        }
    }

    #[test]
    fn identical_frames_produce_same_output() {
        let frame = make_solid(100, 150, 200, 255, W, H);
        // When from == to, any transition at any progress should produce the same frame
        // (with possible minor deviations for effects like LightLeak/Flash)
        let result = render_transition(TransitionKind::CrossDissolve, &frame, &frame, 0.5, W, H);
        assert_approx_rgb(&result, 100, 150, 200, TOLERANCE);
    }
}
