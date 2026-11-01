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

use super::rows::{rail_row, switch, TEXT};
use crate::widgets::inspector_group::section_card;

/// Chip render size: twice the display size so it is sharp on a 2×
/// display.
const CHIP_W: u32 = 224;
const CHIP_H: u32 = 126;
/// A gallery chip on screen.
const GALLERY_CHIP: egui::Vec2 = egui::vec2(104.0, 58.5);
/// The chip in an In / Out row.
const ROW_CHIP: egui::Vec2 = egui::vec2(72.0, 40.5);
/// The picker's large preview, rendered at this size (#608).
const BIG_W: u32 = 320;
const BIG_H: u32 = 180;
/// Playback: the frame step, and how long a loop plays before it starts
/// over.
const FRAME_STEP: f32 = 1.0 / 25.0;
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
        let mut td = TitleData { objects: Vec::new(), next_object_id: 0, animation_type: Default::default(), background: klipp_timeline::TitleBackground::None, style: None, published: Vec::new(), template: None, protect: Default::default() };
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
    format!("{:?}{:?}{:?}", tb.font_style, tb.font_weight, tb.font_axes).hash(&mut h);
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

/// How a chip plays while it is watched (#608): the whole effect at its
/// own speed, with a moment on either side so the eye sees where the
/// text comes from and where it ends up. A loop of the first 1.6 s,
/// starting the instant it was hovered, was over before it was seen.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Play {
    /// An entrance `span` seconds long: nothing yet, the entrance, then
    /// the text at rest.
    Enter { span: f32 },
    /// An exit `span` seconds long, which ends at the chip title's end:
    /// the text at rest, the exit, then nothing.
    Exit { span: f32 },
    /// A loop, straight through `len` seconds.
    Loop { len: f32 },
    /// A still at `at`: a style, which does not move.
    Still { at: f32 },
}

/// Before an entrance, and after an exit: the empty frame.
const PLAY_EMPTY: f32 = 0.45;
/// After an entrance, and before an exit: the text at rest.
const PLAY_REST: f32 = 1.0;

impl Play {
    fn len(self) -> f32 {
        match self {
            Play::Enter { span } | Play::Exit { span } => PLAY_EMPTY + span + PLAY_REST,
            Play::Loop { len } => len,
            Play::Still { .. } => FRAME_STEP,
        }
    }

    /// The chip title's time `elapsed` seconds into watching it, in
    /// frame steps.
    fn time(self, elapsed: f32) -> f32 {
        let u = ((elapsed % self.len().max(FRAME_STEP)) / FRAME_STEP).floor() * FRAME_STEP;
        match self {
            Play::Enter { .. } => (u - PLAY_EMPTY).clamp(0.0, WINDOW),
            Play::Exit { span } => (WINDOW - span - PLAY_REST + u).clamp(0.0, WINDOW),
            Play::Loop { .. } => u,
            Play::Still { at } => at,
        }
    }
}

/// How long `m` takes on `text`: its delay and duration, and the stagger
/// across the text's pieces, to the last one.
fn motion_span(m: &Motion, text: &str) -> f32 {
    let units = match m.stagger {
        StaggerUnit::None => 1,
        StaggerUnit::Char => text.chars().filter(|c| !c.is_whitespace()).count(),
        StaggerUnit::Word => text.split_whitespace().count(),
        StaggerUnit::Line => text.lines().count(),
    }
    .max(1);
    m.delay_secs + m.duration_secs + m.stagger_secs * (units - 1) as f32
}

struct ChipSpec<'a> {
    /// What the chip is, for the cache and the hover clock.
    id: String,
    subject: &'a ChipSubject,
    /// The layer's stack, which is where the style a chip shows lives.
    fx: &'a LayerFx,
    /// The moment shown at rest, and how it plays while watched.
    rest: f32,
    play: Play,
    selected: bool,
    label: Option<&'a str>,
    /// Played all the time, not only while hovered: the picker's large
    /// preview.
    always: bool,
    /// The size it is rendered at.
    render: (u32, u32),
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
    // The clock is the effect's own, so a large preview that turns to
    // another effect starts it from the beginning.
    let elapsed = hover_elapsed(ui, resp.id.with(&spec.id), hovered || spec.always);
    let time = match elapsed {
        Some(e) => spec.play.time(e),
        None => spec.rest,
    };
    let (rw, rh) = spec.render;
    if elapsed.is_some() && !matches!(spec.play, Play::Still { .. }) {
        // The next few steps, so play never waits on a render.
        for k in 1..=8 {
            let t = spec.play.time(elapsed.unwrap_or(0.0) + k as f32 * FRAME_STEP);
            let key = TitlePreviews::key(&template, &text, t);
            let mk = make.clone();
            chips.previews.request_sized(&key, t, rw, rh, mk);
        }
        ui.ctx().request_repaint();
    }
    let key = TitlePreviews::key(&template, &text, time);
    let tex = chips
        .previews
        .get_or_request_sized(&key, time, rw, rh, make.clone())
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

/// The large preview's size: a third of the picker's width, at 16:9. At
/// the whole width it pushed the effects out of the inspector.
fn big_size(ui: &egui::Ui) -> egui::Vec2 {
    let w = ui.available_width() / 3.0;
    egui::vec2(w, w * 9.0 / 16.0)
}


// ─── The effect picker ───────────────────────────────────────────────

/// One entry of an effect picker: its key (for Recent), name and group.
/// An empty group is shown under All only (None).
struct GalleryEntry {
    key: String,
    label: &'static str,
    group: &'static str,
}

/// How many picks a picker's Recent keeps.
const RECENT_PICKS: usize = 6;

/// The body of an effect picker (#596), one for In, Out and Loops: a
/// search field, the groups as pills (All, Recent once there is any,
/// then each group), and the matching entries as live chips, three to a
/// row, scrolling. `draw` draws entry `i`'s chip; the entry clicked comes
/// back and goes to the front of the picker's Recent, kept under
/// `recent_key` for the session. A row of buttons, one per effect, did
/// not scale past a handful and duplicated the pickers.
fn gallery_body(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    id: egui::Id,
    recent_key: &str,
    entries: &[GalleryEntry],
    mut draw: impl FnMut(&mut egui::Ui, usize, bool) -> egui::Response,
) -> Option<usize> {
    let (search_id, group_id, focus_id) = (id.with("search"), id.with("group"), id.with("focused"));
    let recent_id = egui::Id::new(("title_gallery_recent", recent_key));
    let mut search: String = ui.memory(|m| m.data.get_temp(search_id)).unwrap_or_default();
    let mut group: Option<String> = ui.memory(|m| m.data.get_temp(group_id)).unwrap_or_default();
    let mut recent: Vec<String> = ui.memory(|m| m.data.get_temp(recent_id)).unwrap_or_default();
    recent.retain(|k| entries.iter().any(|e| &e.key == k));

    ui.set_min_width(3.0 * GALLERY_CHIP.x + 2.0 * 8.0 + 16.0);
    ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
    let field = ui.add(egui::TextEdit::singleline(&mut search).hint_text("Search").desired_width(f32::INFINITY));
    if !ui.memory(|m| m.data.get_temp::<bool>(focus_id).unwrap_or(false)) {
        field.request_focus();
        ui.memory_mut(|m| m.data.insert_temp(focus_id, true));
    }
    let mut groups: Vec<&'static str> = Vec::new();
    for e in entries {
        if !e.group.is_empty() && !groups.contains(&e.group) {
            groups.push(e.group);
        }
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let pill = |ui: &mut egui::Ui, on: bool, text: &str| {
            ui.selectable_label(on, egui::RichText::new(text).size(crate::theme::type_scale::CAPTION)).clicked()
        };
        if pill(ui, group.is_none(), "All") {
            group = None;
        }
        if !recent.is_empty() && pill(ui, group.as_deref() == Some("Recent"), "Recent") {
            group = Some("Recent".into());
        }
        for g in &groups {
            if pill(ui, group.as_deref() == Some(*g), g) {
                group = Some((*g).to_string());
            }
        }
    });
    let needle = search.trim().to_lowercase();
    let shown: Vec<usize> = match group.as_deref() {
        Some("Recent") => recent.iter().filter_map(|k| entries.iter().position(|e| &e.key == k)).collect::<Vec<usize>>(),
        g => (0..entries.len()).filter(|&i| g.is_none_or(|g| entries[i].group == g)).collect(),
    }
    .into_iter()
    .filter(|&i| needle.is_empty() || entries[i].label.to_lowercase().contains(&needle) || entries[i].group.to_lowercase().contains(&needle))
    .collect();
    let mut picked = None;
    egui::ScrollArea::vertical().max_height(330.0).auto_shrink([false, true]).show(ui, |ui| {
        if shown.is_empty() {
            ui.label(egui::RichText::new("Nothing matches").size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
        }
        for row in shown.chunks(3) {
            ui.horizontal(|ui| {
                for &i in row {
                    let resp = draw(ui, i, false);
                    if resp.clicked() {
                        picked = Some(i);
                    }
                    if resp.hovered() {
                        ui.memory_mut(|m| m.data.insert_temp(id.with("hovered"), Some(i)));
                    }
                }
            });
        }
    });
    if let Some(i) = picked {
        let key = entries[i].key.clone();
        recent.retain(|k| *k != key);
        recent.insert(0, key);
        recent.truncate(RECENT_PICKS);
        search.clear();
        ui.memory_mut(|m| m.data.remove::<bool>(focus_id));
    }
    ui.memory_mut(|m| {
        m.data.insert_temp(search_id, search);
        m.data.insert_temp(group_id, group);
        m.data.insert_temp(recent_id, recent);
    });
    picked
}

