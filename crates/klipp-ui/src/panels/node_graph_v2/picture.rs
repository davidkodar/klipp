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
    /// The ports in words (#1747), as the canvas names them.
    input_words: Vec<String>,
    output_words: Vec<String>,
    /// The node's kind in words when no description names it.
    kind_words: String,
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
    /// The node picked on the canvas: an effect a matte added now masks.
    selected: Option<NodeId>,
    pub(super) keyboard_enabled: bool,
    /// The add menu grouped for the Composite page (#2758).
    pub(super) composite_menu: bool,
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

/// A node of a grade, inside its group: anything but the group's own
/// input, output and frame.
fn is_grade_node(n: &Node) -> bool {
    !matches!(n.type_id.as_str(), lt::GROUP_INPUT | lt::GROUP_OUTPUT | lt::GROUP_FRAME)
}

/// Whether `node` is on as the menu and canvas show it: a clip's grade
/// group by the nodes inside it, which are what its Bypass switches
/// (#2553); any other node by its own switch.
fn shown_on(node: &Node) -> bool {
    match node.group.as_ref().filter(|_| node.type_id.as_str() == lt::GROUP) {
        Some(body) if body.graph.nodes.iter().any(is_grade_node) => body.graph.nodes.iter().any(|n| is_grade_node(n) && n.enabled),
        _ => node.enabled,
    }
}

fn description(node: &Node, native: Option<&klipp_nodes::Node>) -> Option<&'static NodeTypeDesc> {
    if let Some(n) = native {
        return klipp_nodes::registry::node_type(n.kind).map(|t| &t.desc);
    }
    klipp_graph_lift::effect_params::tag(node)
        .and_then(klipp_effects::registry::effect_type).map(|t| &t.desc)
        .or_else(|| klipp_effects::registry::effect_types().map(|t| &t.desc).find(|d| d.id == node.type_id.as_str()))
}

fn structural_name(node: &Node) -> String {
    match node.type_id.as_str() {
        lt::SOURCE => "Source", lt::OUTPUT => "Output", lt::GROUP => "Grade",
        lt::GROUP_INPUT => "Input", lt::GROUP_OUTPUT => "Output", lt::GROUP_FRAME => "Frame",
        lt::PLACEMENT => "Placement", lt::SCENE_TO_DISPLAY => "Scene to Display",
        lt::TEMPORAL_DENOISE => "Temporal Denoise", lt::FRAME_BLEND => "Frame Blend",
        // A plug-in by its own name ("com.example.Glow" is Glow), not
        // the kind of entry it is (#1747).
        klipp_graph_format::types::LEGACY_OFX => {
            let name = node.legacy.as_ref().and_then(|l| l.payload.get("plugin_name")).and_then(|v| v.as_str());
            return name.map_or_else(|| "Plug-in".to_owned(), readable_id);
        }
        other => return readable_id(other),
    }
    .to_owned()
}

/// A type id or key no description names, in words (#1747): its last
/// part ("klipp.fx.film_burn" → "Film burn"), separators as spaces,
/// the first letter capital.
fn readable_id(id: &str) -> String {
    let last = id.rsplit(['.', ':', '/']).find(|s| !s.is_empty()).unwrap_or(id);
    let spaced = last.replace(['_', '-'], " ");
    let mut chars = spaced.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => id.to_owned(),
    }
}

/// A port's key in words (#1747): "in" is Input, "mask" Mask, "fg"
/// Foreground; "r", "g" and "b" are colours on a node that splits or
/// combines them, letters elsewhere; numbered keys count from one.
fn port_words(key: &str, rgb: bool) -> String {
    let named = match key {
        "in" => "Input", "out" => "Output", "mask" => "Mask", "matte" => "Matte",
        "pregrade" => "Before the grade", "fg" => "Foreground", "bg" => "Background",
        "ref" => "Reference", "frame" => "Frame", "a" => "A",
        "r" if rgb => "Red", "g" if rgb => "Green", "b" if rgb => "Blue",
        "b" => "B",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_owned();
    }
    for (prefix, word) in [("in", "Input"), ("p", "Port")] {
        if let Some(n) = key.strip_prefix(prefix).and_then(|n| n.parse::<u32>().ok()) {
            return format!("{word} {}", n + 1);
        }
    }
    readable_id(key)
}

