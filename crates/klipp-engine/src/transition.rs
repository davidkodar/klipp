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
    /// The partner clip (the clip on the other side of the transition from the current clip).
    pub partner_clip: klipp_timeline::Clip,
    pub track_index: usize,
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
            let (_clip_a, clip_b) = match (clip_a, clip_b) {
                (Some(a), Some(b)) => (a, b),
                _ => continue,
            };

            // The transition region is the overlap between clip_a's end and clip_b's start.
            // transition.duration defines how long the transition lasts.
            // The transition starts at clip_b.timeline_in (which should be < clip_a.timeline_out)
            // and ends at clip_a.timeline_out (or clip_b.timeline_in + transition.duration).
            let trans_start = clip_b.timeline_in;
            let trans_end_tc = trans_start + transition.duration;

            if playhead >= trans_start && playhead < trans_end_tc {
                let elapsed = (playhead - trans_start).to_secs_f64();
                let total = transition.duration.to_secs_f64();
                let progress = if total > 0.0 {
                    (elapsed / total).clamp(0.0, 1.0) as f32
                } else {
                    0.0
                };

                // Determine which clip is the "partner" — the one NOT currently playing.
                // During a transition, the V1 decoder is on clip_a (outgoing).
                // The partner (incoming) is clip_b.
                return Some(ActiveTransition {
                    kind: transition.kind,
                    progress,
                    partner_clip: clip_b.clone(),
                    track_index: track_idx,
                });
            }
        }
    }
    None
}

