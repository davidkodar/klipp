//! A 3D LUT and a colour-space transform on a clip's picture come back
//! from a project as they went in (#1746), read by the strict loader: the
//! LUT's file, opacity and interpolation, the transform's gamuts and
//! curves, and the LUT opacity's keys. What the renderer prepares for them
//! each run is never written.

use klipp_core::keyframe::Keyframe;
use klipp_core::Timecode;
use klipp_effects::BuiltinEffect;
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_media::MediaPool;
use klipp_project::{load_project_strict, save_project};
use klipp_timeline::{Clip, Sequence};

#[test]
fn a_lut_and_a_transform_come_back_as_they_went_in() {
    let dir = std::env::temp_dir().join(format!("klipp_colour_effects_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let tc = |s: f64| Timecode::from_secs_f64(s, 24);
    let mut clip = Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0));
    let fx = [
        BuiltinEffect::ColourSpaceTransform { input_gamut: 6, input_curve: 2, output_gamut: 0, output_curve: 0, prepared: None },
        BuiltinEffect::Lut3D { path: "bundled:fuji-xtrans/look.cube".into(), opacity: 0.7, tetrahedral: false, prepared: None },
    ];
    // Added as the Picture view adds them, after the display step; the
    // LUT's opacity keyed.
    let at = |id| NodePath::new(Root::Clip { clip: clip.id.0 }, [id]);
    let picture = clip.picture_v2().unwrap().picture.clone();
    let mut after = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let mut ops = Vec::new();
    let mut ids = Vec::new();
    for e in &fx {
        let payload = serde_json::to_value(e).unwrap();
        let node: klipp_graph_format::Node = serde_json::from_value(serde_json::json!({
            "id": klipp_graph_format::NodeId::new_v4(),
            "type_id": klipp_graph_lift::names::effect_node_type(payload["type"].as_str().unwrap()),
            "legacy": { "payload": payload },
        }))
        .unwrap();
        ids.push(node.id);
        ops.push(GraphOp::AddNode { after: at(after), node: Box::new(node) });
        after = *ids.last().unwrap();
    }
    for (time_secs, value) in [(0.0, 0.2), (2.0, 0.9)] {
        let key = Keyframe { time_secs, value, interpolation: Default::default(), tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() };
        ops.push(GraphOp::AddKeyAt { path: at(ids[1]), key: "opacity".into(), keyframe: serde_json::to_value(key).unwrap() });
    }
    clip.apply_picture_ops(&ops).unwrap();
    let clip_id = clip.id;
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips = vec![clip.clone()];

    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project_strict(&dir).unwrap();
    let back = sequences[0].tracks[0].clips.iter().find(|c| c.id == clip_id).expect("the clip");
    let node = |c: &Clip, id| c.picture_v2().unwrap().picture.graph.nodes.iter().find(|n| n.id == id).cloned().unwrap_or_else(|| panic!("{id} came back"));
    for (id, effect) in ids.iter().zip(&fx) {
        let (went, came) = (node(&clip, *id), node(back, *id));
        assert_eq!(klipp_graph_lift::effect_params::effect_of(&came).unwrap(), *effect);
        assert_eq!(came.anim, went.anim, "{effect:?}: its keys");
    }
    assert_eq!(node(back, ids[1]).anim.len(), 1, "the LUT's opacity is keyed");

    let written = std::fs::read_to_string(dir.join("project.json")).unwrap();
    assert!(written.contains("\"lut_3d\"") && written.contains("\"colour_space_transform\""), "each is written under its own name");
    assert!(written.contains("bundled:fuji-xtrans/look.cube"), "the LUT's name is kept as stored");
    assert!(!written.contains("prepared"), "what the renderer prepares is never written");
    let _ = std::fs::remove_dir_all(&dir);
}
