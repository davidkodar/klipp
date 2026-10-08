use klipp_graph_format::lifted::{types as lt, Origin};
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::{
    Edge, Graph, GroupBody, Node, NodeId, NodeSpec, PortCatalog, PortKey, PortSpec, PortType,
};
use klipp_graph_lift::stack_view::{
    self, Item, PlacementBoundary, ProjectionError, ResolvedPlacement, Section, StackView,
};

struct Catalog;
struct Fallback;
impl PortCatalog for Fallback {
    fn node(&self, _: &str) -> Option<NodeSpec> {
        None
    }
}
impl PortCatalog for Catalog {
    fn node(&self, ty: &str) -> Option<NodeSpec> {
        let ports: &[&str] = match ty {
            "fx" => &["in", "mask"],
            "merge" => &["bg", "fg"],
            "mixer" => &["in0", "in1", "in2"],
            "custom" => &["other", "picture"],
            _ => return None,
        };
        let port = |key: &str| PortSpec {
            key: key.into(),
            ty: if key == "mask" {
                PortType::Matte
            } else {
                PortType::Image
            },
            optional: key == "mask",
            params: Vec::new(),
        };
        Some(NodeSpec {
            inputs: ports.iter().map(|k| port(k)).collect(),
            outputs: vec![port("out")],
            mixable: ty == "fx",
        })
    }
    fn primary_input(&self, n: &Node) -> Option<PortKey> {
        match n.type_id.as_str() {
            "merge" => Some("bg".into()),
            "mixer" => Some("in0".into()),
            "custom" => Some("picture".into()),
            _ => Fallback.primary_input(n),
        }
    }
}

fn id(n: u128) -> NodeId {
    NodeId::from_u128(n)
}
fn root() -> Root {
    Root::Clip { clip: id(1000) }
}
fn addr(n: u128) -> NodePath {
    NodePath::new(root(), [id(n)])
}
fn node(n: u128, ty: &str) -> Node {
    Node {
        id: id(n),
        type_id: ty.into(),
        type_version: 1,
        label: format!("{ty}/{n}"),
        enabled: true,
        params: Default::default(),
        anim: Default::default(),
        mix: None,
        wireless: Default::default(),
        group: None,
        legacy: None,
        legacy_flags: None,
        origin: None,
    }
}
fn edge(from: u128, to: u128, port: &str) -> Edge {
    Edge {
        from: (id(from), "out".into()),
        to: (id(to), port.into()),
    }
}
fn chain(types: &[&str]) -> Graph {
    let mut graph = Graph::empty();
    graph.nodes.push(node(1, lt::SOURCE));
    for (i, ty) in types.iter().enumerate() {
        graph.nodes.push(node(i as u128 + 2, ty));
    }
    let last = types.len() as u128 + 2;
    graph.nodes.push(node(last, lt::OUTPUT));
    graph.output = id(last);
    graph.edges = (1..last).map(|i| edge(i, i + 1, "in")).collect();
    graph
}
fn view(graph: &Graph) -> StackView {
    stack_view::project(root(), graph, &Catalog, None).unwrap()
}
fn ids(view: &StackView) -> Vec<NodeId> {
    view.rows().map(|r| r.addr.node().unwrap()).collect()
}

#[test]
fn a_custom_graph_that_revisits_a_section_repeats_the_header() {
    let graph = chain(&[lt::TEMPORAL_DENOISE, "fx", lt::GROUP, "fx", lt::FRAME_BLEND]);
    let before = graph.clone();
    let view = view(&graph);
    assert_eq!(ids(&view), (2..=6).map(id).collect::<Vec<_>>());
    let headers: Vec<_> = view
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Header(s) => Some(*s),
            _ => None,
        })
        .collect();
    assert_eq!(
        headers,
        [
            Section::Source,
            Section::Effects,
            Section::Colour,
            Section::Effects,
            Section::Source
        ]
    );
    assert_eq!(graph, before);
}