/// Render a transition between two RGBA frames on the CPU.
pub fn render_transition(
    kind: TransitionKind,
    from: &[u8],
    to: &[u8],
    progress: f32,
    width: u32,
    height: u32,
) -> Vec<u8> {
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
            let threshold = (p * width as f32) as u32;
            for y in 0..height {
                for x in 0..width {
                    let idx = (y * width + x) as usize * 4;
                    if x < threshold {
                        output[idx..idx + 4].copy_from_slice(&to[idx..idx + 4]);
                    } else {
                        output[idx..idx + 4].copy_from_slice(&from[idx..idx + 4]);
                    }
                }
            }
        }
        TransitionKind::WipeRight => {
            let threshold = ((1.0 - p) * width as f32) as u32;
            for y in 0..height {
                for x in 0..width {
                    let idx = (y * width + x) as usize * 4;
                    if x >= threshold {
                        output[idx..idx + 4].copy_from_slice(&to[idx..idx + 4]);
                    } else {
                        output[idx..idx + 4].copy_from_slice(&from[idx..idx + 4]);
                    }
                }
            }
        }
        TransitionKind::WipeUp => {
            let threshold = (p * height as f32) as u32;
            for y in 0..height {
                for x in 0..width {
                    let idx = (y * width + x) as usize * 4;
                    if y < threshold {
                        output[idx..idx + 4].copy_from_slice(&to[idx..idx + 4]);
                    } else {
                        output[idx..idx + 4].copy_from_slice(&from[idx..idx + 4]);
                    }
                }
            }
        }
        TransitionKind::WipeDown => {
            let threshold = ((1.0 - p) * height as f32) as u32;
            for y in 0..height {
                for x in 0..width {
                    let idx = (y * width + x) as usize * 4;
                    if y >= threshold {
                        output[idx..idx + 4].copy_from_slice(&to[idx..idx + 4]);
                    } else {
                        output[idx..idx + 4].copy_from_slice(&from[idx..idx + 4]);
                    }
                }
            }
        }
        TransitionKind::DipToBlack => {
            if p < 0.5 {
                let t = p * 2.0;
                let inv = 1.0 - t;
                for i in (0..byte_count).step_by(4) {
                    output[i] = (from[i] as f32 * inv + 0.5) as u8;
                    output[i + 1] = (from[i + 1] as f32 * inv + 0.5) as u8;
                    output[i + 2] = (from[i + 2] as f32 * inv + 0.5) as u8;
                    output[i + 3] = (from[i + 3] as f32 * inv + 255.0 * t + 0.5) as u8;
                }
            } else {
                let t = (p - 0.5) * 2.0;
                let inv = 1.0 - t;
                for i in (0..byte_count).step_by(4) {
                    output[i] = (to[i] as f32 * t + 0.5) as u8;
                    output[i + 1] = (to[i + 1] as f32 * t + 0.5) as u8;
                    output[i + 2] = (to[i + 2] as f32 * t + 0.5) as u8;
                    output[i + 3] = (to[i + 3] as f32 * t + 255.0 * inv + 0.5) as u8;
                }
            }
        }
        TransitionKind::DipToWhite => {
            if p < 0.5 {
                let t = p * 2.0;
                let inv = 1.0 - t;
                for i in (0..byte_count).step_by(4) {
                    output[i] = (from[i] as f32 * inv + 255.0 * t + 0.5) as u8;
                    output[i + 1] = (from[i + 1] as f32 * inv + 255.0 * t + 0.5) as u8;
                    output[i + 2] = (from[i + 2] as f32 * inv + 255.0 * t + 0.5) as u8;
                    output[i + 3] = from[i + 3];
                }
            } else {
                let t = (p - 0.5) * 2.0;
                let inv = 1.0 - t;
                for i in (0..byte_count).step_by(4) {
                    output[i] = (255.0 * inv + to[i] as f32 * t + 0.5) as u8;
                    output[i + 1] = (255.0 * inv + to[i + 1] as f32 * t + 0.5) as u8;
                    output[i + 2] = (255.0 * inv + to[i + 2] as f32 * t + 0.5) as u8;
                    output[i + 3] = to[i + 3];
                }
            }
        }
        TransitionKind::PushLeft => {
            // Outgoing frame slides left, incoming frame enters from right
            let offset = (p * width as f32) as i32;
            for y in 0..height {
                for x in 0..width {
                    let idx = (y * width + x) as usize * 4;
                    let src_x = x as i32 + offset;
                    if src_x < width as i32 {
                        // Still showing from-frame (shifted left)
                        let src_idx = (y as i32 * width as i32 + src_x) as usize * 4;
                        output[idx..idx + 4].copy_from_slice(&from[src_idx..src_idx + 4]);
                    } else {
                        // Incoming to-frame from right
                        let src_x2 = src_x - width as i32;
                        let src_idx = (y as i32 * width as i32 + src_x2) as usize * 4;
                        output[idx..idx + 4].copy_from_slice(&to[src_idx..src_idx + 4]);
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
            let radius = ((p * (1.0 - p) * 4.0) * 16.0) as i32; // peaks at p=0.5
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
            // Quick flash to white at midpoint, then reveal incoming
            let white_intensity = if p < 0.3 {
                // Ramp to white
                (p / 0.3).min(1.0)
            } else if p < 0.5 {
                // Hold white
                1.0
            } else if p < 0.7 {
                // Fade from white to incoming
                1.0 - ((p - 0.5) / 0.2).min(1.0)
            } else {
                0.0
            };
            let src = if p < 0.4 { from } else { to };
            let blend_p = if p < 0.4 { 0.0 } else { ((p - 0.4) / 0.6).min(1.0) };
            for i in (0..byte_count).step_by(4) {
                for c in 0..3 {
                    let base = if p < 0.4 {
                        from[i + c] as f32
                    } else {
                        from[i + c] as f32 * (1.0 - blend_p) + to[i + c] as f32 * blend_p
                    };
                    output[i + c] = (base * (1.0 - white_intensity) + 255.0 * white_intensity + 0.5).min(255.0) as u8;
                }
                output[i + 3] = src[i + 3];
            }
        }
        TransitionKind::Spin => {
            // Rotation dissolve: sample pixels with rotation transform
            let angle = p * std::f32::consts::PI * 2.0; // full rotation
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let cos_a = angle.cos();
            let sin_a = angle.sin();
            let scale = 1.0 + (p * (1.0 - p) * 4.0) * 0.3; // slight zoom at midpoint
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
            // Warm light leak: orange/amber glow that peaks at midpoint
            let leak_intensity = (p * (1.0 - p) * 4.0).powf(0.7); // peaks at 0.5
            let inv = 1.0 - p;
            for i in (0..byte_count).step_by(4) {
                // Base dissolve
                let r = from[i] as f32 * inv + to[i] as f32 * p;
                let g = from[i + 1] as f32 * inv + to[i + 1] as f32 * p;
                let b = from[i + 2] as f32 * inv + to[i + 2] as f32 * p;
                // Add warm light leak (amber: R=255, G=180, B=60)
                output[i] = (r + (255.0 - r) * leak_intensity * 0.7 + 0.5).min(255.0) as u8;
                output[i + 1] = (g + (180.0 - g).max(0.0) * leak_intensity * 0.5 + 0.5).min(255.0) as u8;
                output[i + 2] = (b + (60.0 - b).max(0.0) * leak_intensity * 0.3 + 0.5).min(255.0) as u8;
                output[i + 3] = (from[i + 3] as f32 * inv + to[i + 3] as f32 * p + 0.5) as u8;
            }
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_solid(r: u8, g: u8, b: u8, a: u8, w: u32, h: u32) -> Vec<u8> {
        let mut buf = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            buf.extend_from_slice(&[r, g, b, a]);
        }
        buf
    }

    #[test]
    fn cross_dissolve_at_zero() {
        let from = make_solid(255, 0, 0, 255, 4, 4);
        let to = make_solid(0, 0, 255, 255, 4, 4);
        let result = render_transition(TransitionKind::CrossDissolve, &from, &to, 0.0, 4, 4);
        assert_eq!(result[0], 255); // R from 'from'
        assert_eq!(result[2], 0);   // B from 'from'
    }

    #[test]
    fn cross_dissolve_at_one() {
        let from = make_solid(255, 0, 0, 255, 4, 4);
        let to = make_solid(0, 0, 255, 255, 4, 4);
        let result = render_transition(TransitionKind::CrossDissolve, &from, &to, 1.0, 4, 4);
        assert_eq!(result[0], 0);   // R from 'to'
        assert_eq!(result[2], 255); // B from 'to'
    }

    #[test]
    fn cross_dissolve_midpoint() {
        let from = make_solid(200, 0, 0, 255, 4, 4);
        let to = make_solid(0, 0, 200, 255, 4, 4);
        let result = render_transition(TransitionKind::CrossDissolve, &from, &to, 0.5, 4, 4);
        assert!((result[0] as i32 - 100).abs() <= 1);
        assert!((result[2] as i32 - 100).abs() <= 1);
    }

    #[test]
    fn wipe_left_halfway() {
        let from = make_solid(255, 0, 0, 255, 10, 1);
        let to = make_solid(0, 255, 0, 255, 10, 1);
        let result = render_transition(TransitionKind::WipeLeft, &from, &to, 0.5, 10, 1);
        // First half should be 'to' (green), second half 'from' (red)
        assert_eq!(result[0], 0);    // pixel 0: to (green R=0)
        assert_eq!(result[1], 255);  // pixel 0: to (green G=255)
        assert_eq!(result[24], 255); // pixel 6: from (red R=255)
        assert_eq!(result[25], 0);   // pixel 6: from (red G=0)
    }

    #[test]
    fn kind_to_id_mapping() {
        assert_eq!(transition_kind_to_id(TransitionKind::CrossDissolve), 0);
        assert_eq!(transition_kind_to_id(TransitionKind::DipToWhite), 6);
    }
}
