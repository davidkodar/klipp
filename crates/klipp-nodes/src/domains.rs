//! Picture colour domains (#1290): whether the picture on an edge is
//! scene-referred or display-referred, and the connections that would
//! mix the two.
//!
//! A picture starts scene-referred at its Source (relative to its level:
//! a grade's Source reads what its clip gives it) and becomes
//! display-referred at a Scene to Display node. A plug-in (OFX, frei0r)
//! declares no domain, so what comes out of one is unknown; nor does a
//! generator, whose picture is made from colours authored for it. Every other
//! node keeps the domain of the pictures it reads. A compound's inside
//! reads the domain its input carries, and gives its output's.
//!
//! Two things are refused:
//! - a Scene to Display fed by a picture that is display-referred already
//!   (the tone map would run twice), or of unknown domain (nothing says it
//!   is scene-referred);
//! - a node reading a scene-referred and a display-referred picture
//!   together (a Blend, a Layer, a merge's two pictures).
//!
//! An unknown domain mixes with either: nothing proves it wrong. Mattes
//! (a key's, a window's, an effect's mask) carry no picture and have no
//! domain. A switched-off node passes its picture through, so it keeps its
//! input's domain whatever it is.
//!
//! [`step`] is the rule, one node at a time, shared by the executor
//! (each top-level run, from the domain the renderer says its Source
//! reads, #1867) and by graph
//! editing, which applies it to a picture's nodes to refuse a connection
//! before it lands.

use std::collections::HashMap;

use crate::graph::NodeGraph;
use crate::kinds::{NodeKind, PortRole};
use crate::NodeId;

/// The colour domain of a picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PictureDomain {
    /// Scene-referred: before the display tone map.
    Scene,
    /// Display-referred: after it.
    Display,
    /// Not known: a plug-in's output, or a picture from nowhere this level
    /// can see.
    Unknown,
}

impl std::fmt::Display for PictureDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PictureDomain::Scene => "scene-referred",
            PictureDomain::Display => "display-referred",
            PictureDomain::Unknown => "of unknown domain",
        })
    }
}

/// What a node does to the domain of the pictures it reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomainStep {
    /// Gives a picture of this domain whatever it reads (a Source).
    Gives(PictureDomain),
    /// Turns a scene-referred picture into a display-referred one.
    ToDisplay,
    /// Declares no domain: what comes out is unknown (a plug-in).
    Undeclared,
    /// Keeps the domain of the pictures it reads.
    Keeps,
}

/// The domain of what a node gives, from what `step` does and the domains
/// of the pictures it reads (its primary first); why not, when the node
/// can't take them.
pub fn step(step: DomainStep, inputs: &[PictureDomain]) -> Result<PictureDomain, String> {
    if let DomainStep::Gives(d) = step {
        return Ok(d);
    }
    let known: Vec<PictureDomain> = inputs.iter().copied().filter(|d| *d != PictureDomain::Unknown).collect();
    if known.contains(&PictureDomain::Scene) && known.contains(&PictureDomain::Display) {
        return Err("it would mix a scene-referred picture with a display-referred one".into());
    }
    match step {
        DomainStep::Gives(_) => unreachable!("returned above"),
        DomainStep::ToDisplay => match inputs.first() {
            None | Some(PictureDomain::Scene) => Ok(PictureDomain::Display),
            Some(PictureDomain::Display) => Err("the picture reaching it is display-referred already: the display tone map would run twice".into()),
            Some(PictureDomain::Unknown) => Err("the picture reaching it is of unknown domain (a plug-in's), so nothing says it is scene-referred".into()),
        },
        DomainStep::Undeclared => Ok(PictureDomain::Unknown),
        DomainStep::Keeps => Ok(inputs.first().copied().filter(|d| *d != PictureDomain::Unknown).or_else(|| known.first().copied()).unwrap_or(PictureDomain::Unknown)),
    }
}

/// A node that can't take the pictures it reads, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DomainFault {
    /// The node, and the compounds it is inside, outermost first.
    pub path: Vec<NodeId>,
    pub why: String,
}

impl std::fmt::Display for DomainFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let node = self.path.last().map_or_else(String::new, |n| n.to_string());
        write!(f, "node {node}: {}", self.why)
    }
}

