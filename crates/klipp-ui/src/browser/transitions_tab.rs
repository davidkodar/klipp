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
/// Display name and kind_id are looked up from the registry via `spec_for_kind`.
struct TransitionDef {
    /// Which transition this entry represents.
    kind: klipp_core::TransitionKind,
    /// Category for grouping/filtering (Dissolve, Wipe, Slide, etc.)
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
        kind: klipp_core::TransitionKind::CrossDissolve,
        category: TransitionCategory::Dissolve,
        preview: |p, r, _| preview_gradient_h(p, r, 8, PREVIEW_A, PREVIEW_B),
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::WipeLeft,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| preview_split_h(p, r, 0.5, PREVIEW_A, PREVIEW_B),
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::WipeRight,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| preview_split_h(p, r, 0.5, PREVIEW_B, PREVIEW_A),
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::WipeUp,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| preview_split_v(p, r, 0.5, PREVIEW_A, PREVIEW_B),
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::WipeDown,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| preview_split_v(p, r, 0.5, PREVIEW_B, PREVIEW_A),
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::DipToBlack,
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
        kind: klipp_core::TransitionKind::DipToWhite,
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
        kind: klipp_core::TransitionKind::PushLeft,
        category: TransitionCategory::Slide,
        preview: |p, r, _| {
            preview_split_h(p, r, 0.5, PREVIEW_A, PREVIEW_B);
            let cx = r.center();
            p.arrow(
                egui::pos2(cx.x + 8.0, cx.y),
                egui::vec2(-16.0, 0.0),
                egui::Stroke::new(2.0_f32, egui::Color32::WHITE),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::FilmDissolve,
        category: TransitionCategory::Dissolve,
        preview: |p, r, _| preview_gradient_h(p, r, 12, PREVIEW_A, PREVIEW_B),
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::BlurDissolve,
        category: TransitionCategory::Dissolve,
        preview: |p, r, _| {
            preview_gradient_h(p, r, 6, PREVIEW_A, PREVIEW_B);
            for i in 0..4 {
                let x = r.min.x + r.width() * (i as f32 + 1.0) / 5.0;
                p.line_segment(
                    [egui::pos2(x - 2.0, r.min.y), egui::pos2(x + 2.0, r.max.y)],
                    egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(40)),
                );
            }
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::Flash,
        category: TransitionCategory::Effect,
        preview: |p, r, _| {
            preview_split_h(p, r, 0.5, PREVIEW_A, PREVIEW_B);
            preview_circle(p, r, 0.6, egui::Color32::from_white_alpha(180));
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::Spin,
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
        kind: klipp_core::TransitionKind::CircleWipeIn,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_A);
            let radius = r.height() * 0.45;
            p.circle_filled(r.center(), radius, PREVIEW_B);
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::CircleWipeOut,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_B);
            let radius = r.height() * 0.25;
            p.circle_filled(r.center(), radius, PREVIEW_A);
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::BarnDoorOpen,
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
                egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(60)),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::BarnDoorClose,
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
        kind: klipp_core::TransitionKind::SlideLeft,
        category: TransitionCategory::Slide,
        preview: |p, r, _| {
            preview_split_h(p, r, 0.6, PREVIEW_A, PREVIEW_B);
            p.arrow(
                egui::pos2(r.center().x + 10.0, r.center().y),
                egui::vec2(-20.0, 0.0),
                egui::Stroke::new(2.0_f32, egui::Color32::WHITE),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::SlideRight,
        category: TransitionCategory::Slide,
        preview: |p, r, _| {
            preview_split_h(p, r, 0.4, PREVIEW_B, PREVIEW_A);
            p.arrow(
                egui::pos2(r.center().x - 10.0, r.center().y),
                egui::vec2(20.0, 0.0),
                egui::Stroke::new(2.0_f32, egui::Color32::WHITE),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::SlideUp,
        category: TransitionCategory::Slide,
        preview: |p, r, _| {
            preview_split_v(p, r, 0.6, PREVIEW_A, PREVIEW_B);
            p.arrow(
                egui::pos2(r.center().x, r.center().y + 10.0),
                egui::vec2(0.0, -20.0),
                egui::Stroke::new(2.0_f32, egui::Color32::WHITE),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::SlideDown,
        category: TransitionCategory::Slide,
        preview: |p, r, _| {
            preview_split_v(p, r, 0.4, PREVIEW_B, PREVIEW_A);
            p.arrow(
                egui::pos2(r.center().x, r.center().y - 10.0),
                egui::vec2(0.0, 20.0),
                egui::Stroke::new(2.0_f32, egui::Color32::WHITE),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::CoverLeft,
        category: TransitionCategory::Slide,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_A);
            let cover_w = r.width() * 0.55;
            p.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(r.min.x, r.min.y),
                    egui::vec2(cover_w, r.height()),
                ),
                0.0,
                PREVIEW_B,
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::CoverRight,
        category: TransitionCategory::Slide,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_A);
            let cover_w = r.width() * 0.55;
            p.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(r.max.x - cover_w, r.min.y),
                    egui::vec2(cover_w, r.height()),
                ),
                0.0,
                PREVIEW_B,
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::UncoverLeft,
        category: TransitionCategory::Slide,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_B);
            let uncover_w = r.width() * 0.45;
            p.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(r.min.x, r.min.y),
                    egui::vec2(uncover_w, r.height()),
                ),
                0.0,
                PREVIEW_A,
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::UncoverRight,
        category: TransitionCategory::Slide,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_B);
            let uncover_w = r.width() * 0.45;
            p.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(r.max.x - uncover_w, r.min.y),
                    egui::vec2(uncover_w, r.height()),
                ),
                0.0,
                PREVIEW_A,
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::SqueezeH,
        category: TransitionCategory::Slide,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_B);
            let squeeze_w = r.width() * 0.3;
            p.rect_filled(
                egui::Rect::from_center_size(r.center(), egui::vec2(squeeze_w, r.height())),
                0.0,
                PREVIEW_A,
            );
            p.arrow(
                egui::pos2(r.center().x - squeeze_w * 0.5 - 8.0, r.center().y),
                egui::vec2(8.0, 0.0),
                egui::Stroke::new(1.5_f32, egui::Color32::WHITE),
            );
            p.arrow(
                egui::pos2(r.center().x + squeeze_w * 0.5 + 8.0, r.center().y),
                egui::vec2(-8.0, 0.0),
                egui::Stroke::new(1.5_f32, egui::Color32::WHITE),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::SqueezeV,
        category: TransitionCategory::Slide,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_B);
            let squeeze_h = r.height() * 0.3;
            p.rect_filled(
                egui::Rect::from_center_size(r.center(), egui::vec2(r.width(), squeeze_h)),
                0.0,
                PREVIEW_A,
            );
            p.arrow(
                egui::pos2(r.center().x, r.center().y - squeeze_h * 0.5 - 8.0),
                egui::vec2(0.0, 8.0),
                egui::Stroke::new(1.5_f32, egui::Color32::WHITE),
            );
            p.arrow(
                egui::pos2(r.center().x, r.center().y + squeeze_h * 0.5 + 8.0),
                egui::vec2(0.0, -8.0),
                egui::Stroke::new(1.5_f32, egui::Color32::WHITE),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::BoxWipeIn,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_A);
            let inner = r.shrink(r.width() * 0.2);
            p.rect_filled(inner, 0.0, PREVIEW_B);
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::BoxWipeOut,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_B);
            let inner = r.shrink(r.width() * 0.2);
            p.rect_filled(inner, 0.0, PREVIEW_A);
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::DiagonalWipeTLBR,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_B);
            let pts = vec![r.min, egui::pos2(r.max.x, r.min.y), egui::pos2(r.min.x, r.max.y)];
            p.add(egui::Shape::convex_polygon(pts, PREVIEW_A, egui::Stroke::NONE));
            p.line_segment(
                [egui::pos2(r.max.x, r.min.y), egui::pos2(r.min.x, r.max.y)],
                egui::Stroke::new(1.5_f32, egui::Color32::from_white_alpha(100)),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::DiagonalWipeTRBL,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_B);
            let pts = vec![r.min, r.max, egui::pos2(r.max.x, r.min.y)];
            p.add(egui::Shape::convex_polygon(pts, PREVIEW_A, egui::Stroke::NONE));
            p.line_segment(
                [r.min, r.max],
                egui::Stroke::new(1.5_f32, egui::Color32::from_white_alpha(100)),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::CheckerboardWipe,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| {
            let grid = 4usize;
            let cell_w = r.width() / grid as f32;
            let cell_h = r.height() / grid as f32;
            for row in 0..grid {
                for col in 0..grid {
                    let color = if (row + col) % 2 == 0 { PREVIEW_A } else { PREVIEW_B };
                    p.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(
                                r.min.x + col as f32 * cell_w,
                                r.min.y + row as f32 * cell_h,
                            ),
                            egui::vec2(cell_w, cell_h),
                        ),
                        0.0,
                        color,
                    );
                }
            }
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::VenetianBlindHorizontal,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| {
            let strips = 5;
            let strip_h = r.height() / strips as f32;
            for i in 0..strips {
                let y = r.min.y + i as f32 * strip_h;
                let color = if i % 2 == 0 { PREVIEW_A } else { PREVIEW_B };
                p.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(r.min.x, y),
                        egui::vec2(r.width(), strip_h - 1.0),
                    ),
                    0.0,
                    color,
                );
            }
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::VenetianBlindVertical,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| {
            let strips = 5;
            let strip_w = r.width() / strips as f32;
            for i in 0..strips {
                let x = r.min.x + i as f32 * strip_w;
                let color = if i % 2 == 0 { PREVIEW_A } else { PREVIEW_B };
                p.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(x, r.min.y),
                        egui::vec2(strip_w - 1.0, r.height()),
                    ),
                    0.0,
                    color,
                );
            }
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::SplitWipeHorizontal,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_A);
            let center_w = r.width() * 0.35;
            p.rect_filled(
                egui::Rect::from_center_size(r.center(), egui::vec2(center_w, r.height())),
                0.0,
                PREVIEW_B,
            );
            p.line_segment(
                [
                    egui::pos2(r.center().x - center_w * 0.5, r.min.y),
                    egui::pos2(r.center().x - center_w * 0.5, r.max.y),
                ],
                egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(80)),
            );
            p.line_segment(
                [
                    egui::pos2(r.center().x + center_w * 0.5, r.min.y),
                    egui::pos2(r.center().x + center_w * 0.5, r.max.y),
                ],
                egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(80)),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::SplitWipeVertical,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_A);
            let center_h = r.height() * 0.35;
            p.rect_filled(
                egui::Rect::from_center_size(r.center(), egui::vec2(r.width(), center_h)),
                0.0,
                PREVIEW_B,
            );
            p.line_segment(
                [
                    egui::pos2(r.min.x, r.center().y - center_h * 0.5),
                    egui::pos2(r.max.x, r.center().y - center_h * 0.5),
                ],
                egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(80)),
            );
            p.line_segment(
                [
                    egui::pos2(r.min.x, r.center().y + center_h * 0.5),
                    egui::pos2(r.max.x, r.center().y + center_h * 0.5),
                ],
                egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(80)),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::StarWipe,
        category: TransitionCategory::Wipe,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_A);
            let cx = r.center();
            let outer_r = r.height() * 0.42;
            let inner_r = outer_r * 0.4;
            let points = 5usize;
            let mut pts = Vec::new();
            for i in 0..points * 2 {
                let angle = i as f32 * std::f32::consts::PI / points as f32
                    - std::f32::consts::FRAC_PI_2;
                let radius = if i % 2 == 0 { outer_r } else { inner_r };
                pts.push(egui::pos2(
                    cx.x + angle.cos() * radius,
                    cx.y + angle.sin() * radius,
                ));
            }
            // Star isn't strictly convex, but convex_polygon renders it as a fan from pts[0].
            p.add(egui::Shape::convex_polygon(pts, PREVIEW_B, egui::Stroke::NONE));
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::ClockWipe,
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
            p.line_segment([cx, hand_end], egui::Stroke::new(2.0_f32, egui::Color32::WHITE));
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::PageTurn,
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
                egui::Stroke::new(2.0_f32, egui::Color32::from_black_alpha(120)),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::PageTurnReverse,
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
                egui::Stroke::new(2.0_f32, egui::Color32::from_black_alpha(120)),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::ZoomInDissolve,
        category: TransitionCategory::Zoom,
        preview: |pa, r, _| {
            pa.rect_filled(r, 0.0, PREVIEW_B);
            let shrink = r.shrink(r.width() * 0.15);
            pa.rect_filled(
                shrink,
                0.0,
                egui::Color32::from_rgba_unmultiplied(
                    PREVIEW_A.r(),
                    PREVIEW_A.g(),
                    PREVIEW_A.b(),
                    180,
                ),
            );
            let c = r.center();
            for (dx, dy) in [(1.0_f32, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
                pa.arrow(
                    egui::pos2(c.x + dx * 8.0, c.y + dy * 8.0),
                    egui::vec2(dx * 10.0, dy * 10.0),
                    egui::Stroke::new(1.5_f32, egui::Color32::from_white_alpha(160)),
                );
            }
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::ZoomOutDissolve,
        category: TransitionCategory::Zoom,
        preview: |pa, r, _| {
            pa.rect_filled(r, 0.0, PREVIEW_B);
            pa.rect_filled(
                r,
                0.0,
                egui::Color32::from_rgba_unmultiplied(
                    PREVIEW_A.r(),
                    PREVIEW_A.g(),
                    PREVIEW_A.b(),
                    120,
                ),
            );
            let inner = r.shrink(r.width() * 0.2);
            pa.rect_filled(
                inner,
                0.0,
                egui::Color32::from_rgba_unmultiplied(
                    PREVIEW_A.r(),
                    PREVIEW_A.g(),
                    PREVIEW_A.b(),
                    180,
                ),
            );
            let c = r.center();
            for (dx, dy) in [(1.0_f32, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
                pa.arrow(
                    egui::pos2(c.x + dx * 18.0, c.y + dy * 18.0),
                    egui::vec2(-dx * 10.0, -dy * 10.0),
                    egui::Stroke::new(1.5_f32, egui::Color32::from_white_alpha(160)),
                );
            }
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::CrossZoom,
        category: TransitionCategory::Zoom,
        preview: |pa, r, _| {
            let mid = r.min.x + r.width() * 0.5;
            let left = egui::Rect::from_min_max(r.min, egui::pos2(mid, r.max.y));
            pa.rect_filled(left, 0.0, PREVIEW_A);
            pa.arrow(
                egui::pos2(left.center().x, left.center().y),
                egui::vec2(12.0, 0.0),
                egui::Stroke::new(1.5_f32, egui::Color32::from_white_alpha(180)),
            );
            let right = egui::Rect::from_min_max(egui::pos2(mid, r.min.y), r.max);
            pa.rect_filled(right, 0.0, PREVIEW_B);
            pa.arrow(
                egui::pos2(right.center().x, right.center().y),
                egui::vec2(-12.0, 0.0),
                egui::Stroke::new(1.5_f32, egui::Color32::from_white_alpha(180)),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::BokehDissolve,
        category: TransitionCategory::Cinematic,
        preview: |pa, r, _| {
            preview_gradient_h(pa, r, 8, PREVIEW_A, PREVIEW_B);
            let circles: [(f32, f32, f32, u8); 5] = [
                (0.3, 0.4, 14.0, 60),
                (0.6, 0.6, 10.0, 80),
                (0.5, 0.3, 18.0, 50),
                (0.75, 0.5, 8.0, 70),
                (0.2, 0.65, 12.0, 55),
            ];
            for (fx, fy, radius, alpha) in circles {
                let cx = r.min.x + fx * r.width();
                let cy = r.min.y + fy * r.height();
                pa.circle_stroke(
                    egui::pos2(cx, cy),
                    radius,
                    egui::Stroke::new(
                        1.5_f32,
                        egui::Color32::from_rgba_unmultiplied(255, 240, 200, alpha),
                    ),
                );
                pa.circle_filled(
                    egui::pos2(cx, cy),
                    radius * 0.3,
                    egui::Color32::from_rgba_unmultiplied(255, 250, 230, alpha / 2),
                );
            }
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::AnamorphicFlare,
        category: TransitionCategory::Cinematic,
        preview: |pa, r, _| {
            pa.rect_filled(r, 0.0, PREVIEW_A);
            let streak_h = r.height() * 0.12;
            let streak_rect = egui::Rect::from_center_size(
                r.center(),
                egui::vec2(r.width(), streak_h),
            );
            pa.rect_filled(
                streak_rect,
                0.0,
                egui::Color32::from_rgba_unmultiplied(100, 180, 255, 180),
            );
            let core_rect = egui::Rect::from_center_size(
                r.center(),
                egui::vec2(r.width(), streak_h * 0.3),
            );
            pa.rect_filled(
                core_rect,
                0.0,
                egui::Color32::from_rgba_unmultiplied(200, 230, 255, 240),
            );
            for i in 0..5 {
                let x = r.min.x + r.width() * (i as f32 + 0.5) / 5.0;
                pa.circle_filled(
                    egui::pos2(x, r.center().y),
                    2.0,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200),
                );
            }
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::DreamSequence,
        category: TransitionCategory::Cinematic,
        preview: |pa, r, _| {
            preview_gradient_h(pa, r, 8, PREVIEW_A, PREVIEW_B);
            pa.rect_filled(
                r,
                0.0,
                egui::Color32::from_rgba_unmultiplied(255, 220, 150, 60),
            );
            preview_circle(
                pa,
                r,
                0.7,
                egui::Color32::from_rgba_unmultiplied(255, 240, 200, 40),
            );
            preview_circle(
                pa,
                r,
                0.4,
                egui::Color32::from_rgba_unmultiplied(255, 250, 230, 60),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::ProjectorFlicker,
        category: TransitionCategory::Cinematic,
        preview: |pa, r, _| {
            pa.rect_filled(r, 0.0, egui::Color32::from_rgb(40, 35, 25));
            for i in 0..3 {
                let y = r.min.y + r.height() * (i as f32 + 0.5) / 3.0;
                pa.rect_filled(
                    egui::Rect::from_center_size(
                        egui::pos2(r.min.x + 6.0, y),
                        egui::vec2(4.0, 6.0),
                    ),
                    1.0,
                    egui::Color32::from_rgb(20, 18, 12),
                );
            }
            pa.rect_filled(
                r.shrink2(egui::vec2(12.0, 4.0)),
                0.0,
                PREVIEW_A,
            );
            pa.rect_filled(
                r.shrink2(egui::vec2(12.0, 4.0)),
                0.0,
                egui::Color32::from_rgba_unmultiplied(255, 240, 200, 40),
            );
            let sx = r.min.x + r.width() * 0.7;
            pa.line_segment(
                [egui::pos2(sx, r.min.y), egui::pos2(sx + 1.0, r.max.y)],
                egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 200, 120),
                ),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::Halation,
        category: TransitionCategory::Cinematic,
        preview: |pa, r, _| {
            pa.rect_filled(r, 0.0, PREVIEW_A);
            let cx = egui::pos2(r.min.x + r.width() * 0.65, r.min.y + r.height() * 0.35);
            pa.circle_filled(
                cx,
                r.height() * 0.5,
                egui::Color32::from_rgba_unmultiplied(255, 100, 30, 30),
            );
            pa.circle_filled(
                cx,
                r.height() * 0.3,
                egui::Color32::from_rgba_unmultiplied(255, 140, 60, 60),
            );
            pa.circle_filled(
                cx,
                r.height() * 0.15,
                egui::Color32::from_rgba_unmultiplied(255, 200, 150, 140),
            );
            pa.circle_filled(
                cx,
                r.height() * 0.06,
                egui::Color32::from_rgba_unmultiplied(255, 240, 220, 240),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::GlitchRGBSplit,
        category: TransitionCategory::Effect,
        preview: |pa, r, _| {
            pa.rect_filled(r, 0.0, egui::Color32::from_rgb(15, 15, 25));
            let block_h = r.height() / 5.0;
            for i in 0..5 {
                let y = r.min.y + i as f32 * block_h;
                let shift_r = (i as f32 * 37.0).sin() * 15.0;
                let shift_b = (i as f32 * 73.0).sin() * -12.0;
                pa.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(r.min.x + shift_r, y),
                        egui::vec2(r.width(), block_h - 1.0),
                    ),
                    0.0,
                    egui::Color32::from_rgba_unmultiplied(200, 0, 0, 100),
                );
                pa.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(r.min.x, y),
                        egui::vec2(r.width(), block_h - 1.0),
                    ),
                    0.0,
                    egui::Color32::from_rgba_unmultiplied(0, 180, 0, 80),
                );
                pa.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(r.min.x + shift_b, y),
                        egui::vec2(r.width(), block_h - 1.0),
                    ),
                    0.0,
                    egui::Color32::from_rgba_unmultiplied(0, 0, 200, 100),
                );
            }
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::StaticTV,
        category: TransitionCategory::Effect,
        preview: |pa, r, _| {
            let line_h = 4.0_f32;
            let lines = (r.height() / line_h) as usize;
            for i in 0..lines {
                let y = r.min.y + i as f32 * line_h;
                let brightness = if i % 2 == 0 { 180u8 } else { 120u8 };
                let noise = (i as f32 * 37.3).sin().abs();
                let val = (brightness as f32 * (0.5 + noise * 0.5)) as u8;
                let color = egui::Color32::from_rgb(val, val, val);
                pa.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(r.min.x, y),
                        egui::vec2(r.width(), line_h - 0.5),
                    ),
                    0.0,
                    color,
                );
            }
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::VHSTracking,
        category: TransitionCategory::Effect,
        preview: |pa, r, _| {
            pa.rect_filled(r, 0.0, PREVIEW_A);
            let bands = 5usize;
            let band_h = r.height() / bands as f32;
            for i in 0..bands {
                let y = r.min.y + i as f32 * band_h;
                let shift = (i as f32 * 37.0).sin() * 12.0;
                if i % 2 == 0 {
                    pa.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(r.min.x + shift, y),
                            egui::vec2(r.width(), band_h - 1.0),
                        ),
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(
                            PREVIEW_B.r(),
                            PREVIEW_B.g(),
                            PREVIEW_B.b(),
                            180,
                        ),
                    );
                }
            }
            pa.rect_filled(
                egui::Rect::from_min_size(r.min, egui::vec2(4.0, r.height())),
                0.0,
                egui::Color32::from_rgba_unmultiplied(255, 100, 100, 100),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::Swirl,
        category: TransitionCategory::Effect,
        preview: |pa, r, _| {
            pa.rect_filled(r, 0.0, PREVIEW_A);
            let cx = r.center();
            let steps = 24usize;
            let max_r = r.height() * 0.45;
            let mut pts = Vec::new();
            for i in 0..=steps {
                let t = i as f32 / steps as f32;
                let radius = t * max_r;
                let angle = t * std::f32::consts::TAU * 1.5
                    - std::f32::consts::FRAC_PI_2;
                pts.push(egui::pos2(
                    cx.x + angle.cos() * radius,
                    cx.y + angle.sin() * radius,
                ));
            }
            for i in 0..pts.len().saturating_sub(1) {
                let alpha = (i as f32 / pts.len() as f32 * 200.0) as u8;
                pa.line_segment(
                    [pts[i], pts[i + 1]],
                    egui::Stroke::new(
                        2.0_f32,
                        egui::Color32::from_rgba_unmultiplied(
                            PREVIEW_B.r(),
                            PREVIEW_B.g(),
                            PREVIEW_B.b(),
                            alpha,
                        ),
                    ),
                );
            }
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::WaveDistortion,
        category: TransitionCategory::Effect,
        preview: |pa, r, _| {
            preview_gradient_h(pa, r, 8, PREVIEW_A, PREVIEW_B);
            let steps = 40usize;
            let amp = r.height() * 0.2;
            let mut prev = egui::pos2(r.min.x, r.center().y);
            for i in 1..=steps {
                let t = i as f32 / steps as f32;
                let x = r.min.x + t * r.width();
                let y = r.center().y + (t * std::f32::consts::TAU * 2.0).sin() * amp;
                let next = egui::pos2(x, y);
                pa.line_segment(
                    [prev, next],
                    egui::Stroke::new(1.5_f32, egui::Color32::from_white_alpha(180)),
                );
                prev = next;
            }
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::ColorizeFlash,
        category: TransitionCategory::Effect,
        preview: |p, r, _| {
            preview_split_h(p, r, 0.5, PREVIEW_A, PREVIEW_B);
            preview_circle(
                p,
                r,
                0.55,
                egui::Color32::from_rgba_unmultiplied(0, 200, 255, 150),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::HueRotate,
        category: TransitionCategory::Effect,
        preview: |pa, r, _| {
            let steps = 12usize;
            for i in 0..steps {
                let t = i as f32 / steps as f32;
                let hue = t * 360.0;
                let hi = (hue / 60.0) as u32 % 6;
                let f = (hue / 60.0) - (hue / 60.0).floor();
                let v = 0.7_f32;
                let s = 0.8_f32;
                let p2 = v * (1.0 - s);
                let q = v * (1.0 - s * f);
                let t2 = v * (1.0 - s * (1.0 - f));
                let (rr, gg, bb) = match hi {
                    0 => (v, t2, p2),
                    1 => (q, v, p2),
                    2 => (p2, v, t2),
                    3 => (p2, q, v),
                    4 => (t2, p2, v),
                    _ => (v, p2, q),
                };
                let strip = egui::Rect::from_min_size(
                    egui::pos2(r.min.x + t * r.width(), r.min.y),
                    egui::vec2(r.width() / steps as f32 + 1.0, r.height()),
                );
                pa.rect_filled(
                    strip,
                    0.0,
                    egui::Color32::from_rgb(
                        (rr * 255.0) as u8,
                        (gg * 255.0) as u8,
                        (bb * 255.0) as u8,
                    ),
                );
            }
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::NegativeFlash,
        category: TransitionCategory::Effect,
        preview: |p, r, _| {
            preview_split_h(p, r, 0.5, PREVIEW_A, PREVIEW_B);
            let strip_w = r.width() * 0.25;
            p.rect_filled(
                egui::Rect::from_center_size(r.center(), egui::vec2(strip_w, r.height())),
                0.0,
                egui::Color32::from_rgb(
                    255 - PREVIEW_A.r() / 2 - PREVIEW_B.r() / 2,
                    255 - PREVIEW_A.g() / 2 - PREVIEW_B.g() / 2,
                    255 - PREVIEW_A.b() / 2 - PREVIEW_B.b() / 2,
                ),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::Sepia,
        category: TransitionCategory::Cinematic,
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
                egui::Color32::from_rgb(112, 84, 56),
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
        kind: klipp_core::TransitionKind::GlitchDissolve,
        category: TransitionCategory::Effect,
        preview: |p, r, _| {
            let block_h = r.height() / 6.0;
            for i in 0..6 {
                let y = r.min.y + i as f32 * block_h;
                let shift = ((i as f32 * 37.0) % 20.0) - 10.0;
                let color = if i % 2 == 0 { PREVIEW_A } else { PREVIEW_B };
                p.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(r.min.x + shift, y),
                        egui::vec2(r.width(), block_h - 1.0),
                    ),
                    0.0,
                    color,
                );
            }
            p.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(r.min.x, r.min.y),
                    egui::vec2(3.0, r.height()),
                ),
                0.0,
                egui::Color32::from_rgba_unmultiplied(255, 0, 0, 120),
            );
            p.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(r.max.x - 3.0, r.min.y),
                    egui::vec2(3.0, r.height()),
                ),
                0.0,
                egui::Color32::from_rgba_unmultiplied(0, 0, 255, 120),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::Desaturate,
        category: TransitionCategory::Effect,
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
                egui::Color32::from_rgb(90, 90, 90),
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
        kind: klipp_core::TransitionKind::AdditiveDissolve,
        category: TransitionCategory::Dissolve,
        preview: |p, r, _| {
            preview_gradient_h(p, r, 8, PREVIEW_A, PREVIEW_B);
            p.rect_filled(
                egui::Rect::from_center_size(
                    r.center(),
                    egui::vec2(r.width() * 0.4, r.height()),
                ),
                0.0,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 80),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::FadeToColor,
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
                egui::Color32::from_rgb(30, 30, 80),
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
        kind: klipp_core::TransitionKind::FilmBurn,
        category: TransitionCategory::Cinematic,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, PREVIEW_A);
            let cx = egui::pos2(r.min.x + r.width() * 0.6, r.min.y + r.height() * 0.4);
            p.circle_filled(
                cx,
                r.height() * 0.4,
                egui::Color32::from_rgba_unmultiplied(255, 160, 40, 60),
            );
            p.circle_filled(
                cx,
                r.height() * 0.25,
                egui::Color32::from_rgba_unmultiplied(255, 200, 100, 140),
            );
            p.circle_filled(
                cx,
                r.height() * 0.12,
                egui::Color32::from_rgba_unmultiplied(255, 240, 220, 220),
            );
            for i in 0..8 {
                let gx = r.min.x + (i as f32 * 37.0) % r.width();
                let gy = r.min.y + (i as f32 * 53.0) % r.height();
                p.circle_filled(
                    egui::pos2(gx, gy),
                    1.0,
                    egui::Color32::from_rgba_unmultiplied(255, 220, 150, 80),
                );
            }
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::ChromaticAberration,
        category: TransitionCategory::Effect,
        preview: |p, r, _| {
            p.rect_filled(r, 0.0, egui::Color32::from_rgb(20, 20, 30));
            let cx = r.center();
            let hw = r.width() * 0.3;
            let hh = r.height() * 0.35;
            p.rect_filled(
                egui::Rect::from_center_size(
                    egui::pos2(cx.x - 6.0, cx.y),
                    egui::vec2(hw * 2.0, hh * 2.0),
                ),
                0.0,
                egui::Color32::from_rgba_unmultiplied(255, 0, 0, 120),
            );
            p.rect_filled(
                egui::Rect::from_center_size(cx, egui::vec2(hw * 2.0, hh * 2.0)),
                0.0,
                egui::Color32::from_rgba_unmultiplied(0, 255, 0, 120),
            );
            p.rect_filled(
                egui::Rect::from_center_size(
                    egui::pos2(cx.x + 6.0, cx.y),
                    egui::vec2(hw * 2.0, hh * 2.0),
                ),
                0.0,
                egui::Color32::from_rgba_unmultiplied(0, 0, 255, 120),
            );
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::Ripple,
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
                    egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(alpha)),
                );
            }
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::Pixelate,
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
        kind: klipp_core::TransitionKind::LuminanceDissolve,
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
        kind: klipp_core::TransitionKind::LightLeak,
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
                search_lower.is_empty()
                    || klipp_core::spec_for_kind(t.kind)
                        .display_name
                        .to_lowercase()
                        .contains(&search_lower);
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
                        let spec = klipp_core::spec_for_kind(tr.kind);
                        let resp = transition_card(ui, tr, spec, theme, card_w);
                        if resp.drag_started() {
                            state.drag_drop.begin(DragPayload::Transition {
                                name: spec.display_name.to_string(),
                                kind_id: spec.kind_id,
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
                                                    kind_id: spec.kind_id,
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
                                    state.toast.show(format!("\"{}\" applied", spec.display_name));
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
    spec: &klipp_core::TransitionSpec,
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
        painter.rect_stroke(rect, 6.0, egui::Stroke::new(1.0_f32, border_color));

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
            spec.display_name,
            egui::FontId::proportional(10.0),
            if hovered { theme.text_primary } else { theme.text_secondary },
        );
    }

    resp
}
