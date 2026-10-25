//! The About Klipp dialog. A hero with the identity on top, a few lines of
//! facts below, then the ways onward (updates, source, issues). An update
//! check runs in place: its result is a row under the buttons.
//!
//! The mark and the wordmark are drawn as shapes, from the same geometry
//! as `assets/branding/klipp-refined/build.py`, so they can move: when the
//! dialog opens the K is whole, then its leg is cut and slips into place,
//! and a warm sheen passes over the identity now and then.

use egui::{pos2, vec2, Color32, Pos2, Rect, Stroke};

use crate::theme::KlippTheme;
use crate::updates::{Install, Status};

/// What the person asked for in the dialog this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AboutAction {
    None,
    Close,
    CheckForUpdates,
    /// Open the package's updater for the version the check found.
    Install,
}

const SOURCE: &str = "https://github.com/davidkodar/klipp";
const ISSUES: &str = "https://github.com/davidkodar/klipp/issues";

const WIDTH: f32 = 460.0;
const HERO: f32 = 288.0;
const ROUND: f32 = 12.0;

// Brand colours: the mark keeps them whatever the theme.
const AMBER: Color32 = Color32::from_rgb(0xD9, 0x9A, 0x38);
const TILE: Color32 = Color32::from_rgb(0x24, 0x26, 0x29);
const WHITE: Color32 = Color32::from_rgb(0xEA, 0xE7, 0xE0);

/// When the K is cut, and how long the leg takes to slip.
const CUT_AT: f64 = 0.28;
const CUT_FOR: f64 = 0.42;
/// The sheen: first pass, the time between passes, and one pass's length.
const SHEEN_AT: f64 = 0.75;
const SHEEN_EVERY: f64 = 7.0;
const SHEEN_FOR: f64 = 1.5;

/// Draw the dialog, with the update check where `status` says it is.
/// Call it each frame while it is open.
pub(crate) fn show(ctx: &egui::Context, theme: &KlippTheme, status: &Status<'_>) -> AboutAction {
    let mut action = AboutAction::None;
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        action = AboutAction::Close;
    }
    let t = seconds_open(ctx);
    let frame = egui::Frame::window(&ctx.style())
        .fill(theme.bg_panel)
        .stroke(Stroke::new(1.0_f32, theme.border))
        .rounding(ROUND)
        .inner_margin(0.0);
    egui::Window::new("About Klipp")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .frame(frame)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_width(WIDTH);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            if hero(ui, theme, t) {
                action = AboutAction::Close;
            }
            if let Some(a) = body(ui, theme, status) {
                action = a;
            }
        });
    if let Some(next) = next_repaint(t) {
        ctx.request_repaint_after(next);
    }
    action
}

/// Seconds since the dialog opened. A dialog not drawn last frame was
/// closed, so this frame opens it again and the animation starts over.
fn seconds_open(ctx: &egui::Context) -> f64 {
    let id = egui::Id::new("about_klipp_opened");
    let now = ctx.input(|i| i.time);
    let frame_nr = ctx.cumulative_pass_nr();
    let opened = ctx.data_mut(|d| {
        let (at, seen) = d.get_temp::<(f64, u64)>(id).unwrap_or((now, frame_nr));
        let at = if frame_nr > seen + 1 { now } else { at };
        d.insert_temp(id, (at, frame_nr));
        at
    });
    now - opened
}

/// How soon to draw again: smoothly while something moves, else at the
/// start of the next sheen.
fn next_repaint(t: f64) -> Option<std::time::Duration> {
    let moving = t < CUT_AT + CUT_FOR || sheen_phase(t).is_some();
    if moving {
        return Some(std::time::Duration::from_millis(16));
    }
    let since = (t - SHEEN_AT).rem_euclid(SHEEN_EVERY);
    Some(std::time::Duration::from_secs_f64((SHEEN_EVERY - since).max(0.05)))
}

fn ease_out(x: f64) -> f32 {
    let x = x.clamp(0.0, 1.0);
    (1.0 - (1.0 - x).powi(3)) as f32
}

fn ease_in_out(x: f64) -> f32 {
    let x = x.clamp(0.0, 1.0);
    (if x < 0.5 { 4.0 * x * x * x } else { 1.0 - (-2.0 * x + 2.0).powi(3) / 2.0 }) as f32
}

/// How far the cut has gone: 0 is a whole K, 1 the leg slipped into place.
fn cut(t: f64) -> f32 {
    ease_out((t - CUT_AT) / CUT_FOR)
}

/// Where the sheen is in its pass, 0 to 1, while one is under way.
fn sheen_phase(t: f64) -> Option<f32> {
    if t < SHEEN_AT {
        return None;
    }
    let p = (t - SHEEN_AT).rem_euclid(SHEEN_EVERY) / SHEEN_FOR;
    (p <= 1.0).then(|| ease_in_out(p))
}

