//! #1394 through a real save and a strict reload: a moved clip made
//! automatic keeps its move as a picture-truth picture, which the file
//! stores and the loader takes back as it was.

use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::{ParamKey, ParamValue};
use klipp_graph_lift::ops::GraphOp;
use klipp_media::MediaPool;
use klipp_project::{load_project_strict, save_project};
use klipp_timeline::{Clip, Sequence};

#[test]
fn an_automatic_move_is_saved_and_comes_back() {
    let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut c = Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    c.set_effect_list(vec![E::Transform { position_x: 40.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }], vec![true]);
    let path = c.placement_path().expect("a placement");
    c.apply_picture_ops(&[GraphOp::SetParam { path, key: ParamKey::from("mode"), value: ParamValue::Enum(lt::PLACEMENT_AUTO) }]).expect("Auto keeps the move");
    assert!(c.is_picture_truth(), "an automatic move is picture-truth");
    let id = c.id;
    let dir = std::env::temp_dir().join(format!("klipp_auto_move_save_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips.push(c);
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project_strict(&dir).expect("a strict reload");
    let back = sequences[0].all_clips().find(|c| c.id == id).expect("the clip");
    assert!(back.is_picture_truth(), "still picture-truth");
    let p = back.picture_v2().unwrap().picture.clone();
    assert_eq!(p.placement.pos_x, Some(40.0), "the move is kept");
    let placement = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::PLACEMENT).expect("a placement node");
    assert_eq!(placement.params.get(&ParamKey::from("mode")), Some(&ParamValue::Enum(lt::PLACEMENT_AUTO)));
    let _ = std::fs::remove_dir_all(&dir);
}
