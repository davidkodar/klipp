//! Reusable timecode display + inline edit widget.
//!
//! Shows a timecode as a clickable label. On click, switches to a
//! `TextEdit::singleline`. On Enter / focus-lost, parses the buffer
//! and returns the new time (or None if invalid or unchanged).
//! Escape cancels.
//!
//! Mirrors the inlined pattern from `panels/transport.rs:135-181` but
//! extracted so per-row subtitle editing (and any future per-row
//! timecode field) can reuse it.

use klipp_core::RationalTime;
#[cfg(test)]
use klipp_core::Fps;

use crate::panels::transport::parse_timecode_input_pub;

/// Display a timecode as a clickable label with inline edit support.
///
/// `editing` tracks which timecode field across the whole panel is currently
/// being edited — this allows multiple `timecode_edit` calls on the same page
/// without them fighting each other. When `*editing == Some(id)`, this
/// instance renders a TextEdit; otherwise it renders a label.
///
/// Returns `Some(new_time)` only when the user committed a valid change.
/// Returns `None` in all other cases (no interaction, editing in progress,
/// invalid input, unchanged value, Escape pressed).
pub fn timecode_edit(
    ui: &mut egui::Ui,
    id: egui::Id,
    current: RationalTime,
    tc: klipp_core::TimecodeDisplay,
    editing: &mut Option<egui::Id>,
    edit_buf: &mut String,
) -> Option<RationalTime> {
    if *editing == Some(id) {
        // Edit mode
        let response = ui.add(
            egui::TextEdit::singleline(edit_buf)
                .desired_width(96.0)
                .font(egui::TextStyle::Monospace),
        );
        if !response.has_focus() {
            response.request_focus();
        }

        // Escape cancels without committing
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            *editing = None;
            return None;
        }

        // Enter or focus-lost commits
        if response.lost_focus() {
            let confirmed = ui.input(|i| i.key_pressed(egui::Key::Enter));
            *editing = None;
            if confirmed {
                if let Some(parsed) = parse_timecode_input_pub(edit_buf, tc) {
                    if parsed != current {
                        return Some(parsed);
                    }
                }
            }
        }
        None
    } else {
        // Display mode
        let tc_str = tc.position(current);
        let resp = ui.add(
            egui::Label::new(
                egui::RichText::new(&tc_str)
                    .monospace()
                    .size(12.0),
            )
            .sense(egui::Sense::click()),
        );
        if resp.clicked() {
            *editing = Some(id);
            *edit_buf = tc_str;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fps_24() -> Fps {
        Fps::film_24()
    }

    #[test]
    fn parses_valid_hmsf_input() {
        let parsed = parse_timecode_input_pub("00:00:02:12", klipp_core::TimecodeDisplay::plain(fps_24()));
        assert!(parsed.is_some());
        let tc = parsed.unwrap();
        // 2 seconds + 12 frames at 24fps = 60 frames
        assert_eq!(tc.rate, 24);
        assert_eq!(tc.value, 60);
    }

    #[test]
    fn rejects_invalid_input() {
        let parsed = parse_timecode_input_pub("not a timecode", klipp_core::TimecodeDisplay::plain(fps_24()));
        assert!(parsed.is_none());
    }

    #[test]
    fn parses_plain_frame_number() {
        let parsed = parse_timecode_input_pub("48", klipp_core::TimecodeDisplay::plain(fps_24()));
        assert!(parsed.is_some());
        let tc = parsed.unwrap();
        assert_eq!(tc.value, 48);
    }

    #[test]
    fn parses_decimal_seconds() {
        let parsed = parse_timecode_input_pub("2.5", klipp_core::TimecodeDisplay::plain(fps_24()));
        assert!(parsed.is_some());
        let tc = parsed.unwrap();
        // 2.5 seconds at 24fps = 60 frames
        assert_eq!(tc.value, 60);
    }
}
