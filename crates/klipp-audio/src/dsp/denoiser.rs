use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};
use realfft::{num_complex::Complex32, ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

const FFT_SIZE: usize = 1024;
/// 75 % overlap. Hann analysis and synthesis windows at this hop sum to a
/// constant, so an untouched spectrum reconstructs the input exactly.
const HOP: usize = FFT_SIZE / 4;
const BINS: usize = FFT_SIZE / 2 + 1;
/// Write position a channel's input FIFO restarts at after each frame.
const FIFO_START: usize = FFT_SIZE - HOP;
/// Samples between a sample entering the effect and leaving it (21 ms at
/// 48 kHz), reported through `latency_samples` for delay compensation.
const LATENCY: usize = FFT_SIZE;
/// Sum of the squared Hann window over the overlapping frames at `HOP`.
const OLA_GAIN: f32 = 1.5;
const MAX_CHANNELS: usize = 2;

/// Number of log-spaced bands a learned noise profile is stored in. The
/// profile lives in the clip's effect parameters (`noise_band_00` ..), so
/// it survives chain rebuilds, seeks, saves and export.
pub const PROFILE_BANDS: usize = 32;
const BAND_LOW_HZ: f32 = 50.0;
const BAND_HIGH_HZ: f32 = 16_000.0;
const BAND_PREFIX: &str = "noise_band_";

/// Adaptive noise tracking: the noise floor of each bin is the minimum of
/// its smoothed power over roughly one second (12 sub-windows of 16 hops
/// at 48 kHz). Speech and gusts rise above that floor; steady noise is it.
const SUBWINDOW_FRAMES: usize = 16;
const SUBWINDOWS: usize = 12;
const POWER_SMOOTHING: f32 = 0.8;
/// The minimum of a smoothed periodogram sits below the mean noise power.
const MIN_BIAS: f32 = 1.8;

/// Wind stage: wind is turbulence at each microphone, so it is incoherent
/// between the two channels while real sound reaches both coherently. The
/// stage works below `WIND_FULL_HZ`, fading out by `WIND_EDGE_HZ`.
const WIND_FULL_HZ: f32 = 800.0;
const WIND_EDGE_HZ: f32 = 2500.0;
const COHERENCE_SMOOTHING: f32 = 0.7;
/// Coherence below `WIND_COH_LOW` reads as pure wind, above
/// `WIND_COH_HIGH` as pure sound.
const WIND_COH_LOW: f32 = 0.2;
const WIND_COH_HIGH: f32 = 0.8;
/// Gust detector: wind piles energy into the low end without the mid-band
/// harmonics a voice brings. When the power below `GUST_SPLIT_HZ` rises
/// above the power from there to `GUST_MID_TOP_HZ`, the low bins are
/// ducked back to `GUST_TARGET_DB` (fading out by `GUST_EDGE_HZ`). Phones sum their
/// microphones' low end, so this is the stage that catches their wind.
const GUST_LOW_HZ: f32 = 20.0;
const GUST_SPLIT_HZ: f32 = 400.0;
const GUST_EDGE_HZ: f32 = 800.0;
const GUST_MID_TOP_HZ: f32 = 4000.0;
const GUST_MAX_DUCK_DB: f32 = 30.0;
/// Low-over-mid tilt the gust detector ducks down to. Calm outdoor sound
/// and speech sit below it; a gust pushes the low end well above.
const GUST_TARGET_DB: f32 = -3.0;

/// Broadband spectral denoiser.
///
/// A short-time Fourier transform (1024-point Hann, 75 % overlap) splits
/// each channel into bins; every bin is attenuated by how far its power
/// sits above the noise estimate. The noise estimate is either a profile
/// learned from a noise-only section of the source (stored as parameters)
/// or, without one, a per-bin minimum tracked over the last second, so the
/// effect cleans steady noise as soon as it is added.
///
/// On stereo material a wind stage also attenuates low-frequency bins
/// whose left/right coherence is low: gusts on a camera's or phone's
/// microphone pair are uncorrelated, the voice in front of them is not.
pub struct Denoiser {
    sample_rate: f32,
    strength: f32,
    floor: f32,
    profile_seconds: f32,
    wind: f32,
    /// Learned profile as noise power per band in dB, when one is set.
    profile_bands: [f32; PROFILE_BANDS],
    profile_learned: bool,
    /// Where in the source the profile was learned, in seconds (display).
    profile_at: f32,
    /// `profile_bands` interpolated to FFT bins, rebuilt lazily.
    learned_noise: Vec<f32>,
    learned_noise_dirty: bool,
    fft: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    window: Vec<f32>,
    frame: Vec<f32>,
    spectra: [Vec<Complex32>; MAX_CHANNELS],
    gain_scratch: Vec<f32>,
    wind_gain: Vec<f32>,
    /// Smoothed auto- and cross-spectra of the two channels.
    cross_ll: Vec<f32>,
    cross_rr: Vec<f32>,
    cross_lr: Vec<Complex32>,
    /// Smoothed low-over-mid tilt in dB for the gust detector.
    gust_tilt_db: f32,
    channels: [ChannelState; MAX_CHANNELS],
}

struct ChannelState {
    input: Vec<f32>,
    output: Vec<f32>,
    accum: Vec<f32>,
    pos: usize,
    smoothed: Vec<f32>,
    primed: bool,
    sub_min: Vec<f32>,
    history: Vec<Vec<f32>>,
    history_next: usize,
    sub_count: usize,
    gain: Vec<f32>,
}

impl ChannelState {
    fn new() -> Self {
        Self {
            input: vec![0.0; FFT_SIZE],
            output: vec![0.0; FFT_SIZE],
            accum: vec![0.0; FFT_SIZE],
            pos: FIFO_START,
            smoothed: vec![0.0; BINS],
            primed: false,
            sub_min: vec![f32::INFINITY; BINS],
            history: vec![vec![f32::INFINITY; BINS]; SUBWINDOWS],
            history_next: 0,
            sub_count: 0,
            gain: vec![1.0; BINS],
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "strength", label: "Reduction", default: 0.75, min: 0.0, max: 1.0, unit: ParamUnit::Percent },
    AudioParamDescriptor { name: "floor", label: "Noise Floor", default: 0.08, min: 0.0, max: 1.0, unit: ParamUnit::Percent },
    AudioParamDescriptor { name: "wind", label: "Wind Reduction", default: 0.0, min: 0.0, max: 1.0, unit: ParamUnit::Percent },
    AudioParamDescriptor { name: "profile_seconds", label: "Learn Window (s)", default: 0.5, min: 0.1, max: 5.0, unit: ParamUnit::None },
];

/// Parameter name of one stored profile band.
pub fn band_param_name(band: usize) -> String {
    format!("{BAND_PREFIX}{band:02}")
}

fn band_centre_hz(band: usize) -> f32 {
    let t = band as f32 / (PROFILE_BANDS - 1) as f32;
    BAND_LOW_HZ * (BAND_HIGH_HZ / BAND_LOW_HZ).powf(t)
}

fn bin_hz(bin: usize, sample_rate: f32) -> f32 {
    bin as f32 * sample_rate / FFT_SIZE as f32
}

/// The band whose centre is nearest `hz` on a log scale.
fn nearest_band(hz: f32) -> usize {
    if hz <= BAND_LOW_HZ {
        return 0;
    }
    let t = (hz / BAND_LOW_HZ).ln() / (BAND_HIGH_HZ / BAND_LOW_HZ).ln();
    ((t * (PROFILE_BANDS - 1) as f32).round() as usize).min(PROFILE_BANDS - 1)
}

fn hann_window() -> Vec<f32> {
    (0..FFT_SIZE)
        .map(|n| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * n as f32 / FFT_SIZE as f32).cos())
        .collect()
}

/// Learn a noise profile from a noise-only stretch of mono audio. Returns
/// the noise power per band in dB, in the form the effect stores as its
/// `noise_band_NN` parameters, or `None` when the audio is shorter than one
/// analysis frame or digitally silent.
pub fn learn_noise_profile(mono: &[f32], sample_rate: u32) -> Option<Vec<f32>> {
    if mono.len() < FFT_SIZE {
        return None;
    }
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let window = hann_window();
    let mut frame = vec![0.0f32; FFT_SIZE];
    let mut spectrum = fft.make_output_vec();
    let mut power = vec![0.0f64; BINS];
    let mut frames = 0usize;
    let mut start = 0;
    while start + FFT_SIZE <= mono.len() {
        for (i, dst) in frame.iter_mut().enumerate() {
            *dst = mono[start + i] * window[i];
        }
        if fft.process(&mut frame, &mut spectrum).is_ok() {
            for (bin, value) in spectrum.iter().enumerate() {
                power[bin] += value.norm_sqr() as f64;
            }
            frames += 1;
        }
        start += HOP;
    }
    if frames == 0 || power.iter().all(|p| *p <= 0.0) {
        return None;
    }
    let sr = sample_rate as f32;
    let mut sums = [0.0f64; PROFILE_BANDS];
    let mut counts = [0usize; PROFILE_BANDS];
    for (bin, p) in power.iter().enumerate().skip(1) {
        let band = nearest_band(bin_hz(bin, sr));
        sums[band] += p / frames as f64;
        counts[band] += 1;
    }
    let bands = (0..PROFILE_BANDS)
        .map(|band| {
            let mean = if counts[band] > 0 {
                sums[band] / counts[band] as f64
            } else {
                // Low bands narrower than a bin borrow the nearest bin.
                let bin = ((band_centre_hz(band) * FFT_SIZE as f32 / sr).round() as usize).clamp(1, BINS - 1);
                power[bin] / frames as f64
            };
            (10.0 * (mean + 1e-20).log10()) as f32
        })
        .collect();
    Some(bands)
}

/// Start (in samples) of the quietest `window_secs` stretch of `mono`,
/// ignoring digital silence, for learning a profile without the user
/// having to find the noise-only section themselves.
pub fn quietest_window(mono: &[f32], sample_rate: u32, window_secs: f32) -> Option<usize> {
    let window = ((window_secs.max(0.0) * sample_rate as f32) as usize).max(FFT_SIZE);
    if mono.len() < FFT_SIZE {
        return None;
    }
    if mono.len() <= window {
        return Some(0);
    }
    let mut prefix = Vec::with_capacity(mono.len() + 1);
    prefix.push(0.0f64);
    let mut acc = 0.0f64;
    for s in mono {
        acc += (*s as f64) * (*s as f64);
        prefix.push(acc);
    }
    let step = (sample_rate as usize / 20).max(1);
    let mut best: Option<(usize, f64)> = None;
    let mut start = 0;
    while start + window <= mono.len() {
        let mean = (prefix[start + window] - prefix[start]) / window as f64;
        if mean > 1e-12 && best.is_none_or(|(_, b)| mean < b) {
            best = Some((start, mean));
        }
        start += step;
    }
    best.map(|(start, _)| start)
}

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
            wind: 0.0,
            profile_bands: [-200.0; PROFILE_BANDS],
            profile_learned: false,
            profile_at: -1.0,
            learned_noise: vec![0.0; BINS],
            learned_noise_dirty: true,
            spectra: [fft.make_output_vec(), fft.make_output_vec()],
            frame: vec![0.0; FFT_SIZE],
            window: hann_window(),
            gain_scratch: vec![1.0; BINS],
            wind_gain: vec![1.0; BINS],
            cross_ll: vec![0.0; BINS],
            cross_rr: vec![0.0; BINS],
            cross_lr: vec![Complex32::new(0.0, 0.0); BINS],
            gust_tilt_db: f32::NEG_INFINITY,
            channels: [ChannelState::new(), ChannelState::new()],
            fft,
            inverse,
        }
    }

    /// Whether a learned profile drives the noise estimate (otherwise the
    /// effect tracks the noise floor adaptively).
    pub fn has_learned_profile(&self) -> bool {
        self.profile_learned
    }

    fn rebuild_learned_noise(&mut self) {
        let centres: Vec<f32> = (0..PROFILE_BANDS).map(|b| band_centre_hz(b).ln()).collect();
        for bin in 0..BINS {
            let hz = bin_hz(bin, self.sample_rate).max(1.0).ln();
            let db = if hz <= centres[0] {
                self.profile_bands[0]
            } else if hz >= centres[PROFILE_BANDS - 1] {
                self.profile_bands[PROFILE_BANDS - 1]
            } else {
                let upper = centres.iter().position(|c| *c >= hz).unwrap_or(PROFILE_BANDS - 1).max(1);
                let lower = upper - 1;
                let t = (hz - centres[lower]) / (centres[upper] - centres[lower]);
                self.profile_bands[lower] + (self.profile_bands[upper] - self.profile_bands[lower]) * t
            };
            self.learned_noise[bin] = 10f32.powf(db / 10.0);
        }
        self.learned_noise_dirty = false;
    }

    /// Per-bin gain of the wind stage from the two channels' coherence.
    fn update_wind_gain(&mut self) {
        let [left, right] = &self.spectra;
        for bin in 0..BINS {
            let (l, r) = (left[bin], right[bin]);
            let a = COHERENCE_SMOOTHING;
            self.cross_ll[bin] = a * self.cross_ll[bin] + (1.0 - a) * l.norm_sqr();
            self.cross_rr[bin] = a * self.cross_rr[bin] + (1.0 - a) * r.norm_sqr();
            self.cross_lr[bin] = self.cross_lr[bin] * a + l * r.conj() * (1.0 - a);
        }
        for bin in 0..BINS {
            let hz = bin_hz(bin, self.sample_rate);
            let weight = if hz <= WIND_FULL_HZ {
                1.0
            } else if hz >= WIND_EDGE_HZ {
                0.0
            } else {
                1.0 - (hz - WIND_FULL_HZ) / (WIND_EDGE_HZ - WIND_FULL_HZ)
            };
            if weight <= 0.0 {
                self.wind_gain[bin] = 1.0;
                continue;
            }
            // Average the spectra over neighbouring bins: coherence from a
            // handful of frames alone is biased towards 1.
            let lo = bin.saturating_sub(2);
            let hi = (bin + 2).min(BINS - 1);
            let (mut ll, mut rr, mut lr) = (0.0f32, 0.0f32, Complex32::new(0.0, 0.0));
            for k in lo..=hi {
                ll += self.cross_ll[k];
                rr += self.cross_rr[k];
                lr += self.cross_lr[k];
            }
            let coherence = if ll * rr > 1e-20 { (lr.norm_sqr() / (ll * rr)).min(1.0) } else { 1.0 };
            let pass = ((coherence - WIND_COH_LOW) / (WIND_COH_HIGH - WIND_COH_LOW)).clamp(0.0, 1.0).max(self.floor);
            self.wind_gain[bin] = 1.0 - self.wind * weight * (1.0 - pass);
        }
    }

    /// Duck the low bins while the low end towers over the mid band.
    fn apply_gust_gain(&mut self, channels: usize, ok: &[bool; MAX_CHANNELS]) {
        let (mut low, mut mid) = (0.0f32, 0.0f32);
        for ch in (0..channels).filter(|ch| ok[*ch]) {
            for (bin, value) in self.spectra[ch].iter().enumerate() {
                let hz = bin_hz(bin, self.sample_rate);
                if hz > GUST_LOW_HZ && hz <= GUST_SPLIT_HZ {
                    low += value.norm_sqr();
                } else if hz > GUST_SPLIT_HZ && hz <= GUST_MID_TOP_HZ {
                    mid += value.norm_sqr();
                }
            }
        }
        if low <= 1e-20 || mid <= 1e-20 {
            return;
        }
        let tilt = 10.0 * (low / mid).log10();
        self.gust_tilt_db = if self.gust_tilt_db.is_finite() { 0.7 * self.gust_tilt_db + 0.3 * tilt } else { tilt };
        let duck_db = ((self.gust_tilt_db - GUST_TARGET_DB).max(0.0) * self.wind).min(GUST_MAX_DUCK_DB);
        if duck_db <= 0.0 {
            return;
        }
        for bin in 0..BINS {
            let hz = bin_hz(bin, self.sample_rate);
            let weight = if hz <= GUST_SPLIT_HZ {
                1.0
            } else if hz >= GUST_EDGE_HZ {
                break;
            } else {
                1.0 - (hz - GUST_SPLIT_HZ) / (GUST_EDGE_HZ - GUST_SPLIT_HZ)
            };
            let gain = 10f32.powf(-duck_db * weight / 20.0).max(self.floor);
            self.wind_gain[bin] = self.wind_gain[bin].min(gain);
        }
    }

    fn process_frames(&mut self, channels: usize) {
        let mut ok = [false; MAX_CHANNELS];
        for ch in 0..channels {
            for i in 0..FFT_SIZE {
                self.frame[i] = self.channels[ch].input[i] * self.window[i];
            }
            ok[ch] = self.fft.process(&mut self.frame, &mut self.spectra[ch]).is_ok();
        }
        self.wind_gain.fill(1.0);
        if self.wind > 0.0 {
            if channels == 2 && ok[0] && ok[1] {
                self.update_wind_gain();
            }
            self.apply_gust_gain(channels, &ok);
        }
        for ch in 0..channels {
            if ok[ch] {
                self.process_channel(ch);
            }
        }
    }

    fn process_channel(&mut self, ch: usize) {
        let state = &mut self.channels[ch];
        let spectrum = &mut self.spectra[ch];

        // Adaptive noise floor: smoothed power, minimum over ~1 s.
        for (bin, value) in spectrum.iter().enumerate() {
            let p = value.norm_sqr();
            state.smoothed[bin] = if state.primed {
                POWER_SMOOTHING * state.smoothed[bin] + (1.0 - POWER_SMOOTHING) * p
            } else {
                p
            };
            state.sub_min[bin] = state.sub_min[bin].min(state.smoothed[bin]);
        }
        state.primed = true;
        state.sub_count += 1;
        if state.sub_count >= SUBWINDOW_FRAMES {
            let slot = state.history_next;
            state.history[slot].copy_from_slice(&state.sub_min);
            state.history_next = (slot + 1) % SUBWINDOWS;
            state.sub_min.fill(f32::INFINITY);
            state.sub_count = 0;
        }

        // Over-subtract harder as Reduction rises, then let Reduction set
        // how much of the computed attenuation is applied.
        let alpha = 1.0 + 2.0 * self.strength;
        for (bin, value) in spectrum.iter().enumerate() {
            let noise = if self.profile_learned {
                self.learned_noise[bin]
            } else {
                let mut floor = state.sub_min[bin];
                for h in &state.history {
                    floor = floor.min(h[bin]);
                }
                floor * MIN_BIAS
            };
            let p = value.norm_sqr().max(1e-20);
            self.gain_scratch[bin] = (1.0 - alpha * noise / p).clamp(0.0, 1.0);
        }
        // 1-2-1 smoothing across frequency tames isolated "musical" bins.
        let mut prev = self.gain_scratch[0];
        for bin in 0..BINS {
            let cur = self.gain_scratch[bin];
            let next = if bin + 1 < BINS { self.gain_scratch[bin + 1] } else { cur };
            let smoothed = 0.25 * prev + 0.5 * cur + 0.25 * next;
            prev = cur;
            let limited = smoothed.max(self.floor);
            let target = (1.0 - self.strength * (1.0 - limited)).min(self.wind_gain[bin]);
            // Open fast so consonants keep their attack; close slower so
            // the noise does not flutter between frames.
            let last = state.gain[bin];
            let g = if target > last { 0.2 * last + 0.8 * target } else { 0.5 * last + 0.5 * target };
            state.gain[bin] = g;
            spectrum[bin] *= g;
        }
        // DC and Nyquist bins must stay real for the inverse transform.
        spectrum[0].im = 0.0;
        spectrum[BINS - 1].im = 0.0;

        if self.inverse.process(spectrum, &mut self.frame).is_err() {
            return;
        }
        let scale = 1.0 / (FFT_SIZE as f32 * OLA_GAIN);
        for i in 0..FFT_SIZE {
            state.accum[i] += self.frame[i] * self.window[i] * scale;
        }
        state.output[..HOP].copy_from_slice(&state.accum[..HOP]);
        state.accum.copy_within(HOP.., 0);
        state.accum[FFT_SIZE - HOP..].fill(0.0);
        state.input.copy_within(HOP.., 0);
    }
}

