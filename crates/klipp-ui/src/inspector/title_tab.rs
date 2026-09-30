//! The Title tab's first tier: the motion card (In, Out and the loops)
//! and the style card (presets, fill, stroke, shadow, glow). Every
//! choice is a chip rendered by the real title engine on the clip's
//! own text, so what is picked is what will play.

use crate::browser::title_previews::TitlePreviews;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use klipp_timeline::{TitleObjectContent, LayerFx, 
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
/// What a chip previews a motion or a loop on: the text it belongs
/// to, or the layer itself when the layer is a shape or a picture —
/// a card should see its own pill rise, not a word standing in for it.
#[derive(Clone)]
pub enum ChipSubject {
    Text(TextBox),
    Layer(TitleObjectContent),
}

fn chip_title(subject: &ChipSubject, fx: &LayerFx, adjust: impl FnOnce(&mut TextBox, &mut LayerFx)) -> TitleData {
    let mut f = fx.clone();
    let ChipSubject::Text(tb) = subject else {
        let ChipSubject::Layer(content) = subject else { unreachable!() };
        // The layer, fitted to the chip, with its stack and the motion
        // or loop the chip is about; the text box the adjustment sees
        // is scratch, since a layer has no type to restyle.
        f.motion = TitleMotion::default();
        f.loops.clear();
        let mut scratch = TextBox::default();
        adjust(&mut scratch, &mut f);
        let mut td = TitleData { objects: Vec::new(), next_object_id: 0, animation_type: Default::default(), background: klipp_timeline::TitleBackground::None };
        let mut xf = klipp_timeline::default_text_transform();
        xf.size = klipp_timeline::TitlePosition { x: 0.6, y: 0.6 };
        let id = td.add_object(content.clone(), xf);
        td.object_mut(id).expect("just added").fx = Some(f);
        return td;
    };
    let mut t = tb.clone();
    let line = t.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let short: String = line.chars().take(14).collect();
    t.text = if short.is_empty() { "Title".to_string() } else { short };
    t.font_size = CHIP_FONT;
    t.alignment = TextAlignment::Center;
    t.style_runs.clear();
    t.write_on = 1.0;
    t.outline_write_on = 1.0;
    f.motion = TitleMotion::default();
    f.loops.clear();
    t.text_path = None;
    t.columns = 1;
    t.vertical_layout = false;
    t.drop_cap_scale = 0.0;
    t.baseline_shift = 0.0;
    adjust(&mut t, &mut f);
    TitleData::single_text_fx(t, f)
}

/// A signature of everything about the layer that changes how a chip
/// looks, so the cache renews when the style does.
fn style_signature(subject: &ChipSubject, fx: &LayerFx) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let tb = match subject {
        ChipSubject::Text(tb) => tb,
        ChipSubject::Layer(content) => {
            format!("{content:?}{:?}{:?}{:?}{:?}", fx.outlines, fx.shadows, fx.glows, fx.inner_shadows).hash(&mut h);
            return h.finish();
        }
    };
    tb.font_name.hash(&mut h);
    format!("{:?}", tb.font_style).hash(&mut h);
    tb.color.hash(&mut h);
    format!("{:?}{:?}{:?}{:?}{:?}", fx.outlines, fx.shadows, fx.glows, tb.gradient, fx.inner_shadows).hash(&mut h);
    format!("{:?}{:?}{:?}", tb.extrusion, tb.pattern_fill, tb.texture_fill).hash(&mut h);
    format!("{:?}", tb.case_transform).hash(&mut h);
    tb.tracking.to_bits().hash(&mut h);
    h.finish()
}

