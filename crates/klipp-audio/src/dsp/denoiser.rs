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

/// Surge detector: a gust also shows as the low end jumping far above its
/// own calm level. Beach and street wind often lifts the mids with it,
/// which hides it from the tilt test above.
///
/// Both tests only act on noise-like sound. A voice lifting the low end is
/// harmonic: between `FLAT_LOW_HZ` and `FLAT_HIGH_HZ` its bins stand far
/// above their local spectral envelope, while wind's do not. Measuring
/// flatness against that envelope rather than the raw spectrum keeps
/// wind's steep low-end slope from reading as structure.
const SURGE_LOW_HZ: f32 = 20.0;
const SURGE_TOP_HZ: f32 = 300.0;
const FLAT_LOW_HZ: f32 = 140.0;
const FLAT_HIGH_HZ: f32 = 1500.0;
/// Bins either side averaged into the local envelope.
const FLAT_ENVELOPE_BINS: usize = 3;
/// Calm level of the low end: the 20th percentile of up to 20
/// quarter-second averages (47 hops each at 48 kHz), about the last five
/// seconds. A minimum would sit in the troughs of wind's own flutter and
/// read calm wind as a gust.
const SURGE_SUBWINDOW_FRAMES: usize = 47;
const SURGE_SUBWINDOWS: usize = 20;
/// Rise above the calm level where a gust starts to count. Ducking grows
/// at twice the rise past it, so from twice the margin a gust is pulled
/// all the way back to the calm level.
const SURGE_MARGIN_DB: f32 = 3.0;
/// Voicing: peak of the normalised autocorrelation of the whitened
/// 300-3500 Hz spectrum over pitch lags of 70-700 Hz. A frame counts as
/// wind only when it is noise-like by both flatness and voicing.
/// Measured on the 2048-point detector: beach wind flatness 0.62-0.68 and
/// voicing 0.15-0.19; speech flatness 0.31-0.58 with voiced frames above
/// 0.45; steady voices at 120 Hz and 85 Hz read 0.14/0.72 and 0.49/0.52.
const VOICING_LOW_HZ: f32 = 300.0;
const VOICING_HIGH_HZ: f32 = 3500.0;
const VOICING_MIN_PITCH_HZ: f32 = 70.0;
const VOICING_MAX_PITCH_HZ: f32 = 700.0;
const VOICING_WIND: f32 = 0.22;
const VOICING_VOICED: f32 = 0.32;
/// Envelope-relative flatness of voiced speech sits below `FLAT_VOICED`,
/// wind above `FLAT_NOISE`.
const FLAT_VOICED: f32 = 0.52;
const FLAT_NOISE: f32 = 0.62;

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
    surge: SurgeState,
    detector: Detector,
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

/// Running state of the surge detector (shared by both channels).
struct SurgeState {
    low_db: f32,
    flatness: f32,
    voicing: f32,
    sub_sum: f32,
    history: [f32; SURGE_SUBWINDOWS],
    history_len: usize,
    history_next: usize,
    sub_count: usize,
}

impl SurgeState {
    fn new() -> Self {
        Self {
            low_db: f32::NEG_INFINITY,
            flatness: 0.0,
            voicing: 1.0,
            sub_sum: 0.0,
            history: [0.0; SURGE_SUBWINDOWS],
            history_len: 0,
            history_next: 0,
            sub_count: 0,
        }
    }

    /// Feed one frame's low-end level, flatness and voicing; returns how
    /// many dB the low end stands above its calm level, and how noise-like
    /// (0..1) the sound is.
    fn update(&mut self, low_db: f32, flatness: f32, voicing: f32) -> (f32, f32) {
        if self.low_db.is_finite() {
            self.low_db = 0.7 * self.low_db + 0.3 * low_db;
            self.flatness = 0.8 * self.flatness + 0.2 * flatness;
            self.voicing = 0.6 * self.voicing + 0.4 * voicing;
        } else {
            self.low_db = low_db;
            self.flatness = flatness;
            self.voicing = voicing;
        }
        self.sub_sum += self.low_db;
        self.sub_count += 1;
        if self.sub_count >= SURGE_SUBWINDOW_FRAMES {
            self.history[self.history_next] = self.sub_sum / SURGE_SUBWINDOW_FRAMES as f32;
            self.history_next = (self.history_next + 1) % SURGE_SUBWINDOWS;
            self.history_len = (self.history_len + 1).min(SURGE_SUBWINDOWS);
            self.sub_sum = 0.0;
            self.sub_count = 0;
        }
        let rise = if self.history_len == 0 {
            0.0
        } else {
            let mut calm = self.history;
            let calm = &mut calm[..self.history_len];
            calm.sort_by(|a, b| a.total_cmp(b));
            self.low_db - calm[calm.len() / 5]
        };
        let excess = (2.0 * (rise - SURGE_MARGIN_DB)).min(rise);
        let flat = ((self.flatness - FLAT_VOICED) / (FLAT_NOISE - FLAT_VOICED)).clamp(0.0, 1.0);
        let unvoiced = ((VOICING_VOICED - self.voicing) / (VOICING_VOICED - VOICING_WIND)).clamp(0.0, 1.0);
        (excess.clamp(0.0, GUST_MAX_DUCK_DB), flat * unvoiced)
    }
}

