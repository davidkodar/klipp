//! A grade's wheels and its Colour Warper in the inspector (M3): every
//! wheel's axes (its puck across and down, its luma master), the
//! saturation and brightness under a wheel, the log wheels' ranges, and
//! each of the warper's twelve sectors, on each page that draws them. A
//! wheel is driven as a user drives it, by a drag in its disc or along its
//! master; a sector by a drag from its point, across the ring (its hue) or
//! out from the centre (its saturation). Each is opened (nothing written),
//! dragged, reset and cancelled by Escape, and held to what the edit must
//! store (`parity_controls::oracle_case`): a puck by its rule, the rest by
//! having moved, then the picture writing it by path leaves, one undo
//! step, and an exact undo and redo. Each drag is drawn on both render
//! routes, which must change and agree (`parity_pixels::edit_drawn`). Run
//! through `on_roots` too, so the sequence's grades are earned alike.
use super::parity_controls::{grade_variant, oracle_case, placed, Grade, Page};
use super::parity_grade_numeric::{button, key, row_at, row_cases, shown_in_frame};
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
    (NodeKind::WheelSatBri, &[Surface::GradePerNode, Surface::GradeBottomTools]),
    (NodeKind::ColorWarper, &[Surface::GradePerNode, Surface::GradeTabbedRight, Surface::GradeBottomTools]),
    // The picked zone's wheel (#240): its puck the colour, its Y the
    // exposure.
    (NodeKind::HdrZones, &[Surface::GradePerNode]),
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
    // An HDR zone's wheel, titled "Colour" whichever zone is picked.
    if ty.kind == NodeKind::HdrZones {
        let axis = if desc.key.ends_with("_colour_x") {
            WheelAxis::X
        } else if desc.key.ends_with("_colour_y") {
            WheelAxis::Y
        } else if desc.key.ends_with("_exposure") {
            WheelAxis::Master
        } else {
            return None;
        };
        return Some(Control::Wheel { label: "Colour", axis });
    }
    // A Wheel Sat/Bri node's cells are presented as sliders, and drawn as
    // cells under wide wheels, or numeric rows on their own narrow page.
    if !matches!(pres.widget, Widget::WheelPart(_)) && ty.kind != NodeKind::WheelSatBri {
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
    Ok(Grade::new(state, clip, node).on(page).picking(desc.key))
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

/// A press at `from`, moved by `by` in one move, then released, or taken
/// back by Escape first: one move is one step of the control's rule.
fn drag_from(g: &mut Grade, from: egui::Pos2, by: egui::Vec2, escape: bool) -> Result<(), String> {
    g.frame(vec![egui::Event::PointerMoved(from)]);
    let before = g.picture();
    g.frame(vec![button(from, true)]);
    g.frame(vec![egui::Event::PointerMoved(from + by)]);
    if escape {
        // A drag taken back must have moved first, or Escape proves nothing.
        if g.picture() == before {
            return Err("the drag hadn't moved anything before Escape".into());
        }
        g.frame(vec![key(egui::Key::Escape, egui::Modifiers::NONE)]);
    }
    g.frame(vec![button(from + by, false)]);
    g.frame(vec![egui::Event::PointerGone]);
    Ok(())
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
    drag_by(g, control, 1.0, escape)
}

/// [`drag`] the other way when `sign` is -1: the same press, the move
/// reversed.
fn drag_by(g: &mut Grade, control: Control, sign: f32, escape: bool) -> Result<(), String> {
    match control {
        Control::Wheel { label, axis } => {
            // Whole pixels, as a user's: the views draw a wheel at one size,
            // so the same pixels store the same value.
            let w = wheel(g, label)?;
            match axis {
                WheelAxis::X => drag_from(g, w.centre, egui::vec2(PUCK_DRAG, 0.0) * sign, escape)?,
                WheelAxis::Y => drag_from(g, w.centre, egui::vec2(0.0, PUCK_DRAG) * sign, escape)?,
                _ => drag_from(g, w.master, egui::vec2(PUCK_DRAG, 0.0) * sign, escape)?,
            }
        }
        Control::Under { wheel, axis } => {
            let at = under(g, wheel, axis)?;
            drag_from(g, at, egui::vec2(-12.0, 0.0) * sign, escape)?;
        }
        Control::Sector { index, sat } => {
            let (at, by) = sector(g, index, sat)?;
            drag_from(g, at, by * sign, escape)?;
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

/// What a drag must store, where the control's rule is plain from what's
/// drawn: a puck moves by the drag over the wheel's radius (`color_wheel`:
/// a full radius of travel is half the range), from where it is. `None`
/// for a control whose rule goes through its own layout (a master's
/// track, a rail, a sector), held to moving instead.
fn drag_rule(g: &mut Grade, control: Control, start: f32) -> Result<Option<f32>, String> {
    match control {
        Control::Wheel { label, axis: WheelAxis::X | WheelAxis::Y } => Ok(Some(start + PUCK_DRAG / (wheel(g, label)?.diameter / 2.0))),
        _ => Ok(None),
    }
}

/// Whether [`drag`] stores more than its reverse: a master's or a sector's
/// drag goes up its scale (to the right; across the ring; out from the
/// centre), a rail's to the left, down it.
fn rises(control: Control) -> bool {
    !matches!(control, Control::Under { .. })
}

/// The pixels a wheel's puck or master is dragged by.
const PUCK_DRAG: f32 = 18.0;

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
    let number = |g: &Grade| g.value(desc).as_f64().map(|v| v as f32).ok_or_else(|| format!("{} isn't a number", desc.key));
    report.case(&name("opened"), vec![k(Interaction::OpenWithoutWrite)], || {
        let mut g = start.twin();
        let before = g.picture();
        g.drawn();
        g.drawn();
        if g.picture() != before || g.state.active_history().undo_count() != 0 || !g.unapplied.is_empty() {
            return Err("opening wrote".into());
        }
        Ok(())
    });
    // Shown under a condition (a linear wheel with log off, a log one with
    // it on): the wheel comes and goes with it, found by its name.
    if ty.presentation(desc).visible_if.is_some() {
        let wheel = match control {
            Control::Wheel { label, .. } => Some(label),
            Control::Under { wheel, .. } => WHEELS.get(wheel).map(|w| w.1),
            _ => None,
        };
        report.case(&name("shown with its condition"), vec![k(Interaction::ConditionalVisibility)], || {
            super::parity_controls::visibility_case(start, ty, desc, wheel.ok_or("a conditional control with no name to find")?)
        });
    }
    report.case(&name("dragged"), vec![k(interaction(control)), k(Interaction::UndoRedo)], || {
        let from = number(start)?;
        // What it must store: the puck's rule; where the rule goes through
        // the control's own layout (a master's track, a rail, a sector),
        // the order a drag and its reverse must store in (`rises`), which
        // the control's arithmetic doesn't decide. The value reached is
        // then only where the path write goes: the picture it leaves, one
        // step and nothing else changed are held by `oracle_case`.
        let mut probe = start.twin();
        let want = match drag_rule(&mut probe, control, from)? {
            Some(want) => want,
            None => {
                let mut back = start.twin();
                drag(&mut probe, control, false)?;
                drag_by(&mut back, control, -1.0, false)?;
                let (to, other) = (number(&probe)?, number(&back)?);
                let (high, low, what) = match rises(control) {
                    true => (to, other, "the drag over its reverse"),
                    false => (other, to, "the reverse over the drag"),
                };
                if high <= low {
                    return Err(format!("{what} must store more: {} {to}, reversed {other}", desc.key));
                }
                to
            }
        };
        oracle_case(start, desc, want, |g| drag(g, control, false))
    });
    // The drag drawn: it changes the frame, and the CPU draws it as the GPU
    // does, through the stage the grade is on, with what the control needs
    // to show set first (`shown_in_frame`).
    report.case_or_blocked(&name("pixels"), [k(Interaction::Pixels)], || {
        let start = &shown_in_frame(start, ty, desc);
        let mut new = start.twin();
        new.drawn();
        drag(&mut new, control, false)?;
        super::parity_pixels::edit_drawn(&start.frame_sequence(), &new.frame_sequence())
    });
    report.case(&name("reset"), vec![k(Interaction::Reset), k(Interaction::UndoRedo)], || {
        // From a dragged value, so the reset changes it.
        let mut away = start.twin();
        drag(&mut away, control, false)?;
        if !away.unapplied.is_empty() {
            return Err(format!("setting it away: unapplied {:?}", away.unapplied));
        }
        oracle_case(&away.twin(), desc, 0.0, |g| reset(g, control))
    });
    // Its diamond keys it (#891): on its type's own page, the clip's grade,
    // as a grade row's is (`rules::GRADE_KEY_REPRESENTATIVE`).
    if desc.keyframable && start.root == GradeRoot::Clip && super::parity_manifest::primary_grade_page(id, desc.key) == Some(surface) && matches!(control, Control::Wheel { .. } | Control::Under { .. }) {
        report.case(&name("keyed by its diamond"), vec![k(Interaction::AddKey)], || key_case(start, desc, control));
    }
    report.case(&name("cancelled"), vec![k(Interaction::CancelDrag)], || {
        // Escape takes the drag back: the picture from before it, no step.
        let mut g = start.twin();
        g.drawn();
        let before = g.picture();
        drag(&mut g, control, true)?;
        if g.picture() != before || g.state.active_history().undo_count() != 0 || !g.unapplied.is_empty() {
            return Err(format!("Escape didn't take the drag back ({} steps, unapplied {:?})", g.state.active_history().undo_count(), g.unapplied));
        }
        Ok(())
    });
}

/// The diamond that keys `control`: a wheel's, right of its title (its
/// puck and master keyed together); a rail's under a wheel, the nearest
/// right of its letter or title on its line.
fn diamond(g: &mut Grade, control: Control) -> Result<egui::Pos2, String> {
    let (title, nth) = match control {
        Control::Wheel { label, .. } => (label, 0),
        Control::Under { wheel, axis } => {
            let wide = matches!(g.page, Page::Bottom(_));
            let title = match (axis, wide) {
                (WheelAxis::Saturation, false) => "S",
                (WheelAxis::Saturation, true) => "Saturation",
                (_, false) => "B",
                (_, true) => "Brightness",
            };
            (title, wheel)
        }
        _ => return Err("only a wheel or a rail under one has a diamond".into()),
    };
    let drawn = g.drawn();
    let mut labels: Vec<egui::Pos2> = drawn.iter().filter(|(t, _)| t == title).map(|(_, p)| *p).collect();
    labels.sort_by(|a, b| (a.y.round(), a.x).partial_cmp(&(b.y.round(), b.x)).unwrap());
    let beside = |at: &egui::Pos2| g.diamonds.iter().map(|d| d.center()).filter(|d| (d.y - at.y).abs() < 6.0 && d.x > at.x).min_by(|a, b| a.x.total_cmp(&b.x));
    // Labels with a diamond beside them: a wheel's title (not a group
    // header of the same name), each rail's letter.
    labels.iter().filter_map(beside).nth(nth).ok_or_else(|| format!("no diamond beside {title:?} #{nth}"))
}

/// A wheel's or a rail's diamond keys its parameter at the frame's time
/// with the value it shows, as one undo step undone exactly.
fn key_case(start: &Grade, desc: &ParamDesc, control: Control) -> Result<(), String> {
    const AT: f64 = 0.5;
    let mut g = start.twin();
    g.drawn();
    let before = g.picture();
    let value = g.value(desc).as_f64().map(|v| v as f32);
    let at = diamond(&mut g, control)?;
    g.click_at(at);
    if !g.unapplied.is_empty() {
        return Err(format!("unapplied {:?}", g.unapplied));
    }
    let keys = g.grade().nodes.iter().find(|n| n.id == g.node).and_then(|n| n.keyframes.tracks.iter().find(|t| t.param_name == desc.key)).map(|t| t.keyframes.clone()).unwrap_or_default();
    if keys.len() != 1 || (keys[0].time_secs - AT).abs() > 1e-9 || value.is_some_and(|v| (keys[0].value - v).abs() > 1e-4) {
        return Err(format!("the diamond didn't key {} at {AT} s with its value {value:?}: {keys:?}", desc.key));
    }
    if g.state.active_history().undo_count() != 1 {
        return Err(format!("{} undo steps, not one", g.state.active_history().undo_count()));
    }
    g.step(false)?;
    if g.picture() != before {
        return Err("undo doesn't give back the grade before".into());
    }
    Ok(())
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
                    _ if *kind == NodeKind::WheelSatBri && *surface == Surface::GradePerNode => row_cases(report, ty, desc, &start, *surface, variant),
                    Control::Range => row_cases(report, ty, desc, &start, *surface, variant),
                    _ => control_cases(report, ty, desc, control, &start, *surface, variant),
                }
            }
        }
    }
}

