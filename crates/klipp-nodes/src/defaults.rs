use std::collections::HashMap;

use crate::graph::{Edge, Node, NodeGraph, NodeId, NodePos};
use crate::kinds::NodeKind;
use crate::params::{NodeKeyframeTracks, NodeParams};

/// Build the default node graph for a new clip — Source → Corrector
/// → Output. The Corrector bundles every primary-grade param so the
/// inspector's tab-based UI (Wheels / Curves / Hue-Sat / Warper /
/// Adjustments) always has somewhere to write without forcing the
/// user to add nodes manually first. Power users add additional
/// single-purpose nodes via the V2 editor when they want granular
/// pipelines, parallel mixers, secondaries, etc.
///
/// (Earlier versions pre-wired all 12 grade nodes — see
/// `default_full_chain_graph`, kept around for tests.)
pub fn default_graph() -> NodeGraph {
    let source_id = NodeId::new_v4();
    let corrector_id = NodeId::new_v4();
    let output_id = NodeId::new_v4();
    let nodes = vec![
        make_node(source_id, NodeKind::Source, "Source"),
        make_node(corrector_id, NodeKind::Corrector, "Corrector"),
        make_node(output_id, NodeKind::Output, "Output"),
    ];
    let edges = vec![
        make_edge(source_id, 0, corrector_id, 0),
        make_edge(corrector_id, 0, output_id, 0),
    ];
    let mut node_positions = HashMap::new();
    node_positions.insert(source_id, NodePos { x: 0.0, y: 0.0 });
    node_positions.insert(corrector_id, NodePos { x: 220.0, y: 0.0 });
    node_positions.insert(output_id, NodePos { x: 440.0, y: 0.0 });
    NodeGraph {
        nodes,
        edges,
        output_node_id: output_id,
        node_positions,
        working_space_override: None,
    }
}

/// The legacy 12-step linear chain that used to be the default — kept
/// for tests that exercise specific grade nodes by kind, and for any
/// future "Insert default chain" UI that wants to populate everything
/// in one shot.
///
/// Order: Source → IDT → Exposure → Tone → Wheels → WheelSatBri →
///        Contrast → Saturation → Curves → HueSat → ColorBalance → Output
pub fn default_full_chain_graph() -> NodeGraph {
    let source_id = NodeId::new_v4();
    let idt_id = NodeId::new_v4();
    let exposure_id = NodeId::new_v4();
    let tone_id = NodeId::new_v4();
    let wheels_id = NodeId::new_v4();
    let wheelsatbri_id = NodeId::new_v4();
    let contrast_id = NodeId::new_v4();
    let saturation_id = NodeId::new_v4();
    let curves_id = NodeId::new_v4();
    let huesat_id = NodeId::new_v4();
    let warper_id = NodeId::new_v4();
    let colorbal_id = NodeId::new_v4();
    let output_id = NodeId::new_v4();

    let nodes = vec![
        make_node(source_id, NodeKind::Source, "Source"),
        make_node(idt_id, NodeKind::InputTransform, "Input Transform"),
        make_node(exposure_id, NodeKind::Exposure, "Exposure"),
        make_node(tone_id, NodeKind::ToneControls, "Tone"),
        make_node(wheels_id, NodeKind::ColorWheels, "Color Wheels"),
        make_node(wheelsatbri_id, NodeKind::WheelSatBri, "Wheel Sat/Bri"),
        make_node(contrast_id, NodeKind::Contrast, "Contrast"),
        make_node(saturation_id, NodeKind::Saturation, "Saturation"),
        make_node(curves_id, NodeKind::RGBCurves, "Curves"),
        make_node(huesat_id, NodeKind::HueSatCurves, "Hue/Sat"),
        make_node(warper_id, NodeKind::ColorWarper, "Color Warper"),
        make_node(colorbal_id, NodeKind::ColorBalance, "Color Balance"),
        make_node(output_id, NodeKind::Output, "Output"),
    ];

    let edges = vec![
        make_edge(source_id, 0, idt_id, 0),
        make_edge(idt_id, 0, exposure_id, 0),
        make_edge(exposure_id, 0, tone_id, 0),
        make_edge(tone_id, 0, wheels_id, 0),
        make_edge(wheels_id, 0, wheelsatbri_id, 0),
        make_edge(wheelsatbri_id, 0, contrast_id, 0),
        make_edge(contrast_id, 0, saturation_id, 0),
        make_edge(saturation_id, 0, curves_id, 0),
        make_edge(curves_id, 0, huesat_id, 0),
        make_edge(huesat_id, 0, warper_id, 0),
        make_edge(warper_id, 0, colorbal_id, 0),
        make_edge(colorbal_id, 0, output_id, 0),
    ];

    // Pre-set positions for a clean left-to-right layout.
    let ids = [
        source_id,
        idt_id,
        exposure_id,
        tone_id,
        wheels_id,
        wheelsatbri_id,
        contrast_id,
        saturation_id,
        curves_id,
        huesat_id,
        warper_id,
        colorbal_id,
        output_id,
    ];
    let mut node_positions = HashMap::new();
    for (i, id) in ids.iter().enumerate() {
        node_positions.insert(
            *id,
            NodePos {
                x: i as f32 * 200.0,
                y: 0.0,
            },
        );
    }

    NodeGraph {
        nodes,
        edges,
        output_node_id: output_id,
        node_positions,
        working_space_override: None,
    }
}

fn make_node(id: NodeId, kind: NodeKind, label: &str) -> Node {
    Node {
        id,
        kind,
        label: label.to_string(),
        enabled: true,
        params: NodeParams::default(),
        keyframes: NodeKeyframeTracks::default(),
        output_link: None,
        input_links: Vec::new(),
    }
}

fn make_edge(from_node: NodeId, from_port: u8, to_node: NodeId, to_port: u8) -> Edge {
    Edge {
        from_node,
        from_port,
        to_node,
        to_port,
    }
}
