//! Synthesised audio assets used as building blocks for opt-in
//! convenience layers (today: hyperzoom whoosh stinger).
//!
//! # Whoosh contract
//!
//! All variants share an identical envelope shape: a half-cycle
//! sine that starts at 0, peaks at the temporal midpoint of the
//! clip, and returns to 0. The variants differ only in
//! **duration**, **frequency sweep range**, and **noise mix** —
//! never in envelope shape. This is the contract that lets
//! callers align the audio peak to a cut boundary with a single
//! formula: `clip_in = cut_time - whoosh_duration / 2`.
//!
//! Output is 48 kHz mono `f32` in `[-1, 1]`. Peak target ≈ −6 dBFS
//! (0.5 amplitude). Synthesis is deterministic — fixed RNG seed —
//! so tests can pin output bytes and the WAV-cache layer can
//! safely deduplicate by variant.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// Engine-wide native sample rate. Matches the audio mix-thread's
/// internal rate; rendering whooshes at this rate avoids a resample
/// when the clip plays through.
pub const SAMPLE_RATE: u32 = 48_000;

/// Peak amplitude target for synthesised whooshes. ≈ −6 dBFS — leaves
/// 6 dB of headroom above the synth before any user limiter / track
/// gain applies.
const PEAK_TARGET: f32 = 0.5;

/// Three preset durations + frequency profiles. Variant ids match
/// `ApplyHyperzoom.whoosh_variant` u8 codes (0/1/2). Variant 255 =
/// custom sample (handled elsewhere — not synthesised).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhooshVariant {
    Short,
    Medium,
    Long,
}

impl WhooshVariant {
    /// Total duration in seconds. The peak-at-midpoint envelope
    /// guarantees the audio peak lands at `duration_secs / 2`.
    pub const fn duration_secs(self) -> f32 {
        match self {
            WhooshVariant::Short => 0.30,
            WhooshVariant::Medium => 0.50,
            WhooshVariant::Long => 0.80,
        }
    }

    /// Sweep start frequency (Hz). Higher = whippier; lower = bassier.
    pub const fn freq_start_hz(self) -> f32 {
        match self {
            WhooshVariant::Short => 1200.0,
            WhooshVariant::Medium => 800.0,
            WhooshVariant::Long => 600.0,
        }
    }

    /// Sweep end frequency (Hz). The exponential glide from start
    /// to end is the perceptual core of the whoosh.
    pub const fn freq_end_hz(self) -> f32 {
        match self {
            WhooshVariant::Short => 200.0,
            WhooshVariant::Medium => 80.0,
            WhooshVariant::Long => 60.0,
        }
    }

    /// Ratio of broadband noise mixed into the tone (0..1). More
    /// noise = airier / less tonal.
    pub const fn noise_mix(self) -> f32 {
        match self {
            WhooshVariant::Short => 0.05,
            WhooshVariant::Medium => 0.10,
            WhooshVariant::Long => 0.15,
        }
    }

    /// Stable filename stem for the cached WAV — used by callers
    /// that materialise the synth output to disk and reuse across
    /// applies. Pure ASCII so it's safe on every filesystem.
    pub const fn cache_stem(self) -> &'static str {
        match self {
            WhooshVariant::Short => "whoosh-short",
            WhooshVariant::Medium => "whoosh-medium",
            WhooshVariant::Long => "whoosh-long",
        }
    }
}

/// Synthesise one whoosh variant as a mono f32 buffer at
/// [`SAMPLE_RATE`]. Deterministic — calling twice with the same
/// variant produces the same bytes.
pub fn synthesise_whoosh(variant: WhooshVariant) -> Vec<f32> {
    let dur = variant.duration_secs();
    let n_samples = (dur * SAMPLE_RATE as f32) as usize;
    let f_start = variant.freq_start_hz();
    let f_end = variant.freq_end_hz();
    let noise_mix = variant.noise_mix();
    let log_ratio = (f_end / f_start).ln();
    let two_pi = std::f32::consts::TAU;
    let pi = std::f32::consts::PI;
    let sr_f = SAMPLE_RATE as f32;
    // Fixed seed — synthesis must be deterministic. LCG (Numerical
    // Recipes constants) is plenty for noise of this character and
    // costs nothing.
    let mut rng_state: u32 = 0xC0DE_F00D;
    let mut phase = 0.0_f32;
    let mut out = Vec::with_capacity(n_samples);
    for i in 0..n_samples {
        let t = i as f32 / sr_f;
        let t_norm = t / dur;
        // Exponential frequency sweep — log-linear sounds natural to
        // the ear (constant musical-interval glide), unlike a linear
        // ramp which feels lopsided.
        let freq = f_start * (t_norm * log_ratio).exp();
        phase += two_pi * freq / sr_f;
        if phase > two_pi {
            phase -= two_pi;
        }
        let tone = phase.sin();
        rng_state = rng_state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let noise = (rng_state as f32 / u32::MAX as f32) * 2.0 - 1.0;
        // Half-cycle sine envelope: 0 at start + end, 1 at midpoint.
        // Smooth at endpoints (no clicks), peak guaranteed at
        // duration / 2. This is the synth contract — vary
        // freq/duration/noise per variant, never envelope shape.
        let env = (pi * t_norm).sin();
        let mixed = tone * (1.0 - noise_mix) + noise * noise_mix;
        out.push(mixed * env * PEAK_TARGET);
    }
    out
}

