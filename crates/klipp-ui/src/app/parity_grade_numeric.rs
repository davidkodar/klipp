//! A grade's numeric rows through both views (M3 step 18): every slider
//! and number of a grade node, on each page that draws it: the per-node
//! layout for a per-node type, and the Corrector's tabbed page and the
//! Colour page's bottom tools for the types they show. Each row is opened
//! (nothing written), typed, dragged, cancelled by Escape and reset, in
//! the generated view and the old panel, which must leave the same
//! picture, one undo step and an exact undo and redo (`edit_case`); the
//! typed edit is drawn on the CPU and GPU routes, alike in both views
//! (`same_sequence_edit`). Run through `on_roots` too, so the sequence's
//! grades are earned alike, drawn by their own stage.
//!
//! Wheel axes and the warper's sectors are another driver's (a drag in a
//! disc) and another family's (`parity_grade_wheels`).

use super::parity_controls::{grade_variant, meet_conditions, placed, Grade, Page};
use super::parity_harness::GradeRoot;
use super::parity_manifest::{catalog, CoverageKey, Interaction, Manifest, Report, Surface};
use crate::inspector::generated::row;
use crate::state::ColorTab;
use klipp_effects::registry::presentation::Widget;
use klipp_effects::registry::ParamDesc;
use klipp_nodes::registry::NodeType;
use klipp_nodes::NodeKind;

/// The grade types the tabbed page and the bottom tools show, and on
/// which: as the central rules place them (`rules::surfaces`), the tabs
/// they're drawn on found by looking.
const PAGE_TYPES: &[(NodeKind, &[Surface])] = &[
    (NodeKind::Corrector, &[Surface::GradeTabbedRight, Surface::GradeBottomTools]),
    (NodeKind::Exposure, &[Surface::GradeTabbedRight, Surface::GradeBottomTools]),
    (NodeKind::Contrast, &[Surface::GradeTabbedRight, Surface::GradeBottomTools]),
    (NodeKind::Saturation, &[Surface::GradeTabbedRight, Surface::GradeBottomTools]),
    (NodeKind::ToneControls, &[Surface::GradeTabbedRight, Surface::GradeBottomTools]),
    (NodeKind::ColorBalance, &[Surface::GradeTabbedRight, Surface::GradeBottomTools]),
    (NodeKind::ColorWheels, &[Surface::GradeTabbedRight, Surface::GradeBottomTools]),
    (NodeKind::RGBCurves, &[Surface::GradeTabbedRight, Surface::GradeBottomTools]),
    (NodeKind::HueSatCurves, &[Surface::GradeTabbedRight, Surface::GradeBottomTools]),
    (NodeKind::InputTransform, &[Surface::GradeTabbedRight, Surface::GradeBottomTools]),
];

/// Rows whose old drag is pinned, not compared, past the end: the old row
/// drags over another range and stalls on a slow drag (#1427). The
/// generated row is still held to reaching the end, clamped.
const OLD_DRAG_PINNED: &[(&str, &str)] = &[("denoise_radius", "#1427: the old Radius row drags over 1..5, not the catalog's 1..7, and stalls on a slow drag")];

const TABS: [ColorTab; 5] = [ColorTab::Adjustments, ColorTab::Wheels, ColorTab::Curves, ColorTab::HueSat, ColorTab::Warper];

/// A numeric row this family drives: a slider or number, not a wheel's
/// axis or a sector (another family's), not hidden.
fn numeric(ty: &NodeType, desc: &ParamDesc) -> bool {
    matches!(ty.presentation(desc).widget, Widget::Scrub | Widget::Slider | Widget::Gradient(_) | Widget::HueRainbow | Widget::Vec2)
        // The warper's sectors and the wheels' saturation and brightness
        // cells are the wheels family's; Colour Match's values are readouts
        // (#1421).
        && !matches!(ty.kind, NodeKind::ColorWarper | NodeKind::WheelSatBri | NodeKind::ColorMatch)
        && !(desc.key.ends_with("_sat") || desc.key.ends_with("_bri"))
        // The log wheels' ranges sit under the wheels (`wheel_shape_sliders`).
        && !matches!(desc.key, "log_low_range" | "log_high_range")
}

