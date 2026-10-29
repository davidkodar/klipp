//! "Animate letters" (#1510): a text layer's animators in the Title
//! tab. Three presets add one in a click; each shows as a line saying
//! what it does, its full controls folded under Details. The range's
//! start, end and offset key through the title layer keyframe commands.

use klipp_core::title_motion::{Animator, Easing, RangeSelector, SelectOrder, SelectShape, SelectUnit};
use klipp_core::{AppCommand, ClipId, Keyframe};
use klipp_timeline::{LayerFx, TitleObjectId};

use crate::theme::KlippTheme;

/// The presets, for a clip `clip_secs` long, as klipp-core names them.
pub fn presets(clip_secs: f64) -> Vec<Animator> {
    klipp_core::title_motion::animator_preset_ids().into_iter().filter_map(|n| klipp_core::title_motion::animator_preset(n, clip_secs)).collect()
}

fn unit_label(u: SelectUnit) -> &'static str {
    match u {
        SelectUnit::Char => "Letters",
        SelectUnit::Word => "Words",
        SelectUnit::Line => "Lines",
    }
}

fn shape_label(s: SelectShape) -> &'static str {
    match s {
        SelectShape::Square => "Square",
        SelectShape::RampUp => "Ramp up",
        SelectShape::RampDown => "Ramp down",
        SelectShape::Triangle => "Triangle",
        SelectShape::Round => "Round",
        SelectShape::Smooth => "Smooth",
    }
}

fn order_label(o: SelectOrder) -> &'static str {
    match o {
        SelectOrder::Forward => "In order",
        SelectOrder::Reverse => "Reversed",
        SelectOrder::FromCentre => "From the centre",
        SelectOrder::Random { .. } => "Random",
    }
}

/// One line saying what an animator does: its name, what it picks and
/// what it does to them.
pub fn summary(a: &Animator) -> String {
    let s = &a.selector;
    let keyed = !(s.start_keyframes.is_empty() && s.end_keyframes.is_empty() && s.offset_keyframes.is_empty());
    let mut does = Vec::new();
    let p = &a.props;
    if p.dx != 0.0 || p.dy != 0.0 {
        does.push("move".to_string());
    }
    if p.scale != 1.0 {
        does.push(format!("scale {:.0}%", p.scale * 100.0));
    }
    if p.rotation != 0.0 {
        does.push(format!("turn {:.0}\u{00b0}", p.rotation));
    }
    if p.opacity != 1.0 {
        does.push(if p.opacity == 0.0 { "fade".into() } else { format!("opacity {:.0}%", p.opacity * 100.0) });
    }
    if p.blur != 0.0 {
        does.push("blur".into());
    }
    if p.colour.is_some() {
        does.push("colour".into());
    }
    let name = if a.name.is_empty() { "Animator" } else { &a.name };
    let what = if does.is_empty() { "nothing yet".to_string() } else { does.join(", ") };
    let moving = match (keyed, &a.within) {
        (_, Some(w)) => format!("{}, within {} {:.0}\u{2013}{:.0}%", if keyed { ", animated" } else { "" }, unit_label(w.unit).to_lowercase(), w.start * 100.0, w.end * 100.0),
        (true, None) => ", animated".to_string(),
        (false, None) => String::new(),
    };
    format!("{name} \u{00b7} {} {:.0}\u{2013}{:.0}%{moving} \u{00b7} {what}", unit_label(s.unit).to_lowercase(), s.start * 100.0, s.end * 100.0)
}

/// What the section asks for this frame, besides its animators edited
/// in place: a key added or taken at the playhead, by its param key.
struct KeyRequest {
    param_key: String,
    value: f32,
    edit: super::TextKeyEdit,
    keyframes: Vec<Keyframe>,
}

