//! Deflicker: measuring a clip's brightness pumping, and undoing it.
//!
//! A camera whose shutter is not a whole multiple of the mains period,
//! under fluorescent or cheap LED lighting, records a brightness that
//! oscillates frame to frame. Stage and gym footage does it, and so
//! does anything shot at a high frame rate under lights that pulse at
//! 50 or 60 Hz. Nothing else fixes it: a grade moves every frame the
//! same way, and a denoiser works inside a frame rather than across
//! them.
//!
//! The correction is a scalar per frame. Measure each frame's average
//! light, fit a smooth curve through the sequence, and scale each
//! frame by the ratio between the curve and the measurement. What
//! oscillates faster than the curve is flicker and goes; what the
//! curve follows is the shot genuinely getting brighter or darker, and
//! stays.
//!
//! This module handles **global** flicker — the whole frame pumping
//! together. Banding, where the light pulses faster than the sensor
//! reads out and brightness varies down the frame, is a different
//! problem and is not addressed here.
//!
//! Measured and corrected in linear light, because exposure is
//! multiplicative there: a shutter catching less of a light's cycle
//! scales the light by a constant, and undoing it is a scale back.
//! Done on the encoded values the correction would depend on how dark
//! the frame was.

use crate::cpu::decode;

/// What one analysis pass measured. Cheap: one float per frame.
#[derive(Debug, Clone, PartialEq)]
pub struct DeflickerData {
    /// Mean linear luma per frame, in order from the clip's source in.
    pub luma: Vec<f32>,
    pub fps: f64,
}

/// The gains for a clip, and the window they were computed for.
///
/// Kept beside the measurement the way stabilization keeps its
/// smoothed trajectory beside its raw one: the measurement is the
/// expensive part and does not change, while the window is a control
/// the user moves.
#[derive(Debug, Clone, PartialEq)]
pub struct DeflickerGains {
    pub gains: Vec<f32>,
    pub window_secs: f64,
    pub fps: f64,
}

impl DeflickerGains {
    /// Rebuild if the window has moved, otherwise keep what is there.
    pub fn refresh(
        existing: Option<DeflickerGains>,
        data: &DeflickerData,
        window_secs: f64,
    ) -> DeflickerGains {
        match existing {
            Some(g) if (g.window_secs - window_secs).abs() < 1e-6 && g.fps == data.fps => g,
            _ => DeflickerGains {
                gains: data.gains(window_secs),
                window_secs,
                fps: data.fps,
            },
        }
    }
}

/// Correction below which a frame is left alone, so an unflickering
/// clip is not scaled by a rounding error every frame.
const DEAD_ZONE: f32 = 0.0005;

/// How far a single frame may be pushed. A frame whose measured light
/// is near zero — a cut to black, a lens cap — would otherwise ask for
/// an unbounded gain to reach the curve.
const MAX_GAIN: f32 = 4.0;
const MIN_GAIN: f32 = 0.25;

/// Below this the measurement is noise rather than picture, and the
/// ratio that would correct it means nothing.
const TOO_DARK: f32 = 1e-4;

impl DeflickerData {
    pub fn frame_count(&self) -> usize {
        self.luma.len()
    }

    /// The per-frame gains that flatten the flicker.
    ///
    /// `window_secs` sets what counts as flicker: variation faster than
    /// the window is removed, variation slower than it is kept. It has
    /// to be longer than the flicker's own period and shorter than the
    /// shot's real brightness changes — half a second to a couple of
    /// seconds covers mains beating against a shutter.
    pub fn gains(&self, window_secs: f64) -> Vec<f32> {
        let n = self.luma.len();
        if n == 0 {
            return Vec::new();
        }
        let half = (((window_secs * self.fps) / 2.0).round() as usize).max(1);
        let mut gains = Vec::with_capacity(n);
        for i in 0..n {
            // A centred mean over the window, clipped at the ends
            // rather than wrapped: frame 0's window is the first half
            // window, which is what a shot's start actually has.
            let lo = i.saturating_sub(half);
            let hi = (i + half + 1).min(n);
            let sum: f32 = self.luma[lo..hi].iter().sum();
            let smoothed = sum / (hi - lo) as f32;
            let measured = self.luma[i];
            let g = if measured <= TOO_DARK || smoothed <= TOO_DARK {
                1.0
            } else {
                (smoothed / measured).clamp(MIN_GAIN, MAX_GAIN)
            };
            gains.push(if (g - 1.0).abs() < DEAD_ZONE { 1.0 } else { g });
        }
        gains
    }
}

