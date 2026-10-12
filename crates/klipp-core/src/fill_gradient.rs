//! Gradient fills for titles (#1509): any number of colour stops, laid
//! along a line at any angle or out from a centre. Text and shapes fill
//! with them alike; the colours between two stops are mixed in linear
//! light, so a red to blue gradient passes through a bright purple
//! rather than the dark one an sRGB mix gives.
//!
//! Geometry is in the filled box: the text block or the shape's box,
//! `u` across and `v` down, 0..1 each.

use serde::{Deserialize, Serialize};

/// The most stops a gradient keeps; the editor and the ops refuse more.
pub const MAX_GRADIENT_STOPS: usize = 16;

/// One colour stop: straight sRGB RGBA at `position` (0..1) along the
/// gradient.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct GradientStop {
    pub position: f32,
    pub color: [u8; 4],
}

/// How the stops are laid out over the filled box.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum GradientKind {
    /// Along a line through the box's middle. `angle_deg` is the
    /// direction, as the gradient generator has it: 0 = top to bottom,
    /// 90 = left to right, 180 = bottom to top. The first stop is on the
    /// box's first corner along the line, the last on its last.
    Linear { angle_deg: f32 },
    /// Out from `centre` (box fractions): the first stop at the centre,
    /// the last at `radius`, a fraction of the box's width across and of
    /// its height down, so on a wide box the rings are ellipses.
    Radial { centre: [f32; 2], radius: f32 },
}

impl Default for GradientKind {
    /// Top to bottom: the direction the two-colour gradient always had.
    fn default() -> Self {
        GradientKind::Linear { angle_deg: 0.0 }
    }
}

impl GradientKind {
    /// Top to bottom, as every gradient was before #1509.
    pub fn is_default(&self) -> bool {
        *self == GradientKind::default()
    }

    /// Where the point `(u, v)` of a `w` x `h` pixel box falls along the
    /// gradient, clamped to 0..1.
    pub fn position_at(&self, u: f32, v: f32, w: f32, h: f32) -> f32 {
        let t = match *self {
            GradientKind::Linear { angle_deg } => {
                let (dir_x, dir_y) = angle_deg.to_radians().sin_cos();
                let half = dir_x.abs() * w * 0.5 + dir_y.abs() * h * 0.5;
                if half <= 0.0 {
                    return 0.0;
                }
                ((u - 0.5) * w * dir_x + (v - 0.5) * h * dir_y) / (2.0 * half) + 0.5
            }
            GradientKind::Radial { centre, radius } => {
                let r = radius.max(1e-6);
                (((u - centre[0]) / r).powi(2) + ((v - centre[1]) / r).powi(2)).sqrt()
            }
        };
        if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 }
    }
}

/// Entries in a [`GradientRamp`]: fine enough that a stop's colour lands
/// on its position to within a code value, and one more than a power of
/// two, so the halves and quarters fall on an entry exactly.
const RAMP_LEN: usize = 4097;

/// A gradient's colours along 0..1, mixed once and looked up per pixel.
pub struct GradientRamp {
    lut: Vec<[u8; 4]>,
}

impl GradientRamp {
    /// The ramp through `stops`, in position order; before the first
    /// stop is its colour, after the last is the last's. Mixed in linear
    /// light with premultiplied alpha, so a stop fading to transparent
    /// does not darken the colour on its way out.
    pub fn new(stops: &[GradientStop]) -> Self {
        let mut stops: Vec<GradientStop> = stops.to_vec();
        stops.sort_by(|a, b| a.position.total_cmp(&b.position));
        let lut = (0..RAMP_LEN)
            .map(|i| colour_at(&stops, i as f32 / (RAMP_LEN - 1) as f32))
            .collect();
        Self { lut }
    }

    /// The colour at `t` (0..1) along the gradient, straight RGBA.
    pub fn at(&self, t: f32) -> [u8; 4] {
        let i = (t.clamp(0.0, 1.0) * (RAMP_LEN - 1) as f32).round() as usize;
        self.lut[i.min(RAMP_LEN - 1)]
    }
}

