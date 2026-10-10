//! The picture graph as canvas data. Port indices belong only to the
//! drawing; every emitted edit resolves them back to stored port keys.

use std::collections::HashMap;
use klipp_core::AppCommand;
use klipp_effects::registry::{Category, NodeTypeDesc, PortDesc};
use klipp_graph_format::{Graph, Node, NodeId, PortCatalog, PortDirection, PortKey, PortSpec, PortType};
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodeOutput, NodePath};
use klipp_graph_format::op::GraphOp;
use klipp_nodes::{Edge, NodeFrame, NodeKind, NodePos, NodeStickyNote, PortRole};
use super::model::{CanvasGraph, CanvasKind, CanvasNode, MenuCategory, Palette, category_palette};

pub(super) struct PictureNode<'a> {
    node: &'a Node,
    label: String,
    desc: Option<&'static NodeTypeDesc>,
    native: Option<klipp_nodes::Node>,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    input_links: Vec<Option<String>>,
}

pub(super) struct PictureCanvas<'a> {
    graph: &'a Graph,
    level: NodePath,
    nodes: Vec<PictureNode<'a>>,
    edges: Vec<Edge>,
    positions: HashMap<NodeId, NodePos>,
    frames: Vec<NodeFrame>,
    notes: Vec<NodeStickyNote>,
    after: Option<NodeId>,
    pub(super) keyboard_enabled: bool,
}

fn image_port(key: &str) -> PortSpec {
    PortSpec { key: key.into(), ty: PortType::Image, optional: false, params: Vec::new() }
}

fn native(node: &Node) -> Option<klipp_nodes::Node> {
    if !matches!(node.type_id.as_str(), klipp_graph_format::types::LEGACY_NODE | lt::MERGE | lt::GROUP_INPUT | lt::GROUP_OUTPUT) { return None; }
    let legacy = node.legacy.as_ref()?;
    let kind: NodeKind = serde_json::from_value(legacy.payload.get("kind")?.clone()).ok()?;
    Some(klipp_nodes::Node {
        id: node.id, kind, label: node.label.clone(), enabled: node.enabled,
        params: legacy.payload.get("params").cloned().map(serde_json::from_value).transpose().ok()?.unwrap_or_default(),
        keyframes: legacy.keyframes.clone().map(serde_json::from_value).transpose().ok()?.unwrap_or_default(),
        output_link: node.wireless.output_link.clone(), input_links: Vec::new(),
    })
}

fn description(node: &Node, native: Option<&klipp_nodes::Node>) -> Option<&'static NodeTypeDesc> {
    if let Some(n) = native {
        return klipp_nodes::registry::node_type(n.kind).map(|t| &t.desc);
    }
    klipp_graph_lift::effect_params::tag(node)
        .and_then(klipp_effects::registry::effect_type).map(|t| &t.desc)
        .or_else(|| klipp_effects::registry::effect_types().map(|t| &t.desc).find(|d| d.id == node.type_id.as_str()))
}

fn structural_name(node: &Node) -> &str {
    match node.type_id.as_str() {
        lt::SOURCE => "Source", lt::OUTPUT => "Output", lt::GROUP => "Grade",
        lt::GROUP_INPUT => "Input", lt::GROUP_OUTPUT => "Output", lt::GROUP_FRAME => "Frame",
        lt::PLACEMENT => "Placement", lt::SCENE_TO_DISPLAY => "Scene to Display",
        lt::TEMPORAL_DENOISE => "Temporal Denoise", lt::FRAME_BLEND => "Frame Blend",
        _ => node.type_id.as_str(),
    }
}

