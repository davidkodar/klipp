//! #1290: an edit that would leave a node unable to take the pictures it
//! reads is refused, the picture left as it was: a picture after its
//! display step merged with one before it, a display step fed after a
//! plug-in, a second display step after the first. An edit that keeps
//! every node able to is applied, and a picture that was already wrong
//! isn't refused an unrelated edit for it.

use klipp_graph_format::{
    lifted::{types as lt, ClipPicture},
    path::{NodePath, Root},
    Edge, Graph, Node, NodeId, NodeSpec, PortCatalog, PortSpec, PortType,
};
use klipp_graph_lift::{ops, ops::GraphOp, paths::Documents};
use klipp_nodes::domains::PictureDomain;

struct Catalog;
impl PortCatalog for Catalog {
    fn node(&self, ty: &str) -> Option<NodeSpec> {
        let p = |key: &str, ty, optional| PortSpec { key: key.into(), ty, optional, params: Vec::new() };
        let (inputs, output) = match ty {
            "fx" => (vec![p("in", PortType::Image, false), p("mask", PortType::Matte, true)], PortType::Image),
            "matte" => (vec![p("in", PortType::Image, false)], PortType::Matte),
            "merge" => (vec![p("in", PortType::Image, false), p("fg", PortType::Image, true)], PortType::Image),
            t if t == lt::SCENE_TO_DISPLAY || t == lt::OFX => (vec![p("in", PortType::Image, false)], PortType::Image),
            _ => return None,
        };
        Some(NodeSpec { inputs, outputs: vec![p("out", output, false)], mixable: false })
    }
}

fn id(n: u128) -> NodeId {
    NodeId::from_u128(n)
}
fn at(n: u128) -> NodePath {
    NodePath::new(Root::Clip { clip: id(100) }, [id(n)])
}
fn node(n: u128, ty: &str) -> Node {
    serde_json::from_value(serde_json::json!({"id": id(n), "type_id": ty, "type_version": 1, "label": format!("node {n}"), "enabled": true})).unwrap()
}
fn edge(from: u128, to: u128, port: &str) -> Edge {
    Edge { from: (id(from), "out".into()), to: (id(to), port.into()) }
}

/// Source(1) -> fx(2) -> display step(3) -> plug-in(4) -> merge(5) ->
/// Output(6), and a key (7) off the source beside it.
fn graph() -> Graph {
    Graph {
        nodes: vec![node(1, lt::SOURCE), node(2, "fx"), node(3, lt::SCENE_TO_DISPLAY), node(4, lt::OFX), node(5, "merge"), node(6, lt::OUTPUT), node(7, "matte")],
        edges: vec![edge(1, 2, "in"), edge(2, 3, "in"), edge(3, 4, "in"), edge(4, 5, "in"), edge(5, 6, "in"), edge(1, 7, "in")],
        output: id(6),
        ..Graph::empty()
    }
}
fn docs(g: Graph) -> Documents {
    let p: ClipPicture = serde_json::from_value(serde_json::json!({"graph": g, "opacity": 1.0})).unwrap();
    let mut docs = Documents::default();
    docs.clips.insert(id(100), p);
    docs
}
fn connect(from: u128, to: u128, port: &str) -> GraphOp {
    GraphOp::Connect { from: at(from), from_port: "out".into(), to: at(to), to_port: port.into() }
}

fn refused(g: Graph, op: GraphOp, why: &str) {
    let mut d = docs(g.clone());
    let err = ops::apply_with_catalog(&mut d, &op, &Catalog).unwrap_err();
    assert!(err.contains("unable to take the pictures it reads") && err.contains(why), "{err}");
    assert_eq!(d.clips[&id(100)].graph, g, "the picture left as it was");
}

#[test]
fn the_picture_as_lifted_has_a_domain_on_every_edge() {
    assert_eq!(klipp_graph_lift::domains::fault(&graph(), PictureDomain::Scene, &Catalog), None);
}

#[test]
fn a_merge_of_both_domains_is_refused_and_one_of_either_side_allowed() {
    // The source (scene-referred) into the merge after the display step
    // and the plug-in: the plug-in's domain is unknown, so this alone
    // mixes nothing; a merge reading the display step itself mixes.
    let mut d = docs(graph());
    ops::apply_with_catalog(&mut d, &connect(1, 5, "fg"), &Catalog).expect("unknown mixes with either");
    let mut g = graph();
    g.edges.retain(|e| e.to != (id(5), "in".into()));
    g.edges.push(edge(3, 5, "in"));
    refused(g.clone(), connect(1, 5, "fg"), "mix a scene-referred picture with a display-referred one");
    // Control: the fx before the display step is scene-referred too, so
    // feeding the display step's picture is the one allowed.
    let mut d = docs(g);
    ops::apply_with_catalog(&mut d, &connect(3, 5, "fg"), &Catalog).expect("both display-referred");
}