#[test]
fn mask_branches_are_chips_not_rows() {
    let mut graph = chain(&["fx"]);
    graph.nodes.push(node(90, "mask-producer"));
    graph.edges.extend([edge(1, 90, "in"), edge(90, 2, "mask")]);
    let view = view(&graph);
    assert_eq!(ids(&view), [id(2)]);
    let row = view.rows().next().unwrap();
    assert!(row.mixable);
    assert_eq!(row.mask_chips.len(), 1);
    assert_eq!(row.mask_chips[0].source, addr(90));
    assert_eq!(row.mask_chips[0].input.as_str(), "mask");
    assert!(row.sub_stacks.is_empty());
}

#[test]
fn a_group_is_one_row_until_explicitly_entered() {
    let mut graph = chain(&[lt::GROUP]);
    let mut body = chain(&["fx", "fx"]);
    body.nodes[0].type_id = lt::GROUP_INPUT.into();
    body.nodes.last_mut().unwrap().type_id = lt::GROUP_OUTPUT.into();
    graph.nodes[1].group = Some(Box::new(GroupBody {
        graph: body.clone(),
        ports: Vec::new(),
        interface: Vec::new(),
    }));
    let outer = view(&graph);
    assert_eq!(ids(&outer), [id(2)]);
    assert!(outer.rows().next().unwrap().is_group);
    let entered = stack_view::project_at(root(), &[id(2)], &body, &Catalog, None).unwrap();
    assert_eq!(
        entered.rows().map(|r| r.addr.clone()).collect::<Vec<_>>(),
        [
            NodePath::new(root(), [id(2), id(2)]),
            NodePath::new(root(), [id(2), id(3)])
        ]
    );
}

#[test]
fn merge_background_is_the_spine_and_foreground_is_collapsed() {
    let mut graph = chain(&["fx", "merge"]);
    graph.edges[1].to.1 = "bg".into();
    graph.nodes.push(node(90, "fx"));
    graph.edges.extend([edge(1, 90, "in"), edge(90, 3, "fg")]);
    let projected = view(&graph);
    assert_eq!(ids(&projected), [id(2), id(3)]);
    let merge = projected.rows().last().unwrap();
    assert_eq!(merge.sub_stacks.len(), 1);
    assert_eq!(merge.sub_stacks[0].input.as_str(), "fg");
    assert_eq!(ids(&merge.sub_stacks[0].stack), [id(90)]);
    assert_eq!(merge.sub_stacks[0].stack.upstream, Some(addr(1)));
    graph.edges.reverse();
    assert_eq!(
        view(&graph),
        projected,
        "edge order never chooses the primary input"
    );
}

#[test]
fn every_mixer_side_input_is_preserved() {
    let mut graph = chain(&["mixer"]);
    graph.edges[0].to.1 = "in0".into();
    for (n, port) in [(90, "in1"), (91, "in2")] {
        graph.nodes.push(node(n, "fx"));
        graph.edges.extend([edge(1, n, "in"), edge(n, 2, port)]);
    }
    let projected = view(&graph);
    let mixer = projected.rows().next().unwrap();
    assert_eq!(
        mixer
            .sub_stacks
            .iter()
            .map(|b| b.input.as_str())
            .collect::<Vec<_>>(),
        ["in1", "in2"]
    );
    assert_eq!(ids(&mixer.sub_stacks[0].stack), [id(90)]);
    assert_eq!(ids(&mixer.sub_stacks[1].stack), [id(91)]);
}

#[test]
fn the_catalog_declares_a_primary_input_other_than_its_first_port() {
    let mut graph = chain(&["custom"]);
    graph.edges[0].to.1 = "picture".into();
    graph.nodes.push(node(90, "fx"));
    graph
        .edges
        .extend([edge(1, 90, "in"), edge(90, 2, "other")]);
    assert_eq!(ids(&view(&graph)), [id(2)]);
}