impl<'a> PictureCanvas<'a> {
    pub(super) fn new(root: &'a Graph, level: NodePath, selected: Option<NodeId>) -> Result<Self, String> {
        let mut graph = root;
        for id in &level.nodes {
            graph = &graph.nodes.iter().find(|n| n.id == *id).and_then(|n| n.group.as_ref())
                .ok_or_else(|| format!("The graph group {id} is no longer available"))?.graph;
        }
        let mut nodes = Vec::with_capacity(graph.nodes.len());
        for node in &graph.nodes {
            let native = native(node);
            let desc = description(node, native.as_ref());
            let spec = klipp_timeline::RegistryCatalog.node_spec(node).or_else(|| {
                // Lifted merge nodes retain a grade kind but use a structural
                // type id. Their unused sockets must still be shown.
                native.as_ref()?;
                let desc = desc?;
                let kind = node.legacy.as_ref()?.payload.get("kind")?.as_str()?;
                let ports = |ports: &[PortDesc], output| ports.iter().enumerate().map(|(i, p)| PortSpec {
                    key: (if output { klipp_graph_format::ports::output_key(kind, i as u8) } else { klipp_graph_format::ports::input_key(kind, i as u8) }).to_string(),
                    ty: if p.ty == klipp_effects::registry::PortType::Matte { PortType::Matte } else { PortType::Image },
                    optional: p.optional, params: Vec::new(),
                }).collect();
                Some(klipp_graph_format::NodeSpec { inputs: ports(desc.inputs, false), outputs: ports(desc.outputs, true), mixable: desc.mixable })
            });
            let (mut inputs, mut outputs) = spec.map(|s| (s.inputs, s.outputs)).unwrap_or_else(|| {
                match node.type_id.as_str() {
                    lt::SOURCE | lt::GROUP_INPUT | lt::GROUP_FRAME => (Vec::new(), vec![image_port("out")]),
                    lt::OUTPUT | lt::GROUP_OUTPUT => (vec![image_port("in")], Vec::new()),
                    _ => (vec![image_port("in")], vec![image_port("out")]),
                }
            });
            if let Some(group) = &node.group {
                if group.graph.nodes.iter().any(|n| n.type_id.as_str() == lt::GROUP_FRAME) {
                    inputs.push(image_port("frame"));
                }
                for p in &group.interface {
                    let ports = if p.direction == PortDirection::Input { &mut inputs } else { &mut outputs };
                    if let Some(existing) = ports.iter_mut().find(|s| s.key == p.key.as_str()) {
                        existing.ty = p.ty;
                    } else {
                        ports.push(PortSpec { key: p.key.to_string(), ty: p.ty, optional: false, params: Vec::new() });
                    }
                }
            }
            // Unknown plug-ins and migrated odd ports remain visible. Never
            // drop an edge just because no current descriptor names its port.
            for edge in &graph.edges {
                for (endpoint, ports) in [(&edge.from, &mut outputs), (&edge.to, &mut inputs)] {
                    if endpoint.0 == node.id && !ports.iter().any(|p| p.key == endpoint.1.as_str()) {
                        ports.push(image_port(endpoint.1.as_str()));
                    }
                }
            }
            if inputs.len() > u8::MAX as usize || outputs.len() > u8::MAX as usize {
                return Err(format!("{} has more ports than this canvas can display", node.type_id));
            }
            let label = if node.label.is_empty() {
                desc.map(|d| d.name).unwrap_or_else(|| structural_name(node)).to_owned()
            } else { node.label.clone() };
            let input_links = inputs.iter().map(|p| node.wireless.input_links.get(&PortKey::from(p.key.as_str())).cloned()).collect();
            nodes.push(PictureNode { node, label, desc, native, inputs, outputs, input_links });
        }
        let port = |id, key: &PortKey, output| -> Option<u8> {
            let n = nodes.iter().find(|n| n.node.id == id)?;
            (if output { &n.outputs } else { &n.inputs }).iter().position(|p| p.key == key.as_str()).map(|p| p as u8)
        };
        let edges = graph.edges.iter().map(|e| {
            Ok(Edge { from_node: e.from.0, from_port: port(e.from.0, &e.from.1, true).ok_or("A wire's output node is missing")?,
                to_node: e.to.0, to_port: port(e.to.0, &e.to.1, false).ok_or("A wire's input node is missing")? })
        }).collect::<Result<Vec<_>, String>>()?;
        let defaults = layout(graph);
        let positions = graph.nodes.iter().map(|n| {
            let p = graph.ui.positions.get(&n.id).and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(defaults[&n.id]);
            (n.id, p)
        }).collect();
        let after = selected.filter(|id| nodes.iter().any(|n| n.node.id == *id && !n.kind().is_output()))
            .or_else(|| add_after(graph));
        Ok(Self { graph, level, nodes, edges, positions,
            frames: graph.ui.frames.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect(),
            notes: graph.ui.sticky_notes.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect(), after, keyboard_enabled: true })
    }

    fn path(&self, node: NodeId) -> NodePath {
        NodePath::new(self.level.root, self.level.nodes.iter().copied().chain([node]))
    }

    fn port(&self, node: NodeId, index: u8, output: bool) -> Option<PortKey> {
        let n = self.nodes.iter().find(|n| n.node.id == node)?;
        (if output { &n.outputs } else { &n.inputs }).get(index as usize).map(|p| PortKey::from(p.key.as_str()))
    }

    fn output(&self, node: NodeId) -> Option<NodeOutput> {
        Some(NodeOutput { path: self.path(node), port: self.port(node, 0, true)? })
    }
}

/// The last effect on the primary picture chain, otherwise its display
/// or grade stage. Storage order and side branches never choose the site.
fn add_after(graph: &Graph) -> Option<NodeId> {
    let mut at = graph.output;
    let mut seen = std::collections::HashSet::new();
    while seen.insert(at) {
        let node = graph.nodes.iter().find(|n| n.id == at)?;
        if node.origin.is_none() && matches!(node.type_id.as_str(), klipp_graph_format::types::LEGACY_EFFECT | klipp_graph_format::types::LEGACY_OFX | lt::SCENE_TO_DISPLAY | lt::GROUP | lt::SOURCE) {
            return Some(at);
        }
        let port = klipp_timeline::RegistryCatalog.primary_input(node)?;
        at = graph.edges.iter().find(|e| e.to == (at, port.clone()))?.from.0;
    }
    None
}

/// Stable layered placement for nodes with no saved editor position.
fn layout(graph: &Graph) -> HashMap<NodeId, NodePos> {
    let mut depths = HashMap::<NodeId, usize>::new();
    let mut remaining: Vec<_> = graph.nodes.iter().map(|n| n.id).collect();
    while !remaining.is_empty() {
        let ready: Vec<_> = remaining.iter().copied().filter(|id| graph.edges.iter().filter(|e| e.to.0 == *id).all(|e| depths.contains_key(&e.from.0))).collect();
        if ready.is_empty() { break; }
        for id in ready {
            let depth = graph.edges.iter().filter(|e| e.to.0 == id).filter_map(|e| depths.get(&e.from.0).map(|d| d + 1)).max().unwrap_or(0);
            depths.insert(id, depth);
            remaining.retain(|n| *n != id);
        }
    }
    let mut rows = HashMap::<usize, usize>::new();
    graph.nodes.iter().enumerate().map(|(i, n)| {
        let depth = depths.get(&n.id).copied().unwrap_or(i);
        let row = rows.entry(depth).or_default();
        let p = NodePos { x: depth as f32 * 200.0, y: *row as f32 * 110.0 };
        *row += 1;
        (n.id, p)
    }).collect()
}

