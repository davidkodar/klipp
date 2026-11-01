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
//! HDR sources on the way in (code values → scene-linear, the SDR display
//! tone map), HDR deliveries on the way out (display light, the PQ and HLG
//! encodes, BT.2408 placement of SDR material), and the HDR10 statistics
//! (MaxCLL / MaxFALL, mastering-display defaults).

/// Scale factor mapping `1.0 linear` to nits (cd/m²).
///
/// See module docs for the rationale. If you change this, audit every
/// tone-map operator and every HDR ground-truth test — the white point
/// / highlight cap relationships break otherwise.
pub const NITS_PER_LINEAR: f32 = 100.0;

/// Running light-level statistics for an HDR10 master (CTA-861.3).
///
/// Fed one frame at a time via [`Self::add_frame`] with what the file
/// actually carries — the encoder measures the signal it writes, after
/// the peak is held — and read out at finalise via
/// [`Self::max_cll_nits`] / [`Self::max_fall_nits`].
///
/// Both values are expressed in **cd/m² (nits)**, rounded to the
/// nearest integer, and clamped to `u32::MAX` at the top — HDR10
/// signals them as unsigned 16-bit (max 65 535 nits), but we return
/// `u32` so a caller that upgrades to AV1's 18-bit `hdr_metadata_obu`
/// field doesn't need a type change.
#[derive(Debug, Clone, Default)]
pub struct LightLevelStats {
    /// Brightest pixel so far, cd/m².
    max_cll: f32,
    /// Brightest frame average so far, cd/m².
    max_fall: f32,
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

    /// One frame's light levels, cd/m²: its brightest pixel and its
    /// average pixel, each pixel measured by its brightest channel
    /// (CTA-861.3's maxRGB, for both).
    pub fn add_frame(&mut self, max_nits: f32, average_nits: f32) {
        self.max_cll = self.max_cll.max(max_nits);
        self.max_fall = self.max_fall.max(average_nits);
        self.frames += 1;
    }

    /// Max Content Light Level in cd/m², rounded to nearest integer.
    pub fn max_cll_nits(&self) -> u32 {
        to_nits_u32(self.max_cll)
    }

    /// Max Frame-Average Light Level in cd/m², rounded to nearest integer.
    pub fn max_fall_nits(&self) -> u32 {
        to_nits_u32(self.max_fall)
    }
}

fn to_nits_u32(nits: f32) -> u32 {
    if !(nits > 0.0) {
        return 0;
    }
    let nits = nits.round();
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
//              encode then proceed exactly as for SDR clips. An HDR
//              delivery takes `apply_hdr_display_light` instead (see
//              "HDR deliveries" below).
//
// The tone map used to run inside the decoder, which meant every grade
// operated on an image whose highlights had already been compressed to
// SDR — and the auto-seeded InputTransform(PQ) then decoded PQ a second
// time. Moving it to the end of the clip is what lets a grade see the
// HDR range at all.

/// Scene-referred transfers decoded on input: sources whose encoded
/// range is wider than the display's, which the grade has to see before
/// any tone map. PQ and HLG are HDR delivery formats; camera log curves
/// (S-Log3, LogC, V-Log, Apple Log, ...) are the other kind — a camera
/// capturing 12-15 stops. Both take the same route: code values from the
/// decoder, scene-linear for the grade, a display tone map last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrTransfer {
    /// SMPTE ST 2084.
    Pq,
    /// ARIB STD-B67 / BT.2100 HLG.
    Hlg,
    /// A camera log curve, as the decoder's `ColorTransfer`.
    Log(klipp_core::ColorTransfer),
}

/// Every camera log transfer the decoder can decode.
const CAMERA_LOGS: &[klipp_core::ColorTransfer] = {
    use klipp_core::ColorTransfer as T;
    &[
        T::SLog3, T::SLog2, T::VLog, T::LogC3, T::LogC4, T::CanonLog, T::CanonLog3,
        T::RedLog3G10, T::BmdFilmGen5, T::DJIDLog, T::FLog, T::AppleLog, T::NLog, T::GoProFlat,
        T::LeicaLLog,
    ]
};

impl HdrTransfer {
    /// The `klipp_core::color_ids` transfer id, for GPU kernels.
    pub fn transfer_id(self) -> u32 {
        match self {
            Self::Pq => klipp_core::color_ids::TRANSFER_PQ,
            Self::Hlg => klipp_core::color_ids::TRANSFER_HLG,
            Self::Log(t) => klipp_core::color_ids::transfer_id(t),
        }
    }

    /// From a `klipp_core::color_ids` transfer id. `None` for every
    /// display-referred transfer (sRGB, BT.709, ...), so callers can use
    /// it as the "does this need the scene-referred route" test as well.
    pub fn from_transfer_id(id: u32) -> Option<Self> {
        match id {
            klipp_core::color_ids::TRANSFER_PQ => Some(Self::Pq),
            klipp_core::color_ids::TRANSFER_HLG => Some(Self::Hlg),
            _ => CAMERA_LOGS
                .iter()
                .copied()
                .find(|t| klipp_core::color_ids::transfer_id(*t) == id)
                .map(Self::Log),
        }
    }

    pub fn is_camera_log(self) -> bool {
        matches!(self, Self::Log(_))
    }

    /// Which of [`hdr_display_light`]'s three derivations this transfer
    /// takes: 0 PQ, 1 HLG, 2 camera log.
    ///
    /// A three-way category rather than [`Self::transfer_id`], because
    /// that is all the arithmetic distinguishes — every camera log has
    /// already been linearised by the time this runs — and a GPU twin
    /// branching on it does not have to carry `color_ids`' whole table.
    /// `display_light_kinds_cover_every_transfer` holds the mapping.
    pub fn display_light_kind(self) -> u32 {
        match self {
            Self::Pq => 0,
            Self::Hlg => 1,
            Self::Log(_) => 2,
        }
    }

    /// Multiplier from the curve's own output to scene-linear.
    ///
    /// PQ/HLG curves return 1.0 = 10 000 nits; this module's convention is
    /// 1.0 = 100 nits, so 100. Camera log curves already return scene
    /// reflectance (0.18 = mid grey), so 1.
    pub fn scene_scale(self) -> f32 {
        match self {
            Self::Pq | Self::Hlg => PQ_PEAK_NITS / NITS_PER_LINEAR,
            Self::Log(_) => 1.0,
        }
    }