impl AudioEffect for Denoiser {
    fn type_id(&self) -> &'static str { "denoiser" }
    fn name(&self) -> &'static str { "Broadband Denoiser" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { PARAM_DESCS }
    fn get_param(&self, name: &str) -> Option<f32> {
        match name {
            "strength" => Some(self.strength),
            "floor" => Some(self.floor),
            "profile_seconds" => Some(self.profile_seconds),
            "wind" => Some(self.wind),
            "profile_learned" => Some(if self.profile_learned { 1.0 } else { 0.0 }),
            "profile_at" => Some(self.profile_at),
            _ => name
                .strip_prefix(BAND_PREFIX)
                .and_then(|i| i.parse::<usize>().ok())
                .filter(|i| *i < PROFILE_BANDS)
                .map(|i| self.profile_bands[i]),
        }
    }
    fn set_param(&mut self, name: &str, value: f32) {
        match name {
            "strength" => self.strength = value.clamp(0.0, 1.0),
            "floor" => self.floor = value.clamp(0.0, 1.0),
            "profile_seconds" => self.profile_seconds = value.clamp(0.1, 5.0),
            "wind" => self.wind = value.clamp(0.0, 1.0),
            "profile_learned" => self.profile_learned = value > 0.5,
            "profile_at" => self.profile_at = value,
            // `learn` and `profile_reset` were one-shot runtime triggers in
            // earlier builds. Profiles are learned offline now; projects that
            // still carry them must not reset anything on rebuild.
            "learn" | "profile_reset" => {}
            _ => {
                if let Some(i) = name.strip_prefix(BAND_PREFIX).and_then(|i| i.parse::<usize>().ok()) {
                    if i < PROFILE_BANDS && value.is_finite() {
                        self.profile_bands[i] = value.clamp(-200.0, 200.0);
                        self.learned_noise_dirty = true;
                    }
                }
            }
        }
    }
    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        let channels = ctx.channels as usize;
        if channels == 0 {
            return;
        }
        if self.profile_learned && self.learned_noise_dirty {
            self.rebuild_learned_noise();
        }
        let active = channels.min(MAX_CHANNELS);
        // Channels advance in lockstep so the wind stage sees both halves
        // of the same frame.
        for frame in buffer.chunks_exact_mut(channels) {
            for (ch, sample) in frame.iter_mut().take(active).enumerate() {
                let state = &mut self.channels[ch];
                state.input[state.pos] = *sample;
                *sample = state.output[state.pos - FIFO_START];
                state.pos += 1;
            }
            if self.channels[0].pos >= FFT_SIZE {
                for state in &mut self.channels[..active] {
                    state.pos = FIFO_START;
                }
                self.process_frames(active);
            }
        }
    }
    fn reset(&mut self) {
        for ch in &mut self.channels {
            ch.reset();
        }
        self.cross_ll.fill(0.0);
        self.cross_rr.fill(0.0);
        self.cross_lr.fill(Complex32::new(0.0, 0.0));
        self.gust_tilt_db = f32::NEG_INFINITY;
    }
    fn latency_samples(&self) -> u32 { LATENCY as u32 }
    fn serialize_params(&self) -> Vec<(String, f32)> {
        let mut params = vec![
            ("strength".into(), self.strength),
            ("floor".into(), self.floor),
            ("profile_seconds".into(), self.profile_seconds),
            ("wind".into(), self.wind),
            ("profile_learned".into(), if self.profile_learned { 1.0 } else { 0.0 }),
            ("profile_at".into(), self.profile_at),
        ];
        params.extend((0..PROFILE_BANDS).map(|b| (band_param_name(b), self.profile_bands[b])));
        params
    }
    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (k, v) in params {
            self.set_param(k, *v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn ctx(channels: u32) -> AudioProcessContext {
        AudioProcessContext { sample_rate: SR, channels, clip_time_secs: 0.0 }
    }

    /// Deterministic white noise in [-amp, amp].
    fn noise(len: usize, amp: f32, seed: u32) -> Vec<f32> {
        let mut state = seed.max(1);
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state as f32 / u32::MAX as f32 * 2.0 - 1.0) * amp
            })
            .collect()
    }

    fn sine(len: usize, hz: f32, amp: f32) -> Vec<f32> {
        (0..len)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / SR as f32).sin() * amp)
            .collect()
    }

    fn energy(buf: &[f32]) -> f32 {
        buf.iter().map(|v| v * v).sum()
    }

    fn with_profile(fx: &mut Denoiser, bands: &[f32]) {
        for (b, db) in bands.iter().enumerate() {
            fx.set_param(&band_param_name(b), *db);
        }
        fx.set_param("profile_learned", 1.0);
    }

    #[test]
    fn zero_reduction_reconstructs_input_after_latency() {
        let mut fx = Denoiser::new(SR);
        fx.set_param("strength", 0.0);
        let input = noise(SR as usize, 0.3, 7);
        let mut out = input.clone();
        // Uneven block sizes, as the keyframed chain delivers them.
        let mut offset = 0;
        for block in [64usize, 1000, 2048, 37].iter().cycle() {
            if offset >= out.len() { break; }
            let end = (offset + block).min(out.len());
            fx.process(&mut out[offset..end], &ctx(1));
            offset = end;
        }
        for i in LATENCY + FFT_SIZE..input.len() {
            assert!((out[i] - input[i - LATENCY]).abs() < 1e-3, "sample {i}: {} vs {}", out[i], input[i - LATENCY]);
        }
    }

    #[test]
    fn learned_profile_removes_noise_and_keeps_tone() {
        let noise_only = noise(SR as usize, 0.1, 3);
        let bands = learn_noise_profile(&noise_only, SR).expect("profile");
        assert_eq!(bands.len(), PROFILE_BANDS);

        let mut fx = Denoiser::new(SR);
        fx.set_param("strength", 1.0);
        with_profile(&mut fx, &bands);
        let mut hiss = noise(SR as usize * 2, 0.1, 11);
        let before = energy(&hiss[SR as usize..]);
        fx.process(&mut hiss, &ctx(1));
        let after = energy(&hiss[SR as usize..]);
        assert!(after < before * 0.05, "noise should drop by >13 dB: {before} -> {after}");

        let mut fx = Denoiser::new(SR);
        fx.set_param("strength", 1.0);
        with_profile(&mut fx, &bands);
        let tone = sine(SR as usize * 2, 1000.0, 0.5);
        let mut out = tone.clone();
        fx.process(&mut out, &ctx(1));
        let kept = energy(&out[SR as usize..]) / energy(&tone[SR as usize..]);
        assert!(kept > 0.9, "tone well above the noise must pass: kept {kept}");
    }

    #[test]
    fn reduction_slider_changes_the_result() {
        let run = |strength: f32| {
            let mut fx = Denoiser::new(SR);
            fx.set_param("strength", strength);
            let mut buf = noise(SR as usize * 3, 0.1, 5);
            fx.process(&mut buf, &ctx(1));
            energy(&buf[SR as usize * 2..])
        };
        let light = run(0.25);
        let heavy = run(1.0);
        assert!(heavy < light * 0.25, "full reduction must be clearly stronger: {light} vs {heavy}");
    }

    #[test]
    fn adaptive_mode_cleans_steady_noise_without_a_profile() {
        let mut fx = Denoiser::new(SR);
        fx.set_param("strength", 1.0);
        let mut buf = noise(SR as usize * 3, 0.1, 9);
        let before = energy(&buf[SR as usize * 2..]);
        fx.process(&mut buf, &ctx(1));
        let after = energy(&buf[SR as usize * 2..]);
        assert!(after < before * 0.1, "adaptive tracking should remove steady hiss: {before} -> {after}");
        assert!(buf.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn channels_do_not_bleed_into_each_other() {
        let mut fx = Denoiser::new(SR);
        let left = noise(SR as usize, 0.3, 13);
        let mut stereo: Vec<f32> = left.iter().flat_map(|l| [*l, 0.0]).collect();
        fx.process(&mut stereo, &ctx(2));
        let right_energy: f32 = stereo.iter().skip(1).step_by(2).map(|v| v * v).sum();
        assert!(right_energy < 1e-9, "silent right channel picked up {right_energy}");
    }

    #[test]
    fn wind_stage_cuts_incoherent_low_end_and_keeps_a_centred_voice() {
        let run = |wind: f32, left: &[f32], right: &[f32]| {
            let mut fx = Denoiser::new(SR);
            fx.set_param("strength", 0.0);
            fx.set_param("wind", wind);
            let mut stereo: Vec<f32> = left.iter().zip(right).flat_map(|(l, r)| [*l, *r]).collect();
            fx.process(&mut stereo, &ctx(2));
            energy(&stereo[SR as usize * 2..])
        };
        let len = SR as usize * 3;
        // Independent low-frequency turbulence at each microphone.
        let lowpass = |x: Vec<f32>| {
            let mut y = 0.0f32;
            x.into_iter().map(|v| { y += 0.05 * (v - y); y }).collect::<Vec<f32>>()
        };
        let (wl, wr) = (
            lowpass(lowpass(lowpass(noise(len, 1.0, 31)))),
            lowpass(lowpass(lowpass(noise(len, 1.0, 37)))),
        );
        let gusts = run(1.0, &wl, &wr) / run(0.0, &wl, &wr);
        assert!(gusts < 0.1, "incoherent wind should drop by >10 dB, kept {gusts}");

        let voice: Vec<f32> = sine(len, 220.0, 0.1).iter().zip(sine(len, 900.0, 0.2)).zip(sine(len, 2000.0, 0.1))
            .map(|((a, b), c)| a + b + c).collect();
        let kept = run(1.0, &voice, &voice) / run(0.0, &voice, &voice);
        assert!(kept > 0.9, "a sound reaching both mics must pass, kept {kept}");
    }

    #[test]
    fn gust_stage_ducks_a_low_end_surge_but_not_a_balanced_voice() {
        let run = |wind: f32, mono: &[f32]| {
            let mut fx = Denoiser::new(SR);
            fx.set_param("strength", 0.0);
            fx.set_param("wind", wind);
            let mut buf = mono.to_vec();
            fx.process(&mut buf, &ctx(1));
            energy(&buf[SR as usize..])
        };
        let len = SR as usize * 2;
        // Rumble at 80 Hz towering 14 dB over a 1 kHz component.
        let gust: Vec<f32> = sine(len, 80.0, 0.5).iter().zip(sine(len, 1000.0, 0.1)).map(|(a, b)| a + b).collect();
        let ducked = run(1.0, &gust) / run(0.0, &gust);
        assert!(ducked < 0.2, "a low-end surge should be ducked, kept {ducked}");

        // A voice-like spectrum with more energy in the mids than the lows.
        let voice: Vec<f32> = sine(len, 150.0, 0.1).iter().zip(sine(len, 900.0, 0.2)).zip(sine(len, 2000.0, 0.1))
            .map(|((a, b), c)| a + b + c).collect();
        let kept = run(1.0, &voice) / run(0.0, &voice);
        assert!(kept > 0.95, "balanced material must pass untouched, kept {kept}");
    }

    #[test]
    fn profile_survives_serialize_round_trip() {
        let bands = learn_noise_profile(&noise(SR as usize / 2, 0.1, 17), SR).unwrap();
        let mut fx = Denoiser::new(SR);
        with_profile(&mut fx, &bands);
        fx.set_param("profile_at", 12.5);
        let saved = fx.serialize_params();

        let mut rebuilt = Denoiser::new(SR);
        rebuilt.deserialize_params(&saved);
        assert!(rebuilt.has_learned_profile());
        assert_eq!(rebuilt.get_param("profile_at"), Some(12.5));
        for (b, db) in bands.iter().enumerate() {
            assert_eq!(rebuilt.get_param(&band_param_name(b)), Some(*db));
        }
    }

    #[test]
    fn legacy_learn_triggers_are_ignored() {
        let mut fx = Denoiser::new(SR);
        with_profile(&mut fx, &[-40.0; PROFILE_BANDS]);
        fx.deserialize_params(&[("learn".into(), 1.0), ("profile_reset".into(), 3.0)]);
        assert!(fx.has_learned_profile());
    }

    #[test]
    fn quietest_window_finds_the_noise_only_gap() {
        let mut mono = sine(SR as usize * 3, 300.0, 0.5);
        let gap = noise(SR as usize / 2, 0.01, 21);
        let start = SR as usize + SR as usize / 4;
        mono[start..start + gap.len()].copy_from_slice(&gap);
        let found = quietest_window(&mono, SR, 0.4).unwrap();
        assert!(found >= start && found + (0.4 * SR as f32) as usize <= start + gap.len(), "found {found}, gap at {start}");
    }
}
