//! The Video tab's Transform and Opacity card and the viewer's transform
//! gizmo, behind the generated-stack switch, edit the clip's placement and
//! opacity as clip properties (M3 step 13): by path, on the clip's
//! PLACEMENT node, never by finding (or adding) a Transform or Opacity
//! entry in its effect list.
//!
//! They still say what they change as they always have; [`by_path`] turns
//! those commands into the path commands that change the same thing:
//! - an update (or an add) of the Transform or the Opacity they show, into
//!   one `SetParamAt` per value that differs from the clip's;
//! - their drag's begin, end and abandon, into a drag over the placement
//!   node's parameters together (`BeginParamEditAt` with an empty key);
//! - a key put on or taken off a placement or opacity lane, into
//!   `AddKeyAt` / `RemoveKeyAt` on the node's key.
//!
//! Anything else passes as it was.

use klipp_core::{AppCommand, VideoParam};
use klipp_effects::BuiltinEffect;
use klipp_graph_format::{ParamKey, ParamValue};
use klipp_timeline::Clip;

/// The drag keys the card and the gizmo hold their drags under.
fn is_placement_drag(key: &str) -> bool {
    key.ends_with("video_transform") || key.ends_with("video_opacity") || key.starts_with("viewer_transform_")
}

/// The node key a placement or opacity lane is, if it is one.
pub fn lane_key(param: VideoParam) -> Option<&'static str> {
    Some(match param {
        VideoParam::Opacity => "opacity",
        VideoParam::PositionX => "pos_x",
        VideoParam::PositionY => "pos_y",
        VideoParam::Scale => "scale",
        VideoParam::Rotation => "rotation",
        VideoParam::TiltX => "tilt_x",
        VideoParam::TiltY => "tilt_y",
        VideoParam::MatteFeather | VideoParam::MatteChoke => return None,
    })
}

/// `effect`'s values as the placement's keys, if it is a Transform or an
/// Opacity.
fn values(effect: &BuiltinEffect) -> Vec<(&'static str, f32)> {
    match effect {
        BuiltinEffect::Transform { position_x, position_y, scale_x, rotation, anchor_x, anchor_y, rotation_x, rotation_y, perspective, .. } => vec![
            ("pos_x", *position_x),
            ("pos_y", *position_y),
            ("scale", *scale_x),
            ("rotation", *rotation),
            ("anchor_x", *anchor_x),
            ("anchor_y", *anchor_y),
            ("tilt_x", *rotation_x),
            ("tilt_y", *rotation_y),
            ("perspective", *perspective),
        ],
        BuiltinEffect::Opacity { value } => vec![("opacity", *value)],
        _ => Vec::new(),
    }
}

/// The clip's placement and opacity as its picture holds them, by key.
fn current(clip: &Clip) -> Vec<(&'static str, f32)> {
    let Ok(lifted) = clip.picture_v2() else { return Vec::new() };
    let p = &lifted.picture.placement;
    [
        ("pos_x", p.pos_x.unwrap_or(0.0)),
        ("pos_y", p.pos_y.unwrap_or(0.0)),
        ("scale", p.scale.unwrap_or(1.0)),
        ("rotation", p.rotation.unwrap_or(0.0)),
        ("anchor_x", p.anchor_x.unwrap_or(0.5)),
        ("anchor_y", p.anchor_y.unwrap_or(0.5)),
        ("tilt_x", p.tilt_x.unwrap_or(0.0)),
        ("tilt_y", p.tilt_y.unwrap_or(0.0)),
        ("perspective", p.perspective.unwrap_or(0.0)),
        ("opacity", lifted.picture.opacity),
    ]
    .to_vec()
}

