//! The inspector with nothing selected: the sequence. What it
//! is at a glance, its range, and the viewer and playback switches,
//! each a control or plainly a fact. Its timeline counts and storage
//! are on the Info tab, its delivery on the Export page.

use egui::{RichText, Ui};
use klipp_core::{AppCommand, ViewerOverlay};

use crate::state::AppState;
use crate::theme::KlippTheme;
use super::rows::{point_row, row, switch};

/// The sequence, for the inspector with nothing selected.
pub fn show(ui: &mut Ui, theme: &KlippTheme, state: &AppState, cmds: &mut Vec<AppCommand>) {
    let seq = state.sequence();
    header(ui, theme, state, cmds);
    ui.add_space(10.0);

    crate::widgets::inspector_group::section_card(ui, "RANGE", None, theme, |ui| {
        let fallback_out = state.sequence_duration();
        let tc = seq.timecode_display();
        point_row(ui, theme, "In", seq.in_point.map(|t| tc.position(t)), cmds, AppCommand::MarkRange { in_point: Some(None), out_point: None });
        point_row(ui, theme, "Out", seq.out_point.map(|t| tc.position(t)), cmds, AppCommand::MarkRange { in_point: None, out_point: Some(None) });
        let end = seq.out_point.unwrap_or(fallback_out);
        // From the In point, or the start, to the Out point or the end.
        let length = seq.in_point.map_or(end, |start| if start < end { end - start } else { end - start.max(end) });
        row(ui, theme, "Length", |ui| {
            ui.label(RichText::new(tc.duration(length)).monospace().size(12.5).color(theme.text_primary));
        });
        ui.add_space(4.0);
    });

    crate::widgets::inspector_group::section_card(ui, "VIEWER", None, theme, |ui| {
        let o = state.viewer_overlays;
        for (label, overlay, on) in [
            ("Safe areas", ViewerOverlay::SafeAreas, o.safe_areas),
            ("Grid", ViewerOverlay::Grid, o.grid),
            ("Transparency grid", ViewerOverlay::Checkerboard, o.checkerboard),
        ] {
            // The viewer has no transparency to show yet: its switch greyed.
            let ready = overlay != ViewerOverlay::Checkerboard;
            row(ui, theme, label, |ui| {
                let r = ui.add_enabled_ui(ready, |ui| switch(ui, theme, on)).inner;
                if r.clicked() {
                    cmds.push(AppCommand::SetViewerOverlay { overlay, on: !on });
                }
                r.on_disabled_hover_text(crate::panels::viewer::CHECKERBOARD_NOT_YET);
            });
        }
        // One channel of the picture in grey (#2758).
        // The channels side by side where they fit beside the label, else
        // one drop-down: in a narrow inspector they drew over the label
        // and pushed the card past the panel's edge.
        row(ui, theme, "Channel", |ui| {
            let label = crate::panels::viewer::view_channel_label;
            let font = egui::FontId::proportional(12.0);
            let pad = ui.spacing().button_padding.x * 2.0 + ui.spacing().item_spacing.x;
            let wide: f32 = ui.fonts(|f| klipp_core::ViewChannel::ALL.iter().map(|c| f.layout_no_wrap(label(*c).to_owned(), font.clone(), egui::Color32::WHITE).size().x + pad).sum());
            let mut chosen = state.view_channel;
            if wide <= ui.available_width() {
                for channel in klipp_core::ViewChannel::ALL {
                    if ui.selectable_label(chosen == channel, RichText::new(label(channel)).size(12.0)).clicked() {
                        chosen = channel;
                    }
                }
            } else {
                egui::ComboBox::from_id_salt("inspector_view_channel")
                    .selected_text(RichText::new(label(chosen)).size(12.0))
                    .show_ui(ui, |ui| {
                        for channel in klipp_core::ViewChannel::ALL {
                            ui.selectable_value(&mut chosen, channel, label(channel));
                        }
                    });
            }
            if chosen != state.view_channel {
                cmds.push(AppCommand::SetViewChannel { channel: chosen });
            }
        });
        ui.add_space(4.0);
    });

    crate::widgets::inspector_group::section_card(ui, "PLAYBACK", None, theme, |ui| {
        row(ui, theme, "Use proxies", |ui| {
            if switch(ui, theme, state.use_proxies).clicked() {
                cmds.push(AppCommand::SetUseProxies(!state.use_proxies));
            }
        });
        ui.add_space(4.0);
    });

    ui.add_space(12.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("Select a clip to see its settings.").size(12.0).color(theme.text_secondary));
    });
}

