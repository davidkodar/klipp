/// CPU fallback transition rendering.
/// GPU path in gpu.rs is preferred; this is used when GPU is unavailable.
use klipp_timeline::TransitionKind;

/// Map TransitionKind to the GPU shader's kind_id.
pub fn transition_kind_to_id(kind: TransitionKind) -> u32 {
    match kind {
        TransitionKind::CrossDissolve => 0,
        TransitionKind::WipeLeft => 1,
        TransitionKind::WipeRight => 2,
        TransitionKind::WipeUp => 3,
        TransitionKind::WipeDown => 4,
        TransitionKind::DipToBlack => 5,
        TransitionKind::DipToWhite => 6,
        TransitionKind::PushLeft => 7,
        TransitionKind::FilmDissolve => 8,
        TransitionKind::BlurDissolve => 9,
        TransitionKind::Flash => 10,
        TransitionKind::Spin => 11,
        TransitionKind::LightLeak => 12,
    }
}

/// Map a u8 kind_id back to TransitionKind.
pub fn transition_kind_from_id(id: u8) -> TransitionKind {
    match id {
        0 => TransitionKind::CrossDissolve,
        1 => TransitionKind::WipeLeft,
        2 => TransitionKind::WipeRight,
        3 => TransitionKind::WipeUp,
        4 => TransitionKind::WipeDown,
        5 => TransitionKind::DipToBlack,
        6 => TransitionKind::DipToWhite,
        7 => TransitionKind::PushLeft,
        8 => TransitionKind::FilmDissolve,
        9 => TransitionKind::BlurDissolve,
        10 => TransitionKind::Flash,
        11 => TransitionKind::Spin,
        12 => TransitionKind::LightLeak,
        _ => TransitionKind::CrossDissolve,
    }
}

