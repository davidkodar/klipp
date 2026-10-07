//! The node descriptions graph ops check ports, masks and mix against:
//! the effect and grade registries, read as `klipp-graph-format`'s
//! [`PortCatalog`] (node-graph M2 step 15).

use klipp_graph_format::{types, Node, NodeSpec, PortCatalog, PortSpec, PortType};

/// Every built-in effect and grade node type, from their registries.
#[derive(Clone, Copy, Debug, Default)]
pub struct RegistryCatalog;

impl RegistryCatalog {
    fn describe(desc: &klipp_effects::registry::NodeTypeDesc) -> NodeSpec {
        let ports = |ports: &[klipp_effects::registry::PortDesc]| {
            ports
                .iter()
                .map(|p| PortSpec {
                    key: p.key.into(),
                    ty: match p.ty {
                        klipp_effects::registry::PortType::Image => PortType::Image,
                        klipp_effects::registry::PortType::Matte => PortType::Matte,
                    },
                    optional: p.optional,
                    params: p.params.iter().map(|p| (p.key.into(), p.options.iter().map(|s| (*s).into()).collect())).collect(),
                })
                .collect()
        };
        NodeSpec { inputs: ports(desc.inputs), outputs: ports(desc.outputs), mixable: desc.mixable }
    }
}

impl PortCatalog for RegistryCatalog {
    fn node(&self, type_id: &str) -> Option<NodeSpec> {
        klipp_effects::registry::effect_types()
            .map(|t| &t.desc)
            .chain(klipp_nodes::registry::node_types().map(|t| &t.desc))
            .find(|d| d.id == type_id)
            .map(Self::describe)
    }

    fn node_spec(&self, node: &Node) -> Option<NodeSpec> {
        if node.type_id.as_str() == types::LEGACY_NODE {
            let name = node.legacy.as_ref()?.payload.get("kind")?.as_str()?;
            let kind: klipp_nodes::NodeKind = serde_json::from_value(serde_json::json!(name)).ok()?;
            let mut spec = Self::describe(&klipp_nodes::registry::node_type(kind)?.desc);
            // Registry descriptors use semantic names (Qualifier's matte),
            // while migrated graphs retain the keys from their old indices.
            for (index, p) in spec.inputs.iter_mut().enumerate() {
                p.key = klipp_graph_format::ports::input_key(name, index as u8).to_string();
            }
            for (index, p) in spec.outputs.iter_mut().enumerate() {
                p.key = klipp_graph_format::ports::output_key(name, index as u8).to_string();
            }
            return Some(spec);
        }
        if node.type_id.as_str() == types::LEGACY_EFFECT {
            let tag = node.legacy.as_ref()?.payload.get("type")?.as_str()?;
            return klipp_effects::registry::effect_type(tag).map(|t| Self::describe(&t.desc));
        }
        self.node(node.type_id.as_str())
    }
}
