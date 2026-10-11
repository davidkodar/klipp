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
    /// render's `height`.
    pub fn at(&self, effect: &BuiltinEffect, source_secs: f64, height: u32) -> AtFrame {
        match self {
            NodeAnalysis::Deflicker { gains, fps } => AtFrame { gain: entry(source_secs, *fps, gains.len()).map(|i| gains[i]), ..Default::default() },
            NodeAnalysis::Stabilisation { smoothed, fps } => {
                let stabilization = entry(source_secs, *fps, smoothed.corrections.len()).map(|i| {
                    let h = &smoothed.corrections[i].h;
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
        assert_eq!(a.at(&fx, 2.0, 8).gain, Some(50.0));
        assert_eq!(a.at(&fx, 2.04, 8).gain, Some(51.0));
        // Past the analysis, and before it: no entry, never the last one's
        // or the first one's.
        assert_eq!(a.at(&fx, 3.96, 8).gain, Some(99.0));
        assert_eq!(a.at(&fx, 3.99, 8).gain, Some(99.0), "the last frame's later half is still its own");
        assert_eq!(a.at(&fx, 4.0, 8).gain, None);
        assert_eq!(a.at(&fx, -0.04, 8).gain, None);
        // Far past the analysis: none either.
        assert_eq!(a.at(&fx, 100.0, 8).gain, None, "far past the analysis: none");
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
}
