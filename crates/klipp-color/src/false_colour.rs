//! False colour: the picture repainted by exposure, so a glance says
//! where the light is sitting.
//!
//! Every camera and every grading suite has one, and they all do the
//! same job: a colourist cannot judge exposure by looking at a picture,
//! because the eye adapts to whatever is in front of it. A waveform
//! answers the question but makes you find the part of the trace that
//! is the face. False colour paints the answer onto the face itself.
//!
//! **The band that matters is skin, and it is pink**, as it is on an
//! ARRI and on a Blackmagic — an operator lights until the cheek turns
//! pink, and that muscle memory is worth more than any scale invented
//! here. The colour is the convention; the *level* is not a fixed IRE
//! window but the `skin_l` of whatever look the piece is being held
//! to, which on a reference-derived look is a measurement of an actual
//! approved picture. So the false colour agrees with the skin
//! findings, the trend strip and the grading loop rather than being a
//! second opinion from a different era.
//!
//! The rest of the scale is the ordinary one: something for the black
//! clip, something for the white clip, greys in between, and a
//! deliberately ugly colour at each rail so that losing detail is
//! impossible to miss.

use crate::measure::LookTarget;

/// A band of the scale: the lightness at which it starts, its colour,
/// and what it means.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub from_l: f32,
    pub rgb: [u8; 3],
    pub meaning: &'static str,
}

/// How wide the skin band is, in L*. A face is not a single number —
/// the lit and shaded sides of one cheek differ by several L* — so the
/// band has to be wide enough to hold a whole face when it is right,
/// and narrow enough that a face a stop out falls outside it. Measured
/// over the reference portraits, the inter-quartile spread of a face's
/// own lightness runs 4 to 7 L*, so ±5 holds a face and not much else.
pub const SKIN_HALF_WIDTH: f32 = 5.0;

/// The scale for a look, lightest band last.
///
/// `skin_l` from the look when it names one; otherwise the band is
/// dropped rather than guessed, because a false colour that says "skin
/// is right here" when nothing measured said so is worse than none.
pub fn bands(look: &LookTarget) -> Vec<Band> {
    let mut out = vec![
        Band { from_l: 0.0, rgb: [120, 0, 160], meaning: "at black — detail gone" },
        Band { from_l: 3.0, rgb: [30, 40, 120], meaning: "deep shadow" },
        Band { from_l: 12.0, rgb: [0, 110, 150], meaning: "shadow" },
        Band { from_l: 30.0, rgb: [90, 90, 90], meaning: "mid grey" },
        Band { from_l: 70.0, rgb: [220, 220, 220], meaning: "highlight" },
        Band { from_l: 92.0, rgb: [240, 170, 40], meaning: "near white" },
        Band { from_l: 98.0, rgb: [230, 40, 40], meaning: "at white — detail gone" },
    ];
    if let Some(l) = look.skin_l {
        let lo = (l - SKIN_HALF_WIDTH).clamp(4.0, 96.0);
        let hi = (l + SKIN_HALF_WIDTH).clamp(lo + 1.0, 97.0);
        // The skin band sits *over* the greys: a face at the right
        // level turns green, and one either side stays grey.
        out.retain(|b| b.from_l < lo || b.from_l > hi);
        out.push(Band { from_l: lo, rgb: [235, 130, 170], meaning: "skin, exposed as the look asks" });
        // Back to whatever the scale was doing above the band.
        let above = if hi >= 70.0 { [220, 220, 220] } else { [90, 90, 90] };
        let meaning = if hi >= 70.0 { "highlight" } else { "mid grey" };
        out.push(Band { from_l: hi, rgb: above, meaning });
    }
    out.sort_by(|a, b| a.from_l.total_cmp(&b.from_l));
    out
}

/// The colour a lightness is painted.
pub fn colour_of(bands: &[Band], l: f32) -> [u8; 3] {
    let mut rgb = bands.first().map(|b| b.rgb).unwrap_or([0, 0, 0]);
    for b in bands {
        if l >= b.from_l {
            rgb = b.rgb;
        }
    }
    rgb
}