/// Kinds whose per-node controls are one shared editor in both views (a
/// registered whole-node widget, kept by step 19, not old code): their
/// rows aren't generated number rows, so their reset and cancel are held
/// alike rather than to IC14 and IC15.
const WHOLE_NODE: &[NodeKind] = &[NodeKind::ExternalMatte, NodeKind::DifferenceKey, NodeKind::LayerMixer];

fn per_node_types() -> impl Iterator<Item = &'static NodeType> {
    klipp_nodes::registry::node_types().filter(|t| super::main_layout::per_node_kind(t.kind))
}

/// The obligations of every row this family drives, on the surface and
/// variant its cases use, and on the sequence's grades (`on_roots`).
pub(super) fn require(manifest: &mut Manifest) {
    let entries = catalog();
    let mut want = |ty: &NodeType, desc: &ParamDesc, surface: Surface| {
        let Some(entry) = entries.iter().find(|e| e.type_id == ty.desc.id && e.parameter == desc.key) else { return };
        let variant = grade_variant(ty, desc);
        manifest.require_fixture(entry, surface, variant);
        for root in GradeRoot::SEQUENCE {
            manifest.require_fixture(entry, Surface::NonClipGrade, &format!("{}: {surface:?} {variant}", root.variant()));
        }
    };
    for ty in per_node_types() {
        for desc in ty.desc.params.iter().filter(|d| numeric(ty, d)) {
            want(ty, desc, Surface::GradePerNode);
        }
    }
    for (kind, surfaces) in PAGE_TYPES {
        let Some(ty) = klipp_nodes::registry::node_type(*kind) else { continue };
        for desc in ty.desc.params.iter().filter(|d| numeric(ty, d)) {
            for surface in *surfaces {
                want(ty, desc, *surface);
            }
        }
    }
}

/// The row labelled `label`, where drawn: its label and its number, right
/// of the label on its line, or else the nearest below it; the `nth` such
/// row in reading order when several share the label.
pub(super) fn row_at(g: &mut Grade, label: &str, nth: usize) -> Result<(egui::Pos2, egui::Pos2), String> {
    let drawn = g.drawn();
    // Compared trimmed: a readout pads its labels to line them up.
    let mut labels: Vec<egui::Pos2> = drawn.iter().filter(|(t, _)| t.trim() == label).map(|(_, p)| *p).collect();
    labels.sort_by(|a, b| (a.y.round(), a.x).partial_cmp(&(b.y.round(), b.x)).unwrap());
    let is_number = |t: &str| t.trim_start_matches(['-', '+']).starts_with(|c: char| c.is_ascii_digit());
    let number_of = |at: egui::Pos2| {
        let right = drawn.iter().filter(|(t, p)| (p.y - at.y).abs() < 6.0 && p.x > at.x && is_number(t)).min_by(|a, b| a.1.x.total_cmp(&b.1.x));
        let below = drawn.iter().filter(|(t, p)| p.y > at.y && p.y - at.y < 40.0 && (p.x - at.x).abs() < 80.0 && is_number(t)).min_by(|a, b| a.1.distance(at).total_cmp(&b.1.distance(at)));
        right.or(below).map(|(_, p)| *p)
    };
    if labels.is_empty() {
        return Err(format!("no {label:?} row"));
    }
    // The `nth` same-labelled row with a number: a label without one (a
    // bypass strip naming its node, a section header) isn't a row.
    let rows: Vec<(egui::Pos2, egui::Pos2)> = labels.iter().filter_map(|at| number_of(*at).map(|n| (*at, n))).collect();
    rows.get(nth).or(rows.last()).copied().ok_or_else(|| format!("no number on the {label:?} row"))
}

/// Which of several same-labelled rows a parameter's is: its wheel, in
/// the order the wheels are drawn.
fn nth(desc: &ParamDesc) -> usize {
    ["lift", "gamma", "gain", "offset"].iter().position(|w| desc.key.starts_with(w)).unwrap_or(0)
}

pub(super) fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
}

pub(super) fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
}

