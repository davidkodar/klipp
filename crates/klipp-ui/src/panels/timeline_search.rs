//! Timeline find / search — pure predicates + hit-list builder.
//!
//! The egui surface lives in `panels/timeline.rs`. Pulling the match
//! logic into its own module mirrors `markers::marker_matches`: pure
//! functions that can be unit-tested without standing up a UI.
//!
//! ## Match space (v1)
//!
//! - Clip filename (resolved via media-pool lookup on `clip.media_id()`).
//!   Clip itself has no `name` / `notes` field, so the searchable text
//!   for a clip is whatever the media pool returns for its source.
//! - Sequence markers — label + comment + kind label.
//! - Per-clip markers — name + comment.
//!
//! Captions and cross-sequence search are deferred to v2.

use klipp_core::{ClipId, MarkerKind, Timecode};

/// A single match against the current search query. Built fresh each
/// frame from the active sequence — search results don't survive a
/// clip/marker edit, but that's fine because the cost of rebuilding is
/// O(n) over a typical sequence size.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub kind: SearchHitKind,
    /// Where to jump the playhead when this hit is selected.
    pub position: Timecode,
    /// Track row to scroll into view (None for sequence markers, which
    /// live on the ruler).
    pub track_idx: Option<usize>,
    /// Clip whose rect should pulse (the clip itself for clip hits,
    /// the host clip for clip-marker hits; None for sequence markers).
    pub clip_id: Option<ClipId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchHitKind {
    /// Clip filename matched.
    Clip,
    /// Sequence marker on the ruler matched.
    SequenceMarker,
    /// Per-clip marker matched (the clip itself doesn't have to match;
    /// just one of its markers).
    ClipMarker,
}

/// Pure predicate: does `text` contain `query` under the current case
/// policy? Empty query matches everything. Pulled out of the call sites
/// so case-folding happens in exactly one place.
pub fn text_matches(text: &str, query: &str, case_sensitive: bool) -> bool {
    if query.is_empty() {
        return true;
    }
    if case_sensitive {
        text.contains(query)
    } else {
        text.to_lowercase().contains(&query.to_lowercase())
    }
}

/// Pure predicate: does any of a clip's searchable text (just the
/// resolved media filename for now — clips have no `name` / `notes`
/// fields) match the query? Returns false for clips with no resolved
/// filename (e.g. an offline clip whose media isn't in the pool).
pub fn clip_filename_matches(filename: Option<&str>, query: &str, case_sensitive: bool) -> bool {
    if query.is_empty() {
        return true;
    }
    match filename {
        Some(f) => text_matches(f, query, case_sensitive),
        None => false,
    }
}

/// Pure predicate: does a sequence-level marker match? Mirrors
/// `markers::marker_matches` exactly but with the case-sensitivity
/// toggle threaded through.
pub fn sequence_marker_matches(
    label: &str,
    comment: &str,
    kind: MarkerKind,
    query: &str,
    case_sensitive: bool,
) -> bool {
    if query.is_empty() {
        return true;
    }
    let kind_label = kind.label();
    let kind_compact: String = kind_label.chars().filter(|c| !c.is_whitespace()).collect();
    text_matches(label, query, case_sensitive)
        || text_matches(comment, query, case_sensitive)
        || text_matches(kind_label, query, case_sensitive)
        || text_matches(&kind_compact, query, case_sensitive)
}

/// Pure predicate: does a per-clip marker (name + comment) match?
pub fn clip_marker_matches(name: &str, comment: &str, query: &str, case_sensitive: bool) -> bool {
    if query.is_empty() {
        return true;
    }
    text_matches(name, query, case_sensitive) || text_matches(comment, query, case_sensitive)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_everything() {
        assert!(text_matches("anything", "", false));
        assert!(text_matches("", "", false));
        assert!(clip_filename_matches(Some("clip.mp4"), "", false));
        assert!(clip_filename_matches(None, "", false));
        assert!(sequence_marker_matches("L", "C", MarkerKind::None, "", false));
        assert!(clip_marker_matches("n", "c", "", false));
    }

    #[test]
    fn case_insensitive_default() {
        assert!(text_matches("INTERVIEW_A.mp4", "interview", false));
        assert!(text_matches("interview_a.mp4", "INTERVIEW", false));
        assert!(text_matches("Mixed_Case", "MIxeD", false));
    }

    #[test]
    fn case_sensitive_when_flag_on() {
        assert!(text_matches("INTERVIEW", "INTERVIEW", true));
        assert!(!text_matches("INTERVIEW", "interview", true));
        assert!(!text_matches("interview", "INTERVIEW", true));
    }

    #[test]
    fn clip_filename_with_no_media_never_matches_nonempty_query() {
        assert!(!clip_filename_matches(None, "anything", false));
        assert!(!clip_filename_matches(None, "x", true));
    }

    #[test]
    fn sequence_marker_label_comment_kind_all_searchable() {
        // Label hit.
        assert!(sequence_marker_matches("Cut on action", "", MarkerKind::None, "cut", false));
        // Comment hit, label and kind don't help.
        assert!(sequence_marker_matches(
            "M3",
            "trim two frames off",
            MarkerKind::None,
            "trim",
            false
        ));
        // Kind label hit ("Chapter").
        assert!(sequence_marker_matches("M1", "", MarkerKind::Chapter, "chapter", false));
        // Kind compact form ("ToDo" → "todo" stays as one word — no
        // compaction needed; use "Best Take" → "BestTake" instead).
        assert!(sequence_marker_matches("M4", "", MarkerKind::BestTake, "besttake", false));
        // None of the fields contain "zzz" — must not match.
        assert!(!sequence_marker_matches("Cut", "trim", MarkerKind::Chapter, "zzz", false));
    }

    #[test]
    fn clip_marker_name_and_comment_searchable() {
        assert!(clip_marker_matches("intro", "", "intro", false));
        assert!(clip_marker_matches("", "intro applause", "applause", false));
        assert!(!clip_marker_matches("intro", "applause", "zzz", false));
    }

    #[test]
    fn multi_word_substring() {
        // Token-spanning substring works — "intro app" is contiguous.
        assert!(clip_marker_matches("intro applause", "", "intro app", false));
        // Non-contiguous "intro xxx app" must NOT match (no fuzzy match
        // in v1 — that's a v2 question if anyone asks).
        assert!(!clip_marker_matches("intro applause", "", "intro xxx app", false));
    }

    #[test]
    fn clip_filename_extension_searchable() {
        assert!(clip_filename_matches(Some("interview_a.mp4"), ".mp4", false));
        assert!(clip_filename_matches(Some("shot.mov"), "mov", false));
        // Case-insensitive on extension too.
        assert!(clip_filename_matches(Some("shot.MOV"), "mov", false));
    }

    #[test]
    fn search_hit_eq_for_test_assertions() {
        // Verifies the derive lands — used by integration tests that
        // assert specific hit shapes.
        let h1 = SearchHit {
            kind: SearchHitKind::Clip,
            position: Timecode::zero(),
            track_idx: Some(0),
            clip_id: None,
        };
        let h2 = h1.clone();
        assert_eq!(h1, h2);
    }
}
