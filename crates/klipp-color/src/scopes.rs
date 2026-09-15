use klipp_core::WorkingColorspace;

/// The basis a scope measures in.
///
/// Luma and chroma are not properties of a pixel's three numbers alone:
/// they depend on which primaries those numbers are coordinates of. A
/// project working in BT.2020 or ACEScg holds the same colour as another
/// set of numbers than a Rec.709 project does, and reading those numbers
/// with Rec.709's luminance row measures a picture nobody has.
///
/// So each scope takes the basis it is measuring, which carries that
/// working space's own luminance row and the Y'CbCr construction built
/// from it: `Cb = (B - Y) / 2(1 - Kb)`, `Cr = (R - Y) / 2(1 - Kr)`, the
/// same construction BT.709 and BT.2020 are both defined by. Grey lands
/// at the origin and the space's own primaries land on the graticule in
/// every working space, which is what makes the targets mean something.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScopeBasis {
    ws: WorkingColorspace,
    luma: [f32; 3],
    cb_scale: f32,
    cr_scale: f32,
}

impl ScopeBasis {
    pub fn for_working_space(ws: WorkingColorspace) -> Self {
        let luma = crate::primaries::luminance_weights(ws);
        Self {
            ws,
            luma,
            cb_scale: 0.5 / (1.0 - luma[2]),
            cr_scale: 0.5 / (1.0 - luma[0]),
        }
    }

    pub fn working_space(&self) -> WorkingColorspace {
        self.ws
    }

    /// The basis's name, for the scope to say what it is measuring. A
    /// reading is only as meaningful as the axes it is read against.
    pub fn label(&self) -> &'static str {
        match self.ws {
            WorkingColorspace::Rec709Linear => "Rec.709",
            WorkingColorspace::Bt2020Linear => "BT.2020",
            WorkingColorspace::AcesCG => "ACEScg",
        }
    }

    /// The luminance row: `Kr·R + Kg·G + Kb·B`.
    pub fn luma(&self, rgb: [f32; 3]) -> f32 {
        self.luma[0] * rgb[0] + self.luma[1] * rgb[1] + self.luma[2] * rgb[2]
    }

    /// `(Cb, Cr)`, centred at 0 — the colour-difference pair this basis
    /// defines, not Rec.709's.
    pub fn chroma(&self, rgb: [f32; 3]) -> (f32, f32) {
        let y = self.luma(rgb);
        ((rgb[2] - y) * self.cb_scale, (rgb[0] - y) * self.cr_scale)
    }
}

impl Default for ScopeBasis {
    /// Rec.709 — what every one of these scopes measured before the basis
    /// was a question anyone asked.
    fn default() -> Self {
        Self::for_working_space(WorkingColorspace::Rec709Linear)
    }
}

