//! M3: the grade's specialist editors (d9's family), each driven through
//! the generated view on the grade driver of `parity_controls` and held
//! to what the edit must leave (the file it names, the mask stack it
//! makes), one undo step, and exact undo and redo.
//!
//! A LUT node's file (`node_inspector::lut_picker`). A look picked
//! from the bundled library, the file cleared, and a file this machine
//! doesn't have. Browse opens the system's file dialog, which a headless
//! test can't drive; its cancel sends nothing by construction.
//!
//! A Mask node's stack (`render_masks_body`): a mask added from its menu,
//! removed by its ×, and turned off by its switch.

use super::parity_controls::{placed, Grade, Page};
use super::parity_manifest::{catalog, CoverageKey, Interaction, Manifest, Report, Surface};
use klipp_nodes::NodeKind;
use klipp_core::AppCommand;

/// A LUT node with `path` set (or none), as the per-node layout draws it.
fn lut_node(path: Option<&str>) -> Result<Grade, String> {
    let (mut state, clip, node) = placed(NodeKind::Lut3D).ok_or("the grade won't take a LUT node")?;
    let c = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip)).unwrap();
    klipp_timeline::GradeAccess::edit_grade(c, |g| g.nodes.iter_mut().filter(|n| n.id == node).for_each(|n| n.params.lut_path = path.map(Into::into)));
    Ok(Grade::new(state, clip, node).on(Page::PerNode))
}

fn lut_path(g: &Grade) -> Option<std::path::PathBuf> {
    g.grade().nodes.iter().find(|n| n.id == g.node).and_then(|n| n.params.lut_path.clone())
}

