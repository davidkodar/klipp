//! A 3D LUT and a colour-space transform as effects (#1746).
//!
//! Both need what only the renderer knows: the LUT's file read, the
//! project's working space, the camera gamuts' matrices and the log
//! curves, all in `klipp-color`, which this crate can't depend on. The
//! renderer works them out once and hands them to the effect in its
//! `prepared` field, as it hands a colour fade its progress: never saved,
//! written before every run. An effect with nothing prepared passes the
//! picture through; the renderer says why (a LUT file it couldn't read is
//! a declined node, never a silent passthrough).
//!
//! The CPU here and the GPU's passes compute the same thing from the same
//! numbers: the LUT lookup is the grade's Lut3D one
//! ([`crate::apply_3d_lut_bracketed`] and its shader), and the transform
//! reads its curves from the same two tables on both.

use std::sync::Arc;

use rayon::prelude::*;

/// A 3×3 matrix, rows first.
pub type Matrix3 = [[f32; 3]; 3];

/// What the renderer worked out for an effect. Equal when it is the same
/// preparation, or holds the same numbers.
#[derive(Clone, Debug)]
pub struct Prepared<T>(pub Arc<T>);

impl<T: PartialEq> PartialEq for Prepared<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0
    }
}

impl<T> std::ops::Deref for Prepared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// A LUT read from its file, bracketed for the working space: what the
/// grade's Lut3D node runs.
#[derive(Clone, Debug, PartialEq)]
pub struct LutTable {
    /// The grid's size along each side.
    pub size: usize,
    /// RGB triples, blue-major, `size`³ of them.
    pub data: Vec<f32>,
    /// The working space into the LUT's input primaries, when they differ.
    pub pre: Option<Matrix3>,
    /// The LUT's output primaries back to the working space.
    pub post: Option<Matrix3>,
}

/// Samples in each curve table: a curve's linear values at codes evenly
/// spaced from 0 to 1. Two tables fill one GPU slot's table exactly.
pub const CURVE_SAMPLES: usize = 256;

/// A colour-space transform worked out for the working space.
#[derive(Clone, Debug, PartialEq)]
pub struct CstTables {
    /// The input gamut to the output gamut; `None` when they're the same.
    pub matrix: Option<Matrix3>,
    /// The input curve's linear values at [`CURVE_SAMPLES`] codes, when
    /// the picture is read through one.
    pub decode: Option<Vec<f32>>,
    /// The output curve's, when the result is written through one.
    pub encode: Option<Vec<f32>>,
}

/// The gamuts a colour-space transform converts between, in the order its
/// `input_gamut` and `output_gamut` number them. "Working space" is the
/// project's.
pub const GAMUTS: &[&str] = &[
    "Working space",
    "Rec.709",
    "Rec.2020",
    "Display P3",
    "DCI-P3",
    "S-Gamut3",
    "S-Gamut3.Cine",
    "V-Gamut",
    "Cinema Gamut",
    "ARRI Wide Gamut 3",
    "ARRI Wide Gamut 4",
    "RED Wide Gamut",
    "Blackmagic Wide Gamut",
];

/// The curves a colour-space transform reads or writes the picture
/// through, in the order its `input_curve` and `output_curve` number them.
/// "None" leaves the values as they are; "Linear" takes the picture's
/// displayed values as linear light; the rest are camera log curves.
pub const CURVES: &[&str] = &[
    "None",
    "Linear",
    "S-Log3",
    "S-Log2",
    "LogC3",
    "LogC4",
    "Canon Log",
    "Canon Log 3",
    "V-Log",
    "N-Log",
    "F-Log",
    "D-Log",
    "Apple Log",
    "L-Log",
    "Log3G10",
    "Blackmagic Film Gen 5",
];

/// `table`'s linear value at `code`, interpolated, carried on along the
/// end segments past 0 and 1.
pub fn curve_decode(table: &[f32], code: f32) -> f32 {
    let last = table.len() - 1;
    let x = code * last as f32;
    let i = (x.floor().max(0.0) as usize).min(last - 1);
    let f = x - i as f32;
    table[i] + (table[i + 1] - table[i]) * f
}

