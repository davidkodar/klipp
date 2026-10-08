//! M3 step 18 parity: the grade's specialist editors (d9's family), each
//! driven through the generated view and the old panel on the grade
//! driver of `parity_controls`, and held to the same picture, one undo
//! step, and exact undo and redo.
//!
//! A LUT node's file (`node_inspector::lut_picker`). A look picked
//! from the bundled library, the file cleared, and a file this machine
//! doesn't have. Browse opens the system's file dialog, which a headless
//! test can't drive; its cancel sends nothing by construction.
//!
//! A Mask node's stack (`render_masks_body`): a mask added from its menu,
//! removed by its ×, and turned off by its switch.

use super::parity_controls::{placed, Grade, Page};
use super::parity_manifest::Report;
use klipp_nodes::NodeKind;

/// A LUT node with `path` set (or none), as the per-node layout draws it.
fn lut_node(path: Option<&str>) -> Result<Grade, String> {
    let (mut state, clip, node) = placed(NodeKind::Lut3D).ok_or("the grade won't take a LUT node")?;
    let c = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip)).unwrap();
    klipp_timeline::GradeAccess::edit_grade(c, |g| g.nodes.iter_mut().filter(|n| n.id == node).for_each(|n| n.params.lut_path = path.map(Into::into)));
    Ok(Grade::new(state, clip, node, true).on(Page::PerNode))
}

fn lut_path(g: &Grade) -> Option<std::path::PathBuf> {
    klipp_timeline::GradeAccess::grade(g.clip()).nodes.iter().find(|n| n.id == g.node).and_then(|n| n.params.lut_path.clone())
}

/// One LUT edit through both views from `start`: the same file after, in
/// one step, undone and redone exactly.
fn lut_case(start: &Grade, want: Option<&std::path::Path>, drive: impl Fn(&mut Grade) -> Result<(), String>) -> Result<(), String> {
    let (mut new, mut old) = (start.twin(true), start.twin(false));
    new.drawn();
    old.drawn();
    let before = new.picture();
    drive(&mut new).map_err(|e| format!("new: {e}"))?;
    drive(&mut old).map_err(|e| format!("old: {e}"))?;
    for (view, g) in [("new", &new), ("old", &old)] {
        if !g.unapplied.is_empty() {
            return Err(format!("{view}: unapplied {:?}", g.unapplied));
        }
        if lut_path(g).as_deref() != want {
            return Err(format!("{view}: {:?}, want {want:?}", lut_path(g)));
        }
        if g.state.active_history().undo_count() != 1 {
            return Err(format!("{view}: {} steps, not one", g.state.active_history().undo_count()));
        }
    }
    if new.picture() != old.picture() {
        return Err("the pictures differ".into());
    }
    let after = new.picture();
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
    report.case("LUT picked from the bundled looks", [], || {
        let (category, file) = first.clone().ok_or("no bundled look to pick")?;
        let name = file.file_stem().unwrap().to_string_lossy().to_string();
        let start = lut_node(None)?;
        lut_case(&start, Some(&file), |g| {
            click(g, "Bundled")?;
            click(g, &category)?;
            click(g, &name)
        })
    });
    report.case("LUT cleared", [], || {
        let start = lut_node(Some("/nonexistent/klipp-parity/look.cube"))?;
        lut_case(&start, None, |g| click(g, "Clear"))
    });
    // A file this machine doesn't have: named in both views, nothing
    // written by drawing it.
    report.case("LUT missing", [], || {
        let start = lut_node(Some("/nonexistent/klipp-parity/look.cube"))?;
        for generated in [false, true] {
            let mut g = start.twin(generated);
            let before = g.picture();
            let drawn = g.drawn();
            if !drawn.iter().any(|(t, _)| t == "look") {
                return Err(format!("generated {generated}: the missing file isn't named"));
            }
            if g.picture() != before || g.state.active_history().undo_count() != 0 || !g.unapplied.is_empty() {
                return Err(format!("generated {generated}: drawing it wrote"));
            }
        }
        Ok(())
    });
}

#[test]
fn a_lut_nodes_file_edits_as_the_old_panel() {
    let mut report = Report::default();
    report.family("LUT file", run_lut);
    report.family_result().unwrap();
}

fn masks(g: &Grade) -> Vec<klipp_core::Mask> {
    klipp_timeline::GradeAccess::grade(g.clip()).nodes.iter().find(|n| n.id == g.node).and_then(|n| n.params.masks.clone()).map(|m| m.masks).unwrap_or_default()
}

