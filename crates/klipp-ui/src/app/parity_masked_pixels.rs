//! A masked effect's edits drawn by the real renderer:
//! `MaskedPixels`, one effect (a Gaussian Blur) per mask kind.
//!
//! A mask is wired on the graph canvas (or over the socket), the same in
//! either view; what the flip moves is the masked effect's row. A mask
//! makes the clip's picture whole (picture-truth), and the inspector's rows
//! write by path. Each case wires the mask into the fixture, types the
//! blur's radius in the inspector, and requires:
//! - the active control: the masked frame differs from the same clip
//!   unmasked, on each route;
//! - the picture setting the radius by path leaves, the mask kept, and the
//!   edit drawn: it changes the frame, the CPU's within a level of the
//!   GPU's;
//! - the GPU route gives the mask (`effects_mask_unavailable` 0), else
//!   BLOCKED (`parity_pixels`).
//!
//! The mask kinds are the matte producers an effect can read: a window
//! inside the grade, published by the grade group, and a window node
//! beside the clip's chain.

use super::parity_harness::Run;
use super::parity_manifest::{CoverageKey, Interaction, Manifest, Report, Surface};
use super::parity_numeric::{type_value, RowLabel};
use super::parity_pixels::{clip_edit_drawn, frames_at, Routes, AT, SMALL};
use klipp_core::{Mask, MaskShape, MaskStack};
use klipp_effects::BuiltinEffect;
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::PortBinding;
use klipp_graph_lift::ops::GraphOp;
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
use klipp_timeline::GradeAccess;

const TYPE: &str = "klipp.fx.gaussian_blur";

#[derive(Clone, Copy, Debug)]
enum Kind {
    /// A window inside the grade, reading its source, published by the
    /// grade group.
    InTheGrade,
    /// A window node beside the clip's chain, reading its source.
    Beside,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::InTheGrade => "@mask/in-the-grade",
            Kind::Beside => "@mask/beside",
        }
    }
}

fn key(kind: Kind) -> CoverageKey {
    CoverageKey::new(TYPE, kind.name(), Surface::EffectsStack, "masked", Interaction::MaskedPixels)
}

/// The keys this family earns: the central rules' representatives
/// (`rules::REPRESENTATIVE`), one per mask kind.
pub(super) fn require(manifest: &mut Manifest) {
    manifest.require_representatives();
}

/// A circle in the frame's middle, feathered.
fn window_params() -> NodeParams {
    let masks = MaskStack { masks: vec![Mask { shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 }, feather: 0.1, ..Mask::default() }] };
    NodeParams { masks: Some(masks), ..Default::default() }
}

fn grade_node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

/// A clip with a blur, its grade holding the window when the mask is
/// inside it, and the window's id there.
fn clip(kind: Kind) -> (klipp_timeline::Clip, Option<NodeId>) {
    let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut c = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    c.push_effect(BuiltinEffect::GaussianBlur { radius: 4.0 });
    let mut window = None;
    if matches!(kind, Kind::InTheGrade) {
        let source = grade_node(NodeKind::Source, NodeParams::default());
        let output = grade_node(NodeKind::Output, NodeParams::default());
        let w = grade_node(NodeKind::Mask, window_params());
        let edges = vec![
            Edge { from_node: source.id, from_port: 0, to_node: output.id, to_port: 0 },
            Edge { from_node: source.id, from_port: 0, to_node: w.id, to_port: 0 },
        ];
        window = Some(w.id);
        c.set_grade(NodeGraph { output_node_id: output.id, nodes: vec![source, output, w], edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] });
    }
    (c, window)
}

/// A window as a node beside the chain: the lift's node for a grade's
/// Mask, with the window's settings.
fn side_window() -> klipp_graph_format::Node {
    let params = serde_json::to_value(window_params()).unwrap();
    klipp_graph_format::Node {
        id: klipp_graph_format::NodeId::from_u128(0x5e1de),
        type_id: klipp_graph_format::types::LEGACY_NODE.into(),
        type_version: 1,
        label: String::new(),
        enabled: true,
        params: Default::default(),
        anim: Default::default(),
        mix: None,
        wireless: Default::default(),
        group: None,
        legacy: Some(klipp_graph_format::Legacy { slot: None, payload: serde_json::json!({"kind": "Mask", "label": "Window", "params": params}), keyframes: Some(serde_json::json!({"tracks": []})), modifiers: Default::default() }),
        legacy_flags: None,
        origin: None,
    }
}