/// Which of the tab's pickers is open, if any: one at a time, so the tab
/// never shows two grids of effects at once (#604).
fn open_picker(ui: &egui::Ui) -> Option<egui::Id> {
    ui.memory(|m| m.data.get_temp(egui::Id::new("title_open_picker"))).flatten()
}

/// Open picker `id`, or none. Whichever way a picker closes or opens —
/// a pick, Esc, its field clicked again, another one opened — its search
/// field is forgotten as focused, so the next time it opens it takes the
/// keyboard again (#605 review).
fn set_open_picker(ui: &egui::Ui, id: Option<egui::Id>) {
    let was = open_picker(ui);
    ui.memory_mut(|m| {
        for p in [was, id].into_iter().flatten() {
            m.data.remove::<bool>(p.with("body").with("focused"));
        }
        m.data.insert_temp(egui::Id::new("title_open_picker"), id);
    });
}

/// A picker field (#604): what is chosen, by name, or what choosing does,
/// on a full-width button that opens the picker below it inside the
/// inspector. It reads as a place to choose from many, where a small
/// "None ▾" read as a plain dropdown. Returns true when clicked.
fn picker_field(ui: &mut egui::Ui, theme: &KlippTheme, text: &str, open: bool) -> bool {
    let arrow = if open { "\u{25b4}" } else { "\u{25be}" };
    let label = egui::RichText::new(format!("{text}   {arrow}")).size(crate::theme::type_scale::CONTROL).color(theme.text_primary);
    ui.add(egui::Button::new(label).min_size(egui::vec2(ui.available_width(), crate::theme::control::COMPACT + 4.0)))
        .on_hover_text("Choose from every effect, with a live preview of each")
        .clicked()
}

/// The picker, open inside the inspector under its field: a large
/// preview, then the shared body, in a frame, closed by Esc or by its
/// field. `draw(ui, i, big)` draws entry `i`'s chip, or with `big` its
/// large preview. Returns the entry picked, if any; picking closes it.
///
/// The large preview plays the effect under the pointer, else `current`,
/// the whole of it, over and over (#608): a small chip playing its first
/// moment while hovered went by too fast to tell a Drop from a Rise.
#[allow(clippy::too_many_arguments)]
fn inline_picker(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    id: egui::Id,
    recent_key: &str,
    entries: &[GalleryEntry],
    current: Option<usize>,
    mut draw: impl FnMut(&mut egui::Ui, usize, bool) -> egui::Response,
) -> Option<usize> {
    let mut picked = None;
    let hovered_id = id.with("body").with("hovered");
    // The chip under the pointer last frame, kept while the pointer moves
    // across the gaps between chips so the preview does not jump back.
    let hovered: Option<usize> = ui.memory(|m| m.data.get_temp(hovered_id)).flatten();
    let frame = egui::Frame::none()
        .fill(theme.bg_panel_alt)
        .stroke(egui::Stroke::new(1.0_f32, theme.border))
        .rounding(crate::theme::radius::PANEL)
        .inner_margin(egui::Margin::same(8.0))
        .show(ui, |ui| {
            // The picture with its name beside it: the same height either
            // way, so the chips below never move under the pointer.
            ui.horizontal(|ui| {
                let name = match hovered.or(current).filter(|&i| i < entries.len()) {
                    Some(i) => {
                        draw(ui, i, true);
                        egui::RichText::new(entries[i].label).color(theme.text_primary)
                    }
                    None => {
                        let (rect, _) = ui.allocate_exact_size(big_size(ui), egui::Sense::hover());
                        ui.painter().rect_filled(rect, crate::theme::radius::PANEL, theme.bg_panel);
                        egui::RichText::new("Point at an effect to see it play").color(theme.text_dim)
                    }
                };
                ui.add(egui::Label::new(name.size(crate::theme::type_scale::CONTROL)).wrap());
            });
            ui.add_space(4.0);
            picked = gallery_body(ui, theme, id.with("body"), recent_key, entries, &mut draw);
        });
    if !ui.rect_contains_pointer(frame.response.rect) {
        ui.memory_mut(|m| m.data.remove::<Option<usize>>(hovered_id));
    }
    if picked.is_some() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        set_open_picker(ui, None);
    }
    picked
}

// ─── Motion card ─────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Enter,
    Exit,
}