/// The sequence's name, its format on one line, and its settings.
fn header(ui: &mut Ui, theme: &KlippTheme, state: &AppState, cmds: &mut Vec<AppCommand>) {
    let seq = state.sequence();
    egui::Frame::none()
        .fill(theme.bg_panel_alt)
        .rounding(6.0)
        .inner_margin(egui::Margin::symmetric(14.0, 12.0))
        .outer_margin(egui::Margin::symmetric(10.0, 8.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(&seq.name).size(15.0).strong().color(theme.text_primary));
            ui.add_space(4.0);
            ui.label(RichText::new(format_line(state)).size(12.5).color(theme.text_secondary));
            ui.label(RichText::new(colour_line(state)).size(12.5).color(theme.text_secondary));
            ui.add_space(8.0);
            if ui.button(RichText::new("Sequence Settings\u{2026}").size(12.5)).clicked() {
                cmds.push(AppCommand::OpenSequenceSettings { sequence_idx: state.active_sequence_idx });
            }
        });
}

/// "1920 × 1080 · 24 fps · 16:9 · 00:01:55:12".
pub(crate) fn format_line(state: &AppState) -> String {
    let seq = state.sequence();
    let (w, h) = (seq.resolution.width, seq.resolution.height);
    let g = gcd(w, h).max(1);
    let fps = seq.fps.num as f64 / seq.fps.den.max(1) as f64;
    let fps = if (fps - fps.round()).abs() < 0.01 { format!("{} fps", fps.round() as u32) } else { format!("{fps:.3} fps") };
    format!("{w} \u{00D7} {h} \u{00B7} {fps} \u{00B7} {}:{} \u{00B7} {}", w / g, h / g, seq.timecode_display().duration(state.sequence_duration()))
}