/// The canvas's ops that mask the clip's blur by its window.
fn masking(c: &klipp_timeline::Clip, kind: Kind, window: Option<NodeId>) -> Result<Vec<GraphOp>, String> {
    use klipp_graph_format::ports::{input_key, output_key};
    let p = c.picture_v2()?.picture.clone();
    let at = |ids: &[klipp_graph_format::NodeId]| NodePath::new(Root::Clip { clip: c.id.0 }, ids.iter().copied());
    let find = |t: &str| p.graph.nodes.iter().find(|n| n.type_id.as_str() == t).map(|n| n.id).ok_or_else(|| format!("no {t}"));
    let fx = find(klipp_graph_format::types::LEGACY_EFFECT)?;
    Ok(match kind {
        Kind::InTheGrade => {
            let group = find(lt::GROUP)?;
            vec![
                GraphOp::PublishOutput { group: at(&[group]), endpoint: PortBinding { node: window.ok_or("the window")?, port: "out".into() }, key: "selection".into(), label: "Selection".into() },
                GraphOp::Connect { from: at(&[group]), from_port: "selection".into(), to: at(&[fx]), to_port: "mask".into() },
            ]
        }
        Kind::Beside => {
            let source = find(lt::SOURCE)?;
            let side = side_window();
            vec![
                GraphOp::AddNodeBeside { at: at(&[fx]), node: Box::new(side.clone()) },
                GraphOp::Connect { from: at(&[source]), from_port: "out".into(), to: at(&[side.id]), to_port: input_key("Mask", 0) },
                GraphOp::Connect { from: at(&[side.id]), from_port: output_key("Mask", 0), to: at(&[fx]), to_port: "mask".into() },
            ]
        }
    })
}

/// Whether the clip's blur reads a mask.
fn masked(c: &klipp_timeline::Clip) -> Result<bool, String> {
    let p = c.picture_v2()?.picture.clone();
    let fx = p.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::types::LEGACY_EFFECT).map(|n| n.id);
    Ok(p.graph.edges.iter().any(|e| Some(e.to.0) == fx && e.to.1.as_str() == "mask"))
}

fn case(kind: Kind) -> Result<(), String> {
    let (plain, window) = clip(kind);
    let mut start = plain.clone();
    start.apply_picture_ops(&masking(&plain, kind, window)?)?;
    if !masked(&start)? {
        return Err("the fixture's blur reads no mask".into());
    }
    // The active control: the mask shows.
    if frames_at(Routes::Both, SMALL, &start, AT)? == frames_at(Routes::Both, SMALL, &plain, AT)? {
        return Err("the masked frame is the unmasked one".into());
    }
    // The view types the radius; the expected clip has it set by path.
    let mut run = Run::new(start.clone(), true);
    type_value(&mut run, RowLabel { text: "Radius", group: None }, "12")?;
    if !run.unapplied.is_empty() {
        return Err(format!("unapplied {:?}", run.unapplied));
    }
    let new = run.clip().clone();
    if !masked(&new)? {
        return Err("the edit lost the mask".into());
    }
    let p = start.picture_v2()?.picture.clone();
    let fx = p.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::types::LEGACY_EFFECT).map(|n| n.id).ok_or("no blur")?;
    let mut expected = start.clone();
    expected.apply_picture_ops(&[GraphOp::SetParam { path: NodePath::new(Root::Clip { clip: start.id.0 }, [fx]), key: klipp_graph_format::ParamKey::from("radius"), value: klipp_graph_format::ParamValue::Float(12.0) }])?;
    if new.picture_v2()? != expected.picture_v2()? {
        return Err("the typed radius left another picture than setting it by path does".into());
    }
    clip_edit_drawn(&start, &new)
}

pub(super) fn run(report: &mut Report) {
    for kind in [Kind::InTheGrade, Kind::Beside] {
        report.case_or_blocked(&format!("{TYPE} masked {kind:?} / pixels"), [key(kind)], || case(kind));
    }
}

#[test]
fn a_masked_effects_edit_draws_as_the_document_op_on_both_routes() {
    let mut manifest = Manifest::default();
    require(&mut manifest);
    let mut report = Report::default();
    run(&mut report);
    report.family_result().unwrap();
    assert!(report.blocked.is_empty(), "a masked effect left the native route: {:?}", report.blocked);
    assert_eq!(report.passed, manifest.required, "every mask kind earned");
}
