use crate::drag_drop::DragPayload;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use klipp_timeline::{
    FontStyle, TextAlignment, TitleBackground, TitleData, TitlePosition, TitleShadow,
};

struct TitleTemplate {
    name: &'static str,
    preview_text: &'static str,
    create_data: fn() -> TitleData,
}

fn lower_third() -> TitleData {
    TitleData {
        text: "Name \u{00b7} Title".into(),
        font_size: 36.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Regular,
        color: [255, 255, 255, 255],
        alignment: TextAlignment::Left,
        position: TitlePosition { x: 0.15, y: 0.85 },
        background: TitleBackground::Color {
            color: [0, 0, 0, 180],
            padding: 12.0,
        },
        shadow: None,
    }
}

fn full_title() -> TitleData {
    TitleData {
        text: "Chapter One".into(),
        font_size: 96.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Regular,
        color: [255, 255, 255, 255],
        alignment: TextAlignment::Center,
        position: TitlePosition { x: 0.5, y: 0.5 },
        background: TitleBackground::None,
        shadow: Some(TitleShadow {
            color: [0, 0, 0, 160],
            offset_x: 3.0,
            offset_y: 3.0,
            blur: 6.0,
        }),
    }
}

fn subtitle() -> TitleData {
    TitleData {
        text: "A Documentary Film".into(),
        font_size: 28.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Regular,
        color: [255, 255, 255, 255],
        alignment: TextAlignment::Center,
        position: TitlePosition { x: 0.5, y: 0.9 },
        background: TitleBackground::Color {
            color: [0, 0, 0, 153],
            padding: 10.0,
        },
        shadow: None,
    }
}

fn credits() -> TitleData {
    TitleData {
        text: "DIRECTED BY".into(),
        font_size: 48.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Regular,
        color: [255, 255, 255, 255],
        alignment: TextAlignment::Center,
        position: TitlePosition { x: 0.5, y: 0.5 },
        background: TitleBackground::None,
        shadow: None,
    }
}

fn kinetic() -> TitleData {
    TitleData {
        text: "BREAKING".into(),
        font_size: 72.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Bold,
        color: [255, 60, 60, 255],
        alignment: TextAlignment::Center,
        position: TitlePosition { x: 0.5, y: 0.5 },
        background: TitleBackground::None,
        shadow: Some(TitleShadow {
            color: [0, 0, 0, 180],
            offset_x: 2.0,
            offset_y: 2.0,
            blur: 4.0,
        }),
    }
}

fn outline() -> TitleData {
    TitleData {
        text: "OUTLINE".into(),
        font_size: 72.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Bold,
        color: [255, 255, 255, 255],
        alignment: TextAlignment::Center,
        position: TitlePosition { x: 0.5, y: 0.5 },
        background: TitleBackground::None,
        shadow: Some(TitleShadow {
            color: [0, 0, 0, 200],
            offset_x: 0.0,
            offset_y: 0.0,
            blur: 3.0,
        }),
    }
}

fn minimal_bar() -> TitleData {
    TitleData {
        text: "Interview".into(),
        font_size: 24.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Regular,
        color: [255, 255, 255, 255],
        alignment: TextAlignment::Left,
        position: TitlePosition { x: 0.08, y: 0.88 },
        background: TitleBackground::Color {
            color: [200, 160, 40, 200],
            padding: 8.0,
        },
        shadow: None,
    }
}

fn chapter_card() -> TitleData {
    TitleData {
        text: "PART I".into(),
        font_size: 64.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Regular,
        color: [255, 195, 50, 255],
        alignment: TextAlignment::Center,
        position: TitlePosition { x: 0.5, y: 0.5 },
        background: TitleBackground::FullFrame {
            color: [20, 20, 25, 240],
        },
        shadow: None,
    }
}

