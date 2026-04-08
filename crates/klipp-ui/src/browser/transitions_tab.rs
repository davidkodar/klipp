use crate::drag_drop::DragPayload;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

struct TransitionDef {
    name: &'static str,
    kind_id: u8,
}

const TRANSITIONS: &[TransitionDef] = &[
    TransitionDef { name: "Cross Dissolve", kind_id: 0 },
    TransitionDef { name: "Dip to Black", kind_id: 5 },
    TransitionDef { name: "Dip to White", kind_id: 6 },
    TransitionDef { name: "Push Left", kind_id: 7 },
    TransitionDef { name: "Film Dissolve", kind_id: 8 },
    TransitionDef { name: "Blur Dissolve", kind_id: 9 },
    TransitionDef { name: "Flash", kind_id: 10 },
    TransitionDef { name: "Spin", kind_id: 11 },
    TransitionDef { name: "Light Leak", kind_id: 12 },
    TransitionDef { name: "Wipe Left", kind_id: 1 },
    TransitionDef { name: "Wipe Right", kind_id: 2 },
    TransitionDef { name: "Wipe Up", kind_id: 3 },
    TransitionDef { name: "Wipe Down", kind_id: 4 },
];

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &str,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let search_lower = search_query.to_lowercase();

    let filtered: Vec<_> = TRANSITIONS
        .iter()
        .filter(|t| search_lower.is_empty() || t.name.to_lowercase().contains(&search_lower))
        .collect();

    egui::ScrollArea::vertical().id_salt("browser_transitions_scroll").show(ui, |ui| {
        let avail_w = ui.available_width() - 16.0;
        let card_w = (avail_w - 6.0) / 2.0;

        egui::Grid::new("transitions_grid")
            .num_columns(2)
            .spacing([6.0, 6.0])
            .min_col_width(card_w)
            .show(ui, |ui| {
                for (i, tr) in filtered.iter().enumerate() {
                    let resp = transition_card(ui, theme, tr.name, card_w);
                    if resp.drag_started() {
                        state.drag_drop.begin(DragPayload::Transition {
                            name: tr.name.to_string(),
                            kind_id: tr.kind_id,
                        });
                    }
                    if resp.double_clicked() {
                        // Find selected clip and apply transition at its nearest cut point
                        if let Some(clip_id) = state.selected_clip {
                            let seq = state.sequence();
                            let mut applied = false;
                            for (ti, track) in seq.tracks.iter().enumerate() {
                                if let Some(pos) = track.clips.iter().position(|c| c.id == clip_id) {
                                    // Try cut point after this clip
                                    if pos + 1 < track.clips.len() {
                                        let next_id = track.clips[pos + 1].id;
                                        let already = track.transitions.iter().any(|t| {
                                            t.clip_a_id == clip_id && t.clip_b_id == next_id
                                        });
                                        if !already {
                                            cmds.push(AppCommand::AddTransition {
                                                track_index: ti,
                                                clip_a_id: clip_id,
                                                clip_b_id: next_id,
                                                kind_id: tr.kind_id,
                                                duration_frames: state.default_transition_frames,
                                            });
                                            applied = true;
                                        }
                                    }
                                    break;
                                }
                            }
                            if applied {
                                state.toast.show(format!("\"{}\" applied", tr.name));
                            } else {
                                state.toast.show(String::from("Select a clip with an adjacent cut point"));
                            }
                        } else {
                            state.toast.show(String::from("Select a clip first"));
                        }
                    }
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
    });

    cmds
}

fn transition_card(ui: &mut egui::Ui, theme: &KlippTheme, name: &str, width: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, 68.0),
        egui::Sense::click_and_drag(),
    );

    let hovered = resp.hovered();
    let border_color = if hovered { theme.accent_dim } else { theme.border };

    ui.painter().rect_filled(rect, 6.0, theme.bg_raised);
    ui.painter()
        .rect_stroke(rect, 6.0, egui::Stroke::new(1.0, border_color));

    // Preview area (48px)
    let preview_rect = egui::Rect::from_min_size(rect.min, egui::vec2(width, 48.0));
    ui.painter().rect_filled(
        preview_rect,
        egui::Rounding { nw: 6.0, ne: 6.0, sw: 0.0, se: 0.0 },
        egui::Color32::BLACK,
    );
    paint_transition_preview(ui.painter(), preview_rect, name, theme);

    // Transition name
    let name_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x, rect.min.y + 48.0),
        egui::vec2(width, 20.0),
    );
    ui.painter().text(
        name_rect.center(),
        egui::Align2::CENTER_CENTER,
        name,
        egui::FontId::proportional(10.0),
        theme.text_dim,
    );

    resp
}

