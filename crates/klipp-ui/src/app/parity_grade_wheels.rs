//! A grade's wheels and its Colour Warper through both views (M3 step
//! 18): every wheel's axes (its puck across and down, its luma master),
//! the saturation and brightness under a wheel, the log wheels' ranges,
//! and each of the warper's twelve sectors, on each page that draws them.
//! A wheel is driven as a user drives it, by a drag in its disc or along
//! its master; a sector by a drag from its point, across the ring (its
//! hue) or out from the centre (its saturation). Each is opened (nothing
//! written), dragged, reset and cancelled by Escape, in the generated
//! view and the old panel, which must leave the same picture, one undo
//! step and an exact undo and redo (`parity_grade_numeric::edit_case`).
//! Run through `on_roots` too, so the sequence's grades are earned alike.
//! Their pixels are the grade pixels family's.

use super::parity_controls::{grade_variant, placed, Grade, GradePicture, Page};
use super::parity_grade_numeric::{alike, button, key, row_at, row_cases};
use super::parity_harness::GradeRoot;
use super::parity_manifest::{catalog, CoverageKey, Interaction, Manifest, Report, Surface, WheelAxis};
use crate::state::ColorTab;
use klipp_effects::registry::presentation::Widget;
use klipp_effects::registry::ParamDesc;
use klipp_nodes::registry::NodeType;
use klipp_nodes::NodeKind;

/// The types with wheels or a warper, the surfaces that draw them, as the
/// central rules place them (`rules::surfaces`).
const TYPES: &[(NodeKind, &[Surface])] = &[
    (NodeKind::Corrector, &[Surface::GradeTabbedRight, Surface::GradeBottomTools]),
    (NodeKind::ColorWheels, &[Surface::GradeTabbedRight, Surface::GradeBottomTools]),
    (NodeKind::WheelSatBri, &[Surface::GradeBottomTools]),
    (NodeKind::ColorWarper, &[Surface::GradePerNode, Surface::GradeTabbedRight, Surface::GradeBottomTools]),
];

/// The variant of a fixture whose Corrector is off: the Colour page's
/// tabs and bottom tools edit an enabled Corrector whatever node is
/// selected (`render_tab_content`, `show_wheels_wide`), so a Color Wheels,
/// Wheel Sat/Bri or Colour Warper node is drawn there only without one.
const CORRECTOR_OFF: &str = "corrector off";

/// What drives a row: a wheel's axis, a slider under a wheel, a log range
/// (a number row), or a warper sector.
#[derive(Clone, Copy, Debug)]
enum Control {
    Wheel { label: &'static str, axis: WheelAxis },
    Under { wheel: usize, axis: WheelAxis },
    Range,
    Sector { index: usize, sat: bool },
}

const WHEELS: [(&str, &str); 7] = [("lift", "Lift"), ("gamma", "Gamma"), ("gain", "Gain"), ("offset", "Offset"), ("log_low", "Low"), ("log_mid", "Mid"), ("log_high", "High")];

fn control(ty: &NodeType, desc: &ParamDesc) -> Option<Control> {
    let pres = ty.presentation(desc);
    if let Widget::Custom("colour_warper") = pres.widget {
        let (sat, index) = desc.key.strip_prefix("warper_sat_").map(|i| (true, i)).or_else(|| desc.key.strip_prefix("warper_hue_").map(|i| (false, i)))?;
        return Some(Control::Sector { index: index.parse().ok()?, sat });
    }
    if matches!(desc.key, "log_low_range" | "log_high_range") {
        return Some(Control::Range);
    }
    if !matches!(pres.widget, Widget::WheelPart(_)) {
        return None;
    }
    let (prefix, axis) = desc.key.rsplit_once('_')?;
    let axis = match axis {
        "x" => WheelAxis::X,
        "y" => WheelAxis::Y,
        "master" => WheelAxis::Master,
        "sat" => WheelAxis::Saturation,
        "bri" => WheelAxis::Brightness,
        _ => return None,
    };
    let wheel = WHEELS.iter().position(|(p, _)| *p == prefix)?;
    Some(match axis {
        WheelAxis::Saturation | WheelAxis::Brightness => Control::Under { wheel, axis },
        _ => Control::Wheel { label: WHEELS[wheel].1, axis },
    })
}

fn rows(ty: &'static NodeType) -> impl Iterator<Item = (&'static ParamDesc, Control)> {
    ty.desc.params.iter().filter_map(move |d| control(ty, d).map(|c| (d, c)))
}

fn is_log(desc: &ParamDesc) -> bool {
    desc.key.starts_with("log_")
}

/// The variant `start` names a row by: the fixture's Corrector off, its
/// log wheels on, or the row's own conditions.
fn variant(ty: &NodeType, desc: &ParamDesc, surface: Surface) -> &'static str {
    if corrector_off(ty.kind, surface) {
        CORRECTOR_OFF
    } else if is_log(desc) {
        "conditions met"
    } else {
        grade_variant(ty, desc)
    }
}