/// The mean linear luma of one sRGB RGBA8 frame.
///
/// Sampled rather than summed in full: a mean over a few thousand
/// pixels spread across the frame is the same number to far more
/// precision than the correction needs, and analysis walks every frame
/// of the clip.
pub fn frame_luma(rgba: &[u8], width: u32, height: u32) -> f32 {
    let pixels = (width as usize) * (height as usize);
    if pixels == 0 || rgba.len() < pixels * 4 {
        return 0.0;
    }
    // A prime stride so the samples do not land on one column of a
    // repeating pattern — a picket fence, a striped shirt, scanlines.
    const TARGET_SAMPLES: usize = 4096;
    let mut step = (pixels / TARGET_SAMPLES).max(1);
    if step > 1 {
        // Nudge to the next odd number; an even stride on an
        // even-width frame samples the same columns every row.
        step |= 1;
    }
    let mut sum = 0.0f64;
    let mut taken = 0u64;
    let mut i = 0usize;
    while i < pixels {
        let p = i * 4;
        let r = decode(rgba[p] as f32 / 255.0);
        let g = decode(rgba[p + 1] as f32 / 255.0);
        let b = decode(rgba[p + 2] as f32 / 255.0);
        // BT.709's weights, deliberately, and not the working space's.
        // These are sRGB frames straight from the media decoder, not
        // pixels in the project's working space — the analysis runs on
        // the source, before any basis change. And the weighting
        // largely cancels regardless: the gain is a ratio of two means
        // taken the same way.
        sum += (0.2126 * r + 0.7152 * g + 0.0722 * b) as f64;
        taken += 1;
        i += step;
    }
    if taken == 0 {
        0.0
    } else {
        (sum / taken as f64) as f32
    }
}

/// Walk a clip and measure every frame's light.
///
/// `decode_frame` takes a time in seconds from the clip's source in.
/// `progress` is called with 0.0..1.0 and returns false to cancel.
///
/// The same shape as `stabilization::analyze_clip`, and for the same
/// reason: the answer does not depend on the grade in front of it, so
/// it is measured once and kept rather than recomputed per frame.
pub fn analyze_clip(
    fps: f64,
    frame_count: usize,
    decode_frame: &mut dyn FnMut(f64) -> Option<(Vec<u8>, u32, u32)>,
    progress: &dyn Fn(f32) -> bool,
) -> Result<DeflickerData, String> {
    if frame_count < 2 {
        return Err("need at least 2 frames".to_string());
    }
    if fps <= 0.0 {
        return Err("frame rate must be positive".to_string());
    }
    let mut luma = Vec::with_capacity(frame_count);
    for f in 0..frame_count {
        let time = f as f64 / fps;
        let (rgba, w, h) = decode_frame(time)
            .ok_or_else(|| format!("failed to decode frame {f}"))?;
        luma.push(frame_luma(&rgba, w, h));
        if !progress((f + 1) as f32 / frame_count as f32) {
            return Err("cancelled".to_string());
        }
    }
    Ok(DeflickerData { luma, fps })
}

