//! Sound read at a speed, as tape plays it: faster and higher, slower and
//! lower, backward when the speed is negative (#642). The one reading
//! playback's PCM feeder and the export's mixdown both do, so a retimed
//! clip sounds the same in each and stays on its picture.

/// Zero crossings of the low-pass kernel either side of its centre, and
/// table points between two of them.
const ZEROS: usize = 24;
const PER_ZERO: usize = 256;

/// The low-pass kernel's right half, sinc(u) under a Kaiser window (beta
/// 9, some 90 dB down past the band), at u = i / PER_ZERO.
fn kernel() -> &'static [f32] {
    static K: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    K.get_or_init(|| {
        // The zeroth-order modified Bessel function, by its series.
        let i0 = |x: f64| {
            let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
            while term > sum * 1e-12 {
                term *= (x / (2.0 * k)) * (x / (2.0 * k));
                sum += term;
                k += 1.0;
            }
            sum
        };
        let beta = 9.0;
        (0..=ZEROS * PER_ZERO + 1)
            .map(|i| {
                let u = i as f64 / PER_ZERO as f64;
                let sinc = if u == 0.0 { 1.0 } else { (std::f64::consts::PI * u).sin() / (std::f64::consts::PI * u) };
                let r = u / ZEROS as f64;
                let w = if r >= 1.0 { 0.0 } else { i0(beta * (1.0 - r * r).sqrt()) / i0(beta) };
                (sinc * w) as f32
            })
            .collect()
    })
}

/// The kernel at `u` zero crossings from its centre.
fn tap(k: &[f32], u: f64) -> f32 {
    let x = u.abs() * PER_ZERO as f64;
    let i = x as usize;
    if i >= ZEROS * PER_ZERO {
        return 0.0;
    }
    let t = (x - i as f64) as f32;
    k[i] + (k[i + 1] - k[i]) * t
}

