//! The Title tab's first tier: the motion card (In, Out and the loops)
//! and the style card (presets, fill, stroke, shadow, glow). Every
//! choice is a chip rendered by the real title engine on the clip's
//! own text, so what is picked is what will play.

use crate::browser::title_previews::TitlePreviews;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use klipp_timeline::{
    CrawlDirection, Easing, Motion, MotionKind, ScrollDirection, StaggerUnit, TextAlignment, TextBox,
    TitleData, TitleLoop, TitleMotion,
};
use std::hash::{Hash, Hasher};

/// Chip render size: twice the display size so it is sharp on a 2×
/// display.
const CHIP_W: u32 = 224;
const CHIP_H: u32 = 126;
/// A gallery chip on screen.
const GALLERY_CHIP: egui::Vec2 = egui::vec2(104.0, 58.5);
/// The chip in an In / Out row.
const ROW_CHIP: egui::Vec2 = egui::vec2(72.0, 40.5);
/// Hover playback: the frame step and the loop length.
const FRAME_STEP: f32 = 1.0 / 20.0;
const HOVER_LOOP: f32 = 1.6;
const LOOPS_HOVER_LOOP: f32 = 3.0;
/// The moment an entrance chip shows at rest: mid-entrance, so the
/// kind is visible in a still.
const REST_IN: f32 = 0.3;
/// The preview clip length the worker renders with.
const WINDOW: f32 = crate::browser::title_previews::PREVIEW_WINDOW;
/// The font size, at 1080p, chips render their text in.
const CHIP_FONT: f32 = 220.0;

/// The inspector's chip cache: the same worker the Library uses,
/// plus a deferred-drop ring so a displaced texture outlives the
/// frame that referenced it.
#[derive(Default)]
pub struct TitleChips {
    previews: TitlePreviews,
    drops: [Vec<egui::TextureHandle>; 3],
    slot: usize,
}

impl TitleChips {
    /// Once per frame, before any chip is drawn: release the handles
    /// stashed two frames ago and upload finished renders.
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        let next = (self.slot + 1) % 3;
        self.drops[next].clear();
        self.slot = next;
        let mut displaced = Vec::new();
        self.previews.poll(ctx, &mut displaced);
        self.drops[self.slot].extend(displaced);
    }

    /// Every cached handle, for shutdown.
    pub fn drain(&mut self) -> Vec<egui::TextureHandle> {
        let mut out = self.previews.drain();
        for b in &mut self.drops {
            out.append(b);
        }
        out
    }
}

// ─── Chips ───────────────────────────────────────────────────────────

/// The clip's text as a chip would show it: the first line, short,
/// large and centred, in the layer's own face and style.
fn chip_title(tb: &TextBox, adjust: impl FnOnce(&mut TextBox)) -> TitleData {
    let mut t = tb.clone();
    let line = t.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let short: String = line.chars().take(14).collect();
    t.text = if short.is_empty() { "Title".to_string() } else { short };
    t.font_size = CHIP_FONT;
    t.alignment = TextAlignment::Center;
    t.style_runs.clear();
    t.write_on = 1.0;
    t.outline_write_on = 1.0;
    t.motion = TitleMotion::default();
    t.loops.clear();
    t.text_path = None;
    t.columns = 1;
    t.vertical_layout = false;
    t.drop_cap_scale = 0.0;
    t.baseline_shift = 0.0;
    adjust(&mut t);
    TitleData::single_text(t)
}

/// A signature of everything about the layer that changes how a chip
/// looks, so the cache renews when the style does.
fn style_signature(tb: &TextBox) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    tb.font_name.hash(&mut h);
    format!("{:?}", tb.font_style).hash(&mut h);
    tb.color.hash(&mut h);
    format!("{:?}{:?}{:?}{:?}{:?}", tb.outlines, tb.shadows, tb.glows, tb.gradient, tb.inner_shadows).hash(&mut h);
    format!("{:?}{:?}{:?}", tb.extrusion, tb.pattern_fill, tb.texture_fill).hash(&mut h);
    format!("{:?}", tb.case_transform).hash(&mut h);
    tb.tracking.to_bits().hash(&mut h);
    h.finish()
}

fn chip_text(tb: &TextBox) -> String {
    tb.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("Title").trim().chars().take(14).collect()
}

