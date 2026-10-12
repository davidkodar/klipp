//! The About Klipp dialog. A hero with the identity on top, a few lines of
//! facts below, then the ways onward (updates, source, issues).
//!
//! The mark and the wordmark are drawn as shapes, from the same geometry
//! as `assets/branding/klipp-refined/build.py`, so they can move: when the
//! dialog opens the K is whole, then its leg is cut and slips into place,
//! and a warm sheen passes over the identity now and then.

use egui::{pos2, vec2, Color32, Pos2, Rect, Stroke};

use crate::theme::KlippTheme;

/// What the person asked for in the dialog this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AboutAction {
    None,
    Close,
    CheckForUpdates,
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

/// Draw the dialog. Call it each frame while it is open.
pub fn show(ctx: &egui::Context, theme: &KlippTheme) -> AboutAction {
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
            if let Some(a) = body(ui, theme) {
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
    let c = if resp.hovered() { theme.text_primary } else { theme.text_dim };
    let d = 4.5;
    let m = r.center();
    painter.line_segment([pos2(m.x - d, m.y - d), pos2(m.x + d, m.y + d)], Stroke::new(1.5_f32, c));
    painter.line_segment([pos2(m.x - d, m.y + d), pos2(m.x + d, m.y - d)], Stroke::new(1.5_f32, c));
    resp.clicked()
}

/// The facts and the ways onward.
fn body(ui: &mut egui::Ui, theme: &KlippTheme) -> Option<AboutAction> {
    let mut action = None;
    let build = klipp_core::build_info::get();
    egui::Frame::none().inner_margin(egui::Margin { left: 32.0, right: 32.0, top: 22.0, bottom: 24.0 }).show(ui, |ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
        let row = |ui: &mut egui::Ui, label: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
            ui.horizontal(|ui| {
                ui.add_sized([92.0, 22.0], egui::Label::new(egui::RichText::new(label).size(12.0).color(theme.text_dim)).selectable(false));
                add(ui);
            });
            ui.add_space(4.0);
        };
        row(ui, "Version", &mut |ui| {
            ui.label(egui::RichText::new(build.version).size(13.0).color(theme.text_primary).strong());
            let commit = format!("{}{}", build.commit, if build.dirty { "-dirty" } else { "" });
            ui.label(egui::RichText::new(format!("{commit} · {}", build.profile)).size(12.0).color(theme.text_secondary));
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

        ui.add_space(20.0);
        let line = ui.cursor().min.y;
        ui.painter().hline(ui.max_rect().x_range(), line, Stroke::new(1.0_f32, theme.border));
        ui.add_space(14.0);
        ui.label(
            egui::RichText::new("Klipp is Swedish for “cut”. Built with Rust, egui, wgpu and FFmpeg.")
                .size(11.5)
                .color(theme.text_dim),
        );
    });
    action
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
    let _ = std::process::Command::new("xdg-open").arg(url).stdin(std::process::Stdio::null()).spawn();
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
                    assert_eq!(show(ctx, &theme), AboutAction::None);
                });
            }
        }
    }
}
