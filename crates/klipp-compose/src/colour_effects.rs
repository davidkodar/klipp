//! What a LUT or colour-space transform effect needs before it runs
//! (#1746): the LUT's file read and bracketed for the working space, the
//! transform's matrix and curve tables. Worked out here, where the colour
//! science is, and handed to the effect's `prepared` field before each run
//! on every route, as the effects' authored colours are put into the
//! working space ([`crate::render::Renderer`]'s `authored_colours`).
//!
//! A LUT file is read once and again only when it changes on disk; one
//! that can't be read leaves the effect unprepared (it passes the picture)
//! and is a declined node with its reason, never a silent passthrough.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use klipp_color::primaries::{invert3, matrix3_mul};
use klipp_core::{ColorPrimaries, LogFormat, WorkingColorspace};
use klipp_effects::colour_transform::{CstTables, LutTable, Prepared, CURVE_SAMPLES};
use klipp_effects::BuiltinEffect;

/// A LUT file as last read: when it was changed, and its table or why it
/// couldn't be read.
type Read = (Option<SystemTime>, Result<Arc<LutTable>, String>);

/// The preparations made so far, kept so each is made once.
#[derive(Default)]
pub struct ColourEffects {
    luts: HashMap<(String, WorkingColorspace), Read>,
    transforms: HashMap<([u32; 4], WorkingColorspace), Arc<CstTables>>,
}

impl ColourEffects {
    /// Each LUT and transform in `fx` prepared for `working`; a LUT whose
    /// file can't be read is left with nothing.
    pub fn prepare(&mut self, fx: &mut [BuiltinEffect], working: WorkingColorspace) {
        for effect in fx {
            match effect {
                BuiltinEffect::Lut3D { path, prepared, .. } => *prepared = self.lut(path, working).ok().map(Prepared),
                BuiltinEffect::ColourSpaceTransform { input_gamut, input_curve, output_gamut, output_curve, prepared } => {
                    let key = [*input_gamut, *input_curve, *output_gamut, *output_curve];
                    let t = self.transforms.entry((key, working)).or_insert_with(|| Arc::new(transform(key, working)));
                    *prepared = Some(Prepared(t.clone()));
                }
                _ => {}
            }
        }
    }

    /// `path`'s LUT for `working`, read again when the file changed; or
    /// why it can't be had, in words a person reads.
    pub fn lut(&mut self, path: &str, working: WorkingColorspace) -> Result<Arc<LutTable>, String> {
        if path.is_empty() {
            return Err("no LUT file is chosen".into());
        }
        // A look from the bundled library is stored by its name in it.
        let file = klipp_core::assets::resolve_lut_path(path, klipp_core::assets::bundled_luts_dir());
        let modified = std::fs::metadata(&file).and_then(|m| m.modified()).ok();
        let key = (path.to_owned(), working);
        match self.luts.get(&key) {
            Some((when, read)) if *when == modified && modified.is_some() => read.clone(),
            _ => {
                let read = read_lut(&file, working);
                self.luts.insert(key, (modified, read.clone()));
                read
            }
        }
    }
}

fn read_lut(path: &Path, working: WorkingColorspace) -> Result<Arc<LutTable>, String> {
    if !path.exists() {
        return Err(format!("its LUT file {} isn't there", path.display()));
    }
    let lut = klipp_color::load_lut(path).map_err(|e| format!("its LUT file {} can't be read: {e}", path.display()))?;
    let (pre, post) = klipp_color::lut_bracket(working, lut.input_space, lut.output_space);
    Ok(Arc::new(LutTable { size: lut.size, data: lut.data, pre, post }))
}

