//! HDR10 signalling helpers: linear-light → nits convention, light-level
//! accumulators, and reference primaries.
//!
//! # Nits-per-linear-unit convention
//!
//! Klipp's pipeline treats `1.0 linear` as *diffuse white*. The HDR10
//! spec expresses light levels in absolute cd/m² (nits), so we need a
//! scale factor. We pick **100 cd/m²** — the SDR reference white level
//! from BT.2035 — which means:
//!
//! * `1.0 linear` → 100 nits (a sheet of white paper under normal
//!   office lighting)
//! * `10.0 linear` → 1000 nits (a typical HDR10 highlight cap)
//! * `100.0 linear` → 10 000 nits (PQ peak)
//!
//! Every HDR tool has to pick a convention here — there is no single
//! right answer — and this one matches how the Hable tone-mapper's
//! `W = 11.2` white point is wired (`display.rs`), so highlights rolled
//! off by Hable correspond to ~1120 nits in metadata land.
//!
//! # Scope
//!
//! This module handles the *statistics* side of HDR10 (MaxCLL / MaxFALL
//! computation + mastering-display defaults). Actual PQ/HLG pixel
//! encoding lives in a later phase — today the encoder still writes
//! sRGB pixels and only *tags* them as HDR via side-data.

/// Scale factor mapping `1.0 linear` to nits (cd/m²).
///
/// See module docs for the rationale. If you change this, audit every
/// tone-map operator and every HDR ground-truth test — the white point
/// / highlight cap relationships break otherwise.
pub const NITS_PER_LINEAR: f32 = 100.0;

/// Running light-level statistics for an HDR10 master.
///
/// Fed one linear-light frame at a time via [`Self::update`]; the final
/// MaxCLL / MaxFALL are read out at export time via
/// [`Self::max_cll_nits`] / [`Self::max_fall_nits`].
///
/// Both values are expressed in **cd/m² (nits)**, rounded to the
/// nearest integer, and clamped to `u32::MAX` at the top — HDR10
/// signals them as unsigned 16-bit (max 65 535 nits), but we return
/// `u32` so a caller that upgrades to AV1's 18-bit `hdr_metadata_obu`
/// field doesn't need a type change.
#[derive(Debug, Clone, Default)]
pub struct LightLevelStats {
    /// Running max of the per-pixel max-channel value across all
    /// frames. Linear units.
    max_cll_linear: f32,
    /// Running max of per-frame average luminance. Linear units.
    max_fall_linear: f32,
    /// Number of frames observed — lets us detect an empty stream.
    frames: u64,
}

impl LightLevelStats {
    /// Fresh accumulator. No frames yet, MaxCLL / MaxFALL are zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of frames fed so far.
    pub fn frame_count(&self) -> u64 {
        self.frames
    }

    /// Update the stats with one linear-light RGBA frame.
    ///
    /// * `linear_rgba.len()` must be `width * height * 4`.
    /// * Alpha is ignored — MaxCLL / MaxFALL are light-level statistics.
    /// * Negative channel values (legal mid-pipeline) clamp to zero
    ///   for the purpose of stats; they don't contribute brightness.
    pub fn update(&mut self, linear_rgba: &[f32]) {
        if linear_rgba.is_empty() {
            return;
        }
        let mut frame_max = 0.0_f32;
        let mut frame_sum = 0.0_f64;
        let mut pixels: u64 = 0;
        for chunk in linear_rgba.chunks_exact(4) {
            let r = chunk[0].max(0.0);
            let g = chunk[1].max(0.0);
            let b = chunk[2].max(0.0);
            frame_max = frame_max.max(r.max(g).max(b));
            // BT.2020 luma coefficients — matches the HDR primaries we
            // tag exports with. Done in linear light, which is the
            // only domain where these weights are correct.
            let y = 0.262_7 * r + 0.678_0 * g + 0.059_3 * b;
            frame_sum += y as f64;
            pixels += 1;
        }
        if pixels == 0 {
            return;
        }
        let frame_avg = (frame_sum / pixels as f64) as f32;
        if frame_max > self.max_cll_linear {
            self.max_cll_linear = frame_max;
        }
        if frame_avg > self.max_fall_linear {
            self.max_fall_linear = frame_avg;
        }
        self.frames += 1;
    }