/// The light a sheen at `band` (an x on screen) throws on a point.
#[derive(Clone, Copy)]
struct Sheen {
    band: f32,
    width: f32,
    tilt_origin: f32,
}

impl Sheen {
    fn at(&self, p: Pos2) -> f32 {
        // Leaning a little, as light across a surface would.
        let x = p.x + 0.38 * (p.y - self.tilt_origin);
        let d = (x - self.band) / self.width;
        (-d * d).exp()
    }
}

/// A convex shape drawn in a colour, which the sheen can light.
struct Piece {
    points: Vec<Pos2>,
    colour: Color32,
    highlight: Color32,
}

/// The hero: soft warmth, the icon, the wordmark and the line under it.
/// Returns whether its close button was clicked.
fn hero(ui: &mut egui::Ui, theme: &KlippTheme, t: f64) -> bool {
    let rect = Rect::from_min_size(ui.cursor().min, vec2(WIDTH, HERO));
    let painter = ui.painter_at(rect);
    let dark = ui.visuals().dark_mode;
    painter.rect_filled(rect, egui::Rounding { nw: ROUND, ne: ROUND, sw: 0.0, se: 0.0 }, theme.bg_deep);
    painter.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0_f32, theme.border));

    let appear = ease_out(t / 0.45);
    let rise = (1.0 - appear) * 8.0;
    let cut = cut(t);

    let icon_size = 96.0;
    let icon_rect = Rect::from_center_size(pos2(rect.center().x, rect.top() + 40.0 + icon_size / 2.0 + rise), vec2(icon_size, icon_size));
    warmth(&painter, icon_rect.center(), if dark { 0.10 } else { 0.08 } * appear);

    let mut pieces = Vec::new();
    // The icon: tile, then the K.
    painter.rect_filled(icon_rect, 56.0 * icon_size / 256.0, TILE.gamma_multiply(appear));
    let s = icon_size / 256.0;
    let o = icon_rect.min;
    let at = |x: f32, y: f32| pos2(o.x + x * s, o.y + y * s);
    cut_k(&mut pieces, &at, KGeom { x: 58.0, y: 40.0, h: 176.0, stem: 42.0, arm: 50.0, slip: 12.0, gap: 10.0 }, WHITE, AMBER, cut);

    // The wordmark, cap height 34 points.
    let ws = 34.0 / 140.0;
    let word_w = 514.0 * ws;
    let wo = pos2(rect.center().x - word_w / 2.0, icon_rect.bottom() + 26.0);
    let wat = |x: f32, y: f32| pos2(wo.x + x * ws, wo.y + y * ws);
    let ink = if dark { WHITE } else { TILE };
    cut_k(&mut pieces, &wat, KGeom { x: 0.0, y: 0.0, h: 140.0, stem: 34.0, arm: 40.0, slip: 10.0, gap: 8.0 }, ink, AMBER, cut);
    let rect_piece = |x0: f32, y0: f32, x1: f32, y1: f32, c: Color32| Piece {
        points: vec![wat(x0, y0), wat(x1, y0), wat(x1, y1), wat(x0, y1)],
        colour: c,
        highlight: highlight_for(c),
    };
    pieces.push(rect_piece(152.0, 0.0, 180.0, 140.0, ink)); // l
    pieces.push(rect_piece(206.0, 40.0, 234.0, 140.0, ink)); // i
    pieces.push(rect_piece(206.0, 0.0, 234.0, 28.0, AMBER)); // the dot, cut square
    for x in [260.0, 400.0] {
        pieces.push(rect_piece(x, 40.0, x + 28.0, 184.0, ink)); // p stem
        bowl(&mut pieces, &wat, x + 28.0, ink);
    }

    let sheen = sheen_phase(t).map(|p| {
        let left = icon_rect.left().min(wo.x) - 60.0;
        let right = icon_rect.right().max(wo.x + word_w) + 60.0;
        Sheen { band: left + (right - left) * p, width: 34.0, tilt_origin: rect.center().y }
    });
    for piece in &pieces {
        paint_piece(&painter, piece, sheen, appear);
    }

    // The line under the identity.
    let galley_pos = pos2(rect.center().x, wo.y + 184.0 * ws + 22.0 + rise * 0.5);
    let galley = painter.layout_no_wrap("A native video editor for Linux".to_owned(), egui::FontId::proportional(13.5), theme.text_secondary.gamma_multiply(appear));
    painter.galley(pos2(galley_pos.x - galley.size().x / 2.0, galley_pos.y), galley, Color32::WHITE);

    ui.allocate_rect(rect, egui::Sense::hover());
    close_button(ui, rect, theme)
}

