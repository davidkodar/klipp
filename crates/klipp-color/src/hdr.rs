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