/// The space the sequence is graded in.
pub(crate) fn colour_line(state: &AppState) -> String {
    let space = match state.project_working_colorspace {
        klipp_core::WorkingColorspace::Rec709Linear => "Rec.709",
        klipp_core::WorkingColorspace::AcesCG => "ACEScg",
        klipp_core::WorkingColorspace::Bt2020Linear => "Rec.2020",
    };
    format!("{space} working space")
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    /// One frame of the view, with `events`: its text and its commands.
    fn frame(ctx: &egui::Context, state: &AppState, events: Vec<egui::Event>) -> (Vec<(String, egui::Pos2)>, Vec<AppCommand>) {
        let mut cmds = Vec::new();
        let theme = KlippTheme::default();
        let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(360.0, 900.0))), events, ..Default::default() }, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| show(ui, &theme, state, &mut cmds));
        });
        let texts = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos + t.galley.size() / 2.0)),
            _ => None,
        }).collect();
        (texts, cmds)
    }

    fn click(ctx: &egui::Context, state: &AppState, at: egui::Pos2) -> Vec<AppCommand> {
        let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut cmds = frame(ctx, state, vec![egui::Event::PointerMoved(at), press(true)]).1;
        cmds.extend(frame(ctx, state, vec![press(false)]).1);
        cmds
    }

    /// With nothing selected the inspector shows the sequence's real
    /// format and working space, not fixed words; the playhead, the
    /// timeline counts, the storage and the delivery are elsewhere (the
    /// viewer, the Info tab, the Export page), never here.
    #[test]
    fn the_sequence_is_shown_as_it_is() {
        let ctx = egui::Context::default();
        let state = AppState { project_working_colorspace: klipp_core::WorkingColorspace::AcesCG, ..Default::default() };
        let (texts, _) = frame(&ctx, &state, Vec::new());
        let all: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
        let res = state.sequence().resolution;
        assert!(all.iter().any(|t| t.starts_with(&format!("{} \u{00D7} {}", res.width, res.height))), "{all:?}");
        assert!(all.contains(&"ACEScg working space"), "{all:?}");
        for gone in ["PLAYHEAD POSITION", "STORAGE", "OUTPUT", "H.264 / AAC", "TIMELINE STATS"] {
            assert!(!all.iter().any(|t| t.contains(gone)), "{gone} is shown elsewhere: {all:?}");
        }
    }

    /// The switches and buttons send what they say: a viewer overlay on,
    /// proxies off, the In point cleared, the sequence's settings opened.
    #[test]
    fn the_controls_do_what_they_say() {
        let ctx = egui::Context::default();
        let mut state = AppState { use_proxies: true, ..Default::default() };
        state.sequences[state.active_sequence_idx].in_point = Some(klipp_core::RationalTime::from_secs_f64(2.0, 24));
        let (texts, _) = frame(&ctx, &state, Vec::new());
        let at = |label: &str| texts.iter().find(|(t, _)| t == label).unwrap_or_else(|| panic!("no {label}: {texts:?}")).1;
        // A switch sits at the row's right edge.
        let switch_of = |label: &str| egui::pos2(360.0 - 8.0 - 14.0 - 16.0, at(label).y);
        assert!(matches!(click(&ctx, &state, switch_of("Grid")).as_slice(), [AppCommand::SetViewerOverlay { overlay: ViewerOverlay::Grid, on: true }]));
        assert!(matches!(click(&ctx, &state, switch_of("Use proxies")).as_slice(), [AppCommand::SetUseProxies(false)]));
        assert!(matches!(click(&ctx, &state, at("\u{2715}")).as_slice(), [AppCommand::MarkRange { in_point: Some(None), out_point: None }]));
        assert!(matches!(click(&ctx, &state, at("Sequence Settings\u{2026}")).as_slice(), [AppCommand::OpenSequenceSettings { .. }]));
        assert!(texts.iter().any(|(t, _)| t == "Not set"), "the Out point unset: {texts:?}");
    }

    /// The inspector at its narrowest, as a narrow window opens it from
    /// its rail, and wider: no text over another, none past its edges.
    /// The Channel row's six buttons drew over its label and pushed the
    /// card's rows past the panel's left edge ("se proxies").
    #[test]
    fn nothing_overlaps_at_the_inspectors_narrowest() {
        // Less a scrollbar's width, too.
        for width in [crate::theme::INSPECTOR_MIN_W - 24.0, crate::theme::INSPECTOR_MIN_W, 300.0, 360.0] {
            let ctx = egui::Context::default();
            let state = AppState::default();
            let theme = KlippTheme::default();
            let mut texts = Vec::new();
            for _ in 0..3 {
                let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(width, 900.0))), ..Default::default() }, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| show(ui, &theme, &state, &mut Vec::new()));
                });
                texts = out.shapes.iter().filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) if !t.galley.text().trim().is_empty() => Some((t.galley.text().to_owned(), t.visual_bounding_rect())),
                    _ => None,
                }).collect::<Vec<_>>();
            }
            for (i, (a, ra)) in texts.iter().enumerate() {
                assert!(ra.left() >= -0.5 && ra.right() <= width + 0.5, "{a:?} {ra:?} past the edge at {width}");
                for (b, rb) in &texts[i + 1..] {
                    assert!(!ra.shrink(0.5).intersects(rb.shrink(0.5)), "{a:?} {ra:?} over {b:?} {rb:?} at {width}");
                }
            }
            assert!(texts.iter().any(|(t, _)| t == "Use proxies") && texts.iter().any(|(t, _)| t == "Channel"), "{texts:?}");
        }
    }
}