fn chip_text(subject: &ChipSubject) -> String {
    match subject {
        ChipSubject::Text(tb) => tb.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("Title").trim().chars().take(14).collect(),
        ChipSubject::Layer(TitleObjectContent::Shape(_)) => "shape".into(),
        ChipSubject::Layer(_) => "picture".into(),
    }
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
    subject: &'a ChipSubject,
    /// The layer's stack, which is where the style a chip shows lives.
    fx: &'a LayerFx,
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
    let template = format!("chip:{}:{:x}", spec.id, style_signature(spec.subject, spec.fx));
    let text = chip_text(spec.subject);
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
fn motion_chip_title(subject: &ChipSubject, fx: &LayerFx, dir: Direction, m: Option<Motion>) -> TitleData {
    chip_title(subject, fx, |_, f| {
        f.motion = match dir {
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
    subject: &ChipSubject,
    fx: &LayerFx,
    dir: Direction,
    slot: &mut Option<Motion>,
) -> bool {
    let mut changed = false;
    let (rest, hover_from, hover_len) = dir.timing();
    let popup_id = ui.id().with(("title_motion_gallery", dir.label()));
    let mut open_gallery = false;
    // The motion row contains four controls and is wider than the compact
    // Inspector at its normal width.  Wrapping keeps every control inside
    // the panel instead of letting the final duration field disappear past
    // the right edge.
    ui.horizontal_wrapped(|ui| {
        ui.add_sized(
            [28.0, ROW_CHIP.y],
            egui::Label::new(egui::RichText::new(dir.label()).size(crate::theme::type_scale::CONTROL).color(theme.text_secondary)),
        );
        let current = *slot;
        let s_c = subject.clone();
        let fx_c = fx.clone();
        let make = move || motion_chip_title(&s_c, &fx_c, dir, current);
        let kind_id = current.map(|m| format!("{:?}:{:.2}:{:?}", m.kind, m.duration_secs, m.easing)).unwrap_or_else(|| "none".into());
        let resp = chip(
            ui,
            theme,
            chips,
            ChipSpec { id: format!("row:{}:{}", dir.label(), kind_id), subject, fx, rest, hover_from, hover_len, selected: false, label: None },
            ROW_CHIP,
            make,
        );
        if resp.clicked() {
            open_gallery = true;
        }
        let name = current.map(|m| m.kind.label()).unwrap_or("None");
        let kind_width = ui.available_width().clamp(86.0, 120.0);
        let btn = ui.add_sized(
            [kind_width, crate::theme::control::COMPACT],
            egui::Button::new(egui::RichText::new(format!("{name}  \u{25be}")).size(crate::theme::type_scale::CONTROL)),
        );
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
                    let s_c = subject.clone();
                    let fx_c = fx.clone();
                    let make = move || motion_chip_title(&s_c, &fx_c, dir, m);
                    let label = k.map(|k| k.label()).unwrap_or("None");
                    let id = format!("gallery:{}:{}", dir.label(), label);
                    let resp = chip(
                        ui,
                        theme,
                        chips,
                        ChipSpec { id, subject, fx, rest, hover_from, hover_len, selected: *k == selected_kind, label: Some(label) },
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
        ui.horizontal_wrapped(|ui| {
            ui.add_space(28.0 + 8.0);
            let arrow = if open { "\u{25be}" } else { "\u{25b8}" };
            if ui.add(egui::Button::new(egui::RichText::new(format!("{arrow} Details")).size(crate::theme::type_scale::CAPTION).color(theme.text_secondary)).frame(false)).clicked() {
                open = !open;
                ui.memory_mut(|mem| mem.data.insert_temp(details_id, open));
            }
        });
        if open {
            ui.horizontal_wrapped(|ui| {
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
#[allow(clippy::too_many_arguments)]
pub fn show_motion_card(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    subject: &ChipSubject,
    fx: &LayerFx,
    travel: Travel,
    chips: &mut TitleChips,
    cmds: &mut Vec<AppCommand>,
) {
    caption(ui, theme, "ANIMATION");
    ui.label(
        egui::RichText::new("Entrance and exit animation for this text layer")
            .size(crate::theme::type_scale::CAPTION)
            .color(theme.text_secondary),
    );
    ui.add_space(4.0);
    let mut motion = fx.motion;
    let mut changed = false;

    // Keep the most common title actions one click away.  The full motion
    // gallery remains below, but users should not have to discover that a
    // thumbnail opens a picker just to fade or type a title on.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        if ui.small_button("Fade In").on_hover_text("Fade this text layer in").clicked() {
            motion.enter = Some(Motion::new(MotionKind::Fade));
            changed = true;
        }
        if ui.small_button("Fade Out").on_hover_text("Fade this text layer out").clicked() {
            motion.exit = Some(Motion::new(MotionKind::Fade));
            changed = true;
        }
        if ui.small_button("Type On").on_hover_text("Reveal the text one character at a time").clicked() {
            motion.enter = Some(Motion::new(MotionKind::Typewriter));
            changed = true;
        }
        if ui.small_button("Rise").on_hover_text("Bring the text up into place").clicked() {
            motion.enter = Some(Motion::new(MotionKind::Rise));
            changed = true;
        }
        if ui.small_button("Slide").on_hover_text("Slide the text into place").clicked() {
            motion.enter = Some(Motion::new(MotionKind::SlideRight));
            changed = true;
        }
        if ui.small_button("Clear").on_hover_text("Remove entrance and exit animation").clicked() {
            motion.enter = None;
            motion.exit = None;
            changed = true;
        }
    });
    let in_label = motion.enter.map(|m| m.kind.label()).unwrap_or("None");
    let out_label = motion.exit.map(|m| m.kind.label()).unwrap_or("None");
    ui.label(
        egui::RichText::new(format!("In: {in_label}  ·  Out: {out_label}"))
            .size(crate::theme::type_scale::CAPTION)
            .color(theme.text_dim),
    );
    ui.add_space(4.0);
    changed |= motion_row(ui, theme, chips, subject, fx, Direction::Enter, &mut motion.enter);
    ui.add_space(4.0);
    changed |= motion_row(ui, theme, chips, subject, fx, Direction::Exit, &mut motion.exit);
    if changed {
        cmds.push(AppCommand::SetTitleMotion { clip_id, motion });
    }
    ui.add_space(8.0);
    show_loops(ui, theme, clip_id, subject, fx, travel, chips, cmds);
}

fn loop_chip_title(subject: &ChipSubject, fx: &LayerFx, l: &TitleLoop) -> TitleData {
    let l = l.clone();
    chip_title(subject, fx, move |_, f| f.loops = vec![l])
}

/// The loops: a gallery to add from, then one row per loop with its
/// own timing.
#[allow(clippy::too_many_arguments)]
fn show_loops(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    subject: &ChipSubject,
    fx: &LayerFx,
    travel: Travel,
    chips: &mut TitleChips,
    cmds: &mut Vec<AppCommand>,
) {
    let mut loops = fx.loops.clone();
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
                        let s_c = subject.clone();
                        let fx_c = fx.clone();
                        let l_c = l.clone();
                        let make = move || loop_chip_title(&s_c, &fx_c, &l_c);
                        let resp = chip(
                            ui,
                            theme,
                            chips,
                            ChipSpec { id: format!("loop:{}", l.label()), subject, fx, rest: 0.35, hover_from: 0.0, hover_len: LOOPS_HOVER_LOOP, selected: false, label: Some(l.label()) },
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
                TitleLoop::Scroll { direction, .. } => {
                    let up = *direction == ScrollDirection::Up;
                    if let Some(i) = crate::widgets::segmented::segmented(ui, &["Up", "Down"], if up { 0 } else { 1 }, theme, egui::FontId::proportional(crate::theme::type_scale::CAPTION), crate::theme::control::COMPACT) {
                        *direction = if i == 0 { ScrollDirection::Up } else { ScrollDirection::Down };
                        changed = true;
                    }
                }
                TitleLoop::Crawl { direction, repeat, .. } => {
                    let left = *direction == CrawlDirection::Left;
                    if let Some(i) = crate::widgets::segmented::segmented(ui, &["\u{2190}", "\u{2192}"], if left { 0 } else { 1 }, theme, egui::FontId::proportional(crate::theme::type_scale::CAPTION), crate::theme::control::COMPACT) {
                        *direction = if i == 0 { CrawlDirection::Left } else { CrawlDirection::Right };
                        changed = true;
                    }
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
        // The travelling loops' speed on a line of its own, always shown.
        match l {
            TitleLoop::Scroll { speed_px_per_sec, .. } => changed |= roll_speed(ui, theme, speed_px_per_sec, travel, true),
            TitleLoop::Crawl { speed_px_per_sec, .. } => changed |= roll_speed(ui, theme, speed_px_per_sec, travel, false),
            _ => {}
        }
    }
    if let Some(i) = remove {
        loops.remove(i);
        changed = true;
    }
    if changed {
        cmds.push(AppCommand::SetTitleLoops { clip_id, loops });
    }
}

/// What a travelling loop's speed is measured against: the frame and
/// the layer in pixels of a 1080p frame, as the speed itself is, and the
/// clip's length, which a fitted roll takes.
#[derive(Debug, Clone, Copy)]
pub struct Travel {
    frame: (f32, f32),
    layer: (f32, f32),
    clip_secs: f32,
}

impl Travel {
    /// `layer` is the layer's (width, height) at 1080p; `frame` the
    /// sequence's, and `clip_secs` the clip's length on the timeline.
    pub fn new(frame: klipp_core::Resolution, layer: (f32, f32), clip_secs: f64) -> Self {
        let (w, h) = (frame.width.max(1) as f32, frame.height.max(1) as f32);
        let s = klipp_core::title_motion::frame_scale(w, h);
        Self { frame: (w / s, h / s), layer, clip_secs: clip_secs.max(0.001) as f32 }
    }

    fn distance(&self, vertical: bool) -> f32 {
        if vertical {
            klipp_core::title_motion::travel_distance(self.frame.1, self.layer.1)
        } else {
            klipp_core::title_motion::travel_distance(self.frame.0, self.layer.0)
        }
    }
}

/// A travelling loop's speed, always in view: the rate in px/s at 1080p
/// and the time the travel takes, either of them editable, and whether
/// it is fitted to the clip. Fitted, the rate shown is the one the clip's
/// length gives, and editing it sets that rate. It used to be hidden
/// until "Fit clip" was unticked, and fitted there was no telling how
/// fast a roll went (#588). Returns true when it changed.
fn roll_speed(ui: &mut egui::Ui, theme: &KlippTheme, speed: &mut f32, travel: Travel, vertical: bool) -> bool {
    let mut changed = false;
    let distance = travel.distance(vertical);
    let clip = travel.clip_secs;
    let fitted = distance / clip;
    let dim = |t: &str| egui::RichText::new(t).size(crate::theme::type_scale::CAPTION).color(theme.text_dim);
    ui.horizontal_wrapped(|ui| {
        ui.add_space(4.0);
        ui.label(dim("Speed"));
        let mut rate = if *speed > 0.0 { *speed } else { fitted };
        let r = ui
            .add(egui::DragValue::new(&mut rate).range(1.0..=5000.0).speed(1.0).suffix(" px/s").fixed_decimals(0))
            .on_hover_text("Pixels per second of a 1080p frame: the same pace on any frame size");
        if r.changed() {
            *speed = rate.max(1.0);
            changed = true;
        }
        let mut fit = *speed <= 0.0;
        if ui.checkbox(&mut fit, "Fit clip").on_hover_text("Travel the whole way over the clip's length").changed() {
            // Unfitted at the pace it had, so nothing jumps.
            *speed = if fit { 0.0 } else { fitted.round().max(1.0) };
            changed = true;
        }
        ui.label(dim("takes"));
        let mut secs = klipp_core::title_motion::travel_secs(distance, *speed, clip);
        if ui
            .add(egui::DragValue::new(&mut secs).range(0.2..=3600.0).speed(0.05).suffix(" s").fixed_decimals(1))
            .on_hover_text("How long it takes to travel through the frame")
            .changed()
        {
            *speed = (distance / secs.max(0.2)).max(1.0);
            changed = true;
        }
    });
    if *speed > 0.0 {
        let secs = distance / *speed;
        let note = if secs > clip + 0.05 {
            Some(format!("Longer than the clip ({clip:.1} s): the end is cut off"))
        } else if secs < clip - 0.05 {
            Some(format!("Centred in the clip: nothing on screen for {:.1} s at each end", (clip - secs) * 0.5))
        } else {
            None
        };
        if let Some(note) = note {
            ui.label(dim(&note));
        }
    }
    changed
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
    fx: &LayerFx,
    chips: &mut TitleChips,
    cmds: &mut Vec<AppCommand>,
) {
    let subject = ChipSubject::Text(tb.clone());
    caption(ui, theme, "APPEARANCE");
    ui.add_space(4.0);
    // Presets, rendered on this text.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        for (id, label) in super::PRESET_REGISTRY.iter().filter(|(id, _)| *id != 0) {
            let s_c = ChipSubject::Text(tb.clone());
            let fx_c = fx.clone();
            let pid = *id;
            let make = move || chip_title(&s_c, &fx_c, |t, f| crate::app::apply_title_preset(t, f, pid));
            let resp = chip(
                ui,
                theme,
                chips,
                ChipSpec { id: format!("preset:{pid}"), subject: &subject, fx, rest: 1.0, hover_from: 1.0, hover_len: FRAME_STEP, selected: false, label: Some(label) },
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
        let o = fx.outlines.first();
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
        let s = fx.shadows.first();
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
        let g = fx.glows.first();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A shape previews as itself: the chip's title is the shape, sized
    /// for the chip, carrying the motion the chip is about and nothing
    /// of the stack's own motion or loops.
    #[test]
    fn a_shape_chip_is_the_shape_with_the_motion_on_it() {
        let shape = TitleObjectContent::Shape(klipp_timeline::Shape { kind: klipp_timeline::ShapeKind::Rectangle { corner_radius: 4.0 }, fill: Some([1, 2, 3, 255]), stroke: None, fit: None });
        let fx = LayerFx { loops: vec![TitleLoop::Pulse { rate_hz: 1.0, min_opacity: 0.5 }], ..Default::default() };
        let rise = Motion::new(MotionKind::Rise).duration(0.3);
        let td = motion_chip_title(&ChipSubject::Layer(shape.clone()), &fx, Direction::Enter, Some(rise));
        assert_eq!(td.objects.len(), 1);
        assert_eq!(td.objects[0].content, shape, "the chip shows the shape itself");
        let chip_fx = td.objects[0].fx();
        assert_eq!(chip_fx.motion.enter, Some(rise), "with the motion the chip is about");
        assert!(chip_fx.loops.is_empty(), "and not the layer's own loops");
        assert_eq!(chip_text(&ChipSubject::Layer(shape)), "shape");
    }
}

#[cfg(test)]
mod roll_speed_tests {
    use super::Travel;
    use klipp_core::title_motion::{eval_loops, frame_scale, travel_secs, LoopGeometry};
    use klipp_timeline::{ScrollDirection, TextBox, TitleLoop};

    /// #588: the time the Title tab shows a roll taking is the time the
    /// renderer takes: it starts just below the frame when the roll
    /// begins and has just left it when that time is up, on landscape,
    /// 4K and vertical frames alike.
    #[test]
    fn the_roll_time_shown_is_the_time_the_render_takes() {
        let tb = TextBox { text: "Director\nA Name\n\nEditor\nA Name".into(), font_size: 60.0, ..Default::default() };
        let (clip, speed) = (40.0f32, 150.0f32);
        let loops = [TitleLoop::Scroll { direction: ScrollDirection::Up, speed_px_per_sec: speed }];
        for (w, h) in [(1920u32, 1080u32), (3840, 2160), (2160, 3840)] {
            let travel = Travel::new(klipp_core::Resolution { width: w, height: h }, tb.loop_extent(tb.font_size), clip as f64);
            let secs = travel_secs(travel.distance(true), speed, clip);
            assert!(secs < clip, "{w}x{h}: the roll fits the clip in this test");
            let s = frame_scale(w as f32, h as f32);
            let (text_w, text_h) = tb.loop_extent(tb.font_size * s);
            let geo = LoopGeometry { frame_w: w as f32, frame_h: h as f32, text_w, text_h, font_px: tb.font_size * s };
            let offset = klipp_core::title_motion::travel_distance(h as f32, text_h) * 0.5;
            let pad = (clip - secs) * 0.5;
            let dy = |t: f32| eval_loops(&loops, t, Some(clip), geo, [255; 4]).dy;
            assert!((dy(pad) - offset).abs() < 0.5, "{w}x{h}: starts below the frame: {} vs {offset}", dy(pad));
            assert!((dy(pad + secs) + offset).abs() < 0.5, "{w}x{h}: has just left it {secs:.2} s later: {} vs {}", dy(pad + secs), -offset);
            assert!(dy(pad + secs * 0.9) > -offset + 1.0, "{w}x{h}: and not before");
        }
    }
}
