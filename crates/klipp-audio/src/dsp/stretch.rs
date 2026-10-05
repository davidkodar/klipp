//! Sound read at a speed with its pitch kept (#925): a clip slowed or
//! sped up sounds at its own pitch, longer or shorter, where
//! [`super::varispeed`] plays it as tape does.
//!
//! Waveform-similarity overlap-add: grain `k` lands at output frame
//! `k * HOP`, half a grain after the one before, and is read at normal
//! rate from near `anchor + k * HOP * step` in the source, shifted by up
//! to 400 frames (`TOLERANCE`) to where it best continues the grain before.
//! Two Hann windows overlap at every frame and sum to one, so a steady
//! sound keeps its level, and the shift keeps the overlapping grains in
//! phase, so it does not cancel itself.
//!
//! The grid is fixed to the clip's first output frame, so playback's
//! feeder and the export's mixdown place grains alike; a [`Stretcher`]
//! carries the last grain from one read to the next, and starts afresh
//! where a read does not follow on (a seek).

/// Grain length, 40 ms at 48 kHz, and the hop between grain starts.
const GRAIN: usize = 1920;
const HOP: usize = GRAIN / 2;
/// How far a grain may move to line up with the one before: half a
/// period of a 60 Hz voice.
const TOLERANCE: i64 = 400;

/// The Hann window at frame `i` of a grain.
fn window(i: usize) -> f32 {
    (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / GRAIN as f64).cos()) as f32
}

/// A clip's sound read at `step` source frames an output frame (above 0)
/// at its own pitch, its output frame 0 at source frame `anchor`.
pub struct Stretcher {
    anchor: f64,
    step: f64,
    /// Output made and not yet read, interleaved, from output frame `at`.
    ready: std::collections::VecDeque<f32>,
    at: i64,
    /// The next grain's index.
    grain: i64,
    /// The last grain's second half, windowed, to overlap the next.
    tail: Vec<f32>,
    /// Where the last grain would have gone on in the source: what the
    /// next one lines up with. None after a fresh start.
    follow: Option<i64>,
    /// The source ran out: no more grains.
    done: bool,
}

impl Stretcher {
    pub fn new(anchor: f64, step: f64) -> Self {
        Self { anchor, step, ready: Default::default(), at: i64::MIN, grain: 0, tail: Vec::new(), follow: None, done: false }
    }

    /// Whether it reads for this anchor and step.
    pub fn reads(&self, anchor: f64, step: f64) -> bool {
        self.anchor == anchor && self.step == step
    }

    /// `n` interleaved frames of `samples` (`channels` to a frame): output
    /// frames `first..first + n`. `first` may be below zero, for a
    /// crossfade's handle before the clip. Fewer when the source runs out.
    pub fn read(&mut self, samples: &[f32], channels: usize, first: i64, n: usize) -> Vec<f32> {
        let channels = channels.max(1);
        let frames = (samples.len() / channels) as i64;
        if frames < 2 || self.step <= 0.0 || !self.step.is_finite() {
            return Vec::new();
        }
        if first != self.at {
            // A fresh start, at the grain whose first half holds `first`.
            self.grain = first.div_euclid(HOP as i64);
            self.at = self.grain * HOP as i64;
            self.ready.clear();
            self.tail.clear();
            self.follow = None;
            self.done = false;
        }
        let want = (first - self.at) as usize + n;
        while self.ready.len() < want * channels && !self.done {
            self.grain_into_ready(samples, channels, frames);
        }
        // Frames before `first`, made by a fresh start's first grain.
        let skip = ((first - self.at) as usize * channels).min(self.ready.len());
        self.ready.drain(..skip);
        let take = (n * channels).min(self.ready.len());
        let out: Vec<f32> = self.ready.drain(..take).collect();
        self.at = first + (out.len() / channels) as i64;
        out
    }

    /// Make the next grain's first half, over the last one's tail.
    fn grain_into_ready(&mut self, samples: &[f32], channels: usize, frames: i64) {
        let nominal = (self.anchor + (self.grain * HOP as i64) as f64 * self.step).round() as i64;
        if nominal >= frames {
            self.done = true;
            return;
        }
        let start = match self.follow {
            Some(follow) => nominal + best_shift(samples, channels, frames, nominal, follow),
            None => nominal,
        };
        let at = |i: i64, ch: usize| if (0..frames).contains(&i) { samples[i as usize * channels + ch] } else { 0.0 };
        // After a fresh start, with no tail, the source as it is.
        for i in 0..HOP {
            for ch in 0..channels {
                let v = match self.tail.get(i * channels + ch) {
                    Some(t) => t + window(i) * at(start + i as i64, ch),
                    None => at(start + i as i64, ch),
                };
                self.ready.push_back(v);
            }
        }
        self.tail.clear();
        for i in HOP..GRAIN {
            for ch in 0..channels {
                self.tail.push(window(i) * at(start + i as i64, ch));
            }
        }
        self.follow = Some(start + HOP as i64);
        self.grain += 1;
    }
}

