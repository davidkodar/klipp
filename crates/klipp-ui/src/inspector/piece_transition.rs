//! The transition inspector's controls for the piece transitions (#985):
//! how many strips, rows or shards, which way they go, how they move
//! (slide, tilt, turn, fall, shrink), in what order, how they ease,
//! whether they leave or arrive, the gap between them and their shadows. Typed as
//! `PieceParams`; what changed comes back as the transition's parameters.

use klipp_core::piece_transition::{PieceEase, PieceMotion, PieceOrder, PieceParams};
use klipp_core::{TransitionKind, TransitionUserParams};

use crate::theme::KlippTheme;

/// The panels' four ways, by angle.
const PANEL_WAYS: [(&str, f32); 4] = [("Right", 0.0), ("Down", 90.0), ("Left", 180.0), ("Up", 270.0)];

fn label(ui: &mut egui::Ui, theme: &KlippTheme, text: &str) {
    ui.add_sized([72.0, 18.0], egui::Label::new(egui::RichText::new(text).color(theme.text_secondary).size(11.0)));
}

/// The controls for `kind` with `params`; the new parameters when one
/// was changed.
pub fn show(ui: &mut egui::Ui, theme: &KlippTheme, kind: TransitionKind, params: &TransitionUserParams) -> Option<TransitionUserParams> {
    let old = PieceParams::read(kind, params);
    let mut p = old;
    ui.add_space(6.0);
    match kind {
        TransitionKind::SlideStrips => {
            ui.horizontal(|ui| {
                label(ui, theme, "Strips");
                ui.add(egui::DragValue::new(&mut p.count).range(1..=64).speed(0.1));
            });
            ui.horizontal(|ui| {
                label(ui, theme, "Direction");
                ui.add(egui::DragValue::new(&mut p.angle).range(-360.0..=360.0).speed(1.0).suffix("\u{b0}"))
                    .on_hover_text("The way the strips slide: 0\u{b0} sideways, 90\u{b0} up and down");
            });
        }
        TransitionKind::SlidePanels => {
            ui.horizontal(|ui| {
                label(ui, theme, "Rows");
                // Posed panels are drawn forward, each one: fewer of them.
                let most = if p.motion == PieceMotion::Slide { 16 } else { 8 };
                ui.add(egui::DragValue::new(&mut p.count).range(1..=most).speed(0.1)).on_hover_text("As many columns as make the panels square");
            });
            ui.horizontal(|ui| {
                label(ui, theme, "Direction");
                let quarter = ((p.angle / 90.0).round().rem_euclid(4.0)) as usize;
                for (i, (name, angle)) in PANEL_WAYS.iter().enumerate() {
                    if ui.selectable_label(i == quarter, *name).clicked() {
                        p.angle = *angle;
                    }
                }
            });
        }
        TransitionKind::Shatter => {
            ui.horizontal(|ui| {
                label(ui, theme, "Shards");
                ui.add(egui::DragValue::new(&mut p.count).range(4..=96).speed(0.2));
            });
            ui.horizontal(|ui| {
                label(ui, theme, "Direction");
                ui.add(egui::DragValue::new(&mut p.angle).range(-360.0..=360.0).speed(1.0).suffix("\u{b0}"))
                    .on_hover_text("The way the shards are pushed as they break away");
            });
        }
        _ => {
            ui.horizontal(|ui| {
                label(ui, theme, "Line");
                ui.add(egui::DragValue::new(&mut p.angle).range(-360.0..=360.0).speed(1.0).suffix("\u{b0}"))
                    .on_hover_text("The line the picture splits down: 90\u{b0} upright, 0\u{b0} level");
            });
            ui.horizontal(|ui| {
                label(ui, theme, "Halves");
                if ui.selectable_label(!p.shear, "Part").on_hover_text("Move apart, across the line").clicked() {
                    p.shear = false;
                }
                if ui.selectable_label(p.shear, "Slide past").on_hover_text("Slide past each other, along the line").clicked() {
                    p.shear = true;
                }
            });
        }
    }
    ui.horizontal(|ui| {
        label(ui, theme, "Motion");
        egui::ComboBox::from_id_salt("piece_motion").selected_text(p.motion.label()).show_ui(ui, |ui| {
            for m in PieceMotion::ALL {
                ui.selectable_value(&mut p.motion, m, m.label());
            }
        });
    });
    ui.horizontal(|ui| {
        label(ui, theme, "Pieces");
        if ui.selectable_label(!p.assemble, "Leave").on_hover_text("The outgoing picture's pieces move away").clicked() {
            p.assemble = false;
        }
        if ui.selectable_label(p.assemble, "Arrive").on_hover_text("The incoming picture's pieces move in").clicked() {
            p.assemble = true;
        }
    });
    ui.horizontal(|ui| {
        label(ui, theme, "Order");
        egui::ComboBox::from_id_salt("piece_order").selected_text(p.order.label()).show_ui(ui, |ui| {
            for o in PieceOrder::ALL {
                ui.selectable_value(&mut p.order, o, o.label());
            }
        });
    });
    ui.horizontal(|ui| {
        label(ui, theme, "Stagger");
        ui.add(egui::Slider::new(&mut p.stagger, 0.0..=0.9).show_value(true)).on_hover_text("How far apart the pieces start");
    });
    ui.horizontal(|ui| {
        label(ui, theme, "Easing");
        egui::ComboBox::from_id_salt("piece_ease").selected_text(p.ease.label()).show_ui(ui, |ui| {
            for e in PieceEase::ALL {
                ui.selectable_value(&mut p.ease, e, e.label());
            }
        });
    });
    if kind != TransitionKind::PartHalves {
        ui.horizontal(|ui| {
            label(ui, theme, "");
            ui.checkbox(&mut p.alternate, if kind == TransitionKind::SlideStrips { "Alternate ways" } else { "Rows alternate" })
                .on_hover_text("Neighbours move opposite ways");
        });
    }
    ui.horizontal(|ui| {
        label(ui, theme, "Gap");
        ui.add(egui::DragValue::new(&mut p.gap).range(0..=31).speed(0.2).suffix(" px")).on_hover_text("A gap that opens between the pieces as they move");
    });
    // Shadows and the tumbling's pattern are the posed pieces': the
    // sliding ones are flat on the picture.
    if kind == TransitionKind::Shatter || p.motion != PieceMotion::Slide {
        ui.horizontal(|ui| {
            label(ui, theme, "");
            ui.checkbox(&mut p.shadow, "Shadows").on_hover_text("Soft shadows under the moving pieces");
        });
        ui.horizontal(|ui| {
            label(ui, theme, "Pattern");
            ui.add(egui::DragValue::new(&mut p.seed).range(0..=255).speed(0.3)).on_hover_text("Another cut of the shards, another way for each piece to turn");
        });
    }
    if p.motion != PieceMotion::Slide && kind == TransitionKind::SlidePanels {
        p.count = p.count.min(8);
    }
    (p != old).then(|| {
        let mut out = *params;
        p.write(kind, &mut out);
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Panel {
        ctx: egui::Context,
        kind: TransitionKind,
        params: TransitionUserParams,
        clock: f64,
    }

    impl Panel {
        fn new(kind: TransitionKind) -> Self {
            Panel { ctx: egui::Context::default(), kind, params: klipp_core::spec_for_kind(kind).default_params, clock: 0.0 }
        }

        /// One frame; a change is kept, as the command it sends would.
        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
            self.clock += 0.05;
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 500.0))), time: Some(self.clock), events, ..Default::default() };
            let theme = KlippTheme::default();
            let (kind, params) = (self.kind, self.params);
            let mut changed = None;
            let out = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| changed = show(ui, &theme, kind, &params));
            });
            if let Some(p) = changed {
                self.params = p;
            }
            out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect()
        }

        fn click(&mut self, text: &str) {
            let at = self.frame(Vec::new()).into_iter().rev().find(|(t, _)| t == text).unwrap_or_else(|| panic!("no {text:?}")).1;
            self.frame(vec![egui::Event::PointerMoved(at)]);
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }]);
            }
        }

        fn read(&self) -> PieceParams {
            PieceParams::read(self.kind, &self.params)
        }
    }

    /// #985, with pointer events: the panels' way, leave or arrive, the
    /// order and the easing from their lists, the halves' motion; each
    /// change is the transition's parameters with that one setting moved.
    #[test]
    fn the_piece_controls_set_the_transition() {
        let mut panels = Panel::new(TransitionKind::SlidePanels);
        panels.frame(Vec::new());
        panels.click("Up");
        assert_eq!(panels.read().angle, 270.0);
        panels.click("Arrive");
        assert!(panels.read().assemble);
        panels.click("In sequence");
        panels.click("Centre out");
        assert_eq!(panels.read().order, PieceOrder::CentreOut);
        panels.click("Smooth");
        panels.click("Overshoot");
        assert_eq!(panels.read().ease, PieceEase::Overshoot);
        panels.click("Rows alternate");
        let p = panels.read();
        assert!(p.alternate && p.count == 3 && p.assemble && p.angle == 270.0, "{p:?}");

        let mut halves = Panel::new(TransitionKind::PartHalves);
        halves.frame(Vec::new());
        assert!(halves.frame(Vec::new()).iter().all(|(t, _)| t != "Alternate ways" && t != "Rows alternate"), "two halves have no neighbours to alternate");
        halves.click("Slide past");
        assert!(halves.read().shear);
        assert_eq!(halves.params.param1, 70.0, "the line kept");

        // Shatter: its motion from the list, its shadows off; a panel's
        // motion other than sliding offers shadows too.
        let mut shatter = Panel::new(TransitionKind::Shatter);
        shatter.frame(Vec::new());
        shatter.click("Fall");
        shatter.click("Turn");
        assert_eq!(shatter.read().motion, PieceMotion::Turn);
        shatter.click("Shadows");
        assert!(!shatter.read().shadow, "on by default, clicked off");
        assert!(panels.frame(Vec::new()).iter().all(|(t, _)| t != "Shadows"), "sliding panels have none");
        panels.click("Slide");
        panels.click("Tilt away");
        assert_eq!(panels.read().motion, PieceMotion::Tilt);
        assert!(panels.frame(Vec::new()).iter().any(|(t, _)| t == "Shadows"));

        // Nothing touched, nothing sent.
        let theme = KlippTheme::default();
        let mut sent = None;
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| sent = show(ui, &theme, TransitionKind::SlideStrips, &klipp_core::spec_for_kind(TransitionKind::SlideStrips).default_params));
        });
        assert!(sent.is_none());
    }
}