/// One mask-stack edit through both views from `start`: the same masks
/// after, `check`ed, in one step, undone to the masks before exactly.
fn mask_case(start: &Grade, check: impl Fn(&[klipp_core::Mask], &[klipp_core::Mask]) -> Result<(), String>, drive: impl Fn(&mut Grade) -> Result<(), String>) -> Result<(), String> {
    let (mut new, mut old) = (start.twin(true), start.twin(false));
    new.drawn();
    old.drawn();
    let before = masks(&new);
    drive(&mut new).map_err(|e| format!("new: {e}"))?;
    drive(&mut old).map_err(|e| format!("old: {e}"))?;
    for (view, g) in [("new", &mut new), ("old", &mut old)] {
        if !g.unapplied.is_empty() {
            return Err(format!("{view}: unapplied {:?}", g.unapplied));
        }
        check(&before, &masks(g)).map_err(|e| format!("{view}: {e}"))?;
        if g.state.active_history().undo_count() != 1 {
            return Err(format!("{view}: {} steps, not one", g.state.active_history().undo_count()));
        }
        g.step(false)?;
        if masks(g) != before {
            return Err(format!("{view}: undo doesn't give back the masks before: {:?}", masks(g)));
        }
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
    Ok(Grade::new(state, clip, node, true).on(Page::PerNode))
}

pub(super) fn run_masks(report: &mut Report) {
    report.case("mask added", [], || {
        let start = mask_node(false)?;
        mask_case(&start, |before, after| if after.len() == before.len() + 1 && matches!(after.last().map(|m| &m.shape), Some(klipp_core::MaskShape::Circle { .. })) { Ok(()) } else { Err(format!("{after:?}")) }, |g| {
            click(g, "+ Add Mask")?;
            click(g, "Circle")
        })
    });
    report.case("mask removed", [], || {
        let start = mask_node(true)?;
        mask_case(&start, |before, after| if after.len() + 1 == before.len() { Ok(()) } else { Err(format!("{after:?}")) }, |g| click(g, "\u{00D7}"))
    });
    report.case("mask turned off", [], || {
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
fn a_mask_stack_edits_as_the_old_panel() {
    let mut report = Report::default();
    report.family("mask stack", run_masks);
    report.family_result().unwrap();
}

/// A Difference Key's switch, in both views: one step, and its undo
/// gives back the key as it was (unset). The old panel drew it in place
/// and sent it too, so the step started from the edit and undid nothing.
#[test]
fn a_difference_key_edit_is_one_step_undone() {
    let (state, clip, node) = placed(NodeKind::DifferenceKey).expect("a Difference Key node");
    let base = Grade::new(state, clip, node, true).on(Page::PerNode);
    let key = |g: &Grade| klipp_timeline::GradeAccess::grade(g.clip()).nodes.iter().find(|n| n.id == g.node).and_then(|n| n.params.difference_key.clone());
    for generated in [false, true] {
        let mut g = base.twin(generated);
        g.drawn();
        let before = key(&g);
        click(&mut g, "Enabled").unwrap();
        assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
        assert_ne!(key(&g), before, "generated {generated}: the switch changed the key");
        assert_eq!(g.state.active_history().undo_count(), 1, "generated {generated}: one step");
        g.step(false).unwrap();
        assert_eq!(key(&g), before, "generated {generated}: undo gives back the key");
    }
}

/// Escape during a grade slider's drag takes it back in both views: the
/// old panel's `grade_slider` (a Soft Clip's Knee) and the generated row.
#[test]
fn escape_takes_back_a_grade_slider_drag() {
    let (mut state, clip, node) = placed(NodeKind::SoftClip).expect("a Soft Clip node");
    let c = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip)).unwrap();
    klipp_timeline::GradeAccess::edit_grade(c, |g| g.nodes.iter_mut().filter(|n| n.id == node).for_each(|n| n.params.soft_clip = Some(klipp_core::color_grade::SoftClipSettings { enabled: true, ..Default::default() })));
    let base = Grade::new(state, clip, node, true).on(Page::PerNode);
    for generated in [false, true] {
        let mut g = base.twin(generated);
        let drawn = g.drawn();
        let before = g.picture();
        let label = drawn.iter().rev().find(|(t, _)| t == "Knee").map(|(_, p)| *p).expect("a Knee row");
        let at = drawn.iter().filter(|(t, p)| (p.y - label.y).abs() < 6.0 && p.x > label.x && t.trim_start_matches('-').starts_with(|c: char| c.is_ascii_digit())).map(|(_, p)| *p).next().expect("its number");
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        g.frame(vec![egui::Event::PointerMoved(at), button(at, true)]);
        for dx in [6.0, 12.0, 18.0] {
            g.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))]);
        }
        assert!(g.picture() != before, "generated {generated}: the drag moved the knee");
        g.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
        g.frame(vec![button(at + egui::vec2(18.0, 0.0), false)]);
        g.frame(Vec::new());
        assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
        assert_eq!(g.state.active_history().undo_count(), 0, "generated {generated}: no step");
        assert!(g.picture() == before, "generated {generated}: Escape gives back the picture");
    }
}