/// One LUT edit from `start`: the node's file `want` after, the picture
/// exactly the start's with only that file written, in one step, undone
/// and redone exactly.
fn lut_case(start: &Grade, want: Option<&std::path::Path>, drive: impl Fn(&mut Grade) -> Result<(), String>) -> Result<(), String> {
    let mut new = start.twin();
    new.drawn();
    let before = new.picture();
    let expected = super::parity_controls::expected_with(&new, |n| {
        n.params.lut_path = want.map(Into::into);
        Ok(())
    })?;
    drive(&mut new)?;
    if !new.unapplied.is_empty() {
        return Err(format!("unapplied {:?}", new.unapplied));
    }
    if lut_path(&new).as_deref() != want {
        return Err(format!("{:?}, want {want:?}", lut_path(&new)));
    }
    if new.state.active_history().undo_count() != 1 {
        return Err(format!("{} steps, not one", new.state.active_history().undo_count()));
    }
    let after = new.picture();
    if after != expected {
        return Err("not the expected picture: more than the file was written".into());
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

/// Click the drawn text `text`: its last occurrence (an open menu is drawn
/// over the panel).
fn click(g: &mut Grade, text: &str) -> Result<(), String> {
    let at = g.drawn().into_iter().rev().find(|(t, _)| t == text).map(|(_, p)| p).ok_or_else(|| format!("{text:?} isn't drawn"))?;
    g.click_at(at);
    Ok(())
}

/// The LUT node's file editor, and the Mask node's stack: the catalog
/// entries these cases drive, on the per-node layout.
const LUT_ENTRY: (&str, &str) = ("klipp.grade.lut_3d", "lut_path");
const MASK_ENTRY: (&str, &str) = ("klipp.grade.mask", "masks");

fn key((type_id, parameter): (&str, &str), interaction: Interaction) -> CoverageKey {
    CoverageKey::new(type_id, parameter, Surface::GradePerNode, "sample", interaction)
}

/// The obligations of the entries these cases drive, on the surface and
/// fixture they drive them on.
pub(super) fn require(manifest: &mut Manifest) {
    let entries = catalog();
    for (type_id, parameter) in [LUT_ENTRY, MASK_ENTRY] {
        if let Some(entry) = entries.iter().find(|e| e.type_id == type_id && e.parameter == parameter) {
            manifest.require_fixture(entry, Surface::GradePerNode, "sample");
            // These cases run on the sequence's grades too (`on_roots`).
            for root in super::parity_harness::GradeRoot::SEQUENCE {
                manifest.require_fixture(entry, Surface::NonClipGrade, &format!("{}: GradePerNode sample", root.variant()));
            }
        }
    }
}

pub(super) fn run_lut(report: &mut Report) {
    let library = klipp_core::assets::bundled_luts_dir();
    // The first bundled look, by its category and name.
    let first = library.and_then(|dir| {
        let mut categories: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().filter(|e| e.path().is_dir()).map(|e| e.path()).collect();
        categories.sort();
        categories.into_iter().find_map(|c| {
            let mut files: Vec<_> = std::fs::read_dir(&c).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "cube")).collect();
            files.sort();
            files.into_iter().next().map(|f| (c.file_name().unwrap().to_string_lossy().to_string(), f))
        })
    });
    report.case("LUT opened", [key(LUT_ENTRY, Interaction::OpenWithoutWrite)], || {
        let mut g = lut_node(Some("/nonexistent/klipp-parity/look.cube"))?.twin();
        opened_writes_nothing(&mut g)
    });
    // Browse, its dialog dismissed: a picker asked for, and nothing sent
    // or written (`file_dialog::cancelling` answers it cancelled).
    report.case("LUT browse cancelled", [key(LUT_ENTRY, Interaction::FileCancel)], || {
        let mut g = lut_node(Some("/nonexistent/klipp-parity/look.cube"))?.twin();
        g.drawn();
        let before = (g.picture(), g.state.active_history().undo_count());
        let mut clicked = Ok(());
        let asked = crate::file_dialog::cancelling(|| clicked = click(&mut g, "Browse\u{2026}"));
        clicked?;
        if asked != 1 {
            return Err(format!("Browse asked for {asked} pickers, not one"));
        }
        if (g.picture(), g.state.active_history().undo_count()) != before || !g.unapplied.is_empty() || !g.side.is_empty() {
            return Err(format!("a dismissed Browse wrote or sent: {:?} {:?}", g.unapplied, g.side));
        }
        Ok(())
    });
    report.case("LUT picked from the bundled looks", [key(LUT_ENTRY, Interaction::FileSelect), key(LUT_ENTRY, Interaction::UndoRedo)], || {
        let (category, file) = first.clone().ok_or("no bundled look to pick")?;
        let name = file.file_stem().unwrap().to_string_lossy().to_string();
        let start = lut_node(None)?;
        lut_case(&start, Some(&file), |g| {
            click(g, "Bundled")?;
            click(g, &category)?;
            click(g, &name)
        })
    });
    // The look drawn: picking it changes the frame, the CPU's the GPU's,
    // through the stage the grade is on.
    report.case_or_blocked("LUT picked, drawn", [key(LUT_ENTRY, Interaction::Pixels)], || {
        let (category, file) = first.clone().ok_or("no bundled look to pick")?;
        let name = file.file_stem().unwrap().to_string_lossy().to_string();
        let start = lut_node(None)?;
        let mut g = start.twin();
        g.drawn();
        click(&mut g, "Bundled")?;
        click(&mut g, &category)?;
        click(&mut g, &name)?;
        if !g.unapplied.is_empty() {
            return Err(format!("unapplied: {:?}", g.unapplied));
        }
        super::parity_pixels::edit_drawn(&start.frame_sequence(), &g.frame_sequence())
    });
    report.case("LUT cleared", [key(LUT_ENTRY, Interaction::FileClear), key(LUT_ENTRY, Interaction::UndoRedo)], || {
        let start = lut_node(Some("/nonexistent/klipp-parity/look.cube"))?;
        lut_case(&start, None, |g| click(g, "Clear"))
    });
    // A file this machine doesn't have: named, nothing written by drawing
    // it.
    report.case("LUT missing", [key(LUT_ENTRY, Interaction::MissingAsset)], || {
        let start = lut_node(Some("/nonexistent/klipp-parity/look.cube"))?;
        let mut g = start.twin();
        let before = g.picture();
        let drawn = g.drawn();
        if !drawn.iter().any(|(t, _)| t == "look") {
            return Err("the missing file isn't named".into());
        }
        if g.picture() != before || g.state.active_history().undo_count() != 0 || !g.unapplied.is_empty() {
            return Err("drawing it wrote".into());
        }
        Ok(())
    });
}