/// Render `variant` to disk as a 16-bit PCM WAV at [`SAMPLE_RATE`].
/// Hand-rolled RIFF emitter — adding a wav crate dep just for two
/// chunks isn't worth it. Caller is responsible for parent dir
/// creation.
pub fn write_whoosh_wav(variant: WhooshVariant, path: &Path) -> io::Result<()> {
    let samples = synthesise_whoosh(variant);
    write_wav_pcm16_mono(&samples, path)
}

/// Write a mono f32 buffer as 16-bit PCM WAV. Public so commit 2's
/// cache-warm step can call it with the synth output without a
/// double-synth.
pub fn write_wav_pcm16_mono(samples: &[f32], path: &Path) -> io::Result<()> {
    let f = File::create(path)?;
    let mut w = BufWriter::new(f);
    let n = samples.len() as u32;
    let data_bytes = n * 2; // 16-bit mono
    // RIFF header
    w.write_all(b"RIFF")?;
    w.write_all(&(36u32 + data_bytes).to_le_bytes())?;
    w.write_all(b"WAVE")?;
    // fmt chunk
    w.write_all(b"fmt ")?;
    w.write_all(&16u32.to_le_bytes())?; // chunk size
    w.write_all(&1u16.to_le_bytes())?; // format = PCM
    w.write_all(&1u16.to_le_bytes())?; // channels = 1
    w.write_all(&SAMPLE_RATE.to_le_bytes())?;
    w.write_all(&(SAMPLE_RATE * 2).to_le_bytes())?; // byte rate (1 ch × 2 bytes)
    w.write_all(&2u16.to_le_bytes())?; // block align
    w.write_all(&16u16.to_le_bytes())?; // bits per sample
    // data chunk
    w.write_all(b"data")?;
    w.write_all(&data_bytes.to_le_bytes())?;
    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        let i16_sample = (clamped * 32767.0).round() as i16;
        w.write_all(&i16_sample.to_le_bytes())?;
    }
    w.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expected_sample_count(variant: WhooshVariant) -> usize {
        (variant.duration_secs() * SAMPLE_RATE as f32) as usize
    }

    #[test]
    fn whoosh_short_sample_count() {
        let s = synthesise_whoosh(WhooshVariant::Short);
        assert_eq!(s.len(), expected_sample_count(WhooshVariant::Short));
        assert_eq!(s.len(), 14_400); // 0.30 × 48 000
    }

    #[test]
    fn whoosh_medium_sample_count() {
        let s = synthesise_whoosh(WhooshVariant::Medium);
        assert_eq!(s.len(), 24_000); // 0.50 × 48 000
    }

    #[test]
    fn whoosh_long_sample_count() {
        let s = synthesise_whoosh(WhooshVariant::Long);
        assert_eq!(s.len(), 38_400); // 0.80 × 48 000
    }

    #[test]
    fn whoosh_starts_and_ends_at_zero() {
        // Envelope is sin(π · t_norm); endpoints are exactly 0.
        // Catches accidental envelope changes — discontinuities at
        // clip boundaries cause audible clicks in production.
        for variant in [WhooshVariant::Short, WhooshVariant::Medium, WhooshVariant::Long] {
            let s = synthesise_whoosh(variant);
            assert!(s[0].abs() < 1e-6, "variant {variant:?} starts at {}", s[0]);
            // Last sample's envelope value is sin(π · (n-1)/n · 1/dur · dur) ≈ sin(π · (1 − 1/n)),
            // which is small but non-zero. Close to zero is enough — exact zero would require n+1
            // samples or a different parametrisation.
            assert!(
                s[s.len() - 1].abs() < 0.01,
                "variant {variant:?} last sample {} too far from zero",
                s[s.len() - 1],
            );
        }
    }

    #[test]
    fn whoosh_peak_below_target_plus_headroom() {
        // PEAK_TARGET is 0.5; allow 20% slack for tone+noise
        // alignment. If this fires, the envelope or amplitude
        // formula changed — re-pin or fix.
        for variant in [WhooshVariant::Short, WhooshVariant::Medium, WhooshVariant::Long] {
            let s = synthesise_whoosh(variant);
            let peak = s.iter().copied().fold(0.0_f32, |a, b| a.max(b.abs()));
            assert!(
                peak <= 0.6,
                "variant {variant:?} peak {peak} exceeds budget; got > 0.6",
            );
            // Lower bound — synth output should be audibly present,
            // not silent.
            assert!(
                peak > 0.3,
                "variant {variant:?} peak {peak} suspiciously quiet; expected > 0.3",
            );
        }
    }

    #[test]
    fn whoosh_peak_lands_near_midpoint() {
        // Pin the synth contract: envelope peaks at the temporal
        // midpoint. Callers rely on this for cut-aligned placement
        // (clip_in = cut_time − duration / 2).
        for variant in [WhooshVariant::Short, WhooshVariant::Medium, WhooshVariant::Long] {
            let s = synthesise_whoosh(variant);
            let (peak_idx, _) = s
                .iter()
                .enumerate()
                .map(|(i, v)| (i, v.abs()))
                .fold((0usize, 0.0_f32), |a, b| if b.1 > a.1 { b } else { a });
            let midpoint = s.len() / 2;
            // Within 5% of duration. Tighter than that picks up
            // sample-level noise interference; looser would let an
            // envelope shape change slip through.
            let tolerance = s.len() / 20;
            assert!(
                peak_idx.abs_diff(midpoint) <= tolerance,
                "variant {variant:?} peak at idx {peak_idx}, expected near {midpoint} (tolerance {tolerance})",
            );
        }
    }

    #[test]
    fn whoosh_is_deterministic() {
        // Synth must be deterministic: same variant → byte-identical
        // output. Caches and tests both rely on this.
        for variant in [WhooshVariant::Short, WhooshVariant::Medium, WhooshVariant::Long] {
            let a = synthesise_whoosh(variant);
            let b = synthesise_whoosh(variant);
            assert_eq!(a, b, "variant {variant:?} not deterministic");
        }
    }

    #[test]
    fn whoosh_variants_have_distinct_durations() {
        // Pin: variants differ in duration so the
        // "vary duration / freq / noise only" contract is visible
        // at the type level.
        assert_ne!(
            WhooshVariant::Short.duration_secs(),
            WhooshVariant::Medium.duration_secs()
        );
        assert_ne!(
            WhooshVariant::Medium.duration_secs(),
            WhooshVariant::Long.duration_secs()
        );
    }

    #[test]
    fn cache_stems_are_distinct_and_ascii() {
        let stems = [
            WhooshVariant::Short.cache_stem(),
            WhooshVariant::Medium.cache_stem(),
            WhooshVariant::Long.cache_stem(),
        ];
        for s in stems {
            assert!(s.is_ascii());
            assert!(!s.is_empty());
            assert!(!s.contains('/') && !s.contains('\\'));
        }
        // Distinct
        assert_ne!(stems[0], stems[1]);
        assert_ne!(stems[1], stems[2]);
        assert_ne!(stems[0], stems[2]);
    }

    #[test]
    fn write_wav_produces_valid_riff_header() {
        let dir = tempdir_for_test();
        let path = dir.join("whoosh-test-header.wav");
        write_whoosh_wav(WhooshVariant::Short, &path).expect("write wav");
        let bytes = std::fs::read(&path).expect("read wav");
        // RIFF / WAVE magic
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        // fmt chunk: format = 1 (PCM), channels = 1, sr = 48000,
        // bits = 16
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 16);
        assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 1);
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            SAMPLE_RATE
        );
        assert_eq!(u16::from_le_bytes(bytes[34..36].try_into().unwrap()), 16);
        // data chunk
        assert_eq!(&bytes[36..40], b"data");
        let data_size = u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize;
        assert_eq!(data_size, 14_400 * 2); // short = 14 400 samples × 2 bytes
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn write_wav_data_payload_matches_sample_count() {
        let dir = tempdir_for_test();
        let path = dir.join("whoosh-test-payload.wav");
        write_whoosh_wav(WhooshVariant::Medium, &path).expect("write wav");
        let bytes = std::fs::read(&path).expect("read wav");
        let header_size = 44; // RIFF (12) + fmt (24) + data header (8)
        let data_bytes = bytes.len() - header_size;
        assert_eq!(data_bytes, 24_000 * 2);
        std::fs::remove_file(&path).ok();
    }

    fn tempdir_for_test() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push("klipp-synth-test");
        std::fs::create_dir_all(&p).expect("mk tmp");
        p
    }
}
