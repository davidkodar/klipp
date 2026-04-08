use crate::drag_drop::DragPayload;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

/// Clip A preview colour (blue-ish)
const PREVIEW_A: egui::Color32 = egui::Color32::from_rgb(60, 80, 120);
/// Clip B preview colour (orange-ish)
const PREVIEW_B: egui::Color32 = egui::Color32::from_rgb(120, 80, 60);
/// Midpoint accent (white flash, dip colour, etc.)
#[allow(dead_code)]
const PREVIEW_MID: egui::Color32 = egui::Color32::from_rgb(220, 220, 220);

/// All data needed to register, display, and drag a transition.
struct TransitionDef {
    /// Display name shown in the browser card and inspector.
    name: &'static str,
    /// Wire ID — must match TransitionKind::to_id() and the GPU shader kind_id.
    kind_id: u8,
    /// Category for future grouping/filtering (Dissolve, Wipe, Slide, etc.)
    category: TransitionCategory,
    /// Draws a static 2-colour preview into the given rect.
    /// Use painter primitives only — no egui layout.
    preview: fn(painter: &egui::Painter, rect: egui::Rect, theme: &KlippTheme),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransitionCategory {
    Dissolve,
    Wipe,
    Slide,
    #[allow(dead_code)]
    Zoom,
    Effect,
    Cinematic,
}

impl TransitionCategory {
    fn label(self) -> &'static str {
        match self {
            Self::Dissolve => "Dissolve",
            Self::Wipe => "Wipe",
            Self::Slide => "Slide",
            Self::Zoom => "Zoom",
            Self::Effect => "Effect",
            Self::Cinematic => "Cinematic",
        }
    }
}

// ---------- Preview helpers ----------

fn preview_split_h(p: &egui::Painter, r: egui::Rect, frac: f32, a: egui::Color32, b: egui::Color32) {
    let mid = r.min.x + r.width() * frac;
    p.rect_filled(egui::Rect::from_min_max(r.min, egui::pos2(mid, r.max.y)), 0.0, a);
    p.rect_filled(egui::Rect::from_min_max(egui::pos2(mid, r.min.y), r.max), 0.0, b);
}

fn preview_split_v(p: &egui::Painter, r: egui::Rect, frac: f32, a: egui::Color32, b: egui::Color32) {
    let mid = r.min.y + r.height() * frac;
    p.rect_filled(egui::Rect::from_min_max(r.min, egui::pos2(r.max.x, mid)), 0.0, a);
    p.rect_filled(egui::Rect::from_min_max(egui::pos2(r.min.x, mid), r.max), 0.0, b);
}

#[allow(dead_code)]
fn preview_diagonal(p: &egui::Painter, r: egui::Rect, a: egui::Color32, b: egui::Color32) {
    p.rect_filled(r, 0.0, b);
    let pts = vec![r.min, egui::pos2(r.max.x, r.min.y), egui::pos2(r.min.x, r.max.y)];
    p.add(egui::Shape::convex_polygon(pts, a, egui::Stroke::NONE));
}

fn preview_gradient_h(p: &egui::Painter, r: egui::Rect, steps: usize, a: egui::Color32, b: egui::Color32) {
    for i in 0..steps {
        let t = i as f32 / steps as f32;
        let x = r.min.x + t * r.width();
        let w = r.width() / steps as f32;
        let strip = egui::Rect::from_min_size(egui::pos2(x, r.min.y), egui::vec2(w, r.height()));
        let color = egui::Color32::from_rgb(
            lerp_u8_preview(a.r(), b.r(), t),
            lerp_u8_preview(a.g(), b.g(), t),
            lerp_u8_preview(a.b(), b.b(), t),
        );
        p.rect_filled(strip, 0.0, color);
    }
}

fn preview_circle(p: &egui::Painter, r: egui::Rect, radius_frac: f32, color: egui::Color32) {
    let radius = r.height() * 0.5 * radius_frac;
    p.circle_filled(r.center(), radius, color);
}

fn lerp_u8_preview(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t) as u8
}

// ---------- Transition registry ----------

