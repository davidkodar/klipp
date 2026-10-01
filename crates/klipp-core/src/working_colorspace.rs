//! The project-wide *working colorspace* — the linear-light RGB basis
//! every grade node operates against.
//!
//! This is distinct from [`crate::colorspace::SourceColorspace`]: that one
//! describes how a *decoded video frame* is encoded (range / matrix /
//! primaries / transfer) and is read off the file's metadata. The
//! working colorspace is a **creative decision** about which linear RGB
//! basis the grade chain runs in — it determines what gamut the grade
//! has access to and what the RRT/ODT chain expects on input.
//!
//! In every professional NLE the working colorspace is a per-project
//! setting, not a user preference: it's part of the project's creative
//! output and must travel with the project file. Two editors on the same
//! `.klipp` must see the same colors.
//!
//! Default is [`WorkingColorspace::Rec709Linear`] — matches the pipeline's behaviour
//! before Phase E and means existing projects deserialize unchanged via
//! `#[serde(default)]`.
//!
//! Wide-gamut / HDR delivery uses [`WorkingColorspace::AcesCG`] (linear ACES AP1) —
//! the industry-standard interchange space for ACES workflows.
//! [`WorkingColorspace::Bt2020Linear`] is exposed for HDR-only pipelines that don't
//! want the ACES tone curve, but the well-tested path is ACEScg.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum WorkingColorspace {
    /// Linear-light, BT.709 / sRGB primaries, D65 white. The pipeline's
    /// long-standing behaviour and the back-compat default for any
    /// project file written before the working-space upgrade.
    #[default]
    Rec709Linear,
    /// Linear-light, ACES AP1 primaries, D60 white. The ACES working
    /// space used by Resolve, Baselight, Nuke. New projects default
    /// here once the full ACES pipeline is wired through.
    AcesCG,
    /// Linear-light, BT.2020 primaries, D65 white. HDR-only delivery
    /// pipelines that want a wider gamut than BT.709 without the ACES
    /// RRT tone curve. Less common — use only when explicitly asked
    /// for; ACEScg is the well-tested path.
    Bt2020Linear,
}

impl WorkingColorspace {
    /// Wire-stable u8 tag — used by callers that need to pack the
    /// enum into a `u8`-sized atomic, an FFI integer, or a fixed-width
    /// project-file field. Discriminants are pinned so a future
    /// reorder of the enum can't silently change the meaning of an
    /// already-stored value.
    #[inline]
    pub const fn as_u8(self) -> u8 {
        match self {
            WorkingColorspace::Rec709Linear => 0,
            WorkingColorspace::AcesCG => 1,
            WorkingColorspace::Bt2020Linear => 2,
        }
    }

    /// Inverse of [`Self::as_u8`]. Unknown tags fall back to
    /// [`Self::Rec709Linear`] — the safe default that matches
    /// pre-upgrade behaviour, so a corrupted atomic never produces
    /// a wider-gamut surprise.
    #[inline]
    pub const fn from_u8(tag: u8) -> Self {
        match tag {
            1 => WorkingColorspace::AcesCG,
            2 => WorkingColorspace::Bt2020Linear,
            _ => WorkingColorspace::Rec709Linear,
        }
    }
}

/// What the grade's controls act in, chosen per project (#467): the
/// BT.1886 power curve every project has graded in, or under an ACEScg
/// working space ACEScct or ACEScc, the ACES log encodings colourists
/// grade in, where offset is an exposure and the wheels move evenly
/// across the stops. Only ACEScg takes the ACES ones; in another working
/// space the grade is BT.1886 whatever is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GradeEncoding {
    #[default]
    Bt1886,
    AcesCct,
    AcesCc,
}

impl GradeEncoding {
    /// The wire tag, as `klipp_grade_model::Encoding::from_u8` reads it.
    pub const fn as_u8(self) -> u8 {
        match self {
            GradeEncoding::Bt1886 => 0,
            GradeEncoding::AcesCct => 1,
            GradeEncoding::AcesCc => 2,
        }
    }

