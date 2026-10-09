//! M3 step 18 parity (and step 11c's `workflow_tools_still_write_their_params`):
//! the grade's sampling tools, Auto White Balance and its picker, the
//! qualifier's eyedropper, and Colour Match, as a family that earns their
//! workflow keys on the surfaces each is drawn on. Each reads the viewer's
//! frame in the app, which a headless test doesn't have, so each is held
//! in parts:
//! - its trigger: the one request its control must send, writing nothing;
//! - what it computes: a known frame or colour through the same pure
//!   function the app uses, landed through the same write the app makes,
//!   the values asserted, one step, undone exactly;
//! - its result landing while a grade slider's drag is held (a socket
//!   write, a missed release, a late sample): the drag is closed as its
//!   own step first, the result is one step, and undo walks back both
//!   exactly.
//!
//! Pixels are the picture oracle's family, not this one.

use super::parity_controls::{placed, Grade, Page};
use super::parity_manifest::{catalog, CatalogEntry, CoverageKey, Interaction, Manifest, Report, Surface};
use crate::state::ColorTab;
use klipp_core::AppCommand;
use klipp_nodes::NodeKind;

/// Click the drawn text `text`: its last occurrence.
fn click(g: &mut Grade, text: &str) -> Result<(), String> {
    let at = g.drawn().into_iter().rev().find(|(t, _)| t == text).map(|(_, p)| p).ok_or_else(|| format!("{text:?} isn't drawn"))?;
    g.click_at(at);
    Ok(())
}

/// Click the icon button by `label` (an eyedropper has no text of its
/// own): right of it on its line, or on a line just above, from the
/// left, where the view puts it over the rows. Found on throwaway twins, as the first point whose click sends
/// the request `want` and writes nothing, so a control on the way (the
/// label's own checkbox, a button beside it) is never clicked here.
fn click_icon_by(g: &mut Grade, label: &str, want: &str) -> Result<(), String> {
    let at = g.drawn().into_iter().rev().find(|(t, _)| t == label).map(|(_, p)| p).ok_or_else(|| format!("{label:?} isn't drawn"))?;
    let right = (0..40).map(|i| egui::pos2(at.x + 4.0 + 4.0 * i as f32, at.y));
    let above = (1..=3).flat_map(|row| (0..50).map(move |i| egui::pos2(4.0 + 4.0 * i as f32, at.y - 20.0 * row as f32)));
    for p in right.chain(above) {
        let mut t = g.twin();
        t.drawn();
        let before = t.picture();
        t.click_at(p);
        if t.side.len() == 1 && t.side[0].starts_with(want) && t.picture() == before {
            g.click_at(p);
            return Ok(());
        }
    }
    Err(format!("no {want} button by {label:?}"))
}

fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
}

/// Press on the slider of the row labelled `label` and drag it, holding
/// the button: where the pointer is. The point pressed is found on
/// throwaway twins, as the first right of the label, on its line or just
/// under it, whose drag moves the picture.
fn hold_drag(g: &mut Grade, label: &str) -> Result<egui::Pos2, String> {
    let at = g.drawn().into_iter().rev().find(|(t, _)| t == label).map(|(_, p)| p).ok_or_else(|| format!("no {label:?} row"))?;
    let drag = |g: &mut Grade, p: egui::Pos2| {
        g.frame(vec![egui::Event::PointerMoved(p), button(p, true)]);
        for dx in [8.0, 16.0, 24.0] {
            g.frame(vec![egui::Event::PointerMoved(p + egui::vec2(dx, 0.0))]);
        }
    };
    // On the label's line, or the one under it (a slider's rail).
    for (dy, i) in [0.0, 16.0, 22.0].into_iter().flat_map(|dy| (0..30).map(move |i| (dy, i))) {
        let p = egui::pos2(at.x + 30.0 + 8.0 * i as f32, at.y + dy);
        let mut t = g.twin();
        t.drawn();
        let before = t.picture();
        drag(&mut t, p);
        if t.picture() != before && t.unapplied.is_empty() {
            g.drawn();
            drag(g, p);
            return Ok(p + egui::vec2(24.0, 0.0));
        }
    }
    Err(format!("no slider to drag on the {label:?} row"))
}

