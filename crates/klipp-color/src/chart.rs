//! A colour chart the render path can be held against.
//!
//! A colourist trusts a grade only as far as they trust the monitor and
//! the pipeline behind it, so the first thing on a calibrated stage is a
//! known chart shot under known light: put a patch in, read it out, and
//! see whether it came back where it went in. Software has the same
//! obligation and an easier job — the chart can be *synthesised*, so
//! there is no camera, no lens and no lamp between the number sent and
//! the number returned. Anything that moves is the pipeline.
//!
//! What this catches, and what nothing else here catches: an output
//! transform applied twice, a working space that disagrees with the
//! delivery, a LUT left on, a legal-range squeeze, a green-channel
//! matrix slip. Those do not look like bugs in a single frame — they
//! look like "the grade needs a bit more warmth", and a grading loop
//! will happily chase them for ever.
//!
//! **The patch values are the published ones.** They are the BabelColor
//! averages of the X-Rite ColorChecker Classic, the figures the industry
//! quotes, as 8-bit sRGB under D65. They are not invented here and not
//! re-derived from a photograph: the whole point is an external
//! reference. (The photographed chart in the test material is for the
//! opposite job — teaching the skin band what a *camera* does to those
//! patches.)

use crate::measure::{delta_e2000, srgb8_to_lab, Lab};
use serde::{Deserialize, Serialize};

/// The 24 patches of an X-Rite ColorChecker Classic, in reading order
/// (row 1 left to right, then row 2...), as 8-bit sRGB / D65.
///
/// Source: BabelColor's average of 30 charts, the published sRGB values.
/// Row 4 is the neutral ramp — white, neutral 8, 6.5, 5, 3.5, black —
/// and it is the row that catches a transfer-function fault, because
/// those six are a tone curve with the colour taken out.
pub const PATCHES: [(&str, [u8; 3]); 24] = [
    ("dark skin", [115, 82, 68]),
    ("light skin", [194, 150, 130]),
    ("blue sky", [98, 122, 157]),
    ("foliage", [87, 108, 67]),
    ("blue flower", [133, 128, 177]),
    ("bluish green", [103, 189, 170]),
    ("orange", [214, 126, 44]),
    ("purplish blue", [80, 91, 166]),
    ("moderate red", [193, 90, 99]),
    ("purple", [94, 60, 108]),
    ("yellow green", [157, 188, 64]),
    ("orange yellow", [224, 163, 46]),
    ("blue", [56, 61, 150]),
    ("green", [70, 148, 73]),
    ("red", [175, 54, 60]),
    ("yellow", [231, 199, 31]),
    ("magenta", [187, 86, 149]),
    ("cyan", [8, 133, 161]),
    ("white", [243, 243, 242]),
    ("neutral 8", [200, 200, 200]),
    ("neutral 6.5", [160, 160, 160]),
    ("neutral 5", [122, 122, 121]),
    ("neutral 3.5", [85, 85, 85]),
    ("black", [52, 52, 52]),
];

/// The indices of the neutral ramp, lightest first.
pub const NEUTRALS: [usize; 6] = [18, 19, 20, 21, 22, 23];

/// The chart is 6 patches across and 4 down, on a mid-grey surround —
/// mid-grey because a black surround makes a bloom-prone pipeline look
/// clean and a white one hides highlight clipping.
pub const COLS: usize = 6;
pub const ROWS: usize = 4;

/// The chart as sRGB RGBA8, `width` × `height`. Any size works; the
/// patches are laid out proportionally and read back from their centres.
pub fn chart_rgba(width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0u8; w * h * 4];
    // The surround: neutral 5, the middle of the chart's own ramp.
    for px in out.chunks_exact_mut(4) {
        px.copy_from_slice(&[122, 122, 121, 255]);
    }
    for (i, (_, rgb)) in PATCHES.iter().enumerate() {
        let (x0, y0, pw, ph) = patch_box(i, width, height);
        for y in y0..(y0 + ph).min(h as u32) {
            for x in x0..(x0 + pw).min(w as u32) {
                let o = ((y as usize) * w + x as usize) * 4;
                out[o] = rgb[0];
                out[o + 1] = rgb[1];
                out[o + 2] = rgb[2];
                out[o + 3] = 255;
            }
        }
    }
    out
}

/// Where patch `i` sits: x, y, width, height in pixels.
fn patch_box(i: usize, width: u32, height: u32) -> (u32, u32, u32, u32) {
    // A tenth of the frame as a margin all round, so a pipeline that
    // crops or overscans by a few percent still reads every patch.
    let (mx, my) = (width / 10, height / 10);
    let (gw, gh) = (width - 2 * mx, height - 2 * my);
    let (pw, ph) = (gw / COLS as u32, gh / ROWS as u32);
    let (col, row) = (i % COLS, i / COLS);
    (mx + col as u32 * pw, my + row as u32 * ph, pw, ph)
}

