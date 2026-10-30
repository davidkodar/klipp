//! The sums aux sends and buses make (#2861), shared by the playback
//! mixer and the export mixdown so the two mix them alike.

/// `src` into `dest` at a gain moving from `from` to `to` across the
/// block, frame by frame, as a track's fader ramps: the last frame at
/// `to`. Held, the gain is `to` throughout.
pub fn add_ramped(src: &[f32], from: f32, to: f32, dest: &mut [f32]) {
    if from == to {
        for (d, s) in dest.iter_mut().zip(src) {
            *d += s * to;
        }
        return;
    }
    let frames = (src.len() / 2).max(1) as f32;
    for (j, (d, s)) in dest.chunks_mut(2).zip(src.chunks(2)).enumerate() {
        let g = from + (to - from) * ((j + 1) as f32 / frames);
        for (d, s) in d.iter_mut().zip(s) {
            *d += s * g;
        }
    }
}

/// A bus's sum into `dest` at its fader and its pan's gains; its peaks,
/// left and right, after them.
pub fn add_bus(bus: &[f32], volume: f32, (pan_l, pan_r): (f32, f32), dest: &mut [f32]) -> (f32, f32) {
    let (mut peak_l, mut peak_r) = (0.0f32, 0.0f32);
    for (d, s) in dest.chunks_mut(2).zip(bus.chunks(2)) {
        let out_l = s[0] * volume * pan_l;
        peak_l = peak_l.max(out_l.abs());
        d[0] += out_l;
        if let (Some(d1), Some(s1)) = (d.get_mut(1), s.get(1)) {
            let out_r = s1 * volume * pan_r;
            peak_r = peak_r.max(out_r.abs());
            *d1 += out_r;
        }
    }
    (peak_l, peak_r)
}
