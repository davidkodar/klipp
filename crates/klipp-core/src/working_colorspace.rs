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
//! Default is [`Self::Rec709Linear`] — matches the pipeline's behaviour
//! before Phase E and means existing projects deserialize unchanged via
//! `#[serde(default)]`.
//!
//! Wide-gamut / HDR delivery uses [`Self::AcesCG`] (linear ACES AP1) —
//! the industry-standard interchange space for ACES workflows.
//! [`Self::Bt2020Linear`] is exposed for HDR-only pipelines that don't
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

#[cfg(test)]
mod tests {
    use super::*;

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
