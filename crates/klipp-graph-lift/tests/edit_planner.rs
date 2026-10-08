use klipp_graph_format::{
    lifted::{types as lt, ClipPicture},
    path::{NodePath, Root},
    Edge, Graph, Node, NodeId, NodeSpec, PortCatalog, PortSpec, PortType,
};
use klipp_graph_lift::{
    edit_planner::{self, Action, Request},
    ops,
    paths::Documents,
};

struct Catalog;
impl PortCatalog for Catalog {
    fn node(&self, ty: &str) -> Option<NodeSpec> {
        let (inputs, output): (&[(&str, PortType, bool)], _) = match ty {
            "fx" => (
                &[
                    ("in", PortType::Image, false),
                    ("mask", PortType::Matte, true),
                ],
                PortType::Image,
            ),
            "convert" => (&[("in", PortType::Matte, false)], PortType::Image),
            "matte" => (&[("in", PortType::Image, false)], PortType::Matte),
            "merge" => (
                &[
                    ("fg", PortType::Image, false),
                    ("bg", PortType::Image, false),
                ],
                PortType::Image,
            ),
            _ => return None,
        };
        let p = |key: &str, ty, optional| PortSpec {
            key: key.into(),
            ty,
            optional,
            params: Vec::new(),
        };
        Some(NodeSpec {
            inputs: inputs.iter().map(|(k, t, o)| p(k, *t, *o)).collect(),
            outputs: vec![p("out", output, false)],
            mixable: ty == "fx",
        })
    }
}
fn id(n: u128) -> NodeId {
    NodeId::from_u128(n)
}
fn root() -> Root {
    Root::Clip { clip: id(100) }
}
fn path(n: u128) -> NodePath {
    NodePath::new(root(), [id(n)])
}
fn node(n: u128, ty: &str) -> Node {
    serde_json::from_value(serde_json::json!({"id": id(n), "type_id": ty, "type_version": 1, "label": format!("node {n}"), "enabled": true})).unwrap()
}
fn edge(from: u128, to: u128, port: &str) -> Edge {
    Edge {
        from: (id(from), "out".into()),
        to: (id(to), port.into()),
    }
}
fn graph() -> Graph {
    Graph {
        nodes: vec![
            node(1, lt::SOURCE),
            node(2, "fx"),
            node(3, "fx"),
            node(4, lt::OUTPUT),
        ],
        edges: vec![edge(1, 2, "in"), edge(2, 3, "in"), edge(3, 4, "in")],
        output: id(4),
        ..Graph::empty()
    }
}
fn docs(g: Graph) -> Documents {
    let p: ClipPicture =
        serde_json::from_value(serde_json::json!({"graph":g,"opacity":1.0})).unwrap();
    let mut docs = Documents::default();
    docs.clips.insert(id(100), p);
    docs
}
fn request(n: u128, action: Action) -> Request {
    Request {
        path: path(n),
        action,
    }
}
fn current(d: &Documents) -> &Graph {
    &d.clips[&id(100)].graph
}
fn masked() -> Graph {
    let mut g = graph();
    g.nodes.push(node(10, "matte"));
    g.edges.extend([edge(1, 10, "in"), edge(10, 2, "mask")]);
    g
}
fn refused(g: Graph, request: Request, reason: &str) {
    let d = docs(g.clone());
    let err = edit_planner::propose(&d, request, 1, &Catalog).unwrap_err();
    assert!(err.to_string().contains(reason), "{err}");
    assert_eq!(current(&d), &g);
}
#[test]
fn linear_bypass_and_exact_undo() {
    let mut d = docs(graph());
    let before = current(&d).clone();
    let p = edit_planner::propose(&d, request(2, Action::Remove), 1, &Catalog).unwrap();
    assert_eq!(current(&d), &before, "preview never mutates");
    let done = edit_planner::apply(&mut d, &p, 1, &Catalog).unwrap();
    assert_eq!(current(&d).edges, vec![edge(3, 4, "in"), edge(1, 3, "in")]);
    ops::undo(&mut d, &done.edit).unwrap();
    assert_eq!(current(&d), &before);
}
#[test]
fn merge_background_is_the_bypass_not_the_first_input() {
    let mut g = graph();
    g.nodes[1] = node(2, "merge");
    g.nodes.push(node(10, "fx"));
    g.edges[0].to.1 = "bg".into();
    g.edges.extend([edge(1, 10, "in"), edge(10, 2, "fg")]);
    let mut d = docs(g);
    let p = edit_planner::propose(&d, request(2, Action::Remove), 1, &Catalog).unwrap();
    edit_planner::apply(&mut d, &p, 1, &Catalog).unwrap();
    assert!(current(&d).edges.contains(&edge(1, 3, "in")));
    assert!(
        !current(&d).nodes.iter().any(|n| n.id == id(10)),
        "exclusive foreground removed"
    );
}
#[test]
fn exclusive_mask_moves_and_removes_with_its_consumer() {
    let g = masked();
    let mut d = docs(g.clone());
    let p = edit_planner::propose(&d, request(2, Action::MoveAfter(id(3))), 1, &Catalog).unwrap();
    edit_planner::apply(&mut d, &p, 1, &Catalog).unwrap();
    assert!(current(&d).edges.contains(&edge(10, 2, "mask")));
    assert!(current(&d).edges.contains(&edge(3, 2, "in")));
    assert!(current(&d).edges.contains(&edge(2, 4, "in")));
    let p = edit_planner::propose(&d, request(2, Action::Remove), 2, &Catalog).unwrap();
    edit_planner::apply(&mut d, &p, 2, &Catalog).unwrap();
    assert!(!current(&d)
        .nodes
        .iter()
        .any(|n| [id(2), id(10)].contains(&n.id)));
}
#[test]
fn shared_mask_and_nested_shared_ancestor_are_refused() {
    let mut g = masked();
    g.edges.push(edge(10, 3, "mask"));
    refused(g, request(2, Action::Remove), "This branch is shared");
    let mut g = masked();
    g.nodes.extend([node(20, "fx"), node(21, "fx")]);
    g.edges.retain(|e| e.to != (id(10), "in".into()));
    g.edges
        .extend([edge(1, 20, "in"), edge(20, 10, "in"), edge(20, 21, "in")]);
    refused(
        g,
        request(2, Action::MoveAfter(id(3))),
        "This branch is shared",
    );
}
#[test]
fn wireless_consumers_count_as_sharing() {
    let mut g = masked();
    g.nodes
        .iter_mut()
        .find(|n| n.id == id(10))
        .unwrap()
        .wireless
        .output_link = Some("mask".into());
    g.nodes[2]
        .wireless
        .input_links
        .insert("mask".into(), "mask".into());
    refused(g, request(2, Action::Remove), "This branch is shared");
}
#[test]
fn explicit_mask_producer_removal_ungates_and_names_every_reader() {
    let mut g = masked();
    g.edges.push(edge(10, 3, "mask"));
    let mut d = docs(g);
    let p = edit_planner::propose(&d, request(10, Action::Remove), 1, &Catalog).unwrap();
    assert_eq!(
        p.ungated.iter().map(|n| n.path.clone()).collect::<Vec<_>>(),
        vec![path(2), path(3)]
    );
    edit_planner::apply(&mut d, &p, 1, &Catalog).unwrap();
    assert!(current(&d).edges.iter().all(|e| e.to.1.as_str() != "mask"));
}
#[test]
fn stale_proposal_revalidates_new_sharing_even_without_revision_bump() {
    let mut d = docs(masked());
    let p = edit_planner::propose(&d, request(2, Action::Remove), 1, &Catalog).unwrap();
    d.clips
        .get_mut(&id(100))
        .unwrap()
        .graph
        .edges
        .push(edge(10, 3, "mask"));
    let before = current(&d).clone();
    assert!(edit_planner::apply(&mut d, &p, 1, &Catalog)
        .unwrap_err()
        .to_string()
        .contains("shared"));
    assert_eq!(current(&d), &before);
}
#[test]
fn unrelated_disconnected_work_is_never_collected() {
    let mut g = masked();
    g.nodes.push(node(20, "fx"));
    g.edges.push(edge(1, 20, "in"));
    let mut d = docs(g);
    let p = edit_planner::propose(&d, request(2, Action::Remove), 1, &Catalog).unwrap();
    edit_planner::apply(&mut d, &p, 1, &Catalog).unwrap();
    assert!(current(&d).nodes.iter().any(|n| n.id == id(20)));
}
#[test]
fn source_output_and_placement_are_protected() {
    for n in [1, 4] {
        refused(graph(), request(n, Action::Remove), "ends");
    }
    let mut g = graph();
    g.nodes[1] = node(2, lt::PLACEMENT);
    refused(g, request(2, Action::Remove), "Placement");
}

