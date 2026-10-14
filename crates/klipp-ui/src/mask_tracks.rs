//! A clip's mask keys as tracks (#1947): every keyed parameter of every
//! mask on the clip's grade, its Mask nodes inside groups and compounds
//! too, named after the mask, for the curve editor's list and the
//! timeline's key lane.

use klipp_core::{Keyframe, Mask, MaskStack};
use klipp_nodes::{NodeGraph, NodeId, NodeKind};
use klipp_timeline::{Clip, GradeAccess};

/// One mask's key track: its Mask node, its place in the node's stack,
/// and the parameter ("radius", "feather", …, or "shape" for a freeform
/// mask's shape keys).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MaskTrack {
    pub node: NodeId,
    pub mask: usize,
    pub param: String,
}

impl MaskTrack {
    pub fn is_shape(&self) -> bool {
        self.param == Mask::SHAPE_TRACK
    }
}

/// Every Mask node of `graph`, inside its compounds too, with its masks.
fn mask_nodes(graph: &NodeGraph, out: &mut Vec<(NodeId, MaskStack)>) {
    for n in &graph.nodes {
        if n.kind == NodeKind::Mask {
            if let Some(m) = &n.params.masks {
                out.push((n.id, m.clone()));
            }
        }
        if let Some(sub) = &n.params.compound_subgraph {
            mask_nodes(sub, out);
        }
    }
}

fn stacks(clip: &Clip) -> Vec<(NodeId, MaskStack)> {
    let mut out = Vec::new();
    mask_nodes(clip.grade(), &mut out);
    out
}

/// The mask a track names.
pub fn mask_of(clip: &Clip, track: &MaskTrack) -> Option<Mask> {
    stacks(clip).into_iter().find(|(id, _)| *id == track.node)?.1.masks.get(track.mask).cloned()
}

/// A parameter's name in a list: "center_x" is "Center x".
pub fn param_label(param: &str) -> String {
    let words = param.replace('_', " ");
    let mut c = words.chars();
    c.next().map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
}

/// A mask's name in a list: its own, else "Mask n".
fn mask_name(mask: &Mask, index: usize) -> String {
    if mask.name.trim().is_empty() { format!("Mask {}", index + 1) } else { mask.name.trim().to_owned() }
}

/// A track's name: "Face · Shape", "Mask 2 · Feather".
pub fn name(clip: &Clip, track: &MaskTrack) -> String {
    let mask = mask_of(clip, track).map_or_else(|| format!("Mask {}", track.mask + 1), |m| mask_name(&m, track.mask));
    format!("{mask} \u{00B7} {}", param_label(&track.param))
}

/// The clip's mask tracks that hold keys, with their names: by Mask
/// node, then mask, then parameter as the mask list shows them.
pub fn keyed(clip: &Clip) -> Vec<(MaskTrack, String)> {
    let mut out = Vec::new();
    for (node, stack) in stacks(clip) {
        for (i, m) in stack.masks.iter().enumerate() {
            for p in m.keyed_tracks() {
                let t = MaskTrack { node, mask: i, param: p.to_owned() };
                let n = format!("{} \u{00B7} {}", mask_name(m, i), param_label(p));
                out.push((t, n));
            }
        }
    }
    out
}

/// The clip's mask parameters with no keys yet, by mask, each with its
/// value now: where a track can be started.
pub fn startable(clip: &Clip) -> Vec<(String, Vec<(MaskTrack, String, f32)>)> {
    let mut out = Vec::new();
    for (node, stack) in stacks(clip) {
        for (i, m) in stack.masks.iter().enumerate() {
            let params: Vec<_> = m
                .unkeyed_tracks()
                .into_iter()
                .map(|p| (MaskTrack { node, mask: i, param: p.to_owned() }, param_label(p), m.param_value(p).unwrap_or(0.0)))
                .collect();
            if !params.is_empty() {
                out.push((mask_name(m, i), params));
            }
        }
    }
    out
}

/// A track's keys, as the curve editor and the lane draw them: a shape
/// key with no value of its own, on the middle of its row.
pub fn keys(clip: &Clip, track: &MaskTrack) -> Vec<Keyframe> {
    mask_of(clip, track).map(|m| m.track_keys(&track.param)).unwrap_or_default()
}

/// A track's (min, max, default) on a curve: rotation in degrees, the rest
/// as frame fractions; the shape's row a fixed 0..1.
pub fn range(param: &str) -> (f32, f32, f32) {
    match param {
        "rotation" => (-180.0, 180.0, 0.0),
        "opacity" => (0.0, 1.0, 1.0),
        "center_x" | "center_y" => (0.0, 1.0, 0.5),
        _ => (0.0, 1.0, 0.5),
    }
}
