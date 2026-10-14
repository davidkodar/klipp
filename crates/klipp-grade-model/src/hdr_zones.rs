//! HDR zones (#240): exposure, colour and saturation placed by tonal
//! zone, each zone's reach set in stops of exposure about mid grey.
//!
//! A pixel's place is `s = log2(Y / 0.18)`, `Y` the working space's
//! luminance of the node's *input*, so one zone's change never moves a
//! pixel into another, and a stop means the same tone on SDR, HDR and
//! log material alike. Seven zones:
//!
//! * Global (zone 0) weighs 1 everywhere.
//! * Black, Dark, Shadow (zones 1 to 3) reach down from their boundary:
//!   `1 − smoothstep(b − f/2, b + f/2, s)`.
//! * Light, Highlight, Specular (zones 4 to 6) reach up from theirs:
//!   `smoothstep(b − f/2, b + f/2, s)`.
//!
//! `b` is the zone's boundary in stops from mid grey, `f` its falloff
//! width in stops (0: a hard edge). Each zone's exposure (stops) and
//! colour (a balance in stops per channel, its luma-weighted mean taken
//! out so a tint never changes brightness) add, weighted, in stops:
//!
//! ```text
//!   gain_c = 2^(Σ w_i · (e_i + t_i,c))
//! ```
//!
//! and saturation, about the pixel's luminance after the gains,
//! multiplies: `Π (1 + w_i · (sat_i − 1))`. At every amount 0 and every
//! saturation 1 the zones change nothing, exactly.
//!
//! `klipp_effects::cpu_grade::hdr_zones` runs [`apply`] per pixel, and
//! the `SHADER_HDR_ZONES` shader spells the same out; the parity tests
//! hold them to it.

/// How many zones: Global, then Black, Dark, Shadow, Light, Highlight,
/// Specular.
pub const ZONES: usize = 7;

/// The zones' names, in order.
pub const ZONE_NAMES: [&str; ZONES] = ["Global", "Black", "Dark", "Shadow", "Light", "Highlight", "Specular"];

/// Mid grey, linear: stop 0.
pub const MID_GREY: f32 = 0.18;

/// The darkest luminance a stop is read from: −20 stops. Black, and
/// anything below it, sits there, in the lowest zones.
pub const MIN_LUMA: f32 = MID_GREY / 1_048_576.0;

/// Which way zone `i` reaches from its boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// The whole picture (Global).
    All,
    /// Everything darker than the boundary.
    Down,
    /// Everything brighter than the boundary.
    Up,
}

/// Zone `i`'s reach.
pub fn reach(i: usize) -> Reach {
    match i {
        0 => Reach::All,
        1..=3 => Reach::Down,
        _ => Reach::Up,
    }
}

/// Where luminance `y` sits, in stops from mid grey.
#[inline]
pub fn stops(y: f32) -> f32 {
    (y.max(MIN_LUMA) / MID_GREY).log2()
}

/// Hermite smoothstep from `a` to `b`; a step at `a` when they meet.
#[inline]
fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    if b <= a {
        return if x >= a { 1.0 } else { 0.0 };
    }
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Zone `i`'s weight at `s` stops, its boundary `boundary` and falloff
/// `falloff` stops wide.
#[inline]
pub fn weight(i: usize, s: f32, boundary: f32, falloff: f32) -> f32 {
    let half = falloff.max(0.0) * 0.5;
    match reach(i) {
        Reach::All => 1.0,
        Reach::Down => 1.0 - smoothstep(boundary - half, boundary + half, s),
        Reach::Up => smoothstep(boundary - half, boundary + half, s),
    }
}

/// The zones' settings as a pixel uses them: each zone's gain in stops
/// per channel (exposure plus its colour, the colour's luma-weighted mean
/// taken out), saturation, boundary and falloff.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Terms {
    pub gain_stops: [[f32; 3]; ZONES],
    pub saturation: [f32; ZONES],
    pub boundary: [f32; ZONES],
    pub falloff: [f32; ZONES],
    /// The working space's luminance weights.
    pub luma: [f32; 3],
}