#[test]
fn a_published_output_is_an_external_consumer() {
    use klipp_graph_format::{GroupBody, GroupPort, PortBinding, PortDirection};
    let mut body = masked();
    body.nodes[0].type_id = lt::GROUP_INPUT.into();
    body.nodes[3].type_id = lt::GROUP_OUTPUT.into();
    let mut group = node(50, lt::GROUP);
    group.group = Some(Box::new(GroupBody {
        graph: body,
        ports: Vec::new(),
        interface: vec![GroupPort {
            key: "published_mask".into(),
            direction: PortDirection::Output,
            ty: PortType::Matte,
            binding: PortBinding {
                node: id(10),
                port: "out".into(),
            },
            label: "Mask".into(),
        }],
    }));
    let mut g = graph();
    g.nodes = vec![node(1, lt::SOURCE), group, node(4, lt::OUTPUT)];
    g.edges = vec![edge(1, 50, "in"), edge(50, 4, "in")];
    refused(
        g,
        Request {
            path: NodePath::new(root(), [id(50), id(2)]),
            action: Action::Remove,
        },
        "shared",
    );
}

#[test]
fn shared_image_output_is_an_ambiguous_reconnect() {
    let mut g = graph();
    g.nodes.push(node(20, "fx"));
    g.edges.push(edge(2, 20, "in"));
    refused(g, request(2, Action::Remove), "shared");
}

