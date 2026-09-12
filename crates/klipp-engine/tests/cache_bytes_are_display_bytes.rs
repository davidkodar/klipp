//! Why a render-cache hit composes a table instead of just showing the
//! cached bytes.
//!
//! The cache stores each frame sRGB-encoded, so that changing the display
//! transform does not invalidate it. In the plain SDR case — no tone map,
//! Rec.709 working space — the display transform *is* an sRGB encode, which
//! invites the shortcut of publishing the cached bytes unchanged.
//!
//! They are not the same bytes. The cache's encoder quantises the linear
//! value to 12 bits and looks the result up in a 4096-entry table, while
//! the display transform calls `linear_to_srgb` on the value itself, and
//! near black — where sRGB's linear segment packs codes most densely —
//! that extra quantisation step costs a code value. So the viewer composes
//! the transform into a 256-entry table over the cached frame's own byte
//! values instead, which is exact (see
//! `klipp_color::display::display_transform_u8_lut` and
//! `engine::cached_frame_display_tests`).
//!
//! If this test ever fails, the two encoders have been made to agree, and
//! the hit path can be simplified to publish the cached bytes directly.
//! Read it as a note, not a defect.

use klipp_core::{ToneMapOp, WorkingColorspace};

/// The cache's encoder, as `engine.rs` has it — 12-bit quantise into a
/// 4096-entry table of encoded sRGB bytes.
fn cache_encode(linear: &[f32]) -> Vec<u8> {
    let lut: Vec<u8> = (0..4096)
        .map(|i| {
            let v = i as f32 / 4095.0;
            (klipp_color::transfer::linear_to_srgb(v).clamp(0.0, 1.0) * 255.0 + 0.5) as u8
        })
        .collect();
    linear
        .chunks_exact(4)
        .flat_map(|c| {
            let q = |v: f32| -> usize { (v.clamp(0.0, 1.0) * 4095.0 + 0.5) as usize };
            [lut[q(c[0])], lut[q(c[1])], lut[q(c[2])], (c[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8]
        })
        .collect()
}

/// Linear values worth checking: every 12-bit step (the cache table's own
/// grid), the midpoints between them, and values outside `[0, 1]`, which a
/// linear pipeline produces freely.
fn probe_values() -> Vec<f32> {
    let mut v = Vec::new();
    for i in 0..4096 {
        v.push(i as f32 / 4095.0);
        v.push((i as f32 + 0.5) / 4095.0);
    }
    v.extend([-1.0, -0.001, 0.0, 1.0, 1.0001, 2.0, 8.0, f32::MAX]);
    v
}

#[test]
fn the_cached_bytes_are_not_the_screens_bytes() {
    let values = probe_values();
    let linear: Vec<f32> = values.iter().flat_map(|&v| [v, v, v, 1.0]).collect();

    let cached = cache_encode(&linear);
    let shown = klipp_color::display::display_transform_ws(
        &linear,
        ToneMapOp::None,
        WorkingColorspace::Rec709Linear,
    );
    assert_eq!(cached.len(), shown.len());

    let mut differing = 0usize;
    let mut worst = 0u8;
    let mut worst_at = 0.0f32;
    for (i, &v) in values.iter().enumerate() {
        let (c, s) = (cached[i * 4], shown[i * 4]);
        if c != s {
            differing += 1;
            if c.abs_diff(s) > worst {
                worst = c.abs_diff(s);
                worst_at = v;
            }
        }
    }
    eprintln!(
        "{differing} of {} probes differ; worst {worst} code value(s), at linear {worst_at}",
        values.len(),
    );
    assert!(
        differing > 0,
        "the two encoders now agree — the hit path no longer needs its table, \
         and could publish the cached bytes unchanged",
    );
    // A quantisation artefact, not a transform difference: if this ever grew
    // past a code value, one of the two encoders would be wrong rather than
    // merely coarser.
    assert_eq!(worst, 1, "the encoders differ by more than quantisation noise");
}