impl CanvasNode for PictureNode<'_> {
    type Kind<'a> = &'a Self where Self: 'a;
    fn id(&self) -> NodeId { self.node.id }
    fn kind(&self) -> Self::Kind<'_> { self }
    fn label(&self) -> &str { &self.label }
    fn enabled(&self) -> bool { self.node.enabled }
    fn effective_enabled(&self, time: f64) -> bool { self.native.as_ref().map_or(self.node.enabled, |n| klipp_nodes::effective_enabled(n, time)) }
    fn input_count(&self) -> u8 { self.inputs.len() as u8 }
    fn modified(&self) -> bool { self.native.as_ref().map_or(!self.node.params.is_empty(), |n| !n.params.is_default()) }
    fn animated(&self) -> bool { !self.node.anim.is_empty() || self.native.as_ref().is_some_and(|n| !n.keyframes.is_empty()) }
    fn child_count(&self) -> usize { self.node.group.as_ref().map_or_else(|| self.native.as_ref().map_or(0, CanvasNode::child_count), |g| g.graph.nodes.len()) }
    fn matte_image(&self) -> Option<&std::path::Path> { self.native.as_ref().and_then(CanvasNode::matte_image) }
    fn output_link(&self) -> Option<&str> { self.node.wireless.output_link.as_deref() }
    fn input_links(&self) -> &[Option<String>] { &self.input_links }
}

impl CanvasKind for &PictureNode<'_> {
    fn is_source(self) -> bool { matches!(self.node.type_id.as_str(), lt::SOURCE | lt::GROUP_INPUT | lt::GROUP_FRAME) }
    fn is_output(self) -> bool { matches!(self.node.type_id.as_str(), lt::OUTPUT | lt::GROUP_OUTPUT) }
    fn is_reroute(self) -> bool { self.native.as_ref().is_some_and(|n| n.kind == NodeKind::Reroute) }
    fn is_group(self) -> bool { self.node.group.is_some() }
    fn is_external_matte(self) -> bool { self.native.as_ref().is_some_and(|n| n.kind == NodeKind::ExternalMatte) }
    fn is_variadic(self) -> bool { false }
    fn palette(self) -> Palette {
        if self.is_source() { Palette::Source } else if self.is_output() { Palette::Output }
        else if self.is_group() { Palette::Structural }
        else { self.native.as_ref().map_or_else(|| self.desc.map_or(Palette::Structural, |d| category_palette(d.category)), |n| n.kind.palette()) }
    }
    fn glyph(self) -> &'static str {
        if self.is_source() { "▶" } else if self.is_output() { "⏏" } else if self.is_group() { "⊞" }
        else { self.native.as_ref().map_or("F", |n| n.kind.glyph()) }
    }
    fn display_name(&self) -> &str { self.desc.map(|d| d.name).unwrap_or_else(|| structural_name(self.node)) }
    fn inputs(self) -> &'static [PortDesc] { self.desc.map_or(&[], |d| d.inputs) }
    fn outputs(self) -> &'static [PortDesc] { self.desc.map_or(&[], |d| d.outputs) }
    fn output_port_count(&self) -> u8 { self.outputs.len() as u8 }
    fn input_port_role(&self, port: u8) -> PortRole { role(self.inputs.get(port as usize)) }
    fn output_port_role(&self, port: u8) -> PortRole { role(self.outputs.get(port as usize)) }
    fn input_port_label(&self, port: u8) -> &str { self.inputs.get(port as usize).map_or("", |p| p.key.as_str()) }
    fn output_port_label(&self, port: u8) -> &str { self.outputs.get(port as usize).map_or("", |p| p.key.as_str()) }
}

fn role(port: Option<&PortSpec>) -> PortRole {
    if port.is_some_and(|p| p.ty == PortType::Matte) { PortRole::Matte } else { PortRole::Primary }
}

