//! Tempo and beats from a clip's sound, for cutting to the music.
//!
//! Deliberately light: an energy-flux onset envelope (no FFT), then a
//! normalised autocorrelation over the musical tempo range to find the
//! beat period, a fold toward the 90–180 BPM range to shake off the
//! usual octave error, and a phase search to lay the beat grid down. Good
//! for music with a clear pulse — a drum track, a song — which is exactly
//! the "cut it on the beat" case. Quiet or beatless audio comes back with
//! a low confidence, and callers ignore it.

use std::path::Path;

/// The tempo and beat grid of a piece of sound.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct BeatData {
    /// Estimated tempo in beats per minute.
    pub bpm: f32,
    /// 0 (no pulse) to 1 (a strong, steady beat). Below ~0.3 the audio
    /// probably has no usable beat.
    pub confidence: f32,
    /// Beat times in seconds from the start of the file.
    pub beat_times: Vec<f64>,
}

/// The onset envelope's sample rate: how many analysis frames per second.
fn env_rate(sample_rate: u32, hop: usize) -> f64 {
    sample_rate as f64 / hop as f64
}

/// Build the energy-flux onset envelope: per hop, the rise in RMS energy
/// since the previous hop, half-wave rectified. Peaks sit on note onsets.
fn onset_envelope(pcm: &[f32], hop: usize) -> Vec<f64> {
    let n = pcm.len() / hop;
    let mut env = Vec::with_capacity(n);
    let mut prev = 0.0_f64;
    for i in 0..n {
        let start = i * hop;
        let end = (start + hop).min(pcm.len());
        let frame = &pcm[start..end];
        let energy = (frame.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>() / frame.len().max(1) as f64).sqrt();
        env.push((energy - prev).max(0.0));
        prev = energy;
    }
    env
}

/// The normalised autocorrelation of `env` at `lag` — a Pearson-style
/// score in `0..1` for a non-negative envelope, so it doubles as a
/// confidence.
fn autocorr(env: &[f64], lag: usize) -> f64 {
    if lag == 0 || lag >= env.len() {
        return 0.0;
    }
    let (mut num, mut ea, mut eb) = (0.0, 0.0, 0.0);
    for i in lag..env.len() {
        let (a, b) = (env[i], env[i - lag]);
        num += a * b;
        ea += a * a;
        eb += b * b;
    }
    let denom = (ea * eb).sqrt();
    if denom <= f64::EPSILON {
        0.0
    } else {
        num / denom
    }
}

/// Blur the envelope with a small triangular kernel, widening onset
/// peaks so the autocorrelation is not thrown by a fractional beat period.
fn smooth(env: &mut [f64]) {
    let k = [1.0_f64, 2.0, 3.0, 2.0, 1.0];
    let ksum: f64 = k.iter().sum();
    let src: Vec<f64> = env.to_vec();
    let n = src.len() as isize;
    for (i, out) in env.iter_mut().enumerate() {
        let mut acc = 0.0;
        for (j, &w) in k.iter().enumerate() {
            let idx = i as isize + j as isize - 2;
            if idx >= 0 && idx < n {
                acc += src[idx as usize] * w;
            }
        }
        *out = acc / ksum;
    }
}

/// Tempo and beats from mono `f32` samples at `sample_rate`.
pub fn detect_from_pcm(pcm: &[f32], sample_rate: u32) -> BeatData {
    const HOP: usize = 256;
    let empty = BeatData { bpm: 0.0, confidence: 0.0, beat_times: Vec::new() };
    if pcm.len() < sample_rate as usize {
        return empty;
    }
    let mut env = onset_envelope(pcm, HOP);
    if env.len() < 8 {
        return empty;
    }
    // Centre the envelope so the autocorrelation measures pulse, not level.
    let mean = env.iter().sum::<f64>() / env.len() as f64;
    for v in env.iter_mut() {
        *v = (*v - mean).max(0.0);
    }
    // Widen the onset peaks a touch (a triangular blur) so the
    // autocorrelation tolerates a beat period that falls between frames —
    // and so the envelope resembles a real bass-drum hit, not a spike.
    smooth(&mut env);
    let rate = env_rate(sample_rate, HOP);

    // Search the musical range; the peak autocorrelation is the period.
    let (bpm_lo, bpm_hi) = (60.0_f64, 200.0_f64);
    let lag_min = ((60.0 / bpm_hi) * rate).round() as usize;
    let lag_max = (((60.0 / bpm_lo) * rate).round() as usize).min(env.len() / 2);
    if lag_max <= lag_min {
        return empty;
    }
    let (mut best_lag, mut best) = (lag_min, f64::MIN);
    for lag in lag_min..=lag_max {
        let s = autocorr(&env, lag);
        if s > best {
            best = s;
            best_lag = lag;
        }
    }
    let mut period = best_lag as f64 / rate;
    let mut bpm = 60.0 / period;
    // Fold octave errors toward 90–180 BPM when the alternative is nearly
    // as strong (a half-time read of a 128 BPM song scores highly too).
    let fold = |factor: f64, cur_bpm: f64| -> Option<(f64, f64)> {
        let cand_bpm = cur_bpm * factor;
        if cand_bpm < bpm_lo || cand_bpm > bpm_hi {
            return None;
        }
        let cand_lag = ((60.0 / cand_bpm) * rate).round() as usize;
        let s = autocorr(&env, cand_lag);
        (s > best * 0.8).then_some((cand_bpm, cand_lag as f64 / rate))
    };
    if bpm < 90.0 {
        if let Some((b, p)) = fold(2.0, bpm) {
            bpm = b;
            period = p;
        }
    } else if bpm > 180.0 {
        if let Some((b, p)) = fold(0.5, bpm) {
            bpm = b;
            period = p;
        }
    }
    let confidence = best.clamp(0.0, 1.0) as f32;

    // Phase: slide the beat grid over one period and keep the offset that
    // lands the most onset energy on the beats.
    let period_frames = (period * rate).max(1.0);
    let steps = 24usize;
    let (mut best_phase, mut best_phase_score) = (0.0_f64, f64::MIN);
    for k in 0..steps {
        let phase = k as f64 / steps as f64 * period_frames;
        let mut score = 0.0;
        let mut pos = phase;
        while (pos as usize) < env.len() {
            score += env[pos as usize];
            pos += period_frames;
        }
        if score > best_phase_score {
            best_phase_score = score;
            best_phase = phase;
        }
    }
    let mut beat_times = Vec::new();
    let total_frames = env.len() as f64;
    let mut pos = best_phase;
    while pos < total_frames {
        beat_times.push(pos / rate);
        pos += period_frames;
    }

    BeatData { bpm: bpm as f32, confidence, beat_times }
}