impl Direction {
    /// The key its picker's state and Recent are kept under.
    fn label(self) -> &'static str {
        match self {
            Direction::Enter => "In",
            Direction::Exit => "Out",
        }
    }
    fn title(self) -> &'static str {
        match self {
            Direction::Enter => "Entrance",
            Direction::Exit => "Exit",
        }
    }
    fn prompt(self) -> &'static str {
        match self {
            Direction::Enter => "Choose entrance\u{2026}",
            Direction::Exit => "Choose exit\u{2026}",
        }
    }
    /// The moment a chip of this direction shows at rest.
    fn rest(self) -> f32 {
        match self {
            Direction::Enter => REST_IN,
            Direction::Exit => WINDOW - REST_IN,
        }
    }
    /// How `m` plays in this direction on `text`.
    fn play(self, m: Option<Motion>, text: &str) -> Play {
        let span = m.map_or(0.0, |m| motion_span(&m, text)).min(WINDOW - PLAY_EMPTY - PLAY_REST);
        match self {
            Direction::Enter => Play::Enter { span },
            Direction::Exit => Play::Exit { span },
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
    let rest = dir.rest();
    let text = chip_text(subject);
    let picker_id = egui::Id::new(("title_motion_picker", dir.label()));
    let open = open_picker(ui) == Some(picker_id);
    let presets = klipp_core::title_motion::MotionPreset::all();
    // The chosen motion by its preset's name where it is one, else its kind.
    let name = slot.map(|m| presets.iter().find(|p| p.motion.kind == m.kind && p.motion.stagger == m.stagger).map_or(m.kind.label(), |p| p.name));
    ui.horizontal(|ui| {
        ui.add_sized(
            [58.0, ROW_CHIP.y],
            egui::Label::new(egui::RichText::new(dir.title()).size(TEXT).color(theme.text_secondary)),
        );
        let current = *slot;
        let s_c = subject.clone();
        let fx_c = fx.clone();
        let make = move || motion_chip_title(&s_c, &fx_c, dir, current);
        let kind_id = current.map(|m| format!("{:?}:{:.2}:{:?}:{:?}", m.kind, m.duration_secs, m.easing, m.stagger)).unwrap_or_else(|| "none".into());
        let resp = chip(
            ui,
            theme,
            chips,
            ChipSpec {
                id: format!("row:{}:{}", dir.label(), kind_id),
                subject,
                fx,
                rest,
                play: dir.play(current, &text),
                selected: false,
                label: None,
                always: false,
                render: (CHIP_W, CHIP_H),
            },
            ROW_CHIP,
            make,
        );
        let clicked_field = ui.vertical(|ui| picker_field(ui, theme, name.unwrap_or(dir.prompt()), open)).inner;
        if resp.clicked() || clicked_field {
            set_open_picker(ui, if open { None } else { Some(picker_id) });
        }
    });
    if let Some(m) = slot.as_mut() {
        rail_row(ui, theme, "Duration", |ui| {
            if ui.add(egui::DragValue::new(&mut m.duration_secs).range(0.05..=klipp_core::title_motion::MAX_DURATION_SECS).speed(0.02).suffix(" s").fixed_decimals(2)).changed() {
                changed = true;
            }
        });
    }
    // The picker, inside the inspector under its field: None and every
    // preset, rendered on this text.
    let mut pick: Option<Option<Motion>> = None;
    if open {
        let items: Vec<Option<klipp_core::title_motion::MotionPreset>> = std::iter::once(None).chain(presets.into_iter().map(Some)).collect();
        let entries: Vec<GalleryEntry> = items
            .iter()
            .map(|p| GalleryEntry { key: p.map_or("none".into(), |p| p.name.to_string()), label: p.map_or("None", |p| p.name), group: p.map_or("", |p| p.group) })
            .collect();
        let current = slot.map(|m| (m.kind, m.stagger));
        let current_i = items.iter().position(|p| p.map(|p| (p.motion.kind, p.motion.stagger)) == current);
        let picked = inline_picker(ui, theme, picker_id, dir.label(), &entries, current_i, |ui, i, big| {
            let m = items[i].map(|p| p.motion);
            let s_c = subject.clone();
            let fx_c = fx.clone();
            let make = move || motion_chip_title(&s_c, &fx_c, dir, m);
            let label = entries[i].label;
            let selected = m.map(|m| (m.kind, m.stagger)) == current;
            if big {
                let id = format!("big:{}:{}", dir.label(), label);
                let spec = ChipSpec { id, subject, fx, rest, play: dir.play(m, &text), selected: false, label: None, always: true, render: (BIG_W, BIG_H) };
                return chip(ui, theme, chips, spec, big_size(ui), make);
            }
            // The large preview plays the chip under the pointer, so the
            // chip itself holds still.
            let id = format!("gallery:{}:{}", dir.label(), label);
            let spec = ChipSpec { id, subject, fx, rest, play: Play::Still { at: rest }, selected, label: Some(label), always: false, render: (CHIP_W, CHIP_H) };
            chip(ui, theme, chips, spec, GALLERY_CHIP, make)
        });
        if let Some(i) = picked {
            pick = Some(items[i].map(|p| p.motion));
        }
    }
    if let Some(p) = pick {
        // The preset whole (its easing, timing and stagger), keeping the
        // delay the slot had.
        // An exit takes its kind's exit easing for a preset's default one.
        let delay = slot.map(|m| m.delay_secs);
        *slot = p.map(|mut m| {
            if let Some(d) = delay {
                m.delay_secs = d;
            }
            if dir == Direction::Exit { m.as_exit() } else { m }
        });
        changed = true;
    }
    // Details: delay, easing, stagger, travel.
    if let Some(m) = slot.as_mut() {
        let details_id = ui.id().with(("title_motion_details", dir.label()));
        let mut open: bool = ui.memory(|mem| mem.data.get_temp(details_id)).unwrap_or(false);
        ui.horizontal_wrapped(|ui| {
            ui.add_space(super::rows::RAIL_PAD);
            let arrow = if open { "\u{25be}" } else { "\u{25b8}" };
            if ui.add(egui::Button::new(egui::RichText::new(format!("{arrow} Details")).size(TEXT).color(theme.text_secondary)).frame(false)).clicked() {
                open = !open;
                ui.memory_mut(|mem| mem.data.insert_temp(details_id, open));
            }
        });
        if open {
            rail_row(ui, theme, "Delay", |ui| {
                if ui.add(egui::DragValue::new(&mut m.delay_secs).range(0.0..=10.0).speed(0.02).suffix(" s").fixed_decimals(2)).changed() {
                    changed = true;
                }
            });
            rail_row(ui, theme, "Easing", |ui| {
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
                        // A curve of its own, shaped below (#1511).
                        let custom = matches!(m.easing, Easing::Custom(_));
                        if ui.selectable_label(custom, "Custom\u{2026}").clicked() && !custom {
                            m.easing = Easing::Custom(Default::default());
                            changed = true;
                        }
                    });
            });
            if let Easing::Custom(curve) = &mut m.easing {
                ui.horizontal(|ui| {
                    ui.add_space(super::rows::RAIL_PAD);
                    if crate::widgets::easing_curve::easing_curve(ui, theme, curve).changed() {
                        changed = true;
                    }
                });
            }
            rail_row(ui, theme, "Stagger", |ui| {
                if m.stagger != StaggerUnit::None
                    && ui.add(egui::DragValue::new(&mut m.stagger_secs).range(0.0..=1.0).speed(0.005).suffix(" s").prefix("every ").fixed_decimals(3)).changed()
                {
                    changed = true;
                }
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
            });
            let moves = matches!(m.kind, MotionKind::Rise | MotionKind::Drop | MotionKind::SlideLeft | MotionKind::SlideRight);
            if moves {
                rail_row(ui, theme, "Travel", |ui| {
                    if ui.add(egui::DragValue::new(&mut m.distance).range(0.1..=8.0).speed(0.02).suffix("\u{00d7} font").fixed_decimals(2)).changed() {
                        changed = true;
                    }
                });
            }
        }
    }
    changed
}

