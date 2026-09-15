use crate::browser::effects_tab::{EffectSort, EffectViewMode};
use crate::drag_drop::DragPayload;
use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

/// Source filter for the Transitions tab — built-in vs OpenFX.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub enum TransitionsSourceFilter {
    #[default]
    All,
    Builtin,
    Ofx,
}


/// Public alias of the category enum so the BrowserPanel can hold the
/// active selection.
pub use TransitionCategory as TransitionsCategory;

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
    /// Draws a 2-colour preview into the given rect at the supplied
    /// progress (0.0 = before, 1.0 = after). Static cards pass 0.5 so
    /// the user sees the mid-point pose; hovered cards animate by
    /// driving progress with a wall-clock loop.
    preview: fn(painter: &egui::Painter, rect: egui::Rect, theme: &KlippTheme, progress: f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionCategory {
    Dissolve,
    Wipe,
    Slide,
    Zoom,
    Effect,
    Cinematic,
}

impl TransitionCategory {
    pub fn label(self) -> &'static str {
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
        preview: |p, r, _, _progress| preview_gradient_h(p, r, 8, PREVIEW_A, PREVIEW_B),
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::WipeLeft,
        category: TransitionCategory::Wipe,
        preview: |p, r, _, _progress| preview_split_h(p, r, 0.5, PREVIEW_A, PREVIEW_B),
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::WipeRight,
        category: TransitionCategory::Wipe,
        preview: |p, r, _, _progress| preview_split_h(p, r, 0.5, PREVIEW_B, PREVIEW_A),
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::WipeUp,
        category: TransitionCategory::Wipe,
        preview: |p, r, _, _progress| preview_split_v(p, r, 0.5, PREVIEW_A, PREVIEW_B),
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::WipeDown,
        category: TransitionCategory::Wipe,
        preview: |p, r, _, _progress| preview_split_v(p, r, 0.5, PREVIEW_B, PREVIEW_A),
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::DipToBlack,
        category: TransitionCategory::Dissolve,
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| preview_gradient_h(p, r, 12, PREVIEW_A, PREVIEW_B),
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::BlurDissolve,
        category: TransitionCategory::Dissolve,
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
            preview_split_h(p, r, 0.5, PREVIEW_A, PREVIEW_B);
            preview_circle(p, r, 0.6, egui::Color32::from_white_alpha(180));
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::Spin,
        category: TransitionCategory::Effect,
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
            p.rect_filled(r, 0.0, PREVIEW_A);
            let radius = r.height() * 0.45;
            p.circle_filled(r.center(), radius, PREVIEW_B);
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::CircleWipeOut,
        category: TransitionCategory::Wipe,
        preview: |p, r, _, _progress| {
            p.rect_filled(r, 0.0, PREVIEW_B);
            let radius = r.height() * 0.25;
            p.circle_filled(r.center(), radius, PREVIEW_A);
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::BarnDoorOpen,
        category: TransitionCategory::Wipe,
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
            p.rect_filled(r, 0.0, PREVIEW_A);
            let inner = r.shrink(r.width() * 0.2);
            p.rect_filled(inner, 0.0, PREVIEW_B);
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::BoxWipeOut,
        category: TransitionCategory::Wipe,
        preview: |p, r, _, _progress| {
            p.rect_filled(r, 0.0, PREVIEW_B);
            let inner = r.shrink(r.width() * 0.2);
            p.rect_filled(inner, 0.0, PREVIEW_A);
        },
    },
    TransitionDef {
        kind: klipp_core::TransitionKind::DiagonalWipeTLBR,
        category: TransitionCategory::Wipe,
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |pa, r, _, _progress| {
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
        preview: |pa, r, _, _progress| {
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
        preview: |pa, r, _, _progress| {
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
        preview: |pa, r, _, _progress| {
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
        preview: |pa, r, _, _progress| {
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
        preview: |pa, r, _, _progress| {
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
        preview: |pa, r, _, _progress| {
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
        preview: |pa, r, _, _progress| {
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
        preview: |pa, r, _, _progress| {
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
        preview: |pa, r, _, _progress| {
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
        preview: |pa, r, _, _progress| {
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
        preview: |pa, r, _, _progress| {
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
        preview: |pa, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |pa, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
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
        preview: |p, r, _, _progress| {
            preview_gradient_h(p, r, 8, PREVIEW_A, PREVIEW_B);
            preview_circle(p, r, 0.4, egui::Color32::from_rgba_unmultiplied(255, 180, 60, 120));
        },
    },
];

// ---------- UI ----------

const TRANSITIONS_SIDEBAR_W: f32 = 180.0;

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &mut String,
    active_category: &mut Option<TransitionCategory>,
    source_filter: &mut TransitionsSourceFilter,
    gpu_only: &mut bool,
    view_mode: &mut EffectViewMode,
    sort_mode: &mut EffectSort,
    selected: &mut Option<String>,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let avail = ui.available_rect_before_wrap();

    // ── Split: sidebar | content ──
    let sidebar_w = TRANSITIONS_SIDEBAR_W.min(avail.width() * 0.40);
    let sidebar_rect = egui::Rect::from_min_size(
        avail.min,
        egui::vec2(sidebar_w, avail.height()),
    );
    let content_rect = egui::Rect::from_min_max(
        egui::pos2(avail.min.x + sidebar_w, avail.min.y),
        avail.max,
    );

    // ── Sidebar ──
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(sidebar_rect), |ui| {
        ui.set_min_size(sidebar_rect.size());
        ui.painter().rect_filled(sidebar_rect, 0.0, theme.bg_panel);
        ui.painter().vline(
            sidebar_rect.max.x,
            sidebar_rect.y_range(),
            egui::Stroke::new(1.0_f32, theme.border),
        );
        egui::ScrollArea::vertical()
            .id_salt("transitions_sidebar_scroll")
            .show(ui, |ui| {
                ui.add_space(6.0);
                let total = TRANSITIONS.len();
                tx_general_row(ui, theme, Icon::Star, "Favorites", 0, "tx_fav");
                tx_general_row(ui, theme, Icon::ArrowCounterClockwise, "Recent", 0, "tx_recent");
                tx_general_row(ui, theme, Icon::UploadSimple, "Installed", total, "tx_inst");
                ui.add_space(8.0);
                tx_section_separator(ui, theme);
                tx_section_header(ui, theme, "Transitions");

                tx_sidebar_row(
                    ui,
                    theme,
                    Some(Icon::List),
                    "All",
                    total,
                    active_category.is_none(),
                    "tx_cat_all",
                    &mut |c| *active_category = c,
                    None,
                );
                for cat in [
                    TransitionCategory::Dissolve,
                    TransitionCategory::Wipe,
                    TransitionCategory::Slide,
                    TransitionCategory::Zoom,
                    TransitionCategory::Effect,
                    TransitionCategory::Cinematic,
                ] {
                    let count = TRANSITIONS.iter().filter(|t| t.category == cat).count();
                    let is_active = *active_category == Some(cat);
                    tx_sidebar_row(
                        ui,
                        theme,
                        Some(category_icon(cat)),
                        cat.label(),
                        count,
                        is_active,
                        &format!("tx_cat_{:?}", cat),
                        &mut |c| *active_category = c,
                        Some(cat),
                    );
                }
                ui.add_space(8.0);
            });
    });

    // ── Right pane ──
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(content_rect), |ui| {
        ui.set_min_size(content_rect.size());
        ui.add_space(8.0);

        // Search bar
        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(8.0, 0.0))
            .show(ui, |ui| {
                super::browser_search_field(ui, theme, search_query, "Search transitions\u{2026}", state);
            });
        ui.add_space(6.0);

        // Filter row
        let scoped_total = TRANSITIONS
            .iter()
            .filter(|t| match active_category {
                Some(cat) => t.category == *cat,
                None => true,
            })
            .count();
        let builtin_count = scoped_total; // all entries are built-in today
        let ofx_count: usize = 0;

        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(8.0, 0.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    tx_segmented_filter(
                        ui,
                        theme,
                        source_filter,
                        scoped_total,
                        builtin_count,
                        ofx_count,
                    );
                    ui.add_space(6.0);
                    gpu_only_pill(ui, theme, gpu_only);
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            view_mode_button(ui, theme, view_mode, EffectViewMode::List, Icon::List, "List view");
                            view_mode_button(ui, theme, view_mode, EffectViewMode::Grid, Icon::GridFour, "Grid view");
                        },
                    );
                });
            });

        ui.add_space(8.0);

        // Apply filters
        let search_lower = search_query.to_lowercase();
        let mut filtered: Vec<&TransitionDef> = TRANSITIONS
            .iter()
            .filter(|t| match active_category {
                Some(cat) => t.category == *cat,
                None => true,
            })
            .filter(|_t| match *source_filter {
                TransitionsSourceFilter::All | TransitionsSourceFilter::Builtin => true,
                TransitionsSourceFilter::Ofx => false, // no OFX transitions yet
            })
            .filter(|t| {
                if search_lower.is_empty() {
                    return true;
                }
                klipp_core::spec_for_kind(t.kind)
                    .display_name
                    .to_lowercase()
                    .contains(&search_lower)
            })
            .collect();
        match *sort_mode {
            EffectSort::NameAsc => filtered.sort_by(|a, b| {
                klipp_core::spec_for_kind(a.kind)
                    .display_name
                    .to_lowercase()
                    .cmp(&klipp_core::spec_for_kind(b.kind).display_name.to_lowercase())
            }),
            EffectSort::NameDesc => filtered.sort_by(|a, b| {
                klipp_core::spec_for_kind(b.kind)
                    .display_name
                    .to_lowercase()
                    .cmp(&klipp_core::spec_for_kind(a.kind).display_name.to_lowercase())
            }),
            EffectSort::Category => {
                filtered.sort_by_key(|t| t.category.label());
            }
            _ => {}
        }

        // Result strip
        tx_result_strip(ui, theme, filtered.len(), sort_mode);
        ui.add_space(4.0);

        // Card grid
        egui::ScrollArea::vertical()
            .id_salt("transitions_grid_scroll")
            .show(ui, |ui| {
                if filtered.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        let msg = if !search_query.is_empty() {
                            format!("No transitions match \"{}\"", search_query)
                        } else {
                            "No transitions in this category".to_string()
                        };
                        ui.label(
                            egui::RichText::new(msg)
                                .size(11.0)
                                .color(theme.text_dim),
                        );
                    });
                    return;
                }
                let avail_w = ui.available_width() - 8.0;
                let spacing = 6.0;
                let render_grid = *view_mode == EffectViewMode::Grid;
                if render_grid {
                    let card_w = ((avail_w - spacing) / 2.0).floor();
                    let card_h = (card_w * 9.0 / 16.0).round().max(96.0);
                    egui::Grid::new("transitions_grid")
                        .num_columns(2)
                        .spacing([spacing, spacing])
                        .min_col_width(card_w)
                        .show(ui, |ui| {
                            for (i, tr) in filtered.iter().enumerate() {
                                let spec = klipp_core::spec_for_kind(tr.kind);
                                let is_selected = selected.as_deref() == Some(spec.display_name);
                                let elapsed = tx_read_anim_elapsed(ui, spec.display_name);
                                let resp = transition_card(
                                    ui, tr, spec, theme, card_w, card_h,
                                    is_selected, elapsed,
                                );
                                let active = resp.hovered() || is_selected;
                                tx_update_anim_state(ui, spec.display_name, active);
                                if active {
                                    ui.ctx().request_repaint();
                                }
                                handle_transition_interaction(
                                    state, &mut cmds, tr, spec, &resp, selected,
                                );
                                if i % 2 == 1 {
                                    ui.end_row();
                                }
                            }
                        });
                } else {
                    for tr in &filtered {
                        let spec = klipp_core::spec_for_kind(tr.kind);
                        let is_selected = selected.as_deref() == Some(spec.display_name);
                        let elapsed = tx_read_anim_elapsed(ui, spec.display_name);
                        let resp = transition_list_row(
                            ui, tr, spec, theme, is_selected, elapsed,
                        );
                        let active = resp.hovered() || is_selected;
                        tx_update_anim_state(ui, spec.display_name, active);
                        if active {
                            ui.ctx().request_repaint();
                        }
                        handle_transition_interaction(
                            state, &mut cmds, tr, spec, &resp, selected,
                        );
                    }
                }
            });
    });

    cmds
}

/// Per-card hover-animation tracking. Mirrors the helpers in
/// `titles_tab` — we read the previous frame's hover state from
/// memory, paint accordingly, then save the new state before the
/// frame ends.
fn tx_read_anim_elapsed(ui: &egui::Ui, name: &str) -> f32 {
    let active_id = ui.id().with(("tx_anim_active", name));
    let start_id = ui.id().with(("tx_anim_start", name));
    let now = ui.ctx().input(|i| i.time);
    let active: bool = ui.memory(|m| m.data.get_temp(active_id)).unwrap_or(false);
    if !active {
        return 0.0;
    }
    let start: f64 = ui.memory(|m| m.data.get_temp(start_id)).unwrap_or(now);
    (now - start).max(0.0) as f32
}

fn tx_update_anim_state(ui: &egui::Ui, name: &str, active: bool) {
    let active_id = ui.id().with(("tx_anim_active", name));
    let start_id = ui.id().with(("tx_anim_start", name));
    let now = ui.ctx().input(|i| i.time);
    let prev_active: bool = ui.memory(|m| m.data.get_temp(active_id)).unwrap_or(false);
    ui.memory_mut(|m| {
        m.data.insert_temp(active_id, active);
        if active && !prev_active {
            m.data.insert_temp(start_id, now);
        }
        if !active {
            m.data.remove::<f64>(start_id);
        }
    });
}

/// Compute the looped progress (0..=1) for a hovered card. The
/// transition plays over `LOOP_SECS` then snaps back so the user can
/// keep watching the cycle as long as they hover.
fn tx_progress(elapsed: f32) -> f32 {
    const LOOP_SECS: f32 = 1.6;
    const HOLD_SECS: f32 = 0.4; // pause briefly at the end before resetting
    let cycle = elapsed.rem_euclid(LOOP_SECS + HOLD_SECS);
    if cycle < LOOP_SECS {
        // Smoothstep ease so the transition reads as eased-in/out.
        let t = cycle / LOOP_SECS;
        t * t * (3.0 - 2.0 * t)
    } else {
        1.0
    }
}

fn handle_transition_interaction(
    state: &mut AppState,
    cmds: &mut Vec<AppCommand>,
    _tr: &TransitionDef,
    spec: &klipp_core::TransitionSpec,
    resp: &egui::Response,
    selected: &mut Option<String>,
) {
    if resp.clicked() {
        *selected = Some(spec.display_name.to_string());
    }
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
                if let Some(pos) = track.clips.iter().position(|c| c.id == clip_id) {
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
                                duration_frames: state.default_transition_frames,
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
                state.toast.show(String::from("Select a clip with an adjacent cut point"));
            }
        } else {
            state.toast.show(String::from("Select a clip first"));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn transition_card(
    ui: &mut egui::Ui,
    tr: &TransitionDef,
    spec: &klipp_core::TransitionSpec,
    theme: &KlippTheme,
    width: f32,
    height: f32,
    is_selected: bool,
    elapsed_secs: f32,
) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click_and_drag());

    if ui.is_rect_visible(rect) {
        let hovered = resp.hovered();
        let border_color = if is_selected {
            theme.accent
        } else if hovered {
            theme.accent_dim
        } else {
            theme.border
        };
        let stroke_w: f32 = if is_selected { 2.0 } else { 1.0 };
        let rounding = egui::Rounding::same(4.0);

        // Base canvas — neutral dark fill so the two-tone preview pops.
        ui.painter().rect_filled(rect, rounding, theme.bg_panel);
        ui.painter()
            .rect_stroke(rect, rounding, egui::Stroke::new(stroke_w, border_color));

        // Animate when hovered/selected; otherwise show the static
        // mid-pose so the card communicates the transition's shape at
        // a glance.
        let progress = if elapsed_secs > 0.0 {
            tx_progress(elapsed_secs)
        } else {
            0.5
        };

        // Static A/B halves (always the same — provides identity even
        // if the per-transition closure is a no-op for some kinds).
        let canvas = rect.shrink(1.0);
        paint_ab_canvas(ui.painter(), canvas, progress);

        // Per-transition preview overlay, clipped to the canvas. The
        // closure either paints a static composition (most kinds) or
        // a progress-driven shape (the few that use _progress).
        let clip_painter = ui.painter().with_clip_rect(canvas);
        (tr.preview)(&clip_painter, canvas, theme, progress);

        // Animated overlay driven by category — wipe sweeps a boundary,
        // slide moves a colored block, zoom expands a tile, etc. This
        // sits on top of the static preview so the user sees motion
        // when hovering even if the underlying preview closure is
        // static.
        if elapsed_secs > 0.0 {
            let clip_painter = ui.painter().with_clip_rect(canvas);
            paint_kind_motion_overlay(&clip_painter, canvas, tr.kind, progress);
        }

        // Center name pill — small dark backdrop tint behind text.
        let pill_text = spec.display_name.to_string();
        let font = egui::FontId::proportional(11.0);
        let galley = ui.painter().layout_no_wrap(
            pill_text.clone(),
            font.clone(),
            theme.text_primary,
        );
        let pad_x = 10.0;
        let pad_y = 5.0;
        let pill_w = galley.size().x + pad_x * 2.0;
        let pill_h = galley.size().y + pad_y * 2.0;
        let pill_rect = egui::Rect::from_center_size(
            canvas.center(),
            egui::vec2(pill_w, pill_h),
        );
        // Backdrop alpha eases from 220 (idle) → 110 (active) so the
        // animation reads through the pill while still keeping the
        // name legible.
        let backdrop_alpha = if elapsed_secs > 0.0 {
            (220.0 - 110.0 * tx_progress(elapsed_secs)) as u8
        } else {
            220
        };
        let backdrop = egui::Color32::from_rgba_unmultiplied(
            theme.bg_panel.r(),
            theme.bg_panel.g(),
            theme.bg_panel.b(),
            backdrop_alpha,
        );
        ui.painter().rect_filled(pill_rect, egui::Rounding::same(4.0), backdrop);
        ui.painter().rect_stroke(
            pill_rect,
            egui::Rounding::same(4.0),
            egui::Stroke::new(1.0_f32, theme.border),
        );
        ui.painter().text(
            canvas.center(),
            egui::Align2::CENTER_CENTER,
            pill_text,
            font,
            theme.text_primary,
        );

        // Top-right BUILT-IN badge.
        paint_corner_badge(ui, theme, rect, "BUILT-IN", true);

        // Optional bottom-right direction indicator for slides/wipes
        // with an obvious axis. Looks at the kind name.
        let kind_name = format!("{:?}", tr.kind).to_lowercase();
        let arrow = if kind_name.contains("up") {
            Some("\u{2191}")
        } else if kind_name.contains("down") {
            Some("\u{2193}")
        } else if kind_name.contains("left") {
            Some("\u{2190}")
        } else if kind_name.contains("right") {
            Some("\u{2192}")
        } else {
            None
        };
        if let Some(g) = arrow {
            let pos = egui::pos2(rect.max.x - 20.0, rect.max.y - 18.0);
            let ind_rect = egui::Rect::from_center_size(pos, egui::vec2(28.0, 18.0));
            ui.painter().rect_filled(
                ind_rect,
                egui::Rounding::same(4.0),
                egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180),
            );
            ui.painter().text(
                ind_rect.center(),
                egui::Align2::CENTER_CENTER,
                g,
                egui::FontId::proportional(11.0),
                theme.text_primary,
            );
        }
    }

    resp
}