impl Terms {
    /// From each zone's exposure (stops), colour (stops per channel),
    /// saturation (1: none), boundary and falloff (stops), in the working
    /// space whose luminance weights are `luma`.
    pub fn new(exposure: [f32; ZONES], colour: [[f32; 3]; ZONES], saturation: [f32; ZONES], boundary: [f32; ZONES], falloff: [f32; ZONES], luma: [f32; 3]) -> Self {
        let mut gain_stops = [[0.0; 3]; ZONES];
        for (i, g) in gain_stops.iter_mut().enumerate() {
            let t = colour[i];
            let mean = luma[0] * t[0] + luma[1] * t[1] + luma[2] * t[2];
            *g = [0, 1, 2].map(|c| exposure[i] + (t[c] - mean));
        }
        Terms { gain_stops, saturation, boundary, falloff, luma }
    }

    /// Whether any zone changes a pixel.
    pub fn is_active(&self) -> bool {
        self.gain_stops.iter().flatten().any(|v| *v != 0.0) || self.saturation.iter().any(|v| *v != 1.0)
    }
}

/// The zones applied to one linear pixel.
#[inline]
pub fn apply(rgb: [f32; 3], t: &Terms) -> [f32; 3] {
    let l = t.luma;
    let s = stops(l[0] * rgb[0] + l[1] * rgb[1] + l[2] * rgb[2]);
    let mut sum = [0.0f32; 3];
    let mut sat = 1.0f32;
    for i in 0..ZONES {
        let w = weight(i, s, t.boundary[i], t.falloff[i]);
        for (c, v) in sum.iter_mut().enumerate() {
            *v += w * t.gain_stops[i][c];
        }
        sat *= 1.0 + w * (t.saturation[i] - 1.0);
    }
    let mut out = [0, 1, 2].map(|c| if sum[c] == 0.0 { rgb[c] } else { rgb[c] * sum[c].exp2() });
    if sat != 1.0 {
        let y = l[0] * out[0] + l[1] * out[1] + l[2] * out[2];
        out = out.map(|v| y + (v - y) * sat);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const REC709: [f32; 3] = [0.2126, 0.7152, 0.0722];
    const BOUNDARY: [f32; ZONES] = [0.0, -4.5, -2.5, -1.0, 1.0, 2.5, 4.5];
    const FALLOFF: [f32; ZONES] = [1.0; ZONES];

    fn terms(zone: usize, exposure: f32, colour: [f32; 3], saturation: f32) -> Terms {
        let mut e = [0.0; ZONES];
        let mut c = [[0.0; 3]; ZONES];
        let mut s = [1.0; ZONES];
        e[zone] = exposure;
        c[zone] = colour;
        s[zone] = saturation;
        Terms::new(e, c, s, BOUNDARY, FALLOFF, REC709)
    }

    fn grey(stop: f32) -> [f32; 3] {
        [MID_GREY * stop.exp2(); 3]
    }

    #[test]
    fn a_zone_weighs_half_at_its_boundary_and_all_or_nothing_past_its_falloff() {
        for i in 1..ZONES {
            let b = BOUNDARY[i];
            assert!((weight(i, b, b, 1.0) - 0.5).abs() < 1e-6, "zone {i} at its boundary");
            let (inside, outside) = if reach(i) == Reach::Down { (b - 0.5, b + 0.5) } else { (b + 0.5, b - 0.5) };
            assert_eq!(weight(i, inside, b, 1.0), 1.0, "zone {i} half a falloff in");
            assert_eq!(weight(i, outside, b, 1.0), 0.0, "zone {i} half a falloff out");
            // A wider falloff still reaches past it.
            assert!(weight(i, outside, b, 3.0) > 0.0, "zone {i}, a 3-stop falloff");
        }
        assert_eq!(weight(0, -30.0, 0.0, 1.0), 1.0);
        assert_eq!(weight(0, 30.0, 0.0, 1.0), 1.0);
    }

    #[test]
    fn a_falloff_of_nothing_is_a_hard_edge() {
        assert_eq!(weight(4, 0.999, 1.0, 0.0), 0.0);
        assert_eq!(weight(4, 1.0, 1.0, 0.0), 1.0);
        assert_eq!(weight(3, -1.001, -1.0, 0.0), 1.0);
        assert_eq!(weight(3, -1.0, -1.0, 0.0), 0.0);
    }

    #[test]
    fn at_the_defaults_nothing_changes_exactly() {
        let t = Terms::new([0.0; ZONES], [[0.0; 3]; ZONES], [1.0; ZONES], BOUNDARY, FALLOFF, REC709);
        assert!(!t.is_active());
        for px in [[0.0, 0.0, 0.0], [0.18, 0.05, 0.9], [12.0, 3.0, 40.0], [-0.01, 0.2, 0.3], [1.0e-9, 1.0, 0.5]] {
            assert_eq!(apply(px, &t), px);
        }
    }

    /// A zone's exposure is stops: +1 doubles what it reaches, and leaves
    /// alone what it doesn't. Control: the zone's opposite end moves not
    /// at all.
    #[test]
    fn a_zones_exposure_is_stops_where_it_reaches() {
        let t = terms(5, 1.0, [0.0; 3], 1.0);
        let lit = apply(grey(4.0), &t);
        assert!((lit[0] / grey(4.0)[0] - 2.0).abs() < 1e-5, "{lit:?}");
        assert_eq!(apply(grey(-3.0), &t), grey(-3.0));
        let t = terms(1, -1.0, [0.0; 3], 1.0);
        let crushed = apply(grey(-7.0), &t);
        assert!((crushed[0] / grey(-7.0)[0] - 0.5).abs() < 1e-5, "{crushed:?}");
        assert_eq!(apply(grey(3.0), &t), grey(3.0));
    }

    /// A colour never changes brightness: its luma-weighted mean is taken
    /// out, so a pure tint keeps a grey pixel's luminance.
    #[test]
    fn a_zones_colour_keeps_luminance() {
        let t = terms(0, 0.0, [0.5, 0.0, -0.3], 1.0);
        let px = grey(0.0);
        let out = apply(px, &t);
        let y = |p: [f32; 3]| REC709[0] * p[0] + REC709[1] * p[1] + REC709[2] * p[2];
        assert!(out[0] > out[1] && out[1] > out[2], "tinted: {out:?}");
        assert!((y(out) - y(px)).abs() / y(px) < 0.02, "luminance {} from {}", y(out), y(px));
    }

    #[test]
    fn saturation_turns_about_the_pixels_luminance() {
        let t = terms(0, 0.0, [0.0; 3], 0.0);
        let out = apply([0.4, 0.1, 0.05], &t);
        assert!((out[0] - out[1]).abs() < 1e-6 && (out[1] - out[2]).abs() < 1e-6, "{out:?}");
    }

    /// Zones read stops about mid grey, not code values: a pixel 2.5
    /// stops over mid grey is at the Highlight boundary on SDR material
    /// (0.32 linear) and on HDR scene light alike, and a picture lit k
    /// stops brighter meets the same zones with their boundaries moved k
    /// stops. Control: read as a code value, the same pixel lands
    /// elsewhere.
    #[test]
    fn a_stop_is_the_same_zone_at_any_level() {
        let sdr = MID_GREY * 2f32.powf(2.5);
        assert!((stops(sdr) - 2.5).abs() < 1e-5);
        assert!((weight(5, stops(sdr), 2.5, 1.0) - 0.5).abs() < 1e-5);
        for k in [-3.0f32, 1.0, 4.0, 6.5] {
            for y in [0.002f32, 0.05, 0.18, 0.7, 3.0] {
                for i in 1..ZONES {
                    let (a, b) = (weight(i, stops(y), BOUNDARY[i], 1.0), weight(i, stops(y * k.exp2()), BOUNDARY[i] + k, 1.0));
                    assert!((a - b).abs() < 1e-4, "zone {i}, {y} lit {k} stops: {a} against {b}");
                }
            }
        }
        assert!((weight(5, sdr, 2.5, 1.0) - 0.5).abs() > 0.4, "a code value isn't a stop");
    }
}