/// All transition kinds with their display labels.
pub fn all_transition_kinds() -> &'static [(u8, &'static str)] {
    &[
        (0, "Cross Dissolve"),
        (1, "Wipe Left"),
        (2, "Wipe Right"),
        (3, "Wipe Up"),
        (4, "Wipe Down"),
        (5, "Dip to Black"),
        (6, "Dip to White"),
        (7, "Push Left"),
        (8, "Film Dissolve"),
        (9, "Blur Dissolve"),
        (10, "Flash"),
        (11, "Spin"),
        (12, "Light Leak"),
    ]
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

/// Render a transition between two RGBA frames on the CPU, honouring per-kind
/// user parameters. Currently no built-in kind reads `user_params` — the shader
/// and CPU paths still use hardcoded constants — but the plumbing is in place
/// so individual kinds can opt in one at a time.
/// GLSL-style smoothstep: 0 below `edge0`, 1 above `edge1`, smooth Hermite in between.
fn smoothstep_f32(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Linear blend of two RGBA pixels into `out`. `t=0` → from, `t=1` → to.
fn blend_pixel(from: &[u8], to: &[u8], t: f32, out: &mut [u8]) {
    let inv = 1.0 - t;
    out[0] = (from[0] as f32 * inv + to[0] as f32 * t + 0.5) as u8;
    out[1] = (from[1] as f32 * inv + to[1] as f32 * t + 0.5) as u8;
    out[2] = (from[2] as f32 * inv + to[2] as f32 * t + 0.5) as u8;
    out[3] = (from[3] as f32 * inv + to[3] as f32 * t + 0.5) as u8;
}

/// Remap a linear [0,1] `t` through an easing curve selected by `curve` (0..=4).
/// Used by CrossDissolve for user-selectable easing. Exposed so the engine's
/// GPU path can reshape progress before dispatching the shader.
pub fn apply_easing(t: f32, curve: f32) -> f32 {
    match curve as u32 {
        1 => t * t,                                                 // Ease In
        2 => t * (2.0 - t),                                         // Ease Out
        3 => if t < 0.5 { 2.0 * t * t } else { -1.0 + (4.0 - 2.0 * t) * t }, // In-Out
        4 => (-(t * std::f32::consts::PI).cos() + 1.0) * 0.5,       // Sine
        _ => t,                                                     // Linear
    }
}

pub fn render_transition_with_params(
    kind: TransitionKind,
    from: &[u8],
    to: &[u8],
    progress: f32,
    width: u32,
    height: u32,
    user_params: &klipp_core::TransitionUserParams,
) -> Vec<u8> {
    // CrossDissolve: reshape progress via user-selected easing curve.
    let progress = if matches!(kind, TransitionKind::CrossDissolve) {
        apply_easing(progress, user_params.param0)
    } else {
        progress
    };
    let pixel_count = (width * height) as usize;
    let byte_count = pixel_count * 4;
    debug_assert_eq!(from.len(), byte_count);
    debug_assert_eq!(to.len(), byte_count);

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
            let feather = user_params.param0.max(0.001);
            let threshold = p * (width as f32 + 2.0 * feather) - feather;
            for y in 0..height {
                for x in 0..width {
                    let idx = (y * width + x) as usize * 4;
                    let t = 1.0 - smoothstep_f32(threshold - feather, threshold + feather, x as f32);
                    blend_pixel(&from[idx..idx + 4], &to[idx..idx + 4], t, &mut output[idx..idx + 4]);
                }
            }
        }
        TransitionKind::WipeRight => {
            let feather = user_params.param0.max(0.001);
            let threshold = (1.0 - p) * (width as f32 + 2.0 * feather) - feather;
            for y in 0..height {
                for x in 0..width {
                    let idx = (y * width + x) as usize * 4;
                    let t = smoothstep_f32(threshold - feather, threshold + feather, x as f32);
                    blend_pixel(&from[idx..idx + 4], &to[idx..idx + 4], t, &mut output[idx..idx + 4]);
                }
            }
        }
        TransitionKind::WipeUp => {
            let feather = user_params.param0.max(0.001);
            let threshold = p * (height as f32 + 2.0 * feather) - feather;
            for y in 0..height {
                for x in 0..width {
                    let idx = (y * width + x) as usize * 4;
                    let t = 1.0 - smoothstep_f32(threshold - feather, threshold + feather, y as f32);
                    blend_pixel(&from[idx..idx + 4], &to[idx..idx + 4], t, &mut output[idx..idx + 4]);
                }
            }
        }
        TransitionKind::WipeDown => {
            let feather = user_params.param0.max(0.001);
            let threshold = (1.0 - p) * (height as f32 + 2.0 * feather) - feather;
            for y in 0..height {
                for x in 0..width {
                    let idx = (y * width + x) as usize * 4;
                    let t = smoothstep_f32(threshold - feather, threshold + feather, y as f32);
                    blend_pixel(&from[idx..idx + 4], &to[idx..idx + 4], t, &mut output[idx..idx + 4]);
                }
            }
        }
        TransitionKind::DipToBlack | TransitionKind::DipToWhite => {
            // Unified dip-to-colour. param0 = hold duration at midpoint,
            // color = dip colour (default black for DipToBlack, white for DipToWhite).
            let dip_r = (user_params.color[0] * 255.0).clamp(0.0, 255.0);
            let dip_g = (user_params.color[1] * 255.0).clamp(0.0, 255.0);
            let dip_b = (user_params.color[2] * 255.0).clamp(0.0, 255.0);
            let dip_pixel = [dip_r as u8, dip_g as u8, dip_b as u8, 255u8];
            let hold = user_params.param0.clamp(0.0, 0.45);
            let mid_start = 0.5 - hold * 0.5;
            let mid_end = 0.5 + hold * 0.5;
            for i in (0..byte_count).step_by(4) {
                if p <= mid_start {
                    let t = p / mid_start.max(0.001);
                    blend_pixel(&from[i..i + 4], &dip_pixel, t, &mut output[i..i + 4]);
                } else if p >= mid_end {
                    let t = (p - mid_end) / (1.0 - mid_end).max(0.001);
                    blend_pixel(&dip_pixel, &to[i..i + 4], t, &mut output[i..i + 4]);
                } else {
                    output[i..i + 4].copy_from_slice(&dip_pixel);
                }
            }
        }
        TransitionKind::PushLeft => {
            // Push: hard slide, no blending. param0 = direction.
            // 0 = Left (default), 1 = Right, 2 = Up, 3 = Down
            let dir = user_params.param0.round() as i32;
            let w = width as i32;
            let h = height as i32;
            match dir {
                1 => {
                    // Push Right
                    let offset = (p * w as f32) as i32;
                    for y in 0..h {
                        for x in 0..w {
                            let idx = (y * w + x) as usize * 4;
                            let src_x = x - offset;
                            let (src, sx) = if src_x >= 0 {
                                (from, src_x)
                            } else {
                                (to, src_x + w)
                            };
                            let src_idx = (y * w + sx) as usize * 4;
                            output[idx..idx + 4].copy_from_slice(&src[src_idx..src_idx + 4]);
                        }
                    }
                }
                2 => {
                    // Push Up
                    let offset = (p * h as f32) as i32;
                    for y in 0..h {
                        for x in 0..w {
                            let idx = (y * w + x) as usize * 4;
                            let src_y = y + offset;
                            let (src, sy) = if src_y < h {
                                (from, src_y)
                            } else {
                                (to, src_y - h)
                            };
                            let src_idx = (sy * w + x) as usize * 4;
                            output[idx..idx + 4].copy_from_slice(&src[src_idx..src_idx + 4]);
                        }
                    }
                }
                3 => {
                    // Push Down
                    let offset = (p * h as f32) as i32;
                    for y in 0..h {
                        for x in 0..w {
                            let idx = (y * w + x) as usize * 4;
                            let src_y = y - offset;
                            let (src, sy) = if src_y >= 0 {
                                (from, src_y)
                            } else {
                                (to, src_y + h)
                            };
                            let src_idx = (sy * w + x) as usize * 4;
                            output[idx..idx + 4].copy_from_slice(&src[src_idx..src_idx + 4]);
                        }
                    }
                }
                _ => {
                    // Push Left (default — matches the original PushLeft behaviour)
                    let offset = (p * w as f32) as i32;
                    for y in 0..h {
                        for x in 0..w {
                            let idx = (y * w + x) as usize * 4;
                            let src_x = x + offset;
                            let (src, sx) = if src_x < w {
                                (from, src_x)
                            } else {
                                (to, src_x - w)
                            };
                            let src_idx = (y * w + sx) as usize * 4;
                            output[idx..idx + 4].copy_from_slice(&src[src_idx..src_idx + 4]);
                        }
                    }
                }
            }
        }
        TransitionKind::FilmDissolve => {
            // Dissolve with perceptual gamma curve (looks more natural than linear)
            // Uses sqrt blending for a film-like optical dissolve
            let t_sqrt = p.sqrt();
            let inv_sqrt = (1.0 - p).sqrt();
            for i in (0..byte_count).step_by(4) {
                for c in 0..3 {
                    let f = from[i + c] as f32 / 255.0;
                    let t = to[i + c] as f32 / 255.0;
                    let blended = (f * f * (1.0 - p) + t * t * p).sqrt();
                    output[i + c] = (blended * 255.0 + 0.5).min(255.0) as u8;
                }
                output[i + 3] = (from[i + 3] as f32 * inv_sqrt + to[i + 3] as f32 * t_sqrt + 0.5) as u8;
            }
        }
        TransitionKind::BlurDissolve => {
            // Cross dissolve with simulated blur: blend + average with neighbors
            // CPU approximation: box-blur-like blend weighted by progress
            let inv = 1.0 - p;
            // First pass: standard dissolve
            for i in 0..byte_count {
                output[i] = (from[i] as f32 * inv + to[i] as f32 * p + 0.5) as u8;
            }
            // Second pass: horizontal box blur (radius proportional to progress)
            // param0 = max blur radius in pixels
            let max_radius = user_params.param0.max(1.0);
            let radius = (p * (1.0 - p) * 4.0 * max_radius) as i32; // peaks at p=0.5
            if radius > 0 {
                let temp = output.clone();
                for y in 0..height as i32 {
                    for x in 0..width as i32 {
                        let idx = (y * width as i32 + x) as usize * 4;
                        let mut r = 0u32;
                        let mut g = 0u32;
                        let mut b = 0u32;
                        let mut count = 0u32;
                        for dx in -radius..=radius {
                            let sx = (x + dx).clamp(0, width as i32 - 1);
                            let si = (y * width as i32 + sx) as usize * 4;
                            r += temp[si] as u32;
                            g += temp[si + 1] as u32;
                            b += temp[si + 2] as u32;
                            count += 1;
                        }
                        output[idx] = (r / count) as u8;
                        output[idx + 1] = (g / count) as u8;
                        output[idx + 2] = (b / count) as u8;
                        output[idx + 3] = temp[idx + 3];
                    }
                }
            }
        }
        TransitionKind::Flash => {
            // Bright colour flash with parameterized peak.
            // param0 = intensity, param1 = peak position, color = flash colour.
            let flash_r = (user_params.color[0] * 255.0).clamp(0.0, 255.0);
            let flash_g = (user_params.color[1] * 255.0).clamp(0.0, 255.0);
            let flash_b = (user_params.color[2] * 255.0).clamp(0.0, 255.0);
            let intensity = user_params.param0.clamp(0.0, 2.0);
            let peak = user_params.param1.clamp(0.05, 0.95);
            let ramp_up = peak * 0.6;
            let ramp_down = peak + (1.0 - peak) * 0.4;
            let dissolve_start = peak * 0.8;

            let flash_t = if p < ramp_up {
                p / ramp_up.max(0.001)
            } else if p < peak {
                1.0
            } else if p < ramp_down {
                1.0 - (p - peak) / (ramp_down - peak).max(0.001)
            } else {
                0.0
            };
            let flash_t = (flash_t * intensity).clamp(0.0, 1.0);
            let dissolve_t = ((p - dissolve_start) / (1.0 - dissolve_start).max(0.001))
                .clamp(0.0, 1.0);

            for i in (0..byte_count).step_by(4) {
                let base_r = from[i] as f32 * (1.0 - dissolve_t) + to[i] as f32 * dissolve_t;
                let base_g = from[i + 1] as f32 * (1.0 - dissolve_t) + to[i + 1] as f32 * dissolve_t;
                let base_b = from[i + 2] as f32 * (1.0 - dissolve_t) + to[i + 2] as f32 * dissolve_t;
                let base_a = from[i + 3] as f32 * (1.0 - dissolve_t) + to[i + 3] as f32 * dissolve_t;
                output[i] = (base_r * (1.0 - flash_t) + flash_r * flash_t + 0.5).min(255.0) as u8;
                output[i + 1] = (base_g * (1.0 - flash_t) + flash_g * flash_t + 0.5).min(255.0) as u8;
                output[i + 2] = (base_b * (1.0 - flash_t) + flash_b * flash_t + 0.5).min(255.0) as u8;
                output[i + 3] = base_a as u8;
            }
        }
        TransitionKind::Spin => {
            // Rotation dissolve: sample pixels with rotation transform.
            // param0 = rotations (1.0 = one full turn), param1 = zoom bloom amount.
            let rotations = user_params.param0.max(0.05);
            let zoom_amount = user_params.param1.clamp(0.0, 1.0);
            let angle = p * rotations * std::f32::consts::PI * 2.0;
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let cos_a = angle.cos();
            let sin_a = angle.sin();
            let scale = 1.0 + (p * (1.0 - p) * 4.0) * zoom_amount;
            let inv_scale = 1.0 / scale;
            for y in 0..height {
                for x in 0..width {
                    let idx = (y * width + x) as usize * 4;
                    // Rotate around center
                    let dx = x as f32 - cx;
                    let dy = y as f32 - cy;
                    let rx = (dx * cos_a + dy * sin_a) * inv_scale + cx;
                    let ry = (-dx * sin_a + dy * cos_a) * inv_scale + cy;
                    let sx = rx.round().clamp(0.0, width as f32 - 1.0) as u32;
                    let sy = ry.round().clamp(0.0, height as f32 - 1.0) as u32;
                    let src_idx = (sy * width + sx) as usize * 4;
                    // Blend rotated from with to
                    let inv = 1.0 - p;
                    for c in 0..4 {
                        output[idx + c] = (from[src_idx + c] as f32 * inv + to[idx + c] as f32 * p + 0.5) as u8;
                    }
                }
            }
        }
        TransitionKind::LightLeak => {
            // Additive coloured glow that peaks at midpoint.
            // param0 = intensity, color = leak colour (default warm amber).
            let leak_r = user_params.color[0] * 255.0;
            let leak_g = user_params.color[1] * 255.0;
            let leak_b = user_params.color[2] * 255.0;
            let intensity = user_params.param0.clamp(0.0, 2.0);
            let leak_t = (p * (1.0 - p) * 4.0).powf(0.7) * intensity;
            let inv = 1.0 - p;
            // Pure additive — leak colour added uniformly so non-amber leaks look right.
            let add_r = leak_r * leak_t;
            let add_g = leak_g * leak_t;
            let add_b = leak_b * leak_t;
            for i in (0..byte_count).step_by(4) {
                let r = from[i] as f32 * inv + to[i] as f32 * p;
                let g = from[i + 1] as f32 * inv + to[i + 1] as f32 * p;
                let b = from[i + 2] as f32 * inv + to[i + 2] as f32 * p;
                output[i] = (r + add_r + 0.5).min(255.0) as u8;
                output[i + 1] = (g + add_g + 0.5).min(255.0) as u8;
                output[i + 2] = (b + add_b + 0.5).min(255.0) as u8;
                output[i + 3] = (from[i + 3] as f32 * inv + to[i + 3] as f32 * p + 0.5) as u8;
            }
        }
    }

    output
}

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
    }

    #[test]
    fn kind_id_round_trip() {
        for id in 0..=12u8 {
            let kind = transition_kind_from_id(id);
            assert_eq!(transition_kind_to_id(kind), id as u32);
        }
    }

    #[test]
    fn unknown_kind_id_defaults_to_cross_dissolve() {
        assert_eq!(transition_kind_from_id(255), TransitionKind::CrossDissolve);
        assert_eq!(transition_kind_from_id(13), TransitionKind::CrossDissolve);
    }

    #[test]
    fn all_transition_kinds_returns_13() {
        assert_eq!(all_transition_kinds().len(), 13);
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