/// The row's number typed to `text`, as a user does: focused by a double
/// click, all selected, typed, Enter.
fn type_into(g: &mut Grade, label: &str, n: usize, text: &str) -> Result<(), String> {
    let (_, at) = row_at(g, label, n)?;
    for pressed in [true, false, true, false] {
        g.frame(vec![button(at, pressed)]);
    }
    // A row that opens a text field on the double click focuses it on the
    // frame after (the old pages' `inspector_row`): keys sent before then
    // would be lost.
    g.frame(Vec::new());
    g.frame(Vec::new());
    g.frame(vec![key(egui::Key::A, egui::Modifiers::COMMAND)]);
    g.frame(vec![egui::Event::Text(text.into())]);
    g.frame(vec![key(egui::Key::Enter, egui::Modifiers::NONE)]);
    g.frame(vec![egui::Event::PointerGone]);
    Ok(())
}

/// A drag of the row's number by `dx`, released, or taken back by Escape.
fn drag(g: &mut Grade, label: &str, n: usize, dx: f32, escape: bool) -> Result<(), String> {
    let (_, at) = row_at(g, label, n)?;
    g.frame(vec![egui::Event::PointerMoved(at)]);
    g.frame(vec![button(at, true)]);
    for i in 1..=3 {
        g.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx * i as f32 / 3.0, 0.0))]);
    }
    if escape {
        g.frame(vec![key(egui::Key::Escape, egui::Modifiers::NONE)]);
    }
    g.frame(vec![button(at + egui::vec2(dx, 0.0), false)]);
    g.frame(vec![egui::Event::PointerGone]);
    Ok(())
}

/// The row's reset: a click on its label (IC15).
fn reset(g: &mut Grade, label: &str, n: usize) -> Result<(), String> {
    let (at, _) = row_at(g, label, n)?;
    g.click_at(at);
    Ok(())
}

/// The row shown on a page that draws it: each tab tried, the row's
/// conditions met there, the first that draws its label; with the
/// fixture's variant.
pub(super) fn shown_on(base: &Grade, ty: &NodeType, desc: &ParamDesc, wide: bool) -> Result<(Grade, &'static str), String> {
    let label = ty.presentation(desc).label.unwrap_or(desc.label);
    let mut last = format!("no tab draws {label:?}");
    // A mix row is its tab's: several tabs draw a "Mix".
    let only = match desc.key {
        k if k.starts_with("curves_") => Some(ColorTab::Curves),
        k if k.starts_with("huesat_") => Some(ColorTab::HueSat),
        "luma_mix" => Some(ColorTab::Wheels),
        _ => None,
    };
    for tab in TABS.into_iter().filter(|t| only.is_none_or(|o| o == *t)) {
        let on = base.twin(true).on(if wide { Page::Bottom(tab) } else { Page::Tabbed(tab) });
        match meet_conditions(&on, ty, desc) {
            Ok((mut shown, variant)) => {
                if shown.drawn().iter().any(|(t, _)| t == label) {
                    return Ok((shown.twin(true), variant));
                }
            }
            Err(e) => last = format!("on {tab:?}: {e}"),
        }
    }
    Err(last)
}

/// An edit through both views (`drive`), from `start`: the generated view
/// and the old panel must store the same value of the row, leave the same
/// picture, one undo step each, and the generated one undo and redo
/// exactly. A failure names the row's stored value in each view and each
/// view's steps.
pub(super) fn edit_case(start: &Grade, desc: &ParamDesc, drive: impl Fn(&mut Grade) -> Result<(), String>) -> Result<(), String> {
    let (mut new, mut old) = (start.twin(true), start.twin(false));
    new.drawn();
    old.drawn();
    let before = new.picture();
    let value_before = new.value(desc);
    drive(&mut new)?;
    drive(&mut old).map_err(|e| format!("the old panel: {e}"))?;
    if !new.unapplied.is_empty() || !old.unapplied.is_empty() {
        return Err(format!("unapplied: new {:?}, old {:?}", new.unapplied, old.unapplied));
    }
    let stored = |g: &Grade| g.value(desc);
    // The row's own node changed, not another the page also edits.
    if stored(&new) == value_before {
        return Err(format!("the edit left this node's {} at {value_before}: it changed another node", desc.key));
    }
    let steps = |g: &Grade| g.state.active_history().undo_count();
    let after = new.picture();
    if after == before {
        return Err(format!("the control changed nothing (old stores {}, {} steps)", stored(&old), steps(&old)));
    }
    if after != old.picture() {
        return Err(format!("the views store {} (new) and {} (old)", stored(&new), stored(&old)));
    }
    if (steps(&new), steps(&old)) != (1, 1) {
        return Err(format!("undo steps: new {}, old {}", steps(&new), steps(&old)));
    }
    new.step(false)?;
    if new.picture() != before {
        return Err("undo doesn't give back the picture before".into());
    }
    new.step(true)?;
    if new.picture() != after {
        return Err("redo doesn't give back the picture after".into());
    }
    Ok(())
}

