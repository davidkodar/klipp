//! Inspection by path (M3 step 16a): what the grade tools ask of a clip's
//! picture besides the picture (a node's thumbnail, the scope's tap, a
//! soloed node, a matte) named by the node's path in the clip's picture
//! and its output port, and resolved to the node the renderer runs.
//!
//! A path names a node of the clip's own grade group, of a stage of its
//! grade group, of its track's grade ([`klipp_timeline::Sequence::grade_node_at`],
//! which knows where the bakes put each), or one of its effects (whose
//! spliced node takes the effect's node id). A request on an output that
//! is a matte (a qualifier's, a mask's) shows the matte. A path that names
//! nothing to inspect is unavailable, with why, and is never taken for
//! another node or for the output.

use std::collections::{HashMap, HashSet};

use klipp_core::ClipId;
use klipp_graph_format::path::{NodePath, Root};
use klipp_nodes::NodeId;
use klipp_timeline::{GradeAccess, Sequence};

/// A node's output to inspect: the node `path` names in the inspected
/// clip's picture, at its output `port`.
pub use klipp_graph_format::path::NodeOutput as InspectAt;

/// The output of a node of `clip`'s own grade, by its id there: what the
/// grade tools hold of the grade they edit.
pub fn grade_node(clip: ClipId, node: NodeId) -> InspectAt {
    let group = klipp_graph_lift::ids::entry_id(klipp_graph_lift::ids::RootKind::Clip, clip.0, "grade", 0);
    InspectAt::out(NodePath::new(Root::Clip { clip: clip.0 }, [group, node]))
}

/// An inspection resolved to the nodes the renderer runs.
#[derive(Default, Clone)]
pub(crate) struct Resolved {
    pub thumbs: Option<HashSet<NodeId>>,
    /// Which requests each thumbnail answers.
    pub thumb_of: HashMap<NodeId, Vec<InspectAt>>,
    pub scope_tap: Option<NodeId>,
    pub matte_view: Option<NodeId>,
    pub solo: Option<NodeId>,
    /// Each resolved request by the node it reads, to say which ones the
    /// run didn't hold.
    pub asked: Vec<(NodeId, InspectAt)>,
}

/// The node `at` names in `clip`'s picture as `seq` renders it, and
/// whether its output is a matte; why not, when it names none.
fn node_of(seq: &Sequence, clip: ClipId, at: &InspectAt) -> Result<(NodeId, bool), String> {
    // The timeline grade, inspected through its proxy (the clip its
    // composite is graded as, `Clip::timeline_grade_of`): a node of it by
    // its own id, as the proxy runs the grade unchanged (#1428).
    if let Root::TimelineGrade { sequence } = at.path.root {
        if clip != seq.timeline_grade_clip_id() || sequence != seq.id.0 {
            return Err("the timeline grade is read through its own proxy, not a clip".into());
        }
        let [node] = at.path.nodes.as_slice() else {
            return Err("a node inside a compound of the timeline grade isn't inspected yet, only one in a clip's own grade (#1475)".into());
        };
        let kind = seq.timeline_grade().and_then(|g| g.nodes.iter().find(|n| n.id == *node)).map(|n| n.kind).ok_or_else(|| format!("{node} isn't in the timeline grade"))?;
        let name = serde_json::to_value(kind).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
        let index = klipp_graph_format::ports::output_index(&name, &at.port).ok_or_else(|| format!("a {name} has no output {}", at.port))?;
        return Ok((*node, kind.output_port_role(index) == klipp_nodes::kinds::PortRole::Matte));
    }
    match seq.grade_node_at(clip, &at.path) {
        Ok(node) => {
            // The grade the clip runs: its own on a render copy, else the
            // bakes replayed for it.
            // At any depth of its compounds (#1475).
            fn kind_in(g: &klipp_nodes::NodeGraph, id: NodeId) -> Option<klipp_nodes::NodeKind> {
                g.nodes.iter().find_map(|n| if n.id == id { Some(n.kind) } else { n.params.compound_subgraph.as_ref().and_then(|sub| kind_in(sub, id)) })
            }
            let running = seq.all_clips().find(|x| x.id == clip).and_then(|x| kind_in(x.grade(), node));
            let kind = running
                .or_else(|| seq.render_grade(clip).and_then(|g| kind_in(&g, node)))
                .ok_or_else(|| format!("{node} doesn't run"))?;
            let name = serde_json::to_value(kind).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
            let index = klipp_graph_format::ports::output_index(&name, &at.port).ok_or_else(|| format!("a {name} has no output {}", at.port))?;
            Ok((node, kind.output_port_role(index) == klipp_nodes::kinds::PortRole::Matte))
        }
        Err(why) => match (at.path.root, at.path.nodes.as_slice()) {
            // One of the clip's effects: its spliced node takes its id.
            (Root::Clip { clip: c }, [node]) if c == clip.0 => {
                let picture = seq.all_clips().find(|x| x.id == clip).and_then(|x| x.picture_v2().ok());
                let kind = picture.as_ref().and_then(|p| p.picture.graph.nodes.iter().find(|n| n.id == *node).map(|n| n.type_id.as_str().to_owned()));
                match kind.as_deref() {
                    Some(klipp_graph_format::types::LEGACY_EFFECT) if at.port.as_str() == "out" => Ok((*node, false)),
                    Some(klipp_graph_format::types::LEGACY_EFFECT) => Err(format!("an effect has no output {}", at.port)),
                    Some(klipp_graph_format::types::LEGACY_OFX) => Err("a plug-in effect runs outside the grade and can't be inspected yet".into()),
                    // They read neighbouring source frames before the
                    // grade runs, outside the plan (#1305).
                    Some(klipp_graph_format::lifted::types::TEMPORAL_DENOISE | klipp_graph_format::lifted::types::FRAME_BLEND) => {
                        Err("a temporal denoise or a frame blend runs before the grade, outside the plan, so it can't be inspected yet (#1305)".into())
                    }
                    _ => Err(why),
                }
            }
            _ => Err(why),
        },
    }
}