fn transition_list_row(
    ui: &mut egui::Ui,
    tr: &TransitionDef,
    spec: &klipp_core::TransitionSpec,
    theme: &KlippTheme,
    is_selected: bool,
    elapsed_secs: f32,
) -> egui::Response {
    let avail_w = ui.available_width();
    let row_h = 56.0;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(avail_w, row_h),
        egui::Sense::click_and_drag(),
    );
    let hovered = resp.hovered();
    if is_selected {
        let bg = egui::Color32::from_rgba_unmultiplied(
            theme.accent.r(), theme.accent.g(), theme.accent.b(), 28,
        );
        ui.painter().rect_filled(rect, egui::Rounding::same(4.0), bg);
    } else if hovered {
        ui.painter().rect_filled(rect, egui::Rounding::same(4.0), theme.bg_raised);
    }
    let mini_w = 96.0;
    let mini_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x + 8.0, rect.min.y + 6.0),
        egui::vec2(mini_w, row_h - 12.0),
    );
    let progress = if elapsed_secs > 0.0 { tx_progress(elapsed_secs) } else { 0.5 };
    paint_ab_canvas(ui.painter(), mini_rect, progress);
    let clip_painter = ui.painter().with_clip_rect(mini_rect);
    (tr.preview)(&clip_painter, mini_rect, theme, progress);
    if elapsed_secs > 0.0 {
        paint_kind_motion_overlay(&clip_painter, mini_rect, tr.kind, progress);
    }
    let text_x = mini_rect.max.x + 12.0;
    let text_y = rect.center().y;
    ui.painter().text(
        egui::pos2(text_x, text_y - 7.0),
        egui::Align2::LEFT_CENTER,
        spec.display_name,
        egui::FontId::proportional(12.0),
        theme.text_primary,
    );
    ui.painter().text(
        egui::pos2(text_x, text_y + 8.0),
        egui::Align2::LEFT_CENTER,
        tr.category.label(),
        egui::FontId::proportional(10.0),
        theme.text_dim,
    );
    resp
}