    pub const fn from_u8(tag: u8) -> Self {
        match tag {
            1 => GradeEncoding::AcesCct,
            2 => GradeEncoding::AcesCc,
            _ => GradeEncoding::Bt1886,
        }
    }

    /// What the grade acts in, in the working space `ws`: this only
    /// under ACEScg, BT.1886 otherwise.
    pub const fn effective(self, ws: WorkingColorspace) -> GradeEncoding {
        match ws {
            WorkingColorspace::AcesCG => self,
            _ => GradeEncoding::Bt1886,
        }
    }
}

static GRADE_ENCODING: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// The project's chosen grade encoding, set when a project loads or the
/// choice changes; the grade executors read it with the working space.
pub fn set_grade_encoding(enc: GradeEncoding) {
    GRADE_ENCODING.store(enc.as_u8(), std::sync::atomic::Ordering::Relaxed);
}

/// The project's chosen grade encoding (see [`GradeEncoding::effective`]
/// for what applies).
pub fn grade_encoding() -> GradeEncoding {
    GradeEncoding::from_u8(GRADE_ENCODING.load(std::sync::atomic::Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #467: the ACES encodings only under ACEScg.
    #[test]
    fn the_aces_grade_encodings_need_acescg() {
        assert_eq!(GradeEncoding::AcesCct.effective(WorkingColorspace::AcesCG), GradeEncoding::AcesCct);
        assert_eq!(GradeEncoding::AcesCct.effective(WorkingColorspace::Rec709Linear), GradeEncoding::Bt1886);
        assert_eq!(GradeEncoding::AcesCc.effective(WorkingColorspace::Bt2020Linear), GradeEncoding::Bt1886);
        for e in [GradeEncoding::Bt1886, GradeEncoding::AcesCct, GradeEncoding::AcesCc] {
            assert_eq!(GradeEncoding::from_u8(e.as_u8()), e);
        }
    }

    #[test]
    fn default_is_rec709_linear() {
        assert_eq!(WorkingColorspace::default(), WorkingColorspace::Rec709Linear);
    }

    #[test]
    fn variants_distinct() {
        // Cheap guard against an accidental enum dedupe via macro.
        assert_ne!(WorkingColorspace::Rec709Linear, WorkingColorspace::AcesCG);
        assert_ne!(WorkingColorspace::AcesCG, WorkingColorspace::Bt2020Linear);
        assert_ne!(WorkingColorspace::Rec709Linear, WorkingColorspace::Bt2020Linear);
    }

    #[test]
    fn u8_round_trip() {
        for ws in [
            WorkingColorspace::Rec709Linear,
            WorkingColorspace::AcesCG,
            WorkingColorspace::Bt2020Linear,
        ] {
            assert_eq!(WorkingColorspace::from_u8(ws.as_u8()), ws);
        }
    }

    #[test]
    fn u8_tags_are_stable() {
        // Pin the wire tags. Reordering the enum or inserting a new
        // variant must not silently change these — atomic stores and
        // FFI consumers depend on them.
        assert_eq!(WorkingColorspace::Rec709Linear.as_u8(), 0);
        assert_eq!(WorkingColorspace::AcesCG.as_u8(), 1);
        assert_eq!(WorkingColorspace::Bt2020Linear.as_u8(), 2);
    }

    #[test]
    fn unknown_u8_falls_back_to_rec709_linear() {
        // A corrupted atomic or FFI value must not produce a
        // wider-gamut surprise — the safe fallback is the default.
        for tag in [3u8, 7, 42, 255] {
            assert_eq!(WorkingColorspace::from_u8(tag), WorkingColorspace::Rec709Linear);
        }
    }

    // The serde-tagged JSON round-trip + back-compat tests live in
    // `klipp-project::format::tests` — that crate already has
    // `serde_json` as a dep (the .klipp file format itself is JSON), and
    // we keep `klipp-core` free of `serde_json` per the workspace
    // dependency policy laid out in commit 3e2ca08.
}