    /// Exposure ahead of the display tone map. See `HDR_DISPLAY_EXPOSURE`
    /// and `LOG_DISPLAY_EXPOSURE` for why they differ.
    pub fn display_exposure(self) -> f32 {
        match self {
            Self::Pq | Self::Hlg => HDR_DISPLAY_EXPOSURE,
            Self::Log(_) => LOG_DISPLAY_EXPOSURE,
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

/// Exposure ahead of the display tone map for camera log footage: the
/// value that keeps 18% grey at display mid grey, `filmic(0.18 * k) =
/// 0.18` (sRGB 0.461). 90% white then lands at 0.85 and three stops over
/// white at 0.996 — a normal-looking image whose highlights roll off
/// instead of clipping, which is the "converted automatically" behaviour.
///
/// PQ uses a different constant for a principled reason, not an arbitrary
/// one: PQ encodes absolute display luminance, and its exposure was chosen
/// to keep the look HDR clips already had; camera log encodes scene
/// reflectance, and the natural SDR rendering of reflectance maps grey to
/// grey. Using PQ's 0.18 here would put mid grey at sRGB 0.16.
pub const LOG_DISPLAY_EXPOSURE: f32 = 0.723_171;

/// PQ's absolute peak, in nits.
const PQ_PEAK_NITS: f32 = 10_000.0;

/// A PQ or HLG code value in `[0, 1]` → scene-linear, `1.0 = 100 nits`.
///
/// HLG is relative rather than absolute, so it has no true nits value.
/// It is scaled by the same factor as PQ, which is what the decoder did
/// before this module existed; that keeps existing HLG material looking
/// the same rather than claiming a luminance interpretation it never had.
pub fn hdr_code_to_scene_linear(code: f32, transfer: HdrTransfer) -> f32 {
    let curve = match transfer {
        HdrTransfer::Pq => crate::camera_log::pq_to_linear(code),
        HdrTransfer::Hlg => crate::camera_log::hlg_to_linear(code),
        // For camera logs `code` is CV/1023, the domain the curves are
        // published in. Clamped at 0 exactly as the decoder shader does,
        // so a preview and a graded frame agree below black too.
        HdrTransfer::Log(t) => crate::camera_log::apply_inverse_eotf(code, t),
    };
    curve * transfer.scene_scale()
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

/// Scene-linear channel → display-referred linear, `[0, 1]`, at the given
/// exposure (`HdrTransfer::display_exposure`).
pub fn hdr_display_tonemap(scene_linear: f32, exposure: f32) -> f32 {
    filmic((scene_linear * exposure).max(0.0))
}

/// Tone-map a graded scene-linear RGBA frame to display-referred linear,
/// in place. Alpha is untouched.
///
/// This is the last step of an HDR clip's own processing. After it the
/// clip is display-referred like any SDR clip, so compositing and the
/// sRGB display encode need no HDR awareness at all.
pub fn apply_hdr_display_tonemap(rgba: &mut [f32], transfer: HdrTransfer) {
    use rayon::prelude::*;
    let k = transfer.display_exposure();
    rgba.par_chunks_exact_mut(4).for_each(|px| {
        px[0] = hdr_display_tonemap(px[0], k);
        px[1] = hdr_display_tonemap(px[1], k);
        px[2] = hdr_display_tonemap(px[2], k);
    });
}

// ─── HDR deliveries ───────────────────────────────────────────────────
//
// An HLG or PQ export renders every clip to *display light* instead of
// the SDR display picture, in units where 1.0 is SDR reference white,
// 203 cd/m² (BT.2408):
//
//   SDR clips, titles, generators   unchanged — their white lands at
//                                   203 nits, as BT.2408 places SDR
//                                   material in an HDR programme.
//   PQ sources                      their own absolute luminance: an
//                                   ungraded PQ clip exports as it came.
//   HLG sources                     the BT.2100 HLG OOTF on a 1000-nit
//                                   display (75% signal → 203 nits).
//   camera log                      the BT.2100 PQ reference OOTF, exposed
//                                   so 18% grey lands at BT.2408's 26
//                                   nits, highlights rolled off into the
//                                   master's peak.
//
// The encoder then converts to BT.2020, holds the peak, and writes PQ
// (`pq_encode_nits`) or HLG (`hlg_encode_nits`).

/// SDR reference white in an HDR picture, cd/m² (ITU-R BT.2408): where an
/// HDR delivery puts SDR white, and the unit of its display light.
pub const HDR_REFERENCE_WHITE_NITS: f32 = 203.0;

/// Nominal peak of the BT.2100 HLG reference display, cd/m².
pub const HLG_NOMINAL_PEAK_NITS: f32 = 1000.0;

/// BT.2100 HLG system gamma at the nominal 1000-nit peak.
const HLG_GAMMA: f32 = 1.2;

/// Exposure from camera-log scene reflectance to the BT.2100 PQ reference
/// OOTF's input: 18% grey → 26 cd/m², BT.2408's grey. Diffuse white
/// (100%) then lands at 205 nits, next to SDR white.
pub const LOG_HDR_EXPOSURE: f32 = 0.031_024_16;

/// Where the camera-log highlight roll-off starts, as a fraction of the
/// master's peak. Below it the reference OOTF is untouched.
const LOG_HDR_KNEE: f32 = 0.5;

/// The BT.2100 PQ reference OOTF: scene light `e` (1.0 = the OOTF's
/// 10 000-nit top) → display light, cd/m². The BT.709 OETF followed by
/// the BT.1886 EOTF, with its toe and 1.2-ish system gamma.
fn pq_reference_ootf(e: f32) -> f32 {
    let x = 59.520_8 * e.max(0.0);
    let ep = if x <= 0.018 { 4.5 * x } else { 1.099 * x.powf(0.45) - 0.099 };
    100.0 * ep.powf(2.4)
}

/// Compress `x` above `knee` so it approaches `peak` without reaching it:
/// slope 1 at the knee, so the curve has no corner.
fn roll_off(x: f32, knee: f32, peak: f32) -> f32 {
    if x <= knee {
        return x;
    }
    let t = (x - knee) / (peak - knee);
    knee + (peak - knee) * t / (1.0 + t)
}

/// A graded scene-linear pixel → HDR display light (1.0 = 203 cd/m²).
/// `luma` is the working space's [`crate::primaries::luminance_weights`],
/// for the HLG OOTF; `peak_nits` the master's peak, for the camera-log
/// roll-off. PQ and HLG sources are display light already and are not
/// rolled off: the encoder holds the peak.
pub fn hdr_display_light(rgb: [f32; 3], transfer: HdrTransfer, luma: [f32; 3], peak_nits: f32) -> [f32; 3] {
    let nits = match transfer {
        HdrTransfer::Pq => rgb.map(|c| c * NITS_PER_LINEAR),
        HdrTransfer::Hlg => {
            // Undo the decoder's PQ-sized scale: HLG scene light, 0..1.
            // Channels stay signed: a BT.2020 colour outside the working
            // gamut is negative here and must survive the trip back.
            let e = rgb.map(|c| c / transfer.scene_scale());
            let ys = luma[0] * e[0] + luma[1] * e[1] + luma[2] * e[2];
            let gain = if ys > 0.0 { HLG_NOMINAL_PEAK_NITS * ys.powf(HLG_GAMMA - 1.0) } else { 0.0 };
            e.map(|c| c * gain)
        }
        HdrTransfer::Log(_) => {
            // Odd-symmetric, for the same reason.
            let n = rgb.map(|c| pq_reference_ootf(c.abs() * LOG_HDR_EXPOSURE).copysign(c));
            // Roll off the brightest channel and scale the others with it,
            // so a highlight keeps its hue as it compresses.
            let m = n[0].max(n[1]).max(n[2]);
            let rolled = roll_off(m, LOG_HDR_KNEE * peak_nits, peak_nits);
            let k = if m > 0.0 { rolled / m } else { 1.0 };
            n.map(|c| c * k)
        }
    };
    nits.map(|c| c / HDR_REFERENCE_WHITE_NITS)
}

/// [`hdr_display_light`] over a graded scene-linear RGBA frame, in place:
/// the last step of a scene-referred clip in an HDR delivery, where an SDR
/// one runs [`apply_hdr_display_tonemap`]. Alpha is untouched.
pub fn apply_hdr_display_light(rgba: &mut [f32], transfer: HdrTransfer, luma: [f32; 3], peak_nits: f32) {
    use rayon::prelude::*;
    rgba.par_chunks_exact_mut(4).for_each(|px| {
        let d = hdr_display_light([px[0], px[1], px[2]], transfer, luma, peak_nits);
        px[..3].copy_from_slice(&d);
    });
}

/// A scene-referred clip's last step of its own: its graded scene-linear
/// picture to the display, by [`apply_hdr_display_tonemap`] for an SDR
/// picture or [`apply_hdr_display_light`] for an HDR delivery or monitor.
/// One value names the step, so the renderer's CPU route and the plan
/// node that runs it on the GPU (phase 2b M2, step 11c) read the same
/// settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneToDisplay {
    /// The source's transfer.
    pub transfer: HdrTransfer,
    /// The delivery's or monitor's peak, cd/m², for display light; `None`
    /// tone-maps to an SDR picture.
    pub peak_nits: Option<f32>,
    /// The working space's [`crate::primaries::luminance_weights`].
    pub luma: [f32; 3],
}

impl SceneToDisplay {
    /// Apply the step to an RGBA frame, in place. Alpha is untouched.
    pub fn apply(&self, rgba: &mut [f32]) {
        match self.peak_nits {
            Some(peak) => apply_hdr_display_light(rgba, self.transfer, self.luma, peak),
            None => apply_hdr_display_tonemap(rgba, self.transfer),
        }
    }
}

/// BT.2020 display light (1.0 = 203 cd/m²) → absolute cd/m², clamped to
/// `[0, peak_nits]`. Over the peak the pixel is scaled down as a whole, so
/// a highlight too bright for the master keeps its hue instead of turning
/// towards white or a primary channel by channel.
#[inline]
pub fn display_light_to_nits(rgb2020: [f32; 3], peak_nits: f32) -> [f32; 3] {
    let n = rgb2020.map(|c| (c * HDR_REFERENCE_WHITE_NITS).max(0.0));
    let m = n[0].max(n[1]).max(n[2]);
    if m > peak_nits { n.map(|c| c * (peak_nits / m)) } else { n }
}

/// Entries in [`pq_encode_nits`]'s table.
const PQ_TABLE_SIZE: usize = 1024;

/// [`pq_encode_nits`]'s table: `linear_to_pq` at the eighth root of
/// luminance, `PQ_TABLE_SIZE + 1` entries.
fn pq_encode_table() -> &'static [f32] {
    static TABLE: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        (0..=PQ_TABLE_SIZE)
            .map(|i| crate::camera_log::linear_to_pq((i as f32 / PQ_TABLE_SIZE as f32).powi(8)))
            .collect()
    })
}

