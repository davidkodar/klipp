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

/// How a track's automation is played and recorded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AutomationMode {
    /// The keys are ignored: the controls are as set.
    Off,
    /// The keys are played into the controls (the default).
    #[default]
    Read,
    /// A control's moves are written while it is held; let go, it
    /// returns to the keys.
    Touch,
    /// A control's moves are written from its first touch until
    /// playback stops, its last value held after it is let go.
    Latch,
    /// The controls are written over the whole pass, held or not.
    Write,
}

impl AutomationMode {
    pub const ALL: [AutomationMode; 5] = [Self::Off, Self::Read, Self::Touch, Self::Latch, Self::Write];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Read => "Read",
            Self::Touch => "Touch",
            Self::Latch => "Latch",
            Self::Write => "Write",
        }
    }
}

/// A track control that can be automated. More join here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TrackParam {
    /// The fader: linear gain, 0..2, interpolated in dB between keys.
    Volume,
    /// The pan: -1 (left) to 1 (right), interpolated linearly.
    Pan,
}

impl TrackParam {
    pub fn label(self) -> &'static str {
        match self {
            Self::Volume => "Track volume",
            Self::Pan => "Track pan",
        }
    }
}

/// A track's automation: its mode, and its keys by control.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackAutomation {
    #[serde(default)]
    pub mode: AutomationMode,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lanes: BTreeMap<TrackParam, Vec<Keyframe>>,
}

/// The quietest level a key is taken as, in dB: silence (0 gain) can't be
/// expressed in dB, so it is this, and back to 0 below it.
const FLOOR_DB: f32 = -100.0;

fn to_db(gain: f32) -> f32 {
    if gain <= 0.0 { FLOOR_DB } else { (20.0 * gain.log10()).max(FLOOR_DB) }
}

fn from_db(db: f32) -> f32 {
    if db <= FLOOR_DB { 0.0 } else { 10f32.powf(db / 20.0) }
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
    /// linearly.
    pub fn value_at(&self, param: TrackParam, secs: f64, static_value: f32) -> f32 {
        if !self.plays(param) {
            return static_value;
        }
        let keys = self.keys(param);
        match param {
            TrackParam::Volume => {
                let in_db: Vec<Keyframe> = keys.iter().map(|k| Keyframe { value: to_db(k.value), tangent_in: (k.tangent_in.0, 0.0), tangent_out: (k.tangent_out.0, 0.0), ..k.clone() }).collect();
                from_db(klipp_core::keyframe::evaluate(&in_db, secs, to_db(static_value)))
            }
            TrackParam::Pan => klipp_core::keyframe::evaluate(keys, secs, static_value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
