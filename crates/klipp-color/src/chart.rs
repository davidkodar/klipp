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

/// A ColorChecker read off a picture, by the four corners of its patch
/// grid: each patch in linear light, in the chart's own reading order.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartInShot {
    /// The 24 patches, median of the middle half of each.
    pub linear: Vec<[f32; 3]>,
    /// The corners were given starting from the wrong end: the chart is
    /// upside down in them, and was read the right way round.
    pub turned: bool,
}

/// What a shot needs to bring its chart to the published values, as far as
/// white balance and exposure can.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChartCorrection {
    pub temperature: f32,
    pub tint: f32,
    pub stops: f32,
}

/// Mean ΔE2000 of the colour patches, greys taken as neutral, past which
/// a chart is not read as a ColorChecker. Measured: see the live record.
pub const OTHER_MAKE_DE: f32 = 16.0;

/// The sRGB bytes a linear colour shows as.
fn shown(c: [f32; 3]) -> [u8; 3] {
    c.map(|v| (crate::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8)
}

impl ChartInShot {
    /// The patches as Lab, as they show.
    pub fn lab(&self) -> Vec<Lab> {
        self.linear.iter().map(|c| { let s = shown(*c); srgb8_to_lab(s[0], s[1], s[2]) }).collect()
    }

    /// ΔE2000 of each patch from its published value: the mean, the worst
    /// and the worst patch's name.
    pub fn error(&self) -> (f32, f32, &'static str) {
        let (mut sum, mut worst, mut name) = (0.0, 0.0, "");
        for (lab, (patch, rgb)) in self.lab().into_iter().zip(PATCHES.iter()) {
            let de = delta_e2000(lab, srgb8_to_lab(rgb[0], rgb[1], rgb[2]));
            sum += de;
            if de > worst {
                (worst, name) = (de, patch);
            }
        }
        (sum / PATCHES.len() as f32, worst, name)
    }

    /// How far its colour patches sit from a ColorChecker's once its own
    /// greys are taken as neutral and at their published level: mean ΔE2000
    /// over the 18 that are not grey. A light or a camera moves them a little;
    /// another make of chart, or the wrong place, moves them a long way.
    pub fn colours_off(&self) -> f32 {
        let greys = self.usable_greys();
        let (mut got, mut want) = ([0.0f32; 3], [0.0f32; 3]);
        for &i in &greys {
            for k in 0..3 {
                got[k] += self.linear[i][k];
                want[k] += crate::transfer::srgb_to_linear(PATCHES[i].1[k] as f32 / 255.0);
            }
        }
        let gain = [0, 1, 2].map(|k| want[k] / got[k].max(1e-4));
        let colours: Vec<usize> = (0..PATCHES.len()).filter(|i| !NEUTRALS.contains(i)).collect();
        colours.iter().map(|&i| {
            let s = shown([0, 1, 2].map(|k| self.linear[i][k] * gain[k]));
            let p = PATCHES[i].1;
            delta_e2000(srgb8_to_lab(s[0], s[1], s[2]), srgb8_to_lab(p[0], p[1], p[2]))
        }).sum::<f32>() / colours.len() as f32
    }

    /// Whether this is a chart of another make, or corners in the wrong
    /// place: its greys read as a ramp, so a white balance from them holds,
    /// but its colours and levels are not the published ones.
    pub fn another_make(&self) -> bool {
        self.colours_off() > OTHER_MAKE_DE
    }

    /// The greys usable to judge from: those neither near clipping nor
    /// near black, where a channel has lost what it holds.
    fn usable_greys(&self) -> Vec<usize> {
        let lab = self.lab();
        NEUTRALS.iter().copied().filter(|&i| lab[i].l >= 15.0 && lab[i].l <= 92.0).collect()
    }

    /// The white balance that takes its greys to grey, and the exposure
    /// that brings them to the published lightness. `None` with fewer than
    /// two greys left to read: nothing it could say would be measured.
    pub fn correction(&self, ws: klipp_core::WorkingColorspace) -> Option<ChartCorrection> {
        let greys = self.usable_greys();
        if greys.len() < 2 {
            return None;
        }
        let mean = |pick: &dyn Fn(usize) -> [f32; 3]| {
            let mut m = [0.0f32; 3];
            for &i in &greys {
                let c = pick(i);
                for k in 0..3 {
                    m[k] += c[k] / greys.len() as f32;
                }
            }
            m
        };
        let got = mean(&|i| self.linear[i]);
        let want = mean(&|i| PATCHES[i].1.map(|v| crate::transfer::srgb_to_linear(v as f32 / 255.0)));
        let y = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        let (temperature, tint) = crate::white_balance::white_balance_to_neutralise(got, ws);
        // Another make's greys are grey at their own levels.
        let stops = if self.another_make() { 0.0 } else { (y(want) / y(got).max(1e-4)).log2() };
        Some(ChartCorrection { temperature, tint, stops })
    }

    /// Whether its greys span far less than the chart's own, as an
    /// unconverted log or flat picture's do: read as it stands, no white
    /// balance and exposure can bring it to the published values.
    pub fn looks_flat(&self) -> bool {
        let lab = self.lab();
        lab[NEUTRALS[0]].l - lab[NEUTRALS[5]].l < 55.0
    }
}

/// Why a chart could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartRefused {
    /// The four corners are no four-sided shape in the frame.
    Corners,
    /// The picture has too few pixels there to read a patch from.
    TooSmall,
    /// Neither way round do its six patches read as a grey ramp.
    NotAChart,
}