/// Hover elapsed time for a chip, kept in egui's temp data.
fn hover_elapsed(ui: &egui::Ui, id: egui::Id, hovered: bool) -> Option<f32> {
    let start_id = id.with("hover_start");
    let now = ui.ctx().input(|i| i.time);
    if !hovered {
        ui.memory_mut(|m| m.data.remove::<f64>(start_id));
        return None;
    }
    let start: f64 = ui.memory_mut(|m| *m.data.get_temp_mut_or_insert_with(start_id, || now));
    Some((now - start).max(0.0) as f32)
}

/// The moment a hovered chip shows `elapsed` seconds into its hover,
/// looping over `[from, from + len)` in frame steps.
fn hover_time(elapsed: f32, from: f32, len: f32) -> f32 {
    let t = elapsed % len;
    from + (t / FRAME_STEP).floor() * FRAME_STEP
}

struct ChipSpec<'a> {
    /// What the chip is, for the cache and the hover clock.
    id: String,
    tb: &'a TextBox,
    /// The moment shown at rest, and the hover loop's start and length.
    rest: f32,
    hover_from: f32,
    hover_len: f32,
    selected: bool,
    label: Option<&'a str>,
}

/// Draw one chip: the rendered still at rest, the motion playing while
/// hovered. Returns the chip's response.
fn chip(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    chips: &mut TitleChips,
    spec: ChipSpec<'_>,
    size: egui::Vec2,
    make: impl Fn() -> TitleData + Clone,
) -> egui::Response {
    let label_h = if spec.label.is_some() { 16.0 } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size.x, size.y + label_h), egui::Sense::click());
    let pic = egui::Rect::from_min_size(rect.min, size);
    let template = format!("chip:{}:{:x}", spec.id, style_signature(spec.tb));
    let text = chip_text(spec.tb);
    let hovered = resp.hovered();
    let elapsed = hover_elapsed(ui, resp.id, hovered);
    let time = match elapsed {
        Some(e) => hover_time(e, spec.hover_from, spec.hover_len),
        None => spec.rest,
    };
    if elapsed.is_some() {
        // The next few steps, so hover play never waits on a render.
        for k in 1..=6 {
            let t = hover_time(elapsed.unwrap_or(0.0) + k as f32 * FRAME_STEP, spec.hover_from, spec.hover_len);
            let key = TitlePreviews::key(&template, &text, t);
            let mk = make.clone();
            chips.previews.request_sized(&key, t, CHIP_W, CHIP_H, mk);
        }
        ui.ctx().request_repaint();
    }
    let key = TitlePreviews::key(&template, &text, time);
    let tex = chips
        .previews
        .get_or_request_sized(&key, time, CHIP_W, CHIP_H, make.clone())
        .cloned()
        .or_else(|| chips.previews.nearest_for(&template, &text, time).cloned());

    let painter = ui.painter();
    let rounding = crate::theme::radius::PANEL;
    painter.rect_filled(pic, rounding, theme.bg_panel_alt);
    if let Some(t) = tex {
        painter.image(t.id(), pic, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
    }
    let stroke = if spec.selected {
        egui::Stroke::new(2.0_f32, theme.accent)
    } else if hovered {
        egui::Stroke::new(1.0_f32, theme.text_secondary)
    } else {
        egui::Stroke::new(1.0_f32, theme.border)
    };
    painter.rect_stroke(pic, rounding, stroke);
    if let Some(l) = spec.label {
        let color = if spec.selected { theme.text_primary } else { theme.text_secondary };
        painter.text(
            egui::pos2(pic.center().x, pic.max.y + 3.0),
            egui::Align2::CENTER_TOP,
            l,
            egui::FontId::proportional(crate::theme::type_scale::CAPTION),
            color,
        );
    }
    resp
}

/// A caption above a card section, in the tab's own voice.
fn caption(ui: &mut egui::Ui, theme: &KlippTheme, text: &str) {
    ui.label(egui::RichText::new(text).size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
}

// ─── Motion card ─────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Enter,
    Exit,
}

impl Direction {
    fn label(self) -> &'static str {
        match self {
            Direction::Enter => "In",
            Direction::Exit => "Out",
        }
    }
    fn default_kind(self) -> MotionKind {
        match self {
            Direction::Enter => MotionKind::Rise,
            Direction::Exit => MotionKind::Fade,
        }
    }
    /// The rest moment and hover loop for a chip of this direction.
    fn timing(self) -> (f32, f32, f32) {
        match self {
            Direction::Enter => (REST_IN, 0.0, HOVER_LOOP),
            Direction::Exit => (WINDOW - REST_IN, WINDOW - HOVER_LOOP, HOVER_LOOP),
        }
    }
}