/// Paint the standard A/B canvas the cards use as their backdrop:
/// left half = clip A (orange), right half = clip B (blue), with a
/// soft gradient mix in the middle. Static — animation overlay
/// happens on top.
fn paint_ab_canvas(painter: &egui::Painter, rect: egui::Rect, _progress: f32) {
    let a = egui::Color32::from_rgb(0xc4, 0x6e, 0x3d); // warm orange
    let b = egui::Color32::from_rgb(0x60, 0x7a, 0xa8); // cool blue
    let mid_x = rect.center().x;
    let left = egui::Rect::from_min_max(rect.min, egui::pos2(mid_x, rect.max.y));
    let right = egui::Rect::from_min_max(egui::pos2(mid_x, rect.min.y), rect.max);
    painter.rect_filled(left, egui::Rounding::same(4.0), a);
    painter.rect_filled(right, egui::Rounding::same(4.0), b);
}

/// Per-kind motion overlay. Each transition gets its own visual
/// signature — wipes name their direction, box wipes shrink/grow,
/// barn doors split/close, slides push from a specific edge, etc.
/// The point of the library is to scan and recognise; identical
/// generic motions defeat that.
///
/// All paint primitives — no offscreen rendering. Cheap enough to
/// run at 60 fps on every hovered card.
fn paint_kind_motion_overlay(
    p: &egui::Painter,
    rect: egui::Rect,
    kind: klipp_core::TransitionKind,
    progress: f32,
) {
    use klipp_core::TransitionKind as K;
    let t = progress.clamp(0.0, 1.0);
    let a = egui::Color32::from_rgb(0xc4, 0x6e, 0x3d);
    let b = egui::Color32::from_rgb(0x60, 0x7a, 0xa8);
    let r = egui::Rounding::same(4.0);

    // Helper closures kept local — don't pollute the module's public
    // surface. Each is a single visual primitive.
    let fade_over = |colour: egui::Color32, alpha_t: f32| {
        let alpha = (alpha_t.clamp(0.0, 1.0) * 230.0) as u8;
        p.rect_filled(
            rect,
            r,
            egui::Color32::from_rgba_unmultiplied(
                colour.r(), colour.g(), colour.b(), alpha,
            ),
        );
    };
    let dip_through = |colour: egui::Color32| {
        // Half-cycle bell — peaks at progress=0.5.
        let bell = (t * std::f32::consts::PI).sin();
        let alpha = (bell * 235.0) as u8;
        p.rect_filled(
            rect,
            r,
            egui::Color32::from_rgba_unmultiplied(
                colour.r(), colour.g(), colour.b(), alpha,
            ),
        );
        // After the dip, B has replaced A.
        if t > 0.5 {
            fade_over(b, (t - 0.5) * 2.0);
        }
    };
    let slide_in_from = |dx: f32, dy: f32| {
        // dx/dy in [-1, 1] meaning starting offset relative to a full
        // canvas. Block ends flush at (0, 0) when t = 1.
        let off_x = dx * (1.0 - t) * rect.width();
        let off_y = dy * (1.0 - t) * rect.height();
        let block = rect.translate(egui::vec2(off_x, off_y));
        p.rect_filled(block.intersect(rect), r, b);
    };
    let push = |dx: f32, dy: f32| {
        // A slides off in (dx, dy), B slides in from -(dx, dy).
        let off_x_a = -dx * t * rect.width();
        let off_y_a = -dy * t * rect.height();
        let off_x_b = dx * (1.0 - t) * rect.width();
        let off_y_b = dy * (1.0 - t) * rect.height();
        let a_block = rect.translate(egui::vec2(off_x_a, off_y_a));
        let b_block = rect.translate(egui::vec2(off_x_b, off_y_b));
        p.rect_filled(a_block.intersect(rect), r, a);
        p.rect_filled(b_block.intersect(rect), r, b);
    };
    let wipe_h = |from_left: bool| {
        let split_x = if from_left {
            rect.min.x + rect.width() * t
        } else {
            rect.max.x - rect.width() * t
        };
        let revealed = if from_left {
            egui::Rect::from_min_max(rect.min, egui::pos2(split_x, rect.max.y))
        } else {
            egui::Rect::from_min_max(egui::pos2(split_x, rect.min.y), rect.max)
        };
        p.rect_filled(revealed, r, b);
    };
    let wipe_v = |from_top: bool| {
        let split_y = if from_top {
            rect.min.y + rect.height() * t
        } else {
            rect.max.y - rect.height() * t
        };
        let revealed = if from_top {
            egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, split_y))
        } else {
            egui::Rect::from_min_max(egui::pos2(rect.min.x, split_y), rect.max)
        };
        p.rect_filled(revealed, r, b);
    };
    let diagonal_wipe = |tl_to_br: bool| {
        // A diagonal "line" sweep — approximate with a clipped
        // triangle that grows.
        let w = rect.width();
        let h = rect.height();
        let off = (w + h) * t;
        let pts = if tl_to_br {
            // Triangle in top-left, growing toward bottom-right.
            vec![
                rect.min,
                egui::pos2(rect.min.x + off, rect.min.y),
                egui::pos2(rect.min.x, rect.min.y + off),
            ]
        } else {
            vec![
                egui::pos2(rect.max.x, rect.min.y),
                egui::pos2(rect.max.x - off, rect.min.y),
                egui::pos2(rect.max.x, rect.min.y + off),
            ]
        };
        p.add(egui::Shape::convex_polygon(pts, b, egui::Stroke::NONE));
    };
    let barn_open = || {
        // Two halves split apart from centre, exposing B in the gap.
        let half_w = rect.width() * 0.5 * t;
        let gap = egui::Rect::from_min_max(
            egui::pos2(rect.center().x - half_w, rect.min.y),
            egui::pos2(rect.center().x + half_w, rect.max.y),
        );
        p.rect_filled(gap, r, b);
    };
    let barn_close = || {
        // Two B-coloured panels closing in from edges toward centre.
        let panel_w = rect.width() * 0.5 * t;
        let left = egui::Rect::from_min_max(
            rect.min,
            egui::pos2(rect.min.x + panel_w, rect.max.y),
        );
        let right = egui::Rect::from_min_max(
            egui::pos2(rect.max.x - panel_w, rect.min.y),
            rect.max,
        );
        p.rect_filled(left, r, b);
        p.rect_filled(right, r, b);
    };
    let split_h = || {
        // SplitWipeHorizontal — A splits horizontally, B revealed in
        // a horizontal band that grows from the centre.
        let half_h = rect.height() * 0.5 * t;
        let band = egui::Rect::from_min_max(
            egui::pos2(rect.min.x, rect.center().y - half_h),
            egui::pos2(rect.max.x, rect.center().y + half_h),
        );
        p.rect_filled(band, r, b);
    };
    let split_v = || {
        // SplitWipeVertical — vertical band growing from centre.
        let half_w = rect.width() * 0.5 * t;
        let band = egui::Rect::from_min_max(
            egui::pos2(rect.center().x - half_w, rect.min.y),
            egui::pos2(rect.center().x + half_w, rect.max.y),
        );
        p.rect_filled(band, r, b);
    };
    let box_in = || {
        // BoxWipeIn — B revealed in a rectangle growing from centre.
        let bw = rect.width() * t;
        let bh = rect.height() * t;
        let inner = egui::Rect::from_center_size(rect.center(), egui::vec2(bw, bh));
        p.rect_filled(inner, egui::Rounding::same(2.0), b);
    };
    let box_out = || {
        // BoxWipeOut — full B canvas with an A "hole" shrinking toward centre.
        p.rect_filled(rect, r, b);
        let aw = rect.width() * (1.0 - t);
        let ah = rect.height() * (1.0 - t);
        let hole = egui::Rect::from_center_size(rect.center(), egui::vec2(aw, ah));
        p.rect_filled(hole, egui::Rounding::same(2.0), a);
    };
    let circle_in = || {
        // CircleWipeIn — disc grows from centre.
        let radius = rect.width().min(rect.height()) * 0.65 * t;
        p.circle_filled(rect.center(), radius, b);
    };
    let circle_out = || {
        // CircleWipeOut — full B with A disc shrinking.
        p.rect_filled(rect, r, b);
        let radius = rect.width().min(rect.height()) * 0.65 * (1.0 - t);
        p.circle_filled(rect.center(), radius, a);
    };
    let iris = || {
        // Iris — circle grows but with a thin ring outline so it
        // visually distinguishes from CircleWipeIn.
        let radius = rect.width().min(rect.height()) * 0.65 * t;
        p.circle_filled(rect.center(), radius, b);
        p.circle_stroke(
            rect.center(),
            radius + 1.0,
            egui::Stroke::new(2.0_f32, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 180)),
        );
    };
    let clock_wipe = || {
        // Sector growing clockwise from 12 o'clock.
        let centre = rect.center();
        let radius = rect.width().max(rect.height()) * 0.8;
        let segments = 64;
        let total_angle = std::f32::consts::TAU * t;
        let mut pts: Vec<egui::Pos2> = Vec::with_capacity(segments + 2);
        pts.push(centre);
        for i in 0..=segments {
            let frac = i as f32 / segments as f32;
            let theta = -std::f32::consts::FRAC_PI_2 + frac * total_angle;
            pts.push(egui::pos2(
                centre.x + theta.cos() * radius,
                centre.y + theta.sin() * radius,
            ));
        }
        if pts.len() >= 3 {
            p.add(egui::Shape::convex_polygon(pts, b, egui::Stroke::NONE));
        }
    };
    let star_wipe = || {
        // 5-pointed star growing from centre.
        let centre = rect.center();
        let outer = rect.width().min(rect.height()) * 0.45 * t;
        let inner = outer * 0.4;
        let mut pts: Vec<egui::Pos2> = Vec::with_capacity(10);
        for i in 0..10 {
            let theta = -std::f32::consts::FRAC_PI_2
                + i as f32 * std::f32::consts::PI / 5.0;
            let r = if i % 2 == 0 { outer } else { inner };
            pts.push(egui::pos2(
                centre.x + theta.cos() * r,
                centre.y + theta.sin() * r,
            ));
        }
        if pts.len() >= 3 {
            p.add(egui::Shape::convex_polygon(pts, b, egui::Stroke::NONE));
        }
    };
    let venetian_h = || {
        // Multiple horizontal strips that grow vertically.
        let strips = 5;
        let strip_h = rect.height() / strips as f32;
        let grow = strip_h * t;
        for i in 0..strips {
            let y = rect.min.y + i as f32 * strip_h;
            let band = egui::Rect::from_min_size(
                egui::pos2(rect.min.x, y),
                egui::vec2(rect.width(), grow),
            );
            p.rect_filled(band, egui::Rounding::ZERO, b);
        }
    };
    let venetian_v = || {
        let strips = 6;
        let strip_w = rect.width() / strips as f32;
        let grow = strip_w * t;
        for i in 0..strips {
            let x = rect.min.x + i as f32 * strip_w;
            let band = egui::Rect::from_min_size(
                egui::pos2(x, rect.min.y),
                egui::vec2(grow, rect.height()),
            );
            p.rect_filled(band, egui::Rounding::ZERO, b);
        }
    };
    let checker = || {
        let cols = 6;
        let rows = 4;
        let cw = rect.width() / cols as f32;
        let ch = rect.height() / rows as f32;
        for cy in 0..rows {
            for cx in 0..cols {
                // Each cell reveals at a slightly different t
                // (diagonal sweep) so the pattern reads as a
                // "checkerboard wipe" not a simultaneous flash.
                let cell_t_start =
                    (cx + cy) as f32 / (cols + rows) as f32;
                let cell_t = ((t - cell_t_start) * 2.0).clamp(0.0, 1.0);
                if cell_t > 0.0 && (cx + cy) % 2 == 0 {
                    let cell = egui::Rect::from_min_size(
                        egui::pos2(
                            rect.min.x + cx as f32 * cw,
                            rect.min.y + cy as f32 * ch,
                        ),
                        egui::vec2(cw, ch),
                    );
                    p.rect_filled(cell, egui::Rounding::ZERO, b);
                }
            }
        }
        // Filled-cell sweep finishes when t crosses 1; remaining
        // cells fill in to complete the canvas.
        if t > 0.85 {
            fade_over(b, (t - 0.85) / 0.15);
        }
    };
    let pixelate = || {
        // Coarse mosaic of B blocks growing in resolution.
        let cells = (4.0 + t * 8.0) as i32;
        let cw = rect.width() / cells as f32;
        let ch = rect.height() / cells as f32;
        for cy in 0..cells {
            for cx in 0..cells {
                let cell_t = ((cx + cy) as f32 / (cells * 2) as f32 + t).clamp(0.0, 1.0);
                if cell_t > 0.5 {
                    let cell = egui::Rect::from_min_size(
                        egui::pos2(
                            rect.min.x + cx as f32 * cw,
                            rect.min.y + cy as f32 * ch,
                        ),
                        egui::vec2(cw + 1.0, ch + 1.0),
                    );
                    p.rect_filled(cell, egui::Rounding::ZERO, b);
                }
            }
        }
    };
    let squeeze_h = || {
        // A squeezes horizontally toward centre, B fills the
        // exposed strips on either side.
        let collapsed_half = rect.width() * 0.5 * (1.0 - t);
        let a_block = egui::Rect::from_center_size(
            rect.center(),
            egui::vec2(collapsed_half * 2.0, rect.height()),
        );
        let left_strip = egui::Rect::from_min_max(
            rect.min,
            egui::pos2(a_block.min.x, rect.max.y),
        );
        let right_strip = egui::Rect::from_min_max(
            egui::pos2(a_block.max.x, rect.min.y),
            rect.max,
        );
        p.rect_filled(left_strip, r, b);
        p.rect_filled(right_strip, r, b);
    };
    let squeeze_v = || {
        let collapsed_half = rect.height() * 0.5 * (1.0 - t);
        let a_block = egui::Rect::from_center_size(
            rect.center(),
            egui::vec2(rect.width(), collapsed_half * 2.0),
        );
        let top_strip = egui::Rect::from_min_max(
            rect.min,
            egui::pos2(rect.max.x, a_block.min.y),
        );
        let bot_strip = egui::Rect::from_min_max(
            egui::pos2(rect.min.x, a_block.max.y),
            rect.max,
        );
        p.rect_filled(top_strip, r, b);
        p.rect_filled(bot_strip, r, b);
    };
    let zoom_in_dissolve = || {
        // A scales up while fading to B.
        // Visualize: a larger A rect overshooting the canvas, then
        // B revealed underneath as alpha rises.
        let scale = 1.0 + t * 0.6;
        let big = egui::Rect::from_center_size(
            rect.center(),
            egui::vec2(rect.width() * scale, rect.height() * scale),
        );
        p.rect_filled(big, r, a);
        fade_over(b, t);
    };
    let zoom_out_dissolve = || {
        // A scales down to nothing while B replaces it.
        let scale = (1.0 - t * 0.7).max(0.0);
        let small = egui::Rect::from_center_size(
            rect.center(),
            egui::vec2(rect.width() * scale, rect.height() * scale),
        );
        p.rect_filled(rect, r, b);
        p.rect_filled(small, egui::Rounding::same(2.0), a);
    };
    let cross_zoom = || {
        // Both clips zoom toward centre with a flash mid-way.
        let bell = (t * std::f32::consts::PI).sin();
        fade_over(egui::Color32::WHITE, bell * 0.7);
        let scale = 1.0 + bell * 0.3;
        let big = egui::Rect::from_center_size(
            rect.center(),
            egui::vec2(rect.width() * scale, rect.height() * scale),
        );
        if t < 0.5 {
            p.rect_filled(big, r, a);
        } else {
            p.rect_filled(big, r, b);
        }
    };
    let page_turn = |reverse: bool| {
        // Diagonal "page peel" from top-right (or top-left if reverse).
        let w = rect.width();
        let h = rect.height();
        let curl = w.min(h) * 0.7 * t;
        let pts = if !reverse {
            vec![
                rect.max,
                egui::pos2(rect.max.x - curl, rect.max.y),
                egui::pos2(rect.max.x, rect.max.y - curl),
            ]
        } else {
            vec![
                egui::pos2(rect.min.x, rect.max.y),
                egui::pos2(rect.min.x + curl, rect.max.y),
                egui::pos2(rect.min.x, rect.max.y - curl),
            ]
        };
        p.add(egui::Shape::convex_polygon(pts, b, egui::Stroke::NONE));
        // Fade the rest as the page completes.
        if t > 0.7 {
            fade_over(b, (t - 0.7) / 0.3);
        }
    };
    let ripple = || {
        // Concentric expanding rings, dissolving to B underneath.
        let max_r = rect.width().max(rect.height()) * 0.7;
        for i in 0..4 {
            let phase = (t + i as f32 * 0.18).rem_euclid(1.0);
            let radius = phase * max_r;
            let alpha = ((1.0 - phase) * 200.0) as u8;
            p.circle_stroke(
                rect.center(),
                radius,
                egui::Stroke::new(
                    2.0_f32,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, alpha),
                ),
            );
        }
        fade_over(b, t);
    };
    let film_burn = || {
        // Orange/yellow bloom growing from a corner, eating the canvas.
        let centre = egui::pos2(rect.max.x, rect.min.y);
        let radius = (rect.width() + rect.height()) * 0.9 * t;
        p.circle_filled(centre, radius, egui::Color32::from_rgba_unmultiplied(0xff, 0x80, 0x20, 220));
        if t > 0.6 {
            fade_over(b, (t - 0.6) / 0.4);
        }
    };
    let light_leak = || {
        // Diagonal warm streak across the canvas.
        let pulse = (t * std::f32::consts::PI).sin();
        let x = rect.min.x + rect.width() * t;
        let band = egui::Rect::from_center_size(
            egui::pos2(x, rect.center().y),
            egui::vec2(rect.width() * 0.35, rect.height()),
        );
        p.rect_filled(
            band,
            egui::Rounding::same(2.0),
            egui::Color32::from_rgba_unmultiplied(0xff, 0xc0, 0x60, (pulse * 220.0) as u8),
        );
        if t > 0.5 {
            fade_over(b, (t - 0.5) * 2.0);
        }
    };
    let halation = || {
        let pulse = (t * std::f32::consts::PI).sin();
        let alpha = (pulse * 200.0) as u8;
        p.circle_filled(
            rect.center(),
            rect.width().max(rect.height()) * 0.4,
            egui::Color32::from_rgba_unmultiplied(0xff, 0xa0, 0x60, alpha),
        );
        fade_over(b, t);
    };
    let anamorphic_flare = || {
        // Horizontal lens streak across the centre.
        let band = egui::Rect::from_center_size(
            rect.center(),
            egui::vec2(rect.width() * 0.95, 8.0),
        );
        let pulse = (t * std::f32::consts::PI).sin();
        let alpha = (pulse * 235.0) as u8;
        p.rect_filled(
            band,
            egui::Rounding::same(4.0),
            egui::Color32::from_rgba_unmultiplied(0xff, 0xff, 0xc0, alpha),
        );
        fade_over(b, t);
    };
    let bokeh = || {
        // Random discs fading in.
        let positions = [(0.2, 0.3), (0.7, 0.45), (0.4, 0.7), (0.85, 0.8), (0.15, 0.85)];
        for (i, (px, py)) in positions.iter().enumerate() {
            let phase_t = ((t + i as f32 * 0.07) - 0.1).clamp(0.0, 1.0);
            let radius = phase_t * 12.0;
            let alpha = (phase_t * 200.0) as u8;
            p.circle_filled(
                egui::pos2(rect.min.x + px * rect.width(), rect.min.y + py * rect.height()),
                radius,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, alpha),
            );
        }
        fade_over(b, t);
    };
    let dream = || {
        // Gentle haze + slow B fade.
        fade_over(egui::Color32::WHITE, (t * std::f32::consts::PI).sin() * 0.5);
        fade_over(b, t);
    };
    let chromatic = || {
        // RGB split bands across the canvas.
        let off = rect.width() * 0.05 * (t * std::f32::consts::PI).sin();
        let band_h = rect.height();
        let r_band = egui::Rect::from_min_size(
            rect.min + egui::vec2(off, 0.0),
            egui::vec2(rect.width() * 0.5, band_h),
        );
        let b_band = egui::Rect::from_min_size(
            rect.min + egui::vec2(-off + rect.width() * 0.5, 0.0),
            egui::vec2(rect.width() * 0.5, band_h),
        );
        p.rect_filled(
            r_band,
            r,
            egui::Color32::from_rgba_unmultiplied(255, 80, 80, 130),
        );
        p.rect_filled(
            b_band,
            r,
            egui::Color32::from_rgba_unmultiplied(80, 80, 255, 130),
        );
        fade_over(b, t);
    };
    let static_tv = || {
        // Random horizontal noise bars.
        let bars = 7;
        let bh = rect.height() / bars as f32;
        for i in 0..bars {
            let phase = (t * 11.0 + i as f32 * 1.7).sin();
            let alpha = ((phase * 0.5 + 0.5) * 200.0) as u8;
            let y = rect.min.y + i as f32 * bh;
            let band = egui::Rect::from_min_size(
                egui::pos2(rect.min.x, y),
                egui::vec2(rect.width(), bh - 1.0),
            );
            p.rect_filled(
                band,
                egui::Rounding::ZERO,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, alpha),
            );
        }
        fade_over(b, t);
    };
    let vhs_tracking = || {
        // Single rolling band moving down the canvas.
        let band_h = rect.height() * 0.18;
        let y = rect.min.y + (t * (rect.height() + band_h)) - band_h;
        let band = egui::Rect::from_min_size(
            egui::pos2(rect.min.x, y),
            egui::vec2(rect.width(), band_h),
        );
        p.rect_filled(
            band.intersect(rect),
            egui::Rounding::ZERO,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200),
        );
        fade_over(b, t);
    };
    let glitch_dissolve = || {
        // Horizontal slices offset randomly.
        let bars = 12;
        let bh = rect.height() / bars as f32;
        for i in 0..bars {
            let stagger = ((i as f32 * 0.27 + t * 4.0).sin() * 0.5 + 0.5).clamp(0.0, 1.0);
            if stagger > 1.0 - t {
                let y = rect.min.y + i as f32 * bh;
                let off = ((i as f32 * 1.13 + t * 6.0).sin()) * rect.width() * 0.08;
                let band = egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + off, y),
                    egui::vec2(rect.width(), bh - 0.5),
                );
                p.rect_filled(band.intersect(rect), egui::Rounding::ZERO, b);
            }
        }
    };
    let glitch_rgb_split = || {
        let off = (t * 12.0).sin() * 8.0;
        p.rect_filled(
            rect.translate(egui::vec2(off, 0.0)).intersect(rect),
            r,
            egui::Color32::from_rgba_unmultiplied(255, 80, 80, 110),
        );
        p.rect_filled(
            rect.translate(egui::vec2(-off, 0.0)).intersect(rect),
            r,
            egui::Color32::from_rgba_unmultiplied(80, 100, 255, 110),
        );
        fade_over(b, t);
    };
    let projector_flicker = || {
        let flick = ((t * 30.0).sin() * 0.5 + 0.5).clamp(0.0, 1.0);
        let alpha = (flick * 235.0) as u8;
        p.rect_filled(
            rect,
            r,
            egui::Color32::from_rgba_unmultiplied(255, 240, 200, alpha),
        );
        fade_over(b, t);
    };
    let swirl = || {
        // Concentric arcs rotating — fake a vortex.
        let centre = rect.center();
        let max_r = rect.width().max(rect.height()) * 0.6;
        for i in 0..6 {
            let phase = t * std::f32::consts::TAU + i as f32 * 0.5;
            let radius = max_r * (i as f32 / 6.0);
            for j in 0..12 {
                let theta = phase + j as f32 * std::f32::consts::TAU / 12.0;
                let pos = egui::pos2(
                    centre.x + theta.cos() * radius,
                    centre.y + theta.sin() * radius,
                );
                p.circle_filled(
                    pos,
                    1.5,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 150),
                );
            }
        }
        fade_over(b, t);
    };
    let wave_distortion = || {
        // Sine wave rolling across the canvas as a horizontal band.
        let cols = 32;
        let cw = rect.width() / cols as f32;
        for i in 0..cols {
            let x = rect.min.x + i as f32 * cw;
            let phase = i as f32 * 0.4 + t * 8.0;
            let y_off = phase.sin() * rect.height() * 0.06;
            let band = egui::Rect::from_min_size(
                egui::pos2(x, rect.min.y + y_off),
                egui::vec2(cw + 1.0, rect.height()),
            );
            p.rect_filled(band.intersect(rect), egui::Rounding::ZERO, b);
        }
    };
    let spin = || {
        // Approximation of a rotation: scale jitter + flash mid-way.
        let bell = (t * std::f32::consts::PI).sin();
        let scale = 1.0 - 0.3 * bell;
        let block = egui::Rect::from_center_size(
            rect.center(),
            egui::vec2(rect.width() * scale, rect.height() * scale),
        );
        if t < 0.5 {
            p.rect_filled(block, r, a);
        } else {
            p.rect_filled(block, r, b);
        }
        fade_over(egui::Color32::WHITE, bell * 0.4);
    };

    match kind {
        // ── Plain dissolves and fades ──
        K::CrossDissolve => fade_over(b, t),
        K::FilmDissolve => {
            fade_over(egui::Color32::from_rgba_unmultiplied(255, 220, 180, 60), t);
            fade_over(b, t);
        }
        K::AdditiveDissolve => {
            fade_over(egui::Color32::WHITE, (t * std::f32::consts::PI).sin() * 0.4);
            fade_over(b, t);
        }
        K::LuminanceDissolve => fade_over(b, t),
        K::BlurDissolve => {
            // Lay down B then a soft white veil to suggest blur.
            fade_over(b, t);
            fade_over(
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60),
                (1.0 - (t - 0.5).abs() * 2.0).max(0.0),
            );
        }
        K::DipToBlack => dip_through(egui::Color32::BLACK),
        K::DipToWhite => dip_through(egui::Color32::WHITE),
        K::FadeToColor => dip_through(egui::Color32::from_rgb(0xc4, 0x6e, 0x3d)),
        K::Desaturate => {
            // Fade saturation to grey then to B.
            fade_over(egui::Color32::from_rgba_unmultiplied(120, 120, 120, 200), t);
            fade_over(b, t);
        }
        K::Sepia => {
            fade_over(egui::Color32::from_rgba_unmultiplied(120, 95, 60, 180), t);
            fade_over(b, t);
        }
        K::HueRotate => {
            // Cycle through a few hues as t advances.
            let hue_color = match (t * 4.0) as i32 {
                0 => egui::Color32::from_rgba_unmultiplied(220, 80, 80, 150),
                1 => egui::Color32::from_rgba_unmultiplied(220, 220, 80, 150),
                2 => egui::Color32::from_rgba_unmultiplied(80, 220, 80, 150),
                _ => egui::Color32::from_rgba_unmultiplied(80, 80, 220, 150),
            };
            fade_over(hue_color, 1.0);
            fade_over(b, t);
        }
        K::ColorizeFlash => {
            let bell = (t * std::f32::consts::PI).sin();
            fade_over(
                egui::Color32::from_rgba_unmultiplied(0xff, 0xa0, 0x40, 220),
                bell,
            );
            if t > 0.5 {
                fade_over(b, (t - 0.5) * 2.0);
            }
        }
        K::NegativeFlash => {
            let bell = (t * std::f32::consts::PI).sin();
            fade_over(
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 235),
                bell,
            );
            if t > 0.5 {
                fade_over(b, (t - 0.5) * 2.0);
            }
        }

        // ── Directional wipes ──
        K::WipeLeft => wipe_h(false),  // boundary moves right→left, B revealed on the right
        K::WipeRight => wipe_h(true),
        K::WipeUp => wipe_v(false),
        K::WipeDown => wipe_v(true),
        K::DiagonalWipeTLBR => diagonal_wipe(true),
        K::DiagonalWipeTRBL => diagonal_wipe(false),

        // ── Slides + Push (whole frame moves) ──
        K::SlideLeft => slide_in_from(1.0, 0.0),  // B comes in from the right, ends up filling
        K::SlideRight => slide_in_from(-1.0, 0.0),
        K::SlideUp => slide_in_from(0.0, 1.0),
        K::SlideDown => slide_in_from(0.0, -1.0),
        K::PushLeft => push(-1.0, 0.0),
        K::CoverLeft => slide_in_from(1.0, 0.0),
        K::CoverRight => slide_in_from(-1.0, 0.0),
        K::UncoverLeft => {
            // A slides off to the left, exposing B underneath.
            p.rect_filled(rect, r, b);
            let off = -t * rect.width();
            p.rect_filled(
                rect.translate(egui::vec2(off, 0.0)).intersect(rect),
                r,
                a,
            );
        }
        K::UncoverRight => {
            p.rect_filled(rect, r, b);
            let off = t * rect.width();
            p.rect_filled(
                rect.translate(egui::vec2(off, 0.0)).intersect(rect),
                r,
                a,
            );
        }

        // ── Squeezes ──
        K::SqueezeH => squeeze_h(),
        K::SqueezeV => squeeze_v(),

        // ── Box / circle / iris / shape wipes ──
        K::BoxWipeIn => box_in(),
        K::BoxWipeOut => box_out(),
        K::CircleWipeIn => circle_in(),
        K::CircleWipeOut => circle_out(),
        K::ClockWipe => clock_wipe(),
        K::StarWipe => star_wipe(),

        // ── Splits / barn doors ──
        K::BarnDoorOpen => barn_open(),
        K::BarnDoorClose => barn_close(),
        K::SplitWipeHorizontal => split_h(),
        K::SplitWipeVertical => split_v(),

        // ── Pattern wipes ──
        K::VenetianBlindHorizontal => venetian_h(),
        K::VenetianBlindVertical => venetian_v(),
        K::CheckerboardWipe => checker(),
        K::Pixelate => pixelate(),

        // ── Zooms ──
        K::ZoomInDissolve => zoom_in_dissolve(),
        K::ZoomOutDissolve => zoom_out_dissolve(),
        K::CrossZoom => cross_zoom(),

        // ── Effect / glitch ──
        K::Flash => fade_over(
            egui::Color32::WHITE,
            (t * std::f32::consts::PI).sin() * 0.95,
        ),
        K::Spin => spin(),
        K::PageTurn => page_turn(false),
        K::PageTurnReverse => page_turn(true),
        K::Ripple => ripple(),
        K::FilmBurn => film_burn(),
        K::LightLeak => light_leak(),
        K::Halation => halation(),
        K::AnamorphicFlare => anamorphic_flare(),
        K::DreamSequence => dream(),
        K::BokehDissolve => bokeh(),
        K::ChromaticAberration => chromatic(),
        K::StaticTV => static_tv(),
        K::VHSTracking => vhs_tracking(),
        K::GlitchDissolve => glitch_dissolve(),
        K::GlitchRGBSplit => glitch_rgb_split(),
        K::ProjectorFlicker => projector_flicker(),
        K::Swirl => swirl(),
        K::WaveDistortion => wave_distortion(),
        // Iris isn't its own variant — it's used as a pseudonym in
        // other UI but maps to circle-wipe-in territory; keep this
        // arm here in case the registry adds it later. The current
        // exhaustive match doesn't actually need it.
    }

    let _ = iris; // silence unused-closure warning for the iris
                  // helper kept around for future Iris* variants.
}