/// A low, warm light behind the icon: amber at the centre, gone at the edge.
fn warmth(painter: &egui::Painter, centre: Pos2, strength: f32) {
    let mut mesh = egui::Mesh::default();
    let inner = Color32::from_rgba_unmultiplied(AMBER.r(), AMBER.g(), AMBER.b(), (255.0 * strength) as u8);
    mesh.colored_vertex(centre, inner);
    let n = 64;
    let (rx, ry) = (190.0, 120.0);
    for i in 0..=n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        mesh.colored_vertex(pos2(centre.x + rx * a.cos(), centre.y + ry * a.sin()), Color32::TRANSPARENT);
    }
    for i in 1..=n as u32 {
        mesh.add_triangle(0, i, i + 1);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// The warm light a colour takes in the sheen.
fn highlight_for(c: Color32) -> Color32 {
    if c == AMBER {
        Color32::from_rgb(0xFF, 0xD2, 0x8A)
    } else if c == TILE {
        AMBER
    } else {
        Color32::from_rgb(0xFF, 0xF1, 0xD6)
    }
}

/// The cut K's measurements, in the units of the shape it sits in.
#[derive(Clone, Copy)]
struct KGeom {
    x: f32,
    y: f32,
    h: f32,
    stem: f32,
    arm: f32,
    slip: f32,
    gap: f32,
}

/// The K as three convex pieces: stem, arm, and the leg, which is cut
/// from the waist and slipped right as `cut` goes from 0 to 1.
fn cut_k(out: &mut Vec<Piece>, at: &dyn Fn(f32, f32) -> Pos2, k: KGeom, main: Color32, leg: Color32, cut: f32) {
    let mid = k.y + k.h / 2.0;
    let g = k.gap / 2.0 * cut;
    let sx = k.x + k.stem;
    let reach = sx + k.h / 2.0 * 0.70;
    let lean = (k.h / 2.0 - k.gap / 2.0) * 0.82;
    let lx = sx + k.h * 2.0 / 176.0 + k.slip * cut;
    // Whole, the leg's top meets the arm's foot: start it there.
    let lx = lx - (1.0 - cut) * k.h * 2.0 / 176.0;
    out.push(Piece { points: vec![at(k.x, k.y), at(sx, k.y), at(sx, k.y + k.h), at(k.x, k.y + k.h)], colour: main, highlight: highlight_for(main) });
    out.push(Piece {
        points: vec![at(sx, mid - g), at(reach, k.y), at(reach + k.arm, k.y), at(sx + k.arm, mid - g)],
        colour: main,
        highlight: highlight_for(main),
    });
    // Until it is cut the leg wears the K's colour, then warms to amber.
    let leg_colour = lerp(main, leg, cut);
    out.push(Piece {
        points: vec![at(lx, mid + g), at(lx + k.arm, mid + g), at(lx + k.arm + lean, k.y + k.h), at(lx + lean, k.y + k.h)],
        colour: leg_colour,
        highlight: highlight_for(leg),
    });
}

/// The bowl of a p whose stem ends at `x`: two bars and a half ring,
/// cut into slices so each piece is convex.
fn bowl(out: &mut Vec<Piece>, at: &dyn Fn(f32, f32) -> Pos2, x: f32, c: Color32) {
    let (r_out, r_in) = (50.0, 24.0);
    let cx = x + r_out - 14.0;
    let cy = 90.0;
    let bar = |y0: f32, y1: f32| Piece { points: vec![at(x - 1.0, y0), at(cx, y0), at(cx, y1), at(x - 1.0, y1)], colour: c, highlight: highlight_for(c) };
    out.push(bar(40.0, 66.0));
    out.push(bar(114.0, 140.0));
    let n = 18;
    for i in 0..n {
        let a0 = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / n as f32;
        let a1 = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * (i + 1) as f32 / n as f32;
        // A hair of overlap so neighbouring slices leave no seam.
        let (a0, a1) = (a0 - 0.004, a1 + 0.004);
        out.push(Piece {
            points: vec![
                at(cx + r_in * a0.cos(), cy + r_in * a0.sin()),
                at(cx + r_out * a0.cos(), cy + r_out * a0.sin()),
                at(cx + r_out * a1.cos(), cy + r_out * a1.sin()),
                at(cx + r_in * a1.cos(), cy + r_in * a1.sin()),
            ],
            colour: c,
            highlight: highlight_for(c),
        });
    }
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()), m(a.a(), b.a()))
}

