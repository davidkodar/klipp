//! Track Subject (#239): what the External Matte card shows about a
//! tracked subject matte, and the line the status bar shows while a pass
//! runs. The app works it out each moment from the analysis service and
//! the folder (`app/track_subject.rs`); the card only reads it.

use std::path::{Path, PathBuf};

/// Where a tracked matte stands, for its card.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackCard {
    /// The pass running: how far, 0..1.
    pub running: Option<f32>,
    /// The frames the render reads that have a matte, of how many.
    pub covered: usize,
    pub needed: usize,
    /// Frames where the subject wasn't found (an empty matte).
    pub lost: usize,
    /// The model it was made with, as `TrackSubject` names it.
    pub model: &'static str,
    /// The correction it gates, by name.
    pub gates: Option<String>,
}

impl TrackCard {
    /// The card's line.
    pub fn line(&self) -> String {
        let frames = |n: usize| if n == 1 { "1 frame".to_string() } else { format!("{n} frames") };
        if let Some(p) = self.running {
            return format!("Tracking subject \u{b7} {:.0}% ({} of {})", (p * 100.0).clamp(0.0, 100.0), self.covered, frames(self.needed));
        }
        if self.covered < self.needed {
            return format!("Covers {} of {}", self.covered, frames(self.needed));
        }
        let mut s = format!("Subject tracked \u{b7} {}", frames(self.needed));
        if self.lost > 0 {
            s.push_str(&format!(" \u{b7} {} without the subject", self.lost));
        }
        s
    }
}

/// The model a matte sequence was made with, as `TrackSubject` names it.
pub fn model_name(model_id: &str) -> &'static str {
    if model_id.starts_with("birefnet") { "foreground" } else { "subject" }
}

thread_local! {
    static CARDS: std::cell::RefCell<std::collections::HashMap<PathBuf, TrackCard>> = Default::default();
}

/// The card for the matte sequence in `dir`, as last worked out.
pub fn card(dir: &Path) -> Option<TrackCard> {
    CARDS.with(|c| c.borrow().get(dir).cloned())
}

/// Set what the card for `dir` shows.
pub fn set_card(dir: &Path, card: TrackCard) {
    CARDS.with(|c| {
        c.borrow_mut().insert(dir.to_path_buf(), card);
    });
}

thread_local! {
    static SUBJECT_CARDS: std::cell::RefCell<std::collections::HashMap<(klipp_core::ClipId, u32), TrackCard>> = Default::default();
}

/// The card for a clip's subject matte bound by model (#1881).
pub fn subject_card(clip: klipp_core::ClipId, model: u32) -> Option<TrackCard> {
    SUBJECT_CARDS.with(|c| c.borrow().get(&(clip, model)).cloned())
}

/// Set what the card for a clip's subject bound by model shows.
pub fn set_subject_card(clip: klipp_core::ClipId, model: u32, card: TrackCard) {
    SUBJECT_CARDS.with(|c| {
        c.borrow_mut().insert((clip, model), card);
    });
}

thread_local! {
    static TRANSITION_CARDS: std::cell::RefCell<std::collections::HashMap<klipp_core::TransitionId, TrackCard>> = Default::default();
}

/// The card for the subject a piece transition cuts round (#985 phase 3).
pub fn transition_card(id: klipp_core::TransitionId) -> Option<TrackCard> {
    TRANSITION_CARDS.with(|c| c.borrow().get(&id).cloned())
}

/// Set what the transition's subject card shows.
pub fn set_transition_card(id: klipp_core::TransitionId, card: TrackCard) {
    TRANSITION_CARDS.with(|c| {
        c.borrow_mut().insert(id, card);
    });
}

