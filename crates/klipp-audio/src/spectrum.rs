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

    bins_of_window(&mut mono, sample_rate, num_bins, output);
}

/// One window of mono samples → log-frequency dBFS bins.
///
/// Shared by the live analyser and the whole-file spectrogram on
/// purpose: a person comparing the scrolling display with the offline
/// one of the same file has to be looking at the same measurement, or
/// neither can be trusted. `mono` is windowed in place.
pub fn bins_of_window(mono: &mut [f32], sample_rate: u32, num_bins: usize, output: &mut [f32]) {
    let fft_size = mono.len();
    if fft_size == 0 || num_bins == 0 || sample_rate == 0 {
        for v in output.iter_mut() {
            *v = -96.0;
        }
        return;
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
    let _ = fft.process(mono, &mut spectrum);

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
            assert!((-96.0..=0.0).contains(&v),
                "bin {} out of range: {}", i, v);
        }
    }
}

/// A whole signal as a spectrogram: `slices` columns of `num_bins`
/// log-frequency dBFS values, oldest first.
///
/// The scrolling analyser shows what is playing. That is the right
/// instrument for setting a level and the wrong one for finding the
/// two minutes of a long interview where a hum crept in, or the moment
/// a radio mic started hissing — for that you want the whole file at
/// once, which is what a spectrogram is actually for.
///
/// `slices` is chosen by the caller from the width it has to draw in,
/// so a twenty-minute file and a twenty-second one both cost the same:
/// each column is one hop, and the hop is the signal's length divided
/// by the column count. Where a hop is shorter than the window the
/// windows overlap, which is ordinary; where it is longer the gaps
/// between them are not measured, which is the honest trade for a
/// bounded cost and is why the hop is reported back.
pub fn spectrogram(mono: &[f32], sample_rate: u32, fft_size: usize, slices: usize, num_bins: usize) -> (Vec<Vec<f32>>, f64) {
    if mono.is_empty() || slices == 0 || num_bins == 0 || fft_size == 0 || sample_rate == 0 {
        return (Vec::new(), 0.0);
    }
    let hop = (mono.len() as f64 / slices as f64).max(1.0);
    let mut out = Vec::with_capacity(slices);
    let mut window = vec![0.0f32; fft_size];
    for i in 0..slices {
        // Centre each window on its column's moment, so a transient
        // lands in the column it happened in rather than the one after.
        let centre = (i as f64 + 0.5) * hop;
        let start = (centre - fft_size as f64 / 2.0).round() as i64;
        for (k, w) in window.iter_mut().enumerate() {
            let at = start + k as i64;
            *w = if at >= 0 && (at as usize) < mono.len() { mono[at as usize] } else { 0.0 };
        }
        let mut bins = vec![-96.0f32; num_bins];
        bins_of_window(&mut window, sample_rate, num_bins, &mut bins);
        out.push(bins);
    }
    (out, hop / sample_rate as f64)
}

/// The frequency a log-spaced bin sits at, in Hz — the inverse of the
/// mapping [`bins_of_window`] uses, so a reading can be named.
pub fn bin_hz(bin: usize, num_bins: usize, sample_rate: u32) -> f32 {
    if num_bins == 0 || sample_rate == 0 {
        return 0.0;
    }
    let nyquist = sample_rate as f32 / 2.0;
    let (log_min, log_max) = (20.0f32.log10(), nyquist.max(20.0).log10());
    let t = (bin as f32 + 0.5) / num_bins as f32;
    10.0f32.powf(log_min + (log_max - log_min) * t)
}

#[cfg(test)]
mod whole_file_tests {
    use super::*;

    fn sine(hz: f32, secs: f32, sample_rate: u32, amp: f32) -> Vec<f32> {
        let n = (secs * sample_rate as f32) as usize;
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * hz * i as f32 / sample_rate as f32).sin())
            .collect()
    }

    /// The loudest bin of a column, and the frequency it stands for.
    fn peak_hz(col: &[f32], sample_rate: u32) -> f32 {
        let i = col.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, _)| i).unwrap_or(0);
        bin_hz(i, col.len(), sample_rate)
    }

    #[test]
    fn a_tone_lands_on_its_own_frequency() {
        let sr = 32_000;
        let (spec, _) = spectrogram(&sine(1000.0, 2.0, sr, 0.5), sr, 2048, 20, 96);
        assert_eq!(spec.len(), 20);
        for (i, col) in spec.iter().enumerate() {
            let hz = peak_hz(col, sr);
            assert!((hz - 1000.0).abs() < 120.0, "column {i} peaked at {hz:.0} Hz, not 1 kHz");
        }
    }

    #[test]
    fn a_hum_that_starts_half_way_shows_only_in_the_second_half() {
        // The whole point of the offline view: *when*, not just whether.
        let sr = 32_000;
        let mut signal = vec![0.0f32; sr as usize * 2];
        for (i, s) in sine(50.0, 2.0, sr, 0.4).into_iter().enumerate() {
            signal.push(s + 0.0 * i as f32);
        }
        let (spec, hop) = spectrogram(&signal, sr, 4096, 16, 96);
        assert!((hop - 0.25).abs() < 0.02, "four seconds over sixteen columns: {hop}");
        let hum_bin = (0..96).min_by(|a, b| {
            (bin_hz(*a, 96, sr) - 50.0).abs().total_cmp(&(bin_hz(*b, 96, sr) - 50.0).abs())
        }).unwrap();
        let quiet: f32 = spec[..7].iter().map(|c| c[hum_bin]).sum::<f32>() / 7.0;
        let humming: f32 = spec[9..].iter().map(|c| c[hum_bin]).sum::<f32>() / 7.0;
        assert!(humming > quiet + 25.0, "the hum is found where it is: silent {quiet:.0} dB, humming {humming:.0} dB");
    }

    #[test]
    fn a_long_file_and_a_short_one_cost_the_same() {
        let sr = 16_000;
        let short = sine(440.0, 1.0, sr, 0.3);
        let long = sine(440.0, 600.0, sr, 0.3);
        let (a, hop_a) = spectrogram(&short, sr, 1024, 200, 64);
        let (b, hop_b) = spectrogram(&long, sr, 1024, 200, 64);
        assert_eq!(a.len(), b.len(), "the column count is the caller's, not the file's");
        assert!(hop_b > hop_a * 100.0, "and the hop says what was skipped: {hop_a} against {hop_b}");
    }

    #[test]
    fn silence_and_nonsense_do_not_panic() {
        let sr = 32_000;
        assert!(spectrogram(&[], sr, 1024, 10, 64).0.is_empty());
        assert!(spectrogram(&[0.0; 100], sr, 0, 10, 64).0.is_empty());
        assert!(spectrogram(&[0.0; 100], 0, 1024, 10, 64).0.is_empty());
        let (spec, _) = spectrogram(&[0.0; 4000], sr, 1024, 8, 32);
        assert!(spec.iter().all(|c| c.iter().all(|v| *v <= -90.0)), "silence reads as silence");
    }
}