fn paint_corner_badge(
    ui: &egui::Ui,
    theme: &KlippTheme,
    rect: egui::Rect,
    label: &str,
    right_aligned: bool,
) {
    let painter = ui.painter();
    let font = egui::FontId::proportional(10.0);
    let galley = painter.layout_no_wrap(
        label.to_string(),
        font.clone(),
        theme.text_secondary,
    );
    let pad_x = 7.0;
    let h = 17.0;
    let w = galley.size().x + pad_x * 2.0;
    let pos = if right_aligned {
        egui::pos2(rect.max.x - w - 8.0, rect.min.y + 8.0)
    } else {
        egui::pos2(rect.min.x + 8.0, rect.min.y + 8.0)
    };
    let badge_rect = egui::Rect::from_min_size(pos, egui::vec2(w, h));
    painter.rect_filled(
        badge_rect,
        egui::Rounding::same(2.0),
        egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180),
    );
    painter.galley(
        egui::pos2(
            badge_rect.min.x + pad_x,
            badge_rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        theme.text_secondary,
    );
}

// ─────────────── Sidebar widgets ───────────────

fn category_icon(cat: TransitionCategory) -> Icon {
    match cat {
        TransitionCategory::Dissolve => Icon::Eye,
        TransitionCategory::Wipe => Icon::CaretRight,
        TransitionCategory::Slide => Icon::CaretRight,
        TransitionCategory::Zoom => Icon::ArrowsOut,
        TransitionCategory::Effect => Icon::Lightning,
        TransitionCategory::Cinematic => Icon::FilmStrip,
    }
}