/// Absolute cd/m² → PQ signal (SMPTE ST 2084), `[0, 1]`; above 10 000
/// nits, 1.
///
/// By table: the curve sampled at the eighth root of luminance, where it
/// is nearly straight, and interpolated. That is within 3e-5 of the f32
/// curve everywhere (a thirtieth of a 10-bit code) at a fraction of its
/// two `powf`s per channel, which cost a 4K export ~20 ms a frame.
#[inline]
pub fn pq_encode_nits(nits: f32) -> f32 {
    let table = pq_encode_table();
    let t = (nits / PQ_PEAK_NITS).clamp(0.0, 1.0).sqrt().sqrt().sqrt() * PQ_TABLE_SIZE as f32;
    let i = (t as usize).min(PQ_TABLE_SIZE - 1);
    let f = t - i as f32;
    table[i] + (table[i + 1] - table[i]) * f
}

/// Offsets into [`hdr_encode_gpu_data`], in floats.
pub mod gpu_layout {
    /// `pq_table_entries`.
    pub const HEADER: usize = 0;
    pub const HEADER_LEN: usize = 1;
    /// The working space → BT.2020 matrix, row-major, then the working
    /// space's luminance weights (for the HLG OOTF), padded to a matrix
    /// row so the curve below starts at a predictable offset.
    pub const TO_2020: usize = HEADER_LEN;
    pub const TO_2020_LEN: usize = 9;
    pub const LUMA: usize = TO_2020 + TO_2020_LEN;
    pub const LUMA_LEN: usize = 3;
    /// [`super::pq_encode_nits`]'s table.
    pub const PQ_TABLE: usize = LUMA + LUMA_LEN;
}

/// The numbers a GPU needs to encode an HDR delivery: the basis matrix,
/// the working space's luma weights, and [`pq_encode_nits`]'s table.
///
/// The table is *shipped* rather than recomputed in the shader for the
/// same reason the ACES curves are: the CPU already interpolates this
/// table rather than evaluating ST-2084 directly, so a shader that
/// evaluated the real curve would differ from the CPU by the table's own
/// 3e-5 error. Sharing it makes the two agree by construction, and the
/// GPU skips two `pow`s a channel either way.
///
/// HLG needs no table — its OETF is a `sqrt` and a `log`, exactly.
pub fn hdr_encode_gpu_data(working: klipp_core::WorkingColorspace) -> Vec<f32> {
    let table = pq_encode_table();
    let mut out = Vec::with_capacity(gpu_layout::PQ_TABLE + table.len());
    out.push(table.len() as f32);
    debug_assert_eq!(out.len(), gpu_layout::TO_2020);
    for row in crate::primaries::working_to_bt2020(working) {
        out.extend_from_slice(&row);
    }
    debug_assert_eq!(out.len(), gpu_layout::LUMA);
    out.extend_from_slice(&crate::primaries::luminance_weights(working));
    debug_assert_eq!(out.len(), gpu_layout::PQ_TABLE);
    out.extend_from_slice(table);
    out
}

/// PQ signal → absolute cd/m².
#[inline]
pub fn pq_decode_nits(signal: f32) -> f32 {
    crate::camera_log::pq_to_linear(signal) * PQ_PEAK_NITS
}

/// HLG OETF (BT.2100): scene light `[0, 1]` → signal `[0, 1]`.
#[inline]
pub fn hlg_oetf(e: f32) -> f32 {
    let (a, b, c) = (0.178_832_77_f32, 0.284_668_92_f32, 0.559_910_73_f32);
    let e = e.clamp(0.0, 1.0);
    if e <= 1.0 / 12.0 { (3.0 * e).sqrt() } else { a * (12.0 * e - b).ln() + c }
}

/// BT.2020 display light in cd/m² (at most the HLG display's 1000) → HLG
/// signal: the inverse HLG OOTF on the nominal display, then the OETF.
/// The BT.2100 conversion from display light, so SDR white at 203 nits
/// arrives at the 75% signal BT.2408 gives it.
#[inline]
pub fn hlg_encode_nits(nits2020: [f32; 3]) -> [f32; 3] {
    const Y2020: [f32; 3] = [0.2627, 0.6780, 0.0593];
    let f = nits2020.map(|c| c.clamp(0.0, HLG_NOMINAL_PEAK_NITS) / HLG_NOMINAL_PEAK_NITS);
    let yd = Y2020[0] * f[0] + Y2020[1] * f[1] + Y2020[2] * f[2];
    if yd <= 0.0 {
        return [0.0; 3];
    }
    // Fd = Ys^(γ-1)·E with Yd = Ys^γ, so E = Fd·Yd^((1-γ)/γ).
    let k = yd.powf((1.0 - HLG_GAMMA) / HLG_GAMMA);
    f.map(|c| hlg_oetf(c * k))
}

/// Every 10-bit code value → scene-linear. Evaluating the curve 1024
/// times up front turns the per-pixel `powf` chain into a table read,
/// and is exact: the input only ever takes these 1024 values.
pub fn hdr_code_lut(transfer: HdrTransfer) -> Vec<f32> {
    (0..1024u32)
        .map(|i| hdr_code_to_scene_linear(i as f32 / 1023.0, transfer))
        .collect()
}

/// Every 16-bit code value → scene-linear, for sources deeper than 10
/// bits (#1226): 65 536 entries, made once per curve and kept, so a deep
/// frame's unpack is a table read as a 10-bit one is.
pub fn hdr_code_lut16(transfer: HdrTransfer) -> std::sync::Arc<Vec<f32>> {
    static LUTS: std::sync::Mutex<Vec<(HdrTransfer, std::sync::Arc<Vec<f32>>)>> = std::sync::Mutex::new(Vec::new());
    let mut luts = LUTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, lut)) = luts.iter().find(|(t, _)| *t == transfer) {
        return lut.clone();
    }
    let lut = std::sync::Arc::new((0..65_536u32).map(|i| hdr_code_to_scene_linear(i as f32 / 65_535.0, transfer)).collect::<Vec<f32>>());
    luts.push((transfer, lut.clone()));
    lut
}

/// A scene-referred frame's code values in the source's primaries: three
/// 10-bit codes a pixel for sources up to 10 bits, three 16-bit codes a
/// pixel for deeper ones, so a 12- or 16-bit source keeps its precision
/// up to the curve (#1226).
#[derive(Debug, Clone, PartialEq)]
pub enum SceneCodes {
    /// [`pack_rgb10`] codes.
    Packed10(Vec<u32>),
    /// [`pack_rgb16`] codes.
    Deep16(Vec<u64>),
}

