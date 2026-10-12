//! YouTube chapter lists (#890); subtitles and captions are read and written by `klipp-captions`.

#[allow(unused_imports)]
use super::*;
pub(super) use klipp_captions::*;

/// Format an absolute time for YouTube chapter lines. YouTube accepts both
/// `M:SS` and `H:MM:SS`; we always emit `H:MM:SS` for consistency regardless
/// of sequence length. Negative inputs are clamped to zero.
pub(super) fn format_youtube_chapter_time(total_secs: f64) -> String {
    let secs = total_secs.max(0.0) as u64;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{}:{:02}:{:02}", h, m, s)
}

/// Render a sorted `(position_secs, label)` list as YouTube chapter text.
/// Blank labels are substituted with `Chapter N` (1-indexed across all
/// entries, including any synthetic "Intro" the caller prepends). No trailing
/// newline — callers choose whether to append one.
pub(super) fn generate_youtube_chapters(entries: &[(f64, String)]) -> String {
    entries
        .iter()
        .enumerate()
        .map(|(i, (secs, label))| {
            let tc = format_youtube_chapter_time(*secs);
            let display_label = if label.trim().is_empty() {
                format!("Chapter {}", i + 1)
            } else {
                label.clone()
            };
            format!("{} {}", tc, display_label)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod youtube_chapter_tests {
    use super::*;

    #[test]
    fn format_youtube_chapter_time_under_one_hour() {
        assert_eq!(format_youtube_chapter_time(0.0),   "0:00:00");
        assert_eq!(format_youtube_chapter_time(83.0),  "0:01:23");
        assert_eq!(format_youtube_chapter_time(287.0), "0:04:47");
    }

    #[test]
    fn format_youtube_chapter_time_over_one_hour() {
        assert_eq!(format_youtube_chapter_time(3661.0), "1:01:01");
    }

    #[test]
    fn generate_youtube_chapters_blank_label_substituted() {
        let entries = vec![
            (0.0, "Intro".to_string()),
            (83.0, "".to_string()),
        ];
        let out = generate_youtube_chapters(&entries);
        assert!(out.contains("Chapter 2"), "expected blank label → 'Chapter 2' in: {out}");
        // "Intro" is not blank, so "Chapter 1" should NOT appear.
        assert!(!out.contains("Chapter 1"), "unexpected 'Chapter 1' in: {out}");
    }

    #[test]
    fn generate_youtube_chapters_format() {
        let entries = vec![
            (0.0, "Intro".to_string()),
            (83.0, "Main Content".to_string()),
            (287.0, "Outro".to_string()),
        ];
        let expected = "0:00:00 Intro\n0:01:23 Main Content\n0:04:47 Outro";
        assert_eq!(generate_youtube_chapters(&entries), expected);
    }

}
