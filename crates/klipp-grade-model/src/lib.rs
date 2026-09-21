//! The primary grade's model: what lift, gamma, gain, offset, contrast,
//! the tone controls and the log wheels do to a pixel.
//!
//! The controls act in the *grade encoding*: the working-space linear
//! values taken through the BT.1886 power 2.4 curve, sign-preserving and
//! unclamped, so a value above white keeps its headroom and one below
//! black keeps its sign. It is the encoding of a Rec.709 grading
//! monitor, which is where these controls were designed: mid grey sits
//! at 0.49, white at 1.0. In it the wheels are the ASC CDL —
//!
//! ```text
//!   slope  = gain · (1 − lift)
//!   offset = lift + offset
//!   power  = 1 / gamma
//!   out    = (in · slope + offset) ^ power
//! ```
//!
//! — so lift pins white and raises black, gain pins black and scales,
//! gamma is a power about the ends, and offset is an offset; every
//! grading system a colourist has used behaves this way, and a grade can
//! be handed on as a CDL. Contrast pivots on mid grey in the encoding,
//! which is symmetric in stops and clips nothing.
//!
//! The controls used to add luma-weighted amounts to the *linear* values.
//! Linear luma puts mid grey at 0.18, so lift — weighted by 1 − luma —
//! gave mid grey 82 % of what it gave black and was nearly an offset,
//! gain was a plain multiply by luma, and contrast about a linear pivot
//! sent shadows negative and white to 1.4 at +0.5.
//!
//! `klipp_effects::cpu_grade` and the `SHADER_COLOR_GRADE` shader spell
//! these out per pixel and must agree with the functions here; the
//! parity tests hold them to it.

/// The grade encoding's exponent: BT.1886's display gamma.
pub const GRADE_GAMMA: f32 = 2.4;

/// Linear working-space value → grade encoding. Sign-preserving,
/// unclamped: 2.0 encodes to 1.335, −0.1 to −0.383.
#[inline]
pub fn grade_encode(x: f32) -> f32 {
    let e = x.abs().powf(1.0 / GRADE_GAMMA);
    if x < 0.0 { -e } else { e }
}

/// Grade encoding → linear working-space value; the inverse of
/// [`grade_encode`].
#[inline]
pub fn grade_decode(e: f32) -> f32 {
    let x = e.abs().powf(GRADE_GAMMA);
    if e < 0.0 { -x } else { x }
}

/// Mid grey, 0.18 linear, in the grade encoding: where contrast pivots
/// and the log wheels' mid band centres.
pub const MID_GREY_ENCODED: f32 = 0.489_437_1;

/// One channel of the CDL in the grade encoding. `lift`, `gain`, `gamma`
/// and `offset` are the wheel amounts: 0 is neutral for all four, so
/// `gain` and `gamma` are `1 + amount`.
#[inline]
pub fn cdl_channel(e: f32, lift: f32, gamma: f32, gain: f32, offset: f32) -> f32 {
    let slope = (1.0 + gain) * (1.0 - lift);
    let t = e * slope + lift + offset;
    let power = 1.0 / (1.0 + gamma).max(0.05);
    if t > 0.0 { t.powf(power) } else { t }
}

/// The ASC CDL a wheel setting is: `(slope, offset, power)` per channel,
/// for interchange. `lift`, `gamma`, `gain` and `offset` are wheel
/// amounts as [`cdl_channel`] takes them.
pub fn cdl_of(lift: [f32; 3], gamma: [f32; 3], gain: [f32; 3], offset: [f32; 3]) -> ([f32; 3], [f32; 3], [f32; 3]) {
    let mut slope = [0.0; 3];
    let mut off = [0.0; 3];
    let mut power = [0.0; 3];
    for i in 0..3 {
        slope[i] = (1.0 + gain[i]) * (1.0 - lift[i]);
        off[i] = lift[i] + offset[i];
        power[i] = 1.0 / (1.0 + gamma[i]).max(0.05);
    }
    (slope, off, power)
}