const TRANSITIONS: &[TransitionDef] = &[
    TransitionDef {
        name: "Cross Dissolve",
        kind_id: 0,
        category: TransitionCategory::Dissolve,
        preview: |p, r, _| preview_gradient_h(p, r, 8, PREVIEW_A, PREVIEW_B),
    },
    TransitionDef {
        name: "Wipe Left",
        kind_id: 1,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| preview_split_h(p, r, 0.5, PREVIEW_A, PREVIEW_B),
    },
    TransitionDef {
        name: "Wipe Right",
        kind_id: 2,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| preview_split_h(p, r, 0.5, PREVIEW_B, PREVIEW_A),
    },
    TransitionDef {
        name: "Wipe Up",
        kind_id: 3,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| preview_split_v(p, r, 0.5, PREVIEW_A, PREVIEW_B),
    },
    TransitionDef {
        name: "Wipe Down",
        kind_id: 4,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| preview_split_v(p, r, 0.5, PREVIEW_B, PREVIEW_A),
    },
    TransitionDef {
        name: "Dip to Black",
        kind_id: 5,
        category: TransitionCategory::Dissolve,
        preview: |p, r, _| {
            let third = r.width() / 3.0;
            p.rect_filled(
                egui::Rect::from_min_size(r.min, egui::vec2(third, r.height())),
                0.0,
                PREVIEW_A,
            );
            p.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(r.min.x + third, r.min.y),
                    egui::vec2(third, r.height()),
                ),
                0.0,
                egui::Color32::BLACK,
            );
            p.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(r.min.x + third * 2.0, r.min.y),
                    egui::vec2(third, r.height()),
                ),
                0.0,
                PREVIEW_B,
            );
        },
    },
    TransitionDef {
        name: "Dip to White",
        kind_id: 6,
        category: TransitionCategory::Dissolve,
        preview: |p, r, _| {
            let third = r.width() / 3.0;
            p.rect_filled(
                egui::Rect::from_min_size(r.min, egui::vec2(third, r.height())),
                0.0,
                PREVIEW_A,
            );
            p.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(r.min.x + third, r.min.y),
                    egui::vec2(third, r.height()),
                ),
                0.0,
                egui::Color32::WHITE,
            );
            p.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(r.min.x + third * 2.0, r.min.y),
                    egui::vec2(third, r.height()),
                ),
                0.0,
                PREVIEW_B,
            );
        },
    },
    TransitionDef {
        name: "Push Left",
        kind_id: 7,
        category: TransitionCategory::Slide,
        preview: |p, r, _| {
            preview_split_h(p, r, 0.5, PREVIEW_A, PREVIEW_B);
            let cx = r.center();
            p.arrow(
                egui::pos2(cx.x + 8.0, cx.y),
                egui::vec2(-16.0, 0.0),
                egui::Stroke::new(2.0, egui::Color32::WHITE),
            );
        },
    },
    TransitionDef {
        name: "Film Dissolve",
        kind_id: 8,
        category: TransitionCategory::Dissolve,
        preview: |p, r, _| preview_gradient_h(p, r, 12, PREVIEW_A, PREVIEW_B),
    },
    TransitionDef {
        name: "Blur Dissolve",
        kind_id: 9,
        category: TransitionCategory::Dissolve,
        preview: |p, r, _| {
            preview_gradient_h(p, r, 6, PREVIEW_A, PREVIEW_B);
            for i in 0..4 {
                let x = r.min.x + r.width() * (i as f32 + 1.0) / 5.0;
                p.line_segment(
                    [egui::pos2(x - 2.0, r.min.y), egui::pos2(x + 2.0, r.max.y)],
                    egui::Stroke::new(1.0, egui::Color32::from_white_alpha(40)),
                );
            }
        },
    },
    TransitionDef {
        name: "Flash",
        kind_id: 10,
        category: TransitionCategory::Effect,
        preview: |p, r, _| {
            preview_split_h(p, r, 0.5, PREVIEW_A, PREVIEW_B);
            preview_circle(p, r, 0.6, egui::Color32::from_white_alpha(180));
        },
    },
    TransitionDef {
        name: "Spin",
        kind_id: 11,
        category: TransitionCategory::Effect,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_A);
            let cx = r.center();
            let hw = r.width() * 0.3;
            let hh = r.height() * 0.3;
            let pts = vec![
                egui::pos2(cx.x - hh, cx.y - hw),
                egui::pos2(cx.x + hw, cx.y - hh),
                egui::pos2(cx.x + hh, cx.y + hw),
                egui::pos2(cx.x - hw, cx.y + hh),
            ];
            p.add(egui::Shape::convex_polygon(pts, PREVIEW_B, egui::Stroke::NONE));
        },
    },
    TransitionDef {
        name: "Circle Wipe In",
        kind_id: 14,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_A);
            let radius = r.height() * 0.45;
            p.circle_filled(r.center(), radius, PREVIEW_B);
        },
    },
    TransitionDef {
        name: "Circle Wipe Out",
        kind_id: 15,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_B);
            let radius = r.height() * 0.25;
            p.circle_filled(r.center(), radius, PREVIEW_A);
        },
    },
    TransitionDef {
        name: "Barn Door Open",
        kind_id: 17,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_B);
            let quarter = r.width() * 0.25;
            p.rect_filled(
                egui::Rect::from_min_max(r.min, egui::pos2(r.min.x + quarter, r.max.y)),
                0.0,
                PREVIEW_A,
            );
            p.rect_filled(
                egui::Rect::from_min_max(egui::pos2(r.max.x - quarter, r.min.y), r.max),
                0.0,
                PREVIEW_A,
            );
            p.line_segment(
                [egui::pos2(r.center().x, r.min.y), egui::pos2(r.center().x, r.max.y)],
                egui::Stroke::new(1.0, egui::Color32::from_white_alpha(60)),
            );
        },
    },
    TransitionDef {
        name: "Barn Door Close",
        kind_id: 18,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_A);
            let quarter = r.width() * 0.25;
            p.rect_filled(
                egui::Rect::from_min_max(r.min, egui::pos2(r.min.x + quarter, r.max.y)),
                0.0,
                PREVIEW_B,
            );
            p.rect_filled(
                egui::Rect::from_min_max(egui::pos2(r.max.x - quarter, r.min.y), r.max),
                0.0,
                PREVIEW_B,
            );
        },
    },
    TransitionDef {
        name: "Clock Wipe",
        kind_id: 16,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_A);
            let cx = r.center();
            let radius = r.height() * 0.45;
            let steps = 32usize;
            let sweep = std::f32::consts::TAU * 0.75;
            let mut pts = vec![cx];
            for i in 0..=steps {
                let angle = -std::f32::consts::FRAC_PI_2
                    + (i as f32 / steps as f32) * sweep;
                pts.push(egui::pos2(
                    cx.x + angle.cos() * radius,
                    cx.y + angle.sin() * radius,
                ));
            }
            p.add(egui::Shape::convex_polygon(pts, PREVIEW_B, egui::Stroke::NONE));
            let hand_angle = -std::f32::consts::FRAC_PI_2 + sweep;
            let hand_end = egui::pos2(
                cx.x + hand_angle.cos() * radius,
                cx.y + hand_angle.sin() * radius,
            );
            p.line_segment([cx, hand_end], egui::Stroke::new(2.0, egui::Color32::WHITE));
        },
    },
    TransitionDef {
        name: "Page Turn",
        kind_id: 21,
        category: TransitionCategory::Cinematic,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_B);
            let fold = r.min.x + r.width() * 0.6;
            p.rect_filled(
                egui::Rect::from_min_max(r.min, egui::pos2(fold, r.max.y)),
                0.0,
                PREVIEW_A,
            );
            let pts = vec![
                egui::pos2(fold, r.min.y),
                egui::pos2(r.max.x * 0.85, r.min.y),
                egui::pos2(fold, r.max.y * 0.6),
            ];
            p.add(egui::Shape::convex_polygon(
                pts,
                egui::Color32::from_rgb(80, 60, 50),
                egui::Stroke::NONE,
            ));
            p.line_segment(
                [egui::pos2(fold, r.min.y), egui::pos2(fold, r.max.y)],
                egui::Stroke::new(2.0, egui::Color32::from_black_alpha(120)),
            );
        },
    },
    TransitionDef {
        name: "Page Turn Reverse",
        kind_id: 22,
        category: TransitionCategory::Cinematic,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_B);
            let fold = r.min.x + r.width() * 0.4;
            p.rect_filled(
                egui::Rect::from_min_max(egui::pos2(fold, r.min.y), r.max),
                0.0,
                PREVIEW_A,
            );
            let pts = vec![
                egui::pos2(fold, r.min.y),
                egui::pos2(r.min.x + r.width() * 0.15, r.min.y),
                egui::pos2(fold, r.max.y * 0.6),
            ];
            p.add(egui::Shape::convex_polygon(
                pts,
                egui::Color32::from_rgb(80, 60, 50),
                egui::Stroke::NONE,
            ));
            p.line_segment(
                [egui::pos2(fold, r.min.y), egui::pos2(fold, r.max.y)],
                egui::Stroke::new(2.0, egui::Color32::from_black_alpha(120)),
            );
        },
    },
    TransitionDef {
        name: "Ripple",
        kind_id: 20,
        category: TransitionCategory::Effect,
        preview: |p, r, _| {
            preview_gradient_h(p, r, 8, PREVIEW_A, PREVIEW_B);
            let cx = r.center();
            for i in 1..=4i32 {
                let radius = i as f32 * r.height() * 0.12;
                let alpha = (40 + i * 15).min(255) as u8;
                p.circle_stroke(
                    cx,
                    radius,
                    egui::Stroke::new(1.0, egui::Color32::from_white_alpha(alpha)),
                );
            }
        },
    },
    TransitionDef {
        name: "Pixelate",
        kind_id: 19,
        category: TransitionCategory::Effect,
        preview: |p, r, _| {
            let block = 8.0;
            let cols = (r.width() / block) as usize;
            let rows = (r.height() / block) as usize;
            for row in 0..rows {
                for col in 0..cols {
                    let t = col as f32 / cols.max(1) as f32;
                    let color = egui::Color32::from_rgb(
                        lerp_u8_preview(PREVIEW_A.r(), PREVIEW_B.r(), t),
                        lerp_u8_preview(PREVIEW_A.g(), PREVIEW_B.g(), t),
                        lerp_u8_preview(PREVIEW_A.b(), PREVIEW_B.b(), t),
                    );
                    let rect = egui::Rect::from_min_size(
                        egui::pos2(
                            r.min.x + col as f32 * block,
                            r.min.y + row as f32 * block,
                        ),
                        egui::vec2(block - 1.0, block - 1.0),
                    );
                    p.rect_filled(rect, 0.0, color);
                }
            }
        },
    },
    TransitionDef {
        name: "Luminance Dissolve",
        kind_id: 13,
        category: TransitionCategory::Dissolve,
        preview: |p, r, _| {
            // Gradient from dark (A) toward bright, with B bleeding through brights.
            for i in 0..8 {
                let t = i as f32 / 8.0;
                let x = r.min.x + t * r.width();
                let strip = egui::Rect::from_min_size(
                    egui::pos2(x, r.min.y),
                    egui::vec2(r.width() / 8.0, r.height()),
                );
                let brightness = (t * 255.0) as u8;
                let a = egui::Color32::from_rgb(
                    lerp_u8_preview(PREVIEW_A.r(), brightness, t),
                    lerp_u8_preview(PREVIEW_A.g(), brightness, t),
                    lerp_u8_preview(PREVIEW_A.b(), brightness, t),
                );
                p.rect_filled(strip, 0.0, a);
            }
            for i in 4..8 {
                let t = (i - 4) as f32 / 4.0;
                let x = r.min.x + i as f32 * r.width() / 8.0;
                let strip = egui::Rect::from_min_size(
                    egui::pos2(x, r.min.y),
                    egui::vec2(r.width() / 8.0, r.height()),
                );
                p.rect_filled(
                    strip,
                    0.0,
                    egui::Color32::from_rgba_unmultiplied(
                        PREVIEW_B.r(),
                        PREVIEW_B.g(),
                        PREVIEW_B.b(),
                        (t * 180.0) as u8,
                    ),
                );
            }
        },
    },
    TransitionDef {
        name: "Light Leak",
        kind_id: 12,
        category: TransitionCategory::Cinematic,
        preview: |p, r, _| {
            preview_gradient_h(p, r, 8, PREVIEW_A, PREVIEW_B);
            preview_circle(p, r, 0.4, egui::Color32::from_rgba_unmultiplied(255, 180, 60, 120));
        },
    },
];

