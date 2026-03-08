/// Generate a waveform from f32 interleaved stereo PCM samples.
/// Returns `Vec<(min, max)>` — one entry per pixel column at the requested width.
pub fn generate_waveform_from_samples(
    samples: &[f32],
    channels: usize,
    width_pixels: usize,
) -> Vec<(f32, f32)> {
    if samples.is_empty() || width_pixels == 0 {
        return vec![(0.0, 0.0); width_pixels];
    }

    let mono_len = samples.len() / channels.max(1);
    let samples_per_pixel = (mono_len / width_pixels).max(1);
    let mut waveform = Vec::with_capacity(width_pixels);

    for pixel in 0..width_pixels {
        let start = pixel * samples_per_pixel;
        let end = ((pixel + 1) * samples_per_pixel).min(mono_len);
        let mut min = 0.0f32;
        let mut max = 0.0f32;
        for i in start..end {
            let s = samples[i * channels]; // first channel
            min = min.min(s);
            max = max.max(s);
        }
        waveform.push((min, max));
    }

    waveform
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_samples() {
        let result = generate_waveform_from_samples(&[], 2, 100);
        assert_eq!(result.len(), 100);
        assert!(result.iter().all(|&(min, max)| min == 0.0 && max == 0.0));
    }

    #[test]
    fn zero_width() {
        let samples = vec![0.5, -0.5, 0.3, -0.3];
        let result = generate_waveform_from_samples(&samples, 2, 0);
        assert!(result.is_empty());
    }

    #[test]
    fn sine_wave_stereo() {
        // Generate a simple stereo signal: left channel has a sine-ish pattern
        let mut samples = Vec::new();
        for i in 0..1000 {
            let t = i as f32 / 1000.0 * std::f32::consts::TAU;
            let l = t.sin();
            let r = t.cos();
            samples.push(l);
            samples.push(r);
        }
        let result = generate_waveform_from_samples(&samples, 2, 10);
        assert_eq!(result.len(), 10);
        // The waveform should have some non-zero ranges
        assert!(result.iter().any(|&(min, max)| min < 0.0 || max > 0.0));
    }

    #[test]
    fn mono_signal() {
        let samples = vec![0.5, -0.3, 0.8, -0.9, 0.1];
        let result = generate_waveform_from_samples(&samples, 1, 2);
        assert_eq!(result.len(), 2);
        // First bin: samples 0,1 → min=-0.3, max=0.5
        assert!((result[0].0 - (-0.3)).abs() < 0.001);
        assert!((result[0].1 - 0.5).abs() < 0.001);
    }

    #[test]
    fn width_larger_than_samples() {
        let samples = vec![0.5, -0.5]; // 1 mono sample in stereo
        let result = generate_waveform_from_samples(&samples, 2, 100);
        assert_eq!(result.len(), 100);
    }

    #[test]
    fn peak_detection() {
        // All zeros except one spike
        let mut samples = vec![0.0f32; 200]; // 100 stereo frames
        samples[50] = 0.9; // spike at frame 25 left channel
        let result = generate_waveform_from_samples(&samples, 2, 10);
        assert_eq!(result.len(), 10);
        // At least one bin should have max near 0.9
        assert!(result.iter().any(|&(_, max)| max > 0.8));
    }
}