    /// Max Content Light Level in cd/m², rounded to nearest integer.
    pub fn max_cll_nits(&self) -> u32 {
        to_nits_u32(self.max_cll_linear)
    }

    /// Max Frame-Average Light Level in cd/m², rounded to nearest integer.
    pub fn max_fall_nits(&self) -> u32 {
        to_nits_u32(self.max_fall_linear)
    }
}

fn to_nits_u32(linear: f32) -> u32 {
    if !(linear > 0.0) {
        return 0;
    }
    let nits = (linear * NITS_PER_LINEAR).round();
    if nits >= u32::MAX as f32 {
        u32::MAX
    } else {
        nits as u32
    }
}

/// CIE 1931 xy chromaticity for the BT.2020 / Rec.2100 primaries and
/// D65 white point. Used as the default mastering-display metadata for
/// HDR10 exports.
pub mod bt2020_primaries {
    pub const RED: (f32, f32) = (0.708, 0.292);
    pub const GREEN: (f32, f32) = (0.170, 0.797);
    pub const BLUE: (f32, f32) = (0.131, 0.046);
    pub const WHITE_D65: (f32, f32) = (0.3127, 0.3290);
}

/// Default mastering-display luminance range (cd/m²) baked into HDR10
/// exports when the project hasn't configured anything explicit.
///
/// 1000 nit peak is the most common consumer HDR10 mastering target
/// (the "HDR10 1000" certification level); 0.005 nit floor matches
/// OLED reference displays.
pub const DEFAULT_MASTERING_MAX_LUMA_CD_M2: f32 = 1000.0;
pub const DEFAULT_MASTERING_MIN_LUMA_CD_M2: f32 = 0.005;

// ─── HDR input and display transforms ─────────────────────────────────
//
// How an HDR (PQ / HLG) source travels through Klipp:
//
//   decoder    YUV → RGB *code values*, still PQ/HLG-encoded, in the
//              source primaries. Packed 10 bits per channel.
//   input      code → scene-linear (1.0 = 100 nits, see
//              `NITS_PER_LINEAR`) → working-space primaries.
//              `hdr10_codes_to_scene_linear_rgba` here, and the matching
//              GPU bridge in klipp-effects.
//   grade      runs on that scene-linear image, highlights above 1.0
//              intact.
//   output     `apply_hdr_display_tonemap` brings the graded clip to
//              display-referred linear; composite and the sRGB display
//              encode then proceed exactly as for SDR clips.
//
// The tone map used to run inside the decoder, which meant every grade
// operated on an image whose highlights had already been compressed to
// SDR — and the auto-seeded InputTransform(PQ) then decoded PQ a second
// time. Moving it to the end of the clip is what lets a grade see the
// HDR range at all.

/// HDR transfer characteristics decoded on input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrTransfer {
    /// SMPTE ST 2084.
    Pq,
    /// ARIB STD-B67 / BT.2100 HLG.
    Hlg,
}

impl HdrTransfer {
    /// From a `klipp_core::color_ids` transfer id. `None` for every
    /// non-HDR transfer, so callers can use it as the "is this HDR"
    /// test as well.
    pub fn from_transfer_id(id: u32) -> Option<Self> {
        match id {
            klipp_core::color_ids::TRANSFER_PQ => Some(Self::Pq),
            klipp_core::color_ids::TRANSFER_HLG => Some(Self::Hlg),
            _ => None,
        }
    }
}

/// Exposure applied before the display tone map.
///
/// Chosen so the display output matches what the decoder produced before
/// the tone map moved: it computed `filmic(18 · L)` with `L` in PQ's
/// `1.0 = 10 000 nits` units. In this module's `1.0 = 100 nits` units
/// that is `filmic(0.18 · s)`. Changing it changes how every HDR clip
/// looks, graded or not.
pub const HDR_DISPLAY_EXPOSURE: f32 = 0.18;

/// PQ's absolute peak, in nits.
const PQ_PEAK_NITS: f32 = 10_000.0;