/// Tempo and beats from a media file, decoded through ffmpeg. Reads up to
/// `max_secs` (0 for the default cap) from the start.
/// The store's name and schema version for a file's beat read.
pub const CACHE_ID: &str = "beats";
pub const CACHE_VERSION: u32 = 1;

/// A file's beats as read before, if they were: kept in the analysis store
/// beside its other analyses, keyed by the file, so reading them again
/// costs nothing (#785 review: the next-step hint asks on every apply).
pub fn cached(path: &Path) -> Option<BeatData> {
    let key = crate::store::key_for(path)?;
    serde_json::from_value(crate::store::load(&key, CACHE_ID, CACHE_VERSION)?).ok()
}

/// Reads `path`'s beats with ffmpeg and keeps them in the analysis store
/// for [`cached`]. Slow: a whole decode of the opening minutes.
pub fn read_and_cache(ffmpeg: &Path, path: &Path) -> Result<BeatData, String> {
    let beats = detect(ffmpeg, path, 0.0)?;
    if let Some(key) = crate::store::key_for(path) {
        let value = serde_json::to_value(&beats).map_err(|e| e.to_string())?;
        crate::store::save(&key, CACHE_ID, CACHE_VERSION, &value)?;
    }
    Ok(beats)
}

pub fn detect(ffmpeg: &Path, path: &Path, max_secs: f64) -> Result<BeatData, String> {
    const SR: u32 = 22_050;
    let cap = if max_secs > 0.0 { max_secs } else { 180.0 };
    let pcm = crate::audio_sync::decode_mono_pcm(ffmpeg, path, SR, 0.0, cap)?;
    Ok(detect_from_pcm(&pcm, SR))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic click every 0.5 s is 120 BPM, and the detector should
    /// land on it with the beats half a second apart.
    #[test]
    fn a_120_bpm_click_reads_as_120() {
        let sr = 22_050u32;
        let period = 0.5_f64; // 120 BPM
        let mut pcm = vec![0f32; (sr as f64 * 8.0) as usize];
        // A short 1 kHz burst at each beat.
        let mut t = 0.2;
        while t < 8.0 {
            let start = (t * sr as f64) as usize;
            for i in 0..(0.03 * sr as f64) as usize {
                if start + i < pcm.len() {
                    let ph = (start + i) as f32 / sr as f32;
                    pcm[start + i] = (2.0 * std::f32::consts::PI * 1000.0 * ph).sin() * 0.8;
                }
            }
            t += period;
        }
        let b = detect_from_pcm(&pcm, sr);
        assert!((b.bpm - 120.0).abs() < 6.0, "expected ~120 BPM, got {}", b.bpm);
        assert!(b.confidence > 0.4, "a clear click should be confident, got {}", b.confidence);
        assert!(b.beat_times.len() >= 12, "beats laid across the clip: {}", b.beat_times.len());
        // Consecutive beats about half a second apart.
        let gap = b.beat_times[5] - b.beat_times[4];
        assert!((gap - 0.5).abs() < 0.05, "beat spacing {gap}");
    }

    /// Near-silence has no beat: low confidence, no crash.
    #[test]
    fn quiet_audio_has_no_beat() {
        let sr = 22_050u32;
        let pcm = vec![0.0001f32; sr as usize * 4];
        let b = detect_from_pcm(&pcm, sr);
        assert!(b.confidence < 0.5, "quiet audio is not confident: {}", b.confidence);
    }
}
