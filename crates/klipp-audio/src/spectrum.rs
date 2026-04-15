//! Real-time audio spectrum analyser — FFT + log-frequency bin mapping.
//!
//! Consumes a stereo-interleaved PCM ring buffer, mixes to mono, applies a
//! Hann window, runs a real-FFT, and maps the linear FFT bins into a
//! log-frequency output array in dBFS. Intended to be called once per UI
//! frame (~60 Hz). The realfft planner is created per call; it caches
//! internally and is cheap enough at UI rates.

use realfft::RealFftPlanner;
use std::collections::VecDeque;

/// Compute a log-scaled magnitude spectrum from a stereo-interleaved PCM ring
/// buffer.
///
/// * `ring` — interleaved L,R,L,R,… PCM samples. The most-recent
///   `fft_size` mono frames are used; fewer samples → zero-pad at the start.
/// * `sample_rate` — source sample rate in Hz.
/// * `fft_size` — FFT window length in samples (power of two, e.g. 2048).
/// * `num_bins` — number of log-frequency output bins (`output.len()`).
/// * `output` — pre-allocated slice of length `num_bins`; filled with dBFS
///   values clamped to `-96.0 ..= 0.0`.
pub fn compute_spectrum(
    ring: &VecDeque<f32>,
    sample_rate: u32,
    fft_size: usize,
    num_bins: usize,
    output: &mut [f32],
) {
    assert_eq!(output.len(), num_bins, "output slice must match num_bins");
    if fft_size == 0 || num_bins == 0 || sample_rate == 0 {
        for v in output.iter_mut() { *v = -96.0; }
        return;
    }

    // 1. Pull the most-recent `fft_size` mono frames out of the stereo ring.
    //    Zero-pad at the start when fewer frames are available so a near-empty
    //    ring still returns sane (silent) output.
    let available_frames = ring.len() / 2;
    let copy_frames = available_frames.min(fft_size);
    let mut mono = vec![0.0f32; fft_size];
    let pad = fft_size - copy_frames;
    let ring_start = ring.len().saturating_sub(copy_frames * 2);
    for i in 0..copy_frames {
        let base = ring_start + i * 2;
        let l = ring.get(base).copied().unwrap_or(0.0);
        let r = ring.get(base + 1).copied().unwrap_or(0.0);
        mono[pad + i] = (l + r) * 0.5;
    }

    // 2. Apply Hann window in-place.
    if fft_size > 1 {
        let denom = (fft_size - 1) as f32;
        for (i, s) in mono.iter_mut().enumerate() {
            let w = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / denom).cos());
            *s *= w;
        }
    }

    // 3. FFT via realfft.
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(fft_size);
    let mut spectrum = fft.make_output_vec();
    let _ = fft.process(&mut mono, &mut spectrum);

    // 4. Map linear FFT bins onto `num_bins` log-frequency output bins in
    //    [20 Hz, Nyquist]. For each output bin, RMS-average the FFT magnitudes
    //    that fall into its frequency range, then convert to dBFS (normalised
    //    by `fft_size` and Hann window coherent gain ≈ 0.5 for amplitude).
    let nyquist = sample_rate as f32 / 2.0;
    let log_min = 20.0f32.log10();
    let log_max = nyquist.max(20.0).log10();
    let bins_per_hz = fft_size as f32 / sample_rate as f32;
    let spec_len = spectrum.len();

    // Coherent gain compensation for Hann (≈ 0.5) plus 1/N FFT scale. Together
    // a 0 dBFS sine at FFT-bin-exact frequency produces magnitude ≈ N/2 at
    // that bin → mag / (fft_size * 0.5) ≈ 1.0 → 0 dBFS.
    let norm = (fft_size as f32) * 0.5;

    for bin_i in 0..num_bins {
        let t_lo = bin_i as f32 / num_bins as f32;
        let t_hi = (bin_i + 1) as f32 / num_bins as f32;
        let f_lo = 10.0f32.powf(log_min + (log_max - log_min) * t_lo);
        let f_hi = 10.0f32.powf(log_min + (log_max - log_min) * t_hi);

        let fft_lo = ((f_lo * bins_per_hz) as usize).max(1).min(spec_len);
        let fft_hi = ((f_hi * bins_per_hz) as usize + 1).min(spec_len).max(fft_lo + 1);

        // Peak magnitude across the FFT bins in this range is more perceptually
        // meaningful than RMS for sine-tone detection — a single strong FFT
        // bin within a wide log-bin shouldn't be diluted by its neighbours.
        let mut peak = 0.0f32;
        for c in &spectrum[fft_lo..fft_hi] {
            let m = c.norm();
            if m > peak { peak = m; }
        }

        let db = if peak > 0.0 {
            20.0 * (peak / norm).log10()
        } else {
            -96.0
        };
        output[bin_i] = db.clamp(-96.0, 0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_produces_floor_bins() {
        let ring: VecDeque<f32> = VecDeque::new();
        let mut out = vec![0.0f32; 64];
        compute_spectrum(&ring, 48000, 1024, 64, &mut out);
        assert!(out.iter().all(|&v| v <= -90.0),
            "Silence should produce near-floor bins, got {:?}", out);
    }

    #[test]
    fn sine_wave_peaks_at_correct_bin() {
        use std::f32::consts::PI;
        let sr = 48000u32;
        let freq = 1000.0f32;
        let fft_size = 2048usize;
        let mut ring: VecDeque<f32> = VecDeque::new();
        for i in 0..fft_size {
            let s = (2.0 * PI * freq * i as f32 / sr as f32).sin();
            ring.push_back(s);
            ring.push_back(s);
        }
        let mut out = vec![0.0f32; 128];
        compute_spectrum(&ring, sr, fft_size, 128, &mut out);
        let peak_bin = out.iter().enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        // 1 kHz on a log[20..24k] axis: t = log10(1000/20) / log10(24000/20) ≈ 0.566
        // → bin ≈ 72 of 128, i.e. middle third.
        assert!(peak_bin > 20 && peak_bin < 100,
            "1 kHz peak bin {} out of expected range 20–100", peak_bin);
        assert!(out[peak_bin] > -20.0,
            "1 kHz peak {:.1} dB too weak", out[peak_bin]);
    }

    #[test]
    fn empty_output_slice_asserts() {
        let ring: VecDeque<f32> = VecDeque::new();
        let mut out: Vec<f32> = Vec::new();
        // num_bins=0 → early-return path, not assertion.
        compute_spectrum(&ring, 48000, 1024, 0, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn partial_ring_zero_pads_cleanly() {
        // Only 100 mono frames of silence; fft_size 1024 should still return
        // floor values, no panic.
        let mut ring: VecDeque<f32> = VecDeque::new();
        for _ in 0..100 {
            ring.push_back(0.0);
            ring.push_back(0.0);
        }
        let mut out = vec![0.0f32; 32];
        compute_spectrum(&ring, 48000, 1024, 32, &mut out);
        assert!(out.iter().all(|&v| v <= -90.0));
    }

    #[test]
    fn output_bins_clamped_to_valid_range() {
        use std::f32::consts::PI;
        let sr = 48000u32;
        let fft_size = 2048usize;
        let mut ring: VecDeque<f32> = VecDeque::new();
        // Loud 440 Hz sine at amplitude 1.0 — should read close to 0 dBFS.
        for i in 0..fft_size {
            let s = (2.0 * PI * 440.0 * i as f32 / sr as f32).sin();
            ring.push_back(s);
            ring.push_back(s);
        }
        let mut out = vec![0.0f32; 64];
        compute_spectrum(&ring, sr, fft_size, 64, &mut out);
        for (i, &v) in out.iter().enumerate() {
            assert!(v >= -96.0 && v <= 0.0,
                "bin {} out of range: {}", i, v);
        }
    }
}