/// How much of the surge duck reaches a bin: all of it up to 1 kHz, where
/// beach gusts are loudest, then a fading share of the buffeting crackle
/// above. Only frames the gate judged to be wind are ducked at all.
fn surge_weight(hz: f32) -> f32 {
    if hz <= 1000.0 {
        1.0
    } else if hz <= 3000.0 {
        1.0 - 0.5 * (hz - 1000.0) / 2000.0
    } else if hz <= 8000.0 {
        0.5 * (1.0 - (hz - 3000.0) / 5000.0)
    } else {
        0.0
    }
}

/// Analysis size of the wind gate. The audio path's 1024 points resolve
/// 47 Hz, too coarse to separate the harmonics of a voice pitched at
/// 120 Hz or below, which then looks as smooth as wind. The gate looks
/// back over 2048 samples (23 Hz) instead; the audio path is unchanged.
const DETECT_SIZE: usize = 2048;
const DETECT_BINS: usize = DETECT_SIZE / 2 + 1;

/// Mono look-back and transforms for the wind gate's flatness and voicing.
struct Detector {
    input: Vec<f32>,
    window: Vec<f32>,
    fft: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    frame: Vec<f32>,
    spectrum: Vec<Complex32>,
    power: Vec<f32>,
    whitened: Vec<Complex32>,
    lags: Vec<f32>,
}

impl Detector {
    fn new(planner: &mut RealFftPlanner<f32>) -> Self {
        let fft = planner.plan_fft_forward(DETECT_SIZE);
        let inverse = planner.plan_fft_inverse(DETECT_SIZE);
        Self {
            input: vec![0.0; DETECT_SIZE],
            window: (0..DETECT_SIZE)
                .map(|n| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * n as f32 / DETECT_SIZE as f32).cos())
                .collect(),
            spectrum: fft.make_output_vec(),
            frame: vec![0.0; DETECT_SIZE],
            power: vec![0.0; DETECT_BINS],
            whitened: vec![Complex32::new(0.0, 0.0); DETECT_BINS],
            lags: vec![0.0; DETECT_SIZE],
            fft,
            inverse,
        }
    }

    /// Append the newest hop, averaged over the channels.
    fn push(&mut self, tails: &[&[f32]]) {
        if tails.is_empty() {
            return;
        }
        self.input.copy_within(HOP.., 0);
        let scale = 1.0 / tails.len() as f32;
        for i in 0..HOP {
            self.input[DETECT_SIZE - HOP + i] = tails.iter().map(|t| t[i]).sum::<f32>() * scale;
        }
    }

    /// Spectral flatness against the local envelope over
    /// `FLAT_LOW_HZ..FLAT_HIGH_HZ`, and voicing: the peak normalised
    /// autocorrelation of the whitened `VOICING_LOW_HZ..VOICING_HIGH_HZ`
    /// band over pitch lags.
    fn analyse(&mut self, sample_rate: f32) -> Option<(f32, f32)> {
        for i in 0..DETECT_SIZE {
            self.frame[i] = self.input[i] * self.window[i];
        }
        self.fft.process(&mut self.frame, &mut self.spectrum).ok()?;
        for (p, v) in self.power.iter_mut().zip(&self.spectrum) {
            *p = v.norm_sqr();
        }
        let bin_hz = |bin: usize| bin as f32 * sample_rate / DETECT_SIZE as f32;
        let (mut flat_log, mut flat_sum, mut flat_bins) = (0.0f32, 0.0f32, 0usize);
        self.whitened.fill(Complex32::new(0.0, 0.0));
        for bin in FLAT_ENVELOPE_BINS..DETECT_BINS - FLAT_ENVELOPE_BINS {
            let hz = bin_hz(bin);
            let in_flat = hz > FLAT_LOW_HZ && hz <= FLAT_HIGH_HZ;
            let in_voicing = (VOICING_LOW_HZ..=VOICING_HIGH_HZ).contains(&hz);
            if !in_flat && !in_voicing {
                if hz > FLAT_HIGH_HZ.max(VOICING_HIGH_HZ) {
                    break;
                }
                continue;
            }
            let envelope = self.power[bin - FLAT_ENVELOPE_BINS..=bin + FLAT_ENVELOPE_BINS].iter().sum::<f32>()
                / (2 * FLAT_ENVELOPE_BINS + 1) as f32;
            let ratio = self.power[bin] / envelope.max(1e-20);
            if in_flat {
                flat_log += (ratio + 1e-20).ln();
                flat_sum += ratio;
                flat_bins += 1;
            }
            if in_voicing {
                self.whitened[bin].re = ratio;
            }
        }
        if flat_bins == 0 || flat_sum <= 0.0 {
            return None;
        }
        let flatness = (flat_log / flat_bins as f32).exp() / (flat_sum / flat_bins as f32);
        self.inverse.process(&mut self.whitened, &mut self.lags).ok()?;
        let r0 = self.lags[0];
        let voicing = if r0 > 1e-20 {
            let first = (sample_rate / VOICING_MAX_PITCH_HZ) as usize;
            let last = ((sample_rate / VOICING_MIN_PITCH_HZ) as usize).min(DETECT_SIZE / 2);
            self.lags[first..=last].iter().fold(0.0f32, |m, r| m.max(*r / r0))
        } else {
            1.0
        };
        Some((flatness, voicing))
    }

    fn reset(&mut self) {
        self.input.fill(0.0);
    }
}