/// The shift within [`TOLERANCE`] that makes the source at `nominal` most
/// like the source at `follow`, the last grain's continuation, over a
/// hop, by normalised correlation of the channels' sum (every fourth
/// frame, which is plenty for the shape).
fn best_shift(samples: &[f32], channels: usize, frames: i64, nominal: i64, follow: i64) -> i64 {
    let mono = |i: i64| -> f32 { if (0..frames).contains(&i) { (0..channels).map(|ch| samples[i as usize * channels + ch]).sum() } else { 0.0 } };
    let reference: Vec<f32> = (0..HOP as i64).step_by(4).map(|i| mono(follow + i)).collect();
    let mut best = (0, f32::MIN);
    for d in -TOLERANCE..=TOLERANCE {
        let (mut dot, mut energy) = (0.0f32, 1e-9f32);
        for (j, r) in reference.iter().enumerate() {
            let v = mono(nominal + d + 4 * j as i64);
            dot += v * r;
            energy += v * v;
        }
        let score = dot / energy.sqrt();
        if score > best.1 {
            best = (d, score);
        }
    }
    best.0
}

#[cfg(test)]
mod tests {
    use super::Stretcher;

    fn tone(hz: f64, secs: f64) -> Vec<f32> {
        (0..(48_000.0 * secs) as usize)
            .flat_map(|i| {
                let v = (2.0 * std::f64::consts::PI * hz * i as f64 / 48_000.0).sin() as f32;
                [v, v]
            })
            .collect()
    }

    /// Zero crossings a second, from the left channel: twice the pitch.
    fn pitch(v: &[f32]) -> f64 {
        let left: Vec<f32> = v.chunks(2).map(|f| f[0]).collect();
        let n = left.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
        n as f64 / (left.len() as f64 / 48_000.0) / 2.0
    }

    /// The quietest 10 ms of the left channel, as RMS: a self-cancelling
    /// overlap dips far below the tone's 0.707.
    fn quietest(v: &[f32]) -> f32 {
        let left: Vec<f32> = v.chunks(2).map(|f| f[0]).collect();
        left.chunks(480).filter(|c| c.len() == 480).map(|c| (c.iter().map(|s| s * s).sum::<f32>() / 480.0).sqrt()).fold(f32::MAX, f32::min)
    }

    #[test]
    fn half_speed_is_twice_as_long_at_the_same_pitch_and_level() {
        let src = tone(440.0, 2.0);
        let out = Stretcher::new(0.0, 0.5).read(&src, 2, 0, 200_000);
        assert!(out.len() / 2 > 190_000, "about four seconds: {}", out.len() / 2);
        let body = &out[..180_000 * 2];
        assert!((pitch(body) - 440.0).abs() < 5.0, "pitch kept: {} Hz", pitch(body));
        assert!(quietest(body) > 0.6, "no dips: {}", quietest(body));
    }

    #[test]
    fn double_speed_is_half_as_long_at_the_same_pitch_and_level() {
        let src = tone(440.0, 2.0);
        let out = Stretcher::new(0.0, 2.0).read(&src, 2, 0, 200_000);
        assert!(((out.len() / 2) as i64 - 48_000).abs() < 1000, "about one second: {}", out.len() / 2);
        let body = &out[..40_000 * 2];
        assert!((pitch(body) - 440.0).abs() < 5.0, "pitch kept: {} Hz", pitch(body));
        assert!(quietest(body) > 0.6, "no dips: {}", quietest(body));
    }

    /// Read in blocks of any size, the samples are those of one read:
    /// playback asks in blocks, the export in one.
    #[test]
    fn blocks_read_the_same_samples_as_one_read() {
        let src = tone(300.0, 1.0);
        let whole = Stretcher::new(1000.0, 0.75).read(&src, 2, 0, 20_000);
        let mut s = Stretcher::new(1000.0, 0.75);
        let mut blocks = Vec::new();
        let mut at = 0;
        for size in [700usize, 960, 1, 3000, 1234].iter().cycle() {
            if at >= 20_000 {
                break;
            }
            let n = (*size).min(20_000 - at);
            blocks.extend(s.read(&src, 2, at as i64, n));
            at += n;
        }
        assert_eq!(whole.len(), blocks.len());
        assert_eq!(whole, blocks);
    }

    /// After a seek it starts afresh, and still sounds the tone.
    #[test]
    fn a_seek_starts_afresh() {
        let src = tone(440.0, 2.0);
        let mut s = Stretcher::new(0.0, 0.5);
        s.read(&src, 2, 0, 5000);
        let out = s.read(&src, 2, 50_000, 20_000);
        assert_eq!(out.len(), 40_000);
        assert!((pitch(&out) - 440.0).abs() < 5.0 && quietest(&out) > 0.6);
    }
}