/// A mask's own keyframes stay with it: with two masks keyed apart,
/// removing the first leaves the second's track whole, now first, in both
/// views (step 11's `mask_shapes_keep_their_own_tracks`).
#[test]
fn mask_shapes_keep_their_own_tracks() {
    let key = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) };
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
    let base = Grade::new(state, clip, node, true).on(Page::PerNode);
    for generated in [false, true] {
        let mut g = base.twin(generated);
        // The first row's ×.
        let drawn = g.drawn();
        let row = drawn.iter().find(|(t, _)| t.starts_with("Mask 1")).map(|(_, p)| *p).expect("a Mask 1 row");
        let x = drawn.iter().filter(|(t, p)| t == "\u{00D7}" && (p.y - row.y).abs() < 8.0).map(|(_, p)| *p).next().expect("its ×");
        g.click_at(x);
        assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
        let left = masks(&g);
        assert_eq!(left.len(), 1, "generated {generated}: one mask left");
        assert_eq!(left[0].keyframes.center_x, second.keyframes.center_x, "generated {generated}: the second keeps its own track");
        assert!(left[0].keyframes.center_y.is_empty(), "generated {generated}: and not the first's");
    }
}

/// A mask's Track button asks for its region to be tracked, in both views,
/// and writes nothing itself (step 11's `track_and_retrack_click_through`,
/// its first half).
#[test]
fn track_clicks_through_in_both_views() {
    let start = mask_node(true).unwrap();
    for generated in [false, true] {
        let mut g = start.twin(generated);
        g.drawn();
        let before = g.picture();
        click(&mut g, "\u{1F3AF} Track").unwrap();
        assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
        assert_eq!(g.side.len(), 1, "generated {generated}: {:?}", g.side);
        assert!(g.side[0].starts_with("TrackMaskRegion") && g.side[0].contains("mask_index: 0"), "generated {generated}: {:?}", g.side);
        assert!(g.picture() == before && g.state.active_history().undo_count() == 0, "generated {generated}: the request wrote");
    }
}

/// A tracked mask's Re-track and Untrack in the generated view: Re-track
/// takes the old tracker off and tracks again, Untrack takes it off, and
/// neither writes itself (step 11's `track_and_retrack_click_through`, its
/// second half). The old per-node panel draws its mask list with no tracked
/// masks, so it never offers them (pinned, #1398; step 19 removes it).
#[test]
fn retrack_and_untrack_click_through() {
    let start = mask_node(true).unwrap();
    let tracker: klipp_core::TrackPoint = serde_json::from_value(serde_json::json!({
        "id": uuid::Uuid::new_v4(), "name": "Mask 1", "color": [255, 200, 0, 255],
        "positions": [[0.5, 0.5], [0.52, 0.5]], "start_frame": 0, "enabled": true, "search_radius": 24,
        "linked_mask": [start.node, 0],
    })).unwrap();
    for (button, want) in [("\u{1F3AF} Re-track", vec!["RemoveMaskTracking", "TrackMaskRegion"]), ("\u{1F6AB} Untrack", vec!["RemoveMaskTracking"])] {
        let mut g = start.twin(true);
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
    // The old per-node panel: Track only (#1398).
    let mut old = start.twin(false);
    let clip = old.clip;
    old.state.sequences[0].all_clips_mut().find(|c| c.id == clip).unwrap().track_points.push(tracker);
    let drawn: Vec<String> = old.drawn().into_iter().map(|(t, _)| t).collect();
    assert!(drawn.iter().any(|t| t == "\u{1F3AF} Track") && !drawn.iter().any(|t| t.contains("Re-track")), "the old panel offers Track only (#1398)");
}
