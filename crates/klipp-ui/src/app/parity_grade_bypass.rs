//! A grade node's bypass (M3, flip readiness ruling 10-09): the per-node
//! layout's header and the tabbed page's and bottom tools' strip, one
//! generated switch with its keys whatever the node. Switched off and on,
//! its frame, and its keys (its diamond adds and removes one; once keyed,
//! the switch edits the key at the playhead; its carets seek to the keys
//! either side; its menu holds a key) are earned on the central
//! rules' representative nodes (`rules::BYPASS_REPRESENTATIVE`), on the
//! clip's grade and, through `on_roots`, each sequence grade, each held to
//! the node changed straight in the document, one undo step, and an exact
//! undo and redo. Per type, a node's page opens with its bypass drawn and
//! nothing written.

use super::parity_controls::{expected_with, placed, Grade, Page};
use super::parity_harness::GradeRoot;
use super::parity_manifest::{catalog, CatalogValue, CoverageKey, Interaction, Manifest, Report, Surface};
use klipp_core::{AppCommand, Keyframe};
use klipp_nodes::NodeKind;
use klipp_timeline::GradeAccess;

/// The frame's time the grade pages draw at (the harness's selection).
const AT: f64 = 0.5;

/// The bypass's diamond, drawn as a glyph: empty, or filled at a key.
const DIAMONDS: [&str; 2] = ["\u{25C7}", "\u{25C6}"];

/// The bypass's diamond, where drawn.
fn diamond(g: &mut Grade) -> Result<egui::Pos2, String> {
    g.drawn().into_iter().find(|(t, _)| DIAMONDS.contains(&t.as_str())).map(|(_, p)| p).ok_or_else(|| "no bypass diamond drawn".into())
}

/// The bypass's switch: the checkbox on the diamond's line.
fn switch(g: &mut Grade) -> Result<egui::Pos2, String> {
    let at = diamond(g)?;
    g.boxes.iter().filter(|b| (b.center().y - at.y).abs() < 8.0).map(|b| b.center()).min_by(|a, b| (a.x - at.x).abs().total_cmp(&(b.x - at.x).abs())).ok_or_else(|| "no bypass switch on the diamond's line".into())
}

/// `start` with its node changed by `edit`, written straight into the
/// document, and a fresh history.
fn with_node(start: &Grade, edit: impl FnOnce(&mut klipp_nodes::Node)) -> Result<Grade, String> {
    let mut g = start.twin();
    let node = g.node;
    g.with_stand_in(|g| -> Result<(), String> {
        let clip_id = g.clip;
        let clip = g.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).ok_or("no clip for the grade")?;
        clip.edit_grade(|graph| graph.nodes.iter_mut().find(|n| n.id == node).map(edit).ok_or_else(|| "no node".to_string()))
    })?;
    Ok(g.twin())
}

/// The bypass's caret toward the previous or the next key: the small icon
/// beside its diamond, on that side.
fn caret(g: &mut Grade, next: bool) -> Result<egui::Pos2, String> {
    let at = diamond(g)?;
    g.icons.iter().filter(|i| (i.center().y - at.y).abs() < 8.0 && (i.center().x > at.x) == next && (i.center().x - at.x).abs() < 40.0 && i.width() < 12.0).map(|i| i.center()).min_by(|a, b| (a.x - at.x).abs().total_cmp(&(b.x - at.x).abs())).ok_or_else(|| format!("no {} caret beside the bypass diamond", if next { "next" } else { "previous" }))
}

/// A bypass key track of `keys` (time, on).
fn keyed(keys: &[(f64, f32)]) -> impl Fn(&mut klipp_nodes::Node) + '_ {
    keyed_as(keys, klipp_core::Interpolation::Step)
}

/// [`keyed`], the keys interpolated by `interpolation`.
fn keyed_as(keys: &[(f64, f32)], interpolation: klipp_core::Interpolation) -> impl Fn(&mut klipp_nodes::Node) + '_ {
    move |n| {
        let track = n.keyframes.track_mut(klipp_nodes::ENABLED_TRACK);
        track.keyframes = keys.iter().map(|&(time_secs, value)| Keyframe { time_secs, value, interpolation, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() }).collect();
    }
}

/// From keys either side of the frame, a caret seeks to the key on its
/// side, in the timeline's time, and writes nothing.
fn caret_case(start: &Grade, next: bool) -> Result<(), String> {
    let s = with_node(start, keyed(&[(0.25, 1.0), (1.25, 0.0)]))?;
    let mut g = s.twin();
    g.drawn();
    let before = g.picture();
    let at = caret(&mut g, next)?;
    g.click_at(at);
    g.drawn();
    let want = if next { 1.25 } else { 0.25 };
    let lands = |t: f64| g.state.sequences[0].all_clips().any(|c| (t - c.timeline_in.to_secs_f64() - want).abs() < 0.02);
    if g.seeks.len() != 1 || !lands(g.seeks[0].to_secs_f64()) {
        return Err(format!("seeks {:?}, not one to the key at {want}", g.seeks));
    }
    if g.picture() != before || g.state.active_history().undo_count() != 0 || !g.unapplied.is_empty() {
        return Err(format!("a caret wrote: undo {}, unapplied {:?}", g.state.active_history().undo_count(), g.unapplied));
    }
    Ok(())
}