/// The name the canvas gives the node `at` names in `clip`'s picture:
/// what the viewer says it is viewing (#1763).
pub(crate) fn node_name(clip: &klipp_timeline::Clip, at: &NodePath) -> Option<String> {
    let picture = clip.picture_v2().ok()?;
    let (&node, level) = at.nodes.split_last()?;
    let model = PictureCanvas::new(&picture.picture.graph, NodePath::new(at.root, level.iter().copied()), None).ok()?;
    model.nodes.iter().find(|n| n.id() == node).map(|n| n.label.clone())
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
            // A reroute's pins take the colour of what reaches it (#1877).
            if let Some(ty) = klipp_graph_format::lifted::reroute_type(graph, node.id, &klipp_timeline::RegistryCatalog) {
                for p in inputs.iter_mut().chain(outputs.iter_mut()) {
                    p.ty = ty;
                }
            }
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
            let kind_words = desc.map_or_else(|| structural_name(node), |d| d.name.to_owned());
            let label = if node.label.is_empty() { kind_words.clone() } else { node.label.clone() };
            let input_links = inputs.iter().map(|p| node.wireless.input_links.get(&PortKey::from(p.key.as_str())).cloned()).collect();
            let rgb = native.as_ref().is_some_and(|n| matches!(n.kind, NodeKind::RGBSplitter | NodeKind::RGBCombiner));
            let input_words = inputs.iter().map(|p| port_words(&p.key, rgb)).collect();
            let output_words = outputs.iter().map(|p| port_words(&p.key, rgb)).collect();
            nodes.push(PictureNode { node, label, desc, native, inputs, outputs, input_links, input_words, output_words, kind_words });
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
            notes: graph.ui.sticky_notes.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect(), after, selected, keyboard_enabled: true, composite_menu: false })
    }

    fn path(&self, node: NodeId) -> NodePath {
        NodePath::new(self.level.root, self.level.nodes.iter().copied().chain([node]))
    }

    fn port(&self, node: NodeId, index: u8, output: bool) -> Option<PortKey> {
        let n = self.nodes.iter().find(|n| n.node.id == node)?;
        (if output { &n.outputs } else { &n.inputs }).get(index as usize).map(|p| PortKey::from(p.key.as_str()))
    }

    /// A node of this level the editor copies, duplicates or replaces
    /// (#1745): anything but the level's ends and a group.
    fn copyable(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.node.id == id).filter(|n| !n.kind().is_terminal() && !n.kind().is_group()).map(|n| n.node)
    }

    /// How many wires leave `node`'s output `port` (#1744).
    fn readers(&self, node: NodeId, port: u8) -> usize {
        self.edges.iter().filter(|e| e.from_node == node && e.from_port == port).count()
    }

    /// An effect's stored entry as a reset leaves it (#1745); `None` for
    /// a node that is not an effect.
    fn effect_reset(&self, id: NodeId) -> Option<serde_json::Value> {
        let node = self.copyable(id)?;
        let tag = klipp_graph_lift::effect_params::tag(node)?;
        crate::inspector::stack_view::new_effect_node(tag)?.legacy.map(|l| l.payload)
    }

    fn output(&self, node: NodeId) -> Option<NodeOutput> {
        Some(NodeOutput { path: self.path(node), port: self.port(node, 0, true)? })
    }

    /// The level's frames and notes, as `edit` leaves them, set whole as
    /// one step named `label` (#1743): the command names the level by its
    /// output node. `None` when `edit` changed nothing.
    fn annotate(&self, label: &'static str, edit: impl FnOnce(&mut Vec<NodeFrame>, &mut Vec<NodeStickyNote>)) -> Option<AppCommand> {
        let (mut frames, mut notes) = (self.frames.clone(), self.notes.clone());
        edit(&mut frames, &mut notes);
        if frames == self.frames && notes == self.notes {
            return None;
        }
        Some(AppCommand::SetAnnotationsAt {
            at: self.path(self.graph.output),
            frames: frames.iter().map(|f| serde_json::to_value(f).ok()).collect::<Option<_>>()?,
            sticky_notes: notes.iter().map(|n| serde_json::to_value(n).ok()).collect::<Option<_>>()?,
            label,
        })
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
    fn enabled(&self) -> bool { shown_on(self.node) }
    fn effective_enabled(&self, time: f64) -> bool { self.native.as_ref().map_or(shown_on(self.node), |n| klipp_nodes::effective_enabled(n, time)) }
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
    fn display_name(&self) -> &str { &self.kind_words }
    fn inputs(self) -> &'static [PortDesc] { self.desc.map_or(&[], |d| d.inputs) }
    fn outputs(self) -> &'static [PortDesc] { self.desc.map_or(&[], |d| d.outputs) }
    fn output_port_count(&self) -> u8 { self.outputs.len() as u8 }
    fn input_port_role(&self, port: u8) -> PortRole { role(self.inputs.get(port as usize)) }
    fn output_port_role(&self, port: u8) -> PortRole { role(self.outputs.get(port as usize)) }
    fn input_port_label(&self, port: u8) -> &str { self.input_words.get(port as usize).map_or("", String::as_str) }
    fn output_port_label(&self, port: u8) -> &str { self.output_words.get(port as usize).map_or("", String::as_str) }
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
    fn add_menu(&self) -> &[MenuCategory] { if self.composite_menu { composite_menu() } else { picture_menu() } }
    fn grade_tools(&self) -> bool { false }
    fn keyboard_enabled(&self) -> bool { self.keyboard_enabled }
    fn thumbnail_output(&self, _: klipp_core::ClipId, node: NodeId, _: &[NodeId]) -> Option<NodeOutput> { self.output(node) }
    fn refusal(&self, command: &AppCommand) -> Option<String> {
        let no_place = || if self.level.nodes.is_empty() {
            "This picture has no chain to add an effect to".to_owned()
        } else {
            "Effects go on the clip's picture chain, not inside this group: go up to Clip to add one".to_owned()
        };
        let matte = |kind: &str| crate::picture_mattes::matte(kind).is_some() || crate::picture_mattes::source(kind).is_some();
        match command {
            // A matte stands beside the chain at the clip's top, reading its
            // source (#1746).
            AppCommand::AddNode { kind, .. } if matte(kind) => {
                (!self.graph.nodes.iter().any(|n| n.type_id.as_str() == lt::SOURCE)).then(|| "A matte goes beside the clip's picture chain: go up to Clip to add one".to_owned())
            }
            AppCommand::InsertNodeOnWire { kind, .. } | AppCommand::AddNodeWired { kind, .. } | AppCommand::ReplaceNode { kind, .. } if matte(kind) => {
                Some("A matte or a source stands beside the picture's chain: add it from its menu, then wire it to an effect's mask or a merge's picture".to_owned())
            }
            AppCommand::AddNode { kind, .. } | AppCommand::ReplaceNode { kind, .. } if picture_node(kind).is_none() => {
                Some(format!("{kind} can't be added to a picture"))
            }
            AppCommand::AddNode { .. } | AppCommand::PasteNodes { .. } => self.after.is_none().then(no_place),
            AppCommand::DuplicateNodes { node_ids, .. } | AppCommand::CopyNodes { node_ids, .. } if !node_ids.iter().any(|id| self.copyable(*id).is_some()) => {
                Some("Only a picture's effects are copied here; a group is entered to copy what it holds".to_owned())
            }
            AppCommand::InsertNodeOnWire { kind, .. } | AppCommand::AddNodeWired { kind, .. } if picture_node(kind).is_none() => {
                Some(format!("{kind} can't go in a picture: it is a grade node"))
            }
            AppCommand::InsertNodeOnWire { from, from_port, .. } | AppCommand::SpliceNodeIntoWire { from, from_port, .. } if self.readers(*from, *from_port) != 1 => {
                Some("That wire's source feeds more than one node: a node goes in after it from its own menu".to_owned())
            }
            AppCommand::SpliceNodeIntoWire { node_id, .. } if self.copyable(*node_id).is_none() => {
                Some("Only an effect moves into a wire here".to_owned())
            }
            AppCommand::ResetNode { node_id, .. } if self.effect_reset(*node_id).is_none() => {
                Some("Reset works on an effect here; the grade's nodes reset in the grade".to_owned())
            }
            _ => None,
        }
    }
    fn command(&self, command: AppCommand) -> Option<AppCommand> {
        let single = |op, label: &str| Some(AppCommand::GraphOps { ops: vec![op], label: label.into() });
        match command {
            AppCommand::MoveNode { node_id, delta_x, delta_y, .. } => {
                let p = self.positions.get(&node_id)?;
                Some(AppCommand::SetNodePositionAt { addr: self.path(node_id), position: (p.x + delta_x, p.y + delta_y) })
            }
            AppCommand::RenameNode { node_id, label, .. } => single(GraphOp::SetNodeMeta { path: self.path(node_id), label: Some(label), position: None }, "Rename node"),
            AppCommand::RemoveNode { node_id, .. } => Some(AppCommand::RemoveNodeAt { addr: self.path(node_id) }),
            AppCommand::ToggleNodeEnabled { node_id, .. } => Some(AppCommand::SetNodeEnabledAt { addr: self.path(node_id), enabled: !shown_on(self.graph.nodes.iter().find(|n| n.id == node_id)?) }),
            AppCommand::ConnectNodes { from, from_port, to, to_port, .. } => single(GraphOp::Connect { from: self.path(from), from_port: self.port(from, from_port, true)?, to: self.path(to), to_port: self.port(to, to_port, false)? }, "Connect nodes"),
            AppCommand::DisconnectEdge { to, to_port, .. } => single(GraphOp::Disconnect { to: self.path(to), port: self.port(to, to_port, false)? }, "Disconnect nodes"),
            AppCommand::DescendIntoCompound { node_id, .. } => {
                let body = self.graph.nodes.iter().find(|n| n.id == node_id)?.group.as_ref()?;
                let mut path = self.path(node_id);
                path.nodes.push(body.graph.output);
                Some(AppCommand::RevealGraph { path, select: false })
            },
            AppCommand::AutoLayoutNodeGraph { .. } => Some(AppCommand::GraphOps { ops: layout(self.graph).into_iter().map(|(id, p)| GraphOp::SetNodeMeta { path: self.path(id), label: None, position: Some(serde_json::json!({"x":p.x,"y":p.y})) }).collect(), label: "Arrange picture graph".into() }),
            AppCommand::AddNode { kind, node_id, position, .. } if crate::picture_mattes::source(&kind).is_some() => {
                let s = crate::picture_mattes::source(&kind)?;
                let ops = crate::picture_mattes::source_ops(self.graph, &self.level, s, node_id.unwrap_or_else(NodeId::new_v4), position, self.selected)?;
                Some(AppCommand::GraphOps { ops, label: format!("Add {}", s.name) })
            }
            AppCommand::AddNode { kind, node_id, position, .. } if crate::picture_mattes::matte(&kind).is_some() => {
                let m = crate::picture_mattes::matte(&kind)?;
                let ops = crate::picture_mattes::add_ops(self.graph, &self.level, m, node_id.unwrap_or_else(NodeId::new_v4), position, self.selected)?;
                Some(AppCommand::GraphOps { ops, label: format!("Add {}", m.name) })
            }
            AppCommand::AddNode { kind, node_id, position, .. } => {
                let mut node = picture_node(&kind)?;
                if let Some(id) = node_id { node.id = id; }
                let path = self.path(node.id);
                Some(AppCommand::GraphOps { ops: vec![GraphOp::AddNode { after: self.path(self.after?), node: Box::new(node) },
                    GraphOp::SetNodeMeta { path, label: None, position: Some(serde_json::json!({"x":position.0,"y":position.1})) }], label: "Add effect".into() })
            }
            // Frames and notes (#1743): the level's lists, set whole.
            AppCommand::CreateNodeFrame { node_ids, label, color_rgba, .. } => self.annotate("Add frame", |frames, _| {
                let contained: std::collections::BTreeSet<NodeId> = node_ids.into_iter().filter(|id| self.nodes.iter().any(|n| n.node.id == *id)).collect();
                if !contained.is_empty() {
                    frames.push(NodeFrame { id: klipp_nodes::NodeFrameId::new_v4(), label, description: String::new(), color_rgba, contained_node_ids: contained });
                }
            }),
            AppCommand::RenameNodeFrame { frame_id, label, .. } => self.annotate("Rename frame", |frames, _| {
                if let Some(f) = frames.iter_mut().find(|f| f.id == frame_id) { f.label = label; }
            }),
            AppCommand::SetNodeFrameDescription { frame_id, description, .. } => self.annotate("Edit frame description", |frames, _| {
                if let Some(f) = frames.iter_mut().find(|f| f.id == frame_id) { f.description = description; }
            }),
            AppCommand::RecolorNodeFrame { frame_id, color_rgba, .. } => self.annotate("Recolour frame", |frames, _| {
                if let Some(f) = frames.iter_mut().find(|f| f.id == frame_id) { f.color_rgba = color_rgba; }
            }),
            AppCommand::DeleteNodeFrame { frame_id, .. } => self.annotate("Delete frame", |frames, _| frames.retain(|f| f.id != frame_id)),
            AppCommand::CreateStickyNote { x, y, .. } => self.annotate("Add note", |_, notes| {
                notes.push(NodeStickyNote { id: klipp_nodes::NodeStickyNoteId::new_v4(), pos: NodePos { x, y }, width: 200.0, text: String::new(), color_rgba: crate::app::STICKY_DEFAULT_COLOR_RGBA });
            }),
            AppCommand::SetStickyNoteText { sticky_id, text, .. } => self.annotate("Edit note", |_, notes| {
                if let Some(n) = notes.iter_mut().find(|n| n.id == sticky_id) { n.text = text; }
            }),
            AppCommand::MoveStickyNote { sticky_id, delta_x, delta_y, .. } => self.annotate(MOVE_NOTE, |_, notes| {
                if let Some(n) = notes.iter_mut().find(|n| n.id == sticky_id) { n.pos.x += delta_x; n.pos.y += delta_y; }
            }),
            AppCommand::DeleteStickyNote { sticky_id, .. } => self.annotate("Delete note", |_, notes| notes.retain(|n| n.id != sticky_id)),
            // The node editor's own edits (#1745), by path at this level.
            AppCommand::DuplicateNodes { node_ids, .. } => {
                let mut ops = Vec::new();
                for id in node_ids {
                    let Some(node) = self.copyable(id) else { continue };
                    let mut copy = node.clone();
                    copy.id = NodeId::new_v4();
                    let p = self.positions.get(&id).copied().unwrap_or_default();
                    ops.push(GraphOp::AddNode { after: self.path(id), node: Box::new(copy.clone()) });
                    ops.push(GraphOp::SetNodeMeta { path: self.path(copy.id), label: None, position: Some(serde_json::json!({"x": p.x + 40.0, "y": p.y + 40.0})) });
                }
                (!ops.is_empty()).then(|| AppCommand::GraphOps { ops, label: "Duplicate nodes".into() })
            }
            AppCommand::CopyNodes { node_ids, .. } => {
                let nodes: Vec<Node> = node_ids.iter().filter_map(|id| self.copyable(*id).cloned()).collect();
                (!nodes.is_empty()).then_some(AppCommand::CopyPictureNodes { nodes })
            }
            AppCommand::PasteNodes { position, .. } => Some(AppCommand::PastePictureNodes { after: self.path(self.after?), position }),
            AppCommand::ReplaceNode { node_id, kind, .. } => {
                self.copyable(node_id)?;
                let node = picture_node(&kind)?;
                let p = self.positions.get(&node_id).copied().unwrap_or_default();
                let path = self.path(node.id);
                // The new node after the old, then the old out: the new
                // one reads what the old one read, and is read as it was.
                Some(AppCommand::GraphOps { ops: vec![
                    GraphOp::AddNode { after: self.path(node_id), node: Box::new(node) },
                    GraphOp::SetNodeMeta { path, label: None, position: Some(serde_json::json!({"x": p.x, "y": p.y})) },
                    GraphOp::RemoveNode { path: self.path(node_id) },
                ], label: "Replace node".into() })
            }
            AppCommand::ResetNode { node_id, .. } => Some(AppCommand::SetPayloadAt { addr: self.path(node_id), payload: self.effect_reset(node_id)? }),
            AppCommand::ShowPanel { .. } => Some(command),
            // Wiring (#1744): a node into a wire goes after the wire's
            // source, which this wire alone reads; a wire's end moves by
            // path, both halves one step.
            AppCommand::InsertNodeOnWire { kind, position, from, .. } | AppCommand::AddNodeWired { kind, position, from, .. } => {
                let node = picture_node(&kind)?;
                let path = self.path(node.id);
                Some(AppCommand::GraphOps { ops: vec![
                    GraphOp::AddNode { after: self.path(from), node: Box::new(node) },
                    GraphOp::SetNodeMeta { path, label: None, position: Some(serde_json::json!({"x": position.0, "y": position.1})) },
                ], label: "Insert node".into() })
            }
            AppCommand::SpliceNodeIntoWire { node_id, from, from_port, to, .. } => {
                (node_id != from && node_id != to && self.readers(from, from_port) == 1).then_some(())?;
                self.copyable(node_id)?;
                Some(AppCommand::MoveNodeAt { addr: self.path(node_id), after: from })
            }
            AppCommand::ReconnectEdge { to, to_port, new_to, new_to_port, .. } => {
                let source = self.edges.iter().find(|e| e.to_node == to && e.to_port == to_port)?;
                Some(AppCommand::GraphOps { ops: vec![
                    GraphOp::Disconnect { to: self.path(to), port: self.port(to, to_port, false)? },
                    GraphOp::Connect { from: self.path(source.from_node), from_port: self.port(source.from_node, source.from_port, true)?, to: self.path(new_to), to_port: self.port(new_to, new_to_port, false)? },
                ], label: "Reconnect wire".into() })
            }
            AppCommand::SetNodeSolo { at } => Some(AppCommand::SetNodeSolo { at: match at { Some(at) => Some(self.output(at.path.node()?)?), None => None } }),
            AppCommand::SetVisibleNodeThumbs { request, clip_id, at } => Some(AppCommand::SetVisibleNodeThumbs { request, clip_id, at: at.iter().filter_map(|at| self.output(at.path.node()?)).collect() }),
            // Already by path (#1753; the A/B wipe, #2758).
            label @ AppCommand::SetNodeColourLabel { .. } => Some(label),
            compare @ AppCommand::SetSplitCompareNode { .. } => Some(compare),
            // Grade-only clipboard and versions have no picture writer
            // yet. The workspace integration hides those actions.
            _ => None,
        }
    }
}