fn corrector_off(kind: NodeKind, surface: Surface) -> bool {
    surface != Surface::GradePerNode && kind != NodeKind::Corrector
}

/// The obligations of every row this family drives, on the surface and
/// variant its cases use, and on the sequence's grades (`on_roots`).
pub(super) fn require(manifest: &mut Manifest) {
    let entries = catalog();
    for (kind, surfaces) in TYPES {
        let Some(ty) = klipp_nodes::registry::node_type(*kind) else { continue };
        for (desc, _) in rows(ty) {
            let Some(entry) = entries.iter().find(|e| e.type_id == ty.desc.id && e.parameter == desc.key) else { continue };
            for surface in *surfaces {
                let variant = variant(ty, desc, *surface);
                manifest.require_fixture(entry, *surface, variant);
                for root in GradeRoot::SEQUENCE {
                    manifest.require_fixture(entry, Surface::NonClipGrade, &format!("{}: {surface:?} {variant}", root.variant()));
                }
            }
        }
    }
}

/// The fixture a row is drawn on: `kind` graded, its log wheels on for a
/// log row, its Corrector off where `variant` says, on `surface`'s page
/// (the Wheels or Warper tab).
fn fixture(kind: NodeKind, desc: &ParamDesc, surface: Surface, sector: bool) -> Result<Grade, String> {
    let (mut state, clip, node) = if surface == Surface::GradePerNode { placed(kind) } else { super::grade_inspector_tests::graded(kind) }.ok_or("the grade won't take one")?;
    let corrector_off = corrector_off(kind, surface);
    let log = is_log(desc);
    let c = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip)).unwrap();
    klipp_timeline::GradeAccess::edit_grade(c, |g| {
        for n in &mut g.nodes {
            if corrector_off && n.kind == NodeKind::Corrector {
                n.enabled = false;
            }
            if log && n.id == node {
                n.params.wheels_log_mode = Some(true);
                // A range shapes its log wheel's reach: with the wheel
                // turned, moving the range moves the picture.
                match desc.key {
                    "log_low_range" => n.params.log_low_master = Some(0.3),
                    "log_high_range" => n.params.log_high_master = Some(-0.3),
                    _ => {}
                }
            }
        }
    });
    let tab = if sector { ColorTab::Warper } else { ColorTab::Wheels };
    let page = match surface {
        Surface::GradePerNode => Page::PerNode,
        Surface::GradeTabbedRight => Page::Tabbed(tab),
        _ => Page::Bottom(tab),
    };
    Ok(Grade::new(state, clip, node, true).on(page))
}

/// The wheel labelled `label`: its disc's centre, a point on its master's
/// track at rest, and its diameter. The label is centred over the disc,
/// and the master's "Y" sits `38 + diameter` below it
/// (`color_wheel_sized`).
struct Wheel {
    centre: egui::Pos2,
    master: egui::Pos2,
    diameter: f32,
}

fn wheel(g: &mut Grade, label: &str) -> Result<Wheel, String> {
    let drawn = g.drawn();
    let mut labels: Vec<egui::Pos2> = drawn.iter().filter(|(t, _)| t == label).map(|(_, p)| *p).collect();
    labels.sort_by(|a, b| (a.y.round(), a.x).partial_cmp(&(b.y.round(), b.x)).unwrap());
    for at in labels {
        // Its "Y" is left of the label, the nearest one on the first row
        // of them below it.
        let master = drawn.iter().filter(|(t, p)| t == "Y" && p.y > at.y + 60.0 && p.y < at.y + 260.0 && p.x < at.x && at.x - p.x < 150.0).map(|(_, p)| *p).min_by(|a, b| (a.y.round(), at.x - a.x).partial_cmp(&(b.y.round(), at.x - b.x)).unwrap());
        if let Some(y) = master {
            let diameter = y.y - at.y - 38.0;
            return Ok(Wheel { centre: egui::pos2(at.x, at.y + 7.0 + diameter / 2.0), master: egui::pos2(at.x - 8.0, y.y), diameter });
        }
    }
    Err(format!("no {label:?} wheel"))
}