/// One bypass edit by `drive` from `start`, held to `expected` (the node
/// changed straight in the document): one step, undone and redone exactly.
fn edit_case(start: &Grade, expected: impl FnOnce(&mut klipp_nodes::Node), drive: impl FnOnce(&mut Grade) -> Result<(), String>) -> Result<(), String> {
    let want = expected_with(start, |n| {
        expected(n);
        Ok(())
    })?;
    let mut g = start.twin();
    g.drawn();
    let before = g.picture();
    drive(&mut g)?;
    g.drawn();
    if !g.unapplied.is_empty() {
        return Err(format!("unapplied {:?}", g.unapplied));
    }
    let after = g.picture();
    if after != want {
        let node = g.grade().nodes.iter().find(|n| n.id == g.node).cloned();
        return Err(format!("the bypass left another grade than the edit made in the document: enabled {:?}, keys {:?}", node.as_ref().map(|n| n.enabled), node.map(|n| n.keyframes)));
    }
    if g.state.active_history().undo_count() != 1 {
        return Err(format!("{} undo steps, not one", g.state.active_history().undo_count()));
    }
    g.step(false)?;
    if g.picture() != before {
        return Err("undo doesn't give back the grade before".into());
    }
    g.step(true)?;
    if g.picture() != after {
        return Err("redo doesn't give back the edit".into());
    }
    Ok(())
}

/// A value of the representative's that shows in the frame, so its
/// bypass changes the frame.
fn showing(start: &Grade, type_id: &str) -> Grade {
    let mut g = start.twin();
    let (clip_id, node_id) = (g.clip, g.node);
    let (param, value) = if type_id == "klipp.grade.denoise" { ("denoise_luma", 0.8) } else { ("exposure", 1.5) };
    g.apply(AppCommand::SetNodeParam { clip_id, node_id, param: param.into(), value });
    g.twin()
}

pub(super) fn run(report: &mut Report) {
    for (page, type_id) in super::parity_manifest::bypass_representatives() {
        let key = |i: Interaction| CoverageKey::new(type_id, "@enabled", page, super::parity_manifest::bypass_variant(), i);
        let start = || -> Result<Grade, String> {
            let parameter = if type_id == "klipp.grade.denoise" { "denoise_luma" } else { "exposure" };
            Ok(showing(&super::parity_grade_keys::representative(page, type_id, parameter)?.0, type_id))
        };
        let name = |what: &str| format!("{type_id} bypass on {page:?}: {what}");
        report.case(&name("switched off"), [key(Interaction::Toggle(false)), key(Interaction::UndoRedo)], || {
            let s = start()?;
            edit_case(&s, |n| n.enabled = false, |g| {
                let at = switch(g)?;
                g.click_at(at);
                Ok(())
            })
        });
        report.case(&name("switched on"), [key(Interaction::Toggle(true))], || {
            let s = with_node(&start()?, |n| n.enabled = false)?;
            edit_case(&s, |n| n.enabled = true, |g| {
                let at = switch(g)?;
                g.click_at(at);
                Ok(())
            })
        });
        report.case_or_blocked(&name("drawn"), [key(Interaction::Pixels)], || {
            let s = start()?;
            let mut off = s.twin();
            let at = switch(&mut off)?;
            off.click_at(at);
            super::parity_pixels::edit_drawn(&s.frame_sequence(), &off.frame_sequence())
        });
        report.case(&name("key added"), [key(Interaction::AddKey)], || {
            let s = start()?;
            edit_case(&s, keyed(&[(AT, 1.0)]), |g| {
                let at = diamond(g)?;
                g.click_at(at);
                Ok(())
            })
        });
        report.case(&name("key removed"), [key(Interaction::RemoveKey)], || {
            let s = with_node(&start()?, keyed(&[(AT, 1.0), (1.25, 1.0)]))?;
            edit_case(&s, keyed(&[(1.25, 1.0)]), |g| {
                let at = diamond(g)?;
                g.click_at(at);
                Ok(())
            })
        });
        report.case(&name("previous key"), [key(Interaction::PreviousKey)], || caret_case(&start()?, false));
        report.case(&name("next key"), [key(Interaction::NextKey)], || caret_case(&start()?, true));
        report.case(&name("key held"), [key(Interaction::Interpolation(super::parity_manifest::KeyInterpolation::Step))], || {
            // A key that eases (one keyed before keys held): Step from the
            // diamond's menu holds it.
            let s = with_node(&start()?, keyed_as(&[(AT, 1.0), (1.25, 0.0)], klipp_core::Interpolation::Linear))?;
            edit_case(&s, |n| {
                keyed_as(&[(AT, 1.0), (1.25, 0.0)], klipp_core::Interpolation::Linear)(n);
                n.keyframes.track_mut(klipp_nodes::ENABLED_TRACK).keyframes[0].interpolation = klipp_core::Interpolation::Step;
            }, |g| {
                let at = diamond(g)?;
                g.right_click_at(at);
                // The menu is drawn last.
                let item = g.frame(Vec::new()).into_iter().rev().find(|(t, _)| t.trim().trim_start_matches('*').trim() == "Step (Hold)").map(|(_, p)| p).ok_or("no Step (Hold) in the bypass diamond's menu")?;
                g.click_at(item);
                Ok(())
            })
        });
        report.case(&name("key edited by the switch"), [key(Interaction::EditKey)], || {
            let s = with_node(&start()?, keyed(&[(AT, 1.0), (1.25, 1.0)]))?;
            edit_case(&s, keyed(&[(AT, 0.0), (1.25, 1.0)]), |g| {
                let at = switch(g)?;
                g.click_at(at);
                Ok(())
            })
        });
    }
}

