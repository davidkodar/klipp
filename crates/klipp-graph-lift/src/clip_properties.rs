//! A clip's placement and opacity as properties of its picture (M3 step
//! 13): addressed as parameters of the clip's PLACEMENT node, held in the
//! picture's `placement`, `placement_anim`, `opacity` and `opacity_anim`,
//! never in the node's own parameters.
//!
//! The keys are the lift's own names (`placement` in `lift.rs`): `pos_x`,
//! `pos_y`, `scale`, `scale_y`, `rotation`, `anchor_x`, `anchor_y`,
//! `tilt_x`, `tilt_y`, `perspective`, `flip_x`, `flip_y` (on at 1, off at
//! 0), and `opacity`. All but the anchor, the perspective and the flips
//! are animated.

use klipp_graph_format::lifted::{types as lt, ClipPicture};
use klipp_graph_format::op::GraphOp;
use klipp_graph_format::{Animation, ParamKey, ParamValue};
use serde_json::Value;

/// The placement's keys.
pub const PLACEMENT: [&str; 12] = ["pos_x", "pos_y", "scale", "scale_y", "rotation", "anchor_x", "anchor_y", "tilt_x", "tilt_y", "perspective", "flip_x", "flip_y"];

/// The opacity's key.
pub const OPACITY: &str = "opacity";

/// Whether `key` names a clip property.
pub fn is_property(key: &ParamKey) -> bool {
    key.as_str() == OPACITY || PLACEMENT.contains(&key.as_str())
}

/// Whether `key` takes keyframes: the opacity, and the placement values a
/// clip keys (`PLACEMENT_ANIM_KEYS`; its height follows the width's lane
/// unless it has a lane of its own, #1471).
pub fn is_animated(key: &ParamKey) -> bool {
    key.as_str() == OPACITY || klipp_graph_format::lifted::PLACEMENT_ANIM_KEYS.contains(&key.as_str())
}

/// The id a clip's placement is addressed by, whatever its node's id now:
/// the automatic placement's (the lift's `lift/placement`). A fixed
/// placement takes its Transform's id, and the first placement write on a
/// clip without one makes it fixed; an address that changed under a drag
/// would lose the drag, so the placement is always found by this one.
pub fn placement_id(clip: uuid::Uuid) -> klipp_graph_format::NodeId {
    crate::ids::entry_id(crate::ids::RootKind::Clip, clip, "lift/placement", 0)
}

/// The picture's own PLACEMENT node (at its top level).
pub fn placement_node(picture: &ClipPicture) -> Option<&klipp_graph_format::Node> {
    picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::PLACEMENT && n.origin.is_none())
}

/// The key `op` sets on a clip property: an op on the picture's own
/// PLACEMENT node, by its id or by [`placement_id`], that names one.
/// `None` for any other op, which edits nodes as ever.
pub fn property_of<'a>(picture: &ClipPicture, op: &'a GraphOp) -> Option<&'a ParamKey> {
    let (path, key) = match op {
        GraphOp::SetParam { path, key, .. } | GraphOp::AddKeyAt { path, key, .. } | GraphOp::RemoveKeyAt { path, key, .. } | GraphOp::SetAnim { path, key, .. } => (path, key),
        _ => return None,
    };
    let [id] = path.nodes.as_slice() else { return None };
    let node = placement_node(picture)?;
    let named = node.id == *id || path.root_clip().is_some_and(|clip| placement_id(clip) == *id);
    (named && is_property(key)).then_some(key)
}

/// `op` (one [`property_of`] names) applied to the picture's properties.
pub fn apply(picture: &mut ClipPicture, op: &GraphOp) -> Result<(), String> {
    let key = property_of(picture, op).ok_or("not an op on a clip property")?.clone();
    if let GraphOp::SetParam { value, .. } = op {
        let v = match value {
            ParamValue::Float(v) if v.is_finite() => *v,
            other => return Err(format!("{key} takes a number, not {other:?}")),
        };
        let p = &mut picture.placement;
        match key.as_str() {
            "pos_x" => p.pos_x = Some(v),
            "pos_y" => p.pos_y = Some(v),
            // The width; the height keeps the aspect under it.
            "scale" => p.scale = Some(v),
            // The height, kept as the aspect against the width (#1406).
            "scale_y" => p.aspect = crate::names::aspect(p.scale.or(Some(1.0)), Some(v)),
            "rotation" => p.rotation = Some(v),
            "anchor_x" => p.anchor_x = Some(v),
            "anchor_y" => p.anchor_y = Some(v),
            "tilt_x" => p.tilt_x = Some(v),
            "tilt_y" => p.tilt_y = Some(v),
            "perspective" => p.perspective = Some(v),
            // A flip is on or off (#2705).
            "flip_x" => p.flip_x = v >= 0.5,
            "flip_y" => p.flip_y = v >= 0.5,
            // Clamped as the renderer clamps a static opacity; under keys
            // it is stored as given.
            _ => picture.opacity = if picture.opacity_anim.is_some() { v } else { v.clamp(0.0, 1.0) },
        }

        return Ok(());
    }
    if !is_animated(&key) {
        return Err(format!("{key} doesn't take keyframes"));
    }
    let current = if key.as_str() == OPACITY { picture.opacity_anim.clone() } else { picture.placement_anim.get(&key).cloned() };
    let next = animated(current, op)?;
    if key.as_str() == OPACITY {
        picture.opacity_anim = next;
    } else {
        match next {
            Some(a) => picture.placement_anim.insert(key, a),
            None => picture.placement_anim.remove(&key),
        };
    }
    Ok(())
}

/// Keys at the same time are one key.
const SAME_TIME: f64 = 1e-9;

fn key_time(k: &Value) -> Result<f64, String> {
    k.get("time_secs").and_then(Value::as_f64).ok_or_else(|| format!("a keyframe without its time: {k}"))
}

/// An animation after the key op `op`: a key put in (replacing one at its
/// time) or taken off, or the animation set whole.
fn animated(current: Option<Animation>, op: &GraphOp) -> Result<Option<Animation>, String> {
    Ok(match op {
        GraphOp::AddKeyAt { keyframe, .. } => {
            let t = key_time(keyframe)?;
            let mut a = current.unwrap_or(Animation { keys: Vec::new(), modifiers: None });
            a.keys.retain(|k| key_time(k).map_or(true, |kt| (kt - t).abs() > SAME_TIME));
            let at = a.keys.iter().position(|k| key_time(k).is_ok_and(|kt| kt > t)).unwrap_or(a.keys.len());
            a.keys.insert(at, keyframe.clone());
            Some(a)
        }
        GraphOp::RemoveKeyAt { key, time_secs, .. } => {
            let mut a = current.ok_or_else(|| format!("{key} isn't animated"))?;
            let before = a.keys.len();
            a.keys.retain(|k| key_time(k).map_or(true, |kt| (kt - time_secs).abs() > SAME_TIME));
            if a.keys.len() == before {
                return Err(format!("{key} has no key at {time_secs} s"));
            }
            (!a.keys.is_empty() || a.modifiers.is_some()).then_some(a)
        }
        GraphOp::SetAnim { anim, .. } => {
            for k in anim.iter().flat_map(|a| &a.keys) {
                key_time(k)?;
            }
            anim.clone()
        }
        _ => current,
    })
}
