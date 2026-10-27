//! The Make Shorts dialog (#2720): a long talk, podcast, stream or vlog's
//! moments that stand on their own as short vertical clips, found from
//! its transcript, reviewed, then built, each a sequence of its own.
//!
//! Find runs `klipp_analysis::shorts::find` on the file's transcript;
//! the list shows each moment's place, length, score, hook and why, each
//! kept or dropped; Build sends, for each kept one, a new 1080x1920
//! sequence and [`AppCommand::FillShort`] to fill it.

use klipp_analysis::shorts::{Ask, ShortMoment};
use klipp_core::{AppCommand, MediaId};

/// The open Make Shorts dialog.
#[derive(Debug, Clone)]
pub struct ShortsDialog {
    pub media_id: MediaId,
    pub filename: String,
    /// Seconds aimed for, and how many to find.
    pub length: f64,
    pub count: usize,
    /// Pauses and filler taken out (the platforms' norm).
    pub tighten: bool,
    /// The hook as a title over the opening.
    pub hook_title: bool,
    /// What Find found, each kept or not; or why it found nothing.
    pub found: Option<Result<Vec<(ShortMoment, bool)>, String>>,
}

impl ShortsDialog {
    pub fn new(media_id: MediaId, filename: String) -> Self {
        Self { media_id, filename, length: 30.0, count: 5, tighten: true, hook_title: false, found: None }
    }

    /// Find the moments in `words`, the file's transcript: `None` when it
    /// has none yet.
    pub fn find(&mut self, words: Option<&[klipp_analysis::transcript::Word]>) {
        self.found = Some(match words {
            None => Err("This file has no transcript yet: analyze it (or Transcribe Clip), then find again.".into()),
            Some(words) => {
                let ask = Ask { target_secs: self.length, min_secs: (self.length * 0.5).max(8.0), max_secs: (self.length * 2.0).min(90.0), count: self.count, ..Ask::default() };
                let found = klipp_analysis::shorts::find(words, &ask);
                if found.is_empty() {
                    Err(format!("No moment of {:.0} to {:.0} seconds stands on its own here.", ask.min_secs, ask.max_secs))
                } else {
                    Ok(found.into_iter().map(|m| (m, true)).collect())
                }
            }
        });
    }

    /// The commands that build the kept moments, each a sequence of its
    /// own named after its hook, at `fps`, its pieces cut from `words`.
    pub fn build(&self, words: &[klipp_document::ops::SpokenWord], fps: klipp_core::Fps) -> Vec<AppCommand> {
        let Some(Ok(found)) = &self.found else { return Vec::new() };
        let mut cmds = Vec::new();
        for (k, (m, _)) in found.iter().filter(|(_, keep)| *keep).enumerate() {
            let pieces = klipp_document::shorts::pieces(words, m.from_secs, m.to_secs, self.tighten);
            if pieces.is_empty() {
                continue;
            }
            let hook: String = m.hook.chars().take(40).collect();
            cmds.push(AppCommand::AddSequenceAndOpen { name: format!("Short {}: {}", k + 1, hook.trim()), fps_num: fps.num, fps_den: fps.den, width: 1080, height: 1920 });
            cmds.push(AppCommand::FillShort { media_id: self.media_id, pieces, hook: self.hook_title.then(|| m.hook.clone()) });
        }
        cmds
    }

    fn kept(&self) -> usize {
        match &self.found {
            Some(Ok(found)) => found.iter().filter(|(_, keep)| *keep).count(),
            _ => 0,
        }
    }
}