/// The colour at `t` through stops already in position order.
fn colour_at(stops: &[GradientStop], t: f32) -> [u8; 4] {
    let (Some(first), Some(last)) = (stops.first(), stops.last()) else { return [0; 4] };
    if t <= first.position {
        return first.color;
    }
    if t >= last.position {
        return last.color;
    }
    let i = stops.iter().position(|s| s.position > t).unwrap_or(stops.len() - 1).max(1);
    let (a, b) = (stops[i - 1], stops[i]);
    let span = b.position - a.position;
    let f = if span > 0.0 { (t - a.position) / span } else { 1.0 };
    mix_linear(a.color, b.color, f)
}

/// `a` to `b` by `f`, in linear light with premultiplied alpha.
pub fn mix_linear(a: [u8; 4], b: [u8; 4], f: f32) -> [u8; 4] {
    let (aa, ba) = (a[3] as f32 / 255.0, b[3] as f32 / 255.0);
    let alpha = aa + (ba - aa) * f;
    let mut out = [0u8; 4];
    for c in 0..3 {
        let (x, y) = (to_linear(a[c]) * aa, to_linear(b[c]) * ba);
        let v = x + (y - x) * f;
        out[c] = if alpha > 1e-6 { (to_srgb(v / alpha) * 255.0).round().clamp(0.0, 255.0) as u8 } else { 0 };
    }
    out[3] = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
    out
}

fn to_linear(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stop(position: f32, color: [u8; 4]) -> GradientStop {
        GradientStop { position, color }
    }

    #[test]
    fn red_to_blue_meets_in_linear_light() {
        let ramp = GradientRamp::new(&[stop(0.0, [255, 0, 0, 255]), stop(1.0, [0, 0, 255, 255])]);
        // Half of each in linear light is 0.5, which encodes as 188; an
        // sRGB mix would give 128.
        assert_eq!(ramp.at(0.5), [188, 0, 188, 255]);
        assert_eq!(ramp.at(0.0), [255, 0, 0, 255]);
        assert_eq!(ramp.at(1.0), [0, 0, 255, 255]);
    }

    #[test]
    fn stops_are_taken_in_position_order() {
        let ramp = GradientRamp::new(&[stop(1.0, [0, 0, 255, 255]), stop(0.0, [255, 0, 0, 255]), stop(0.5, [0, 255, 0, 255])]);
        assert_eq!(ramp.at(0.5), [0, 255, 0, 255]);
        assert_eq!(ramp.at(0.0), [255, 0, 0, 255]);
    }

    #[test]
    fn a_stop_fading_out_keeps_its_colour() {
        assert_eq!(mix_linear([255, 0, 0, 255], [0, 0, 0, 0], 0.5), [255, 0, 0, 128]);
    }

    #[test]
    fn linear_angles_run_their_way_across_the_box() {
        let down = GradientKind::Linear { angle_deg: 0.0 };
        assert_eq!(down.position_at(0.3, 0.0, 200.0, 100.0), 0.0);
        assert_eq!(down.position_at(0.3, 1.0, 200.0, 100.0), 1.0);
        let right = GradientKind::Linear { angle_deg: 90.0 };
        assert!((right.position_at(0.25, 0.9, 200.0, 100.0) - 0.25).abs() < 1e-5);
        // Corner to corner at 45 degrees, the middle in the middle.
        let diag = GradientKind::Linear { angle_deg: 45.0 };
        assert!(diag.position_at(0.0, 0.0, 200.0, 100.0) < 1e-5);
        assert!((diag.position_at(1.0, 1.0, 200.0, 100.0) - 1.0).abs() < 1e-5);
        assert!((diag.position_at(0.5, 0.5, 200.0, 100.0) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn radial_runs_from_the_centre_to_the_radius() {
        let k = GradientKind::Radial { centre: [0.5, 0.5], radius: 0.25 };
        assert_eq!(k.position_at(0.5, 0.5, 100.0, 100.0), 0.0);
        assert!((k.position_at(0.75, 0.5, 100.0, 100.0) - 1.0).abs() < 1e-5);
        assert!((k.position_at(0.5, 0.625, 100.0, 100.0) - 0.5).abs() < 1e-5);
    }
}