fn paint_transition_preview(
    painter: &egui::Painter,
    rect: egui::Rect,
    name: &str,
    theme: &KlippTheme,
) {
    match name {
        "Cross Dissolve" => {
            let half = rect.width() / 2.0;
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min, egui::vec2(half, rect.height())),
                0.0,
                egui::Color32::from_rgba_premultiplied(60, 80, 120, 128),
            );
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + half, rect.min.y),
                    egui::vec2(half, rect.height()),
                ),
                0.0,
                egui::Color32::from_rgba_premultiplied(120, 80, 60, 128),
            );
        }
        "Dip to Black" => {
            let third = rect.width() / 3.0;
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min, egui::vec2(third, rect.height())),
                0.0,
                egui::Color32::from_rgb(0x22, 0x22, 0x22),
            );
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + third, rect.min.y),
                    egui::vec2(third, rect.height()),
                ),
                0.0,
                egui::Color32::BLACK,
            );
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + third * 2.0, rect.min.y),
                    egui::vec2(third, rect.height()),
                ),
                0.0,
                egui::Color32::from_rgb(0x22, 0x22, 0x22),
            );
        }
        "Push Left" => {
            let half = rect.width() / 2.0;
            let blue = egui::Color32::from_rgb(60, 80, 120);
            let orange = egui::Color32::from_rgb(120, 80, 60);
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min, egui::vec2(half * 0.6, rect.height())),
                0.0, blue,
            );
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + half * 0.6, rect.min.y),
                    egui::vec2(half * 1.4, rect.height()),
                ),
                0.0, orange,
            );
            painter.text(
                rect.center(), egui::Align2::CENTER_CENTER,
                "\u{2190}", egui::FontId::proportional(20.0),
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 140),
            );
        }
        "Film Dissolve" => {
            // Gradient blend preview
            let steps = 8;
            let step_w = rect.width() / steps as f32;
            for i in 0..steps {
                let t = i as f32 / (steps - 1) as f32;
                let r = (60.0 * (1.0 - t) + 120.0 * t) as u8;
                let g = (80.0 * (1.0 - t) + 80.0 * t) as u8;
                let b = (120.0 * (1.0 - t) + 60.0 * t) as u8;
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(rect.min.x + i as f32 * step_w, rect.min.y),
                        egui::vec2(step_w + 1.0, rect.height()),
                    ),
                    0.0, egui::Color32::from_rgb(r, g, b),
                );
            }
        }
        "Blur Dissolve" => {
            // Blurry gradient blend
            let steps = 6;
            let step_w = rect.width() / steps as f32;
            for i in 0..steps {
                let t = i as f32 / (steps - 1) as f32;
                let alpha = if t < 0.5 { (180.0 - t * 200.0) as u8 } else { (80.0 + (t - 0.5) * 200.0) as u8 };
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(rect.min.x + i as f32 * step_w, rect.min.y),
                        egui::vec2(step_w + 1.0, rect.height()),
                    ),
                    0.0, egui::Color32::from_rgba_premultiplied(100, 100, 120, alpha),
                );
            }
        }
        "Wipe Left" | "Wipe Right" => {
            let split = rect.width() * 0.5;
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min, egui::vec2(split, rect.height())),
                0.0, egui::Color32::from_rgb(60, 80, 120),
            );
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + split, rect.min.y),
                    egui::vec2(rect.width() - split, rect.height()),
                ),
                0.0, egui::Color32::from_rgb(120, 80, 60),
            );
        }
        "Wipe Up" | "Wipe Down" => {
            let split = rect.height() * 0.5;
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), split)),
                0.0, egui::Color32::from_rgb(60, 80, 120),
            );
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.min.x, rect.min.y + split),
                    egui::vec2(rect.width(), rect.height() - split),
                ),
                0.0, egui::Color32::from_rgb(120, 80, 60),
            );
        }
        "Dip to White" => {
            let third = rect.width() / 3.0;
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min, egui::vec2(third, rect.height())),
                0.0,
                egui::Color32::from_rgb(0x88, 0x88, 0x88),
            );
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + third, rect.min.y),
                    egui::vec2(third, rect.height()),
                ),
                0.0,
                egui::Color32::WHITE,
            );
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + third * 2.0, rect.min.y),
                    egui::vec2(third, rect.height()),
                ),
                0.0,
                egui::Color32::from_rgb(0x88, 0x88, 0x88),
            );
        }
        "Flash" => {
            painter.circle_filled(
                rect.center(),
                rect.height() * 0.35,
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 200),
            );
        }
        "Spin" => {
            painter.circle_stroke(
                rect.center(),
                8.0,
                egui::Stroke::new(2.0, egui::Color32::from_rgba_premultiplied(255, 255, 255, 100)),
            );
        }
        "Light Leak" => {
            painter.circle_filled(
                egui::pos2(rect.min.x + rect.width() * 0.3, rect.center().y),
                rect.height() * 0.4,
                egui::Color32::from_rgba_premultiplied(255, 200, 80, 120),
            );
        }
        _ => {
            painter.rect_filled(rect, 0.0, theme.bg_panel);
        }
    }
}