/// The section: presets to add from, then a line per animator with its
/// controls folded under Details. Edits of the animators themselves go
/// out as one `SetTitleAnimators`; a range that is keyed (or a diamond
/// clicked) goes out as the title layer keyframe commands.
#[allow(clippy::too_many_arguments)]
pub fn show_animators(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: ClipId,
    target: Option<TitleObjectId>,
    fx: &LayerFx,
    clip_secs: f64,
    clip_time_secs: f64,
    keyframe_tolerance: f64,
    cmds: &mut Vec<AppCommand>,
) {
    let mut animators = fx.animators.clone();
    let mut changed = false;
    let mut remove = None;
    let mut keys: Vec<KeyRequest> = Vec::new();
    crate::widgets::inspector_group::section_card(ui, "ANIMATE LETTERS", None, theme, |ui| {
        ui.horizontal_wrapped(|ui| {
            for preset in presets(clip_secs) {
                if ui.button(&preset.name).on_hover_text(summary(&preset)).clicked() {
                    animators.push(preset);
                    changed = true;
                }
            }
        });
        for (i, a) in animators.iter_mut().enumerate() {
            ui.add_space(4.0);
            // Wrapped, so a long line never widens the inspector past its
            // rows' diamonds.
            ui.horizontal_wrapped(|ui| {
                changed |= ui.checkbox(&mut a.enabled, "").on_hover_text("On or off").changed();
                ui.label(egui::RichText::new(summary(a)).size(super::rows::TEXT).color(theme.text_secondary));
                if ui.small_button("\u{2715}").on_hover_text("Remove this animator").clicked() {
                    remove = Some(i);
                }
            });
            egui::CollapsingHeader::new("Details").id_salt(("title_animator", clip_id, i)).show(ui, |ui| {
                changed |= details(ui, theme, i, a, target.is_some(), clip_time_secs, keyframe_tolerance, &mut keys);
            });
        }
    });
    if let Some(i) = remove {
        animators.remove(i);
        changed = true;
    }
    if changed {
        cmds.push(AppCommand::SetTitleAnimators { clip_id, animators: animators.clone() });
    }
    // A range is asked to key only on a layer, by its diamond or with
    // keys already, so a plain set never comes of it; were it to, it
    // would set the animators as they stand.
    for k in keys {
        super::handle_text_key_edit(cmds, clip_id, target, &k.param_key, k.value, &k.keyframes, clip_time_secs, keyframe_tolerance, k.edit, |_| AppCommand::SetTitleAnimators { clip_id, animators: animators.clone() });
    }
}

/// A selector's unit and its range, start, end and offset, keyable as
/// `animator.{i}.{prefix}{field}`; true when one changed in place.
#[allow(clippy::too_many_arguments)]
fn range_rows(ui: &mut egui::Ui, theme: &KlippTheme, i: usize, prefix: &str, s: &mut RangeSelector, keyable: bool, t: f64, tolerance: f64, keys: &mut Vec<KeyRequest>) -> bool {
    use super::rows::param_row;
    let mut changed = false;
    egui::ComboBox::from_id_salt(("animator_unit", i, prefix.to_string())).selected_text(unit_label(s.unit)).show_ui(ui, |ui| {
        for u in [SelectUnit::Char, SelectUnit::Word, SelectUnit::Line] {
            changed |= ui.selectable_value(&mut s.unit, u, unit_label(u)).changed();
        }
    });
    let (start, end, offset) = (s.start, s.end, s.offset);
    for (field, label, base, keyframes) in [
        ("start", "Start", start, &mut s.start_keyframes),
        ("end", "End", end, &mut s.end_keyframes),
        ("offset", "Offset", offset, &mut s.offset_keyframes),
    ] {
        let mut v = klipp_core::keyframe::evaluate(keyframes, t, base) * 100.0;
        let range = if field == "offset" { -100.0..=100.0 } else { 0.0..=100.0 };
        let default = if field == "end" { 100.0 } else { 0.0 };
        let ks = keyable.then(|| crate::inspector::keyframe_selection::state(keyframes, t, tolerance));
        let resp = param_row(ui, theme, label, &mut v, range, default, 1.0, 0, "%", ks);
        let edit: super::TextKeyEdit = (&resp).into();
        // Without a layer to key, or unkeyed, the value is the range's own.
        if !keyable || (keyframes.is_empty() && !edit.toggled) {
            if edit.changed {
                let v = v / 100.0;
                match field {
                    "start" => s.start = v,
                    "end" => s.end = v,
                    _ => s.offset = v,
                }
                changed = true;
            }
        } else if edit.toggled || edit.changed || edit.interpolation.is_some() {
            keys.push(KeyRequest { param_key: klipp_core::title_effect_keys::animator_key(i, &format!("{prefix}{field}")), value: v / 100.0, edit, keyframes: keyframes.clone() });
        }
    }
    changed
}