static PARAM_DESCS: &[AudioParamDescriptor] = &[
    AudioParamDescriptor { name: "strength", label: "Reduction", default: 0.75, min: 0.0, max: 1.0, unit: ParamUnit::Fraction },
    AudioParamDescriptor { name: "floor", label: "Noise Floor", default: 0.08, min: 0.0, max: 1.0, unit: ParamUnit::Fraction },
    AudioParamDescriptor { name: "wind", label: "Wind Reduction", default: 0.0, min: 0.0, max: 1.0, unit: ParamUnit::Fraction },
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
            surge: SurgeState::new(),
            detector: Detector::new(&mut planner),
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
    /// Duck the low bins while a gust is detected: either the low end
    /// towers over the mid band, or it surges above its calm level while
    /// the sound stays noise-like.
    fn apply_gust_gain(&mut self, channels: usize, ok: &[bool; MAX_CHANNELS]) {
        let (mut low, mut mid, mut surge_low) = (0.0f32, 0.0f32, 0.0f32);
        let active: Vec<usize> = (0..channels).filter(|ch| ok[*ch]).collect();
        if active.is_empty() {
            return;
        }
        let power = &mut self.gain_scratch;
        for (bin, p) in power.iter_mut().enumerate() {
            *p = active.iter().map(|ch| self.spectra[*ch][bin].norm_sqr()).sum::<f32>() / active.len() as f32;
            let hz = bin_hz(bin, self.sample_rate);
            if hz > GUST_LOW_HZ && hz <= GUST_SPLIT_HZ {
                low += *p;
            } else if hz > GUST_SPLIT_HZ && hz <= GUST_MID_TOP_HZ {
                mid += *p;
            }
            if hz > SURGE_LOW_HZ && hz <= SURGE_TOP_HZ {
                surge_low += *p;
            }
        }
        if low <= 1e-20 || mid <= 1e-20 {
            return;
        }
        let Some((flatness, voicing)) = self.detector.analyse(self.sample_rate) else { return };
        let (surge_db, noise_like) = self.surge.update(10.0 * (surge_low + 1e-20).log10(), flatness, voicing);
        let surge_duck = surge_db * noise_like * self.wind;

        let tilt = 10.0 * (low / mid).log10();
        self.gust_tilt_db = if self.gust_tilt_db.is_finite() { 0.7 * self.gust_tilt_db + 0.3 * tilt } else { tilt };
        let tilt_duck = ((self.gust_tilt_db - GUST_TARGET_DB).max(0.0) * noise_like * self.wind).min(GUST_MAX_DUCK_DB);

        if tilt_duck <= 0.0 && surge_duck <= 0.0 {
            return;
        }
        for bin in 0..BINS {
            let hz = bin_hz(bin, self.sample_rate);
            let tilt_weight = if hz <= GUST_SPLIT_HZ {
                1.0
            } else if hz >= GUST_EDGE_HZ {
                0.0
            } else {
                1.0 - (hz - GUST_SPLIT_HZ) / (GUST_EDGE_HZ - GUST_SPLIT_HZ)
            };
            let duck_db = (tilt_duck * tilt_weight).max(surge_duck * surge_weight(hz));
            if duck_db <= 0.0 {
                continue;
            }
            let gain = 10f32.powf(-duck_db / 20.0).max(self.floor);
            self.wind_gain[bin] = self.wind_gain[bin].min(gain);
        }
    }

    fn process_frames(&mut self, channels: usize) {
        if self.wind > 0.0 {
            let tails: Vec<&[f32]> = self.channels[..channels].iter().map(|c| &c.input[FFT_SIZE - HOP..]).collect();
            self.detector.push(&tails);
        }
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
        self.surge = SurgeState::new();
        self.detector.reset();
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

    /// Low-passed noise: a stand-in for wind's rumble.
    fn rumble(len: usize, amp: f32, seed: u32) -> Vec<f32> {
        let (mut a, mut b, mut c) = (0.0f32, 0.0f32, 0.0f32);
        noise(len, amp, seed)
            .into_iter()
            .map(|v| {
                a += 0.02 * (v - a);
                b += 0.02 * (a - b);
                c += 0.02 * (b - c);
                c * 60.0
            })
            .collect()
    }

    fn wind_run(wind: f32, mono: &[f32]) -> Vec<f32> {
        let mut fx = Denoiser::new(SR);
        fx.set_param("strength", 0.0);
        fx.set_param("wind", wind);
        let mut buf = mono.to_vec();
        fx.process(&mut buf, &ctx(1));
        buf
    }

    #[test]
    fn gust_stage_ducks_a_low_end_surge() {
        // Rumble towering over a little broadband noise.
        let len = SR as usize * 2;
        let gust: Vec<f32> = rumble(len, 0.5, 3).iter().zip(noise(len, 0.02, 4)).map(|(a, b)| a + b).collect();
        let ducked = energy(&wind_run(1.0, &gust)[SR as usize..]) / energy(&wind_run(0.0, &gust)[SR as usize..]);
        assert!(ducked < 0.2, "a low-end surge should be ducked, kept {ducked}");
    }

    #[test]
    fn surge_stage_ducks_a_broadband_gust_over_calm_wind() {
        // Two seconds of calm wind, then a gust 15 dB louder that lifts the
        // mids too, as on the beach recording that the tilt test missed.
        let calm = SR as usize * 2;
        let len = calm + SR as usize * 2;
        let base: Vec<f32> = rumble(len, 0.05, 5).iter().zip(noise(len, 0.01, 6)).map(|(a, b)| a + b).collect();
        let gust: Vec<f32> = base.iter().enumerate().map(|(i, v)| if i >= calm { v * 5.6 } else { *v }).collect();
        let tail = calm + SR as usize / 2;
        let ducked = energy(&wind_run(1.0, &gust)[tail..]) / energy(&wind_run(0.0, &gust)[tail..]);
        assert!(ducked < 0.25, "a gust over calm wind should drop by >6 dB, kept {ducked}");
    }

    #[test]
    fn wind_stages_leave_a_voice_alone() {
        // A harmonic voice (120 Hz with 5 Hz vibrato and a syllable rhythm,
        // harmonics to 3 kHz falling 6 dB an octave) entering over calm
        // wind raises the low end just like a gust does, but it is not
        // noise and must pass.
        let calm = SR as usize * 2;
        let len = calm + SR as usize * 2;
        let bed = rumble(len, 0.02, 7);
        let mut phase = 0.0f32;
        let voice: Vec<f32> = (0..len)
            .map(|i| {
                let t = i as f32 / SR as f32;
                let f0 = 120.0 * (1.0 + 0.03 * (2.0 * std::f32::consts::PI * 5.0 * t).sin());
                phase += 2.0 * std::f32::consts::PI * f0 / SR as f32;
                if i < calm { return 0.0; }
                let syllable = 0.6 + 0.4 * (2.0 * std::f32::consts::PI * 4.0 * t).sin();
                syllable * (1..=25).map(|h| (phase * h as f32).sin() * 0.3 / h as f32).sum::<f32>()
            })
            .collect();
        let mix: Vec<f32> = bed.iter().zip(&voice).map(|(a, b)| a + b).collect();
        let tail = calm + SR as usize / 2;
        let kept = energy(&wind_run(1.0, &mix)[tail..]) / energy(&wind_run(0.0, &mix)[tail..]);
        assert!(kept > 0.85, "a voice must not be taken for a gust, kept {kept}");
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
