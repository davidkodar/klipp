use crate::theme::KlippTheme;
use crate::widgets::{role_pill, slider};
use klipp_core::AppCommand;
use klipp_timeline::Clip;

pub fn show(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip: &Clip,
    cmds: &mut Vec<AppCommand>,
) {
    // ── Role section ──
    section_header(ui, theme, "Role");
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
        let roles = [
            ("Video", theme.role_video),
            ("Dialogue", theme.role_dialogue),
            ("Music", theme.role_music),
            ("SFX", theme.role_sfx),
            ("B-Roll", theme.role_broll),
        ];
        // Determine active role (placeholder — no role field on Clip yet)
        let active_idx = 0usize; // Default to Video
        for (i, (label, color)) in roles.iter().enumerate() {
            if role_pill::role_pill(ui, theme, label, *color, i == active_idx) {
                // Would set clip role here
                let _ = cmds;
            }
        }
    });
    ui.add_space(14.0);

    // ── Transform section ──
    section_header(ui, theme, "Transform");

    // Position X
    inspector_row_value(ui, theme, "Position X", "0.0");
    // Position Y
    inspector_row_value(ui, theme, "Position Y", "0.0");

    // Scale
    let mut scale_val = 100.0f32;
    let scale_display = format!("{:.0}%", scale_val);
    inspector_row_slider(ui, theme, "Scale", &mut scale_val, 10.0..=300.0, &scale_display);

    // Rotation
    let mut rotation = 0.0f32;
    let rot_display = format!("{:.0}\u{00b0}", rotation);
    inspector_row_slider(ui, theme, "Rotation", &mut rotation, -180.0..=180.0, &rot_display);

    // Opacity
    let _ = clip; // clip used for future property binding
    let mut opacity = 100.0f32;
    let opacity_display = format!("{:.0}%", opacity);
    inspector_row_slider(ui, theme, "Opacity", &mut opacity, 0.0..=100.0, &opacity_display);

    ui.add_space(14.0);

    // ── Speed section ──
    section_header(ui, theme, "Speed");

    let mut rate = 100.0f32;
    let rate_display = format!("{:.0}%", rate);
    inspector_row_slider(ui, theme, "Rate", &mut rate, 10.0..=400.0, &rate_display);
    inspector_row_value(ui, theme, "Direction", "Forward");
}

fn section_header(ui: &mut egui::Ui, theme: &KlippTheme, label: &str) {
    ui.label(
        egui::RichText::new(label.to_uppercase())
            .size(10.0)
            .strong()
            .color(theme.text_dim),
    );
    ui.add_space(7.0);
}

fn inspector_row_value(ui: &mut egui::Ui, theme: &KlippTheme, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            egui::RichText::new(label)
                .size(11.0)
                .color(theme.text_secondary),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            egui::Frame::none()
                .fill(theme.bg_raised)
                .stroke(egui::Stroke::new(1.0, theme.border))
                .rounding(4.0)
                .inner_margin(egui::Margin::symmetric(7.0, 3.0))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(value)
                            .size(11.0)
                            .monospace()
                            .color(theme.text_primary),
                    );
                });
        });
    });
    ui.add_space(5.0);
}

fn inspector_row_slider(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    display: &str,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.add_sized(
            [64.0, 16.0],
            egui::Label::new(
                egui::RichText::new(label)
                    .size(11.0)
                    .color(theme.text_secondary),
            ),
        );
        let slider_w = (ui.available_width() - 52.0).max(40.0);
        slider::styled_slider(ui, theme, value, range, slider_w);
        // Value box
        egui::Frame::none()
            .fill(theme.bg_raised)
            .stroke(egui::Stroke::new(1.0, theme.border))
            .rounding(4.0)
            .inner_margin(egui::Margin::symmetric(5.0, 3.0))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(display)
                        .size(10.0)
                        .monospace()
                        .color(theme.text_primary),
                );
            });
    });
    ui.add_space(5.0);
}