impl SceneCodes {
    /// Pixels.
    pub fn len(&self) -> usize {
        match self {
            Self::Packed10(c) => c.len(),
            Self::Deep16(c) => c.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes held.
    pub fn byte_len(&self) -> usize {
        match self {
            Self::Packed10(c) => c.len() * 4,
            Self::Deep16(c) => c.len() * 8,
        }
    }

    /// The 10-bit codes, the form the GPU bridge takes; `None` for deep
    /// ones, which convert on the CPU.
    pub fn packed10(&self) -> Option<&[u32]> {
        match self {
            Self::Packed10(c) => Some(c),
            Self::Deep16(_) => None,
        }
    }

    /// Scene-linear RGBA f32 in the working space: see
    /// [`hdr10_codes_to_scene_linear_rgba`].
    pub fn to_scene_linear_rgba(&self, transfer: HdrTransfer, source_to_working: [[f32; 3]; 3]) -> Vec<f32> {
        match self {
            Self::Packed10(c) => hdr10_codes_to_scene_linear_rgba(c, transfer, source_to_working),
            Self::Deep16(c) => hdr16_codes_to_scene_linear_rgba(c, transfer, source_to_working),
        }
    }

    /// As the viewer shows them ungraded: see
    /// [`hdr10_codes_to_display_srgb8`].
    pub fn to_display_srgb8(&self, transfer: HdrTransfer, source_to_display: [[f32; 3]; 3]) -> Vec<u8> {
        let mut linear = self.to_scene_linear_rgba(transfer, source_to_display);
        apply_hdr_display_tonemap(&mut linear, transfer);
        display_linear_to_srgb8(&linear)
    }
}

/// Pack three 16-bit code values into one `u64`: R in bits 0-15, G in
/// 16-31, B in 32-47.
pub fn pack_rgb16(r: u16, g: u16, b: u16) -> u64 {
    r as u64 | (g as u64) << 16 | (b as u64) << 32
}

/// Inverse of [`pack_rgb16`].
pub fn unpack_rgb16(p: u64) -> [u16; 3] {
    [p as u16, (p >> 16) as u16, (p >> 32) as u16]
}

/// [`hdr10_codes_to_scene_linear_rgba`] for 16-bit codes, through
/// [`hdr_code_lut16`].
pub fn hdr16_codes_to_scene_linear_rgba(packed: &[u64], transfer: HdrTransfer, source_to_working: [[f32; 3]; 3]) -> Vec<f32> {
    use rayon::prelude::*;
    let lut = hdr_code_lut16(transfer);
    let m = source_to_working;
    let mut out = vec![0.0_f32; packed.len() * 4];
    out.par_chunks_exact_mut(4).zip(packed.par_iter()).for_each(|(dst, &p)| {
        let [r, g, b] = unpack_rgb16(p);
        let (r, g, b) = (lut[r as usize], lut[g as usize], lut[b as usize]);
        dst[0] = m[0][0] * r + m[0][1] * g + m[0][2] * b;
        dst[1] = m[1][0] * r + m[1][1] * g + m[1][2] * b;
        dst[2] = m[2][0] * r + m[2][1] * g + m[2][2] * b;
        dst[3] = 1.0;
    });
    out
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

/// A scene-referred frame's codes as the viewer shows them ungraded: scene
/// linear, the display tone map, sRGB-encoded RGBA8. `source_to_display`
/// takes the source primaries to BT.709.
pub fn hdr10_codes_to_display_srgb8(
    packed: &[u32],
    transfer: HdrTransfer,
    source_to_display: [[f32; 3]; 3],
) -> Vec<u8> {
    let mut linear = hdr10_codes_to_scene_linear_rgba(packed, transfer, source_to_display);
    apply_hdr_display_tonemap(&mut linear, transfer);
    display_linear_to_srgb8(&linear)
}

/// Display-referred linear RGBA f32 → sRGB-encoded RGBA8, alpha opaque.
pub fn display_linear_to_srgb8(linear: &[f32]) -> Vec<u8> {
    use rayon::prelude::*;
    let e = |c: f32| (crate::transfer::linear_to_srgb(c).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let mut out = vec![0u8; linear.len()];
    out.par_chunks_exact_mut(4).zip(linear.par_chunks_exact(4)).for_each(|(o, px)| {
        o.copy_from_slice(&[e(px[0]), e(px[1]), e(px[2]), 255]);
    });
    out
}

/// One decoded frame's Y'CbCr samples at their native bit depth, any
/// chroma subsampling. `u` and `v` hold `chroma_width x chroma_height`
/// samples, where `chroma_width = ceil(width / 2^chroma_shift_w)`.
pub struct YuvSamples<'a> {
    pub y: &'a [u16],
    pub u: &'a [u16],
    pub v: &'a [u16],
    pub width: u32,
    pub height: u32,
    /// log2 of the horizontal / vertical chroma subsampling: 4:2:0 is
    /// (1, 1), 4:2:2 is (1, 0), 4:4:4 is (0, 0).
    pub chroma_shift_w: u32,
    pub chroma_shift_h: u32,
    pub bit_depth: u32,
    /// Where the file says its chroma samples sit (#3266).
    pub siting: ChromaSiting,
}

impl YuvSamples<'_> {
    pub fn chroma_width(&self) -> u32 {
        (self.width + (1 << self.chroma_shift_w) - 1) >> self.chroma_shift_w
    }

    pub fn chroma_height(&self) -> u32 {
        (self.height + (1 << self.chroma_shift_h) - 1) >> self.chroma_shift_h
    }

    /// Chroma at a luma position, reconstructed rather than repeated.
    ///
    /// A subsampled chroma plane is a *sampled* signal. Taking the
    /// nearest sample repeats one value across the whole 2x2 luma block,
    /// which puts a hard colour step wherever chroma changes: measured
    /// against swscale on a red/blue edge, the nearest path crossed in
    /// zero pixels where an interpolating one ramped over two.
    /// Interpolating reconstructs what the encoder sampled.
    ///
    /// Siting is the file's own, [`Self::siting`]. Untagged, it follows
    /// H.264/HEVC `chroma_sample_loc_type = 0`, the default for most of
    /// what Klipp decodes: a chroma sample sits on the same column as the
    /// even luma sample, and vertically *between* the two luma rows. So
    /// the horizontal coordinate divides exactly while the vertical one
    /// carries a quarter-sample offset. Dropping that
    /// offset shifts chroma half a sample up the frame, which reads as
    /// colour fringing along one side of every horizontal edge — the
    /// failure this is easiest to introduce and hardest to see.
    ///
    /// 4:2:2 (`chroma_shift_h == 0`) and 4:4:4 fall out of the same
    /// arithmetic: a shift of zero makes the coordinate exact and both
    /// weights collapse onto the one sample.
    pub fn chroma_at(&self, sx: usize, sy: usize) -> (f32, f32) {
        let (cw, ch) = (self.chroma_width() as usize, self.chroma_height() as usize);
        let (x0, x1, tx) = chroma_taps(sx, self.chroma_shift_w, cw, self.siting.h);
        let (y0, y1, ty) = chroma_taps(sy, self.chroma_shift_h, ch, self.siting.v);
        let at = |p: &[u16], x: usize, y: usize| p[y * cw + x] as f32;
        let lerp2 = |p: &[u16]| {
            let top = at(p, x0, y0) + (at(p, x1, y0) - at(p, x0, y0)) * tx;
            let bot = at(p, x0, y1) + (at(p, x1, y1) - at(p, x0, y1)) * tx;
            top + (bot - top) * ty
        };
        (lerp2(self.u), lerp2(self.v))
    }
}

/// Where a chroma sample sits relative to the luma samples it covers,
/// along one axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromaAxis {
    /// The chroma sample shares its coordinate with the even luma sample
    /// — 4:2:0's and 4:2:2's horizontal axis.
    CoSited,
    /// The chroma sample sits midway *between* the luma samples it
    /// covers — 4:2:0's vertical axis.
    Between,
}

/// Where a file's chroma samples sit on each axis, from its
/// `chroma_location` (ffmpeg's `AVChromaLocation`, H.264/HEVC
/// `chroma_sample_loc_type`). Every converter, CPU and GPU, reconstructs
/// chroma there (#3266), so a centre-sited file (MPEG-1, JPEG) or a
/// top-left-sited one is not read a quarter of a chroma sample off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChromaSiting {
    pub h: ChromaAxis,
    pub v: ChromaAxis,
}

impl Default for ChromaSiting {
    /// Left: what H.264, HEVC and MPEG-2 mean when a file says nothing,
    /// and what Klipp read every file as before it read the tag.
    fn default() -> Self {
        Self { h: ChromaAxis::CoSited, v: ChromaAxis::Between }
    }
}

impl ChromaSiting {
    /// From an `AVChromaLocation`: 1 left, 2 center, 3 top-left, 4 top,
    /// 5 bottom-left, 6 bottom; 0 (unspecified) and anything else is
    /// left. The two bottom positions, a whole luma row down, are rare
    /// and read as between the rows, the nearest siting there is.
    pub fn from_av_location(location: i32) -> Self {
        use ChromaAxis::{Between, CoSited};
        match location {
            2 | 6 => Self { h: Between, v: Between },
            3 => Self { h: CoSited, v: CoSited },
            4 => Self { h: Between, v: CoSited },
            _ => Self::default(),
        }
    }

