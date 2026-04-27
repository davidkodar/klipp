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
    /// behaviour through Phase D and the back-compat default for any
    /// project file written before Phase E.
    #[default]
    Rec709Linear,
    /// Linear-light, ACES AP1 primaries, D60 white. The ACES working
    /// space used by Resolve, Baselight, Nuke. New projects default
    /// here once the Phase E pipeline is wired through (Sessions 2-7).
    AcesCG,
    /// Linear-light, BT.2020 primaries, D65 white. HDR-only delivery
    /// pipelines that want a wider gamut than BT.709 without the ACES
    /// RRT tone curve. Less common — use only when explicitly asked
    /// for; ACEScg is the well-tested path.
    Bt2020Linear,
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

    // The serde-tagged JSON round-trip + back-compat tests live in
    // `klipp-project::format::tests` — that crate already has
    // `serde_json` as a dep (the .klipp file format itself is JSON), and
    // we keep `klipp-core` free of `serde_json` per the workspace
    // dependency policy laid out in commit 3e2ca08.
}
