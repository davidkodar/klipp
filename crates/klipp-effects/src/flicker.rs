//! Flicker (#989): the level a Flicker effect jumps to at a time, and
//! the plain effect that level is. The CPU and the GPU both run that
//! plain effect (opacity, a gain, or a gamma), so they agree by
//! construction.

use crate::BuiltinEffect;

/// A random number in 0..1 for jump `i` of pattern `seed`, `salt`
/// picking one of several streams.
fn noise(seed: u32, i: i64, salt: u32) -> f32 {
    let mut h = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (u64::from(seed) << 32 | u64::from(salt)).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^= h >> 33;
    h = h.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    h ^= h >> 33;
    (h >> 40) as f32 / (1u64 << 24) as f32
}

/// The size of jump `i`, 0..1: none for a jump that holds.
fn jump(seed: u32, i: i64, hold: f32) -> f32 {
    if noise(seed, i, 1) < hold { 0.0 } else { noise(seed, i, 0) }
}

/// How far the flicker is jumped at `time` seconds, 0..1: `speed` jumps
/// a second, `hold` the share that hold still, and `smoothness` the mix
/// between cutting from jump to jump (0) and a smooth wave through the
/// jumps, each at its middle (1).
pub fn level(speed: f32, smoothness: f32, hold: f32, seed: u32, time: f32) -> f32 {
    let n = (time.max(0.0) * speed.max(0.0)) as f64;
    let cut = jump(seed, n.floor() as i64, hold);
    let s = smoothness.clamp(0.0, 1.0);
    if s <= 0.0 {
        return cut;
    }
    // The wave: eased from one jump's middle to the next's.
    let c = n - 0.5;
    let k = c.floor() as i64;
    let u = (c - c.floor()) as f32;
    let (a, b) = (jump(seed, k, hold), jump(seed, k + 1, hold));
    let wave = a + (b - a) * u * u * (3.0 - 2.0 * u);
    cut + (wave - cut) * s
}

/// The plain effect a Flicker is at its time, dimmed by its level: its
/// opacity down by up to `amount`, its light down by up to three times
/// `amount` stops, or its midtones down by a gamma of up to `amount`
/// stops. At level 0 each is at rest. `None` for any other effect.
pub fn as_plain(effect: &BuiltinEffect) -> Option<BuiltinEffect> {
    let BuiltinEffect::Flicker { amount, speed, smoothness, hold, seed, target, time } = *effect else { return None };
    let v = level(speed, smoothness, hold, seed, time);
    Some(match target {
        0 => BuiltinEffect::Opacity { value: (1.0 - amount * v).clamp(0.0, 1.0) },
        1 => BuiltinEffect::Deflicker { strength: 1.0, window_secs: 1.0, gain: 2f32.powf(-3.0 * amount * v) },
        _ => BuiltinEffect::Gamma { gamma: 2f32.powf(-amount * v) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The level stays in 0..1, holds within a jump when not smoothed,
    /// is the same for the same time, and changes with the seed.
    #[test]
    fn the_level_jumps_and_holds() {
        let at = |t: f32, seed: u32, smooth: f32| level(8.0, smooth, 0.0, seed, t);
        let samples: Vec<f32> = (0..400).map(|k| at(k as f32 / 100.0, 3, 0.0)).collect();
        assert!(samples.iter().all(|v| (0.0..=1.0).contains(v)));
        assert_eq!(at(0.01, 3, 0.0), at(0.11, 3, 0.0), "one jump lasts an eighth of a second");
        assert_ne!(at(0.01, 3, 0.0), at(0.14, 3, 0.0), "the next is another");
        assert_ne!(at(0.5, 3, 0.0), at(0.5, 4, 0.0), "another seed, another pattern");
        let distinct = samples.windows(2).filter(|w| w[0] != w[1]).count();
        assert!((25..=33).contains(&distinct), "about eight jumps a second over four seconds: {distinct}");
        // Smoothed, it moves between jumps.
        assert_ne!(at(0.11, 3, 1.0), at(0.12, 3, 1.0));
        // Held throughout, nothing moves.
        assert!((0..100).all(|k| level(8.0, 0.5, 1.0, 3, k as f32 * 0.05) == 0.0));
    }

    /// Each target is its plain effect, at rest when the level is.
    #[test]
    fn each_target_is_a_plain_effect() {
        let f = |target, time| BuiltinEffect::Flicker { amount: 0.8, speed: 8.0, smoothness: 0.0, hold: 0.0, seed: 3, target, time };
        let v = level(8.0, 0.0, 0.0, 3, 0.3);
        assert_eq!(as_plain(&f(0, 0.3)), Some(BuiltinEffect::Opacity { value: 1.0 - 0.8 * v }));
        assert!(v > 0.0);
        assert!(matches!(as_plain(&f(1, 0.3)), Some(BuiltinEffect::Deflicker { gain, .. }) if (gain - 2f32.powf(-2.4 * v)).abs() < 1e-6));
        assert!(matches!(as_plain(&f(2, 0.3)), Some(BuiltinEffect::Gamma { gamma }) if gamma < 1.0));
        let held = BuiltinEffect::Flicker { amount: 0.8, speed: 8.0, smoothness: 0.0, hold: 1.0, seed: 3, target: 1, time: 0.3 };
        assert!(matches!(as_plain(&held), Some(BuiltinEffect::Deflicker { gain, .. }) if gain == 1.0), "a held jump leaves the light");
        assert_eq!(as_plain(&BuiltinEffect::Opacity { value: 0.5 }), None);
    }
}