impl<'a> CanvasGraph for PictureCanvas<'a> {
    type Node = PictureNode<'a>;
    fn nodes(&self) -> &[Self::Node] { &self.nodes }
    fn edges(&self) -> &[Edge] { &self.edges }
    fn positions(&self) -> &HashMap<NodeId, NodePos> { &self.positions }
    fn frames(&self) -> &[NodeFrame] { &self.frames }
    fn sticky_notes(&self) -> &[NodeStickyNote] { &self.notes }
    fn signal_role(&self, node: NodeId, port: u8) -> PortRole { self.nodes.iter().find(|n| n.node.id == node).map_or(PortRole::Primary, |n| n.kind().output_port_role(port)) }
    fn add_menu(&self) -> &[MenuCategory] { picture_menu() }
    fn grade_tools(&self) -> bool { false }
    fn keyboard_enabled(&self) -> bool { self.keyboard_enabled }
    fn thumbnail_output(&self, _: klipp_core::ClipId, node: NodeId, _: &[NodeId]) -> Option<NodeOutput> { self.output(node) }
    fn command(&self, command: AppCommand) -> Option<AppCommand> {
        let single = |op, label: &str| Some(AppCommand::GraphOps { ops: vec![op], label: label.into() });
        match command {
            AppCommand::MoveNode { node_id, delta_x, delta_y, .. } => {
                let p = self.positions.get(&node_id)?;
                Some(AppCommand::SetNodePositionAt { addr: self.path(node_id), position: (p.x + delta_x, p.y + delta_y) })
            }
            AppCommand::RenameNode { node_id, label, .. } => single(GraphOp::SetNodeMeta { path: self.path(node_id), label: Some(label), position: None }, "Rename node"),
            AppCommand::RemoveNode { node_id, .. } => Some(AppCommand::RemoveNodeAt { addr: self.path(node_id) }),
            AppCommand::ToggleNodeEnabled { node_id, .. } => Some(AppCommand::SetNodeEnabledAt { addr: self.path(node_id), enabled: !self.graph.nodes.iter().find(|n| n.id == node_id)?.enabled }),
            AppCommand::ConnectNodes { from, from_port, to, to_port, .. } => single(GraphOp::Connect { from: self.path(from), from_port: self.port(from, from_port, true)?, to: self.path(to), to_port: self.port(to, to_port, false)? }, "Connect nodes"),
            AppCommand::DisconnectEdge { to, to_port, .. } => single(GraphOp::Disconnect { to: self.path(to), port: self.port(to, to_port, false)? }, "Disconnect nodes"),
            AppCommand::DescendIntoCompound { node_id, .. } => {
                let body = self.graph.nodes.iter().find(|n| n.id == node_id)?.group.as_ref()?;
                let mut path = self.path(node_id);
                path.nodes.push(body.graph.output);
                Some(AppCommand::RevealGraph { path, select: false })
            },
            AppCommand::AutoLayoutNodeGraph { .. } => Some(AppCommand::GraphOps { ops: layout(self.graph).into_iter().map(|(id, p)| GraphOp::SetNodeMeta { path: self.path(id), label: None, position: Some(serde_json::json!({"x":p.x,"y":p.y})) }).collect(), label: "Arrange picture graph".into() }),
            AppCommand::AddNode { kind, node_id, position, .. } => {
                let mut node = crate::inspector::stack_view::new_effect_node(&kind)?;
                if let Some(id) = node_id { node.id = id; }
                let path = self.path(node.id);
                Some(AppCommand::GraphOps { ops: vec![GraphOp::AddNode { after: self.path(self.after?), node: Box::new(node) },
                    GraphOp::SetNodeMeta { path, label: None, position: Some(serde_json::json!({"x":position.0,"y":position.1})) }], label: "Add effect".into() })
            }
            AppCommand::SetNodeSolo { at } => Some(AppCommand::SetNodeSolo { at: match at { Some(at) => Some(self.output(at.path.node()?)?), None => None } }),
            AppCommand::SetVisibleNodeThumbs { request, clip_id, at } => Some(AppCommand::SetVisibleNodeThumbs { request, clip_id, at: at.iter().filter_map(|at| self.output(at.path.node()?)).collect() }),
            // Grade-only clipboard, versions and annotations have no picture
            // writer yet. The workspace integration hides those actions.
            _ => None,
        }
    }
}

