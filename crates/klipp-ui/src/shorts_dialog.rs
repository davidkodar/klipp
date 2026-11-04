//! The Make Shorts dialog (#2720): a long talk, podcast, stream or vlog's
//! moments that stand on their own as short vertical clips, found from
//! its transcript, reviewed, then built, each a sequence of its own.
//!
//! Find runs `klipp_analysis::shorts::find` on the file's transcript;
//! the list shows each moment's place, length, score, hook and why, each
//! kept or dropped; Build sends [`AppCommand::MakeShorts`] with the kept
//! ones, each made a 1080x1920 sequence and all added as one undo step.

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
                // The project's language picks the hook words (#2889); with no
                // list for it, moments are found by their shape alone.
                let lists = crate::transcript::word_lists();
                let none = klipp_core::lang::Words::none("?");
                let found = klipp_analysis::shorts::find(words, &ask, lists.as_deref().unwrap_or(&none));
                if found.is_empty() {
                    Err(format!("No moment of {:.0} to {:.0} seconds stands on its own here.{}", ask.min_secs, ask.max_secs, lists.err().map(|why| format!(" ({why})")).unwrap_or_default()))
                } else {
                    Ok(found.into_iter().map(|m| (m, true)).collect())
                }
            }
        });
    }

    /// The command that makes the kept moments, each a sequence of its
    /// own named after its hook, at `fps`, its pieces cut from `words` of
    /// a file `length` seconds long, inside which they stay.
    pub fn build(&self, words: &[klipp_document::ops::SpokenWord], fps: klipp_core::Fps, length: f64) -> Vec<AppCommand> {
        let Some(Ok(found)) = &self.found else { return Vec::new() };
        let shorts: Vec<klipp_core::ShortAsk> = found
            .iter()
            .filter(|(_, keep)| *keep)
            .map(|(m, _)| klipp_core::ShortAsk {
                pieces: klipp_document::shorts::pieces(words, m.from_secs, m.to_secs, length, self.tighten, crate::transcript::word_lists().ok().as_deref()),
                hook: m.hook.clone(),
                hook_title: self.hook_title.then(|| m.hook.clone()),
            })
            .filter(|s| !s.pieces.is_empty())
            .collect();
        if shorts.is_empty() {
            return Vec::new();
        }
        vec![AppCommand::MakeShorts { media_id: self.media_id, fps_num: fps.num, fps_den: fps.den, shorts }]
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
    crate::widgets::window("Make Shorts")
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
        let length = state.media_pool.get(&media_id).map(|m| m.duration.to_secs_f64()).filter(|d| *d > 0.0).unwrap_or(f64::INFINITY);
        return dialog.build(&spoken, fps, length);
    }
    if open && !cancel {
        state.shorts_dialog = Some(dialog);
    }
    Vec::new()
}