#[allow(clippy::too_many_arguments)]
fn tx_sidebar_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    icon: Option<Icon>,
    label: &str,
    count: usize,
    is_active: bool,
    id_salt: &str,
    on_click: &mut dyn FnMut(Option<TransitionCategory>),
    target: Option<TransitionCategory>,
) {
    let row_h = 26.0;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), row_h),
        egui::Sense::click(),
    );
    let click_resp = ui.interact(rect, ui.id().with(id_salt), egui::Sense::click());
    let hovered = resp.hovered() || click_resp.hovered();
    if is_active {
        ui.painter().rect_filled(rect, 0.0, theme.accent_subtle);
        let stripe = egui::Rect::from_min_size(rect.min, egui::vec2(2.0, rect.height()));
        ui.painter().rect_filled(stripe, 0.0, theme.accent);
    } else if hovered {
        ui.painter().rect_filled(rect, 0.0, theme.bg_raised);
    }
    let indent = 14.0;
    let icon_size = 13.0;
    let icon_gap = 8.0;
    let label_x = rect.min.x + indent + icon_size + icon_gap;
    let label_color = if is_active || hovered {
        theme.text_primary
    } else {
        theme.text_secondary
    };
    let icon_tint = if is_active { theme.accent } else { theme.text_dim };
    if let Some(icon) = icon {
        let icon_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x + indent, rect.center().y - icon_size * 0.5),
            egui::vec2(icon_size, icon_size),
        );
        let img = icon.image(icon_size).tint(icon_tint);
        ui.allocate_new_ui(
            egui::UiBuilder::new().max_rect(icon_rect),
            |ui| {
                ui.add(img);
            },
        );
    }
    let label_galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::FontId::proportional(11.0),
        label_color,
    );
    ui.painter().galley(
        egui::pos2(label_x, rect.center().y - label_galley.size().y * 0.5),
        label_galley,
        label_color,
    );
    let count_color = if is_active { theme.accent } else { theme.text_dim };
    let count_galley = ui.painter().layout_no_wrap(
        format!("{}", count),
        egui::FontId::monospace(10.0),
        count_color,
    );
    ui.painter().galley(
        egui::pos2(
            rect.max.x - 14.0 - count_galley.size().x,
            rect.center().y - count_galley.size().y * 0.5,
        ),
        count_galley,
        count_color,
    );
    if click_resp.clicked() || resp.clicked() {
        on_click(target);
    }
}

