//! A clip's opacity faded across the frame (#2704): one gradient Mask
//! node from the Source into the Output's alpha, found again by its label.
use crate::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
use klipp_core::{Mask, MaskStack};

/// The label of the node the opacity gradient lives in.
pub const OPACITY_GRADIENT: &str = "Opacity Gradient";

/// Why the opacity gradient could not be set.
#[derive(Debug, Clone, PartialEq)]
pub enum OpacityGradientError {
    /// The Output's alpha is another matte already, by its name.
    AlphaTaken(String),
    /// Nothing to take off.
    NoneToClear,
}

impl NodeGraph {
    /// The node of the clip's opacity gradient, when it has one.
    pub fn opacity_gradient(&self) -> Option<NodeId> {
        self.nodes.iter().find(|n| n.kind == NodeKind::Mask && n.label == OPACITY_GRADIENT).map(|n| n.id)
    }

    /// The clip's opacity as `mask` across the frame, the one there
    /// replaced; `None` takes it off. Another matte already the alpha is
    /// left as it is.
    pub fn set_opacity_gradient(&mut self, mask: Option<Mask>) -> Result<(), OpacityGradientError> {
        let ours = self.opacity_gradient();
        if let Some(other) = self.input_of(self.output_node_id, 1).filter(|n| Some(*n) != ours) {
            let name = self.nodes.iter().find(|n| n.id == other).map_or_else(|| "a matte".to_string(), |n| if n.label.is_empty() { n.kind.display_name().to_string() } else { n.label.clone() });
            return Err(OpacityGradientError::AlphaTaken(name));
        }
        let Some(mask) = mask else {
            let node = ours.ok_or(OpacityGradientError::NoneToClear)?;
            self.nodes.retain(|n| n.id != node);
            self.edges.retain(|e| e.from_node != node && e.to_node != node);
            return Ok(());
        };
        let params = NodeParams { masks: Some(MaskStack { masks: vec![mask] }), ..Default::default() };
        if let Some(node) = ours {
            self.nodes.iter_mut().find(|n| n.id == node).expect("found above").params = params;
            return Ok(());
        }
        if !self.nodes.iter().any(|n| n.kind == NodeKind::Source) {
            *self = crate::defaults::default_graph();
        }
        let source = self.nodes.iter().find(|n| n.kind == NodeKind::Source).expect("a graph with a source").id;
        let node = NodeId::new_v4();
        self.nodes.push(Node {
            id: node,
            kind: NodeKind::Mask,
            label: OPACITY_GRADIENT.into(),
            enabled: true,
            params,
            keyframes: Default::default(),
            output_link: None,
            input_links: Vec::new(),
        });
        self.edges.push(Edge { from_node: source, from_port: 0, to_node: node, to_port: 0 });
        self.edges.push(Edge { from_node: node, from_port: 0, to_node: self.output_node_id, to_port: 1 });
        Ok(())
    }
}
