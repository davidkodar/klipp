//! WGSL prelude + helper functions for shaders that branch on a
//! source's colourspace IDs.
//!
//! The numeric IDs match `klipp_core::color_ids` exactly; we don't
//! depend on klipp-core here (`klipp-core` already depends on this
//! crate via `BuiltinEffect`, so a back-edge would cycle). Whenever
//! `klipp_core::color_ids` constants are renumbered, update both
//! sides — there's a unit test in `klipp_core::color_ids::tests` that
//! pins the values, and a smoke test in this crate that pins the
//! same values from the other side. A divergence will fail one or
//! the other immediately.
//!
//! Concatenate [`WGSL_PRELUDE`] then [`WGSL_HELPERS`] at the top of
//! any shader source string that needs colourspace-aware YUV→RGB
//! decoding.

/// Numeric ID constants exposed to WGSL via `const` declarations.
/// These MUST match `klipp_core::color_ids`'s constants. The
/// `pinned_ids_match_klipp_core` test in this crate guards that.
pub const WGSL_PRELUDE: &str = include_str!(concat!(env!("OUT_DIR"), "/shaders/colour/yuv_prelude_body.wgsl"));

/// Pack a row-major 3x3 matrix into the WGSL-friendly layout used by
/// the decoder shaders' `Params` uniform: three rows, each padded to
/// `vec4<f32>` so consecutive rows land on 16-byte boundaries (WGSL's
/// alignment rule for `vec3<f32>` in uniform memory). The 4th element
/// of each row is unused padding; callers don't read it.
///
/// The shader dots the input RGB against each row to produce the
/// output channel — i.e. `out_r = dot(row0, rgb)` etc. That keeps the
/// WGSL identical regardless of which working space the matrix
/// represents (BT.709 / ACEScg / future BT.2020 etc.).
#[inline]
pub fn pack_primaries_matrix(m: [[f32; 3]; 3]) -> [[f32; 4]; 3] {
    [
        [m[0][0], m[0][1], m[0][2], 0.0],
        [m[1][0], m[1][1], m[1][2], 0.0],
        [m[2][0], m[2][1], m[2][2], 0.0],
    ]
}

/// 3x3 identity in the same packed layout. Useful as a default when a
/// caller doesn't have the source colourspace yet (e.g. the tests
/// that exercise BT.709 SDR sources where source-primaries already
/// match the working-space basis).
pub const IDENTITY_PRIMARIES_MATRIX_PACKED: [[f32; 4]; 3] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
];

/// Helper functions for the four conversion stages:
///
/// 1. `yuv_normalise(y, u, v, range, bit_depth_max)` — raw codepoint
///    `f32` → normalised `(Y in [0,1], U/V in ~[-0.5,+0.5])`.
/// 2. `yuv_to_rgb_display(yuv, matrix_id)` → display-encoded RGB
///    (gamma-encoded for SDR; PQ/HLG-encoded for HDR transfers).
/// 3. `display_to_linear(rgb, transfer_id)` → linear-light in source
///    primaries.
/// 4. `primaries_to_bt709(rgb, primaries_id)` → linear in the engine's
///    BT.709 working space.
///
/// Plus `linear_to_srgb_vec` for re-encoding to the existing sRGB-u8
/// hand-off shape consumed by `rgba_u8_to_linear` downstream.
pub const WGSL_HELPERS: &str = include_str!(concat!(env!("OUT_DIR"), "/shaders/colour/yuv_helpers_body.wgsl"));