fn picture_menu() -> &'static [MenuCategory] {
    static MENU: std::sync::OnceLock<Vec<MenuCategory>> = std::sync::OnceLock::new();
    MENU.get_or_init(|| {
        [Category::Colour, Category::BlurDetail, Category::LightGlow, Category::Geometry, Category::FilmTemporal, Category::Key, Category::PlugIn].into_iter().map(|category| {
            let label = match category { Category::Colour => "Colour", Category::BlurDetail => "Blur and detail", Category::LightGlow => "Light", Category::Geometry => "Geometry", Category::FilmTemporal => "Film and time", Category::Key => "Keying", Category::PlugIn => "Plug-ins", Category::Grade => "Grade" };
            let mut entries: Vec<_> = klipp_effects::registry::effect_types().filter(|t| t.desc.category == category && !crate::inspector::stack_view::is_placed_kind(t.tag)).map(|t| (t.desc.name, t.tag)).collect();
            entries.sort_by_key(|e| e.0);
            (label, entries)
        }).filter(|(_, entries)| !entries.is_empty()).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_graph_format::path::Root;
    use klipp_timeline::Clip;
    use klipp_timeline::test_clip::TestClip;

    fn clip() -> Clip {
        let t = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
        TestClip::of(Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0))).effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 }).build()
    }

    fn root(clip: &Clip) -> NodePath { NodePath::new(Root::Clip { clip: clip.id.0 }, []) }

    fn check_level(graph: &Graph, level: NodePath) -> usize {
        let model = PictureCanvas::new(graph, level.clone(), None).unwrap();
        let mut held = graph;
        for id in &level.nodes { held = &held.nodes.iter().find(|n| n.id == *id).unwrap().group.as_ref().unwrap().graph; }
        assert_eq!(model.nodes.len(), held.nodes.len());
        for (drawn, stored) in model.nodes.iter().zip(&held.nodes) {
            assert_eq!(drawn.id(), stored.id);
            assert_eq!(drawn.node.type_id, stored.type_id);
            assert_eq!(drawn.enabled(), stored.enabled);
        }
        assert_eq!(model.edges.len(), held.edges.len());
        for edge in &model.edges {
            let from = model.port(edge.from_node, edge.from_port, true).unwrap();
            let to = model.port(edge.to_node, edge.to_port, false).unwrap();
            assert!(held.edges.iter().any(|e| e.from == (edge.from_node, from.clone()) && e.to == (edge.to_node, to.clone())));
        }
        let mut count = model.nodes.len();
        for n in &held.nodes {
            if n.group.is_some() {
                let mut child = level.clone(); child.nodes.push(n.id);
                count += check_level(graph, child);
            }
        }
        count
    }

    #[test]
    fn picture_nodes_and_named_ports_are_not_lost_in_the_canvas() {
        let c = clip();
        let p = c.picture_v2().unwrap();
        assert!(check_level(&p.picture.graph, root(&c)) > p.picture.graph.nodes.len(), "the grade body is checked too");
        let model = PictureCanvas::new(&p.picture.graph, root(&c), None).unwrap();
        for edge in &model.edges {
            let command = model.command(AppCommand::ConnectNodes { clip_id: c.id, from: edge.from_node, from_port: edge.from_port, to: edge.to_node, to_port: edge.to_port }).unwrap();
            let AppCommand::GraphOps { ops, .. } = command else { panic!("path connection") };
            let [GraphOp::Connect { from, from_port, to, to_port }] = ops.as_slice() else { panic!("one wire") };
            assert_eq!(from, &model.path(edge.from_node));
            assert_eq!(to, &model.path(edge.to_node));
            assert_eq!(Some(from_port.clone()), model.port(edge.from_node, edge.from_port, true));
            assert_eq!(Some(to_port.clone()), model.port(edge.to_node, edge.to_port, false));
        }
    }

    #[test]
    fn group_descent_and_inspection_keep_the_full_path() {
        let c = clip();
        let p = c.picture_v2().unwrap();
        let model = PictureCanvas::new(&p.picture.graph, root(&c), None).unwrap();
        let group = p.picture.graph.nodes.iter().find(|n| n.group.is_some()).unwrap();
        let command = model.command(AppCommand::DescendIntoCompound { clip_id: c.id, node_id: group.id }).unwrap();
        let AppCommand::RevealGraph { path, select: false } = command else { panic!("navigation by path") };
        assert_eq!(path.nodes, [group.id, group.group.as_ref().unwrap().graph.output]);
        let inner = PictureCanvas::new(&p.picture.graph, NodePath::new(root(&c).root, [group.id]), None).unwrap();
        let node = inner.nodes.iter().find(|n| !n.outputs.is_empty()).unwrap();
        assert_eq!(inner.output(node.id()).unwrap().path.nodes, [group.id, node.id()]);
    }

    #[test]
    fn a_lifted_merge_keeps_its_named_sockets() {
        let mut c = clip();
        let id = NodeId::new_v4();
        let node = klipp_nodes::Node { id, kind: NodeKind::Blend, label: String::new(), enabled: true,
            params: Default::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() };
        assert!(klipp_timeline::GradeAccess::insert_grade_node(&mut c, None, node));
        let p = c.picture_v2().unwrap();
        let group = p.picture.graph.nodes.iter().find(|n| n.group.is_some()).unwrap();
        let model = PictureCanvas::new(&p.picture.graph, NodePath::new(root(&c).root, [group.id]), None).unwrap();
        let n = model.nodes.iter().find(|n| n.id() == id).unwrap();
        assert_eq!(n.node.type_id.as_str(), lt::MERGE);
        assert_eq!(n.inputs.iter().map(|p| p.key.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(n.outputs.iter().map(|p| p.key.as_str()).collect::<Vec<_>>(), ["out"]);
    }

    #[test]
    fn unknown_legacy_nodes_and_their_ports_still_draw() {
        let c = clip();
        let mut graph = c.picture_v2().unwrap().picture.graph.clone();
        let n = graph.nodes.iter_mut().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("gaussian_blur")).unwrap();
        n.type_id = "ofx:an-unavailable-plugin".into();
        n.legacy = None;
        let id = n.id;
        let edge = graph.edges.iter_mut().find(|e| e.from.0 == id).unwrap();
        edge.from.1 = "custom_output".into();
        let model = PictureCanvas::new(&graph, root(&c), None).unwrap();
        assert_eq!(model.nodes.len(), graph.nodes.len());
        assert_eq!(model.edges.len(), graph.edges.len());
        let n = model.nodes.iter().find(|n| n.id() == id).unwrap();
        assert!(n.outputs.iter().any(|p| p.key == "custom_output"));
        assert_eq!(n.label(), "ofx:an-unavailable-plugin");
    }

    #[test]
    fn adding_an_effect_keeps_its_position_and_can_be_undone() {
        let mut c = clip();
        let p = c.picture_v2().unwrap();
        let before = (*p).clone();
        let model = PictureCanvas::new(&p.picture.graph, root(&c), None).unwrap();
        let command = model.command(AppCommand::AddNode { clip_id: c.id, kind: "sharpen".into(), node_id: None, position: (300.0, 120.0) }).unwrap();
        let AppCommand::GraphOps { ops, .. } = command else { panic!("one graph batch") };
        let edit = c.apply_picture_ops(&ops).unwrap();
        let changed = c.picture_v2().unwrap();
        let added = changed.picture.graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("sharpen")).unwrap();
        assert_eq!(changed.picture.graph.ui.positions[&added.id], serde_json::json!({"x":300.0,"y":120.0}));
        assert_eq!(changed.picture.graph.nodes.len(), p.picture.graph.nodes.len() + 1);
        c.restore_picture(&edit.before);
        assert_eq!(*c.picture_v2().unwrap(), before);
    }

    /// Use the real canvas's context menu, then the normal path handler
    /// and history: an effect's bypass changes the clip and undoes once.
    #[test]
    fn editing_an_effect_in_the_picture_root_changes_the_clip_and_undoes_in_one_step() {
        let c = clip();
        let p = c.picture_v2().unwrap();
        let model = PictureCanvas::new(&p.picture.graph, root(&c), None).unwrap();
        let effect = model.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n.node) == Some("gaussian_blur")).unwrap();
        let label = effect.label.clone();
        let addr = model.path(effect.id());
        let ctx = egui::Context::default();
        let mut panel = super::super::NodeGraphV2Panel::default();
        let mut level = root(&c);
        let (mut selected, mut set, mut time) = (None, Default::default(), 0.0);
        let mut frame = |events| {
            time += 0.05;
            let mut cmds = Vec::new();
            let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 700.0))), time: Some(time), events, ..Default::default() }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    cmds = panel.show_picture(ui, &c, &mut level, &mut selected, &mut set, &crate::theme::KlippTheme::default(), 0.0, true, &Default::default(), 0, &mut Vec::new()).unwrap();
                });
            });
            let texts: Vec<_> = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos + t.galley.size() / 2.0)), _ => None,
            }).collect();
            (cmds, texts)
        };
        let mut texts = Vec::new();
        for _ in 0..3 { texts = frame(Vec::new()).1; }
        assert!(!texts.iter().any(|(s, _)| s.contains("Note") || s.contains("Save version")), "grade-only tools aren't offered");
        let at = texts.iter().find(|(s, _)| s == &label).unwrap().1;
        let button = |at, button, pressed| egui::Event::PointerButton { pos: at, button, pressed, modifiers: egui::Modifiers::NONE };
        frame(vec![egui::Event::PointerMoved(at), button(at, egui::PointerButton::Secondary, true)]);
        frame(vec![button(at, egui::PointerButton::Secondary, false)]);
        for _ in 0..3 { texts = frame(Vec::new()).1; }
        let at = texts.iter().find(|(s, _)| s == "Bypass").unwrap().1;
        frame(vec![egui::Event::PointerMoved(at)]);
        frame(vec![button(at, egui::PointerButton::Primary, true)]);
        let (cmds, _) = frame(vec![button(at, egui::PointerButton::Primary, false)]);
        assert!(matches!(cmds.as_slice(), [AppCommand::SetNodeEnabledAt { addr: a, enabled: false }] if *a == addr), "{cmds:?}");

        let mut state = crate::state::AppState::default();
        let track = state.sequences[0].tracks.iter_mut().find(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        track.clips.push(c.clone());
        let before = (*p).clone();
        state.before_dispatch(&cmds[0]);
        assert_eq!(state.path_command(&cmds[0]), Some(true));
        assert_eq!(state.active_history().undo_count(), 1);
        let changed = state.sequences[0].all_clips().find(|n| n.id == c.id).unwrap().picture_v2().unwrap();
        assert!(!changed.picture.graph.nodes.iter().find(|n| n.id == addr.node().unwrap()).unwrap().enabled);
        let mut command = state.active_history_mut().undo().unwrap().command;
        while let crate::history::EditCommand::Named { edit, .. } = command { command = *edit; }
        let crate::history::EditCommand::PictureEdit { edit, .. } = command else { panic!("picture undo") };
        let restored = state.sequences[0].all_clips_mut().find(|n| n.id == c.id).unwrap();
        restored.restore_picture(&edit.before);
        assert_eq!(*restored.picture_v2().unwrap(), before);
    }

    /// Descending into a group by double-clicking it and climbing back by
    /// the "Clip" crumb is navigation alone: every command the canvas sends
    /// for it, applied as the app applies it, leaves the project clean, its
    /// history empty and the clip's picture as it was, while the level does
    /// move in and out.
    #[test]
    fn navigation_never_dirties_the_project() {
        let c = clip();
        let before = c.picture_v2().unwrap();
        let model = PictureCanvas::new(&before.picture.graph, root(&c), None).unwrap();
        let group = model.nodes.iter().find(|n| n.node.group.is_some()).expect("the clip's grade group");
        let (group_id, group_label) = (group.id(), group.label.clone());
        let mut state = crate::state::AppState::default();
        state.sequences[0].tracks.iter_mut().find(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap().clips.push(c.clone());
        state.selected_clip = Some(c.id);
        state.graph_dock_open = true;
        let ctx = egui::Context::default();
        let mut panel = super::super::NodeGraphV2Panel::default();
        let (mut selected, mut set, mut time) = (None, Default::default(), 0.0);
        let mut sent = Vec::new();
        // One frame as the graph dock draws it: the canvas at the level the
        // app keeps (`graph_dock_path`), and every command it sends applied
        // as the app applies it, the canvas's own move of the level kept.
        let mut frame = |events, state: &mut crate::state::AppState| {
            time += 0.1;
            let mut level = NodePath::new(root(&c).root, state.graph_dock_path.clone());
            let mut cmds = Vec::new();
            let output = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 700.0))), time: Some(time), events, ..Default::default() }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    cmds = panel.show_picture(ui, &c, &mut level, &mut selected, &mut set, &crate::theme::KlippTheme::default(), 0.0, true, &Default::default(), 0, &mut Vec::new()).unwrap();
                });
            });
            state.graph_dock_path = level.nodes.clone();
            for cmd in &cmds {
                state.before_dispatch(cmd);
                state.path_command(cmd);
            }
            sent.extend(cmds);
            output.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos + t.galley.size() / 2.0)), _ => None,
            }).collect::<Vec<_>>()
        };
        let button = |at, pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut texts = Vec::new();
        for _ in 0..3 { texts = frame(Vec::new(), &mut state); }
        // In: a double click on the group.
        let at = texts.iter().find(|(s, _)| *s == group_label).unwrap_or_else(|| panic!("the group is drawn: {texts:?}")).1;
        frame(vec![egui::Event::PointerMoved(at)], &mut state);
        for _ in 0..2 {
            frame(vec![button(at, true)], &mut state);
            frame(vec![button(at, false)], &mut state);
        }
        for _ in 0..3 { texts = frame(Vec::new(), &mut state); }
        assert_eq!(state.graph_dock_path, [group_id], "the double click descends into the group: sent {sent:?}");
        // Out: the "Clip" crumb.
        let at = texts.iter().find(|(s, _)| s == "Clip").unwrap_or_else(|| panic!("the Clip crumb is drawn: {texts:?}")).1;
        frame(vec![egui::Event::PointerMoved(at), button(at, true)], &mut state);
        frame(vec![button(at, false)], &mut state);
        for _ in 0..2 { frame(Vec::new(), &mut state); }
        assert!(state.graph_dock_path.is_empty(), "the crumb climbs back to the clip: {:?}", state.graph_dock_path);
        assert!(!state.dirty, "navigating dirtied the project: {sent:?}");
        assert_eq!(state.active_history().undo_count(), 0, "navigating left an undo step: {sent:?}");
        assert_eq!(state.sequence().all_clips().find(|x| x.id == c.id).unwrap().picture_v2().unwrap(), before);
    }

    #[test]
    fn opening_selecting_panning_and_zooming_the_picture_writes_nothing() {
        let c = clip();
        let before = c.picture_v2().unwrap();
        let model = PictureCanvas::new(&before.picture.graph, root(&c), None).unwrap();
        let effect = model.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n.node) == Some("gaussian_blur")).unwrap();
        let (id, label) = (effect.id(), effect.label.clone());
        let ctx = egui::Context::default();
        let mut panel = super::super::NodeGraphV2Panel::default();
        let mut level = root(&c);
        let (mut selected, mut set, mut time) = (None, Default::default(), 0.0);
        let mut frame = |events| {
            time += 0.1;
            let output = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 700.0))), time: Some(time), events, ..Default::default() }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let commands = panel.show_picture(ui, &c, &mut level, &mut selected, &mut set, &crate::theme::KlippTheme::default(), 0.0, true, &Default::default(), 0, &mut Vec::new()).unwrap();
                    assert!(commands.iter().all(|cmd| matches!(cmd, AppCommand::SetVisibleNodeThumbs { .. })), "{commands:?}");
                });
            });
            let texts: Vec<_> = output.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos + t.galley.size() / 2.0)), _ => None,
            }).collect();
            (texts, panel.pan, panel.zoom, selected)
        };
        let mut texts = Vec::new();
        for _ in 0..3 { texts = frame(Vec::new()).0; }
        let at = texts.iter().find(|(s, _)| s == &label).unwrap().1;
        let button = |at, button, pressed| egui::Event::PointerButton { pos: at, button, pressed, modifiers: egui::Modifiers::NONE };
        frame(vec![egui::Event::PointerMoved(at), button(at, egui::PointerButton::Primary, true)]);
        assert_eq!(frame(vec![button(at, egui::PointerButton::Primary, false)]).3, Some(id));
        let at = egui::pos2(40.0, 620.0);
        frame(vec![egui::Event::PointerMoved(at)]);
        let pan = frame(vec![button(at, egui::PointerButton::Middle, true)]).1;
        let moved = at + egui::vec2(60.0, 20.0);
        assert_ne!(frame(vec![egui::Event::PointerMoved(moved)]).1, pan);
        let zoom = frame(vec![button(moved, egui::PointerButton::Middle, false)]).2;
        assert_ne!(frame(vec![egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, 80.0), modifiers: egui::Modifiers::NONE }]).2, zoom);
        assert_eq!(c.picture_v2().unwrap(), before);
    }

    #[test]
    fn moving_and_renaming_picture_nodes_keeps_rendered_pixels() {
        struct Chart;
        impl klipp_compose::FrameSource for Chart {
            fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<klipp_compose::SourceFrame> {
                let pixels = (0..h).flat_map(|y| (0..w).flat_map(move |x| [x as f32 / w as f32, y as f32 / h as f32, if (x / 6 + y / 6) % 2 == 0 { 0.8 } else { 0.1 }, 1.0])).collect();
                Some(klipp_compose::SourceFrame { pixels: klipp_compose::SourcePixels::Linear(std::sync::Arc::new(pixels)), width: w, height: h, pts: None })
            }
        }
        let render = |clip: &Clip| {
            let mut seq = klipp_timeline::Sequence::new_default();
            seq.resolution = klipp_core::Resolution { width: 96, height: 54 };
            seq.tracks.iter_mut().find(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap().clips.push(clip.clone());
            let mut renderer = klipp_compose::Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            renderer.set_gpu_enabled(false);
            renderer.render(&seq, klipp_core::RationalTime::from_secs_f64(1.0, 25), 96, 54, &mut Chart, Default::default())
        };
        let c = clip();
        let id = c.id;
        let before = render(&c);
        assert!(before.chunks_exact(4).any(|p| p[0] > 0.1 && p[1] > 0.1));
        let p = c.picture_v2().unwrap();
        let model = PictureCanvas::new(&p.picture.graph, root(&c), None).unwrap();
        let node = model.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n.node) == Some("gaussian_blur")).unwrap().id();
        let mut control = c.clone();
        control.apply_picture_ops(&[GraphOp::SetEnabled { path: model.path(node), enabled: false }]).unwrap();
        assert!(render(&control) != before, "the source and effect must exercise the renderer");
        let mut state = crate::state::AppState::default();
        state.sequences[0].tracks.iter_mut().find(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap().clips.push(c);
        for command in [AppCommand::MoveNode { clip_id: id, node_id: node, delta_x: 100.0, delta_y: 50.0 }, AppCommand::RenameNode { clip_id: id, node_id: node, label: "Softness".into() }] {
            let command = model.command(command).unwrap();
            state.before_dispatch(&command);
            if let AppCommand::GraphOps { ops, label } = &command { state.apply_path_ops(ops, label).unwrap(); }
            else { assert_eq!(state.path_command(&command), Some(true), "{command:?}"); }
            let after = render(state.sequence().all_clips().find(|c| c.id == id).unwrap());
            assert_eq!(before.len(), after.len());
            let error = before.iter().zip(&after).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
            assert!(error <= 1e-6, "metadata changed a rendered channel by {error}");
        }
    }

    #[test]
    fn delete_only_edits_the_picture_when_its_canvas_has_focus() {
        for focused in [false, true] {
            let c = clip();
            let p = c.picture_v2().unwrap();
            let effect = p.picture.graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("gaussian_blur")).unwrap().id;
            let ctx = egui::Context::default();
            let mut panel = super::super::NodeGraphV2Panel::default();
            let mut level = root(&c);
            let mut selected = Some(effect);
            let mut set = [effect].into_iter().collect();
            let mut cmds = Vec::new();
            let _ = ctx.run(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 700.0))),
                events: vec![egui::Event::Key { key: egui::Key::Delete, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }],
                ..Default::default()
            }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    cmds = panel.show_picture(ui, &c, &mut level, &mut selected, &mut set, &crate::theme::KlippTheme::default(), 0.0, focused, &Default::default(), 0, &mut Vec::new()).unwrap();
                });
            });
            cmds.retain(|cmd| !matches!(cmd, AppCommand::SetVisibleNodeThumbs { .. }));
            if focused { assert!(matches!(cmds.as_slice(), [AppCommand::RemoveNodeAt { addr }] if addr.node() == Some(effect)), "{cmds:?}"); }
            else { assert!(cmds.is_empty(), "{cmds:?}"); }
            assert_eq!(c.picture_v2().unwrap(), p);
        }
    }

    #[test]
    fn a_picture_canvas_pointer_drag_is_one_cancellable_undo_step() {
        for cancel in [false, true] {
            let c = clip();
            let id = c.id;
            let before = c.picture_v2().unwrap();
            let effect = before.picture.graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("gaussian_blur")).unwrap().id;
            let label = PictureCanvas::new(&before.picture.graph, root(&c), None).unwrap().nodes.into_iter().find(|n| n.id() == effect).unwrap().label;
            let mut state = crate::state::AppState::default();
            state.sequences[0].tracks.iter_mut().find(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap().clips.push(c.clone());
            let ctx = egui::Context::default();
            let mut panel = super::super::NodeGraphV2Panel::default();
            let mut level = root(&c);
            let (mut selected, mut set, mut time) = (None, Default::default(), 0.0);
            let mut frame = |state: &mut crate::state::AppState, events| {
                time += 0.05;
                let clip = state.sequence().all_clips().find(|c| c.id == id).unwrap();
                let mut cmds = Vec::new();
                let output = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 700.0))), time: Some(time), events, ..Default::default() }, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        cmds = panel.show_picture(ui, clip, &mut level, &mut selected, &mut set, &crate::theme::KlippTheme::default(), 0.0, true, &Default::default(), 0, &mut Vec::new()).unwrap();
                    });
                });
                cmds.retain(|cmd| !matches!(cmd, AppCommand::SetVisibleNodeThumbs { .. }));
                for cmd in &cmds {
                    state.before_dispatch(cmd);
                    assert!(state.path_command(cmd).is_some(), "{cmd:?}");
                }
                let texts: Vec<_> = output.shapes.iter().filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos + t.galley.size() / 2.0)), _ => None,
                }).collect();
                (cmds, texts)
            };
            let mut texts = Vec::new();
            for _ in 0..3 { texts = frame(&mut state, Vec::new()).1; }
            let at = texts.iter().find(|(s, _)| s == &label).unwrap().1 + egui::vec2(0.0, 18.0);
            let button = |at, pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            let mut sent = frame(&mut state, vec![egui::Event::PointerMoved(at), button(at, true)]).0;
            for distance in [30.0, 60.0, 90.0] {
                sent.extend(frame(&mut state, vec![egui::Event::PointerMoved(at + egui::vec2(distance, 20.0))]).0);
            }
            assert_ne!(state.sequence().all_clips().find(|c| c.id == id).unwrap().picture_v2().unwrap(), before);
            if cancel {
                sent.extend(frame(&mut state, vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]).0);
            }
            sent.extend(frame(&mut state, vec![button(at + egui::vec2(90.0, 20.0), false)]).0);
            assert_eq!(sent.iter().filter(|c| matches!(c, AppCommand::BeginNodeLayoutEdit { .. })).count(), 1, "{sent:?}");
            assert_eq!(state.active_history().undo_count(), usize::from(!cancel));
            if !cancel {
                assert_eq!(sent.iter().filter(|c| matches!(c, AppCommand::EndNodeLayoutEdit { .. })).count(), 1);
                let mut command = state.active_history_mut().undo().unwrap().command;
                while let crate::history::EditCommand::Named { edit, .. } = command { command = *edit; }
                let crate::history::EditCommand::PictureEdit { edit, .. } = command else { panic!("picture undo") };
                state.sequences[0].all_clips_mut().find(|c| c.id == id).unwrap().restore_picture(&edit.before);
            }
            assert_eq!(state.sequence().all_clips().find(|c| c.id == id).unwrap().picture_v2().unwrap(), before);
        }
    }

    /// Local validation corpus, never checked into the repository.
    #[test]
    #[ignore = "requires KLIPP_CANVAS_PROJECT_LIST naming the 18-project corpus"]
    fn the_picture_canvas_lists_every_node_of_the_18() {
        let list = std::env::var("KLIPP_CANVAS_PROJECT_LIST").expect("project-list path");
        let paths = std::fs::read_to_string(list).unwrap();
        let paths: Vec<_> = paths.lines().map(str::trim).filter(|s| !s.is_empty() && !s.starts_with('#')).collect();
        assert_eq!(paths.len(), 18);
        let mut clips = 0;
        let mut nodes = 0;
        for path in paths {
            let (sequences, ..) = klipp_project::load_project(std::path::Path::new(path)).unwrap();
            for clip in sequences.iter().flat_map(|s| s.all_clips()) {
                let picture = clip.picture_v2().unwrap();
                nodes += check_level(&picture.picture.graph, root(clip));
                clips += 1;
            }
        }
        assert!(clips > 18 && nodes > clips);
        eprintln!("18 projects: {clips} clips, {nodes} canvas nodes, all named edges retained");
    }
}