#[test]
fn a_lut_nodes_file_stores_the_file_picked() {
    let mut report = Report::default();
    report.family("LUT file", run_lut);
    // The same cases with the grade on the timeline grade and a group's
    // stages, drawn through their stand-ins (M3 step 19).
    super::parity_controls::on_roots(&mut report, run_lut);
    // Its cases claim no coverage keys on any page yet, so the run on each
    // root is a behaviour check: every case passes there too.
    report.family_result().unwrap();
}

fn masks(g: &Grade) -> Vec<klipp_core::Mask> {
    g.grade().nodes.iter().find(|n| n.id == g.node).and_then(|n| n.params.masks.clone()).map(|m| m.masks).unwrap_or_default()
}

/// One mask-stack edit from `start`: the masks after `check`ed against
/// the masks before (the edit the control names), in one step, undone to
/// the picture before and redone to the picture after exactly.
fn mask_case(start: &Grade, check: impl Fn(&[klipp_core::Mask], &[klipp_core::Mask]) -> Result<(), String>, drive: impl Fn(&mut Grade) -> Result<(), String>) -> Result<(), String> {
    let mut new = start.twin();
    new.drawn();
    let (before, picture) = (masks(&new), new.picture());
    drive(&mut new)?;
    if !new.unapplied.is_empty() {
        return Err(format!("unapplied {:?}", new.unapplied));
    }
    check(&before, &masks(&new))?;
    // Nothing but the stack written: the start with the stack it now has.
    let stack = new.grade().nodes.iter().find(|n| n.id == new.node).and_then(|n| n.params.masks.clone());
    let expected = super::parity_controls::expected_with(start, |n| {
        n.params.masks = stack;
        Ok(())
    })?;
    let after = new.picture();
    if after != expected {
        return Err("not the expected picture: more than the mask stack was written".into());
    }
    if new.state.active_history().undo_count() != 1 {
        return Err(format!("{} steps, not one", new.state.active_history().undo_count()));
    }
    new.step(false)?;
    if new.picture() != picture {
        return Err(format!("undo doesn't give back the masks before: {:?}", masks(&new)));
    }
    new.step(true)?;
    if new.picture() != after {
        return Err("redo doesn't give back the picture after".into());
    }
    Ok(())
}

/// A Mask node, with one circle on it when `with_one`.
fn mask_node(with_one: bool) -> Result<Grade, String> {
    let (mut state, clip, node) = placed(NodeKind::Mask).ok_or("the grade won't take a Mask node")?;
    let c = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip)).unwrap();
    klipp_timeline::GradeAccess::edit_grade(c, |g| g.nodes.iter_mut().filter(|n| n.id == node).for_each(|n| {
        let stack = n.params.masks.get_or_insert_with(Default::default);
        stack.masks.clear();
        if with_one {
            stack.masks.push(klipp_core::Mask { shape: klipp_core::MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.2 }, ..Default::default() });
        }
    }));
    Ok(Grade::new(state, clip, node).on(Page::PerNode))
}

/// Drawing `g` twice writes nothing: no picture change, no step, nothing
/// sent.
fn opened_writes_nothing(g: &mut Grade) -> Result<(), String> {
    let before = (g.picture(), g.state.active_history().undo_count());
    g.drawn();
    g.drawn();
    if (g.picture(), g.state.active_history().undo_count()) != before || !g.unapplied.is_empty() {
        return Err(format!("opening wrote: {:?}", g.unapplied));
    }
    Ok(())
}

