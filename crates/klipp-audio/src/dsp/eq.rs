use super::{AudioEffect, AudioParamDescriptor, AudioProcessContext, ParamUnit};
use std::f32::consts::PI;

const NUM_BANDS: usize = 6;
const MAX_CHANNELS: usize = 2;

/// Biquad filter type for each EQ band.
#[derive(Debug, Clone, Copy, PartialEq)]
enum FilterType {
    LowShelf,
    Peaking,
    HighShelf,
}

/// Biquad filter coefficients (Direct Form I).
#[derive(Debug, Clone, Copy)]
struct BiquadCoeffs {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Default for BiquadCoeffs {
    fn default() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }
}

/// Per-channel biquad state (2 delay elements).
#[derive(Debug, Clone, Copy, Default)]
struct BiquadState {
    z1: f32,
    z2: f32,
}

/// Band parameters.
struct Band {
    filter_type: FilterType,
    frequency: f32,
    gain: f32,
    q: f32,
    enabled: bool,
    coeffs: BiquadCoeffs,
    state: [[BiquadState; 1]; MAX_CHANNELS], // one biquad section per channel
}

/// 6-band Parametric EQ using Robert Bristow-Johnson biquad cookbook formulas.
pub struct ParametricEq {
    sample_rate: f32,
    bands: [Band; NUM_BANDS],
    dirty: bool,
}

impl ParametricEq {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let default_bands = [
            (FilterType::LowShelf, 80.0_f32),
            (FilterType::Peaking, 250.0),
            (FilterType::Peaking, 1000.0),
            (FilterType::Peaking, 3000.0),
            (FilterType::Peaking, 8000.0),
            (FilterType::HighShelf, 12000.0),
        ];

        let bands = std::array::from_fn(|i| {
            let (ft, freq) = default_bands[i];
            Band {
                filter_type: ft,
                frequency: freq,
                gain: 0.0,
                q: 1.0,
                enabled: true,
                coeffs: BiquadCoeffs::default(),
                state: [[BiquadState::default(); 1]; MAX_CHANNELS],
            }
        });

        let mut eq = Self {
            sample_rate: sr,
            bands,
            dirty: true,
        };
        eq.recompute_coefficients();
        eq
    }

    fn recompute_coefficients(&mut self) {
        for band in &mut self.bands {
            if !band.enabled {
                band.coeffs = BiquadCoeffs::default();
                continue;
            }
            band.coeffs =
                compute_biquad(band.filter_type, band.frequency, band.gain, band.q, self.sample_rate);
        }
        self.dirty = false;
    }
}