/// Paint a piece, then the sheen's light over it in thin vertical slices
/// so the light can fall across the piece rather than only at its corners.
fn paint_piece(painter: &egui::Painter, piece: &Piece, sheen: Option<Sheen>, appear: f32) {
    painter.add(egui::Shape::convex_polygon(piece.points.clone(), piece.colour.gamma_multiply(appear), Stroke::NONE));
    let Some(sheen) = sheen else { return };
    let (min_x, max_x) = piece.points.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p.x), b.max(p.x)));
    if max_x < sheen.band - sheen.width * 4.0 || min_x > sheen.band + sheen.width * 4.0 + 80.0 {
        return;
    }
    let mut mesh = egui::Mesh::default();
    let step = 2.0;
    let mut x = min_x;
    while x < max_x {
        let slice = clip_x(&piece.points, x, (x + step).min(max_x));
        if slice.len() >= 3 {
            let base = mesh.vertices.len() as u32;
            for p in &slice {
                let light = sheen.at(*p) * 0.62 * appear;
                let c = piece.highlight;
                mesh.colored_vertex(*p, Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (light * 255.0) as u8));
            }
            for i in 1..slice.len() as u32 - 1 {
                mesh.add_triangle(base, base + i, base + i + 1);
            }
        }
        x += step;
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// The part of a convex polygon between two verticals.
fn clip_x(points: &[Pos2], x0: f32, x1: f32) -> Vec<Pos2> {
    let keep = |pts: &[Pos2], inside: &dyn Fn(Pos2) -> bool, cross: &dyn Fn(Pos2, Pos2) -> Pos2| {
        let mut out = Vec::with_capacity(pts.len() + 2);
        for i in 0..pts.len() {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            match (inside(a), inside(b)) {
                (true, true) => out.push(b),
                (true, false) => out.push(cross(a, b)),
                (false, true) => {
                    out.push(cross(a, b));
                    out.push(b);
                }
                (false, false) => {}
            }
        }
        out
    };
    let at_x = |x: f32| move |a: Pos2, b: Pos2| {
        let t = (x - a.x) / (b.x - a.x);
        pos2(x, a.y + (b.y - a.y) * t)
    };
    let left = keep(points, &|p| p.x >= x0, &at_x(x0));
    keep(&left, &|p| p.x <= x1, &at_x(x1))
}

/// The × at the hero's top right. Returns whether it was clicked.
fn close_button(ui: &mut egui::Ui, hero: Rect, theme: &KlippTheme) -> bool {
    let r = Rect::from_center_size(pos2(hero.right() - 22.0, hero.top() + 22.0), vec2(26.0, 26.0));
    let resp = ui.interact(r, ui.id().with("about_close"), egui::Sense::click()).on_hover_text("Close");
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(r, 6.0, theme.bg_hover);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let c = if resp.hovered() { theme.text_primary } else { theme.text_secondary };
    let d = 4.5;
    let m = r.center();
    painter.line_segment([pos2(m.x - d, m.y - d), pos2(m.x + d, m.y + d)], Stroke::new(1.5_f32, c));
    painter.line_segment([pos2(m.x - d, m.y + d), pos2(m.x + d, m.y - d)], Stroke::new(1.5_f32, c));
    resp.clicked()
}

/// The width of the facts' label column.
const LABEL_W: f32 = 68.0;

/// The facts and the ways onward.
fn body(ui: &mut egui::Ui, theme: &KlippTheme, status: &Status<'_>) -> Option<AboutAction> {
    let mut action = None;
    let build = klipp_core::build_info::get();
    egui::Frame::none().inner_margin(egui::Margin { left: 32.0, right: 32.0, top: 22.0, bottom: 24.0 }).show(ui, |ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
        let row = |ui: &mut egui::Ui, label: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
            ui.horizontal(|ui| {
                // A narrow column read from the left: the values sit close
                // to their labels. The labels in the secondary colour, not
                // the dim one, which reads below 4.5:1 on the panel.
                ui.allocate_ui_with_layout(vec2(LABEL_W, 22.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_min_width(LABEL_W);
                    ui.add(egui::Label::new(egui::RichText::new(label).size(12.0).color(theme.text_secondary)).selectable(false));
                });
                add(ui);
            });
            ui.add_space(4.0);
        };
        row(ui, "Version", &mut |ui| {
            ui.label(egui::RichText::new(build.version).size(13.0).color(theme.text_primary).strong());
            let commit = format!("{}{}", build.commit, if build.dirty { "-dirty" } else { "" });
            let detail = if build.number > 0 { format!("build {} · {commit} · {}", build.number, build.profile) } else { format!("{commit} · {}", build.profile) };
            ui.label(egui::RichText::new(detail).size(12.0).color(theme.text_secondary));
            if quiet_link(ui, theme, "Copy").on_hover_text("Copy the full version").clicked() {
                ui.ctx().copy_text(build.label());
            }
        });
        if let Some(at) = build.built_at {
            row(ui, "Built", &mut |ui| {
                ui.label(egui::RichText::new(klipp_core::build_info::format_utc(at)).size(13.0).color(theme.text_secondary));
            });
        }
        row(ui, "Licence", &mut |ui| {
            ui.label(egui::RichText::new("GPL-3.0, free and open source").size(13.0).color(theme.text_secondary));
        });
        row(ui, "Source", &mut |ui| {
            if quiet_link(ui, theme, "github.com/davidkodar/klipp").clicked() {
                open(SOURCE);
            }
        });

        ui.add_space(18.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.spacing_mut().button_padding = vec2(14.0, 6.0);
            // The brand's amber, not the theme's accent: this is Klipp's own page.
            let primary = egui::Button::new(egui::RichText::new("Check for updates…").size(13.0).color(TILE).strong())
                .fill(AMBER)
                .stroke(Stroke::NONE)
                .rounding(7.0)
                .min_size(vec2(0.0, 32.0));
            if ui.add(primary).clicked() {
                action = Some(AboutAction::CheckForUpdates);
            }
            let secondary = egui::Button::new(egui::RichText::new("Report an issue").size(13.0).color(theme.text_primary))
                .fill(theme.bg_raised)
                .stroke(Stroke::new(1.0_f32, theme.border))
                .rounding(7.0)
                .min_size(vec2(0.0, 32.0));
            if ui.add(secondary).clicked() {
                open(ISSUES);
            }
        });
        if let Some(a) = status_row(ui, theme, status) {
            action = Some(a);
        }

        ui.add_space(20.0);
        let line = ui.cursor().min.y;
        ui.painter().hline(ui.max_rect().x_range(), line, Stroke::new(1.0_f32, theme.border));
        ui.add_space(14.0);
        ui.label(
            egui::RichText::new("Klipp is Swedish for “cut”. Built with Rust, egui, wgpu and FFmpeg.")
                .size(12.0)
                .color(theme.text_secondary),
        );
    });
    action
}

