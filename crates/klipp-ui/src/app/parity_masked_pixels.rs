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
    for key in super::parity_manifest::representatives_of(super::parity_manifest::Interaction::MaskedPixels) {
        manifest.require(key);
    }
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
    let mut run = Run::new(start.clone());
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

/// A masked effect's card picks the channel it reads its mask by
/// (#1348): "Mask channel" is on the card a mask feeds and not on one no
/// mask does; it shows the channel set, and its choice is the effect's
/// channel by path, one step that undo takes back.
#[test]
fn a_masked_effect_s_card_picks_its_mask_channel() {
    let (plain, window) = clip(Kind::Beside);
    let mut masked_clip = plain.clone();
    masked_clip.apply_picture_ops(&masking(&plain, Kind::Beside, window).unwrap()).unwrap();
    for (c, masked) in [(plain, false), (masked_clip, true)] {
        let mut state = crate::state::AppState::default();
        state.sequences[0].tracks[0].clips.push(c.clone());
        let ctx = egui::Context::default();
        let mut clock = 0.0;
        let mut frame = |state: &crate::state::AppState, events: Vec<egui::Event>| {
            clock += 0.1;
            let mut cmds = Vec::new();
            let c = state.sequences[0].all_clips().find(|x| x.id == c.id).unwrap().clone();
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 1600.0))), time: Some(clock), events, ..Default::default() };
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| crate::inspector::stack_view::show(ui, &crate::theme::KlippTheme::default(), state, &c, &mut cmds, 0.0));
            });
            let texts: Vec<(String, egui::Pos2)> = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.visual_bounding_rect().center())),
                _ => None,
            }).collect();
            (cmds, texts)
        };
        frame(&state, Vec::new());
        let texts = frame(&state, Vec::new()).1;
        let row = texts.iter().find(|(t, _)| t == "Mask channel");
        assert_eq!(row.is_some(), masked, "masked {masked}: {texts:?}");
        if !masked {
            continue;
        }
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let click = |frame: &mut dyn FnMut(&crate::state::AppState, Vec<egui::Event>) -> (Vec<klipp_core::AppCommand>, Vec<(String, egui::Pos2)>), state: &crate::state::AppState, at: egui::Pos2| {
            frame(state, vec![egui::Event::PointerMoved(at), button(at, true)]);
            let cmds = frame(state, vec![button(at, false)]).0;
            (cmds, frame(state, Vec::new()).1)
        };
        let y = row.unwrap().1.y;
        // The channel shown, right of the label: the window set Luminance.
        let shown = texts.iter().find(|(t, p)| t == "Luminance" && (p.y - y).abs() < 6.0).unwrap_or_else(|| panic!("the channel shown: {texts:?}")).1;
        let (_, texts) = click(&mut frame, &state, shown);
        let alpha = texts.iter().find(|(t, _)| t == "Alpha").unwrap_or_else(|| panic!("no Alpha choice: {texts:?}")).1;
        let (cmds, _) = click(&mut frame, &state, alpha);
        let fx = c.picture_v2().unwrap().picture.graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("gaussian_blur")).unwrap().id;
        let addr = NodePath::new(Root::Clip { clip: c.id.0 }, [fx]);
        assert!(matches!(cmds.as_slice(), [klipp_core::AppCommand::SetMaskChannelAt { addr: a, channel: Some(ch) }] if *a == addr && ch == "alpha"), "{cmds:?}");
        let before = state.sequences[0].all_clips().find(|x| x.id == c.id).unwrap().picture_v2().unwrap().picture.clone();
        assert_eq!(state.path_command(&cmds[0]), Some(true), "{:?}", state.toast.message_for_test());
        let channel = |state: &crate::state::AppState| state.sequences[0].all_clips().find(|x| x.id == c.id).unwrap().picture_v2().unwrap().picture.graph.nodes.iter().find(|n| n.id == fx).unwrap().params.get(&klipp_graph_format::ParamKey::from("mask.channel")).cloned();
        assert_eq!(channel(&state), Some(klipp_graph_format::ParamValue::Text("alpha".into())));
        assert_eq!(state.active_history().undo_count(), 1, "one step");
        let entry = state.active_history_mut().undo().unwrap();
        let mut command = entry.command;
        if let klipp_document::history::EditCommand::Named { edit, .. } = command {
            command = *edit;
        }
        let klipp_document::history::EditCommand::PictureEdit { clip_id, edit, .. } = command else { panic!("a picture edit") };
        state.set_picture(clip_id, &edit.before);
        assert_eq!(state.sequences[0].all_clips().find(|x| x.id == c.id).unwrap().picture_v2().unwrap().picture, before, "undo takes it back");
    }
}