/// Go on dragging from `at`, then release and leave.
fn release(g: &mut Grade, at: egui::Pos2) {
    for dx in [8.0, 16.0] {
        g.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))]);
    }
    g.frame(vec![button(at + egui::vec2(16.0, 0.0), false)]);
    g.frame(vec![egui::Event::PointerGone]);
}

/// Opened: drawing the tool's page writes nothing.
fn opened(start: &Grade) -> Result<(), String> {
    let mut g = start.twin();
    let before = g.picture();
    g.drawn();
    g.drawn();
    if !g.unapplied.is_empty() || g.picture() != before || g.state.active_history().undo_count() != 0 {
        return Err(format!("opening wrote ({:?})", g.unapplied));
    }
    Ok(())
}

/// A frame of one colour, `w` by `h`.
fn flat(rgb: [u8; 3], w: u32, h: u32) -> Vec<u8> {
    (0..w * h).flat_map(|_| [rgb[0], rgb[1], rgb[2], 255]).collect()
}

/// The trigger: driving `control` sends the one request `want`, and
/// writes nothing.
fn trigger(start: &Grade, want: &str, control: impl Fn(&mut Grade) -> Result<(), String>) -> Result<(), String> {
    let mut g = start.twin();
    g.drawn();
    let before = g.picture();
    control(&mut g)?;
    if !g.unapplied.is_empty() {
        return Err(format!("unapplied {:?}", g.unapplied));
    }
    if g.side.len() != 1 || !g.side[0].starts_with(want) {
        return Err(format!("sent {:?}, want one {want}", g.side));
    }
    if g.picture() != before || g.state.active_history().undo_count() != 0 {
        return Err("the request wrote".into());
    }
    Ok(())
}

/// The computation landed: `land` writes the result as the app does,
/// `read` is what it set; one step, `read` is `want`, and undo gives back
/// what was there.
fn lands<T: PartialEq + std::fmt::Debug>(start: &Grade, land: impl Fn(&mut Grade) -> Result<(), String>, read: impl Fn(&Grade) -> T, want: impl Fn(&T) -> bool) -> Result<(), String> {
    let mut g = start.twin();
    let before = read(&g);
    land(&mut g)?;
    if !g.unapplied.is_empty() {
        return Err(format!("unapplied {:?}", g.unapplied));
    }
    let now = read(&g);
    if !want(&now) {
        return Err(format!("holds {now:?}"));
    }
    if g.state.active_history().undo_count() != 1 {
        return Err(format!("{} steps, not one", g.state.active_history().undo_count()));
    }
    g.step(false)?;
    if read(&g) != before {
        return Err(format!("undo gives back {:?}, not {before:?}", read(&g)));
    }
    Ok(())
}

/// The steps a held drag and a result leave: the drag closed as its own
/// step before the result, the result one step, the drag going on as one
/// more.
const DRAG_RESULT_STEPS: usize = 3;

/// The history's step labels, top first: unwinding `g`, for a failure's
/// message.
fn labels(g: &mut Grade) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(l) = g.state.active_history().undo_label().map(str::to_owned) {
        out.push(l);
        if g.step(false).is_err() {
            break;
        }
    }
    out
}

