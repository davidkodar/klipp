//! Streaming sample-rate conversion for effects that only run at one rate.
//!
//! A windowed-sinc resampler with a precomputed polyphase table. It is
//! causal and has a fixed delay: output sample `n` is the input at time
//! `n * in_rate / out_rate - delay_in` (in input samples), and it is emitted
//! as soon as the input sample it needs has arrived. That fixed delay is
//! what lets an effect built on it report an exact latency.

use std::collections::VecDeque;

/// Zero crossings of the sinc on each side, at the narrower of the two
/// rates. 24 keeps the pass band flat to within a fraction of a dB and
/// images below -80 dB.
const ZERO_CROSSINGS: usize = 24;
/// Pass band edge as a fraction of the lower Nyquist frequency.
const PASS: f64 = 0.96;
/// More phases than this means an awkward rate pair; refuse it.
const MAX_PHASES: usize = 8192;

pub struct Resampler {
    /// Input and output rates divided by their greatest common divisor.
    step_in: u64,
    phases: u64,
    /// Half-width of the kernel in input samples; also the delay.
    half: usize,
    /// `phases` rows of `2 * half` taps, each row summing to 1.
    table: Vec<f32>,
    /// Extra delay in 1/`phases` of an input sample, on the table's grid.
    offset: u64,
    history: VecDeque<f32>,
    /// Absolute index of `history[0]` in the input stream.
    history_start: u64,
    /// Input samples received so far.
    received: u64,
    /// Output samples produced so far.
    produced: u64,
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn blackman(x: f64) -> f64 {
    // x in [0, 1]
    let t = 2.0 * std::f64::consts::PI * x;
    0.42 - 0.5 * t.cos() + 0.08 * (2.0 * t).cos()
}

impl Resampler {
    /// A resampler from `in_rate` to `out_rate`, or `None` for rates whose
    /// ratio needs an unreasonably large table.
    pub fn new(in_rate: u32, out_rate: u32) -> Option<Self> {
        if in_rate == 0 || out_rate == 0 {
            return None;
        }
        let g = gcd(in_rate as u64, out_rate as u64);
        let step_in = in_rate as u64 / g;
        let phases = out_rate as u64 / g;
        if phases as usize > MAX_PHASES {
            return None;
        }
        // Low-pass at the lower of the two Nyquists, in cycles per input sample.
        let ratio = out_rate as f64 / in_rate as f64;
        let cutoff = 0.5 * ratio.min(1.0) * PASS;
        let half = (ZERO_CROSSINGS as f64 / ratio.min(1.0)).ceil() as usize;
        let taps = 2 * half;
        let mut table = vec![0.0f32; phases as usize * taps];
        for r in 0..phases as usize {
            let frac = r as f64 / phases as f64;
            let row = &mut table[r * taps..(r + 1) * taps];
            let mut sum = 0.0f64;
            let mut values = vec![0.0f64; taps];
            for (j, v) in values.iter_mut().enumerate() {
                // Tap j reads input q + 1 - 2*half + j; the wanted time is
                // q + frac - half, so the distance in input samples is:
                let x = frac + half as f64 - 1.0 - j as f64;
                let arg = 2.0 * cutoff * x;
                let sinc = if arg.abs() < 1e-12 { 1.0 } else { (std::f64::consts::PI * arg).sin() / (std::f64::consts::PI * arg) };
                let w = blackman(((x + half as f64) / (2.0 * half as f64)).clamp(0.0, 1.0));
                *v = 2.0 * cutoff * sinc * w;
                sum += *v;
            }
            for (dst, v) in row.iter_mut().zip(&values) {
                *dst = (*v / sum) as f32;
            }
        }
        Some(Self {
            step_in,
            phases,
            half,
            table,
            offset: 0,
            history: VecDeque::new(),
            history_start: 0,
            received: 0,
            produced: 0,
        })
    }

    /// Whole input samples of delay between the input and the output
    /// (before any extra fractional delay).
    pub fn delay_in_samples(&self) -> usize {
        self.half
    }

    /// The finest step an extra delay can take, in input samples.
    pub fn delay_step(&self) -> f64 {
        1.0 / self.phases as f64
    }

    /// Add `steps` of [`Self::delay_step`] to the delay, so a chain of
    /// resamplers can land on a whole number of samples. Set before use.
    pub fn with_extra_delay(mut self, steps: u64) -> Self {
        self.offset = steps;
        self
    }

    /// Total delay in input samples, fractional part included.
    pub fn delay(&self) -> f64 {
        self.half as f64 + self.offset as f64 / self.phases as f64
    }