/// `drive` in both views from `start`: the same picture and the same undo
/// depth after it, whatever each did. For a row both views draw with the
/// old code, where an intended change can't be asked of the generated one.
pub(super) fn alike(start: &Grade, drive: impl Fn(&mut Grade) -> Result<(), String>) -> Result<(), String> {
    let (mut new, mut old) = (start.twin(true), start.twin(false));
    new.drawn();
    old.drawn();
    drive(&mut new)?;
    drive(&mut old).map_err(|e| format!("the old panel: {e}"))?;
    let steps = |g: &Grade| g.state.active_history().undo_count();
    if new.picture() != old.picture() || steps(&new) != steps(&old) || !new.unapplied.is_empty() || !old.unapplied.is_empty() {
        return Err(format!("the views differ: steps new {}, old {}; unapplied new {:?}, old {:?}", steps(&new), steps(&old), new.unapplied, old.unapplied));
    }
    Ok(())
}

/// `start` with what the row's edit needs to show in the frame at all, set
/// alike for both views before the pixel case: a pivot needs a contrast, a
/// mix something to mix, a radius or a grain's shape a strength, a LUT's
/// opacity a LUT, a softness an edge, a difference key a plate, and a
/// matte or a mix a correction to select or mix (the exposure of the other
/// Correctors in the grade). Set through the colour commands,
/// then twinned, so its history starts empty.
fn shown_in_frame(start: &Grade, ty: &NodeType, desc: &ParamDesc) -> Grade {
    use klipp_core::color_grade::{CurveSet, FilmGrainSettings, HueSatCurves};
    use klipp_core::AppCommand;
    let mut g = start.twin(true);
    let (clip_id, node_id) = (g.clip, g.node);
    let param = |param: &str, value: f32| AppCommand::SetNodeParam { clip_id, node_id, param: param.into(), value };
    let mut cmds = match desc.key {
        "contrast_pivot" => vec![param("contrast", 0.5)],
        "global_mix" => vec![param("temperature", 0.5)],
        "luma_mix" => vec![param("gain_master", 0.3)],
        "denoise_radius" => vec![param("denoise_luma", 0.5)],
        "curves_mix" => vec![AppCommand::SetNodeCurves { clip_id, node_id, curves: CurveSet { master: vec![[0.0, 0.0], [0.5, 0.75], [1.0, 1.0]], ..Default::default() } }],
        "huesat_mix" => vec![AppCommand::SetNodeHueSatCurves { clip_id, node_id, huesat: HueSatCurves { lum_vs_sat: vec![[0.0, 0.5], [1.0, 0.5]], ..Default::default() } }],
        "lut_opacity" => vec![AppCommand::SetNodeLut { clip_id, node_id, path: Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/luts/cinematic/D-CINELIKE -0_3 BASE.cube")) }],
        // A softness needs an edge to soften: a low end above the bottom.
        "luma_key_soft" => vec![param("luma_key_low", 0.3)],
        "qual_lum_soft" => vec![param("qual_lum_low", 0.4), param("qual_lum_high", 0.6)],
        "qual_sat_soft" => vec![param("qual_sat_low", 0.3)],
        k if k.starts_with("difference_key.") => {
            let mut difference_key = g.grade().nodes.iter().find(|n| n.id == node_id).and_then(|n| n.params.difference_key.clone()).unwrap_or_default();
            difference_key.plate_path = Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/effect_preview_reference.png"));
            vec![AppCommand::SetNodeDifferenceKey { clip_id, node_id, difference_key }]
        }
        k if k.starts_with("grain.") && k != "grain.amount" => vec![AppCommand::SetNodeFilmGrain { clip_id, node_id, grain: FilmGrainSettings { enabled: true, amount: 0.5, ..Default::default() } }],
        _ => Vec::new(),
    };
    if ty.desc.outputs.iter().any(|p| p.ty == klipp_effects::registry::PortType::Matte) || matches!(ty.kind, NodeKind::Blend | NodeKind::ApplyMatte) {
        for corrector in g.grade().nodes.iter().filter(|n| n.kind == NodeKind::Corrector && n.id != node_id).map(|n| n.id).collect::<Vec<_>>() {
            cmds.push(AppCommand::SetNodeParam { clip_id, node_id: corrector, param: "exposure".into(), value: 1.0 });
        }
    }
    for cmd in cmds {
        g.apply(cmd);
    }
    g.twin(true)
}

/// One row's cases on `start` (the row drawn), counted on `surface`.
pub(super) fn row_cases(report: &mut Report, ty: &NodeType, desc: &'static ParamDesc, start: &Grade, surface: Surface, variant: &str) {
    // The bottom tools' wide wheels (`show_wheels_wide`, kept by step 19)
    // draw the luma mix in both views.
    // The bottom tools' log ranges are `wheel_shape_sliders` in both views.
    let shared = (surface == Surface::GradePerNode && WHOLE_NODE.contains(&ty.kind))
        || (surface == Surface::GradeBottomTools && desc.key == "luma_mix")
        || (surface == Surface::GradeBottomTools && matches!(desc.key, "log_low_range" | "log_high_range"));
    let id = ty.desc.id;
    let pres = ty.presentation(desc);
    let label = pres.label.unwrap_or(desc.label);
    let n = nth(desc);
    let k = |interaction| CoverageKey::new(id, desc.key, surface, variant, interaction);
    let name = |what: &str| format!("{id} {} on {surface:?} {what}", desc.key);
    let encoding = klipp_grade_model::Encoding::from_u8(klipp_core::grade_encoding().effective(klipp_engine::video_decoder::working_colorspace()).as_u8());
    let (lo, hi) = row::shown_range(desc, &pres, encoding);
    // A value the row shows, apart from its default either way.
    let typed = format!("{:.*}", usize::from(pres.decimals), lo + (hi - lo) * 0.7);
    // A drag toward the middle of the range, so one starting at an end
    // moves.
    let current = start.value(desc).as_f64().unwrap_or(0.0) as f32;
    // A whole-number row's further, so it moves at least one unit.
    let reach = if desc.kind == klipp_effects::registry::ParamKind::Whole { 72.0 } else { 24.0 };
    let dx = if current > (lo + hi) / 2.0 { -reach } else { reach };
    let pinned = OLD_DRAG_PINNED.iter().any(|(key, _)| *key == desc.key);

    report.case(&name("opened"), vec![k(Interaction::OpenWithoutWrite)], || {
        for generated in [false, true] {
            let mut g = start.twin(generated);
            let before = g.picture();
            g.drawn();
            g.drawn();
            if g.picture() != before || g.state.active_history().undo_count() != 0 || !g.unapplied.is_empty() {
                return Err(format!("generated {generated}: opening wrote"));
            }
        }
        Ok(())
    });
    report.case(&name("typed"), vec![k(Interaction::TypeNumber), k(Interaction::UndoRedo)], || edit_case(start, desc, |g| type_into(g, label, n, &typed)));
    // The typed edit drawn: both views' grades draw the frame the edit
    // changed, alike on each route, through the stage the grade is on.
    report.case_or_blocked(&name("pixels"), [k(Interaction::Pixels)], || {
        let start = &shown_in_frame(start, ty, desc);
        let (mut new, mut old) = (start.twin(true), start.twin(false));
        new.drawn();
        old.drawn();
        type_into(&mut new, label, n, &typed)?;
        type_into(&mut old, label, n, &typed).map_err(|e| format!("the old panel: {e}"))?;
        super::parity_pixels::same_sequence_edit(&start.frame_sequence(), &old.frame_sequence(), &new.frame_sequence())
    });
    report.case(&name("dragged"), vec![k(Interaction::Drag), k(Interaction::UndoRedo)], || {
        if pinned {
            // The old drag is pinned (#1427): the generated one moves.
            let mut g = start.twin(true);
            g.drawn();
            let from = g.value(desc);
            drag(&mut g, label, n, dx, false)?;
            return if g.value(desc) != from && g.state.active_history().undo_count() == 1 { Ok(()) } else { Err("the generated drag didn't move in one step".into()) };
        }
        edit_case(start, desc, |g| drag(g, label, n, dx, false))
    });
    // A drag past the row's low end, from just above it (the harness's
    // screen is narrower than a span): both views store the end, clamped
    // alike, never a value beyond it.
    report.case(&name("dragged past the end"), vec![k(Interaction::Drag)], || {
        // At least one shown step above the end, which a whole-number row
        // would otherwise round down to.
        let step = if desc.kind == klipp_effects::registry::ParamKind::Whole { 1.0 } else { 10f32.powi(-i32::from(pres.decimals)) };
        let near = format!("{:.*}", usize::from(pres.decimals), lo.min(hi) + ((hi - lo).abs() * 0.06).max(step));
        let mut set = start.twin(true);
        type_into(&mut set, label, n, &near)?;
        let near_end = set.twin(true);
        if !pinned {
            edit_case(&near_end, desc, |g| drag(g, label, n, -100.0, false))?;
        }
        let mut g = near_end.twin(true);
        g.drawn();
        let from = g.value(desc);
        drag(&mut g, label, n, -100.0, false)?;
        if pinned && g.value(desc) == from {
            return Err("the generated drag didn't move".into());
        }
        let stored = g.value(desc).as_f64().ok_or("the row's value isn't a number")? as f32;
        let (slo, shi) = (lo.min(hi), lo.max(hi));
        let shown = pres.display.map_or(stored, |m| (m.to_shown)(stored, encoding));
        if shown < slo - 1e-4 || shown > shi + 1e-4 {
            return Err(format!("the drag stored {stored}, past the row's range {slo}..={shi}"));
        }
        Ok(())
    });
    report.case(&name("reset"), vec![k(Interaction::Reset), k(Interaction::UndoRedo)], || {
        // From the typed value, so the reset changes it.
        let mut away = start.twin(true);
        type_into(&mut away, label, n, &typed)?;
        if !away.unapplied.is_empty() {
            return Err(format!("setting it away: unapplied {:?}", away.unapplied));
        }
        let away = away.twin(true);
        if shared {
            // Both views run the same shared editor: held alike, not to
            // IC15.
            return alike(&away, |g| reset(g, label, n));
        }
        // The generated row resets from its label to its declared value, in
        // one step, undone and redone exactly.
        let mut new = away.twin(true);
        new.drawn();
        let before = new.picture();
        reset(&mut new, label, n)?;
        let want = row::reset_value(desc, &pres);
        let now = new.value(desc).as_f64().ok_or("the row's value isn't a number")? as f32;
        if (now - want).abs() > 1e-4 || !new.unapplied.is_empty() {
            return Err(format!("the generated row reset to {now}, not {want}; unapplied {:?}", new.unapplied));
        }
        let after = new.picture();
        if new.state.active_history().undo_count() != 1 {
            return Err(format!("the generated reset left {} undo steps, not one", new.state.active_history().undo_count()));
        }
        new.step(false)?;
        if new.picture() != before {
            return Err("undo doesn't give back the picture before".into());
        }
        new.step(true)?;
        if new.picture() != after {
            return Err("redo doesn't give back the picture after".into());
        }
        // The old panel resets to the same picture, or, lacking a label
        // reset, changes nothing: intended change IC15 (m3-work-breakdown
        // M3-6), every number row resetting from its label in the
        // generated view only.
        let mut old = away.twin(false);
        old.drawn();
        let old_before = old.picture();
        reset(&mut old, label, n)?;
        if old.picture() != after && old.picture() != old_before {
            return Err("the old panel's reset leaves another picture".into());
        }
        Ok(())
    });
    report.case(&name("cancelled"), vec![k(Interaction::CancelDrag)], || {
        if shared {
            // Both views run the same shared editor: held alike, not to
            // IC14.
            return alike(start, |g| drag(g, label, n, dx, true));
        }
        // The generated row takes the drag back on Escape: the picture from
        // before it, no step. The old panel commits it instead: intended
        // change IC14 (m3-work-breakdown M3-5), held as the effect rows'.
        let mut g = start.twin(true);
        g.drawn();
        let before = g.picture();
        drag(&mut g, label, n, dx, true)?;
        if g.picture() != before || g.state.active_history().undo_count() != 0 || !g.unapplied.is_empty() {
            return Err("Escape didn't take the generated drag back".into());
        }
        let mut old = start.twin(false);
        drag(&mut old, label, n, dx, true)?;
        if !old.unapplied.is_empty() {
            return Err(format!("the old panel: unapplied {:?}", old.unapplied));
        }
        Ok(())
    });
}

pub(super) fn run(report: &mut Report) {
    for ty in per_node_types() {
        let rows: Vec<_> = ty.desc.params.iter().filter(|d| numeric(ty, d)).collect();
        if rows.is_empty() {
            continue;
        }
        let Some((state, clip, node)) = placed(ty.kind) else {
            report.case(ty.desc.id, [], || Err("the grade won't take one".into()));
            continue;
        };
        let base = Grade::new(state, clip, node, true);
        for desc in rows {
            match meet_conditions(&base, ty, desc) {
                Ok((shown, variant)) => row_cases(report, ty, desc, &shown, Surface::GradePerNode, variant),
                Err(e) => report.case(&format!("{} {}", ty.desc.id, desc.key), [], || Err(e)),
            }
        }
    }
    for (kind, surfaces) in PAGE_TYPES {
        let Some(ty) = klipp_nodes::registry::node_type(*kind) else { continue };
        let Some((mut state, clip, node)) = super::grade_inspector_tests::graded(*kind) else {
            report.case(ty.desc.id, [], || Err("the grade won't take one".into()));
            continue;
        };
        // The pages write an enabled Corrector whatever node is selected:
        // another kind's rows are its own with the Corrector off.
        if *kind != NodeKind::Corrector {
            if let Some(c) = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip)) {
                klipp_timeline::GradeAccess::edit_grade(c, |g| g.nodes.iter_mut().filter(|n| n.kind == NodeKind::Corrector).for_each(|n| n.enabled = false));
            }
        }
        let base = Grade::new(state, clip, node, true);
        for desc in ty.desc.params.iter().filter(|d| numeric(ty, d)) {
            for surface in *surfaces {
                match shown_on(&base, ty, desc, *surface == Surface::GradeBottomTools) {
                    Ok((shown, variant)) => row_cases(report, ty, desc, &shown, *surface, variant),
                    Err(e) => report.case(&format!("{} {} on {surface:?}", ty.desc.id, desc.key), [], || Err(e)),
                }
            }
        }
    }
}