/// A result landing (`land`) while a grade slider's drag (`label`, on
/// `start`'s page) is held: the result is kept through the rest of the
/// drag, and undoing every step gives back the grade before, the result's
/// step undoing the result alone.
fn result_during_drag<T: PartialEq + std::fmt::Debug>(start: &Grade, label: &str, land: impl Fn(&mut Grade) -> Result<(), String>, read: impl Fn(&Grade) -> T) -> Result<(), String> {
    let mut g = start.twin();
    let (before, result_before) = (g.picture(), read(&g));
    let at = hold_drag(&mut g, label)?;
    land(&mut g).map_err(|e| format!("landing: {e}"))?;
    let result = read(&g);
    if result == result_before {
        return Err("the result didn't land".into());
    }
    release(&mut g, at);
    if !g.unapplied.is_empty() {
        return Err(format!("unapplied {:?}", g.unapplied));
    }
    let steps = g.state.active_history().undo_count();
    if steps != DRAG_RESULT_STEPS {
        return Err(format!("{steps} steps {:?}, not {DRAG_RESULT_STEPS}", labels(&mut g)));
    }
    if read(&g) != result {
        return Err("the rest of the drag lost the result".into());
    }
    // The drag's second part, then the result alone.
    g.step(false)?;
    if read(&g) != result {
        return Err("undoing the drag's rest took the result".into());
    }
    g.step(false)?;
    if read(&g) != result_before {
        return Err("undoing the result's step left it".into());
    }
    g.step(false)?;
    if g.picture() != before {
        return Err("undoing every step doesn't give back the grade before".into());
    }
    Ok(())
}

/// The key of `type_id`'s workflow `id` on `surface`.
fn key(type_id: &str, id: &str, surface: Surface, interaction: Interaction) -> CoverageKey {
    CoverageKey::new(type_id, &format!("@workflow/{id}"), surface, "sample", interaction)
}

/// The workflow entries and the clip surfaces their tools are drawn on:
/// white balance on the Corrector's Adjustments tab (right and bottom),
/// the qualifier's eyedropper and Colour Match on their per-node pages.
fn entries() -> Vec<(CatalogEntry, Vec<(Surface, Page)>)> {
    let pages = |id: &str| match id {
        "white_balance" => vec![(Surface::GradeTabbedRight, Page::Tabbed(ColorTab::Adjustments)), (Surface::GradeBottomTools, Page::Bottom(ColorTab::Adjustments))],
        _ => vec![(Surface::GradePerNode, Page::PerNode)],
    };
    catalog().into_iter().filter_map(|e| {
        let id = e.parameter.strip_prefix("@workflow/")?.to_owned();
        matches!(id.as_str(), "white_balance" | "qualifier_picker" | "colour_match").then(|| {
            let p = pages(&id);
            (e, p)
        })
    }).collect()
}

/// Colour Match's measured values: readouts on its page (#1421), held
/// here beside the tool that writes them.
fn match_values() -> Vec<CatalogEntry> {
    catalog().into_iter().filter(|e| e.type_id == "klipp.grade.color_match" && matches!(e.value, super::parity_manifest::CatalogValue::Parameter { .. })).collect()
}

pub(super) fn require(manifest: &mut Manifest) {
    for entry in match_values() {
        manifest.require_fixture(&entry, Surface::GradePerNode, "sample");
        for root in super::parity_harness::GradeRoot::SEQUENCE {
            manifest.require_fixture(&entry, Surface::NonClipGrade, &format!("{}: GradePerNode sample", root.variant()));
        }
    }
    for (entry, surfaces) in entries() {
        for (surface, _) in surfaces {
            manifest.require_fixture(&entry, surface, "sample");
            // On the sequence's own grades too (`on_roots`), under the
            // root's variant.
            for root in super::parity_harness::GradeRoot::SEQUENCE {
                let variant = format!("{}: {surface:?} sample", root.variant());
                manifest.require_fixture(&entry, Surface::NonClipGrade, &variant);
            }
        }
    }
}

/// The Corrector's balance: its temperature and tint.
fn balance(g: &Grade) -> (Option<f32>, Option<f32>) {
    g.grade().nodes.iter().find(|n| n.kind == NodeKind::Corrector).map(|n| (n.params.temperature, n.params.tint)).unwrap_or_default()
}