    /// Feed input samples and append every output sample they complete.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.history.extend(input.iter().copied());
        self.received += input.len() as u64;
        let taps = 2 * self.half;
        loop {
            // Output n sits at input time n * step_in / phases - half; it
            // needs input up to index q = floor(n * step_in / phases).
            let pos = (self.produced * self.step_in) as i64 - self.offset as i64;
            let q = pos.div_euclid(self.phases as i64);
            let r = pos.rem_euclid(self.phases as i64) as usize;
            if q >= self.received as i64 {
                break;
            }
            let row = &self.table[r * taps..(r + 1) * taps];
            // Taps read input indices q + 1 - taps ..= q (zero before the start).
            let first = q + 1 - taps as i64;
            let mut acc = 0.0f32;
            for (j, c) in row.iter().enumerate() {
                let idx = first + j as i64;
                if idx >= self.history_start as i64 {
                    acc += c * self.history[(idx as u64 - self.history_start) as usize];
                }
            }
            out.push(acc);
            self.produced += 1;
        }
        // Keep only what future outputs can still read.
        let next_first = ((self.produced * self.step_in) as i64 - self.offset as i64).div_euclid(self.phases as i64) + 1 - taps as i64;
        while (self.history_start as i64) < next_first && !self.history.is_empty() {
            self.history.pop_front();
            self.history_start += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, hz: f32, secs: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize).map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin() * 0.5).collect()
    }

    fn run(r: &mut Resampler, input: &[f32], blocks: &[usize]) -> Vec<f32> {
        let mut out = Vec::new();
        let mut pos = 0;
        for b in blocks.iter().cycle() {
            if pos >= input.len() { break; }
            let end = (pos + b).min(input.len());
            r.push(&input[pos..end], &mut out);
            pos = end;
        }
        out
    }

    #[test]
    fn a_tone_arrives_at_the_new_rate_delayed_by_the_reported_delay() {
        for (a, b) in [(44_100u32, 48_000u32), (48_000, 44_100), (32_000, 48_000), (96_000, 48_000)] {
            let mut r = Resampler::new(a, b).unwrap();
            let input = tone(a, 1000.0, 1.0);
            let out = run(&mut r, &input, &[4096]);
            // Output n is the input at n * a / b - delay, in input samples.
            let delay = r.delay_in_samples() as f64;
            let mut err = 0.0f64;
            let mut n_checked = 0;
            for (n, y) in out.iter().enumerate().skip(b as usize / 10).take(b as usize / 2) {
                let t = (n as f64 * a as f64 / b as f64 - delay) / a as f64;
                let want = (2.0 * std::f64::consts::PI * 1000.0 * t).sin() * 0.5;
                err += (*y as f64 - want).powi(2);
                n_checked += 1;
            }
            let rms_err = (err / n_checked as f64).sqrt();
            assert!(rms_err < 1e-3, "{a}->{b}: error {rms_err}");
        }
    }

    #[test]
    fn an_extra_delay_shifts_the_output_by_exactly_that_much() {
        let (a, b) = (48_000u32, 44_100u32);
        let r = Resampler::new(a, b).unwrap().with_extra_delay(37);
        let delay = r.delay();
        let mut r = r;
        let input = tone(a, 700.0, 1.0);
        let out = run(&mut r, &input, &[512]);
        let mut err = 0.0f64;
        for (n, y) in out.iter().enumerate().skip(4410).take(22_050) {
            let t = (n as f64 * a as f64 / b as f64 - delay) / a as f64;
            err += (*y as f64 - (2.0 * std::f64::consts::PI * 700.0 * t).sin() * 0.5).powi(2);
        }
        assert!((err / 22_050.0).sqrt() < 1e-3);
    }

    #[test]
    fn block_sizes_do_not_change_the_output() {
        let input = tone(44_100, 440.0, 0.5);
        let whole = run(&mut Resampler::new(44_100, 48_000).unwrap(), &input, &[input.len()]);
        let pieces = run(&mut Resampler::new(44_100, 48_000).unwrap(), &input, &[1, 37, 480, 2048, 3]);
        assert_eq!(whole, pieces);
    }

    #[test]
    fn output_keeps_pace_with_input() {
        let mut r = Resampler::new(44_100, 48_000).unwrap();
        let mut out = Vec::new();
        r.push(&vec![0.0; 44_100], &mut out);
        assert!((out.len() as i64 - 48_000).abs() <= 1, "{}", out.len());
    }
}
