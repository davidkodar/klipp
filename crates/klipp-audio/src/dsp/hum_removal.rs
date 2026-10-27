//! Mains-hum removal — cascaded narrow-Q notch filters at fundamental
//! + harmonics.
//!
//! Defaults match Resolve / iZotope RX: 50 Hz fundamental, 3 harmonics
//! (50/100/150 Hz for Europe/Asia, or 60/120/180 Hz when the palette
//! flips `fundamental_hz` to 60 for the Americas), Q = 30 (≥ 30 dB
//! attenuation at the centre, ≤ 0.5 dB ripple at ±25 Hz so dialog
//! intelligibility is preserved).
//!
//! Internally cascades up to 5 [`super::biquad::BiquadFilter`] sections
//! in `FilterType::Notch`. Per-harmonic enable toggles let surgical
//! users keep the fundamental + 3rd while killing the 2nd, etc.

use super::biquad::{BiquadFilter, FilterType};
use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};

const MAX_CHANNELS: usize = 2;
const MAX_HARMONICS: usize = 5;

/// Per-channel cascade of up to `MAX_HARMONICS` notch sections.
type ChannelCascade = [BiquadFilter; MAX_HARMONICS];

pub struct HumRemoval {
    sample_rate: f32,
    fundamental_hz: f32,
    harmonics: usize,
    q: f32,
    /// Per-harmonic enable. `harmonic_enabled[0]` is the fundamental
    /// itself and is always treated as enabled — the toggles only
    /// gate harmonics 2..=5 (indices 1..=4) so users can surgically
    /// drop a harmonic that happens to overlap with musical content.
    harmonic_enabled: [bool; MAX_HARMONICS],
    channels: [ChannelCascade; MAX_CHANNELS],
    dirty: bool,
}

impl HumRemoval {
    pub fn new(sample_rate: u32) -> Self {
        let mut hr = Self {
            sample_rate: sample_rate as f32,
            fundamental_hz: 50.0,
            // Up to the 5th: mains hum carries its odd harmonics most, and
            // with three the 250 Hz of 50 Hz hum stayed (#2779).
            harmonics: 5,
            q: 30.0,
            harmonic_enabled: [true; MAX_HARMONICS],
            channels: std::array::from_fn(|_| std::array::from_fn(|_| BiquadFilter::new())),
            dirty: true,
        };
        hr.recompute();
        hr
    }

    fn recompute(&mut self) {
        let n = self.harmonics.clamp(1, MAX_HARMONICS);
        for ch in 0..MAX_CHANNELS {
            for h in 0..MAX_HARMONICS {
                let freq = self.fundamental_hz * (h as f32 + 1.0);
                // Even when a harmonic is gated off we still set
                // coefficients so a later toggle-on doesn't see stale
                // state. Bypass happens at process-time.
                self.channels[ch][h].set_params(
                    FilterType::Notch,
                    freq,
                    0.0, // gain unused for Notch
                    self.q,
                    self.sample_rate,
                );
                if h >= n {
                    // Beyond the user's harmonic count — reset state so
                    // a later harmonics-bump doesn't audibly click from
                    // accumulated state.
                    self.channels[ch][h].reset();
                }
            }
        }
        self.dirty = false;
    }

    /// Whether harmonic `h` (0-indexed: 0 = fundamental) participates
    /// in processing right now. Fundamental is unconditional; 2nd+
    /// require both `h < harmonics` AND the per-harmonic toggle.
    fn harmonic_active(&self, h: usize) -> bool {
        if h == 0 {
            return self.harmonics >= 1;
        }
        h < self.harmonics && self.harmonic_enabled[h]
    }