#[cfg(test)]
mod tests {
    /// Pin the numeric IDs so a future klipp-core renumbering breaks
    /// the build instead of producing silently wrong shader output.
    /// If this test fails, update both `klipp_core::color_ids` AND
    /// the `WGSL_PRELUDE` constant in this file to match.
    #[test]
    fn pinned_ids_match_klipp_core() {
        // We can't import klipp-core here (cycle), so just pin the
        // numeric values literally. The companion test on the
        // klipp-core side
        // (`color_ids::tests::ids_for_default_bt709_limited_sdr` and
        // friends) pins them from the other direction.
        let prelude = super::WGSL_PRELUDE;
        assert!(prelude.contains("COLOR_RANGE_LIMITED: u32 = 0u"));
        assert!(prelude.contains("COLOR_RANGE_FULL: u32 = 1u"));
        assert!(prelude.contains("MATRIX_BT709: u32 = 0u"));
        assert!(prelude.contains("MATRIX_BT601: u32 = 1u"));
        assert!(prelude.contains("MATRIX_BT2020_NCL: u32 = 2u"));
        assert!(prelude.contains("MATRIX_SMPTE240: u32 = 3u"));
        assert!(prelude.contains("MATRIX_IDENTITY: u32 = 4u"));
        assert!(prelude.contains("TRANSFER_BT709: u32 = 0u"));
        assert!(prelude.contains("TRANSFER_SRGB: u32 = 1u"));
        assert!(prelude.contains("TRANSFER_LINEAR: u32 = 2u"));
        assert!(prelude.contains("TRANSFER_PQ: u32 = 3u"));
        assert!(prelude.contains("TRANSFER_HLG: u32 = 4u"));
        assert!(prelude.contains("TRANSFER_GAMMA22: u32 = 5u"));
        assert!(prelude.contains("TRANSFER_GAMMA28: u32 = 6u"));
        assert!(prelude.contains("PRIMARIES_BT709: u32 = 0u"));
        assert!(prelude.contains("PRIMARIES_BT2020: u32 = 1u"));
        assert!(prelude.contains("PRIMARIES_BT601_525: u32 = 2u"));
        assert!(prelude.contains("PRIMARIES_BT601_625: u32 = 3u"));
        assert!(prelude.contains("PRIMARIES_DCI_P3: u32 = 4u"));
        assert!(prelude.contains("PRIMARIES_DISPLAY_P3: u32 = 5u"));
    }

    #[test]
    fn helpers_define_required_functions() {
        let h = super::WGSL_HELPERS;
        for name in [
            "fn yuv_normalise(",
            "fn yuv_to_rgb_display(",
            "fn display_to_linear(",
            "fn linear_to_srgb_vec(",
            "fn srgb_eotf_scalar(",
            "fn pq_eotf_scalar(",
            "fn hlg_eotf_scalar(",
        ] {
            assert!(h.contains(name), "missing WGSL helper: {name}");
        }
    }

    /// Gamut conversion now lives in the decoder shaders' `Params`
    /// uniform, not in `WGSL_HELPERS`. The old switch-on-primaries-id
    /// function is intentionally absent; this test pins that decision
    /// so a future "let's bring it back" tweak fails loudly and gets
    /// a justification in the diff.
    #[test]
    fn helpers_does_not_define_legacy_primaries_to_bt709() {
        assert!(
            !super::WGSL_HELPERS.contains("fn primaries_to_bt709("),
            "primaries_to_bt709 helper was deliberately removed when \
             gamut conversion moved to a uniform matrix; reintroducing \
             it without removing the matrix path would be two sources \
             of truth",
        );
    }

    /// Identity-matrix smoke test for the matrix-packing helper. The
    /// shader treats each row as a `vec3<f32>` (with the `.w` slot
    /// unused padding); the helper must place the identity diagonal
    /// at `[i][i]` and zero everything else.
    #[test]
    fn pack_primaries_matrix_identity() {
        let id = super::pack_primaries_matrix([
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ]);
        assert_eq!(id, super::IDENTITY_PRIMARIES_MATRIX_PACKED);
    }

    #[test]
    fn pack_primaries_matrix_preserves_first_three_columns() {
        // Arbitrary matrix; each input row shows up in the first three
        // slots of the corresponding output row, with 0.0 padding.
        let m = [
            [0.1, 0.2, 0.3],
            [0.4, 0.5, 0.6],
            [0.7, 0.8, 0.9],
        ];
        let p = super::pack_primaries_matrix(m);
        for i in 0..3 {
            for j in 0..3 {
                assert_eq!(p[i][j], m[i][j], "row {i} col {j}");
            }
            assert_eq!(p[i][3], 0.0, "row {i} pad slot");
        }
    }
}
