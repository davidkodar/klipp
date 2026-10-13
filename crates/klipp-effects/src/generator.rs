//! A picture made, not changed (#1746): a solid colour or a gradient that
//! replaces whatever it is given, the foreground a Merge lays over a
//! clip's picture when it stands beside the picture's chain.
//!
//! The CPU here and the GPU's pass compute each pixel by the same formula
//! from the same numbers: the colours decoded from their sRGB bytes here,
//! before either runs.

use rayon::prelude::*;

/// The shapes a generator draws, in the order its `shape` numbers them.
pub const SHAPES: &[&str] = &["Solid", "Linear gradient", "Radial gradient"];

/// An authored colour (sRGB bytes, straight alpha) in linear light.
pub fn linear(c: [u8; 4]) -> [f32; 4] {
    let d = |v: u8| crate::srgb_u8_to_linear_f32_rgba(&[v, 0, 0, 255])[0];
    [d(c[0]), d(c[1]), d(c[2]), f32::from(c[3]) / 255.0]
}

/// A generator's settings, worked out for a frame `width` by `height`:
/// what the CPU and the GPU's pass both draw from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gen {
    pub shape: u32,
    pub a: [f32; 4],
    pub b: [f32; 4],
    /// The gradient's direction, a unit vector.
    pub dir: [f32; 2],
    /// Its centre, in pixels.
    pub centre: [f32; 2],
    /// A radial gradient's radius, and a linear one's half length, in
    /// pixels.
    pub radius: f32,
}

impl Gen {
    #[allow(clippy::too_many_arguments)]
    pub fn new(shape: u32, color_a: [u8; 4], color_b: [u8; 4], angle: f32, center_x: f32, center_y: f32, radius: f32, width: u32, height: u32) -> Self {
        let (w, h) = (width as f32, height as f32);
        let (s, c) = angle.to_radians().sin_cos();
        Gen { shape, a: linear(color_a), b: linear(color_b), dir: [c, s], centre: [center_x * w, center_y * h], radius: (radius * h).max(1e-3) }
    }

    /// The colour at pixel `(x, y)`, its centre sampled.
    pub fn at(&self, x: u32, y: u32) -> [f32; 4] {
        let (dx, dy) = (x as f32 + 0.5 - self.centre[0], y as f32 + 0.5 - self.centre[1]);
        let t = match self.shape {
            1 => 0.5 + (dx * self.dir[0] + dy * self.dir[1]) / (2.0 * self.radius),
            2 => (dx * dx + dy * dy).sqrt() / self.radius,
            _ => 0.0,
        }
        .clamp(0.0, 1.0);
        [0, 1, 2, 3].map(|i| self.a[i] + (self.b[i] - self.a[i]) * t)
    }
}

/// `g` over a whole frame, replacing it.
pub fn fill(frame: &mut [f32], g: &Gen, width: u32) {
    frame.par_chunks_exact_mut(4).enumerate().for_each(|(i, p)| {
        let i = i as u32;
        p.copy_from_slice(&g.at(i % width, i / width));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_linear_gradient_runs_from_its_first_colour_to_its_second() {
        // Across the whole width: a half length of half of it, 50 of 100.
        let g = Gen::new(1, [0, 0, 0, 255], [255, 255, 255, 255], 0.0, 0.5, 0.5, 1.0, 100, 50);
        assert!(g.at(0, 25)[0] < 0.01, "black at the left");
        assert!(g.at(99, 25)[0] > 0.98, "white at the right");
        let mid = g.at(49, 25)[0];
        assert!((mid - 0.5).abs() < 0.02, "half way in light: {mid}");
    }

    #[test]
    fn a_solid_is_its_first_colour_everywhere() {
        let g = Gen::new(0, [255, 0, 0, 128], [0, 0, 255, 255], 30.0, 0.2, 0.8, 0.3, 10, 10);
        assert_eq!(g.at(0, 0), g.at(9, 9));
        assert!((g.at(3, 3)[3] - 128.0 / 255.0).abs() < 1e-6);
    }
}
