//! The built-in audio effects (#2888): one entry each, which the engine's
//! effect list, the ops' accepted types and params, the effects browser's
//! rows and the stable ids are made from.
//!
//! A new audio effect is an entry here and its arm in
//! `klipp_audio::dsp::create_effect`; a test there fails until both agree,
//! the params included.

/// Where an audio effect sits in the effects browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioGroup {
    Eq,
    Dynamics,
    Reverb,
    Delay,
    Modulation,
    Pitch,
    Restoration,
    Utility,
}

impl AudioGroup {
    /// The group's heading in the browser.
    pub fn label(self) -> &'static str {
        match self {
            AudioGroup::Eq => "EQ",
            AudioGroup::Dynamics => "Dynamics",
            AudioGroup::Reverb => "Reverb",
            AudioGroup::Delay => "Delay",
            AudioGroup::Modulation => "Modulation",
            AudioGroup::Pitch => "Pitch",
            AudioGroup::Restoration => "Restoration",
            AudioGroup::Utility => "Utility",
        }
    }
}

/// One built-in audio effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioEffectSpec {
    /// What a chain saves it by, and the ops name it by.
    pub type_id: &'static str,
    /// The name shown for it.
    pub name: &'static str,
    pub group: AudioGroup,
    /// The line under its name in the browser.
    pub description: &'static str,
    /// Its parameters' names, in the order its DSP describes them.
    pub params: &'static [&'static str],
}

/// The id prefix of an audio effect: `klipp.audio.<type_id>`.
pub const AUDIO: &str = "klipp.audio.";

const fn fx(type_id: &'static str, name: &'static str, group: AudioGroup, description: &'static str, params: &'static [&'static str]) -> AudioEffectSpec {
    AudioEffectSpec { type_id, name, group, description, params }
}

use AudioGroup as G;

/// Every built-in audio effect, in the order the menus list them.
pub const AUDIO_EFFECTS: &[AudioEffectSpec] = &[
    fx("eq", "Parametric EQ", G::Eq, "6-band parametric equaliser", &[
        "band1_enabled", "band1_frequency", "band1_gain", "band1_q", "band2_enabled", "band2_frequency", "band2_gain", "band2_q",
        "band3_enabled", "band3_frequency", "band3_gain", "band3_q", "band4_enabled", "band4_frequency", "band4_gain", "band4_q",
        "band5_enabled", "band5_frequency", "band5_gain", "band5_q", "band6_enabled", "band6_frequency", "band6_gain", "band6_q",
    ]),
    fx("compressor", "Compressor", G::Dynamics, "Dynamic range control", &["threshold", "ratio", "attack", "release", "makeup", "knee"]),
    fx("limiter", "Limiter", G::Dynamics, "Peak protection. Caps the track before its fader", &["threshold", "ceiling", "input_gain", "output_gain", "attack", "release", "lookahead", "mix"]),
    fx("gate", "Noise Gate", G::Dynamics, "Silence low-level noise", &["threshold", "attack", "release", "range"]),
    fx("high_pass", "High-pass Filter", G::Restoration, "Remove low-frequency rumble and wind", &["frequency", "slope"]),
    fx("denoiser", "Broadband Denoiser", G::Restoration, "Learn and reduce steady background noise", &["strength", "floor", "wind", "profile_seconds"]),
    fx("voice_isolation", "Voice Isolation", G::Restoration, "Separate speech from wind, traffic and room noise (AI)", &["reduction", "mode"]),
    fx("reverb", "Reverb", G::Reverb, "Room & space simulation", &["room_size", "damping", "wet", "width", "pre_delay"]),
    fx("delay", "Delay", G::Delay, "Time-based repeat", &["delay_time", "feedback", "mix", "ping_pong"]),
    fx("deesser", "De-Esser", G::Restoration, "Reduce sibilance in vocals", &["frequency", "threshold", "reduction", "bandwidth"]),
    fx("hum_removal", "Hum Removal", G::Restoration, "Notch out mains hum and its harmonics", &["fundamental_hz", "harmonics", "q", "harmonic_2_enabled", "harmonic_3_enabled"]),
    // A multi-band gate, not spectral noise reduction: the type id stays
    // so saved projects keep loading, the name says what it does.
    fx("noise_reduction", "Multi-band Gate", G::Dynamics, "Per-band noise gating", &["reduction", "sensitivity", "smoothing"]),
    fx("declick", "De-click", G::Restoration, "Remove pops & clicks", &["threshold", "max_click_ms"]),
    fx("normalize", "Normalize", G::Utility, "Set target loudness (LUFS)", &["target_lufs", "mode"]),
    fx("pitch_shift", "Pitch Shift", G::Pitch, "Transpose audio pitch", &["semitones", "fine_cents"]),
    fx("chorus", "Chorus", G::Modulation, "Modulation & width", &["rate", "depth", "mix", "voices"]),
    fx("multiband_compressor", "Multiband Compressor", G::Dynamics, "Per-band compression", &[
        "xover_low", "xover_mid", "xover_high", "threshold_1", "threshold_2", "threshold_3", "threshold_4",
        "ratio_1", "ratio_2", "ratio_3", "ratio_4", "makeup_1", "makeup_2", "makeup_3", "makeup_4", "attack", "release",
    ]),
];

/// The built-in audio effect saved as `type_id`.
pub fn audio_effect(type_id: &str) -> Option<&'static AudioEffectSpec> {
    AUDIO_EFFECTS.iter().find(|s| s.type_id == type_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_effect_and_param_is_named_once() {
        let mut ids: Vec<&str> = AUDIO_EFFECTS.iter().map(|s| s.type_id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), AUDIO_EFFECTS.len(), "a type id twice");
        for s in AUDIO_EFFECTS {
            let mut p = s.params.to_vec();
            p.sort_unstable();
            p.dedup();
            assert_eq!(p.len(), s.params.len(), "{}: a param twice", s.type_id);
            assert!(!s.name.is_empty() && !s.description.is_empty(), "{}", s.type_id);
        }
    }
}