#[test]
fn grade_wheels_and_warper_store_what_each_drag_must() {
    let mut manifest = Manifest::default();
    require(&mut manifest);
    let mut report = Report::default();
    report.family("grade wheels", run);
    report.family_result().unwrap();
    assert!(!report.passed.is_empty(), "no wheel or sector was driven");
    assert!(report.passed.iter().all(|key| manifest.required.contains(key)));
}

#[test]
fn grade_wheels_and_warper_store_what_each_drag_must_on_the_sequences_grades() {
    let mut report = Report::default();
    super::parity_controls::on_roots(&mut report, run);
    report.family_result().unwrap();
    super::parity_controls::assert_on_every_root(&report);
}

/// An HDR Zones page (#240): Highlight picked from its zone menu, its
/// wheel's Y dragged, stores that zone's exposure and no other zone's, one
/// step; and the grade then moves only what Highlight reaches, bit for bit
/// elsewhere. Control: before the pick, the same drag is Global's.
#[test]
fn a_zone_picked_from_the_menu_and_its_exposure_dragged_moves_only_that_zone() {
    use klipp_color::grade_model::hdr_zones::{stops, weight};
    let (state, clip, node) = placed(NodeKind::HdrZones).expect("a grade takes one");
    let zones = |g: &Grade| g.grade().nodes.iter().find(|n| n.id == g.node).and_then(|n| n.params.hdr_zones.clone()).unwrap_or_default();
    let fresh = Grade::new(state, clip, node);

    // Control: unpicked, the page is Global's.
    let mut global = fresh.twin();
    let w = wheel(&mut global, "Colour").unwrap();
    drag_from(&mut global, w.master, egui::vec2(PUCK_DRAG, 0.0), false).unwrap();
    let z = zones(&global);
    assert!(z.exposure[0] > 0.0 && z.exposure[1..].iter().all(|e| *e == 0.0), "Global's: {:?}", z.exposure);

    let mut g = fresh.twin();
    let drawn = g.drawn();
    let at = drawn.iter().find(|(t, _)| t == "Highlight").map(|(_, p)| *p).expect("the zone menu's Highlight");
    g.click_at(at);
    let w = wheel(&mut g, "Colour").unwrap();
    drag_from(&mut g, w.master, egui::vec2(PUCK_DRAG, 0.0), false).unwrap();
    assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
    let z = zones(&g);
    assert!(z.exposure[5] > 0.0, "Highlight's exposure stored: {:?}", z.exposure);
    assert!(z.exposure.iter().enumerate().all(|(i, e)| i == 5 || *e == 0.0), "no other zone's: {:?}", z.exposure);
    assert_eq!(g.state.active_history().undo_count(), 1, "one step");

    // The grade moves only what Highlight reaches.
    let luma = klipp_color::primaries::luminance_weights(klipp_core::WorkingColorspace::Rec709Linear);
    let input: Vec<f32> = (0..64).flat_map(|i| {
        let v = 0.18 * (-8.0 + 16.0 * i as f32 / 63.0).exp2();
        [v * 1.1, v, v * 0.9, 1.0]
    }).collect();
    let node = g.grade().nodes.iter().find(|n| n.id == g.node).unwrap().clone();
    let mut graph = klipp_nodes::NodeGraph::default();
    graph.splice_after_source(node);
    let out = klipp_nodes::GraphExecutor::new_cpu_only().execute_linear(&input, &graph, 64, 1, 0.0, 0, None, None).unwrap().0;
    let (mut moved, mut kept) = (0, 0);
    for (a, b) in out.chunks_exact(4).zip(input.chunks_exact(4)) {
        let s = stops(luma[0] * b[0] + luma[1] * b[1] + luma[2] * b[2]);
        if weight(5, s, z.boundary[5], z.falloff[5]) > 0.0 {
            moved += usize::from(a != b);
        } else {
            assert_eq!(a, b, "a pixel at {s} stops Highlight doesn't reach");
            kept += 1;
        }
    }
    assert!(moved > 0 && kept > 0, "moved {moved}, kept {kept}");
}