/// Every grade type's bypass entry, on the page its node is first drawn on
/// (`rules::surfaces`): the per-type half of the bypass rule.
fn entries() -> Vec<super::parity_manifest::CatalogEntry> {
    catalog().into_iter().filter(|e| e.grade && matches!(e.value, CatalogValue::Enabled) && super::parity_manifest::owed_page(e).is_some()).collect()
}

pub(super) fn require(manifest: &mut Manifest) {
    for entry in entries() {
        if let Some(page) = super::parity_manifest::owed_page(&entry) {
            manifest.require_fixture(&entry, page, "sample");
        }
    }
}

/// A grade holding a node of `kind`, drawn on `page`: placed as the
/// families place it, or, for the grade's own Source and Output, found in
/// a Corrector's grade.
fn opened(kind: NodeKind, page: Surface) -> Result<Grade, String> {
    let (state, clip, node) = match placed(kind) {
        Some(f) => f,
        None => {
            let (state, clip, _) = super::grade_inspector_tests::graded(NodeKind::Corrector).ok_or("no grade")?;
            let node = state.sequences[0].all_clips().find(|c| c.id == clip).and_then(|c| c.grade().nodes.iter().find(|n| n.kind == kind).map(|n| n.id)).ok_or_else(|| format!("no {kind:?} node in a grade"))?;
            (state, clip, node)
        }
    };
    let g = Grade::new(state, clip, node);
    Ok(match page {
        Surface::GradePerNode => g,
        _ => g.on(Page::Tabbed(crate::state::ColorTab::Adjustments)),
    })
}

pub(super) fn run_opened(report: &mut Report) {
    for entry in entries() {
        let Some(page) = super::parity_manifest::owed_page(&entry) else { continue };
        let Some(ty) = klipp_nodes::registry::node_types().find(|t| t.desc.id == entry.type_id) else { continue };
        let key = CoverageKey::new(entry.type_id, &entry.parameter, page, "sample", Interaction::OpenWithoutWrite);
        report.case(&format!("{} opened with its bypass on {page:?}", entry.type_id), [key], || {
            let mut g = opened(ty.kind, page)?;
            if g.root != GradeRoot::Clip {
                return Ok(());
            }
            let before = g.picture();
            g.drawn();
            g.drawn();
            if g.picture() != before || g.state.active_history().undo_count() != 0 || !g.unapplied.is_empty() {
                return Err(format!("opening wrote: {:?}", g.unapplied));
            }
            Ok(())
        });
    }
}

#[test]
fn a_grade_bypass_switches_draws_and_keys_as_one_step() {
    let mut report = Report::default();
    report.family("grade bypass", run);
    report.family_result().unwrap();
    assert!(report.blocked.is_empty(), "a bypass left the native route: {:?}", report.blocked);
}

#[test]
fn a_grade_bypass_does_the_same_on_the_sequences_grades() {
    let mut report = Report::default();
    super::parity_controls::on_roots(&mut report, run);
    report.family_result().unwrap();
    super::parity_controls::assert_on_every_root(&report);
}

#[test]
fn every_grade_type_opens_with_its_bypass_and_writes_nothing() {
    let mut manifest = Manifest::default();
    require(&mut manifest);
    let mut report = Report::default();
    report.family("grade bypass opened", run_opened);
    report.family_result().unwrap();
    assert_eq!(report.passed, manifest.required, "every type's bypass opened");
}