pub(super) fn run_masks(report: &mut Report) {
    report.case("mask stack opened", [key(MASK_ENTRY, Interaction::OpenWithoutWrite)], || opened_writes_nothing(&mut mask_node(true)?.twin()));
    // Escape during a shape slider's drag takes it back: the masks as they
    // were, no step.
    report.case("mask drag cancelled", [key(MASK_ENTRY, Interaction::CancelDrag)], || {
        let mut g = mask_node(true)?.twin();
        let at = mask_slider(&mut g, "Radius")?;
        let (before, picture) = (masks(&g), g.picture());
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        g.frame(vec![egui::Event::PointerMoved(at), button(at, true)]);
        for dx in [8.0, 16.0, 24.0] {
            g.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))]);
        }
        if masks(&g) == before {
            return Err("the drag didn't move the radius, so Escape had nothing to take back".into());
        }
        g.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
        g.frame(vec![button(at + egui::vec2(24.0, 0.0), false)]);
        g.frame(Vec::new());
        if !g.unapplied.is_empty() || g.state.active_history().undo_count() != 0 || masks(&g) != before || g.picture() != picture {
            return Err(format!("Escape didn't take the drag back: {} steps, {:?}", g.state.active_history().undo_count(), g.unapplied));
        }
        Ok(())
    });
    report.case("mask added", [key(MASK_ENTRY, Interaction::AddMask), key(MASK_ENTRY, Interaction::UndoRedo)], || {
        let start = mask_node(false)?;
        mask_case(&start, |before, after| if after.len() == before.len() + 1 && matches!(after.last().map(|m| &m.shape), Some(klipp_core::MaskShape::Circle { .. })) { Ok(()) } else { Err(format!("{after:?}")) }, |g| {
            click(g, "+ Add Mask")?;
            click(g, "Circle")
        })
    });
    // A mask added, drawn: the window it opens changes the frame, the CPU's
    // the GPU's, through a correction it selects (the window's Corrector
    // turned up).
    report.case_or_blocked("mask added, drawn", [key(MASK_ENTRY, Interaction::Pixels)], || {
        let mut start = mask_node(false)?;
        let clip_id = start.clip;
        for corrector in start.grade().nodes.iter().filter(|n| n.kind == NodeKind::Corrector).map(|n| n.id).collect::<Vec<_>>() {
            start.apply(AppCommand::SetNodeParam { clip_id, node_id: corrector, param: "exposure".into(), value: 1.5 });
        }
        let start = start.twin();
        let mut g = start.twin();
        g.drawn();
        click(&mut g, "+ Add Mask")?;
        click(&mut g, "Circle")?;
        if !g.unapplied.is_empty() {
            return Err(format!("unapplied: {:?}", g.unapplied));
        }
        super::parity_pixels::edit_drawn(&start.frame_sequence(), &g.frame_sequence())
    });
    report.case("mask removed", [key(MASK_ENTRY, Interaction::RemoveMask), key(MASK_ENTRY, Interaction::UndoRedo)], || {
        let start = mask_node(true)?;
        mask_case(&start, |before, after| if after.len() + 1 == before.len() { Ok(()) } else { Err(format!("{after:?}")) }, |g| click(g, "\u{00D7}"))
    });
    report.case("mask turned off", [key(MASK_ENTRY, Interaction::EditMask), key(MASK_ENTRY, Interaction::UndoRedo)], || {
        let start = mask_node(true)?;
        mask_case(&start, |_, after| if after.len() == 1 && !after[0].enabled { Ok(()) } else { Err(format!("{after:?}")) }, |g| {
            // Its switch, left of its name.
            let drawn = g.drawn();
            let name = drawn.iter().find(|(t, _)| t.starts_with("Mask 1")).map(|(_, p)| *p).ok_or("no Mask 1 row")?;
            let mut x = name.x - 30.0;
            while x > 4.0 {
                g.click_at(egui::pos2(x, name.y));
                if masks(g).first().is_some_and(|m| !m.enabled) {
                    return Ok(());
                }
                x -= 6.0;
            }
            Err("no switch for Mask 1".into())
        })
    });
}