/// The mean sRGB of the middle half of each patch, read back from a
/// rendered chart. The middle half, not the whole patch, so a resample
/// or a soft edge cannot bleed a neighbour into the reading — the same
/// reason a colourist reads the centre of a chart patch and not its
/// border.
pub fn read_patches(rgba: &[u8], width: u32, height: u32) -> Vec<Lab> {
    let w = width as usize;
    (0..PATCHES.len())
        .map(|i| {
            let (x0, y0, pw, ph) = patch_box(i, width, height);
            let (ix, iy) = (pw / 4, ph / 4);
            let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
            for y in (y0 + iy)..(y0 + ph - iy).min(height) {
                for x in (x0 + ix)..(x0 + pw - ix).min(width) {
                    let o = ((y as usize) * w + x as usize) * 4;
                    if o + 3 >= rgba.len() {
                        continue;
                    }
                    r += rgba[o] as u64;
                    g += rgba[o + 1] as u64;
                    b += rgba[o + 2] as u64;
                    n += 1;
                }
            }
            let n = n.max(1);
            srgb8_to_lab((r / n) as u8, (g / n) as u8, (b / n) as u8)
        })
        .collect()
}

/// One patch, in and out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchResult {
    pub name: String,
    pub sent: Lab,
    pub back: Lab,
    /// ΔE2000 between them.
    pub de: f32,
}

/// What the render path did to the chart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChartCheck {
    pub patches: Vec<PatchResult>,
    pub max_de: f32,
    pub mean_de: f32,
    /// The worst patch, by name.
    pub worst: String,
    /// The most colour any of the six neutrals came back with. A neutral
    /// that is no longer neutral is a matrix or a white-point fault, and
    /// it is the finding that matters most: every other judgement in the
    /// tool set is made against neutrals.
    pub neutral_cast: f32,
    /// Whether the neutral ramp still runs light to dark without a step
    /// out of order — a transfer function applied twice folds it.
    pub ramp_ordered: bool,
    /// Patches that came back pinned at 0 or 255 in a channel although
    /// they went in inside the gamut.
    pub clipped: Vec<String>,
}

/// ΔE2000 tolerances for a round trip that should be the identity.
///
/// A ΔE2000 of 1.0 is the textbook just-noticeable difference for two
/// patches side by side, and it is also roughly what 8-bit quantisation
/// plus a resample can cost on its own — so 1.0 is not yet a fault.
/// Above 2.0 a difference is visible without the patches touching, which
/// is the point at which a grading decision made on that reading is a
/// decision made on the pipeline instead. Above 5.0 the picture is
/// simply a different colour.
pub const CHART_WARN_DE: f32 = 2.0;
pub const CHART_FAIL_DE: f32 = 5.0;
/// A neutral holds its neutrality to about this much chroma; beyond it
/// the cast is visible on skin, which is where casts get noticed first.
pub const NEUTRAL_CAST_FAIL: f32 = 1.5;

/// Hold a rendered chart against the one that was sent.
pub fn check(rendered_rgba: &[u8], width: u32, height: u32) -> ChartCheck {
    let back = read_patches(rendered_rgba, width, height);
    let mut patches = Vec::with_capacity(PATCHES.len());
    for (i, (name, rgb)) in PATCHES.iter().enumerate() {
        let sent = srgb8_to_lab(rgb[0], rgb[1], rgb[2]);
        let got = back[i];
        patches.push(PatchResult { name: (*name).to_string(), sent, back: got, de: delta_e2000(sent, got) });
    }
    let max_de = patches.iter().map(|p| p.de).fold(0.0f32, f32::max);
    let mean_de = patches.iter().map(|p| p.de).sum::<f32>() / patches.len() as f32;
    let worst = patches.iter().max_by(|a, b| a.de.total_cmp(&b.de)).map(|p| p.name.clone()).unwrap_or_default();
    let neutral_cast = NEUTRALS.iter().map(|&i| patches[i].back.chroma()).fold(0.0f32, f32::max);
    let ramp_ordered = NEUTRALS.windows(2).all(|w| patches[w[0]].back.l > patches[w[1]].back.l);
    // A patch is "clipped" when the rendered picture pins a channel that
    // the chart did not: the chart's own extremes are 8 and 243, so
    // nothing sent is at a rail to begin with.
    let w = width as usize;
    let mut clipped = Vec::new();
    for (i, (name, _)) in PATCHES.iter().enumerate() {
        let (x0, y0, pw, ph) = patch_box(i, width, height);
        let (cx, cy) = (x0 + pw / 2, y0 + ph / 2);
        let o = ((cy as usize) * w + cx as usize) * 4;
        if o + 2 < rendered_rgba.len() {
            let px = [rendered_rgba[o], rendered_rgba[o + 1], rendered_rgba[o + 2]];
            if px.iter().any(|&c| c == 0 || c == 255) {
                clipped.push((*name).to_string());
            }
        }
    }
    ChartCheck { patches, max_de, mean_de, worst, neutral_cast, ramp_ordered, clipped }
}

impl ChartCheck {
    /// Whether the path can be graded on.
    pub fn trustworthy(&self) -> bool {
        self.max_de < CHART_FAIL_DE && self.neutral_cast < NEUTRAL_CAST_FAIL && self.ramp_ordered && self.clipped.is_empty()
    }