    /// The shaders' chroma offsets, horizontal and vertical: what each
    /// axis takes off `s / 2` for a 4:2:0 plane, 0 co-sited or 0.25
    /// between, as [`chroma_taps`] works it out.
    pub fn shader_offsets(self) -> [f32; 2] {
        let off = |a: ChromaAxis| if a == ChromaAxis::Between { 0.25 } else { 0.0 };
        [off(self.h), off(self.v)]
    }
}

/// The two chroma samples to blend along one axis, and the weight of the
/// second, for a luma coordinate `s` on an axis subsampled by
/// `2^shift` into `extent` samples.
///
/// This is the one definition of Klipp's chroma siting, and every
/// converter goes through it — [`YuvSamples::chroma_at`] for the
/// scene-referred path, `camera_log`'s direct 10-bit converter, and the
/// decode shaders transcribe it. Four implementations of the same
/// arithmetic is how they drift apart, and a drift here is a colour
/// shift of half a chroma sample that looks like fringing rather than
/// like a bug.
///
/// Siting is H.264/HEVC `chroma_sample_loc_type = 0`, confirmed against
/// ffmpeg's own `av_chroma_location_enum_to_pos`, which puts
/// `AVCHROMA_LOC_LEFT` at xpos=0, ypos=128 in 1/256-luma units: no
/// horizontal offset, half a *luma* sample vertically — which is the
/// quarter of a *chroma* sample that [`ChromaAxis::Between`] applies.
///
/// An unsubsampled axis (`shift == 0`) returns both taps on the same
/// sample with weight 0, so 4:2:2 and 4:4:4 need no special case.
#[inline]
pub fn chroma_taps(s: usize, shift: u32, extent: usize, axis: ChromaAxis) -> (usize, usize, f32) {
    debug_assert!(extent > 0, "a chroma plane with no samples");
    let f = if shift == 0 {
        // Exact, and `Between` is meaningless without subsampling: there
        // is nothing to sit between.
        s as f32
    } else {
        let scale = (1u32 << shift) as f32;
        match axis {
            ChromaAxis::CoSited => s as f32 / scale,
            ChromaAxis::Between => (s as f32 + 0.5) / scale - 0.5,
        }
    };
    // Clamped before the floor, so `Between`'s offset cannot walk off
    // the start of the plane at s = 0, where `f` is negative.
    let f = f.clamp(0.0, (extent - 1) as f32);
    let i0 = f.floor() as usize;
    (i0, (i0 + 1).min(extent - 1), f - i0 as f32)
}