#[test]
fn a_mask_stack_makes_the_edit_named() {
    let mut report = Report::default();
    report.family("mask stack", run_masks);
    // The same cases with the grade on the timeline grade and a group's
    // stages, drawn through their stand-ins (M3 step 19).
    super::parity_controls::on_roots(&mut report, run_masks);
    // Its cases claim no coverage keys on any page yet, so the run on each
    // root is a behaviour check: every case passes there too.
    report.family_result().unwrap();
}

/// A Difference Key's switch: one step, and its undo gives back the key
/// as it was (unset), not a step started from the edit that undoes
/// nothing.
#[test]
fn a_difference_key_edit_is_one_step_undone() {
    let (state, clip, node) = placed(NodeKind::DifferenceKey).expect("a Difference Key node");
    let base = Grade::new(state, clip, node).on(Page::PerNode);
    let key = |g: &Grade| g.grade().nodes.iter().find(|n| n.id == g.node).and_then(|n| n.params.difference_key.clone());
    let mut g = base.twin();
    g.drawn();
    let before = key(&g);
    click(&mut g, "Enabled").unwrap();
    assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
    assert_ne!(key(&g), before, "the switch changed the key");
    assert_eq!(g.state.active_history().undo_count(), 1, "one step");
    g.step(false).unwrap();
    assert_eq!(key(&g), before, "undo gives back the key");
}

/// Escape during a grade slider's drag (a Soft Clip's Knee) takes it back.
#[test]
fn escape_takes_back_a_grade_slider_drag() {
    let (mut state, clip, node) = placed(NodeKind::SoftClip).expect("a Soft Clip node");
    let c = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip)).unwrap();
    klipp_timeline::GradeAccess::edit_grade(c, |g| g.nodes.iter_mut().filter(|n| n.id == node).for_each(|n| n.params.soft_clip = Some(klipp_core::color_grade::SoftClipSettings { enabled: true, ..Default::default() })));
    let base = Grade::new(state, clip, node).on(Page::PerNode);
    let mut g = base.twin();
    let drawn = g.drawn();
    let before = g.picture();
    let label = drawn.iter().rev().find(|(t, _)| t == "Knee").map(|(_, p)| *p).expect("a Knee row");
    let at = drawn.iter().filter(|(t, p)| (p.y - label.y).abs() < 6.0 && p.x > label.x && t.trim_start_matches('-').starts_with(|c: char| c.is_ascii_digit())).map(|(_, p)| *p).next().expect("its number");
    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    g.frame(vec![egui::Event::PointerMoved(at), button(at, true)]);
    for dx in [6.0, 12.0, 18.0] {
        g.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))]);
    }
    assert!(g.picture() != before, "the drag moved the knee");
    g.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
    g.frame(vec![button(at + egui::vec2(18.0, 0.0), false)]);
    g.frame(Vec::new());
    assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
    assert_eq!(g.state.active_history().undo_count(), 0, "no step");
    assert!(g.picture() == before, "Escape gives back the picture");
}

/// A mask's own keyframes stay with it: with two masks keyed apart,
/// removing the first leaves the second's track whole, now first, in both
/// views (step 11's `mask_shapes_keep_their_own_tracks`).
#[test]
fn mask_shapes_keep_their_own_tracks() {
    let key = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
    let (mut state, clip, node) = placed(NodeKind::Mask).expect("a Mask node");
    let c = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip)).unwrap();
    let second = {
        let mut m = klipp_core::Mask { shape: klipp_core::MaskShape::Rectangle { center_x: 0.5, center_y: 0.5, width: 0.4, height: 0.3, rotation: 0.0 }, ..Default::default() };
        m.keyframes.center_x = vec![key(0.0, 0.2), key(1.0, 0.8)];
        m
    };
    let first = {
        let mut m = klipp_core::Mask { shape: klipp_core::MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.2 }, ..Default::default() };
        m.keyframes.center_y = vec![key(0.0, 0.1), key(1.0, 0.9)];
        m
    };
    let (f, s2) = (first.clone(), second.clone());
    klipp_timeline::GradeAccess::edit_grade(c, |g| g.nodes.iter_mut().filter(|n| n.id == node).for_each(|n| n.params.masks = Some(klipp_core::MaskStack { masks: vec![f.clone(), s2.clone()] })));
    let base = Grade::new(state, clip, node).on(Page::PerNode);
    let mut g = base.twin();
    // The first row's ×.
    let drawn = g.drawn();
    let row = drawn.iter().find(|(t, _)| t.starts_with("Mask 1")).map(|(_, p)| *p).expect("a Mask 1 row");
    let x = drawn.iter().filter(|(t, p)| t == "\u{00D7}" && (p.y - row.y).abs() < 8.0).map(|(_, p)| *p).next().expect("its ×");
    g.click_at(x);
    assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
    let left = masks(&g);
    assert_eq!(left.len(), 1, "one mask left");
    assert_eq!(left[0].keyframes.center_x, second.keyframes.center_x, "the second keeps its own track");
    assert!(left[0].keyframes.center_y.is_empty(), "and not the first's");
}