const TITLES: &[TitleTemplate] = &[
    TitleTemplate { name: "Lower Third", preview_text: "Name \u{00b7} Title", create_data: lower_third },
    TitleTemplate { name: "Full Title", preview_text: "Chapter One", create_data: full_title },
    TitleTemplate { name: "Subtitle", preview_text: "A Documentary Film", create_data: subtitle },
    TitleTemplate { name: "Credits", preview_text: "DIRECTED BY", create_data: credits },
    TitleTemplate { name: "Kinetic", preview_text: "BREAKING", create_data: kinetic },
    TitleTemplate { name: "Outline", preview_text: "OUTLINE", create_data: outline },
    TitleTemplate { name: "Minimal Bar", preview_text: "Interview", create_data: minimal_bar },
    TitleTemplate { name: "Chapter Card", preview_text: "PART I", create_data: chapter_card },
];

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &str,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let search_lower = search_query.to_lowercase();

    let filtered: Vec<_> = TITLES
        .iter()
        .filter(|t| search_lower.is_empty() || t.name.to_lowercase().contains(&search_lower))
        .collect();

    egui::ScrollArea::vertical().id_salt("browser_titles_scroll").show(ui, |ui| {
        let avail_w = ui.available_width() - 16.0;
        let card_w = (avail_w - 6.0) / 2.0;

        egui::Grid::new("titles_grid")
            .num_columns(2)
            .spacing([6.0, 6.0])
            .min_col_width(card_w)
            .show(ui, |ui| {
                for (i, title) in filtered.iter().enumerate() {
                    let resp = title_card(ui, theme, title, card_w);

                    // Double-click: create title clip at playhead
                    if resp.double_clicked() {
                        let title_data = (title.create_data)();
                        state.pending_title_data = Some(title_data);
                        // Find a free video track or create one (same logic as Ctrl+T)
                        let ph = state.playhead;
                        let track_index = state.sequences[state.active_sequence_idx].tracks.iter()
                            .enumerate()
                            .find(|(_, t)| {
                                t.kind == klipp_timeline::TrackKind::Video
                                    && t.clip_at_timecode(ph).is_none()
                            })
                            .map(|(idx, _)| idx);
                        let track_index = match track_index {
                            Some(idx) => idx,
                            None => {
                                let track_num = state.sequences[state.active_sequence_idx].tracks.iter()
                                    .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                                    .count() + 1;
                                let track = klipp_timeline::Track::new(
                                    &format!("Video {track_num}"),
                                    klipp_timeline::TrackKind::Video,
                                );
                                state.sequences[state.active_sequence_idx].tracks.insert(0, track);
                                0
                            }
                        };
                        let fps_rate = state.sequences[state.active_sequence_idx].fps.rate();
                        let duration_frames = fps_rate * 5;
                        cmds.push(AppCommand::CreateTitleClip {
                            track_index,
                            timeline_in: ph,
                            duration_frames,
                        });
                        state.toast.show(format!(
                            "\"{}\" title added to timeline",
                            title.name
                        ));
                    }

                    // Drag: begin title drag
                    if resp.drag_started() {
                        let data = (title.create_data)();
                        state.drag_drop.begin(DragPayload::Title {
                            template_name: title.name.to_string(),
                            title_data: data,
                        });
                    }

                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
    });

    cmds
}

fn title_card(ui: &mut egui::Ui, theme: &KlippTheme, title: &TitleTemplate, width: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, 76.0),
        egui::Sense::click_and_drag(),
    );

    let hovered = resp.hovered();
    let border_color = if hovered { theme.accent_dim } else { theme.border };

    // Card background
    ui.painter().rect_filled(rect, 6.0, theme.bg_raised);
    ui.painter()
        .rect_stroke(rect, 6.0, egui::Stroke::new(1.0, border_color));

    // Preview area (top 56px, black background)
    let preview_rect = egui::Rect::from_min_size(rect.min, egui::vec2(width, 56.0));
    ui.painter().rect_filled(
        preview_rect,
        egui::Rounding { nw: 6.0, ne: 6.0, sw: 0.0, se: 0.0 },
        egui::Color32::BLACK,
    );

    // Render title preview
    paint_title_preview(ui.painter(), preview_rect, title, theme);

    // Title name
    let name_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x, rect.min.y + 56.0),
        egui::vec2(width, 20.0),
    );
    ui.painter().text(
        name_rect.center(),
        egui::Align2::CENTER_CENTER,
        title.name,
        egui::FontId::proportional(10.0),
        theme.text_dim,
    );

    resp
}

fn paint_title_preview(
    painter: &egui::Painter,
    rect: egui::Rect,
    title: &TitleTemplate,
    theme: &KlippTheme,
) {
    match title.name {
        "Lower Third" => {
            // Gradient at bottom
            let grad_rect = egui::Rect::from_min_max(
                egui::pos2(rect.min.x, rect.max.y - 20.0),
                rect.max,
            );
            painter.rect_filled(grad_rect, 0.0, egui::Color32::from_rgba_premultiplied(0, 0, 0, 180));
            painter.text(
                egui::pos2(rect.min.x + 8.0, rect.max.y - 10.0),
                egui::Align2::LEFT_CENTER,
                title.preview_text,
                egui::FontId::proportional(11.0),
                egui::Color32::WHITE,
            );
        }
        "Full Title" => {
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgba_premultiplied(0, 0, 0, 200));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                title.preview_text,
                egui::FontId::proportional(14.0),
                egui::Color32::WHITE,
            );
        }
        "Subtitle" => {
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgba_premultiplied(0, 0, 0, 153));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                title.preview_text,
                egui::FontId::proportional(10.0),
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 180),
            );
        }
        "Credits" => {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                title.preview_text,
                egui::FontId::proportional(9.0),
                egui::Color32::WHITE,
            );
        }
        "Kinetic" => {
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(0x0a, 0x0a, 0x0c));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                title.preview_text,
                egui::FontId::proportional(13.0),
                theme.accent,
            );
        }
        "Outline" => {
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(0x11, 0x11, 0x11));
            // Outlined text approximation
            let center = rect.center();
            for dx in [-1.0f32, 0.0, 1.0] {
                for dy in [-1.0f32, 0.0, 1.0] {
                    if dx == 0.0 && dy == 0.0 { continue; }
                    painter.text(
                        egui::pos2(center.x + dx, center.y + dy),
                        egui::Align2::CENTER_CENTER,
                        title.preview_text,
                        egui::FontId::proportional(12.0),
                        egui::Color32::WHITE,
                    );
                }
            }
            painter.text(
                center,
                egui::Align2::CENTER_CENTER,
                title.preview_text,
                egui::FontId::proportional(12.0),
                egui::Color32::BLACK,
            );
        }
        "Minimal Bar" => {
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgba_premultiplied(0, 0, 0, 180));
            // Accent bar
            let bar_y = rect.center().y - 6.0;
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.center().x - 20.0, bar_y),
                    egui::vec2(40.0, 2.0),
                ),
                0.0,
                theme.accent,
            );
            painter.text(
                egui::pos2(rect.center().x, bar_y + 12.0),
                egui::Align2::CENTER_CENTER,
                title.preview_text,
                egui::FontId::proportional(10.0),
                egui::Color32::WHITE,
            );
        }
        "Chapter Card" => {
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(0x0d, 0x1e, 0x30));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                title.preview_text,
                egui::FontId::proportional(9.0),
                theme.accent,
            );
        }
        _ => {}
    }
}
