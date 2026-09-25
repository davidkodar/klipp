use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};
use realfft::{num_complex::Complex32, RealFftPlanner};

const FFT_SIZE: usize = 1024;
const PROFILE_BINS: usize = FFT_SIZE / 2 + 1;

/// Broadband spectral denoiser. The first `profile_seconds` of a clip are
/// learned as a noise profile, then spectral subtraction is applied to each
/// analysis block. The profile is deliberately kept in the effect instance
/// so playback and export share the same deterministic state model.
pub struct Denoiser {
    sample_rate: f32,
    strength: f32,
    floor: f32,
    profile_seconds: f32,
    profile: Vec<f32>,
    learned_frames: usize,
    profile_blocks: usize,
    learn_requested: bool,
    fft: std::sync::Arc<dyn realfft::RealToComplex<f32>>,
    inverse: std::sync::Arc<dyn realfft::ComplexToReal<f32>>,
    spectrum: Vec<Complex32>,
    input: Vec<f32>,
    mono_scratch: Vec<f32>,
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "strength", label: "Reduction", default: 0.75, min: 0.0, max: 1.0, unit: ParamUnit::Percent },
    AudioParamDescriptor { name: "floor", label: "Noise Floor", default: 0.08, min: 0.0, max: 1.0, unit: ParamUnit::Percent },
    AudioParamDescriptor { name: "profile_seconds", label: "Learn Window (s)", default: 0.5, min: 0.1, max: 5.0, unit: ParamUnit::None },
    AudioParamDescriptor { name: "learn", label: "Learn Profile", default: 0.0, min: 0.0, max: 1.0, unit: ParamUnit::None },
];

impl Denoiser {
    pub fn new(sample_rate: u32) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let inverse = planner.plan_fft_inverse(FFT_SIZE);
        Self {
            sample_rate: sample_rate as f32,
            strength: 0.75,
            floor: 0.08,
            profile_seconds: 0.5,
            profile: vec![0.0; PROFILE_BINS],
            learned_frames: 0,
            profile_blocks: 0,
            learn_requested: false,
            spectrum: fft.make_output_vec(),
            input: vec![0.0; FFT_SIZE],
            mono_scratch: Vec::new(),
            fft,
            inverse,
        }
    }

    fn learn_limit(&self) -> usize { (self.profile_seconds * self.sample_rate) as usize }

    fn process_mono(&mut self, frames: usize, ctx: &AudioProcessContext) {
        let learn_limit = self.learn_limit();
        for block in self.mono_scratch[..frames].chunks_mut(FFT_SIZE) {
            self.input.fill(0.0);
            self.input[..block.len()].copy_from_slice(block);
            if self.fft.process(&mut self.input, &mut self.spectrum).is_err() { continue; }
            let frames = block.len();
            let learning = self.learn_requested || self.learned_frames < learn_limit;
            for (bin, value) in self.spectrum.iter_mut().enumerate() {
                let magnitude = value.norm();
                if learning {
                    let prior = self.profile[bin];
                    let count = (self.profile_blocks + 1) as f32;
                    self.profile[bin] = prior + (magnitude - prior) / count;
                } else {
                    let noise = self.profile[bin];
                    let clean = (magnitude * magnitude - noise * noise * self.strength).max(0.0).sqrt();
                    let gain = (clean / magnitude.max(1e-8)).max(self.floor).min(1.0);
                    *value *= gain;
                }
            }
            if self.inverse.process(&mut self.spectrum, &mut self.input).is_err() { continue; }
            let scale = 1.0 / FFT_SIZE as f32;
            for (dst, src) in block.iter_mut().zip(self.input.iter()) { *dst = *src * scale; }
            self.learned_frames = self.learned_frames.saturating_add(frames);
            if learning {
                self.profile_blocks = self.profile_blocks.saturating_add(1);
                if self.learned_frames >= learn_limit { self.learn_requested = false; }
            }
        }
        let _ = ctx;
    }
}

impl AudioEffect for Denoiser {
    fn type_id(&self) -> &'static str { "denoiser" }
    fn name(&self) -> &'static str { "Broadband Denoiser" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { PARAM_DESCS }
    fn get_param(&self, name: &str) -> Option<f32> {
        match name { "strength" => Some(self.strength), "floor" => Some(self.floor), "profile_seconds" => Some(self.profile_seconds), _ => None }
    }
    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "strength" => self.strength = value.clamp(0.0, 1.0),
            "floor" => self.floor = value.clamp(0.0, 1.0),
            "profile_seconds" => { self.profile_seconds = value.clamp(0.1, 5.0); self.reset(); },
            "learn" if value > 0.5 => { self.reset(); self.learn_requested = true; },
            _ => {}
        }
    }
    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        let channels = ctx.channels as usize;
        if channels == 0 { return; }
        for ch in 0..channels.min(2) {
            let frames = buffer.len() / channels;
            self.mono_scratch.resize(frames, 0.0);
            for i in 0..frames { self.mono_scratch[i] = buffer[i * channels + ch]; }
            self.process_mono(frames, ctx);
            for i in 0..frames { buffer[i * channels + ch] = self.mono_scratch[i]; }
        }
    }
    fn reset(&mut self) { self.profile.fill(0.0); self.learned_frames = 0; self.profile_blocks = 0; self.learn_requested = false; }
    fn serialize_params(&self) -> Vec<(String, f32)> {
        vec![("strength".into(), self.strength), ("floor".into(), self.floor), ("profile_seconds".into(), self.profile_seconds)]
    }
    fn deserialize_params(&mut self, params: &[(String, f32)]) { for (k, v) in params { self.set_param(k, *v); } }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> AudioProcessContext {
        AudioProcessContext { sample_rate: 48_000, channels: 1, clip_time_secs: 0.0 }
    }

    #[test]
    fn learns_profile_and_reduces_steady_noise() {
        let mut fx = Denoiser::new(48_000);
        fx.set_param("profile_seconds", 0.1);
        let mut profile = vec![0.0f32; FFT_SIZE * 8];
        for (i, sample) in profile.iter_mut().enumerate() {
            *sample = (2.0 * std::f32::consts::PI * 180.0 * i as f32 / 48_000.0).sin() * 0.2;
        }
        fx.process(&mut profile, &ctx());
        let mut mixed = vec![0.0f32; FFT_SIZE * 8];
        for (i, sample) in mixed.iter_mut().enumerate() {
            let t = i as f32 / 48_000.0;
            *sample = (2.0 * std::f32::consts::PI * 180.0 * t).sin() * 0.2
                + (2.0 * std::f32::consts::PI * 1000.0 * t).sin() * 0.5;
        }
        let before = mixed.iter().map(|v| v * v).sum::<f32>();
        fx.process(&mut mixed, &ctx());
        let after = mixed.iter().map(|v| v * v).sum::<f32>();
        assert!(after < before, "denoiser should reduce learned broadband component");
        assert!(mixed.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn learn_button_restarts_profile() {
        let mut fx = Denoiser::new(48_000);
        let mut input = vec![0.1; FFT_SIZE * 4];
        fx.process(&mut input, &ctx());
        assert!(fx.learned_frames > 0);
        fx.set_param("learn", 1.0);
        assert_eq!(fx.learned_frames, 0);
        assert!(fx.learn_requested);
    }
}
