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
        TransitionKind::LuminanceDissolve => 13,
        TransitionKind::CircleWipeIn => 14,
        TransitionKind::CircleWipeOut => 15,
        TransitionKind::ClockWipe => 16,
        TransitionKind::BarnDoorOpen => 17,
        TransitionKind::BarnDoorClose => 18,
        TransitionKind::Pixelate => 19,
        TransitionKind::Ripple => 20,
        TransitionKind::PageTurn => 21,
        TransitionKind::PageTurnReverse => 22,
        TransitionKind::ChromaticAberration => 23,
        TransitionKind::FilmBurn => 24,
        TransitionKind::StarWipe => 25,
        TransitionKind::SplitWipeHorizontal => 26,
        TransitionKind::SplitWipeVertical => 27,
        TransitionKind::VenetianBlindHorizontal => 28,
        TransitionKind::VenetianBlindVertical => 29,
        TransitionKind::CheckerboardWipe => 30,
        TransitionKind::DiagonalWipeTLBR => 31,
        TransitionKind::DiagonalWipeTRBL => 32,
        TransitionKind::BoxWipeIn => 33,
        TransitionKind::BoxWipeOut => 34,
        TransitionKind::SlideLeft => 35,
        TransitionKind::SlideRight => 36,
        TransitionKind::SlideUp => 37,
        TransitionKind::SlideDown => 38,
        TransitionKind::CoverLeft => 39,
        TransitionKind::CoverRight => 40,
        TransitionKind::UncoverLeft => 41,
        TransitionKind::UncoverRight => 42,
        TransitionKind::SqueezeH => 43,
        TransitionKind::SqueezeV => 44,
        TransitionKind::AdditiveDissolve => 45,
        TransitionKind::FadeToColor => 46,
        TransitionKind::GlitchDissolve => 47,
        TransitionKind::Desaturate => 48,
        TransitionKind::NegativeFlash => 49,
        TransitionKind::Sepia => 50,
        TransitionKind::ColorizeFlash => 51,
        TransitionKind::HueRotate => 52,
        TransitionKind::Swirl => 53,
        TransitionKind::WaveDistortion => 54,
        TransitionKind::StaticTV => 55,
        TransitionKind::VHSTracking => 56,
        TransitionKind::GlitchRGBSplit => 57,
        TransitionKind::ProjectorFlicker => 58,
        TransitionKind::Halation => 59,
        TransitionKind::AnamorphicFlare => 60,
        TransitionKind::DreamSequence => 61,
        TransitionKind::BokehDissolve => 62,
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
        13 => TransitionKind::LuminanceDissolve,
        14 => TransitionKind::CircleWipeIn,
        15 => TransitionKind::CircleWipeOut,
        16 => TransitionKind::ClockWipe,
        17 => TransitionKind::BarnDoorOpen,
        18 => TransitionKind::BarnDoorClose,
        19 => TransitionKind::Pixelate,
        20 => TransitionKind::Ripple,
        21 => TransitionKind::PageTurn,
        22 => TransitionKind::PageTurnReverse,
        23 => TransitionKind::ChromaticAberration,
        24 => TransitionKind::FilmBurn,
        25 => TransitionKind::StarWipe,
        26 => TransitionKind::SplitWipeHorizontal,
        27 => TransitionKind::SplitWipeVertical,
        28 => TransitionKind::VenetianBlindHorizontal,
        29 => TransitionKind::VenetianBlindVertical,
        30 => TransitionKind::CheckerboardWipe,
        31 => TransitionKind::DiagonalWipeTLBR,
        32 => TransitionKind::DiagonalWipeTRBL,
        33 => TransitionKind::BoxWipeIn,
        34 => TransitionKind::BoxWipeOut,
        35 => TransitionKind::SlideLeft,
        36 => TransitionKind::SlideRight,
        37 => TransitionKind::SlideUp,
        38 => TransitionKind::SlideDown,
        39 => TransitionKind::CoverLeft,
        40 => TransitionKind::CoverRight,
        41 => TransitionKind::UncoverLeft,
        42 => TransitionKind::UncoverRight,
        43 => TransitionKind::SqueezeH,
        44 => TransitionKind::SqueezeV,
        45 => TransitionKind::AdditiveDissolve,
        46 => TransitionKind::FadeToColor,
        47 => TransitionKind::GlitchDissolve,
        48 => TransitionKind::Desaturate,
        49 => TransitionKind::NegativeFlash,
        50 => TransitionKind::Sepia,
        51 => TransitionKind::ColorizeFlash,
        52 => TransitionKind::HueRotate,
        53 => TransitionKind::Swirl,
        54 => TransitionKind::WaveDistortion,
        55 => TransitionKind::StaticTV,
        56 => TransitionKind::VHSTracking,
        57 => TransitionKind::GlitchRGBSplit,
        58 => TransitionKind::ProjectorFlicker,
        59 => TransitionKind::Halation,
        60 => TransitionKind::AnamorphicFlare,
        61 => TransitionKind::DreamSequence,
        62 => TransitionKind::BokehDissolve,
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
        (13, "Luminance Dissolve"),
        (14, "Circle Wipe In"),
        (15, "Circle Wipe Out"),
        (16, "Clock Wipe"),
        (17, "Barn Door Open"),
        (18, "Barn Door Close"),
        (19, "Pixelate"),
        (20, "Ripple"),
        (21, "Page Turn"),
        (22, "Page Turn Reverse"),
        (23, "Chromatic Aberration"),
        (24, "Film Burn"),
        (25, "Star Wipe"),
        (26, "Split Wipe H"),
        (27, "Split Wipe V"),
        (28, "Venetian Blind H"),
        (29, "Venetian Blind V"),
        (30, "Checkerboard Wipe"),
        (31, "Diagonal TL-BR"),
        (32, "Diagonal TR-BL"),
        (33, "Box Wipe In"),
        (34, "Box Wipe Out"),
        (35, "Slide Left"),
        (36, "Slide Right"),
        (37, "Slide Up"),
        (38, "Slide Down"),
        (39, "Cover Left"),
        (40, "Cover Right"),
        (41, "Uncover Left"),
        (42, "Uncover Right"),
        (43, "Squeeze H"),
        (44, "Squeeze V"),
        (45, "Additive Dissolve"),
        (46, "Fade to Color"),
        (47, "Glitch Dissolve"),
        (48, "Desaturate"),
        (49, "Negative Flash"),
        (50, "Sepia"),
        (51, "Colorize Flash"),
        (52, "Hue Rotate"),
        (53, "Swirl"),
        (54, "Wave Distortion"),
        (55, "Static TV"),
        (56, "VHS Tracking"),
        (57, "Glitch RGB Split"),
        (58, "Projector Flicker"),
        (59, "Halation"),
        (60, "Anamorphic Flare"),
        (61, "Dream Sequence"),
        (62, "Bokeh Dissolve"),
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
        TransitionKind::LuminanceDissolve => {
            // Bright pixels of `from` dissolve to `to` first, dark pixels last.
            // param0 = softness (default 1.0, range 0.0-3.0)
            let softness = (user_params.param0 * 0.15).max(0.01);
            for i in (0..byte_count).step_by(4) {
                let r = from[i] as f32 / 255.0;
                let g = from[i + 1] as f32 / 255.0;
                let b = from[i + 2] as f32 / 255.0;
                let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                let t = smoothstep_f32(p - softness, p + softness, luma);
                output[i] = (from[i] as f32 * (1.0 - t) + to[i] as f32 * t + 0.5) as u8;
                output[i + 1] = (from[i + 1] as f32 * (1.0 - t) + to[i + 1] as f32 * t + 0.5) as u8;
                output[i + 2] = (from[i + 2] as f32 * (1.0 - t) + to[i + 2] as f32 * t + 0.5) as u8;
                output[i + 3] = 255;
            }
        }
        TransitionKind::CircleWipeIn | TransitionKind::CircleWipeOut => {
            // Circle expanding (In) or contracting (Out) from a normalized center.
            // param0 = softness, param1 = cx (0..1), param2 = cy (0..1)
            let cx = user_params.param1 * width as f32;
            let cy = user_params.param2 * height as f32;
            let max_r = ((cx.max(width as f32 - cx)).powi(2)
                + (cy.max(height as f32 - cy)).powi(2))
                .sqrt()
                .max(1.0);
            let softness = (user_params.param0 * 0.04).max(0.001);
            let is_in = matches!(kind, TransitionKind::CircleWipeIn);
            // Extend threshold past [0,1] so endpoints fully cover through the soft edge.
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
                    let s_val = smoothstep_f32(radius - softness, radius + softness, dist);
                    let t = if is_in { 1.0 - s_val } else { s_val };
                    blend_pixel(
                        &from[idx..idx + 4],
                        &to[idx..idx + 4],
                        t,
                        &mut output[idx..idx + 4],
                    );
                }
            }
        }
        TransitionKind::BokehDissolve => {
            // Out-of-focus bokeh bloom with chromatic fringe.
            // param0 = bokeh size, param1 = brightness boost
            let bokeh_size = user_params.param0;
            let brightness_boost = user_params.param1;
            let envelope = (p * std::f32::consts::PI).sin();
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let inv = 1.0 - p;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let base_r = from[fi] as f32 * inv + to[fi] as f32 * p;
                    let base_g = from[fi + 1] as f32 * inv + to[fi + 1] as f32 * p;
                    let base_b = from[fi + 2] as f32 * inv + to[fi + 2] as f32 * p;
                    let luma =
                        (0.2126 * base_r + 0.7152 * base_g + 0.0722 * base_b) / 255.0;
                    let bloom_strength =
                        luma * bokeh_size * envelope * brightness_boost * 2.0;
                    let bloom_val = bloom_strength * 255.0;
                    // Screen blend
                    let screen_r =
                        255.0 - (255.0 - base_r) * (255.0 - bloom_val) / 255.0;
                    let screen_g =
                        255.0 - (255.0 - base_g) * (255.0 - bloom_val) / 255.0;
                    let screen_b =
                        255.0 - (255.0 - base_b) * (255.0 - bloom_val) / 255.0;
                    // Chromatic fringe
                    let dx = (x as f32 - cx) / cx.max(1.0);
                    let dy = (y as f32 - cy) / cy.max(1.0);
                    let radial = (dx * dx + dy * dy).sqrt();
                    let fringe = radial * bokeh_size * envelope * 8.0;
                    output[fi] = (screen_r + fringe).clamp(0.0, 255.0) as u8;
                    output[fi + 1] = screen_g.clamp(0.0, 255.0) as u8;
                    output[fi + 2] = (screen_b - fringe).clamp(0.0, 255.0) as u8;
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::AnamorphicFlare => {
            // Horizontal lens flare streak sweeping across at the cut.
            // param0 = intensity, param1 = streak width, color = flare color
            let intensity = user_params.param0;
            let streak_width = user_params.param1;
            let flare_r = user_params.color[0] * 255.0;
            let flare_g = user_params.color[1] * 255.0;
            let flare_b = user_params.color[2] * 255.0;
            let envelope = (p * std::f32::consts::PI).sin() * intensity;
            let inv = 1.0 - p;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let base_r = from[fi] as f32 * inv + to[fi] as f32 * p;
                    let base_g = from[fi + 1] as f32 * inv + to[fi + 1] as f32 * p;
                    let base_b = from[fi + 2] as f32 * inv + to[fi + 2] as f32 * p;
                    let ny = (y as f32 / height as f32 - 0.5).abs() * 2.0;
                    let streak_t =
                        (1.0 - ny / streak_width.max(0.001)).max(0.0);
                    let streak_t2 = streak_t * streak_t;
                    let shimmer = 0.7
                        + 0.3
                            * (x as f32 / width as f32 * std::f32::consts::PI * 8.0)
                                .sin();
                    let flare_strength = streak_t2 * envelope * shimmer;
                    output[fi] =
                        (base_r + flare_r * flare_strength).clamp(0.0, 255.0) as u8;
                    output[fi + 1] =
                        (base_g + flare_g * flare_strength).clamp(0.0, 255.0) as u8;
                    output[fi + 2] =
                        (base_b + flare_b * flare_strength).clamp(0.0, 255.0) as u8;
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::DreamSequence => {
            // Heavy bloom + warm glow + overexposure — dreamy look.
            // param0 = bloom intensity, param1 = warmth
            let bloom_intensity = user_params.param0;
            let warmth = user_params.param1;
            let envelope = (p * std::f32::consts::PI).sin();
            let inv = 1.0 - p;
            for i in (0..byte_count).step_by(4) {
                let base_r = from[i] as f32 * inv + to[i] as f32 * p;
                let base_g = from[i + 1] as f32 * inv + to[i + 1] as f32 * p;
                let base_b = from[i + 2] as f32 * inv + to[i + 2] as f32 * p;
                let luma =
                    (0.2126 * base_r + 0.7152 * base_g + 0.0722 * base_b) / 255.0;
                let bloom = luma * bloom_intensity * envelope * 60.0;
                let warm_r = warmth * envelope * 30.0;
                let warm_g = warmth * envelope * 15.0;
                let warm_b = -warmth * envelope * 10.0;
                let lift = envelope * bloom_intensity * 15.0;
                output[i] =
                    (base_r + bloom + warm_r + lift).clamp(0.0, 255.0) as u8;
                output[i + 1] =
                    (base_g + bloom + warm_g + lift).clamp(0.0, 255.0) as u8;
                output[i + 2] =
                    (base_b + bloom + warm_b + lift).clamp(0.0, 255.0) as u8;
                output[i + 3] = 255;
            }
        }
        TransitionKind::ProjectorFlicker => {
            // Old film projector: brightness flicker, gate weave, frame jump.
            // param0 = flicker intensity, param1 = gate weave px
            let flicker_intensity = user_params.param0;
            let weave_amount = user_params.param1;
            let envelope = (p * std::f32::consts::PI).sin() * flicker_intensity;
            let frame_hash =
                ((p * 127.1).sin() * 43758.545).fract().abs();
            let frame_hash2 =
                ((p * 311.7).sin() * 12345.678).fract().abs();
            let frame_hash3 =
                ((p * 543.2).sin() * 98765.432).fract().abs();
            let brightness = 1.0 + (frame_hash - 0.5) * envelope * 0.6;
            let weave = ((frame_hash2 - 0.5) * weave_amount * envelope) as isize;
            let jump = if frame_hash3 < envelope * 0.1 {
                ((frame_hash3 - 0.5) * 8.0) as isize
            } else {
                0
            };
            let inv = 1.0 - p;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let sx = (x as isize + weave).clamp(0, w as isize - 1) as usize;
                    let sy = (y as isize + jump).clamp(0, h as isize - 1) as usize;
                    let si = (sy * w + sx) * 4;
                    let r = (from[si] as f32 * inv + to[si] as f32 * p) * brightness;
                    let g =
                        (from[si + 1] as f32 * inv + to[si + 1] as f32 * p) * brightness;
                    let b =
                        (from[si + 2] as f32 * inv + to[si + 2] as f32 * p) * brightness;
                    output[fi] = r.clamp(0.0, 255.0) as u8;
                    output[fi + 1] = g.clamp(0.0, 255.0) as u8;
                    output[fi + 2] = b.clamp(0.0, 255.0) as u8;
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::Halation => {
            // Film halation: per-pixel brightness-threshold additive glow. O(1).
            // param0 = threshold, param1 = spread, color = halo color
            let threshold = user_params.param0;
            let spread = user_params.param1;
            let halo_r = user_params.color[0] * 255.0;
            let halo_g = user_params.color[1] * 255.0;
            let halo_b = user_params.color[2] * 255.0;
            let envelope = (p * std::f32::consts::PI).sin();
            let inv = 1.0 - p;
            for i in (0..byte_count).step_by(4) {
                let base_r = from[i] as f32 * inv + to[i] as f32 * p;
                let base_g = from[i + 1] as f32 * inv + to[i + 1] as f32 * p;
                let base_b = from[i + 2] as f32 * inv + to[i + 2] as f32 * p;
                let luma =
                    (0.2126 * base_r + 0.7152 * base_g + 0.0722 * base_b) / 255.0;
                let excess =
                    (luma - threshold).max(0.0) / (1.0 - threshold).max(0.001);
                let halo_strength = excess * spread * envelope * 3.0;
                output[i] =
                    (base_r + halo_r * halo_strength).clamp(0.0, 255.0) as u8;
                output[i + 1] =
                    (base_g + halo_g * halo_strength).clamp(0.0, 255.0) as u8;
                output[i + 2] =
                    (base_b + halo_b * halo_strength).clamp(0.0, 255.0) as u8;
                output[i + 3] = 255;
            }
        }
        TransitionKind::GlitchRGBSplit => {
            // Hard digital glitch: per-block RGB channel separation with blackouts.
            // param0 = intensity, param1 = block size
            let intensity = user_params.param0;
            let block_size = user_params.param1.max(1.0) as usize;
            let envelope = (p * std::f32::consts::PI).sin() * intensity;
            let inv = 1.0 - p;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let block_x = x / block_size.max(1);
                    let block_y = y / block_size.max(1);
                    let hash_r = ((block_x as f32 * 127.1
                        + block_y as f32 * 311.7
                        + p * 999.9)
                        .sin()
                        * 43758.545)
                        .fract()
                        .abs();
                    let hash_g = ((block_x as f32 * 269.5
                        + block_y as f32 * 183.3
                        + p * 543.2)
                        .sin()
                        * 12345.678)
                        .fract()
                        .abs();
                    let hash_b = ((block_x as f32 * 419.2
                        + block_y as f32 * 371.9
                        + p * 777.3)
                        .sin()
                        * 98765.432)
                        .fract()
                        .abs();
                    let r_shift = ((hash_r - 0.5) * envelope * 80.0) as isize;
                    let g_shift = ((hash_g - 0.5) * envelope * 60.0) as isize;
                    let b_shift = ((hash_b - 0.5) * envelope * 80.0) as isize;
                    let rx = (x as isize + r_shift).clamp(0, w as isize - 1) as usize;
                    let gx = (x as isize + g_shift).clamp(0, w as isize - 1) as usize;
                    let bx = (x as isize + b_shift).clamp(0, w as isize - 1) as usize;
                    let ri = (y * w + rx) * 4;
                    let gi = (y * w + gx) * 4;
                    let bi = (y * w + bx) * 4;
                    let blackout: f32 =
                        if hash_r < envelope * 0.15 { 0.0 } else { 1.0 };
                    let r = (from[ri] as f32 * inv + to[ri] as f32 * p) * blackout;
                    let g =
                        (from[gi + 1] as f32 * inv + to[gi + 1] as f32 * p) * blackout;
                    let b =
                        (from[bi + 2] as f32 * inv + to[bi + 2] as f32 * p) * blackout;
                    output[fi] = r.clamp(0.0, 255.0) as u8;
                    output[fi + 1] = g.clamp(0.0, 255.0) as u8;
                    output[fi + 2] = b.clamp(0.0, 255.0) as u8;
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::StaticTV => {
            // Television static noise overtakes the frame at midpoint.
            // param0 = intensity, param1 = scan line height
            let intensity = user_params.param0;
            let line_h = user_params.param1.max(1.0) as usize;
            let envelope = (p * std::f32::consts::PI).sin() * intensity;
            let inv = 1.0 - p;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let hash = ((x as f32 * 127.1 + y as f32 * 311.7 + p * 999.9).sin()
                        * 43758.545)
                        .fract()
                        .abs();
                    let hash2 = ((x as f32 * 269.5 + y as f32 * 183.3 + p * 543.2).sin()
                        * 12345.678)
                        .fract()
                        .abs();
                    let scan_line: f32 =
                        if (y / line_h.max(1)) % 2 == 0 { 1.0 } else { 0.75 };
                    let static_val = hash * 255.0 * envelope * scan_line;
                    let band = y / line_h.max(1);
                    let band_hash = ((band as f32 * 127.1 + p * 311.7).sin() * 43758.545)
                        .fract()
                        .abs();
                    let shift = ((band_hash - 0.5) * envelope * 20.0) as isize;
                    let sx = (x as isize + shift).clamp(0, w as isize - 1) as usize;
                    let si = (y * w + sx) * 4;
                    let base_r = from[si] as f32 * inv + to[si] as f32 * p;
                    let base_g = from[si + 1] as f32 * inv + to[si + 1] as f32 * p;
                    let base_b = from[si + 2] as f32 * inv + to[si + 2] as f32 * p;
                    let static_mix = envelope * hash2 * 0.8;
                    output[fi] =
                        (base_r + (static_val - base_r) * static_mix).clamp(0.0, 255.0) as u8;
                    output[fi + 1] =
                        (base_g + (static_val - base_g) * static_mix).clamp(0.0, 255.0) as u8;
                    output[fi + 2] =
                        (base_b + (static_val - base_b) * static_mix).clamp(0.0, 255.0) as u8;
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::VHSTracking => {
            // VHS tape tracking error with band shifts and color bleed.
            // param0 = intensity, param1 = band count
            let intensity = user_params.param0;
            let band_count = user_params.param1.max(1.0);
            let envelope = (p * std::f32::consts::PI).sin() * intensity;
            let inv = 1.0 - p;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let band = (y as f32 / height as f32 * band_count) as usize;
                    let band_hash = ((band as f32 * 127.1 + p * 311.7).sin() * 43758.545)
                        .fract()
                        .abs();
                    let band_hash2 = ((band as f32 * 269.5 + p * 183.3).sin() * 12345.678)
                        .fract()
                        .abs();
                    let shift = ((band_hash - 0.5) * envelope * 40.0) as isize;
                    let cshift = ((band_hash2 - 0.5) * envelope * 15.0) as isize;
                    let sx = (x as isize + shift).clamp(0, w as isize - 1) as usize;
                    let csx = (x as isize + cshift).clamp(0, w as isize - 1) as usize;
                    let si = (y * w + sx) * 4;
                    let csi = (y * w + csx) * 4;
                    let base_r = from[si] as f32 * inv + to[si] as f32 * p;
                    let base_g = from[si + 1] as f32 * inv + to[si + 1] as f32 * p;
                    let base_b = from[si + 2] as f32 * inv + to[si + 2] as f32 * p;
                    let luma = 0.2126 * base_r + 0.7152 * base_g + 0.0722 * base_b;
                    let chr = from[csi] as f32 * inv + to[csi] as f32 * p;
                    let chrb = from[csi + 2] as f32 * inv + to[csi + 2] as f32 * p;
                    let brightness = 1.0 - band_hash * envelope * 0.4;
                    let vhs_r = base_r + (chr - base_r) * envelope * 0.5;
                    let vhs_g = base_g + (luma - base_g) * envelope * 0.5;
                    let vhs_b = base_b + (chrb - base_b) * envelope * 0.5;
                    output[fi] = (vhs_r * brightness).clamp(0.0, 255.0) as u8;
                    output[fi + 1] = (vhs_g * brightness).clamp(0.0, 255.0) as u8;
                    output[fi + 2] = (vhs_b * brightness).clamp(0.0, 255.0) as u8;
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::Swirl => {
            // Rotational distortion spiraling from center, peaking at midpoint.
            // param0 = max angle (degrees), param1 = radius falloff
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let max_r = (cx * cx + cy * cy).sqrt().max(1.0);
            let max_angle = user_params.param0 * std::f32::consts::PI / 180.0;
            let falloff = user_params.param1;
            let envelope = (p * std::f32::consts::PI).sin();
            let inv = 1.0 - p;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let dx = x as f32 - cx;
                    let dy = y as f32 - cy;
                    let dist = (dx * dx + dy * dy).sqrt();
                    let norm_dist = dist / max_r;
                    let swirl_angle = max_angle * envelope * (-norm_dist * falloff).exp();
                    let cos_a = swirl_angle.cos();
                    let sin_a = swirl_angle.sin();
                    let sx = (cos_a * dx - sin_a * dy + cx)
                        .clamp(0.0, width as f32 - 1.0) as usize;
                    let sy = (sin_a * dx + cos_a * dy + cy)
                        .clamp(0.0, height as f32 - 1.0) as usize;
                    let si = (sy * w + sx) * 4;
                    for c in 0..3 {
                        output[fi + c] =
                            (from[si + c] as f32 * inv + to[si + c] as f32 * p + 0.5) as u8;
                    }
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::WaveDistortion => {
            // Sinusoidal displacement ripples across the frame.
            // param0 = amplitude (px), param1 = frequency, param2 = direction
            let amplitude = user_params.param0;
            let frequency = user_params.param1;
            let dir = user_params.param2.round() as u32;
            let envelope = (p * std::f32::consts::PI).sin() * amplitude;
            let two_pi = std::f32::consts::TAU;
            let inv = 1.0 - p;
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    let (off_x, off_y) = match dir {
                        1 => (
                            0.0,
                            (x as f32 / width as f32 * frequency * two_pi).sin() * envelope,
                        ),
                        2 => (
                            (y as f32 / height as f32 * frequency * two_pi).sin()
                                * envelope
                                * 0.7,
                            (x as f32 / width as f32 * frequency * two_pi).sin()
                                * envelope
                                * 0.7,
                        ),
                        _ => (
                            (y as f32 / height as f32 * frequency * two_pi).sin() * envelope,
                            0.0,
                        ),
                    };
                    let sx = (x as f32 + off_x).clamp(0.0, width as f32 - 1.0) as usize;
                    let sy = (y as f32 + off_y).clamp(0.0, height as f32 - 1.0) as usize;
                    let si = (sy * w + sx) * 4;
                    for c in 0..3 {
                        output[fi + c] =
                            (from[si + c] as f32 * inv + to[si + c] as f32 * p + 0.5) as u8;
                    }
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::ColorizeFlash => {
            // User-defined color washes over the frame at the cut.
            // param0 = intensity, param1 = peak position, color = flash color
            let flash_r = user_params.color[0] * 255.0;
            let flash_g = user_params.color[1] * 255.0;
            let flash_b = user_params.color[2] * 255.0;
            let intensity = user_params.param0.clamp(0.0, 1.0);
            let peak = user_params.param1.clamp(0.05, 0.95);
            let ramp_up = peak * 0.6;
            let ramp_down = peak + (1.0 - peak) * 0.4;
            let flash_t = (if p < ramp_up {
                p / ramp_up.max(0.001)
            } else if p < peak {
                1.0
            } else if p < ramp_down {
                1.0 - (p - peak) / (ramp_down - peak).max(0.001)
            } else {
                0.0
            }) * intensity;
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
            // Hue spins through the color wheel at midpoint while dissolving.
            // param0 = rotations
            let rotations = user_params.param0;
            let envelope = (p * std::f32::consts::PI).sin() * rotations;
            let hue_shift = envelope * 360.0;
            let inv = 1.0 - p;
            for i in (0..byte_count).step_by(4) {
                let base_r =
                    (from[i] as f32 * inv + to[i] as f32 * p) / 255.0;
                let base_g =
                    (from[i + 1] as f32 * inv + to[i + 1] as f32 * p) / 255.0;
                let base_b =
                    (from[i + 2] as f32 * inv + to[i + 2] as f32 * p) / 255.0;
                // RGB → HSV
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
                // Shift hue
                let h2 = ((h + hue_shift) % 360.0 + 360.0) % 360.0;
                // HSV → RGB
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
            // Brief photographic negative inversion at the cut point.
            // param0 = intensity, param1 = peak position
            let intensity = user_params.param0.clamp(0.0, 1.0);
            let peak = user_params.param1.clamp(0.05, 0.95);
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
            } * intensity;
            let dissolve_start = peak * 0.8;
            let dissolve_t =
                ((p - dissolve_start) / (1.0 - dissolve_start).max(0.001)).clamp(0.0, 1.0);
            let inv_d = 1.0 - dissolve_t;
            for i in (0..byte_count).step_by(4) {
                let base_r = from[i] as f32 * inv_d + to[i] as f32 * dissolve_t;
                let base_g = from[i + 1] as f32 * inv_d + to[i + 1] as f32 * dissolve_t;
                let base_b = from[i + 2] as f32 * inv_d + to[i + 2] as f32 * dissolve_t;
                // mix(base, 255-base, flash_t) = base + (255 - 2*base)*flash_t
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
            // Warm sepia tone through the cut, peaking at midpoint.
            // param0 = strength, param1 = warmth
            let strength = user_params.param0.clamp(0.0, 1.0);
            let warmth = user_params.param1.clamp(0.5, 2.0);
            let envelope = (p * std::f32::consts::PI).sin() * strength;
            let inv = 1.0 - p;
            for i in (0..byte_count).step_by(4) {
                let base_r = from[i] as f32 * inv + to[i] as f32 * p;
                let base_g = from[i + 1] as f32 * inv + to[i + 1] as f32 * p;
                let base_b = from[i + 2] as f32 * inv + to[i + 2] as f32 * p;
                let luma = 0.2126 * base_r + 0.7152 * base_g + 0.0722 * base_b;
                let sepia_r = (luma * warmth * 1.2).clamp(0.0, 255.0);
                let sepia_g = (luma * warmth * 1.0).clamp(0.0, 255.0);
                let sepia_b = (luma * warmth * 0.75).clamp(0.0, 255.0);
                output[i] = (base_r + (sepia_r - base_r) * envelope + 0.5) as u8;
                output[i + 1] = (base_g + (sepia_g - base_g) * envelope + 0.5) as u8;
                output[i + 2] = (base_b + (sepia_b - base_b) * envelope + 0.5) as u8;
                output[i + 3] = 255;
            }
        }
        TransitionKind::GlitchDissolve => {
            // Digital corruption: horizontal block shifts + RGB split + noise.
            // param0 = intensity, param1 = block size
            let intensity = user_params.param0;
            let block_size = user_params.param1.max(1.0) as usize;
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
                    let sx =
                        (x as isize + shift).clamp(0, w as isize - 1) as usize;
                    let r_sx =
                        (x as isize + shift / 2).clamp(0, w as isize - 1) as usize;
                    let b_sx =
                        (x as isize - shift / 2).clamp(0, w as isize - 1) as usize;
                    let si = (y * w + sx) * 4;
                    let r_si = (y * w + r_sx) * 4;
                    let b_si = (y * w + b_sx) * 4;
                    let noise = ((hash2 - 0.5) * envelope * 60.0) as i16;
                    let r = ((from[r_si] as f32 * inv + to[r_si] as f32 * p) as i16 + noise)
                        .clamp(0, 255) as u8;
                    let g = ((from[si + 1] as f32 * inv + to[si + 1] as f32 * p) as i16
                        + noise / 2)
                        .clamp(0, 255) as u8;
                    let b = ((from[b_si + 2] as f32 * inv + to[b_si + 2] as f32 * p) as i16
                        + noise)
                        .clamp(0, 255) as u8;
                    output[fi] = r;
                    output[fi + 1] = g;
                    output[fi + 2] = b;
                    output[fi + 3] = 255;
                }
            }
        }
        TransitionKind::Desaturate => {
            // Drain to greyscale at midpoint then resaturate into clip B.
            // param0 = depth (how fully it desaturates)
            let depth = user_params.param0.clamp(0.0, 1.0);
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
            // Cross-dissolve with additive brightness boost peaking at midpoint.
            let intensity = user_params.param0;
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
            // Fade out to a solid color, then fade in from it.
            let fade_r = (user_params.color[0] * 255.0 + 0.5) as u8;
            let fade_g = (user_params.color[1] * 255.0 + 0.5) as u8;
            let fade_b = (user_params.color[2] * 255.0 + 0.5) as u8;
            let hold = user_params.param0.clamp(0.0, 0.45);
            let mid_start = 0.5 - hold * 0.5;
            let mid_end = 0.5 + hold * 0.5;
            for i in (0..byte_count).step_by(4) {
                if p <= mid_start {
                    let t = p / mid_start.max(0.001);
                    let inv = 1.0 - t;
                    output[i] = (from[i] as f32 * inv + fade_r as f32 * t + 0.5) as u8;
                    output[i + 1] =
                        (from[i + 1] as f32 * inv + fade_g as f32 * t + 0.5) as u8;
                    output[i + 2] =
                        (from[i + 2] as f32 * inv + fade_b as f32 * t + 0.5) as u8;
                } else if p >= mid_end {
                    let t = (p - mid_end) / (1.0 - mid_end).max(0.001);
                    let inv = 1.0 - t;
                    output[i] = (fade_r as f32 * inv + to[i] as f32 * t + 0.5) as u8;
                    output[i + 1] =
                        (fade_g as f32 * inv + to[i + 1] as f32 * t + 0.5) as u8;
                    output[i + 2] =
                        (fade_b as f32 * inv + to[i + 2] as f32 * t + 0.5) as u8;
                } else {
                    output[i] = fade_r;
                    output[i + 1] = fade_g;
                    output[i + 2] = fade_b;
                }
                output[i + 3] = 255;
            }
        }
        TransitionKind::SqueezeH | TransitionKind::SqueezeV => {
            // Proper squeeze: from collapses to a 1-pixel center line at p=0.5,
            // then to expands from that line to full frame. Black outside.
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
                            // Collapsed below one pixel — all black.
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
                                let si =
                                    if is_h { (y * w + src) * 4 } else { (src * w + x) * 4 };
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
                                let si =
                                    if is_h { (y * w + src) * 4 } else { (src * w + x) * 4 };
                                output[fi..fi + 4].copy_from_slice(&to[si..si + 4]);
                            }
                        }
                    }
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
        | TransitionKind::UncoverRight => {
            // Pixel-offset sampling family.
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
                                let fx = (x as isize - offset_w as isize)
                                    .clamp(0, wi - 1) as usize;
                                (false, fx, y)
                            }
                        }
                        TransitionKind::SlideRight => {
                            let to_x = x as isize - wi + offset_w as isize;
                            if to_x >= 0 {
                                (true, (to_x as usize).min(w - 1), y)
                            } else {
                                let fx = (x + offset_w).min(w - 1);
                                (false, fx, y)
                            }
                        }
                        TransitionKind::SlideUp => {
                            let to_y = y as isize + hi - offset_h as isize;
                            if to_y < hi {
                                (true, x, to_y.max(0) as usize)
                            } else {
                                let fy = (y as isize - offset_h as isize)
                                    .clamp(0, hi - 1) as usize;
                                (false, x, fy)
                            }
                        }
                        TransitionKind::SlideDown => {
                            let to_y = y as isize - hi + offset_h as isize;
                            if to_y >= 0 {
                                (true, x, (to_y as usize).min(h - 1))
                            } else {
                                let fy = (y + offset_h).min(h - 1);
                                (false, x, fy)
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
        }
        TransitionKind::BoxWipeIn | TransitionKind::BoxWipeOut => {
            // Rectangle expanding from center (In) or contracting to center (Out).
            // param0 = softness, param1 = square blend (0 = frame aspect, 1 = square)
            let softness = (user_params.param0 * 0.03).max(0.001);
            let square = user_params.param1.clamp(0.0, 1.0);
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let is_in = matches!(kind, TransitionKind::BoxWipeIn);
            // In square mode, landscape frames produce norm_dist up to cx/cy>1.
            // Scale threshold to max possible distance so corners are fully revealed.
            let aspect = (cx / cy.max(1.0)).max(1.0);
            let max_norm = 1.0 + (aspect - 1.0) * square;
            let threshold = if is_in {
                p * (max_norm + 2.0 * softness) - softness
            } else {
                (1.0 - p) * (max_norm + 2.0 * softness) - softness
            };
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let idx = (y * w + x) * 4;
                    let dx = (x as f32 - cx).abs() / cx.max(1.0);
                    let dy = (y as f32 - cy).abs() / cy.max(1.0);
                    let box_dist = dx.max(dy);
                    let sq_dist = (dx * cx / cy.max(1.0)).max(dy);
                    let dist = box_dist + (sq_dist - box_dist) * square;
                    let s_val = smoothstep_f32(
                        threshold - softness,
                        threshold + softness,
                        dist,
                    );
                    // In: reveal where dist < threshold → invert.
                    // Out: reveal where dist > threshold → use directly (with flipped threshold).
                    let t = if is_in { 1.0 - s_val } else { s_val };
                    blend_pixel(
                        &from[idx..idx + 4],
                        &to[idx..idx + 4],
                        t,
                        &mut output[idx..idx + 4],
                    );
                }
            }
        }
        TransitionKind::DiagonalWipeTLBR | TransitionKind::DiagonalWipeTRBL => {
            // Diagonal sweep from one corner to its opposite.
            // param0 = softness
            let softness = (user_params.param0 * 0.03).max(0.001);
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
                    let s_val = smoothstep_f32(
                        threshold - softness,
                        threshold + softness,
                        diag,
                    );
                    let t = 1.0 - s_val;
                    blend_pixel(
                        &from[idx..idx + 4],
                        &to[idx..idx + 4],
                        t,
                        &mut output[idx..idx + 4],
                    );
                }
            }
        }
        TransitionKind::CheckerboardWipe => {
            // Alternating squares wipe left-to-right or right-to-left.
            // param0 = softness, param1 = grid size (squares per axis)
            let softness = (user_params.param0 * 0.04).max(0.001);
            let grid = user_params.param1.max(1.0);
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
                    // Reveal `to` where local_pos < threshold (same pattern as BarnDoorOpen).
                    let s_val = smoothstep_f32(
                        threshold - softness,
                        threshold + softness,
                        local_pos,
                    );
                    let t = 1.0 - s_val;
                    blend_pixel(
                        &from[idx..idx + 4],
                        &to[idx..idx + 4],
                        t,
                        &mut output[idx..idx + 4],
                    );
                }
            }
        }
        TransitionKind::VenetianBlindHorizontal | TransitionKind::VenetianBlindVertical => {
            // N parallel strips wipe simultaneously.
            // param0 = softness, param1 = blind count
            let softness = (user_params.param0 * 0.04).max(0.001);
            let blinds = user_params.param1.max(1.0);
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
                    let s_val = smoothstep_f32(
                        threshold - softness,
                        threshold + softness,
                        strip_pos,
                    );
                    // Blinds reveal `to` where strip_pos < threshold.
                    let t = 1.0 - s_val;
                    blend_pixel(
                        &from[idx..idx + 4],
                        &to[idx..idx + 4],
                        t,
                        &mut output[idx..idx + 4],
                    );
                }
            }
        }
        TransitionKind::SplitWipeHorizontal | TransitionKind::SplitWipeVertical => {
            // Split from center outward — `to` reveals in the center first.
            // param0 = softness
            let softness = (user_params.param0 * 0.03).max(0.001);
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
                    let s_val = smoothstep_f32(
                        threshold - softness,
                        threshold + softness,
                        dist,
                    );
                    let t = 1.0 - s_val;
                    blend_pixel(
                        &from[idx..idx + 4],
                        &to[idx..idx + 4],
                        t,
                        &mut output[idx..idx + 4],
                    );
                }
            }
        }
        TransitionKind::StarWipe => {
            // N-pointed star expanding from center.
            // param0 = softness, param1 = point count, param2 = inner radius ratio
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let points = user_params.param1.max(3.0);
            let inner_ratio = user_params.param2.clamp(0.2, 0.8);
            let softness = (user_params.param0 * 0.03).max(0.001);
            let max_r = (cx * cx + cy * cy).sqrt().max(1.0);
            let sector = std::f32::consts::PI / points;
            let two_sector = 2.0 * sector;
            // Corners of the frame can reach norm_dist = 1/inner_ratio in the star's
            // concave sectors, so scale the threshold to that range for full coverage.
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
                    let sector_angle =
                        ((angle % two_sector) + two_sector) % two_sector;
                    let star_r_norm = inner_ratio
                        + (1.0 - inner_ratio)
                            * smoothstep_f32(0.0, sector, (sector_angle - sector).abs());
                    let norm_dist = dist / (max_r * star_r_norm.max(0.01));
                    let t = 1.0
                        - smoothstep_f32(
                            threshold - softness,
                            threshold + softness,
                            norm_dist,
                        );
                    blend_pixel(
                        &from[idx..idx + 4],
                        &to[idx..idx + 4],
                        t,
                        &mut output[idx..idx + 4],
                    );
                }
            }
        }
        TransitionKind::FilmBurn => {
            // Organic hot-spot bleach/burn with grain, peaks at midpoint.
            // param0 = temperature (0=white, 1=amber), param1 = speed, param2 = grain
            let temp = user_params.param0;
            let speed = user_params.param1;
            let grain = user_params.param2;
            let burn_r: f32 = 1.0;
            let burn_g: f32 = 1.0 - temp * 0.35;
            let burn_b: f32 = 1.0 - temp * 0.75;
            let shaped_p = (p * speed).powf(0.6).min(1.0);
            let envelope = (shaped_p * std::f32::consts::PI).sin();
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    // Pseudo-random per-pixel variation.
                    let hash = ((x as f32 * 127.1 + y as f32 * 311.7).sin() * 43758.545)
                        .fract()
                        .abs();
                    let hash2 = ((x as f32 * 269.5 + y as f32 * 183.3).sin() * 12345.678)
                        .fract()
                        .abs();
                    let pixel_thresh = hash * 0.6 + 0.2;
                    // burn_t: active only while envelope exceeds per-pixel threshold.
                    // No additive bias, so at envelope=0 (p=0 and p=1) burn_t=0.
                    let burn_t = ((envelope - pixel_thresh) / 0.3).clamp(0.0, 1.0);
                    let grain_val = (hash2 - 0.5) * grain * envelope;
                    let base_r =
                        (from[fi] as f32 * (1.0 - p) + to[fi] as f32 * p) / 255.0;
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
        TransitionKind::ChromaticAberration => {
            // RGB channel separation peaking at midpoint.
            // param0 = max offset (px), param1 = direction (0=H, 1=V, 2=diag)
            let max_offset = user_params.param0;
            let dir = user_params.param1.round() as u32;
            let envelope = (p * std::f32::consts::PI).sin() * max_offset;
            let (r_ox, r_oy, b_ox, b_oy) = match dir {
                1 => (0.0, -envelope, 0.0, envelope),
                2 => (
                    -envelope * 0.707,
                    -envelope * 0.707,
                    envelope * 0.707,
                    envelope * 0.707,
                ),
                _ => (-envelope, 0.0, envelope, 0.0),
            };
            let w = width as usize;
            let h = height as usize;
            for y in 0..h {
                for x in 0..w {
                    let fi = (y * w + x) * 4;
                    // Green channel: base blend at pixel position.
                    let base_g = (from[fi + 1] as f32 * (1.0 - p)
                        + to[fi + 1] as f32 * p
                        + 0.5) as u8;
                    // Red channel from r_offset sample.
                    let rx = (x as f32 + r_ox).clamp(0.0, w as f32 - 1.0) as usize;
                    let ry = (y as f32 + r_oy).clamp(0.0, h as f32 - 1.0) as usize;
                    let ri = (ry * w + rx) * 4;
                    let r_val = (from[ri] as f32 * (1.0 - p)
                        + to[ri] as f32 * p
                        + 0.5) as u8;
                    // Blue channel from b_offset sample.
                    let bx = (x as f32 + b_ox).clamp(0.0, w as f32 - 1.0) as usize;
                    let by = (y as f32 + b_oy).clamp(0.0, h as f32 - 1.0) as usize;
                    let bi = (by * w + bx) * 4;
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
        TransitionKind::PageTurn | TransitionKind::PageTurnReverse => {
            // Page peels from one side revealing `to` underneath, with fold shadow.
            let shadow_intensity = user_params.param0;
            let flip = matches!(kind, TransitionKind::PageTurnReverse) || user_params.param1 > 0.5;
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
        TransitionKind::Ripple => {
            // Water ripple: radial sine displacement peaking at midpoint.
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let amplitude = user_params.param0;
            let frequency = user_params.param1;
            let speed = user_params.param2;
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
        TransitionKind::Pixelate => {
            // Blocks grow to `max_block` at p=0.5, shrink to 1 at endpoints.
            // param0 = max block size (px), param1 = mode (0=pixelate then dissolve, 1=cross)
            let max_block = (user_params.param0.max(2.0)) as usize;
            let cross = user_params.param1 > 0.5;
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
                    if cross {
                        // Cross mode: pixelate both and cross-dissolve through p.
                        for c in 0..3 {
                            output[fi + c] = (from[bi + c] as f32 * (1.0 - p)
                                + to[bi + c] as f32 * p
                                + 0.5) as u8;
                        }
                        output[fi + 3] = 255;
                    } else if p < 0.5 {
                        // First half: show pixelated `from` only.
                        output[fi..fi + 4].copy_from_slice(&from[bi..bi + 4]);
                    } else {
                        // Second half: dissolve from pixelated-from to crisp `to`.
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
        TransitionKind::BarnDoorOpen | TransitionKind::BarnDoorClose => {
            // Two panels split from center outward (Open) or close from edges inward (Close).
            // param0 = softness, param1 = orientation (0=horizontal, 1=vertical)
            let vertical = user_params.param1 > 0.5;
            let softness = (user_params.param0 * 0.03).max(0.001);
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let is_open = matches!(kind, TransitionKind::BarnDoorOpen);
            for y in 0..height {
                for x in 0..width {
                    let idx = (y * width + x) as usize * 4;
                    let dist = if vertical {
                        (y as f32 - cy).abs() / cy.max(1.0)
                    } else {
                        (x as f32 - cx).abs() / cx.max(1.0)
                    };
                    let t = if is_open {
                        // Open: reveal expands from center outward — show `to` where dist<thresh
                        let threshold = p * (1.0 + 2.0 * softness) - softness;
                        1.0 - smoothstep_f32(threshold - softness, threshold + softness, dist)
                    } else {
                        // Close: reveal sweeps from edges inward — show `to` where dist>thresh
                        let threshold = (1.0 - p) * (1.0 + 2.0 * softness) - softness;
                        smoothstep_f32(threshold - softness, threshold + softness, dist)
                    };
                    blend_pixel(
                        &from[idx..idx + 4],
                        &to[idx..idx + 4],
                        t,
                        &mut output[idx..idx + 4],
                    );
                }
            }
        }
        TransitionKind::ClockWipe => {
            // Angular sweep from a starting angle around the frame center.
            // param0 = softness, param1 = start angle deg, param2 = direction (0=CW, 1=CCW)
            let cx = width as f32 * 0.5;
            let cy = height as f32 * 0.5;
            let start_rad = user_params.param1 * std::f32::consts::PI / 180.0;
            let ccw = user_params.param2 > 0.5;
            let softness = (user_params.param0 * 0.02).max(0.001);
            let two_pi = std::f32::consts::TAU;
            // Extend threshold so endpoints fully cover through the soft edge.
            let threshold = p * (1.0 + 2.0 * softness) - softness;
            for y in 0..height {
                for x in 0..width {
                    let idx = (y * width + x) as usize * 4;
                    let dx = x as f32 - cx;
                    let dy = y as f32 - cy;
                    // atan2(dx, -dy): 0 at 12 o'clock, increasing clockwise.
                    let mut angle = dx.atan2(-dy) - start_rad;
                    angle = ((angle % two_pi) + two_pi) % two_pi;
                    if ccw {
                        angle = two_pi - angle;
                    }
                    let norm_angle = angle / two_pi;
                    let s_val =
                        smoothstep_f32(threshold - softness, threshold + softness, norm_angle);
                    let t = 1.0 - s_val;
                    blend_pixel(
                        &from[idx..idx + 4],
                        &to[idx..idx + 4],
                        t,
                        &mut output[idx..idx + 4],
                    );
                }
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
    }

    #[test]
    fn kind_id_round_trip() {
        for id in 0..=62u8 {
            let kind = transition_kind_from_id(id);
            assert_eq!(transition_kind_to_id(kind), id as u32);
        }
    }

    #[test]
    fn unknown_kind_id_defaults_to_cross_dissolve() {
        assert_eq!(transition_kind_from_id(255), TransitionKind::CrossDissolve);
        assert_eq!(transition_kind_from_id(63), TransitionKind::CrossDissolve);
    }

    #[test]
    fn all_transition_kinds_returns_63() {
        assert_eq!(all_transition_kinds().len(), 63);
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
