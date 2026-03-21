use crate::theme::KlippTheme;
use crate::widgets::{slider, waveform};
use klipp_core::AppCommand;
use klipp_timeline::Clip;

pub fn show(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    _clip: &Clip,
    _cmds: &mut Vec<AppCommand>,
) {
    // ── Audio section ──
    section_header(ui, theme, "Audio");

    // Volume
    let mut volume = 100.0f32;
    audio_row_slider(ui, theme, "Volume", &mut volume, 0.0..=200.0, "0 dB");

    // Pan
    let mut pan = 0.0f32;
    audio_row_slider(ui, theme, "Pan", &mut pan, -100.0..=100.0, "C");

    // Fade In
    let mut fade_in = 10.0f32;
    audio_row_slider(ui, theme, "Fade In", &mut fade_in, 0.0..=100.0, "0.5s");

    // Fade Out
    let mut fade_out = 10.0f32;
    audio_row_slider(ui, theme, "Fade Out", &mut fade_out, 0.0..=100.0, "0.5s");

    ui.add_space(14.0);

    // ── Waveform section ──
    section_header(ui, theme, "Waveform");
    let wf_rect_size = egui::vec2(ui.available_width() - 20.0, 48.0);
    egui::Frame::none()
        .fill(theme.bg_raised)
        .stroke(egui::Stroke::new(1.0, theme.border))
        .rounding(4.0)
        .inner_margin(egui::Margin::symmetric(4.0, 4.0))
        .show(ui, |ui| {
            let (rect, _) = ui.allocate_exact_size(wf_rect_size, egui::Sense::hover());
            waveform::draw_role_waveform(ui.painter(), rect, theme.role_dialogue, None);
        });
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

fn audio_row_slider(
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
