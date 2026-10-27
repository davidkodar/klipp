//! An analysis node's data (M4 step 9): its asset's raw analysis of the
//! whole file, derived once with the node's settings, then read for each
//! frame at the frame's source time.
//!
//! The asset covers the media from its first frame, so a frame's entry is
//! its source time times the analysis rate: a trimmed, sped-up or reversed
//! clip reads the entry of the frame it shows (#1467), where the old route
//! indexed by the clip's timeline time.

use crate::deflicker::DeflickerData;
use crate::homography::Homography;
use crate::rolling_shutter::{compute_row_transforms, RollingShutterData};
use crate::stabilization::{smooth_trajectory, SmoothedTrajectory, StabilizationData};
use crate::{BuiltinEffect, MotionData};

/// A node's analysis, its settings applied.
#[derive(Debug, Clone)]
pub enum NodeAnalysis {
    /// The gain for each frame, at the node's window.
    Deflicker { gains: Vec<f32>, fps: f64 },
    /// The correction for each frame, at the node's smoothing.
    Stabilisation { smoothed: SmoothedTrajectory, fps: f64 },
    /// Each frame's motion to the frames either side.
    RollingShutter(RollingShutterData),
}

/// What an analysis node applies on one frame: its motion and its gain.
#[derive(Clone, Default)]
pub struct AtFrame {
    pub motion: MotionData,
    /// A deflicker's gain at full weight (its strength is the effect's).
    pub gain: Option<f32>,
}

/// The settings of `effect` that shape what's derived from its raw
/// analysis, for keying a derivation: `None` for an effect that reads none.
pub fn derivation_params(effect: &BuiltinEffect) -> Option<u64> {
    match effect {
        BuiltinEffect::Deflicker { window_secs, .. } => Some(window_secs.to_bits() as u64),
        BuiltinEffect::VideoStabilization { smoothing, .. } => Some(smoothing.to_bits() as u64),
        BuiltinEffect::RollingShutterCorrection { .. } => Some(0),
        _ => None,
    }
}

/// `effect`'s analysis from its asset's `raw` data, its settings applied;
/// `None` when the data isn't this effect's.
pub fn derive(effect: &BuiltinEffect, raw: &serde_json::Value) -> Option<NodeAnalysis> {
    match effect {
        BuiltinEffect::Deflicker { window_secs, .. } => {
            let data: DeflickerData = serde_json::from_value(raw.clone()).ok()?;
            Some(NodeAnalysis::Deflicker { gains: data.gains(*window_secs as f64), fps: data.fps })
        }
        BuiltinEffect::VideoStabilization { smoothing, .. } => {
            let data: StabilizationData = serde_json::from_value(raw.clone()).ok()?;
            // As the window the smoothing has always meant: a share of the
            // analysed media's length.
            let duration = data.frame_count as f64 / data.fps.max(1.0);
            let window = (smoothing.clamp(0.0, 1.0) as f64 * duration).max(0.05);
            Some(NodeAnalysis::Stabilisation { smoothed: smooth_trajectory(&data, window), fps: data.fps })
        }
        BuiltinEffect::RollingShutterCorrection { .. } => serde_json::from_value(raw.clone()).ok().map(NodeAnalysis::RollingShutter),
        _ => None,
    }
}

/// The entry for source time `source_secs` in `n` entries at `fps`: the
/// frame shown then. `None` for a time no entry is for, before the first
/// or past the last: an analysis that doesn't cover the frame gives it no
/// correction, rather than another frame's (step 10's decline says so).
fn entry(source_secs: f64, fps: f64, n: usize) -> Option<usize> {
    let at = source_secs * fps;
    // The nearest entry; the last frame's whole span is the last's, so a
    // time in its later half doesn't round past the end.
    let i = at.round().min(if at < n as f64 { n as f64 - 1.0 } else { f64::INFINITY });
    (i >= 0.0 && (i as usize) < n).then_some(i as usize)
}

