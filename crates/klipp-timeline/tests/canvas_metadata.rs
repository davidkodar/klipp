//! Canvas metadata follows stable picture nodes through legacy field edits.
use klipp_core::{MediaId, RationalTime};
use klipp_effects::BuiltinEffect;
use klipp_graph_format::{op::GraphOp, path::{NodePath, Root}};
use klipp_timeline::Clip;
use serde_json::json;

#[test]
fn canvas_metadata_survives_effect_edits_reorder_and_removal() {
    let t = |s| RationalTime::from_secs_f64(s, 25);
    let mut c = Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
    c.push_effect(BuiltinEffect::GaussianBlur { radius: 2.0 });
    c.push_effect(BuiltinEffect::Sharpen { amount: 0.5 });
    let before = c.picture_v2().unwrap();
    let blur = before.picture.graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("gaussian_blur")).unwrap().id;
    let root = Root::Clip { clip: c.id.0 };
    // Arrange every root node, including structural ends and the grade group.
    let ops: Vec<_> = before.picture.graph.nodes.iter().enumerate().map(|(i, n)| GraphOp::SetNodeMeta {
        path: NodePath::new(root, [n.id]), label: Some(format!("Node {i}")), position: Some(json!({"x":i as f32 * 200.0,"y":70.0})),
    }).collect();
    let edit = c.apply_picture_ops(&ops).unwrap();
    let arranged = c.picture_v2().unwrap();
    assert!(c.is_picture_truth());
    let effect = c.effect_id_at(0).unwrap();
    assert!(c.set_effect_enabled(effect, false));
    assert!(c.move_builtin_effect(0, 1));
    let reordered = c.picture_v2().unwrap();
    assert_eq!(reordered.picture.graph.ui.positions, arranged.picture.graph.ui.positions);
    for old in &arranged.picture.graph.nodes {
        assert_eq!(reordered.picture.graph.nodes.iter().find(|n| n.id == old.id).unwrap().label, old.label);
    }
    assert!(!reordered.picture.graph.nodes.iter().find(|n| n.id == blur).unwrap().enabled);
    c.remove_builtin_effect(1).unwrap();
    let removed = c.picture_v2().unwrap();
    assert!(!removed.picture.graph.ui.positions.contains_key(&blur));
    assert!(!removed.picture.graph.nodes.iter().any(|n| n.id == blur));
    c.restore_picture(&edit.before);
    assert_eq!(c.picture_v2().unwrap(), before);
}