/// The step a note dragged on the picture canvas makes: its moves go
/// into the canvas's layout gesture, one step for the drag (#1743).
pub(super) const MOVE_NOTE: &str = "Move note";

/// The menu tag of a reroute (#1746): a point to route a wire through,
/// which passes its picture on.
pub(super) const REROUTE: &str = "reroute";

/// The menu tag of a grade in the picture's chain (#1746).
pub(super) const GRADE: &str = "grade";

/// A new node of the picture for the menu tag `kind`: an effect, a
/// reroute (a grade's reroute, which the renderer passes through), or a
/// grade compound with one Corrector.
fn picture_node(kind: &str) -> Option<Node> {
    match kind {
        REROUTE => Some(crate::inspector::grade_view::new_grade_node(NodeKind::Reroute, NodeId::new_v4())),
        GRADE => klipp_graph_lift::grade_compound::new_compound(NodeId::new_v4()).ok(),
        _ => crate::inspector::stack_view::new_effect_node(kind),
    }
}

pub(super) fn picture_menu() -> &'static [MenuCategory] {
    static MENU: std::sync::OnceLock<Vec<MenuCategory>> = std::sync::OnceLock::new();
    MENU.get_or_init(|| {
        [Category::Colour, Category::BlurDetail, Category::LightGlow, Category::Geometry, Category::FilmTemporal, Category::Key, Category::Composite, Category::PlugIn].into_iter().map(|category| {
            let label = match category { Category::Colour => "Colour", Category::BlurDetail => "Blur and detail", Category::LightGlow => "Light", Category::Geometry => "Geometry", Category::FilmTemporal => "Film and time", Category::Key => "Keying", Category::Composite => "Composite", Category::PlugIn => "Plug-ins", Category::Grade => "Grade" };
            let mut entries: Vec<_> = klipp_effects::registry::effect_types().filter(|t| t.desc.category == category && !crate::inspector::stack_view::is_placed_kind(t.tag) && !klipp_effects::draws_a_side_picture(&(t.sample)())).map(|t| (t.desc.name, t.tag)).collect();
            entries.sort_by_key(|e| e.0);
            (label, entries)
        }).filter(|(_, entries)| !entries.is_empty())
        // Mattes beside the chain, for an effect's mask (#1746).
        .chain([("Mattes", crate::picture_mattes::MATTES.iter().map(|m| (m.name, m.tag)).collect())])
        // Pictures beside the chain, for a merge (#1746).
        .chain([("Sources", crate::picture_mattes::SOURCES.iter().map(|s| (s.name, s.tag)).collect())])
        // A point to route a wire through (#1746).
        .chain([("Wiring", vec![("Reroute", REROUTE)])])
        // A grade in the chain, after its display step (#1746).
        .chain([("Grade", vec![("Grade", GRADE)])])
        .collect()
    })
}

