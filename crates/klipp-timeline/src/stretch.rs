//! A clip's content stretched in time (#3055): what is timed from the
//! clip's start — its keyframes, its grade's keys, its markers and its
//! multicam cuts — moved with the content when the clip plays faster or
//! slower to fill a new length.

use klipp_core::Keyframe;
use klipp_graph_format::op::GraphOp;
use klipp_graph_format::{Animation, Graph, Node, NodeId, ParamKey};

use crate::Clip;

/// Every key in `keys` at `f` times its time from the clip's start, its
/// handles as long in time with it: the same curve over the stretched
/// clip.
pub fn scale_keys(keys: &mut [Keyframe], f: f64) {
    for k in keys {
        k.time_secs *= f;
        k.tangent_out.0 *= f;
        k.tangent_in.0 *= f;
    }
}

/// A speed ramp's keys stretched by `f`: each at `f` times its time, its
/// speed divided by `f`, so the ramp has the same shape and runs through
/// the same stretch of the file over `f` times the length.
pub fn scale_ramp(keys: &mut [Keyframe], f: f64) {
    scale_keys(keys, f);
    for k in keys {
        k.value = (f64::from(k.value) / f) as f32;
        k.tangent_out.1 = (f64::from(k.tangent_out.1) / f) as f32;
        k.tangent_in.1 = (f64::from(k.tangent_in.1) / f) as f32;
    }
}

/// A picture's animation stretched by `f`, as its keys are stored: each
/// key's time and its handles' time lengths; its behaviours as they were.
fn scale_anim(anim: &Animation, f: f64) -> Animation {
    let mut keys = anim.keys.clone();
    for k in &mut keys {
        let Some(o) = k.as_object_mut() else { continue };
        if let Some(t) = o.get("time_secs").and_then(serde_json::Value::as_f64) {
            o.insert("time_secs".into(), serde_json::json!(t * f));
        }
        for side in ["tangent_out", "tangent_in"] {
            if let Some(dt) = o.get(side).and_then(|v| v.get(0)).and_then(serde_json::Value::as_f64) {
                o.get_mut(side).expect("there")[0] = serde_json::json!(dt * f);
            }
        }
    }
    Animation { keys, modifiers: anim.modifiers.clone() }
}

/// The node at `ids` down through `graph`'s groups.
fn node_at<'a>(graph: &'a Graph, ids: &[NodeId]) -> Option<&'a Node> {
    let (first, rest) = ids.split_first()?;
    let n = graph.nodes.iter().find(|n| n.id == *first)?;
    if rest.is_empty() {
        Some(n)
    } else {
        node_at(&n.group.as_ref()?.graph, rest)
    }
}

impl Clip {
    /// Everything the clip times from its start stretched by `f` (#3055):
    /// its picture's keys (placement, opacity, effects, grade), its
    /// sound's, its matte keys, its markers, its multicam cuts and its
    /// speed ramp. Its fades are left as they are, being felt as a length
    /// of time, and so are its edges and speed, which the stretch sets. A
    /// picture that refuses the change leaves the clip as it was.
    pub fn scale_content_time(&mut self, f: f64) -> Result<(), String> {
        if !f.is_finite() || f <= 0.0 || (f - 1.0).abs() < 1e-12 {
            return Ok(());
        }
        let ops = self.picture_scale_ops(f)?;
        if !ops.is_empty() {
            self.apply_picture_ops(&ops)?;
        }
        for keys in [&mut self.volume_keyframes, &mut self.pan_keyframes, &mut self.gain_keyframes, &mut self.matte_feather_keyframes, &mut self.matte_choke_keyframes] {
            scale_keys(keys, f);
        }
        for keys in self.audio_effect_keyframes.values_mut() {
            scale_keys(keys, f);
        }
        scale_ramp(&mut self.speed_keyframes, f);
        for m in &mut self.clip_markers {
            m.time_secs *= f;
        }
        for cut in &mut self.multicam_cuts {
            cut.0 *= f;
        }
        Ok(())
    }