/// A motion-graphics template's fields (#1139): each live published field
/// of `td` as one control, its text in a box, its colour in a swatch, its
/// speed on a slider, each change one `SetTitleField` (a slider's drag one
/// step when it ends). A text's misspelt words are underlined in
/// `language` (#3236). Whether there was any field to draw.
pub fn template_fields(ui: &mut egui::Ui, theme: &KlippTheme, clip_id: klipp_core::ClipId, td: &klipp_timeline::TitleData, spell: &crate::spelling::SpellCheck, language: &str, cmds: &mut Vec<AppCommand>) -> bool {
    use klipp_core::TitleFieldValue as V;
    let fields = td.live_fields();
    if fields.is_empty() {
        return false;
    }
    ui.label(egui::RichText::new("TEMPLATE").small().strong().color(theme.text_dim));
    ui.add_space(2.0);
    for (index, field, value) in fields {
        let mut set = |value: V, done: bool| cmds.push(AppCommand::SetTitleField { clip_id, index, value, done });
        match value {
            V::Text(text) => {
                ui.label(field.label.as_str());
                let mut edited = text.clone();
                let mut spell_cmds = Vec::new();
                let (_, respelt) = crate::spelling::show(
                    ui,
                    spell,
                    &mut edited,
                    |e| e.desired_rows(1).desired_width(f32::INFINITY).id_salt(("template_text", clip_id, index)),
                    language,
                    egui::TextStyle::Body.resolve(ui.style()),
                    ui.visuals().text_color(),
                    theme.marker_error,
                    &mut spell_cmds,
                );
                if let Some(respelt) = respelt {
                    edited = respelt;
                }
                if edited != text {
                    set(V::Text(edited), true);
                }
                cmds.extend(spell_cmds);
            }
            V::Colour(c) => {
                ui.horizontal(|ui| {
                    ui.label(field.label.as_str());
                    let mut col = egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
                    if ui.color_edit_button_srgba(&mut col).changed() {
                        set(V::Colour(col.to_srgba_unmultiplied()), true);
                    }
                });
            }
            V::Speed(f) => {
                let mut v = f;
                let r = ui.add(
                    egui::Slider::new(&mut v, klipp_timeline::TITLE_SPEED_RANGE)
                        .logarithmic(true)
                        .text(field.label.as_str())
                        .suffix("\u{00d7}")
                        .fixed_decimals(2),
                );
                // A drag is one step: shown as it goes, recorded when it ends.
                if r.changed() {
                    set(V::Speed(v), !r.dragged());
                } else if r.drag_stopped() {
                    set(V::Speed(v), true);
                }
            }
            V::Size(f) => {
                let mut v = f;
                let r = ui.add(
                    egui::Slider::new(&mut v, klipp_timeline::TITLE_SIZE_RANGE)
                        .text(field.label.as_str())
                        .suffix("\u{00d7}")
                        .fixed_decimals(2),
                );
                // As Speed: shown as it goes, one step when it ends.
                if r.changed() {
                    set(V::Size(v), !r.dragged());
                } else if r.drag_stopped() {
                    set(V::Size(v), true);
                }
            }
            // A family by name (#2757); one Klipp's catalogue has is
            // fetched once it is set.
            V::Font(family) => {
                ui.horizontal(|ui| {
                    ui.label(field.label.as_str());
                    let mut edited = family.clone();
                    let r = ui.add(egui::TextEdit::singleline(&mut edited).desired_width(f32::INFINITY).id_salt(("template_font", clip_id, index)));
                    if r.lost_focus() && edited.trim() != family && !edited.trim().is_empty() {
                        crate::missing_fonts::fetch_family(edited.trim());
                        set(V::Font(edited.trim().to_string()), true);
                    }
                });
            }
            // How long the motion is laid over (#2757), as a drag of
            // seconds: shown as it goes, one step when it ends.
            V::Duration(secs) => {
                ui.horizontal(|ui| {
                    ui.label(field.label.as_str());
                    let mut v = secs;
                    let r = ui.add(egui::DragValue::new(&mut v).range(klipp_timeline::TITLE_DURATION_RANGE).speed(0.05).suffix(" s").fixed_decimals(2));
                    if r.changed() {
                        set(V::Duration(v), !r.dragged());
                    } else if r.drag_stopped() {
                        set(V::Duration(v), true);
                    }
                });
            }
            // An image layer's file (#2757): chosen from disk.
            V::Picture(path) => {
                ui.horizontal(|ui| {
                    ui.label(field.label.as_str());
                    let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(path.clone());
                    ui.label(egui::RichText::new(name).color(theme.text_dim)).on_hover_text(path.as_str());
                    if ui.button("Choose\u{2026}").clicked() {
                        crate::file_dialog::spawn_pick(
                            || rfd::FileDialog::new().set_title("Picture").add_filter("Images", &["png", "jpg", "jpeg", "webp", "tif", "tiff", "exr"]).pick_file(),
                            move |chosen| vec![AppCommand::SetTitleField { clip_id, index, value: V::Picture(chosen.display().to_string()), done: true }],
                        );
                    }
                });
            }
        }
        ui.add_space(4.0);
    }
    true
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
    section_card(ui, "ANIMATION", None, theme, |ui| {
        let mut motion = fx.motion;
        let mut changed = false;
        changed |= motion_row(ui, theme, chips, subject, fx, Direction::Enter, &mut motion.enter);
        ui.add_space(4.0);
        changed |= motion_row(ui, theme, chips, subject, fx, Direction::Exit, &mut motion.exit);
        if changed {
            cmds.push(AppCommand::SetTitleMotion { clip_id, motion });
        }
    });
    show_loops(ui, theme, clip_id, subject, fx, travel, chips, cmds);
    show_edge_fade(ui, theme, clip_id, fx, cmds);
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
    let picker_id = egui::Id::new("title_loop_picker");
    let open = open_picker(ui) == Some(picker_id);
    section_card(ui, "LOOPS", None, theme, |ui| {
        if picker_field(ui, theme, "Add loop\u{2026}", open) {
            set_open_picker(ui, if open { None } else { Some(picker_id) });
        }
        if open {
            let presets = TitleLoop::presets();
            let entries: Vec<GalleryEntry> = presets.iter().map(|(name, l)| GalleryEntry { key: (*name).into(), label: name, group: l.category() }).collect();
            let items: Vec<TitleLoop> = presets.into_iter().map(|(_, l)| l).collect();
            let picked = inline_picker(ui, theme, picker_id, "Loops", &entries, None, |ui, i, big| {
                let l = &items[i];
                let s_c = subject.clone();
                let fx_c = fx.clone();
                let l_c = l.clone();
                let make = move || loop_chip_title(&s_c, &fx_c, &l_c);
                let label = entries[i].label;
                if big {
                    let spec = ChipSpec {
                        id: format!("bigloop:{label}"),
                        subject,
                        fx,
                        rest: 0.35,
                        play: Play::Loop { len: LOOPS_HOVER_LOOP },
                        selected: false,
                        label: None,
                        always: true,
                        render: (BIG_W, BIG_H),
                    };
                    return chip(ui, theme, chips, spec, big_size(ui), make);
                }
                let spec = ChipSpec {
                    id: format!("loop:{label}"),
                    subject,
                    fx,
                    rest: 0.35,
                    play: Play::Still { at: 0.35 },
                    selected: false,
                    label: Some(label),
                    always: false,
                    render: (CHIP_W, CHIP_H),
                };
                chip(ui, theme, chips, spec, GALLERY_CHIP, make)
            });
            if let Some(i) = picked {
                loops.push(items[i].clone());
                changed = true;
            }
        }
        if loops.is_empty() {
            ui.label(egui::RichText::new("None. A loop runs the whole time the title is on screen.").size(TEXT).color(theme.text_secondary));
        }
        for (i, l) in loops.iter_mut().enumerate() {
            ui.horizontal_wrapped(|ui| {
                ui.add_sized(
                    [72.0, crate::theme::control::COMPACT],
                    egui::Label::new(egui::RichText::new(l.label()).size(TEXT).color(theme.text_secondary)),
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
                    TitleLoop::Float { amplitude, rate_hz } | TitleLoop::Sway { amplitude, rate_hz } => {
                        changed |= ui.add(egui::DragValue::new(amplitude).range(0.0..=2.0).speed(0.005).suffix("\u{00d7} font").fixed_decimals(3)).changed();
                        changed |= ui.add(egui::DragValue::new(rate_hz).range(0.02..=5.0).speed(0.01).suffix(" Hz").fixed_decimals(2)).changed();
                    }
                    TitleLoop::Flicker { rate_hz, depth } => {
                        changed |= ui.add(egui::DragValue::new(rate_hz).range(0.5..=30.0).speed(0.05).suffix(" Hz").fixed_decimals(1)).changed();
                        changed |= ui.add(egui::DragValue::new(depth).range(0.0..=1.0).speed(0.01).prefix("dips to ").fixed_decimals(2)).changed();
                    }
                    TitleLoop::Drift { dx, dy } => {
                        let mut x = *dx * 100.0;
                        if ui.add(egui::DragValue::new(&mut x).range(-50.0..=50.0).speed(0.2).prefix("x ").suffix(" %").fixed_decimals(1)).on_hover_text("How far across the frame it drifts over the clip").changed() {
                            *dx = x / 100.0;
                            changed = true;
                        }
                        let mut y = *dy * 100.0;
                        if ui.add(egui::DragValue::new(&mut y).range(-50.0..=50.0).speed(0.2).prefix("y ").suffix(" %").fixed_decimals(1)).on_hover_text("How far down the frame it drifts over the clip").changed() {
                            *dy = y / 100.0;
                            changed = true;
                        }
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
            // The travelling loops' speed on a line of its own, always shown,
            // and how they fade with where they are on screen.
            match l {
                TitleLoop::Scroll { speed_px_per_sec, shape, .. } => {
                    changed |= roll_speed(ui, theme, speed_px_per_sec, travel, true, false);
                    changed |= roll_shape(ui, theme, shape);
                }
                TitleLoop::Crawl { speed_px_per_sec, repeat, shape, .. } => {
                    changed |= roll_speed(ui, theme, speed_px_per_sec, travel, false, *repeat);
                    changed |= roll_shape(ui, theme, shape);
                }
                _ => {}
            }
        }
        if let Some(i) = remove {
            loops.remove(i);
            changed = true;
        }
    });
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
fn roll_speed(ui: &mut egui::Ui, theme: &KlippTheme, speed: &mut f32, travel: Travel, vertical: bool, repeat: bool) -> bool {
    let mut changed = false;
    let distance = travel.distance(vertical);
    let clip = travel.clip_secs;
    let fitted = distance / clip;
    let dim = |t: &str| egui::RichText::new(t).size(TEXT).color(theme.text_secondary);
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
    // A repeating crawl starts again as it leaves, from the first
    // frame: it is neither centred in the clip nor cut off by it.
    if *speed > 0.0 && !repeat {
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

/// A roll's or a crawl's focus band (#592): full strength in a band
/// around the frame's middle, thinning towards its edges. Returns true
/// when it changed.
fn roll_shape(ui: &mut egui::Ui, theme: &KlippTheme, shape: &mut klipp_core::title_motion::RollShape) -> bool {
    let mut changed = false;
    let dim = |t: &str| egui::RichText::new(t).size(TEXT).color(theme.text_secondary);
    ui.horizontal_wrapped(|ui| {
        ui.add_space(4.0);
        let mut on = shape.focus.is_some();
        if ui
            .checkbox(&mut on, "Focus band")
            .on_hover_text("Full strength in a band around the middle of the frame, thinning towards its edges")
            .changed()
        {
            shape.focus = on.then(klipp_core::title_motion::FocusBand::default);
            changed = true;
        }
        if let Some(focus) = &mut shape.focus {
            let mut band = focus.band * 100.0;
            if ui
                .add(egui::DragValue::new(&mut band).range(0.0..=99.0).speed(0.5).suffix(" %").fixed_decimals(0))
                .on_hover_text("How much of the frame, around its middle, is at full strength")
                .changed()
            {
                focus.band = band / 100.0;
                changed = true;
            }
            ui.label(dim("edges at"));
            let mut edge = focus.edge_opacity * 100.0;
            if ui
                .add(egui::DragValue::new(&mut edge).range(0.0..=100.0).speed(0.5).suffix(" %").fixed_decimals(0))
                .on_hover_text("How much of the layer shows at the frame's edges")
                .changed()
            {
                focus.edge_opacity = edge / 100.0;
                changed = true;
            }
            ui.label(dim("blur"));
            if ui
                .add(egui::DragValue::new(&mut focus.blur).range(0.0..=60.0).speed(0.2).suffix(" px").fixed_decimals(1))
                .on_hover_text("How soft it goes towards the frame's edges, in pixels of a 1080p frame; 0 for sharp")
                .changed()
            {
                changed = true;
            }
        }
    });
    changed
}

/// The layer's edge fade (#592): how much of the frame at each edge it
/// fades out over, by where it is on screen, standing still or moving.
fn show_edge_fade(ui: &mut egui::Ui, theme: &KlippTheme, clip_id: klipp_core::ClipId, fx: &LayerFx, cmds: &mut Vec<AppCommand>) {
    let mut fade = fx.edge_fade;
    let mut changed = false;
    section_card(ui, "EDGE FADE", None, theme, |ui| {
        for (label, zone, tip) in [
            ("Top", &mut fade.top, "Fade out over this much of the frame at its top edge"),
            ("Bottom", &mut fade.bottom, "Fade out over this much of the frame at its bottom edge"),
            ("Left", &mut fade.left, "Fade out over this much of the frame at its left edge"),
            ("Right", &mut fade.right, "Fade out over this much of the frame at its right edge"),
        ] {
            rail_row(ui, theme, label, |ui| {
                let mut pct = *zone * 100.0;
                if ui
                    .add(egui::DragValue::new(&mut pct).range(0.0..=50.0).speed(0.5).suffix(" %").fixed_decimals(0))
                    .on_hover_text(format!("{tip}; 0 for none"))
                    .changed()
                {
                    *zone = pct / 100.0;
                    changed = true;
                }
            });
        }
    });
    if changed {
        cmds.push(AppCommand::SetTitleEdgeFade { clip_id, edge_fade: fade });
    }
}

// ─── Style card ──────────────────────────────────────────────────────

fn to_c32(c: [u8; 4]) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

/// The style card: presets as chips, then fill, gradient, stroke, shadow
/// and glow as one row each, its switch at the right. An effect that is
/// off and was never set shows only its switch: the values it would
/// start from are not the layer's. The full stacks live under Advanced.
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
    section_card(ui, "APPEARANCE", None, theme, |ui| {
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
                    ChipSpec {
                        id: format!("preset:{pid}"),
                        subject: &subject,
                        fx,
                        rest: 1.0,
                        play: Play::Still { at: 1.0 },
                        selected: false,
                        label: Some(label),
                        always: false,
                        render: (CHIP_W, CHIP_H),
                    },
                    ROW_CHIP,
                    make,
                );
                if resp.clicked() {
                    cmds.push(AppCommand::ApplyTitlePreset { clip_id, preset_id: pid });
                }
            }
        });
        ui.add_space(6.0);
        style_rows(ui, theme, clip_id, tb, fx, cmds);
    });
}

/// Fill, gradient, stroke, shadow and glow, one row each. Right to left,
/// so the switch is added first and each row's values after it in
/// reverse.
fn style_rows(ui: &mut egui::Ui, theme: &KlippTheme, clip_id: klipp_core::ClipId, tb: &TextBox, fx: &LayerFx, cmds: &mut Vec<AppCommand>) {
    rail_row(ui, theme, "Fill", |ui| {
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
    });
    rail_row(ui, theme, "Gradient", |ui| {
        let g = tb.gradient.as_ref();
        let mut on = g.is_some_and(|g| g.enabled);
        let (mut top, mut bottom) = g.map(|g| (g.top_color, g.bottom_color)).unwrap_or((tb.color, [tb.color[0] / 2, tb.color[1] / 2, tb.color[2] / 2, tb.color[3]]));
        let mut send = false;
        if switch(ui, theme, on).clicked() {
            on = !on;
            send = true;
        }
        if on {
            let mut b = to_c32(bottom);
            if ui.color_edit_button_srgba(&mut b).on_hover_text("Bottom").changed() {
                bottom = b.to_array();
                send = true;
            }
            let mut t = to_c32(top);
            if ui.color_edit_button_srgba(&mut t).on_hover_text("Top").changed() {
                top = t.to_array();
                send = true;
            }
        }
        if send {
            cmds.push(AppCommand::SetTitleGradient { clip_id, enabled: on, top_color: top, bottom_color: bottom });
        }
    });
    rail_row(ui, theme, "Stroke", |ui| {
        let o = fx.outlines.first();
        let mut on = o.is_some_and(|o| o.enabled);
        let (mut color, mut width) = o.map(|o| (o.color, o.width)).unwrap_or(([0, 0, 0, 255], 3.0));
        let mut send = false;
        if switch(ui, theme, on).clicked() {
            on = !on;
            send = true;
        }
        if on || o.is_some() {
            if ui.add(egui::DragValue::new(&mut width).range(0.5..=40.0).speed(0.1).suffix(" px").fixed_decimals(1)).changed() {
                send = true;
                on = true;
            }
            let mut c = to_c32(color);
            if ui.color_edit_button_srgba(&mut c).changed() {
                color = c.to_array();
                send = true;
                on = true;
            }
        }
        if send {
            cmds.push(AppCommand::SetTitleOutline { clip_id, enabled: on, color, width });
        }
    });
    rail_row(ui, theme, "Shadow", |ui| {
        let s = fx.shadows.first();
        let mut on = s.is_some_and(|s| s.enabled);
        let (mut color, mut ox, mut oy, mut blur) = s.map(|s| (s.color, s.offset_x, s.offset_y, s.blur)).unwrap_or(([0, 0, 0, 200], 4.0, 4.0, 6.0));
        let mut send = false;
        if switch(ui, theme, on).clicked() {
            on = !on;
            send = true;
        }
        if on || s.is_some() {
            if ui.add(egui::DragValue::new(&mut blur).range(0.0..=60.0).speed(0.2).prefix("blur ").fixed_decimals(0)).changed() {
                send = true;
                on = true;
            }
            if ui.add(egui::DragValue::new(&mut oy).range(-100.0..=100.0).speed(0.2).prefix("y ").fixed_decimals(0)).changed() {
                send = true;
                on = true;
            }
            if ui.add(egui::DragValue::new(&mut ox).range(-100.0..=100.0).speed(0.2).prefix("x ").fixed_decimals(0)).changed() {
                send = true;
                on = true;
            }
            let mut c = to_c32(color);
            if ui.color_edit_button_srgba(&mut c).changed() {
                color = c.to_array();
                send = true;
                on = true;
            }
        }
        if send {
            cmds.push(AppCommand::SetTitleShadow { clip_id, enabled: on, color, offset_x: ox, offset_y: oy, blur });
        }
    });
    rail_row(ui, theme, "Glow", |ui| {
        let g = fx.glows.first();
        let mut on = g.is_some_and(|g| g.enabled);
        let (mut color, mut radius, mut intensity) = g.map(|g| (g.color, g.radius, g.intensity)).unwrap_or((tb.color, 12.0, 1.0));
        let mut send = false;
        if switch(ui, theme, on).clicked() {
            on = !on;
            send = true;
        }
        if on || g.is_some() {
            if ui.add(egui::DragValue::new(&mut intensity).range(0.1..=3.0).speed(0.02).prefix("\u{00d7}").fixed_decimals(2)).changed() {
                send = true;
                on = true;
            }
            if ui.add(egui::DragValue::new(&mut radius).range(1.0..=100.0).speed(0.2).prefix("radius ").fixed_decimals(0)).changed() {
                send = true;
                on = true;
            }
            let mut c = to_c32(color);
            if ui.color_edit_button_srgba(&mut c).changed() {
                color = c.to_array();
                send = true;
                on = true;
            }
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
        let shape = TitleObjectContent::Shape(klipp_timeline::Shape { kind: klipp_timeline::ShapeKind::Rectangle { corner_radius: 4.0 }, fill: Some([1, 2, 3, 255]), gradient: None, stroke: None, fit: None });
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
        let loops = [TitleLoop::Scroll { direction: ScrollDirection::Up, speed_px_per_sec: speed, shape: Default::default() }];
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

#[cfg(test)]
mod play_tests {
    use super::*;

    /// Every piece of `text` at `t` into the chip title, entering by `m`.
    fn pieces(m: Motion, text: &str, t: f32, dir: Direction) -> Vec<f32> {
        let motion = match dir {
            Direction::Enter => TitleMotion { enter: Some(m), exit: None },
            Direction::Exit => TitleMotion { enter: None, exit: Some(m) },
        };
        let n = match m.stagger {
            StaggerUnit::None => 1,
            StaggerUnit::Char => text.chars().filter(|c| !c.is_whitespace()).count(),
            StaggerUnit::Word => text.split_whitespace().count(),
            StaggerUnit::Line => text.lines().count(),
        };
        (0..n)
            .map(|u| {
                let (a, b) = motion.progress(t, Some(WINDOW), u);
                a.min(b)
            })
            .collect()
    }

    /// The preview plays an effect whole (#608): nothing at first, every
    /// piece, the last of a stagger too, at rest after it, and for an
    /// exit the text at rest first and gone at the end.
    #[test]
    fn a_preview_plays_the_whole_effect_with_a_moment_either_side() {
        let text = "Coming soon";
        for m in [
            Motion::new(MotionKind::Drop).duration(0.6),
            Motion::new(MotionKind::Fade).duration(0.4).delay(0.3),
            Motion::new(MotionKind::Rise).duration(0.5).stagger(StaggerUnit::Char, 0.06),
        ] {
            let span = motion_span(&m, text);
            let enter = Direction::Enter.play(Some(m), text);
            assert!(pieces(m, text, enter.time(0.0), Direction::Enter).iter().all(|&p| p <= 0.0), "{m:?}: nothing yet");
            let settled = enter.time(PLAY_EMPTY + span + FRAME_STEP);
            assert!(pieces(m, text, settled, Direction::Enter).iter().all(|&p| p >= 1.0), "{m:?}: every piece at rest");
            let half = enter.time(PLAY_EMPTY + span * 0.5);
            assert!(pieces(m, text, half, Direction::Enter).iter().any(|&p| p > 0.0 && p < 1.0), "{m:?}: moving half way");

            let exit = Direction::Exit.play(Some(m), text);
            assert!(pieces(m, text, exit.time(0.0), Direction::Exit).iter().all(|&p| p >= 1.0), "{m:?}: at rest first");
            let gone = exit.time(PLAY_REST + span + FRAME_STEP);
            assert!(pieces(m, text, gone, Direction::Exit).iter().all(|&p| p <= 0.0), "{m:?}: gone after");
            // And it starts over.
            assert_eq!(exit.time(exit.len() + 0.001), exit.time(0.0));
        }
    }

    /// A stagger's span reaches its last piece, not its first.
    #[test]
    fn a_staggered_span_covers_every_piece() {
        let m = Motion::new(MotionKind::Rise).duration(0.5).stagger(StaggerUnit::Char, 0.1);
        assert!((motion_span(&m, "ab cd") - (0.5 + 3.0 * 0.1)).abs() < 1e-5);
        let w = m.stagger(StaggerUnit::Word, 0.2);
        assert!((motion_span(&w, "ab cd") - (0.5 + 0.2)).abs() < 1e-5);
    }
}

#[cfg(test)]
mod gallery_tests {
    use super::{gallery_body, GalleryEntry};
    use crate::theme::KlippTheme;

    fn entries() -> Vec<GalleryEntry> {
        let e = |key: &str, label: &'static str, group: &'static str| GalleryEntry { key: key.into(), label, group };
        vec![
            e("none", "None", ""),
            e("fade", "Fade", "Fading"),
            e("rise", "Rise", "Move"),
            e("slide_l", "Slide from left", "Move"),
            e("slide_r", "Slide from right", "Move"),
            e("up", "Scale up", "Scale"),
        ]
    }

    /// One frame of the picker; the labels it drew with where they are,
    /// and the entry picked, if any.
    fn frame(ctx: &egui::Context, events: Vec<egui::Event>) -> (Vec<(String, egui::Pos2)>, Option<usize>) {
        let list = entries();
        let mut picked = None;
        let out = ctx.run(
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 800.0))), events, ..Default::default() },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    picked = gallery_body(ui, &KlippTheme::default(), egui::Id::new("test_gallery"), "test", &list, |ui, i, _| ui.button(list[i].label));
                });
            },
        );
        let texts = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
                _ => None,
            })
            .collect();
        (texts, picked)
    }

    fn shown(texts: &[(String, egui::Pos2)]) -> Vec<&str> {
        let names = ["None", "Fade", "Rise", "Slide from left", "Slide from right", "Scale up"];
        names.into_iter().filter(|n| texts.iter().any(|(t, _)| t == n)).collect()
    }

    fn click(ctx: &egui::Context, texts: &[(String, egui::Pos2)], label: &str) -> Option<usize> {
        let at = texts.iter().find(|(t, _)| t == label).map(|(_, p)| *p).unwrap_or_else(|| panic!("{label} is drawn"));
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        frame(ctx, vec![egui::Event::PointerMoved(at), button(true)]);
        frame(ctx, vec![button(false)]).1
    }

    /// #604: the picker opens inside the inspector, one at a time; it
    /// closes when an effect is picked, and on Esc.
    #[test]
    fn the_inline_picker_closes_on_a_pick_and_on_escape() {
        use super::{inline_picker, open_picker};
        let ctx = egui::Context::default();
        let id = egui::Id::new("inline_test");
        let list = entries();
        let run = |events: Vec<egui::Event>| {
            let mut picked = None;
            let out = ctx.run(
                egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 800.0))), events, ..Default::default() },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        if open_picker(ui) == Some(id) {
                            picked = inline_picker(ui, &KlippTheme::default(), id, "inline", &list, None, |ui, i, big| {
                                if big {
                                    ui.allocate_exact_size(super::big_size(ui), egui::Sense::click()).1
                                } else {
                                    ui.button(list[i].label)
                                }
                            });
                        }
                    });
                },
            );
            let texts: Vec<(String, egui::Pos2)> = out
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
                    _ => None,
                })
                .collect();
            (texts, picked)
        };
        let is_open = || ctx.memory(|m| m.data.get_temp::<Option<egui::Id>>(egui::Id::new("title_open_picker"))).flatten() == Some(id);
        ctx.memory_mut(|m| m.data.insert_temp(egui::Id::new("title_open_picker"), Some(id)));
        let (texts, _) = run(Vec::new());
        assert!(texts.iter().any(|(t, _)| t == "Rise"), "open, it shows the effects");
        let at = texts.iter().find(|(t, _)| t == "Rise").unwrap().1;
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        run(vec![egui::Event::PointerMoved(at), button(true)]);
        let (_, picked) = run(vec![button(false)]);
        assert_eq!(picked, Some(2));
        assert!(!is_open(), "a pick closes it");

        ctx.memory_mut(|m| m.data.insert_temp(egui::Id::new("title_open_picker"), Some(id)));
        run(Vec::new());
        run(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
        assert!(!is_open(), "Esc closes it");
    }

    /// #604: the whole path, as the user clicks it. The Entrance field
    /// opens the picker and it stays open frame after frame (the popup of
    /// #601 closed on the click that opened it: it blinked); picking Pop
    /// sets the slot to it and closes the picker.
    #[test]
    fn the_entrance_field_opens_a_picker_that_stays_and_picks() {
        use super::{motion_row, ChipSubject, Direction, TitleChips};
        let ctx = egui::Context::default();
        let mut chips = TitleChips::default();
        let subject = ChipSubject::Text(klipp_timeline::TextBox { text: "Credits".into(), ..Default::default() });
        let fx = klipp_timeline::LayerFx::default();
        let mut slot: Option<klipp_timeline::Motion> = None;
        let mut run = |events: Vec<egui::Event>, slot: &mut Option<klipp_timeline::Motion>| -> Vec<(String, egui::Pos2)> {
            let out = ctx.run(
                egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 2000.0))), events, ..Default::default() },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        motion_row(ui, &KlippTheme::default(), &mut chips, &subject, &fx, Direction::Enter, slot);
                    });
                },
            );
            out.shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
                    _ => None,
                })
                .collect()
        };
        let find = |texts: &[(String, egui::Pos2)], prefix: &str| texts.iter().find(|(t, _)| t.starts_with(prefix)).map(|(_, p)| *p);
        let texts = run(Vec::new(), &mut slot);
        let field = find(&texts, "Choose entrance").expect("the field says what it is for");
        assert!(find(&texts, "Pop").is_none(), "closed at first");
        let click = |at: egui::Pos2, pressed: bool| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        run(vec![egui::Event::PointerMoved(field), click(field, true)], &mut slot);
        run(vec![click(field, false)], &mut slot);
        for _ in 0..4 {
            let texts = run(Vec::new(), &mut slot);
            assert!(find(&texts, "Fade").is_some() && find(&texts, "Search").is_some(), "the picker stays open");
        }
        // The search field has the keyboard as the picker opens: typing
        // narrows it to Pop, which was further down than the view shows.
        run(vec![egui::Event::Text("pop".into())], &mut slot);
        let texts = run(Vec::new(), &mut slot);
        let pop = texts.iter().find(|(t, _)| t == "Pop").map(|(_, p)| *p).expect("Pop is shown when searched for");
        run(vec![egui::Event::PointerMoved(pop), click(pop, true)], &mut slot);
        run(vec![click(pop, false)], &mut slot);
        assert_eq!(slot.map(|m| m.kind), Some(klipp_timeline::MotionKind::Pop), "picking Pop sets it");
        let texts = run(Vec::new(), &mut slot);
        assert!(find(&texts, "Pop letters").is_none(), "and closes the picker");
        let field = find(&texts, "Pop").expect("the field now names it");

        // Opened, closed by its own field, opened again: it takes the
        // keyboard again, so typing still searches (#605 review).
        for _ in 0..3 {
            run(vec![egui::Event::PointerMoved(field), click(field, true)], &mut slot);
            run(vec![click(field, false)], &mut slot);
            run(Vec::new(), &mut slot);
        }
        run(vec![egui::Event::Text("slam".into())], &mut slot);
        let texts = run(Vec::new(), &mut slot);
        assert!(find(&texts, "Slam").is_some() && find(&texts, "Fly from left").is_none(), "reopened, typing searches again");
    }

    /// #596: search narrows the picker by name or group, a group's pill
    /// narrows it to the group, and what is picked comes back first under
    /// Recent. None sits under All only.
    #[test]
    fn the_picker_searches_groups_and_remembers() {
        let ctx = egui::Context::default();
        let (texts, _) = frame(&ctx, Vec::new());
        assert_eq!(shown(&texts), ["None", "Fade", "Rise", "Slide from left", "Slide from right", "Scale up"]);
        assert!(!texts.iter().any(|(t, _)| t == "Recent"), "no Recent before a pick");

        ctx.memory_mut(|m| m.data.insert_temp(egui::Id::new("test_gallery").with("search"), "slide".to_string()));
        let (texts, _) = frame(&ctx, Vec::new());
        assert_eq!(shown(&texts), ["Slide from left", "Slide from right"], "search by name");
        ctx.memory_mut(|m| m.data.insert_temp(egui::Id::new("test_gallery").with("search"), "scale".to_string()));
        let (texts, _) = frame(&ctx, Vec::new());
        assert_eq!(shown(&texts), ["Scale up"], "and by group");
        ctx.memory_mut(|m| m.data.insert_temp(egui::Id::new("test_gallery").with("search"), String::new()));

        let (texts, _) = frame(&ctx, Vec::new());
        assert_eq!(click(&ctx, &texts, "Move"), None, "a pill is not a pick");
        let (texts, _) = frame(&ctx, Vec::new());
        assert_eq!(shown(&texts), ["Rise", "Slide from left", "Slide from right"], "the group, without None");

        assert_eq!(click(&ctx, &texts, "Rise"), Some(2));
        let (texts, _) = frame(&ctx, Vec::new());
        assert_eq!(click(&ctx, &texts, "Recent"), None);
        let (texts, _) = frame(&ctx, Vec::new());
        assert_eq!(shown(&texts), ["Rise"], "Recent holds the pick");
    }
}