#[test]
fn a_display_step_after_a_plugin_or_a_second_one_is_refused() {
    // A plug-in (10) and a display step (11) beside the chain, the step
    // reading the scene-referred fx.
    let mut beside = graph();
    beside.nodes.extend([node(10, lt::OFX), node(11, lt::SCENE_TO_DISPLAY)]);
    beside.edges.extend([edge(2, 10, "in"), edge(2, 11, "in")]);
    refused(beside, connect(10, 11, "in"), "unknown domain");
    let mut g = graph();
    g.nodes.push(node(8, lt::SCENE_TO_DISPLAY));
    g.edges.push(edge(2, 8, "in"));
    refused(g.clone(), connect(3, 8, "in"), "twice");
    // Control: fed the scene-referred fx, as it was, it's allowed.
    let mut d = docs(g);
    ops::apply_with_catalog(&mut d, &connect(1, 8, "in"), &Catalog).expect("the source is scene-referred");
}

#[test]
fn a_mask_carries_no_domain() {
    // The key off the scene-referred source masks the merge's input fx...
    let mut g = graph();
    g.nodes.push(node(9, "fx"));
    g.edges.retain(|e| e.to != (id(5), "in".into()));
    g.edges.extend([edge(3, 9, "in"), edge(9, 5, "in")]);
    let mut d = docs(g);
    ops::apply_with_catalog(&mut d, &connect(7, 9, "mask"), &Catalog).expect("a matte is no picture");
}

#[test]
fn a_picture_wrong_already_is_not_refused_an_unrelated_edit() {
    let mut g = graph();
    g.nodes.push(node(8, lt::SCENE_TO_DISPLAY));
    g.edges.push(edge(3, 8, "in"));
    assert!(klipp_graph_lift::domains::fault(&g, PictureDomain::Scene, &Catalog).is_some());
    let mut d = docs(g);
    ops::apply_with_catalog(&mut d, &connect(1, 5, "fg"), &Catalog).expect("not this edit's fault");
}

/// Source(1) -> fx(2) -> merge(5) -> Output(6), the merge reading the fx
/// on both of its pictures.
fn merged_twice() -> Graph {
    Graph {
        nodes: vec![node(1, lt::SOURCE), node(2, "fx"), node(5, "merge"), node(6, lt::OUTPUT)],
        edges: vec![edge(1, 2, "in"), edge(2, 5, "in"), edge(2, 5, "fg"), edge(5, 6, "in")],
        output: id(6),
        ..Graph::empty()
    }
}

/// A display step (8) inserted between the fx and the merge, by a batch
/// that rewires the merge's two pictures one at a time: half way, the
/// merge reads one before the step and one after it. A batch is checked
/// whole at its end, so it lands; the same half-way op alone is refused,
/// and a batch that ends half way is refused, the picture as it was.
#[test]
fn an_insert_between_rewires_through_a_mixed_state_in_one_batch() {
    let insert = |whole: bool| {
        let mut ops = vec![
            GraphOp::AddNodeBeside { at: at(2), node: Box::new(node(8, lt::SCENE_TO_DISPLAY)) },
            connect(2, 8, "in"),
            connect(8, 5, "in"),
        ];
        if whole {
            ops.push(connect(8, 5, "fg"));
        }
        ops
    };
    let mut d = docs(merged_twice());
    ops::apply_batch_with_catalog(&mut d, &insert(true), &Catalog).expect("the insert lands whole");
    let g = &d.clips[&id(100)].graph;
    assert!(g.edges.contains(&edge(8, 5, "in")) && g.edges.contains(&edge(8, 5, "fg")) && g.edges.contains(&edge(2, 8, "in")));
    assert_eq!(klipp_graph_lift::domains::fault(g, PictureDomain::Scene, &Catalog), None);

    // Control: the half-way op on its own is refused...
    let mut half = merged_twice();
    half.nodes.push(node(8, lt::SCENE_TO_DISPLAY));
    half.edges.push(edge(2, 8, "in"));
    refused(half, connect(8, 5, "in"), "mix");
    // ...and so is a batch that ends half way, the picture left as it was.
    let mut d = docs(merged_twice());
    let err = ops::apply_batch_with_catalog(&mut d, &insert(false), &Catalog).unwrap_err();
    assert!(err.contains("unable to take the pictures it reads"), "{err}");
    assert_eq!(d.clips[&id(100)].graph, merged_twice());
    // The same through the op-by-op batch the app and the socket use.
    let mut d = docs(merged_twice());
    let start = d.clone();
    for op in insert(false) {
        let pending = [id(8)];
        ops::apply_in_batch(&mut d, &op, &Catalog, &pending).expect("an op of a batch isn't checked alone");
    }
    let err = ops::check_batch_end(&d, &start, Root::Clip { clip: id(100) }, &Catalog).unwrap_err();
    assert!(err.why.contains("unable to take the pictures it reads"), "{err}");
}

