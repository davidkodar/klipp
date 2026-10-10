//! A grade compound's body (#1476): its structure edits, as the grade
//! canvas makes them, by path at the compound's level. Adding, removing and
//! reordering a node in its body are each one undo step, undone and redone
//! exactly; removing a node another reads too is refused, writing nothing;
//! revealing a node inside it opens the node view there, writing nothing;
//! and an edit inside it changes the frame alike on the CPU and the GPU.
//! On the clip's grade and, through `on_roots`, each sequence grade.
//!
//! The harness doesn't draw the node canvas: each edit is the one the
//! canvas's command is dispatched as (`AppState::grade_structure_edit`'s
//! ops), applied as the app applies it.

use super::parity_controls::Grade;
use super::parity_manifest::{catalog, CatalogValue, CoverageKey, Interaction, Manifest, Report, Surface};
use super::path_commands::GradeStructure;
use klipp_core::AppCommand;
use klipp_effects::registry::presentation::Widget;
use klipp_nodes::{NodeGraph, NodeId, NodeKind};
use klipp_timeline::GradeAccess;

/// The compound body's catalog entry: its type and parameter.
fn entry() -> Option<super::parity_manifest::CatalogEntry> {
    catalog().into_iter().find(|e| matches!(&e.value, CatalogValue::Parameter { presentation, .. } if matches!(presentation.widget, Widget::Custom("compound_body"))))
}

pub(super) fn require(manifest: &mut Manifest) {
    let Some(entry) = entry() else { return };
    manifest.require_fixture(&entry, Surface::GradePerNode, "sample");
    // These cases run on the sequence's grades too (`on_roots`).
    for root in super::parity_harness::GradeRoot::SEQUENCE {
        manifest.require_fixture(&entry, Surface::NonClipGrade, &format!("{}: GradePerNode sample", root.variant()));
    }
}

/// A clip whose grade holds a compound with an Exposure, a Saturation and
/// a Contrast in a chain, each set so it shows in the frame; with
/// `shared`, a Blend in the Contrast's place whose B reads the Exposure
/// too, so the Exposure's picture goes two ways (the edit planner's
/// shared branch). Opened on the compound's page; its three nodes.
fn compounded(shared: bool) -> Result<(Grade, [NodeId; 3]), String> {
    use super::grade_inspector_tests::{chain_of, graded, nest};
    let (mut state, clip, _) = graded(NodeKind::Corrector).ok_or("no grade")?;
    let last = if shared { NodeKind::Blend } else { NodeKind::Contrast };
    let (mut inner, ids) = chain_of(&[NodeKind::Exposure, NodeKind::Saturation, last]);
    for n in &mut inner.nodes {
        match n.kind {
            NodeKind::Exposure => n.params.exposure = Some(0.6),
            NodeKind::Saturation => n.params.saturation = Some(1.6),
            NodeKind::Contrast => n.params.contrast = Some(0.3),
            _ => {}
        }
    }
    if shared {
        // The Blend's B (an image input, not a mask) reads the Exposure.
        inner.edges.push(klipp_nodes::Edge { from_node: ids[0], from_port: 0, to_node: ids[2], to_port: 1 });
    }
    let mut grade = NodeGraph::default();
    let compound = nest(&mut grade, inner);
    state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip)).ok_or("no clip")?.edit_grade(|g| *g = grade);
    Ok((Grade::new(state, clip, compound), [ids[0], ids[1], ids[2]]))
}

/// The compound's body, as stored.
fn body(g: &Grade) -> Result<NodeGraph, String> {
    let grade = g.grade();
    super::grade_inspector_tests::level_of(grade, &[g.node]).ok_or_else(|| "the compound's body is gone".into())
}

/// `edit` at the compound's level, as the canvas's command is dispatched
/// (`AppState::grade_structure_edit`): a refusal is the harness's
/// unapplied, as `Grade::apply` records one.
fn structure(g: &mut Grade, edit: GradeStructure) -> Result<(), String> {
    let descent = vec![g.node];
    super::parity_manifest::touched_the_root();
    // By the clip the grade is edited through, as the canvas sends it; a
    // sequence grade's root needs no stand-in put on for a path edit (and
    // taking one down would write its copy back over the edit).
    let applied = g.state.grade_structure_edit(g.clip, &descent, edit);
    if !applied {
        let why = g.state.toast.message_for_test().unwrap_or_default().to_owned();
        g.unapplied.push(format!("structure edit refused: {why}"));
    }
    Ok(())
}