fn tx_general_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    icon: Icon,
    label: &str,
    count: usize,
    id_salt: &str,
) {
    let row_h = 26.0;
    let (rect, _resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), row_h),
        egui::Sense::click(),
    );
    let click_resp = ui.interact(rect, ui.id().with(id_salt), egui::Sense::click());
    if click_resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, theme.bg_raised);
    }
    let indent = 14.0;
    let icon_size = 13.0;
    let icon_gap = 8.0;
    let label_x = rect.min.x + indent + icon_size + icon_gap;
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x + indent, rect.center().y - icon_size * 0.5),
        egui::vec2(icon_size, icon_size),
    );
    let img = icon.image(icon_size).tint(theme.text_dim);
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(icon_rect),
        |ui| {
            ui.add(img);
        },
    );
    let label_galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::FontId::proportional(11.0),
        theme.text_secondary,
    );
    ui.painter().galley(
        egui::pos2(label_x, rect.center().y - label_galley.size().y * 0.5),
        label_galley,
        theme.text_secondary,
    );
    let count_galley = ui.painter().layout_no_wrap(
        format!("{}", count),
        egui::FontId::monospace(10.0),
        theme.text_dim,
    );
    ui.painter().galley(
        egui::pos2(
            rect.max.x - 14.0 - count_galley.size().x,
            rect.center().y - count_galley.size().y * 0.5,
        ),
        count_galley,
        theme.text_dim,
    );
}