#[cfg(test)]
mod template_field_tests {
    use super::*;
    use klipp_core::TitleFieldValue as V;
    use klipp_core::ClipId;
    use klipp_timeline::{ClipKind, TextBox, TitleData};

    /// A state holding one title, a template's: its text, colour and (it
    /// rises in) speed published.
    fn state() -> (crate::state::AppState, ClipId) {
        let mut td = TitleData::single_text(TextBox { text: "Jane Doe".into(), ..Default::default() });
        td.objects[0].fx_mut().motion.enter = Some(klipp_core::title_motion::Motion::new(klipp_core::title_motion::MotionKind::Rise).duration(0.6));
        td.published = td.default_fields();
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        let mut clip = klipp_timeline::Clip::gap(t(0.0), t(4.0));
        clip.kind = ClipKind::Title(td);
        let id = clip.id;
        let mut state = crate::state::AppState::default();
        state.sequences[0].tracks[0].clips.push(clip);
        (state, id)
    }

    fn title(state: &crate::state::AppState, id: ClipId) -> TitleData {
        state.sequences[0].all_clips().find(|c| c.id == id).unwrap().title_data().unwrap().clone()
    }

    fn undo(state: &mut crate::state::AppState, id: ClipId) {
        let entry = state.active_history_mut().undo().expect("a step");
        let crate::history::EditCommand::EditTitle { old_data, .. } = entry.command else { panic!("a title step") };
        let clip = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(id)).unwrap();
        clip.kind = ClipKind::Title(old_data);
    }

    /// Each field's change is one undo step (#1139): a speed drag's steps
    /// are one, recorded when it ends; colours picked close together on
    /// one field are one; undo puts the title back as it was. A field
    /// whose layer is gone is refused with nothing changed.
    #[test]
    fn a_field_change_is_one_step() {
        let (mut s, id) = state();
        let made = title(&s, id);
        for f in [1.5, 2.0, 2.5] {
            s.set_title_field(id, 2, &V::Speed(f), false).unwrap();
        }
        assert_eq!(s.active_history().undo_count(), 0, "a drag under way records nothing");
        s.set_title_field(id, 2, &V::Speed(3.0), true).unwrap();
        assert_eq!(s.active_history().undo_count(), 1, "the drag is one step");
        assert_eq!(title(&s, id).field_value(2), Some(V::Speed(3.0)));
        undo(&mut s, id);
        assert_eq!(title(&s, id), made, "undo: the template as it was");

        for c in [[255, 0, 0, 255], [255, 100, 0, 255], [255, 200, 0, 255]] {
            s.set_title_field(id, 1, &V::Colour(c), true).unwrap();
        }
        assert_eq!(s.active_history().undo_count(), 1, "colours picked together are one step");
        s.set_title_field(id, 0, &V::Text("John Roe".into()), true).unwrap();
        assert_eq!(s.active_history().undo_count(), 2);
        undo(&mut s, id);
        undo(&mut s, id);
        assert_eq!(title(&s, id), made);

        let clip = s.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(id)).unwrap();
        let ClipKind::Title(td) = &mut clip.kind else { unreachable!() };
        td.objects.clear();
        let gone = title(&s, id);
        assert!(s.set_title_field(id, 0, &V::Text("x".into()), true).is_err());
        assert_eq!(title(&s, id), gone, "nothing changed");
    }

    /// The Template section draws each live field, and a speed drag sends
    /// its steps as under way and its end as done.
    #[test]
    fn the_template_section_draws_its_fields_and_a_drag_ends_done() {
        let (s, id) = state();
        let td = title(&s, id);
        let ctx = egui::Context::default();
        let theme = KlippTheme::default();
        let mut clock = 0.0;
        let mut frame = |events: Vec<egui::Event>| {
            clock += 0.1;
            let mut cmds = Vec::new();
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 600.0))), time: Some(clock), events, ..Default::default() };
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| assert!(template_fields(ui, &theme, id, &td, &crate::spelling::SpellCheck::default(), "en", &mut cmds)));
            });
            let texts: Vec<(String, egui::Rect)> = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.visual_bounding_rect())),
                _ => None,
            }).collect();
            (cmds, texts)
        };
        frame(Vec::new());
        let (_, texts) = frame(Vec::new());
        for want in ["TEMPLATE", "Text 1", "Text 1 colour", "Speed"] {
            assert!(texts.iter().any(|(t, _)| t == want), "{want}: {texts:?}");
        }
        // The slider's rail lies left of its label.
        let speed = texts.iter().find(|(t, _)| t == "Speed").unwrap().1;
        let at = egui::pos2(speed.left() - 60.0, speed.center().y);
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut sent = Vec::new();
        sent.extend(frame(vec![egui::Event::PointerMoved(at), button(at, true)]).0);
        for dx in [10.0, 20.0, 30.0] {
            sent.extend(frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))]).0);
        }
        sent.extend(frame(vec![button(at + egui::vec2(30.0, 0.0), false)]).0);
        let dones: Vec<bool> = sent.iter().filter_map(|c| match c { AppCommand::SetTitleField { index: 2, done, .. } => Some(*done), _ => None }).collect();
        assert!(dones.len() >= 2, "steps sent: {sent:?}");
        assert!(dones[..dones.len() - 1].iter().all(|d| !d), "under way: {dones:?}");
        assert_eq!(dones.last(), Some(&true), "the end is done: {dones:?}");
    }
}

