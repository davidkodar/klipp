//! PNG image-diff snapshot harness.
//!
//! Pattern: a test produces an RGBA image; this module either
//! - **records** it as a golden file on first run (or whenever the
//!   `KLIPP_UPDATE_GOLDENS` env var is set to `1`),
//! - or **diffs** it against the existing golden with an epsilon
//!   tolerance and panics with a readable summary on mismatch.
//!
//! Failure mode produces a `.actual.png` and `.diff.png` next to the
//! golden so a human can eyeball the discrepancy without rerunning
//! the test.
//!
//! ```ignore
//! use klipp_testing::snapshot;
//! let rgba = render_my_thing(); // Vec<u8>, RGBA8 packed
//! snapshot::assert_matches_golden_png(
//!     &rgba, 256, 256,
//!     // Path is relative to the calling crate's manifest dir; we
//!     // expand to <crate>/goldens/<name>.png.
//!     "mask_circle_no_feather",
//!     snapshot::Tolerance::strict(),
//! );
//! ```
//!
//! To accept a new golden (e.g. you intentionally changed the
//! algorithm and the diff is correct), re-run with:
//! `KLIPP_UPDATE_GOLDENS=1 cargo test --test mask_snapshots`.
//! The harness rewrites the golden in-place.

use std::path::{Path, PathBuf};

/// Pixel-comparison tolerance for [`assert_matches_golden_png`].
#[derive(Debug, Clone, Copy)]
pub struct Tolerance {
    /// Maximum allowed absolute per-channel difference, in 0–255 units.
    /// `0` requires byte-identical output. `2` is roughly the floor
    /// for f32 → u8 round-trip noise across hardware.
    pub per_channel_max: u8,
    /// Maximum fraction of pixels allowed to exceed `per_channel_max`.
    /// `0.0` is "every pixel must match within the channel tolerance."
    /// Use a small fraction (e.g. `0.001`) when comparing across CPU
    /// vs GPU implementations of the same algorithm — there's
    /// usually a tiny halo of edge pixels that drift by more than
    /// rounding noise alone.
    pub max_bad_pixel_fraction: f32,
}

impl Tolerance {
    /// Byte-identical match.
    pub fn strict() -> Self {
        Self { per_channel_max: 0, max_bad_pixel_fraction: 0.0 }
    }
    /// Roundtrip-noise tolerance: ≤2/255 per channel, every pixel.
    pub fn roundtrip() -> Self {
        Self { per_channel_max: 2, max_bad_pixel_fraction: 0.0 }
    }
    /// Cross-implementation tolerance: ≤2/255 per channel, with up
    /// to a 0.1% bad-pixel halo around hard edges. Use for CPU-vs-GPU
    /// parity tests where edge antialiasing differs by sub-pixel.
    pub fn cross_impl() -> Self {
        Self { per_channel_max: 2, max_bad_pixel_fraction: 0.001 }
    }
}

/// Compare an in-memory RGBA8 image to a golden PNG on disk.
///
/// `name` is a logical key — converted to `<crate manifest dir>/
/// goldens/<name>.png`. The directory is created if missing.
///
/// On mismatch, three files are written next to the golden so a
/// human can inspect:
/// - `<name>.actual.png` — what the test produced
/// - `<name>.diff.png`   — per-pixel max absolute diff, scaled into
///   the visible 0–255 range so small drift is still visible
/// - `<name>.png` is left untouched (the source of truth)
///
/// Set `KLIPP_UPDATE_GOLDENS=1` to overwrite the golden with the
/// current actual output. Use sparingly — golden regeneration is
/// the moment a regression turns into "expected behaviour."
#[track_caller]
pub fn assert_matches_golden_png(
    rgba: &[u8],
    width: u32,
    height: u32,
    name: &str,
    tolerance: Tolerance,
) {
    assert_matches_golden_png_in(rgba, width, height, &goldens_dir(), name, tolerance);
}