/// A press at `from`, moved by `by` over three frames, then released, or
/// taken back by Escape first.
fn drag_from(g: &mut Grade, from: egui::Pos2, by: egui::Vec2, escape: bool) {
    g.frame(vec![egui::Event::PointerMoved(from)]);
    g.frame(vec![button(from, true)]);
    for i in 1..=3 {
        g.frame(vec![egui::Event::PointerMoved(from + by * i as f32 / 3.0)]);
    }
    if escape {
        g.frame(vec![key(egui::Key::Escape, egui::Modifiers::NONE)]);
    }
    g.frame(vec![button(from + by, false)]);
    g.frame(vec![egui::Event::PointerGone]);
}

fn secondary_click(g: &mut Grade, at: egui::Pos2) {
    let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE };
    g.frame(vec![egui::Event::PointerMoved(at), press(true)]);
    g.frame(vec![egui::Event::PointerMoved(at), press(false)]);
    g.frame(vec![egui::Event::PointerGone]);
}

/// The slider under the `wheel`th wheel for `axis`: "S" and "B" under the
/// per-node and tabbed wheels (`wheel_block`), "Saturation" and
/// "Brightness" under the bottom tools' (`show_wheels_wide`). A point on
/// its track: left of its value, which is drawn right of the track in
/// both.
fn under(g: &mut Grade, wheel: usize, axis: WheelAxis) -> Result<egui::Pos2, String> {
    let wide = matches!(g.page, Page::Bottom(_));
    let label = match (axis, wide) {
        (WheelAxis::Saturation, false) => "S",
        (WheelAxis::Saturation, true) => "Saturation",
        (_, false) => "B",
        (_, true) => "Brightness",
    };
    let (_, value) = row_at(g, label, wheel)?;
    Ok(value - egui::vec2(35.0, 0.0))
}

/// The warper's centre and ring radius, exactly: its sector labels sit 12
/// px outside a ring of a whole-pixel radius (`show_warper_widget`), "R"
/// level with the centre and "C" opposite it. Read off them, not averaged
/// from all twelve, whose trigonometry rounds differently wherever the
/// widget is drawn: the same point relative to the centre in each view
/// is then the same pointer offset, to the bit.
fn warper(g: &mut Grade) -> Result<(egui::Pos2, f32), String> {
    let drawn = g.drawn();
    let at = |name: &str| drawn.iter().rev().find(|(t, _)| t == name).map(|(_, p)| *p).ok_or_else(|| format!("the warper's {name:?} label isn't drawn"));
    let (r, c) = (at("R")?, at("C")?);
    let out = ((r.x - c.x) / 2.0).round();
    Ok((egui::pos2(r.x - out, r.y), out - 12.0))
}

/// `v` on a 1/64-pixel grid: a centre plus it, less the centre, is it
/// again, exactly.
fn grid(v: egui::Vec2) -> egui::Vec2 {
    egui::vec2((v.x * 64.0).round() / 64.0, (v.y * 64.0).round() / 64.0)
}

/// Sector `index`'s point at rest, and a move from it: across the ring
/// for its hue, out from the centre for its saturation.
fn sector(g: &mut Grade, index: usize, sat: bool) -> Result<(egui::Pos2, egui::Vec2), String> {
    let (centre, radius) = warper(g)?;
    let a = (index as f32 * 30.0).to_radians();
    let out = egui::vec2(a.cos(), -a.sin());
    let across = egui::vec2(-a.sin(), -a.cos());
    Ok((centre + grid(out * radius * 0.7), grid(if sat { out } else { across } * radius * 0.12)))
}

/// The warper's reset: the icon at the right of its "COLOUR WARPER"
/// header (`warper_block`).
fn warper_reset(g: &mut Grade) -> Result<(), String> {
    let drawn = g.drawn();
    let header = drawn.iter().find(|(t, _)| t == "COLOUR WARPER").map(|(_, p)| *p).ok_or("no COLOUR WARPER header")?;
    let at = g.icons.iter().filter(|r| (r.center().y - header.y).abs() < 6.0 && r.center().x > header.x).map(|r| r.center()).max_by(|a, b| a.x.total_cmp(&b.x)).ok_or("no reset on the warper's header")?;
    g.click_at(at);
    Ok(())
}

