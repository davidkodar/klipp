//! Judging an HDR picture on an SDR screen.
//!
//! An HDR delivery is rendered as display light where 1.0 is SDR
//! reference white, 203 cd/m² (see [`crate::hdr`]). An SDR monitor cannot
//! show what is above that, so this module gives the two things a
//! colourist uses instead of the picture:
//!
//! * [`hdr_to_sdr_preview`] — the HDR picture as an SDR one: reference
//!   white stays white, so the mid-tones are where the grade put them,
//!   and the light above it rolls off into the top instead of clipping.
//!   Highlight *structure* stays visible; the absolute levels are the
//!   readout's and the false colours' job.
//! * [`false_colour`] — the standard levels as flat colours, so a glance
//!   says which part of the picture sits at diffuse white, which is in the
//!   specular range and which has reached the master's peak.
//!
//! Both read display light; [`nits`] converts it to cd/m².

use crate::hdr::HDR_REFERENCE_WHITE_NITS;

/// Display light (1.0 = SDR reference white) → cd/m².
#[inline]
pub fn nits(display_light: f32) -> f32 {
    display_light * HDR_REFERENCE_WHITE_NITS
}

/// cd/m² → display light.
#[inline]
pub fn from_nits(nits: f32) -> f32 {
    nits / HDR_REFERENCE_WHITE_NITS
}

/// One channel of HDR display light → SDR display-referred linear.
///
/// Identity up to `knee` (0.75 of reference white), then a shoulder that
/// approaches 1.0 without reaching it, so reference white lands just
/// under SDR white and the highlights above it stay ordered and
/// distinguishable. A picture graded for HDR therefore looks like the
/// grade, not like a clipped mess, on an SDR screen.
#[inline]
pub fn hdr_to_sdr_preview(display_light: f32) -> f32 {
    const KNEE: f32 = 0.75;
    let v = display_light.max(0.0);
    if v <= KNEE {
        return v;
    }
    let t = (v - KNEE) / (1.0 - KNEE);
    KNEE + (1.0 - KNEE) * (t / (1.0 + t))
}

/// The bands [`false_colour`] paints: the lower bound of each, in cd/m²,
/// with its colour. A sample takes the colour of the highest band it
/// reaches.
///
/// The levels HDR is judged against (ITU-R BT.2408) — 26 cd/m² for an 18%
/// grey card, 203 for diffuse white — get a band *around* them, not a
/// boundary *at* them: a face lit to grey or a white shirt authored at
/// white sits a hair either side of the number, and a boundary there
/// would flip its colour on rounding. 900 up is the peak of the master
/// Klipp writes, where the highlights have nowhere left to go.
pub const FALSE_COLOUR_BANDS: [(f32, [u8; 3]); 8] = [
    (0.0, [0, 0, 0]),          // black: nothing there
    (0.1, [30, 30, 90]),       // deep shadow, blue
    (5.0, [0, 110, 160]),      // shadow, cyan
    (20.0, [110, 110, 110]),   // an 18% grey card (26), mid grey
    (32.0, [60, 140, 60]),     // mid-tones, green
    (180.0, [240, 240, 240]),  // diffuse white (203), near white
    (230.0, [240, 170, 40]),   // specular, amber
    (900.0, [230, 40, 40]),    // at the master's peak, red
];

/// The false colour of a pixel, by its brightest channel's light.
///
/// The brightest channel, not the luminance: a saturated highlight clips
/// channel by channel, and that is what the colourist needs to see.
pub fn false_colour(display_light: [f32; 3]) -> [u8; 3] {
    let brightest = display_light[0].max(display_light[1]).max(display_light[2]);
    let nits = nits(brightest);
    let mut colour = FALSE_COLOUR_BANDS[0].1;
    for (level, band) in FALSE_COLOUR_BANDS {
        if nits >= level {
            colour = band;
        }
    }
    colour
}

/// A frame of HDR display light (RGBA f32) → the false-colour picture,
/// sRGB RGBA8. Alpha is opaque: the bands are a measurement, not a layer.
pub fn false_colour_frame(display_light: &[f32]) -> Vec<u8> {
    use rayon::prelude::*;
    let mut out = vec![255u8; display_light.len() / 4 * 4];
    out.par_chunks_exact_mut(4).zip(display_light.par_chunks_exact(4)).for_each(|(o, px)| {
        let c = false_colour([px[0], px[1], px[2]]);
        o[..3].copy_from_slice(&c);
    });
    out
}