/// Repaint an sRGB frame by exposure. The output is sRGB RGBA, the same
/// size as the input, ready to be shown where the picture was.
pub fn repaint(rgba: &[u8], width: u32, height: u32, look: &LookTarget) -> Vec<u8> {
    let bands = bands(look);
    let n = (width as usize) * (height as usize);
    let mut out = vec![255u8; n * 4];
    for i in 0..n.min(rgba.len() / 4) {
        let p = i * 4;
        let l = crate::measure::srgb8_to_lab(rgba[p], rgba[p + 1], rgba[p + 2]).l;
        let c = colour_of(&bands, l);
        out[p] = c[0];
        out[p + 1] = c[1];
        out[p + 2] = c[2];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look_with_skin(l: f32) -> LookTarget {
        LookTarget { name: "test".into(), skin_l: Some(l), ..Default::default() }
    }

    #[test]
    fn a_face_at_the_looks_level_turns_the_skin_colour() {
        let look = look_with_skin(60.0);
        let b = bands(&look);
        let skin = colour_of(&b, 60.0);
        assert_eq!(skin, [235, 130, 170], "a face on the number is the skin colour, pink as every camera draws it");
        // A stop either side is not. One stop is a factor of two in
        // light, which near mid-grey is about 15 L*.
        assert_ne!(colour_of(&b, 45.0), skin, "a stop under is not skin");
        assert_ne!(colour_of(&b, 75.0), skin, "a stop over is not skin");
    }

    #[test]
    fn the_band_follows_the_look_rather_than_a_fixed_window() {
        // The whole point: the scale agrees with whatever the piece is
        // being held to, including a look measured off a reference.
        for l in [42.0f32, 51.0, 68.0] {
            let b = bands(&look_with_skin(l));
            assert_eq!(colour_of(&b, l), [235, 130, 170], "skin at L* {l}");
            assert_ne!(colour_of(&b, l - 12.0), [235, 130, 170]);
        }
    }

    #[test]
    fn a_look_with_no_skin_target_has_no_skin_band() {
        // Saying "skin is right here" when nothing measured said so is
        // worse than saying nothing.
        let look = LookTarget { name: "bare".into(), ..Default::default() };
        let b = bands(&look);
        assert!(!b.iter().any(|x| x.rgb == [235, 130, 170]), "no band invented: {b:?}");
        assert!(b.iter().any(|x| x.meaning.contains("at black")));
        assert!(b.iter().any(|x| x.meaning.contains("at white")));
    }

    #[test]
    fn both_rails_are_their_own_colour() {
        let b = bands(&look_with_skin(60.0));
        assert_eq!(colour_of(&b, 0.0), [120, 0, 160], "black clip");
        assert_eq!(colour_of(&b, 100.0), [230, 40, 40], "white clip");
        // And they are not the same as anything in between.
        for l in [10.0f32, 25.0, 40.0, 55.0, 80.0] {
            assert_ne!(colour_of(&b, l), [120, 0, 160]);
            assert_ne!(colour_of(&b, l), [230, 40, 40]);
        }
    }

    #[test]
    fn the_scale_only_ever_climbs() {
        // A band that started below the one before it would paint a
        // lighter pixel a darker band's colour, which is a lie in the
        // one direction the view exists to show.
        let b = bands(&look_with_skin(51.0));
        assert!(b.windows(2).all(|w| w[0].from_l < w[1].from_l), "{b:?}");
    }

    #[test]
    fn a_frame_comes_back_the_size_it_went_in() {
        let (w, h) = (16u32, 9u32);
        let frame: Vec<u8> = (0..(w * h)).flat_map(|i| { let v = (i * 3) as u8; [v, v, v, 255] }).collect();
        let out = repaint(&frame, w, h, &look_with_skin(60.0));
        assert_eq!(out.len(), frame.len());
        assert!(out.chunks_exact(4).all(|p| p[3] == 255), "opaque throughout");
        // The darkest pixel is the black-clip colour and the picture is
        // not left as it was.
        assert_eq!(&out[..3], &[120, 0, 160]);
        assert_ne!(out, frame);
    }
}