// ---------- UI ----------

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &str,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let search_lower = search_query.to_lowercase();

    // Category filter state (persisted in ui memory)
    let filter_id = egui::Id::new("transition_category_filter");
    let mut active_category: Option<TransitionCategory> =
        ui.memory_mut(|m| m.data.get_temp(filter_id));

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if ui.selectable_label(active_category.is_none(), "All").clicked() {
            active_category = None;
        }
        for cat in [
            TransitionCategory::Dissolve,
            TransitionCategory::Wipe,
            TransitionCategory::Slide,
            TransitionCategory::Zoom,
            TransitionCategory::Effect,
            TransitionCategory::Cinematic,
        ] {
            if ui
                .selectable_label(active_category == Some(cat), cat.label())
                .clicked()
            {
                active_category = if active_category == Some(cat) { None } else { Some(cat) };
            }
        }
    });

    ui.memory_mut(|m| m.data.insert_temp(filter_id, active_category));

    ui.add_space(4.0);

    let filtered: Vec<_> = TRANSITIONS
        .iter()
        .filter(|t| {
            let matches_search =
                search_lower.is_empty() || t.name.to_lowercase().contains(&search_lower);
            let matches_category = active_category.map_or(true, |cat| t.category == cat);
            matches_search && matches_category
        })
        .collect();

    egui::ScrollArea::vertical()
        .id_salt("browser_transitions_scroll")
        .show(ui, |ui| {
            let avail_w = ui.available_width() - 16.0;
            let card_w = (avail_w - 6.0) / 2.0;

            egui::Grid::new("transitions_grid")
                .num_columns(2)
                .spacing([6.0, 6.0])
                .min_col_width(card_w)
                .show(ui, |ui| {
                    for (i, tr) in filtered.iter().enumerate() {
                        let resp = transition_card(ui, tr, theme, card_w);
                        if resp.drag_started() {
                            state.drag_drop.begin(DragPayload::Transition {
                                name: tr.name.to_string(),
                                kind_id: tr.kind_id,
                            });
                        }
                        if resp.double_clicked() {
                            if let Some(clip_id) = state.selected_clip {
                                let seq = state.sequence();
                                let mut applied = false;
                                for (ti, track) in seq.tracks.iter().enumerate() {
                                    if let Some(pos) =
                                        track.clips.iter().position(|c| c.id == clip_id)
                                    {
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
                                                    duration_frames: state
                                                        .default_transition_frames,
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
                                    state.toast.show(String::from(
                                        "Select a clip with an adjacent cut point",
                                    ));
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

fn transition_card(
    ui: &mut egui::Ui,
    tr: &TransitionDef,
    theme: &KlippTheme,
    width: f32,
) -> egui::Response {
    let preview_h = 48.0;
    let label_h = 20.0;
    let total_h = preview_h + label_h;

    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(width, total_h), egui::Sense::click_and_drag());

    if ui.is_rect_visible(rect) {
        let hovered = resp.hovered();
        let bg = if hovered { theme.bg_hover } else { theme.bg_raised };
        let border_color = if hovered { theme.accent_dim } else { theme.border };

        let painter = ui.painter();
        painter.rect_filled(rect, 6.0, bg);
        painter.rect_stroke(rect, 6.0, egui::Stroke::new(1.0, border_color));

        // Preview area — black backdrop then inline preview, clipped to preview rect.
        let preview_rect = egui::Rect::from_min_size(rect.min, egui::vec2(width, preview_h));
        painter.rect_filled(
            preview_rect,
            egui::Rounding { nw: 6.0, ne: 6.0, sw: 0.0, se: 0.0 },
            egui::Color32::BLACK,
        );
        let clip_painter = painter.with_clip_rect(preview_rect);
        (tr.preview)(&clip_painter, preview_rect, theme);

        // Name label
        let label_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x, rect.min.y + preview_h),
            egui::vec2(width, label_h),
        );
        painter.text(
            label_rect.center(),
            egui::Align2::CENTER_CENTER,
            tr.name,
            egui::FontId::proportional(10.0),
            if hovered { theme.text_primary } else { theme.text_secondary },
        );
    }

    resp
}