    /// Magnitude of the cascade at `freq`. Used by tests; mirrors
    /// `BiquadFilter::magnitude_at` shape so we can pin attenuation
    /// without standing up an FFT.
    ///
    /// Takes `&mut self` so a stale dirty flag (from a recent
    /// `set_param`) triggers a recompute first — otherwise tests that
    /// tweak params then probe magnitude would see stale coefficients.
    /// AT EXACT NOTCH CENTRES this measure is unreliable in f32: both
    /// numerator and denominator are tiny (zero and pole nearly on top
    /// of each other for high Q) and cancellation noise dominates.
    /// Probe a few Hz off-centre for stable readings.
    pub fn magnitude_at(&mut self, freq: f32) -> f32 {
        if self.dirty {
            self.recompute();
        }
        let mut mag = 1.0_f32;
        for h in 0..MAX_HARMONICS {
            if !self.harmonic_active(h) {
                continue;
            }
            mag *= self.channels[0][h].magnitude_at(freq, self.sample_rate);
        }
        mag
    }
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor {
        name: "fundamental_hz",
        label: "Fundamental",
        default: 50.0,
        min: 30.0,
        max: 200.0,
        unit: ParamUnit::Hz,
    },
    AudioParamDescriptor {
        name: "harmonics",
        label: "Harmonics",
        default: 5.0,
        min: 1.0,
        max: 5.0,
        unit: ParamUnit::None,
    },
    AudioParamDescriptor {
        name: "q",
        label: "Q",
        default: 30.0,
        min: 5.0,
        max: 60.0,
        unit: ParamUnit::None,
    },
    AudioParamDescriptor {
        name: "harmonic_2_enabled",
        label: "2nd Harmonic",
        default: 1.0,
        min: 0.0,
        max: 1.0,
        unit: ParamUnit::None,
    },
    AudioParamDescriptor {
        name: "harmonic_3_enabled",
        label: "3rd Harmonic",
        default: 1.0,
        min: 0.0,
        max: 1.0,
        unit: ParamUnit::None,
    },
];