/// What `kind` does to the domain, switched on.
pub fn kind_step(kind: NodeKind, params: &crate::params::NodeParams) -> DomainStep {
    match kind {
        NodeKind::Source => DomainStep::Gives(PictureDomain::Scene),
        NodeKind::SceneToDisplay => DomainStep::ToDisplay,
        NodeKind::Ofx | NodeKind::Frei0r => DomainStep::Undeclared,
        // A picture handed to the run (#1746): another media item's frame,
        // not made from what the node reads.
        NodeKind::SidePicture => DomainStep::Undeclared,
        // A plug-in's picture, or a generator's, made from colours
        // authored for it: neither is the domain of what it reads.
        NodeKind::Effect | NodeKind::EffectSegment
            if params.effects.iter().any(|e| matches!(e, klipp_effects::BuiltinEffect::Frei0r { .. } | klipp_effects::BuiltinEffect::Generator { .. })) =>
        {
            DomainStep::Undeclared
        }
        _ => DomainStep::Keeps,
    }
}

/// Whether `kind`'s input `port` takes a picture, not a matte: an
/// effect's mask (its port 1) and a matte port carry no picture.
fn takes_a_picture(kind: NodeKind, port: u8) -> bool {
    if matches!(kind, NodeKind::Effect | NodeKind::EffectSegment | NodeKind::Frei0r) && port == 1 {
        return false;
    }
    kind.input_port_role(port) != PortRole::Matte
}

/// The domain of every node's output in `graph`, its Source reading
/// `source`; the first node that can't take the pictures it reads, when
/// one can't. A compound's inside is checked with the domain its input
/// carries, and its output is its inside's.
pub fn domains(graph: &NodeGraph, source: PictureDomain) -> Result<HashMap<NodeId, PictureDomain>, DomainFault> {
    domains_within(graph, source, &[])
}

fn domains_within(graph: &NodeGraph, source: PictureDomain, outer: &[NodeId]) -> Result<HashMap<NodeId, PictureDomain>, DomainFault> {
    let order = graph.topo_sort().map_err(|_| DomainFault { path: outer.to_vec(), why: "the graph loops".into() })?;
    let by_id: HashMap<NodeId, &crate::graph::Node> = graph.nodes.iter().map(|n| (n.id, n)).collect();
    let mut out: HashMap<NodeId, PictureDomain> = HashMap::new();
    for id in order {
        let Some(node) = by_id.get(&id) else { continue };
        // The pictures it reads, by port, its primary first; a matte
        // carries none.
        let mut read: Vec<(u8, PictureDomain)> = graph
            .edges
            .iter()
            .filter(|e| e.to_node == id && takes_a_picture(node.kind, e.to_port))
            .filter(|e| by_id.get(&e.from_node).is_some_and(|from| from.kind.output_port_role(e.from_port) != PortRole::Matte))
            .map(|e| (e.to_port, out.get(&e.from_node).copied().unwrap_or(PictureDomain::Unknown)))
            .collect();
        read.sort_by_key(|(port, _)| *port);
        let inputs: Vec<PictureDomain> = read.into_iter().map(|(_, d)| d).collect();
        let fault = |why: String| {
            let mut path = outer.to_vec();
            path.push(id);
            DomainFault { path, why }
        };
        let given = if node.kind == NodeKind::Source {
            source
        } else if !node.enabled {
            // Switched off, it passes its picture through as it came.
            inputs.first().copied().unwrap_or(PictureDomain::Unknown)
        } else if let (NodeKind::Compound, Some(inside)) = (node.kind, node.params.compound_subgraph.as_ref()) {
            step(DomainStep::Keeps, &inputs).map_err(fault)?;
            let mut path = outer.to_vec();
            path.push(id);
            let carried = inputs.first().copied().unwrap_or(PictureDomain::Unknown);
            let inner = domains_within(inside, carried, &path)?;
            inside.input_of(inside.output_node_id, 0).and_then(|n| inner.get(&n).copied()).unwrap_or(carried)
        } else {
            step(kind_step(node.kind, &node.params), &inputs).map_err(fault)?
        };
        out.insert(id, given);
    }
    Ok(out)
}

/// `graph` checked with its Source scene-referred, as at the head of a
/// scene-referred clip's grade (#1290): every node that runs able to take
/// the pictures it reads.
pub fn check(graph: &NodeGraph) -> Result<(), DomainFault> {
    domains(graph, PictureDomain::Scene).map(|_| ())
}