/// How long "Copied" shows in place of Copy.
const COPIED_FOR: f64 = 1.6;

/// The update check's row: a card under the buttons with a mark, what
/// the check found and what to do next. Nothing before a check.
fn status_row(ui: &mut egui::Ui, theme: &KlippTheme, status: &Status<'_>) -> Option<AboutAction> {
    if *status == Status::Idle {
        return None;
    }
    let mut action = None;
    ui.add_space(14.0);
    egui::Frame::none()
        .fill(theme.bg_deep)
        .stroke(Stroke::new(1.0_f32, theme.border))
        .rounding(8.0)
        .inner_margin(egui::Margin { left: 12.0, right: 14.0, top: 11.0, bottom: 11.0 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = vec2(10.0, 0.0);
            ui.horizontal_top(|ui| {
                mark(ui, theme, status);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
                    action = status_text(ui, theme, status);
                });
            });
        });
    action
}

/// The mark at the row's left: a spinner while checking, a tick, a warm
/// dot for a new version, or a ring with a bang for a failure.
fn mark(ui: &mut egui::Ui, theme: &KlippTheme, status: &Status<'_>) {
    let size = vec2(16.0, 20.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter();
    let c = pos2(rect.center().x, rect.top() + 10.0);
    match status {
        Status::Checking => {
            // A faint ring with an amber arc going round it: the same
            // length at every turn, where egui's spinner shrinks to a dot.
            let r = 6.5;
            painter.circle_stroke(c, r, Stroke::new(1.8_f32, AMBER.gamma_multiply(0.22)));
            let start = ui.input(|i| i.time) as f32 * std::f32::consts::TAU * 0.9;
            let arc = (0..=16).map(|i| start + i as f32 / 16.0 * std::f32::consts::FRAC_PI_2 * 1.2).map(|a| pos2(c.x + r * a.cos(), c.y + r * a.sin())).collect();
            painter.add(egui::Shape::line(arc, Stroke::new(1.8_f32, AMBER)));
            ui.ctx().request_repaint();
        }
        Status::Latest { .. } => {
            painter.circle_filled(c, 8.0, theme.marker_success);
            let ink = theme.bg_deep;
            painter.add(egui::Shape::line(vec![pos2(c.x - 3.6, c.y + 0.2), pos2(c.x - 1.0, c.y + 2.8), pos2(c.x + 3.8, c.y - 2.6)], Stroke::new(1.8_f32, ink)));
        }
        Status::Available { .. } => {
            painter.circle_filled(c, 8.0, AMBER.gamma_multiply(0.22));
            painter.circle_filled(c, 4.0, AMBER);
        }
        Status::Failed(_) => {
            painter.circle_stroke(c, 7.2, Stroke::new(1.5_f32, theme.marker_error));
            painter.line_segment([pos2(c.x, c.y - 3.6), pos2(c.x, c.y + 0.8)], Stroke::new(1.6_f32, theme.marker_error));
            painter.circle_filled(pos2(c.x, c.y + 3.4), 0.95, theme.marker_error);
        }
        Status::Idle => {}
    }
}

/// The row's headline, with a link at its right when there is one.
fn headline(ui: &mut egui::Ui, theme: &KlippTheme, text: &str, link: Option<&str>) -> bool {
    let mut clicked = false;
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.add(egui::Label::new(egui::RichText::new(text).size(13.0).color(theme.text_primary).strong()).selectable(false));
        if let Some(link) = link {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                clicked = quiet_link(ui, theme, link).clicked();
            });
        }
    });
    clicked
}

