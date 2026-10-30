//! A track's own automation (#2306): keys for its mixer controls over
//! the sequence, and how they are played and recorded.
//!
//! A track's volume and pan were one value each for the whole sequence;
//! the automation lane drew the clips' own keys. These keys are the
//! track's, in sequence seconds: they stay where they are when clips are
//! moved, inserted or rippled, as a mix written against the picture
//! should. With no keys on a control, or the mode Off, the control's own
//! value is used, so a track that was never automated sounds as it did.
//!
//! Playback and export evaluate a control with the one function,
//! [`TrackAutomation::value_at`], so the two agree.

use std::collections::BTreeMap;

use klipp_core::Keyframe;
use serde::{Deserialize, Serialize};

pub use klipp_core::automation::{AutomationMode, TrackParam};

/// A track's automation: its mode, and its keys by control.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackAutomation {
    #[serde(default)]
    pub mode: AutomationMode,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lanes: BTreeMap<TrackParam, Vec<Keyframe>>,
}

/// The level a key at silence (0 gain, which has no level in dB) is
/// moved to and from, at most: at -60 dB a fade to silence is heard to its
/// end, where a lower level spent most of the fade below hearing and
/// sounded like a cut. Keys quieter than it are their own level; silence
/// is then this far below the quietest.
const FLOOR_DB: f32 = -60.0;
const SILENCE_BELOW_DB: f32 = 20.0;

/// A volume in dB, silence at the floor: how automation moves and thins it.
pub fn to_db(gain: f32) -> f32 {
    if gain <= 0.0 { FLOOR_DB } else { 20.0 * gain.log10() }
}

/// A gain at `secs` from keys in gain, interpolated in dB, so a fade
/// sounds even: the track's volume, and an aux send's level (#2861).
pub fn gain_at(keys: &[Keyframe], secs: f64) -> f32 {
    let quietest = keys.iter().filter(|k| k.value > 0.0).map(|k| to_db(k.value)).fold(f32::INFINITY, f32::min);
    let silence = FLOOR_DB.min(quietest - SILENCE_BELOW_DB);
    let level = |g: f32| if g <= 0.0 { silence } else { to_db(g) };
    let in_db: Vec<Keyframe> = keys.iter().map(|k| Keyframe { value: level(k.value), tangent_in: (k.tangent_in.0, 0.0), tangent_out: (k.tangent_out.0, 0.0), ..k.clone() }).collect();
    let db = klipp_core::keyframe::evaluate(&in_db, secs, silence);
    if db <= silence + 1e-3 { 0.0 } else { 10f32.powf(db / 20.0) }
}

impl TrackAutomation {
    /// Whether it is as a track that was never automated has it: saved
    /// only when not.
    pub fn is_default(&self) -> bool {
        self.mode == AutomationMode::default() && self.lanes.values().all(Vec::is_empty)
    }

    /// The keys of `param`, if it has any.
    pub fn keys(&self, param: TrackParam) -> &[Keyframe] {
        self.lanes.get(&param).map_or(&[], Vec::as_slice)
    }

    /// Whether `param` is played from its keys: it has some, and the mode
    /// isn't Off.
    pub fn plays(&self, param: TrackParam) -> bool {
        self.mode != AutomationMode::Off && !self.keys(param).is_empty()
    }

    /// `param` at `secs` on the sequence: its keys' value there when it is
    /// played from them, else `static_value`, the control as set. Volume
    /// is interpolated in dB between keys, so a fade sounds even; pan
    /// linearly. A volume key's value tangents are in gain and don't carry
    /// over to dB, so a Bezier volume curve keeps its timing, flat at its
    /// keys; recorded keys are linear.
    pub fn value_at(&self, param: TrackParam, secs: f64, static_value: f32) -> f32 {
        if !self.plays(param) {
            return static_value;
        }
        let keys = self.keys(param);
        match param {
            TrackParam::Volume => gain_at(keys, secs),
            TrackParam::Pan => klipp_core::keyframe::evaluate(keys, secs, static_value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_db(db: f32) -> f32 {
        10f32.powf(db / 20.0)
    }

    fn key(t: f64, v: f32) -> Keyframe {
        Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
    }

    /// No keys, or Off: the control as set, exactly.
    #[test]
    fn an_unautomated_track_plays_its_controls_as_set() {
        let mut a = TrackAutomation::default();
        assert!(a.is_default());
        assert_eq!(a.value_at(TrackParam::Volume, 3.0, 0.8).to_bits(), 0.8_f32.to_bits());
        a.lanes.insert(TrackParam::Volume, vec![key(0.0, 1.0), key(2.0, 0.5)]);
        a.mode = AutomationMode::Off;
        assert_eq!(a.value_at(TrackParam::Volume, 1.0, 0.8), 0.8);
        assert_eq!(a.value_at(TrackParam::Pan, 1.0, -0.25), -0.25, "a control with no keys");
    }

    /// Volume moves evenly in dB between keys: halfway from 0 dB to
    /// -12 dB is -6 dB, not the linear midpoint; pan moves linearly.
    #[test]
    fn volume_is_interpolated_in_db_and_pan_linearly() {
        let mut a = TrackAutomation::default();
        a.lanes.insert(TrackParam::Volume, vec![key(0.0, 1.0), key(2.0, from_db(-12.0))]);
        a.lanes.insert(TrackParam::Pan, vec![key(0.0, -1.0), key(2.0, 1.0)]);
        let mid = a.value_at(TrackParam::Volume, 1.0, 1.0);
        assert!((to_db(mid) + 6.0).abs() < 1e-3, "{} dB", to_db(mid));
        assert!((a.value_at(TrackParam::Pan, 1.0, 0.0)).abs() < 1e-6);
        assert_eq!(a.value_at(TrackParam::Volume, 5.0, 1.0), from_db(-12.0), "held past the last key");
        a.lanes.insert(TrackParam::Volume, vec![key(0.0, 1.0), key(1.0, 0.0)]);
        assert_eq!(a.value_at(TrackParam::Volume, 1.0, 1.0), 0.0, "a key at silence is silence");
        let half = a.value_at(TrackParam::Volume, 0.5, 1.0);
        assert!((to_db(half) + 30.0).abs() < 1e-3, "a fade to silence is halfway down at halfway: {} dB", to_db(half));
        a.lanes.insert(TrackParam::Volume, vec![key(0.0, from_db(-70.0)), key(1.0, from_db(-80.0)), key(2.0, 0.0)]);
        assert!((to_db(a.value_at(TrackParam::Volume, 0.0, 1.0)) + 70.0).abs() < 1e-3, "a quiet key is its own level, not silence");
        assert!((to_db(a.value_at(TrackParam::Volume, 1.5, 1.0)) + 90.0).abs() < 1e-3, "and fades on to silence below it");
        assert_eq!(a.value_at(TrackParam::Volume, 2.0, 1.0), 0.0);
    }
}