impl NodeAnalysis {
    /// What `effect` applies on the frame shown at `source_secs`, at the
    /// render's `width` × `height`.
    pub fn at(&self, effect: &BuiltinEffect, source_secs: f64, width: u32, height: u32) -> AtFrame {
        match self {
            NodeAnalysis::Deflicker { gains, fps } => AtFrame { gain: entry(source_secs, *fps, gains.len()).map(|i| gains[i]), ..Default::default() },
            NodeAnalysis::Stabilisation { smoothed, fps } => {
                let (w, h) = (width.max(1) as f64, height.max(1) as f64);
                // Enlarged as far as the clip's corrections need past the
                // effect's own crop, so no frame's edge is ever stretched
                // in (#2771).
                let own = match effect {
                    BuiltinEffect::VideoStabilization { crop_ratio, zoom, .. } => crate::stabilization_crop_zoom(*crop_ratio, *zoom) as f64,
                    _ => 1.0,
                };
                let more = (smoothed.fill_zoom(w / h) / own).max(1.0);
                let enlarge = Homography { h: [[1.0 / more, 0.0, 0.5 * w * (1.0 - 1.0 / more)], [0.0, 1.0 / more, 0.5 * h * (1.0 - 1.0 / more)], [0.0, 0.0, 1.0]] };
                let stabilization = entry(source_secs, *fps, smoothed.corrections.len()).map(|i| {
                    let h = &crate::stabilization::in_pixels(&smoothed.corrections[i], w, h).multiply(&enlarge).h;
                    [h[0][0] as f32, h[0][1] as f32, h[0][2] as f32, h[1][0] as f32, h[1][1] as f32, h[1][2] as f32, h[2][0] as f32, h[2][1] as f32, h[2][2] as f32, 0.0, 0.0, 0.0]
                });
                AtFrame { motion: MotionData { stabilization, rsc_rows: None }, gain: None }
            }
            NodeAnalysis::RollingShutter(data) => {
                let BuiltinEffect::RollingShutterCorrection { strength, readout_ratio } = effect else { return AtFrame::default() };
                let rows = entry(source_secs, data.fps, data.motion.len()).map(|i| {
                    let (prev, next): (Homography, Homography) = data.motion[i];
                    compute_row_transforms(&prev, &next, height, *readout_ratio, *strength)
                });
                AtFrame { motion: MotionData { stabilization: None, rsc_rows: rows }, gain: None }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed clip's frame reads the entry of the source frame it shows,
    /// not of its own place on the timeline (#1467).
    #[test]
    fn a_frame_reads_the_entry_at_its_source_time() {
        let gains: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let a = NodeAnalysis::Deflicker { gains, fps: 25.0 };
        let fx = BuiltinEffect::Deflicker { strength: 1.0, window_secs: 1.0, gain: 1.0 };
        // A clip trimmed to start at 2 s: its first frame shows source 2 s.
        assert_eq!(a.at(&fx, 2.0, 8, 8).gain, Some(50.0));
        assert_eq!(a.at(&fx, 2.04, 8, 8).gain, Some(51.0));
        // Past the analysis, and before it: no entry, never the last one's
        // or the first one's.
        assert_eq!(a.at(&fx, 3.96, 8, 8).gain, Some(99.0));
        assert_eq!(a.at(&fx, 3.99, 8, 8).gain, Some(99.0), "the last frame's later half is still its own");
        assert_eq!(a.at(&fx, 4.0, 8, 8).gain, None);
        assert_eq!(a.at(&fx, -0.04, 8, 8).gain, None);
        // Far past the analysis: none either.
        assert_eq!(a.at(&fx, 100.0, 8, 8).gain, None, "far past the analysis: none");
    }

    /// A deflicker's window and a stabilisation's smoothing shape what's
    /// derived; the raw data is what the asset holds.
    #[test]
    fn the_settings_are_applied_to_the_raw_analysis() {
        let raw = serde_json::to_value(DeflickerData { luma: vec![0.5, 0.6, 0.4, 0.5, 0.55, 0.45, 0.5, 0.5], fps: 25.0 }).unwrap();
        let derive_at = |w: f32| match derive(&BuiltinEffect::Deflicker { strength: 1.0, window_secs: w, gain: 1.0 }, &raw) {
            Some(NodeAnalysis::Deflicker { gains, .. }) => gains,
            _ => panic!("not derived"),
        };
        assert_ne!(derive_at(0.08), derive_at(0.3), "another window, other gains");
        assert!(derive(&BuiltinEffect::Invert { mix: 1.0 }, &raw).is_none());
        assert_ne!(derivation_params(&BuiltinEffect::Deflicker { strength: 1.0, window_secs: 0.5, gain: 1.0 }), derivation_params(&BuiltinEffect::Deflicker { strength: 1.0, window_secs: 1.0, gain: 1.0 }));
    }

    /// A handheld tracking shot (#2771): the camera follows sideways at a
    /// steady 6 px a frame of a 960 × 540 frame, the ground at the bottom
    /// passing faster than the trees at the top, and shakes. The scene
    /// point frame `i` shows at its pixel (`x`, `y`).
    fn tracking(i: usize, x: f64, y: f64) -> (f64, f64) {
        let t = i as f64;
        let turn = 0.004 * (1.1 * t).sin();
        let (dx, dy) = (x - 480.0, y - 270.0);
        let (qx, qy) = (480.0 + dx * turn.cos() - dy * turn.sin(), 270.0 + dx * turn.sin() + dy * turn.cos());
        let shake = (5.0 * (1.9 * t).sin() + 3.0 * (0.7 * t + 0.4).sin(), 4.0 * (1.3 * t + 1.0).sin());
        (qx + t * 6.0 * (0.7 + 0.6 * qy / 540.0) + shake.0, qy + shake.1)
    }

    /// The tracking shot's frame `i` as the analysis decodes it, 480 wide:
    /// blocks of grey, each its own.
    fn tracking_frame(i: usize) -> (Vec<u8>, u32, u32) {
        let (w, h) = (480u32, 270u32);
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (u, v) = tracking(i, x as f64 * 2.0 + 0.5, y as f64 * 2.0 + 0.5);
                let cell = ((u / 20.0).floor() as i64).wrapping_mul(73_856_093) ^ ((v / 20.0).floor() as i64).wrapping_mul(19_349_663);
                let g = 30 + (cell.rem_euclid(196)) as u8;
                px.extend_from_slice(&[g, g, g, 255]);
            }
        }
        (px, w, h)
    }

    /// Stabilised, the tracking shot keeps its track and loses its shake:
    /// each frame's correction only turns, scales and shifts it, never
    /// leaning it however the parallax builds up; the picture still moves
    /// at the camera's pace; and the frame is enlarged just enough that
    /// no output pixel is taken from past its edge.
    #[test]
    fn a_tracking_shot_keeps_its_track_and_loses_its_shake() {
        let n = 96;
        let mut next = 0;
        let mut decode = |_t: f64| {
            next += 1;
            Some(tracking_frame(next - 1))
        };
        let raw = serde_json::to_value(crate::stabilization::analyze_clip(24.0, n, &mut decode, &|_| true).unwrap()).unwrap();
        let fx = BuiltinEffect::VideoStabilization { smoothing: 0.5, crop_ratio: 0.0, zoom: false };
        let a = derive(&fx, &raw).unwrap();
        let (w, h) = (960.0, 540.0);
        let mut view = Vec::new();
        let mut lean = 0.0f64;
        let mut turn = 0.0f64;
        let mut outside = 0.0f64;
        for i in 0..n {
            let m = a.at(&fx, i as f64 / 24.0, w as u32, h as u32).motion.stabilization.unwrap();
            let map = |x: f64, y: f64| {
                let m = m.map(f64::from);
                let z = m[6] * x + m[7] * y + m[8];
                ((m[0] * x + m[1] * y + m[2]) / z, (m[3] * x + m[4] * y + m[5]) / z)
            };
            // A turn, a scale and a shift have equal diagonals and
            // opposite off-diagonals; a lean doesn't.
            lean = lean.max(((m[0] - m[4]).abs() + (m[1] + m[3]).abs()) as f64);
            turn = turn.max((m[3] as f64).atan2(m[0] as f64).abs());
            for (x, y) in [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)] {
                let (sx, sy) = map(x, y);
                outside = outside.max(-sx).max(sx - w).max(-sy).max(sy - h);
            }
            let (cx, cy) = map(w / 2.0, h / 2.0);
            view.push(tracking(i, cx, cy));
        }
        assert!(lean < 0.01, "no lean: {lean}");
        assert!(turn < 0.02, "turned by at most a hand's shake: {turn} rad");
        assert!(outside < 1.0, "never past the frame's edge: {outside} px");
        let still: Vec<(f64, f64)> = (0..n).map(|i| tracking(i, w / 2.0, h / 2.0)).collect();
        let shake = |p: &[(f64, f64)]| {
            let d: Vec<f64> = p.windows(3).map(|k| (k[0].0 - 2.0 * k[1].0 + k[2].0).hypot(k[0].1 - 2.0 * k[1].1 + k[2].1)).collect();
            (d.iter().map(|v| v * v).sum::<f64>() / d.len() as f64).sqrt()
        };
        assert!(shake(&view) < shake(&still) * 0.25, "the shake gone: {} of {}", shake(&view), shake(&still));
        let pace = (view[n - 1].0 - view[0].0) / (n - 1) as f64;
        assert!((pace - 6.0).abs() < 0.9, "the track kept, at 6 px a frame: {pace}");
        let zoom = a.at(&fx, 1.0, w as u32, h as u32);
        let m = zoom.motion.stabilization.unwrap();
        let scale = (m[0] as f64).hypot(m[3] as f64);
        assert!(scale > 0.85, "enlarged no more than the shake needs: {}", 1.0 / scale);
    }
}