/// A white balance's correction landed as Auto White Balance and its
/// picker land it: past the dispatch boundary, as the app's command
/// passes it, then added to the Corrector's balance.
fn land_balance(g: &mut Grade, temp: f32, tint: f32) -> Result<(), String> {
    let clip = g.clip;
    g.state.before_dispatch(&AppCommand::AutoWhiteBalance { clip_id: clip });
    // Inside the stand-in's window on a sequence grade, as the colour
    // dispatch runs it.
    match g.with_stand_in(|g| g.state.apply_white_balance(clip, temp, tint)) {
        Some(_) => Ok(()),
        None => Err("no balance to add to".into()),
    }
}

fn run_white_balance(report: &mut Report, type_id: &str, surface: Surface, page: Page) {
    let k = |i| key(type_id, "white_balance", surface, i);
    let Some((state, clip, node)) = super::grade_inspector_tests::graded(NodeKind::Corrector) else {
        report.case(&format!("white balance on {surface:?}"), [], || Err("the grade won't take a Corrector".into()));
        return;
    };
    let start = Grade::new(state, clip, node).on(page);
    report.case(&format!("white balance opened on {surface:?}"), [k(Interaction::OpenWithoutWrite)], || opened(&start));
    // A grey card under a warm light, kept under chroma 8, where the frame
    // is surely grey; a warmer one reads as skin, never balanced on (#403).
    let warm = flat([176, 168, 158], 32, 32);
    report.case(&format!("Auto WB on {surface:?}"), [k(Interaction::AutoWhiteBalance), k(Interaction::UndoRedo)], || {
        trigger(&start, "AutoWhiteBalance", |g| click(g, "Auto WB"))?;
        let (temp, tint) = klipp_color::auto_white_balance(&warm, 32, 32, &[]).map_err(|e| format!("{e:?}"))?;
        if temp >= 0.0 {
            return Err(format!("a warm cast is cooled: {temp}"));
        }
        lands(&start, |g| land_balance(g, temp, tint), balance, |&(t, n)| t.is_some_and(|t| (t - temp).abs() < 1e-4) && n.is_some_and(|n| (n - tint).abs() < 1e-4))
    });
    report.case(&format!("white balance picker on {surface:?}"), [k(Interaction::Eyedropper), k(Interaction::UndoRedo)], || {
        trigger(&start, "PickWhiteBalance", |g| click_icon_by(g, "Auto WB", "PickWhiteBalance"))?;
        // A grey picked under a warm light, as the picker reads it.
        let (temp, tint) = klipp_color::white_balance::white_balance_to_neutralise([0.5, 0.45, 0.4], klipp_core::WorkingColorspace::Rec709Linear);
        lands(&start, |g| land_balance(g, temp, tint), balance, |&(t, _)| t.is_some_and(|t| (t - temp).abs() < 1e-4))
    });
    report.case(&format!("white balance result during a drag on {surface:?}"), [k(Interaction::ResultDuringDrag), k(Interaction::UndoRedo)], || {
        let (temp, tint) = klipp_color::auto_white_balance(&warm, 32, 32, &[]).map_err(|e| format!("{e:?}"))?;
        result_during_drag(&start, "Saturation", |g| land_balance(g, temp, tint), balance)
    });
}