    /// The picture ops that put every animated key of the clip's picture
    /// at `f` times its time: the placement's and opacity's lanes, each
    /// effect's and each grade node's.
    fn picture_scale_ops(&self, f: f64) -> Result<Vec<GraphOp>, String> {
        let lifted = self.picture_v2()?;
        let picture = &lifted.picture;
        let mut ops = Vec::new();
        if let Some(path) = self.placement_path() {
            for (key, anim) in &picture.placement_anim {
                ops.push(GraphOp::SetAnim { path: path.clone(), key: key.clone(), anim: Some(scale_anim(anim, f)) });
            }
            if let Some(anim) = &picture.opacity_anim {
                ops.push(GraphOp::SetAnim { path: path.clone(), key: ParamKey::from(klipp_graph_lift::clip_properties::OPACITY), anim: Some(scale_anim(anim, f)) });
            }
        }
        for (path, key, _) in self.animated_effect_keys() {
            if let Some(anim) = node_at(&picture.graph, &path.nodes).and_then(|n| n.anim.get(&key)) {
                ops.push(GraphOp::SetAnim { path, key, anim: Some(scale_anim(anim, f)) });
            }
        }
        // A grade node's keys, as the curve editor reads them.
        for (path, key, _) in self.animated_grade_keys() {
            let Some(mut keys) = self.node_key_state(&path, &key).map(|k| k.keys) else { continue };
            scale_keys(&mut keys, f);
            let keys = keys.iter().map(serde_json::to_value).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
            ops.push(GraphOp::SetAnim { path, key, anim: Some(Animation { keys, modifiers: None }) });
        }
        Ok(ops)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::Interpolation;

    fn key(t: f64, v: f32) -> Keyframe {
        Keyframe { time_secs: t, value: v, interpolation: Interpolation::Bezier, tangent_out: (0.5, 0.2), tangent_in: (-0.5, -0.2), handles: Default::default() }
    }

    fn times(anim: &Animation) -> Vec<f64> {
        anim.keys.iter().filter_map(|k| k["time_secs"].as_f64()).collect()
    }

    /// A ramp from 2x to 0.5x over 2 s, stretched to twice its length:
    /// its keys at twice the time and half the speed, so it runs through
    /// the same stretch of the file, and its curve's handles twice as
    /// long in time and half as tall.
    #[test]
    fn a_ramp_keeps_its_shape_and_its_stretch_of_the_file() {
        let mut ramp = vec![key(0.0, 2.0), key(2.0, 0.5)];
        let before = crate::speed_integral_keyframes(&ramp, 2.0, 9600.0, 1.0);
        scale_ramp(&mut ramp, 2.0);
        assert_eq!((ramp[1].time_secs, ramp[1].value), (4.0, 0.25));
        assert_eq!((ramp[0].tangent_out.0, ramp[0].tangent_out.1), (1.0, 0.1));
        let after = crate::speed_integral_keyframes(&ramp, 4.0, 9600.0, 1.0);
        assert!((before - after).abs() < 1e-3, "the same source: {before} then {after}");
    }

    /// Every lane timed from the clip's start moves with its content: the
    /// placement and opacity lanes in the picture, an effect's keys, the
    /// sound's, the markers and the multicam cuts; the fades keep their
    /// seconds.
    #[test]
    fn the_clips_keys_markers_and_cuts_move_with_the_content() {
        let mut clip = crate::test_clip::TestClip::media(4.0)
            .effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 })
            .key(0, "radius", 2.0, 4.0)
            .lane("pos_x", 3.0, 10.0)
            .lane("opacity", 1.0, 0.5)
            .build();
        clip.volume_keyframes = vec![key(1.0, 0.5)];
        clip.audio_effect_keyframes.insert("0:gain".into(), vec![key(0.5, 1.0)]);
        clip.clip_markers.push(crate::ClipMarker { time_secs: 1.5, name: "beat".into(), color: klipp_core::MarkerColor::Red, comment: String::new() });
        clip.multicam_cuts = vec![(2.0, 1)];
        clip.fade_out_secs = 0.5;
        clip.scale_content_time(2.0).unwrap();
        let lifted = clip.picture_v2().unwrap();
        assert_eq!(times(&lifted.picture.placement_anim[&ParamKey::from("pos_x")]), vec![6.0]);
        assert_eq!(times(lifted.picture.opacity_anim.as_ref().unwrap()), vec![2.0]);
        let (path, k, _) = clip.animated_effect_keys().into_iter().next().expect("the blur's key");
        assert_eq!(times(&node_at(&lifted.picture.graph, &path.nodes).unwrap().anim[&k]), vec![4.0]);
        assert_eq!(clip.volume_keyframes[0].time_secs, 2.0);
        assert_eq!(clip.audio_effect_keyframes["0:gain"][0].time_secs, 1.0);
        assert_eq!(clip.clip_markers[0].time_secs, 3.0);
        assert_eq!(clip.multicam_cuts, vec![(4.0, 1)]);
        assert_eq!(clip.fade_out_secs, 0.5, "a fade keeps its seconds");
    }

    /// A grade node's keys, kept with the node, move with the content too.
    #[test]
    fn a_grade_nodes_keys_move_with_the_content() {
        use crate::GradeAccess;
        let mut clip = crate::test_clip::TestClip::media(4.0).build();
        let mut g = klipp_nodes::NodeGraph::default();
        let corrector = g.ensure_corrector();
        clip.set_grade(g);
        let path = clip.grade_nodes().into_iter().map(|(p, ..)| p).find(|p| p.node() == Some(corrector)).expect("the Corrector by path");
        for (t, v) in [(1.0, 0.2), (3.0, 0.8)] {
            let keyframe = serde_json::to_value(key(t, v)).unwrap();
            clip.apply_picture_ops(&[GraphOp::AddKeyAt { path: path.clone(), key: ParamKey::from("exposure"), keyframe }]).unwrap();
        }
        clip.scale_content_time(1.5).unwrap();
        let keys = clip.node_key_state(&path, &ParamKey::from("exposure")).map(|s| s.keys).unwrap_or_default();
        assert_eq!(keys.iter().map(|k| k.time_secs).collect::<Vec<_>>(), vec![1.5, 4.5]);
        assert_eq!(keys.iter().map(|k| k.value).collect::<Vec<_>>(), vec![0.2, 0.8], "the values as they were");
    }
}
