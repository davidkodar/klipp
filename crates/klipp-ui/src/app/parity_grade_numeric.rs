//! A grade's numeric rows in the inspector (M3): every slider and number
//! of a grade node, on each page that draws it: the per-node layout for a
//! per-node type, and the Corrector's tabbed page and the Colour page's
//! bottom tools for the types they show. Each row is opened (nothing
//! written), typed, dragged, dragged past its end, cancelled by Escape and
//! reset, and held to what that edit must store (`oracle_case`): the
//! picture writing it by path leaves, one undo step, and an exact undo and
//! redo. The typed edit is drawn on the CPU and GPU routes, which must
//! change and agree (`parity_pixels::edit_drawn`). Run through `on_roots`
//! too, so the sequence's grades are earned alike, drawn by their own
//! stage.
//!
//! Wheel axes and the warper's sectors are another driver's (a drag in a
//! disc) and another family's.

use super::parity_controls::{grade_variant, meet_conditions, oracle_case, placed, Grade, Page};
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

/// Grade types whose rows the CPU and the GPU stack draw differently:
/// known defects, their Pixels excluded with the issue until fixed.
const ROUTE_DEFECTS: &[(NodeKind, &str)] = &[];

fn route_defect(ty: &NodeType) -> Option<&'static str> {
    ROUTE_DEFECTS.iter().find(|(kind, _)| *kind == ty.kind).map(|d| d.1)
}

const TABS: [ColorTab; 5] = [ColorTab::Adjustments, ColorTab::Wheels, ColorTab::Curves, ColorTab::HueSat, ColorTab::Warper];

/// A numeric row this family drives: a slider or number, not a wheel's
/// axis or a sector (another family's), not hidden.
fn numeric(ty: &NodeType, desc: &ParamDesc) -> bool {
    matches!(ty.presentation(desc).widget, Widget::Scrub | Widget::Slider | Widget::Gradient(_) | Widget::HueRainbow | Widget::Vec2)
        // The warper's sectors and the wheels' saturation and brightness
        // cells are the wheels family's; Colour Match's values are readouts
        // (#1421), held beside the tool that writes them (the workflows
        // family).
        && !matches!(ty.kind, NodeKind::ColorWarper | NodeKind::WheelSatBri | NodeKind::ColorMatch)
        && !(desc.key.ends_with("_sat") || desc.key.ends_with("_bri"))
        // The log wheels' ranges sit under the wheels (`wheel_shape_sliders`).
        && !matches!(desc.key, "log_low_range" | "log_high_range")
}

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
        if let Some(reason) = route_defect(ty) {
            manifest.exclude_key(CoverageKey::new(entry.type_id, &entry.parameter, surface, variant, Interaction::Pixels), reason);
            for root in GradeRoot::SEQUENCE {
                manifest.exclude_key(CoverageKey::new(entry.type_id, &entry.parameter, Surface::NonClipGrade, &format!("{}: {surface:?} {variant}", root.variant()), Interaction::Pixels), reason);
            }
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
pub(super) fn nth(desc: &ParamDesc) -> usize {
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
pub(super) fn type_into(g: &mut Grade, label: &str, n: usize, text: &str) -> Result<(), String> {
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
    let pressed = g.picture();
    for i in 1..=3 {
        g.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx * i as f32 / 3.0, 0.0))]);
    }
    if escape {
        // A drag taken back must have moved first, or Escape proves nothing.
        if g.picture() == pressed {
            return Err("the drag hadn't moved anything before Escape".into());
        }
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
        let on = base.twin().on(if wide { Page::Bottom(tab) } else { Page::Tabbed(tab) });
        match meet_conditions(&on, ty, desc) {
            Ok((mut shown, variant)) => {
                if shown.drawn().iter().any(|(t, _)| t == label) {
                    return Ok((shown.twin(), variant));
                }
            }
            Err(e) => last = format!("on {tab:?}: {e}"),
        }
    }
    Err(last)
}