/// A mask's Track button asks for its region to be tracked, and writes
/// nothing itself (step 11's `track_and_retrack_click_through`,
/// its first half).
#[test]
fn track_clicks_through() {
    let start = mask_node(true).unwrap();
    let mut g = start.twin();
    g.drawn();
    let before = g.picture();
    click(&mut g, "\u{1F3AF} Track").unwrap();
    assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
    assert_eq!(g.side.len(), 1, "{:?}", g.side);
    assert!(g.side[0].starts_with("TrackMaskRegion") && g.side[0].contains("mask_index: 0"), "{:?}", g.side);
    assert!(g.picture() == before && g.state.active_history().undo_count() == 0, "the request wrote");
}

/// A tracked mask's Re-track and Untrack: Re-track takes the old tracker
/// off and tracks again, Untrack takes it off, and neither writes itself
/// (step 11's `track_and_retrack_click_through`, its second half).
#[test]
fn retrack_and_untrack_click_through() {
    let start = mask_node(true).unwrap();
    let tracker: klipp_core::TrackPoint = serde_json::from_value(serde_json::json!({
        "id": uuid::Uuid::new_v4(), "name": "Mask 1", "color": [255, 200, 0, 255],
        "positions": [[0.5, 0.5], [0.52, 0.5]], "start_frame": 0, "enabled": true, "search_radius": 24,
        "linked_mask": [start.node, 0],
    })).unwrap();
    for (button, want) in [("\u{1F3AF} Re-track", vec!["RemoveMaskTracking", "TrackMaskRegion"]), ("\u{1F6AB} Untrack", vec!["RemoveMaskTracking"])] {
        let mut g = start.twin();
        let clip = g.clip;
        g.state.sequences[0].all_clips_mut().find(|c| c.id == clip).unwrap().track_points.push(tracker.clone());
        g.drawn();
        let before = g.picture();
        click(&mut g, button).unwrap();
        assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
        let sent: Vec<&str> = g.side.iter().map(|c| c.split([' ', '{']).next().unwrap()).collect();
        assert_eq!(sent, want, "{button}: {:?}", g.side);
        assert!(g.picture() == before && g.state.active_history().undo_count() == 0, "{button}: the request wrote");
    }
}

/// Where the mask's `param` slider is pressed: on its rail, between its
/// name and the number it shows.
fn mask_slider(g: &mut Grade, param: &str) -> Result<egui::Pos2, String> {
    let drawn = g.drawn();
    let label = drawn.iter().rev().find(|(t, _)| t == param).map(|(_, p)| *p).ok_or_else(|| format!("no {param} slider"))?;
    let value = drawn.iter().filter(|(t, p)| (p.y - label.y).abs() < 6.0 && p.x > label.x && t.starts_with(|c: char| c.is_ascii_digit())).map(|(_, p)| *p).min_by(|a, b| a.x.total_cmp(&b.x)).ok_or_else(|| format!("no number on the {param} slider"))?;
    Ok(egui::pos2((label.x + value.x) / 2.0, value.y))
}