/// A reroute (#1746) carries the picture it reads, its domain too: one
/// between the grade and the display step feeds the step a scene-referred
/// picture, which it takes. Read with the real catalog, whose reroute
/// entry makes it keep its input's domain.
#[test]
fn a_reroute_before_the_display_step_keeps_its_pictures_domain() {
    let reroute: Node = serde_json::from_value(serde_json::json!({
        "id": id(8), "type_id": "klipp.legacy.node", "type_version": 1, "label": "", "enabled": true,
        "legacy": { "payload": { "kind": "Reroute", "label": "Reroute", "params": {} } },
    }))
    .unwrap();
    let g = Graph {
        nodes: vec![node(1, lt::SOURCE), reroute, node(3, lt::SCENE_TO_DISPLAY), node(6, lt::OUTPUT)],
        edges: vec![edge(1, 8, "in"), edge(8, 3, "in"), edge(3, 6, "in")],
        output: id(6),
        ..Graph::empty()
    };
    assert_eq!(klipp_graph_lift::domains::fault(&g, PictureDomain::Scene, &klipp_graph_lift::registry_catalog::RegistryCatalog), None);
}

/// An effect node for `e`, as the Picture view makes one.
fn effect_node(n: u128, e: &klipp_effects::BuiltinEffect) -> Node {
    let payload = serde_json::to_value(e).unwrap();
    serde_json::from_value(serde_json::json!({
        "id": id(n), "type_id": klipp_graph_lift::names::effect_node_type(payload["type"].as_str().unwrap()), "type_version": 1, "label": "", "enabled": true,
        "legacy": { "payload": payload },
    }))
    .unwrap()
}

/// A media source's frame is display-referred, an HDR or log item's given
/// its display step as it is read (#1857): wired into a merge after the
/// picture's display step it is taken, into one before it (scene light)
/// refused. Control: a generator declares no domain, so either merge
/// takes it.
#[test]
fn a_media_source_is_display_referred_and_refused_before_the_display_step() {
    use klipp_effects::BuiltinEffect as E;
    // Source(1) -> merge(9) -> fx(2) -> display step(3) -> merge(5) ->
    // Output(6), the media source (8) and a generator (10) beside, each
    // reading the source as the Picture view wires one.
    let media = effect_node(8, &E::MediaSource { media: "6f1d1c34-5b0e-4c55-9a39-0d3b3c7b2a10".into(), offset: 0.0 });
    let generator = effect_node(10, &klipp_effects::registry::effect_type("generator").map(|t| (t.sample)()).unwrap());
    let g = Graph {
        nodes: vec![node(1, lt::SOURCE), node(9, "merge"), node(2, "fx"), node(3, lt::SCENE_TO_DISPLAY), node(5, "merge"), node(6, lt::OUTPUT), media, generator],
        edges: vec![edge(1, 9, "in"), edge(9, 2, "in"), edge(2, 3, "in"), edge(3, 5, "in"), edge(5, 6, "in"), edge(1, 8, "in"), edge(1, 10, "in")],
        output: id(6),
        ..Graph::empty()
    };
    let mut d = docs(g.clone());
    ops::apply_with_catalog(&mut d, &connect(8, 5, "fg"), &Catalog).expect("display-referred with display-referred");
    refused(g.clone(), connect(8, 9, "fg"), "mix a scene-referred picture with a display-referred one");
    for to in [9, 5] {
        let mut d = docs(g.clone());
        ops::apply_with_catalog(&mut d, &connect(10, to, "fg"), &Catalog).expect("a generator mixes with either");
    }
}