fn run_qualifier(report: &mut Report, type_id: &str, surface: Surface, page: Page) {
    let k = |i| key(type_id, "qualifier_picker", surface, i);
    let Some((mut state, clip, node)) = placed(NodeKind::Qualifier) else {
        report.case(&format!("qualifier picker on {surface:?}"), [], || Err("the grade won't take a Qualifier".into()));
        return;
    };
    // Enabled, so its rows are drawn.
    if let Some(c) = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip)) {
        klipp_timeline::GradeAccess::edit_grade(c, |g| g.nodes.iter_mut().filter(|n| n.id == node).for_each(|n| n.params.qualifier.get_or_insert_with(Default::default).enabled = true));
    }
    let start = Grade::new(state, clip, node).on(page);
    report.case(&format!("qualifier picker opened on {surface:?}"), [k(Interaction::OpenWithoutWrite)], || opened(&start));
    let key_of = |g: &Grade| g.grade().nodes.iter().find(|n| n.id == g.node).and_then(|n| n.params.qualifier.clone());
    // An orange picked: the key sits on its hue, as the eyedropper's
    // sample lands it (`NodeInputSampled`, then `SetNodeQualifier`).
    let rgb = [0.8, 0.3, 0.05];
    let hue = klipp_effects::cpu_grade::picture_hsl(rgb).h * 360.0;
    let land = move |g: &mut Grade| {
        let base = g.grade().nodes.iter().find(|n| n.id == g.node).and_then(|n| n.params.qualifier.clone()).unwrap_or_default();
        let qualifier = super::colour::qualifier_keyed_on(rgb, base);
        g.apply(AppCommand::SetNodeQualifier { clip_id: g.clip, node_id: g.node, qualifier });
        Ok(())
    };
    report.case(&format!("qualifier eyedropper on {surface:?}"), [k(Interaction::Eyedropper), k(Interaction::UndoRedo)], || {
        trigger(&start, "PickQualifierColour", |g| click_icon_by(g, "Enabled", "PickQualifierColour"))?;
        lands(&start, land, key_of, |q| q.as_ref().is_some_and(|q| (q.hue_center - hue).abs() < 1.0))
    });
    report.case(&format!("qualifier result during a drag on {surface:?}"), [k(Interaction::ResultDuringDrag), k(Interaction::UndoRedo)], || {
        // The keyed hue is the result held: the drag is on Hue Width, which
        // the key sets too.
        result_during_drag(&start, "Hue Width", land, |g| key_of(g).map(|q| q.hue_center))
    });
}

