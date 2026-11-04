//! A colour match as the Colour Match node draws it: klipp-color's
//! estimate of exposure, white balance and saturation, its saturation then
//! measured on what the node itself makes of the source and brought to the
//! reference's. The estimate reads saturation off a simulation of the
//! correction, not off the node's grade, and in a colourist's check it
//! left a warm, dark shot matched to a neutral one a fifth less saturated
//! than the reference, the skin grey.

use crate::graph::NodeGraph;
use crate::GraphExecutor;
use klipp_core::ColorMatchSettings;

/// Frames are read at most this wide: plenty for an average.
const WORK_W: u32 = 256;
/// Rounds of measuring and correcting the saturation.
const ROUNDS: usize = 4;
/// The share of a frame's pixels that has to look like skin for skin to
/// be what the saturation is matched on.
const SKIN_SHARE: f32 = 0.03;

/// The match of `source` (`sw` by `sh`, RGBA) to `reference` (`rw` by
/// `rh`, RGBA), as the Colour Match node's settings: the saturation is the
/// one that, through the node, gives the source the reference's chroma,
/// on skin when both frames show some, else over the whole frame.
pub fn match_for_node(source: &[u8], sw: u32, sh: u32, reference: &[u8], rw: u32, rh: u32) -> ColorMatchSettings {
    let r = klipp_color::color_match::match_colors(source, reference);
    let mut settings = ColorMatchSettings { exposure: r.exposure_adjust, temperature: r.temperature_adjust, tint: r.tint_adjust, saturation: r.saturation_adjust };
    let (src, w, h) = shrink(source, sw, sh);
    let (refr, _, _) = shrink(reference, rw, rh);
    if src.is_empty() || refr.is_empty() {
        return settings;
    }
    let target = Chroma::of(&refr);
    let mut graph = NodeGraph::default();
    let node = graph.ensure_color_match();
    let mut ex = GraphExecutor::new_cpu_only();
    for _ in 0..ROUNDS {
        if let Some(n) = graph.nodes.iter_mut().find(|n| n.id == node) {
            n.params.color_match = Some(settings);
        }
        let Ok((out, _)) = ex.execute(&src, &graph, w, h, 0.0, 0, None, None) else { break };
        let now = Chroma::of(&out);
        let (want, got) = match (target.skin_share >= SKIN_SHARE && now.skin_share >= SKIN_SHARE, now.all > 0.5) {
            (true, _) => (target.skin, now.skin),
            (false, true) => (target.all, now.all),
            _ => break,
        };
        if got <= 0.5 {
            break;
        }
        settings.saturation = (settings.saturation * (want / got).clamp(0.5, 2.0)).clamp(0.1, 3.0);
    }
    settings
}

/// `rgba` taken down to at most [`WORK_W`] wide, every pixel sampled
/// nearest; the size it came to.
fn shrink(rgba: &[u8], w: u32, h: u32) -> (Vec<u8>, u32, u32) {
    if w == 0 || h == 0 || rgba.len() < (w * h * 4) as usize {
        return (Vec::new(), 0, 0);
    }
    let step = w.div_ceil(WORK_W).max(1);
    let (nw, nh) = (w / step, h / step);
    let mut out = Vec::with_capacity((nw * nh * 4) as usize);
    for y in 0..nh {
        for x in 0..nw {
            let i = (((y * step) * w + x * step) * 4) as usize;
            out.extend_from_slice(&rgba[i..i + 4]);
        }
    }
    (out, nw, nh)
}

/// A frame's mean chroma in CIELAB, over every pixel and over those that
/// look like skin (a hue of 20° to 70°, some colour, not too dark or
/// bright), and the share of skin.
struct Chroma {
    all: f32,
    skin: f32,
    skin_share: f32,
}

impl Chroma {
    fn of(rgba: &[u8]) -> Self {
        let (mut all, mut skin, mut n, mut skin_n) = (0.0f64, 0.0f64, 0usize, 0usize);
        for px in rgba.chunks_exact(4) {
            let (l, a, b) = lab(px[0], px[1], px[2]);
            let c = a.hypot(b);
            all += c as f64;
            n += 1;
            let hue = b.atan2(a).to_degrees();
            if (20.0..=70.0).contains(&hue) && (6.0..=60.0).contains(&c) && (25.0..=90.0).contains(&l) {
                skin += c as f64;
                skin_n += 1;
            }
        }
        let mean = |s: f64, k: usize| if k == 0 { 0.0 } else { (s / k as f64) as f32 };
        Chroma { all: mean(all, n), skin: mean(skin, skin_n), skin_share: if n == 0 { 0.0 } else { skin_n as f32 / n as f32 } }
    }
}