/// A frame of HDR display light → the light of each pixel's brightest
/// channel, cd/m², at `step`-pixel intervals (a whole-frame map costs
/// 33 MB at 4K and the readout does not need it). Row-major, of
/// `width.div_ceil(step)` by `height.div_ceil(step)`.
pub fn nit_map(display_light: &[f32], width: u32, height: u32, step: u32) -> (Vec<f32>, u32, u32) {
    let step = step.max(1);
    let (mw, mh) = (width.div_ceil(step), height.div_ceil(step));
    let mut out = vec![0.0f32; (mw * mh) as usize];
    for my in 0..mh {
        for mx in 0..mw {
            let (x, y) = ((mx * step).min(width - 1), (my * step).min(height - 1));
            let i = ((y * width + x) * 4) as usize;
            let brightest = display_light[i].max(display_light[i + 1]).max(display_light[i + 2]);
            out[(my * mw + mx) as usize] = nits(brightest);
        }
    }
    (out, mw, mh)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The preview keeps the mid-tones where the grade put them and
    /// orders everything above reference white below SDR white.
    #[test]
    fn the_sdr_preview_keeps_mids_and_orders_highlights() {
        // 18% grey and everything under 0.75 of white: untouched.
        assert_eq!(hdr_to_sdr_preview(from_nits(26.0)), from_nits(26.0));
        assert_eq!(hdr_to_sdr_preview(0.5), 0.5);
        // Reference white just under SDR white, and the peak above it but
        // still inside the range.
        let white = hdr_to_sdr_preview(1.0);
        let peak = hdr_to_sdr_preview(from_nits(1000.0));
        assert!((0.8..1.0).contains(&white), "reference white previews at {white}");
        assert!(peak > white && peak < 1.0, "the 1000-nit peak previews at {peak}");
        // Monotonic, and nothing escapes the range.
        let mut last = -1.0;
        for i in 0..2000 {
            let v = hdr_to_sdr_preview(i as f32 * 0.01);
            assert!(v > last && v < 1.0, "at {}: {v}", i as f32 * 0.01);
            last = v;
        }
        assert_eq!(hdr_to_sdr_preview(-0.5), 0.0);
    }

    /// A sample takes the band of the highest level it reaches, and the
    /// BT.2408 levels land on their own bands.
    #[test]
    fn false_colour_bands_follow_the_reference_levels() {
        let at = |n: f32| false_colour([from_nits(n); 3]);
        assert_eq!(at(0.0), [0, 0, 0], "black");
        assert_eq!(at(1.0), [30, 30, 90], "deep shadow");
        assert_eq!(at(10.0), [0, 110, 160], "shadow");
        // The grey card and diffuse white sit inside a band, not on its
        // edge: a couple of nits either way keeps its colour.
        for n in [24.0, 26.0, 31.0] {
            assert_eq!(at(n), [110, 110, 110], "{n} cd/m² is the grey-card band");
        }
        for n in [190.0, 203.0, 225.0] {
            assert_eq!(at(n), [240, 240, 240], "{n} cd/m² is the diffuse-white band");
        }
        assert_eq!(at(100.0), [60, 140, 60], "mid-tones");
        assert_eq!(at(600.0), [240, 170, 40], "specular");
        assert_eq!(at(1000.0), [230, 40, 40], "the master's peak");
        assert_eq!(at(4000.0), [230, 40, 40], "above the peak");
        // The brightest channel decides: a saturated red highlight is at
        // the peak even though its luminance is far below it.
        assert_eq!(false_colour([from_nits(1000.0), 0.0, 0.0]), [230, 40, 40]);
    }

    #[test]
    fn the_nit_map_samples_the_brightest_channel() {
        let (w, h) = (4u32, 2u32);
        let mut frame = vec![0.0f32; (w * h * 4) as usize];
        // One 1000-nit red pixel at (2, 1), the rest diffuse white.
        for px in frame.chunks_exact_mut(4) {
            px[..3].copy_from_slice(&[1.0, 1.0, 1.0]);
            px[3] = 1.0;
        }
        let i = ((1 * w + 2) * 4) as usize;
        frame[i..i + 3].copy_from_slice(&[from_nits(1000.0), 0.0, 0.0]);
        let (map, mw, mh) = nit_map(&frame, w, h, 1);
        assert_eq!((mw, mh), (4, 2));
        assert!((map[(1 * mw + 2) as usize] - 1000.0).abs() < 0.01);
        assert!((map[0] - 203.0).abs() < 0.01);
        // Subsampled: fewer samples, still the pixels it names.
        let (map, mw, mh) = nit_map(&frame, w, h, 2);
        assert_eq!((mw, mh), (2, 1));
        assert!((map[0] - 203.0).abs() < 0.01);
    }

    #[test]
    fn a_false_colour_frame_paints_every_pixel() {
        let frame: Vec<f32> = [[0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0]].concat();
        let out = false_colour_frame(&frame);
        assert_eq!(out.len(), 8);
        assert_eq!(&out[..4], &[0, 0, 0, 255]);
        assert_eq!(&out[4..], &[240, 240, 240, 255]);
    }
}