/// Scene-referred Y'CbCr → packed 10-bit R'G'B' code values: the CPU twin
/// of the scene-referred branch in the P010 / NV12 decoder shaders, for the
/// software-decode paths (ProRes and other formats NVDEC cannot take, the
/// no-GPU fallback, thumbnails and skim frames).
///
/// It must stay step-for-step identical to the shaders — nearest-neighbour
/// resize with the same rotation mapping, `yuv_normalise`,
/// `yuv_to_rgb_display`, the camera-log CV/1023 remap, the clamp in code
/// space and the rounding — because a clip that decodes in software must
/// look exactly like the same clip decoded on the GPU. Everything after the
/// codes (curve, gamut, tone map) is already shared:
/// [`hdr10_codes_to_scene_linear_rgba`] and [`apply_hdr_display_tonemap`].
///
/// `color_ids` is `[range, matrix, transfer, primaries]` as in
/// `klipp_core::color_ids`. A source of up to 10 bits gives 10-bit codes,
/// as the shaders write them; a deeper one 16-bit codes, so its precision
/// reaches the curve rather than being rounded to 10 bits (#1226).
pub fn yuv_to_hdr_codes(
    s: &YuvSamples,
    dst_w: u32,
    dst_h: u32,
    rotation: u32,
    color_ids: [u32; 4],
) -> SceneCodes {
    use klipp_core::color_ids as ids;
    use rayon::prelude::*;

    let [range, matrix, transfer, _] = color_ids;
    let is_log = transfer >= ids::FIRST_CAMERA_LOG;
    let max = ((1u32 << s.bit_depth) - 1) as f32;
    let (src_w, src_h) = (s.width.max(1), s.height.max(1));

    let normalise = |y: f32, u: f32, v: f32| -> [f32; 3] {
        if range == ids::COLOR_RANGE_FULL {
            [y / max, (u - max * 0.5 - 0.5) / max, (v - max * 0.5 - 0.5) / max]
        } else {
            let scale = (max + 1.0) / 256.0;
            [
                (y - 16.0 * scale) / (219.0 * scale),
                (u - 128.0 * scale) / (224.0 * scale),
                (v - 128.0 * scale) / (224.0 * scale),
            ]
        }
    };
    let to_rgb = |[y, u, v]: [f32; 3]| -> [f32; 3] {
        match matrix {
            ids::MATRIX_BT601 => [y + 1.402 * v, y - 0.344136 * u - 0.714136 * v, y + 1.772 * u],
            ids::MATRIX_BT2020_NCL => [y + 1.4746 * v, y - 0.16455 * u - 0.57135 * v, y + 1.8814 * u],
            ids::MATRIX_SMPTE240 => [y + 1.576 * v, y - 0.227 * u - 0.477 * v, y + 1.826 * u],
            ids::MATRIX_IDENTITY => [v, y, u],
            _ => [y + 1.5748 * v, y - 0.1873 * u - 0.4681 * v, y + 1.8556 * u],
        }
    };

    // The signal, 0 to 1, a destination pixel takes.
    let signal_at = |dx: u32, dy: u32| -> [f32; 3] {
        let (sx, sy) = match rotation {
            90 => (dy * src_w / dst_h, (dst_w - 1 - dx) * src_h / dst_w),
            180 => ((dst_w - 1 - dx) * src_w / dst_w, (dst_h - 1 - dy) * src_h / dst_h),
            270 => ((dst_h - 1 - dy) * src_w / dst_h, dx * src_h / dst_w),
            _ => (dx * src_w / dst_w, dy * src_h / dst_h),
        };
        let (sx, sy) = (sx.min(src_w - 1) as usize, sy.min(src_h - 1) as usize);
        let (cu, cv) = s.chroma_at(sx, sy);
        let rgb = to_rgb(normalise(s.y[sy * src_w as usize + sx] as f32, cu, cv));
        rgb.map(|c| {
            let signal = if is_log && range != ids::COLOR_RANGE_FULL { (64.0 + c * 876.0) / 1023.0 } else { c };
            signal.clamp(0.0, 1.0)
        })
    };

    let row_len = dst_w.max(1) as usize;
    let pixels = (dst_w as usize) * (dst_h as usize);
    if s.bit_depth <= 10 {
        let mut out = vec![0u32; pixels];
        out.par_chunks_mut(row_len).enumerate().for_each(|(dy, row)| {
            for (dx, px) in row.iter_mut().enumerate() {
                let code = signal_at(dx as u32, dy as u32).map(|c| (c * 1023.0 + 0.5) as u32);
                *px = pack_rgb10(code[0], code[1], code[2]);
            }
        });
        SceneCodes::Packed10(out)
    } else {
        let mut out = vec![0u64; pixels];
        out.par_chunks_mut(row_len).enumerate().for_each(|(dy, row)| {
            for (dx, px) in row.iter_mut().enumerate() {
                let code = signal_at(dx as u32, dy as u32).map(|c| (c * 65_535.0 + 0.5) as u16);
                *px = pack_rgb16(code[0], code[1], code[2]);
            }
        });
        SceneCodes::Deep16(out)
    }
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
            let new = hdr_display_tonemap(hdr_code_to_scene_linear(code, HdrTransfer::Pq), HDR_DISPLAY_EXPOSURE);
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
    fn camera_logs_take_the_scene_referred_route_by_id() {
        use klipp_core::ColorTransfer as T;
        for t in CAMERA_LOGS {
            let id = klipp_core::color_ids::transfer_id(*t);
            assert_eq!(HdrTransfer::from_transfer_id(id), Some(HdrTransfer::Log(*t)), "{t:?}");
        }
        assert_eq!(HdrTransfer::from_transfer_id(klipp_core::color_ids::TRANSFER_BT709), None);
        let slog3 = HdrTransfer::Log(T::SLog3);
        assert!(slog3.is_camera_log());
        assert_eq!(slog3.scene_scale(), 1.0);
    }

    /// The log display exposure is defined by one property: mid grey stays
    /// mid grey — through the whole route, S-Log3's published 18% grey code
    /// in, display out.
    #[test]
    fn log_display_exposure_keeps_mid_grey_at_mid_grey() {
        assert!((filmic(0.18 * LOG_DISPLAY_EXPOSURE) - 0.18).abs() < 1e-5);
        let slog3 = HdrTransfer::Log(klipp_core::ColorTransfer::SLog3);
        let grey = hdr_code_to_scene_linear(420.0 / 1023.0, slog3);
        assert!((grey - 0.18).abs() < 1e-4, "S-Log3 code 420 should decode to 0.18, got {grey}");
        let shown = hdr_display_tonemap(grey, slog3.display_exposure());
        assert!((shown - 0.18).abs() < 1e-3, "mid grey should display at 0.18, got {shown}");
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
            let v = hdr_display_tonemap(hdr_code_to_scene_linear(i as f32 / 1023.0, HdrTransfer::Pq), HDR_DISPLAY_EXPOSURE);
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

    const LUMA_709: [f32; 3] = [0.2126, 0.7152, 0.0722];

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    /// BT.2408's reference levels, which the HDR encodes are built on:
    /// SDR white (203 nits) is PQ 58% and HLG 75%; 18% grey (26 nits) is
    /// 38% in both.
    #[test]
    fn hdr_encodes_hit_the_bt2408_reference_levels() {
        assert!(close(pq_encode_nits(203.0), 0.5807, 5e-4), "{}", pq_encode_nits(203.0));
        assert!(close(pq_encode_nits(26.0), 0.3800, 5e-4));
        assert!(close(pq_decode_nits(pq_encode_nits(640.0)), 640.0, 0.05));
        let white = hlg_encode_nits([203.0; 3]);
        assert!(white.iter().all(|&c| close(c, 0.75, 2e-3)), "{white:?}");
        let grey = hlg_encode_nits([26.0; 3]);
        assert!(grey.iter().all(|&c| close(c, 0.38, 5e-3)), "{grey:?}");
        assert_eq!(hlg_encode_nits([1000.0; 3]), [1.0; 3]);
        assert_eq!(hlg_encode_nits([0.0; 3]), [0.0; 3]);
    }

    #[test]
    fn pq_encode_by_table_follows_the_curve() {
        let mut worst = 0.0f32;
        for i in 0..=100_000 {
            let nits = 10_000.0 * (i as f32 / 100_000.0).powi(5);
            let exact = crate::camera_log::linear_to_pq(nits / PQ_PEAK_NITS);
            worst = worst.max((pq_encode_nits(nits) - exact).abs());
        }
        // A thirtieth of a 10-bit code; the f32 curve itself is only good
        // to about 1e-5 (its exponent of 78.8 amplifies rounding).
        assert!(worst < 3e-5, "table strays {worst} from the curve");
        assert_eq!(pq_encode_nits(0.0), crate::camera_log::linear_to_pq(0.0));
        assert!((pq_encode_nits(20_000.0) - 1.0).abs() < 1e-6);
    }

    /// An ungraded PQ clip reaches the delivery at its own luminance.
    #[test]
    fn pq_display_light_is_the_sources_own_luminance() {
        for nits in [0.5_f32, 26.0, 203.0, 640.0, 1000.0, 4000.0] {
            let s = hdr_code_to_scene_linear(pq_encode_nits(nits), HdrTransfer::Pq);
            let d = hdr_display_light([s; 3], HdrTransfer::Pq, LUMA_709, 1000.0);
            assert!(close(d[0] * HDR_REFERENCE_WHITE_NITS, nits, nits * 2e-3), "{nits} → {}", d[0] * 203.0);
        }
    }

    /// HLG in, HLG out: the OOTF there and its inverse back give the
    /// signal it came with, colours included.
    #[test]
    fn hlg_display_light_round_trips_through_the_hlg_encode() {
        for code in [[0.75_f32; 3], [0.38; 3], [0.9, 0.5, 0.3], [0.2, 0.6, 0.8]] {
            let s = code.map(|c| hdr_code_to_scene_linear(c, HdrTransfer::Hlg));
            let d = hdr_display_light(s, HdrTransfer::Hlg, [0.2627, 0.6780, 0.0593], 1000.0);
            let back = hlg_encode_nits(d.map(|c| c * HDR_REFERENCE_WHITE_NITS));
            for ch in 0..3 {
                assert!(close(back[ch], code[ch], 1e-3), "{code:?} → {back:?}");
            }
        }
        // 75% HLG is SDR white.
        let s = hdr_code_to_scene_linear(0.75, HdrTransfer::Hlg);
        let d = hdr_display_light([s; 3], HdrTransfer::Hlg, LUMA_709, 1000.0);
        assert!(close(d[0], 1.0, 5e-3), "{}", d[0]);
    }

    /// Camera log: grey at BT.2408's 26 nits, diffuse white beside SDR
    /// white, highlights rising monotonically into the peak and never
    /// through it, a bright colour keeping its hue as it rolls off.
    #[test]
    fn log_display_light_places_grey_and_rolls_highlights_into_the_peak() {
        let log = HdrTransfer::Log(klipp_core::ColorTransfer::SLog3);
        let nits = |s: f32| hdr_display_light([s; 3], log, LUMA_709, 1000.0)[0] * HDR_REFERENCE_WHITE_NITS;
        assert!(close(nits(0.18), 26.0, 0.05), "grey at {}", nits(0.18));
        assert!(close(nits(1.0), 205.0, 1.0), "white at {}", nits(1.0));
        let mut prev = 0.0;
        for i in 1..=400 {
            let n = nits(i as f32 * 0.1);
            assert!(n > prev && n < 1000.0, "stop {i}: {n}");
            prev = n;
        }
        assert!(prev > 900.0, "40x white should be near the peak, got {prev}");
        let hot = hdr_display_light([30.0, 15.0, 3.0], log, LUMA_709, 1000.0);
        let plain = [30.0f32, 15.0, 3.0].map(|c| pq_reference_ootf(c * LOG_HDR_EXPOSURE));
        assert!(close(hot[1] / hot[0], plain[1] / plain[0], 1e-4));
        assert!(close(hot[2] / hot[0], plain[2] / plain[0], 1e-4));
    }

    #[test]
    fn display_light_holds_the_peak_without_turning_hue() {
        let n = display_light_to_nits([10.0, 5.0, 1.0], 1000.0);
        assert!(close(n[0], 1000.0, 1e-2));
        assert!(close(n[1], 500.0, 1e-2) && close(n[2], 100.0, 1e-2));
        assert_eq!(display_light_to_nits([1.0, -0.1, 0.5], 1000.0), [203.0, 0.0, 101.5]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every transfer has a display-light kind, and the three kinds are
    /// the three branches — so a GPU twin switching on the kind takes the
    /// same branch the CPU does.
    #[test]
    fn display_light_kinds_cover_every_transfer() {
        assert_eq!(HdrTransfer::Pq.display_light_kind(), 0);
        assert_eq!(HdrTransfer::Hlg.display_light_kind(), 1);
        for t in CAMERA_LOGS {
            let x = HdrTransfer::Log(*t);
            assert_eq!(x.display_light_kind(), 2, "{t:?} should be the camera-log branch");
            // And every camera log shares the branch's other inputs, so
            // the kind is the whole story.
            assert_eq!(x.scene_scale(), 1.0, "{t:?}");
        }
    }

    /// The blob's length is derivable from its first entry.
    ///
    /// A GPU host cannot see `gpu_layout` — `klipp-effects` does not
    /// depend on this crate — so it checks the buffer is long enough by
    /// recomputing the length from the header the shader itself reads
    /// (`GpuEffectsPipeline::ensure_hdr_encode_data`). That check is only
    /// a check if this holds.
    #[test]
    fn gpu_hdr_data_length_is_what_the_header_says() {
        for ws in [
            klipp_core::WorkingColorspace::Rec709Linear,
            klipp_core::WorkingColorspace::AcesCG,
            klipp_core::WorkingColorspace::Bt2020Linear,
        ] {
            let data = hdr_encode_gpu_data(ws);
            let entries = data[0] as usize;
            assert_eq!(entries, PQ_TABLE_SIZE + 1, "the header names the table size");
            // 1 header + 9 matrix + 3 luma, the arithmetic the host repeats.
            assert_eq!(data.len(), 13 + entries);
            assert_eq!(gpu_layout::PQ_TABLE, 13, "the table starts where the host assumes");
        }
    }

    /// The shipped table is the one `pq_encode_nits` reads, so a shader
    /// interpolating it lands on the twin's answer rather than near it.
    #[test]
    fn the_shipped_pq_table_is_the_one_the_cpu_uses() {
        let data = hdr_encode_gpu_data(klipp_core::WorkingColorspace::Rec709Linear);
        let table = &data[gpu_layout::PQ_TABLE..];
        // Re-run the CPU's own lookup against the shipped entries.
        let mut worst = 0.0f32;
        for i in 0..4096 {
            let nits = (i as f32 / 4095.0).powi(3) * 10_000.0;
            let t = (nits / PQ_PEAK_NITS).clamp(0.0, 1.0).sqrt().sqrt().sqrt()
                * PQ_TABLE_SIZE as f32;
            let k = (t as usize).min(PQ_TABLE_SIZE - 1);
            let f = t - k as f32;
            let from_blob = table[k] + (table[k + 1] - table[k]) * f;
            worst = worst.max((from_blob - pq_encode_nits(nits)).abs());
        }
        assert_eq!(worst, 0.0, "the blob's table is not bit-identical to the CPU's");
    }

    #[test]
    fn fresh_stats_report_zero() {
        let s = LightLevelStats::new();
        assert_eq!(s.max_cll_nits(), 0);
        assert_eq!(s.max_fall_nits(), 0);
        assert_eq!(s.frame_count(), 0);
    }

    #[test]
    fn stats_keep_the_brightest_pixel_and_the_brightest_frame() {
        let mut stats = LightLevelStats::new();
        // A bright frame on average, then one dark frame with a single
        // 1000-nit highlight.
        stats.add_frame(300.0, 250.4);
        stats.add_frame(1000.2, 3.0);
        assert_eq!(stats.max_cll_nits(), 1000);
        // A running max: the dark frame does not pull it down.
        assert_eq!(stats.max_fall_nits(), 250);
        assert_eq!(stats.frame_count(), 2);
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

#[cfg(test)]
mod chroma_siting_tests {
    use super::*;

    fn flat_luma(n: usize) -> Vec<u16> {
        vec![512u16; n]
    }

    /// 4:2:0 chroma is co-sited *horizontally*: a chroma sample shares a
    /// column with the even luma sample. So even columns read a sample
    /// exactly and the blend lands on the odd ones. A centre-sited
    /// implementation would blend on the even columns instead, moving
    /// every colour edge half a pixel sideways.
    #[test]
    fn horizontal_siting_blends_on_the_odd_columns() {
        let y = flat_luma(4);
        let (u, v) = (vec![0u16, 400], vec![0u16, 400]);
        let s = YuvSamples {
            y: &y, u: &u, v: &v, width: 4, height: 1,
            chroma_shift_w: 1, chroma_shift_h: 0, bit_depth: 10, siting: Default::default(),
        };
        let at = |x| s.chroma_at(x, 0).0;
        assert_eq!(at(0), 0.0, "x=0 sits on chroma column 0");
        assert_eq!(at(1), 200.0, "x=1 sits halfway between the columns");
        assert_eq!(at(2), 400.0, "x=2 sits on chroma column 1");
        assert_eq!(at(3), 400.0, "x=3 has no column to its right and clamps");
    }

    /// 4:2:0 chroma sits *between* luma rows, so the weights are 3:1 and
    /// 1:3, never even. This is the number that is easiest to get wrong
    /// and hardest to see: dropping the quarter-sample offset makes them
    /// 1:1 and slides chroma half a sample up the frame, which shows up
    /// as colour fringing along one side of every horizontal edge rather
    /// than as anything obviously broken.
    #[test]
    fn vertical_siting_carries_the_quarter_sample_offset() {
        let y = flat_luma(4);
        let (u, v) = (vec![0u16, 400], vec![0u16, 400]);
        let s = YuvSamples {
            y: &y, u: &u, v: &v, width: 1, height: 4,
            chroma_shift_w: 0, chroma_shift_h: 1, bit_depth: 10, siting: Default::default(),
        };
        let at = |row| s.chroma_at(0, row).0;
        assert_eq!(at(0), 0.0, "row 0 is above the first chroma centre and clamps");
        assert_eq!(at(1), 100.0, "row 1 is 3/4 chroma row 0 and 1/4 row 1");
        assert_eq!(at(2), 300.0, "row 2 is 1/4 chroma row 0 and 3/4 row 1");
        assert_eq!(at(3), 400.0, "row 3 is below the last chroma centre and clamps");
    }

    /// #3266: a file's `chroma_location` moves its taps. Centre-sited
    /// (MPEG-1, JPEG) chroma sits between the columns too, so the even
    /// columns blend 3:1 where left-sited ones read a sample exactly;
    /// top-left-sited chroma sits on the even rows, read exactly, where
    /// left-sited chroma blends 3:1.
    #[test]
    fn a_files_chroma_location_moves_the_taps() {
        use ChromaAxis::{Between, CoSited};
        let y = flat_luma(4);
        let (u, v) = (vec![0u16, 400], vec![0u16, 400]);
        let across = |siting| YuvSamples { y: &y, u: &u, v: &v, width: 4, height: 1, chroma_shift_w: 1, chroma_shift_h: 0, bit_depth: 10, siting };
        let down = |siting| YuvSamples { y: &y, u: &u, v: &v, width: 1, height: 4, chroma_shift_w: 0, chroma_shift_h: 1, bit_depth: 10, siting };
        let row = |s: YuvSamples| (0..4).map(|x| s.chroma_at(x, 0).0).collect::<Vec<_>>();
        let col = |s: YuvSamples| (0..4).map(|y| s.chroma_at(0, y).0).collect::<Vec<_>>();
        let centre = ChromaSiting::from_av_location(2);
        assert_eq!(centre, ChromaSiting { h: Between, v: Between });
        assert_eq!(row(across(ChromaSiting::default())), vec![0.0, 200.0, 400.0, 400.0], "left-sited");
        assert_eq!(row(across(centre)), vec![0.0, 100.0, 300.0, 400.0], "centre-sited");
        let top_left = ChromaSiting::from_av_location(3);
        assert_eq!(top_left, ChromaSiting { h: CoSited, v: CoSited });
        assert_eq!(col(down(ChromaSiting::default())), vec![0.0, 100.0, 300.0, 400.0], "left-sited, between the rows");
        assert_eq!(col(down(top_left)), vec![0.0, 200.0, 400.0, 400.0], "top-left-sited, on the even rows");
        assert_eq!(ChromaSiting::from_av_location(4), ChromaSiting { h: Between, v: CoSited }, "top");
        for untagged in [0, 1, 5, 7, -1] {
            let want = if untagged == 5 { ChromaSiting { h: CoSited, v: Between } } else { ChromaSiting::default() };
            assert_eq!(ChromaSiting::from_av_location(untagged), want, "{untagged}");
        }
        assert_eq!(ChromaSiting::from_av_location(6), centre, "bottom reads as centre");
        assert_eq!(ChromaSiting::default().shader_offsets(), [0.0, 0.25]);
        assert_eq!(centre.shader_offsets(), [0.25, 0.25]);
        assert_eq!(top_left.shader_offsets(), [0.0, 0.0]);
    }

    /// The reconstruction is the whole point. Nearest sampling repeats
    /// one chroma value across the block and leaves *nothing* between the
    /// two samples, so this fails the moment the interpolation is lost.
    #[test]
    fn chroma_is_reconstructed_not_repeated() {
        let y = flat_luma(4);
        let (u, v) = (vec![0u16, 400], vec![0u16, 400]);
        let s = YuvSamples {
            y: &y, u: &u, v: &v, width: 4, height: 1,
            chroma_shift_w: 1, chroma_shift_h: 0, bit_depth: 10, siting: Default::default(),
        };
        let vals: Vec<f32> = (0..4).map(|x| s.chroma_at(x, 0).0).collect();
        let between = vals.iter().filter(|c| **c > 1.0 && **c < 399.0).count();
        assert!(between > 0, "no value between the two chroma samples — still nearest: {vals:?}");
    }

    /// An unsubsampled axis reads its sample exactly: 4:4:4 has no blend
    /// to introduce, and 4:2:2 keeps every chroma row its own. Same
    /// arithmetic, no special case — which is what makes the shift of
    /// zero worth asserting rather than assuming.
    #[test]
    fn an_unsubsampled_axis_reads_exactly() {
        let y = flat_luma(4);
        let (u, v) = (vec![10u16, 20, 30, 40], vec![40u16, 30, 20, 10]);
        let s = YuvSamples {
            y: &y, u: &u, v: &v, width: 4, height: 1,
            chroma_shift_w: 0, chroma_shift_h: 0, bit_depth: 10, siting: Default::default(),
        };
        for x in 0..4 {
            assert_eq!(s.chroma_at(x, 0), (u[x] as f32, v[x] as f32), "x={x} must be exact");
        }
    }
}

#[cfg(test)]
mod deep_code_tests {
    use super::*;
    use klipp_core::color_ids as ids;

    const SLOG3: [u32; 4] = [ids::COLOR_RANGE_LIMITED, ids::MATRIX_BT709, ids::TRANSFER_SLOG3, 0];
    const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    /// A one-row ramp of `n` consecutive luma codes from `from` at
    /// `depth` bits, chroma neutral (4:4:4).
    fn ramp(from: u16, n: usize, depth: u32) -> (Vec<u16>, Vec<u16>) {
        let y: Vec<u16> = (0..n).map(|i| from + i as u16).collect();
        (y, vec![1u16 << (depth - 1); n])
    }

    fn samples<'a>(y: &'a [u16], c: &'a [u16], depth: u32) -> YuvSamples<'a> {
        YuvSamples { y, u: c, v: c, width: y.len() as u32, height: 1, chroma_shift_w: 0, chroma_shift_h: 0, bit_depth: depth , siting: Default::default()}
    }

    /// The 10-bit codes as they were made before #1226, kept verbatim:
    /// the reference a 10-bit source must still match exactly.
    fn codes_before_1226(s: &YuvSamples, color_ids: [u32; 4]) -> Vec<u32> {
        let [range, _, transfer, _] = color_ids;
        let is_log = transfer >= ids::FIRST_CAMERA_LOG;
        let max = ((1u32 << s.bit_depth) - 1) as f32;
        (0..s.width as usize)
            .map(|sx| {
                let (cu, cv) = s.chroma_at(sx, 0);
                let (y, u, v) = if range == ids::COLOR_RANGE_FULL {
                    (s.y[sx] as f32 / max, (cu - max * 0.5 - 0.5) / max, (cv - max * 0.5 - 0.5) / max)
                } else {
                    let scale = (max + 1.0) / 256.0;
                    ((s.y[sx] as f32 - 16.0 * scale) / (219.0 * scale), (cu - 128.0 * scale) / (224.0 * scale), (cv - 128.0 * scale) / (224.0 * scale))
                };
                let rgb = [y + 1.5748 * v, y - 0.1873 * u - 0.4681 * v, y + 1.8556 * u];
                let code = rgb.map(|c| {
                    let signal = if is_log && range != ids::COLOR_RANGE_FULL { (64.0 + c * 876.0) / 1023.0 } else { c };
                    (signal.clamp(0.0, 1.0) * 1023.0 + 0.5) as u32
                });
                pack_rgb10(code[0], code[1], code[2])
            })
            .collect()
    }

    /// #1226: an 8- or 10-bit source keeps its 10-bit codes, byte for
    /// byte, and the frame they make is the one they made.
    #[test]
    fn a_ten_bit_source_is_unchanged() {
        for depth in [8u32, 10] {
            let max = (1u16 << depth) - 1;
            let (y, c) = ramp(0, max as usize + 1, depth);
            let s = samples(&y, &c, depth);
            let codes = yuv_to_hdr_codes(&s, s.width, 1, 0, SLOG3);
            let want = codes_before_1226(&s, SLOG3);
            assert_eq!(codes, SceneCodes::Packed10(want.clone()), "{depth}-bit codes");
            let t = HdrTransfer::Log(klipp_core::ColorTransfer::SLog3);
            assert_eq!(codes.to_scene_linear_rgba(t, IDENTITY), hdr10_codes_to_scene_linear_rgba(&want, t, IDENTITY), "{depth}-bit frame");
        }
    }

    /// #1226: a 12- and a 16-bit gradient of 200 consecutive codes keep
    /// every step to scene-linear, each within its own precision of the
    /// curve at the exact signal; a three-stop push shows 200 distinct
    /// steps where 10-bit codes held a quarter of them or fewer.
    #[test]
    fn a_deep_source_keeps_its_steps() {
        let t = HdrTransfer::Log(klipp_core::ColorTransfer::SLog3);
        for depth in [12u32, 16] {
            let scale = (1u32 << (depth - 8)) as f32;
            // Around middle grey: S-Log3's 10-bit code 420, at this depth.
            let from = ((420.0 - 64.0) / 876.0 * 219.0 * scale + 16.0 * scale) as u16;
            let (y, c) = ramp(from, 200, depth);
            let s = samples(&y, &c, depth);
            let codes = yuv_to_hdr_codes(&s, s.width, 1, 0, SLOG3);
            assert!(matches!(codes, SceneCodes::Deep16(_)), "{depth}-bit: 16-bit codes");
            let linear = codes.to_scene_linear_rgba(t, IDENTITY);
            for (i, &code) in y.iter().enumerate() {
                let luma = (code as f32 - 16.0 * scale) / (219.0 * scale);
                let exact = hdr_code_to_scene_linear((64.0 + luma * 876.0) / 1023.0, t);
                let got = linear[i * 4 + 1];
                // Half a 16-bit code of signal, through S-Log3's slope
                // (~9 relative per unit of signal): 0.007 %; 10-bit
                // rounding is up to 0.44 %.
                assert!((got - exact).abs() <= exact.abs() * 2e-4, "{depth}-bit code {code}: {got}, the curve says {exact}");
            }
            let pushed: std::collections::BTreeSet<u32> = linear.chunks_exact(4).map(|p| (p[1] * 8.0).to_bits()).collect();
            assert_eq!(pushed.len(), 200, "{depth}-bit: every step survives a three-stop push");
            let ten: std::collections::BTreeSet<u32> = codes_before_1226(&s, SLOG3).into_iter().collect();
            assert!(ten.len() <= 200 / 4 + 1, "the 10-bit codes it used to get held {} steps", ten.len());
        }
    }

    /// The 16-bit table is made once a curve and handed out after.
    #[test]
    fn the_deep_table_is_made_once() {
        let t = HdrTransfer::Pq;
        let (a, b) = (hdr_code_lut16(t), hdr_code_lut16(t));
        assert!(std::sync::Arc::ptr_eq(&a, &b));
        assert_eq!(a.len(), 65_536);
        assert_eq!(unpack_rgb16(pack_rgb16(1, 40_000, 65_535)), [1, 40_000, 65_535]);
    }

    /// What a 4K frame costs to code and unpack, 10-bit against 16-bit:
    /// run with `--ignored --nocapture` to read it.
    #[test]
    #[ignore]
    fn deep_frame_cost_at_4k() {
        let (w, h) = (3840u32, 2160u32);
        let t = HdrTransfer::Log(klipp_core::ColorTransfer::SLog3);
        for depth in [10u32, 12] {
            let n = (w * h) as usize;
            let y: Vec<u16> = (0..n).map(|i| (i % (1 << depth)) as u16).collect();
            let c = vec![1u16 << (depth - 1); n / 2];
            let s = YuvSamples { y: &y, u: &c, v: &c, width: w, height: h, chroma_shift_w: 1, chroma_shift_h: 0, bit_depth: depth , siting: Default::default()};
            let _ = hdr_code_lut16(t);
            let mut best = (f64::MAX, f64::MAX);
            for _ in 0..5 {
                let t0 = std::time::Instant::now();
                let codes = yuv_to_hdr_codes(&s, w, h, 0, SLOG3);
                let t1 = std::time::Instant::now();
                let linear = codes.to_scene_linear_rgba(t, IDENTITY);
                let t2 = std::time::Instant::now();
                std::hint::black_box(linear);
                best = (best.0.min((t1 - t0).as_secs_f64() * 1e3), best.1.min((t2 - t1).as_secs_f64() * 1e3));
            }
            eprintln!("{depth}-bit 4K: codes {:.1} ms, unpack to scene-linear {:.1} ms", best.0, best.1);
        }
    }
}