#[cfg(test)]
mod row_style_tests {
    use super::*;
    use crate::inspector::rows::test_frame::{click, frame};

    fn text() -> TextBox {
        TextBox { text: "Title text".into(), font_name: "Inter".into(), ..Default::default() }
    }

    /// The motion card (with its loops and edge fade), the letter
    /// animators and the style card, as the Title tab draws them.
    fn draw(ui: &mut egui::Ui, theme: &KlippTheme, cmds: &mut Vec<AppCommand>, fx: &LayerFx, chips: &mut TitleChips) {
        let clip_id = klipp_core::ClipId(uuid::Uuid::nil());
        let tb = text();
        let travel = Travel::new(klipp_core::Resolution { width: 1920, height: 1080 }, (400.0, 100.0), 2.0);
        show_motion_card(ui, theme, clip_id, &ChipSubject::Text(tb.clone()), fx, travel, chips, cmds);
        super::super::title_animators::show_animators(ui, theme, clip_id, None, fx, 2.0, 0.0, 1.0 / 48.0, cmds);
        show_style_card(ui, theme, clip_id, &tb, fx, chips, cmds);
    }

    fn texts(fx: &LayerFx) -> Vec<(String, egui::Pos2)> {
        let ctx = egui::Context::default();
        let mut chips = TitleChips::default();
        frame(&ctx, Vec::new(), |ui, theme, cmds| draw(ui, theme, cmds, fx, &mut chips)).0
    }