/// The linear pivot a contrast control may be set to.
///
/// Above zero because the encoding has no meaning at or below it, and
/// capped at diffuse white. [`DEFAULT_CONTRAST_PIVOT`] is mid grey,
/// where it has always been.
pub const MIN_CONTRAST_PIVOT: f32 = 0.01;
/// Diffuse white. A pivot above it sits outside anything but speculars,
/// where contrast stops being a hinge and becomes a one-sided stretch.
pub const MAX_CONTRAST_PIVOT: f32 = 1.0;
pub const DEFAULT_CONTRAST_PIVOT: f32 = 0.18;

/// The grade-encoded pivot for a linear one, clamped to the range the
/// control allows.
///
/// Called once where a grade is built, never per pixel: both the CPU
/// path and the GPU uniform carry the encoded value so neither pays for
/// a `powf` per sample, and both are handed the same number.
#[inline]
pub fn encode_contrast_pivot(pivot_linear: f32) -> f32 {
    grade_encode(pivot_linear.clamp(MIN_CONTRAST_PIVOT, MAX_CONTRAST_PIVOT))
}

/// Contrast about an arbitrary pivot in the grade encoding: `amount` 0
/// is neutral, +1 doubles the distance from the pivot.
///
/// Where the pivot sits decides whether adding contrast crushes the
/// shadows or blows the highlights, and on a log or HDR source the
/// useful pivot is nowhere near mid grey.
#[inline]
pub fn contrast_channel_at(e: f32, amount: f32, pivot_encoded: f32) -> f32 {
    (e - pivot_encoded) * (1.0 + amount) + pivot_encoded
}

/// Contrast about mid grey: [`contrast_channel_at`] at
/// [`MID_GREY_ENCODED`], which is what every grade did before the pivot
/// was adjustable.
#[inline]
pub fn contrast_channel(e: f32, amount: f32) -> f32 {
    contrast_channel_at(e, amount, MID_GREY_ENCODED)
}

/// A ramp from 0 at `lo` to 1 at `hi`, smooth at both ends.
#[inline]
fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The tone controls' band weights at encoded luma `l`: (highlights,
/// shadows, whites, blacks). Whites and blacks are the ends, highlights
/// and shadows the broader bands either side of mid grey; each control
/// adds `amount × weight` to the encoding.
#[inline]
pub fn tone_weights(l: f32) -> [f32; 4] {
    [
        smoothstep(0.45, 0.85, l),
        1.0 - smoothstep(0.30, 0.65, l),
        smoothstep(0.65, 0.95, l),
        1.0 - smoothstep(0.10, 0.40, l),
    ]
}