/// A line of detail under the headline.
fn detail(ui: &mut egui::Ui, theme: &KlippTheme, text: &str) {
    ui.add_space(3.0);
    ui.add(egui::Label::new(egui::RichText::new(text).size(12.0).color(theme.text_secondary)).wrap());
}

/// What the check found, and the one thing to do next.
fn status_text(ui: &mut egui::Ui, theme: &KlippTheme, status: &Status<'_>) -> Option<AboutAction> {
    let version = klipp_core::build_info::get().version;
    match status {
        Status::Idle => None,
        Status::Checking => {
            headline(ui, theme, "Checking for updates…", None);
            None
        }
        Status::Latest { previews, testing } => {
            match testing {
                Some(testing) => headline(ui, theme, &format!("You have the latest version (testing as {testing})"), None),
                None => headline(ui, theme, &format!("You have the latest version ({version})"), None),
            };
            if *previews {
                detail(ui, theme, "Preview releases included");
            }
            None
        }
        Status::Failed(message) => {
            let again = headline(ui, theme, "Could not check for updates", Some("Try again"));
            detail(ui, theme, message);
            again.then_some(AboutAction::CheckForUpdates)
        }
        Status::Available { version: new, notes, install, error, testing } => {
            if headline(ui, theme, &format!("Klipp {new} is available"), Some("What’s new")) {
                open(notes);
            }
            if let Some(testing) = testing {
                detail(ui, theme, &format!("(testing as {testing})"));
            }
            let mut action = None;
            match install {
                Install::Updater => {
                    detail(ui, theme, "Save your work and finish any exports first. Pacman updates the system along with Klipp; reopen Klipp after.");
                    ui.add_space(10.0);
                    let button = egui::Button::new(egui::RichText::new("Install update…").size(12.5).color(TILE).strong())
                        .fill(AMBER)
                        .stroke(Stroke::NONE)
                        .rounding(6.0)
                        .min_size(vec2(0.0, 28.0));
                    if ui.add(button).clicked() {
                        action = Some(AboutAction::Install);
                    }
                }
                Install::Download => {
                    detail(ui, theme, "Download the package from the release page. Preview releases are installed by hand.");
                }
                Install::Steps(steps) => {
                    detail(ui, theme, steps.what);
                    ui.add_space(8.0);
                    commands(ui, theme, &steps.line());
                }
            }
            if let Some(error) = error {
                ui.add_space(6.0);
                ui.add(egui::Label::new(egui::RichText::new(*error).size(12.0).color(theme.marker_error)).wrap());
            }
            action
        }
    }
}

/// Commands to paste, in a well with Copy at its right.
fn commands(ui: &mut egui::Ui, theme: &KlippTheme, line: &str) {
    let id = ui.id().with("about_copied");
    let now = ui.input(|i| i.time);
    let copied = ui.data(|d| d.get_temp::<f64>(id)).is_some_and(|at| now - at < COPIED_FOR);
    egui::Frame::none()
        .fill(theme.bg_panel)
        .stroke(Stroke::new(1.0_f32, theme.border))
        .rounding(6.0)
        .inner_margin(egui::Margin::symmetric(10.0, 7.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let copy_w = 44.0;
                ui.allocate_ui_with_layout(vec2(ui.available_width() - copy_w, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(line).monospace().size(11.5).color(theme.text_primary)).wrap());
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if copied {
                        ui.add(egui::Label::new(egui::RichText::new("Copied").size(12.5).color(theme.text_secondary)).selectable(false));
                        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(COPIED_FOR));
                    } else if quiet_link(ui, theme, "Copy").on_hover_text("Copy the commands").clicked() {
                        ui.ctx().copy_text(line.to_owned());
                        ui.data_mut(|d| d.insert_temp(id, now));
                    }
                });
            });
        });
}

/// A frameless text button that reads as a link.
fn quiet_link(ui: &mut egui::Ui, _theme: &KlippTheme, text: &str) -> egui::Response {
    // Amber on dark; on light, a deeper amber that reads as text.
    let colour = if ui.visuals().dark_mode { AMBER } else { Color32::from_rgb(0x9A, 0x62, 0x14) };
    let resp = ui.add(egui::Label::new(egui::RichText::new(text).size(12.5).color(colour)).sense(egui::Sense::click()).selectable(false));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        let r = resp.rect;
        ui.painter().hline(r.x_range(), r.bottom() - 1.0, Stroke::new(1.0_f32, colour));
    }
    resp
}

fn open(url: &str) {
    let _ = crate::ui_util::open_external(url);
}

