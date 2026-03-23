use serde::{Deserialize, Serialize};

/// Interpolation method between keyframes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Interpolation {
    /// Linear interpolation between values.
    #[default]
    Linear,
    /// Hold the previous keyframe's value until the next keyframe.
    Step,
    /// Smooth cubic bezier interpolation (tangents on KeyframePoint).
    Bezier,
}

/// A single keyframe: a value at a specific time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Keyframe {
    /// Time in seconds (relative to clip's source_in).
    pub time_secs: f64,
    /// The parameter value at this time.
    pub value: f32,
    /// Interpolation method to the NEXT keyframe.
    pub interpolation: Interpolation,
    /// Bezier out-tangent: (time_delta, value_delta) relative to this keyframe.
    /// Positive time_delta means the handle extends to the right.
    #[serde(default)]
    pub tangent_out: (f64, f32),
    /// Bezier in-tangent: (time_delta, value_delta) relative to this keyframe.
    /// Negative time_delta means the handle extends to the left.
    #[serde(default)]
    pub tangent_in: (f64, f32),
}

/// A track of keyframes for a single parameter.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KeyframeTrack {
    /// Which color grade parameter this track animates.
    pub param: super::ColorGradeParam,
    /// Keyframes sorted by time.
    pub keyframes: Vec<Keyframe>,
}

impl KeyframeTrack {
    /// Evaluate the track at a given time (seconds relative to clip source_in).
    /// Returns None if the track has no keyframes.
    pub fn evaluate(&self, time_secs: f64) -> Option<f32> {
        let kfs = &self.keyframes;
        if kfs.is_empty() {
            return None;
        }
        if kfs.len() == 1 {
            return Some(kfs[0].value);
        }

        // Before first keyframe: hold first value
        if time_secs <= kfs[0].time_secs {
            return Some(kfs[0].value);
        }
        // After last keyframe: hold last value
        if time_secs >= kfs[kfs.len() - 1].time_secs {
            return Some(kfs[kfs.len() - 1].value);
        }

        // Binary search for the segment
        let idx = kfs.partition_point(|kf| kf.time_secs <= time_secs);
        let idx = idx.saturating_sub(1);
        let kf0 = &kfs[idx];
        let kf1 = &kfs[(idx + 1).min(kfs.len() - 1)];

        let dt = kf1.time_secs - kf0.time_secs;
        if dt <= 0.0 {
            return Some(kf0.value);
        }

        let t = ((time_secs - kf0.time_secs) / dt) as f32;

        match kf0.interpolation {
            Interpolation::Linear => {
                Some(kf0.value + (kf1.value - kf0.value) * t)
            }
            Interpolation::Step => {
                Some(kf0.value)
            }
            Interpolation::Bezier => {
                // Cubic bezier with tangent handles
                // Control points: P0=kf0.value, P1=kf0.value+tangent_out.1,
                //                 P2=kf1.value+tangent_in.1, P3=kf1.value
                let p0 = kf0.value;
                let p1 = kf0.value + kf0.tangent_out.1;
                let p2 = kf1.value + kf1.tangent_in.1;
                let p3 = kf1.value;
                let u = 1.0 - t;
                let u2 = u * u;
                let t2 = t * t;
                Some(u2 * u * p0 + 3.0 * u2 * t * p1 + 3.0 * u * t2 * p2 + t2 * t * p3)
            }
        }
    }

    /// Add a keyframe at the given time. If a keyframe exists at that time (±epsilon),
    /// update its value instead.
    pub fn set_keyframe(&mut self, time_secs: f64, value: f32) {
        let epsilon = 0.001; // 1ms tolerance
        // Check for existing keyframe at this time
        for kf in &mut self.keyframes {
            if (kf.time_secs - time_secs).abs() < epsilon {
                kf.value = value;
                return;
            }
        }
        // Insert new keyframe in sorted order
        let idx = self.keyframes.partition_point(|kf| kf.time_secs < time_secs);
        self.keyframes.insert(idx, Keyframe {
            time_secs,
            value,
            interpolation: Interpolation::Linear,
            tangent_out: (0.0, 0.0),
            tangent_in: (0.0, 0.0),
        });
    }

    /// Remove the keyframe nearest to the given time (within epsilon).
    pub fn remove_keyframe(&mut self, time_secs: f64) -> bool {
        let epsilon = 0.01;
        if let Some(idx) = self.keyframes.iter().position(|kf| (kf.time_secs - time_secs).abs() < epsilon) {
            self.keyframes.remove(idx);
            true
        } else {
            false
        }
    }
}

/// Collection of keyframe tracks for a clip's color grade.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KeyframeTracks {
    pub tracks: Vec<KeyframeTrack>,
}