#[test]
fn grade_numeric_rows_edit_as_the_old_panel() {
    let mut report = Report::default();
    report.family("grade numeric rows", run);
    report.family_result().unwrap();
    assert!(!report.passed.is_empty(), "no grade numeric row was driven");
}

#[test]
fn grade_numeric_rows_edit_as_the_old_panel_on_the_sequences_grades() {
    let mut report = Report::default();
    super::parity_controls::on_roots(&mut report, run);
    report.family_result().unwrap();
    super::parity_controls::assert_on_every_root(&report);
}





/// A value typed into the old per-node panel's number (`grade_slider`'s
/// DragValue) is one undo step, which undo takes back exactly: it used to
/// go out as a bare `SetNodeParam`, outside any gesture, with no step at
/// all. Now it's a one-shot gesture, as `slider_interact`'s are.
#[test]
fn a_value_typed_in_the_old_panel_is_one_undo_step() {
    let (state, clip, node) = placed(NodeKind::Blend).expect("a Blend in the grade");
    let ty = klipp_nodes::registry::node_type(NodeKind::Blend).unwrap();
    let mix = ty.desc.params.iter().find(|d| d.key == "mix").unwrap();
    let mut old = Grade::new(state, clip, node, false);
    old.drawn();
    let before = old.picture();
    type_into(&mut old, "Mix", 0, "0.30").unwrap();
    assert!(old.unapplied.is_empty(), "{:?}", old.unapplied);
    assert!((old.value(mix).as_f64().unwrap() - 0.3).abs() < 1e-6, "the typed value is stored");
    assert_eq!(old.state.active_history().undo_count(), 1, "one step");
    let after = old.picture();
    old.step(false).unwrap();
    assert!(old.picture() == before, "undo gives back the value before");
    old.step(true).unwrap();
    assert!(old.picture() == after, "redo gives back the typed value");
}