/// Drive a dialog in a test: the text it drew, and clicks on it.
#[cfg(test)]
pub(crate) mod probe {
    /// Each text drawn, with where it was drawn.
    pub(crate) fn texts(out: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
        fn walk(shape: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::Shape::Text(t) => out.push((t.galley.text().to_owned(), t.galley.rect.translate(t.pos.to_vec2()))),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        out.shapes.iter().for_each(|s| walk(&s.shape, &mut found));
        found
    }

    /// Frames of `draw` until settled, then a click on the last text drawn
    /// that starts with `label`. The click's frame.
    pub(crate) fn click(ctx: &egui::Context, label: &str, mut draw: impl FnMut(&egui::Context)) -> egui::FullOutput {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0));
        let frame = |events: Vec<egui::Event>, draw: &mut dyn FnMut(&egui::Context)| {
            let input = egui::RawInput { screen_rect: Some(screen), events, ..Default::default() };
            ctx.run(input, |ctx| draw(ctx))
        };
        let mut seen = Vec::new();
        for _ in 0..3 {
            seen = texts(&frame(Vec::new(), &mut draw));
        }
        let at = seen.iter().rev().find(|(t, _)| t.starts_with(label)).unwrap_or_else(|| panic!("{label} not drawn: {seen:?}")).1.center();
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        frame(vec![egui::Event::PointerMoved(at)], &mut draw);
        frame(vec![button(true)], &mut draw);
        frame(vec![button(false)], &mut draw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The K starts whole, is fully cut once the slip is over, and the
    /// sheen runs only in its windows.
    #[test]
    fn the_animation_settles() {
        assert_eq!(cut(0.0), 0.0);
        assert_eq!(cut(CUT_AT + CUT_FOR + 0.01), 1.0);
        assert!(sheen_phase(0.1).is_none());
        assert!(sheen_phase(SHEEN_AT + SHEEN_FOR / 2.0).is_some());
        assert!(sheen_phase(SHEEN_AT + SHEEN_FOR + 1.0).is_none());
        assert!(sheen_phase(SHEEN_AT + SHEEN_EVERY + 0.1).is_some());
    }

    /// Slicing keeps a convex shape's area: the slices add up to it.
    #[test]
    fn slices_cover_the_shape() {
        let quad = vec![pos2(0.0, 0.0), pos2(10.0, 0.0), pos2(14.0, 8.0), pos2(4.0, 8.0)];
        let area = |p: &[Pos2]| {
            (0..p.len()).map(|i| { let (a, b) = (p[i], p[(i + 1) % p.len()]); a.x * b.y - b.x * a.y }).sum::<f32>().abs() / 2.0
        };
        let total: f32 = (0..14).map(|i| area(&clip_x(&quad, i as f32, i as f32 + 1.0))).sum();
        assert!((total - area(&quad)).abs() < 1e-3, "{total} vs {}", area(&quad));
    }

    /// The dialog draws, in both themes, and asks for nothing by itself.
    #[test]
    fn it_draws() {
        for dark in [true, false] {
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            let theme = KlippTheme::default();
            for frame in 0..3 {
                let input = egui::RawInput { time: Some(frame as f64 * 0.4), ..Default::default() };
                let _ = ctx.run(input, |ctx| {
                    assert_eq!(show(ctx, &theme, &Status::Idle), AboutAction::None);
                });
            }
        }
    }

    /// The facts' labels and the footer read at 4.5:1 or better on the
    /// panel, in the dark theme and the light one: in the dim colour they
    /// read at 3.4:1 on the dark panel.
    #[test]
    fn the_small_text_reads_on_the_panel() {
        fn lum(c: Color32) -> f64 {
            let f = |v: u8| { let x = v as f64 / 255.0; if x <= 0.03928 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) } };
            0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
        }
        let ratio = |a: Color32, b: Color32| { let (x, y) = (lum(a), lum(b)); (x.max(y) + 0.05) / (x.min(y) + 0.05) };
        let themes = [
            (true, KlippTheme::default()),
            (false, KlippTheme::from(crate::theme::parse_builtin(crate::theme::BUILTIN_LIGHT).unwrap())),
        ];
        for (dark, theme) in themes {
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            let mut texts = Vec::new();
            for frame in 0..3 {
                let input = egui::RawInput { time: Some(frame as f64 * 0.4), ..Default::default() };
                let out = ctx.run(input, |ctx| { let _ = show(ctx, &theme, &Status::Idle); });
                texts = out.shapes.iter().filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.galley.job.sections.first().map(|s| s.format.color).unwrap_or(t.fallback_color))),
                    _ => None,
                }).collect::<Vec<_>>();
            }
            for want in ["Version", "Built", "Licence", "Source", "Klipp is Swedish"] {
                let Some((_, colour)) = texts.iter().find(|(t, _)| t.starts_with(want)) else {
                    assert!(want == "Built", "{want} not drawn: {texts:?}");
                    continue;
                };
                let r = ratio(*colour, theme.bg_panel);
                assert!(r >= 4.5, "dark {dark}: {want} at {r:.2}:1");
            }
        }
    }

    fn drawn(status: &Status<'_>) -> String {
        let ctx = egui::Context::default();
        let theme = KlippTheme::default();
        let mut text = Vec::new();
        for frame in 0..3 {
            let input = egui::RawInput { time: Some(frame as f64 * 0.4), ..Default::default() };
            text = probe::texts(&ctx.run(input, |ctx| { let _ = show(ctx, &theme, status); }));
        }
        text.into_iter().map(|(t, _)| t).collect::<Vec<_>>().join("\n")
    }

    fn available(install: Install) -> Status<'static> {
        Status::Available { version: "0.2.0".into(), notes: "https://example.invalid/notes".into(), install, error: None, testing: None }
    }

    /// Each place the check can be in draws as a row in About: nothing
    /// before a check, then checking, up to date, a newer version with the
    /// action for each kind of installation, and a failure.
    #[test]
    fn each_update_status_draws_in_about() {
        let version = klipp_core::build_info::get().version;
        assert!(!drawn(&Status::Idle).contains("Checking"), "no row before a check");
        assert!(drawn(&Status::Checking).contains("Checking for updates…"));
        let latest = drawn(&Status::Latest { previews: false, testing: None });
        assert!(latest.contains(&format!("You have the latest version ({version})")), "{latest}");
        assert!(!latest.contains("Preview"));
        assert!(drawn(&Status::Latest { previews: true, testing: None }).contains("Preview releases included"), "names the channel");

        let updater = drawn(&available(Install::Updater));
        for want in ["Klipp 0.2.0 is available", "What’s new", "Install update…"] {
            assert!(updater.contains(want), "{want} in {updater}");
        }
        assert!(!drawn(&available(Install::Download)).contains("Install update…"), "no updater without the repository");
        assert!(drawn(&available(Install::Download)).contains("release page"));
        for steps in [crate::updates::ARCH_SOURCE_STEPS, crate::updates::SOURCE_STEPS] {
            let text = drawn(&available(Install::Steps(steps)));
            assert!(text.contains(&steps.line()) && text.contains("Copy"), "{text}");
            assert!(!text.contains("Install update…"), "{text}");
        }
        assert!(drawn(&available(Install::Steps(crate::updates::ARCH_SOURCE_STEPS))).contains("makepkg -si"));

        let failed = drawn(&Status::Failed("The release service is rate limited."));
        assert!(failed.contains("Could not check for updates") && failed.contains("Try again"), "{failed}");
        assert!(failed.contains("The release service is rate limited."));
        let error = Status::Available { version: "0.2.0".into(), notes: String::new(), install: Install::Updater, error: Some("Could not open the updater."), testing: None };
        let tested = Status::Available { version: "0.2.0".into(), notes: String::new(), install: Install::Download, error: None, testing: Some("0.1.0-rc.1") };
        assert!(drawn(&tested).contains("(testing as 0.1.0-rc.1)"), "names the version tested as");
        assert!(drawn(&Status::Latest { previews: false, testing: Some("0.1.0-rc.1") }).contains("You have the latest version (testing as 0.1.0-rc.1)"));
        assert!(drawn(&tested).contains(klipp_core::build_info::get().version), "the Version row keeps the real one");
        assert!(drawn(&error).contains("Could not open the updater."));
    }

    /// The buttons ask for what they say: a check, again after a failure,
    /// and the updater.
    #[test]
    fn the_update_buttons_ask_for_their_action() {
        let theme = KlippTheme::default();
        for (status, label, want) in [
            (Status::Idle, "Check for updates…", AboutAction::CheckForUpdates),
            (Status::Failed("Offline."), "Try again", AboutAction::CheckForUpdates),
            (available(Install::Updater), "Install update…", AboutAction::Install),
        ] {
            let ctx = egui::Context::default();
            let mut asked = Vec::new();
            probe::click(&ctx, label, |ctx| asked.push(show(ctx, &theme, &status)));
            assert!(asked.contains(&want), "{label}: {asked:?}");
        }
    }

    /// Copy puts the commands on the clipboard and says so.
    #[test]
    fn copy_copies_the_steps() {
        let theme = KlippTheme::default();
        let steps = crate::updates::SOURCE_STEPS;
        let status = available(Install::Steps(steps));
        let ctx = egui::Context::default();
        let out = probe::click(&ctx, "Copy", |ctx| { let _ = show(ctx, &theme, &status); });
        assert_eq!(out.platform_output.copied_text, steps.line());
        let out = ctx.run(egui::RawInput::default(), |ctx| { let _ = show(ctx, &theme, &status); });
        assert!(probe::texts(&out).iter().any(|(t, _)| t == "Copied"), "says so");
    }
}
