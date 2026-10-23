//! The nodes the frame on screen didn't draw as asked (M4 step 10): a chip
//! in the viewer's corner names each effect and why in plain words, and a
//! click shows that node in the graph and the inspector, whose header
//! gives the same reason. A node switched off is as asked: no chip.

use std::collections::HashMap;

use klipp_core::{AppCommand, ClipId};
use klipp_graph_format::path::{NodePath, Root};
use klipp_nodes::{Decline, NodeId};

use crate::state::AppState;
use crate::theme::KlippTheme;

/// The most chips drawn; the rest are counted on one more.
const MAX_CHIPS: usize = 3;

fn store_id() -> egui::Id {
    egui::Id::new("klipp_frame_declines")
}

/// Keep the shown frame's reasons where the inspector reads them, per
/// clip and node, for this and later UI frames until the next frame.
pub fn publish(ctx: &egui::Context, declines: &[Decline]) {
    let reasons: HashMap<(ClipId, NodeId), String> = declines
        .iter()
        .filter(|d| d.shown())
        .filter_map(|d| Some(((d.clip?, d.node), d.reason.clone())))
        .collect();
    ctx.data_mut(|data| data.insert_temp(store_id(), std::sync::Arc::new(reasons)));
}

/// Why the shown frame didn't draw `node` of `clip` as asked, if it didn't.
pub fn reason(ctx: &egui::Context, clip: ClipId, node: NodeId) -> Option<String> {
    let reasons: Option<std::sync::Arc<HashMap<(ClipId, NodeId), String>>> = ctx.data(|data| data.get_temp(store_id()));
    reasons.and_then(|r| r.get(&(clip, node)).cloned())
}

/// The chips, along the lower left of `rect`; none when the frame drew
/// everything asked.
pub fn draw_chips(ui: &mut egui::Ui, state: &AppState, theme: &KlippTheme, rect: egui::Rect, cmds: &mut Vec<AppCommand>) {
    let shown: Vec<&Decline> = state.frame_declines.iter().filter(|d| d.shown()).collect();
    if shown.is_empty() {
        return;
    }
    let font = egui::FontId::proportional(11.0);
    let pad = egui::vec2(8.0, 4.0);
    let mut bottom = rect.max.y - 8.0;
    let extra = shown.len().saturating_sub(MAX_CHIPS);
    let lines = shown.iter().take(MAX_CHIPS).map(|d| (Some(*d), d.plain())).chain((extra > 0).then(|| (None, format!("+{extra} more"))));
    for (n, (decline, text)) in lines.collect::<Vec<_>>().into_iter().enumerate().rev() {
        let galley = ui.painter().layout_no_wrap(text, font.clone(), theme.text_primary);
        let size = galley.size() + pad * 2.0;
        let chip = egui::Rect::from_min_size(egui::pos2(rect.min.x + 8.0, bottom - size.y), size);
        bottom = chip.min.y - 4.0;
        let response = ui.interact(chip, ui.id().with(("frame_decline", n)), egui::Sense::click());
        let fill = if response.hovered() { theme.bg_hover } else { theme.bg_raised };
        ui.painter().rect_filled(chip, 3.0, fill);
        ui.painter().rect_stroke(chip, 3.0, egui::Stroke::new(1.0_f32, theme.marker_warning));
        ui.painter().galley(chip.min + pad, galley, theme.text_primary);
        let Some(decline) = decline else { continue };
        let response = response.on_hover_text(&decline.why);
        if response.clicked() {
            if let Some(clip) = decline.clip {
                cmds.push(AppCommand::RevealGraph { path: NodePath::new(Root::Clip { clip: clip.0 }, [decline.node]), select: true });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_nodes::{DeclineOutcome, NodeKind};

    /// The program viewer as the app draws it, with the frame's declines.
    struct Viewer {
        ctx: egui::Context,
        state: AppState,
        panel: crate::panels::viewer::ViewerPanel,
        time: f64,
        sent: Vec<AppCommand>,
    }

    impl Viewer {
        fn new(declines: Vec<Decline>) -> Self {
            let state = AppState { frame_declines: declines, ..AppState::default() };
            Self { ctx: egui::Context::default(), state, panel: Default::default(), time: 0.0, sent: Vec::new() }
        }

        /// One frame with `events`: the text drawn, with where.
        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
            self.time += 0.5;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 600.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let theme = KlippTheme::default();
            let (state, panel) = (&mut self.state, &mut self.panel);
            let mut cmds = Vec::new();
            let out = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                    cmds = panel.show(ui, state, &theme, &crate::keybindings::KeyBindings::default());
                });
            });
            self.sent.extend(cmds);
            out.shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
                    _ => None,
                })
                .collect()
        }

        fn click(&mut self, at: egui::Pos2) {
            let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            self.frame(vec![egui::Event::PointerMoved(at), button(true)]);
            self.frame(vec![button(false)]);
        }
    }

    fn decline(clip: ClipId, outcome: DeclineOutcome, name: &str, reason: &str) -> Decline {
        Decline { clip: Some(clip), node: uuid::Uuid::new_v4(), kind: NodeKind::Frei0r, outcome, name: name.into(), reason: reason.into(), why: "detail".into() }
    }

    #[test]
    fn a_declined_node_has_a_chip_on_the_viewer_that_reveals_it() {
        let clip = ClipId::new();
        let delay = decline(clip, DeclineOutcome::Unsupported, "Delay0r", "needs a frame size under 2048");
        let off = decline(clip, DeclineOutcome::Bypass, "Glow", "switched off");
        let mut viewer = Viewer::new(vec![delay.clone(), off]);
        let drawn = viewer.frame(Vec::new());
        let chip = drawn.iter().find(|(t, _)| t == "frei0r Delay0r: needs a frame size under 2048").unwrap_or_else(|| panic!("the chip is drawn: {drawn:?}")).1;
        assert!(!drawn.iter().any(|(t, _)| t.contains("Glow")), "a node switched off has no chip: {drawn:?}");
        viewer.click(chip);
        let want = NodePath::new(Root::Clip { clip: clip.0 }, [delay.node]);
        let revealed = viewer.sent.iter().any(|c| matches!(c, AppCommand::RevealGraph { path, select: true } if *path == want));
        assert!(revealed, "the click shows the node: {:?}", viewer.sent);
    }

    #[test]
    fn no_chip_when_the_frame_drew_everything() {
        let mut viewer = Viewer::new(Vec::new());
        let drawn = viewer.frame(Vec::new());
        assert!(!drawn.iter().any(|(t, _)| t.starts_with("frei0r")), "{drawn:?}");
    }

    #[test]
    fn the_inspector_reads_the_reason_of_the_node_on_screen() {
        let ctx = egui::Context::default();
        let clip = ClipId::new();
        let delay = decline(clip, DeclineOutcome::PluginMissing, "Delay0r", "plug-in missing");
        let off = decline(clip, DeclineOutcome::Bypass, "Glow", "switched off");
        publish(&ctx, &[delay.clone(), off.clone()]);
        assert_eq!(reason(&ctx, clip, delay.node).as_deref(), Some("plug-in missing"));
        assert_eq!(reason(&ctx, clip, off.node), None);
        publish(&ctx, &[]);
        assert_eq!(reason(&ctx, clip, delay.node), None, "gone with the frame");
    }
}
