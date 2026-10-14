//! The form an OFX plug-in is given a frame in (#2013): Rec.709 primaries,
//! sRGB-encoded, 8 bits a channel, straight alpha. One function makes it,
//! for the frame a plug-in processes and for the frames at other times it
//! reads, so the two are the same rendering whatever the project's
//! working space.

use klipp_core::WorkingColorspace;

/// `linear` (RGBA in `working`) as an OFX plug-in takes it.
pub fn encode(linear: &[f32], working: WorkingColorspace) -> Vec<u8> {
    use rayon::prelude::*;
    let m = crate::primaries::working_space_conversion_matrix(working, WorkingColorspace::Rec709Linear);
    let byte = |v: f32| (crate::transfer::linear_to_srgb(v).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let mut out = vec![0u8; linear.len() / 4 * 4];
    out.par_chunks_mut(4 * 4096).zip(linear.par_chunks(4 * 4096)).for_each(|(o, i)| {
        for (o, px) in o.chunks_exact_mut(4).zip(i.chunks_exact(4)) {
            let c = match m {
                Some(m) => crate::primaries::matrix3_mul_vec(m, [px[0], px[1], px[2]]),
                None => [px[0], px[1], px[2]],
            };
            o.copy_from_slice(&[byte(c[0]), byte(c[1]), byte(c[2]), (px[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8]);
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Rec.709 encoding is the sRGB curve to the byte; another working
    /// space's picture is first put in Rec.709 primaries, so one colour in
    /// either space encodes alike. Control: left in ACEScg primaries it
    /// would not.
    #[test]
    fn a_colour_encodes_alike_from_either_working_space() {
        let rec709 = [0.6f32, 0.2, 0.05, 1.0];
        let to_ap1 = crate::primaries::working_space_conversion_matrix(WorkingColorspace::Rec709Linear, WorkingColorspace::AcesCG).unwrap();
        let ap1 = crate::primaries::matrix3_mul_vec(to_ap1, [rec709[0], rec709[1], rec709[2]]);
        let a = encode(&rec709, WorkingColorspace::Rec709Linear);
        let b = encode(&[ap1[0], ap1[1], ap1[2], 1.0], WorkingColorspace::AcesCG);
        assert!(a.iter().zip(&b).all(|(x, y)| x.abs_diff(*y) <= 1), "{a:?} vs {b:?}");
        assert_eq!(a[0], (crate::transfer::linear_to_srgb(0.6) * 255.0 + 0.5) as u8);
        let unconverted = encode(&[ap1[0], ap1[1], ap1[2], 1.0], WorkingColorspace::Rec709Linear);
        assert!(unconverted.iter().zip(&a).any(|(x, y)| x.abs_diff(*y) > 5), "the control: {unconverted:?}");
    }
}