    /// Every heading and row the cards had is drawn, each edge-fade row
    /// on a line of its own (they were drawn over one another), and an
    /// effect that is off and never set shows no values it does not have.
    #[test]
    fn every_row_is_kept_and_nothing_is_made_up() {
        let fx = LayerFx::default();
        let t = texts(&fx);
        let all: Vec<&str> = t.iter().map(|(s, _)| s.as_str()).collect();
        for label in [
            "ANIMATION", "Entrance", "Exit", "LOOPS", "EDGE FADE", "Top", "Bottom", "Left", "Right", "ANIMATE LETTERS", "Wave",
            "APPEARANCE", "Neon", "Fill", "Gradient", "Stroke", "Shadow", "Glow",
        ] {
            assert!(all.contains(&label), "{label} is no longer shown: {all:?}");
        }
        let y = |l: &str| t.iter().find(|(s, _)| s == l).unwrap().1.y;
        let ys = [y("Top"), y("Bottom"), y("Left"), y("Right")];
        assert!(ys.windows(2).all(|w| w[1] > w[0] + 20.0), "one edge per row: {ys:?}");
        for made_up in ["3.0 px", "x 4", "y 4", "blur 6", "radius 12", "\u{00d7}1.00"] {
            assert!(!all.contains(&made_up), "{made_up} is not the layer's: {all:?}");
        }

        // A stroke the layer has shows its own values.
        let mut fx = LayerFx::default();
        fx.outlines.push(serde_json::from_value(serde_json::json!({ "color": [255, 0, 0, 255], "width": 5.5 })).unwrap());
        let all: Vec<String> = texts(&fx).into_iter().map(|(s, _)| s).collect();
        assert!(all.iter().any(|s| s == "5.5 px"), "{all:?}");
    }