fn tx_section_separator(ui: &mut egui::Ui, theme: &KlippTheme) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 1.0),
        egui::Sense::hover(),
    );
    ui.painter().hline(
        (rect.min.x + 8.0)..=(rect.max.x - 8.0),
        rect.center().y,
        egui::Stroke::new(1.0_f32, theme.border),
    );
    ui.add_space(4.0);
}

fn tx_section_header(ui: &mut egui::Ui, theme: &KlippTheme, label: &str) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 20.0),
        egui::Sense::hover(),
    );
    ui.painter().text(
        egui::pos2(rect.min.x + 14.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label.to_uppercase(),
        egui::FontId::proportional(10.0),
        theme.text_dim,
    );
}

// ─────────────── Filter row widgets ───────────────

fn tx_segmented_filter(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    filter: &mut TransitionsSourceFilter,
    all_count: usize,
    builtin_count: usize,
    ofx_count: usize,
) {
    let outer_h = 26.0;
    let pad = 2.0;
    let segments = [
        (TransitionsSourceFilter::All, "All", all_count),
        (TransitionsSourceFilter::Builtin, "Built-in", builtin_count),
        (TransitionsSourceFilter::Ofx, "OFX", ofx_count),
    ];
    let mut seg_widths = [0.0f32; 3];
    let mut total_w = pad * 2.0;
    for (i, (_, label, count)) in segments.iter().enumerate() {
        let lg = ui.painter().layout_no_wrap(
            (*label).to_string(),
            egui::FontId::proportional(11.0),
            theme.text_secondary,
        );
        let cg = ui.painter().layout_no_wrap(
            format!("{}", count),
            egui::FontId::monospace(10.0),
            theme.text_dim,
        );
        let w = lg.size().x + 6.0 + cg.size().x + 22.0;
        seg_widths[i] = w;
        total_w += w + 1.0;
    }
    total_w -= 1.0;
    let (outer_rect, _) = ui.allocate_exact_size(
        egui::vec2(total_w, outer_h),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(outer_rect, 4.0, theme.bg_panel);
    ui.painter().rect_stroke(
        outer_rect,
        5.0,
        egui::Stroke::new(1.0_f32, theme.border),
    );
    let mut x = outer_rect.min.x + pad;
    for (i, (variant, label, count)) in segments.iter().enumerate() {
        let seg_rect = egui::Rect::from_min_size(
            egui::pos2(x, outer_rect.min.y + pad),
            egui::vec2(seg_widths[i], outer_h - pad * 2.0),
        );
        let resp = ui.interact(
            seg_rect,
            ui.id().with(("tx_source_seg", i)),
            egui::Sense::click(),
        );
        let is_active = *filter == *variant;
        let hovered = resp.hovered();
        if is_active {
            ui.painter().rect_filled(seg_rect, 2.0, theme.bg_raised);
        } else if hovered {
            ui.painter().rect_filled(seg_rect, 2.0, theme.accent_subtle);
        }
        let label_color = if is_active || hovered { theme.text_primary } else { theme.text_secondary };
        let count_color = if is_active { theme.accent } else { theme.text_dim };
        let lg = ui.painter().layout_no_wrap(
            (*label).to_string(),
            egui::FontId::proportional(11.0),
            label_color,
        );
        let cg = ui.painter().layout_no_wrap(
            format!("{}", count),
            egui::FontId::monospace(10.0),
            count_color,
        );
        let inner_w = lg.size().x + 6.0 + cg.size().x;
        let lx = seg_rect.center().x - inner_w * 0.5;
        ui.painter().galley(
            egui::pos2(lx, seg_rect.center().y - lg.size().y * 0.5),
            lg.clone(),
            label_color,
        );
        ui.painter().galley(
            egui::pos2(
                lx + lg.size().x + 6.0,
                seg_rect.center().y - cg.size().y * 0.5,
            ),
            cg,
            count_color,
        );
        if resp.clicked() {
            *filter = *variant;
        }
        x += seg_widths[i] + 1.0;
    }
}

fn gpu_only_pill(ui: &mut egui::Ui, theme: &KlippTheme, active: &mut bool) {
    let lg = ui.painter().layout_no_wrap(
        "GPU only".to_string(),
        egui::FontId::proportional(11.0),
        theme.text_secondary,
    );
    let icon_size = 11.0;
    let pad_x = 10.0;
    let gap = 5.0;
    let w = pad_x * 2.0 + icon_size + gap + lg.size().x;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(w, 26.0),
        egui::Sense::click(),
    );
    let hovered = resp.hovered();
    let (bg, border, fg) = if *active {
        (theme.accent_subtle, theme.accent, theme.accent)
    } else if hovered {
        (theme.bg_raised, theme.border, theme.text_primary)
    } else {
        (theme.bg_panel, theme.border, theme.text_secondary)
    };
    ui.painter().rect_filled(rect, 4.0, bg);
    ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(1.0_f32, border));
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x + pad_x, rect.center().y - icon_size * 0.5),
        egui::vec2(icon_size, icon_size),
    );
    let img = Icon::Lightning.image(icon_size).tint(fg);
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(icon_rect), |ui| { ui.add(img); });
    ui.painter().text(
        egui::pos2(rect.min.x + pad_x + icon_size + gap, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "GPU only",
        egui::FontId::proportional(11.0),
        fg,
    );
    if resp.clicked() {
        *active = !*active;
    }
}

