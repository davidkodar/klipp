//! The sRGB curves (IEC 61966-2-1), once for every crate: the decode
//! from an encoded sample to linear light and the encode back. Both run
//! on past 1.0, which the linear-light pipeline relies on: a value is
//! only clipped at the display. The WGSL twins are in
//! `klipp-effects/src/gpu/shaders/include/srgb_components.wgsl`.

/// An sRGB-encoded sample as linear light.
#[inline]
pub fn decode(v: f32) -> f32 {
    if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// Linear light as an sRGB-encoded sample.
#[inline]
pub fn encode(v: f32) -> f32 {
    if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

#[cfg(test)]
mod tests {
    use super::{decode, encode};

    /// The curve's own points, and each way undoes the other, past 1.0 too.
    #[test]
    fn the_curves_meet_their_points_and_undo_each_other() {
        assert_eq!(decode(0.0), 0.0);
        assert!((decode(1.0) - 1.0).abs() < 1e-6);
        assert!((encode(0.18) - 0.461_356).abs() < 1e-5, "{}", encode(0.18));
        assert!((encode(0.003_130_8) - 0.040_45).abs() < 1e-5);
        for v in [0.0, 0.01, 0.04045, 0.2, 0.5, 0.9, 1.0, 2.0, 8.0] {
            assert!((decode(encode(v)) - v).abs() < 1e-4 * v.max(1.0), "{v}");
        }
    }
}