/// An animator's full controls; true when one of them changed it in
/// place. A range row with keys (or its diamond) asks for a key instead.
#[allow(clippy::too_many_arguments)]
fn details(ui: &mut egui::Ui, theme: &KlippTheme, i: usize, a: &mut Animator, keyable: bool, t: f64, tolerance: f64, keys: &mut Vec<KeyRequest>) -> bool {
    use super::rows::param_row;
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("Name");
        changed |= ui.text_edit_singleline(&mut a.name).changed();
    });
    // Each range's rows in an id scope of their own: the two ranges'
    // rows share their labels.
    changed |= ui.push_id(("animator_range", i), |ui| range_rows(ui, theme, i, "", &mut a.selector, keyable, t, tolerance, keys)).inner;
    let s = &mut a.selector;
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(("animator_shape", i)).selected_text(shape_label(s.shape)).show_ui(ui, |ui| {
            for sh in [SelectShape::Square, SelectShape::RampUp, SelectShape::RampDown, SelectShape::Triangle, SelectShape::Round, SelectShape::Smooth] {
                changed |= ui.selectable_value(&mut s.shape, sh, shape_label(sh)).changed();
            }
        });
        egui::ComboBox::from_id_salt(("animator_order", i)).selected_text(order_label(s.order)).show_ui(ui, |ui| {
            for o in [SelectOrder::Forward, SelectOrder::Reverse, SelectOrder::FromCentre] {
                changed |= ui.selectable_value(&mut s.order, o, order_label(o)).changed();
            }
            if ui.selectable_label(matches!(s.order, SelectOrder::Random { .. }), "Random").clicked() && !matches!(s.order, SelectOrder::Random { .. }) {
                s.order = SelectOrder::Random { seed: 1 };
                changed = true;
            }
        });
        if let SelectOrder::Random { seed } = &mut s.order {
            changed |= ui.add(egui::DragValue::new(seed).prefix("Seed ")).changed();
        }
    });
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(("animator_easing", i)).selected_text(s.easing.label()).show_ui(ui, |ui| {
            for e in Easing::ALL {
                changed |= ui.selectable_value(&mut s.easing, e, e.label()).changed();
            }
        });
        let mut amount = s.amount * 100.0;
        if ui.add(egui::DragValue::new(&mut amount).range(-100.0..=100.0).suffix("%").prefix("Amount ")).changed() {
            s.amount = amount / 100.0;
            changed = true;
        }
    });
    // A second range the first is limited to: a random order inside one
    // word, say.
    let mut limited = a.within.is_some();
    if ui.checkbox(&mut limited, "Limit to").on_hover_text("Only the units inside a second range: its weight multiplies this one's").changed() {
        a.within = limited.then(|| RangeSelector { start: 0.0, end: 1.0, ..Default::default() });
        changed = true;
    }
    if let Some(within) = &mut a.within {
        changed |= ui.push_id(("animator_within_range", i), |ui| range_rows(ui, theme, i, "within.", within, keyable, t, tolerance, keys)).inner;
    }
    // What it does at full selection.
    let p = &mut a.props;
    let mut row = |ui: &mut egui::Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, default: f32, speed: f64, suffix: &str| {
        changed |= param_row(ui, theme, label, v, range, default, speed, 2, suffix, None).changed;
    };
    row(ui, "Move X", &mut p.dx, -10.0..=10.0, 0.0, 0.01, " em");
    row(ui, "Move Y", &mut p.dy, -10.0..=10.0, 0.0, 0.01, " em");
    row(ui, "Scale", &mut p.scale, 0.0..=10.0, 1.0, 0.01, "\u{00d7}");
    row(ui, "Rotation", &mut p.rotation, -1080.0..=1080.0, 0.0, 1.0, "\u{00b0}");
    row(ui, "Opacity", &mut p.opacity, 0.0..=1.0, 1.0, 0.01, "");
    row(ui, "Blur", &mut p.blur, 0.0..=2.0, 0.0, 0.01, " em");
    ui.horizontal(|ui| {
        let mut on = p.colour.is_some();
        if ui.checkbox(&mut on, "Colour").changed() {
            p.colour = on.then_some([255, 255, 255, 255]);
            changed = true;
        }
        if let Some(c) = &mut p.colour {
            let mut col = egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
            if ui.color_edit_button_srgba(&mut col).changed() {
                *c = col.to_srgba_unmultiplied();
                changed = true;
            }
        }
    });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Frame {
        texts: Vec<(String, egui::Pos2)>,
        /// The key diamonds drawn, by their centres.
        diamonds: Vec<egui::Pos2>,
        cmds: Vec<AppCommand>,
    }

    fn clip() -> ClipId {
        ClipId(uuid::Uuid::nil())
    }

    /// One frame of the section for a layer whose stack is `fx`, at 0.5 s
    /// into a 4 s clip, with `events`.
    fn frame(ctx: &egui::Context, fx: &LayerFx, events: Vec<egui::Event>) -> Frame {
        let mut cmds = Vec::new();
        let out = ctx.run(
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 2000.0))), events, ..Default::default() },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show_animators(ui, &KlippTheme::default(), clip(), Some(TitleObjectId(3)), fx, 4.0, 0.5, 0.01, &mut cmds);
                });
            },
        );
        let mut texts = Vec::new();
        let mut diamonds = Vec::new();
        for s in &out.shapes {
            match &s.shape {
                egui::Shape::Text(t) => texts.push((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
                egui::Shape::Path(p) if p.points.len() == 4 && p.closed => {
                    let c = p.points.iter().fold(egui::Vec2::ZERO, |a, q| a + q.to_vec2()) / 4.0;
                    diamonds.push(c.to_pos2());
                }
                _ => {}
            }
        }
        Frame { texts, diamonds, cmds }
    }

    fn at(f: &Frame, text: &str) -> egui::Pos2 {
        f.texts.iter().find(|(t, _)| t == text).map(|(_, p)| *p).unwrap_or_else(|| panic!("{text} is drawn"))
    }

    /// A click at `pos`: the commands its release gives.
    fn click(ctx: &egui::Context, fx: &LayerFx, pos: egui::Pos2) -> Vec<AppCommand> {
        let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut cmds = frame(ctx, fx, vec![egui::Event::PointerMoved(pos), button(true)]).cmds;
        cmds.extend(frame(ctx, fx, vec![button(false)]).cmds);
        cmds
    }

    /// #1510: a preset is one click, and one step: the layer's animators
    /// with the preset added, in one command.
    #[test]
    fn a_preset_adds_its_animator_in_one_step() {
        let ctx = egui::Context::default();
        let fx = LayerFx::default();
        let f = frame(&ctx, &fx, Vec::new());
        assert!(f.cmds.is_empty());
        for (n, name) in ["Wave", "Random fade in", "Spin in"].into_iter().enumerate() {
            let cmds = click(&ctx, &fx, at(&f, name));
            match &cmds[..] {
                [AppCommand::SetTitleAnimators { animators, .. }] => assert_eq!(animators, &vec![presets(4.0)[n].clone()], "{name}"),
                other => panic!("{name}: one command, the animators: {other:?}"),
            }
        }
    }

    /// An animator shows as one line saying what it does, and its ✕
    /// takes it away, in one step.
    #[test]
    fn an_animator_is_a_line_and_its_cross_removes_it() {
        let ctx = egui::Context::default();
        let fx = LayerFx { animators: vec![presets(4.0)[2].clone()], ..Default::default() };
        let f = frame(&ctx, &fx, Vec::new());
        let line = summary(&fx.animators[0]);
        assert_eq!(line, "Spin in \u{00b7} letters 0\u{2013}100%, animated \u{00b7} scale 40%, turn 360\u{00b0}, fade");
        at(&f, &line);
        assert!(f.texts.iter().all(|(t, _)| t != "Rotation"), "the controls are folded");
        let cmds = click(&ctx, &fx, at(&f, "\u{2715}"));
        assert!(matches!(&cmds[..], [AppCommand::SetTitleAnimators { animators, .. }] if animators.is_empty()), "{cmds:?}");
    }

    /// Under Details, the range's rows key through the layer keyframe
    /// commands by the animator's key: the diamond of a keyed row adds a
    /// key at the playhead, and one of a row with no keys does too.
    #[test]
    fn the_folded_range_keys_by_its_animator_key() {
        let ctx = egui::Context::default();
        let fx = LayerFx { animators: vec![presets(4.0)[1].clone()], ..Default::default() };
        let f = frame(&ctx, &fx, Vec::new());
        click(&ctx, &fx, at(&f, "Details"));
        let f = frame(&ctx, &fx, Vec::new());
        for row in ["Start", "End"] {
            let y = at(&f, row).y;
            let diamond = *f.diamonds.iter().find(|d| (d.y - y).abs() < 8.0).unwrap_or_else(|| panic!("{row} has a diamond"));
            let cmds = click(&ctx, &fx, diamond);
            let key = format!("animator.0.{}", row.to_lowercase());
            assert!(
                matches!(&cmds[..], [AppCommand::AddTitleObjectEffectKeyframe { object_id: 3, param_key, time_secs, .. }] if *param_key == key && *time_secs == 0.5),
                "{row}: {cmds:?}"
            );
        }
        assert!(f.texts.iter().any(|(t, _)| t == "Rotation"), "the properties are there too");
    }

    /// "Limit to" adds a second range in one step, and its rows key by
    /// the animator's `within.` keys (#1510).
    #[test]
    fn limit_to_adds_a_range_that_keys_by_its_own_keys() {
        let ctx = egui::Context::default();
        let fx = LayerFx { animators: vec![presets(4.0)[2].clone()], ..Default::default() };
        let f = frame(&ctx, &fx, Vec::new());
        click(&ctx, &fx, at(&f, "Details"));
        let f = frame(&ctx, &fx, Vec::new());
        let cmds = click(&ctx, &fx, at(&f, "Limit to"));
        let limited = match &cmds[..] {
            [AppCommand::SetTitleAnimators { animators, .. }] => animators[0].clone(),
            other => panic!("one command: {other:?}"),
        };
        assert_eq!(limited.within, Some(RangeSelector::default()));
        assert!(summary(&limited).contains("within letters 0\u{2013}100%"), "{}", summary(&limited));
        let fx = LayerFx { animators: vec![limited], ..Default::default() };
        let f = frame(&ctx, &fx, Vec::new());
        // The second Start row is the limit's.
        let starts: Vec<f32> = f.texts.iter().filter(|(t, _)| t == "Start").map(|(_, p)| p.y).collect();
        assert_eq!(starts.len(), 2, "both ranges are shown");
        assert!(f.diamonds.iter().all(|d| d.x < 420.0), "every diamond inside the panel: {:?}", f.diamonds);
        let diamond = *f.diamonds.iter().find(|d| (d.y - starts[1]).abs() < 8.0).expect("the limit's Start has a diamond");
        let cmds = click(&ctx, &fx, diamond);
        assert!(matches!(&cmds[..], [AppCommand::AddTitleObjectEffectKeyframe { param_key, .. }] if param_key == "animator.0.within.start"), "{cmds:?}");
    }
}
