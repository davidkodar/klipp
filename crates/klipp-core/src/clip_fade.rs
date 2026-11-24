//! How loud a clip's own fades make it at a moment of it, once, for the
//! playback feeder and the export mixdown alike: what is heard is what
//! is exported. A cut into sound gets a short anti-click ramp at its
//! start (its length a preference); its end is left alone unless it has a
//! fade-out, as the next clip carries on, and a fade-out shorter than the
//! ramp is drawn out to it.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::FadeCurve;

static ANTI_CLICK_MS: AtomicU64 = AtomicU64::new(2.0f64.to_bits());

/// The anti-click ramp's length from the preferences, milliseconds.
pub fn set_anti_click_ms(ms: f64) {
    ANTI_CLICK_MS.store(ms.max(0.0).to_bits(), Ordering::Relaxed);
}

/// The anti-click ramp's length, seconds.
pub fn anti_click_secs() -> f64 {
    f64::from_bits(ANTI_CLICK_MS.load(Ordering::Relaxed)) / 1000.0
}

/// A clip's own fades: their lengths in seconds and their curves'
/// indices (see [`FadeCurve::from_index`]).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ClipFades {
    pub fade_in: f64,
    pub fade_out: f64,
    pub in_curve: u8,
    pub out_curve: u8,
}

/// The gain `pos` seconds into a clip of `dur` seconds: its fade-in, or
/// the anti-click ramp of `ramp` seconds where it has none (a straight
/// one), times its fade-out.
pub fn gain(pos: f64, dur: f64, fades: &ClipFades, ramp: f64) -> f32 {
    let mut g = 1.0;
    let fade_in = fades.fade_in.max(ramp);
    if pos < fade_in {
        let curve = if fades.fade_in > 0.001 { fades.in_curve } else { 0 };
        g = curve_gain((pos / fade_in).clamp(0.0, 1.0), curve);
    }
    g as f32 * fade_out_gain(pos, dur, fades, ramp)
}

/// [`gain`]'s fade-out alone, for a clip whose start a crossfade already
/// brings in from silence.
pub fn fade_out_gain(pos: f64, dur: f64, fades: &ClipFades, ramp: f64) -> f32 {
    if fades.fade_out <= 0.001 {
        return 1.0;
    }
    let fade_out = fades.fade_out.max(ramp);
    if pos > dur - fade_out { curve_gain(((dur - pos) / fade_out).clamp(0.0, 1.0), fades.out_curve) as f32 } else { 1.0 }
}

fn curve_gain(t: f64, curve: u8) -> f64 {
    FadeCurve::from_index(curve as u32).gain(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cut into sound ramps in over the anti-click length and plays on
    /// to its end; a fade-in and a fade-out shape it; a fade-out shorter
    /// than the ramp is drawn out to the ramp's length.
    #[test]
    fn a_clips_fades_and_its_anti_click_ramp() {
        let plain = ClipFades::default();
        assert_eq!(gain(0.0, 10.0, &plain, 0.002), 0.0);
        assert!((gain(0.001, 10.0, &plain, 0.002) - 0.5).abs() < 1e-6);
        assert_eq!(gain(5.0, 10.0, &plain, 0.002), 1.0);
        assert_eq!(gain(9.9999, 10.0, &plain, 0.002), 1.0, "no fade at its end without a fade-out");
        assert_eq!(gain(0.0, 10.0, &plain, 0.0), 1.0, "no ramp when there's none");

        let faded = ClipFades { fade_in: 1.0, fade_out: 2.0, ..Default::default() };
        assert!((gain(0.5, 10.0, &faded, 0.002) - 0.5).abs() < 1e-6);
        assert!((gain(9.0, 10.0, &faded, 0.002) - 0.5).abs() < 1e-6);
        assert_eq!(fade_out_gain(0.0, 10.0, &faded, 0.002), 1.0, "the fade-out alone leaves the start");

        let short = ClipFades { fade_out: 0.0015, ..Default::default() };
        assert!((fade_out_gain(9.999, 10.0, &short, 0.002) - 0.5).abs() < 1e-3, "drawn out to the 2 ms ramp");
    }

    #[test]
    fn the_ramp_is_the_preference() {
        set_anti_click_ms(5.0);
        assert!((anti_click_secs() - 0.005).abs() < 1e-12);
        set_anti_click_ms(2.0);
        assert!((anti_click_secs() - 0.002).abs() < 1e-12);
    }
}