/// Draw the open dialog, if any; the commands its Build sends.
pub fn show_dialog(ctx: &egui::Context, state: &mut crate::state::AppState) -> Vec<AppCommand> {
    let Some(mut dialog) = state.shorts_dialog.take() else { return Vec::new() };
    let mut open = true;
    let (mut find, mut build, mut cancel) = (false, false, false);
    egui::Window::new("Make Shorts")
        .collapsible(false)
        .resizable(true)
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.set_min_width(520.0);
            ui.label(egui::RichText::new(format!("{}: the moments that stand on their own as vertical shorts, from what is said. Each kept one becomes a 1080x1920 sequence, framed on the speaker and captioned.", dialog.filename)).small().weak());
            ui.add_space(4.0);
            egui::Grid::new("shorts_grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Length");
                ui.add(egui::DragValue::new(&mut dialog.length).range(10.0..=90.0).speed(1.0).fixed_decimals(0).suffix(" s"));
                ui.end_row();
                ui.label("How many");
                ui.add(egui::DragValue::new(&mut dialog.count).range(1..=20));
                ui.end_row();
                ui.label("Cut");
                ui.checkbox(&mut dialog.tighten, "Take out pauses and filler").on_hover_text("Each jump is covered by pushing every other piece in a little");
                ui.end_row();
                ui.label("Hook");
                ui.checkbox(&mut dialog.hook_title, "Its first line as a title over the opening");
                ui.end_row();
            });
            ui.add_space(4.0);
            find = ui.button("Find Moments").clicked();
            match &mut dialog.found {
                Some(Err(why)) => {
                    ui.label(egui::RichText::new(why.as_str()).weak());
                }
                Some(Ok(found)) => {
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        for (m, keep) in found.iter_mut() {
                            ui.horizontal(|ui| {
                                ui.checkbox(keep, "");
                                ui.vertical(|ui| {
                                    ui.label(egui::RichText::new(format!("\u{201C}{}\u{201D}", m.hook)).strong());
                                    let why = if m.reasons.is_empty() { String::new() } else { format!(" \u{2014} {}", m.reasons.join(", ")) };
                                    ui.label(egui::RichText::new(format!("{:.1}\u{2013}{:.1} s ({:.0} s), score {:.2}{why}", m.from_secs, m.to_secs, m.to_secs - m.from_secs, m.score)).small().weak());
                                });
                            });
                            ui.separator();
                        }
                    });
                }
                None => {}
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                cancel = ui.button("Cancel").clicked();
                let n = dialog.kept();
                build = ui.add_enabled(n > 0, egui::Button::new(format!("Build {n} Short{}", if n == 1 { "" } else { "s" }))).clicked();
            });
        });
    let media_id = dialog.media_id;
    let words = || {
        let path = state.media_pool.get(&media_id).map(|m| m.path.clone())?;
        klipp_analysis::report::MediaAnalysis::load(&path).transcript.map(|t| t.words)
    };
    if find {
        let words = words();
        dialog.find(words.as_deref());
    }
    if build {
        let spoken: Vec<klipp_document::ops::SpokenWord> = words().unwrap_or_default().into_iter().map(|w| klipp_document::ops::SpokenWord { start_secs: w.start_secs, end_secs: w.end_secs, text: w.text, confidence: w.confidence }).collect();
        let fps = state.media_pool.get(&media_id).map_or(state.sequence().fps, |m| m.fps);
        return dialog.build(&spoken, fps);
    }
    if open && !cancel {
        state.shorts_dialog = Some(dialog);
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(lines: &[(&str, f64)]) -> Vec<klipp_analysis::transcript::Word> {
        let mut t = 0.0;
        let mut out = Vec::new();
        for (line, gap) in lines {
            t += gap;
            for w in line.split_whitespace() {
                out.push(klipp_analysis::transcript::Word { start_secs: t, end_secs: t + 0.3, text: w.into(), confidence: 0.9, corrected: false });
                t += 0.33;
            }
        }
        out
    }

    /// #2720: Find lists the moments, all kept; Build makes a sequence and
    /// fills it for each kept one, a dropped one left out; with no
    /// transcript, Find says to analyze first.
    #[test]
    fn find_lists_the_moments_and_build_makes_the_kept_ones() {
        let words = said(&[
            ("Why do most startups fail in their first year?", 1.0),
            ("It is not the money and it is not the market and it is not the idea either.", 0.2),
            ("We looked at two hundred companies over ten years and the same thing kept coming up.", 0.2),
            ("But the founders who made it all did one thing differently from the rest.", 0.2),
            ("How do you keep a team together when nothing is working?", 1.2),
            ("You tell them the truth every single week and you show them the numbers as they are.", 0.2),
            ("Because people can take bad news but they cannot take being kept in the dark.", 0.2),
        ]);
        let mut d = ShortsDialog { length: 14.0, count: 2, ..ShortsDialog::new(MediaId::new(), "talk.mov".into()) };
        d.find(None);
        assert!(matches!(&d.found, Some(Err(why)) if why.contains("analyze")));
        d.find(Some(&words));
        let Some(Ok(found)) = &mut d.found else { panic!("{:?}", d.found) };
        assert_eq!(found.len(), 2, "{found:?}");
        found[1].1 = false;
        let spoken: Vec<klipp_document::ops::SpokenWord> = words.iter().map(|w| klipp_document::ops::SpokenWord { start_secs: w.start_secs, end_secs: w.end_secs, text: w.text.clone(), confidence: w.confidence }).collect();
        let cmds = d.build(&spoken, klipp_core::Fps::new(30, 1));
        assert!(matches!(cmds.as_slice(), [AppCommand::AddSequenceAndOpen { width: 1080, height: 1920, .. }, AppCommand::FillShort { pieces, hook: None, .. }] if !pieces.is_empty()), "{cmds:?}");
    }
}