/// The gain to apply at `clip_time` seconds, given `strength`.
///
/// `strength` dials the correction in: 0 leaves the frame alone, 1
/// applies the measured gain in full. Sitting between them is the
/// usual place, because a fully flattened shot can look inert.
pub fn gain_at(gains: &[f32], clip_time: f64, fps: f64, strength: f32) -> f32 {
    if gains.is_empty() || strength <= 0.0 {
        return 1.0;
    }
    let idx = ((clip_time * fps).round().max(0.0) as usize).min(gains.len() - 1);
    1.0 + (gains[idx] - 1.0) * strength.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame of one flat sRGB grey, so the measured luma is that grey
    /// decoded — a known number to build sequences from.
    fn flat_frame(code: u8, w: u32, h: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            v.extend_from_slice(&[code, code, code, 255]);
        }
        v
    }

    #[test]
    fn a_flat_frames_luma_is_its_grey_decoded() {
        for code in [0u8, 64, 128, 200, 255] {
            let f = flat_frame(code, 64, 36);
            let want = decode(code as f32 / 255.0);
            let got = frame_luma(&f, 64, 36);
            assert!((got - want).abs() < 1e-5, "code {code}: want {want}, got {got}");
        }
    }

    /// The sampling stride must not change the answer on a picture
    /// with structure in it — that is the whole risk of sampling.
    #[test]
    fn sampling_agrees_with_measuring_every_pixel() {
        let (w, h) = (321u32, 187u32);
        let mut frame = Vec::with_capacity((w * h * 4) as usize);
        for _y in 0..h {
            for x in 0..w {
                // Gradient plus vertical stripes, which is what a
                // careless stride lands on.
                let base = (x * 255 / (w - 1)) as u8;
                let stripe = if x % 2 == 0 { 40 } else { 0 };
                let v = base.saturating_add(stripe);
                frame.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let sampled = frame_luma(&frame, w, h);

        let mut sum = 0.0f64;
        for p in frame.chunks_exact(4) {
            let r = decode(p[0] as f32 / 255.0);
            let g = decode(p[1] as f32 / 255.0);
            let b = decode(p[2] as f32 / 255.0);
            sum += (0.2126 * r + 0.7152 * g + 0.0722 * b) as f64;
        }
        let exact = (sum / (w * h) as f64) as f32;
        assert!(
            (sampled - exact).abs() < 0.01,
            "sampled {sampled} vs exact {exact}",
        );
    }

    /// The thing it is for. A steady light with a flicker on top comes
    /// back steady, and by a wide margin rather than a little.
    #[test]
    fn a_flickering_sequence_flattens() {
        let fps = 25.0;
        // 2 seconds at 0.18 linear, with a ±12% oscillation every
        // other frame — a shutter beating against the mains.
        let luma: Vec<f32> = (0..50)
            .map(|i| 0.18 * if i % 2 == 0 { 1.12 } else { 0.88 })
            .collect();
        let data = DeflickerData { luma: luma.clone(), fps };
        let gains = data.gains(0.5);

        let corrected: Vec<f32> = luma.iter().zip(&gains).map(|(l, g)| l * g).collect();
        // Ignore the first and last half-window, where the centred mean
        // is one-sided by construction.
        let mid = &corrected[8..corrected.len() - 8];
        let lo = mid.iter().cloned().fold(f32::MAX, f32::min);
        let hi = mid.iter().cloned().fold(f32::MIN, f32::max);
        let spread_before = (1.12 - 0.88) / 1.0; // 24% of the mean
        let spread_after = (hi - lo) / 0.18;
        eprintln!("flicker spread: {spread_before:.3} before, {spread_after:.3} after");
        assert!(
            spread_after < spread_before / 10.0,
            "flicker not flattened: {spread_after} vs {spread_before}",
        );
    }

    /// And the thing it must not do. A shot that genuinely brightens —
    /// a fade up, a pan into the sun — is not flicker, and flattening
    /// it would be the tool destroying the picture.
    #[test]
    fn a_real_brightness_ramp_survives() {
        let fps = 25.0;
        // A fade from 0.05 to 0.40 over two seconds.
        let luma: Vec<f32> = (0..50).map(|i| 0.05 + 0.35 * (i as f32 / 49.0)).collect();
        let data = DeflickerData { luma: luma.clone(), fps };
        let gains = data.gains(0.5);
        let corrected: Vec<f32> = luma.iter().zip(&gains).map(|(l, g)| l * g).collect();

        let first = corrected[8];
        let last = corrected[corrected.len() - 9];
        assert!(
            last > first * 3.0,
            "the ramp was flattened: {first} to {last}",
        );
    }

    /// A near-black frame's measurement is noise, and the ratio that
    /// would lift it to the curve is meaningless. It gets left alone.
    #[test]
    fn a_black_frame_is_not_amplified() {
        let fps = 25.0;
        let mut luma = vec![0.2f32; 20];
        luma[10] = 0.0;
        let data = DeflickerData { luma, fps };
        let gains = data.gains(0.5);
        assert_eq!(gains[10], 1.0, "a black frame asked for a gain");
        for (i, g) in gains.iter().enumerate() {
            assert!(
                (MIN_GAIN..=MAX_GAIN).contains(g),
                "frame {i} gain {g} outside the clamp",
            );
        }
    }

    /// An unflickering clip is left exactly alone, not scaled by a
    /// rounding error on every frame.
    #[test]
    fn a_steady_clip_is_untouched() {
        let data = DeflickerData { luma: vec![0.3f32; 40], fps: 25.0 };
        for g in data.gains(0.5) {
            assert_eq!(g, 1.0);
        }
    }

    /// `strength` has to reach the picture, and its ends have to mean
    /// what they say.
    #[test]
    fn strength_dials_the_correction_in() {
        let gains = [1.5f32, 1.5, 1.5];
        assert_eq!(gain_at(&gains, 0.0, 25.0, 0.0), 1.0, "0 must be a no-op");
        assert_eq!(gain_at(&gains, 0.0, 25.0, 1.0), 1.5, "1 must be the full gain");
        assert!((gain_at(&gains, 0.0, 25.0, 0.5) - 1.25).abs() < 1e-6);
        // Past the end of the clip, the last frame's gain holds rather
        // than the correction snapping to 1.
        assert_eq!(gain_at(&gains, 99.0, 25.0, 1.0), 1.5);
        assert_eq!(gain_at(&[], 0.0, 25.0, 1.0), 1.0, "no analysis, no change");
    }

    #[test]
    fn analysis_walks_every_frame_and_can_be_cancelled() {
        let mut calls = 0;
        let mut decode_frame = |_t: f64| {
            calls += 1;
            Some((flat_frame(128, 8, 8), 8, 8))
        };
        let data = analyze_clip(25.0, 10, &mut decode_frame, &|_| true).expect("analysis");
        assert_eq!(data.frame_count(), 10);
        assert_eq!(calls, 10);

        let mut decode_frame = |_t: f64| Some((flat_frame(128, 8, 8), 8, 8));
        let err = analyze_clip(25.0, 10, &mut decode_frame, &|p| p < 0.25).unwrap_err();
        assert_eq!(err, "cancelled");

        let mut decode_frame = |_t: f64| Some((flat_frame(128, 8, 8), 8, 8));
        assert!(analyze_clip(25.0, 1, &mut decode_frame, &|_| true).is_err());
    }
}
