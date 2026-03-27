use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

struct AudioFxDef {
    name: &'static str,
    desc: &'static str,
    color: egui::Color32,
}

const AUDIO_FX: &[AudioFxDef] = &[
    AudioFxDef { name: "Compressor", desc: "Dynamic range control", color: egui::Color32::from_rgb(0x3a, 0x8c, 0x6e) },
    AudioFxDef { name: "Parametric EQ", desc: "6-band equalizer", color: egui::Color32::from_rgb(0x2d, 0x6f, 0xa8) },
    AudioFxDef { name: "Limiter", desc: "Peak protection", color: egui::Color32::from_rgb(0xa8, 0x5c, 0x2d) },
    AudioFxDef { name: "Noise Reduction", desc: "Remove background noise", color: egui::Color32::from_rgb(0x7a, 0x4f, 0xa8) },
    AudioFxDef { name: "De-click", desc: "Remove pops & clicks", color: egui::Color32::from_rgb(0x7a, 0x4f, 0xa8) },
    AudioFxDef { name: "Reverb", desc: "Room & space simulation", color: egui::Color32::from_rgb(0x2d, 0x6f, 0xa8) },
    AudioFxDef { name: "Echo / Delay", desc: "Time-based repeat", color: egui::Color32::from_rgb(0x4a, 0x6e, 0xa8) },
    AudioFxDef { name: "Normalize", desc: "Set target loudness (LUFS)", color: egui::Color32::from_rgb(0x3a, 0x8c, 0x6e) },
    AudioFxDef { name: "Pitch Shift", desc: "Transpose audio pitch", color: egui::Color32::from_rgb(0xa8, 0x43, 0x2d) },
    AudioFxDef { name: "Gate", desc: "Silence low-level noise", color: egui::Color32::from_rgb(0x50, 0x50, 0x60) },
    AudioFxDef { name: "Chorus", desc: "Modulation & width", color: egui::Color32::from_rgb(0x4a, 0x6e, 0xa8) },
    AudioFxDef { name: "De-esser", desc: "Reduce sibilance", color: egui::Color32::from_rgb(0x7a, 0x4f, 0xa8) },
];

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &str,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();

    // Live preview waveform (decorative)
    egui::Frame::none()
        .fill(theme.bg_raised)
        .stroke(egui::Stroke::new(1.0, theme.border))
        .rounding(6.0)
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
        .outer_margin(egui::Margin::symmetric(8.0, 4.0))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new("LIVE PREVIEW")
                    .size(10.0)
                    .strong()
                    .color(theme.text_dim),
            );
            ui.add_space(4.0);
            // Draw decorative waveform bars
            let (bar_rect, _) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 36.0), egui::Sense::hover());
            let bar_count = (bar_rect.width() / 3.0) as usize;
            let painter = ui.painter();
            for i in 0..bar_count {
                let x = bar_rect.min.x + i as f32 * 3.0;
                // Pseudo-random bar height
                let h = ((i * 37 + 13) % 30 + 4) as f32;
                let y_center = bar_rect.center().y;
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(x, y_center - h / 2.0),
                        egui::vec2(2.0, h),
                    ),
                    1.0,
                    egui::Color32::from_rgb(0x3a, 0x8c, 0x6e),
                );
            }
        });

    // Audio effects list
    let search_lower = search_query.to_lowercase();
    egui::ScrollArea::vertical().id_salt("browser_audio_scroll").show(ui, |ui| {
        ui.add_space(4.0);
        for fx in AUDIO_FX {
            if !search_lower.is_empty() && !fx.name.to_lowercase().contains(&search_lower) {
                continue;
            }

            let resp = egui::Frame::none()
                .fill(theme.bg_raised)
                .stroke(egui::Stroke::new(1.0, theme.border))
                .rounding(6.0)
                .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                .outer_margin(egui::Margin::symmetric(8.0, 2.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // Icon
                        let icon_bg = egui::Color32::from_rgba_premultiplied(
                            fx.color.r(),
                            fx.color.g(),
                            fx.color.b(),
                            33, // ~13%
                        );
                        let icon_border = egui::Color32::from_rgba_premultiplied(
                            fx.color.r(),
                            fx.color.g(),
                            fx.color.b(),
                            69, // ~27%
                        );
                        let (icon_rect, _) =
                            ui.allocate_exact_size(egui::vec2(32.0, 32.0), egui::Sense::hover());
                        ui.painter().rect_filled(icon_rect, 6.0, icon_bg);
                        ui.painter()
                            .rect_stroke(icon_rect, 6.0, egui::Stroke::new(1.0, icon_border));
                        // Waveform icon
                        let wf_rect = egui::Rect::from_center_size(icon_rect.center(), egui::vec2(14.0, 14.0));
                        Icon::Waveform.image(14.0).tint(fx.color).paint_at(ui, wf_rect);

                        ui.add_space(6.0);

                        // Name + description
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new(fx.name)
                                    .size(11.0)
                                    .strong()
                                    .color(theme.text_primary),
                            );
                            ui.label(
                                egui::RichText::new(fx.desc)
                                    .size(10.0)
                                    .color(theme.text_dim),
                            );
                        });
                    });
                })
                .response;

            if resp.double_clicked() {
                // Map browser names to DSP effect type IDs
                let effect_type_id = match fx.name {
                    "Compressor" => Some("compressor"),
                    "Parametric EQ" => Some("eq"),
                    "Limiter" => Some("limiter"),
                    "Reverb" => Some("reverb"),
                    "Echo / Delay" => Some("delay"),
                    "Gate" => Some("gate"),
                    "De-esser" => Some("deesser"),
                    "Noise Reduction" => Some("noise_reduction"),
                    "De-click" => Some("declick"),
                    "Normalize" => Some("normalize"),
                    "Pitch Shift" => Some("pitch_shift"),
                    "Chorus" => Some("chorus"),
                    _ => None,
                };

                if let Some(type_id) = effect_type_id {
                    if let Some(clip_id) = state.selected_clip {
                        cmds.push(AppCommand::AddAudioEffect {
                            clip_id,
                            effect_type: type_id.to_string(),
                        });
                        state.toast.show(format!("\"{}\" added to clip", fx.name));
                    } else {
                        state.toast.show("Select a clip first".to_string());
                    }
                } else {
                    state.toast.show(format!(
                        "\"{}\" is not yet implemented",
                        fx.name
                    ));
                }
            }
        }
    });

    cmds
}