fn compute_biquad(ft: FilterType, freq: f32, gain_db: f32, q: f32, sr: f32) -> BiquadCoeffs {
    let a = 10.0_f32.powf(gain_db / 40.0); // sqrt of linear gain
    let w0 = 2.0 * PI * freq / sr;
    let cos_w0 = w0.cos();
    let sin_w0 = w0.sin();
    let alpha = sin_w0 / (2.0 * q);

    let (b0, b1, b2, a0, a1, a2) = match ft {
        FilterType::Peaking => {
            let b0 = 1.0 + alpha * a;
            let b1 = -2.0 * cos_w0;
            let b2 = 1.0 - alpha * a;
            let a0 = 1.0 + alpha / a;
            let a1 = -2.0 * cos_w0;
            let a2 = 1.0 - alpha / a;
            (b0, b1, b2, a0, a1, a2)
        }
        FilterType::LowShelf => {
            let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
            let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
            let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
            let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
            let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
            let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
            let a2 = (a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha;
            (b0, b1, b2, a0, a1, a2)
        }
        FilterType::HighShelf => {
            let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
            let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
            let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
            let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
            let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
            let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
            let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha;
            (b0, b1, b2, a0, a1, a2)
        }
    };

    let inv_a0 = 1.0 / a0;
    BiquadCoeffs {
        b0: b0 * inv_a0,
        b1: b1 * inv_a0,
        b2: b2 * inv_a0,
        a1: a1 * inv_a0,
        a2: a2 * inv_a0,
    }
}

#[inline]
fn process_biquad(sample: f32, coeffs: &BiquadCoeffs, state: &mut BiquadState) -> f32 {
    // Transposed Direct Form II
    let out = coeffs.b0 * sample + state.z1;
    state.z1 = coeffs.b1 * sample - coeffs.a1 * out + state.z2;
    state.z2 = coeffs.b2 * sample - coeffs.a2 * out;
    out
}

// Parameter descriptors: 6 bands x 4 params each = 24 params
static PARAM_DESCS: &[AudioParamDescriptor] = &[
    // Band 1 (Low Shelf)
    AudioParamDescriptor { name: "band1_enabled", label: "Band 1 On", default: 1.0, min: 0.0, max: 1.0, unit: ParamUnit::None },
    AudioParamDescriptor { name: "band1_frequency", label: "Band 1 Freq", default: 80.0, min: 20.0, max: 20000.0, unit: ParamUnit::Hz },
    AudioParamDescriptor { name: "band1_gain", label: "Band 1 Gain", default: 0.0, min: -24.0, max: 24.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "band1_q", label: "Band 1 Q", default: 1.0, min: 0.1, max: 10.0, unit: ParamUnit::None },
    // Band 2 (Parametric)
    AudioParamDescriptor { name: "band2_enabled", label: "Band 2 On", default: 1.0, min: 0.0, max: 1.0, unit: ParamUnit::None },
    AudioParamDescriptor { name: "band2_frequency", label: "Band 2 Freq", default: 250.0, min: 20.0, max: 20000.0, unit: ParamUnit::Hz },
    AudioParamDescriptor { name: "band2_gain", label: "Band 2 Gain", default: 0.0, min: -24.0, max: 24.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "band2_q", label: "Band 2 Q", default: 1.0, min: 0.1, max: 10.0, unit: ParamUnit::None },
    // Band 3 (Parametric)
    AudioParamDescriptor { name: "band3_enabled", label: "Band 3 On", default: 1.0, min: 0.0, max: 1.0, unit: ParamUnit::None },
    AudioParamDescriptor { name: "band3_frequency", label: "Band 3 Freq", default: 1000.0, min: 20.0, max: 20000.0, unit: ParamUnit::Hz },
    AudioParamDescriptor { name: "band3_gain", label: "Band 3 Gain", default: 0.0, min: -24.0, max: 24.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "band3_q", label: "Band 3 Q", default: 1.0, min: 0.1, max: 10.0, unit: ParamUnit::None },
    // Band 4 (Parametric)
    AudioParamDescriptor { name: "band4_enabled", label: "Band 4 On", default: 1.0, min: 0.0, max: 1.0, unit: ParamUnit::None },
    AudioParamDescriptor { name: "band4_frequency", label: "Band 4 Freq", default: 3000.0, min: 20.0, max: 20000.0, unit: ParamUnit::Hz },
    AudioParamDescriptor { name: "band4_gain", label: "Band 4 Gain", default: 0.0, min: -24.0, max: 24.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "band4_q", label: "Band 4 Q", default: 1.0, min: 0.1, max: 10.0, unit: ParamUnit::None },
    // Band 5 (Parametric)
    AudioParamDescriptor { name: "band5_enabled", label: "Band 5 On", default: 1.0, min: 0.0, max: 1.0, unit: ParamUnit::None },
    AudioParamDescriptor { name: "band5_frequency", label: "Band 5 Freq", default: 8000.0, min: 20.0, max: 20000.0, unit: ParamUnit::Hz },
    AudioParamDescriptor { name: "band5_gain", label: "Band 5 Gain", default: 0.0, min: -24.0, max: 24.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "band5_q", label: "Band 5 Q", default: 1.0, min: 0.1, max: 10.0, unit: ParamUnit::None },
    // Band 6 (High Shelf)
    AudioParamDescriptor { name: "band6_enabled", label: "Band 6 On", default: 1.0, min: 0.0, max: 1.0, unit: ParamUnit::None },
    AudioParamDescriptor { name: "band6_frequency", label: "Band 6 Freq", default: 12000.0, min: 20.0, max: 20000.0, unit: ParamUnit::Hz },
    AudioParamDescriptor { name: "band6_gain", label: "Band 6 Gain", default: 0.0, min: -24.0, max: 24.0, unit: ParamUnit::Db },
    AudioParamDescriptor { name: "band6_q", label: "Band 6 Q", default: 1.0, min: 0.1, max: 10.0, unit: ParamUnit::None },
];

impl AudioEffect for ParametricEq {
    fn type_id(&self) -> &'static str {
        "eq"
    }

    fn name(&self) -> &'static str {
        "Parametric EQ"
    }

    fn param_descriptors(&self) -> &[AudioParamDescriptor] {
        PARAM_DESCS
    }

    fn get_param(&self, name: &str) -> Option<f32> {
        // Parse band index from param name (e.g. "band1_frequency")
        let (band_idx, field) = parse_band_param(name)?;
        let band = &self.bands[band_idx];
        match field {
            "enabled" => Some(if band.enabled { 1.0 } else { 0.0 }),
            "frequency" => Some(band.frequency),
            "gain" => Some(band.gain),
            "q" => Some(band.q),
            _ => None,
        }
    }

    fn set_param(&mut self, name: &str, value: f32) {
        if let Some((band_idx, field)) = parse_band_param(name) {
            let band = &mut self.bands[band_idx];
            match field {
                "enabled" => band.enabled = value > 0.5,
                "frequency" => band.frequency = value.clamp(20.0, 20000.0),
                "gain" => band.gain = value.clamp(-24.0, 24.0),
                "q" => band.q = value.clamp(0.1, 10.0),
                _ => return,
            }
            self.dirty = true;
        }
    }

    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        if self.dirty {
            self.recompute_coefficients();
        }

        let channels = ctx.channels as usize;
        if channels == 0 {
            return;
        }

        let num_frames = buffer.len() / channels;

        for band in &mut self.bands {
            if !band.enabled || (band.gain.abs() < 0.001 && band.filter_type == FilterType::Peaking) {
                continue;
            }

            let coeffs = &band.coeffs;
            for frame in 0..num_frames {
                for ch in 0..channels.min(MAX_CHANNELS) {
                    let idx = frame * channels + ch;
                    buffer[idx] = process_biquad(buffer[idx], coeffs, &mut band.state[ch][0]);
                }
            }
        }
    }

    fn reset(&mut self) {
        for band in &mut self.bands {
            for ch_states in &mut band.state {
                for s in ch_states.iter_mut() {
                    *s = BiquadState::default();
                }
            }
        }
    }

    fn serialize_params(&self) -> Vec<(String, f32)> {
        let mut params = Vec::with_capacity(NUM_BANDS * 4);
        for (i, band) in self.bands.iter().enumerate() {
            let n = i + 1;
            params.push((format!("band{n}_enabled"), if band.enabled { 1.0 } else { 0.0 }));
            params.push((format!("band{n}_frequency"), band.frequency));
            params.push((format!("band{n}_gain"), band.gain));
            params.push((format!("band{n}_q"), band.q));
        }
        params
    }

    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (name, value) in params {
            self.set_param(name, *value);
        }
        self.dirty = true;
    }
}

fn parse_band_param(name: &str) -> Option<(usize, &str)> {
    // Expected format: "bandN_field"
    let name = name.strip_prefix("band")?;
    let digit = name.chars().next()?;
    let idx = digit.to_digit(10)? as usize;
    if !(1..=NUM_BANDS).contains(&idx) {
        return None;
    }
    let field = name.get(2..)?; // skip digit and underscore
    Some((idx - 1, field))
}