/// A PQ or HLG code value in `[0, 1]` → scene-linear, `1.0 = 100 nits`.
///
/// HLG is relative rather than absolute, so it has no true nits value.
/// It is scaled by the same factor as PQ, which is what the decoder did
/// before this module existed; that keeps existing HLG material looking
/// the same rather than claiming a luminance interpretation it never had.
pub fn hdr_code_to_scene_linear(code: f32, transfer: HdrTransfer) -> f32 {
    let normalised = match transfer {
        HdrTransfer::Pq => crate::camera_log::pq_to_linear(code),
        HdrTransfer::Hlg => crate::camera_log::hlg_to_linear(code),
    };
    normalised * (PQ_PEAK_NITS / NITS_PER_LINEAR)
}

/// Narkowicz's fit of the ACES filmic curve, clamped to `[0, 1]`.
///
/// The same constants as `aces_filmic_scalar` in the P010 shader; the
/// two must stay in step or graded and ungraded HDR frames will
/// disagree.
pub fn filmic(x: f32) -> f32 {
    let (a, b, c, d, e) = (2.51_f32, 0.03_f32, 2.43_f32, 0.59_f32, 0.14_f32);
    ((x * (a * x + b)) / (x * (c * x + d) + e)).clamp(0.0, 1.0)
}

/// Scene-linear HDR channel → display-referred linear, `[0, 1]`.
pub fn hdr_display_tonemap(scene_linear: f32) -> f32 {
    filmic((scene_linear * HDR_DISPLAY_EXPOSURE).max(0.0))
}

/// Tone-map a graded scene-linear RGBA frame to display-referred linear,
/// in place. Alpha is untouched.
///
/// This is the last step of an HDR clip's own processing. After it the
/// clip is display-referred like any SDR clip, so compositing and the
/// sRGB display encode need no HDR awareness at all.
pub fn apply_hdr_display_tonemap(rgba: &mut [f32]) {
    use rayon::prelude::*;
    rgba.par_chunks_exact_mut(4).for_each(|px| {
        px[0] = hdr_display_tonemap(px[0]);
        px[1] = hdr_display_tonemap(px[1]);
        px[2] = hdr_display_tonemap(px[2]);
    });
}

/// Every 10-bit code value → scene-linear. Evaluating the curve 1024
/// times up front turns the per-pixel `powf` chain into a table read,
/// and is exact: the input only ever takes these 1024 values.
pub fn hdr_code_lut(transfer: HdrTransfer) -> Vec<f32> {
    (0..1024u32)
        .map(|i| hdr_code_to_scene_linear(i as f32 / 1023.0, transfer))
        .collect()
}

/// Pack three 10-bit code values into one `u32`: R in bits 0-9, G in
/// 10-19, B in 20-29, bits 30-31 zero. The layout the P010 shader writes.
pub fn pack_rgb10(r: u32, g: u32, b: u32) -> u32 {
    (r & 0x3FF) | ((g & 0x3FF) << 10) | ((b & 0x3FF) << 20)
}

/// Inverse of [`pack_rgb10`].
pub fn unpack_rgb10(p: u32) -> [u32; 3] {
    [p & 0x3FF, (p >> 10) & 0x3FF, (p >> 20) & 0x3FF]
}

/// Packed 10-bit HDR code values (source primaries) → scene-linear RGBA
/// f32 in the working space. The CPU half of the input transform.
///
/// `source_to_working` is the same matrix the decoder uploads to the
/// shader (`camera_log::primaries_to_working_matrix`). It is applied
/// *after* the transfer curve because gamut conversion is only valid on
/// linear light.
pub fn hdr10_codes_to_scene_linear_rgba(
    packed: &[u32],
    transfer: HdrTransfer,
    source_to_working: [[f32; 3]; 3],
) -> Vec<f32> {
    use rayon::prelude::*;
    let lut = hdr_code_lut(transfer);
    let m = source_to_working;
    let mut out = vec![0.0_f32; packed.len() * 4];
    out.par_chunks_exact_mut(4)
        .zip(packed.par_iter())
        .for_each(|(dst, &p)| {
            let [r, g, b] = unpack_rgb10(p);
            let (r, g, b) = (lut[r as usize], lut[g as usize], lut[b as usize]);
            dst[0] = m[0][0] * r + m[0][1] * g + m[0][2] * b;
            dst[1] = m[1][0] * r + m[1][1] * g + m[1][2] * b;
            dst[2] = m[2][0] * r + m[2][1] * g + m[2][2] * b;
            dst[3] = 1.0;
        });
    out
}