/// The log wheels' band weights at encoded luma `l`: (low, mid, high),
/// Gaussian bands about the shadows, mid grey and the highlights. Each
/// wheel adds `amount × weight` to the encoding.
#[inline]
pub fn log_wheel_weights(l: f32) -> [f32; 3] {
    let g = |centre: f32, sigma: f32| (-((l - centre) * (l - centre)) / (2.0 * sigma * sigma)).exp();
    [g(0.20, 0.14), g(MID_GREY_ENCODED, 0.16), g(0.78, 0.14)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn the_encoding_round_trips_and_keeps_headroom_and_sign() {
        for x in [-0.2, 0.0, 0.001, 0.18, 1.0, 2.0, 40.0] {
            assert!(close(grade_decode(grade_encode(x)), x, 1e-5 * x.abs().max(1.0)), "{x}");
        }
        assert!(close(grade_encode(0.18), MID_GREY_ENCODED, 1e-6), "mid grey sits at {}", grade_encode(0.18));
        assert!(close(grade_encode(1.0), 1.0, 1e-7), "white is 1.0");
        assert!(grade_encode(2.0) > 1.0, "a value above white stays above it");
        assert!(grade_encode(-0.1) < 0.0, "a value below black keeps its sign");
    }

    /// Each wheel alone does what its name says, at the ends of the range.
    #[test]
    fn lift_pins_white_gain_pins_black_gamma_bends_the_middle_offset_moves_all() {
        let (black, mid, white) = (0.0, MID_GREY_ENCODED, 1.0);
        // Lift 0.1: black to 0.1, white stays, mid rises.
        assert!(close(cdl_channel(black, 0.1, 0.0, 0.0, 0.0), 0.1, 1e-6));
        assert!(close(cdl_channel(white, 0.1, 0.0, 0.0, 0.0), 1.0, 1e-6), "lift pins white");
        assert!(cdl_channel(mid, 0.1, 0.0, 0.0, 0.0) > mid);
        // Gain +0.5: black stays, white to 1.5, mid scales.
        assert!(close(cdl_channel(black, 0.0, 0.0, 0.5, 0.0), 0.0, 1e-6), "gain pins black");
        assert!(close(cdl_channel(white, 0.0, 0.0, 0.5, 0.0), 1.5, 1e-6));
        assert!(close(cdl_channel(mid, 0.0, 0.0, 0.5, 0.0), mid * 1.5, 1e-6));
        // Gamma +0.5 (gamma 1.5, power 1/1.5): ends stay, the middle rises.
        assert!(close(cdl_channel(black, 0.0, 0.5, 0.0, 0.0), 0.0, 1e-6));
        assert!(close(cdl_channel(white, 0.0, 0.5, 0.0, 0.0), 1.0, 1e-6), "gamma pins both ends");
        assert!(close(cdl_channel(mid, 0.0, 0.5, 0.0, 0.0), mid.powf(1.0 / 1.5), 1e-6));
        // Offset 0.1: everything moves by 0.1.
        for e in [black, mid, white] {
            assert!(close(cdl_channel(e, 0.0, 0.0, 0.0, 0.1), e + 0.1, 1e-6));
        }
        // Neutral is the identity.
        for e in [black, mid, white, 1.4] {
            assert!(close(cdl_channel(e, 0.0, 0.0, 0.0, 0.0), e, 1e-6));
        }
    }

    /// The composition is the ASC CDL: slope, offset, power. A grade can
    /// therefore be written out as one.
    #[test]
    fn the_wheels_compose_as_a_cdl() {
        let (lift, gamma, gain, offset) = (0.05, 0.2, 0.3, -0.02);
        let slope = (1.0 + gain) * (1.0 - lift);
        let cdl_offset = lift + offset;
        let power = 1.0 / (1.0 + gamma);
        for e in [0.0, 0.2, MID_GREY_ENCODED, 0.8, 1.0, 1.3] {
            let want = (e * slope + cdl_offset).max(0.0).powf(power);
            assert!(close(cdl_channel(e, lift, gamma, gain, offset), want, 1e-6), "at {e}");
        }
    }

    /// What `cdl_of` reports is what `cdl_channel` applies.
    #[test]
    fn the_reported_cdl_is_the_applied_one() {
        let (lift, gamma, gain, offset) = ([0.05, 0.0, -0.02], [0.2, 0.1, 0.0], [0.3, 0.0, 0.1], [-0.02, 0.01, 0.0]);
        let (slope, off, power) = cdl_of(lift, gamma, gain, offset);
        for i in 0..3 {
            for e in [0.0, 0.3, MID_GREY_ENCODED, 0.9, 1.2] {
                let t = e * slope[i] + off[i];
                let want = if t > 0.0 { t.powf(power[i]) } else { t };
                assert!(close(cdl_channel(e, lift[i], gamma[i], gain[i], offset[i]), want, 1e-6));
            }
        }
        let (s, o, p) = cdl_of([0.0; 3], [0.0; 3], [0.0; 3], [0.0; 3]);
        assert_eq!((s, o, p), ([1.0; 3], [0.0; 3], [1.0; 3]), "neutral wheels are the identity CDL");
    }

    /// The pivot is the one point contrast leaves alone. That is what
    /// makes it the pivot, and it has to hold wherever it is put.
    #[test]
    fn contrast_leaves_its_own_pivot_where_it_is() {
        for pivot_linear in [0.02, 0.09, DEFAULT_CONTRAST_PIVOT, 0.45, 0.9, 1.8] {
            let p = encode_contrast_pivot(pivot_linear);
            for amount in [-0.8, -0.2, 0.0, 0.35, 1.0, 3.0] {
                assert!(
                    close(contrast_channel_at(p, amount, p), p, 1e-6),
                    "pivot {pivot_linear} moved at amount {amount}",
                );
            }
        }
    }

    /// Where the pivot sits decides which end of the scale a contrast
    /// push spends. A low pivot holds the shadows and opens the
    /// highlights; a high one does the reverse. This is the whole
    /// reason for the control.
    #[test]
    fn a_lower_pivot_lifts_what_a_higher_pivot_crushes() {
        let low = encode_contrast_pivot(0.05);
        let high = encode_contrast_pivot(0.60);
        let shadow = grade_encode(0.02);
        assert!(
            contrast_channel_at(shadow, 0.5, low) > contrast_channel_at(shadow, 0.5, high),
            "a low pivot should hold a shadow up relative to a high one",
        );
        let highlight = grade_encode(1.5);
        assert!(
            contrast_channel_at(highlight, 0.5, low) > contrast_channel_at(highlight, 0.5, high),
            "a low pivot should push a highlight further than a high one",
        );
    }

    /// Mid grey is still exactly where contrast pivoted before it was
    /// adjustable, so no existing grade moves.
    #[test]
    fn the_default_pivot_is_the_old_fixed_one() {
        assert!(close(encode_contrast_pivot(DEFAULT_CONTRAST_PIVOT), MID_GREY_ENCODED, 1e-6));
        for e in [0.0, 0.2, MID_GREY_ENCODED, 0.8, 1.2] {
            for amount in [-0.5, 0.0, 0.7] {
                assert!(close(
                    contrast_channel(e, amount),
                    contrast_channel_at(e, amount, encode_contrast_pivot(DEFAULT_CONTRAST_PIVOT)),
                    1e-6,
                ));
            }
        }
    }

    /// A pivot outside the range is clamped rather than producing a
    /// NaN encoding: `grade_encode` of a negative is a negative, and
    /// contrast about a negative pivot inverts the picture.
    #[test]
    fn a_pivot_out_of_range_is_clamped_not_encoded() {
        assert_eq!(encode_contrast_pivot(-5.0), grade_encode(MIN_CONTRAST_PIVOT));
        assert_eq!(encode_contrast_pivot(0.0), grade_encode(MIN_CONTRAST_PIVOT));
        assert_eq!(encode_contrast_pivot(99.0), grade_encode(MAX_CONTRAST_PIVOT));
        assert!(encode_contrast_pivot(f32::NAN).is_finite() || encode_contrast_pivot(f32::NAN).is_nan());
    }

    /// Contrast leaves mid grey where it is and moves a stop up and a
    /// stop down by the same encoded amount.
    #[test]
    fn contrast_is_symmetric_about_mid_grey_and_clips_nothing() {
        assert!(close(contrast_channel(MID_GREY_ENCODED, 0.5), MID_GREY_ENCODED, 1e-6));
        let up = grade_encode(0.36);
        let down = grade_encode(0.09);
        let (up2, down2) = (contrast_channel(up, 0.5), contrast_channel(down, 0.5));
        assert!(close(up2 - MID_GREY_ENCODED, (up - MID_GREY_ENCODED) * 1.5, 1e-6));
        assert!(close(MID_GREY_ENCODED - down2, (MID_GREY_ENCODED - down) * 1.5, 1e-6));
        // Two stops under mid grey stays above black; white lands above
        // white rather than at a clip. (Far below black, past about four
        // stops under at +0.5, a pivoted contrast does reach zero — as on
        // every grading system; the old linear one got there at one.)
        assert!(grade_decode(contrast_channel(grade_encode(0.045), 0.5)) > 0.0);
        assert!(grade_decode(contrast_channel(1.0, 0.5)) > 1.0);
    }

    /// The bands sit where their names say on the encoding.
    #[test]
    fn the_bands_sit_on_the_tones_they_name() {
        let [hi, sh, wh, bl] = tone_weights(grade_encode(0.0));
        assert!((hi, sh, wh, bl) == (0.0, 1.0, 0.0, 1.0), "black: {:?}", (hi, sh, wh, bl));
        let [hi, sh, wh, bl] = tone_weights(1.0);
        assert!((hi, sh, wh, bl) == (1.0, 0.0, 1.0, 0.0), "white: {:?}", (hi, sh, wh, bl));
        let [hi, sh, wh, bl] = tone_weights(MID_GREY_ENCODED);
        assert!(hi < 0.1 && sh < 0.5 && wh == 0.0 && bl == 0.0, "mid grey: {:?}", (hi, sh, wh, bl));
        let [low, mid, high] = log_wheel_weights(MID_GREY_ENCODED);
        assert!(close(mid, 1.0, 1e-6) && low < 0.2 && high < 0.2, "the mid band peaks at mid grey");
        assert!(log_wheel_weights(0.20)[0] > 0.99 && log_wheel_weights(0.78)[2] > 0.99);
    }
}

/// ASC CDL interchange: the `.cdl` / `.cc` / `.ccc` XML files every
/// grading pipeline reads and writes.
pub mod cdl {
    /// One colour correction: slope, offset, power per channel, and
    /// saturation.
    #[derive(Clone, Debug, PartialEq)]
    pub struct Cdl {
        pub slope: [f32; 3],
        pub offset: [f32; 3],
        pub power: [f32; 3],
        pub saturation: f32,
    }

    impl Cdl {
        /// The CDL the wheels and saturation are (see [`super::cdl_of`]).
        pub fn from_wheels(lift: [f32; 3], gamma: [f32; 3], gain: [f32; 3], offset: [f32; 3], saturation: f32) -> Self {
            let (slope, off, power) = super::cdl_of(lift, gamma, gain, offset);
            Self { slope, offset: off, power, saturation }
        }

        /// Wheel amounts that apply this CDL: lift stays neutral (a CDL
        /// has three numbers per channel, the wheels four), gain carries
        /// the slope, offset the offset, gamma the power.
        pub fn wheel_amounts(&self) -> ([f32; 3], [f32; 3], [f32; 3], [f32; 3]) {
            let lift = [0.0; 3];
            let mut gamma = [0.0; 3];
            let mut gain = [0.0; 3];
            for i in 0..3 {
                gain[i] = self.slope[i] - 1.0;
                gamma[i] = 1.0 / self.power[i].max(0.05) - 1.0;
            }
            (lift, gamma, gain, self.offset)
        }

        /// The correction as an ASC CDL 1.01 `ColorDecisionList` document.
        pub fn to_xml(&self, id: &str) -> String {
            let v = |a: [f32; 3]| format!("{} {} {}", trim(a[0]), trim(a[1]), trim(a[2]));
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<ColorDecisionList xmlns=\"urn:ASC:CDL:v1.01\">\n\
  <ColorDecision>\n\
    <ColorCorrection id=\"{}\">\n\
      <SOPNode>\n\
        <Slope>{}</Slope>\n\
        <Offset>{}</Offset>\n\
        <Power>{}</Power>\n\
      </SOPNode>\n\
      <SatNode>\n\
        <Saturation>{}</Saturation>\n\
      </SatNode>\n\
    </ColorCorrection>\n\
  </ColorDecision>\n\
</ColorDecisionList>\n",
                escape(id),
                v(self.slope),
                v(self.offset),
                v(self.power),
                trim(self.saturation)
            )
        }

        /// The first colour correction in a `.cdl`, `.ccc` or `.cc` file.
        /// Reads the `Slope`, `Offset`, `Power` and `Saturation` elements
        /// wherever they sit and whatever namespace prefix they carry; a
        /// missing SatNode is saturation 1.
        pub fn parse(text: &str) -> Result<Self, String> {
            let triple = |name: &str| -> Result<[f32; 3], String> {
                let body = element(text, name).ok_or_else(|| format!("no <{name}> in the file"))?;
                let nums = body
                    .split_whitespace()
                    .map(|t| t.parse::<f32>().map_err(|e| format!("<{name}>: {e}")))
                    .collect::<Result<Vec<f32>, String>>()?;
                if nums.len() != 3 {
                    return Err(format!("<{name}> has {} values, not 3", nums.len()));
                }
                Ok([nums[0], nums[1], nums[2]])
            };
            let slope = triple("Slope")?;
            let offset = triple("Offset")?;
            let power = triple("Power")?;
            let saturation = match element(text, "Saturation") {
                Some(b) => b.trim().parse::<f32>().map_err(|e| format!("<Saturation>: {e}"))?,
                None => 1.0,
            };
            if power.iter().any(|p| *p <= 0.0) {
                return Err("a Power must be positive".into());
            }
            Ok(Self { slope, offset, power, saturation })
        }
    }

    /// The text of the first `<name>` or `<prefix:name>` element.
    fn element<'a>(text: &'a str, name: &str) -> Option<&'a str> {
        let mut from = 0;
        while let Some(rel) = text[from..].find('<') {
            let start = from + rel;
            let rest = &text[start + 1..];
            let tag_end = rest.find('>')?;
            let tag = &rest[..tag_end];
            let tag_name = tag.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
            let local = tag_name.rsplit(':').next().unwrap_or(tag_name);
            if local == name && !tag.starts_with('/') && !tag.ends_with('/') {
                let body_start = start + 1 + tag_end + 1;
                let close = text[body_start..].find("</")?;
                return Some(&text[body_start..body_start + close]);
            }
            from = start + 1;
        }
        None
    }

    fn trim(v: f32) -> String {
        let s = format!("{v:.6}");
        let s = s.trim_end_matches('0').trim_end_matches('.');
        if s.is_empty() || s == "-0" {
            "0".into()
        } else {
            s.to_string()
        }
    }

    fn escape(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('"', "&quot;")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_grade_round_trips_through_the_file() {
            let cdl = Cdl::from_wheels([0.05, 0.0, -0.02], [0.2, 0.1, 0.0], [0.3, 0.0, 0.1], [-0.02, 0.01, 0.0], 0.9);
            let back = Cdl::parse(&cdl.to_xml("shot 3")).unwrap();
            for i in 0..3 {
                assert!((back.slope[i] - cdl.slope[i]).abs() < 1e-5);
                assert!((back.offset[i] - cdl.offset[i]).abs() < 1e-5);
                assert!((back.power[i] - cdl.power[i]).abs() < 1e-5);
            }
            assert!((back.saturation - 0.9).abs() < 1e-6);
            // Put on the wheels and read off again, it is the same CDL.
            let (lift, gamma, gain, offset) = back.wheel_amounts();
            let again = Cdl::from_wheels(lift, gamma, gain, offset, back.saturation);
            for i in 0..3 {
                assert!((again.slope[i] - cdl.slope[i]).abs() < 1e-5);
                assert!((again.offset[i] - cdl.offset[i]).abs() < 1e-5);
                assert!((again.power[i] - cdl.power[i]).abs() < 1e-5);
            }
        }

        /// A file as another system writes it: a namespace prefix, a bare
        /// ColorCorrection, no SatNode, extra whitespace.
        #[test]
        fn reads_what_other_systems_write() {
            let text = "<cdl:ColorCorrection xmlns:cdl=\"urn:ASC:CDL:v1.01\" id=\"x\">\n  <cdl:SOPNode>\n   <cdl:Slope> 1.2  0.9 1.0 </cdl:Slope>\n   <cdl:Offset>0.01 -0.02 0</cdl:Offset>\n   <cdl:Power>1.1 1 0.95</cdl:Power>\n  </cdl:SOPNode>\n</cdl:ColorCorrection>";
            let c = Cdl::parse(text).unwrap();
            assert_eq!(c.slope, [1.2, 0.9, 1.0]);
            assert_eq!(c.offset, [0.01, -0.02, 0.0]);
            assert_eq!(c.power, [1.1, 1.0, 0.95]);
            assert_eq!(c.saturation, 1.0);
            assert!(Cdl::parse("<ColorCorrection><SOPNode><Slope>1 1</Slope></SOPNode></ColorCorrection>").is_err());
        }
    }
}