fn view_mode_button(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    current: &mut EffectViewMode,
    target: EffectViewMode,
    icon: Icon,
    tooltip: &str,
) {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(26.0, 26.0),
        egui::Sense::click(),
    );
    let is_active = *current == target;
    let hovered = resp.hovered();
    let (bg, border, fg) = if is_active {
        (theme.accent_subtle, theme.accent, theme.accent)
    } else if hovered {
        (theme.bg_raised, theme.border, theme.text_primary)
    } else {
        (theme.bg_panel, theme.border, theme.text_secondary)
    };
    ui.painter().rect_filled(rect, 4.0, bg);
    ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(1.0_f32, border));
    let size = 13.0;
    let icon_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(size, size));
    let img = icon.image(size).tint(fg);
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(icon_rect), |ui| { ui.add(img); });
    let _ = resp.clone().on_hover_text(tooltip);
    if resp.clicked() {
        *current = target;
    }
}

// ─────────────── Result strip ───────────────

fn tx_result_strip(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    count: usize,
    sort_mode: &mut EffectSort,
) {
    let avail_w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(avail_w, 24.0),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(rect, 0.0, theme.bg_panel);
    ui.painter().hline(
        rect.x_range(),
        rect.max.y,
        egui::Stroke::new(1.0_f32, theme.border),
    );
    ui.painter().text(
        egui::pos2(rect.min.x + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        format!("{} Transitions", count),
        egui::FontId::proportional(11.0),
        theme.text_secondary,
    );
    let dropdown_w = 116.0;
    let dropdown_rect = egui::Rect::from_min_max(
        egui::pos2(rect.max.x - dropdown_w - 8.0, rect.min.y + 1.0),
        egui::pos2(rect.max.x - 8.0, rect.max.y - 1.0),
    );
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(dropdown_rect), |ui| {
        ui.style_mut().visuals.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
        ui.style_mut().visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
        let label = format!("Sort: {}", sort_mode.label());
        egui::ComboBox::from_id_salt("transitions_sort_dd")
            .selected_text(egui::RichText::new(label).size(11.0).color(theme.text_dim))
            .width(dropdown_w - 4.0)
            .show_ui(ui, |ui| {
                for variant in [EffectSort::NameAsc, EffectSort::NameDesc, EffectSort::Category] {
                    ui.selectable_value(sort_mode, variant, variant.label());
                }
            });
    });
}