#[cfg(test)]
mod hdr_io_tests {
    use super::*;

    const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    /// The convention the whole change rests on: PQ 100 nits must land
    /// on scene-linear 1.0, and 1000 nits on 10.0.
    #[test]
    fn pq_reference_levels_land_on_the_documented_convention() {
        for nits in [100.0_f32, 1000.0, 203.0] {
            let code = crate::camera_log::linear_to_pq(nits / PQ_PEAK_NITS);
            let s = hdr_code_to_scene_linear(code, HdrTransfer::Pq);
            let expected = nits / NITS_PER_LINEAR;
            assert!(
                (s - expected).abs() / expected < 1e-3,
                "{nits} nits -> {s}, expected {expected}",
            );
        }
    }

    /// Moving the tone map out of the decoder must not change how an
    /// ungraded HDR clip looks: `filmic(0.18 · s)` has to equal the
    /// decoder's old `filmic(18 · L)` for every code value.
    #[test]
    fn display_tonemap_reproduces_the_decoders_old_curve() {
        for i in 0..1024u32 {
            let code = i as f32 / 1023.0;
            let old = filmic((crate::camera_log::pq_to_linear(code) * 18.0).max(0.0));
            let new = hdr_display_tonemap(hdr_code_to_scene_linear(code, HdrTransfer::Pq));
            assert!((old - new).abs() < 1e-5, "code {i}: old {old} new {new}");
        }
    }

    #[test]
    fn lut_matches_the_analytic_curve_at_every_code() {
        for t in [HdrTransfer::Pq, HdrTransfer::Hlg] {
            let lut = hdr_code_lut(t);
            assert_eq!(lut.len(), 1024);
            for (i, v) in lut.iter().enumerate() {
                assert_eq!(*v, hdr_code_to_scene_linear(i as f32 / 1023.0, t));
            }
        }
    }

    #[test]
    fn rgb10_round_trips_and_masks() {
        for (r, g, b) in [(0, 0, 0), (1023, 1023, 1023), (1, 512, 1022), (700, 3, 999)] {
            assert_eq!(unpack_rgb10(pack_rgb10(r, g, b)), [r, g, b]);
        }
        assert_eq!(pack_rgb10(1023, 1023, 1023) >> 30, 0, "top bits stay clear");
        assert_eq!(unpack_rgb10(pack_rgb10(1024 + 5, 0, 0))[0], 5, "overflow is masked");
    }

    /// Scene-linear really does exceed 1.0 — the whole point of grading
    /// before the tone map — and the tone map then brings it back to
    /// [0, 1], monotonically.
    #[test]
    fn highlights_survive_until_the_tonemap_and_it_is_monotonic() {
        let peak = hdr10_codes_to_scene_linear_rgba(&[pack_rgb10(1023, 1023, 1023)], HdrTransfer::Pq, IDENTITY);
        assert!(peak[0] > 50.0, "PQ peak should be ~100 scene-linear, got {}", peak[0]);
        let mut prev = -1.0_f32;
        for i in 0..1024u32 {
            let v = hdr_display_tonemap(hdr_code_to_scene_linear(i as f32 / 1023.0, HdrTransfer::Pq));
            assert!((0.0..=1.0).contains(&v));
            assert!(v >= prev, "tone map must be monotonic (code {i})");
            prev = v;
        }
    }