/// A drag of the row's number by `dx` in one move, released or taken back
/// by Escape: one move is one step of the drag's rule, so its result is
/// `grade_drag(start, dx, span)` exactly.
fn drag_once(g: &mut Grade, label: &str, n: usize, dx: f32, escape: bool) -> Result<(), String> {
    let (_, at) = row_at(g, label, n)?;
    g.frame(vec![egui::Event::PointerMoved(at)]);
    g.frame(vec![button(at, true)]);
    g.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))]);
    if escape {
        g.frame(vec![key(egui::Key::Escape, egui::Modifiers::NONE)]);
    }
    g.frame(vec![button(at + egui::vec2(dx, 0.0), false)]);
    g.frame(vec![egui::Event::PointerGone]);
    Ok(())
}

/// `start` with what the row's edit needs to show in the frame at all, set
/// before the pixel case: a pivot needs a contrast, a
/// mix something to mix, a radius or a grain's shape a strength, a LUT's
/// opacity a LUT, a softness an edge, a difference key a plate, and a
/// matte or a mix a correction to select or mix (the exposure of the other
/// Correctors in the grade). Set through the colour commands,
/// then twinned, so its history starts empty.
pub(super) fn shown_in_frame(start: &Grade, ty: &NodeType, desc: &ParamDesc) -> Grade {
    use klipp_core::color_grade::{CurveSet, FilmGrainSettings, HueSatCurves};
    use klipp_core::AppCommand;
    let mut g = start.twin();
    let (clip_id, node_id) = (g.clip, g.node);
    let param = |param: &str, value: f32| AppCommand::SetNodeParam { clip_id, node_id, param: param.into(), value };
    let mut cmds = match desc.key {
        "contrast_pivot" => vec![param("contrast", 0.5)],
        "global_mix" => vec![param("temperature", 0.5)],
        // A turned linear wheel: the log mode, which reads log wheels
        // instead, then moves the frame.
        "luma_mix" | "wheels_log_mode" => vec![param("gain_master", 0.3)],
        // A coloured curve: preserving luma then changes what it does.
        "curves_preserve_luma" => vec![AppCommand::SetNodeCurves { clip_id, node_id, curves: CurveSet { red: vec![[0.0, 0.0], [0.5, 0.8], [1.0, 1.0]], ..Default::default() } }],
        "denoise_radius" => vec![param("denoise_luma", 0.5)],
        "curves_mix" => vec![AppCommand::SetNodeCurves { clip_id, node_id, curves: CurveSet { master: vec![[0.0, 0.0], [0.5, 0.75], [1.0, 1.0]], ..Default::default() } }],
        "huesat_mix" => vec![AppCommand::SetNodeHueSatCurves { clip_id, node_id, huesat: HueSatCurves { lum_vs_sat: vec![[0.0, 0.5], [1.0, 0.5]], ..Default::default() } }],
        "lut_opacity" | "lut_input_space" | "lut_output_space" => vec![AppCommand::SetNodeLut { clip_id, node_id, path: Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/luts/cinematic/D-CINELIKE -0_3 BASE.cube")) }],
        // A softness needs an edge to soften: a low end above the bottom.
        // A window that keys part of the frame (the default keys it all,
        // as if off): the soft edge then has an edge to soften, and the
        // switch something to switch.
        "luma_key_soft" | "luma_key_enabled" => vec![param("luma_key_low", 0.3)],
        // Two mattes that each select part of the frame, a key's and a
        // window's, so intersecting, joining and subtracting them differ.
        "matte_combine_op" => {
            let grade = g.grade();
            let of = |kind: NodeKind| grade.nodes.iter().find(|n| n.kind == kind).map(|n| n.id);
            let mut cmds = Vec::new();
            if let Some(mask) = of(NodeKind::Mask) {
                let circle = klipp_core::Mask { shape: klipp_core::MaskShape::Circle { center_x: 0.35, center_y: 0.5, radius: 0.25 }, ..Default::default() };
                cmds.push(AppCommand::SetNodeMasks { clip_id, node_id: mask, masks: klipp_core::MaskStack { masks: vec![circle] } });
            }
            if let Some(qualifier) = of(NodeKind::Qualifier) {
                let qualifier_settings = klipp_core::color_grade::QualifierSettings { enabled: true, lum_low: 0.0, lum_high: 0.5, ..Default::default() };
                cmds.push(AppCommand::SetNodeQualifier { clip_id, node_id: qualifier, qualifier: qualifier_settings });
            }
            cmds
        }
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
    // A matte image to read a channel of, or invert: written to the node
    // straight, since the app sets it in its own colour handler, which
    // this harness doesn't run (`SetExternalMattePath`).
    if matches!(desc.key, "matte_channel" | "matte_invert") {
        let image = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/effect_preview_reference.png");
        if let Ok(with_image) = super::parity_controls::with_node(&g, |n| {
            n.params.matte_image_path = Some(image);
            Ok(())
        }) {
            return with_image.twin();
        }
    }
    g.twin()
}

/// One row's cases on `start` (the row drawn), counted on `surface`: the
/// generated view driven, held to what each edit must store (the typed
/// value clamped to the row's range, `grade_drag`'s result, the range's
/// end, `reset_value`), one undo step, and the edit drawn alike on the
/// CPU and the GPU.
pub(super) fn row_cases(report: &mut Report, ty: &NodeType, desc: &'static ParamDesc, start: &Grade, surface: Surface, variant: &str) {
    let id = ty.desc.id;
    let pres = ty.presentation(desc);
    let label = pres.label.unwrap_or(desc.label);
    let n = nth(desc);
    let k = |interaction| CoverageKey::new(id, desc.key, surface, variant, interaction);
    let name = |what: &str| format!("{id} {} on {surface:?} {what}", desc.key);
    let encoding = klipp_grade_model::Encoding::from_u8(klipp_core::grade_encoding().effective(klipp_engine::video_decoder::working_colorspace()).as_u8());
    let (lo, hi) = row::shown_range(desc, &pres, encoding);
    let (slo, shi) = (lo.min(hi), lo.max(hi));
    let to_shown = |v: f32| pres.display.map_or(v, |m| (m.to_shown)(v, encoding));
    let to_stored = |v: f32| pres.display.map_or(v, |m| (m.to_stored)(v, encoding));
    // What a drag of `dx` from `stored` stores: the drag's rule over the
    // row's shown span, clamped to its range (M3-4).
    let dragged_to = |stored: f32, dx: f32| {
        let (sa, sb) = (to_stored(slo), to_stored(shi));
        to_stored(crate::widgets::grade_drag::grade_drag(to_shown(stored), dx, shi - slo).clamp(slo, shi)).clamp(sa.min(sb), sa.max(sb))
    };
    let number = |g: &Grade| g.value(desc).as_f64().map(|v| v as f32).ok_or_else(|| format!("{} isn't a number", desc.key));
    // A value the row shows, apart from its default either way.
    let typed = format!("{:.*}", usize::from(pres.decimals), lo + (hi - lo) * 0.7);
    // A drag toward the middle of the range, so one starting at an end
    // moves; a whole-number row's further, so it moves at least one unit.
    let current = start.value(desc).as_f64().unwrap_or(0.0) as f32;
    let reach = if desc.kind == klipp_effects::registry::ParamKind::Whole { 72.0 } else { 24.0 };
    let dx = if to_shown(current) > (lo + hi) / 2.0 { -reach } else { reach };

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
    // Shown under a condition: it comes and goes with it.
    if pres.visible_if.is_some() {
        report.case(&name("shown with its condition"), vec![k(Interaction::ConditionalVisibility)], || super::parity_controls::visibility_case(start, ty, desc, label));
    }
    report.case(&name("typed"), vec![k(Interaction::TypeNumber), k(Interaction::UndoRedo)], || {
        let shown: f32 = typed.parse().map_err(|_| format!("{typed:?} isn't a number"))?;
        oracle_case(start, desc, to_stored(shown.clamp(slo, shi)), |g| type_into(g, label, n, &typed))
    });
    // The typed edit drawn: it changes the frame, and the CPU draws it as
    // the GPU does, through the stage the grade is on.
    let pixels = || {
        let start = &shown_in_frame(start, ty, desc);
        let mut new = start.twin();
        new.drawn();
        type_into(&mut new, label, n, &typed)?;
        super::parity_pixels::edit_drawn(&start.frame_sequence(), &new.frame_sequence())
    };
    match route_defect(ty) {
        None => report.case_or_blocked(&name("pixels"), [k(Interaction::Pixels)], pixels),
        // Its key excluded with the issue: the edit still changes the frame.
        Some(_) => report.case(&name("pixels, routes apart (pinned)"), [], || super::parity_pixels::with_routes_apart(pixels)),
    }
    report.case(&name("dragged"), vec![k(Interaction::Drag), k(Interaction::UndoRedo)], || {
        let from = number(start)?;
        oracle_case(start, desc, dragged_to(from, dx), |g| drag_once(g, label, n, dx, false))
    });
    // A drag past the row's low end, from just above it (the harness's
    // screen is narrower than a span): the end, never a value beyond it.
    report.case(&name("dragged past the end"), vec![k(Interaction::Drag)], || {
        // At least one shown step above the end, which a whole-number row
        // would otherwise round down to.
        let step = if desc.kind == klipp_effects::registry::ParamKind::Whole { 1.0 } else { 10f32.powi(-i32::from(pres.decimals)) };
        let near = format!("{:.*}", usize::from(pres.decimals), slo + ((shi - slo) * 0.06).max(step));
        let mut set = start.twin();
        type_into(&mut set, label, n, &near)?;
        let near_end = set.twin();
        let from = number(&near_end)?;
        oracle_case(&near_end, desc, dragged_to(from, -100.0), |g| drag_once(g, label, n, -100.0, false))
    });
    report.case(&name("reset"), vec![k(Interaction::Reset), k(Interaction::UndoRedo)], || {
        // From the typed value, so the reset changes it.
        let mut away = start.twin();
        type_into(&mut away, label, n, &typed)?;
        if !away.unapplied.is_empty() {
            return Err(format!("setting it away: unapplied {:?}", away.unapplied));
        }
        oracle_case(&away.twin(), desc, row::reset_value(desc, &pres), |g| reset(g, label, n))
    });
    report.case(&name("cancelled"), vec![k(Interaction::CancelDrag)], || {
        // Escape takes the drag back: the picture from before it, no step.
        let mut g = start.twin();
        g.drawn();
        let before = g.picture();
        drag(&mut g, label, n, dx, true)?;
        if g.picture() != before || g.state.active_history().undo_count() != 0 || !g.unapplied.is_empty() {
            return Err("Escape didn't take the drag back".into());
        }
        Ok(())
    });
    // Its diamond takes a key, on the page the row is first drawn on, in
    // the clip's own grade: the per-type half of the grade key rule (the
    // controls themselves are the representatives', `parity_grade_keys`).
    if desc.keyframable && start.root == GradeRoot::Clip && super::parity_manifest::primary_grade_page(id, desc.key) == Some(surface) {
        report.case(&name("keyed by its diamond"), vec![k(Interaction::AddKey)], || super::parity_grade_keys::add_key_case(start, ty, desc));
    }
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
        let base = Grade::new(state, clip, node);
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
        let base = Grade::new(state, clip, node);
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
fn grade_numeric_rows_store_what_each_edit_must() {
    let mut report = Report::default();
    report.family("grade numeric rows", run);
    report.family_result().unwrap();
    assert!(!report.passed.is_empty(), "no grade numeric row was driven");
}

#[test]
fn grade_numeric_rows_store_what_each_edit_must_on_the_sequences_grades() {
    let mut report = Report::default();
    super::parity_controls::on_roots(&mut report, run);
    report.family_result().unwrap();
    super::parity_controls::assert_on_every_root(&report);
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