#[test]
fn cycles_are_refused_without_writing() {
    let mut g = masked();
    g.edges.retain(|e| e.to != (id(10), "in".into()));
    g.edges.push(edge(2, 10, "in"));
    refused(g, request(2, Action::Remove), "Cycle");
}

#[test]
fn move_and_undo_preserve_animation_and_layout() {
    let mut g = masked();
    g.nodes[1].anim.insert(
        "gain".into(),
        klipp_graph_format::Animation {
            keys: vec![serde_json::json!({"time_secs":0.25,"value":0.5})],
            modifiers: None,
        },
    );
    g.ui.positions.insert(id(2), serde_json::json!([10, 20]));
    let before = g.clone();
    let selected = path(2);
    let mut d = docs(g);
    let p = edit_planner::propose(&d, request(2, Action::MoveAfter(id(3))), 1, &Catalog).unwrap();
    let done = edit_planner::apply(&mut d, &p, 1, &Catalog).unwrap();
    let selected_node = klipp_graph_lift::paths::resolve(&d, &selected).unwrap();
    assert_eq!(selected_node.anim, before.nodes[1].anim);
    assert_eq!(current(&d).ui, before.ui);
    ops::undo(&mut d, &done.edit).unwrap();
    assert_eq!(current(&d), &before);
    assert_eq!(
        klipp_graph_lift::paths::resolve(&d, &selected).unwrap(),
        &before.nodes[1]
    );
}

#[test]
fn a_bypass_that_changes_port_type_is_refused() {
    let mut g = graph();
    g.nodes[1] = node(2, "convert");
    g.nodes.push(node(10, "matte"));
    g.edges[0] = edge(10, 2, "in");
    g.edges.push(edge(1, 10, "in"));
    refused(g, request(2, Action::Remove), "PortTypeMismatch");
}

#[test]
fn moving_a_dormant_wireless_sender_cannot_change_an_external_reader() {
    let mut g = graph();
    let mut dormant = node(20, "fx");
    dormant.wireless.output_link = Some("shared".into());
    g.nodes[1].wireless.output_link = Some("shared".into());
    let selected = g.nodes.remove(1);
    g.nodes.insert(2, dormant);
    g.nodes.insert(3, selected);
    let mut reader = node(30, "fx");
    reader
        .wireless
        .input_links
        .insert("in".into(), "shared".into());
    g.nodes.push(reader);
    g.edges.push(edge(1, 20, "in"));
    refused(g, request(2, Action::MoveAfter(id(3))), "shared");
}

fn boundary_graph() -> Graph {
    let mut g = graph();
    g.nodes.insert(2, node(20, lt::SCENE_TO_DISPLAY));
    g.edges.retain(|e| e.to.0 != id(3));
    g.edges.extend([edge(2, 20, "in"), edge(20, 3, "in")]);
    g
}
#[test]
fn a_move_across_the_display_boundary_requires_an_explicit_declaration() {
    let g = boundary_graph();
    for (target, after) in [(2, 3), (3, 1)] {
        let err = edit_planner::propose(
            &docs(g.clone()),
            request(target, Action::MoveAfter(id(after))),
            1,
            &Catalog,
        )
        .unwrap_err();
        assert!(err.reason.contains("scene-referred") && err.reason.contains("display-referred"));
        assert_eq!(err.reveal, path(target));
    }
    struct Dual;
    impl PortCatalog for Dual {
        fn node(&self, ty: &str) -> Option<NodeSpec> {
            Catalog.node(ty)
        }
        fn allows_both_picture_domains(&self, node: &Node) -> bool {
            node.type_id.as_str() == "fx"
        }
    }
    let mut d = docs(g);
    let p = edit_planner::propose(&d, request(2, Action::MoveAfter(id(3))), 1, &Dual).unwrap();
    edit_planner::apply(&mut d, &p, 1, &Dual).unwrap();
    assert!(current(&d).edges.contains(&edge(3, 2, "in")));
}
#[test]
fn the_display_boundary_cannot_be_removed_or_moved() {
    for action in [Action::Remove, Action::MoveAfter(id(3))] {
        refused(boundary_graph(), request(20, action), "cannot");
    }
}
