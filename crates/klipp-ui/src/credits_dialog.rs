//! Rolling credits from a list (#1514): paste or load a CSV or
//! tab-separated list (one field a section heading, a role then its names
//! an entry), choose columns or centred, and a duration the credits fit
//! or a speed they scroll at; the title goes on a free text track at the
//! playhead.

use klipp_core::AppCommand;
use klipp_timeline::{CreditsLayout, CreditsStyle};

/// The open credits dialog.
#[derive(Debug, Clone)]
pub struct CreditsDialog {
    pub text: String,
    pub layout: CreditsLayout,
    /// Fit `secs` (true), or scroll at `speed` px/s for `secs`.
    pub fit: bool,
    pub secs: f32,
    pub speed: f32,
}

impl Default for CreditsDialog {
    fn default() -> Self {
        Self { text: String::new(), layout: CreditsLayout::Columns, fit: true, secs: 30.0, speed: 60.0 }
    }
}

impl CreditsDialog {
    /// The credits as a title, and how long its clip runs.
    pub fn title(&self) -> (klipp_timeline::TitleData, f32) {
        let style = CreditsStyle { layout: self.layout, speed_px_per_sec: if self.fit { 0.0 } else { self.speed.max(1.0) }, ..CreditsStyle::default() };
        (klipp_timeline::credits_title(&klipp_timeline::parse_credits(&self.text), &style), self.secs.max(1.0))
    }
}

/// The first text track free from `from` to `to`, or `None` when no text
/// track has room there.
pub fn free_text_track(seq: &klipp_timeline::Sequence, from: klipp_core::Timecode, to: klipp_core::Timecode) -> Option<usize> {
    seq.tracks
        .iter()
        .position(|t| t.kind == klipp_timeline::TrackKind::Text && !t.locked && !t.clips.iter().any(|c| c.timeline_in < to && c.timeline_out > from))
}

/// Put the dialog's credits on the timeline at the playhead: on a text
/// track with room, else on a new one on top. The commands that place it.
pub fn place(state: &mut crate::state::AppState, dialog: &CreditsDialog) -> Vec<AppCommand> {
    let (td, secs) = dialog.title();
    let seq_idx = state.active_sequence_idx;
    let fps_rate = state.sequences[seq_idx].fps.rate();
    let duration_frames = (secs * fps_rate as f32).round().max(1.0) as u32;
    let from = state.playhead;
    let to = from + klipp_core::RationalTime::from_frames(i64::from(duration_frames), fps_rate);
    let track_index = match free_text_track(&state.sequences[seq_idx], from, to) {
        Some(i) => i,
        None => {
            let n = state.sequences[seq_idx].tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Text).count() + 1;
            state.sequences[seq_idx].tracks.insert(0, klipp_timeline::Track::new(&format!("Text {n}"), klipp_timeline::TrackKind::Text));
            0
        }
    };
    state.pending_title_data = Some(td);
    vec![AppCommand::CreateTitleClip { track_index, timeline_in: from, duration_frames }]
}