/// `n` interleaved frames of `samples` (`channels` to a frame), read from
/// frame position `pos` at `step` frames a frame; fewer when the end the
/// read is heading for comes first. Where it has not reached the sound
/// yet — read backward from past its end, or forward from before its
/// start — it is silence, and the read goes on into it: a reversed clip
/// starts at its source out, which for most camera files is the
/// picture's end, a few milliseconds past the sound's last sample, and
/// read from there its sound was nothing at all (#3219).
///
/// Up to 1x either way it reads by linear interpolation between the
/// frames either side. Faster, that would throw sound away above the
/// new half rate and fold it back down as aliasing (#651): 12 kHz and up
/// at 2x came back as a metallic whine. So each frame is read through a
/// low-pass at 0.45 of the rate over the speed, a windowed sinc as wide
/// as the speed asks.
pub fn read(samples: &[f32], channels: usize, pos: f64, step: f64, n: usize) -> Vec<f32> {
    let channels = channels.max(1);
    let frames = samples.len() / channels;
    let mut out = Vec::with_capacity(n * channels);
    if frames == 0 {
        return out;
    }
    let last = (frames - 1) as f64;
    // Outside the sound at `p`: `Some(true)` where the read has yet to
    // reach it (silence, and on), `Some(false)` past the end it heads
    // for (stop), `None` inside.
    let outside = |p: f64| {
        if (0.0..=last).contains(&p) {
            None
        } else {
            Some(if p > last { step < 0.0 } else { step > 0.0 })
        }
    };
    if step.abs() > 1.0 {
        // The kernel's u per frame of source, and how far it reaches.
        let per = 0.9 / step.abs();
        let reach = ZEROS as f64 / per;
        let k = kernel();
        let mut weights: Vec<f32> = Vec::new();
        let mut acc = vec![0.0f64; channels];
        for m in 0..n {
            let p = pos + m as f64 * step;
            match outside(p) {
                Some(true) => {
                    out.extend(std::iter::repeat_n(0.0, channels));
                    continue;
                }
                Some(false) => break,
                None => {}
            }
            let lo = (p - reach).ceil().max(0.0) as usize;
            let hi = ((p + reach).floor() as usize).min(frames - 1);
            weights.clear();
            weights.extend((lo..=hi).map(|j| tap(k, (p - j as f64) * per)));
            let total: f64 = weights.iter().map(|&w| w as f64).sum();
            acc.iter_mut().for_each(|a| *a = 0.0);
            for (w, frame) in weights.iter().zip(samples[lo * channels..(hi + 1) * channels].chunks_exact(channels)) {
                for (a, s) in acc.iter_mut().zip(frame) {
                    *a += *w as f64 * *s as f64;
                }
            }
            out.extend(acc.iter().map(|a| (a / total) as f32));
        }
        return out;
    }
    for k in 0..n {
        let p = pos + k as f64 * step;
        match outside(p) {
            Some(true) => {
                out.extend(std::iter::repeat_n(0.0, channels));
                continue;
            }
            Some(false) => break,
            None => {}
        }
        let i = p.floor() as usize;
        let t = (p - i as f64) as f32;
        let j = (i + 1).min(frames - 1);
        for ch in 0..channels {
            let a = samples[i * channels + ch];
            let b = samples[j * channels + ch];
            out.push(a + (b - a) * t);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #3219: read backward from past the sound's end, as a reversed clip
    /// whose picture outlasts its sound starts, it is silence up to the
    /// last sample and then the sound backward, not nothing; the same at
    /// 2x, through the low-pass. Forward from before the start likewise.
    /// Past the end it heads for, it still stops.
    #[test]
    fn a_read_from_beyond_the_sound_waits_for_it() {
        let ramp: Vec<f32> = (1..=10).flat_map(|i| [i as f32, -(i as f32)]).collect();
        let left = |v: Vec<f32>| v.chunks(2).map(|f| f[0]).collect::<Vec<_>>();
        let back = left(read(&ramp, 2, 12.0, -1.0, 100));
        assert_eq!(back[..3], [0.0, 0.0, 0.0], "past the end: silence");
        assert_eq!(back[3..], (1..=10).rev().map(|i| i as f32).collect::<Vec<_>>()[..], "then the sound, backward, to its start");
        let fast = left(read(&ramp, 2, 13.0, -2.0, 100));
        assert_eq!((fast.len(), fast[0], fast[1]), (7, 0.0, 0.0), "at 2x: two frames of silence, then five read: {fast:?}");
        assert!(fast[2..].iter().all(|v| *v > 0.5), "the sound itself at 2x: {fast:?}");
        let ahead = left(read(&ramp, 2, -2.0, 1.0, 4));
        assert_eq!(ahead, vec![0.0, 0.0, 1.0, 2.0], "forward from before the start");
        assert!(read(&ramp, 2, 12.0, 1.0, 5).is_empty(), "forward from past the end: nothing to wait for");
        assert!(read(&ramp, 2, -1.0, -1.0, 5).is_empty(), "backward from before the start: nothing either");
    }

    /// A ramp read at 0.5x reads halfway between its frames; backward from
    /// the end, the ramp the other way; at 2x it stops at the end.
    #[test]
    fn a_ramp_reads_at_its_speed_both_ways() {
        let ramp: Vec<f32> = (0..10).flat_map(|i| [i as f32, -(i as f32)]).collect();
        let left = |v: Vec<f32>| v.chunks(2).map(|f| f[0]).collect::<Vec<_>>();
        assert_eq!(read(&ramp, 2, 0.0, 2.0, 100).len(), 10);
        assert_eq!(left(read(&ramp, 2, 0.0, 0.5, 5)), vec![0.0, 0.5, 1.0, 1.5, 2.0]);
        assert_eq!(left(read(&ramp, 2, 9.0, -1.0, 100)), (0..10).rev().map(|i| i as f32).collect::<Vec<_>>());
        assert_eq!(read(&ramp, 2, 3.0, 1.0, 1), vec![3.0, -3.0], "channels kept apart");
        assert!(read(&[], 2, 0.0, 1.0, 4).is_empty());
    }

    /// Faster than 1x a ramp still reads at its speed, away from the ends
    /// where the low-pass runs out of sound, and stops at the ends.
    #[test]
    fn a_ramp_reads_at_its_speed_faster_too() {
        let ramp: Vec<f32> = (0..400).flat_map(|i| [i as f32, -(i as f32)]).collect();
        let got = read(&ramp, 2, 100.0, 2.0, 50);
        for (m, f) in got.chunks(2).enumerate() {
            let want = 100.0 + 2.0 * m as f32;
            assert!((f[0] - want).abs() < 0.01 && (f[1] + want).abs() < 0.01, "frame {m}: {f:?}");
        }
        assert_eq!(read(&ramp, 2, 0.0, 3.0, 1000).len() / 2, 134);
        assert_eq!(read(&ramp, 2, 399.0, -2.5, 1000).len() / 2, 160);
    }

    /// #651: at 2x a 16 kHz tone would land at 32 kHz, past the half rate,
    /// and fold back to 16 kHz; it is filtered out instead, below -60 dB,
    /// forward and backward. A 5 kHz tone, at 10 kHz after, passes whole.
    #[test]
    fn faster_sound_does_not_alias() {
        let tone = |hz: f64| -> Vec<f32> {
            (0..48_000).flat_map(|i| {
                let v = (2.0 * std::f64::consts::PI * hz * i as f64 / 48_000.0).sin() as f32;
                [v, v]
            }).collect()
        };
        // RMS of the left channel, away from the ends.
        let rms = |v: &[f32]| {
            let left: Vec<f32> = v.chunks(2).map(|f| f[0]).skip(500).collect();
            let left = &left[..left.len() - 500];
            (left.iter().map(|s| s * s).sum::<f32>() / left.len() as f32).sqrt()
        };
        let full = std::f32::consts::FRAC_1_SQRT_2;
        let db = |r: f32| 20.0 * (r / full).log10();
        for (pos, step) in [(0.0, 2.0), (47_999.0, -2.0), (0.0, 3.7)] {
            let high = read(&tone(16_000.0), 2, pos, step, 30_000);
            assert!(db(rms(&high)) < -60.0, "at {step}x a 16 kHz tone is {:.1} dB", db(rms(&high)));
        }
        let low = read(&tone(5_000.0), 2, 0.0, 2.0, 24_000);
        assert!(db(rms(&low)).abs() < 0.1, "a 5 kHz tone at 2x: {:.2} dB", db(rms(&low)));
    }
}