fn run_colour_match(report: &mut Report, type_id: &str, surface: Surface, page: Page) {
    let k = |i| key(type_id, "colour_match", surface, i);
    let Some((state, clip, node)) = placed(NodeKind::ColorMatch) else {
        report.case(&format!("colour match on {surface:?}"), [], || Err("the grade won't take a Colour Match node".into()));
        return;
    };
    let start = Grade::new(state, clip, node).on(page);
    report.case(&format!("colour match opened on {surface:?}"), [k(Interaction::OpenWithoutWrite)], || opened(&start));
    let matched = |g: &Grade| g.grade().nodes.iter().find(|n| n.kind == NodeKind::ColorMatch).and_then(|n| n.params.color_match);
    // A frame darker than its reference: the match brightens it.
    let result = klipp_color::color_match::match_colors(&flat([90, 90, 90], 32, 32), &flat([150, 150, 150], 32, 32));
    let settings = klipp_core::ColorMatchSettings { exposure: result.exposure_adjust, temperature: result.temperature_adjust, tint: result.tint_adjust, saturation: result.saturation_adjust };
    // As `ApplyColorMatch` lands it: through `SetNodeColorMatch`.
    let land = move |g: &mut Grade| {
        let cm = g.grade().nodes.iter().find(|n| n.kind == NodeKind::ColorMatch).map(|n| n.id).ok_or("no Colour Match node")?;
        g.apply(AppCommand::SetNodeColorMatch { clip_id: g.clip, node_id: cm, settings });
        Ok(())
    };
    report.case(&format!("colour match reference on {surface:?}"), [k(Interaction::CaptureReference)], || {
        trigger(&start, "CaptureColorMatchReference", |g| click(g, "Capture reference"))
    });
    report.case(&format!("colour match applied on {surface:?}"), [k(Interaction::ColourMatch), k(Interaction::UndoRedo)], || {
        trigger(&start, "ApplyColorMatch", |g| click(g, "Apply match"))?;
        if result.exposure_adjust <= 0.0 {
            return Err(format!("a darker frame is brightened: {}", result.exposure_adjust));
        }
        lands(&start, land, matched, |m| *m == Some(settings))
    });
    // Its values, readouts (#1421): drawing them writes nothing, and the
    // match they show changes the frame, the CPU's the GPU's.
    let values = match_values();
    let value_keys = |i| values.iter().map(move |e| CoverageKey::new(e.type_id, &e.parameter, surface, "sample", i)).collect::<Vec<_>>();
    report.case(&format!("colour match values opened on {surface:?}"), value_keys(Interaction::OpenWithoutWrite), || {
        let mut matched = start.twin();
        land(&mut matched)?;
        opened(&matched.twin())
    });
    report.case_or_blocked(&format!("colour match values drawn on {surface:?}"), value_keys(Interaction::Pixels), || {
        let mut matched = start.twin();
        land(&mut matched)?;
        super::parity_pixels::edit_drawn(&start.frame_sequence(), &matched.frame_sequence())
    });
    report.case(&format!("colour match reset on {surface:?}"), [k(Interaction::Reset), k(Interaction::UndoRedo)], || {
        // From a match, its Reset: the match cleared, one step, undone
        // exactly.
        let mut from = start.twin();
        land(&mut from)?;
        let mut g = from.twin();
        let before = g.picture();
        click(&mut g, "Reset")?;
        if !g.unapplied.is_empty() {
            return Err(format!("unapplied {:?}", g.unapplied));
        }
        if !matched(&g).is_none_or(|m| m.is_identity()) {
            return Err(format!("the match is still {:?}", matched(&g)));
        }
        if g.state.active_history().undo_count() != 1 {
            return Err(format!("{} steps, not one", g.state.active_history().undo_count()));
        }
        g.step(false)?;
        if g.picture() != before {
            return Err("undo doesn't give back the match".into());
        }
        Ok(())
    });
    report.case(&format!("colour match result during a drag on {surface:?}"), [k(Interaction::ResultDuringDrag), k(Interaction::UndoRedo)], || {
        // The drag held on the grade's Corrector, on its Adjustments tab:
        // the Colour Match page has no slider of its own.
        let corrector = start.grade().nodes.iter().find(|n| n.kind == NodeKind::Corrector).map(|n| n.id).ok_or("no Corrector")?;
        let on_corrector = start.twin().on_node(corrector).on(Page::Tabbed(ColorTab::Adjustments));
        result_during_drag(&on_corrector, "Saturation", land, matched)
    });
}

pub(super) fn run(report: &mut Report) {
    for (entry, surfaces) in entries() {
        let id = entry.parameter.trim_start_matches("@workflow/");
        for (surface, page) in surfaces {
            match id {
                "white_balance" => run_white_balance(report, entry.type_id, surface, page),
                "qualifier_picker" => run_qualifier(report, entry.type_id, surface, page),
                _ => run_colour_match(report, entry.type_id, surface, page),
            }
        }
    }
}

/// The sampling tools on the grade's clip surfaces and the sequence's own
/// grades: every case passes, and
/// every key they're required for there is earned, but Pixels (the
/// picture oracle's) and keys excluded with a citation.
#[test]
fn workflow_tools_still_write_their_params() {
    let mut report = Report::default();
    report.family("grade workflows", run);
    // The same on the timeline grade and a group's stages, drawn through
    // their stand-ins (M3 step 19), earning `NonClipGrade`.
    super::parity_controls::on_roots(&mut report, run);
    report.family_result().unwrap();
    super::parity_controls::assert_on_every_root(&report);
    let mut manifest = Manifest::default();
    require(&mut manifest);
    let missing: Vec<_> = manifest.required.iter().filter(|k| k.interaction != Interaction::Pixels && !manifest.excluded_keys.contains_key(*k) && !report.passed.contains(k)).collect();
    assert!(missing.is_empty(), "required and not earned: {missing:#?}");
    assert!(report.passed.iter().all(|k| manifest.required.contains(k)), "earned a key not required: {:?}", report.passed.iter().filter(|k| !manifest.required.contains(k)).collect::<Vec<_>>());
}