/// The old panel's one-shot producers that used to send their sets bare,
/// with no undo step: each click is one step now, which undo takes back.
/// The LUT's colour-space dropdown, the wide wheels' saturation rail and a
/// per-node wheel's S rail, each found from the text drawn by it.
#[test]
fn the_old_panel_s_one_shot_producers_are_one_undo_step_each() {
    type Find = fn(&[(String, egui::Pos2)]) -> Option<egui::Pos2>;
    let cases: [(&str, NodeKind, Page, bool, Find, Option<&str>); 3] = [
        ("the LUT's colour space", NodeKind::Lut3D, Page::PerNode, false, |d| d.iter().find(|(t, _)| t == "Use LUT default").map(|(_, p)| *p), Some("ACEScg")),
        ("the wide wheels' saturation rail", NodeKind::Corrector, Page::Bottom(ColorTab::Wheels), true, |d| d.iter().filter(|(t, _)| t == "Saturation").map(|(_, p)| *p).find(|p| d.iter().any(|(t, b)| t == "Brightness" && (b.x - p.x).abs() < 2.0 && b.y > p.y && b.y - p.y < 50.0)).map(|p| p + egui::vec2(-15.0, 15.0)), None),
        ("a wheel's S rail", NodeKind::ColorWheels, Page::PerNode, false, |d| d.iter().find(|(t, _)| t == "S").map(|(_, p)| *p + egui::vec2(28.0, 0.0)), None),
    ];
    for (what, kind, page, on_page, find, then) in cases {
        let fixture = if on_page { super::grade_inspector_tests::graded(kind) } else { placed(kind) };
        let (state, clip, node) = fixture.unwrap_or_else(|| panic!("{what}: no fixture"));
        let mut old = Grade::new(state, clip, node, false).on(page);
        let drawn = old.drawn();
        let before = old.picture();
        let at = find(&drawn).unwrap_or_else(|| panic!("{what}: not drawn: {drawn:?}"));
        old.click_at(at);
        if let Some(item) = then {
            let drawn = old.drawn();
            let at = drawn.iter().find(|(t, _)| t == item).map(|(_, p)| *p).unwrap_or_else(|| panic!("{what}: no {item:?} item: {drawn:?}"));
            old.click_at(at);
        }
        old.frame(Vec::new());
        assert!(old.unapplied.is_empty(), "{what}: unapplied {:?}", old.unapplied);
        assert!(old.picture() != before, "{what}: the click changed nothing");
        assert_eq!(old.state.active_history().undo_count(), 1, "{what}: one step");
        old.step(false).unwrap();
        assert!(old.picture() == before, "{what}: undo gives back the picture before");
    }
}

/// A grade edited under the same clip id draws anew on a renderer kept
/// across frames, on both routes, as the app's renderer is kept while a
/// clip's grade is edited: nothing it keeps by a clip's id holds a
/// graded frame past an edit.
#[test]
fn a_grade_edited_under_one_clip_id_draws_anew_on_a_kept_renderer() {
    use super::parity_pixels::{frames_at, Routes};
    let (state, clip, node) = super::grade_inspector_tests::graded(NodeKind::Corrector).unwrap();
    let base = state.sequences[0].all_clips().find(|c| c.id == clip).unwrap().clone();
    let with = |exposure: f32| {
        let mut c = base.clone();
        klipp_timeline::GradeAccess::set_grade_param(&mut c, node, "exposure", exposure);
        let (cpu, gpu) = frames_at(Routes::Both, (320, 180), &c, 0.5).unwrap();
        (cpu, gpu.unwrap())
    };
    let mut last = with(0.0);
    for exposure in [1.5, 0.0, -1.0] {
        let now = with(exposure);
        assert!(now.0 != last.0 && now.1 != last.1, "exposure {exposure}: a frame drawn as before the edit");
        last = now;
    }
    assert!(with(-1.0) == last, "the same grade draws the same frame");
}