/// The chip title for one motion in one direction.
fn motion_chip_title(tb: &TextBox, dir: Direction, m: Option<Motion>) -> TitleData {
    chip_title(tb, |t| {
        t.motion = match dir {
            Direction::Enter => TitleMotion { enter: m, exit: None },
            Direction::Exit => TitleMotion { enter: None, exit: m },
        };
    })
}

/// One row of the motion card: the chip, the kind, the duration, and
/// the details behind a disclosure. Returns true when `slot` changed.
#[allow(clippy::too_many_arguments)]
fn motion_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    chips: &mut TitleChips,
    tb: &TextBox,
    dir: Direction,
    slot: &mut Option<Motion>,
) -> bool {
    let mut changed = false;
    let (rest, hover_from, hover_len) = dir.timing();
    let popup_id = ui.id().with(("title_motion_gallery", dir.label()));
    let mut open_gallery = false;
    ui.horizontal(|ui| {
        ui.add_sized(
            [28.0, ROW_CHIP.y],
            egui::Label::new(egui::RichText::new(dir.label()).size(crate::theme::type_scale::CONTROL).color(theme.text_secondary)),
        );
        let current = *slot;
        let tb_c = tb.clone();
        let make = move || motion_chip_title(&tb_c, dir, current);
        let kind_id = current.map(|m| format!("{:?}:{:.2}:{:?}", m.kind, m.duration_secs, m.easing)).unwrap_or_else(|| "none".into());
        let resp = chip(
            ui,
            theme,
            chips,
            ChipSpec { id: format!("row:{}:{}", dir.label(), kind_id), tb, rest, hover_from, hover_len, selected: false, label: None },
            ROW_CHIP,
            make,
        );
        if resp.clicked() {
            open_gallery = true;
        }
        let name = current.map(|m| m.kind.label()).unwrap_or("None");
        let btn = ui.add(egui::Button::new(egui::RichText::new(format!("{name}  \u{25be}")).size(crate::theme::type_scale::CONTROL)).min_size(egui::vec2(120.0, crate::theme::control::COMPACT)));
        if btn.clicked() {
            open_gallery = true;
        }
        if let Some(m) = slot.as_mut() {
            if ui.add(egui::DragValue::new(&mut m.duration_secs).range(0.05..=5.0).speed(0.02).suffix(" s").fixed_decimals(2)).on_hover_text("Duration").changed() {
                changed = true;
            }
        }
    });
    if open_gallery {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
    }
    // The gallery: None and every kind, rendered on this text.
    let anchor = ui.min_rect();
    let anchor_resp = ui.interact(egui::Rect::from_min_size(anchor.left_bottom(), egui::vec2(1.0, 1.0)), popup_id.with("anchor"), egui::Sense::hover());
    let mut pick: Option<Option<MotionKind>> = None;
    egui::popup_below_widget(ui, popup_id, &anchor_resp, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
        ui.set_min_width(3.0 * GALLERY_CHIP.x + 2.0 * 8.0 + 16.0);
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
        let mut items: Vec<Option<MotionKind>> = vec![None];
        items.extend(MotionKind::ALL.iter().map(|k| Some(*k)));
        let selected_kind = slot.map(|m| m.kind);
        for row in items.chunks(3) {
            ui.horizontal(|ui| {
                for k in row {
                    let m = k.map(|k| {
                        let mut m = slot.unwrap_or_else(|| Motion::new(k));
                        m.kind = k;
                        m
                    });
                    let tb_c = tb.clone();
                    let make = move || motion_chip_title(&tb_c, dir, m);
                    let label = k.map(|k| k.label()).unwrap_or("None");
                    let id = format!("gallery:{}:{}", dir.label(), label);
                    let resp = chip(
                        ui,
                        theme,
                        chips,
                        ChipSpec { id, tb, rest, hover_from, hover_len, selected: *k == selected_kind, label: Some(label) },
                        GALLERY_CHIP,
                        make,
                    );
                    if resp.clicked() {
                        pick = Some(*k);
                    }
                }
            });
        }
    });
    if let Some(k) = pick {
        *slot = k.map(|k| {
            let mut m = slot.unwrap_or_else(|| Motion::new(dir.default_kind()));
            m.kind = k;
            m
        });
        changed = true;
        ui.memory_mut(|m| m.close_popup());
    }
    // Details: delay, easing, stagger, travel.
    if let Some(m) = slot.as_mut() {
        let details_id = ui.id().with(("title_motion_details", dir.label()));
        let mut open: bool = ui.memory(|mem| mem.data.get_temp(details_id)).unwrap_or(false);
        ui.horizontal(|ui| {
            ui.add_space(28.0 + 8.0);
            let arrow = if open { "\u{25be}" } else { "\u{25b8}" };
            if ui.add(egui::Button::new(egui::RichText::new(format!("{arrow} Details")).size(crate::theme::type_scale::CAPTION).color(theme.text_secondary)).frame(false)).clicked() {
                open = !open;
                ui.memory_mut(|mem| mem.data.insert_temp(details_id, open));
            }
        });
        if open {
            ui.horizontal(|ui| {
                ui.add_space(36.0);
                if ui.add(egui::DragValue::new(&mut m.delay_secs).range(0.0..=10.0).speed(0.02).suffix(" s").prefix("Delay ").fixed_decimals(2)).changed() {
                    changed = true;
                }
                egui::ComboBox::from_id_salt(("title_motion_easing", dir.label()))
                    .selected_text(m.easing.label())
                    .width(0.0)
                    .wrap_mode(egui::TextWrapMode::Extend)
                    .show_ui(ui, |ui| {
                        for e in Easing::ALL {
                            if ui.selectable_value(&mut m.easing, e, e.label()).changed() {
                                changed = true;
                            }
                        }
                    });
            });
            ui.horizontal(|ui| {
                ui.add_space(36.0);
                egui::ComboBox::from_id_salt(("title_motion_stagger", dir.label()))
                    .selected_text(m.stagger.label())
                    .width(0.0)
                    .wrap_mode(egui::TextWrapMode::Extend)
                    .show_ui(ui, |ui| {
                        for u in StaggerUnit::ALL {
                            if ui.selectable_value(&mut m.stagger, u, u.label()).changed() {
                                changed = true;
                            }
                        }
                    });
                if m.stagger != StaggerUnit::None
                    && ui.add(egui::DragValue::new(&mut m.stagger_secs).range(0.0..=1.0).speed(0.005).suffix(" s").prefix("every ").fixed_decimals(3)).changed()
                {
                    changed = true;
                }
                let moves = matches!(m.kind, MotionKind::Rise | MotionKind::Drop | MotionKind::SlideLeft | MotionKind::SlideRight);
                if moves && ui.add(egui::DragValue::new(&mut m.distance).range(0.1..=8.0).speed(0.02).suffix("\u{00d7} font").prefix("Travel ").fixed_decimals(2)).changed() {
                    changed = true;
                }
            });
        }
    }
    changed
}