    /// No rule between rows: the only hairlines are the section headers'.
    #[test]
    fn no_rule_between_rows() {
        let ctx = egui::Context::default();
        let fx = LayerFx::default();
        let mut chips = TitleChips::default();
        let theme = KlippTheme::default();
        let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 6000.0))), ..Default::default() }, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| draw(ui, &theme, &mut Vec::new(), &fx, &mut chips));
        });
        let rules = out.shapes.iter().filter(|s| matches!(&s.shape, egui::Shape::LineSegment { points, .. } if points[0].y == points[1].y)).count();
        assert_eq!(rules, 5, "ANIMATION, LOOPS, EDGE FADE, ANIMATE LETTERS and APPEARANCE headers, and none between rows");
    }

    /// The switches answer real clicks: Stroke on sends the outline it
    /// starts from; an existing shadow off keeps its values.
    #[test]
    fn the_switches_do_what_they_say() {
        let ctx = egui::Context::default();
        let fx = LayerFx::default();
        let mut chips = TitleChips::default();
        let mut d = |ui: &mut egui::Ui, theme: &KlippTheme, cmds: &mut Vec<AppCommand>| draw(ui, theme, cmds, &fx, &mut chips);
        let (t, _) = frame(&ctx, Vec::new(), &mut d);
        let at = |l: &str| egui::pos2(360.0 - 8.0 - super::super::rows::RAIL_PAD - 16.0, t.iter().find(|(s, _)| s == l).unwrap().1.y);
        let cmds = click(&ctx, at("Stroke"), &mut d);
        assert!(matches!(cmds.as_slice(), [AppCommand::SetTitleOutline { enabled: true, width, .. }] if *width == 3.0), "{cmds:?}");
        let cmds = click(&ctx, at("Gradient"), &mut d);
        assert!(matches!(cmds.as_slice(), [AppCommand::SetTitleGradient { enabled: true, .. }]), "{cmds:?}");
    }
}