/// The code `table` gives `linear` at: the inverse of [`curve_decode`],
/// the segment found by halving (the curves rise), carried on past the
/// ends.
pub fn curve_encode(table: &[f32], linear: f32) -> f32 {
    let last = table.len() - 1;
    // The last sample at or below `linear`, kept off the final one so a
    // segment always follows it.
    let (mut lo, mut hi) = (0usize, last);
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if table[mid] <= linear { lo = mid } else { hi = mid }
    }
    let (a, b) = (table[lo], table[lo + 1]);
    let f = if b != a { (linear - a) / (b - a) } else { 0.0 };
    (lo as f32 + f) / last as f32
}

/// The sRGB encoding, as the LUT lookup and the display-scale effects use
/// it, carried on below 0.
fn encode_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

fn decode_srgb(c: f32) -> f32 {
    if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// One pixel's colour through `t`: read through the input curve (the
/// displayed value taken as its code), through the matrix, written
/// through the output curve (its code shown as the displayed value).
pub fn cst_pixel(t: &CstTables, rgb: [f32; 3]) -> [f32; 3] {
    let x = match &t.decode {
        Some(d) => rgb.map(|c| curve_decode(d, encode_srgb(c))),
        None => rgb,
    };
    let y = match t.matrix {
        Some(m) => [0, 1, 2].map(|r| m[r][0] * x[0] + m[r][1] * x[1] + m[r][2] * x[2]),
        None => x,
    };
    match &t.encode {
        Some(e) => y.map(|c| decode_srgb(curve_encode(e, c))),
        None => y,
    }
}

/// `t` over a frame, alpha kept.
pub fn apply_cst(frame: &mut [f32], t: &CstTables) {
    if t.matrix.is_none() && t.decode.is_none() && t.encode.is_none() {
        return;
    }
    frame.par_chunks_exact_mut(4).for_each(|p| {
        let [r, g, b] = cst_pixel(t, [p[0], p[1], p[2]]);
        p[0] = r;
        p[1] = g;
        p[2] = b;
    });
}

/// `lut` over a frame by `opacity`, as the grade's Lut3D node applies it.
pub fn apply_lut(frame: &mut [f32], lut: &LutTable, opacity: f32, tetrahedral: bool) {
    crate::cpu::apply_3d_lut_bracketed(frame, &lut.data, lut.size, opacity, lut.pre, lut.post, tetrahedral);
}

/// The two curve tables one GPU slot holds, the input curve's first; a
/// curve not used is zeros, which the pass doesn't read.
pub fn slot_table(t: &CstTables) -> Vec<f32> {
    let mut out = vec![0.0; CURVE_SAMPLES * 2];
    if let Some(d) = &t.decode {
        out[..CURVE_SAMPLES].copy_from_slice(&d[..CURVE_SAMPLES]);
    }
    if let Some(e) = &t.encode {
        out[CURVE_SAMPLES..].copy_from_slice(&e[..CURVE_SAMPLES]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s_curve() -> Vec<f32> {
        (0..CURVE_SAMPLES).map(|i| {
            let c = i as f32 / (CURVE_SAMPLES - 1) as f32;
            (2.0f32.powf(c * 10.0) - 1.0) / 64.0
        }).collect()
    }

    #[test]
    fn encode_undoes_decode_inside_and_past_the_table() {
        let t = s_curve();
        for code in [-0.1, 0.0, 0.013, 0.25, 0.5, 0.731, 1.0, 1.08] {
            let back = curve_encode(&t, curve_decode(&t, code));
            assert!((back - code).abs() < 1e-4, "{code} came back {back}");
        }
    }

    #[test]
    fn a_transform_with_nothing_to_do_leaves_the_picture() {
        let t = CstTables { matrix: None, decode: None, encode: None };
        let mut f = vec![0.2, -0.1, 3.0, 0.5];
        apply_cst(&mut f, &t);
        assert_eq!(f, [0.2, -0.1, 3.0, 0.5]);
    }

    #[test]
    fn reading_and_writing_through_one_curve_is_the_identity() {
        let t = CstTables { matrix: None, decode: Some(s_curve()), encode: Some(s_curve()) };
        for v in [0.0, 0.02, 0.18, 0.9, 4.0] {
            let out = cst_pixel(&t, [v; 3]);
            assert!((out[0] - v).abs() < 1e-4 * v.max(1.0), "{v} came back {}", out[0]);
        }
    }
}