/// The motion card: In, Out and the loops of the text layer.
pub fn show_motion_card(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    tb: &TextBox,
    chips: &mut TitleChips,
    cmds: &mut Vec<AppCommand>,
) {
    caption(ui, theme, "MOTION");
    ui.add_space(4.0);
    let mut motion = tb.motion;
    let mut changed = false;
    changed |= motion_row(ui, theme, chips, tb, Direction::Enter, &mut motion.enter);
    ui.add_space(4.0);
    changed |= motion_row(ui, theme, chips, tb, Direction::Exit, &mut motion.exit);
    if changed {
        cmds.push(AppCommand::SetTitleMotion { clip_id, motion });
    }
    ui.add_space(8.0);
    show_loops(ui, theme, clip_id, tb, chips, cmds);
}

fn loop_chip_title(tb: &TextBox, l: &TitleLoop) -> TitleData {
    let l = l.clone();
    chip_title(tb, move |t| t.loops = vec![l])
}

/// The loops: a gallery to add from, then one row per loop with its
/// own timing.
fn show_loops(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    tb: &TextBox,
    chips: &mut TitleChips,
    cmds: &mut Vec<AppCommand>,
) {
    let mut loops = tb.loops.clone();
    let mut changed = false;
    let mut remove: Option<usize> = None;
    let popup_id = ui.id().with("title_loop_gallery");
    let mut add_btn: Option<egui::Response> = None;
    ui.horizontal(|ui| {
        caption(ui, theme, "LOOPS");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let b = ui.add(egui::Button::new(egui::RichText::new("+ Add").size(crate::theme::type_scale::CAPTION)).min_size(egui::vec2(0.0, crate::theme::control::COMPACT)));
            if b.clicked() {
                ui.memory_mut(|m| m.toggle_popup(popup_id));
            }
            add_btn = Some(b);
        });
    });
    if let Some(b) = add_btn {
        let mut pick: Option<TitleLoop> = None;
        egui::popup_below_widget(ui, popup_id, &b, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            ui.set_min_width(3.0 * GALLERY_CHIP.x + 2.0 * 8.0 + 16.0);
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            let items = TitleLoop::catalogue();
            for row in items.chunks(3) {
                ui.horizontal(|ui| {
                    for l in row {
                        let tb_c = tb.clone();
                        let l_c = l.clone();
                        let make = move || loop_chip_title(&tb_c, &l_c);
                        let resp = chip(
                            ui,
                            theme,
                            chips,
                            ChipSpec { id: format!("loop:{}", l.label()), tb, rest: 0.35, hover_from: 0.0, hover_len: LOOPS_HOVER_LOOP, selected: false, label: Some(l.label()) },
                            GALLERY_CHIP,
                            make,
                        );
                        if resp.clicked() {
                            pick = Some(l.clone());
                        }
                    }
                });
            }
        });
        if let Some(l) = pick {
            loops.push(l);
            changed = true;
            ui.memory_mut(|m| m.close_popup());
        }
    }
    if loops.is_empty() {
        ui.label(egui::RichText::new("None. A loop runs the whole time the title is on screen.").size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
    }
    for (i, l) in loops.iter_mut().enumerate() {
        ui.horizontal_wrapped(|ui| {
            ui.add_sized(
                [72.0, crate::theme::control::COMPACT],
                egui::Label::new(egui::RichText::new(l.label()).size(crate::theme::type_scale::CONTROL)),
            );
            match l {
                TitleLoop::Blink { rate_hz, duty } => {
                    changed |= ui.add(egui::DragValue::new(rate_hz).range(0.1..=20.0).speed(0.05).suffix(" Hz").fixed_decimals(2)).changed();
                    changed |= ui.add(egui::DragValue::new(duty).range(0.05..=0.95).speed(0.01).prefix("on ").fixed_decimals(2)).changed();
                }
                TitleLoop::Pulse { rate_hz, min_opacity } => {
                    changed |= ui.add(egui::DragValue::new(rate_hz).range(0.05..=10.0).speed(0.02).suffix(" Hz").fixed_decimals(2)).changed();
                    changed |= ui.add(egui::DragValue::new(min_opacity).range(0.0..=1.0).speed(0.01).prefix("min ").fixed_decimals(2)).changed();
                }
                TitleLoop::Shake { amplitude, rate_hz } | TitleLoop::Wave { amplitude, rate_hz } => {
                    changed |= ui.add(egui::DragValue::new(amplitude).range(0.0..=2.0).speed(0.005).suffix("\u{00d7} font").fixed_decimals(3)).changed();
                    changed |= ui.add(egui::DragValue::new(rate_hz).range(0.05..=30.0).speed(0.05).suffix(" Hz").fixed_decimals(2)).changed();
                }
                TitleLoop::ColorCycle { rate_hz } => {
                    changed |= ui.add(egui::DragValue::new(rate_hz).range(0.01..=5.0).speed(0.01).suffix(" Hz").fixed_decimals(2)).changed();
                }
                TitleLoop::ColorBlend { color_a, color_b, rate_hz } => {
                    let mut a = egui::Color32::from_rgba_unmultiplied(color_a[0], color_a[1], color_a[2], color_a[3]);
                    if ui.color_edit_button_srgba(&mut a).changed() {
                        *color_a = a.to_array();
                        changed = true;
                    }
                    let mut b = egui::Color32::from_rgba_unmultiplied(color_b[0], color_b[1], color_b[2], color_b[3]);
                    if ui.color_edit_button_srgba(&mut b).changed() {
                        *color_b = b.to_array();
                        changed = true;
                    }
                    changed |= ui.add(egui::DragValue::new(rate_hz).range(0.01..=5.0).speed(0.01).suffix(" Hz").fixed_decimals(2)).changed();
                }
                TitleLoop::Scroll { direction, speed_px_per_sec } => {
                    let up = *direction == ScrollDirection::Up;
                    if let Some(i) = crate::widgets::segmented::segmented(ui, &["Up", "Down"], if up { 0 } else { 1 }, theme, egui::FontId::proportional(crate::theme::type_scale::CAPTION), crate::theme::control::COMPACT) {
                        *direction = if i == 0 { ScrollDirection::Up } else { ScrollDirection::Down };
                        changed = true;
                    }
                    changed |= ui.add(egui::DragValue::new(speed_px_per_sec).range(0.0..=2000.0).speed(1.0).suffix(" px/s").fixed_decimals(0)).on_hover_text("0 fits the travel to the clip").changed();
                }
                TitleLoop::Crawl { direction, speed_px_per_sec, repeat } => {
                    let left = *direction == CrawlDirection::Left;
                    if let Some(i) = crate::widgets::segmented::segmented(ui, &["\u{2190}", "\u{2192}"], if left { 0 } else { 1 }, theme, egui::FontId::proportional(crate::theme::type_scale::CAPTION), crate::theme::control::COMPACT) {
                        *direction = if i == 0 { CrawlDirection::Left } else { CrawlDirection::Right };
                        changed = true;
                    }
                    changed |= ui.add(egui::DragValue::new(speed_px_per_sec).range(0.0..=2000.0).speed(1.0).suffix(" px/s").fixed_decimals(0)).on_hover_text("0 fits the travel to the clip").changed();
                    changed |= ui.checkbox(repeat, "Repeat").changed();
                }
                TitleLoop::Counter { from, to, duration_secs, decimals } => {
                    changed |= ui.add(egui::DragValue::new(from).speed(1.0).prefix("from ")).changed();
                    changed |= ui.add(egui::DragValue::new(to).speed(1.0).prefix("to ")).changed();
                    changed |= ui.add(egui::DragValue::new(duration_secs).range(0.1..=60.0).speed(0.05).suffix(" s").fixed_decimals(1)).changed();
                    let mut d = *decimals as f32;
                    if ui.add(egui::DragValue::new(&mut d).range(0.0..=3.0).speed(0.1).prefix("dec ").fixed_decimals(0)).changed() {
                        *decimals = d.round() as u8;
                        changed = true;
                    }
                }
            }
            if ui.small_button("\u{00d7}").on_hover_text("Remove").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        loops.remove(i);
        changed = true;
    }
    if changed {
        cmds.push(AppCommand::SetTitleLoops { clip_id, loops });
    }
}

// ─── Style card ──────────────────────────────────────────────────────

fn to_c32(c: [u8; 4]) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

/// A labelled row of the style card: the label at a fixed width, the
/// controls after it.
fn style_row(ui: &mut egui::Ui, theme: &KlippTheme, label: &str, controls: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [52.0, crate::theme::control::COMPACT],
            egui::Label::new(egui::RichText::new(label).size(crate::theme::type_scale::CONTROL).color(theme.text_secondary)),
        );
        controls(ui);
    });
}