impl KeyframeTracks {
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty() || self.tracks.iter().all(|t| t.keyframes.is_empty())
    }

    /// Find or create a track for the given parameter.
    pub fn track_mut(&mut self, param: super::ColorGradeParam) -> &mut KeyframeTrack {
        if let Some(idx) = self.tracks.iter().position(|t| t.param == param) {
            &mut self.tracks[idx]
        } else {
            self.tracks.push(KeyframeTrack {
                param,
                keyframes: Vec::new(),
            });
            self.tracks.last_mut().unwrap()
        }
    }

    /// Evaluate all tracks at the given time, returning (param, value) pairs.
    pub fn evaluate_all(&self, time_secs: f64) -> Vec<(super::ColorGradeParam, f32)> {
        let mut results = Vec::new();
        for track in &self.tracks {
            if let Some(value) = track.evaluate(time_secs) {
                results.push((track.param, value));
            }
        }
        results
    }

    /// Check if a specific parameter has keyframes.
    pub fn has_keyframes(&self, param: super::ColorGradeParam) -> bool {
        self.tracks.iter().any(|t| t.param == param && !t.keyframes.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_track(keyframes: Vec<Keyframe>) -> KeyframeTrack {
        KeyframeTrack {
            param: crate::ColorGradeParam::Exposure,
            keyframes,
        }
    }

    fn kf(time: f64, value: f32, interp: Interpolation) -> Keyframe {
        Keyframe {
            time_secs: time, value, interpolation: interp,
            tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0),
        }
    }

    #[test]
    fn keyframe_linear_midpoint() {
        let track = make_track(vec![
            kf(0.0, 0.0, Interpolation::Linear),
            kf(1.0, 1.0, Interpolation::Linear),
        ]);
        let v = track.evaluate(0.5).unwrap();
        // Linear midpoint: exactly 0.5
        assert!((v - 0.5).abs() < 0.001, "linear midpoint should be 0.5: got {v}");
    }

    #[test]
    fn keyframe_step_before_second() {
        let track = make_track(vec![
            kf(0.0, 0.0, Interpolation::Step),
            kf(1.0, 1.0, Interpolation::Step),
        ]);
        let v = track.evaluate(0.99).unwrap();
        // Step holds previous value until the next keyframe
        assert!((v - 0.0).abs() < 0.001, "step should hold at 0.0: got {v}");
    }

    #[test]
    fn keyframe_step_at_second() {
        let track = make_track(vec![
            kf(0.0, 0.0, Interpolation::Step),
            kf(1.0, 1.0, Interpolation::Step),
        ]);
        let v = track.evaluate(1.0).unwrap();
        // At or after the second keyframe, should be its value
        assert!((v - 1.0).abs() < 0.001, "at second kf should be 1.0: got {v}");
    }

    #[test]
    fn keyframe_bezier_at_endpoints() {
        // Bezier must pass through its control points (keyframe values)
        let track = make_track(vec![
            kf(0.0, 0.0, Interpolation::Bezier),
            kf(2.0, 1.0, Interpolation::Bezier),
        ]);
        let v0 = track.evaluate(0.0).unwrap();
        let v2 = track.evaluate(2.0).unwrap();
        assert!((v0 - 0.0).abs() < 0.001, "bezier at t=0: got {v0}");
        assert!((v2 - 1.0).abs() < 0.001, "bezier at t=2: got {v2}");
    }

    #[test]
    fn keyframe_bezier_smooth() {
        // Bezier with zero tangents (smooth hermite-like): monotone, no overshoots
        let track = make_track(vec![
            kf(0.0, 0.0, Interpolation::Bezier),
            kf(1.0, 1.0, Interpolation::Bezier),
        ]);
        let mut prev = 0.0f32;
        for i in 0..=20 {
            let t = i as f64 / 20.0;
            let v = track.evaluate(t).unwrap();
            // Monotone: each sample >= previous
            assert!(v >= prev - 0.001, "bezier should be monotone at t={t}: {v} < {prev}");
            // No overshoot past endpoints
            assert!(v >= -0.01 && v <= 1.01, "bezier overshoot at t={t}: {v}");
            prev = v;
        }
    }

    #[test]
    fn keyframe_evaluate_before_first() {
        let track = make_track(vec![
            kf(1.0, 0.7, Interpolation::Linear),
            kf(2.0, 1.0, Interpolation::Linear),
        ]);
        let v = track.evaluate(-1.0).unwrap();
        // Before first keyframe: clamp to first value
        assert!((v - 0.7).abs() < 0.001, "before first kf: got {v}");
    }

    #[test]
    fn keyframe_evaluate_after_last() {
        let track = make_track(vec![
            kf(0.0, 0.0, Interpolation::Linear),
            kf(5.0, 0.3, Interpolation::Linear),
        ]);
        let v = track.evaluate(10.0).unwrap();
        // After last keyframe: clamp to last value
        assert!((v - 0.3).abs() < 0.001, "after last kf: got {v}");
    }

    #[test]
    fn keyframe_single_keyframe() {
        let track = make_track(vec![kf(1.0, 0.7, Interpolation::Linear)]);
        // With only one keyframe, value is constant at any time
        assert!((track.evaluate(0.0).unwrap() - 0.7).abs() < 0.001);
        assert!((track.evaluate(1.0).unwrap() - 0.7).abs() < 0.001);
        assert!((track.evaluate(99.0).unwrap() - 0.7).abs() < 0.001);
    }
}