/// Whether wiring `from`'s output `from_port` into `to`'s input `to_port`
/// leaves `graph` able to run, its Source reading `source`: the graph
/// itself is never changed. A graph that couldn't run before is not this
/// connection's fault, and is not refused for it.
pub fn check_connection(graph: &NodeGraph, source: PictureDomain, from: NodeId, from_port: u8, to: NodeId, to_port: u8) -> Result<(), DomainFault> {
    if domains(graph, source).is_err() {
        return Ok(());
    }
    let mut wired = graph.clone();
    wired.edges.retain(|e| !(e.to_node == to && e.to_port == to_port));
    wired.edges.push(crate::graph::Edge { from_node: from, from_port, to_node: to, to_port });
    domains(&wired, source).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Edge, Node};
    use crate::params::NodeParams;
    use PictureDomain::*;

    fn node(kind: NodeKind) -> Node {
        let params = match kind {
            NodeKind::Exposure => NodeParams { exposure: Some(0.5), ..Default::default() },
            _ => NodeParams::default(),
        };
        Node { id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
    }

    fn edge(from: &Node, to: &Node, to_port: u8) -> Edge {
        Edge { from_node: from.id, from_port: 0, to_node: to.id, to_port }
    }

    /// Source -> each of `kinds` -> Output, and the nodes in order.
    fn chain(kinds: &[NodeKind]) -> (NodeGraph, Vec<Node>) {
        let mut nodes = vec![node(NodeKind::Source)];
        nodes.extend(kinds.iter().map(|k| node(*k)));
        nodes.push(node(NodeKind::Output));
        let edges = nodes.windows(2).map(|w| edge(&w[0], &w[1], 0)).collect();
        let g = NodeGraph { output_node_id: nodes.last().unwrap().id, nodes: nodes.clone(), edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] };
        (g, nodes)
    }

    #[test]
    fn the_rule_one_node_at_a_time() {
        assert_eq!(step(DomainStep::ToDisplay, &[Scene]), Ok(Display));
        assert!(step(DomainStep::ToDisplay, &[Display]).unwrap_err().contains("twice"));
        assert!(step(DomainStep::ToDisplay, &[Unknown]).unwrap_err().contains("unknown domain"));
        assert_eq!(step(DomainStep::Keeps, &[Display]), Ok(Display));
        assert!(step(DomainStep::Keeps, &[Scene, Display]).unwrap_err().contains("mix"));
        // Unknown mixes with either, and a known one names the result.
        assert_eq!(step(DomainStep::Keeps, &[Unknown, Display]), Ok(Display));
        assert_eq!(step(DomainStep::Keeps, &[Scene, Unknown]), Ok(Scene));
        assert_eq!(step(DomainStep::Undeclared, &[Scene]), Ok(Unknown));
        assert!(step(DomainStep::Undeclared, &[Scene, Display]).is_err(), "a plug-in reading both still mixes them");
        assert_eq!(step(DomainStep::Gives(Scene), &[Display]), Ok(Scene));
    }

    /// A grade, its display step, effects after it: scene-referred up to the
    /// step, display-referred after; a second step after the first is
    /// refused, and a switched-off one passes the picture through.
    #[test]
    fn a_scene_to_display_turns_the_picture_once() {
        let (g, n) = chain(&[NodeKind::Exposure, NodeKind::SceneToDisplay, NodeKind::Saturation]);
        let d = domains(&g, Scene).unwrap();
        assert_eq!((d[&n[1].id], d[&n[2].id], d[&n[3].id], d[&n[4].id]), (Scene, Display, Display, Display));
        let (g, n) = chain(&[NodeKind::SceneToDisplay, NodeKind::SceneToDisplay]);
        let fault = check(&g).unwrap_err();
        assert_eq!(fault.path, vec![n[2].id]);
        assert!(fault.why.contains("twice"), "{fault}");
        // Control: the second switched off passes the picture through.
        let (mut g, _) = chain(&[NodeKind::SceneToDisplay, NodeKind::SceneToDisplay]);
        g.nodes[2].enabled = false;
        assert_eq!(check(&g), Ok(()));
        // A Source of a display-referred picture (a timeline grade's)
        // takes no display step.
        let (g, _) = chain(&[NodeKind::SceneToDisplay]);
        assert!(domains(&g, Display).is_err());
    }

    /// A plug-in declares no domain: after one, a display step can't tell
    /// the picture is scene-referred; anything else may follow it.
    #[test]
    fn a_plugins_domain_is_unknown() {
        let (g, n) = chain(&[NodeKind::Ofx, NodeKind::SceneToDisplay]);
        let fault = check(&g).unwrap_err();
        assert_eq!(fault.path, vec![n[2].id]);
        assert!(fault.why.contains("unknown domain"), "{fault}");
        let (g, n) = chain(&[NodeKind::SceneToDisplay, NodeKind::Ofx, NodeKind::Exposure]);
        let d = domains(&g, Scene).unwrap();
        assert_eq!((d[&n[2].id], d[&n[3].id]), (Unknown, Unknown));
        // A frei0r effect node is a plug-in too.
        let (mut g, n) = chain(&[NodeKind::Effect, NodeKind::SceneToDisplay]);
        g.nodes[1].params.effects = vec![klipp_effects::BuiltinEffect::Frei0r { type_id: "frei0r.test".into(), params: vec![] }];
        assert_eq!(check(&g).unwrap_err().path, vec![n[2].id]);
        // A generator's picture is made from colours authored for it: laid
        // over the display-referred picture by a Blend, it mixes nothing.
        let (mut g, n) = chain(&[NodeKind::SceneToDisplay, NodeKind::Blend]);
        let mut generator = node(NodeKind::Effect);
        generator.params.effects = vec![klipp_effects::BuiltinEffect::Generator { shape: 0, color_a: [0, 0, 255, 255], color_b: [255; 4], angle: 0.0, center_x: 0.5, center_y: 0.5, radius: 0.5 }];
        g.edges.push(edge(&n[0], &generator, 0));
        g.edges.push(edge(&generator, &n[2], 1));
        g.nodes.push(generator);
        assert_eq!(check(&g), Ok(()));
        // Control: a built-in effect keeps the domain.
        let (mut g, _) = chain(&[NodeKind::Effect, NodeKind::SceneToDisplay]);
        g.nodes[1].params.effects = vec![klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 }];
        assert_eq!(check(&g), Ok(()));
    }

    /// A Blend reading the picture before the display step and after it
    /// mixes the two and is refused; a matte read beside a picture has no
    /// domain to mix.
    #[test]
    fn a_blend_of_both_domains_is_refused_and_a_matte_has_none() {
        let (mut g, n) = chain(&[NodeKind::SceneToDisplay, NodeKind::Blend]);
        g.edges.push(edge(&n[0], &n[2], 1));
        let fault = check(&g).unwrap_err();
        assert_eq!(fault.path, vec![n[2].id]);
        assert!(fault.why.contains("mix"), "{fault}");
        // Control: both of its pictures after the display step.
        let (mut g, n) = chain(&[NodeKind::SceneToDisplay, NodeKind::Blend]);
        g.edges.push(edge(&n[1], &n[2], 1));
        assert_eq!(check(&g), Ok(()));
        // A key on the scene-referred picture masking an effect after the
        // display step: the mask is a matte, not a picture.
        let (mut g, n) = chain(&[NodeKind::SceneToDisplay, NodeKind::Effect]);
        g.nodes[2].params.effects = vec![klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 }];
        let key = node(NodeKind::LumaKey);
        g.edges.push(edge(&n[0], &key, 0));
        g.edges.push(edge(&key, &n[2], 1));
        g.nodes.push(key);
        assert_eq!(check(&g), Ok(()));
    }

    /// A compound's inside reads the domain its input carries: a display
    /// step inside one after the picture's own is refused there, at its
    /// path; its output gives its inside's domain on.
    #[test]
    fn a_compound_is_checked_with_the_domain_it_reads() {
        let (inside, inner) = chain(&[NodeKind::SceneToDisplay]);
        let (mut g, n) = chain(&[NodeKind::SceneToDisplay, NodeKind::Compound]);
        g.nodes[2].params.compound_subgraph = Some(inside.clone());
        let fault = check(&g).unwrap_err();
        assert_eq!(fault.path, vec![n[2].id, inner[1].id], "the fault at its path through the compound");
        // Control: before the picture's own display step, it turns it.
        let (mut g, n) = chain(&[NodeKind::Compound, NodeKind::Blend]);
        g.nodes[1].params.compound_subgraph = Some(inside);
        g.edges.push(edge(&n[0], &n[2], 1));
        let fault = check(&g).unwrap_err();
        assert_eq!(fault.path, vec![n[2].id], "the compound's output is display-referred, mixed with the source");
    }

    /// The executor refuses to run a graph a node of which can't take
    /// the pictures it reads, rather than draw it wrong; the same graph
    /// with the second step off runs.
    #[test]
    fn the_executor_refuses_a_graph_that_mixes_domains() {
        let step = || Some(klipp_color::hdr::SceneToDisplay { transfer: klipp_color::hdr::HdrTransfer::Pq, peak_nits: None, luma: [0.2126, 0.7152, 0.0722] });
        let (mut g, n) = chain(&[NodeKind::SceneToDisplay, NodeKind::SceneToDisplay]);
        for node in g.nodes.iter_mut().filter(|n| n.kind == NodeKind::SceneToDisplay) {
            node.params.scene_to_display = step();
        }
        let frame = [0.5f32, 0.5, 0.5, 1.0].repeat(4);
        let mut ex = crate::executor::GraphExecutor::new_cpu_only();
        // A scene-referred clip's grade (#1867): its Source reads scene light.
        ex.set_source_domain(PictureDomain::Scene);
        match ex.execute_linear(&frame, &g, 2, 2, 0.0, 0, None, None) {
            Err(crate::executor::ExecutorError::ColourDomain(fault)) => assert_eq!(fault.path, vec![n[2].id]),
            other => panic!("{:?}", other.map(|_| ())),
        }
        // The refusal is a failing decline naming the node and why (#1867).
        let d = ex.last_run_declines();
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].node == n[2].id && d[0].kind == NodeKind::SceneToDisplay && d[0].outcome.fails() && d[0].why.contains("display-referred already"), "{:?}", d[0]);
        g.nodes[2].enabled = false;
        assert!(ex.execute_linear(&frame, &g, 2, 2, 0.0, 0, None, None).is_ok(), "the second step off, it runs");
        assert!(ex.last_run_declines().is_empty(), "a run that runs leaves the refusal behind");
    }

    /// #1867: a run's Source is display-referred unless the renderer says
    /// it reads a scene-referred clip's picture: an SDR clip's picture is
    /// display-referred, so a display-referred picture mixes with it, and a
    /// display step in its run would turn it twice. Told it is
    /// scene-referred (HDR or log, before its display step), the step runs
    /// and a display-referred picture mixed before it is refused.
    #[test]
    fn a_runs_source_is_display_referred_unless_said_scene_referred() {
        let step = || Some(klipp_color::hdr::SceneToDisplay { transfer: klipp_color::hdr::HdrTransfer::Pq, peak_nits: None, luma: [0.2126, 0.7152, 0.0722] });
        let frame = [0.5f32, 0.5, 0.5, 1.0].repeat(4);
        let mut ex = crate::executor::GraphExecutor::new_cpu_only();
        // Source -> display step -> Blend, the Blend's second picture the
        // Source's own: the source mixed with its displayed self.
        let (mut g, n) = chain(&[NodeKind::SceneToDisplay, NodeKind::Blend]);
        g.nodes[1].params.scene_to_display = step();
        g.edges.push(edge(&n[0], &n[2], 1));
        // SDR, the default: the display step reads a display-referred
        // picture, refused there.
        match ex.execute_linear(&frame, &g, 2, 2, 0.0, 0, None, None) {
            Err(crate::executor::ExecutorError::ColourDomain(fault)) => assert_eq!(fault.path, vec![n[1].id], "the display step"),
            other => panic!("{:?}", other.map(|_| ())),
        }
        // Scene-referred: the step runs, and the Blend mixing scene and
        // display light is refused.
        ex.set_source_domain(PictureDomain::Scene);
        match ex.execute_linear(&frame, &g, 2, 2, 0.0, 0, None, None) {
            Err(crate::executor::ExecutorError::ColourDomain(fault)) => assert_eq!(fault.path, vec![n[2].id], "the Blend"),
            other => panic!("{:?}", other.map(|_| ())),
        }
        // An SDR picture with no display step of its own mixes with a
        // display-referred one: every picture of its run is display-referred.
        let (mut g, n) = chain(&[NodeKind::Exposure, NodeKind::Blend]);
        g.edges.push(edge(&n[0], &n[2], 1));
        let d = domains(&g, Display).unwrap();
        assert_eq!((d[&n[0].id], d[&n[1].id], d[&n[2].id]), (Display, Display, Display));
        ex.set_source_domain(PictureDomain::Display);
        assert!(ex.execute_linear(&frame, &g, 2, 2, 0.0, 0, None, None).is_ok());
    }

    /// A connection is checked on a copy: refused when it would make the
    /// graph unable to run, the graph left as it was; one that leaves it
    /// able to run is allowed.
    #[test]
    fn a_connection_is_refused_without_changing_the_graph() {
        let (mut g, n) = chain(&[NodeKind::SceneToDisplay, NodeKind::Blend]);
        g.edges.push(edge(&n[1], &n[2], 1));
        let before = g.clone();
        let refused = check_connection(&g, Scene, n[0].id, 0, n[2].id, 1).unwrap_err();
        assert!(refused.why.contains("mix"), "{refused}");
        assert_eq!(g, before, "the graph unchanged");
        assert_eq!(check_connection(&g, Scene, n[1].id, 0, n[2].id, 1), Ok(()));
        // A graph that couldn't run already doesn't refuse a connection
        // for it.
        let (bad, m) = chain(&[NodeKind::SceneToDisplay, NodeKind::SceneToDisplay, NodeKind::Exposure]);
        assert_eq!(check_connection(&bad, Scene, m[0].id, 0, m[3].id, 0), Ok(()));
    }
}