/// `shorts` of `media` built whole, each named apart from `taken` and
/// from the others, its sound brought up or down by `gain` (linear) when
/// given: the sequences to add together as one undo step, or why the one
/// that could not be built was not, and none of them. Made one by one,
/// each short was a new sequence and two undo steps that left it behind,
/// a refused one left it empty, and every Build's first was "Short 1".
pub fn shorts_edit(taken: &[String], fps: klipp_core::Fps, media: MediaId, shorts: &[klipp_core::ShortAsk], pool: &klipp_media::MediaPool, transcripts: &dyn klipp_document::ops::Transcripts, gain: Option<f32>) -> Result<Vec<klipp_timeline::Sequence>, String> {
    let mut taken = taken.to_vec();
    let mut out = Vec::new();
    for (k, ask) in shorts.iter().enumerate() {
        let name = klipp_document::shorts::short_name(&taken, &ask.hook);
        taken.push(name.clone());
        let mut short = klipp_document::shorts::build_short(&name, fps, media, &ask.pieces, ask.hook_title.as_deref(), pool, transcripts).map_err(|r| format!("short {}: {} {}", k + 1, r.why, r.hint))?;
        if let Some(gain) = gain {
            let sounds: Vec<klipp_core::ClipId> = short.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Audio).flat_map(|t| &t.clips).flat_map(|c| std::iter::once(c.id).chain(c.linked_clip_id)).collect();
            for clip in short.all_clips_mut().filter(|c| sounds.contains(&c.id)) {
                clip.gain = gain;
            }
        }
        out.push(short);
    }
    Ok(out)
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
        let cmds = d.build(&spoken, klipp_core::Fps::new(30, 1), f64::INFINITY);
        assert!(matches!(cmds.as_slice(), [AppCommand::MakeShorts { fps_num: 30, fps_den: 1, shorts, .. }] if shorts.len() == 1 && !shorts[0].pieces.is_empty() && shorts[0].hook_title.is_none()), "one command, the kept one: {cmds:?}");
        // A file that ends just after the moment's last word: its pieces
        // stay inside it, the tail stopping at the file's end.
        let last = found_last_end(&d, &spoken);
        let cmds = d.build(&spoken, klipp_core::Fps::new(30, 1), last + 0.05);
        let Some(AppCommand::MakeShorts { shorts, .. }) = cmds.first() else { panic!("{cmds:?}") };
        assert!(shorts[0].pieces.iter().all(|(_, b)| *b <= last + 0.05 + 1e-9), "inside the file: {:?}", shorts[0].pieces);
    }

    struct Said(Vec<klipp_document::ops::SpokenWord>);

    impl klipp_document::ops::Transcripts for Said {
        fn words(&self, _: &klipp_media::MediaItem) -> Option<Vec<klipp_document::ops::SpokenWord>> {
            Some(self.0.clone())
        }
    }

    /// The kept shorts are built whole and handed back to be added as one
    /// undo step: each named apart from the project's sequences and from
    /// each other, each levelled; one that cannot be built makes none.
    #[test]
    fn the_kept_shorts_are_built_together_or_not_at_all() {
        let words: Vec<klipp_document::ops::SpokenWord> = (0..20).map(|k| klipp_document::ops::SpokenWord { start_secs: 1.0 + k as f64 * 0.4, end_secs: 1.3 + k as f64 * 0.4, text: "word".into(), confidence: 0.9 }).collect();
        let mut pool = klipp_media::MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/shorts-dialog-talk.mov".into(), None);
        item.duration = klipp_core::RationalTime::from_secs_f64(12.0, 30);
        item.video_codec = "hevc".into();
        item.has_audio = true;
        let media = pool.add(item);
        let ask = |a: f64, b: f64| klipp_core::ShortAsk { pieces: vec![(a, b)], hook: "The old town".into(), hook_title: Some("The old town".into()) };
        let taken = vec!["Main Sequence".to_string(), "Short 1: The old town".to_string()];
        let made = shorts_edit(&taken, klipp_core::Fps::new(30, 1), media, &[ask(1.0, 4.0), ask(5.0, 8.0)], &pool, &Said(words.clone()), Some(2.0)).unwrap();
        let names: Vec<&str> = made.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Short 2: The old town", "Short 3: The old town"]);
        for short in &made {
            assert_eq!((short.resolution.width, short.resolution.height), (1080, 1920));
            let sounds: Vec<f32> = short.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Audio).flat_map(|t| &t.clips).map(|c| c.gain).collect();
            assert!(!sounds.is_empty() && sounds.iter().all(|g| *g == 2.0), "{}: levelled {sounds:?}", short.name);
        }
        let refused = shorts_edit(&taken, klipp_core::Fps::new(30, 1), media, &[ask(1.0, 4.0), ask(20.0, 25.0)], &pool, &Said(words), None);
        assert!(refused.is_err_and(|why| why.starts_with("short 2:")), "one past its file makes none");
    }

    /// The end of the last word of the first kept moment.
    fn found_last_end(d: &ShortsDialog, words: &[klipp_document::ops::SpokenWord]) -> f64 {
        let Some(Ok(found)) = &d.found else { panic!("no moments") };
        let (m, _) = found.iter().find(|(_, keep)| *keep).expect("a kept moment");
        words.iter().filter(|w| w.end_secs <= m.to_secs + 0.05).map(|w| w.end_secs).fold(0.0, f64::max)
    }
}