#[test]
fn ofx_rows_keep_their_render_position_and_disabled_rows_remain() {
    let mut graph = chain(&["klipp.legacy.ofx", "fx", "klipp.legacy.ofx", "fx"]);
    graph.nodes[2].enabled = false;
    graph.nodes[3].legacy = Some(
        serde_json::from_value(serde_json::json!({
            "payload": {"enabled": false}
        }))
        .unwrap(),
    );
    let view = view(&graph);
    assert_eq!(
        view.rows().map(|r| r.type_id.as_str()).collect::<Vec<_>>(),
        ["klipp.legacy.ofx", "fx", "klipp.legacy.ofx", "fx"]
    );
    assert!(!view.rows().nth(1).unwrap().enabled);
    assert!(!view.rows().nth(2).unwrap().enabled);
}

#[test]
fn placement_uses_only_the_supplied_resolution() {
    let mut graph = chain(&["fx", lt::PLACEMENT, "fx"]);
    graph.nodes[2].params.insert(
        "mode".into(),
        klipp_graph_format::ParamValue::Enum(lt::PLACEMENT_AUTO),
    );
    let before = graph.clone();
    let pending = view(&graph);
    assert_eq!(ids(&pending), [id(2), id(3), id(4)]);
    assert!(pending
        .rows()
        .nth(1)
        .unwrap()
        .placement
        .as_ref()
        .unwrap()
        .resolved
        .is_none());
    let mut resolved = ResolvedPlacement {
        boundary: PlacementBoundary::Before(addr(2)),
        reason: "executor result".into(),
        provenance: None,
        plan_revision: 17,
    };
    let placed = stack_view::project(root(), &graph, &Catalog, Some(&resolved)).unwrap();
    assert_eq!(ids(&placed), [id(3), id(2), id(4)]);
    assert_eq!(
        placed
            .rows()
            .next()
            .unwrap()
            .placement
            .as_ref()
            .unwrap()
            .resolved,
        Some(resolved.clone())
    );
    resolved.boundary = PlacementBoundary::End;
    assert_eq!(
        ids(&stack_view::project(root(), &graph, &Catalog, Some(&resolved)).unwrap()),
        [id(2), id(4), id(3)]
    );
    resolved.boundary = PlacementBoundary::Before(addr(99));
    assert!(matches!(
        stack_view::project(root(), &graph, &Catalog, Some(&resolved)),
        Err(ProjectionError::PlacementBoundaryNotOnSpine(_))
    ));
    assert_eq!(graph, before);
}

#[test]
fn inherited_rows_are_read_only_with_their_provenance() {
    let mut graph = chain(&["fx"]);
    let origin = Origin::Track { track: id(700) };
    graph.nodes[1].origin = Some(origin);
    let projected = view(&graph);
    let row = projected.rows().next().unwrap();
    assert!(row.read_only);
    assert_eq!(row.provenance, Some(origin));
    assert_eq!(row.addr, addr(2));
}

#[test]
fn wireless_inputs_work_and_drawn_edges_take_precedence() {
    let mut graph = chain(&["fx", "fx"]);
    graph.nodes[1].wireless.output_link = Some("feed".into());
    graph.nodes[2]
        .wireless
        .input_links
        .insert("in".into(), "feed".into());
    let wired = view(&graph);
    graph.edges.retain(|e| e.to != (id(3), "in".into()));
    assert_eq!(view(&graph), wired);
    graph.edges.push(edge(1, 3, "in"));
    assert_eq!(
        ids(&view(&graph)),
        [id(3)],
        "the drawn source bypasses the wireless effect"
    );
}

#[test]
fn invalid_dependencies_are_reported_instead_of_guessed() {
    let mut graph = chain(&["fx"]);
    graph.edges[0] = edge(2, 2, "in");
    assert_eq!(
        stack_view::project(root(), &graph, &Catalog, None),
        Err(ProjectionError::Cycle)
    );
    graph.edges[0] = edge(1, 2, "in");
    graph.edges.push(edge(1, 2, "in"));
    assert!(matches!(
        stack_view::project(root(), &graph, &Catalog, None),
        Err(ProjectionError::MultipleInputs { .. })
    ));
    graph.edges.clear();
    assert!(matches!(
        stack_view::project(root(), &graph, &Catalog, None),
        Err(ProjectionError::MissingInput { .. })
    ));
}