    /// What to say about it, in the terms a colourist would use.
    pub fn findings(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if !self.ramp_ordered {
            out.push(("error", "the chart's grey ramp does not come back in order — two steps have swapped or merged, which is what a transfer function applied twice looks like".to_string()));
        }
        if self.neutral_cast >= NEUTRAL_CAST_FAIL {
            out.push((
                "error",
                format!("the chart's greys come back with {:.1} chroma on them — the path is not neutral, so every cast this tool reports is partly the pipeline's", self.neutral_cast),
            ));
        }
        if !self.clipped.is_empty() {
            out.push(("error", format!("{} came back pinned at a rail although {} sent inside the gamut — the path is clipping", self.clipped.join(", "), if self.clipped.len() == 1 { "it was" } else { "they were" })));
        }
        if self.max_de >= CHART_FAIL_DE {
            out.push(("error", format!("the chart comes back {:.1} ΔE out at worst ({}) — that is a different colour, not a rounding", self.max_de, self.worst)));
        } else if self.max_de >= CHART_WARN_DE {
            out.push(("warning", format!("the chart comes back {:.1} ΔE out at worst ({}) — visible, and enough to mislead a grade by a little", self.max_de, self.worst)));
        }
        if out.is_empty() {
            out.push(("ok", format!("the chart round-trips within {:.2} ΔE (mean {:.2}), greys stay grey and the ramp holds its order — readings from this path can be graded on", self.max_de, self.mean_de)));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chart_comes_back_as_itself_through_nothing() {
        // The identity "render": the chart read straight back. If this
        // is not near zero the reader, not the pipeline, is wrong.
        let (w, h) = (600u32, 400u32);
        let img = chart_rgba(w, h);
        let c = check(&img, w, h);
        assert!(c.max_de < 0.2, "identity round trip should be exact, was {} at {}", c.max_de, c.worst);
        assert!(c.trustworthy());
        assert!(c.ramp_ordered);
        assert!(c.clipped.is_empty(), "nothing in the chart sits at a rail: {:?}", c.clipped);
        assert!(c.neutral_cast < 0.6, "greys are grey: {}", c.neutral_cast);
    }

    #[test]
    fn a_cast_on_the_path_is_caught_by_the_greys() {
        // A blue-biased pipeline: +6% blue everywhere, the shape of a
        // white point set to the wrong one.
        let (w, h) = (600u32, 400u32);
        let mut img = chart_rgba(w, h);
        for px in img.chunks_exact_mut(4) {
            px[2] = ((px[2] as f32 * 1.06).min(255.0)) as u8;
        }
        let c = check(&img, w, h);
        assert!(c.neutral_cast >= NEUTRAL_CAST_FAIL, "a blue path should show on the greys, showed {}", c.neutral_cast);
        assert!(!c.trustworthy());
        assert!(c.findings().iter().any(|(s, t)| *s == "error" && t.contains("greys")));
    }

    #[test]
    fn a_transfer_applied_twice_folds_the_ramp() {
        // sRGB decoded once too often: the ramp stays ordered but the
        // patches move a long way, and the mid-greys move most.
        let (w, h) = (600u32, 400u32);
        let mut img = chart_rgba(w, h);
        for px in img.chunks_exact_mut(4) {
            for c in px.iter_mut().take(3) {
                let v = crate::transfer::srgb_to_linear(*c as f32 / 255.0);
                *c = (v * 255.0 + 0.5) as u8;
            }
        }
        let c = check(&img, w, h);
        assert!(c.max_de >= CHART_FAIL_DE, "a double transfer should fail, was {}", c.max_de);
        assert!(!c.trustworthy());
    }

    #[test]
    fn a_clipping_path_names_the_patches_it_lost() {
        let (w, h) = (600u32, 400u32);
        let mut img = chart_rgba(w, h);
        for px in img.chunks_exact_mut(4) {
            for c in px.iter_mut().take(3) {
                *c = ((*c as f32 - 40.0) * 1.6).clamp(0.0, 255.0) as u8;
            }
        }
        let c = check(&img, w, h);
        assert!(!c.clipped.is_empty(), "a crushed, stretched path clips");
        assert!(c.findings().iter().any(|(s, t)| *s == "error" && t.contains("pinned")));
    }

    #[test]
    fn the_patches_are_where_the_reader_looks_for_them() {
        // Every patch reads back as the value that was drawn, at every
        // sensible chart size, including sizes the grid does not divide.
        for (w, h) in [(300u32, 200u32), (1920, 1080), (641, 361)] {
            let img = chart_rgba(w, h);
            let back = read_patches(&img, w, h);
            for (i, (name, rgb)) in PATCHES.iter().enumerate() {
                let sent = srgb8_to_lab(rgb[0], rgb[1], rgb[2]);
                assert!(delta_e2000(sent, back[i]) < 0.2, "{name} at {w}x{h} read back wrong");
            }
        }
    }
}
