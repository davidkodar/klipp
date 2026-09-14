//! Biquad filter DSP using Robert Bristow-Johnson Audio EQ Cookbook formulas.
//!
//! Single-section [`BiquadFilter`] (Direct Form II Transposed) plus a
//! [`BiquadCascade`] of N sections with Butterworth Q values for steeper
//! HP/LP slopes (12 / 18 / 24 / 36 / 48 dB/oct). Both expose
//! `magnitude_at(freq, sr)` so the inspector EQ can paint a frequency
//! response without an FFT.
//!
//! Forked from Tona Studio (`tona-dsp::biquad`, MIT). Kept verbatim aside
//! from the module-level doc and this attribution; the math + DF-II-T
//! state form match `klipp-audio::dsp::eq`'s private biquad so there's
//! no behaviour delta when callers migrate.

use std::f32::consts::PI;

/// Filter type for a biquad section
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterType {
    LowPass,
    HighPass,
    BandPass,
    Notch,
    AllPass,
    PeakEq,
    LowShelf,
    HighShelf,
}

/// Filter slope for cascaded HP/LP filters
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterSlope {
    Db6,
    Db12,
    Db18,
    Db24,
    Db36,
    Db48,
}

impl FilterSlope {
    /// Number of biquad sections needed for this slope
    pub fn num_sections(self) -> usize {
        match self {
            FilterSlope::Db6 => 1,  // single first-order approximation
            FilterSlope::Db12 => 1,
            FilterSlope::Db18 => 2, // 12 + 6
            FilterSlope::Db24 => 2,
            FilterSlope::Db36 => 3,
            FilterSlope::Db48 => 4,
        }
    }

    /// Convert from a 0-5 index
    pub fn from_index(idx: u32) -> Self {
        match idx {
            0 => FilterSlope::Db6,
            1 => FilterSlope::Db12,
            2 => FilterSlope::Db18,
            3 => FilterSlope::Db24,
            4 => FilterSlope::Db36,
            5 => FilterSlope::Db48,
            _ => FilterSlope::Db12,
        }
    }

    /// Convert to display string
    pub fn display_name(self) -> &'static str {
        match self {
            FilterSlope::Db6 => "6 dB/Oct",
            FilterSlope::Db12 => "12 dB/Oct",
            FilterSlope::Db18 => "18 dB/Oct",
            FilterSlope::Db24 => "24 dB/Oct",
            FilterSlope::Db36 => "36 dB/Oct",
            FilterSlope::Db48 => "48 dB/Oct",
        }
    }
}

/// A single biquad filter section using Direct Form II Transposed
#[derive(Debug, Clone)]
pub struct BiquadFilter {
    // Coefficients
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    // State (Direct Form II Transposed)
    z1: f32,
    z2: f32,
}

impl BiquadFilter {
    /// Create a new biquad filter (passthrough by default)
    pub fn new() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    /// Set filter parameters using RBJ Audio EQ Cookbook formulas
    pub fn set_params(
        &mut self,
        filter_type: FilterType,
        freq: f32,
        gain_db: f32,
        q: f32,
        sample_rate: f32,
    ) {
        let freq = freq.clamp(10.0, sample_rate * 0.499);
        let q = q.max(0.01);
        let w0 = 2.0 * PI * freq / sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q);
        let a = 10.0_f32.powf(gain_db / 40.0); // sqrt of linear gain