/// `control` dragged, released or taken back by Escape.
fn drag(g: &mut Grade, control: Control, escape: bool) -> Result<(), String> {
    match control {
        Control::Wheel { label, axis } => {
            // Whole pixels, as a user's: the views draw a wheel at one size,
            // so the same pixels store the same value.
            let w = wheel(g, label)?;
            match axis {
                WheelAxis::X => drag_from(g, w.centre, egui::vec2(18.0, 0.0), escape),
                WheelAxis::Y => drag_from(g, w.centre, egui::vec2(0.0, 18.0), escape),
                _ => drag_from(g, w.master, egui::vec2(18.0, 0.0), escape),
            }
        }
        Control::Under { wheel, axis } => {
            let at = under(g, wheel, axis)?;
            drag_from(g, at, egui::vec2(-12.0, 0.0), escape);
        }
        Control::Sector { index, sat } => {
            let (at, by) = sector(g, index, sat)?;
            drag_from(g, at, by, escape);
        }
        Control::Range => unreachable!("a range is a number row"),
    }
    Ok(())
}

/// `control`'s reset: a right click on the wheel (its puck and master
/// together) or on the slider, or the warper's header reset.
fn reset(g: &mut Grade, control: Control) -> Result<(), String> {
    match control {
        Control::Wheel { label, .. } => {
            let w = wheel(g, label)?;
            secondary_click(g, w.centre);
        }
        Control::Under { wheel, axis } => {
            let at = under(g, wheel, axis)?;
            secondary_click(g, at);
        }
        Control::Sector { .. } => warper_reset(g)?,
        Control::Range => unreachable!("a range is a number row"),
    }
    Ok(())
}

/// `p` as a JSON value, for comparing two views' pictures to a tolerance.
fn json(p: &GradePicture) -> serde_json::Value {
    match p {
        GradePicture::Clip(l) => serde_json::json!({ "picture": serde_json::to_value(&l.picture).unwrap(), "versions": l.versions }),
        GradePicture::Root(v) => v.clone(),
    }
}

/// Where `a` and `b` first differ, as a path.
fn difference(a: &serde_json::Value, b: &serde_json::Value, path: &str) -> String {
    use serde_json::Value::*;
    match (a, b) {
        (Array(x), Array(y)) if x.len() == y.len() => x.iter().zip(y).enumerate().find(|(_, (x, y))| x != y).map_or_else(std::string::String::new, |(i, (x, y))| difference(x, y, &format!("{path}/{i}"))),
        (Object(x), Object(y)) => x.keys().chain(y.keys()).find(|k| x.get(*k) != y.get(*k)).map_or_else(std::string::String::new, |k| match (x.get(k), y.get(k)) {
            (Some(v), Some(w)) => difference(v, w, &format!("{path}/{k}")),
            (v, w) => format!("{path}/{k}: {v:?} and {w:?}"),
        }),
        _ => format!("{path}: {a} and {b}"),
    }
}

