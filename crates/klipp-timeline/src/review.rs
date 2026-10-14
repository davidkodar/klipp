//! What was decided about a finding of the check (#1932): kept on
//! purpose, with the reason, or to be fixed. Saved with the sequence, so
//! a finding reviewed once is not argued again on every check, and so a
//! delivery report can say what was accepted and why.
//!
//! A review names its finding by the finding's id, which the check makes
//! from what the finding judged: change that (the clip's media, its
//! range, its grade; the words of a craft finding) and the finding comes
//! back under a new id, unreviewed, and the review no longer matches.

use serde::{Deserialize, Serialize};

/// What was decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    /// Kept on purpose: the finding stays visible in the report, apart
    /// from the faults, with its reason, and is not counted as one.
    Accepted,
    /// Seen, and to be fixed: still counted, with the note beside it.
    ToFix,
}

/// One reviewed finding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindingReview {
    /// The id the check gave the finding.
    pub finding_id: String,
    pub decision: ReviewDecision,
    /// Why: what makes an accepted finding deliberate, or what the fix is.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rationale: String,
    /// Where on the timeline the finding was when it was reviewed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_secs: Option<f64>,
    /// The clip it was about, for a finding about one clip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_id: Option<klipp_core::ClipId>,
    /// The project's revision when it was reviewed.
    #[serde(default)]
    pub revision: u64,
}