/// A piece transition's subject block (#985 phase 3): where its matte
/// stands, and the one thing to do: Cancel while it tracks, Track when it
/// doesn't cover the transition. The commands asked for.
pub fn transition_card_ui(ui: &mut egui::Ui, card: Option<&TrackCard>, transition_id: klipp_core::TransitionId, side: klipp_core::ClipId, theme: &crate::theme::KlippTheme) -> Vec<klipp_core::AppCommand> {
    use klipp_core::AppCommand;
    let mut cmds = Vec::new();
    ui.horizontal(|ui| {
        ui.add_space(76.0);
        let Some(card) = card else {
            ui.label(egui::RichText::new("Reading\u{2026}").small().color(theme.text_dim));
            return;
        };
        if let Some(p) = card.running {
            let width = (ui.available_width() - 72.0).max(80.0);
            ui.add(egui::ProgressBar::new(p.clamp(0.0, 1.0)).desired_width(width).text(egui::RichText::new(card.line()).small()));
            if ui.small_button("Cancel").on_hover_text("Stop tracking; the frames made so far are kept").clicked() {
                cmds.push(AppCommand::CancelTrackSubject { clip_id: side });
            }
        } else if card.covered < card.needed {
            let line = if card.covered == 0 { "Not tracked yet".to_string() } else { card.line() };
            ui.label(egui::RichText::new(line).small().color(theme.marker_warning));
            if ui.small_button("Track").on_hover_text("Track the subject through the frames this transition shows; made in the background").clicked() {
                cmds.push(AppCommand::TrackTransitionSubject { transition_id });
            }
        } else {
            ui.label(egui::RichText::new(card.line()).small().color(theme.text_secondary));
        }
    });
    cmds
}