/// An HDR Zones page's Start from (#240): a preset picked from it sets
/// every zone as the preset has them, one step, undone whole. Control:
/// opening the menu writes nothing.
#[test]
fn a_zones_preset_picked_from_start_from_sets_every_zone_in_one_step() {
    let (state, clip, node) = placed(NodeKind::HdrZones).expect("a grade takes one");
    let mut g = Grade::new(state, clip, node);
    let zones = |g: &Grade| g.grade().nodes.iter().find(|n| n.id == g.node).and_then(|n| n.params.hdr_zones.clone()).unwrap_or_default();
    let before = g.picture();
    let drawn = g.drawn();
    let at = drawn.iter().find(|(t, _)| t == "Start from").map(|(_, p)| *p).expect("Start from");
    g.click_at(at);
    assert_eq!(g.picture(), before, "opening the menu writes nothing");
    let drawn = g.drawn();
    let item = drawn.iter().find(|(t, _)| t == "Lift shadows").map(|(_, p)| *p).expect("the preset listed");
    g.click_at(item);
    assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
    assert_eq!(zones(&g), klipp_core::HdrZonesSettings::preset("Lift shadows", false).unwrap());
    assert_eq!(g.state.active_history().undo_count(), 1, "one step");
    g.step(false).unwrap();
    assert_eq!(g.picture(), before, "undone whole");
}