impl AudioEffect for HumRemoval {
    fn type_id(&self) -> &'static str {
        "hum_removal"
    }

    fn name(&self) -> &'static str {
        "Hum Removal"
    }

    fn param_descriptors(&self) -> &[AudioParamDescriptor] {
        PARAM_DESCS
    }

    fn get_param(&self, name: &str) -> Option<f32> {
        match name {
            "fundamental_hz" => Some(self.fundamental_hz),
            "harmonics" => Some(self.harmonics as f32),
            "q" => Some(self.q),
            "harmonic_2_enabled" => Some(if self.harmonic_enabled[1] { 1.0 } else { 0.0 }),
            "harmonic_3_enabled" => Some(if self.harmonic_enabled[2] { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "fundamental_hz" => self.fundamental_hz = value.clamp(30.0, 200.0),
            "harmonics" => {
                let h = value.round().clamp(1.0, MAX_HARMONICS as f32) as usize;
                self.harmonics = h;
            }
            "q" => self.q = value.clamp(5.0, 60.0),
            "harmonic_2_enabled" => self.harmonic_enabled[1] = value > 0.5,
            "harmonic_3_enabled" => self.harmonic_enabled[2] = value > 0.5,
            _ => return,
        }
        self.dirty = true;
    }

    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        if self.dirty {
            self.recompute();
        }
        let channels = ctx.channels as usize;
        if channels == 0 {
            return;
        }
        let frames = buffer.len() / channels;
        for frame in 0..frames {
            for ch in 0..channels.min(MAX_CHANNELS) {
                let idx = frame * channels + ch;
                let mut s = buffer[idx];
                for h in 0..MAX_HARMONICS {
                    if !self.harmonic_active(h) {
                        continue;
                    }
                    s = self.channels[ch][h].process(s);
                }
                buffer[idx] = s;
            }
        }
    }

    fn reset(&mut self) {
        for ch in 0..MAX_CHANNELS {
            for h in 0..MAX_HARMONICS {
                self.channels[ch][h].reset();
            }
        }
    }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        vec![
            ("fundamental_hz".into(), self.fundamental_hz),
            ("harmonics".into(), self.harmonics as f32),
            ("q".into(), self.q),
            (
                "harmonic_2_enabled".into(),
                if self.harmonic_enabled[1] { 1.0 } else { 0.0 },
            ),
            (
                "harmonic_3_enabled".into(),
                if self.harmonic_enabled[2] { 1.0 } else { 0.0 },
            ),
        ]
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (name, value) in params {
            self.set_param(name, *value);
        }
    }

    /// fundamental_hz / harmonics / q rebuild the biquad cascade and
    /// can audibly click on rapid keyframe changes; mark them
    /// structural. The per-harmonic enable toggles are 0/1 step-shape
    /// that the chain's existing sub-chunk smoothing handles fine
    /// (still keyframable).
    fn param_keyframable(&self, name: &str) -> bool {
        !matches!(name, "fundamental_hz" | "harmonics" | "q")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(sr: u32) -> AudioProcessContext {
        AudioProcessContext {
            sample_rate: sr,
            channels: 1,
            clip_time_secs: 0.0,
        }
    }

    #[test]
    fn defaults_are_50hz_5_harmonics_q30() {
        let hr = HumRemoval::new(48000);
        assert_eq!(hr.get_param("fundamental_hz"), Some(50.0));
        assert_eq!(hr.get_param("harmonics"), Some(5.0));
        assert_eq!(hr.get_param("q"), Some(30.0));
    }

    /// Q=30 has a -3 dB bandwidth of ~f0/Q. At f0 ± half-bandwidth the
    /// notch is at ~-3 dB; at the exact centre the response is
    /// theoretically zero but f32 cancellation makes it unreliable to
    /// measure there. We probe at ±0.5 Hz where the cut is still very
    /// deep but the arithmetic is stable.
    fn near_centre_mag(hr: &mut HumRemoval, centre: f32) -> f32 {
        // Average the two flanks — symmetric notch should give equal
        // values, this catches asymmetry too.
        let lo = hr.magnitude_at(centre - 0.5);
        let hi = hr.magnitude_at(centre + 0.5);
        0.5 * (lo + hi)
    }

    #[test]
    fn notch_is_centred_on_fundamental() {
        let mut hr = HumRemoval::new(48000);
        // The notch is deep near 50 Hz and the response climbs out
        // toward the passband on both sides. Use ratio-against-25 Hz
        // (clear passband) to avoid any absolute-magnitude assumption.
        let cut = near_centre_mag(&mut hr, 50.0);
        let pass = hr.magnitude_at(25.0);
        assert!(
            cut < pass * 0.5,
            "near-50 Hz cut={cut:.4} should be < half of 25 Hz pass={pass:.4}"
        );
        // And the cut depth should be at least 6 dB right at the
        // 0.5 Hz flank — generous floor that survives f32 noise.
        assert!(
            cut < 0.5,
            "near-50 Hz cut should be at least -6 dB, got mag={cut:.4}"
        );
    }

    #[test]
    fn notch_is_centred_on_second_harmonic() {
        let mut hr = HumRemoval::new(48000);
        let cut = near_centre_mag(&mut hr, 100.0);
        let pass_lo = hr.magnitude_at(75.0);
        let pass_hi = hr.magnitude_at(125.0);
        assert!(
            cut < pass_lo * 0.7 && cut < pass_hi * 0.7,
            "near-100 Hz cut={cut:.4} should sit well below 75/125 Hz passband ({pass_lo:.4}/{pass_hi:.4})"
        );
    }

    #[test]
    fn notch_is_centred_on_third_harmonic() {
        let mut hr = HumRemoval::new(48000);
        let cut = near_centre_mag(&mut hr, 150.0);
        let pass_lo = hr.magnitude_at(125.0);
        let pass_hi = hr.magnitude_at(175.0);
        assert!(
            cut < pass_lo * 0.7 && cut < pass_hi * 0.7,
            "near-150 Hz cut={cut:.4} should sit well below 125/175 Hz passband ({pass_lo:.4}/{pass_hi:.4})"
        );
    }

    #[test]
    fn passband_flat_at_25hz_offset_below_fundamental() {
        let mut hr = HumRemoval::new(48000);
        // ≤ 0.5 dB ripple at 25 Hz (well below the narrowest notch's
        // ~1.7 Hz -3 dB bandwidth) → mag ≥ 0.944.
        let m = hr.magnitude_at(25.0);
        assert!(
            m > 0.944,
            "expected ≤0.5 dB attenuation at 25 Hz, got mag={m} (≈{:.2} dB)",
            20.0 * m.log10()
        );
    }

    #[test]
    fn passband_flat_at_300hz() {
        let mut hr = HumRemoval::new(48000);
        let m = hr.magnitude_at(300.0);
        assert!(
            m > 0.944,
            "expected ≤0.5 dB attenuation at 300 Hz, got mag={m}"
        );
    }

    #[test]
    fn passband_flat_at_1khz_dialog_band() {
        let mut hr = HumRemoval::new(48000);
        let m = hr.magnitude_at(1000.0);
        assert!(
            m > 0.99,
            "expected ≤0.1 dB attenuation in dialog band, got mag={m}"
        );
    }

    #[test]
    fn switch_to_60hz_moves_notches() {
        let mut hr = HumRemoval::new(48000);
        hr.set_param("fundamental_hz", 60.0);
        // After switching, 60/120/180 Hz should be the cut centres
        // and 50 Hz should be in the passband again.
        let cut_60 = near_centre_mag(&mut hr, 60.0);
        let pass_50 = hr.magnitude_at(50.0);
        assert!(
            cut_60 < pass_50 * 0.5,
            "60 Hz cut={cut_60:.4} should now sit well below 50 Hz pass={pass_50:.4}"
        );
        assert!(
            pass_50 > 0.7,
            "50 Hz should NOT be cut after switching to 60 Hz, got mag={pass_50:.4}"
        );
    }

    #[test]
    fn disabled_harmonic_passes_through() {
        let mut hr = HumRemoval::new(48000);
        hr.set_param("harmonic_3_enabled", 0.0);
        // 150 Hz should now be ~unaffected (no Q=30 notch on it).
        let m = hr.magnitude_at(150.0);
        assert!(
            m > 0.94,
            "150 Hz should pass when 3rd harmonic disabled, got mag={m:.4}"
        );
        // The fundamental + 2nd harmonic notches still bite.
        let cut_50 = near_centre_mag(&mut hr, 50.0);
        let cut_100 = near_centre_mag(&mut hr, 100.0);
        assert!(cut_50 < 0.5, "50 Hz still cut: mag={cut_50:.4}");
        assert!(cut_100 < 0.5, "100 Hz still cut: mag={cut_100:.4}");
    }

    #[test]
    fn harmonics_count_caps_at_5() {
        let mut hr = HumRemoval::new(48000);
        hr.set_param("harmonics", 99.0);
        assert_eq!(hr.get_param("harmonics"), Some(MAX_HARMONICS as f32));
    }

    #[test]
    fn harmonics_count_floor_is_1() {
        let mut hr = HumRemoval::new(48000);
        hr.set_param("harmonics", 0.0);
        assert_eq!(hr.get_param("harmonics"), Some(1.0));
    }

    #[test]
    fn fundamental_hz_clamped_to_30_200() {
        let mut hr = HumRemoval::new(48000);
        hr.set_param("fundamental_hz", 1.0);
        assert_eq!(hr.get_param("fundamental_hz"), Some(30.0));
        hr.set_param("fundamental_hz", 5000.0);
        assert_eq!(hr.get_param("fundamental_hz"), Some(200.0));
    }

    #[test]
    fn serialize_round_trip() {
        let mut a = HumRemoval::new(48000);
        a.set_param("fundamental_hz", 60.0);
        a.set_param("harmonics", 4.0);
        a.set_param("q", 25.0);
        a.set_param("harmonic_2_enabled", 0.0);
        let params = a.serialize_params();
        let mut b = HumRemoval::new(48000);
        b.deserialize_params(&params);
        assert_eq!(b.get_param("fundamental_hz"), Some(60.0));
        assert_eq!(b.get_param("harmonics"), Some(4.0));
        assert_eq!(b.get_param("q"), Some(25.0));
        assert_eq!(b.get_param("harmonic_2_enabled"), Some(0.0));
        assert_eq!(b.get_param("harmonic_3_enabled"), Some(1.0));
    }

    #[test]
    fn process_silence_stays_silence() {
        let mut hr = HumRemoval::new(48000);
        let mut buf = vec![0.0_f32; 4096];
        hr.process(&mut buf, &ctx(48000));
        assert!(buf.iter().all(|s| s.abs() < 1e-9));
    }

    fn rms(buf: &[f32]) -> f32 {
        let sum_sq: f32 = buf.iter().map(|s| s * s).sum();
        (sum_sq / buf.len() as f32).sqrt()
    }

    #[test]
    fn process_attenuates_50hz_sine() {
        // Q=30 settling time τ ≈ Q/(πf0) ≈ 190 ms. Run a 5-second
        // 50 Hz sine and skip the first 1.5 s for transient decay
        // (~7 time constants → impulse < 0.1% of initial). Use RMS
        // not peak — RMS averages out remaining transient ripple.
        let sr = 48000_u32;
        let frames = (sr as usize) * 5;
        let mut buf: Vec<f32> = (0..frames)
            .map(|i| (2.0 * std::f32::consts::PI * 50.0 * i as f32 / sr as f32).sin())
            .collect();
        let input_rms = rms(&buf);
        let mut hr = HumRemoval::new(sr);
        hr.process(&mut buf, &ctx(sr));
        let warmup = (sr as usize) * 3 / 2;
        let output_rms = rms(&buf[warmup..]);
        let attenuation_db = 20.0 * (input_rms / output_rms.max(1e-12)).log10();
        assert!(
            attenuation_db > 30.0,
            "50 Hz sine should attenuate ≥30 dB in steady state, got {attenuation_db:.1} dB (in_rms={input_rms:.4}, out_rms={output_rms:.6})"
        );
    }

    #[test]
    fn process_preserves_1khz_sine() {
        let sr = 48000_u32;
        let frames = sr as usize;
        let mut buf: Vec<f32> = (0..frames)
            .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sr as f32).sin())
            .collect();
        let input_rms = rms(&buf);
        let mut hr = HumRemoval::new(sr);
        hr.process(&mut buf, &ctx(sr));
        let warmup = (sr as usize) / 20;
        let output_rms = rms(&buf[warmup..]);
        let attenuation_db = 20.0 * (input_rms / output_rms.max(1e-12)).log10();
        assert!(
            attenuation_db.abs() < 0.5,
            "1 kHz sine should pass through with ≤0.5 dB change, got {attenuation_db:.2} dB"
        );
    }

    #[test]
    fn structural_params_not_keyframable() {
        let hr = HumRemoval::new(48000);
        assert!(!hr.param_keyframable("fundamental_hz"));
        assert!(!hr.param_keyframable("harmonics"));
        assert!(!hr.param_keyframable("q"));
        // Per-harmonic toggles are 0/1 — chain handles mid-stream
        // toggles via sub-chunk smoothing; keep keyframable.
        assert!(hr.param_keyframable("harmonic_2_enabled"));
        assert!(hr.param_keyframable("harmonic_3_enabled"));
    }

    #[test]
    fn works_at_44_1khz() {
        // Sanity: filter functions at CD rate. The discrete pole
        // position shifts very slightly with sample rate, which
        // pulls the half-Hz flank reading 1-2 % above the 48 kHz
        // value — pin a slightly looser ratio (0.6 vs 0.5) so the
        // assertion isn't on a knife-edge.
        let mut hr = HumRemoval::new(44100);
        let cut = near_centre_mag(&mut hr, 50.0);
        let pass = hr.magnitude_at(25.0);
        assert!(
            cut < pass * 0.6,
            "44.1 kHz: cut at f0±0.5 Hz = {cut:.4}, pass at 25 Hz = {pass:.4}"
        );
        assert!(hr.magnitude_at(1000.0) > 0.99);
    }

    /// #2779: at its defaults, 50 Hz hum and its harmonics up to the 5th
    /// come out, each by more than 10 dB, and a voice tone between them
    /// stays. Measured on each, alone.
    #[test]
    fn the_default_takes_out_the_hum_and_its_harmonics() {
        let level = |hz: f32, fx: &mut HumRemoval| {
            let sr = 48_000u32;
            let mut buf: Vec<f32> = (0..sr as usize).flat_map(|i| {
                let v = 0.1 * (2.0 * std::f32::consts::PI * hz * i as f32 / sr as f32).sin();
                [v, v]
            }).collect();
            fx.reset();
            fx.process(&mut buf, &AudioProcessContext { sample_rate: sr, channels: 2, clip_time_secs: 0.0 });
            let tail = &buf[buf.len() / 2..];
            20.0 * ((tail.iter().map(|s| s * s).sum::<f32>() / tail.len() as f32).sqrt() / (0.1 / 2f32.sqrt())).log10()
        };
        let mut fx = HumRemoval::new(48_000);
        for hz in [50.0, 100.0, 150.0, 200.0, 250.0] {
            let db = level(hz, &mut fx);
            assert!(db < -10.0, "{hz} Hz: {db:+.1} dB");
        }
        let voice = level(1000.0, &mut fx);
        assert!(voice.abs() < 0.5, "1 kHz: {voice:+.2} dB");
    }
}