/// A mask's shape slider dragged is one undo step, not one per frame, and
/// Escape during the drag takes it back (IC13).
#[test]
fn a_mask_slider_drag_is_one_step_and_escape_takes_it_back() {
    let base = mask_node(true).expect("a Mask node with a circle");
    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    for escape in [false, true] {
        let mut g = base.twin();
        let at = mask_slider(&mut g, "Radius").unwrap();
        let before = masks(&g);
        g.frame(vec![egui::Event::PointerMoved(at), button(at, true)]);
        for dx in [8.0, 16.0, 24.0] {
            g.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))]);
        }
        let dragged = masks(&g);
        assert!(dragged != before, "the drag moved the radius");
        if escape {
            g.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
        }
        g.frame(vec![button(at + egui::vec2(24.0, 0.0), false)]);
        g.frame(Vec::new());
        assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
        if escape {
            assert_eq!(g.state.active_history().undo_count(), 0, "Escape leaves no step");
            assert_eq!(masks(&g), before, "Escape gives back the masks");
        } else {
            assert_eq!(g.state.active_history().undo_count(), 1, "one step for the drag");
            g.step(false).unwrap();
            assert_eq!(masks(&g), before, "undo gives back the masks");
        }
    }
}

/// A click on a keyed mask slider keys its value at the playhead as one
/// undo step, undone exactly.
#[test]
fn a_keyed_mask_slider_click_is_one_step() {
    let mut base = mask_node(true).expect("a Mask node with a circle");
    let key = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
    let (clip, node) = (base.clip, base.node);
    let c = base.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip)).unwrap();
    klipp_timeline::GradeAccess::edit_grade(c, |g| g.nodes.iter_mut().filter(|n| n.id == node).for_each(|n| {
        n.params.masks.as_mut().unwrap().masks[0].keyframes.radius = vec![key(0.0, 0.2), key(1.0, 0.3)];
    }));
    let mut g = base.twin();
    let at = mask_slider(&mut g, "Radius").unwrap();
    let before = masks(&g);
    g.click_at(at + egui::vec2(12.0, 0.0));
    assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
    assert!(masks(&g)[0].keyframes.radius != before[0].keyframes.radius, "the click keyed the radius");
    assert_eq!(g.state.active_history().undo_count(), 1, "one step");
    g.step(false).unwrap();
    assert_eq!(masks(&g), before, "undo gives back the keys");
}

/// A file's camera encoding picked from the Input Colour Space menu: the
/// file and its clip set, one step, and one undo gives back both (#1396).
#[test]
fn a_camera_encoding_pick_sets_the_file_and_its_clip() {
    let (mut state, clip, node) = super::grade_inspector_tests::graded(NodeKind::InputTransform).expect("an Input Transform node");
    // The clip's file, in the pool: the encoding is the file's.
    let media = state.sequences[0].all_clips().find(|c| c.id == clip).and_then(|c| c.media_id()).expect("a clip of a file");
    let fps = state.sequences[0].fps;
    state.media_pool.add(klipp_media::MediaItem::new_synthetic_title(media, "shot".into(), klipp_core::RationalTime::from_secs_f64(2.0, 25), fps));
    let base = Grade::new(state, clip, node).on(Page::PerNode);
    let format = |g: &Grade| g.state.media_pool.get(&media).map(|i| (i.log_format, i.log_format_user_set)).expect("the file");
    let mut g = base.twin();
    g.drawn();
    let before = (format(&g), g.picture());
    click(&mut g, "Rec.709 / sRGB (no conversion)").unwrap();
    click(&mut g, "Sony S-Log3").unwrap();
    assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
    assert_eq!(format(&g), (klipp_core::LogFormat::SLog3, true), "the file's encoding");
    assert!(g.picture() != before.1, "the clip's Input Transform");
    assert_eq!(g.state.undo_depth(), 1, "one step");
    assert_eq!(g.state.undo_scopes().0, Some("project"), "the file's: a project step (#1986)");
    g.step(false).unwrap();
    assert_eq!((format(&g), g.picture()), before, "undo gives back the file and the clip");
}