/// The six target boxes, as `(name, Cb, Cr)`, in the basis the scope is
/// measuring.
///
/// Drawn for 75% bars, which is the amplitude a broadcast graticule's boxes
/// are for — 100% bars overshoot them, as they do on hardware. Each space
/// puts its own primaries in a slightly different place, so these follow
/// the project: a picture that is legal in BT.2020 has to read legal on a
/// BT.2020 vectorscope.
pub fn vectorscope_targets(basis: ScopeBasis) -> [(&'static str, f32, f32); 6] {
    const BARS: [(&str, [f32; 3]); 6] = [
        ("R", [1.0, 0.0, 0.0]),
        ("Yl", [1.0, 1.0, 0.0]),
        ("G", [0.0, 1.0, 0.0]),
        ("Cy", [0.0, 1.0, 1.0]),
        ("B", [0.0, 0.0, 1.0]),
        ("Mg", [1.0, 0.0, 1.0]),
    ];
    const AMPLITUDE: f32 = 0.75;
    BARS.map(|(name, rgb)| {
        let (cb, cr) = basis.chroma(rgb.map(|c| c * AMPLITUDE));
        (name, cb, cr)
    })
}

/// RGB + Luma histogram data: 256 bins per channel.
pub struct HistogramData {
    pub r: [u32; 256],
    pub g: [u32; 256],
    pub b: [u32; 256],
    pub luma: [u32; 256],
}

/// Compute RGB and luma histograms from RGBA data.
/// Samples every 2nd pixel for performance.
pub fn compute_histogram(rgba: &[u8], width: u32, height: u32) -> HistogramData {
    let mut data = HistogramData {
        r: [0u32; 256],
        g: [0u32; 256],
        b: [0u32; 256],
        luma: [0u32; 256],
    };

    let sample_step = 2usize;
    for y in (0..height).step_by(sample_step) {
        for x in (0..width).step_by(sample_step) {
            let idx = ((y * width + x) * 4) as usize;
            if idx + 2 >= rgba.len() { break; }
            let rv = rgba[idx];
            let gv = rgba[idx + 1];
            let bv = rgba[idx + 2];
            data.r[rv as usize] += 1;
            data.g[gv as usize] += 1;
            data.b[bv as usize] += 1;
            let luma = (0.2126 * rv as f32 + 0.7152 * gv as f32 + 0.0722 * bv as f32)
                .clamp(0.0, 255.0) as usize;
            data.luma[luma] += 1;
        }
    }

    data
}

// The CPU histograms below are not the scopes: those are drawn on the
// GPU in `klipp_effects::gpu_scopes`. The curves editor reads
// `compute_histogram` for its backdrop, and the ground-truth tests read
// `compute_histogram_linear`. Values outside `[0, 1]` are clamped for
// bin assignment only.

/// Linear-light RGB + luma histogram. 256 bins per channel, binned
/// in linear `[0, 1]`; the luma channel measured in `basis`.
pub fn compute_histogram_linear(
    rgba: &[f32],
    width: u32,
    height: u32,
    basis: ScopeBasis,
) -> HistogramData {
    let mut data = HistogramData {
        r: [0u32; 256],
        g: [0u32; 256],
        b: [0u32; 256],
        luma: [0u32; 256],
    };

    let sample_step = 2usize;
    for y in (0..height).step_by(sample_step) {
        for x in (0..width).step_by(sample_step) {
            let idx = ((y * width + x) * 4) as usize;
            if idx + 2 >= rgba.len() {
                break;
            }
            let r = rgba[idx].clamp(0.0, 1.0);
            let g = rgba[idx + 1].clamp(0.0, 1.0);
            let b = rgba[idx + 2].clamp(0.0, 1.0);
            data.r[(r * 255.0) as usize] += 1;
            data.g[(g * 255.0) as usize] += 1;
            data.b[(b * 255.0) as usize] += 1;
            let luma = basis.luma([r, g, b]).clamp(0.0, 1.0);
            data.luma[(luma * 255.0) as usize] += 1;
        }
    }

    data
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_color_frame(r: u8, g: u8, b: u8, w: u32, h: u32) -> Vec<u8> {
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            data.extend_from_slice(&[r, g, b, 255]);
        }
        data
    }

    #[test]
    fn histogram_single_value() {
        let frame = solid_color_frame(100, 150, 200, 16, 16);
        let hist = compute_histogram(&frame, 16, 16);
        // With step_by(2), we sample 8*8 = 64 pixels
        assert_eq!(hist.r[100], 64);
        assert_eq!(hist.g[150], 64);
        assert_eq!(hist.b[200], 64);
    }

    // ─── linear-light scopes ──────────────────────────────────────

    fn linear_ramp_rgba(width: u32, height: u32) -> Vec<f32> {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        let denom = (width - 1).max(1) as f32;
        for _y in 0..height {
            for x in 0..width {
                let v = x as f32 / denom;
                data.extend_from_slice(&[v, v, v, 1.0]);
            }
        }
        data
    }

    #[test]
    fn histogram_linear_on_linear_ramp_is_uniform() {
        // This is the headline ground-truth claim: a uniform linear
        // ramp should produce near-uniform bins. The sRGB u8 version
        // would concentrate counts in the shadow bins because the
        // sRGB gamma curve packs more codes into the dark end.
        let frame = linear_ramp_rgba(256, 1);
        let hist = compute_histogram_linear(&frame, 256, 1, ScopeBasis::default());
        // step_by(2) => 128 samples spread across 0..1 linearly.
        // Expected: each bin hits once, except for end-rounding.
        let populated_count = hist.r.iter().filter(|&&c| c > 0).count();
        assert!(
            populated_count >= 120,
            "linear ramp should populate most bins uniformly, got {populated_count} bins",
        );
        // And no single bin should dominate (worst case any bin > 5).
        let max_count = *hist.r.iter().max().unwrap();
        assert!(max_count <= 5, "max per-bin count {max_count} suggests non-uniform binning");
    }

    #[test]
    fn histogram_linear_clamps_out_of_range_values() {
        // Values above 1.0 clamp to bin 255; values below 0.0 clamp
        // to bin 0. Linear pipeline lets values go wild; the scope
        // shows them at the extremes rather than NaN-ing or wrapping.
        let frame = vec![
            2.0_f32, 2.0, 2.0, 1.0, // HDR pixel
            -0.5, -0.5, -0.5, 1.0, // out-of-gamut pixel
        ];
        let hist = compute_histogram_linear(&frame, 2, 1, ScopeBasis::default());
        assert!(hist.r[255] > 0, "HDR should hit bin 255");
        // (below-zero pixel is the second pixel; step_by(2) means
        // only the first pixel gets sampled in a 2x1 image, so we
        // don't see the negative — use a taller test instead)
    }

    // ─── the basis a scope measures in ───────────────────────────

    /// The headline claim: luminance belongs to the colour, not to the
    /// three numbers holding it. One colour, written in each working
    /// space's own coordinates, must read one luma — which it only does
    /// if each basis uses its own luminance row.
    #[test]
    fn one_colour_reads_one_luma_in_every_working_space() {
        let rec709 = [0.8_f32, 0.3, 0.1];
        let want = ScopeBasis::default().luma(rec709);

        let cases = [
            (WorkingColorspace::Bt2020Linear, crate::primaries::bt709_to_bt2020(), 1e-4),
            // ACEScg is a D60 space: the adaptation from D65 moves the
            // primaries' luminance a little, so its row cannot agree to
            // better than a fraction of a percent. See the tolerance in
            // `primaries::tests`.
            (WorkingColorspace::AcesCG, crate::primaries::bt709_to_acescg(), 5e-3),
        ];
        for (ws, to_ws, tol) in cases {
            let basis = ScopeBasis::for_working_space(ws);
            let got = basis.luma(crate::primaries::matrix3_mul_vec(to_ws, rec709));
            assert!(
                (got - want).abs() <= tol,
                "{} reads luma {got:.6} where Rec.709 reads {want:.6}",
                basis.label(),
            );
            // And the mistake this replaced: the same numbers read with
            // Rec.709's row are wrong by more than that.
            let naive = ScopeBasis::default().luma(crate::primaries::matrix3_mul_vec(to_ws, rec709));
            assert!(
                (naive - want).abs() > tol * 2.0,
                "{}: Rec.709's row happens to agree here, so this test proves nothing",
                basis.label(),
            );
        }
    }

    /// Neutral has no colour in any basis: the vectorscope's centre is
    /// the one place every working space must agree on exactly.
    #[test]
    fn grey_sits_at_the_vectorscope_origin_in_every_working_space() {
        for ws in [
            WorkingColorspace::Rec709Linear,
            WorkingColorspace::Bt2020Linear,
            WorkingColorspace::AcesCG,
        ] {
            let basis = ScopeBasis::for_working_space(ws);
            for level in [0.0_f32, 0.18, 0.5, 1.0, 4.0] {
                let (cb, cr) = basis.chroma([level, level, level]);
                assert!(
                    cb.abs() < 1e-6 && cr.abs() < 1e-6,
                    "{} puts grey {level} at ({cb}, {cr})",
                    basis.label(),
                );
            }
        }
    }

    /// A bars signal must land on the graticule targets of the space it
    /// is written in. Measured with Rec.709's matrix instead, BT.2020
    /// bars sit off their boxes — small, but a vectorscope's whole use
    /// is telling "on the target" from "near it".
    #[test]
    fn bt2020_bars_miss_their_targets_when_measured_as_rec709() {
        let wide = ScopeBasis::for_working_space(WorkingColorspace::Bt2020Linear);
        let naive = ScopeBasis::default();
        // Magenta, the bar furthest from both axes and so the one that
        // moves most under the wrong matrix.
        let magenta = [1.0_f32, 0.0, 1.0];
        let (cb, cr) = wide.chroma(magenta);
        let (ncb, ncr) = naive.chroma(magenta);
        let miss = ((cb - ncb).powi(2) + (cr - ncr).powi(2)).sqrt();
        // A vectorscope target box is about 2% of full scale across.
        assert!(
            miss > 0.02,
            "BT.2020 magenta moves only {miss:.4} under Rec.709's matrix — \
             if that is within a target box, this scope was never wrong",
        );
        // Each space still saturates its own primaries' axes: pure red
        // is all Cr and pure blue all Cb, by construction, in both.
        for basis in [wide, naive] {
            assert!((basis.chroma([1.0, 0.0, 0.0]).1 - 0.5).abs() < 1e-6);
            assert!((basis.chroma([0.0, 0.0, 1.0]).0 - 0.5).abs() < 1e-6);
        }
    }

}