/// `inspect`'s requests resolved on `seq`, the sequence being rendered;
/// each that names nothing to inspect goes to `unavailable` with why.
pub(crate) fn resolve(seq: &Sequence, inspect: &crate::render::Inspect, unavailable: &mut Vec<(InspectAt, String)>) -> Resolved {
    let mut r = Resolved::default();
    let Some(clip) = inspect.clip else {
        for at in inspect.thumbs.iter().flatten().chain(&inspect.scope_tap).chain(&inspect.matte_view).chain(&inspect.solo) {
            unavailable.push((at.clone(), "no clip is inspected".into()));
        }
        return r;
    };
    let mut one = |at: &InspectAt, unavailable: &mut Vec<(InspectAt, String)>| match node_of(seq, clip, at) {
        Ok(found) => {
            r.asked.push((found.0, at.clone()));
            Some(found)
        }
        Err(why) => {
            unavailable.push((at.clone(), why));
            None
        }
    };
    let mut thumbs = HashSet::new();
    let mut thumb_of: HashMap<NodeId, Vec<InspectAt>> = HashMap::new();
    for at in inspect.thumbs.iter().flatten() {
        match one(at, unavailable) {
            Some((_, true)) => unavailable.push((at.clone(), "a matte's thumbnail isn't drawn yet".into())),
            Some((node, false)) => {
                thumbs.insert(node);
                thumb_of.entry(node).or_default().push(at.clone());
            }
            None => {}
        }
    }
    // A matte output soloed or tapped shows its matte.
    let scope_tap = inspect.scope_tap.as_ref().and_then(|at| one(at, unavailable));
    let solo = inspect.solo.as_ref().and_then(|at| one(at, unavailable));
    let matte_view = inspect.matte_view.as_ref().and_then(|at| one(at, unavailable)).map(|(n, _)| n);
    let matte_of = |found: Option<(NodeId, bool)>| found.filter(|(_, matte)| *matte).map(|(n, _)| n);
    r.matte_view = matte_view.or(matte_of(solo)).or(matte_of(scope_tap));
    r.solo = solo.filter(|(_, matte)| !matte).map(|(n, _)| n);
    r.scope_tap = scope_tap.filter(|(_, matte)| !matte).map(|(n, _)| n);
    r.thumbs = inspect.thumbs.as_ref().map(|_| thumbs);
    r.thumb_of = thumb_of;
    r
}