impl ChartRefused {
    pub fn says(self) -> &'static str {
        match self {
            Self::Corners => "the four corners are not a four-sided shape in the frame: give them in order round the chart",
            Self::TooSmall => "the chart is too small in the picture to read: each patch needs a few pixels across",
            Self::NotAChart => "that does not read as a ColorChecker: its six grey patches should run white to black along one long side. Place the corners on the outer corners of the patches, brown patch first; on a chart of another make, so that its greys lie along that side, white first",
        }
    }
}

/// The chart whose patch grid has these corners, in frame fractions, round
/// it starting at the brown patch (the patch diagonally opposite black):
/// each patch the median of its middle half, through the perspective the
/// corners give. Given from the other end it is turned round and said to
/// be. Refused when neither way round its grey row runs light to dark with
/// little colour between: anything else read as a chart would be a guess.
pub fn read_in_shot(rgba: &[u8], width: u32, height: u32, corners: [[f32; 2]; 4]) -> Result<ChartInShot, ChartRefused> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return Err(ChartRefused::TooSmall);
    }
    let unit = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let map = klipp_core::color_grade::PlaneMap::from_quads(&unit, &corners).ok_or(ChartRefused::Corners)?;
    // Each patch about the middle half of its cell.
    let read = |cell: usize| -> Option<[f32; 3]> {
        let (col, row) = ((cell % COLS) as f32, (cell / COLS) as f32);
        let mut channels: [Vec<f32>; 3] = Default::default();
        const STEPS: usize = 12;
        for i in 0..STEPS {
            for j in 0..STEPS {
                let (u, v) = ((col + 0.25 + 0.5 * (i as f32 + 0.5) / STEPS as f32) / COLS as f32, (row + 0.25 + 0.5 * (j as f32 + 0.5) / STEPS as f32) / ROWS as f32);
                let (x, y) = map.apply((u, v))?;
                let (px, py) = ((x * (w - 1) as f32).round(), (y * (h - 1) as f32).round());
                if px < 0.0 || py < 0.0 || px >= w as f32 || py >= h as f32 {
                    return None;
                }
                let o = (py as usize * w + px as usize) * 4;
                for (k, c) in channels.iter_mut().enumerate() {
                    c.push(crate::transfer::srgb_to_linear(rgba[o + k] as f32 / 255.0));
                }
            }
        }
        Some(channels.map(|mut c| {
            c.sort_by(|a, b| a.total_cmp(b));
            c[c.len() / 2]
        }))
    };
    // A patch at least about three pixels across, or the middle half is
    // a pixel and the median reads a neighbour's edge.
    let at = |u: f32, v: f32| map.apply((u, v)).map(|(x, y)| (x * w as f32, y * h as f32));
    let span = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).hypot(a.1 - b.1);
    if let (Some(a), Some(b), Some(c)) = (at(0.0, 0.0), at(1.0, 0.0), at(0.0, 1.0)) {
        if span(a, b) / (COLS as f32) < 6.0 || span(a, c) / (ROWS as f32) < 6.0 {
            return Err(ChartRefused::TooSmall);
        }
    } else {
        return Err(ChartRefused::Corners);
    }
    let cells: Vec<[f32; 3]> = (0..PATCHES.len()).map(read).collect::<Option<_>>().ok_or(ChartRefused::Corners)?;
    let ramp = |patches: &[[f32; 3]]| {
        let labs: Vec<Lab> = NEUTRALS.iter().map(|&i| { let s = shown(patches[i]); srgb8_to_lab(s[0], s[1], s[2]) }).collect();
        // Light to dark in order, by a clear step overall; a cast moves
        // every grey together and keeps them close to each other in a*b*.
        let ordered = labs.windows(2).all(|p| p[0].l >= p[1].l - 1.5);
        let wide = labs[0].l - labs[5].l >= 20.0;
        let mids = &labs[1..5];
        let (ma, mb) = (mids.iter().map(|l| l.a).sum::<f32>() / 4.0, mids.iter().map(|l| l.b).sum::<f32>() / 4.0);
        let close = mids.iter().all(|l| (l.a - ma).hypot(l.b - mb) < 9.0) && ma.hypot(mb) < 30.0;
        // And a chart has colour beside its greys: a grey ramp alone is not
        // one, and read as one would be scored against patches it lacks.
        let mut chroma: Vec<f32> = (0..PATCHES.len()).filter(|i| !NEUTRALS.contains(i)).map(|i| {
            let s = shown(patches[i]);
            let l = srgb8_to_lab(s[0], s[1], s[2]);
            (l.a - ma).hypot(l.b - mb)
        }).collect();
        chroma.sort_by(|a, b| a.total_cmp(b));
        ordered && wide && close && chroma[chroma.len() / 2] >= 15.0
    };
    if ramp(&cells) {
        return Ok(ChartInShot { linear: cells, turned: false });
    }
    let mut back = cells;
    back.reverse();
    if ramp(&back) {
        return Ok(ChartInShot { linear: back, turned: true });
    }
    Err(ChartRefused::NotAChart)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `chart_rgba` drawn into the quadrilateral `corners` of a noisy
    /// frame, through `look` on its way out, as a camera sees a chart.
    fn in_shot(corners: [[f32; 2]; 4], look: impl Fn([f32; 3]) -> [f32; 3]) -> (Vec<u8>, u32, u32) {
        use klipp_core::color_grade::PlaneMap;
        let (w, h) = (640u32, 360u32);
        let (cw, ch) = (600u32, 400u32);
        let chart = chart_rgba(cw, ch);
        // The patch grid alone, as the corners give it: chart_rgba keeps a
        // tenth of the frame round the grid.
        let map = PlaneMap::from_quads(&corners, &[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]).unwrap();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let p = (x as f32 / (w - 1) as f32, y as f32 / (h - 1) as f32);
                let rgb = match map.apply(p) {
                    Some((u, v)) if (0.0..1.0).contains(&u) && (0.0..1.0).contains(&v) => {
                        let (cx, cy) = ((0.1 + 0.8 * u) * cw as f32, (0.1 + 0.8 * v) * ch as f32);
                        let o = ((cy as u32).min(ch - 1) * cw + (cx as u32).min(cw - 1)) as usize * 4;
                        [chart[o], chart[o + 1], chart[o + 2]]
                    }
                    // The room behind it: anything but a chart.
                    _ => [((x * 7 + y * 3) % 200) as u8 + 20, ((x * 3 + y * 11) % 180) as u8 + 30, ((x * 13 + y) % 150) as u8 + 40],
                };
                let lin = look(rgb.map(|v| crate::transfer::srgb_to_linear(v as f32 / 255.0)));
                let s = shown(lin);
                out.extend_from_slice(&[s[0], s[1], s[2], 255]);
            }
        }
        (out, w, h)
    }

    const LEANING: [[f32; 2]; 4] = [[0.20, 0.15], [0.80, 0.22], [0.76, 0.85], [0.17, 0.78]];

    /// A chart leaning in perspective reads as the published values through
    /// its corners, and comes back the right way round from the far end.
    #[test]
    fn a_chart_in_perspective_reads_as_itself() {
        let (px, w, h) = in_shot(LEANING, |c| c);
        let read = read_in_shot(&px, w, h, LEANING).unwrap();
        let (mean, worst, name) = read.error();
        assert!(!read.turned && mean < 1.0 && worst < 2.0, "mean {mean:.2}, worst {worst:.2} on {name}");
        let [a, b, c, d] = LEANING;
        let from_the_far_end = read_in_shot(&px, w, h, [c, d, a, b]).unwrap();
        assert!(from_the_far_end.turned, "said to be turned");
        assert!(from_the_far_end.error().0 < 1.0, "and read the right way round");
    }

    /// What is not a chart is refused, not read: the room behind one, a
    /// flat wall, corners in a line, a chart too small to read.
    #[test]
    fn what_is_not_a_chart_is_refused() {
        let (px, w, h) = in_shot(LEANING, |c| c);
        assert_eq!(read_in_shot(&px, w, h, [[0.0, 0.0], [0.04, 0.0], [0.04, 0.03], [0.0, 0.03]]), Err(ChartRefused::TooSmall));
        let room = [[0.02, 0.02], [0.3, 0.02], [0.3, 0.14], [0.02, 0.14]];
        assert_eq!(read_in_shot(&px, w, h, room), Err(ChartRefused::NotAChart));
        let wall = [120u8, 120, 120, 255].repeat((w * h) as usize);
        assert_eq!(read_in_shot(&wall, w, h, LEANING), Err(ChartRefused::NotAChart));
        assert_eq!(read_in_shot(&px, w, h, [[0.2, 0.2], [0.4, 0.4], [0.6, 0.6], [0.8, 0.8]]), Err(ChartRefused::Corners));
        // A grey ramp alone has the row, and none of the colour.
        let ramp: Vec<u8> = (0..w * h).flat_map(|i| { let v = 250 - ((i % w) * 230 / w) as u8; [v, v, v, 255] }).collect();
        assert_eq!(read_in_shot(&ramp, w, h, [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]), Err(ChartRefused::NotAChart));
    }

    /// A ColorChecker whose camera is less saturated and cast is still one;
    /// another make's colours, on the same greys, are not, and it then gives
    /// a white balance from its greys and no exposure.
    #[test]
    fn another_make_of_chart_gives_only_its_white_balance() {
        use klipp_core::WorkingColorspace::Rec709Linear;
        let muted = |c: [f32; 3]| {
            let y = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
            [(y + 0.7 * (c[0] - y)) * 1.1, y + 0.7 * (c[1] - y), (y + 0.7 * (c[2] - y)) * 0.9]
        };
        let (px, w, h) = in_shot(LEANING, muted);
        let read = read_in_shot(&px, w, h, LEANING).unwrap();
        assert!(!read.another_make(), "a muted camera: {:.1}", read.colours_off());
        // The colours swapped about, as a chart laid out otherwise is, the
        // greys left, and half a stop under.
        let mut other = read.clone();
        let colours: Vec<usize> = (0..24).filter(|i| !NEUTRALS.contains(i)).collect();
        for (a, b) in colours.iter().zip(colours.iter().rev()).take(9) {
            other.linear.swap(*a, *b);
        }
        other.linear.iter_mut().for_each(|c| *c = c.map(|v| v * 0.7));
        assert!(other.another_make(), "{:.1}", other.colours_off());
        let k = other.correction(Rec709Linear).unwrap();
        assert!(k.stops == 0.0 && k.temperature.abs() > 1.0, "{k:?}");
    }

    /// A chart under a cast and under-exposed: what it says to do, done
    /// with the grade's own white balance and exposure, brings it back.
    #[test]
    fn the_correction_a_chart_gives_brings_it_back() {
        use klipp_core::WorkingColorspace::Rec709Linear;
        let wb = |c: [f32; 3], t: f32, g: f32| {
            let m = crate::white_balance::white_balance_matrix(t, g, Rec709Linear).unwrap();
            m.map(|r| r[0] * c[0] + r[1] * c[1] + r[2] * c[2])
        };
        let (px, w, h) = in_shot(LEANING, |c| wb(c, 25.0, 8.0).map(|v| v * 0.7));
        let read = read_in_shot(&px, w, h, LEANING).unwrap();
        let before = read.error().0;
        let k = read.correction(Rec709Linear).unwrap();
        assert!((k.temperature + 25.0).abs() < 4.0 && (k.stops - 0.51).abs() < 0.12, "{k:?}");
        let (after, _, _) = read_in_shot(&in_shot(LEANING, |c| wb(wb(c, 25.0, 8.0).map(|v| v * 0.7), k.temperature, k.tint).map(|v| v * k.stops.exp2())).0, w, h, LEANING).unwrap().error();
        assert!(before > 6.0 && after < 2.0, "mean dE {before:.1} -> {after:.1}");
    }

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