/// One structure edit from `start`, held by `check` on the body after it:
/// one step, undone and redone exactly.
fn edit_case(start: &Grade, edit: GradeStructure, check: impl FnOnce(&NodeGraph) -> Result<(), String>) -> Result<(), String> {
    let mut g = start.twin();
    g.drawn();
    let before = g.picture();
    structure(&mut g, edit)?;
    g.drawn();
    if !g.unapplied.is_empty() {
        return Err(format!("unapplied {:?}", g.unapplied));
    }
    check(&body(&g)?)?;
    let after = g.picture();
    if after == before {
        return Err("the edit changed nothing".into());
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

pub(super) fn run(report: &mut Report) {
    let Some(entry) = entry() else { return };
    let key = |i: Interaction| CoverageKey::new(entry.type_id, &entry.parameter, Surface::GradePerNode, "sample", i);
    let name = |what: &str| format!("compound body: {what}");
    report.case(&name("opened"), [key(Interaction::OpenWithoutWrite)], || {
        let (mut g, _) = compounded(false)?;
        let before = g.picture();
        g.drawn();
        g.drawn();
        if g.picture() != before || g.state.active_history().undo_count() != 0 || !g.unapplied.is_empty() {
            return Err(format!("opening wrote: {:?}", g.unapplied));
        }
        Ok(())
    });
    report.case(&name("a node added"), [key(Interaction::AddNode), key(Interaction::UndoRedo)], || {
        let (s, _) = compounded(false)?;
        let id = NodeId::new_v4();
        edit_case(&s, GradeStructure::Add { kind: NodeKind::Saturation, id, position: (40.0, 60.0) }, |b| {
            b.nodes.iter().any(|n| n.id == id && n.kind == NodeKind::Saturation).then_some(()).ok_or_else(|| "the node isn't in the compound".into())
        })
    });
    report.case(&name("a node removed"), [key(Interaction::RemoveNode)], || {
        let (s, ids) = compounded(false)?;
        edit_case(&s, GradeStructure::Remove { node: ids[1] }, |b| {
            (!b.nodes.iter().any(|n| n.id == ids[1])).then_some(()).ok_or_else(|| "the node is still in the compound".into())
        })
    });
    report.case(&name("a node reordered"), [key(Interaction::ReorderNode)], || {
        let (s, ids) = compounded(false)?;
        edit_case(&s, GradeStructure::Reorder { node: ids[2], after: ids[0] }, |b| {
            // The Contrast now reads the Exposure.
            b.edges.iter().any(|e| e.from_node == ids[0] && e.to_node == ids[2] && e.to_port == 0).then_some(()).ok_or_else(|| format!("the Contrast doesn't follow the Exposure: {:?}", b.edges))
        })
    });
    report.case(&name("a shared branch refused"), [key(Interaction::SharedBranchRefusal)], || {
        let (s, ids) = compounded(true)?;
        let mut g = s.twin();
        g.drawn();
        let before = g.picture();
        structure(&mut g, GradeStructure::Remove { node: ids[0] })?;
        if !g.unapplied.iter().any(|u| u.contains("shared")) {
            return Err(format!("not refused as shared: {:?}", g.unapplied));
        }
        if g.picture() != before || g.state.active_history().undo_count() != 0 {
            return Err("a refused edit wrote".into());
        }
        Ok(())
    });
    report.case(&name("a node inside revealed"), [key(Interaction::Reveal)], || {
        let (s, ids) = compounded(false)?;
        let mut g = s.twin();
        g.drawn();
        let before = g.picture();
        let compound = g.node;
        let mut path = g.state.grade_level(g.clip, &[compound])?;
        path.nodes.push(ids[1]);
        g.apply(AppCommand::RevealGraph { path: path.clone(), select: true });
        if !g.unapplied.is_empty() {
            return Err(format!("unapplied {:?}", g.unapplied));
        }
        if !g.state.graph_dock_open || g.state.graph_dock_path != path.nodes[..path.nodes.len() - 1] || g.state.selected_node_id != Some(ids[1]) {
            return Err(format!("the node view isn't open at the compound: open {}, at {:?}", g.state.graph_dock_open, g.state.graph_dock_path));
        }
        if g.picture() != before || g.state.active_history().undo_count() != 0 {
            return Err("revealing wrote".into());
        }
        Ok(())
    });
    report.case_or_blocked(&name("drawn"), [key(Interaction::Pixels)], || {
        let (s, ids) = compounded(false)?;
        let mut after = s.twin();
        structure(&mut after, GradeStructure::Remove { node: ids[0] })?;
        if !after.unapplied.is_empty() {
            return Err(format!("unapplied {:?}", after.unapplied));
        }
        super::parity_pixels::edit_drawn(&s.frame_sequence(), &after.frame_sequence())
    });
}

#[test]
fn a_compound_body_s_structure_edits_are_one_step() {
    let mut report = Report::default();
    report.family("compound body", run);
    report.family_result().unwrap();
    assert!(report.blocked.is_empty(), "a compound edit left the native route: {:?}", report.blocked);
}

#[test]
fn a_compound_body_does_the_same_on_the_sequences_grades() {
    let mut report = Report::default();
    super::parity_controls::on_roots(&mut report, run);
    report.family_result().unwrap();
    super::parity_controls::assert_on_every_root(&report);
}