/// The gamut `input_gamut`/`output_gamut` number, in
/// [`klipp_effects::colour_transform::GAMUTS`]' order: `None` for the
/// working space.
fn gamut(index: u32) -> Option<ColorPrimaries> {
    use ColorPrimaries as P;
    Some(match index {
        1 => P::Bt709,
        2 => P::Bt2020,
        3 => P::DisplayP3,
        4 => P::DciP3,
        5 => P::SGamut3,
        6 => P::SGamut3Cine,
        7 => P::VGamut,
        8 => P::CanonCinemaGamut,
        9 => P::AlexaWideGamut3,
        10 => P::AlexaWideGamut4,
        11 => P::RedWideGamut,
        12 => P::BmdWideGamut,
        _ => return None,
    })
}

/// The camera log curve `input_curve`/`output_curve` number from 2, in
/// [`klipp_effects::colour_transform::CURVES`]' order.
fn log_curve(index: u32) -> Option<LogFormat> {
    use LogFormat as L;
    Some(match index {
        2 => L::SLog3,
        3 => L::SLog2,
        4 => L::LogC3,
        5 => L::LogC4,
        6 => L::CanonLog,
        7 => L::CanonLog3,
        8 => L::VLog,
        9 => L::NLog,
        10 => L::FLog,
        11 => L::DJIDLog,
        12 => L::AppleLog,
        13 => L::LeicaLLog,
        14 => L::RedLog3G10,
        15 => L::BmdFilmGen5,
        _ => return None,
    })
}

/// A curve's table: its linear values at evenly spaced codes. `None` for
/// no curve (0, or a number past the list).
fn curve_table(index: u32) -> Option<Vec<f32>> {
    let code = |i: usize| i as f32 / (CURVE_SAMPLES - 1) as f32;
    match index {
        1 => Some((0..CURVE_SAMPLES).map(code).collect()),
        _ => log_curve(index).map(|f| (0..CURVE_SAMPLES).map(|i| klipp_color::camera_log::log_format_to_linear(code(i), f)).collect()),
    }
}

/// The transform `[input_gamut, input_curve, output_gamut, output_curve]`
/// in `working`.
pub fn transform([in_gamut, in_curve, out_gamut, out_curve]: [u32; 4], working: WorkingColorspace) -> CstTables {
    let to_working = |g: u32| gamut(g).map(|p| klipp_color::camera_log::primaries_to_working_matrix(p, working));
    let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let matrix = match (to_working(in_gamut), to_working(out_gamut)) {
        _ if gamut(in_gamut) == gamut(out_gamut) => None,
        (from, to) => Some(matrix3_mul(to.map_or(identity, invert3), from.unwrap_or(identity))),
    };
    CstTables { matrix, decode: curve_table(in_curve), encode: curve_table(out_curve) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_picture_comes_out_as_its_scene_light() {
        // An S-Log3 code of 0.41 is 18% grey; a picture showing that code
        // comes out at 0.18 once read through the curve.
        let t = transform([6, 2, 6, 0], WorkingColorspace::Rec709Linear);
        assert!(t.matrix.is_none());
        let shown = klipp_color::transfer::srgb_to_linear(0.410_557);
        let out = klipp_effects::colour_transform::cst_pixel(&t, [shown; 3]);
        assert!((out[0] - 0.18).abs() < 0.002, "{out:?}");
    }

    #[test]
    fn a_gamut_and_back_is_the_identity() {
        let there = transform([0, 0, 9, 0], WorkingColorspace::AcesCG);
        let back = transform([9, 0, 0, 0], WorkingColorspace::AcesCG);
        let c = [0.3, 0.5, 0.1];
        let out = klipp_effects::colour_transform::cst_pixel(&back, klipp_effects::colour_transform::cst_pixel(&there, c));
        for i in 0..3 {
            assert!((out[i] - c[i]).abs() < 1e-5, "{out:?}");
        }
    }

    #[test]
    fn a_missing_lut_says_where_it_looked() {
        let mut c = ColourEffects::default();
        let why = c.lut("/nowhere/look.cube", WorkingColorspace::Rec709Linear).unwrap_err();
        assert!(why.contains("/nowhere/look.cube") && why.contains("isn't there"), "{why}");
        assert_eq!(c.lut("", WorkingColorspace::Rec709Linear).unwrap_err(), "no LUT file is chosen");
    }
}
