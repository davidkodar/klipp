//! Remove only the subject/image-matte cut-out, preserving other readers.
use crate::{NodeGraph, NodeId, NodeKind};

impl NodeGraph {
    /// A direct External Matte into Output alpha, as the AI cut-out tools
    /// create it. Other keys and wireless alpha connections are user wiring.
    pub fn cut_out_matte(&self) -> Option<NodeId> {
        let edge = self
            .edges
            .iter()
            .find(|e| e.to_node == self.output_node_id && e.to_port == 1 && e.from_port == 0)?;
        self.nodes
            .iter()
            .find(|n| {
                n.id == edge.from_node
                    && n.kind == NodeKind::ExternalMatte
                    && (n.params.matte_subject.is_some() || n.params.matte_image_path.is_some())
            })
            .map(|n| n.id)
    }

    /// Restore the whole clip. Keep a shared matte while any explicit or
    /// wireless reader uses it. Returns false without changing other alphas.
    pub fn restore_background(&mut self) -> bool {
        let Some(matte) = self.cut_out_matte() else {
            return false;
        };
        self.edges.retain(|e| {
            !(e.from_node == matte && e.to_node == self.output_node_id && e.to_port == 1)
        });
        // Removing the explicit edge can expose a wireless receiver for
        // the very same matte. Disconnect that use too, before deciding
        // whether any other reader still needs the node.
        if self.input_of(self.output_node_id, 1) == Some(matte) {
            if let Some(receiver) = self
                .nodes
                .iter_mut()
                .find(|n| n.id == self.output_node_id)
                .and_then(|n| n.input_links.get_mut(1))
            {
                *receiver = None;
            }
        }
        let linked = self
            .nodes
            .iter()
            .find(|n| n.id == matte)
            .and_then(|n| n.output_link.as_ref())
            .is_some_and(|label| {
                self.nodes.iter().any(|n| {
                    n.id != matte && n.input_links.iter().any(|i| i.as_ref() == Some(label))
                })
            });
        if !linked && !self.edges.iter().any(|e| e.from_node == matte) {
            self.nodes.retain(|n| n.id != matte);
            self.edges
                .retain(|e| e.from_node != matte && e.to_node != matte);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Edge, Node, NodeParams, SubjectMatte};
    fn node(kind: NodeKind) -> Node {
        Node {
            id: NodeId::new_v4(),
            kind,
            label: String::new(),
            enabled: true,
            params: Default::default(),
            keyframes: Default::default(),
            output_link: None,
            input_links: Vec::new(),
        }
    }
    fn cut_out(still: bool) -> (NodeGraph, NodeId) {
        let mut g = NodeGraph::default();
        let mut n = node(NodeKind::ExternalMatte);
        n.params = if still {
            NodeParams {
                matte_image_path: Some("/tmp/mask.png".into()),
                ..Default::default()
            }
        } else {
            NodeParams {
                matte_subject: Some(SubjectMatte::Subject),
                ..Default::default()
            }
        };
        let id = n.id;
        g.nodes.push(n);
        g.edges.push(Edge {
            from_node: id,
            from_port: 0,
            to_node: g.output_node_id,
            to_port: 1,
        });
        (g, id)
    }
    #[test]
    fn restore_background_removes_only_the_cut_out_and_unshared_matte() {
        for still in [false, true] {
            let (mut g, id) = cut_out(still);
            assert_eq!(g.cut_out_matte(), Some(id));
            assert!(g.restore_background());
            assert!(g.input_of(g.output_node_id, 1).is_none());
            assert!(!g.nodes.iter().any(|n| n.id == id));
            let before = g.clone();
            assert!(!g.restore_background());
            assert_eq!(g, before);
        }
    }
    #[test]
    fn restore_background_preserves_any_other_reader() {
        for wireless in [false, true] {
            let (mut g, id) = cut_out(false);
            let reader = g.ensure_corrector();
            if wireless {
                g.nodes.iter_mut().find(|n| n.id == id).unwrap().output_link =
                    Some("subject".into());
                g.nodes
                    .iter_mut()
                    .find(|n| n.id == reader)
                    .unwrap()
                    .input_links = vec![None, Some("subject".into())];
            } else {
                g.edges.push(Edge {
                    from_node: id,
                    from_port: 0,
                    to_node: reader,
                    to_port: 1,
                });
            }
            assert!(g.restore_background());
            assert!(g.nodes.iter().any(|n| n.id == id));
            assert_eq!(g.input_of(reader, 1), Some(id));
        }
        let (mut g, id) = cut_out(false);
        let reader = node(NodeKind::Mask);
        g.edges.push(Edge {
            from_node: id,
            from_port: 0,
            to_node: reader.id,
            to_port: 0,
        });
        g.nodes.push(reader);
        assert!(g.restore_background());
        assert!(g.nodes.iter().any(|n| n.id == id));
    }
    #[test]
    fn restore_background_clears_the_same_mattes_wireless_alpha_fallback() {
        for shared in [false, true] {
            let (mut g, id) = cut_out(false);
            g.nodes.iter_mut().find(|n| n.id == id).unwrap().output_link = Some("subject".into());
            g.nodes
                .iter_mut()
                .find(|n| n.id == g.output_node_id)
                .unwrap()
                .input_links = vec![None, Some("subject".into())];
            let reader = shared.then(|| g.ensure_corrector());
            if let Some(reader) = reader {
                g.nodes
                    .iter_mut()
                    .find(|n| n.id == reader)
                    .unwrap()
                    .input_links = vec![None, Some("subject".into())];
            }
            assert_eq!(g.input_of(g.output_node_id, 1), Some(id));
            assert!(g.restore_background());
            assert_eq!(g.input_of(g.output_node_id, 1), None);
            assert!(g
                .nodes
                .iter()
                .find(|n| n.id == g.output_node_id)
                .unwrap()
                .input_links[1]
                .is_none());
            assert_eq!(g.nodes.iter().any(|n| n.id == id), shared);
            if let Some(reader) = reader {
                assert_eq!(
                    g.input_of(reader, 1),
                    Some(id),
                    "other wireless readers survive"
                );
            }
        }
    }

    #[test]
    fn restore_background_preserves_an_unrelated_wireless_alpha() {
        let (mut g, _) = cut_out(false);
        let mut other = node(NodeKind::Mask);
        other.output_link = Some("manual key".into());
        let other_id = other.id;
        g.nodes.push(other);
        g.nodes
            .iter_mut()
            .find(|n| n.id == g.output_node_id)
            .unwrap()
            .input_links = vec![None, Some("manual key".into())];
        assert!(g.restore_background());
        assert_eq!(g.input_of(g.output_node_id, 1), Some(other_id));
        assert_eq!(
            g.nodes
                .iter()
                .find(|n| n.id == g.output_node_id)
                .unwrap()
                .input_links[1]
                .as_deref(),
            Some("manual key")
        );
    }

    #[test]
    fn restore_background_leaves_hand_wired_keys_alone() {
        for kind in [NodeKind::Mask, NodeKind::KeyMixer, NodeKind::ExternalMatte] {
            let (mut g, id) = cut_out(false);
            let n = g.nodes.iter_mut().find(|n| n.id == id).unwrap();
            n.kind = kind;
            n.params = Default::default();
            let before = g.clone();
            assert!(g.cut_out_matte().is_none());
            assert!(!g.restore_background());
            assert_eq!(g, before);
        }
    }
}