/// The card's block on an External Matte that reads a tracked subject:
/// its name, where it stands, and the one thing to do: Cancel while it
/// tracks, Track the rest when it doesn't cover the clip. The commands
/// asked for.
pub fn card_ui(ui: &mut egui::Ui, card: Option<&TrackCard>, clip_id: klipp_core::ClipId, theme: &crate::theme::KlippTheme) -> Vec<klipp_core::AppCommand> {
    use klipp_core::AppCommand;
    let mut cmds = Vec::new();
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Subject matte").strong().color(theme.text_primary));
        if let Some(gates) = card.and_then(|c| c.gates.as_deref()) {
            ui.label(egui::RichText::new(format!("gates \u{201c}{gates}\u{201d}")).small().color(theme.text_dim));
        }
    });
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        let Some(card) = card else {
            ui.label(egui::RichText::new("Reading\u{2026}").small().color(theme.text_dim));
            return;
        };
        if let Some(p) = card.running {
            let width = (ui.available_width() - 72.0).max(80.0);
            ui.add(egui::ProgressBar::new(p.clamp(0.0, 1.0)).desired_width(width).text(egui::RichText::new(card.line()).small()));
            if ui.small_button("Cancel").on_hover_text("Stop tracking; the frames made so far are kept").clicked() {
                cmds.push(AppCommand::CancelTrackSubject { clip_id });
            }
        } else if card.covered < card.needed {
            ui.label(egui::RichText::new(card.line()).small().color(theme.marker_warning));
            if ui.small_button("Track the rest").on_hover_text("Track the subject through the frames this clip shows that have no matte yet").clicked() {
                cmds.push(AppCommand::TrackSubject { clip_id, model: card.model.to_string() });
            }
        } else {
            ui.label(egui::RichText::new(card.line()).small().color(theme.text_secondary));
        }
    });
    cmds
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The card's one action, by pointer: Cancel while tracking, Track
    /// the rest when short, nothing to do when covered.
    /// #985 phase 3: a transition's subject card, by pointer: Track when
    /// its matte is short, Cancel (of the side clip's pass) while it runs.
    #[test]
    fn a_transitions_subject_card_tracks_it() {
        let (tid, side) = (klipp_core::TransitionId::new(), klipp_core::ClipId::new());
        let run = |card: TrackCard, press: &str| -> Vec<klipp_core::AppCommand> {
            let ctx = egui::Context::default();
            let theme = crate::theme::KlippTheme::default();
            let mut sent = Vec::new();
            let frame = |events: Vec<egui::Event>, sent: &mut Vec<klipp_core::AppCommand>, t: f64| {
                let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 400.0))), time: Some(t), events, ..Default::default() };
                let out = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| sent.extend(transition_card_ui(ui, Some(&card), tid, side, &theme)));
                });
                out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect::<Vec<_>>()
            };
            frame(Vec::new(), &mut sent, 0.0);
            let texts = frame(Vec::new(), &mut sent, 0.05);
            let at = texts.iter().find(|(t, _)| t == press).unwrap_or_else(|| panic!("no {press:?} in {texts:?}")).1;
            let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            // Pressed as it arrives: a hover's tooltip isn't up to take it.
            frame(vec![egui::Event::PointerMoved(at), button(true)], &mut sent, 0.15);
            frame(vec![button(false)], &mut sent, 0.2);
            frame(Vec::new(), &mut sent, 0.25);
            sent
        };
        let card = |running, covered| TrackCard { running, covered, needed: 12, lost: 0, model: "subject", gates: None };
        let sent = run(card(None, 0), "Track");
        assert!(matches!(&sent[..], [klipp_core::AppCommand::TrackTransitionSubject { transition_id }] if *transition_id == tid), "{sent:?}");
        assert!(matches!(&run(card(Some(0.5), 6), "Cancel")[..], [klipp_core::AppCommand::CancelTrackSubject { clip_id }] if *clip_id == side));
    }

    #[test]
    fn the_card_offers_one_action_by_where_it_stands() {
        let clip = klipp_core::ClipId::new();
        let run = |card: TrackCard, press: &str| -> Vec<klipp_core::AppCommand> {
            let ctx = egui::Context::default();
            let theme = crate::theme::KlippTheme::default();
            let mut sent = Vec::new();
            let frame = |events: Vec<egui::Event>, sent: &mut Vec<klipp_core::AppCommand>, t: f64| {
                let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 120.0))), time: Some(t), events, ..Default::default() };
                let out = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| sent.extend(card_ui(ui, Some(&card), clip, &theme)));
                });
                out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect::<Vec<_>>()
            };
            frame(Vec::new(), &mut sent, 0.0);
            let texts = frame(Vec::new(), &mut sent, 0.05);
            if let Some((_, at)) = texts.iter().find(|(t, _)| t == press) {
                let at = *at;
                frame(vec![egui::Event::PointerMoved(at)], &mut sent, 0.1);
                for (i, pressed) in [true, false].into_iter().enumerate() {
                    frame(vec![egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }], &mut sent, 0.15 + i as f64 * 0.05);
                }
            } else {
                assert!(press.is_empty(), "no {press:?} in {texts:?}");
            }
            sent
        };
        let card = |running, covered| TrackCard { running, covered, needed: 290, lost: 0, model: "subject", gates: Some("Corrector".into()) };
        assert!(matches!(&run(card(Some(0.4), 116), "Cancel")[..], [klipp_core::AppCommand::CancelTrackSubject { clip_id }] if *clip_id == clip));
        assert!(matches!(&run(card(None, 116), "Track the rest")[..], [klipp_core::AppCommand::TrackSubject { clip_id, model }] if *clip_id == clip && model == "subject"));
        assert!(run(card(None, 290), "").is_empty());
    }

    #[test]
    fn the_cards_line_says_where_it_stands() {
        let card = |running, covered, needed, lost| TrackCard { running, covered, needed, lost, model: "subject", gates: None };
        assert_eq!(card(Some(0.414), 120, 290, 0).line(), "Tracking subject \u{b7} 41% (120 of 290 frames)");
        assert_eq!(card(None, 120, 290, 0).line(), "Covers 120 of 290 frames");
        assert_eq!(card(None, 290, 290, 0).line(), "Subject tracked \u{b7} 290 frames");
        assert_eq!(card(None, 290, 290, 2).line(), "Subject tracked \u{b7} 290 frames \u{b7} 2 without the subject");
        assert_eq!((model_name("birefnet_lite_v1"), model_name("rf_detr_seg_v1")), ("foreground", "subject"));
    }
}
