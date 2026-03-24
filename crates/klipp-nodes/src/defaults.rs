use std::collections::HashMap;

use crate::graph::{Edge, Node, NodeGraph, NodeId, NodePos};
use crate::kinds::NodeKind;
use crate::params::{NodeKeyframeTracks, NodeParams};

/// Build the default node graph for a new clip.
/// Topologically equivalent to the old 12-step linear pipeline.
/// Order: Source → IDT → Exposure → Tone → Wheels → WheelSatBri →
///        Contrast → Saturation → Curves → HueSat → ColorBalance → Output
pub fn default_graph() -> NodeGraph {
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
        make_edge(huesat_id, 0, colorbal_id, 0),
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