        let (b0, b1, b2, a0, a1, a2) = match filter_type {
            FilterType::LowPass => {
                let b1 = 1.0 - cos_w0;
                let b0 = b1 / 2.0;
                let b2 = b0;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            FilterType::HighPass => {
                let b1 = -(1.0 + cos_w0);
                let b0 = (1.0 + cos_w0) / 2.0;
                let b2 = b0;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            FilterType::BandPass => {
                let b0 = alpha;
                let b1 = 0.0;
                let b2 = -alpha;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            FilterType::Notch => {
                let b0 = 1.0;
                let b1 = -2.0 * cos_w0;
                let b2 = 1.0;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            FilterType::AllPass => {
                let b0 = 1.0 - alpha;
                let b1 = -2.0 * cos_w0;
                let b2 = 1.0 + alpha;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            FilterType::PeakEq => {
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

        // Normalize by a0
        let inv_a0 = 1.0 / a0;
        self.b0 = b0 * inv_a0;
        self.b1 = b1 * inv_a0;
        self.b2 = b2 * inv_a0;
        self.a1 = a1 * inv_a0;
        self.a2 = a2 * inv_a0;
    }

    /// Process a single sample (Direct Form II Transposed)
    #[inline]
    pub fn process(&mut self, input: f32) -> f32 {
        let output = self.b0 * input + self.z1;
        self.z1 = self.b1 * input - self.a1 * output + self.z2;
        self.z2 = self.b2 * input - self.a2 * output;
        output
    }

    /// Compute magnitude response at a given frequency (no FFT needed)
    /// Evaluates |H(e^jw)| directly from the transfer function
    pub fn magnitude_at(&self, freq: f32, sample_rate: f32) -> f32 {
        let w = 2.0 * PI * freq / sample_rate;
        let cos_w = w.cos();
        let cos_2w = (2.0 * w).cos();
        let sin_w = w.sin();
        let sin_2w = (2.0 * w).sin();

        // H(z) = (b0 + b1*z^-1 + b2*z^-2) / (1 + a1*z^-1 + a2*z^-2)
        // At z = e^(jw):
        let num_re = self.b0 + self.b1 * cos_w + self.b2 * cos_2w;
        let num_im = -(self.b1 * sin_w + self.b2 * sin_2w);
        let den_re = 1.0 + self.a1 * cos_w + self.a2 * cos_2w;
        let den_im = -(self.a1 * sin_w + self.a2 * sin_2w);

        let num_mag_sq = num_re * num_re + num_im * num_im;
        let den_mag_sq = den_re * den_re + den_im * den_im;

        if den_mag_sq < 1e-20 {
            1.0
        } else {
            (num_mag_sq / den_mag_sq).sqrt()
        }
    }

    /// Reset filter state
    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

impl Default for BiquadFilter {
    fn default() -> Self {
        Self::new()
    }
}

/// Cascaded biquad filters for steeper HP/LP slopes
///
/// Uses Butterworth Q values for cascaded sections to achieve
/// maximally flat magnitude response.
#[derive(Debug, Clone)]
pub struct BiquadCascade {
    sections: Vec<BiquadFilter>,
}

impl BiquadCascade {
    /// Create a new cascade with the given number of sections
    pub fn new(num_sections: usize) -> Self {
        Self {
            sections: vec![BiquadFilter::new(); num_sections.max(1)],
        }
    }

    /// Butterworth Q values for cascaded second-order sections
    fn butterworth_q_values(order: usize) -> Vec<f32> {
        match order {
            1 => vec![std::f32::consts::FRAC_1_SQRT_2],              // single section, Q ≈ 1/sqrt(2)
            2 => vec![std::f32::consts::FRAC_1_SQRT_2],              // 2nd order Butterworth
            3 => vec![1.0, 0.5],            // 3rd order (one 2nd-order + one 1st-order approx)
            4 => vec![0.5412, 1.3065],      // 4th order Butterworth
            6 => vec![0.5176, std::f32::consts::FRAC_1_SQRT_2, 1.9319], // 6th order Butterworth
            8 => vec![0.5098, 0.6013, 0.8999, 2.5629], // 8th order Butterworth
            _ => vec![std::f32::consts::FRAC_1_SQRT_2; order.div_ceil(2)],
        }
    }

    /// Configure as a high-pass filter with the given slope
    pub fn set_hp(&mut self, freq: f32, slope: FilterSlope, sample_rate: f32) {
        let num_sections = slope.num_sections();
        self.resize(num_sections);

        let order = match slope {
            FilterSlope::Db6 => 1,
            FilterSlope::Db12 => 2,
            FilterSlope::Db18 => 3,
            FilterSlope::Db24 => 4,
            FilterSlope::Db36 => 6,
            FilterSlope::Db48 => 8,
        };

        let q_values = Self::butterworth_q_values(order);

        for (i, section) in self.sections.iter_mut().enumerate() {
            let q = q_values.get(i).copied().unwrap_or(std::f32::consts::FRAC_1_SQRT_2);
            section.set_params(FilterType::HighPass, freq, 0.0, q, sample_rate);
        }
    }

    /// Configure as a low-pass filter with the given slope
    pub fn set_lp(&mut self, freq: f32, slope: FilterSlope, sample_rate: f32) {
        let num_sections = slope.num_sections();
        self.resize(num_sections);

        let order = match slope {
            FilterSlope::Db6 => 1,
            FilterSlope::Db12 => 2,
            FilterSlope::Db18 => 3,
            FilterSlope::Db24 => 4,
            FilterSlope::Db36 => 6,
            FilterSlope::Db48 => 8,
        };

        let q_values = Self::butterworth_q_values(order);

        for (i, section) in self.sections.iter_mut().enumerate() {
            let q = q_values.get(i).copied().unwrap_or(std::f32::consts::FRAC_1_SQRT_2);
            section.set_params(FilterType::LowPass, freq, 0.0, q, sample_rate);
        }
    }

    /// Process a single sample through all cascaded sections
    #[inline]
    pub fn process(&mut self, mut input: f32) -> f32 {
        for section in &mut self.sections {
            input = section.process(input);
        }
        input
    }

    /// Compute combined magnitude response at a given frequency
    pub fn magnitude_at(&self, freq: f32, sample_rate: f32) -> f32 {
        let mut mag = 1.0_f32;
        for section in &self.sections {
            mag *= section.magnitude_at(freq, sample_rate);
        }
        mag
    }

    /// Reset all filter states
    pub fn reset(&mut self) {
        for section in &mut self.sections {
            section.reset();
        }
    }

    fn resize(&mut self, num_sections: usize) {
        let num = num_sections.max(1);
        self.sections.resize_with(num, BiquadFilter::new);
        self.sections.truncate(num);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_biquad_passthrough() {
        let mut filter = BiquadFilter::new();
        // Default filter is passthrough (b0=1, all others 0)
        assert!((filter.process(1.0) - 1.0).abs() < 1e-6);
        assert!((filter.process(0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_peak_eq_unity_at_zero_gain() {
        let mut filter = BiquadFilter::new();
        filter.set_params(FilterType::PeakEq, 1000.0, 0.0, 1.0, 44100.0);

        // At 0 dB gain, PeakEq should be unity
        let mag = filter.magnitude_at(1000.0, 44100.0);
        assert!(
            (mag - 1.0).abs() < 0.01,
            "Expected ~1.0, got {mag}"
        );
    }

    #[test]
    fn test_peak_eq_boost() {
        let mut filter = BiquadFilter::new();
        filter.set_params(FilterType::PeakEq, 1000.0, 12.0, 1.0, 44100.0);

        // At the center frequency, magnitude should be boosted
        let mag = filter.magnitude_at(1000.0, 44100.0);
        let mag_db = 20.0 * mag.log10();
        assert!(
            (mag_db - 12.0).abs() < 0.5,
            "Expected ~12 dB, got {mag_db} dB"
        );

        // Far from center, should be near unity
        let mag_far = filter.magnitude_at(50.0, 44100.0);
        assert!(
            (mag_far - 1.0).abs() < 0.1,
            "Expected ~1.0 far from center, got {mag_far}"
        );
    }

    #[test]
    fn test_lowpass_attenuates_high_freq() {
        let mut filter = BiquadFilter::new();
        filter.set_params(FilterType::LowPass, 1000.0, 0.0, std::f32::consts::FRAC_1_SQRT_2, 44100.0);

        let mag_low = filter.magnitude_at(100.0, 44100.0);
        let mag_high = filter.magnitude_at(10000.0, 44100.0);

        assert!(
            mag_low > mag_high,
            "Low-pass should attenuate high frequencies: low={mag_low}, high={mag_high}"
        );
    }

    #[test]
    fn test_highpass_attenuates_low_freq() {
        let mut filter = BiquadFilter::new();
        filter.set_params(FilterType::HighPass, 1000.0, 0.0, std::f32::consts::FRAC_1_SQRT_2, 44100.0);

        let mag_low = filter.magnitude_at(100.0, 44100.0);
        let mag_high = filter.magnitude_at(10000.0, 44100.0);

        assert!(
            mag_high > mag_low,
            "High-pass should attenuate low frequencies: low={mag_low}, high={mag_high}"
        );
    }

    #[test]
    fn test_low_shelf() {
        let mut filter = BiquadFilter::new();
        filter.set_params(FilterType::LowShelf, 200.0, 6.0, 0.71, 44100.0);

        let mag_below = filter.magnitude_at(50.0, 44100.0);
        let mag_above = filter.magnitude_at(5000.0, 44100.0);

        assert!(
            mag_below > mag_above,
            "Low shelf boost: below={mag_below}, above={mag_above}"
        );
    }

    #[test]
    fn test_high_shelf() {
        let mut filter = BiquadFilter::new();
        filter.set_params(FilterType::HighShelf, 5000.0, 6.0, 0.71, 44100.0);

        let mag_below = filter.magnitude_at(100.0, 44100.0);
        let mag_above = filter.magnitude_at(15000.0, 44100.0);

        assert!(
            mag_above > mag_below,
            "High shelf boost: below={mag_below}, above={mag_above}"
        );
    }

    #[test]
    fn test_cascade_steeper_slope() {
        let mut cascade_12 = BiquadCascade::new(1);
        cascade_12.set_lp(1000.0, FilterSlope::Db12, 44100.0);

        let mut cascade_48 = BiquadCascade::new(4);
        cascade_48.set_lp(1000.0, FilterSlope::Db48, 44100.0);

        let mag_12 = cascade_12.magnitude_at(5000.0, 44100.0);
        let mag_48 = cascade_48.magnitude_at(5000.0, 44100.0);

        assert!(
            mag_48 < mag_12,
            "48dB/oct should attenuate more than 12dB/oct: 12={mag_12}, 48={mag_48}"
        );
    }

    #[test]
    fn test_filter_reset() {
        let mut filter = BiquadFilter::new();
        filter.set_params(FilterType::LowPass, 1000.0, 0.0, std::f32::consts::FRAC_1_SQRT_2, 44100.0);

        // Process some samples
        for _ in 0..100 {
            filter.process(1.0);
        }

        filter.reset();
        assert_eq!(filter.z1, 0.0);
        assert_eq!(filter.z2, 0.0);
    }

    #[test]
    fn test_process_produces_finite_output() {
        let mut filter = BiquadFilter::new();
        filter.set_params(FilterType::PeakEq, 1000.0, 12.0, 2.0, 44100.0);

        for i in 0..1000 {
            let input = if i == 0 { 1.0 } else { 0.0 };
            let output = filter.process(input);
            assert!(output.is_finite(), "Output not finite at sample {i}");
        }
    }
}