    #[test]
    fn the_matrix_is_applied_after_the_curve() {
        // Swap R and B: a pure-red code must come out blue, with the
        // magnitude of the *decoded* red — proving the order.
        let swap = [[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]];
        let out = hdr10_codes_to_scene_linear_rgba(&[pack_rgb10(600, 0, 0)], HdrTransfer::Pq, swap);
        let red = hdr_code_to_scene_linear(600.0 / 1023.0, HdrTransfer::Pq);
        assert!(out[0].abs() < 1e-6);
        assert!((out[2] - red).abs() < 1e-5);
        assert_eq!(out[3], 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_stats_report_zero() {
        let s = LightLevelStats::new();
        assert_eq!(s.max_cll_nits(), 0);
        assert_eq!(s.max_fall_nits(), 0);
        assert_eq!(s.frame_count(), 0);
    }

    #[test]
    fn sdr_diffuse_white_master_reports_100_nits() {
        // A pure-white 1.0-linear frame is the SDR reference —
        // MaxCLL should land at NITS_PER_LINEAR (100 nits).
        let mut stats = LightLevelStats::new();
        let frame = vec![1.0_f32; 16 * 16 * 4];
        stats.update(&frame);
        assert_eq!(stats.max_cll_nits(), 100);
        // Full white frame → MaxFALL equals MaxCLL (per BT.2020 luma
        // coefficients, pure white = Y of 1.0).
        assert_eq!(stats.max_fall_nits(), 100);
    }

    #[test]
    fn thousand_nit_highlight_master_reports_1000_cll() {
        // The Phase 11 acceptance criterion. A single bright
        // highlight pixel at 10.0 linear = 1000 nits drives MaxCLL
        // to 1000 even if the rest of the frame is dark.
        let mut stats = LightLevelStats::new();
        let mut frame = vec![0.0_f32; 32 * 32 * 4];
        // alpha
        for px in frame.chunks_exact_mut(4) {
            px[3] = 1.0;
        }
        // one 1000-nit highlight pixel
        frame[0] = 10.0;
        frame[1] = 10.0;
        frame[2] = 10.0;
        stats.update(&frame);
        assert_eq!(stats.max_cll_nits(), 1000);
        // Average luminance should be *well* below 1000 — one bright
        // pixel in 1024 is near-zero average.
        assert!(stats.max_fall_nits() < 10);
    }

    #[test]
    fn stats_track_peak_across_frames() {
        let mut stats = LightLevelStats::new();
        // Frame 1: moderate brightness everywhere
        let frame_a = vec![0.5_f32; 8 * 8 * 4];
        stats.update(&frame_a);
        let fall_after_a = stats.max_fall_nits();
        // Frame 2: single very bright pixel, rest dark
        let mut frame_b = vec![0.0_f32; 8 * 8 * 4];
        frame_b[0] = 20.0;
        frame_b[1] = 20.0;
        frame_b[2] = 20.0;
        stats.update(&frame_b);
        assert_eq!(stats.max_cll_nits(), 2000);
        // MaxFALL should *not* have dropped — it's a running max.
        assert!(stats.max_fall_nits() >= fall_after_a);
        assert_eq!(stats.frame_count(), 2);
    }

    #[test]
    fn negative_channels_ignored() {
        let mut stats = LightLevelStats::new();
        // Over-saturation can produce negative channel values.
        let frame = vec![-5.0_f32, 0.1, -0.2, 1.0, -1.0, 0.2, -0.3, 1.0];
        stats.update(&frame);
        // Peak should come from the non-negative 0.2, not the -5.0.
        assert!(stats.max_cll_nits() <= 20);
    }

    #[test]
    fn empty_frame_is_noop() {
        let mut stats = LightLevelStats::new();
        stats.update(&[]);
        assert_eq!(stats.frame_count(), 0);
    }

    #[test]
    fn bt2020_primaries_are_well_formed() {
        use bt2020_primaries::*;
        // Chromaticity coords must sum to ≤ 1 (x + y < 1, with z the
        // rest). Quick sanity that we haven't copy-pasted wrong
        // numbers.
        for (x, y) in [RED, GREEN, BLUE, WHITE_D65] {
            assert!(x > 0.0 && y > 0.0);
            // BT.2020 Red sits right on the spectral locus (x + y =
            // 1.0 exactly), so allow equality. Anything above 1.0 is
            // non-physical.
            assert!(x + y <= 1.0, "({x}, {y}) lies outside the xy triangle");
        }
    }
}