/// sRGB bytes to CIELAB (D65).
fn lab(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let lin = |v: u8| {
        let c = v as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (lin(r), lin(g), lin(b));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f32| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    (116.0 * f(y) - 16.0, 500.0 * (f(x) - f(y)), 200.0 * (f(y) - f(z)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 96;
    const H: u32 = 54;

    /// A reference: a grey wall on the left, a face's skin in the middle,
    /// a sky on the right.
    fn reference() -> Vec<u8> {
        (0..W * H).flat_map(|i| match (i % W) * 3 / W {
            0 => [128u8, 128, 128, 255],
            1 => [196, 148, 122, 255],
            _ => [150, 178, 214, 255],
        }).collect()
    }

    /// The same scene shot darker, warmer and more saturated: as the
    /// warm-dark shot of the check was.
    fn source() -> Vec<u8> {
        let lin = |v: u8| { let c = v as f32 / 255.0; if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } };
        let enc = |c: f32| { let c = c.clamp(0.0, 1.0); ((if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }) * 255.0).round() as u8 };
        reference().chunks_exact(4).flat_map(|p| {
            let c = [lin(p[0]) * 0.6 * 1.18, lin(p[1]) * 0.6, lin(p[2]) * 0.6 * 0.82];
            let y = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
            let s = c.map(|v| y + (v - y) * 1.5);
            [enc(s[0]), enc(s[1]), enc(s[2]), 255]
        }).collect()
    }

    /// The node's picture with `settings`.
    fn drawn(settings: ColorMatchSettings) -> Vec<u8> {
        let mut g = NodeGraph::default();
        let id = g.ensure_color_match();
        g.nodes.iter_mut().find(|n| n.id == id).unwrap().params.color_match = Some(settings);
        GraphExecutor::new_cpu_only().execute(&source(), &g, W, H, 0.0, 0, None, None).unwrap().0
    }

    /// The match, drawn by the node, gives the source the reference's
    /// saturation on its skin, within a tenth, and keeps its grey wall
    /// grey; the estimate alone missed the skin by more.
    #[test]
    fn the_match_brings_the_skin_to_the_references_saturation() {
        let (refr, src) = (reference(), source());
        let skin = |rgba: &[u8]| -> f32 {
            let row = (H / 2) as usize;
            let px: Vec<f32> = (W / 3 + 4..2 * W / 3 - 4).map(|x| { let i = (row * W as usize + x as usize) * 4; let (_, a, b) = lab(rgba[i], rgba[i + 1], rgba[i + 2]); a.hypot(b) }).collect();
            px.iter().sum::<f32>() / px.len() as f32
        };
        let grey = |rgba: &[u8]| -> f32 { let i = ((H / 2 * W + 4) * 4) as usize; let (_, a, b) = lab(rgba[i], rgba[i + 1], rgba[i + 2]); a.hypot(b) };
        let want = skin(&refr);
        let matched = drawn(match_for_node(&src, W, H, &refr, W, H));
        let off = (skin(&matched) - want).abs() / want;
        assert!(off <= 0.10, "the skin's saturation {:.1} against the reference's {want:.1}", skin(&matched));
        assert!(grey(&matched) < 4.0, "the grey wall stays grey: chroma {:.1}", grey(&matched));
        let r = klipp_color::color_match::match_colors(&src, &refr);
        let estimate = drawn(ColorMatchSettings { exposure: r.exposure_adjust, temperature: r.temperature_adjust, tint: r.tint_adjust, saturation: r.saturation_adjust });
        let before = (skin(&estimate) - want).abs() / want;
        eprintln!("skin chroma: reference {want:.1}, source {:.1}, estimate {:.1}, matched {:.1}", skin(&src), skin(&estimate), skin(&matched));
        assert!(off < before, "closer than the estimate alone: {off:.3} against {before:.3}");
    }
}