/// The style card: presets as chips, then fill, stroke, shadow and
/// glow as one row each. The full stacks live under Advanced.
pub fn show_style_card(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    tb: &TextBox,
    chips: &mut TitleChips,
    cmds: &mut Vec<AppCommand>,
) {
    caption(ui, theme, "STYLE");
    ui.add_space(4.0);
    // Presets, rendered on this text.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        for (id, label) in super::PRESET_REGISTRY.iter().filter(|(id, _)| *id != 0) {
            let tb_c = tb.clone();
            let pid = *id;
            let make = move || chip_title(&tb_c, |t| crate::app::apply_title_preset(t, pid));
            let resp = chip(
                ui,
                theme,
                chips,
                ChipSpec { id: format!("preset:{pid}"), tb, rest: 1.0, hover_from: 1.0, hover_len: FRAME_STEP, selected: false, label: Some(label) },
                ROW_CHIP,
                make,
            );
            if resp.clicked() {
                cmds.push(AppCommand::ApplyTitlePreset { clip_id, preset_id: pid });
            }
        }
    });
    ui.add_space(6.0);

    // Fill.
    style_row(ui, theme, "Fill", |ui| {
        let mut col = to_c32(tb.color);
        if ui.color_edit_button_srgba(&mut col).changed() {
            cmds.push(AppCommand::UpdateTitleData {
                clip_id,
                text: tb.text.clone(),
                font_size: tb.font_size,
                font_name: tb.font_name.clone(),
                font_style: super::font_style_to_u8(tb.font_style),
                color: col.to_array(),
            });
        }
        let g = tb.gradient.as_ref();
        let mut on = g.is_some_and(|g| g.enabled);
        let (mut top, mut bottom) = g.map(|g| (g.top_color, g.bottom_color)).unwrap_or((tb.color, [tb.color[0] / 2, tb.color[1] / 2, tb.color[2] / 2, tb.color[3]]));
        let mut send = false;
        if ui.checkbox(&mut on, "Gradient").changed() {
            send = true;
        }
        if on {
            let mut t = to_c32(top);
            if ui.color_edit_button_srgba(&mut t).changed() {
                top = t.to_array();
                send = true;
            }
            let mut b = to_c32(bottom);
            if ui.color_edit_button_srgba(&mut b).changed() {
                bottom = b.to_array();
                send = true;
            }
        }
        if send {
            cmds.push(AppCommand::SetTitleGradient { clip_id, enabled: on, top_color: top, bottom_color: bottom });
        }
    });
    // Stroke.
    style_row(ui, theme, "Stroke", |ui| {
        let o = tb.outlines.first();
        let mut on = o.is_some_and(|o| o.enabled);
        let (mut color, mut width) = o.map(|o| (o.color, o.width)).unwrap_or(([0, 0, 0, 255], 3.0));
        let mut send = ui.checkbox(&mut on, "").changed();
        let mut c = to_c32(color);
        if ui.color_edit_button_srgba(&mut c).changed() {
            color = c.to_array();
            send = true;
            on = true;
        }
        if ui.add(egui::DragValue::new(&mut width).range(0.5..=40.0).speed(0.1).suffix(" px").fixed_decimals(1)).changed() {
            send = true;
            on = true;
        }
        if send {
            cmds.push(AppCommand::SetTitleOutline { clip_id, enabled: on, color, width });
        }
    });
    // Shadow.
    style_row(ui, theme, "Shadow", |ui| {
        let s = tb.shadows.first();
        let mut on = s.is_some_and(|s| s.enabled);
        let (mut color, mut ox, mut oy, mut blur) = s.map(|s| (s.color, s.offset_x, s.offset_y, s.blur)).unwrap_or(([0, 0, 0, 200], 4.0, 4.0, 6.0));
        let mut send = ui.checkbox(&mut on, "").changed();
        let mut c = to_c32(color);
        if ui.color_edit_button_srgba(&mut c).changed() {
            color = c.to_array();
            send = true;
            on = true;
        }
        if ui.add(egui::DragValue::new(&mut ox).range(-100.0..=100.0).speed(0.2).prefix("x ").fixed_decimals(0)).changed() {
            send = true;
            on = true;
        }
        if ui.add(egui::DragValue::new(&mut oy).range(-100.0..=100.0).speed(0.2).prefix("y ").fixed_decimals(0)).changed() {
            send = true;
            on = true;
        }
        if ui.add(egui::DragValue::new(&mut blur).range(0.0..=60.0).speed(0.2).prefix("blur ").fixed_decimals(0)).changed() {
            send = true;
            on = true;
        }
        if send {
            cmds.push(AppCommand::SetTitleShadow { clip_id, enabled: on, color, offset_x: ox, offset_y: oy, blur });
        }
    });
    // Glow.
    style_row(ui, theme, "Glow", |ui| {
        let g = tb.glows.first();
        let mut on = g.is_some_and(|g| g.enabled);
        let (mut color, mut radius, mut intensity) = g.map(|g| (g.color, g.radius, g.intensity)).unwrap_or((tb.color, 12.0, 1.0));
        let mut send = ui.checkbox(&mut on, "").changed();
        let mut c = to_c32(color);
        if ui.color_edit_button_srgba(&mut c).changed() {
            color = c.to_array();
            send = true;
            on = true;
        }
        if ui.add(egui::DragValue::new(&mut radius).range(1.0..=100.0).speed(0.2).prefix("radius ").fixed_decimals(0)).changed() {
            send = true;
            on = true;
        }
        if ui.add(egui::DragValue::new(&mut intensity).range(0.1..=3.0).speed(0.02).prefix("\u{00d7}").fixed_decimals(2)).changed() {
            send = true;
            on = true;
        }
        if send {
            cmds.push(AppCommand::SetTitleGlow { clip_id, enabled: on, color, radius, intensity });
        }
    });
}