/// Same as [`assert_matches_golden_png`] but with the goldens
/// directory passed explicitly. Useful when the caller manages its
/// own fixture root (e.g. integration tests that group goldens by
/// scenario, or self-tests of the harness that need a tempdir).
#[track_caller]
pub fn assert_matches_golden_png_in(
    rgba: &[u8],
    width: u32,
    height: u32,
    goldens_root: &Path,
    name: &str,
    tolerance: Tolerance,
) {
    assert_eq!(
        rgba.len() as u32,
        width * height * 4,
        "snapshot::assert_matches_golden_png: rgba length {} doesn't match {}x{}x4",
        rgba.len(), width, height,
    );
    let golden_path = goldens_root.join(format!("{name}.png"));
    let update = std::env::var("KLIPP_UPDATE_GOLDENS").as_deref() == Ok("1");

    if update || !golden_path.exists() {
        write_png(&golden_path, rgba, width, height)
            .unwrap_or_else(|e| panic!("snapshot: failed to write golden {golden_path:?}: {e}"));
        if update {
            eprintln!("snapshot: updated golden {golden_path:?}");
        } else {
            eprintln!(
                "snapshot: NEW golden {golden_path:?} (commit if expected; \
                 otherwise the test will pass any output until you delete it)",
            );
        }
        return;
    }

    let (golden, gw, gh) = read_png(&golden_path)
        .unwrap_or_else(|e| panic!("snapshot: failed to read golden {golden_path:?}: {e}"));
    if gw != width || gh != height {
        let actual_path = golden_path.with_extension("actual.png");
        let _ = write_png(&actual_path, rgba, width, height);
        panic!(
            "snapshot: dimension mismatch for {name}: golden {gw}x{gh}, \
             actual {width}x{height} (wrote {actual_path:?})",
        );
    }

    // Pass 1: count bad pixels. We want one full sweep so the diff
    // image we write afterwards reflects every channel that exceeded
    // tolerance — a single early-exit would only show the first.
    let mut bad_pixels: usize = 0;
    let mut worst_diff: u8 = 0;
    let mut worst_at: (u32, u32, usize) = (0, 0, 0);
    for (pi, (g, a)) in golden.chunks_exact(4).zip(rgba.chunks_exact(4)).enumerate() {
        let mut pixel_bad = false;
        for (ch, (gv, av)) in g.iter().zip(a.iter()).enumerate() {
            let d = gv.abs_diff(*av);
            if d > worst_diff {
                worst_diff = d;
                let py = (pi as u32) / width;
                let px = (pi as u32) % width;
                worst_at = (px, py, ch);
            }
            if d > tolerance.per_channel_max {
                pixel_bad = true;
            }
        }
        if pixel_bad {
            bad_pixels += 1;
        }
    }

    let total_pixels = (width * height) as usize;
    let bad_fraction = bad_pixels as f32 / total_pixels.max(1) as f32;
    if bad_fraction > tolerance.max_bad_pixel_fraction {
        let actual_path = golden_path.with_extension("actual.png");
        let diff_path = golden_path.with_extension("diff.png");
        let _ = write_png(&actual_path, rgba, width, height);
        let _ = write_png(&diff_path, &diff_image(&golden, rgba), width, height);
        let (px, py, ch) = worst_at;
        let g_ix = ((py * width + px) * 4) as usize;
        panic!(
            "snapshot: {name} mismatch — {bad_pixels}/{total_pixels} pixels \
             ({:.3}%) exceed per-channel tolerance ±{} (limit {:.3}%); \
             worst diff {} at pixel ({px},{py}) channel {ch} \
             (golden={:?} actual={:?}). Wrote {actual_path:?} + {diff_path:?}",
            bad_fraction * 100.0,
            tolerance.per_channel_max,
            tolerance.max_bad_pixel_fraction * 100.0,
            worst_diff,
            &golden[g_ix..g_ix + 4],
            &rgba[g_ix..g_ix + 4],
        );
    }
}

/// Build a diff image: per-pixel max-absolute-channel-difference,
/// scaled by 8× so even a 1-LSB drift is visible. Output is greyscale
/// (R=G=B=diff, A=255) so the inspector can flip between actual /
/// golden / diff and immediately spot the affected region.
fn diff_image(golden: &[u8], actual: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(golden.len());
    for (g, a) in golden.chunks_exact(4).zip(actual.chunks_exact(4)) {
        let mut max_d = 0u8;
        for (gv, av) in g.iter().zip(a.iter()) {
            max_d = max_d.max(gv.abs_diff(*av));
        }
        let scaled = (max_d as u32 * 8).min(255) as u8;
        out.extend_from_slice(&[scaled, scaled, scaled, 255]);
    }
    out
}

fn goldens_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR resolves to the *calling* crate's manifest
    // when `assert_matches_golden_png` is monomorphised into a test
    // binary. So `goldens/` always lands inside the crate that owns
    // the test, keeping fixtures near the code they verify.
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(manifest).join("goldens")
}

fn write_png(path: &Path, rgba: &[u8], width: u32, height: u32) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(path)?;
    let w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(io_err)?;
    writer.write_image_data(rgba).map_err(io_err)?;
    Ok(())
}

fn read_png(path: &Path) -> std::io::Result<(Vec<u8>, u32, u32)> {
    let file = std::fs::File::open(path)?;
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().map_err(io_err)?;
    let info = reader.info();
    let (w, h) = (info.width, info.height);
    // Insist on RGBA8 input. The harness writes goldens itself so
    // this is guaranteed for our own files; if a user hand-wrote an
    // RGB or 16-bit PNG we'd rather fail loudly than silently
    // up-/down-convert and have the diff include a colour-space
    // shift.
    if info.color_type != png::ColorType::Rgba {
        return Err(std::io::Error::other(format!(
            "golden PNG must be RGBA, got {:?}", info.color_type,
        )));
    }
    if info.bit_depth != png::BitDepth::Eight {
        return Err(std::io::Error::other(format!(
            "golden PNG must be 8-bit, got {:?}", info.bit_depth,
        )));
    }
    // png 0.18: output_buffer_size returns Option<usize> (None on
    // dimension overflow). Goldens are tiny test fixtures, but
    // surface the error rather than panic-via-unwrap.
    let buf_size = reader.output_buffer_size()
        .ok_or_else(|| std::io::Error::other("PNG dimensions overflow buffer size"))?;
    let mut buf = vec![0u8; buf_size];
    reader.next_frame(&mut buf).map_err(io_err)?;
    Ok((buf, w, h))
}

