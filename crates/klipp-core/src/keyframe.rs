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
                // Smooth step approximation (cubic hermite)
                let t2 = t * t;
                let t3 = t2 * t;
                let s = 3.0 * t2 - 2.0 * t3; // smooth hermite
                Some(kf0.value + (kf1.value - kf0.value) * s)
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