/// Draw the open dialog, if any; the commands its Create sends.
pub fn show_dialog(ctx: &egui::Context, state: &mut crate::state::AppState) -> Vec<AppCommand> {
    let Some(mut dialog) = state.credits_dialog.take() else { return Vec::new() };
    let mut open = true;
    let mut create = false;
    let mut cancel = false;
    egui::Window::new("Credits from a List")
        .collapsible(false)
        .resizable(true)
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.set_min_width(420.0);
            ui.label(egui::RichText::new("One line each: a heading alone; a role, then its names, separated by commas or tabs. A blank line leaves a gap.").small().weak());
            ui.horizontal(|ui| {
                if ui.button("Load CSV\u{2026}").clicked() {
                    crate::file_dialog::spawn(|| {
                        rfd::FileDialog::new()
                            .set_title("Load Credits")
                            .add_filter("Credits list", &["csv", "tsv", "txt"])
                            .pick_file()
                            .map(|path| vec![AppCommand::LoadCreditsList { path }])
                            .unwrap_or_default()
                    });
                }
                let lines = klipp_timeline::parse_credits(&dialog.text).len();
                ui.label(egui::RichText::new(format!("{lines} lines")).small().weak());
            });
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                ui.add(egui::TextEdit::multiline(&mut dialog.text).desired_rows(10).desired_width(f32::INFINITY).hint_text("CAST\nJane Doe,Mara\n\nCREW\nDirector,Ana Ruiz"));
            });
            ui.horizontal(|ui| {
                ui.label("Layout");
                ui.selectable_value(&mut dialog.layout, CreditsLayout::Columns, "Two columns");
                ui.selectable_value(&mut dialog.layout, CreditsLayout::Centred, "Centred");
            });
            ui.horizontal(|ui| {
                ui.radio_value(&mut dialog.fit, true, "Fit");
                ui.radio_value(&mut dialog.fit, false, "Scroll at");
                ui.add_enabled(!dialog.fit, egui::DragValue::new(&mut dialog.speed).range(1.0..=1000.0).suffix(" px/s"));
                ui.label("for");
                ui.add(egui::DragValue::new(&mut dialog.secs).range(1.0..=3600.0).suffix(" s"));
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                cancel = ui.button("Cancel").clicked();
                create = ui.add_enabled(!dialog.text.trim().is_empty(), egui::Button::new("Create")).clicked();
            });
        });
    if create {
        let cmds = place(state, &dialog);
        state.toast.show("Credits added at the playhead");
        return cmds;
    }
    if open && !cancel {
        state.credits_dialog = Some(dialog);
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 200-line list as a CSV: twenty sections of ten roles, a gap before
    /// each heading.
    fn csv() -> String {
        let mut out = String::new();
        for s in 0..20 {
            out.push_str(&format!("\nSECTION {s}\n"));
            for r in 0..9 {
                let n = s * 10 + r;
                out.push_str(&format!("Role {n},Name {n}\n"));
            }
        }
        out
    }

    /// The top of each run of inked rows between columns `x0..x1`.
    fn row_tops(buf: &[u8], w: usize, h: usize, x0: usize, x1: usize) -> Vec<usize> {
        let inked = |y: usize| (x0..x1).any(|x| buf[(y * w + x) * 4 + 3] > 40);
        (0..h).filter(|&y| inked(y) && (y == 0 || !inked(y - 1))).collect()
    }

    /// A 200-line credits list, rendered as it scrolls: at every moment
    /// the role and name columns' rows start at the same heights, and the
    /// credits are below the frame at the start and above it at the end
    /// of the duration chosen, which they fit (#1514).
    #[test]
    fn two_hundred_lines_scroll_with_their_columns_aligned_over_the_duration() {
        let dialog = CreditsDialog { text: csv(), ..Default::default() };
        let (td, secs) = dialog.title();
        assert_eq!(klipp_timeline::parse_credits(&dialog.text).len(), 219, "200 lines and their gaps");
        let (w, h) = (640usize, 360usize);
        let mut renderer = klipp_title::TitleRenderer::new();
        let mut aligned = 0;
        for k in 1..10 {
            let t = secs * k as f32 / 10.0;
            let buf = renderer.render_with_window(&td, w as u32, h as u32, None, t, Some(secs));
            let (roles, names) = (row_tops(&buf, w, h, 0, w * 46 / 100), row_tops(&buf, w, h, w * 54 / 100, w));
            assert!(!roles.is_empty(), "t {t:.1}: credits in frame");
            assert_eq!(roles.len(), names.len(), "t {t:.1}: as many rows each side: {roles:?} {names:?}");
            for (r, n) in roles.iter().zip(&names) {
                assert!(r.abs_diff(*n) <= 1, "t {t:.1}: a role row at {r}, its name at {n}: {roles:?} {names:?}");
            }
            aligned += roles.len();
        }
        assert!(aligned > 50, "rows compared: {aligned}");
        for t in [0.0, secs] {
            let buf = renderer.render_with_window(&td, w as u32, h as u32, None, t, Some(secs));
            assert!(buf.chunks_exact(4).all(|p| p[3] == 0), "t {t}: out of frame");
        }
    }

    /// A text track with room for the whole span is used; one with a clip
    /// in it isn't, nor a locked one.
    #[test]
    fn credits_go_on_a_text_track_with_room_for_all_of_them() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut busy = klipp_timeline::Track::new("Text 1", klipp_timeline::TrackKind::Text);
        busy.clips.push(klipp_timeline::Clip::gap(t(20.0), t(22.0)));
        seq.tracks.insert(0, busy);
        assert_eq!(free_text_track(&seq, t(0.0), t(30.0)), None, "a clip inside the span");
        assert_eq!(free_text_track(&seq, t(0.0), t(10.0)), Some(0), "room before it");
        seq.tracks[0].locked = true;
        assert_eq!(free_text_track(&seq, t(0.0), t(10.0)), None, "locked");
    }
}