/// An edit through both views (`drive`), from `start`, as
/// `parity_grade_numeric::edit_case` holds it: the same picture exactly,
/// one undo step each, and the generated one's undo and redo exact.
fn edit_case(start: &Grade, desc: &ParamDesc, drive: impl Fn(&mut Grade) -> Result<(), String>) -> Result<(), String> {
    let (mut new, mut old) = (start.twin(true), start.twin(false));
    new.drawn();
    old.drawn();
    let before = new.picture();
    drive(&mut new)?;
    drive(&mut old).map_err(|e| format!("the old panel: {e}"))?;
    if !new.unapplied.is_empty() || !old.unapplied.is_empty() {
        return Err(format!("unapplied: new {:?}, old {:?}", new.unapplied, old.unapplied));
    }
    let steps = |g: &Grade| g.state.active_history().undo_count();
    let after = new.picture();
    if after == before {
        return Err("the control changed nothing".into());
    }
    let (a, b) = (json(&after), json(&old.picture()));
    if a != b {
        return Err(format!("the views store {} (new) and {} (old), first apart at {}", new.value(desc), old.value(desc), difference(&a, &b, "")));
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

fn interaction(control: Control) -> Interaction {
    match control {
        Control::Wheel { axis, .. } | Control::Under { axis, .. } => Interaction::WheelDrag(axis),
        _ => Interaction::Drag,
    }
}

/// One row's cases on `start` (the row drawn), counted on `surface`.
fn control_cases(report: &mut Report, ty: &NodeType, desc: &'static ParamDesc, control: Control, start: &Grade, surface: Surface, variant: &str) {
    let id = ty.desc.id;
    let k = |interaction| CoverageKey::new(id, desc.key, surface, variant, interaction);
    let name = |what: &str| format!("{id} {} on {surface:?} {what}", desc.key);
    report.case(&name("opened"), vec![k(Interaction::OpenWithoutWrite)], || {
        // Both views draw the control at one size, so a pixel moves it as
        // far in either: a layout change can't bring that difference back.
        let size = |g: &mut Grade| -> Result<f32, String> {
            match control {
                Control::Wheel { label, .. } => Ok(wheel(g, label)?.diameter),
                Control::Sector { .. } => Ok(warper(g)?.1),
                _ => Ok(0.0),
            }
        };
        let (new, old) = (size(&mut start.twin(true))?, size(&mut start.twin(false))?);
        if new != old {
            return Err(format!("drawn at {new} px (new) and {old} px (old)"));
        }
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
    report.case(&name("dragged"), vec![k(interaction(control)), k(Interaction::UndoRedo)], || {
        edit_case(start, desc, |g| {
            let was = g.value(desc);
            drag(g, control, false)?;
            if g.value(desc) == was {
                return Err(format!("the drag moved another value, not {} (still {was})", desc.key));
            }
            Ok(())
        })
    });
    report.case(&name("reset"), vec![k(Interaction::Reset), k(Interaction::UndoRedo)], || {
        // From a dragged value, so the reset changes it.
        let mut away = start.twin(true);
        drag(&mut away, control, false)?;
        if !away.unapplied.is_empty() {
            return Err(format!("setting it away: unapplied {:?}", away.unapplied));
        }
        let away = away.twin(true);
        edit_case(&away, desc, |g| {
            reset(g, control)?;
            let now = g.value(desc).as_f64().unwrap_or(f64::NAN);
            if now.abs() > 1e-6 {
                return Err(format!("{} reset to {now}, not 0", desc.key));
            }
            Ok(())
        })
    });
    report.case(&name("cancelled"), vec![k(Interaction::CancelDrag)], || {
        // The generated view takes the drag back on Escape: the picture from
        // before it, no step (IC14). The old panel is held alike where both
        // views run one widget's gesture, which is every one here.
        let mut g = start.twin(true);
        g.drawn();
        let before = g.picture();
        drag(&mut g, control, true)?;
        if g.picture() != before || g.state.active_history().undo_count() != 0 || !g.unapplied.is_empty() {
            return Err(format!("Escape didn't take the generated drag back ({} steps, unapplied {:?})", g.state.active_history().undo_count(), g.unapplied));
        }
        alike(start, |g| drag(g, control, true))
    });
}

pub(super) fn run(report: &mut Report) {
    for (kind, surfaces) in TYPES {
        let Some(ty) = klipp_nodes::registry::node_type(*kind) else { continue };
        for (desc, control) in rows(ty) {
            for surface in *surfaces {
                let variant = variant(ty, desc, *surface);
                let start = match fixture(*kind, desc, *surface, matches!(control, Control::Sector { .. })) {
                    Ok(start) => start,
                    Err(e) => {
                        report.case(&format!("{} {} on {surface:?}", ty.desc.id, desc.key), [], || Err(e));
                        continue;
                    }
                };
                match control {
                    Control::Range => row_cases(report, ty, desc, &start, *surface, variant),
                    _ => control_cases(report, ty, desc, control, &start, *surface, variant),
                }
            }
        }
    }
}

#[test]
fn grade_wheels_and_warper_edit_as_the_old_panel() {
    let mut manifest = Manifest::default();
    require(&mut manifest);
    let mut report = Report::default();
    report.family("grade wheels", run);
    report.family_result().unwrap();
    assert!(!report.passed.is_empty(), "no wheel or sector was driven");
    assert!(report.passed.iter().all(|key| manifest.required.contains(key)));
}

#[test]
fn grade_wheels_and_warper_edit_as_the_old_panel_on_the_sequences_grades() {
    let mut report = Report::default();
    super::parity_controls::on_roots(&mut report, run);
    report.family_result().unwrap();
    super::parity_controls::assert_on_every_root(&report);
}
