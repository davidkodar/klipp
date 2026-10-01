//! Sound read at a speed, as tape plays it: faster and higher, slower and
//! lower, backward when the speed is negative (#642). The one reading
//! playback's PCM feeder and the export's mixdown both do, so a retimed
//! clip sounds the same in each and stays on its picture.

/// `n` interleaved frames of `samples` (`channels` to a frame), read from
/// frame position `pos` at `step` frames a frame, by linear interpolation
/// between the frames either side; fewer when an end of the sound comes
/// first.
pub fn read(samples: &[f32], channels: usize, pos: f64, step: f64, n: usize) -> Vec<f32> {
    let channels = channels.max(1);
    let frames = samples.len() / channels;
    let mut out = Vec::with_capacity(n * channels);
    if frames == 0 {
        return out;
    }
    let last = (frames - 1) as f64;
    for k in 0..n {
        let p = pos + k as f64 * step;
        if !(0.0..=last).contains(&p) {
            break;
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

    /// A ramp read at 2x takes every other frame; at 0.5x halfway between
    /// them; backward from the end, the ramp the other way; and it stops
    /// at the ends.
    #[test]
    fn a_ramp_reads_at_its_speed_both_ways() {
        let ramp: Vec<f32> = (0..10).flat_map(|i| [i as f32, -(i as f32)]).collect();
        let left = |v: Vec<f32>| v.chunks(2).map(|f| f[0]).collect::<Vec<_>>();
        assert_eq!(left(read(&ramp, 2, 0.0, 2.0, 100)), vec![0.0, 2.0, 4.0, 6.0, 8.0]);
        assert_eq!(left(read(&ramp, 2, 0.0, 0.5, 5)), vec![0.0, 0.5, 1.0, 1.5, 2.0]);
        assert_eq!(left(read(&ramp, 2, 9.0, -1.0, 100)), (0..10).rev().map(|i| i as f32).collect::<Vec<_>>());
        assert_eq!(read(&ramp, 2, 3.0, 1.0, 1), vec![3.0, -3.0], "channels kept apart");
        assert!(read(&[], 2, 0.0, 1.0, 4).is_empty());
    }
}
