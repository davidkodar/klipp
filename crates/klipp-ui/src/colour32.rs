//! See-through colours as Klipp has always drawn them.
//!
//! egui 0.29 premultiplied a colour's alpha in linear light: a yellow at
//! 15 % alpha kept about 107 of its 255 red. egui 0.30 and later multiply
//! the stored sRGB value instead, so the same colour keeps 38 and every
//! translucent overlay, band, tint and hover in Klipp drew several times
//! weaker (the volume band on an audio clip all but vanished). A guard
//! test keeps egui's own versions out of klipp-ui. These are
//! egui 0.29's conversions, so the panels look as they did.

use egui::ecolor::{gamma_u8_from_linear_f32, linear_f32_from_gamma_u8, linear_f32_from_linear_u8, linear_u8_from_linear_f32};
use egui::Color32;

/// A colour from sRGB bytes and a separate alpha, premultiplied in linear
/// light (egui 0.29's `Color32::from_rgba_unmultiplied`).
#[inline]
pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    match a {
        0 => Color32::TRANSPARENT,
        255 => Color32::from_rgb(r, g, b),
        a => {
            let alpha = linear_f32_from_linear_u8(a);
            let premul = |v: u8| gamma_u8_from_linear_f32(linear_f32_from_gamma_u8(v) * alpha);
            Color32::from_rgba_premultiplied(premul(r), premul(g), premul(b), a)
        }
    }
}

/// White at alpha `a`, premultiplied in linear light (egui 0.29's
/// `Color32::from_white_alpha`).
#[inline]
pub fn white(a: u8) -> Color32 {
    rgba(255, 255, 255, a)
}

/// `colour` with its light and alpha scaled by `factor` in linear light
/// (egui 0.29's `Color32::linear_multiply`).
#[inline]
pub fn linear_multiply(colour: Color32, factor: f32) -> Color32 {
    let [r, g, b, a] = colour.to_array();
    let scale = |v: u8| gamma_u8_from_linear_f32(linear_f32_from_gamma_u8(v) * factor);
    Color32::from_rgba_premultiplied(scale(r), scale(g), scale(b), linear_u8_from_linear_f32(linear_f32_from_linear_u8(a) * factor))
}

/// The sRGB bytes and alpha a colour made by [`rgba`] came from (egui
/// 0.29's `Color32::to_srgba_unmultiplied`).
#[inline]
pub fn unmultiplied(colour: Color32) -> [u8; 4] {
    let [r, g, b, a] = colour.to_array();
    match a {
        0 | 255 => [r, g, b, a],
        a => {
            let alpha = linear_f32_from_linear_u8(a);
            let un = |v: u8| gamma_u8_from_linear_f32(linear_f32_from_gamma_u8(v) / alpha);
            [un(r), un(g), un(b), a]
        }
    }
}

/// [`linear_multiply`] and [`unmultiplied`] as methods, in place of egui's.
pub trait Colour029 {
    /// [`linear_multiply`].
    fn lin_mul(self, factor: f32) -> Color32;
    /// [`unmultiplied`].
    fn unmul(self) -> [u8; 4];
}

impl Colour029 for Color32 {
    #[inline]
    fn lin_mul(self, factor: f32) -> Color32 {
        linear_multiply(self, factor)
    }
    #[inline]
    fn unmul(self) -> [u8; 4] {
        unmultiplied(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The values egui 0.29 gave, worked out by hand from its formula:
    /// the audio band's yellow at alpha 38 and a white at half.
    #[test]
    fn translucent_colours_are_premultiplied_in_linear_light() {
        let band = rgba(255, 195, 50, 38).to_array();
        assert_eq!(band[3], 38);
        // 255 at 38/255 alpha: sRGB of 0.149 linear is about 107.
        assert!((105..=109).contains(&band[0]), "{band:?}");
        assert_eq!(rgba(255, 255, 255, 128).to_array()[0], 188);
        assert_eq!(rgba(10, 20, 30, 255), Color32::from_rgb(10, 20, 30));
        assert_eq!(rgba(10, 20, 30, 0), Color32::TRANSPARENT);
        assert_eq!(white(128).to_array(), [188, 188, 188, 128]);
    }

    /// Unmultiplying gives back the bytes, to a step, for every alpha.
    #[test]
    fn unmultiplied_undoes_rgba() {
        for a in [1u8, 17, 38, 100, 128, 200, 254] {
            for v in [0u8, 40, 128, 200, 255] {
                let back = unmultiplied(rgba(v, v, v, a));
                assert_eq!(back[3], a);
                // A low alpha keeps few steps of the colour to come back to.
                let slack = if a < 40 { 40 } else { 2 };
                assert!(back[0].abs_diff(v) <= slack, "{v} at {a}: {back:?}");
            }
        }
    }

    /// egui's own see-through constructors draw weaker than Klipp always
    /// has; every one in klipp-ui goes through this module instead.
    #[test]
    fn no_panel_uses_eguis_own_translucent_colours() {
        let banned = [concat!("Color32::from_rgba_", "unmultiplied("), concat!("from_white_", "alpha("), concat!(".linear_", "multiply(")];
        let mut found = Vec::new();
        let mut dirs = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    for (n, line) in text.lines().enumerate() {
                        if banned.iter().any(|b| line.contains(b)) {
                            found.push(format!("{}:{}", path.display(), n + 1));
                        }
                    }
                }
            }
        }
        assert!(found.is_empty(), "use crate::colour32 instead: {found:#?}");
    }

    #[test]
    fn linear_multiply_scales_light_and_alpha() {
        let half = linear_multiply(Color32::WHITE, 0.5).to_array();
        assert_eq!(half, [188, 188, 188, 128]);
        assert_eq!(linear_multiply(Color32::from_rgb(1, 2, 3), 1.0), Color32::from_rgb(1, 2, 3));
    }
}