/// The add menu on the Composite page (#2758): the same nodes as
/// [`picture_menu`], grouped by what layering asks for first: what to lay
/// over and the sources it comes from, keys, mattes, moves, looks.
pub(super) fn composite_menu() -> &'static [MenuCategory] {
    static MENU: std::sync::OnceLock<Vec<MenuCategory>> = std::sync::OnceLock::new();
    MENU.get_or_init(|| {
        let of = |categories: &[Category], also: &[&'static str], but: &[&'static str]| -> Vec<(&'static str, &'static str)> {
            let mut entries: Vec<_> = klipp_effects::registry::effect_types()
                .filter(|t| (categories.contains(&t.desc.category) || also.contains(&t.tag)) && !but.contains(&t.tag))
                .filter(|t| !crate::inspector::stack_view::is_placed_kind(t.tag) && !klipp_effects::draws_a_side_picture(&(t.sample)()))
                .map(|t| (t.desc.name, t.tag))
                .collect();
            entries.sort_by_key(|e| e.0);
            entries
        };
        let keys = ["light_wrap"];
        let composite: Vec<_> = of(&[Category::Composite], &[], &[]).into_iter().chain(crate::picture_mattes::SOURCES.iter().map(|s| (s.name, s.tag))).collect();
        [
            ("Composite", composite),
            ("Keying", of(&[Category::Key], &keys, &[])),
            ("Mattes", crate::picture_mattes::MATTES.iter().map(|m| (m.name, m.tag)).collect()),
            ("Transform", of(&[Category::Geometry], &[], &[])),
            ("Stylise", of(&[Category::BlurDetail, Category::LightGlow, Category::FilmTemporal], &[], &keys)),
            ("Colour", of(&[Category::Colour], &[], &[])),
            ("Plug-ins", of(&[Category::PlugIn], &[], &[])),
            ("Wiring", vec![("Reroute", REROUTE)]),
            ("Grade", vec![("Grade", GRADE)]),
        ]
        .into_iter()
        .filter(|(_, entries)| !entries.is_empty())
        .collect()
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

    /// #1877: a reroute's pins are the colour of what reaches it: a
    /// matte's when a window feeds it, a picture's when a picture does.
    #[test]
    fn a_reroutes_pins_follow_what_reaches_it() {
        let mut c = clip();
        let picture = c.picture_v2().unwrap().picture.clone();
        let at = |id| NodePath::new(Root::Clip { clip: c.id.0 }, [id]);
        let source = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
        let blur = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_lift::names::effect_node_type("gaussian_blur")).unwrap().id;
        let legacy = |kind: &str, params: serde_json::Value| -> klipp_graph_format::Node {
            serde_json::from_value(serde_json::json!({ "id": NodeId::new_v4(), "type_id": "klipp.legacy.node", "label": "", "legacy": { "payload": { "kind": kind, "label": kind, "params": params } } })).unwrap()
        };
        let window = legacy("Mask", serde_json::json!({ "masks": { "masks": [{ "shape": { "Circle": { "center_x": 0.5, "center_y": 0.5, "radius": 0.3 } } }] } }));
        let (matte_route, picture_route) = (legacy("Reroute", serde_json::json!({})), legacy("Reroute", serde_json::json!({})));
        c.apply_picture_ops(&[
            GraphOp::AddNodeBeside { at: at(source), node: Box::new(window.clone()) },
            GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(window.id), to_port: "in".into() },
            GraphOp::AddNodeBeside { at: at(source), node: Box::new(matte_route.clone()) },
            GraphOp::Connect { from: at(window.id), from_port: "out".into(), to: at(matte_route.id), to_port: "in".into() },
            GraphOp::Connect { from: at(matte_route.id), from_port: "out".into(), to: at(blur), to_port: "mask".into() },
            GraphOp::AddNode { after: at(blur), node: Box::new(picture_route.clone()) },
        ])
        .unwrap_or_else(|e| panic!("{e}"));
        let picture = c.picture_v2().unwrap().picture.clone();
        let model = PictureCanvas::new(&picture.graph, root(&c), None).unwrap();
        let drawn = |id| model.nodes.iter().find(|n| n.id() == id).unwrap();
        let roles = |id| { let k = drawn(id).kind(); (k.input_port_role(0), k.output_port_role(0)) };
        assert_eq!(roles(matte_route.id), (PortRole::Matte, PortRole::Matte));
        assert_eq!(roles(picture_route.id), (PortRole::Primary, PortRole::Primary));
    }

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
        // In words, not as its type id and keys (#1747).
        assert_eq!(n.label(), "An unavailable plugin");
        let out = n.outputs.iter().position(|p| p.key == "custom_output").unwrap() as u8;
        assert_eq!(n.kind().output_port_label(out), "Custom output");
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

    /// "Grade" from the Add menu puts a grade compound with one Corrector
    /// in the chain after the last effect, one undoable step, the clip now
    /// a picture-truth clip; the canvas enters it, and its Corrector is
    /// there (#1746).
    #[test]
    fn a_grade_from_the_add_menu_goes_in_the_chain_and_opens() {
        let mut c = clip();
        let p = c.picture_v2().unwrap();
        let before = (*p).clone();
        let model = PictureCanvas::new(&p.picture.graph, root(&c), None).unwrap();
        assert!(picture_menu().iter().any(|(label, entries)| *label == "Grade" && entries.iter().any(|(_, tag)| *tag == GRADE)), "the Add menu's Grade heading");
        let command = model.command(AppCommand::AddNode { clip_id: c.id, kind: GRADE.into(), node_id: None, position: (420.0, 120.0) }).unwrap();
        let AppCommand::GraphOps { ops, .. } = command else { panic!("one graph batch") };
        let edit = c.apply_picture_ops(&ops).unwrap();
        assert!(c.is_picture_truth());
        let changed = c.picture_v2().unwrap();
        let blur = changed.picture.graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("gaussian_blur")).unwrap().id;
        let compound = changed.picture.graph.nodes.iter().find(|n| klipp_graph_lift::grade_compound::is_compound(n)).expect("a compound");
        assert!(changed.picture.graph.edges.iter().any(|e| e.from.0 == blur && e.to.0 == compound.id), "after the last effect");
        let model = PictureCanvas::new(&changed.picture.graph, root(&c), None).unwrap();
        let Some(AppCommand::RevealGraph { path, .. }) = model.command(AppCommand::DescendIntoCompound { clip_id: c.id, node_id: compound.id }) else { panic!("the canvas enters it") };
        let inside = PictureCanvas::new(&changed.picture.graph, NodePath::new(path.root, path.nodes[..path.nodes.len() - 1].iter().copied()), None).unwrap();
        assert!(inside.nodes.iter().any(|n| klipp_graph_lift::structure::kind_of(n.node) == "Corrector"), "its Corrector is inside");
        c.restore_picture(&edit.before);
        assert_eq!(*c.picture_v2().unwrap(), before);
    }

    /// #2758: the Composite page's add menu groups what layering asks for
    /// first (a merge and the sources to lay over in Composite, a light
    /// wrap with the keys, a corner pin with the moves). From it a merge
    /// goes on the chain, then, with it picked, a gradient from Composite
    /// goes beside the chain wired into the merge's picture, each one
    /// undoable step; drawn, the clip shows the gradient over it,
    /// measured against the picture before.
    #[test]
    fn the_composite_pages_menu_lays_a_source_over_the_clip() {
        let has = |heading: &str, tag: &str| composite_menu().iter().any(|(label, items)| *label == heading && items.iter().any(|(_, t)| *t == tag));
        assert!(has("Composite", "merge") && has("Composite", "source.gradient") && has("Composite", "source.media") && has("Composite", "source.below"));
        assert!(has("Keying", "chroma_key") && has("Keying", "light_wrap") && has("Transform", "corner_pin") && has("Mattes", "matte.window"));
        // The same nodes as the picture menu, regrouped.
        let tags = |menu: &[MenuCategory]| { let mut t: Vec<&str> = menu.iter().flat_map(|(_, e)| e.iter().map(|(_, t)| *t)).collect(); t.sort(); t };
        assert_eq!(tags(composite_menu()), tags(picture_menu()));

        let t = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
        let mut c = Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
        let draw = |c: &Clip| {
            struct Chart;
            impl klipp_compose::FrameSource for Chart {
                fn frame(&mut self, _c: &Clip, _s: f64, w: u32, h: u32) -> Option<klipp_compose::SourceFrame> {
                    let v = (0..w * h).flat_map(|i| { let x = (i % w) as f32 / w as f32; [0.1 + 0.7 * x, 0.3, 0.6 - 0.4 * x, 1.0] }).collect();
                    Some(klipp_compose::SourceFrame { pixels: klipp_compose::SourcePixels::Linear(std::sync::Arc::new(v)), width: w, height: h, pts: None })
                }
            }
            let mut seq = klipp_timeline::Sequence::new_default();
            seq.resolution = klipp_core::Resolution { width: 64, height: 36 };
            let mut track = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
            track.clips = vec![c.clone()];
            seq.tracks = vec![track];
            let mut r = klipp_compose::Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            r.set_gpu_enabled(false);
            klipp_effects::linear_f32_rgba_to_srgb_u8(&r.render_inspected(&seq, t(1.0), 64, 36, &mut Chart, Default::default(), Default::default()).0)
        };
        let before = draw(&c);
        let pick = |c: &mut Clip, kind: &str, selected: Option<NodeId>| {
            let graph = c.picture_v2().unwrap().picture.graph.clone();
            let mut model = PictureCanvas::new(&graph, root(c), selected).unwrap();
            model.composite_menu = true;
            assert!(model.add_menu().iter().any(|(_, items)| items.iter().any(|(_, t)| *t == kind)), "{kind} on the page's menu");
            let Some(AppCommand::GraphOps { ops, .. }) = model.command(AppCommand::AddNode { clip_id: c.id, kind: kind.into(), node_id: None, position: (420.0, 120.0) }) else { panic!("{kind}: one graph batch") };
            c.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{kind}: {e}"));
        };
        pick(&mut c, "merge", None);
        let graph = c.picture_v2().unwrap().picture.graph.clone();
        let merge = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("merge")).expect("a merge on the chain").id;
        pick(&mut c, "source.gradient", Some(merge));
        let graph = c.picture_v2().unwrap().picture.graph.clone();
        let fed = graph.edges.iter().find(|e| e.to == (merge, "fg".into())).expect("the merge's picture wired").from.0;
        assert_eq!(graph.nodes.iter().find(|n| n.id == fed).and_then(klipp_graph_lift::effect_params::tag), Some("generator"));
        let after = draw(&c);
        let changed = before.iter().zip(&after).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (a, b))| a.abs_diff(*b)).max().unwrap_or(0);
        assert!(changed > 20, "the picture changed by {changed} levels");
    }

    /// A matte from the Mattes menu, with the blur picked, masks the blur
    /// (#1746): beside the chain, reading the source, one undoable step.
    /// Inside the grade, where there is no source, it is refused, said why.
    #[test]
    fn a_matte_added_with_an_effect_picked_masks_it() {
        let mut c = clip();
        let p = c.picture_v2().unwrap();
        let blur = p.picture.graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("gaussian_blur")).unwrap().id;
        let model = PictureCanvas::new(&p.picture.graph, root(&c), Some(blur)).unwrap();
        assert!(picture_menu().iter().any(|(label, items)| *label == "Mattes" && items.iter().any(|(_, tag)| *tag == "matte.luma_key")));
        let add = AppCommand::AddNode { clip_id: c.id, kind: "matte.luma_key".into(), node_id: None, position: (40.0, 300.0) };
        assert_eq!(model.refusal(&add), None);
        let AppCommand::GraphOps { ops, label } = model.command(add).unwrap() else { panic!("one graph batch") };
        assert_eq!(label, "Add Luma Key");
        c.apply_picture_ops(&ops).unwrap();
        let graph = &c.picture_v2().unwrap().picture.graph;
        let key = graph.edges.iter().find(|e| e.to == (blur, "mask".into())).expect("the blur is masked").from.0;
        let source = graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
        assert!(graph.edges.iter().any(|e| e.from.0 == source && e.to.0 == key), "the key reads the source");
        let on_wire = AppCommand::AddNodeWired { clip_id: c.id, kind: "matte.window".into(), position: (0.0, 0.0), from: source, from_port: 0 };
        assert!(model.refusal(&on_wire).is_some_and(|why| why.contains("add it from its menu")));
    }

    /// Every node of a clip's picture is on the canvas, in a wide strip
    /// and in a tall narrow dock (#1726: the dock showed none).
    #[test]
    fn a_picture_graph_is_framed_in_any_dock() {
        let t = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
        let c = TestClip::of(Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0)))
            .effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 })
            .effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 3.0 })
            .effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 4.0 })
            .build();
        let ctx = egui::Context::default();
        let mut panel = super::super::NodeGraphV2Panel::default();
        let mut level = root(&c);
        let (mut selected, mut set, mut time) = (None, Default::default(), 0.0);
        let picture = c.picture_v2().unwrap();
        let model = PictureCanvas::new(&picture.picture.graph, root(&c), None).unwrap();
        let labels: Vec<String> = model.nodes.iter().map(|n| n.label.clone()).collect();
        // First a small graph (a title's), then the clip's own.
        let small = TestClip::of(Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0))).build();
        let mut small_level = root(&small);
        for _ in 0..3 {
            time += 0.05;
            let _ = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(460.0, 900.0))), time: Some(time), ..Default::default() }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    panel.show_picture(ui, &small, &mut small_level, &mut selected, &mut set, &crate::theme::KlippTheme::default(), 0.0, true, &Default::default(), 0, &mut Vec::new()).unwrap();
                });
            });
        }
        // As the app draws it: a dock rect inside a whole window.
        let window = egui::vec2(1900.0, 1100.0);
        for dock in [egui::Rect::from_min_size(egui::pos2(300.0, 700.0), egui::vec2(1300.0, 260.0)), egui::Rect::from_min_size(egui::pos2(1100.0, 60.0), egui::vec2(420.0, 900.0))] {
            let mut texts = Vec::new();
            for _ in 0..3 {
                time += 0.05;
                let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, window)), time: Some(time), ..Default::default() }, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        crate::ui_util::allocate_clipped_ui(ui, dock, |ui| {
                            panel.show_picture(ui, &c, &mut level, &mut selected, &mut set, &crate::theme::KlippTheme::default(), 0.0, true, &Default::default(), 0, &mut Vec::new()).unwrap();
                        });
                    });
                });
                texts = out.shapes.iter().filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos + t.galley.size() / 2.0)), _ => None,
                }).collect::<Vec<_>>();
            }
            for label in labels.iter().filter(|l| !l.is_empty()) {
                let at = texts.iter().find(|(s, _)| s == label).map(|t| t.1);
                assert!(at.is_some_and(|p| dock.contains(p)), "{dock:?}: {label} at {at:?}, zoom {}", panel.zoom);
            }
        }
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
        assert!(!texts.iter().any(|(s, _)| s.contains("Save version")), "grade-only tools aren't offered");
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

    /// The graph dock drawn frame by frame on a clip in an app state, each
    /// command it sends applied as the app applies it (#1743).
    struct Dock {
        ctx: egui::Context,
        panel: super::super::NodeGraphV2Panel,
        state: crate::state::AppState,
        clip: klipp_core::ClipId,
        selected: Option<NodeId>,
        set: std::collections::HashSet<NodeId>,
        time: f64,
        texts: Vec<(String, egui::Pos2)>,
        sent: Vec<AppCommand>,
        /// The keys held through the frames that follow.
        mods: egui::Modifiers,
        /// The node cards drawn, at the canvas's zoom.
        cards: Vec<egui::Rect>,
        /// Every filled rectangle drawn, and its fill.
        fills: Vec<(egui::Rect, egui::Color32)>,
        /// Every circle drawn: its centre, and its fill.
        swatches: Vec<(egui::Pos2, egui::Color32)>,
    }

    impl Dock {
        fn new() -> Self {
            Self::of(clip())
        }
        fn of(c: Clip) -> Self {
            let mut state = crate::state::AppState::default();
            state.sequences[0].tracks.iter_mut().find(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap().clips.push(c.clone());
            state.selected_clip = Some(c.id);
            let mut d = Self { ctx: Default::default(), panel: Default::default(), state, clip: c.id, selected: None, set: Default::default(), time: 0.0, texts: Vec::new(), sent: Vec::new(), mods: egui::Modifiers::NONE, cards: Vec::new(), fills: Vec::new(), swatches: Vec::new() };
            d.settle();
            d
        }
        fn clip(&self) -> Clip {
            self.state.sequences[0].all_clips().find(|c| c.id == self.clip).unwrap().clone()
        }
        fn graph(&self) -> Graph {
            self.clip().picture_v2().unwrap().picture.graph.clone()
        }
        fn frame(&mut self, events: Vec<egui::Event>) {
            self.time += 0.05;
            let c = self.clip();
            let mut level = root(&c);
            // As the graph dock hands it the engine's solo, and how the
            // nodes fared on the frame shown.
            self.panel.picture_solo = self.state.solo_node;
            self.panel.compared_node = self.state.split_compare_node.as_ref().map(|(_, p)| p).and_then(|p| p.node());
            self.panel.node_feedback = self.state.node_feedback(self.clip);
            self.panel.node_labels = crate::app::node_labels::canvas_labels(&self.state.sequences[0]);
            let mut cmds = Vec::new();
            let (panel, selected, set) = (&mut self.panel, &mut self.selected, &mut self.set);
            let out = self.ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 700.0))), time: Some(self.time), events, modifiers: self.mods, ..Default::default() }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    cmds = panel.show_picture(ui, &c, &mut level, selected, set, &crate::theme::KlippTheme::default(), 0.0, true, &Default::default(), 0, &mut Vec::new()).unwrap();
                });
            });
            self.texts = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos + t.galley.size() / 2.0)), _ => None,
            }).collect();
            self.fills = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Rect(r) => Some((r.rect, r.fill)), _ => None,
            }).collect();
            self.swatches = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Circle(c) => Some((c.center, c.fill)), _ => None,
            }).collect();
            let (w, h) = (super::super::NODE_W * self.panel.zoom, super::super::NODE_H * self.panel.zoom);
            self.cards = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Rect(r) if (r.rect.width() - w).abs() < 0.5 && (r.rect.height() - h).abs() < 0.5 => Some(r.rect), _ => None,
            }).collect();
            for cmd in cmds.into_iter().filter(|c| !matches!(c, AppCommand::SetVisibleNodeThumbs { .. })) {
                // The solo the app mirrors for the canvas, as its colour
                // dispatch does.
                if let AppCommand::SetNodeSolo { at } = &cmd {
                    self.state.solo_node = at.as_ref().and_then(|a| a.path.node());
                } else if let AppCommand::GraphOps { ops, label } = &cmd {
                    // As the app applies a batch: one step, or refused.
                    self.state.before_dispatch(&cmd);
                    self.state.apply_path_ops(ops, label).unwrap_or_else(|why| panic!("{cmd:?} refused: {why}"));
                } else if matches!(cmd, AppCommand::ShowPanel { .. }) {
                    // The app's layout: nothing in the document.
                } else if self.state.node_edit(&cmd).is_some() {
                    assert_eq!(self.state.toast.message_for_test().filter(|m| !m.starts_with("Copied")), None, "{cmd:?} refused");
                } else {
                    self.state.before_dispatch(&cmd);
                    assert!(self.state.path_command(&cmd).is_some(), "{cmd:?} unhandled");
                    assert_eq!(self.state.toast.message_for_test(), None, "{cmd:?} refused");
                }
                self.sent.push(cmd);
            }
        }
        fn settle(&mut self) { for _ in 0..3 { self.frame(Vec::new()); } }
        fn at(&self, text: &str) -> egui::Pos2 {
            self.texts.iter().rev().find(|(s, _)| s.contains(text)).unwrap_or_else(|| panic!("no {text:?}: {:?}", self.texts)).1
        }
        fn button(at: egui::Pos2, button: egui::PointerButton, pressed: bool) -> egui::Event {
            egui::Event::PointerButton { pos: at, button, pressed, modifiers: egui::Modifiers::NONE }
        }
        fn click(&mut self, at: egui::Pos2, button: egui::PointerButton) {
            self.frame(vec![egui::Event::PointerMoved(at), Self::button(at, button, true)]);
            self.frame(vec![Self::button(at, button, false)]);
            self.settle();
        }
        fn menu(&mut self, on: &str, item: &str) {
            let at = self.at(on);
            self.click(at, egui::PointerButton::Secondary);
            let at = self.at(item);
            self.click(at, egui::PointerButton::Primary);
        }
        fn steps(&self) -> usize { self.state.active_history().undo_count() }
        fn notes(&self) -> Vec<NodeStickyNote> {
            self.graph().ui.sticky_notes.iter().map(|v| serde_json::from_value(v.clone()).unwrap()).collect()
        }
        fn frames(&self) -> Vec<NodeFrame> {
            self.graph().ui.frames.iter().map(|v| serde_json::from_value(v.clone()).unwrap()).collect()
        }
    }

    /// #1753: an effect node on the picture is given a colour label from
    /// its menu, one undo step kept on the sequence by the node's path,
    /// and its card is drawn with a band of the colour along its top;
    /// None from the same menu takes it off.
    #[test]
    fn a_picture_node_takes_a_colour_label_from_its_menu() {
        let mut d = Dock::new();
        let graph = d.graph();
        let blur = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("gaussian_blur")).unwrap().id;
        let label = PictureCanvas::new(&graph, root(&d.clip()), None).unwrap().nodes.iter().find(|n| n.id() == blur).unwrap().label.clone();
        let path = NodePath::new(Root::Clip { clip: d.clip.0 }, [blur]);
        let card = |d: &Dock| *d.cards.iter().find(|r| r.contains(d.at(&label))).unwrap();
        let purple = super::super::node_label_colour(6).unwrap();
        let banded = |d: &Dock| d.fills.iter().any(|(r, fill)| *fill == purple && card(d).contains_rect(*r) && (r.top() - card(d).top()).abs() < 0.5);
        assert!(!banded(&d), "no band before a label");

        let swatch = |d: &Dock, fill: egui::Color32| d.swatches.iter().rev().find(|(_, f)| *f == fill).unwrap_or_else(|| panic!("no swatch {fill:?}")).0;
        let at = d.at(&label);
        d.click(at, egui::PointerButton::Secondary);
        let at = swatch(&d, purple);
        d.click(at, egui::PointerButton::Primary);
        assert_eq!(d.state.sequences[0].node_label(&path), 6, "labelled: {:?}", d.sent);
        assert_eq!(d.steps(), 1);
        assert_eq!(d.state.active_history().undo_label(), Some("Node Colour Label"));
        assert!(banded(&d), "the card has the colour's band");

        let at = d.at(&label);
        d.click(at, egui::PointerButton::Secondary);
        // None is the first swatch, a step left of the red one.
        let red = swatch(&d, super::super::node_label_colour(1).unwrap());
        let orange = swatch(&d, super::super::node_label_colour(2).unwrap());
        d.click(red - (orange - red), egui::PointerButton::Primary);
        assert!(d.state.sequences[0].node_labels.is_empty());
        assert_eq!(d.steps(), 2);
        assert!(!banded(&d));
    }

    /// Notes and frames edit on the picture as on the grade (#1743), each
    /// edit one undo step: a note added from the toolbar, dragged (one
    /// step for the drag) and deleted from its menu; a frame made round
    /// two nodes, renamed and deleted from its menu. Undo takes the last
    /// edit back.
    #[test]
    fn notes_and_frames_edit_on_the_picture_one_step_each() {
        let mut d = Dock::new();
        // The view panned up off the nodes, so the note, made at its
        // centre, sits clear of them.
        let from = egui::pos2(600.0, 650.0);
        d.frame(vec![egui::Event::PointerMoved(from), Dock::button(from, egui::PointerButton::Middle, true)]);
        d.frame(vec![egui::Event::PointerMoved(from - egui::vec2(0.0, 400.0))]);
        d.frame(vec![Dock::button(from - egui::vec2(0.0, 400.0), egui::PointerButton::Middle, false)]);
        d.settle();
        let at = d.at("Note");
        d.click(at, egui::PointerButton::Primary);
        assert_eq!(d.notes().len(), 1, "a note added: {:?}", d.sent);
        assert_eq!(d.steps(), 1);

        let before = d.notes()[0].pos;
        let at = d.at("Click to edit");
        d.frame(vec![egui::Event::PointerMoved(at), Dock::button(at, egui::PointerButton::Primary, true)]);
        for step in [20.0, 40.0, 60.0] {
            d.frame(vec![egui::Event::PointerMoved(at + egui::vec2(step, step / 2.0))]);
        }
        d.frame(vec![Dock::button(at + egui::vec2(60.0, 30.0), egui::PointerButton::Primary, false)]);
        d.settle();
        let moved = d.notes()[0].pos;
        assert!((moved.x - before.x - 60.0 / d.panel.zoom).abs() < 0.5 && (moved.y - before.y - 30.0 / d.panel.zoom).abs() < 0.5, "{before:?} to {moved:?}: {:?}", d.sent);
        assert_eq!(d.steps(), 2, "the drag is one step");

        d.menu("Click to edit", "Delete note");
        assert!(d.notes().is_empty());
        assert_eq!(d.steps(), 3);

        let at = d.at("Fit");
        d.click(at, egui::PointerButton::Primary);
        let graph = d.graph();
        let blur = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("gaussian_blur")).unwrap().id;
        let grade = graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap().id;
        d.set = [blur, grade].into_iter().collect();
        d.selected = Some(blur);
        d.settle();
        let blur_label = PictureCanvas::new(&graph, root(&d.clip()), None).unwrap().nodes.iter().find(|n| n.id() == blur).unwrap().label.clone();
        d.menu(&blur_label, "Frame 2");
        assert_eq!(d.frames().len(), 1, "a frame made: {:?}", d.sent);
        assert_eq!(d.frames()[0].contained_node_ids, [blur, grade].into_iter().collect());
        assert_eq!(d.steps(), 4);

        d.menu("\u{2014}", "Rename");
        d.frame(vec![egui::Event::Text("Look".into())]);
        d.frame(vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
        d.settle();
        assert_eq!(d.frames()[0].label, "Look", "{:?}", d.sent);
        assert_eq!(d.steps(), 5);

        d.menu("Look", "Delete frame");
        assert!(d.frames().is_empty());
        assert_eq!(d.steps(), 6);

        let mut command = d.state.active_history_mut().undo().unwrap().command;
        while let crate::history::EditCommand::Named { edit, .. } = command { command = *edit; }
        let crate::history::EditCommand::PictureEdit { edit, .. } = command else { panic!("a picture step") };
        d.state.sequences[0].all_clips_mut().find(|c| c.id == d.clip).unwrap().restore_picture(&edit.before);
        assert_eq!(d.frames().len(), 1, "undo brings the frame back");
    }

    /// A node soloed from its menu shows as soloed on the picture canvas,
    /// its chip in the toolbar, and the chip's ✕ clears it (#1743). The
    /// control: handed no solo, as the dock used to, it shows none.
    #[test]
    fn solo_shows_and_clears_on_the_picture() {
        let mut d = Dock::new();
        let graph = d.graph();
        let model = PictureCanvas::new(&graph, root(&d.clip()), None).unwrap();
        let blur = model.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n.node) == Some("gaussian_blur")).unwrap();
        let label = blur.label.clone();
        let id = blur.id();
        d.menu(&label, "View in viewer");
        assert_eq!(d.state.solo_node, Some(id), "{:?}", d.sent);
        assert!(d.texts.iter().any(|(s, _)| s.contains(&format!("Viewing: {label}"))), "the chip: {:?}", d.texts);

        d.state.solo_node = None;
        d.settle();
        assert!(!d.texts.iter().any(|(s, _)| s.contains("Viewing:")), "no solo, no chip");
        d.state.solo_node = Some(id);
        d.settle();
        let at = d.at("\u{2715}");
        d.click(at, egui::PointerButton::Primary);
        assert_eq!(d.state.solo_node, None, "the chip's ✕ clears it: {:?}", d.sent);
        assert!(!d.texts.iter().any(|(s, _)| s.contains("Viewing:")));
    }

    /// A node's menu wipes the viewer between its input and its output
    /// (#2758): the compare is set on that node by its path, the menu then
    /// offers to end it, and ending it clears the compare.
    #[test]
    fn a_node_menu_compares_its_input_and_output() {
        let mut d = Dock::new();
        let graph = d.graph();
        let model = PictureCanvas::new(&graph, root(&d.clip()), None).unwrap();
        let blur = model.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n.node) == Some("gaussian_blur")).unwrap();
        let (label, id) = (blur.label.clone(), blur.id());
        d.menu(&label, "Compare input and output (A/B)");
        let compared = d.state.split_compare_node.clone();
        assert_eq!(compared.as_ref().map(|(c, p)| (*c, p.node())), Some((d.clip, Some(id))), "{:?}", d.sent);
        d.settle();
        d.menu(&label, "End the A/B compare");
        assert_eq!(d.state.split_compare_node, None, "{:?}", d.sent);
    }

    /// The eye on a node's header views it in the program viewer, as
    /// node editors do (#1763): clicked, the node is viewed and the chip
    /// says so; clicked again, or the Output's, the finished picture is
    /// back. The 1 key does the same for the selected node.
    #[test]
    fn the_eye_and_the_1_key_view_a_node_in_the_viewer() {
        let mut d = Dock::new();
        let graph = d.graph();
        let model = PictureCanvas::new(&graph, root(&d.clip()), None).unwrap();
        let blur = model.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n.node) == Some("gaussian_blur")).unwrap();
        let (label, id) = (blur.label.clone(), blur.id());
        let output = model.nodes.iter().find(|n| n.node.type_id.as_str() == lt::OUTPUT).unwrap().label.clone();
        let card = |d: &Dock, label: &str| { let at = d.at(label); *d.cards.iter().find(|r| r.contains(at) || r.expand(20.0).contains(at)).unwrap_or_else(|| panic!("no card for {label}: {:?}", d.cards)) };
        let eye = |d: &Dock, label: &str| super::super::view_eye_rect(card(d, label), d.panel.zoom).center();
        // Hovered, the eye shows; clicked, the node is viewed.
        let at = eye(&d, &label);
        d.frame(vec![egui::Event::PointerMoved(at)]);
        d.click(at, egui::PointerButton::Primary);
        assert_eq!(d.state.solo_node, Some(id), "the eye views it: {:?}", d.sent);
        assert_eq!(node_name(&d.clip(), &NodePath::new(root(&d.clip()).root, [id])), Some(label.clone()), "what the viewer names");
        d.settle();
        assert!(d.texts.iter().any(|(s, _)| s == &format!("Viewing: {label}")), "{:?}", d.texts);
        // Clicked again: back to the finished picture.
        let at = eye(&d, &label);
        d.click(at, egui::PointerButton::Primary);
        assert_eq!(d.state.solo_node, None, "{:?}", d.sent);
        // The 1 key on the selected node, then the Output's eye.
        d.click(d.at(&label), egui::PointerButton::Primary);
        d.frame(vec![egui::Event::Key { key: egui::Key::Num1, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
        d.settle();
        assert_eq!(d.state.solo_node, Some(id), "1 views the selected node: {:?}", d.sent);
        let at = eye(&d, &output);
        d.frame(vec![egui::Event::PointerMoved(at)]);
        d.click(at, egui::PointerButton::Primary);
        assert_eq!(d.state.solo_node, None, "the Output's eye is the finished picture");
    }

    /// An effect picked where the picture has nowhere to put it (inside
    /// the grade group) says why on the canvas instead of doing nothing;
    /// at the clip's level the same pick lands (#1743).
    #[test]
    fn an_add_lands_or_says_why() {
        let c = clip();
        let graph = c.picture_v2().unwrap().picture.graph.clone();
        let grade = graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap().id;
        let inside = NodePath::new(root(&c).root, [grade]);
        let add = AppCommand::AddNode { clip_id: c.id, kind: "gaussian_blur".into(), node_id: None, position: (0.0, 0.0) };
        let canvas = PictureCanvas::new(&graph, inside, None).unwrap();
        assert!(canvas.command(add.clone()).is_none());
        assert!(canvas.refusal(&add).is_some_and(|why| why.contains("not inside this group")), "{:?}", canvas.refusal(&add));
        let canvas = PictureCanvas::new(&graph, root(&c), None).unwrap();
        assert!(canvas.command(add.clone()).is_some(), "at the clip's level it lands");
        assert_eq!(canvas.refusal(&add), None);

        // Through the canvas: the pick in the add menu shows the reason.
        let mut panel = super::super::NodeGraphV2Panel::default();
        let ctx = egui::Context::default();
        let mut level = NodePath::new(root(&c).root, [grade]);
        let (mut selected, mut set) = (None, Default::default());
        let mut texts = Vec::new();
        for i in 0..6 {
            if i == 3 {
                panel.add_menu_open = true;
                panel.add_menu_screen_pos = egui::pos2(100.0, 100.0);
                panel.add_menu_search = "Gaussian".into();
            }
            let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 700.0))), time: Some(i as f64 * 0.05), ..Default::default() }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    panel.show_picture(ui, &c, &mut level, &mut selected, &mut set, &crate::theme::KlippTheme::default(), 0.0, true, &Default::default(), 0, &mut Vec::new()).unwrap();
                });
            });
            texts = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos + t.galley.size() / 2.0)), _ => None,
            }).collect::<Vec<_>>();
        }
        let at = texts.iter().rev().find(|(s, _)| s.contains("Gaussian")).unwrap_or_else(|| panic!("{texts:?}")).1;
        let mut cmds = Vec::new();
        for (i, events) in [vec![egui::Event::PointerMoved(at), egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }], vec![egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }], vec![], vec![]].into_iter().enumerate() {
            let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 700.0))), time: Some(1.0 + i as f64 * 0.05), events, ..Default::default() }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    cmds.extend(panel.show_picture(ui, &c, &mut level, &mut selected, &mut set, &crate::theme::KlippTheme::default(), 0.0, true, &Default::default(), 0, &mut Vec::new()).unwrap());
                });
            });
            texts = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos)), _ => None,
            }).collect::<Vec<_>>();
        }
        assert!(!cmds.iter().any(|c| matches!(c, AppCommand::GraphOps { .. })), "{cmds:?}");
        assert!(texts.iter().any(|(s, _)| s.contains("not inside this group")), "the reason shown: {:?}", texts.iter().map(|t| &t.0).collect::<Vec<_>>());
    }

    fn kinds(d: &Dock) -> Vec<String> {
        d.graph().nodes.iter().filter_map(|n| klipp_graph_lift::effect_params::tag(n).map(str::to_owned)).collect()
    }

    fn blur_label(d: &Dock) -> String {
        let graph = d.graph();
        let model = PictureCanvas::new(&graph, root(&d.clip()), None).unwrap();
        model.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n.node) == Some("gaussian_blur")).unwrap().label.clone()
    }

    /// Ctrl+D, Ctrl+C and Ctrl+V on the picture canvas (#1745): an
    /// effect duplicated, copied and pasted, each edit one undo step.
    #[test]
    fn picture_canvas_duplicates_copies_and_pastes() {
        let mut d = Dock::new();
        let at = d.at(&blur_label(&d));
        d.click(at, egui::PointerButton::Primary);
        let count = |d: &Dock| kinds(d).iter().filter(|k| *k == "gaussian_blur").count();
        assert_eq!(count(&d), 1);
        d.mods = egui::Modifiers::COMMAND;
        let key = |key| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND };
        d.frame(vec![key(egui::Key::D)]);
        d.settle();
        assert_eq!(count(&d), 2, "duplicated: {:?}", d.sent);
        assert_eq!(d.steps(), 1);
        d.frame(vec![key(egui::Key::C)]);
        assert_eq!(d.state.picture_node_clipboard.len(), 1, "copied: {:?}", d.sent);
        d.frame(vec![key(egui::Key::V)]);
        d.settle();
        assert_eq!(count(&d), 3, "pasted: {:?}", d.sent);
        assert_eq!(d.steps(), 2);
    }

    /// The node menu on the picture: Reset to defaults puts an effect's
    /// settings back, "Replace with…" swaps it for another effect in its
    /// place, and Show in Inspector opens the Effects tab; one step each.
    #[test]
    fn the_picture_node_menu_resets_replaces_and_shows() {
        let mut d = Dock::new();
        let label = blur_label(&d);
        let blur = d.graph().nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("gaussian_blur")).unwrap().id;
        // Compared as effects: the stored node holds its settings as
        // parameters, a default left out.
        let effect = |v: serde_json::Value| serde_json::from_value::<klipp_effects::BuiltinEffect>(v).ok();
        let reset = effect(crate::inspector::stack_view::new_effect_node("gaussian_blur").unwrap().legacy.unwrap().payload);
        let payload = |d: &Dock| d.graph().nodes.iter().find(|n| n.id == blur).and_then(|n| klipp_graph_lift::effect_params::effect_of(n).ok());
        assert!(reset.is_some());
        assert_ne!(payload(&d), reset, "the fixture's blur is not at its defaults");
        d.menu(&label, "Reset to defaults");
        assert_eq!(payload(&d), reset, "reset: {:?}", d.sent);
        assert_eq!(d.steps(), 1);

        d.menu(&label, "Show in Inspector");
        assert!(d.sent.iter().any(|c| matches!(c, AppCommand::ShowPanel { inspector_tab: Some(t), .. } if t == "effects")));
        assert_eq!(d.selected, Some(blur));

        d.menu(&label, "Replace with\u{2026}");
        d.frame(vec![egui::Event::Text("Sharpen".into())]);
        d.frame(vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
        d.settle();
        let now = kinds(&d);
        assert!(!now.iter().any(|k| k == "gaussian_blur") && now.iter().any(|k| k == "sharpen"), "{now:?}: {:?}", d.sent);
        assert_eq!(d.steps(), 2);
    }

    /// The picture canvas's node `id`: its card, its input `port`, its
    /// output, as drawn (#1744).
    fn card_of(d: &Dock, id: NodeId) -> egui::Rect {
        let graph = d.graph();
        let model = PictureCanvas::new(&graph, root(&d.clip()), None).unwrap();
        let label = model.nodes.iter().find(|n| n.id() == id).unwrap().label.clone();
        let at = d.at(&label);
        *d.cards.iter().find(|r| r.contains(at)).unwrap_or_else(|| panic!("no card for {label}"))
    }
    fn in_port(d: &Dock, id: NodeId, port: u8) -> egui::Pos2 {
        let graph = d.graph();
        let model = PictureCanvas::new(&graph, root(&d.clip()), None).unwrap();
        super::super::input_port_pos(&card_of(d, id), port, model.nodes.iter().find(|n| n.id() == id).unwrap().input_count())
    }
    fn out_port(d: &Dock, id: NodeId) -> egui::Pos2 {
        let graph = d.graph();
        let model = PictureCanvas::new(&graph, root(&d.clip()), None).unwrap();
        super::super::output_port_pos(&card_of(d, id), 0, &model.nodes.iter().find(|n| n.id() == id).unwrap().kind())
    }
    fn effect(d: &Dock, tag: &str) -> NodeId {
        d.graph().nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some(tag)).unwrap_or_else(|| panic!("no {tag}")).id
    }
    fn display(d: &Dock) -> NodeId {
        d.graph().nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id
    }
    /// What reads `id`'s output, in the picture.
    fn reader(d: &Dock, id: NodeId) -> Option<NodeId> {
        d.graph().edges.iter().find(|e| e.from.0 == id).map(|e| e.to.0)
    }
    impl Dock {
        fn drag(&mut self, from: egui::Pos2, to: egui::Pos2) {
            self.frame(vec![egui::Event::PointerMoved(from), Dock::button(from, egui::PointerButton::Primary, true)]);
            for i in 1..=4 {
                self.frame(vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 4.0))]);
            }
            self.frame(vec![Dock::button(to, egui::PointerButton::Primary, false)]);
            self.settle();
        }
        fn type_and_enter(&mut self, text: &str) {
            self.frame(vec![egui::Event::Text(text.into())]);
            self.frame(vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
            self.settle();
        }
    }

    /// The wire menu on the picture (#1744): Insert node… puts the
    /// effect picked into the wire, one step; there is no reroute here.
    #[test]
    fn a_picture_wire_takes_a_node_from_its_menu() {
        let mut d = Dock::new();
        let (s2d, blur) = (display(&d), effect(&d, "gaussian_blur"));
        let mid = super::super::bezier_at_phase(out_port(&d, s2d), in_port(&d, blur, 0), 0.5);
        d.click(mid, egui::PointerButton::Secondary);
        assert!(d.texts.iter().any(|(t, _)| t == "Insert node\u{2026}") && !d.texts.iter().any(|(t, _)| t == "Add reroute"), "the wire menu: {:?}", d.texts);
        let at = d.at("Insert node\u{2026}");
        d.click(at, egui::PointerButton::Primary);
        d.type_and_enter("Sharpen");
        let sharpen = effect(&d, "sharpen");
        assert_eq!((reader(&d, s2d), reader(&d, sharpen)), (Some(sharpen), Some(blur)), "into the wire: {:?}", d.sent);
        assert_eq!(d.steps(), 1);
    }

    /// A reroute taken from a picture wire's menu goes into the wire, one
    /// step: what fed the wire feeds it, and it feeds what the wire fed
    /// (#1746). The frame is as it was.
    #[test]
    fn a_picture_wire_takes_a_reroute_from_its_menu() {
        let mut d = Dock::new();
        let (s2d, blur) = (display(&d), effect(&d, "gaussian_blur"));
        let before = d.clip();
        let mid = super::super::bezier_at_phase(out_port(&d, s2d), in_port(&d, blur, 0), 0.5);
        d.click(mid, egui::PointerButton::Secondary);
        let at = d.at("Insert node\u{2026}");
        d.click(at, egui::PointerButton::Primary);
        d.type_and_enter("Reroute");
        let reroute = d.graph().nodes.iter().find(|n| klipp_graph_lift::structure::kind_of(n) == "Reroute").map(|n| n.id).unwrap_or_else(|| panic!("no reroute: {:?}", d.sent));
        assert_eq!((reader(&d, s2d), reader(&d, reroute)), (Some(reroute), Some(blur)), "into the wire: {:?}", d.sent);
        assert_eq!(d.steps(), 1);
        let effects = |c: &Clip| klipp_compose::resolve::resolve_effects_at(c, &c.picture_v2().unwrap().picture, c.timeline_in, 25.0, None, None, 1.0).unwrap();
        assert_eq!(effects(&d.clip()), effects(&before), "the effects drawn are the same");
    }

    /// An effect dragged onto a wire of the picture moves there in the
    /// chain, the drag and the drop one undo step (#1744).
    #[test]
    fn a_picture_effect_dropped_on_a_wire_moves_into_it() {
        let t = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
        let c = TestClip::of(Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0)))
            .effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 })
            .effect(klipp_effects::BuiltinEffect::Sharpen { amount: 0.5 })
            .build();
        let mut d = Dock::of(c);
        let (s2d, blur, sharpen) = (display(&d), effect(&d, "gaussian_blur"), effect(&d, "sharpen"));
        assert_eq!((reader(&d, s2d), reader(&d, blur)), (Some(blur), Some(sharpen)));
        let mid = super::super::bezier_at_phase(out_port(&d, s2d), in_port(&d, blur, 0), 0.5);
        let from = card_of(&d, sharpen).center();
        d.drag(from, mid);
        assert_eq!((reader(&d, s2d), reader(&d, sharpen)), (Some(sharpen), Some(blur)), "Sharpen now before Blur: {:?}", d.sent);
        assert_eq!(d.steps(), 1, "one step: {:?}", d.sent);
    }

    /// A wire dragged from an effect's output into empty canvas opens the
    /// add menu; the effect picked comes after it, one step. A wire into
    /// what feeds it is refused on the canvas, saying why (#1744).
    #[test]
    fn a_picture_wire_let_go_adds_a_node_or_refuses_a_loop() {
        let mut d = Dock::new();
        let (s2d, blur) = (display(&d), effect(&d, "gaussian_blur"));
        let below = card_of(&d, blur).center() + egui::vec2(0.0, 200.0);
        d.drag(out_port(&d, blur), below);
        d.type_and_enter("Sharpen");
        let sharpen = effect(&d, "sharpen");
        assert_eq!(reader(&d, blur), Some(sharpen), "fed by the wire: {:?}", d.sent);
        assert_eq!(d.steps(), 1);

        d.drag(out_port(&d, sharpen), in_port(&d, s2d, 0));
        assert_eq!(d.steps(), 1, "nothing made");
        assert!(d.panel.bookmark_toast.as_ref().is_some_and(|(t, _)| t.contains("loop")), "{:?}", d.panel.bookmark_toast);
    }

    /// A node the frame shown declined and one whose preview the renderer
    /// couldn't make each carry a badge, and the reason on hover (#1747).
    /// The preview's reason is the renderer's own, from an inspected render
    /// of the clip asking for every node's thumbnail, as the engine asks.
    #[test]
    fn declined_and_unavailable_nodes_show_their_badge_and_reason() {
        // Two plug-in effects, which the renderer doesn't inspect: one is
        // also declined by the frame shown, as a missing plug-in is.
        let t = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
        let plugin = |name: &str| klipp_plugin::AppliedEffect { plugin_name: name.into(), enabled: true, ..Default::default() };
        let c = TestClip::of(Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0)))
            .effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 })
            .plugin(plugin("com.example.Missing"))
            .plugin(plugin("com.example.Glow"))
            .build();
        let mut d = Dock::of(c);
        let c = d.clip();
        let graph = d.graph();
        let model = PictureCanvas::new(&graph, root(&c), None).unwrap();
        let wanted: Vec<_> = model.nodes.iter().filter_map(|n| model.output(n.id())).collect();
        let seq = d.state.sequence().baked_for_render().into_owned();
        let mut renderer = klipp_compose::Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        renderer.set_gpu_enabled(false);
        let inspect = klipp_compose::Inspect { clip: Some(c.id), only: Some(c.id), thumbs: Some(wanted.iter().cloned().collect()), ..Default::default() };
        let t = klipp_core::RationalTime::from_secs_f64(1.0, 25);
        struct Grey;
        impl klipp_compose::FrameSource for Grey {
            fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<klipp_compose::SourceFrame> {
                Some(klipp_compose::SourceFrame { pixels: klipp_compose::SourcePixels::Linear(std::sync::Arc::new([0.18, 0.18, 0.18, 1.0].repeat((w * h) as usize))), width: w, height: h, pts: None })
            }
        }
        let (_, found) = renderer.render_inspected(&seq, t, 64, 36, &mut Grey, klipp_compose::RenderOptions::default(), inspect);
        let batch = klipp_engine::node_thumbnails::ThumbnailBatch::new(1, 0, klipp_engine::node_thumbnails::PreviewQuality::Viewer, klipp_timeline::SharedSequence::new(seq.clone()).revision(), &wanted, found.thumbs)
            .with_unavailable(&wanted, found.unavailable);
        let plugins: Vec<NodeId> = graph.nodes.iter().filter(|n| n.type_id.as_str() == klipp_graph_format::types::LEGACY_OFX).map(|n| n.id).collect();
        let (declined, no_preview) = (plugins[0], plugins[1]);
        let why = batch.unavailable.iter().find(|(at, _)| at.path.node() == Some(no_preview)).map(|(_, why)| why.clone()).expect("the renderer has no preview of a plug-in");
        d.state.node_thumbs_unavailable = batch.unavailable.clone();
        d.state.frame_declines.push(klipp_nodes::Decline {
            clip: Some(c.id), node: declined, kind: NodeKind::Ofx, outcome: klipp_nodes::DeclineOutcome::PluginMissing,
            name: "Missing".into(), reason: "the plug-in isn't installed".into(), why: String::new(),
        });
        d.settle();
        let tooltip = |d: &mut Dock, node: NodeId| {
            let at = card_of(d, node).center();
            for _ in 0..12 {
                d.frame(vec![egui::Event::PointerMoved(at)]);
            }
            d.texts.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>()
        };
        let texts = tooltip(&mut d, declined);
        assert!(texts.iter().any(|t| t == "Missing") && texts.iter().any(|t| t == "Glow"), "plug-ins by their own names: {texts:?}");
        assert!(texts.iter().any(|t| t == "!"), "the declined badge: {texts:?}");
        assert!(texts.iter().any(|t| t == "Declined: the plug-in isn't installed"), "its reason on hover: {texts:?}");
        let texts = tooltip(&mut d, no_preview);
        assert!(texts.iter().any(|t| t == "\u{2013}"), "the no-preview badge: {texts:?}");
        assert!(texts.iter().any(|t| *t == format!("No preview: {why}")), "the renderer's reason on hover: {texts:?}");

        // The control: with nothing reported there is no badge.
        d.state.frame_declines.clear();
        d.state.node_thumbs_unavailable.clear();
        let texts = tooltip(&mut d, declined);
        assert!(!texts.iter().any(|t| t == "!" || t.starts_with("Declined") || t.starts_with("No preview")), "{texts:?}");
    }

    /// A wire the picture's rules refuse says why on the canvas, where it
    /// was dropped, and nothing is sent or changed (#1747): lifting the
    /// wire into an effect and dropping it on the next node's input
    /// would leave that effect fed by nothing.
    #[test]
    fn a_refused_picture_reconnect_says_why_on_the_canvas() {
        let t = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
        let c = TestClip::of(Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0)))
            .effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 })
            .effect(klipp_effects::BuiltinEffect::Sharpen { amount: 0.5 })
            .build();
        let mut d = Dock::of(c);
        let before = d.graph();
        let sharpen = effect(&d, "sharpen");
        let placement = before.nodes.iter().find(|n| n.type_id.as_str() == lt::PLACEMENT).unwrap().id;
        d.drag(in_port(&d, sharpen, 0), in_port(&d, placement, 0));
        let toast = d.panel.bookmark_toast.as_ref().map(|(t, _)| t.clone());
        assert!(toast.as_deref().is_some_and(|t| t.starts_with("Reconnect wire: ")), "said on the canvas: {toast:?}: {:?}", d.sent);
        assert!(!d.sent.iter().any(|c| matches!(c, AppCommand::GraphOps { .. })), "nothing sent: {:?}", d.sent);
        assert_eq!(d.graph(), before, "nothing changed");
        assert_eq!(d.steps(), 0);
    }
}

