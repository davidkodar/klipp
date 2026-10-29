//! The planar tracker's shaders (#2754): each a mirror of its CPU
//! reference in `crate::planar`, so the two agree within float rounding
//! and the histogram's resolution (the robust scale is a histogram
//! median here, an exact one there).

/// Partial sums each reduction's workgroups write, and how many of them.
pub(super) const GROUPS: u32 = 64;
/// Threads in a reduction's workgroup.
pub(super) const THREADS: u32 = 64;
/// Bins of the residual and strength histograms.
pub(super) const BINS: u32 = 1024;
/// Floats a template point holds: u, the two values, the two channels'
/// eight steepest-descent terms, the two strengths.
pub(super) const POINT: u32 = 22;
/// Floats the alignment's state holds: the map (9), the light (8), the
/// cut, the conditioning, a failure flag, whether the level has
/// converged, whether the map is held to a similarity, and spare.
pub(super) const STATE: u32 = 24;
/// Partial sums of the light fit, per workgroup: the brightness's 4×4
/// (10) and right side (4), the edges' 2×2 (3) and right side (2), the
/// weight.
pub(super) const LIGHT: u32 = 20;
/// Partial sums of the normal equations, per workgroup: the 8×8's upper
/// half (36) and the right side (8).
pub(super) const NORMAL: u32 = 44;

/// The constants the shaders name, as they are written into WGSL.
const NAMED: [(&str, u32); 7] = [("GROUPS", GROUPS), ("THREADS", THREADS), ("BINS", BINS), ("POINT", POINT), ("STATE", STATE), ("LIGHT", LIGHT), ("NORMAL", NORMAL)];

/// [`NAMED`] as WGSL, before every pass (#3094): the shader files use the
/// names, so each value is written once, here, and the CPU tracker's
/// buffers and the shaders' can't disagree.
fn prelude() -> String {
    NAMED.iter().map(|(name, value)| format!("const {name}: u32 = {value}u;\n")).collect()
}

// `include/planar_params.wgsl`: the parameters every pass takes, as a uniform.

// `include/planar_sample.wgsl`: between pixels, bilinearly; `ok` false outside the level.

/// Each pass's source, by label.
pub(super) fn sources() -> Vec<(&'static str, String)> {
    vec![
        // The frame's brightness, from bytes four to a word.
        ("planar_luma", prelude() + gpu_shader!("planar/luma_body.wgsl")),
        // Blurred down the rows and every other row taken: `halve`'s first pass.
        ("planar_halve_rows", prelude() + gpu_shader!("planar/halve_rows_body.wgsl")),
        // Then across the columns and every other column taken.
        ("planar_halve_cols", prelude() + gpu_shader!("planar/halve_cols_body.wgsl")),
        // The strength of the edges, central differences: `edges`.
        ("planar_edges", prelude() + gpu_shader!("planar/edges_body.wgsl")),
        // A template's points: `Template::new`, from the cells the CPU
        // chose, and a histogram of each channel's strength.
        ("planar_template", prelude() + gpu_shader!("planar/template_body.wgsl")),
        // Each channel's median strength from its histogram: the
        // template's `strong` threshold.
        ("planar_strong", prelude() + gpu_shader!("planar/strong_body.wgsl")),
        // Each point sampled through the map: `align_level`'s first loop.
        ("planar_sample", prelude() + gpu_shader!("planar/sample_body.wgsl")),
        // The light fit's sums, per workgroup, weighted by the last
        // step's trust.
        ("planar_light_partial", prelude() + gpu_shader!("planar/light_partial_body.wgsl")),
        // The light solved from the sums: brightness by gain, offset and
        // slope; edges by gain and offset.
        ("planar_light_solve", prelude() + gpu_shader!("planar/light_solve_body.wgsl")),
        // The residuals under the light, and a histogram of the strong
        // points' sizes.
        ("planar_residual", prelude() + gpu_shader!("planar/residual_body.wgsl")),
        // The median residual from the histogram: the Tukey cut. Too few
        // residuals to read a spread from fails the alignment.
        ("planar_cut", prelude() + gpu_shader!("planar/cut_body.wgsl")),
        // The weighted normal equations' sums, per workgroup, and each
        // point's trust for the next step's light.
        ("planar_normal_partial", prelude() + gpu_shader!("planar/normal_partial_body.wgsl")),
        // The step solved and the map updated by its inverse; the
        // Hessian's conditioning kept.
        ("planar_step", prelude() + gpu_shader!("planar/step_body.wgsl")),
        // The last step's Hessian's conditioning, once a level is done:
        // `align_level`'s, which only its last step's matters for.
        ("planar_condition", prelude() + gpu_shader!("planar/condition_body.wgsl")),
        // How alike a template's brightness and the frame's are under the
        // map: per workgroup, the sums ZNCC reads.
        ("planar_zncc", prelude() + gpu_shader!("planar/zncc_body.wgsl")),
        // The coarse search: for each shift of the map (one workgroup
        // each), the summed difference of the brightness over every
        // `step`th point, and how many were read.
        ("planar_search", prelude() + gpu_shader!("planar/search_body.wgsl")),
    ]
}

// `include/planar_solve.wgsl`: Gaussian elimination with partial pivoting on the top-left `n` × `n` of an 8×8: `homography::solve`.

// `include/planar_eigen.wgsl`: the smallest over the largest eigenvalue of the top-left `n` × `n` of a symmetric 8×8, by Jacobi rotations: `planar::conditioning`.

// `include/planar_similarity.wgsl`: the step's normal equations held to a similarity, in the top-left 4×4, and a step in its directions as the eight parameters: `planar::to_similarity`, `planar::from_similarity`.

#[cfg(test)]
mod tests {
    use super::*;

    /// The prelude, read as WGSL, holds each constant at its Rust value,
    /// and every pass compiles with it.
    #[test]
    fn the_prelude_holds_the_rust_constants() {
        let module = naga::front::wgsl::parse_str(&prelude()).expect("the prelude reads");
        let read: std::collections::BTreeMap<String, u32> = module
            .constants
            .iter()
            .filter_map(|(_, c)| match module.global_expressions[c.init] {
                naga::Expression::Literal(naga::Literal::U32(v)) => Some((c.name.clone()?, v)),
                _ => None,
            })
            .collect();
        let want: std::collections::BTreeMap<String, u32> = NAMED.iter().map(|(n, v)| (n.to_string(), *v)).collect();
        assert_eq!(read, want);
        for (label, source) in sources() {
            assert!(source.starts_with(&prelude()), "{label}: the prelude first");
            crate::shader_check::validate(&source).unwrap_or_else(|e| panic!("{label}: {e}"));
        }
    }
}
