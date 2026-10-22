//! What a track's own automation (#2306) is played and recorded by:
//! its mode, and the controls that can be automated. The keys themselves
//! are the track's (`klipp_timeline::TrackAutomation`).

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

