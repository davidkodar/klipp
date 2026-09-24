use std::f32::consts::PI;

use crate::modifier::Waveform;

/// Seeded noise for deterministic Wiggle behavior.
/// Uses a simple hash-based approach (no external crate needed).
pub fn wiggle_noise(time_secs: f64, frequency: f32, amplitude: f32, seed: u64) -> f32 {
    // Use smoothed random sampling at frequency intervals
    // Each "knot" is a random value, interpolated smoothly between knots
    let t = time_secs as f32 * frequency;
    let index = t.floor() as i64;
    let frac = t - t.floor();

    // Smooth interpolation (cubic hermite / smoothstep)
    let frac = frac * frac * (3.0 - 2.0 * frac);

    let v0 = hash_to_float(seed, index - 1);
    let v1 = hash_to_float(seed, index);
    let v2 = hash_to_float(seed, index + 1);
    let v3 = hash_to_float(seed, index + 2);

    // Catmull-Rom interpolation for smooth curves
    let a = -0.5 * v0 + 1.5 * v1 - 1.5 * v2 + 0.5 * v3;
    let b = v0 - 2.5 * v1 + 2.0 * v2 - 0.5 * v3;
    let c = -0.5 * v0 + 0.5 * v2;
    let d = v1;

    amplitude * (a * frac * frac * frac + b * frac * frac + c * frac + d)
}

/// Hash a seed + index to a float in [-1, 1]
fn hash_to_float(seed: u64, index: i64) -> f32 {
    let mut h = seed.wrapping_mul(0x517cc1b727220a95);
    h = h.wrapping_add(index as u64);
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51afd7ed558ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ceb9fe1a85ec53);
    h ^= h >> 33;
    // Map to [-1, 1]
    (h as i64 as f32) / (i64::MAX as f32)
}

/// Generate waveform value at time t
pub fn waveform(t: f32, frequency: f32, phase: f32, shape: Waveform) -> f32 {
    let cycle = (t * frequency + phase).rem_euclid(1.0);
    match shape {
        Waveform::Sine => (cycle * 2.0 * PI).sin(),
        Waveform::Square => {
            if cycle < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        Waveform::Triangle => {
            if cycle < 0.25 {
                cycle * 4.0
            } else if cycle < 0.75 {
                2.0 - cycle * 4.0
            } else {
                cycle * 4.0 - 4.0
            }
        }
        Waveform::Sawtooth => cycle * 2.0 - 1.0,
    }
}

/// Analytical damped spring (for future Spring modifier)
pub fn spring_value(time_secs: f64, target: f32, stiffness: f32, damping: f32) -> f32 {
    let t = time_secs as f32;
    let omega = stiffness.sqrt();
    let zeta = damping / (2.0 * omega);
    if zeta < 1.0 {
        // Underdamped - oscillates
        let omega_d = omega * (1.0 - zeta * zeta).sqrt();
        let envelope = (-zeta * omega * t).exp();
        target
            * (1.0
                - envelope
                    * ((zeta * omega * t).cos() + (zeta * omega / omega_d) * (omega_d * t).sin()))
    } else {
        // Overdamped or critically damped - no oscillation
        let r = (-zeta * omega * t).exp();
        target * (1.0 - r * (1.0 + omega * t))
    }
}