fn io_err<E: std::fmt::Display>(e: E) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

/// Convenience: convert a linear-f32 RGBA buffer to sRGB 8-bit RGBA
/// for snapshotting. Most engine tests work in linear-f32; goldens
/// are PNG (perceptual), so we encode here rather than expecting the
/// caller to remember the conversion at every callsite.
pub fn linear_f32_to_srgb_u8(linear: &[f32]) -> Vec<u8> {
    crate::fixtures::encode_linear_to_srgb_u8(linear)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Each test runs in a unique tempdir so concurrent runs don't
    /// trample each other's goldens. The explicit-dir variant of
    /// the assertion helper means we don't have to fight global
    /// env-var state — pass the path through directly.
    static UNIQ: AtomicU64 = AtomicU64::new(0);

    fn unique_goldens_root() -> PathBuf {
        let n = UNIQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("klipp_testing_snapshot_{}_{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tempdir create");
        dir
    }

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut v = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            v.extend_from_slice(&rgba);
        }
        v
    }

    #[test]
    fn first_run_writes_golden_and_passes() {
        let dir = unique_goldens_root();
        let img = solid(8, 8, [128, 64, 32, 255]);
        // Should not panic — first run = record.
        assert_matches_golden_png_in(&img, 8, 8, &dir, "first_run", Tolerance::strict());
        assert!(dir.join("first_run.png").exists());
    }

    #[test]
    fn matching_image_passes() {
        let dir = unique_goldens_root();
        let img = solid(16, 8, [200, 100, 50, 255]);
        assert_matches_golden_png_in(&img, 16, 8, &dir, "match", Tolerance::strict());
        // Second call against the same golden — must pass cleanly.
        assert_matches_golden_png_in(&img, 16, 8, &dir, "match", Tolerance::strict());
    }

    #[test]
    #[should_panic(expected = "exceed per-channel tolerance")]
    fn mismatched_image_panics_and_dumps_artefacts() {
        let dir = unique_goldens_root();
        let original = solid(4, 4, [100, 100, 100, 255]);
        assert_matches_golden_png_in(&original, 4, 4, &dir, "mismatch", Tolerance::strict());
        let perturbed = solid(4, 4, [120, 100, 100, 255]); // R off by 20
        assert_matches_golden_png_in(&perturbed, 4, 4, &dir, "mismatch", Tolerance::strict());
    }

    #[test]
    fn within_per_channel_tolerance_passes() {
        let dir = unique_goldens_root();
        let original = solid(4, 4, [100, 100, 100, 255]);
        assert_matches_golden_png_in(&original, 4, 4, &dir, "tol", Tolerance::strict());
        let close = solid(4, 4, [101, 100, 99, 255]);
        // ±2 tolerance — passes.
        assert_matches_golden_png_in(&close, 4, 4, &dir, "tol", Tolerance::roundtrip());
    }

    #[test]
    fn dimension_mismatch_dumps_actual_and_panics() {
        let dir = unique_goldens_root();
        let small = solid(4, 4, [50, 50, 50, 255]);
        assert_matches_golden_png_in(&small, 4, 4, &dir, "dim", Tolerance::strict());
        let big = solid(8, 8, [50, 50, 50, 255]);
        let result = std::panic::catch_unwind(|| {
            assert_matches_golden_png_in(&big, 8, 8, &dir, "dim", Tolerance::strict());
        });
        assert!(result.is_err(), "expected dimension mismatch to panic");
        assert!(
            dir.join("dim.actual.png").exists(),
            "expected actual.png artefact on dimension mismatch",
        );
    }

    #[test]
    fn cross_impl_tolerance_allows_small_bad_pixel_halo() {
        let dir = unique_goldens_root();
        // 100x100 image, 9 pixels (0.09%) drift by 5/255. Strict
        // would fail; cross_impl (0.1% bad fraction) passes.
        let mut original = solid(100, 100, [128, 128, 128, 255]);
        assert_matches_golden_png_in(&original, 100, 100, &dir, "halo", Tolerance::strict());
        for &i in &[0, 4, 8, 12, 16, 20, 24, 28, 32] {
            original[i] = 133; // R channel drift of 5
        }
        assert_matches_golden_png_in(&original, 100, 100, &dir, "halo", Tolerance::cross_impl());
    }
}