/// The clip's placement as the picture holds it, as the card and the
/// gizmo show a transform.
pub fn placement(clip: &Clip) -> crate::inspector::video_tab::TransformParams {
    let get = |k: &str| current(clip).into_iter().find(|(key, _)| *key == k).map(|(_, v)| v);
    let mut t = crate::inspector::video_tab::TransformParams::default();
    if let Some(v) = get("pos_x") { t.position_x = v; }
    if let Some(v) = get("pos_y") { t.position_y = v; }
    if let Some(v) = get("scale") { (t.scale_x, t.scale_y) = (v, v); }
    if let Some(v) = get("rotation") { t.rotation = v; }
    if let Some(v) = get("anchor_x") { t.anchor_x = v; }
    if let Some(v) = get("anchor_y") { t.anchor_y = v; }
    if let Some(v) = get("tilt_x") { t.rotation_x = v; }
    if let Some(v) = get("tilt_y") { t.rotation_y = v; }
    if let Some(v) = get("perspective") { t.perspective = v; }
    t
}

/// `cmds`, the card's or the gizmo's for `clip`, as path commands on the
/// clip's placement node. Unchanged when the clip has none.
pub fn by_path(cmds: Vec<AppCommand>, clip: &Clip) -> Vec<AppCommand> {
    let Some(addr) = clip.placement_path() else { return cmds };
    let whole = ParamKey::from("");
    let mut now = current(clip);
    let mut out = Vec::with_capacity(cmds.len());
    for cmd in cmds {
        match &cmd {
            AppCommand::UpdateBuiltinEffect { clip_id, effect, .. } | AppCommand::AddBuiltinEffect { clip_id, effect } if *clip_id == clip.id && !values(effect).is_empty() => {
                for (key, v) in values(effect) {
                    let was = now.iter_mut().find(|(k, _)| *k == key);
                    if was.as_ref().is_some_and(|(_, w)| (*w - v).abs() <= f32::EPSILON) {
                        continue;
                    }
                    if let Some(w) = was {
                        w.1 = v;
                    }
                    out.push(AppCommand::SetParamAt { addr: addr.clone(), key: key.into(), value: ParamValue::Float(v) });
                }
            }
            AppCommand::BeginEffectDrag { key, clip_id, .. } if *clip_id == clip.id && is_placement_drag(key) => out.push(AppCommand::BeginParamEditAt { addr: addr.clone(), key: whole.clone() }),
            AppCommand::EndEffectDrag { key, clip_id, .. } if *clip_id == clip.id && is_placement_drag(key) => out.push(AppCommand::EndParamEditAt { addr: addr.clone(), key: whole.clone() }),
            // The gizmo names no drag key on its abandon; it only ever
            // abandons the placement drag it opened.
            AppCommand::AbandonEffectDrag { clip_id, .. } if *clip_id == clip.id => out.push(AppCommand::AbandonParamEditAt { addr: addr.clone(), key: whole.clone() }),
            AppCommand::AddVideoKeyframe { clip_id, param, time_secs, value } if *clip_id == clip.id && lane_key(*param).is_some() => {
                let key = klipp_core::Keyframe { time_secs: *time_secs, value: *value, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) };
                match serde_json::to_value(key) {
                    Ok(keyframe) => out.push(AppCommand::AddKeyAt { addr: addr.clone(), key: lane_key(*param).unwrap_or_default().into(), keyframe }),
                    Err(_) => out.push(cmd),
                }
            }
            AppCommand::RemoveVideoKeyframe { clip_id, param, time_secs } if *clip_id == clip.id && lane_key(*param).is_some() => {
                out.push(AppCommand::RemoveKeyAt { addr: addr.clone(), key: lane_key(*param).unwrap_or_default().into(), time_secs: *time_secs });
            }
            _ => out.push(cmd),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip() -> Clip {
        let t = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        Clip::media(klipp_core::MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0))
    }

    /// The card's first move on a clip with no Transform: no Transform
    /// added, only the value that changed, on the placement node.
    #[test]
    fn an_add_becomes_the_changed_values_on_the_placement_node() {
        let c = clip();
        let addr = c.placement_path().unwrap();
        let t = BuiltinEffect::Transform { position_x: 30.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
        let out = by_path(vec![AppCommand::AddBuiltinEffect { clip_id: c.id, effect: t }], &c);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(matches!(&out[0], AppCommand::SetParamAt { addr: a, key, value: ParamValue::Float(v) } if *a == addr && key.as_str() == "pos_x" && *v == 30.0), "{out:?}");
    }
}