/// #1351: the selected Sat/Bri node earns its numeric page independently
/// of the wide wheel cells: typing, dragging, reset, keys and exact undo.
#[test]
fn wheel_sat_bri_narrow_rows_store_each_edit() {
    assert!(super::main_layout::per_node_kind(NodeKind::WheelSatBri));
    let ty = klipp_nodes::registry::node_type(NodeKind::WheelSatBri).unwrap();
    let mut manifest = Manifest::default();
    require(&mut manifest);
    let mut report = Report::default();
    for (desc, _) in rows(ty) {
        let surface = Surface::GradePerNode;
        let start = fixture(ty.kind, desc, surface, false).unwrap();
        row_cases(&mut report, ty, desc, &start, surface, variant(ty, desc, surface));
    }
    report.family_result().unwrap();
    assert!(!report.passed.is_empty());
    assert!(report.passed.iter().all(|key| manifest.required.contains(key)));
}

/// Sequence grades use the same rows through their own document roots;
/// editing one value must preserve the others and undo/redo exactly.
#[test]
fn wheel_sat_bri_narrow_rows_edit_sequence_grades() {
    let ty = klipp_nodes::registry::node_type(NodeKind::WheelSatBri).unwrap();
    for root in GradeRoot::SEQUENCE {
        for (desc, _) in rows(ty) {
            let mut start = fixture(ty.kind, desc, Surface::GradePerNode, false).unwrap().on_root(root);
            let before = start.picture();
            start.drawn();
            assert_eq!(start.picture(), before, "opening must preserve values");
            let label = ty.presentation(desc).label.unwrap_or(desc.label);
            oracle_case(&start, desc, 0.25, |g| super::parity_grade_numeric::type_into(g, label, super::parity_grade_numeric::nth(desc), "0.25"))
                .unwrap_or_else(|e| panic!("{root:?} {}: {e}", desc.key));
        }
    }
}
